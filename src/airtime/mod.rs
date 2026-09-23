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
//! a filter rise, six seconds of overlap -- is performed on the deck's own
//! controls: its level, its EQ knobs, its filter, its echo, its tempo.
//!
//! And it is performed by the engine. Each move is turned into an automation
//! curve stamped in output frames and sent once, when the plan arrives or
//! changes; the audio thread then plays it on its exact sample. The panel
//! reads the same curves to draw the knobs moving, but nothing waits on the
//! panel -- a minimised window mixes exactly as well as a watched one.
//!
//! Which means you can take any of it. Touch a control and that control's
//! lane lets go of it and stays yours; everything else carries on. That is
//! the whole point of doing it on the desk rather than beside it.
//!
//! Speech is the one thing that cannot go on a deck, because a deck holds a
//! record. Host lines go to a small voice bus with its own level, and the
//! decks duck underneath them -- which is a dip written into the station's own
//! gain envelope, so it is honoured rather than invented here.

mod automation;
mod clock;
mod planner;
mod protocol;
mod voice;

pub use automation::{band_knob, filter_fader, Mode};
pub use protocol::{queue_from, snapshot_from, Curve, MixWindow, QueueRow, Scheduled, Snapshot, Transition};

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::engine::{Command, Curve as LaneCurve, Lane};
use crate::library::Record;
use crate::station::client::{Backoff, Client, Handle, Subscription};
use crate::DECKS;

use automation::{Points, Situation};
use clock::Clock;
use protocol::{EventKind, Role};

/// How far ahead a voice line is fetched. Lines are seconds long and there
/// are a lot of them, so there is nothing to gain by reaching further.
///
/// Records have no equivalent window on purpose: the next one goes on the
/// spare deck the moment there is a spare deck, which is what a spare deck is
/// for.
const VOICE_LEAD_IN: f64 = 8.0;

/// How long before its air time a record is put on the engine's clock. Far
/// enough ahead that a busy UI frame cannot make it late; near enough that
/// the clock has barely moved since.
const ARM_AHEAD: f64 = 2.0;

const POLL: Duration = Duration::from_millis(1500);
/// With the events stream up the schedule arrives when it changes. It is
/// still asked for now and then: a round trip is the best clock sample
/// there is.
const POLL_STREAMING: Duration = Duration::from_secs(10);

/// The queue changes when you change it or when a request lands, neither of
/// which is often. No reason to ask as hard as for the schedule.
const QUEUE_POLL: Duration = Duration::from_millis(3000);

/// Starting the decks makes the station prepare two records and a mix.
const START_TIMEOUT: Duration = Duration::from_secs(8);
/// What a thumb, a request or a queue move may take.
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Re-send a deck's curves once the clock has moved this far under them.
const CLOCK_DRIFT: f64 = 0.015;

/// The echo return a written transition gets: `mix` 0.444 is a return of
/// 0.8, what the browser player uses.
pub const ECHO_RETURN: f32 = 0.444;

/* ── What autopilot wants done ───────────────────────────────────────── */

/// What a deck looks like from the outside, so autopilot can tell a deck it
/// is allowed to use from one you are using.
#[derive(Clone, Copy)]
pub struct DeckStatus {
    pub loaded: bool,
    pub playing: bool,
    /// Audio-thread telemetry, in source seconds. None disables timing feedback.
    pub position: Option<f64>,
    pub playback_rate: f64,
    /// Channel fader times trim: what a written `gain` lane rides on.
    pub base_gain: f32,
}

impl Default for DeckStatus {
    fn default() -> Self {
        DeckStatus { loaded: false, playing: false, position: None, playback_rate: 1.0, base_gain: 1.0 }
    }
}

/// One turn's worth of intent.
///
/// Plain data on purpose: it is what makes this testable without an audio
/// device, and it is where "the user has this one" is enforced -- a held
/// control's lane is simply absent from what is sent.
#[derive(Default)]
pub struct Plan {
    /// How far speech has the music down, for the panel.
    pub duck: Option<f32>,
    /// A new speed for the deck, for the pitch readout. The engine has it
    /// already, on the rate lane.
    pub speed: [Option<f64>; DECKS],
    pub key_lock: [Option<bool>; DECKS],
    /// Deck, the record, and the dB the station wants it trimmed by.
    pub load: Vec<(usize, Record, f32)>,
    /// Deck, and where in the record to start it.
    pub start: Vec<(usize, f64)>,
    /// The output frame each of those starts on.
    pub start_frame: [Option<u64>; DECKS],
    pub report_started: Vec<(String, String)>,
    pub stop: Vec<usize>,
    /// Where the curves have each deck's knobs and level right now, for the
    /// panel to draw. The engine is already there.
    pub tone: [Option<[f32; 4]>; DECKS],
    pub level: [Option<f32>; DECKS],
    pub crossfade: Option<f32>,
    /// Curves, clears and rolls for the engine.
    pub automation: Vec<Command>,
    /// Lanes a written transition has finished with, handed back so their
    /// controls can be put where the console had them.
    pub released: Vec<(usize, Lane)>,
    pub voice: Vec<Command>,
    /// Decks whose cached stems a coming technique needs loaded.
    pub stems: Vec<usize>,
    /// Give every deck back: lanes cleared, levels restored.
    pub restore: bool,
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

    fn release_deck(&mut self, deck: usize) {
        self.tone[deck] = [false; 4];
        self.gain[deck] = false;
        self.tempo[deck] = false;
        self.key_lock[deck] = false;
    }

    /// The engine lanes behind the controls you hold on a deck.
    fn lanes(&self, deck: usize) -> Vec<Lane> {
        let mut lanes = Vec::new();
        for (band, lane) in [Lane::Low, Lane::Mid, Lane::High, Lane::Sweep].into_iter().enumerate() {
            if self.tone[deck][band] { lanes.push(lane); }
        }
        if self.gain[deck] { lanes.push(Lane::Gain); }
        if self.tempo[deck] { lanes.push(Lane::Rate); }
        lanes
    }
}

/* ── The module itself ───────────────────────────────────────────────── */

struct Assigned {
    id: String,
    /// The record key the deck was given, so a load finishing for some other
    /// record is not mistaken for this one.
    key: String,
    /// True once the deck has been told to play it.
    started: bool,
    ready: bool,
    /// The console's load of this record, so a stale decode cannot mark it.
    generation: Option<u64>,
    /// The station time it was put on the clock for, and the frame.
    armed_at: Option<f64>,
    arm_frame: Option<u64>,
}

impl Assigned {
    fn new(id: &str, key: &str) -> Self {
        Assigned {
            id: id.to_string(), key: key.to_string(), started: false, ready: false,
            generation: None, armed_at: None, arm_frame: None,
        }
    }
}

/// What a deck's curves were built from.
#[derive(Clone, Debug, PartialEq)]
struct Sig {
    id: String,
    fingerprint: u64,
    next: u64,
    mode: Mode,
    skip: Vec<Lane>,
    hold_level: Option<u32>,
    speech: u64,
    base_gain: i32,
}

/// What was sent to a deck, and against which clock.
struct Sent {
    sig: Sig,
    rate: (i64, bool),
    curves: Vec<(Lane, Arc<LaneCurve>)>,
    ref_frame: u64,
    ref_time: f64,
    clear_after: Vec<(Lane, u64)>,
}

impl Sent {
    fn curve(&self, lane: Lane) -> Option<&Arc<LaneCurve>> {
        self.curves.iter().find(|(l, _)| *l == lane).map(|(_, curve)| curve)
    }
}

/// A deck's curves, ready to send.
struct Prepared {
    sig: Sig,
    rate_sig: (i64, bool),
    whole: bool,
    rate_only: bool,
    converted: Vec<(Lane, Arc<LaneCurve>)>,
    fresh_clears: Vec<(Lane, u64)>,
    echo: Option<[f32; 5]>,
    /// Written echo lanes need the return opened; the delay to open it at.
    echo_opened: Option<f32>,
    skip: Vec<Lane>,
    rate_now: Option<f32>,
}

/// Tempo feedback on one deck.
#[derive(Clone, Copy, Default)]
struct Correction {
    applied: f64,
    /// When the error first went past what feedback should chase.
    big_since: Option<f64>,
    sent_at: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
enum Ask {
    Schedule(u64),
    Start(u64),
    Queue,
    Action,
}

pub struct Airtime {
    pub chat: crate::ui::director_chat::Chat,
    root: PathBuf,

    pub on: bool,
    pub held: Held,
    pub note: Option<String>,

    epoch: i64,
    session: u64,
    startup: Option<serde_json::Value>,
    startup_retry: Backoff,
    startup_next: Instant,
    startup_since: Instant,
    startup_said: bool,
    /// The station restarted under us; the next opening lineup is the one
    /// already playing.
    recovering: bool,
    station_ready: bool,
    skip_pending: Option<String>,
    failed: HashSet<String>,
    /// Records whose audio could not be found: when that was first noticed,
    /// and when it was last looked for.
    missing: HashMap<String, (Instant, Instant)>,
    said_missing: HashSet<String>,
    resolved: HashMap<String, PathBuf>,
    clock: Clock,
    /// Which scheduled item each deck is carrying.
    decks: [Option<Assigned>; DECKS],
    reported_starts: HashSet<String>,
    voices: voice::Voices,
    sent: [Option<Sent>; DECKS],
    solo_since: [Option<f64>; DECKS],
    correction: [Correction; DECKS],
    /// The levels the decks were at when you took the crossfader.
    hold_level: Option<[f32; DECKS]>,
    /// Items whose rolls are on the engine, and the first of them as
    /// (station seconds, output frame), to see when the clock has moved
    /// under them.
    events_done: HashMap<String, Option<(f64, u64)>>,
    /// Lanes let go of outside a tick (a deck taken back, a load that
    /// failed), for the next plan to clear and hand back.
    let_go: Vec<(usize, Lane)>,
    /// Decks whose planned rolls are to be called off at the next plan.
    uncue: Vec<usize>,
    /// Voice channels to take off the bus at the next plan.
    voice_off: Vec<usize>,
    /// The item each deck's echo return was last set for, so a curve resent
    /// for some other reason does not reset a tail that is ringing.
    echo_for: [Option<String>; DECKS],
    stems_asked: HashSet<(usize, String)>,
    device: (u32, u64),

    client: Client,
    asks: Handle<Ask>,
    events: Option<Subscription>,
    last_poll: Instant,
    poll_failures: u32,
    last_queue: Instant,
    frame: u64,
    tick_at: Instant,

    pub schedule: Vec<Scheduled>,
    index: HashMap<String, usize>,
    /// Every speech item's span, sorted: what the music ducks under.
    speech: Vec<(f64, f64)>,
    speech_hash: u64,
    pub station_now: f64,

    /// The station's queue, as it last answered.
    pub queue: Vec<QueueRow>,
    /// What you have typed into the request box.
    pub request: String,
    pub request_is_vibe: bool,
    pub request_is_article: bool,
    pub article: String,
    pub catalogue: crate::spotify::Search,
    pub request_selection: Option<crate::spotify::Suggestion>,
}

impl Airtime {
    pub fn new(root: &Path, port: u16, sample_rate: u32) -> Self {
        Self::with_client(root, Client::new(port), port, sample_rate)
    }

    pub fn with_client(root: &Path, client: Client, port: u16, sample_rate: u32) -> Self {
        Airtime {
            chat: crate::ui::director_chat::Chat::new(port),
            root: root.to_path_buf(),
            on: false,
            held: Held::default(),
            note: None,
            epoch: i64::MIN,
            session: 0,
            startup: None,
            startup_retry: Backoff::new(Duration::from_secs(1), Duration::from_secs(8)),
            startup_next: Instant::now(),
            startup_since: Instant::now(),
            startup_said: false,
            recovering: false,
            station_ready: false,
            skip_pending: None,
            failed: HashSet::new(),
            missing: HashMap::new(),
            said_missing: HashSet::new(),
            resolved: HashMap::new(),
            clock: Clock::new(sample_rate),
            decks: [const { None }; DECKS],
            reported_starts: HashSet::new(),
            voices: voice::Voices::default(),
            sent: [const { None }; DECKS],
            solo_since: [None; DECKS],
            correction: [Correction::default(); DECKS],
            hold_level: None,
            events_done: HashMap::new(),
            let_go: Vec::new(),
            uncue: Vec::new(),
            voice_off: Vec::new(),
            echo_for: [const { None }; DECKS],
            stems_asked: HashSet::new(),
            device: (sample_rate, 0),
            asks: client.handle(),
            client,
            events: None,
            last_poll: Instant::now() - POLL,
            poll_failures: 0,
            last_queue: Instant::now() - QUEUE_POLL,
            frame: 0,
            tick_at: Instant::now(),
            schedule: Vec::new(),
            index: HashMap::new(),
            speech: Vec::new(),
            speech_hash: 0,
            station_now: 0.0,
            queue: Vec::new(),
            request: String::new(),
            request_is_vibe: false,
            request_is_article: false,
            article: String::new(),
            catalogue: crate::spotify::Search::new(root),
            request_selection: None,
        }
    }

    /// True when the station has something on a deck.
    pub fn live(&self) -> bool {
        self.decks.iter().any(|d| d.is_some())
    }

    pub fn host_levels(&self, peaks: &[f32; crate::engine::playout::CHANNELS]) -> [f32; 2] {
        let mut levels = [0.0f32; 2];
        if !self.on { return levels; }
        for (id, channel) in &self.voices.on_air {
            let Some(item) = self.item(id) else { continue };
            if self.station_now < item.start_at || self.station_now >= item.ends_at() { continue; }
            let host = match item.host.to_lowercase().as_str() { "mav" => 0, "rue" => 1, _ => continue };
            levels[host] = levels[host].max(peaks.get(*channel).copied().unwrap_or(0.0));
        }
        levels
    }

    /// An item by id: through the index, and by a search when the index is
    /// behind the schedule.
    fn item(&self, id: &str) -> Option<&Scheduled> {
        self.index.get(id).and_then(|&i| self.schedule.get(i)).filter(|item| item.id == id)
            .or_else(|| self.schedule.iter().find(|item| item.id == id))
    }

    /// What this deck is carrying, for the panel to label.
    pub fn on_deck(&self, deck: usize) -> Option<&Scheduled> {
        let assigned = self.decks.get(deck)?.as_ref()?;
        self.item(&assigned.id)
    }

    /// Replace the schedule, and everything worked out from it.
    fn set_schedule(&mut self, items: Vec<Scheduled>) {
        self.schedule = items;
        self.index = self.schedule.iter().enumerate().map(|(i, item)| (item.id.clone(), i)).collect();
        let mut speech: Vec<(f64, f64)> = self.schedule.iter().filter(|i| !i.is_music())
            .map(|i| (i.start_at, i.ends_at())).collect();
        speech.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (a, b) in &speech { a.to_bits().hash(&mut hasher); b.to_bits().hash(&mut hasher); }
        self.speech_hash = hasher.finish();
        self.speech = speech;
        let present: HashSet<&str> = self.schedule.iter().map(|i| i.id.as_str()).collect();
        self.reported_starts.retain(|id| present.contains(id.as_str()));
        self.resolved.retain(|id, _| present.contains(id.as_str()));
        self.missing.retain(|id, _| present.contains(id.as_str()));
        self.said_missing.retain(|id| present.contains(id.as_str()));
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
        self.set_schedule(snapshot.items);
        for deck in 0..DECKS {
            self.decks[deck] = self.schedule.get(deck).map(|item| {
                let mut assigned = Assigned::new(&item.id, &item.id);
                assigned.started = deck == 0;
                assigned.ready = true;
                assigned
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
        let query = if self.request_is_article { &self.article } else { &self.request }.trim().to_string();
        if query.is_empty() {
            return;
        }
        let mode = if self.request_is_article { "article" } else if self.request_is_vibe { "vibe" } else { "request" };
        let selection = self.request_selection.take().filter(|s| mode == "request" && s.query() == query);
        if !self.request_is_article { self.request.clear(); }
        self.catalogue.clear();
        self.post("/api/request", Some(serde_json::json!({ "query": query, "mode": mode,
                   "selection": selection.map(|s| s.payload()) })));
        self.note = Some(if self.request_is_article { "Sending article. Progress appears in the queue; draft kept here.".into() }
                        else { format!("{}: {query}", if self.request_is_vibe { "Setting vibe" } else { "Asked for" }) });
        self.last_queue = Instant::now() - QUEUE_POLL;
    }

    pub fn clear_vibe(&mut self) {
        self.post("/api/vibe/clear", None);
    }

    pub fn request_ad(&mut self, immediately: bool) {
        self.post("/api/ads", Some(serde_json::json!({
            "timing": if immediately { "now" } else { "next_break" }
        })));
        self.note = Some("Preparing a comedy ad...".into());
    }

    pub fn report_started(&mut self, item_id: &str, key: &str) {
        self.post("/api/report", Some(serde_json::json!({"kind": "started", "item_id": item_id, "key": key})));
    }

    pub fn save_mix_settings(&mut self, values: serde_json::Value) {
        self.post("/api/mix/config", Some(values));
        self.note = Some("Mix settings sent. New transitions use the new choices.".into());
    }

    /// Fire a POST at the station and say so if it refuses.
    ///
    /// Without waiting: none of these answer with anything the console
    /// needs, because the next schedule says what actually happened.
    fn post(&mut self, path: &str, body: Option<serde_json::Value>) {
        self.asks.post(Ask::Action, path, body, ACTION_TIMEOUT);
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
        self.recovering = false;
        self.last_poll = Instant::now() - POLL;
        self.clock.reset();
        self.set_schedule(Vec::new());
        self.failed.clear();
        self.reported_starts.clear();
        self.events_done.clear();
        self.stems_asked.clear();
        self.epoch = i64::MIN;
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
        self.opening(tracks);
    }

    /// The station came back after a crash. Hand it what the decks are
    /// playing, and keep playing it: the new session carries on from there.
    pub fn resume_with(&mut self, tracks: serde_json::Value) {
        if !self.on {
            return;
        }
        self.session = self.session.wrapping_add(1);
        self.epoch = i64::MIN;
        self.recovering = true;
        self.opening(tracks);
    }

    fn opening(&mut self, tracks: serde_json::Value) {
        let token = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_nanos().to_string();
        self.startup = Some(serde_json::json!({"session": token, "tracks": tracks}));
        self.startup_retry.reset();
        self.startup_next = Instant::now();
        self.startup_since = Instant::now();
        self.startup_said = false;
    }

    /// Whether the station has answered since it was started. The opening
    /// lineup waits for that, rather than being refused by a station still
    /// importing its library.
    pub fn station_ready(&mut self, ready: bool) {
        self.station_ready = ready;
    }

    /// The audio device's rate and how often it has been reopened. A change
    /// moves the frame clock under everything placed on it.
    ///
    /// Everything placed in frames is placed again: every deck's curves,
    /// the host lines on the bus (which come back off at once and are
    /// fetched again), and the rolls, which follow the clock by themselves.
    pub fn device(&mut self, rate: u32, restarts: u64, frame: u64) -> bool {
        if (rate, restarts) == self.device || rate == 0 {
            return false;
        }
        self.device = (rate, restarts);
        self.frame = frame;
        self.clock.device(rate, restarts, frame);
        for sent in self.sent.iter_mut() {
            if let Some(sent) = sent.as_mut() {
                // Measured against nothing, so the next plan sends it whole.
                sent.ref_time = f64::NAN;
            }
        }
        self.voice_off.extend(self.voices.on_air.values().copied());
        self.voices.clear_air();
        true
    }

    /// The console is loading `generation` of a record onto this deck.
    pub fn loading(&mut self, deck: usize, generation: u64) {
        if let Some(assigned) = self.decks[deck].as_mut() {
            assigned.generation = Some(generation);
        }
    }

    /// A load finished: this record, this generation, on this deck.
    pub fn deck_ready(&mut self, deck: usize, key: &str, generation: u64) {
        if let Some(assigned) = self.decks[deck].as_mut() {
            if assigned.key == key && assigned.generation == Some(generation) {
                assigned.ready = true;
            }
        }
    }

    pub fn deck_failed(&mut self, deck: usize, key: &str, generation: u64) {
        let ours = self.decks[deck].as_ref()
            .is_some_and(|a| a.key == key && a.generation == Some(generation));
        if !ours {
            return;
        }
        if let Some(assigned) = self.decks[deck].take() {
            self.drop_lanes(deck);
            let due = self.item(&assigned.id).is_some_and(|i| i.start_at <= self.station_now);
            self.failed.insert(assigned.id.clone());
            self.give_up(&assigned.id, due);
        }
    }

    /// You loaded something of your own onto a deck the station had. The
    /// deck is yours; the station's record on it will not play here.
    pub fn release_deck(&mut self, deck: usize) {
        if let Some(assigned) = self.decks[deck].take() {
            self.failed.insert(assigned.id);
        }
        self.drop_lanes(deck);
        self.solo_since[deck] = None;
        self.correction[deck] = Correction::default();
        self.held.release_deck(deck);
    }

    /// Forget a deck's curves, and see that the engine lets go of them too:
    /// a lane left on a deck keeps its last value into whatever plays next.
    fn drop_lanes(&mut self, deck: usize) {
        if let Some(sent) = self.sent[deck].take() {
            self.let_go.extend(sent.curves.iter().map(|(lane, _)| (deck, *lane)));
        }
        self.echo_for[deck] = None;
        self.uncue.push(deck);
    }

    /// What `drop_lanes` put aside, into a plan.
    fn let_go_into(&mut self, plan: &mut Plan) {
        for (deck, lane) in self.let_go.drain(..) {
            plan.automation.push(Command::ClearAutomation { deck, lane });
            plan.released.push((deck, lane));
        }
        for deck in self.uncue.drain(..) {
            plan.automation.push(Command::Loop { deck, range: None });
        }
        for channel in self.voice_off.drain(..) {
            plan.voice.push(Command::Air { channel, item: None });
        }
    }

    fn forget(&mut self) {
        self.decks = [const { None }; DECKS];
        self.sent = [const { None }; DECKS];
        self.solo_since = [None; DECKS];
        self.correction = [Correction::default(); DECKS];
        self.hold_level = None;
        self.voices.clear();
        self.held.release();
    }

    /// A deck's record is over, or gone: let go of it.
    fn finish(&mut self, deck: usize, plan: &mut Plan) {
        self.decks[deck] = None;
        self.drop_lanes(deck);
        self.let_go_into(plan);
        self.solo_since[deck] = None;
        self.correction[deck] = Correction::default();
        plan.stop.push(deck);
        // A record you took over is only yours until it ends.
        self.held.release_deck(deck);
    }

    /// Work out what should happen this frame.
    pub fn tick(
        &mut self,
        frame: u64,
        records: &[Record],
        decks: [DeckStatus; DECKS],
        station_running: bool,
    ) -> Plan {
        let mut plan = Plan::default();
        self.frame = frame;
        self.tick_at = Instant::now();

        for message in self.voices.arrived() {
            match message {
                voice::Fetched::Voice { id, track } => {
                    if self.on {
                        self.air_voice(&id, track, &mut plan);
                    }
                }
                voice::Fetched::Failed { error, .. } => self.note = Some(error),
            }
        }

        self.receive(&mut plan);
        self.let_go_into(&mut plan);

        if self.on && !station_running {
            plan.stop.extend((0..DECKS).filter(|d| self.decks[*d].is_some()));
            plan.voice.push(Command::OffAir);
            plan.restore = true;
            self.set_on(false);
        }
        if self.on {
            if let Some(now) = self.clock.station_at(frame) {
                self.station_now = now;
            }
            // Let go of a deck the moment its record is over. Waiting for the
            // next poll to notice costs a second and a half of the window the
            // following record has to load in.
            let now = self.station_now;
            for deck in 0..DECKS {
                if self.on_deck(deck).is_some_and(|item| item.ends_at() <= now) {
                    self.finish(deck, &mut plan);
                }
            }
            if self.startup.is_none() {
                self.assign(records, decks, &mut plan);
                self.drive(decks, &mut plan);
                if self.take_ready_skip() {
                    self.post("/api/skip", None);
                }
            }
            self.poll();
        }
        // Worth watching whenever the station is up, whether or not this
        // console is the thing playing it -- but never when it is not, or the
        // panel fills with complaints about a station nobody started.
        if station_running {
            // A stream that went quiet without closing is replaced.
            if self.events.as_ref().is_some_and(|e| e.stale()) {
                self.events = None;
            }
            if self.events.is_none() && self.station_ready {
                self.events = Some(self.client.subscribe(&["schedule", "queue"]));
            }
            self.poll_queue();
        } else {
            self.events = None;
            if !self.queue.is_empty() {
                self.queue.clear();
            }
        }

        if let Some(note) = self.note.take() {
            plan.note = Some(note);
        }
        plan
    }

    fn streaming(&self) -> bool {
        self.events.as_ref().is_some_and(|e| e.live())
    }

    /// The output frame an `Instant` this tick has heard about fell on.
    fn frame_of(&self, at: Instant) -> f64 {
        let rate = self.clock.rate() as f64;
        match self.tick_at.checked_duration_since(at) {
            Some(before) => self.frame as f64 - before.as_secs_f64() * rate,
            None => self.frame as f64 + at.duration_since(self.tick_at).as_secs_f64() * rate,
        }
    }

    /// Everything the station has said since last time.
    fn receive(&mut self, plan: &mut Plan) {
        for done in self.asks.poll() {
            let sample = (self.frame_of(done.midpoint()), done.round_trip().as_secs_f64());
            match done.tag {
                Ask::Schedule(session) | Ask::Start(session) if session != self.session => {}
                Ask::Schedule(_) => match done.outcome {
                    Ok(body) => {
                        self.poll_failures = 0;
                        if self.on && self.startup.is_none() {
                            self.absorb(snapshot_from(&body, 0), Some(sample), plan);
                        }
                    }
                    Err(failure) => {
                        self.poll_failures += 1;
                        // One missed poll is nothing; the stream or the next
                        // poll will do. A run of them is worth a word.
                        if self.on && self.poll_failures == 3 {
                            self.note = Some(failure.message);
                        }
                    }
                },
                Ask::Start(_) => match done.outcome {
                    Ok(body) => {
                        self.startup = None;
                        if self.on {
                            self.absorb(snapshot_from(&body, 0), Some(sample), plan);
                        }
                    }
                    Err(failure) if failure.refused() => {
                        // The station understood and said no: the opening
                        // lineup is wrong, and asking again will not help.
                        if self.on {
                            self.note = Some(format!("Could not start deck playback: {}", failure.message));
                            self.set_on(false);
                            plan.restore = true;
                        }
                    }
                    Err(failure) => {
                        // Not ready yet, or busy: try again, less and less
                        // often, and only mention it if it goes on.
                        self.startup_next = Instant::now() + self.startup_retry.next();
                        if !self.startup_said && self.startup_since.elapsed() > Duration::from_secs(30) {
                            self.startup_said = true;
                            self.note = Some(format!("The station is still getting ready ({}).", failure.message));
                        }
                    }
                },
                Ask::Queue => {
                    if let Ok(body) = done.outcome {
                        self.queue = queue_from(&body);
                    }
                }
                Ask::Action => match done.outcome {
                    Ok(body) => {
                        if let Some(message) = body["message"].as_str() {
                            self.note = Some(message.to_string());
                        }
                    }
                    Err(failure) => self.note = Some(failure.message),
                },
            }
        }

        let mut pushed = Vec::new();
        if let Some(events) = self.events.as_ref() {
            while let Ok(event) = events.events.try_recv() {
                pushed.push(event);
            }
        }
        for event in pushed {
            match event.topic.as_str() {
                "schedule" if self.on && self.startup.is_none() => {
                    // One way, so its latency is half a round trip nobody
                    // measured: it counts, but a real round trip beats it.
                    let sample = (self.frame_of(event.at), 0.004);
                    self.absorb(snapshot_from(&event.data, 0), Some(sample), plan);
                }
                "queue" => self.queue = queue_from(&event.data),
                _ => {}
            }
        }
    }

    fn poll(&mut self) {
        if let Some(body) = self.startup.as_ref() {
            let start = Ask::Start(self.session);
            if self.station_ready && !self.asks.busy(&start) && Instant::now() >= self.startup_next {
                let body = body.clone();
                self.asks.post(start, "/api/decks/start", Some(body), START_TIMEOUT);
                self.last_poll = Instant::now();
            }
            return;
        }
        let every = if self.streaming() {
            POLL_STREAMING
        } else {
            // A station that is not answering is asked less often.
            POLL * (1 + self.poll_failures.min(3))
        };
        let schedule = Ask::Schedule(self.session);
        if self.asks.busy(&schedule) || self.last_poll.elapsed() < every {
            return;
        }
        self.last_poll = Instant::now();
        self.asks.get(schedule, "/api/schedule");
    }

    fn poll_queue(&mut self) {
        if self.asks.busy(&Ask::Queue) || self.streaming() || self.last_queue.elapsed() < QUEUE_POLL {
            return;
        }
        self.last_queue = Instant::now();
        self.asks.get(Ask::Queue, "/api/queue");
    }

    /// Take in a schedule. `sample` is when the station read its clock, as
    /// (output frame, round trip seconds), when that is known.
    fn absorb(&mut self, snapshot: Snapshot, sample: Option<(f64, f64)>, plan: &mut Plan) {
        let jumped = snapshot.epoch != self.epoch;
        let first = self.epoch == i64::MIN;
        self.epoch = snapshot.epoch;
        match sample {
            Some((frame, _)) if jumped => self.clock.jumped(snapshot.now, frame),
            Some((frame, rtt)) => { self.clock.observe(snapshot.now, frame, rtt, self.frame); }
            None => self.clock.jumped(snapshot.now, self.frame as f64),
        }
        self.station_now = self.clock.station_at(self.frame).unwrap_or(snapshot.now);
        let now = self.station_now;

        if std::mem::take(&mut self.recovering) {
            // The new session's lineup is the records already on the decks,
            // under new names. Keep them playing; only the curves change.
            for deck in 0..DECKS {
                let Some(assigned) = self.decks[deck].as_mut() else { continue };
                if let Some(item) = snapshot.items.iter().find(|item| item.is_music()
                    && (item.key == assigned.key || item.id == assigned.id)
                    && item.preferred_deck.is_none_or(|d| d == deck)) {
                    assigned.id = item.id.clone();
                    assigned.armed_at = Some(now);
                    self.sent[deck] = None;
                    self.correction[deck] = Correction::default();
                }
            }
        }

        self.set_schedule(snapshot.items);

        // A skip moves the station clock. The records on the decks are still
        // the right records -- the clock jumped, not the lineup -- so they are
        // re-cued where they now belong rather than torn down and reloaded,
        // which would put a hole in the output exactly where the skip is.
        if jumped && !first {
            // Speech is scheduled to the sample against the old clock, so it
            // is the one thing that really is wrong now.
            plan.voice.push(Command::OffAir);
            self.voices.clear_air();
            self.events_done.clear();

            for deck in 0..DECKS {
                let Some(assigned) = self.decks[deck].as_ref() else { continue };
                if self.item(&assigned.id).is_some() {
                    // Rolls placed against the old clock would fire at the
                    // wrong moment; the re-cue places them again.
                    plan.automation.push(Command::Loop { deck, range: None });
                    let assigned = self.decks[deck].as_mut().unwrap();
                    assigned.started = false; // `drive` will put it where it goes.
                    assigned.armed_at = None;
                    assigned.arm_frame = None;
                    self.sent[deck] = None;
                    self.solo_since[deck] = None;
                    self.correction[deck] = Correction::default();
                } else {
                    self.finish(deck, plan);
                }
            }
            self.note = Some("Skipped.".into());
        }

        // Let go of decks whose record has finished, so the next one can have
        // them. A scheduled outro can end before the source file does.
        for deck in 0..DECKS {
            let done = self.decks[deck].as_ref()
                .is_some_and(|assigned| self.item(&assigned.id).is_none_or(|item| item.ends_at() <= now));
            if done {
                self.finish(deck, plan);
            }
        }

        let schedule = &self.schedule;
        self.voices.release_ended(|id| {
            schedule.iter().find(|item| item.id == id).is_some_and(|item| item.ends_at() >= now - 0.25)
        });
    }

    fn air_voice(&mut self, id: &str, track: Arc<crate::engine::decode::Track>, plan: &mut Plan) {
        let Some(item) = self.item(id).cloned() else { return };
        match self.voices.air(&item, track, &self.clock, self.frame) {
            Ok(Some(command)) => plan.voice.push(command),
            Ok(None) => {}
            Err(error) => self.note = Some(error),
        }
    }

    /// The part that actually performs. Everything here is a control on the
    /// panel, and everything here is left alone if you are holding it.
    fn drive(&mut self, decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        if !self.clock.anchored() {
            self.clock.anchor(now, self.frame);
        }
        plan.duck = Some(self.speech_duck(now));
        let rate = self.clock.rate() as f64;

        // Put each record on the engine's clock shortly before its air time,
        // to start on its frame; or now, as far in as it should be, if that
        // has passed.
        for deck in 0..DECKS {
            let Some(assigned) = self.decks[deck].as_ref() else { continue };
            if !assigned.ready || !decks[deck].loaded || plan.load.iter().any(|(d, _, _)| *d == deck) {
                continue;
            }
            let Some(item) = self.item(&assigned.id) else { continue };
            let (start_at, key_lock) = (item.start_at, item.key_lock);
            if assigned.started {
                // Armed ahead of time: if the clock has moved since, move
                // the start with it.
                let (Some(frame), Some(target)) = (assigned.arm_frame, self.clock.frame_at(start_at)) else { continue };
                if now < start_at && (target as f64 - frame as f64).abs() > 0.01 * rate {
                    let offset = item.offset;
                    plan.start.push((deck, offset));
                    plan.start_frame[deck] = Some(target);
                    if let Some(assigned) = self.decks[deck].as_mut() { assigned.arm_frame = Some(target); }
                }
                continue;
            }
            if start_at - now > ARM_AHEAD {
                continue;
            }
            let target = start_at.max(now);
            let elapsed = target - start_at;
            let source = item.source_at(elapsed);
            let speed = item.rate_at(elapsed);
            let Some(frame) = self.clock.frame_at(target) else { continue };
            if !self.held.key_lock[deck] { plan.key_lock[deck] = Some(key_lock); }
            plan.start.push((deck, source));
            plan.start_frame[deck] = Some(frame);
            if !self.held.tempo[deck] {
                plan.speed[deck] = Some(speed);
            }
            self.correction[deck] = Correction::default();
            if let Some(assigned) = self.decks[deck].as_mut() {
                assigned.started = true;
                assigned.armed_at = Some(target);
                assigned.arm_frame = Some(frame);
            }
        }

        self.follow_playheads(decks, plan);

        // Report actual deck playback, including an already-playing preload.
        // Keep network work out of this planner; the app applies the report.
        for deck in 0..DECKS {
            let Some(assigned) = &self.decks[deck] else { continue };
            let Some(item) = self.on_deck(deck) else { continue };
            if assigned.ready && assigned.started && decks[deck].playing
                && item.start_at <= now && now < item.ends_at()
                && !self.reported_starts.contains(&item.id) {
                let (id, key) = (item.id.clone(), item.key.clone());
                self.reported_starts.insert(id.clone());
                plan.report_started.push((id, key));
            }
        }

        let modes = self.modes(decks);
        self.automate(modes, decks, plan);
        self.dispatch_events(plan);
        self.want_stems(plan);
        self.show(decks, plan);
    }

    /// A technique that moves stems needs the decks separated before it
    /// starts. Only a separation already in the cache will do -- there is
    /// no time to make one mid-show -- and without one the stem lanes do
    /// nothing, which the station's own envelope still covers.
    fn want_stems(&mut self, plan: &mut Plan) {
        let needs = |transition: Option<&Transition>| {
            transition.is_some_and(|t| t.requires.iter().any(|r| r == "stems"))
        };
        for deck in 0..DECKS {
            let Some(assigned) = self.decks[deck].as_ref() else { continue };
            if !assigned.ready {
                continue;
            }
            let Some(item) = self.item(&assigned.id) else { continue };
            let wanted = needs(item.transition.as_ref())
                || self.next_music(item).is_some_and(|next| needs(next.transition.as_ref()));
            let key = (deck, item.id.clone());
            if wanted && !self.stems_asked.contains(&key) {
                self.stems_asked.insert(key);
                plan.stems.push(deck);
            }
        }
    }

    /// Send each deck's curves if what they were built from has changed.
    fn automate(&mut self, modes: [automation::Mode; DECKS], decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        let Some(frame_now) = self.clock.frame_at(now) else { return };
        for deck in 0..DECKS {
            let Some(Prepared { sig, rate_sig, whole, rate_only, converted, fresh_clears, echo, echo_opened, skip, rate_now })
                = self.prepare(deck, modes[deck], decks[deck], frame_now) else { continue };

            let previous = self.sent[deck].take();
            let mut curves: Vec<(Lane, Arc<LaneCurve>)> = Vec::new();
            for (lane, curve) in converted {
                if rate_only && lane != Lane::Rate {
                    if let Some(curve) = previous.as_ref().and_then(|p| p.curve(lane)) {
                        curves.push((lane, curve.clone()));
                    }
                    continue;
                }
                plan.automation.push(Command::Automate { deck, lane, curve: curve.clone() });
                curves.push((lane, curve));
            }
            // The echo return is set once per record: sending it again with
            // a resend for some other reason would reset a ringing tail.
            let echo_new = whole && self.echo_for[deck].as_deref() != Some(sig.id.as_str());
            if echo_new {
                if let Some([_, _, seconds, mix, feedback]) = echo {
                    plan.automation.push(Command::Echo { deck, mix, feedback, seconds });
                    self.echo_for[deck] = Some(sig.id.clone());
                } else if let Some(seconds) = echo_opened {
                    // Written echo lanes and no return lane: open the return
                    // (0.8 of the dry level, as the browser does) so the sends
                    // are heard. The lanes set the rest.
                    plan.automation.push(Command::Echo { deck, mix: ECHO_RETURN, feedback: 0.3, seconds });
                    self.echo_for[deck] = Some(sig.id.clone());
                }
            }
            if whole && previous.is_none() {
                // Nothing known to be on this deck: whatever a lane only a
                // written transition moves was left at, let it go.
                for lane in Lane::ALL {
                    if !automation::has_legacy(lane) && !curves.iter().any(|(l, _)| *l == lane) && !skip.contains(&lane) {
                        plan.automation.push(Command::ClearAutomation { deck, lane });
                        plan.released.push((deck, lane));
                    }
                }
            }
            if whole {
                // Lanes sent before and not now: held ones let go where they
                // are, the rest are the console's again.
                for (lane, _) in previous.iter().flat_map(|p| p.curves.iter()) {
                    if curves.iter().any(|(l, _)| l == lane) {
                        continue;
                    }
                    plan.automation.push(if skip.contains(lane) {
                        Command::Detach { deck, lane: *lane }
                    } else {
                        Command::ClearAutomation { deck, lane: *lane }
                    });
                    if !automation::has_legacy(*lane) {
                        plan.released.push((deck, *lane));
                    }
                }
            }
            if rate_only {
                if let Some(rate) = rate_now {
                    plan.speed[deck] = Some(rate as f64);
                }
            }
            let clear_after = if whole {
                fresh_clears
            } else {
                previous.as_ref().map(|p| p.clear_after.clone()).unwrap_or_default()
            };
            // A tempo-only resend leaves every other curve where it was
            // placed, so the clock is still measured from there.
            let (ref_frame, ref_time) = match previous.as_ref() {
                Some(p) if rate_only => (p.ref_frame, p.ref_time),
                _ => (frame_now, now),
            };
            self.sent[deck] = Some(Sent {
                sig,
                rate: rate_sig,
                curves,
                ref_frame,
                ref_time,
                clear_after,
            });
        }

        // Lanes a written transition has finished with.
        for deck in 0..DECKS {
            let Some(sent) = self.sent[deck].as_mut() else { continue };
            let (over, keep): (Vec<(Lane, u64)>, Vec<(Lane, u64)>) =
                sent.clear_after.iter().partition(|(_, at)| frame_now >= *at);
            sent.clear_after = keep;
            for (lane, _) in over {
                sent.curves.retain(|(l, _)| *l != lane);
                plan.automation.push(Command::ClearAutomation { deck, lane });
                plan.released.push((deck, lane));
            }
        }
    }

    /// A deck's curves, built -- if what they are built from has changed
    /// since they were last sent, or the clock has moved under them.
    fn prepare(&self, deck: usize, mode: automation::Mode, status: DeckStatus, frame_now: u64) -> Option<Prepared> {
        let now = self.station_now;
        let item = self.on_deck(deck)?;
        let next = self.next_music(item);
        let skip = self.held.lanes(deck);
        let hold_level = self.hold_level.map(|levels| levels[deck]);
        let sig = Sig {
            id: item.id.clone(),
            fingerprint: item.fingerprint,
            next: next.map_or(0, |n| n.fingerprint),
            mode,
            skip: skip.clone(),
            hold_level: hold_level.map(f32::to_bits),
            speech: self.speech_hash,
            base_gain: (status.base_gain * 100.0).round() as i32,
        };
        let correction = self.correction[deck].applied;
        let rate_sig = ((correction * 10_000.0).round() as i64, self.held.tempo[deck]);
        let sent = self.sent[deck].as_ref();
        let drifted = sent.is_some_and(|sent| {
            // Written so a reference that cannot be compared (NaN) counts.
            self.clock.station_at(sent.ref_frame).is_none_or(|t| !((t - sent.ref_time).abs() <= CLOCK_DRIFT))
        });
        let whole = drifted || sent.is_none_or(|sent| sent.sig != sig);
        let rate_only = !whole && sent.is_some_and(|sent| sent.rate != rate_sig);
        if !whole && !rate_only {
            return None;
        }
        let situation = Situation {
            item,
            next: next.and_then(|n| n.transition.as_ref()),
            mode,
            speech: &self.speech,
            hold_level,
            correction,
            now,
            base_gain: status.base_gain,
        };
        let built = automation::build(&situation, &skip);
        Some(Prepared {
            converted: built.lanes.iter()
                .map(|(lane, points)| (*lane, self.lane_curve(points, frame_now)))
                .collect(),
            fresh_clears: built.clear_after.iter()
                .filter_map(|&(lane, t)| Some((lane, self.clock.frame_at(t)?)))
                .collect(),
            rate_now: built.lane(Lane::Rate).and_then(|p| automation::value_at(p, now)),
            echo: item.echo,
            echo_opened: built.echo_written
                .then(|| item.beat_period.unwrap_or(0.5).clamp(0.03, 1.8) as f32),
            sig,
            rate_sig,
            whole,
            rate_only,
            skip,
        })
    }

    /// Station-clock points as an engine curve starting at `frame_now`.
    fn lane_curve(&self, points: &Points, frame_now: u64) -> Arc<LaneCurve> {
        let points = points.iter().filter_map(|&(t, v)| {
            Some((self.clock.frame_at_f(t)? - frame_now as f64, v))
        }).collect();
        Arc::new(LaneCurve::new(frame_now, points))
    }

    /// Rolls and loops the station wrote into a transition, once each.
    fn dispatch_events(&mut self, plan: &mut Plan) {
        let now = self.station_now;
        let mut sends = Vec::new();
        let mut done = Vec::new();
        for deck in 0..DECKS {
            let Some(assigned) = self.decks[deck].as_ref() else { continue };
            if !assigned.started || !assigned.ready {
                continue;
            }
            let Some(incoming) = self.item(&assigned.id) else { continue };
            let Some(transition) = incoming.transition.as_ref() else { continue };
            if transition.events.is_empty() {
                continue;
            }
            let previous = self.previous_music(incoming);
            let out_deck = previous.and_then(|p| self.deck_of(&p.id));
            if let Some(placed) = self.events_done.get(&incoming.id) {
                // Placed already. Place again only if none has started and
                // the clock has moved under them by more than 10 ms.
                let Some((at, frame)) = *placed else { continue };
                let moved = self.clock.frame_at(at)
                    .is_some_and(|now_frame| (now_frame as f64 - frame as f64).abs() > 0.01 * self.clock.rate() as f64);
                if now >= at || !moved {
                    continue;
                }
                sends.push(Command::Loop { deck, range: None });
                if let Some(out) = out_deck {
                    sends.push(Command::Loop { deck: out, range: None });
                }
            }
            let mut first: Option<(f64, u64)> = None;
            for event in &transition.events {
                let (target, item) = match event.role {
                    Role::In => (Some(deck), Some(incoming)),
                    Role::Out => (out_deck, previous),
                };
                let (Some(target), Some(item)) = (target, item) else { continue };
                if event.at < now {
                    continue;
                }
                // Seconds are as heard, at the deck's rate; the engine wants
                // seconds of record. Beats are the record's own already.
                let length = event.length_seconds.map(|s| s * item.rate_at(event.at - item.start_at))
                    .or_else(|| Some(event.length_beats? * item.beat_period?));
                let (Some(length), Some(frame), Some(until)) =
                    (length, self.clock.frame_at(event.at), self.clock.frame_at(event.until)) else { continue };
                // A loop plays as a roll as well: the record drops back in
                // where the station clock has it, not wherever the loop left
                // it, which would put every later move out of time.
                match event.kind {
                    EventKind::Roll | EventKind::Loop => sends.push(Command::LoopAt {
                        deck: target, frame, length_seconds: length, until_frame: until,
                    }),
                }
                if first.is_none_or(|(t, _)| event.at < t) {
                    first = Some((event.at, frame));
                }
            }
            done.push((incoming.id.clone(), first));
        }
        self.events_done.extend(done);
        plan.automation.extend(sends);
    }

    /// Where the curves have everything now, for the panel.
    fn show(&mut self, decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        let Some(frame) = self.clock.frame_at(now) else { return };
        let read = |sent: &Sent, lane: Lane| -> Option<f32> {
            sent.curve(lane).and_then(|curve| curve.at(frame, &mut 0))
        };
        let mut levels = [0.0f32; DECKS];
        let mut sounding = [false; DECKS];
        for deck in 0..DECKS {
            let Some(item) = self.on_deck(deck) else { continue };
            let live = self.sounding(deck, decks, now) && item.start_at <= now && now < item.ends_at();
            let sent = self.sent[deck].as_ref();
            let level = sent.and_then(|s| read(s, Lane::Level)).unwrap_or(1.0);
            sounding[deck] = live;
            levels[deck] = if live { level } else { 0.0 };
            plan.level[deck] = Some(levels[deck]);
            if let Some(sent) = sent {
                let mut tone = [0.5, 0.5, 0.5, 0.0];
                for (band, lane) in [Lane::Low, Lane::Mid, Lane::High, Lane::Sweep].into_iter().enumerate() {
                    if !self.held.tone[deck][band] {
                        if let Some(value) = read(sent, lane) { tone[band] = value; }
                    }
                }
                plan.tone[deck] = Some(tone);
            }
        }
        if !self.held.crossfade && sounding.iter().any(|s| *s) {
            plan.crossfade = Some(mixer_levels(levels).0);
        }
    }

    /// You took the crossfader: each deck keeps the level it had, and the
    /// crossfader and channel faders are yours from there.
    pub fn take_crossfader(&mut self, levels: [f32; DECKS]) {
        self.held.crossfade = true;
        let (_, gain) = mixer_levels(levels);
        self.hold_level = Some([gain.min(1.0); DECKS]);
    }

    /// Hand every control back to the station.
    pub fn return_to_auto(&mut self) {
        self.held.release();
        self.hold_level = None;
    }

    /// Follow the schedule with gentle rate changes, never by jumping a playing
    /// record. Large discontinuities belong to a person (seek/scratch/pause),
    /// not to a feedback controller trying to undo their action -- and a
    /// person's hand on the tempo holds it. A large error nobody made is
    /// waited out, since it is usually a stale reading; if it lasts, the
    /// deck is put back on the clock with one jump.
    fn follow_playheads(&mut self, decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        for deck in 0..DECKS {
            if self.held.tempo[deck] || plan.start.iter().any(|(d, _)| *d == deck) || !decks[deck].playing {
                continue;
            }
            let Some(assigned) = self.decks[deck].as_ref() else { continue };
            let Some(armed) = assigned.armed_at else { continue };
            if now - armed < 0.25 { continue; }
            let Some(item) = self.on_deck(deck) else { continue };
            let [enabled, maximum, tolerance] = item.playback_feedback;
            if now >= item.ends_at() || enabled == 0.0 { continue; }
            let Some(position) = decks[deck].position.filter(|p| p.is_finite()) else { continue };
            let elapsed = now - item.start_at;
            let error = item.source_at(elapsed) - position;
            let nominal = item.rate_at(elapsed);
            let mut state = self.correction[deck];
            if error.abs() > 0.75 {
                let since = *state.big_since.get_or_insert(now);
                if now - since > 3.0 {
                    state.big_since = None;
                    if let Some(assigned) = self.decks[deck].as_mut() { assigned.started = false; }
                    self.note = Some(format!("Deck {} drifted off the station clock; put it back.",
                                             if deck == 0 { "A" } else { "B" }));
                }
                self.correction[deck] = state;
                continue;
            }
            state.big_since = None;
            let wanted = if error.abs() <= tolerance { 0.0 }
                else { ((error / 8.0).clamp(-maximum, maximum) / 0.0005).round() * 0.0005 };
            // Changes go out at most twice a second, except letting go.
            let rested = wanted == 0.0 || state.sent_at.is_none_or(|at| now - at >= 0.5);
            if wanted != state.applied && rested {
                state.applied = wanted;
                state.sent_at = Some(now);
                plan.speed[deck] = Some((nominal * (1.0 + wanted)).clamp(0.92, 1.08));
            }
            self.correction[deck] = state;
        }
    }
}

/// Convert two desired amplitudes to equal-power fader position + gain.
fn mixer_levels(levels: [f32; 2]) -> (f32, f32) {
    let gain = levels[0].hypot(levels[1]);
    let crossfade = if gain > 0.000001 { levels[1].atan2(levels[0]) / std::f32::consts::FRAC_PI_2 } else { 0.5 };
    (crossfade, gain)
}

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

#[cfg(test)]
mod tests;
