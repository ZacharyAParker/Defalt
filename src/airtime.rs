//! Side Room, played on the console's own decks.
//!
//! The first version of this was a bank of channels doing its own mixing past
//! the decks, past the crossfader, past the EQ. It played, and nothing moved
//! on the panel while it did, and you could not reach into a transition and
//! take it. That is the browser player reimplemented behind the UI, which is
//! not what a DJ console is for.
//!
//! This drives the desk instead. A scheduled record is loaded onto a deck and
//! played from it. The transition the station worked out -- a bass swap under
//! a filter rise, six seconds of overlap -- is performed by moving the actual
//! crossfader, the actual EQ knobs and the actual filter fader. The panel
//! shows it because the panel is reading the same numbers autopilot is
//! writing, not a copy of them.
//!
//! Which means you can take any of it. Touch a control and that control stops
//! being automated and stays yours; everything else carries on. That is the
//! whole point of doing it on the desk rather than beside it.
//!
//! Speech is the one thing that cannot go on a deck, because a deck holds a
//! record. Host lines go to a small voice bus with its own level, and the
//! decks duck underneath them -- which is a dip written into the station's own
//! gain envelope, so it is honoured rather than invented here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::engine::decode::Track;
use crate::engine::playout::{Envelope, Item};
use crate::engine::Command;
use crate::library::Record;
use crate::DECKS;

/// How far ahead a voice line is fetched. Lines are seconds long and there
/// are a lot of them, so there is nothing to gain by reaching further.
///
/// Records have no equivalent window on purpose: the next one goes on the
/// spare deck the moment there is a spare deck, which is what a spare deck is
/// for. A window here used to mean a record began decoding twenty seconds
/// before it aired, and a skip -- which winds the clock forward to just before
/// the next transition -- could land inside those twenty seconds. The
/// transition then started against a deck with nothing on it.
const VOICE_LEAD_IN: f64 = 8.0;

const POLL: Duration = Duration::from_millis(1500);

/// The queue changes when you change it or when a request lands, neither of
/// which is often. No reason to ask as hard as for the schedule.
const QUEUE_POLL: Duration = Duration::from_millis(3000);

/* ── Curves ──────────────────────────────────────────────────────────── */

/// A breakpoint curve of (seconds into the item, value), linear between and
/// flat outside. The station publishes its automation in these.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curve(Vec<[f32; 2]>);

impl Curve {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value at `t`, or None if there is no curve to read.
    pub fn at(&self, t: f32) -> Option<f32> {
        let points = &self.0;
        let last = points.len().checked_sub(1)?;
        if t <= points[0][0] {
            return Some(points[0][1]);
        }
        if t >= points[last][0] {
            return Some(points[last][1]);
        }
        for pair in points.windows(2) {
            let ([t0, v0], [t1, v1]) = (pair[0], pair[1]);
            if t0 <= t && t <= t1 {
                if t1 <= t0 {
                    return Some(v1);
                }
                return Some(v0 + (v1 - v0) * ((t - t0) / (t1 - t0)));
            }
        }
        Some(points[last][1])
    }
}

/// What the station wants done to a record's tone while it plays.
///
/// Bands are in decibels and the filters in hertz, because that is what the
/// station reasons in. Turning them into knob positions is this module's job
/// and nobody else's -- see `band_knob` and `filter_fader`.
#[derive(Clone, Debug, Default)]
pub struct Automation {
    pub low: Curve,
    pub mid: Curve,
    pub high: Curve,
    pub lpf: Curve,
    pub hpf: Curve,
}

impl Automation {
    pub fn is_empty(&self) -> bool {
        self.low.is_empty()
            && self.mid.is_empty()
            && self.high.is_empty()
            && self.lpf.is_empty()
            && self.hpf.is_empty()
    }
}

/// A knob position that produces `db` of gain, inverting the strip's taper.
///
/// The taper is deliberately not linear -- it gives the bottom half of the
/// knob thirty decibels so a band can be swapped out convincingly -- so this
/// has to invert the real curve rather than a straight line, or a bass swap
/// would land in the wrong place.
pub fn band_knob(db: f32) -> f32 {
    const MAX_DB: f32 = 6.0;
    if db >= 0.0 {
        return (0.5 + db / MAX_DB * 0.5).clamp(0.5, 1.0);
    }
    if db <= -30.0 {
        return 0.0;
    }
    let t = (-db / 30.0).sqrt();
    (0.5 * (1.0 - t)).clamp(0.0, 0.5)
}

/// A filter fader position that puts the sweep at `hz`.
///
/// Negative is a low-pass coming down from the top, positive a high-pass going
/// up from the bottom, which is how the strip reads it. The low-pass curve has
/// no closed-form inverse worth writing, so it is bisected -- twenty-odd steps
/// at frame rate, which is nothing.
pub fn filter_fader(hz: f32, low_pass: bool) -> f32 {
    if low_pass {
        let target = hz.clamp(60.0, 20_000.0);
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let t = 0.5 * (lo + hi);
            let freq = 20_000.0 * (1.0 - t).powf(2.4) + 120.0 * t;
            if freq > target {
                lo = t;
            } else {
                hi = t;
            }
        }
        -(0.5 * (lo + hi))
    } else {
        let target = hz.clamp(20.0, 12_000.0);
        (((target - 20.0) / 9_000.0).max(0.0)).powf(1.0 / 2.4).clamp(0.0, 1.0)
    }
}

/* ── The schedule ────────────────────────────────────────────────────── */

#[derive(Clone, Debug, Default)]
pub struct Transition {
    pub preset: String,
    pub reason: String,
    pub overlap: f64,
}

/// A planned overlap in seconds of the source file, not station-clock time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixWindow {
    pub start: f64,
    pub end: f64,
    pub incoming: bool,
}

#[derive(Clone, Debug)]
pub struct Scheduled {
    pub id: String,
    pub kind: String,
    pub url: String,
    pub start_at: f64,
    pub duration: f64,
    pub offset: f64,
    pub preferred_deck: Option<usize>,
    /// Per-item gain, which outside a transition is the duck under a voice.
    pub envelope: Vec<[f32; 2]>,
    pub deck_envelope: Vec<[f32; 2]>,
    pub ducking: [f32; 4],
    pub playback_rate: f64,
    pub rate_curve: Vec<[f64; 2]>,
    pub key_lock: bool,
    /// Enabled, maximum fractional speed adjustment, position tolerance in seconds.
    pub playback_feedback: [f64; 3],
    pub echo: Option<[f32; 5]>, // start, end, delay seconds, mix, feedback
    pub automation: Automation,
    pub transition: Option<Transition>,
    pub title: String,
    pub artist: String,
    pub bpm: Option<f64>,
    pub camelot: Option<String>,
    pub key: String,
}

impl Scheduled {
    pub fn rate_at(&self, elapsed: f64) -> f64 {
        let mut previous = [0.0, self.playback_rate];
        for point in &self.rate_curve {
            if elapsed <= point[0] {
                let span = point[0] - previous[0];
                return if span <= 0.0 { point[1] } else {
                    previous[1] + (point[1] - previous[1]) * ((elapsed - previous[0]) / span).clamp(0.0, 1.0)
                };
            }
            previous = *point;
        }
        previous[1]
    }

    pub fn source_at(&self, elapsed: f64) -> f64 {
        let elapsed = elapsed.max(0.0);
        let mut previous = [0.0, self.playback_rate];
        let mut consumed = 0.0;
        for point in &self.rate_curve {
            let until = elapsed.min(point[0]);
            let span = until - previous[0];
            if span > 0.0 {
                let end_rate = if point[0] <= previous[0] { point[1] } else {
                    previous[1] + (point[1] - previous[1]) * span / (point[0] - previous[0])
                };
                consumed += span * (previous[1] + end_rate) * 0.5;
            }
            if elapsed <= point[0] { return self.offset + consumed; }
            previous = *point;
        }
        self.offset + consumed + (elapsed - previous[0]).max(0.0) * previous[1]
    }

    pub fn is_music(&self) -> bool {
        self.kind == "music"
    }

    pub fn ends_at(&self) -> f64 {
        self.start_at + self.duration
    }

    /// A `Record` standing in for this item, so it loads through exactly the
    /// same path a record you picked yourself does.
    fn record(&self, file: PathBuf) -> Record {
        Record {
            key: if self.key.is_empty() { self.id.clone() } else { self.key.clone() },
            artist: self.artist.clone(),
            title: self.title.clone(),
            album: None,
            duration: Some(self.duration),
            bpm: self.bpm,
            camelot: self.camelot.clone(),
            lufs: None,
            file,
            beat_offset: None,
            beat_period: None,
            downbeat_offset: None,
        }
    }
}

/// A row of the station's queue.
///
/// Three stages, and they are not the same thing. `on_deck` is already on the
/// clock with a real air time, so it can be dropped but not reordered -- every
/// transition after it was worked out against where it sits. `queued` is
/// downloaded and waiting, and moves freely. `finding` is a request still being
/// resolved, which can only be called off.
#[derive(Clone, Debug, Default)]
pub struct QueueRow {
    pub id: String,
    pub stage: String,
    pub playing: bool,
    /// Seconds until it airs, where that is known.
    pub eta: Option<f64>,
    pub artist: String,
    pub title: String,
    pub bpm: Option<f64>,
    pub camelot: Option<String>,
    pub can_move: bool,
    pub can_remove: bool,
    pub selection_reason: String,
}

impl QueueRow {
    pub fn label(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", self.artist, self.title)
        }
    }
}

/// What a deck looks like from the outside, so autopilot can tell a deck it
/// is allowed to use from one you are using.
#[derive(Clone, Copy, Default)]
pub struct DeckStatus {
    pub loaded: bool,
    pub playing: bool,
    /// Audio-thread telemetry, in source seconds. None disables timing feedback.
    pub position: Option<f64>,
    pub playback_rate: f64,
}

pub struct Snapshot {
    pub now: f64,
    pub epoch: i64,
    pub items: Vec<Scheduled>,
    pub frame: u64,
}

/* ── What autopilot wants done ───────────────────────────────────────── */

/// One turn's worth of intent.
///
/// Plain data on purpose: it is what makes this testable without an audio
/// device, and it is where "the user has this one" is enforced -- a held
/// control is simply absent from the plan.
#[derive(Default)]
pub struct Plan {
    pub duck: Option<f32>,
    pub speed: [Option<f64>; DECKS],
    pub key_lock: [Option<bool>; DECKS],
    pub echo: [Option<[f32; 3]>; DECKS],
    pub load: Vec<(usize, Record)>,
    /// Deck, and where in the record to start it.
    pub start: Vec<(usize, f64)>,
    pub stop: Vec<usize>,
    pub tone: [Option<[f32; 4]>; DECKS],
    pub gain: [Option<f32>; DECKS],
    pub crossfade: Option<f32>,
    pub voice: Vec<Command>,
    pub note: Option<String>,
}

/// Which controls the user has taken off autopilot.
#[derive(Clone, Copy, Default)]
pub struct Held {
    /// low, mid, high, sweep -- per deck, per control.
    pub tone: [[bool; 4]; DECKS],
    pub gain: [bool; DECKS],
    pub tempo: [bool; DECKS],
    pub key_lock: [bool; DECKS],
    pub crossfade: bool,
}

impl Held {
    pub fn any(&self) -> bool {
        self.crossfade
            || self.tempo.iter().any(|held| *held)
            || self.key_lock.iter().any(|held| *held)
            || self.gain.iter().any(|g| *g)
            || self.tone.iter().flatten().any(|t| *t)
    }

    pub fn release(&mut self) {
        *self = Held::default();
    }
}

/* ── The module itself ───────────────────────────────────────────────── */

struct Assigned {
    id: String,
    /// True once the deck has been told to play it.
    started: bool,
    ready: bool,
}

/// Voice lines have to be decoded here, because the voice bus takes a track
/// rather than a path. Records do not: a deck loads its own.
enum Fetched {
    Voice { id: String, track: Arc<Track> },
    Failed { id: String, error: String },
}

pub struct Airtime {
    root: PathBuf,
    port: u16,
    sample_rate: u32,

    pub on: bool,
    pub held: Held,
    pub note: Option<String>,

    epoch: i64,
    session: u64,
    startup: Option<serde_json::Value>,
    skip_pending: Option<String>,
    failed: HashSet<String>,
    anchor: Option<(f64, u64)>,
    /// Which scheduled item each deck is carrying.
    decks: [Option<Assigned>; DECKS],
    started_at: [Option<f64>; DECKS],
    /// Voice items placed on the speech bus, and the channel they took.
    voices: HashMap<String, usize>,
    free_voice: Vec<usize>,
    pending: HashMap<String, ()>,

    polls: Receiver<(u64, Result<Snapshot, String>, bool)>,
    poll_out: Sender<(u64, Result<Snapshot, String>, bool)>,
    fetches: Receiver<Fetched>,
    fetch_out: Sender<Fetched>,

    last_poll: Instant,
    inflight: bool,

    pub schedule: Vec<Scheduled>,
    pub station_now: f64,

    /// The station's queue, as it last answered.
    pub queue: Vec<QueueRow>,
    queue_in: Receiver<Result<Vec<QueueRow>, String>>,
    queue_out: Sender<Result<Vec<QueueRow>, String>>,
    queue_inflight: bool,
    last_queue: Instant,
    /// Complaints from one-shot actions -- skip, a thumb, a queue move. Kept
    /// off the schedule channel, which owns the in-flight flag: an action
    /// failing must not tell the poller its own request came back.
    gripes: Receiver<String>,
    gripe_out: Sender<String>,
    /// What you have typed into the request box.
    pub request: String,
    pub request_is_vibe: bool,
    pub catalogue: crate::spotify::Search,
    pub request_selection: Option<crate::spotify::Suggestion>,
}

/// The voice bus is small: hosts talk one at a time, and two is enough for one
/// line to tail out under the next.
const VOICE_CHANNELS: usize = 3;

impl Airtime {
    pub fn new(root: &Path, port: u16, sample_rate: u32) -> Self {
        let (poll_out, polls) = channel();
        let (fetch_out, fetches) = channel();
        let (queue_out, queue_in) = channel();
        let (gripe_out, gripes) = channel();
        Airtime {
            root: root.to_path_buf(),
            port,
            sample_rate,
            on: false,
            held: Held::default(),
            note: None,
            epoch: i64::MIN,
            session: 0,
            startup: None,
            skip_pending: None,
            failed: HashSet::new(),
            anchor: None,
            decks: [const { None }; DECKS],
            started_at: [None; DECKS],
            voices: HashMap::new(),
            free_voice: (0..VOICE_CHANNELS).collect(),
            pending: HashMap::new(),
            polls,
            poll_out,
            fetches,
            fetch_out,
            last_poll: Instant::now() - POLL,
            inflight: false,
            schedule: Vec::new(),
            station_now: 0.0,
            queue: Vec::new(),
            queue_in,
            queue_out,
            queue_inflight: false,
            last_queue: Instant::now() - QUEUE_POLL,
            gripes,
            gripe_out,
            request: String::new(),
            request_is_vibe: false,
            catalogue: crate::spotify::Search::new(root),
            request_selection: None,
        }
    }

    /// True when the station has something on a deck.
    pub fn live(&self) -> bool {
        self.decks.iter().any(|d| d.is_some())
    }

    /// What this deck is carrying, for the panel to label.
    pub fn on_deck(&self, deck: usize) -> Option<&Scheduled> {
        let assigned = self.decks.get(deck)?.as_ref()?;
        self.schedule.iter().find(|item| item.id == assigned.id)
    }

    pub fn transition_windows(&self, deck: usize) -> Vec<MixWindow> {
        let Some(item) = self.on_deck(deck) else { return Vec::new() };
        let mut music: Vec<_> = self.schedule.iter().filter(|i| i.is_music()).collect();
        music.sort_by(|a, b| a.start_at.total_cmp(&b.start_at));
        music.windows(2).filter_map(|pair| {
            let (outgoing, incoming) = (pair[0], pair[1]);
            if item.id != outgoing.id && item.id != incoming.id { return None; }
            if incoming.transition.as_ref()?.overlap <= 0.0 { return None; }
            let start = incoming.start_at;
            let end = outgoing.ends_at().min(incoming.ends_at());
            if end <= start { return None; }
            Some(MixWindow {
                start: item.source_at(start - item.start_at),
                end: item.source_at(end - item.start_at),
                incoming: item.id == incoming.id,
            })
        }).collect()
    }

    /// Offline screenshot fixture. It never enables radio, fetches or playback.
    pub fn pose_transition_pair(&mut self, lengths: [f64; DECKS]) {
        let start = (lengths[0] - 8.0).max(0.0);
        let body = serde_json::json!({"now": (start - 4.0).max(0.0), "items": [
            {"id": "pose-a", "kind": "music", "url": "/a", "start_at": 0,
             "duration": lengths[0], "meta": {"deck": 0}},
            {"id": "pose-b", "kind": "music", "url": "/b", "start_at": start,
             "duration": lengths[1], "meta": {"deck": 1,
                 "transition": {"preset": "blend", "overlap": 8.0}}}
        ]});
        let snapshot = snapshot_from(&body, 0);
        self.station_now = snapshot.now;
        self.schedule = snapshot.items;
        for deck in 0..DECKS {
            self.decks[deck] = self.schedule.get(deck).map(|item| Assigned {
                id: item.id.clone(), started: deck == 0, ready: true,
            });
        }
    }

    /// The next talk break due -- news, a station ID, the hosts riffing.
    ///
    /// Worth surfacing on its own: breaks are the thing that makes this a
    /// station rather than a playlist, and they are invisible in the queue,
    /// which only lists music.
    pub fn next_break(&self) -> Option<&Scheduled> {
        self.schedule
            .iter()
            .filter(|item| !item.is_music() && item.ends_at() > self.station_now)
            .min_by(|a, b| a.start_at.total_cmp(&b.start_at))
    }

    /// The next record due, including one already cued on the spare deck.
    pub fn coming_up(&self) -> Option<&Scheduled> {
        self.schedule
            .iter()
            .filter(|item| item.is_music() && item.start_at > self.station_now)
            .min_by(|a, b| a.start_at.total_cmp(&b.start_at))
    }

    /// What is sounding now, for the things that act on it.
    pub fn current(&self) -> Option<&Scheduled> {
        let now = self.station_now;
        (0..DECKS)
            .filter_map(|deck| self.on_deck(deck))
            .filter(|item| item.start_at <= now && now < item.ends_at())
            .max_by(|a, b| a.start_at.total_cmp(&b.start_at))
    }

    /// Skip to just before the next transition.
    ///
    /// Not a hard cut: the station winds its clock forward to a moment before
    /// the crossfade begins, so what you hear is the mix you would have heard
    /// anyway, only sooner -- the blend, the bass swap, and any link the hosts
    /// wrote over it. Cutting the record dead would throw all of that away.
    pub fn skip(&mut self) {
        let Some(current) = self.current() else {
            self.note = Some("Nothing on air to skip.".into());
            return;
        };
        self.skip_pending = Some(current.id.clone());
        self.note = Some("Preparing the next transition...".into());
    }

    /// Wait for the actual spare deck's decode, not just a URL in the queue.
    fn take_ready_skip(&mut self) -> bool {
        let Some(origin) = self.skip_pending.as_ref() else { return false };
        let current = self.current();
        if current.is_none_or(|item| &item.id != origin) || self.overlap(self.station_now).is_some() {
            self.skip_pending = None;
            self.note = Some("The handoff is already underway.".into());
            return false;
        }
        let Some(next) = self.coming_up() else { return false };
        let ready = self.decks.iter().flatten().any(|d| d.id == next.id && d.ready);
        if ready {
            self.skip_pending = None;
            self.note = Some("Skipping to the transition lead-in.".into());
        }
        ready
    }

    /// Thumbs. The station keeps the score; a thumbs down also buys the record
    /// some distance, so it is not queued again the same night.
    pub fn rate(&mut self, up: bool) {
        let Some(item) = self.current() else {
            self.note = Some("Nothing on air to rate.".into());
            return;
        };
        if item.key.is_empty() {
            self.note = Some("The station does not have a key for that one.".into());
            return;
        }
        let body = serde_json::json!({
            "key": item.key,
            "value": if up { "up" } else { "down" },
        });
        let title = item.title.clone();
        self.post("/api/rate", Some(body));
        self.note = Some(format!(
            "{} {title}.",
            if up { "Noted, more like" } else { "Noted, less like" }
        ));
    }

    fn poll_queue(&mut self) {
        if self.queue_inflight || self.last_queue.elapsed() < QUEUE_POLL {
            return;
        }
        self.last_queue = Instant::now();
        self.queue_inflight = true;

        let url = format!("http://127.0.0.1:{}/api/queue", self.port);
        let sender = self.queue_out.clone();
        std::thread::spawn(move || {
            let _ = sender.send(fetch_queue(&url));
        });
    }

    /// `next`, `up`, `down`, `last` or `remove`, on one queue entry.
    pub fn queue_action(&mut self, id: &str, action: &str) {
        let path = format!("/api/queue/{id}/{action}");
        self.post(&path, None);
        self.last_queue = Instant::now() - QUEUE_POLL;
    }

    /// Empty the waiting queue. Requests are kept: you asked for those.
    pub fn queue_clear(&mut self) {
        self.post("/api/queue/clear", Some(serde_json::json!({"keep_requests": true})));
        self.last_queue = Instant::now() - QUEUE_POLL;
    }

    /// The one text box.
    ///
    /// It takes more than a song. A genre, a topic to cover on the next break,
    /// "do the news", or "play less niko b" -- the station works out which of
    /// those it is, so this does not have to.
    pub fn submit_request(&mut self) {
        let query = self.request.trim().to_string();
        if query.is_empty() {
            return;
        }
        let mode = if self.request_is_vibe { "vibe" } else { "request" };
        let selection = self.request_selection.take().filter(|s| !self.request_is_vibe && s.query() == query);
        self.request.clear();
        self.catalogue.clear();
        self.post("/api/request", Some(serde_json::json!({ "query": query, "mode": mode,
                   "selection": selection.map(|s| s.payload()) })));
        self.note = Some(format!("{}: {query}", if self.request_is_vibe { "Setting vibe" } else { "Asked for" }));
        self.last_queue = Instant::now() - QUEUE_POLL;
    }

    /// Fire a POST at the station and say so if it refuses.
    pub fn clear_vibe(&mut self) {
        self.post("/api/vibe/clear", None);
    }

    /// Fire a POST at the station and say so if it refuses.
    ///
    /// Off-thread and without waiting: none of these answer with anything the
    /// console needs, because the next schedule says what actually happened.
    fn post(&mut self, path: &str, body: Option<serde_json::Value>) {
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let sender = self.gripe_out.clone();
        std::thread::spawn(move || {
            let sent = match body {
                Some(body) => ureq::post(&url).send_json(&body),
                None => ureq::post(&url).send_empty(),
            };
            // A refusal carries the station's own reason, which is far more
            // use than "that did not work" -- "too late, that one is already
            // playing" tells you what to do instead.
            let complaint = match sent {
                Ok(_) => return,
                Err(ureq::Error::StatusCode(code)) => {
                    format!("the station said no ({code})")
                }
                Err(_) => "the station is not answering".to_string(),
            };
            let _ = sender.send(complaint);
        });
        // Ask again promptly rather than sitting on a schedule that is now a
        // second and a half stale.
        self.last_poll = Instant::now() - POLL;
    }

    pub fn set_on(&mut self, on: bool) -> bool {
        if self.on == on {
            return false;
        }
        self.on = on;
        self.session = self.session.wrapping_add(1);
        self.skip_pending = None;
        self.inflight = false;
        self.last_poll = Instant::now() - POLL;
        self.anchor = None;
        self.schedule.clear();
        self.failed.clear();
        if !on {
            self.startup = None;
            self.forget();
        }
        true
    }

    /// Start one radio session, keeping the supplied decks as its opening tracks.
    pub fn start_with(&mut self, tracks: serde_json::Value) {
        self.set_on(false);
        self.set_on(true);
        let token = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_nanos().to_string();
        self.startup = Some(serde_json::json!({"session": token, "tracks": tracks}));
    }

    pub fn deck_ready(&mut self, deck: usize) {
        if let Some(assigned) = self.decks[deck].as_mut() { assigned.ready = true; }
    }

    pub fn deck_failed(&mut self, deck: usize) {
        if let Some(assigned) = self.decks[deck].take() {
            let due = self.schedule.iter().any(|i| i.id == assigned.id && i.start_at <= self.station_now);
            self.failed.insert(assigned.id.clone());
            if due { self.skip(); } else { self.queue_action(&assigned.id, "remove"); }
        }
    }

    fn forget(&mut self) {
        self.decks = [const { None }; DECKS];
        self.started_at = [None; DECKS];
        self.voices.clear();
        self.free_voice = (0..VOICE_CHANNELS).collect();
        self.pending.clear();
        self.held.release();
    }

    /// Work out what should happen this frame.
    ///
    /// Deck telemetry allows small clock errors to be corrected without seeking;
    /// manual tempo controls always take priority.
    pub fn tick(
        &mut self,
        frame: u64,
        records: &[Record],
        decks: [DeckStatus; DECKS],
        station_running: bool,
    ) -> Plan {
        let mut plan = Plan::default();

        while let Ok(message) = self.fetches.try_recv() {
            match message {
                Fetched::Voice { id, track } => {
                    self.pending.remove(&id);
                    if self.on {
                        self.air_voice(&id, track, frame, &mut plan);
                    }
                }
                Fetched::Failed { id, error } => {
                    self.pending.remove(&id);
                    self.note = Some(error);
                }
            }
        }

        while let Ok(complaint) = self.gripes.try_recv() {
            self.note = Some(complaint);
        }

        while let Ok(message) = self.queue_in.try_recv() {
            self.queue_inflight = false;
            match message {
                Ok(rows) => self.queue = rows,
                Err(error) => self.note = Some(error),
            }
        }

        while let Ok((session, message, rejected)) = self.polls.try_recv() {
            if session != self.session { continue; }
            self.inflight = false;
            match message {
                Ok(snapshot) => {
                    self.startup = None;
                    self.absorb(snapshot, &mut plan);
                },
                Err(error) => {
                    if self.on {
                        self.note = Some(error);
                        if rejected {
                            self.set_on(false);
                            plan.gain = [Some(1.0); DECKS];
                        }
                    }
                }
            }
        }

        if self.on && !station_running {
            plan.duck = Some(1.0);
            plan.stop.extend((0..DECKS).filter(|d| self.decks[*d].is_some()));
            plan.voice.push(Command::OffAir);
            plan.gain = [Some(1.0); DECKS];
            self.set_on(false);
        }
        if self.on {
            if let Some((now, at)) = self.anchor {
                self.station_now = now + frame.saturating_sub(at) as f64 / self.sample_rate.max(1) as f64;
            }
            // Let go of a deck the moment its record is over. Waiting for the
            // next poll to notice costs a second and a half of the window the
            // following record has to load in.
            let now = self.station_now;
            for deck in 0..DECKS {
                let over = self.on_deck(deck).is_some_and(|item| item.ends_at() <= now);
                if over {
                    self.decks[deck] = None;
                    plan.stop.push(deck);
                    self.held.tone[deck] = [false; 4];
                    self.held.gain[deck] = false;
                    self.held.tempo[deck] = false;
                    self.held.key_lock[deck] = false;
                    self.started_at[deck] = None;
                }
            }
            if self.startup.is_none() {
                self.assign(records, decks, &mut plan);
                self.drive(decks, &mut plan);
                if self.take_ready_skip() {
                    self.post("/api/skip", None);
                }
            }
            self.poll(frame);
        }
        // Worth watching whenever the station is up, whether or not this
        // console is the thing playing it -- but never when it is not, or the
        // panel fills with complaints about a station nobody started.
        if station_running {
            self.poll_queue();
        } else if !self.queue.is_empty() {
            self.queue.clear();
        }

        if let Some(note) = self.note.take() {
            plan.note = Some(note);
        }
        plan
    }

    fn poll(&mut self, frame: u64) {
        if self.inflight || self.last_poll.elapsed() < POLL {
            return;
        }
        self.last_poll = Instant::now();
        self.inflight = true;

        let url = format!("http://127.0.0.1:{}", self.port);
        let startup = self.startup.clone();
        let sender = self.poll_out.clone();
        let session = self.session;
        let rate = self.sample_rate;
        std::thread::spawn(move || {
            let began = Instant::now();
            let mut rejected = false;
            let result = if let Some(body) = startup {
                ureq::post(&format!("{url}/api/decks/start"))
                    .config().http_status_as_error(false)
                    .timeout_global(Some(Duration::from_secs(8))).build().send_json(&body)
                    .map_err(|e| format!("Could not start deck playback: {e}"))
                    .and_then(|mut response| {
                        let status = response.status();
                        rejected = status.is_client_error();
                        let body = response.body_mut().read_json::<serde_json::Value>()
                            .map_err(|e| e.to_string())?;
                        if !status.is_success() {
                            return Err(format!("Could not start deck playback: {}",
                                body["error"].as_str().unwrap_or("restart the station and try again")));
                        }
                        Ok(snapshot_from(&body, frame))
                    })
            } else { fetch(&format!("{url}/api/schedule"), frame) };
            // The server timestamp belongs to the response, not the request's
            // old audio frame. Account for time spent waiting for the server.
            let result = result.map(|mut snapshot| {
                snapshot.frame = frame + (began.elapsed().as_secs_f64() * rate as f64) as u64;
                snapshot
            });
            let _ = sender.send((session, result, rejected));
        });
    }

    fn absorb(&mut self, snapshot: Snapshot, plan: &mut Plan) {
        self.station_now = snapshot.now;

        let jumped = snapshot.epoch != self.epoch;
        let first = self.epoch == i64::MIN;
        self.epoch = snapshot.epoch;
        self.anchor = Some((snapshot.now, snapshot.frame));
        self.schedule = snapshot.items;

        // A skip moves the station clock. The records on the decks are still
        // the right records -- the clock jumped, not the lineup -- so they are
        // re-cued where they now belong rather than torn down and reloaded,
        // which would put a hole in the output exactly where the skip is.
        if jumped && !first {
            // Speech is scheduled to the sample against the old clock, so it
            // is the one thing that really is wrong now.
            plan.voice.push(Command::OffAir);
            self.voices.clear();
            self.free_voice = (0..VOICE_CHANNELS).collect();

            for deck in 0..DECKS {
                let Some(assigned) = self.decks[deck].as_mut() else { continue };
                let id = assigned.id.clone();
                if self.schedule.iter().any(|item| item.id == id) {
                    assigned.started = false; // `drive` will put it where it goes.
                } else {
                    self.decks[deck] = None;
                    plan.stop.push(deck);
                }
            }
            self.note = Some("Skipped.".into());
        }

        // Let go of decks whose record has finished, so the next one can have
        // them. A scheduled outro can end before the source file does.
        for deck in 0..DECKS {
            let done = self.decks[deck].as_ref().is_some_and(|assigned| {
                self.schedule
                    .iter()
                    .find(|item| item.id == assigned.id)
                    .is_none_or(|item| item.ends_at() <= snapshot.now)
            });
            if done {
                self.decks[deck] = None;
                plan.stop.push(deck);
                // A record you took over is only yours until it ends.
                self.held.tone[deck] = [false; 4];
                self.held.gain[deck] = false;
                self.held.tempo[deck] = false;
                self.held.key_lock[deck] = false;
                self.started_at[deck] = None;
            }
        }

        let ended: Vec<String> = self
            .voices
            .keys()
            .filter(|id| {
                self.schedule
                    .iter()
                    .find(|item| &&item.id == id)
                    .is_none_or(|item| item.ends_at() < snapshot.now - 0.25)
            })
            .cloned()
            .collect();
        for id in ended {
            if let Some(channel) = self.voices.remove(&id) {
                self.free_voice.push(channel);
            }
        }
    }

    /// Give upcoming items a deck (or a voice channel) and start fetching.
    /// The deck to put the next record on.
    ///
    /// Whichever one is not spoken for -- and never one you are using. A deck
    /// playing something you loaded yourself is yours, autopilot or not, so it
    /// waits for the other one rather than pulling the record out from under
    /// you. Among free decks it takes the silent one, so a record is cued up
    /// against the one that is still playing.
    fn free_deck(&self, decks: [DeckStatus; DECKS]) -> Option<usize> {
        let spare: Vec<usize> = (0..DECKS)
            .filter(|deck| self.decks[*deck].is_none())
            .filter(|deck| !decks[*deck].playing)
            .collect();
        spare
            .iter()
            .copied()
            .find(|deck| !decks[*deck].loaded)
            .or_else(|| spare.first().copied())
    }

    fn assign(&mut self, records: &[Record], decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        // Sorted, so the earliest record takes the first free deck. The
        // station sends them in order, but nothing here should depend on that.
        let mut due: Vec<Scheduled> = self
            .schedule
            .iter()
            .filter(|item| item.ends_at() > now)
            .filter(|item| !self.failed.contains(&item.id))
            .filter(|item| !self.pending.contains_key(&item.id))
            .filter(|item| item.is_music() || item.start_at < now + VOICE_LEAD_IN)
            .cloned()
            .collect();
        due.sort_by(|a, b| a.start_at.total_cmp(&b.start_at));

        for item in due {
            if item.is_music() {
                if self.decks.iter().flatten().any(|d| d.id == item.id) {
                    continue;
                }
                let Some(deck) = item.preferred_deck.filter(|d| *d < DECKS && self.decks[*d].is_none() && !decks[*d].playing)
                    .or_else(|| self.free_deck(decks)) else {
                    continue; // Both spoken for; it gets one when a record ends.
                };
                let Some(path) = self.resolve(&item.url, records) else {
                    self.note = Some(format!("Cannot find the audio for {}.", item.title));
                    continue;
                };
                // Straight into the plan: `Defalt::load` already decodes off
                // the UI thread, so there is nothing here worth a thread of
                // its own.
                self.decks[deck] = Some(Assigned { id: item.id.clone(), started: false, ready: false });
                self.started_at[deck] = None;
                let record = records.iter().find(|r| r.key == item.key && r.file == path)
                    .cloned().unwrap_or_else(|| item.record(path));
                plan.load.push((deck, record));
            } else {
                if self.voices.contains_key(&item.id) {
                    continue;
                }
                let Some(path) = self.resolve(&item.url, records) else {
                    continue;
                };
                self.pending.insert(item.id.clone(), ());
                let sender = self.fetch_out.clone();
                let id = item.id.clone();
                std::thread::spawn(move || {
                    let message = match crate::engine::decode::load(&path) {
                        Ok(track) => Fetched::Voice { id, track },
                        Err(error) => Fetched::Failed { id, error },
                    };
                    let _ = sender.send(message);
                });
            }
        }
    }

    fn air_voice(&mut self, id: &str, track: Arc<Track>, frame: u64, plan: &mut Plan) {
        let Some(item) = self.schedule.iter().find(|i| i.id == id).cloned() else { return };
        let Some((anchor_now, anchor_frame)) = self.anchor else { return };
        let Some(channel) = self.free_voice.pop() else {
            self.note = Some("More hosts talking than the voice bus can hold.".into());
            return;
        };

        let ahead = (item.start_at - anchor_now) * self.sample_rate as f64;
        let target = anchor_frame as f64 + ahead;
        let (start_frame, offset) = if target < frame as f64 {
            let late = (frame as f64 - target) / self.sample_rate as f64;
            (frame, item.offset + late)
        } else {
            (target as u64, item.offset)
        };
        if offset >= track.seconds() {
            self.free_voice.push(channel);
            return;
        }

        let envelope = if item.envelope.is_empty() {
            Envelope::flat(1.0)
        } else {
            Envelope::new(item.envelope.clone())
        };
        self.voices.insert(item.id.clone(), channel);
        plan.voice.push(Command::Air {
            channel,
            item: Some(Box::new(Item {
                track,
                start_frame,
                offset,
                duration: (item.duration - (offset - item.offset)).max(0.0),
                envelope: Arc::new(envelope),
            })),
        });
    }

    /// The part that actually performs. Everything here is a control on the
    /// panel, and everything here is skipped if you are holding it.
    fn drive(&mut self, decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        plan.duck = Some(self.speech_duck(now));

        // Start a record the moment its airtime comes round.
        for deck in 0..DECKS {
            let Some(assigned) = self.decks[deck].as_ref() else { continue };
            if assigned.started || !assigned.ready || !decks[deck].loaded || plan.load.iter().any(|(d, _)| *d == deck) {
                continue;
            }
            let Some(item) = self.schedule.iter().find(|i| i.id == assigned.id) else { continue };
            if now < item.start_at {
                continue;
            }
            let late = (now - item.start_at).max(0.0);
            if !self.held.key_lock[deck] { plan.key_lock[deck] = Some(item.key_lock); }
            plan.start.push((deck, item.source_at(late)));
            if !self.held.tempo[deck] {
                plan.speed[deck] = Some(item.rate_at(late));
            }
            self.started_at[deck] = Some(now);
            if let Some(assigned) = self.decks[deck].as_mut() {
                assigned.started = true;
            }
        }

        self.follow_playheads(decks, plan);

        // Tone: the station's automation, in knob positions.
        for deck in 0..DECKS {
            let Some(item) = self.on_deck(deck) else { continue };
            if !self.held.key_lock[deck] { plan.key_lock[deck] = Some(item.key_lock); }
            let elapsed = (now - item.start_at) as f32;
            plan.echo[deck] = Some(if let Some([start, end, seconds, mix, feedback]) = item.echo {
                let ramp = ((elapsed - start) / ((end - start) * 0.25).max(0.05)).clamp(0.0, 1.0);
                [if elapsed < end { mix * ramp } else { 0.0 }, feedback, seconds]
            } else { [0.0, 0.3, 0.25] });
            let held = self.held.tone[deck];
            let mut wanted = [0.5, 0.5, 0.5, 0.0];
            let mut any = !held.iter().all(|h| *h);

            for (index, curve) in
                [&item.automation.low, &item.automation.mid, &item.automation.high]
                    .into_iter()
                    .enumerate()
            {
                if held[index] {
                    continue;
                }
                if let Some(db) = curve.at(elapsed) {
                    wanted[index] = band_knob(db);
                    any = true;
                }
            }

            if !held[3] {
                // A low-pass and a high-pass cannot both be on one fader, so
                // whichever the station is actually sweeping wins; the pass
                // that is parked at its own end of the range is not sweeping.
                if let Some(hz) = item.automation.lpf.at(elapsed) {
                    if hz < 19_000.0 {
                        wanted[3] = filter_fader(hz, true);
                        any = true;
                    }
                }
                if let Some(hz) = item.automation.hpf.at(elapsed) {
                    if hz > 25.0 {
                        wanted[3] = filter_fader(hz, false);
                        any = true;
                    }
                }
            }

            if any {
                plan.tone[deck] = Some(wanted);
            }
        }

        // Factor the station's actual envelopes (including voice ducking)
        // through the equal-power crossfader and the real channel faders.
        // This preserves linear fades, cuts and overlap presets exactly.
        let mut levels = [0.0f32; DECKS];
        let mut sounding = [false; DECKS];
        for deck in 0..DECKS {
            let Some(assigned) = &self.decks[deck] else { continue };
            let Some(item) = self.on_deck(deck) else { continue };
            let just_started = plan.start.iter().any(|(d, _)| *d == deck);
            if assigned.ready && assigned.started && (decks[deck].playing || just_started)
                && item.start_at <= now && now < item.ends_at() {
                sounding[deck] = true;
                let envelope = if item.deck_envelope.is_empty() { &item.envelope } else { &item.deck_envelope };
                levels[deck] = Curve(envelope.clone()).at((now - item.start_at) as f32).unwrap_or(1.0).clamp(0.0, 1.0);
            }
        }
        // An incoming decode must never fade out the only deck that can play.
        if let Some((outgoing, incoming, _)) = self.overlap(now) {
            if sounding[outgoing] && !sounding[incoming] {
                levels[outgoing] = 1.0;
                plan.tone[outgoing] = Some([0.5, 0.5, 0.5, 0.0]);
                plan.echo[outgoing] = Some([0.0, 0.0, 0.25]);
            } else if !sounding[outgoing] && sounding[incoming] {
                // An outgoing deck that was paused or ran out cannot provide
                // the other half of the blend. Keep the available record clear.
                levels[incoming] = 1.0;
                plan.tone[incoming] = Some([0.5, 0.5, 0.5, 0.0]);
            } else if sounding[outgoing] && sounding[incoming] {
                // Decode recovery rejoins the existing blend gradually. Without
                // this, a late incoming deck jumps straight into a half-open
                // crossfade after the outgoing deck was deliberately held up.
                if let Some(started) = self.started_at[incoming] {
                    let incoming_item = self.on_deck(incoming).unwrap();
                    let outgoing_item = self.on_deck(outgoing).unwrap();
                    if started - incoming_item.start_at > 0.25 {
                        let recovery = (outgoing_item.ends_at() - started).clamp(0.01, 0.5);
                        let progress = ((now - started) / recovery).clamp(0.0, 1.0) as f32;
                        levels[incoming] *= progress;
                        levels[outgoing] = levels[outgoing].max(1.0 - progress);
                        if let Some(tone) = plan.tone[outgoing].as_mut() {
                            for (index, value) in tone.iter_mut().enumerate() {
                                let neutral = if index == 3 { 0.0 } else { 0.5 };
                                *value = neutral + (*value - neutral) * progress;
                            }
                        }
                        if let Some(echo) = plan.echo[outgoing].as_mut() { echo[0] *= progress; }
                    }
                }
            }
        }
        let (crossfade, gain) = mixer_levels(levels);
        if !self.held.crossfade && sounding.iter().any(|s| *s) {
            plan.crossfade = Some(crossfade);
        }
        for deck in 0..DECKS {
            if self.decks[deck].is_some() && !self.held.gain[deck] {
                plan.gain[deck] = Some(if sounding[deck] { gain } else { 0.0 });
            }
        }
    }

    /// The transition happening at `now`, as (outgoing deck, incoming deck,
    /// how far through it is).
    fn speech_duck(&self, now: f64) -> f32 {
        let Some(item) = (0..DECKS).filter_map(|d| self.on_deck(d))
            .find(|i| !i.deck_envelope.is_empty()) else { return 1.0 };
        let [target, attack, hold, release] = item.ducking;
        let mut windows: Vec<(f64, f64)> = self.schedule.iter().filter(|i| !i.is_music())
            .map(|i| (i.start_at, i.ends_at())).collect();
        windows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for window in windows {
            if let Some(last) = merged.last_mut().filter(|last| window.0 <= last.1 + (hold + attack) as f64) {
                last.1 = last.1.max(window.1);
            } else { merged.push(window); }
        }
        merged.into_iter().map(|(start, end)| {
            if now < start - attack as f64 || now >= end + (hold + release) as f64 { return 1.0; }
            if now < start { return target.powf(((now - start + attack as f64) / attack as f64) as f32); }
            if now <= end + hold as f64 { return target; }
            target.powf(1.0 - ((now - end - hold as f64) / release as f64) as f32)
        }).fold(1.0, f32::min)
    }

    /// Follow the schedule with gentle rate changes, never by jumping a playing
    /// record. Large discontinuities belong to a person (seek/scratch/pause),
    /// not to a feedback controller trying to undo their action.
    fn follow_playheads(&mut self, decks: [DeckStatus; DECKS], plan: &mut Plan) {
        for deck in 0..DECKS {
            if self.held.tempo[deck] || plan.speed[deck].is_some() || !decks[deck].playing {
                continue;
            }
            let Some(started) = self.started_at[deck] else { continue };
            if self.station_now - started < 0.25 { continue; }
            let Some(item) = self.on_deck(deck) else { continue };
            let [enabled, maximum, tolerance] = item.playback_feedback;
            if self.station_now >= item.ends_at() { continue; }
            let elapsed = self.station_now - item.start_at;
            let nominal = item.rate_at(elapsed);
            let expected = item.source_at(elapsed);
            let error = if enabled != 0.0 {
                decks[deck].position.filter(|p| p.is_finite()).map_or(0.0, |position| expected - position)
            } else { 0.0 };
            if error.abs() > 0.75 {
                self.held.tempo[deck] = true;
                self.note = Some(format!("Deck {} timing changed; automatic tempo correction paused.", deck + 1));
                continue;
            }
            let correction = if error.abs() <= tolerance { 0.0 }
                else { (error / 8.0).clamp(-maximum, maximum) };
            let target = (nominal * (1.0 + correction)).clamp(0.92, 1.08);
            if (target - decks[deck].playback_rate).abs() > 0.00005 {
                plan.speed[deck] = Some(target);
            }
        }
    }

    pub fn save_mix_settings(&mut self, values: serde_json::Value) {
        self.post("/api/mix/config", Some(values));
        self.note = Some("Mix settings sent. New transitions use the new choices.".into());
    }

    fn overlap(&self, now: f64) -> Option<(usize, usize, f32)> {
        let mut sounding: Vec<(usize, &Scheduled)> = (0..DECKS)
            .filter_map(|deck| self.on_deck(deck).map(|item| (deck, item)))
            .filter(|(_, item)| item.start_at <= now && now < item.ends_at())
            .collect();
        if sounding.len() < 2 {
            return None;
        }
        sounding.sort_by(|a, b| a.1.start_at.total_cmp(&b.1.start_at));
        let (out_deck, out_item) = sounding[0];
        let (in_deck, in_item) = sounding[1];

        // The overlap runs from the incoming record's start to the outgoing
        // one's end, which is what the station means by it.
        let length = (out_item.ends_at() - in_item.start_at).max(0.001);
        let progress = ((now - in_item.start_at) / length).clamp(0.0, 1.0);
        Some((out_deck, in_deck, progress as f32))
    }

    /// Where an item's audio actually is on this machine.
    ///
    /// The station serves it by basename under a media root, which is right
    /// for a browser and wrong here: the console can read the file. Music is
    /// in the cache when the station downloaded it and in your own library
    /// when it did not -- and the URL has thrown that path away, so only the
    /// library can put it back.
    fn resolve(&self, url: &str, records: &[Record]) -> Option<PathBuf> {
        let name = percent_decode(url.rsplit('/').next()?);

        if url.contains("/media/voice/") {
            let path = self.root.join("cache").join("voice").join(&name);
            return path.is_file().then_some(path);
        }

        let cached = self.root.join("cache").join("audio").join(&name);
        if cached.is_file() {
            return Some(cached);
        }
        records
            .iter()
            .find(|record| {
                record.file.file_name().and_then(|n| n.to_str()) == Some(name.as_str())
            })
            .map(|record| record.file.clone())
    }
}

/// Convert two desired amplitudes to equal-power fader position + gain.
fn mixer_levels(levels: [f32; 2]) -> (f32, f32) {
    let gain = levels[0].hypot(levels[1]);
    let crossfade = if gain > 0.000001 { levels[1].atan2(levels[0]) / std::f32::consts::FRAC_PI_2 } else { 0.5 };
    (crossfade, gain)
}

/* ── Reading the station ─────────────────────────────────────────────── */

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn fetch_queue(url: &str) -> Result<Vec<QueueRow>, String> {
    let body = ureq::get(url)
        .call()
        .map_err(|_| "the station is not answering".to_string())?
        .body_mut()
        .read_json::<serde_json::Value>()
        .map_err(|_| "the station sent an unreadable queue".to_string())?;
    Ok(queue_from(&body))
}

pub fn queue_from(body: &serde_json::Value) -> Vec<QueueRow> {
    body["items"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(QueueRow {
                        id: row["id"].as_str()?.to_string(),
                        stage: row["stage"].as_str().unwrap_or("queued").to_string(),
                        playing: row["playing"].as_bool().unwrap_or(false),
                        eta: row["eta"].as_f64(),
                        artist: row["artist"].as_str().unwrap_or("").to_string(),
                        title: row["title"].as_str().unwrap_or("").to_string(),
                        bpm: row["bpm"].as_f64(),
                        camelot: row["camelot"].as_str().map(str::to_string),
                        can_move: row["can_move"].as_bool().unwrap_or(false),
                        can_remove: row["can_remove"].as_bool().unwrap_or(false),
                        selection_reason: row["selection"]["reason"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fetch(url: &str, frame: u64) -> Result<Snapshot, String> {
    let body = ureq::get(url)
        .call()
        .map_err(|_| "the station is not answering".to_string())?
        .body_mut()
        .read_json::<serde_json::Value>()
        .map_err(|_| "the station sent an unreadable schedule".to_string())?;
    Ok(snapshot_from(&body, frame))
}

pub fn snapshot_from(body: &serde_json::Value, frame: u64) -> Snapshot {
    Snapshot {
        now: body["now"].as_f64().unwrap_or(0.0),
        epoch: body["epoch"].as_i64().unwrap_or(0),
        frame,
        items: body["items"]
            .as_array()
            .map(|items| items.iter().filter_map(item_from).collect())
            .unwrap_or_default(),
    }
}

fn curve_from(value: &serde_json::Value) -> Curve {
    Curve(
        value
            .as_array()
            .map(|points| {
                points
                    .iter()
                    .filter_map(|point| {
                        let pair = point.as_array()?;
                        Some([
                            pair.first()?.as_f64()? as f32,
                            pair.get(1)?.as_f64()? as f32,
                        ])
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn rate_curve_from(value: &serde_json::Value) -> Vec<[f64; 2]> {
    let Some(points) = value.as_array().filter(|p| p.len() <= 16) else { return Vec::new() };
    let mut curve = Vec::with_capacity(points.len());
    let mut last = -1.0;
    for point in points {
        let (Some(time), Some(rate)) = (point[0].as_f64(), point[1].as_f64()) else { return Vec::new() };
        if !time.is_finite() || !rate.is_finite() || time <= last || time < 0.0 || !(0.92..=1.08).contains(&rate) {
            return Vec::new();
        }
        if curve.is_empty() && time != 0.0 { return Vec::new(); }
        curve.push([time, rate]);
        last = time;
    }
    curve
}

fn item_from(value: &serde_json::Value) -> Option<Scheduled> {
    let meta = &value["meta"];
    let automation = &meta["automation"];
    let transition = &meta["transition"];

    Some(Scheduled {
        id: value["id"].as_str()?.to_string(),
        kind: value["kind"].as_str().unwrap_or("").to_string(),
        url: value["url"].as_str()?.to_string(),
        start_at: value["start_at"].as_f64()?,
        duration: value["duration"].as_f64()?,
        offset: value["offset"].as_f64().unwrap_or(0.0),
        preferred_deck: meta["deck"].as_u64().map(|d| d as usize),
        envelope: curve_from(&value["envelope"]).0,
        deck_envelope: curve_from(&meta["deck_envelope"]).0,
        playback_rate: meta["playback_rate"].as_f64().unwrap_or(1.0).clamp(0.92, 1.08),
        rate_curve: rate_curve_from(&meta["rate_curve"]),
        key_lock: meta["key_lock"].as_bool().unwrap_or(false),
        playback_feedback: [
            if meta["playback_feedback"]["enabled"].as_bool().unwrap_or(true) { 1.0 } else { 0.0 },
            meta["playback_feedback"]["max_adjustment"].as_f64().unwrap_or(0.005).clamp(0.0, 0.01),
            meta["playback_feedback"]["tolerance_ms"].as_f64().unwrap_or(30.0).clamp(10.0, 150.0) / 1000.0,
        ],
        echo: meta["echo"].is_object().then(|| [
            meta["echo"]["start"].as_f64().unwrap_or(0.0) as f32,
            meta["echo"]["end"].as_f64().unwrap_or(0.0) as f32,
            meta["echo"]["seconds"].as_f64().unwrap_or(0.25).clamp(0.03, 1.8) as f32,
            meta["echo"]["mix"].as_f64().unwrap_or(0.0).clamp(0.0, 0.5) as f32,
            meta["echo"]["feedback"].as_f64().unwrap_or(0.3).clamp(0.0, 0.65) as f32,
        ]),
        ducking: [
            meta["ducking"]["target_gain"].as_f64().unwrap_or(0.10).clamp(0.001, 1.0) as f32,
            meta["ducking"]["attack"].as_f64().unwrap_or(0.35).clamp(0.01, 3.0) as f32,
            meta["ducking"]["hold_after"].as_f64().unwrap_or(0.40).clamp(0.0, 5.0) as f32,
            meta["ducking"]["release"].as_f64().unwrap_or(1.2).clamp(0.01, 6.0) as f32,
        ],
        automation: Automation {
            low: curve_from(&automation["low"]),
            mid: curve_from(&automation["mid"]),
            high: curve_from(&automation["high"]),
            lpf: curve_from(&automation["lpf"]),
            hpf: curve_from(&automation["hpf"]),
        },
        transition: transition.is_object().then(|| Transition {
            preset: transition["preset"].as_str().unwrap_or("").to_string(),
            reason: transition["reason"].as_str().unwrap_or("").to_string(),
            overlap: transition["overlap"].as_f64().unwrap_or(0.0),
        }),
        title: meta["title"].as_str().or(meta["text"].as_str()).unwrap_or("").to_string(),
        artist: meta["artist"].as_str().unwrap_or("").to_string(),
        bpm: meta["bpm"].as_f64(),
        camelot: meta["camelot"].as_str().map(str::to_string),
        key: meta["key"].as_str().unwrap_or("").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schedule shaped exactly like the station's, taken from a live one.
    const REAL: &str = r#"{
        "now": 593.4, "epoch": 0, "items": [
            {"id": "43ac", "kind": "music", "url": "/media/audio/658c.m4a",
             "start_at": 553.5, "duration": 198.5, "offset": 0.0,
             "envelope": [[0.0, 0.0001], [6.0, 1.0], [198.5, 1.0]],
             "meta": {"title": "GESHUOU", "artist": "INOHA", "bpm": 107.7,
                      "camelot": "8A", "key": "inoha|geshuou",
                      "automation": {"low": [[0.0, -26.0], [6.0, 0.0], [198.5, 0.0]],
                                     "lpf": [[0.0, 380.0], [4.2, 6090.5], [6.0, 20000.0]]},
                      "transition": {"preset": "rise", "overlap": 6.0,
                                     "eq": "end_bass_swap",
                                     "reason": "stepping up in energy"}}}
        ]}"#;

    fn real() -> Snapshot {
        snapshot_from(&serde_json::from_str(REAL).unwrap(), 0)
    }

    fn prepared_pair() -> Airtime {
        let mut a = real().items[0].clone();
        a.id = "a".into();
        a.start_at = 100.0;
        a.duration = 300.0;
        a.offset = 7.0;
        a.playback_rate = 1.1;
        a.rate_curve = vec![[0.0, 1.1], [100.0, 1.0]];
        let mut b = a.clone();
        b.id = "b".into();
        b.start_at = 392.0;
        b.duration = 200.0;
        b.offset = 12.0;
        b.playback_rate = 1.05;
        b.rate_curve.clear();
        b.transition.as_mut().unwrap().overlap = 8.0;
        let mut airtime = airtime_with(vec![a, b], 150.0);
        airtime.decks[0] = Some(Assigned { id: "a".into(), started: true, ready: true });
        airtime.decks[1] = Some(Assigned { id: "b".into(), started: false, ready: false });
        airtime
    }

    #[test]
    fn mix_markers_use_source_offsets_and_integrate_tempo_recovery() {
        let mut airtime = prepared_pair();
        let outgoing = airtime.transition_windows(0);
        let incoming = airtime.transition_windows(1);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(incoming.len(), 1);
        assert!((outgoing[0].start - 304.0).abs() < 1e-6);
        assert!((outgoing[0].end - 312.0).abs() < 1e-6);
        assert!(!outgoing[0].incoming);
        assert_eq!(incoming[0].start, 12.0);
        assert!((incoming[0].end - 20.4).abs() < 1e-6);
        assert!(incoming[0].incoming);
        airtime.station_now = 388.0;
        assert_eq!(airtime.transition_windows(0), outgoing, "Skip moved the waveform's source markers");
    }

    #[test]
    fn markers_follow_the_actual_pair_and_disappear_when_it_is_removed() {
        let mut airtime = prepared_pair();
        airtime.schedule[1].start_at += 5.0;
        let windows = airtime.transition_windows(0);
        assert!((windows[0].end - windows[0].start - 3.0).abs() < 1e-6);
        airtime.schedule[1].transition = None; // A dry break has no overlap markers.
        assert!(airtime.transition_windows(0).is_empty());
        airtime.schedule.pop();
        assert!(airtime.transition_windows(1).is_empty());
    }

    #[test]
    fn skip_waits_for_the_incoming_decode_then_dispatches_once() {
        let mut airtime = prepared_pair();
        airtime.skip();
        assert!(!airtime.take_ready_skip());
        assert!(airtime.skip_pending.is_some());
        airtime.decks[1].as_mut().unwrap().ready = true;
        assert!(airtime.take_ready_skip());
        assert!(!airtime.take_ready_skip());
    }

    #[test]
    fn pending_skip_is_cancelled_when_the_mix_starts_or_radio_stops() {
        let mut airtime = prepared_pair();
        airtime.skip();
        airtime.station_now = 393.0;
        airtime.decks[1].as_mut().unwrap().ready = true;
        assert!(!airtime.take_ready_skip());
        assert!(airtime.skip_pending.is_none());
        airtime.station_now = 150.0;
        airtime.skip();
        airtime.set_on(false);
        assert!(airtime.skip_pending.is_none());
    }

    #[test]
    fn the_stations_own_schedule_reads_completely() {
        let parsed = real();
        let item = &parsed.items[0];
        assert_eq!(item.title, "GESHUOU");
        assert_eq!(item.artist, "INOHA");
        assert_eq!(item.bpm, Some(107.7));
        assert_eq!(item.camelot.as_deref(), Some("8A"));
        assert!(item.is_music());

        let transition = item.transition.as_ref().expect("no transition");
        assert_eq!(transition.preset, "rise");
        assert_eq!(transition.overlap, 6.0);
        assert_eq!(transition.reason, "stepping up in energy");

        assert!(!item.automation.low.is_empty(), "the bass swap was dropped");
        assert!(!item.automation.lpf.is_empty(), "the filter rise was dropped");
    }

    #[test]
    fn a_bass_swap_reads_as_a_knob_that_starts_down_and_comes_up() {
        let parsed = real();
        let low = &parsed.items[0].automation.low;
        let start = band_knob(low.at(0.0).unwrap());
        let end = band_knob(low.at(6.0).unwrap());
        assert!(start < 0.1, "the bass did not start swapped out: {start}");
        assert!((end - 0.5).abs() < 0.01, "the bass did not come back: {end}");
    }

    #[test]
    fn a_filter_rise_reads_as_a_fader_that_opens() {
        let parsed = real();
        let lpf = &parsed.items[0].automation.lpf;
        let start = filter_fader(lpf.at(0.0).unwrap(), true);
        let middle = filter_fader(lpf.at(4.2).unwrap(), true);
        // A low-pass sits on the negative half and comes back towards centre
        // as it opens.
        assert!(start < -0.5, "the filter did not start closed: {start}");
        assert!(middle > start, "the filter did not open: {start} -> {middle}");
    }

    /* -- The knob mappings, against the strip's own taper -- */

    #[test]
    fn the_band_knob_inverts_the_strips_taper() {
        // Every position must survive the round trip through the real curve,
        // or automation lands somewhere other than where the station asked.
        for step in 0..=20 {
            let position = step as f32 / 20.0;
            let db = crate::engine::filters::decibels_for_test(position);
            if db <= -30.0 {
                continue; // Below the taper's range; everything is a kill.
            }
            let back = band_knob(db);
            assert!(
                (back - position).abs() < 0.02,
                "{position} -> {db} dB -> {back}"
            );
        }
    }

    #[test]
    fn a_centred_band_is_a_centred_knob() {
        assert!((band_knob(0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_band_swapped_right_out_is_a_knob_at_the_bottom() {
        assert_eq!(band_knob(-60.0), 0.0);
    }

    #[test]
    fn the_filter_fader_lands_on_the_frequency_it_was_asked_for() {
        for hz in [200.0, 500.0, 1_000.0, 4_000.0, 10_000.0] {
            let fader = filter_fader(hz, true);
            let t = -fader;
            let back = 20_000.0 * (1.0 - t).powf(2.4) + 120.0 * t;
            assert!((back - hz).abs() / hz < 0.05, "{hz} Hz -> {fader} -> {back}");
        }
        for hz in [100.0, 500.0, 2_000.0, 8_000.0] {
            let fader = filter_fader(hz, false);
            let back = 20.0 + 9_000.0 * fader.powf(2.4);
            assert!((back - hz).abs() / hz < 0.05, "{hz} Hz -> {fader} -> {back}");
        }
    }

    #[test]
    fn a_low_pass_is_negative_and_a_high_pass_positive() {
        // They share one fader, and the sign is what tells them apart.
        assert!(filter_fader(500.0, true) < 0.0);
        assert!(filter_fader(500.0, false) > 0.0);
    }

    /* -- Holding a control -- */

    fn airtime_with(items: Vec<Scheduled>, now: f64) -> Airtime {
        let mut airtime = Airtime::new(Path::new("."), 8090, 48_000);
        airtime.on = true;
        airtime.station_now = now;
        airtime.schedule = items;
        airtime
    }

    #[test]
    fn a_held_knob_is_left_out_of_the_plan_and_the_rest_is_not() {
        let parsed = real();
        let mut airtime = airtime_with(parsed.items, 553.5);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        let free = plan.tone[0].expect("nothing was automated at all");

        // Now take the low band by hand.
        airtime.held.tone[0][0] = true;
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        let held = plan.tone[0].expect("holding one control stopped all of them");

        assert!(free[0] < 0.1, "the bass swap was not being driven: {free:?}");
        assert!((held[0] - 0.5).abs() < 1e-6, "a held knob was still written: {held:?}");
        assert_eq!(free[3], held[3], "holding the low band moved the filter");
    }

    #[test]
    fn a_held_crossfader_is_never_written() {
        let parsed = real();
        let mut airtime = airtime_with(parsed.items, 600.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.held.crossfade = true;

        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert!(plan.crossfade.is_none(), "the crossfader was moved under your hand");
    }

    #[test]
    fn one_record_playing_puts_the_crossfader_on_its_deck() {
        let parsed = real();
        let mut airtime = airtime_with(parsed.items, 600.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert_eq!(plan.crossfade, Some(0.0), "the fader was not on deck A");
    }

    #[test]
    fn a_transition_walks_the_crossfader_across() {
        let mut items = real().items;
        let mut second = items[0].clone();
        second.id = "next".into();
        second.start_at = 700.0;
        second.duration = 180.0;
        items[0].duration = 152.5; // ends at 706, so a six second overlap
        items[0].envelope = vec![[0.0, 1.0], [146.5, 1.0], [149.5, 0.707107], [152.5, 0.0]];
        second.envelope = vec![[0.0, 0.0], [3.0, 0.707107], [6.0, 1.0]];
        items.push(second);

        let readings: Vec<f32> = [700.0, 703.0, 706.0]
            .into_iter()
            .map(|now| {
                let mut airtime = airtime_with(items.clone(), now);
                airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
                airtime.decks[1] = Some(Assigned { id: "next".into(), started: true, ready: true });
                let mut plan = Plan::default();
                airtime.drive([busy(true, true), busy(true, true)], &mut plan);
                plan.crossfade.expect("no crossfade during a transition")
            })
            .collect();

        assert!(readings[0] < 0.05, "it did not start on the outgoing deck: {readings:?}");
        assert!(readings[1] > 0.4 && readings[1] < 0.6, "it did not pass through the middle: {readings:?}");
        assert!(readings[2] > 0.95, "it did not arrive on the incoming deck: {readings:?}");
    }

    #[test]
    fn a_duck_moves_the_channel_fader_not_the_crossfader() {
        // Outside a transition the station's envelope is only ever a duck, and
        // a duck is a channel fader move.
        let mut items = real().items;
        items[0].envelope = vec![[0.0, 1.0], [10.0, 1.0], [12.0, 0.4], [20.0, 0.4]];
        let mut airtime = airtime_with(items, 553.5 + 15.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        let gain = plan.gain[0].expect("the fader was not driven");
        assert!((gain - 0.4).abs() < 0.01, "the deck did not duck: {gain}");
    }

    #[test]
    fn a_record_is_started_where_the_station_says_and_only_once() {
        let mut items = real().items;
        items[0].offset = 3.0;
        let mut airtime = airtime_with(items, 553.5);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: false, ready: true });

        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert_eq!(plan.start.len(), 1);
        assert_eq!(plan.start[0].0, 0);
        assert!((plan.start[0].1 - 3.0).abs() < 0.01, "wrong cue point: {:?}", plan.start[0]);

        // A second turn must not restart it.
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert!(plan.start.is_empty(), "it was started twice");
    }

    #[test]
    fn joining_late_starts_further_into_the_record() {
        let mut airtime = airtime_with(real().items, 553.5 + 30.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: false, ready: true });

        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert!((plan.start[0].1 - 30.0).abs() < 0.1, "it restarted: {:?}", plan.start[0]);
    }

    #[test]
    fn a_record_is_not_started_before_it_has_finished_loading() {
        let mut airtime = airtime_with(real().items, 553.5);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: false, ready: true });

        let mut plan = Plan::default();
        airtime.drive([busy(false, false), busy(false, false)], &mut plan);
        assert!(plan.start.is_empty(), "it played a deck with nothing on it");
    }

    /* -- Which deck the next record goes on -- */

    fn busy(loaded: bool, playing: bool) -> DeckStatus {
        DeckStatus { loaded, playing, ..DeckStatus::default() }
    }

    #[test]
    fn the_next_record_goes_on_the_deck_that_is_not_playing() {
        let mut airtime = airtime_with(real().items, 600.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        // A is playing the station's record; B is empty.
        let picked = airtime.free_deck([busy(true, true), busy(false, false)]);
        assert_eq!(picked, Some(1));
    }

    #[test]
    fn a_deck_you_are_playing_yourself_is_not_taken() {
        // You loaded something onto B and started it. The station waits for A
        // rather than pulling the record out from under you.
        let airtime = airtime_with(real().items, 600.0);
        let picked = airtime.free_deck([busy(false, false), busy(true, true)]);
        assert_eq!(picked, Some(0));
    }

    #[test]
    fn both_decks_playing_means_the_next_record_waits() {
        let mut airtime = airtime_with(real().items, 600.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        let picked = airtime.free_deck([busy(true, true), busy(true, true)]);
        assert_eq!(picked, None, "it took a deck that was sounding");
    }

    #[test]
    fn an_empty_deck_is_preferred_over_a_stopped_one_with_a_record_on_it() {
        // Both are fair game, but taking the empty one leaves whatever you
        // cued up by hand sitting there.
        let airtime = airtime_with(real().items, 600.0);
        assert_eq!(airtime.free_deck([busy(true, false), busy(false, false)]), Some(1));
        assert_eq!(airtime.free_deck([busy(false, false), busy(true, false)]), Some(0));
    }

    /* -- Preloading -- */

    /// A library holding a file for every url in `items`, so these tests
    /// exercise which deck a record lands on rather than whether its file can
    /// be found -- which has its own tests.
    fn library_for(items: &[Scheduled]) -> Vec<Record> {
        items
            .iter()
            .map(|item| {
                let name = item.url.rsplit('/').next().unwrap_or("x");
                Record {
                    key: item.key.clone(),
                    artist: item.artist.clone(),
                    title: item.title.clone(),
                    album: None,
                    duration: Some(item.duration),
                    bpm: item.bpm,
                    camelot: item.camelot.clone(),
                    lufs: None,
                    file: PathBuf::from(format!(r"E:\Music\{name}")),
                    beat_offset: None,
                    beat_period: None,
                    downbeat_offset: None,
                }
            })
            .collect()
    }

    /// Two records back to back, the second a long way off.
    fn pair() -> Vec<Scheduled> {
        let mut items = real().items;
        items[0].start_at = 0.0;
        items[0].duration = 200.0;
        let mut second = items[0].clone();
        second.id = "next".into();
        second.key = "second|record".into();
        second.url = "/media/audio/second.m4a".into();
        second.start_at = 194.0;
        second.duration = 200.0;
        items.push(second);
        items
    }

    #[test]
    fn the_next_record_is_cued_up_long_before_it_airs() {
        // The whole reason a console has two decks. Waiting until a record is
        // nearly due means decoding it under time pressure, and a skip can
        // wind the clock straight past the window it was going to use.
        let items = pair();
        let records = library_for(&items);
        let mut airtime = airtime_with(items, 5.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        let mut plan = Plan::default();
        airtime.assign(&records, [busy(true, true), busy(false, false)], &mut plan);

        assert_eq!(plan.load.len(), 1, "it did not cue anything up");
        assert_eq!(plan.load[0].0, 1, "it cued onto the wrong deck");
        assert_eq!(plan.load[0].1.key, "second|record");
        assert!(airtime.decks[1].is_some(), "the deck was not claimed");
    }

    #[test]
    fn a_skip_into_a_transition_finds_the_record_already_on_its_deck() {
        // The bug this guards: the incoming record used to be loaded twenty
        // seconds before it aired, and a skip winds the clock forward to just
        // before the next transition -- which could land inside that window.
        // The transition then began against an empty deck.
        let items = pair();
        let records = library_for(&items);
        let mut airtime = airtime_with(items.clone(), 5.0);
        airtime.epoch = 0;
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        // Cue up as normal, well ahead.
        let mut plan = Plan::default();
        airtime.assign(&records, [busy(true, true), busy(false, false)], &mut plan);
        assert_eq!(plan.load.len(), 1, "nothing was cued up to begin with");

        // Now the station is skipped to just before the transition.
        let mut plan = Plan::default();
        airtime.absorb(snapshot_at(items, 193.0, 1), &mut plan);

        assert!(plan.load.is_empty(), "it reloaded a record it already had");
        assert_eq!(
            airtime.decks[1].as_ref().map(|a| a.id.as_str()),
            Some("next"),
            "the incoming record was not on a deck when the transition arrived"
        );
    }

    #[test]
    fn starting_from_cold_fills_both_decks() {
        let items = pair();
        let records = library_for(&items);
        let mut airtime = airtime_with(items, 0.0);
        let mut plan = Plan::default();
        airtime.assign(&records, [busy(false, false), busy(false, false)], &mut plan);
        assert_eq!(plan.load.len(), 2, "it only cued one of two free decks");
        assert!(airtime.decks.iter().all(|d| d.is_some()));
    }

    #[test]
    fn a_third_record_waits_rather_than_evicting_one_of_the_two() {
        let mut items = pair();
        let mut third = items[0].clone();
        third.id = "third".into();
        third.url = "/media/audio/third.m4a".into();
        third.start_at = 400.0;
        items.push(third);

        let records = library_for(&items);
        let mut airtime = airtime_with(items, 0.0);
        let mut plan = Plan::default();
        airtime.assign(&records, [busy(false, false), busy(false, false)], &mut plan);
        assert_eq!(plan.load.len(), 2, "it tried to cue three records onto two decks");
    }

    #[test]
    fn a_finished_record_gives_its_deck_up_without_waiting_for_a_poll() {
        // A second and a half of the following record's loading time was going
        // into noticing that the last one had ended.
        let mut items = pair();
        items[1].start_at = 260.0; // no overlap, so deck A is plainly finished
        let records = library_for(&items);
        let mut airtime = airtime_with(items, 240.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        let plan = airtime.tick(0, &records, [busy(true, false), busy(false, false)], true);
        assert!(airtime.decks[0].is_none() || plan.load.iter().any(|(d, _)| *d == 0),
                "the deck was still held by a record that had ended");
    }

    /* -- Skipping -- */

    fn snapshot_at(items: Vec<Scheduled>, now: f64, epoch: i64) -> Snapshot {
        Snapshot { now, epoch, items, frame: 0 }
    }

    #[test]
    fn a_skip_re_cues_the_decks_rather_than_reloading_them() {
        // The clock jumped, not the lineup. Tearing the decks down and
        // decoding again would put a hole in the output exactly where the
        // skip is.
        let items = real().items;
        let mut airtime = airtime_with(items.clone(), 560.0);
        airtime.epoch = 0;
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });

        let mut plan = Plan::default();
        airtime.absorb(snapshot_at(items, 700.0, 1), &mut plan);

        assert!(plan.stop.is_empty(), "it stopped a deck it did not need to");
        assert!(plan.load.is_empty(), "it reloaded a record it already had");
        assert!(airtime.decks[0].is_some(), "it gave the deck up");
        assert!(
            !airtime.decks[0].as_ref().unwrap().started,
            "it did not re-cue, so the record is still playing where it was"
        );

        // And the re-cue lands where the station now is.
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        let (deck, at) = plan.start[0];
        assert_eq!(deck, 0);
        assert!((at - (700.0 - 553.5)).abs() < 0.1, "re-cued to the wrong place: {at}");
    }

    #[test]
    fn a_skip_takes_the_voices_off_because_their_timing_is_now_wrong() {
        // Speech is scheduled to the sample against the old clock, so unlike
        // the records it really is wrong after a jump.
        let items = real().items;
        let mut airtime = airtime_with(items.clone(), 560.0);
        airtime.epoch = 0;
        airtime.voices.insert("a-line".into(), 0);
        airtime.free_voice.retain(|c| *c != 0);

        let mut plan = Plan::default();
        airtime.absorb(snapshot_at(items, 700.0, 1), &mut plan);

        assert!(
            plan.voice.iter().any(|c| matches!(c, Command::OffAir)),
            "a line kept playing against a clock that had moved"
        );
        assert_eq!(airtime.free_voice.len(), VOICE_CHANNELS);
    }

    #[test]
    fn a_record_the_skip_left_behind_is_stopped_and_given_up() {
        let items = real().items;
        let mut airtime = airtime_with(items, 560.0);
        airtime.epoch = 0;
        airtime.decks[0] = Some(Assigned { id: "gone".into(), started: true, ready: true });

        let mut plan = Plan::default();
        airtime.absorb(snapshot_at(real().items, 700.0, 1), &mut plan);

        assert_eq!(plan.stop, vec![0]);
        assert!(airtime.decks[0].is_none());
    }

    #[test]
    fn the_first_schedule_is_not_mistaken_for_a_skip() {
        let mut airtime = Airtime::new(Path::new("."), 8090, 48_000);
        airtime.on = true;
        let mut plan = Plan::default();
        airtime.absorb(snapshot_at(real().items, 600.0, 7), &mut plan);
        assert!(plan.voice.is_empty(), "it went off air before it went on");
        assert!(plan.stop.is_empty());
    }

    /* -- Rating -- */

    #[test]
    fn rating_needs_something_on_air() {
        let mut airtime = airtime_with(Vec::new(), 600.0);
        airtime.rate(true);
        assert!(airtime.note.take().is_some_and(|n| n.contains("Nothing on air")));
    }

    #[test]
    fn what_is_on_air_is_the_most_recently_started_record() {
        // Mid-transition both decks are sounding. A thumb belongs to the one
        // coming in, which is the one you are reacting to.
        let mut items = real().items;
        let mut second = items[0].clone();
        second.id = "next".into();
        second.key = "second|record".into();
        second.start_at = 700.0;
        items[0].duration = 152.5;
        items.push(second);

        let mut airtime = airtime_with(items, 703.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.decks[1] = Some(Assigned { id: "next".into(), started: true, ready: true });
        assert_eq!(airtime.current().map(|i| i.key.as_str()), Some("second|record"));
    }

    /* -- The queue -- */

    /// The three stages, exactly as the station reports them.
    const QUEUE: &str = r#"{
        "now": 600.0, "items": [
            {"id": "a1", "stage": "on_deck", "playing": true, "eta": 0.0,
             "artist": "INOHA", "title": "GESHUOU", "key": "inoha|geshuou",
             "bpm": 107.7, "camelot": "8A", "can_move": false, "can_remove": false},
            {"id": "a2", "stage": "on_deck", "playing": false, "eta": 92.4,
             "artist": "Alex G", "title": "Pretend", "bpm": 104.2,
             "camelot": "9A", "can_move": false, "can_remove": true},
            {"id": "q1", "stage": "queued", "playing": false, "eta": null,
             "artist": "Tame Impala", "title": "The Less I Know The Better",
             "bpm": 116.9, "camelot": "5B", "can_move": true, "can_remove": true},
            {"id": "req:7", "stage": "finding", "playing": false, "eta": null,
             "artist": null, "title": "something jazzy",
             "source": "request", "can_move": false, "can_remove": true}
        ]}"#;

    fn queue() -> Vec<QueueRow> {
        queue_from(&serde_json::from_str(QUEUE).unwrap())
    }

    #[test]
    fn the_queue_reads_all_three_stages() {
        let rows = queue();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].stage, "on_deck");
        assert!(rows[0].playing);
        assert_eq!(rows[2].stage, "queued");
        assert_eq!(rows[3].stage, "finding");
    }

    #[test]
    fn what_is_playing_can_never_be_dropped_or_moved() {
        // Its air time is the thing every transition after it was computed
        // against, and it is already sounding.
        let playing = &queue()[0];
        assert!(!playing.can_remove, "the record on air was offered a drop");
        assert!(!playing.can_move);
    }

    #[test]
    fn a_record_on_the_clock_can_be_dropped_but_not_reordered() {
        let on_deck = &queue()[1];
        assert!(on_deck.can_remove);
        assert!(!on_deck.can_move, "moving it would invalidate every time after it");
    }

    #[test]
    fn a_request_still_being_found_can_only_be_called_off() {
        let finding = &queue()[3];
        assert!(finding.can_remove);
        assert!(!finding.can_move);
        // It has no artist yet, so the label must not read " - something".
        assert_eq!(finding.label(), "something jazzy");
    }

    #[test]
    fn a_row_with_an_artist_reads_as_artist_and_title() {
        assert_eq!(queue()[0].label(), "INOHA - GESHUOU");
    }

    #[test]
    fn an_empty_answer_is_an_empty_queue_rather_than_an_error() {
        assert!(queue_from(&serde_json::json!({"items": []})).is_empty());
        assert!(queue_from(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn a_row_missing_its_id_is_dropped_rather_than_guessed() {
        // Every action is addressed by id. A row without one is a button that
        // would fail, so it never gets drawn.
        let rows = queue_from(&serde_json::json!({
            "items": [{"stage": "queued", "title": "no id here"},
                      {"id": "ok", "stage": "queued", "title": "fine"}]
        }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "ok");
    }

    #[test]
    fn a_station_that_is_not_running_shows_no_queue_at_all() {
        // Rather than the last one it answered with, which would be a list of
        // records that are not going to play.
        let mut airtime = Airtime::new(Path::new("."), 8090, 48_000);
        airtime.queue = queue();
        let plan = airtime.tick(0, &[], [DeckStatus::default(); DECKS], false);
        assert!(airtime.queue.is_empty(), "it kept a queue from a dead station");
        assert!(plan.load.is_empty());
    }

    /* -- Asking for things -- */

    #[test]
    fn an_empty_request_is_not_sent() {
        let mut airtime = Airtime::new(Path::new("."), 8090, 48_000);
        airtime.request = "   ".into();
        airtime.submit_request();
        assert!(airtime.note.is_none(), "it sent whitespace to the station");
        assert_eq!(airtime.request, "   ", "it cleared a box it never sent");
    }

    #[test]
    fn sending_a_request_clears_the_box_and_says_what_went() {
        let mut airtime = Airtime::new(Path::new("."), 8090, 48_000);
        airtime.request = "  do the news  ".into();
        airtime.submit_request();
        assert!(airtime.request.is_empty(), "the box kept what was already sent");
        let note = airtime.note.take().expect("it said nothing");
        assert!(note.contains("do the news"), "{note}");
        assert!(!note.contains("  do"), "it did not trim: {note}");
    }

    #[test]
    fn a_percent_encoded_name_decodes() {
        assert_eq!(percent_decode("a%20b.flac"), "a b.flac");
        assert_eq!(percent_decode("100%.flac"), "100%.flac");
    }

    #[test]
    fn a_record_the_station_never_downloaded_resolves_through_the_library() {
        let airtime = Airtime::new(Path::new(r"C:\nowhere"), 8090, 48_000);
        let record = Record {
            key: "a|b".into(),
            artist: "A".into(),
            title: "B".into(),
            album: None,
            duration: None,
            bpm: None,
            camelot: None,
            lufs: None,
            file: PathBuf::from(r"E:\Music\Some Record.wav"),
            beat_offset: None,
            beat_period: None,
            downbeat_offset: None,
        };
        let found = airtime.resolve("/media/audio/Some%20Record.wav", &[record]);
        assert_eq!(found, Some(PathBuf::from(r"E:\Music\Some Record.wav")));
    }

    #[test]
    fn turning_it_off_gives_every_deck_and_channel_back() {
        let mut airtime = airtime_with(real().items, 600.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.held.crossfade = true;

        assert!(airtime.set_on(false));
        assert!(!airtime.live());
        assert!(!airtime.held.any(), "it kept holding controls after going off");
    }

    #[test]
    fn preloaded_records_are_not_mistaken_for_completed_queue_loads() {
        let items = pair();
        let records = library_for(&items);
        let mut airtime = airtime_with(items, 1.0);
        let status = [busy(true, false); DECKS]; // both old records are loaded
        let mut plan = Plan::default();
        airtime.assign(&records, status, &mut plan);
        airtime.drive(status, &mut plan);
        assert_eq!(plan.load.len(), 2);
        assert!(plan.start.is_empty());
        let mut pending = Plan::default();
        airtime.drive(status, &mut pending);
        assert!(pending.start.is_empty(), "old loaded flag acknowledged the new record");
        airtime.deck_ready(0);
        let mut ready = Plan::default();
        airtime.drive(status, &mut ready);
        assert_eq!(ready.start, vec![(0, 1.0)]);
    }

    #[test]
    fn opening_pair_honors_their_original_decks() {
        let mut items = pair();
        items[0].preferred_deck = Some(1);
        items[1].preferred_deck = Some(0);
        let records = library_for(&items);
        let mut airtime = airtime_with(items, 0.0);
        let mut plan = Plan::default();
        airtime.assign(&records, [busy(true, false); DECKS], &mut plan);
        assert_eq!(plan.load.iter().map(|(d, _)| *d).collect::<Vec<_>>(), vec![1, 0]);
    }

    #[test]
    fn clock_advances_between_polls_and_finished_decks_keep_refilling() {
        let mut items = pair();
        let mut third = items[1].clone();
        third.id = "third".into();
        third.start_at = 388.0;
        items.push(third);
        let records = library_for(&items);
        let mut airtime = airtime_with(items, 199.0);
        airtime.anchor = Some((199.0, 0));
        airtime.last_poll = Instant::now();
        airtime.last_queue = Instant::now();
        for (deck, id) in ["43ac", "next"].into_iter().enumerate() {
            airtime.decks[deck] = Some(Assigned { id: id.into(), ready: true, started: true });
        }
        let plan = airtime.tick(96_000, &records, [busy(true, true); DECKS], true);
        assert_eq!(airtime.station_now, 201.0);
        assert_eq!(plan.stop, vec![0]);
        let refill = airtime.tick(96_480, &records, [busy(true, false), busy(true, true)], true);
        assert_eq!(refill.load.len(), 1);
        assert_eq!(refill.load[0].0, 0);
        airtime.deck_ready(0);
        let play = airtime.tick(189 * 48_000, &records, [busy(true, false), busy(true, true)], true);
        assert!(play.start.iter().any(|(d, _)| *d == 0), "third record never started");
    }

    #[test]
    fn actual_deck_mixer_preserves_both_envelopes_including_ducks_and_cuts() {
        for levels in [[1.0, 1.0], [0.22, 0.22], [0.75, 0.25], [0.0, 1.0], [0.0, 0.0]] {
            let (x, gain) = mixer_levels(levels);
            for deck in 0..DECKS {
                let mut audio = crate::engine::deck::Deck::new(48_000);
                audio.track = Some(Arc::new(Track { samples: vec![0.1; 200], sample_rate: 48_000 }));
                audio.playing = true;
                let position = if deck == 0 { x } else { 1.0 - x };
                audio.gain = gain * (position * std::f32::consts::FRAC_PI_2).cos();
                let mut out = [0.0; 100];
                audio.mix_into(&mut out, 48_000);
                assert!((out[80] - 0.1 * levels[deck]).abs() < 0.0001, "{levels:?}: {out:?}");
                assert!(audio.seconds() > 0.0, "a muted deck stopped advancing");
            }
        }
    }

    #[test]
    fn an_incoming_decode_does_not_fade_or_filter_the_only_ready_deck() {
        let mut airtime = airtime_with(pair(), 197.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), ready: true, started: true });
        airtime.decks[1] = Some(Assigned { id: "next".into(), ready: false, started: false });
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(true, false)], &mut plan);
        assert_eq!(plan.gain, [Some(1.0), Some(0.0)]);
        assert_eq!(plan.tone[0], Some([0.5, 0.5, 0.5, 0.0]));
        assert!(plan.start.is_empty());
    }

    #[test]
    fn station_stopping_pauses_the_music_decks_as_well_as_speech() {
        let mut airtime = airtime_with(pair(), 197.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), ready: true, started: true });
        let plan = airtime.tick(0, &[], [busy(true, true); DECKS], false);
        assert_eq!(plan.stop, vec![0]);
        assert!(!airtime.on);
        assert!(plan.voice.iter().any(|c| matches!(c, Command::OffAir)));
    }

    #[test]
    fn an_old_poll_cannot_replace_a_new_opening_lineup() {
        let mut airtime = airtime_with(pair(), 0.0);
        airtime.start_with(serde_json::json!([]));
        airtime.last_poll = Instant::now();
        airtime.last_queue = Instant::now();
        airtime.poll_out.send((airtime.session - 1, Ok(real()), false)).unwrap();
        airtime.tick(0, &[], [busy(true, false); DECKS], true);
        assert!(airtime.schedule.is_empty());
        assert!(airtime.startup.is_some());
    }

    #[test]
    fn rejected_opening_tracks_stop_retrying_and_restore_channel_levels() {
        let mut airtime = airtime_with(pair(), 0.0);
        airtime.start_with(serde_json::json!([]));
        airtime.last_queue = Instant::now();
        airtime.poll_out.send((airtime.session, Err("opening track missing".into()), true)).unwrap();
        let plan = airtime.tick(0, &[], [busy(true, false); DECKS], true);
        assert!(!airtime.on);
        assert_eq!(plan.gain, [Some(1.0); DECKS]);
        assert_eq!(plan.note.as_deref(), Some("opening track missing"));
    }

    #[test]
    fn speech_ducks_held_faders_without_ducking_the_transition_twice() {
        let mut items = pair();
        items[0].deck_envelope = vec![[0.0, 1.0], [200.0, 1.0]];
        items[0].envelope = vec![[0.0, 0.1], [200.0, 0.1]];
        let mut speech = items[1].clone();
        speech.kind = "voice".into();
        speech.id = "speech".into();
        speech.start_at = 10.0;
        speech.duration = 10.0;
        items.push(speech);
        let mut airtime = airtime_with(items, 12.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.held.gain[0] = true;
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert_eq!(plan.gain[0], None);
        assert_eq!(plan.duck, Some(0.1));
        airtime.held.gain[0] = false;
        airtime.drive([busy(true, true), busy(false, false)], &mut plan);
        assert_eq!(plan.gain[0], Some(1.0));
        assert_eq!(airtime.speech_duck(21.7), 1.0);
    }

    #[test]
    fn pitched_records_start_at_the_correct_source_position() {
        let mut items = pair();
        items[0].playback_rate = 0.95;
        let mut airtime = airtime_with(items, 20.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: false, ready: true });
        let mut plan = Plan::default();
        airtime.drive([busy(true, false); DECKS], &mut plan);
        assert_eq!(plan.start, vec![(0, 19.0)]);
        assert_eq!(plan.speed[0], Some(0.95));
    }

    fn following(position: f64, rate: f64) -> [DeckStatus; DECKS] {
        [DeckStatus { loaded: true, playing: true, position: Some(position), playback_rate: rate },
         DeckStatus::default()]
    }

    #[test]
    fn feedback_recovers_small_clock_error_without_a_seek() {
        let mut airtime = airtime_with(pair(), 20.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.started_at[0] = Some(0.0);
        let mut position = 19.8;
        let mut speed = 1.0;
        for _ in 0..600 {
            let mut plan = Plan::default();
            airtime.drive(following(position, speed), &mut plan);
            assert!(plan.start.is_empty(), "feedback must never seek audible playback");
            speed = plan.speed[0].unwrap_or(speed);
            assert!((speed - 1.0).abs() <= 0.005001);
            position += speed * 0.1;
            airtime.station_now += 0.1;
        }
        assert!((airtime.station_now - position).abs() < 0.035);
    }

    #[test]
    fn feedback_respects_manual_tempo_and_large_position_changes() {
        let mut airtime = airtime_with(pair(), 20.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.started_at[0] = Some(0.0);
        airtime.held.tempo[0] = true;
        let mut plan = Plan::default();
        airtime.drive(following(19.8, 0.95), &mut plan);
        assert_eq!(plan.speed[0], None);
        airtime.held.tempo[0] = false;
        airtime.drive(following(8.0, 1.0), &mut plan);
        assert_eq!(plan.speed[0], None);
        assert!(plan.start.is_empty());
        assert!(airtime.held.tempo[0]);
    }

    #[test]
    fn feedback_waits_for_start_telemetry_and_can_be_disabled() {
        let mut airtime = airtime_with(pair(), 20.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), started: true, ready: true });
        airtime.started_at[0] = Some(20.0);
        let mut plan = Plan::default();
        airtime.drive(following(0.0, 1.0), &mut plan);
        assert!(!airtime.held.tempo[0]);
        assert_eq!(plan.speed[0], None);
        airtime.started_at[0] = Some(0.0);
        airtime.schedule[0].playback_feedback[0] = 0.0;
        airtime.drive(following(19.8, 1.0), &mut plan);
        assert_eq!(plan.speed[0], None);
    }

    #[test]
    fn a_manual_tempo_on_the_cued_deck_survives_its_automatic_start() {
        let mut airtime = airtime_with(pair(), 194.0);
        airtime.decks[1] = Some(Assigned { id: "next".into(), started: false, ready: true });
        airtime.held.tempo[1] = true;
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(true, false)], &mut plan);
        assert_eq!(plan.start, vec![(1, 0.0)]);
        assert_eq!(plan.speed[1], None);
    }

    #[test]
    fn paused_outgoing_deck_does_not_leave_incoming_filtered_and_quiet() {
        let mut airtime = airtime_with(pair(), 197.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), ready: true, started: true });
        airtime.decks[1] = Some(Assigned { id: "next".into(), ready: true, started: true });
        let mut plan = Plan::default();
        airtime.drive([busy(true, false), busy(true, true)], &mut plan);
        assert_eq!(plan.gain, [Some(0.0), Some(1.0)]);
        assert_eq!(plan.crossfade, Some(1.0));
        assert_eq!(plan.tone[1], Some([0.5, 0.5, 0.5, 0.0]));
        assert!(plan.start.is_empty(), "a manual pause must not resume itself");
    }

    #[test]
    fn late_incoming_decode_rejoins_the_blend_gradually() {
        let mut airtime = airtime_with(pair(), 197.0);
        airtime.decks[0] = Some(Assigned { id: "43ac".into(), ready: true, started: true });
        airtime.decks[1] = Some(Assigned { id: "next".into(), ready: true, started: false });
        let mut plan = Plan::default();
        airtime.drive([busy(true, true), busy(true, false)], &mut plan);
        assert_eq!(plan.crossfade, Some(0.0));
        assert_eq!(plan.tone[0], Some([0.5, 0.5, 0.5, 0.0]));
        airtime.station_now += 0.25;
        let mut half = Plan::default();
        airtime.drive([busy(true, true); DECKS], &mut half);
        assert!(half.crossfade.unwrap() > 0.0);
        assert!(half.start.is_empty());
    }

    #[test]
    fn tempo_recovery_integrates_source_position_and_late_cues() {
        let mut item = pair().remove(0);
        item.offset = 7.0;
        item.playback_rate = 0.96;
        item.rate_curve = rate_curve_from(&serde_json::json!([[0,0.96],[10,0.96],[30,1.0]]));
        assert!((item.source_at(20.0) - 26.3).abs() < 1e-8);
        assert!((item.source_at(35.0) - 41.2).abs() < 1e-8);
        assert!((item.rate_at(20.0) - 0.98).abs() < 1e-8);
        assert_eq!(item.rate_at(35.0), 1.0);
        item.start_at = 0.0;
        let mut airtime = airtime_with(vec![item], 20.0);
        let id = airtime.schedule[0].id.clone();
        airtime.decks[0] = Some(Assigned { id, ready: true, started: false });
        let mut plan = Plan::default();
        airtime.drive([busy(true, false), DeckStatus::default()], &mut plan);
        assert!((plan.start[0].1 - 26.3).abs() < 1e-8);
        assert!((plan.speed[0].unwrap() - 0.98).abs() < 1e-8);
    }

    #[test]
    fn malformed_rate_curves_fall_back_to_constant_speed() {
        for value in [serde_json::json!([[1,1.0]]), serde_json::json!([[0,1.0],[0,1.02]]),
                      serde_json::json!([[0,1.0],[10,0.5]]), serde_json::json!([[0,1.0],[10,null]])] {
            assert!(rate_curve_from(&value).is_empty());
        }
    }
}
