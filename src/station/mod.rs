//! The radio station, from the console's side.
//!
//! Side Room is a Python process: it picks records, writes what the hosts say,
//! renders their voices and schedules the whole thing on an hour clock. None
//! of that belongs in Rust, and none of it is going to be rewritten here.
//!
//! What this does is run it, watch it, and stop it. The console starts the
//! station as a child, listens to what it says about itself, and shows what
//! is on air.
//!
//! Playing it is `airtime`'s job, not this one's. This starts the station,
//! watches it and stops it; that reads the schedule it publishes and puts it
//! on the console's output.
//!
//! Two things are kept apart here that used to be one: whether the process is
//! alive, and whether its last answer arrived. A station busy writing a news
//! break can miss a poll; that makes it `Degraded`, which keeps the music
//! playing, not gone. Only the process exiting, or ten seconds of silence,
//! counts as not running -- and a station of ours that dies is restarted,
//! while the records on the decks play on.

pub mod client;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use client::{Backoff, Client, Handle, Subscription};

/// What the station says about itself.
#[derive(Clone, Debug, Default)]
pub struct OnAir {
    pub state: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub note: Option<String>,
    /// The last few things the hosts said, newest last.
    pub transcript: Vec<TranscriptLine>,
    pub mix_config: serde_json::Value,
    pub vibe: Option<String>,
    pub track_key: String,
    pub position: f64,
    pub duration: f64,
    pub ad_note: String,
    pub ad_busy: bool,
    pub ads_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct TranscriptLine {
    pub host: String,
    pub text: String,
    pub start_at: f64,
    pub active: bool,
    pub source: Option<String>,
    pub source_url: Option<String>,
}

pub enum Health {
    /// Not running, and we did not start it.
    Off,
    /// Started; waiting for it to answer.
    Starting,
    Live(Box<OnAir>),
    /// Up, but its last answers did not arrive: the last reading, and why.
    /// Still running -- the records keep playing through this.
    Degraded(Box<OnAir>, String),
    Failed(String),
}

/// What the station client asks the station.
#[derive(Clone, Debug, PartialEq)]
enum Ask {
    /// Is something already answering on the port?
    Probe,
    /// `/api/status`, whole or lite.
    Status { full: bool },
}

pub struct Station {
    root: PathBuf,
    port: u16,
    client: Client,
    asks: Handle<Ask>,
    child: Option<Child>,
    job: Option<crate::process::Job>,
    /// True when the process on the port is not ours, so we must not kill it.
    adopted: bool,
    /// Asked to start; waiting to hear whether one is already running.
    probing: bool,
    pub health: Health,
    logs: Receiver<String>,
    log_out: Sender<String>,
    events: Option<Subscription>,
    last_poll: Instant,
    /// When answers stopped arriving, if they have.
    silent_since: Option<Instant>,
    retry: Backoff,
    poll_every: Duration,
    pub log: Vec<String>,
    /// The mix settings as the station last gave them in full, and a count
    /// that moves whenever they arrive, so an open settings window can
    /// pick them up.
    pub mix_config: serde_json::Value,
    pub mix_generation: u64,
    want_full: bool,
    supervisor: Supervisor,
    /// Bumped once a crashed station of ours is answering again, so the
    /// console can hand it the decks it was playing.
    pub recoveries: u64,
    recovering: bool,
    /// A graceful stop, running on its own thread.
    stopper: Option<std::thread::JoinHandle<()>>,
    start_after_stop: bool,
}

/// Often enough to feel live, rarely enough that it is not a load.
const POLL: Duration = Duration::from_millis(1200);
/// With the events stream up, status arrives when it changes; this is only
/// a check that the stream is telling the truth.
const POLL_STREAMING: Duration = Duration::from_secs(10);
/// How long a station may go unheard before it counts as gone.
const SILENCE: Duration = Duration::from_secs(10);
/// A station still importing its library on first run can take a while.
const COLD_START: Duration = Duration::from_secs(120);

impl Station {
    pub fn new(root: &Path) -> Self {
        Self::with_client(root, Client::new(port_of(root)))
    }

    pub fn with_client(root: &Path, client: Client) -> Self {
        let (log_out, logs) = channel();
        let asks = client.handle();
        Station {
            root: root.to_path_buf(),
            port: port_of(root),
            client,
            asks,
            child: None,
            job: None,
            adopted: false,
            probing: false,
            health: Health::Off,
            logs,
            log_out,
            events: None,
            last_poll: Instant::now() - POLL,
            silent_since: None,
            retry: Backoff::new(POLL, Duration::from_secs(5)),
            poll_every: POLL,
            log: Vec::new(),
            mix_config: serde_json::Value::Null,
            mix_generation: 0,
            want_full: false,
            supervisor: Supervisor::default(),
            recoveries: 0,
            recovering: false,
            stopper: None,
            start_after_stop: false,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn running(&self) -> bool {
        !matches!(self.health, Health::Off | Health::Failed(_))
    }

    /// Answering right now, or answered recently enough to trust.
    pub fn ready(&self) -> bool {
        matches!(self.health, Health::Live(_) | Health::Degraded(..))
    }

    /// The last reading, live or not.
    pub fn status(&self) -> Option<&OnAir> {
        match &self.health {
            Health::Live(status) | Health::Degraded(status, _) => Some(status),
            _ => None,
        }
    }

    pub fn ours(&self) -> bool {
        self.child.is_some()
    }

    /// Fetch the whole status once, rather than the lite one: the mix
    /// settings ride on it, and nothing else needs them.
    pub fn want_full_status(&mut self) {
        self.want_full = true;
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.child.is_some() || self.adopted || self.probing {
            return Ok(());
        }
        crate::pull::python(&self.root)
            .ok_or("The station needs its Python environment, which is not set up here.")?;
        if self.stopper.as_ref().is_some_and(|s| !s.is_finished()) {
            // The last one is still putting itself away; start once it has.
            self.start_after_stop = true;
            self.health = Health::Starting;
            return Ok(());
        }
        // A station somebody else started is adopted rather than fought for
        // the port.
        self.probing = true;
        self.health = Health::Starting;
        self.asks.get(Ask::Probe, "/api/status?lite=1");
        Ok(())
    }

    fn spawn(&mut self) -> Result<(), String> {
        let python = crate::pull::python(&self.root)
            .ok_or("The station needs its Python environment, which is not set up here.")?;
        let mut command = crate::process::background(python);
        command
            .arg("-m")
            .arg("radio")
            .current_dir(&self.root)
            // Said outright rather than left to agree by both reading .env.
            .env("PORT", self.port.to_string())
            // The console runs the tunnel; the station must not start its own.
            .env("DEFALT_CONSOLE", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let (mut child, job) = crate::process::spawn_contained(&mut command)
            .map_err(|error| format!("could not start the station: {error}"))?;

        // Its own logging is worth keeping: it says what it is downloading and
        // which model wrote a break.
        for stream in [
            child.stdout.take().map(Streams::Out),
            child.stderr.take().map(Streams::Err),
        ]
        .into_iter()
        .flatten()
        {
            let sender = self.log_out.clone();
            std::thread::spawn(move || stream.pump(sender));
        }

        self.child = Some(child);
        self.job = job;
        self.adopted = false;
        self.silent_since = None;
        Ok(())
    }

    /// Stop it without waiting: ask it to shut down cleanly on a thread of
    /// its own, and let the panel carry on.
    pub fn stop(&mut self) {
        self.start_after_stop = false;
        self.probing = false;
        self.recovering = false;
        self.supervisor = Supervisor::default();
        self.events = None;
        self.asks.abandon(&Ask::Status { full: false });
        self.asks.abandon(&Ask::Status { full: true });
        if let Some(child) = self.child.take() {
            let (client, job) = (self.client.clone(), self.job.take());
            self.stopper = std::thread::Builder::new().name("station-stop".into())
                .spawn(move || shut_down(&client, child, job)).ok();
        }
        // Somebody else's station is theirs to stop.
        self.adopted = false;
        self.health = Health::Off;
    }

    /// Stop it and wait, for when the console itself is going.
    pub fn stop_blocking(&mut self) {
        self.stop();
        if let Some(stopper) = self.stopper.take() {
            let _ = stopper.join();
        }
    }

    pub fn tick(&mut self) {
        while let Ok(line) = self.logs.try_recv() {
            self.log.push(line);
            if self.log.len() > 200 {
                self.log.drain(..100);
            }
        }

        if self.start_after_stop && self.stopper.as_ref().is_none_or(|s| s.is_finished()) {
            self.start_after_stop = false;
            self.stopper = None;
            self.health = Health::Off;
            if let Err(error) = self.start() {
                self.health = Health::Failed(error);
            }
        }

        for done in self.asks.poll() {
            match (done.tag, done.outcome) {
                (Ask::Probe, outcome) => {
                    self.probing = false;
                    if matches!(self.health, Health::Off) {
                        continue; // Stopped while asking.
                    }
                    match outcome {
                        Ok(body) => {
                            crate::logfile::log!("station: adopting the one already on port {}", self.port);
                            self.adopted = true;
                            self.heard(on_air_from(&body), false);
                        }
                        Err(_) => {
                            if let Err(error) = self.spawn() {
                                self.health = Health::Failed(error);
                            }
                        }
                    }
                }
                (Ask::Status { full }, Ok(body)) => self.heard(on_air_from(&body), full),
                (Ask::Status { full }, Err(failure)) => {
                    if full {
                        self.want_full = true;
                    }
                    self.unheard(failure.message);
                }
            }
        }

        while let Some(pushed) = self.events.as_ref().and_then(|e| e.events.try_recv().ok()) {
            if pushed.topic == "status" {
                self.heard(on_air_from(&pushed.data), false);
            }
        }

        self.watch_child();
        self.silence_check();

        let alive = self.child.is_some() || self.adopted;
        if !alive {
            self.events = None;
            return;
        }
        if self.events.as_ref().is_some_and(|e| e.stale()) {
            self.events = None;
        }
        if self.events.is_none() && self.ready() {
            self.events = Some(self.client.subscribe(&["status"]));
        }
        let streaming = self.events.as_ref().is_some_and(|e| e.live());
        let every = if self.silent_since.is_some() {
            self.poll_every.max(POLL)
        } else if streaming {
            POLL_STREAMING
        } else {
            POLL
        };
        let full = self.want_full;
        let status = Ask::Status { full };
        if self.asks.busy(&Ask::Status { full: false }) || self.asks.busy(&Ask::Status { full: true }) {
            return;
        }
        if !full && self.last_poll.elapsed() < every {
            return;
        }
        self.last_poll = Instant::now();
        self.want_full = false;
        self.asks.get(status, if full { "/api/status" } else { "/api/status?lite=1" });
    }

    /// An answer arrived.
    fn heard(&mut self, mut status: OnAir, full: bool) {
        if full || !status.mix_config.is_null() {
            self.mix_config = status.mix_config.clone();
            self.mix_generation += 1;
        }
        // The lite status leaves the settings out; the panel still reads
        // them off the status.
        status.mix_config = self.mix_config.clone();
        self.silent_since = None;
        self.retry.reset();
        self.poll_every = POLL;
        if self.recovering {
            self.recovering = false;
            self.recoveries += 1;
            crate::logfile::log!("station: back up after a restart");
        }
        self.supervisor.healthy(Instant::now());
        self.health = Health::Live(Box::new(status));
    }

    /// An answer did not arrive.
    fn unheard(&mut self, why: String) {
        self.silent_since.get_or_insert_with(Instant::now);
        // Asked again less often while it is not answering.
        self.poll_every = self.retry.next();
        self.health = match std::mem::replace(&mut self.health, Health::Off) {
            // While it is still coming up, silence is expected.
            Health::Starting => Health::Starting,
            Health::Live(status) | Health::Degraded(status, _) => Health::Degraded(status, why),
            other => other,
        };
    }

    /// Ours, and exited: restart it, unless it keeps dying.
    fn watch_child(&mut self) {
        let exited = match self.child.as_mut().map(|child| child.try_wait()) {
            Some(Ok(Some(status))) => Some(status.code()),
            Some(Err(_)) => Some(None),
            _ => None,
        };
        let now = Instant::now();
        if let Some(code) = exited {
            self.child = None;
            self.job = None;
            self.events = None;
            crate::logfile::log!("station: exited ({})", code.map_or("killed".into(), |c| c.to_string()));
            match self.supervisor.exited(now) {
                Verdict::Restart(wait) => {
                    self.recovering = true;
                    self.silent_since = None;
                    let why = format!("the station stopped; restarting in {} s", wait.as_secs().max(1));
                    self.health = match std::mem::replace(&mut self.health, Health::Off) {
                        Health::Live(status) | Health::Degraded(status, _) => Health::Degraded(status, why),
                        _ => Health::Starting,
                    };
                }
                Verdict::GiveUp => {
                    self.recovering = false;
                    self.health = Health::Failed("the station keeps stopping; check logs/station.log".into());
                }
            }
        }
        if self.child.is_none() && !self.adopted && self.supervisor.due(now) {
            crate::logfile::log!("station: restarting");
            if let Err(error) = self.spawn() {
                self.recovering = false;
                self.health = Health::Failed(error);
            }
        }
    }

    /// Ten seconds without an answer is not running any more. Ours is
    /// killed and restarted; somebody else's is let go.
    fn silence_check(&mut self) {
        let Some(since) = self.silent_since else { return };
        let limit = if matches!(self.health, Health::Starting) { COLD_START } else { SILENCE };
        if since.elapsed() < limit {
            return;
        }
        self.silent_since = None;
        if let Some(mut child) = self.child.take() {
            crate::logfile::log!("station: not answering for {} s; restarting it", limit.as_secs());
            crate::process::kill(&mut child, self.job.as_ref());
            let _ = child.wait();
            self.job = None;
            self.events = None;
            match self.supervisor.exited(Instant::now()) {
                Verdict::Restart(_) => {
                    self.recovering = true;
                    self.health = match std::mem::replace(&mut self.health, Health::Off) {
                        Health::Live(status) | Health::Degraded(status, _) =>
                            Health::Degraded(status, "the station stopped answering; restarting".into()),
                        _ => Health::Starting,
                    };
                }
                Verdict::GiveUp => {
                    self.health = Health::Failed("the station stopped answering".into());
                }
            }
        } else if self.adopted {
            self.adopted = false;
            self.events = None;
            self.health = Health::Failed("the station stopped answering".into());
        }
    }
}

/// The graceful way down: ask, wait, and only then pull the plug.
fn shut_down(client: &Client, mut child: Child, job: Option<crate::process::Job>) {
    let asked = client.post_blocking("/api/shutdown", None, Duration::from_secs(3)).is_ok();
    let deadline = Instant::now() + Duration::from_secs(if asked { 5 } else { 1 });
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            // The launcher is gone; the job takes anything it left behind.
            if let Some(job) = job.as_ref() { job.terminate(); }
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    crate::logfile::log!("station: did not stop when asked; ending it");
    crate::process::kill(&mut child, job.as_ref());
    let _ = child.wait();
}

impl Drop for Station {
    fn drop(&mut self) {
        self.stop_blocking();
    }
}

/* ── Keeping it up ───────────────────────────────────────────────────── */

/// What to do about a station that exited.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Start it again after this long.
    Restart(Duration),
    /// It has died too often, too fast. Say so and stop trying.
    GiveUp,
}

/// Restarts with a backoff: one second, then two, four, eight, sixteen.
/// Five deaths without a minute of good health in between is a station
/// that is not going to stay up, and restarting it forever only hides that.
#[derive(Default)]
pub struct Supervisor {
    attempts: u32,
    next: Option<Instant>,
    well_since: Option<Instant>,
}

const RESTARTS: u32 = 5;
const HEALTHY_FOR: Duration = Duration::from_secs(60);

impl Supervisor {
    pub fn exited(&mut self, now: Instant) -> Verdict {
        if self.well_since.is_some_and(|since| now.duration_since(since) >= HEALTHY_FOR) {
            self.attempts = 0;
        }
        self.well_since = None;
        if self.attempts >= RESTARTS {
            self.next = None;
            return Verdict::GiveUp;
        }
        let wait = Duration::from_secs(1 << self.attempts);
        self.attempts += 1;
        self.next = Some(now + wait);
        Verdict::Restart(wait)
    }

    /// It answered.
    pub fn healthy(&mut self, now: Instant) {
        self.well_since.get_or_insert(now);
    }

    /// Time to start it again? Once: the answer is yes one time per exit.
    pub fn due(&mut self, now: Instant) -> bool {
        if self.next.is_some_and(|at| now >= at) {
            self.next = None;
            return true;
        }
        false
    }
}

enum Streams {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Streams {
    fn pump(self, sender: Sender<String>) {
        let reader: Box<dyn BufRead> = match self {
            Streams::Out(stream) => Box::new(BufReader::new(stream)),
            Streams::Err(stream) => Box::new(BufReader::new(stream)),
        };
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            crate::logfile::station_line(&line);
            if sender.send(line).is_err() {
                return;
            }
        }
    }
}

pub fn on_air_from(body: &serde_json::Value) -> OnAir {
    let playing = &body["now_playing"];
    let transcript = body["transcript"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| {
                    Some(TranscriptLine {
                        host: line["host"].as_str().or(line["speaker"].as_str())?.to_string(),
                        text: line["text"].as_str()?.to_string(),
                        start_at: line["start_at"].as_f64().unwrap_or(0.0),
                        active: line["active"].as_bool().unwrap_or(false),
                        source: line["reference"]["source"].as_str().map(str::to_string),
                        source_url: line["reference"]["url"].as_str()
                            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    OnAir {
        state: body["state"]
            .as_str()
            .or(body["note"].as_str())
            .unwrap_or("on air")
            .to_string(),
        title: playing["title"].as_str().map(str::to_string),
        artist: playing["artist"].as_str().map(str::to_string),
        note: body["note"].as_str().map(str::to_string),
        transcript,
        mix_config: body["mix_config"].clone(),
        vibe: body["vibe"]["description"].as_str().map(str::to_string),
        track_key: playing["key"].as_str().unwrap_or("").to_string(),
        position: playing["position"].as_f64().unwrap_or(0.0),
        duration: playing["duration"].as_f64().unwrap_or(0.0),
        ad_note: body["ad"]["message"].as_str().unwrap_or("").to_string(),
        ad_busy: body["ad"]["busy"].as_bool().unwrap_or(false),
        ads_enabled: body["ad"]["enabled"].as_bool().unwrap_or(true),
    }
}

/// The station reads PORT out of `.env`; we have to agree with it. It is
/// also handed PORT outright when this console starts it.
pub fn port_of(root: &Path) -> u16 {
    std::fs::read_to_string(root.join(".env"))
        .ok()
        .and_then(|text| {
            text.lines()
                .filter_map(|line| line.trim().split_once('='))
                .find(|(name, _)| name.trim() == "PORT")
                .and_then(|(_, value)| value.trim().parse().ok())
        })
        .unwrap_or(8090)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_comes_from_the_projects_env() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Whatever it is, it must match what the station will bind.
        let found = port_of(&root);
        let text = std::fs::read_to_string(root.join(".env")).unwrap_or_default();
        let expected: u16 = text
            .lines()
            .filter_map(|l| l.trim().split_once('='))
            .find(|(n, _)| n.trim() == "PORT")
            .and_then(|(_, v)| v.trim().parse().ok())
            .unwrap_or(8090);
        assert_eq!(found, expected);
    }

    #[test]
    fn a_missing_env_falls_back_rather_than_failing() {
        assert_eq!(port_of(&std::env::temp_dir().join("defalt-nothing-here")), 8090);
    }

    #[test]
    fn a_station_starts_off_and_is_not_ours() {
        let station = Station::new(&std::env::temp_dir());
        assert!(!station.running());
        assert!(!station.ours());
    }

    fn live() -> Station {
        let mut station = Station::new(&std::env::temp_dir().join("defalt-no-fixture"));
        station.adopted = true;
        station.heard(OnAir { title: Some("On air".into()), ..OnAir::default() }, false);
        station
    }

    #[test]
    fn one_missed_answer_degrades_the_station_but_keeps_it_running() {
        let mut station = live();
        station.unheard("the station did not answer in time".into());
        assert!(matches!(station.health, Health::Degraded(..)), "one missed poll took it off air");
        assert!(station.running(), "a degraded station must keep playing");
        assert_eq!(station.status().and_then(|s| s.title.as_deref()), Some("On air"),
                   "the last reading was thrown away");
        station.heard(OnAir::default(), false);
        assert!(matches!(station.health, Health::Live(_)));
    }

    #[test]
    fn ten_seconds_of_silence_is_not_running_any_more() {
        let mut station = live();
        station.unheard("nothing".into());
        station.silence_check();
        assert!(station.running(), "gave up at once");
        station.silent_since = Some(Instant::now() - SILENCE - Duration::from_millis(1));
        station.silence_check();
        assert!(!station.running(), "a station silent for ten seconds still counted as up");
        assert!(matches!(station.health, Health::Failed(_)));
    }

    #[test]
    fn a_lite_status_keeps_the_last_full_mix_settings() {
        let mut station = live();
        let full = on_air_from(&serde_json::json!({"mix_config": {"fields": [1]}}));
        station.heard(full, true);
        let generation = station.mix_generation;
        station.heard(on_air_from(&serde_json::json!({"state": "on air"})), false);
        assert_eq!(station.mix_generation, generation, "a lite answer counted as new settings");
        assert_eq!(station.status().unwrap().mix_config["fields"][0], 1);
    }

    #[test]
    fn a_crashing_station_is_restarted_with_a_backoff_then_given_up_on() {
        let start = Instant::now();
        let mut supervisor = Supervisor::default();
        let mut at = start;
        let mut waits = Vec::new();
        for _ in 0..RESTARTS {
            match supervisor.exited(at) {
                Verdict::Restart(wait) => {
                    assert!(!supervisor.due(at), "restarted without waiting");
                    at += wait;
                    assert!(supervisor.due(at));
                    assert!(!supervisor.due(at), "one exit restarted it twice");
                    waits.push(wait.as_secs());
                }
                Verdict::GiveUp => panic!("gave up early"),
            }
        }
        assert_eq!(waits, [1, 2, 4, 8, 16]);
        assert_eq!(supervisor.exited(at), Verdict::GiveUp);
    }

    #[test]
    fn a_minute_of_good_health_forgives_old_crashes() {
        let start = Instant::now();
        let mut supervisor = Supervisor::default();
        for _ in 0..3 { let _ = supervisor.exited(start); }
        supervisor.healthy(start);
        let later = start + HEALTHY_FOR + Duration::from_secs(1);
        assert_eq!(supervisor.exited(later), Verdict::Restart(Duration::from_secs(1)));
    }

    #[test]
    fn a_restarted_station_that_answers_counts_as_recovered_once() {
        let mut station = live();
        station.recovering = true;
        station.heard(OnAir::default(), false);
        station.heard(OnAir::default(), false);
        assert_eq!(station.recoveries, 1);
    }
}
