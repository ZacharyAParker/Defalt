//! A scheduled record's moves, as engine automation.
//!
//! The station plans a transition in its own terms: a gain envelope, EQ in
//! decibels, filters in hertz, an echo span, a tempo curve, and -- from the
//! newer planner -- lanes written straight on the engine's own controls.
//! This turns all of it into one breakpoint list per engine lane, on the
//! station clock, which `Airtime` converts to output frames and sends once.
//! The audio thread then plays every move on its exact sample, whether or
//! not the window is being drawn.
//!
//! Nothing here sends anything or knows about decks. It is a function from
//! an item and its situation to curves, which is what makes it testable.

use crate::engine::Lane;

use super::protocol::{Authored, Curve, Role, Scheduled, Transition};

/// (station seconds, value), sorted. Two points at one time are a step.
pub type Points = Vec<(f64, f32)>;

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
/// no closed-form inverse worth writing, so it is bisected.
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

/// How a deck is playing its part of a transition, which is not always the
/// part the station wrote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// As planned.
    Normal,
    /// The other half of the mix is not there -- still decoding, failed,
    /// paused by hand -- so from `from` this deck plays alone: full level,
    /// flat EQ, no echo. A transition into nothing is silence.
    Solo { from: f64 },
    /// The incoming record started late, at `started`; it is brought into
    /// the blend over `window` seconds rather than jumping in half open.
    RecoveryIn { started: f64, window: f64 },
    /// The outgoing side of the same: it holds its level until the late
    /// record has arrived, and its moves come in as that one does.
    RecoveryOut { started: f64, window: f64 },
}

/// A speech duck: the target gain, attack, hold after, and release.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Duck {
    pub target: f32,
    pub attack: f32,
    pub hold: f32,
    pub release: f32,
}

impl Duck {
    pub fn of(item: &Scheduled) -> Duck {
        let [target, attack, hold, release] = item.ducking;
        Duck { target, attack, hold, release }
    }
}

/// Speech windows merged the way this duck merges them: two lines close
/// enough that the music would only just come back up stay ducked across.
pub fn merge_windows(windows: &[(f64, f64)], duck: Duck) -> Vec<(f64, f64)> {
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for &window in windows {
        if let Some(last) = merged.last_mut().filter(|last| window.0 <= last.1 + (duck.hold + duck.attack) as f64) {
            last.1 = last.1.max(window.1);
        } else {
            merged.push(window);
        }
    }
    merged
}

/// The duck's gain at `t` under merged speech windows.
pub fn duck_at(merged: &[(f64, f64)], duck: Duck, t: f64) -> f32 {
    let (attack, hold, release) = (duck.attack as f64, duck.hold as f64, duck.release as f64);
    merged.iter().map(|&(start, end)| {
        if t < start - attack || t >= end + hold + release { return 1.0; }
        if t < start { return duck.target.powf(((t - start + attack) / attack) as f32); }
        if t <= end + hold { return duck.target; }
        duck.target.powf(1.0 - ((t - end - hold) / release) as f32)
    }).fold(1.0, f32::min)
}

/// Everything a deck's curves depend on.
pub struct Situation<'a> {
    pub item: &'a Scheduled,
    /// The transition out of this record, if the next one has one: its
    /// `out` lanes and events are this deck's.
    pub next: Option<&'a Transition>,
    pub mode: Mode,
    /// Sorted speech windows across the schedule.
    pub speech: &'a [(f64, f64)],
    /// You have the crossfader: the station's envelope is replaced by the
    /// level this deck was at when you took it. The duck still applies.
    pub hold_level: Option<f32>,
    /// Fractional speed correction on top of the planned tempo.
    pub correction: f64,
    /// Where the console is on the station clock. Nothing earlier matters.
    pub now: f64,
    /// Channel gain (fader times trim) a `gain` lane is written against.
    pub base_gain: f32,
}

/// One deck's lanes.
#[derive(Debug, Default, PartialEq)]
pub struct Built {
    pub lanes: Vec<(Lane, Points)>,
    /// Lanes only a written transition moves. They are cleared once it is
    /// over, so the control is the console's again.
    pub clear_after: Vec<(Lane, f64)>,
    /// A written transition moves the echo. There is no lane for its return,
    /// so the deck's return has to be opened for the sends to be heard.
    pub echo_written: bool,
}

impl Built {
    pub fn lane(&self, lane: Lane) -> Option<&Points> {
        self.lanes.iter().find(|(l, _)| *l == lane).map(|(_, points)| points)
    }
}

/// The value of a point list at `t`: linear between points, flat outside,
/// and on a step the value after it -- which is how the engine reads one.
pub fn value_at(points: &[(f64, f32)], t: f64) -> Option<f32> {
    let first = points.first()?;
    if t < first.0 {
        return Some(first.1);
    }
    let index = points.partition_point(|p| p.0 <= t) - 1;
    let (t0, v0) = points[index];
    match points.get(index + 1) {
        Some(&(t1, v1)) if t1 > t0 => Some(v0 + (v1 - v0) * ((t - t0) / (t1 - t0)) as f32),
        _ => Some(v0),
    }
}

/// A station curve (item-relative seconds) as absolute points, each segment
/// cut into `pieces` so a non-linear `map` stays close to the real curve.
/// Steps (two points at one time) are kept exactly.
fn mapped(curve: &Curve, start: f64, pieces: usize, map: impl Fn(f32) -> f32) -> Points {
    let points = curve.points();
    let mut out = Vec::with_capacity(points.len() * pieces);
    for (index, &[t, v]) in points.iter().enumerate() {
        out.push((start + t as f64, map(v)));
        if let Some(&[t1, v1]) = points.get(index + 1) {
            if t1 > t {
                for k in 1..pieces {
                    let f = k as f32 / pieces as f32;
                    out.push((start + (t + (t1 - t) * f) as f64, map(v + (v1 - v) * f)));
                }
            }
        }
    }
    out
}

/// `base` with `g(t, value)` applied, evaluated at its own points and at
/// `extra` times as well. Steps in `base` survive, since `g` is applied to
/// each side of them separately.
fn compose(base: &Points, extra: &[f64], g: impl Fn(f64, f32) -> f32) -> Points {
    let mut out: Points = base.iter().map(|&(t, v)| (t, g(t, v))).collect();
    for &t in extra {
        if !t.is_finite() || base.iter().any(|p| (p.0 - t).abs() < 1e-9) {
            continue;
        }
        let v = value_at(base, t).unwrap_or(0.0);
        out.push((t, g(t, v)));
    }
    // Stable, so the two sides of a step keep their order.
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// The value arriving at `t` from before it: on a step, the first side.
fn value_before(points: &[(f64, f32)], t: f64) -> Option<f32> {
    let index = points.partition_point(|p| p.0 < t - 1e-9);
    match points.get(index) {
        Some(&(at, v)) if (at - t).abs() < 1e-9 => Some(v),
        _ => value_at(points, t),
    }
}

/// Two point lists multiplied, exactly at every breakpoint of either, with
/// the steps of both kept as steps.
fn multiply(a: &Points, b: &Points) -> Points {
    let mut times: Vec<f64> = a.iter().chain(b.iter()).map(|p| p.0).collect();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let mut out = Vec::with_capacity(times.len() + 4);
    for t in times {
        let (Some(before_a), Some(after_a)) = (value_before(a, t), value_at(a, t)) else { continue };
        let (Some(before_b), Some(after_b)) = (value_before(b, t), value_at(b, t)) else { continue };
        let (before, after) = (before_a * before_b, after_a * after_b);
        if (before - after).abs() > 1e-7 {
            out.push((t, before));
        }
        out.push((t, after));
    }
    out
}

/// Replace `base` with `authored` over the span `authored` covers.
fn splice(base: Points, authored: &[(f64, f32)]) -> Points {
    let (Some(first), Some(last)) = (authored.first(), authored.last()) else { return base };
    let mut out: Points = base.iter().copied().filter(|p| p.0 < first.0).collect();
    out.extend_from_slice(authored);
    out.extend(base.into_iter().filter(|p| p.0 > last.0));
    out
}

/// Cut everything before `now` except the one point that says where the
/// curve is coming from, so a curve sent late is no longer than it needs.
fn trim(points: Points, now: f64) -> Points {
    let keep_from = points.partition_point(|p| p.0 <= now).saturating_sub(1);
    points.into_iter().skip(keep_from).collect()
}

fn ramp(t: f64, from: f64, length: f64) -> f32 {
    ((t - from) / length).clamp(0.0, 1.0) as f32
}

/// How far a solo has taken over, 0..1: done by `from`, begun 50 ms before.
const SOLO_RAMP: f64 = 0.05;

fn mode_times(mode: Mode) -> Vec<f64> {
    match mode {
        Mode::Normal => Vec::new(),
        Mode::Solo { from } => vec![from - SOLO_RAMP, from],
        Mode::RecoveryIn { started, window } | Mode::RecoveryOut { started, window } =>
            (0..=8).map(|k| started + window * k as f64 / 8.0).collect(),
    }
}

/// Lanes the legacy protocol already drives, which a written lane is
/// spliced into rather than replacing.
pub(super) fn has_legacy(lane: Lane) -> bool {
    matches!(lane, Lane::Level | Lane::Low | Lane::Mid | Lane::High | Lane::Sweep | Lane::EchoSend | Lane::Rate)
}

/// Build every lane for one deck. `skip` holds the lanes you have taken.
pub fn build(situation: &Situation, skip: &[Lane]) -> Built {
    let item = situation.item;
    let now = situation.now;
    let start = item.start_at;
    let mode = situation.mode;
    let mode_extra = mode_times(mode);

    // Written lanes for this deck: this item's `in`, the next one's `out`.
    let own = item.transition.iter().flat_map(|t| t.lanes_for(Role::In));
    let next = situation.next.into_iter().flat_map(|t| t.lanes_for(Role::Out));
    let authored: Vec<&Authored> = own.chain(next).collect();
    let written = |lane: Lane| -> Option<Points> {
        let mut points: Points = authored.iter().filter(|a| a.lane == lane)
            .flat_map(|a| a.points.iter().copied()).collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        (!points.is_empty()).then_some(points)
    };

    let mut built = Built {
        echo_written: authored.iter().any(|a| matches!(a.lane, Lane::EchoSend | Lane::EchoFeedback | Lane::EchoBeats)),
        ..Built::default()
    };

    // Level: the envelope, times a written level (which rests at 1, and
    // multiplies rather than replaces -- a technique's fader moves are in the
    // envelope already), the situation, then the duck under speech.
    let envelope = item.level_envelope();
    let mut level: Points = if envelope.is_empty() {
        vec![(start, 1.0)]
    } else {
        envelope.iter().map(|&[t, v]| (start + t as f64, v)).collect()
    };
    if let Some(written) = written(Lane::Level) {
        level = multiply(&level, &written);
    }
    if let Some(held) = situation.hold_level {
        level = vec![(now, held)];
    }
    let duck = Duck::of(item);
    // An item carrying its duck in a separate deck envelope has it applied
    // here; one without has it already in its envelope.
    let merged = if item.deck_envelope.is_empty() { Vec::new() } else { merge_windows(situation.speech, duck) };
    let mut extra = mode_extra.clone();
    for &(from, to) in &merged {
        let (attack, hold, release) = (duck.attack as f64, duck.hold as f64, duck.release as f64);
        for k in 0..=6 {
            extra.push(from - attack + attack * k as f64 / 6.0);
            extra.push(to + hold + release * k as f64 / 6.0);
        }
    }
    extra.push(now);
    let level = compose(&level, &extra, |t, v| {
        let v = match mode {
            Mode::Normal => v,
            Mode::Solo { from } => v + (1.0 - v) * ramp(t, from - SOLO_RAMP, SOLO_RAMP),
            Mode::RecoveryIn { started, window } => v * ramp(t, started, window),
            Mode::RecoveryOut { started, window } => v.max(1.0 - ramp(t, started, window)),
        };
        (v * duck_at(&merged, duck, t)).clamp(0.0, 1.0)
    });
    built.lanes.push((Lane::Level, trim(level, now)));

    // Tone: the station's decibels as knob positions.
    let neutralise = |t: f64, v: f32, neutral: f32| -> f32 {
        match mode {
            Mode::Solo { from } => v + (neutral - v) * ramp(t, from - SOLO_RAMP, SOLO_RAMP),
            Mode::RecoveryOut { started, window } => neutral + (v - neutral) * ramp(t, started, window),
            _ => v,
        }
    };
    for (lane, curve) in [(Lane::Low, &item.automation.low), (Lane::Mid, &item.automation.mid),
                          (Lane::High, &item.automation.high)] {
        let mut points = if curve.is_empty() { vec![(now, 0.5)] } else { mapped(curve, start, 6, band_knob) };
        if let Some(written) = written(lane) {
            points = splice(points, &written);
        }
        let points = compose(&points, &mode_extra, |t, v| neutralise(t, v, 0.5));
        built.lanes.push((lane, trim(points, now)));
    }

    // One fader for both filters: whichever the station is actually
    // sweeping wins, since a pass parked at its own end is not sweeping.
    let (lpf, hpf) = (&item.automation.lpf, &item.automation.hpf);
    let mut sweep: Points = if lpf.is_empty() && hpf.is_empty() {
        vec![(now, 0.0)]
    } else {
        let mut times: Vec<f64> = mapped(lpf, start, 6, |v| v).into_iter()
            .chain(mapped(hpf, start, 6, |v| v))
            .map(|p| p.0)
            .collect();
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        times.into_iter().map(|t| {
            let local = (t - start) as f32;
            let mut fader = 0.0;
            if let Some(hz) = lpf.at(local).filter(|hz| *hz < 19_000.0) {
                fader = filter_fader(hz, true);
            }
            if let Some(hz) = hpf.at(local).filter(|hz| *hz > 25.0) {
                fader = filter_fader(hz, false);
            }
            (t, fader)
        }).collect()
    };
    if let Some(points) = written(Lane::Sweep) {
        sweep = splice(sweep, &points);
    }
    let sweep = compose(&sweep, &mode_extra, |t, v| neutralise(t, v, 0.0));
    built.lanes.push((Lane::Sweep, trim(sweep, now)));

    // The echo send: rises over the first quarter of its span, and closes at
    // the end -- where the repeats are left to ring out on their own.
    let mut send: Points = match item.echo {
        Some([from, to, ..]) if to > from => {
            let (t0, t1) = (start + from as f64, start + to as f64);
            let rise = (t0 + ((to - from) as f64 * 0.25).max(0.05)).min(t1);
            vec![(t0, 0.0), (rise, 1.0), (t1, 1.0), (t1, 0.0)]
        }
        _ => vec![(now, 0.0)],
    };
    if let Some(points) = written(Lane::EchoSend) {
        send = splice(send, &points);
    }
    let send = compose(&send, &mode_extra, |t, v| match mode {
        Mode::Solo { from } => v * (1.0 - ramp(t, from - SOLO_RAMP, SOLO_RAMP)),
        Mode::RecoveryOut { started, window } => v * ramp(t, started, window),
        _ => v,
    });
    built.lanes.push((Lane::EchoSend, trim(send, now)));

    // Tempo: the planned curve, with any correction on top from now.
    let scale = 1.0 + situation.correction;
    let nominal = |elapsed: f64| (item.rate_at(elapsed) * scale).clamp(0.92, 1.08) as f32;
    let mut rate: Points = vec![(now, nominal(now - start))];
    rate.extend(item.rate_curve.iter()
        .map(|p| (start + p[0], (p[1] * scale).clamp(0.92, 1.08) as f32))
        .filter(|p| p.0 > now));
    if let Some(points) = written(Lane::Rate) {
        rate = splice(rate, &points);
    }
    built.lanes.push((Lane::Rate, trim(rate, now)));

    // Everything else a transition writes and nothing else drives.
    for lane in Lane::ALL {
        if has_legacy(lane) {
            continue;
        }
        let Some(mut points) = written(lane) else { continue };
        let end = points.last().map_or(now, |p| p.0);
        if end + 0.1 < now {
            continue; // Over: the control is the console's again.
        }
        if lane == Lane::Gain {
            // Written against unity; the channel's own gain rides under it.
            for point in points.iter_mut() {
                point.1 = (point.1 * situation.base_gain).clamp(0.0, 2.0);
            }
        }
        built.clear_after.push((lane, end + 0.1));
        built.lanes.push((lane, trim(points, now)));
    }

    built.lanes.retain(|(lane, _)| !skip.contains(lane));
    built.clear_after.retain(|(lane, _)| !skip.contains(lane));
    built
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::airtime::protocol::snapshot_from;

    fn item(json: serde_json::Value) -> Scheduled {
        snapshot_from(&serde_json::json!({"now": 0, "items": [json]}), 0).items.remove(0)
    }

    fn situation(item: &Scheduled, now: f64) -> Situation<'_> {
        Situation { item, next: None, mode: Mode::Normal, speech: &[], hold_level: None,
                    correction: 0.0, now, base_gain: 1.0 }
    }

    #[test]
    fn a_fade_becomes_a_level_lane_on_the_station_clock() {
        let item = item(serde_json::json!({"id": "a", "kind": "music", "url": "/a",
            "start_at": 100, "duration": 60, "envelope": [[0, 0], [6, 1], [60, 1]]}));
        let built = build(&situation(&item, 90.0), &[]);
        let level = built.lane(Lane::Level).unwrap();
        assert_eq!(value_at(level, 100.0), Some(0.0));
        assert_eq!(value_at(level, 103.0), Some(0.5));
        assert_eq!(value_at(level, 130.0), Some(1.0));
    }

    #[test]
    fn a_gate_stays_a_step() {
        let item = item(serde_json::json!({"id": "a", "kind": "music", "url": "/a",
            "start_at": 0, "duration": 10, "envelope": [[0, 1], [2, 1], [2, 0], [4, 0], [4, 1]]}));
        let level = build(&situation(&item, 0.0), &[]).lane(Lane::Level).cloned().unwrap();
        assert_eq!(value_at(&level, 1.999), Some(1.0));
        assert_eq!(value_at(&level, 2.0), Some(0.0));
        assert_eq!(value_at(&level, 3.9), Some(0.0));
        assert_eq!(value_at(&level, 4.0), Some(1.0));
    }

    #[test]
    fn written_lanes_replace_the_legacy_curve_only_where_they_run() {
        let item = item(serde_json::json!({"id": "b", "kind": "music", "url": "/b",
            "start_at": 100, "duration": 60,
            "meta": {"automation": {"low": [[0, -30], [8, 0]]},
                     "transition": {"preset": "gate", "technique": "gate",
                        "lanes": {"in": {"low": [[102, 1.0], [104, 1.0]], "stem_drums": [[100, 0], [104, 1]],
                                         "wobble": [[100, 1]]},
                                  "sideways": {"low": [[0, 0]]}},
                        "events": [{"type": "roll", "deck": "in", "at": 101, "length_seconds": 0.25, "until": 102}]}}}));
        let transition = item.transition.as_ref().unwrap();
        assert_eq!(transition.lanes.len(), 2, "unknown lanes and roles are dropped: {:?}", transition.lanes);
        assert_eq!(transition.events.len(), 1);
        let built = build(&situation(&item, 90.0), &[]);
        let low = built.lane(Lane::Low).unwrap();
        assert!(value_at(low, 100.0).unwrap() < 0.05, "the legacy bass swap before the written span");
        assert_eq!(value_at(low, 103.0), Some(1.0), "the written lane inside its span");
        assert_eq!(value_at(low, 110.0), Some(0.5), "the legacy curve after it");
        assert_eq!(value_at(built.lane(Lane::StemDrums).unwrap(), 104.0), Some(1.0));
        assert_eq!(built.clear_after, vec![(Lane::StemDrums, 104.1)]);
    }

    #[test]
    fn a_solo_holds_the_level_and_flattens_the_moves() {
        let item = item(serde_json::json!({"id": "a", "kind": "music", "url": "/a",
            "start_at": 0, "duration": 200, "envelope": [[0, 1], [194, 1], [200, 0]],
            "meta": {"automation": {"low": [[194, 0], [197, -30]]},
                     "echo": {"start": 196, "end": 200, "seconds": 0.5, "mix": 0.3}}}));
        let mut situation = situation(&item, 190.0);
        situation.mode = Mode::Solo { from: 194.0 };
        let built = build(&situation, &[]);
        assert_eq!(value_at(built.lane(Lane::Level).unwrap(), 199.0), Some(1.0));
        assert_eq!(value_at(built.lane(Lane::Low).unwrap(), 198.0), Some(0.5));
        assert_eq!(value_at(built.lane(Lane::EchoSend).unwrap(), 198.0), Some(0.0));
    }

    #[test]
    fn speech_ducks_the_level_under_a_separate_deck_envelope() {
        let item = item(serde_json::json!({"id": "a", "kind": "music", "url": "/a",
            "start_at": 0, "duration": 200, "envelope": [[0, 0.1], [200, 0.1]],
            "meta": {"deck_envelope": [[0, 1], [200, 1]],
                     "ducking": {"target_gain": 0.1, "attack": 0.35, "hold_after": 0.4, "release": 1.2}}}));
        let speech = [(10.0, 20.0)];
        let mut situation = situation(&item, 5.0);
        situation.speech = &speech;
        let level = build(&situation, &[]).lane(Lane::Level).cloned().unwrap();
        assert_eq!(value_at(&level, 5.0), Some(1.0));
        assert!((value_at(&level, 12.0).unwrap() - 0.1).abs() < 1e-6);
        assert_eq!(value_at(&level, 21.7), Some(1.0));
    }

    #[test]
    fn held_lanes_are_left_out() {
        let item = item(serde_json::json!({"id": "a", "kind": "music", "url": "/a",
            "start_at": 0, "duration": 10}));
        let built = build(&situation(&item, 0.0), &[Lane::Low, Lane::Rate]);
        assert!(built.lane(Lane::Low).is_none());
        assert!(built.lane(Lane::Rate).is_none());
        assert!(built.lane(Lane::Mid).is_some());
    }

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
            assert!((back - position).abs() < 0.02, "{position} -> {db} dB -> {back}");
        }
        assert!((band_knob(0.0) - 0.5).abs() < 1e-6);
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
        // They share one fader, and the sign is what tells them apart.
        assert!(filter_fader(500.0, true) < 0.0);
        assert!(filter_fader(500.0, false) > 0.0);
    }
}
