//! Automation: a deck's controls moved by the engine, on the output clock.
//!
//! A transition planned on the UI thread and played by sending a fader value
//! every frame of the UI is a transition that lands wherever the UI's frame
//! happened to fall -- 16 ms either way, and worse when the window is behind.
//! Here the plan goes over once, as curves stamped in output frames (the
//! clock `Telemetry::frame` reads out), and the audio thread reads each one
//! at the exact frame it is rendering. A gate lands on its sample; a brake
//! ends on its beat.
//!
//! Curves are built off the audio thread and handed over in an `Arc`. The
//! audio thread only reads them, and gives them back through the graveyard
//! when they are replaced or cleared, so nothing here is ever freed inside a
//! callback.
//!
//! A lane drives the same control the matching command sets. While a curve is
//! running, commands for that control are overwritten on the next frame;
//! once it is cleared or detached, the control keeps the curve's last value
//! and belongs to whoever sets it next. That is what makes "the user grabbed
//! the knob" a single `Detach` rather than a negotiation.

use std::sync::Arc;

/// Every control that can be automated. One enum, so the app can map a lane
/// name from the station's JSON to a lane without a table per kind.
///
/// Values, and what they drive:
///
/// | lane            | name              | value                                    |
/// |-----------------|-------------------|------------------------------------------|
/// | `Gain`          | `"gain"`          | channel gain 0..2 (what `Command::Gain` sets) |
/// | `Level`         | `"level"`         | transition level 0..1, multiplied on top of gain (`Command::Level`) |
/// | `Low`/`Mid`/`High` | `"low"` `"mid"` `"high"` | isolator knob 0..1, 0.5 flat, 0 kill (`Command::Tone`) |
/// | `Sweep`         | `"sweep"`         | filter sweep -1..1, 0 off                |
/// | `EchoSend`      | `"echo_send"`     | how much of the channel feeds the echo, 0..1 |
/// | `EchoFeedback`  | `"echo_feedback"` | echo feedback 0..1; 1 with the send at 0 is a freeze |
/// | `EchoBeats`     | `"echo_beats"`    | echo time in beats of the deck's grid (0.5 s a beat without one) |
/// | `ReverbSend`    | `"reverb_send"`   | send into the shared reverb, 0..1        |
/// | `Rate`          | `"rate"`          | playback rate -4..4 (what `Command::Speed` sets); through 0 is a brake, below 0 plays backwards and bypasses key lock |
/// | `StemDrums` ... | `"stem_drums"` `"stem_bass"` `"stem_harmonic"` `"stem_vocals"` | per-stem level 0..2 |
///
/// Stem lanes do nothing on a deck that has not been separated. There is no
/// honest way to take the drums out of a full mix with a filter, so they do
/// not pretend to: the curve runs, and nothing moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    Gain,
    Level,
    Low,
    Mid,
    High,
    Sweep,
    EchoSend,
    EchoFeedback,
    EchoBeats,
    ReverbSend,
    Rate,
    StemDrums,
    StemBass,
    StemHarmonic,
    StemVocals,
}

pub const LANES: usize = 15;

impl Lane {
    pub const ALL: [Lane; LANES] = [
        Lane::Gain, Lane::Level, Lane::Low, Lane::Mid, Lane::High, Lane::Sweep,
        Lane::EchoSend, Lane::EchoFeedback, Lane::EchoBeats, Lane::ReverbSend, Lane::Rate,
        Lane::StemDrums, Lane::StemBass, Lane::StemHarmonic, Lane::StemVocals,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The lane's name as the station spells it.
    pub fn name(self) -> &'static str {
        match self {
            Lane::Gain => "gain",
            Lane::Level => "level",
            Lane::Low => "low",
            Lane::Mid => "mid",
            Lane::High => "high",
            Lane::Sweep => "sweep",
            Lane::EchoSend => "echo_send",
            Lane::EchoFeedback => "echo_feedback",
            Lane::EchoBeats => "echo_beats",
            Lane::ReverbSend => "reverb_send",
            Lane::Rate => "rate",
            Lane::StemDrums => "stem_drums",
            Lane::StemBass => "stem_bass",
            Lane::StemHarmonic => "stem_harmonic",
            Lane::StemVocals => "stem_vocals",
        }
    }

    /// The lane a name refers to, or `None` for a name this engine does not
    /// have (a newer station talking to an older console, say).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_name(name: &str) -> Option<Lane> {
        Lane::ALL.into_iter().find(|lane| lane.name() == name)
    }

    /// The stem this lane moves, if it is a stem lane.
    pub fn stem(self) -> Option<usize> {
        match self {
            Lane::StemDrums => Some(0),
            Lane::StemBass => Some(1),
            Lane::StemHarmonic => Some(2),
            Lane::StemVocals => Some(3),
            _ => None,
        }
    }
}

/// A value over time: breakpoints stamped in output frames after
/// `start_frame`, straight lines between them, flat after the last.
///
/// Before `start_frame` the curve is not running at all and the control is
/// wherever it was left, so a curve can be sent well ahead of its moment.
/// Two points at the same frame are a step: the value jumps there, on that
/// sample, which is how a gate or a cut is written.
///
/// A curve that starts somewhere other than where its control currently is
/// will jump there when it starts. Start it at the current value.
pub struct Curve {
    start_frame: u64,
    points: Box<[(f64, f32)]>,
}

impl Curve {
    /// Breakpoints as (frames after `start_frame`, value). Sorted here, so
    /// the caller need not; points at the same frame keep their order.
    pub fn new(start_frame: u64, mut points: Vec<(f64, f32)>) -> Self {
        points.retain(|(t, v)| t.is_finite() && v.is_finite());
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Curve { start_frame, points: points.into_boxed_slice() }
    }

    /// Breakpoints as (seconds after `start_frame`, value), converted at the
    /// device rate -- the rate `Telemetry::device_rate` reports.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_seconds(start_frame: u64, points: &[(f64, f32)], rate: u32) -> Self {
        Curve::new(start_frame, points.iter().map(|&(t, v)| (t * rate as f64, v)).collect())
    }

    pub fn start_frame(&self) -> u64 {
        self.start_frame
    }

    /// The last frame at which the curve is still moving.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn end_frame(&self) -> u64 {
        self.start_frame + self.points.last().map_or(0.0, |p| p.0.max(0.0)).ceil() as u64
    }

    /// The value at an output frame, or `None` before the curve starts.
    ///
    /// `cursor` is where the last lookup landed; rendering only moves
    /// forward, so carrying it makes each lookup a step rather than a search.
    /// It is walked rather than trusted, so a stale one is merely slower.
    #[inline]
    pub fn at(&self, frame: u64, cursor: &mut usize) -> Option<f32> {
        if frame < self.start_frame || self.points.is_empty() {
            return None;
        }
        let t = (frame - self.start_frame) as f64;
        let last = self.points.len() - 1;
        if *cursor > last { *cursor = 0; }
        // Land on the last point at or before t.
        while *cursor > 0 && self.points[*cursor].0 > t { *cursor -= 1; }
        while *cursor < last && self.points[*cursor + 1].0 <= t { *cursor += 1; }
        let (t0, v0) = self.points[*cursor];
        if t < t0 {
            // Before the first point: flat at its value.
            return Some(v0);
        }
        if *cursor == last {
            return Some(v0);
        }
        let (t1, v1) = self.points[*cursor + 1];
        Some(v0 + (v1 - v0) * ((t - t0) / (t1 - t0)) as f32)
    }
}

/// One deck's lanes. Fixed size; filling and emptying it never allocates.
pub struct Automation {
    curves: [Option<Arc<Curve>>; LANES],
    cursors: [usize; LANES],
    active: usize,
}

impl Default for Automation {
    fn default() -> Self {
        Automation { curves: std::array::from_fn(|_| None), cursors: [0; LANES], active: 0 }
    }
}

impl Automation {
    /// Put a curve on a lane, handing back the one it replaced so it can be
    /// dropped somewhere other than the audio thread.
    pub fn set(&mut self, lane: Lane, curve: Arc<Curve>) -> Option<Arc<Curve>> {
        let slot = &mut self.curves[lane.index()];
        let old = slot.replace(curve);
        if old.is_none() { self.active += 1; }
        self.cursors[lane.index()] = 0;
        old
    }

    /// Take a lane's curve off. The control keeps its current value.
    pub fn clear(&mut self, lane: Lane) -> Option<Arc<Curve>> {
        let old = self.curves[lane.index()].take();
        if old.is_some() { self.active -= 1; }
        old
    }

    #[inline]
    pub fn any(&self) -> bool {
        self.active > 0
    }

    /// The lane's value at an output frame, if a curve is on it and running.
    #[inline]
    pub fn value(&mut self, lane: Lane, frame: u64) -> Option<f32> {
        let index = lane.index();
        self.curves[index].as_ref()?.at(frame, &mut self.cursors[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lane_round_trips_through_its_name() {
        for lane in Lane::ALL {
            assert_eq!(Lane::from_name(lane.name()), Some(lane));
        }
        assert_eq!(Lane::from_name("wobble"), None);
        assert_eq!(Lane::ALL.iter().map(|l| l.index()).collect::<Vec<_>>(),
                   (0..LANES).collect::<Vec<_>>());
    }

    #[test]
    fn a_curve_is_silent_before_it_starts_then_exact_on_every_frame() {
        let curve = Curve::new(1_000, vec![(0.0, 0.0), (100.0, 1.0)]);
        let mut cursor = 0;
        assert_eq!(curve.at(999, &mut cursor), None);
        assert_eq!(curve.at(1_000, &mut cursor), Some(0.0));
        assert_eq!(curve.at(1_050, &mut cursor), Some(0.5));
        assert_eq!(curve.at(1_100, &mut cursor), Some(1.0));
        assert_eq!(curve.at(9_999, &mut cursor), Some(1.0));
        assert_eq!(curve.end_frame(), 1_100);
    }

    #[test]
    fn two_points_on_one_frame_are_a_step_on_that_frame() {
        let curve = Curve::new(0, vec![(0.0, 1.0), (480.0, 1.0), (480.0, 0.0), (960.0, 0.0),
                                       (960.0, 1.0)]);
        let mut cursor = 0;
        assert_eq!(curve.at(479, &mut cursor), Some(1.0));
        assert_eq!(curve.at(480, &mut cursor), Some(0.0));
        assert_eq!(curve.at(959, &mut cursor), Some(0.0));
        assert_eq!(curve.at(960, &mut cursor), Some(1.0));
        // Backwards with a stale cursor still reads right.
        assert_eq!(curve.at(100, &mut cursor), Some(1.0));
    }

    #[test]
    fn replacing_and_clearing_hand_the_old_curve_back() {
        let mut automation = Automation::default();
        assert!(!automation.any());
        assert!(automation.set(Lane::Gain, Arc::new(Curve::new(0, vec![(0.0, 1.0)]))).is_none());
        assert!(automation.set(Lane::Gain, Arc::new(Curve::new(0, vec![(0.0, 0.5)]))).is_some());
        assert!(automation.any());
        assert_eq!(automation.value(Lane::Gain, 10), Some(0.5));
        assert!(automation.clear(Lane::Gain).is_some());
        assert!(!automation.any());
        assert_eq!(automation.value(Lane::Gain, 10), None);
    }
}
