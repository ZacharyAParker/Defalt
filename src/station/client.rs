//! Talking to the station over HTTP, from one place.
//!
//! Every request the console makes -- a status poll, a schedule poll, a
//! thumbs up, a request for a record -- used to be a thread of its own with
//! whatever timeout ureq felt like, which for a GET was none. A station that
//! stopped answering without closing its socket left those threads waiting
//! forever, and the flag that said "a poll is out" with them, so nothing was
//! ever asked again.
//!
//! Now there is one client. It keeps one connection pool and a few worker
//! threads: GETs on one, with a short global timeout, and POSTs on two more
//! with timeouts sized to what they do (starting the decks takes seconds, a
//! thumb does not). Requests go in as commands and answers come back as
//! events on the asker's own channel, tagged with whatever the asker wants
//! to know them by. And the asker keeps a watchdog: an answer that has not
//! come back well after its timeout is reported as timed out, so no flag
//! can ever latch on a lost reply. A reply that turns up after that is
//! dropped.
//!
//! The events stream (`GET /api/events`) is here too: a long-lived request
//! on a thread of its own, parsed as it arrives, which the station pushes
//! its schedule, queue and status down whenever they change. It is a
//! convenience, never a dependency -- if it drops, the callers go back to
//! polling and the stream is retried with a backoff.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// GETs are status and schedule reads. Anything that takes longer than this
/// on localhost is not coming.
pub const GET_TIMEOUT: Duration = Duration::from_secs(4);
/// How long past its own timeout a request may run before the watchdog
/// gives up on it.
const GRACE: Duration = Duration::from_secs(2);

/// Why a request came to nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// The HTTP status, when the station answered with one.
    pub status: Option<u16>,
    /// The station's own reason when it gave one, else ours.
    pub message: String,
    /// No answer in time -- the watchdog's verdict or the socket's.
    pub timed_out: bool,
}

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Failure { status: None, message: message.into(), timed_out: false }
    }

    fn timed_out() -> Self {
        Failure { status: None, message: "the station did not answer in time".into(), timed_out: true }
    }

    /// A 5xx: the station is up but not ready, or broken for a moment.
    pub fn server_error(&self) -> bool {
        self.status.is_some_and(|s| s >= 500)
    }

    /// A 4xx: the station understood and said no.
    pub fn refused(&self) -> bool {
        self.status.is_some_and(|s| (400..500).contains(&s))
    }
}

pub type Outcome = Result<Value, Failure>;

/// One answered request.
pub struct Done<T> {
    pub tag: T,
    pub outcome: Outcome,
    /// When the request was handed to a worker, and when the answer came
    /// back. Their midpoint is the best guess at when the station read its
    /// clock.
    pub sent: Instant,
    pub received: Instant,
}

impl<T> Done<T> {
    pub fn round_trip(&self) -> Duration {
        self.received.saturating_duration_since(self.sent)
    }

    pub fn midpoint(&self) -> Instant {
        self.sent + self.round_trip() / 2
    }
}

struct Job {
    id: u64,
    url: String,
    body: Option<Option<Value>>,
    timeout: Duration,
    reply: Sender<(u64, Outcome, Instant, Instant)>,
}

/// The shared half: the pool and the workers. Cheap to clone; the workers
/// are started on first use and leave when the last clone does.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    base: String,
    agent: ureq::Agent,
    lanes: Mutex<Option<(Sender<Job>, Sender<Job>)>>,
}

impl Client {
    pub fn new(port: u16) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(GET_TIMEOUT))
            .build()
            .new_agent();
        Client {
            inner: Arc::new(Inner {
                base: format!("http://127.0.0.1:{port}"),
                agent,
                lanes: Mutex::new(None),
            }),
        }
    }

    pub fn base(&self) -> &str {
        &self.inner.base
    }

    /// A handle for one caller, with its own replies and its own watchdog.
    pub fn handle<T: Clone>(&self) -> Handle<T> {
        let (reply, replies) = channel();
        Handle { client: self.clone(), reply, replies, inflight: HashMap::new(), next: 1 }
    }

    fn submit(&self, job: Job) -> bool {
        let mut lanes = self.inner.lanes.lock().unwrap_or_else(|p| p.into_inner());
        let (gets, posts) = lanes.get_or_insert_with(|| {
            let (gets, get_jobs) = channel::<Job>();
            let (posts, post_jobs) = channel::<Job>();
            let agent = self.inner.agent.clone();
            std::thread::Builder::new().name("station-get".into())
                .spawn(move || work(agent, Arc::new(Mutex::new(get_jobs)))).ok();
            let post_jobs = Arc::new(Mutex::new(post_jobs));
            for index in 0..2 {
                let (agent, jobs) = (self.inner.agent.clone(), post_jobs.clone());
                std::thread::Builder::new().name(format!("station-post-{index}"))
                    .spawn(move || work(agent, jobs)).ok();
            }
            (gets, posts)
        });
        let lane = if job.body.is_some() { posts } else { gets };
        lane.send(job).is_ok()
    }

    /// Answer one request right here, blocking. For shutting down, where
    /// there is nothing left to be responsive for.
    pub fn post_blocking(&self, path: &str, body: Option<Value>, timeout: Duration) -> Outcome {
        request(&self.inner.agent, &format!("{}{path}", self.inner.base), Some(body), timeout)
    }
}

/// A worker: answer jobs until nobody can send any more.
fn work(agent: ureq::Agent, jobs: Arc<Mutex<Receiver<Job>>>) {
    loop {
        let job = match jobs.lock() {
            Ok(queue) => match queue.recv() {
                Ok(job) => job,
                Err(_) => return,
            },
            Err(_) => return,
        };
        let sent = Instant::now();
        // A request that panics must still answer, or its caller waits for
        // the watchdog; and it must not take the worker with it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            request(&agent, &job.url, job.body, job.timeout)
        }))
        .unwrap_or_else(|_| Err(Failure::new("the request crashed")));
        let _ = job.reply.send((job.id, outcome, sent, Instant::now()));
    }
}

fn request(agent: &ureq::Agent, url: &str, body: Option<Option<Value>>, timeout: Duration) -> Outcome {
    let sent = match body {
        None => agent.get(url).config().timeout_global(Some(timeout)).build().call(),
        Some(Some(body)) => agent.post(url).config().timeout_global(Some(timeout)).build().send_json(&body),
        Some(None) => agent.post(url).config().timeout_global(Some(timeout)).build().send_empty(),
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(ureq::Error::Timeout(_)) => return Err(Failure::timed_out()),
        Err(ureq::Error::StatusCode(code)) => {
            return Err(Failure { status: Some(code), message: format!("the station said no ({code})"), timed_out: false });
        }
        Err(_) => return Err(Failure::new("the station is not answering")),
    };
    let status = response.status().as_u16();
    let payload = response.body_mut().with_config().limit(64 * 1024 * 1024).read_json::<Value>();
    if (200..300).contains(&status) {
        return payload.map_err(|error| match error {
            ureq::Error::Timeout(_) => Failure::timed_out(),
            _ => Failure { status: Some(status), message: "the station sent something unreadable".into(), timed_out: false },
        });
    }
    // A refusal carries the station's own reason, which is far more use
    // than "that did not work" -- "too late, that one is already playing"
    // tells you what to do instead.
    let payload = payload.unwrap_or_default();
    let message = payload["message"].as_str().or(payload["error"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("the station said no ({status})"));
    Err(Failure { status: Some(status), message, timed_out: false })
}

/// One caller's view of the client.
pub struct Handle<T> {
    client: Client,
    reply: Sender<(u64, Outcome, Instant, Instant)>,
    replies: Receiver<(u64, Outcome, Instant, Instant)>,
    /// What is out, by id: its tag, when it went, and when to stop waiting.
    inflight: HashMap<u64, (T, Instant, Instant)>,
    next: u64,
}

impl<T: Clone + PartialEq> Handle<T> {
    pub fn get(&mut self, tag: T, path: &str) -> u64 {
        self.submit(tag, path, None, GET_TIMEOUT)
    }

    pub fn post(&mut self, tag: T, path: &str, body: Option<Value>, timeout: Duration) -> u64 {
        self.submit(tag, path, Some(body), timeout)
    }

    fn submit(&mut self, tag: T, path: &str, body: Option<Option<Value>>, timeout: Duration) -> u64 {
        let id = self.next;
        self.next += 1;
        let job = Job { id, url: format!("{}{path}", self.client.base()), body, timeout, reply: self.reply.clone() };
        let now = Instant::now();
        self.inflight.insert(id, (tag, now, now + timeout + GRACE));
        if !self.client.submit(job) {
            // No workers: say so on the next poll rather than never.
            if let Some((_, sent, _)) = self.inflight.get_mut(&id) {
                let _ = self.reply.send((id, Err(Failure::new("the station client stopped")), *sent, now));
            }
        }
        id
    }

    /// Pretend a request went out and came back with `outcome`.
    #[cfg(test)]
    pub fn inflight_for_test(&mut self, tag: T, outcome: Outcome) {
        let id = self.next;
        self.next += 1;
        let now = Instant::now();
        self.inflight.insert(id, (tag, now, now + Duration::from_secs(60)));
        let _ = self.reply.send((id, outcome, now, now));
    }

    /// Is a request with this tag still out?
    pub fn busy(&self, tag: &T) -> bool {
        self.inflight.values().any(|(t, _, _)| t == tag)
    }

    /// Forget every request with this tag: whatever it answers is dropped.
    pub fn abandon(&mut self, tag: &T) {
        self.inflight.retain(|_, (t, _, _)| t != tag);
    }

    /// Everything answered since last time, and everything the watchdog has
    /// given up on.
    pub fn poll(&mut self) -> Vec<Done<T>> {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Vec<Done<T>> {
        let mut done = Vec::new();
        while let Ok((id, outcome, sent, received)) = self.replies.try_recv() {
            // Not ours any more: abandoned, or already reported as timed out.
            let Some((tag, _, _)) = self.inflight.remove(&id) else { continue };
            done.push(Done { tag, outcome, sent, received });
        }
        let overdue: Vec<u64> = self.inflight.iter()
            .filter(|(_, (_, _, deadline))| now >= *deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in overdue {
            if let Some((tag, sent, _)) = self.inflight.remove(&id) {
                done.push(Done { tag, outcome: Err(Failure::timed_out()), sent, received: now });
            }
        }
        done
    }
}

/* ── The events stream ───────────────────────────────────────────────── */

/// One pushed topic: its name, its payload, and when it arrived.
pub struct Pushed {
    pub topic: String,
    pub data: Value,
    pub at: Instant,
}

/// A running subscription. Dropping it stops the stream (the thread leaves
/// at its next line or its next retry).
pub struct Subscription {
    pub events: Receiver<Pushed>,
    stop: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    /// Milliseconds since `born` of the last thing read, ping included.
    heard: Arc<AtomicU64>,
    born: Instant,
}

/// Longer than the station's 15 s ping with room to spare: a stream this
/// quiet is gone, whatever its socket thinks.
const STALE: Duration = Duration::from_secs(40);

impl Subscription {
    /// True while the stream is up and has been heard from recently.
    pub fn live(&self) -> bool {
        self.connected.load(Ordering::Acquire) && self.quiet() < STALE
    }

    /// Up for a while and not heard from: a stream that went quiet without
    /// closing. Drop it and subscribe again.
    pub fn stale(&self) -> bool {
        self.born.elapsed() > STALE && !self.live()
    }

    fn quiet(&self) -> Duration {
        let heard = Duration::from_millis(self.heard.load(Ordering::Acquire));
        self.born.elapsed().saturating_sub(heard)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Client {
    /// Subscribe to some of the station's topics.
    pub fn subscribe(&self, topics: &[&str]) -> Subscription {
        let (sender, events) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let heard = Arc::new(AtomicU64::new(0));
        let born = Instant::now();
        let url = format!("{}/api/events?topics={}", self.base(), topics.join(","));
        let agent = self.inner.agent.clone();
        let (s, c, h) = (stop.clone(), connected.clone(), heard.clone());
        std::thread::Builder::new().name("station-events".into())
            .spawn(move || stream(agent, url, sender, s, c, h, born)).ok();
        Subscription { events, stop, connected, heard, born }
    }
}

/// Keep the stream open, reconnecting with a backoff, until told to stop.
fn stream(
    agent: ureq::Agent,
    url: String,
    sender: Sender<Pushed>,
    stop: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    heard: Arc<AtomicU64>,
    born: Instant,
) {
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    while !stop.load(Ordering::Acquire) {
        let opened = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            read_stream(&agent, &url, &sender, &stop, &connected, &heard, born)
        }));
        connected.store(false, Ordering::Release);
        if matches!(result, Ok(Ok(()))) && stop.load(Ordering::Acquire) {
            return;
        }
        // A stream that stayed up a good while was a working stream that
        // ended; start the backoff over.
        if opened.elapsed() > Duration::from_secs(20) {
            backoff.reset();
        }
        let wait = backoff.next();
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if stop.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn read_stream(
    agent: &ureq::Agent,
    url: &str,
    sender: &Sender<Pushed>,
    stop: &AtomicBool,
    connected: &AtomicBool,
    heard: &AtomicU64,
    born: Instant,
) -> Result<(), String> {
    let response = agent.get(url).config()
        .timeout_global(None)
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_recv_response(Some(Duration::from_secs(5)))
        .build()
        .call()
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the events stream answered {}", response.status()));
    }
    let (_, body) = response.into_parts();
    let mut reader = BufReader::new(body.into_reader());
    let mut parser = Sse::default();
    let mut line = String::new();
    connected.store(true, Ordering::Release);
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        line.clear();
        let read = reader.read_line(&mut line).map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("the events stream ended".into());
        }
        heard.store(born.elapsed().as_millis() as u64, Ordering::Release);
        if let Some(Line::Event { name, data }) = parser.feed(&line) {
            let Ok(data) = serde_json::from_str::<Value>(&data) else { continue };
            if sender.send(Pushed { topic: name, data, at: Instant::now() }).is_err() {
                return Ok(());
            }
        }
    }
}

/// What one line of `text/event-stream` came to.
#[derive(Debug, PartialEq)]
pub enum Line {
    /// A blank line ended an event with data in it.
    Event { name: String, data: String },
    /// `: ping`, or any other comment.
    Comment,
    /// `retry: <ms>`.
    Retry(u64),
}

/// The event-stream format, one line at a time: `event:` names the event,
/// each `data:` adds a line to it, a blank line sends it, and a line starting
/// with a colon is a comment.
#[derive(Default)]
pub struct Sse {
    name: String,
    data: String,
    has_data: bool,
}

impl Sse {
    pub fn feed(&mut self, raw: &str) -> Option<Line> {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            let name = std::mem::take(&mut self.name);
            let data = std::mem::take(&mut self.data);
            if !std::mem::take(&mut self.has_data) {
                return None;
            }
            let name = if name.is_empty() { "message".to_string() } else { name };
            return Some(Line::Event { name, data });
        }
        if line.starts_with(':') {
            return Some(Line::Comment);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.name = value.to_string(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "retry" => return value.trim().parse().ok().map(Line::Retry),
            _ => {}
        }
        None
    }
}

/// Doubling waits, capped.
#[derive(Clone, Debug)]
pub struct Backoff {
    first: Duration,
    cap: Duration,
    next: Duration,
}

impl Backoff {
    pub fn new(first: Duration, cap: Duration) -> Self {
        Backoff { first, cap, next: first }
    }

    pub fn next(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(self.cap);
        wait
    }

    pub fn reset(&mut self) {
        self.next = self.first;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_stream_parses_names_data_comments_and_retries() {
        let mut sse = Sse::default();
        let text = "retry: 3000\n\nevent: schedule\ndata: {\"now\": 1}\n\n: ping\n\n\
                    event: queue\ndata: {\"items\":\ndata: []}\n\ndata: plain\n\n";
        let lines: Vec<Line> = text.split_inclusive('\n').filter_map(|l| sse.feed(l)).collect();
        assert_eq!(lines, vec![
            Line::Retry(3000),
            Line::Event { name: "schedule".into(), data: "{\"now\": 1}".into() },
            Line::Comment,
            Line::Event { name: "queue".into(), data: "{\"items\":\n[]}".into() },
            Line::Event { name: "message".into(), data: "plain".into() },
        ]);
    }

    #[test]
    fn a_blank_line_without_data_sends_nothing_and_crlf_is_fine() {
        let mut sse = Sse::default();
        assert_eq!(sse.feed("event: status\r\n"), None);
        assert_eq!(sse.feed("\r\n"), None, "an event with no data is not an event");
        assert_eq!(sse.feed("data:{}\r\n"), None);
        assert_eq!(sse.feed("\r\n"), Some(Line::Event { name: "message".into(), data: "{}".into() }));
    }

    #[test]
    fn the_watchdog_times_out_a_request_that_never_answers() {
        let client = Client::new(9);
        let mut handle: Handle<&'static str> = client.handle();
        // Nothing is sent: stand in for a worker stuck on a dead socket.
        handle.inflight.insert(7, ("schedule", Instant::now(), Instant::now() + Duration::from_millis(5)));
        assert!(handle.busy(&"schedule"));
        assert!(handle.poll_at(Instant::now()).is_empty(), "gave up before the deadline");
        let done = handle.poll_at(Instant::now() + Duration::from_millis(10));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].tag, "schedule");
        assert!(done[0].outcome.as_ref().unwrap_err().timed_out);
        assert!(!handle.busy(&"schedule"), "the in-flight flag latched");
        // The answer turning up afterwards is dropped, not reported twice.
        handle.reply.send((7, Ok(Value::Null), Instant::now(), Instant::now())).unwrap();
        assert!(handle.poll().is_empty());
    }

    #[test]
    fn an_unreachable_station_answers_with_a_failure_not_a_hang() {
        // Port 9 (discard) is closed on any machine running these tests.
        let client = Client::new(9);
        let mut handle: Handle<u8> = client.handle();
        handle.get(1, "/api/status");
        let began = Instant::now();
        let mut done = Vec::new();
        while done.is_empty() && began.elapsed() < GET_TIMEOUT + GRACE + Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(20));
            done = handle.poll();
        }
        assert_eq!(done.len(), 1);
        assert!(done[0].outcome.is_err());
    }

    #[test]
    fn a_stream_quiet_past_its_pings_is_stale() {
        let (_, events) = channel();
        let fresh = Subscription {
            events, stop: Arc::new(AtomicBool::new(false)), connected: Arc::new(AtomicBool::new(true)),
            heard: Arc::new(AtomicU64::new(0)), born: Instant::now(),
        };
        assert!(!fresh.stale());
        let (_, events) = channel();
        let old = Instant::now() - STALE - Duration::from_secs(5);
        let quiet = Subscription {
            events, stop: Arc::new(AtomicBool::new(false)), connected: Arc::new(AtomicBool::new(true)),
            heard: Arc::new(AtomicU64::new(0)), born: old,
        };
        assert!(!quiet.live());
        assert!(quiet.stale(), "a connected stream nobody hears from was kept");
    }

    #[test]
    fn backoff_doubles_to_its_cap_and_resets() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(5));
        let waits: Vec<u64> = (0..5).map(|_| backoff.next().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 5, 5]);
        backoff.reset();
        assert_eq!(backoff.next().as_secs(), 1);
    }
}
