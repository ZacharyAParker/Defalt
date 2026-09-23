//! What the station sends, read.
//!
//! The wire shapes are serde structs, and deliberately forgiving ones: a
//! field of the wrong type is read as absent rather than failing the item,
//! and an item that cannot be read at all is dropped rather than failing the
//! schedule. A newer station talking to an older console should lose a
//! feature, not the music.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::de::{DeserializeOwned, Deserializer};
use serde::Deserialize;
use serde_json::Value;

use crate::engine::Lane;
use crate::library::Record;

/* ── Curves ──────────────────────────────────────────────────────────── */

/// A breakpoint curve of (seconds into the item, value), linear between and
/// flat outside. The station publishes its automation in these.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curve(pub Vec<[f32; 2]>);

impl Curve {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.0
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
/// station reasons in. Turning them into knob positions is `automation`'s
/// job and nobody else's -- see `band_knob` and `filter_fader`.
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

/* ── Transitions ─────────────────────────────────────────────────────── */

/// Which deck of a transition something is about: the one playing the
/// record before (`out`) or the one this item is on (`in`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Out,
    In,
}

impl Role {
    fn named(name: &str) -> Option<Role> {
        match name {
            "out" | "outgoing" => Some(Role::Out),
            "in" | "incoming" => Some(Role::In),
            _ => None,
        }
    }
}

/// One lane of a transition, as the station wrote it: (station seconds,
/// value) on the engine's own scale for that lane.
#[derive(Clone, Debug, PartialEq)]
pub struct Authored {
    pub role: Role,
    pub lane: Lane,
    pub points: Vec<(f64, f32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A slip loop: loops, then drops back in where the record would be.
    Roll,
    /// A loop. Played as a roll too, so the record comes back in on the
    /// station clock rather than wherever the loop left it.
    Loop,
}

/// Something that happens at a moment rather than along a curve.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub role: Role,
    /// Station seconds.
    pub at: f64,
    pub until: f64,
    pub length_seconds: Option<f64>,
    pub length_beats: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Transition {
    pub preset: String,
    pub reason: String,
    pub overlap: f64,
    /// The technique by name, for whatever wants to say it.
    pub technique: String,
    /// The preset underneath a technique.
    pub base: String,
    /// Station seconds: the incoming record's one.
    pub switch_at: Option<f64>,
    /// Worth a host mention.
    pub flashy: bool,
    /// What the technique needs the decks to have: `stems`.
    pub requires: Vec<String>,
    /// Lanes for either deck, keyed by role. Present lanes take precedence
    /// over the legacy curves for the same control.
    pub lanes: Vec<Authored>,
    pub events: Vec<Event>,
}

impl Transition {
    pub fn lanes_for(&self, role: Role) -> impl Iterator<Item = &Authored> {
        self.lanes.iter().filter(move |lane| lane.role == role)
    }
}

/* ── The schedule ────────────────────────────────────────────────────── */

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
    pub host: String,
    pub segment: String,
    /// Where the audio is on this machine, when the station knows.
    pub file: Option<PathBuf>,
    pub lufs: Option<f64>,
    /// Gain to add on top of the envelope, in dB. 0 when the file is
    /// already at the station's loudness.
    pub trim_db: f64,
    pub beat_offset: Option<f64>,
    pub beat_period: Option<f64>,
    pub downbeat_offset: Option<f64>,
    /// A hash of everything the station said about this item, so a changed
    /// plan is noticed without comparing it field by field.
    pub fingerprint: u64,
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

    /// The gain the envelope rides, before any transition or duck.
    pub fn level_envelope(&self) -> &[[f32; 2]] {
        if self.deck_envelope.is_empty() { &self.envelope } else { &self.deck_envelope }
    }

    /// A `Record` standing in for this item, so it loads through exactly the
    /// same path a record you picked yourself does -- carrying the analysis
    /// the station has, so the grid and the tempo are there on the deck.
    pub fn record(&self, file: PathBuf) -> Record {
        Record {
            key: if self.key.is_empty() { self.id.clone() } else { self.key.clone() },
            artist: self.artist.clone(),
            title: self.title.clone(),
            album: None,
            duration: Some(self.duration),
            bpm: self.bpm,
            camelot: self.camelot.clone(),
            lufs: self.lufs,
            file,
            beat_offset: self.beat_offset,
            beat_period: self.beat_period,
            downbeat_offset: self.downbeat_offset,
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
    pub note: String,
    pub picked_by: String,
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

pub struct Snapshot {
    pub now: f64,
    pub epoch: i64,
    pub items: Vec<Scheduled>,
    /// Unused by the console now (the clock learns from round trips), kept
    /// for the fixtures and tools that build snapshots by hand.
    #[allow(dead_code)]
    pub frame: u64,
}

/* ── Reading it ──────────────────────────────────────────────────────── */

/// A field that is read if it has the right shape and ignored if not.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// Breakpoints: `[[t, v], ...]`, with anything else in the list skipped.
#[derive(Default)]
struct Points(Vec<[f32; 2]>);

impl<'de> Deserialize<'de> for Points {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        Ok(Points(pairs(&value).map(|(t, v)| [t as f32, v as f32]).collect()))
    }
}

fn pairs(value: &Value) -> impl Iterator<Item = (f64, f64)> + '_ {
    value.as_array().into_iter().flatten().filter_map(|point| {
        let pair = point.as_array()?;
        Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
    })
}

#[derive(Deserialize)]
struct WireSnapshot {
    #[serde(default, deserialize_with = "lenient")]
    now: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    epoch: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    items: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct WireItem {
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    kind: Option<String>,
    url: String,
    start_at: f64,
    duration: f64,
    #[serde(default, deserialize_with = "lenient")]
    offset: Option<f64>,
    #[serde(default)]
    envelope: Points,
    #[serde(default, deserialize_with = "lenient")]
    meta: Option<WireMeta>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireMeta {
    #[serde(deserialize_with = "lenient")]
    deck: Option<u64>,
    deck_envelope: Points,
    #[serde(deserialize_with = "lenient")]
    playback_rate: Option<f64>,
    rate_curve: Value,
    #[serde(deserialize_with = "lenient")]
    key_lock: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    playback_feedback: Option<WireFeedback>,
    #[serde(deserialize_with = "lenient")]
    echo: Option<WireEcho>,
    #[serde(deserialize_with = "lenient")]
    ducking: Option<WireDucking>,
    #[serde(deserialize_with = "lenient")]
    automation: Option<WireAutomation>,
    #[serde(deserialize_with = "lenient")]
    transition: Option<WireTransition>,
    #[serde(deserialize_with = "lenient")]
    title: Option<String>,
    #[serde(deserialize_with = "lenient")]
    text: Option<String>,
    #[serde(deserialize_with = "lenient")]
    artist: Option<String>,
    #[serde(deserialize_with = "lenient")]
    bpm: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    camelot: Option<String>,
    #[serde(deserialize_with = "lenient")]
    key: Option<String>,
    #[serde(deserialize_with = "lenient")]
    host: Option<String>,
    #[serde(deserialize_with = "lenient")]
    segment: Option<String>,
    #[serde(deserialize_with = "lenient")]
    file: Option<String>,
    #[serde(deserialize_with = "lenient")]
    lufs: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    trim_db: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    beat_offset: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    beat_period: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    downbeat_offset: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireFeedback {
    #[serde(deserialize_with = "lenient")]
    enabled: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    max_adjustment: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    tolerance_ms: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireEcho {
    #[serde(deserialize_with = "lenient")]
    start: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    end: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    seconds: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    mix: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    feedback: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireDucking {
    #[serde(deserialize_with = "lenient")]
    target_gain: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    attack: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    hold_after: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    release: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireAutomation {
    low: Points,
    mid: Points,
    high: Points,
    lpf: Points,
    hpf: Points,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireTransition {
    #[serde(deserialize_with = "lenient")]
    preset: Option<String>,
    #[serde(deserialize_with = "lenient")]
    reason: Option<String>,
    #[serde(deserialize_with = "lenient")]
    overlap: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    technique: Option<String>,
    #[serde(deserialize_with = "lenient")]
    base: Option<String>,
    #[serde(deserialize_with = "lenient")]
    switch_at: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    flashy: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    requires: Option<Vec<String>>,
    lanes: Value,
    events: Value,
}

#[derive(Deserialize)]
struct WireRow {
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    stage: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    playing: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    eta: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    artist: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    title: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    bpm: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    camelot: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    can_move: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    can_remove: Option<bool>,
    #[serde(default)]
    selection: Value,
    #[serde(default, deserialize_with = "lenient")]
    note: Option<String>,
    #[serde(default)]
    selection_origin: Value,
}

pub fn queue_from(body: &Value) -> Vec<QueueRow> {
    body["items"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| serde_json::from_value::<WireRow>(row.clone()).ok())
                .map(|row| QueueRow {
                    id: row.id,
                    stage: row.stage.unwrap_or_else(|| "queued".into()),
                    playing: row.playing.unwrap_or(false),
                    eta: row.eta,
                    artist: row.artist.unwrap_or_default(),
                    title: row.title.unwrap_or_default(),
                    bpm: row.bpm,
                    camelot: row.camelot,
                    can_move: row.can_move.unwrap_or(false),
                    can_remove: row.can_remove.unwrap_or(false),
                    selection_reason: row.selection["reason"].as_str().unwrap_or("").to_string(),
                    note: row.note.unwrap_or_default(),
                    picked_by: row.selection_origin["by"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn snapshot_from(body: &Value, frame: u64) -> Snapshot {
    let wire: WireSnapshot = serde_json::from_value(body.clone())
        .unwrap_or(WireSnapshot { now: None, epoch: None, items: None });
    Snapshot {
        now: wire.now.unwrap_or(0.0),
        epoch: wire.epoch.unwrap_or(0),
        frame,
        items: wire.items.unwrap_or_default().iter().filter_map(item_from).collect(),
    }
}

pub fn rate_curve_from(value: &Value) -> Vec<[f64; 2]> {
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

fn fingerprint(value: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

fn item_from(value: &Value) -> Option<Scheduled> {
    let wire: WireItem = serde_json::from_value(value.clone()).ok()?;
    let meta = wire.meta.unwrap_or_default();
    let feedback = meta.playback_feedback.unwrap_or_default();
    let ducking = meta.ducking.unwrap_or_default();
    let automation = meta.automation.unwrap_or_default();

    Some(Scheduled {
        id: wire.id,
        kind: wire.kind.unwrap_or_default(),
        url: wire.url,
        start_at: wire.start_at,
        duration: wire.duration,
        offset: wire.offset.unwrap_or(0.0),
        preferred_deck: meta.deck.map(|d| d as usize),
        envelope: wire.envelope.0,
        deck_envelope: meta.deck_envelope.0,
        playback_rate: meta.playback_rate.unwrap_or(1.0).clamp(0.92, 1.08),
        rate_curve: rate_curve_from(&meta.rate_curve),
        key_lock: meta.key_lock.unwrap_or(false),
        playback_feedback: [
            if feedback.enabled.unwrap_or(true) { 1.0 } else { 0.0 },
            feedback.max_adjustment.unwrap_or(0.005).clamp(0.0, 0.01),
            feedback.tolerance_ms.unwrap_or(30.0).clamp(10.0, 150.0) / 1000.0,
        ],
        echo: meta.echo.map(|echo| [
            echo.start.unwrap_or(0.0) as f32,
            echo.end.unwrap_or(0.0) as f32,
            echo.seconds.unwrap_or(0.25).clamp(0.03, 1.8) as f32,
            echo.mix.unwrap_or(0.0).clamp(0.0, 0.5) as f32,
            echo.feedback.unwrap_or(0.3).clamp(0.0, 0.65) as f32,
        ]),
        ducking: [
            ducking.target_gain.unwrap_or(0.10).clamp(0.001, 1.0) as f32,
            ducking.attack.unwrap_or(0.35).clamp(0.01, 3.0) as f32,
            ducking.hold_after.unwrap_or(0.40).clamp(0.0, 5.0) as f32,
            ducking.release.unwrap_or(1.2).clamp(0.01, 6.0) as f32,
        ],
        automation: Automation {
            low: Curve(automation.low.0),
            mid: Curve(automation.mid.0),
            high: Curve(automation.high.0),
            lpf: Curve(automation.lpf.0),
            hpf: Curve(automation.hpf.0),
        },
        transition: meta.transition.map(transition_from),
        title: meta.title.or(meta.text).unwrap_or_default(),
        artist: meta.artist.unwrap_or_default(),
        bpm: meta.bpm,
        camelot: meta.camelot,
        key: meta.key.unwrap_or_default(),
        host: meta.host.unwrap_or_default(),
        segment: meta.segment.unwrap_or_else(|| "Host break".into()),
        file: meta.file.filter(|f| !f.trim().is_empty()).map(PathBuf::from),
        lufs: meta.lufs.filter(|l| l.is_finite()),
        trim_db: meta.trim_db.filter(|t| t.is_finite()).unwrap_or(0.0).clamp(-24.0, 12.0),
        beat_offset: meta.beat_offset.filter(|v| v.is_finite()),
        beat_period: meta.beat_period.filter(|p| p.is_finite() && *p > 0.05),
        downbeat_offset: meta.downbeat_offset.filter(|v| v.is_finite()),
        fingerprint: fingerprint(value),
    })
}

fn transition_from(wire: WireTransition) -> Transition {
    let mut lanes = Vec::new();
    // {"out": {"low": [[t, v], ...], ...}, "in": {...}}
    for (role_name, by_lane) in wire.lanes.as_object().into_iter().flatten() {
        let Some(role) = Role::named(role_name) else {
            unknown(&format!("transition role {role_name:?}"));
            continue;
        };
        for (name, points) in by_lane.as_object().into_iter().flatten() {
            let Some(lane) = Lane::from_name(name) else {
                unknown(&format!("automation lane {name:?}"));
                continue;
            };
            let mut points: Vec<(f64, f32)> = pairs(points)
                .filter(|(t, v)| t.is_finite() && v.is_finite())
                .map(|(t, v)| (t, v as f32))
                .collect();
            points.sort_by(|a, b| a.0.total_cmp(&b.0));
            if !points.is_empty() {
                lanes.push(Authored { role, lane, points });
            }
        }
    }
    let events = wire.events.as_array().into_iter().flatten().filter_map(event_from).collect();
    Transition {
        preset: wire.preset.unwrap_or_default(),
        reason: wire.reason.unwrap_or_default(),
        overlap: wire.overlap.unwrap_or(0.0),
        technique: wire.technique.unwrap_or_default(),
        base: wire.base.unwrap_or_default(),
        switch_at: wire.switch_at.filter(|t| t.is_finite()),
        flashy: wire.flashy.unwrap_or(false),
        requires: wire.requires.unwrap_or_default(),
        lanes,
        events,
    }
}

fn event_from(value: &Value) -> Option<Event> {
    let kind = match value["type"].as_str()? {
        "roll" => EventKind::Roll,
        "loop" => EventKind::Loop,
        other => {
            unknown(&format!("transition event {other:?}"));
            return None;
        }
    };
    let role = Role::named(value["deck"].as_str().or(value["deck_role"].as_str())?)?;
    let at = value["at"].as_f64().filter(|t| t.is_finite())?;
    let until = value["until"].as_f64().filter(|t| t.is_finite() && *t > at)?;
    let length_seconds = value["length_seconds"].as_f64().filter(|s| s.is_finite() && *s > 0.0);
    let length_beats = value["length_beats"].as_f64().filter(|b| b.is_finite() && *b > 0.0);
    if length_seconds.is_none() && length_beats.is_none() {
        return None;
    }
    Some(Event { kind, role, at, until, length_seconds, length_beats })
}

/// Say once, in the log, that the station sent something this console does
/// not know. Once per name: the schedule is read every few seconds.
fn unknown(what: &str) {
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
    let Ok(mut seen) = SEEN.lock() else { return };
    if seen.get_or_insert_with(Default::default).insert(what.to_string()) {
        crate::logfile::log!("airtime: ignoring {what} this console does not know");
    }
}
