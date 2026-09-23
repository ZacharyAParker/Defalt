//! The station's clock, on this console's output.
//!
//! The station schedules in its own seconds; the console plays in output
//! frames. Everything placed on the decks -- a start, a fade, a bass swap --
//! is converted from one to the other, so this mapping is what the whole
//! mix hangs off.
//!
//! It is learnt from the answers the station gives. Each answer says what
//! the station's clock read (taken as the very last thing before the reply
//! was written), and the console knows when it asked and when the answer
//! came back. The midpoint of that round trip is the best guess at when the
//! station read its clock, and the answer with the shortest round trip is
//! the one whose midpoint can be trusted most -- so the mapping follows the
//! best of the recent answers rather than the latest.
//!
//! And it slews rather than steps. A correction of a few milliseconds is
//! eased in at half a percent of speed, which nobody hears; only a real
//! jump (a skip, a restarted station, a quarter of a second of error) moves
//! it at once.

use std::collections::VecDeque;

/// Errors larger than this are a jump, not drift.
const STEP: f64 = 0.25;
/// How fast a small error is eased away: seconds per second.
const SLEW: f64 = 0.005;
/// Errors smaller than this are not worth moving for.
const IGNORE: f64 = 0.0005;
/// Answers remembered, for picking the best of them.
const SAMPLES: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Sample {
    frame: f64,
    now: f64,
    rtt: f64,
}

#[derive(Clone, Copy, Debug)]
struct Slew {
    /// Seconds still to be added, eased in over `frames` output frames.
    amount: f64,
    frames: f64,
}

/// What an observation did to the mapping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Moved {
    Nothing,
    /// A small correction, being eased in.
    Slewed,
    /// A jump. Anything placed against the old mapping is wrong now.
    Stepped,
}

pub struct Clock {
    rate: f64,
    /// Station seconds at an output frame. Everything is measured from here.
    base: Option<(u64, f64)>,
    slew: Option<Slew>,
    samples: VecDeque<Sample>,
    restarts: u64,
}

impl Clock {
    pub fn new(rate: u32) -> Self {
        Clock { rate: rate.max(1) as f64, base: None, slew: None, samples: VecDeque::new(), restarts: 0 }
    }

    pub fn anchored(&self) -> bool {
        self.base.is_some()
    }

    pub fn rate(&self) -> u32 {
        self.rate as u32
    }

    /// Forget everything: a new session, a station that has gone.
    pub fn reset(&mut self) {
        self.base = None;
        self.slew = None;
        self.samples.clear();
    }

    /// Put the station's clock at `now` on `frame`, outright.
    pub fn anchor(&mut self, now: f64, frame: u64) {
        self.base = Some((frame, now));
        self.slew = None;
    }

    /// Station seconds at an output frame.
    pub fn station_at(&self, frame: u64) -> Option<f64> {
        self.station_at_f(frame as f64)
    }

    fn station_at_f(&self, frame: f64) -> Option<f64> {
        let (base_frame, base_now) = self.base?;
        let since = frame - base_frame as f64;
        let mut t = base_now + since / self.rate;
        if let Some(slew) = self.slew {
            if since > 0.0 {
                t += slew.amount * (since / slew.frames).min(1.0);
            }
        }
        Some(t)
    }

    /// The output frame at which the station's clock reads `t`, as a
    /// fraction, possibly before the stream opened.
    pub fn frame_at_f(&self, t: f64) -> Option<f64> {
        let (base_frame, base_now) = self.base?;
        let base_frame = base_frame as f64;
        let ahead = t - base_now;
        match self.slew {
            Some(slew) if ahead > 0.0 => {
                // Inside the slew the clock runs at (1/rate + amount/frames)
                // seconds a frame; past it, at 1/rate again.
                let per_frame = 1.0 / self.rate + slew.amount / slew.frames;
                let through = slew.frames * per_frame;
                if per_frame > 0.0 && ahead <= through {
                    Some(base_frame + ahead / per_frame)
                } else {
                    Some(base_frame + slew.frames + (ahead - through) * self.rate)
                }
            }
            _ => Some(base_frame + ahead * self.rate),
        }
    }

    /// The same, as a frame the engine can be given.
    pub fn frame_at(&self, t: f64) -> Option<u64> {
        self.frame_at_f(t).map(|f| f.max(0.0).round() as u64)
    }

    /// Learn from an answer: the station read `now` at output frame `frame`
    /// (the round trip's midpoint), and the round trip took `rtt` seconds.
    /// `current` is the frame the console is at as it learns this.
    pub fn observe(&mut self, now: f64, frame: f64, rtt: f64, current: u64) -> Moved {
        if !now.is_finite() || !frame.is_finite() {
            return Moved::Nothing;
        }
        self.samples.push_back(Sample { frame, now, rtt: rtt.max(0.0) });
        while self.samples.len() > SAMPLES {
            self.samples.pop_front();
        }
        if self.base.is_none() {
            self.step_to(now, frame);
            return Moved::Stepped;
        }
        // A jump shows in the newest answer, however slow it was: no round
        // trip explains a quarter of a second on its own.
        let newest = self.station_at_f(frame).map_or(0.0, |p| now - p);
        if newest.abs() > STEP + rtt.max(0.0) / 2.0 {
            self.samples.clear();
            self.samples.push_back(Sample { frame, now, rtt });
            self.step_to(now, frame);
            return Moved::Stepped;
        }
        // Otherwise the most trustworthy recent answer; of equals, the
        // newest.
        let best = self.samples.iter().rev().copied()
            .min_by(|a, b| a.rtt.total_cmp(&b.rtt))
            .unwrap_or(Sample { frame, now, rtt });
        let predicted = self.station_at_f(best.frame).unwrap_or(best.now);
        let error = best.now - predicted;
        if error.abs() > STEP {
            // A jump: the older answers describe a clock that no longer
            // exists, so only this one counts.
            self.samples.clear();
            self.samples.push_back(Sample { frame, now, rtt });
            self.step_to(now, frame);
            return Moved::Stepped;
        }
        if error.abs() < IGNORE {
            return Moved::Nothing;
        }
        // Fold whatever slew is running into a new base here, and ease the
        // rest of the error in from this frame on.
        let here = self.station_at(current).unwrap_or(now);
        self.base = Some((current, here));
        self.slew = Some(Slew { amount: error, frames: (error.abs() / SLEW) * self.rate });
        // Every remembered answer is now measured against a moved clock;
        // shift them so the same error is not corrected twice.
        for sample in self.samples.iter_mut() {
            sample.now -= error;
        }
        Moved::Slewed
    }

    fn step_to(&mut self, now: f64, frame: f64) {
        let frame = frame.max(0.0);
        let whole = frame.floor();
        self.base = Some((whole as u64, now - (frame - whole) / self.rate));
        self.slew = None;
    }

    /// A clock that jumped for a reason the console knows about (a skip, a
    /// new session): drop what it learnt.
    pub fn jumped(&mut self, now: f64, frame: f64) {
        self.samples.clear();
        self.samples.push_back(Sample { frame, now, rtt: 0.0 });
        self.step_to(now, frame);
    }

    /// The output device was reopened, or runs at another rate now. Frames
    /// keep counting, but not in the same units and not across the gap, so
    /// the mapping is kept at the frame this was noticed and the next answer
    /// is taken as-is.
    pub fn device(&mut self, rate: u32, restarts: u64, current: u64) -> bool {
        let rate = rate.max(1) as f64;
        if rate == self.rate && restarts == self.restarts {
            return false;
        }
        let here = self.station_at(current);
        self.rate = rate;
        self.restarts = restarts;
        self.samples.clear();
        self.slew = None;
        if let Some(here) = here {
            self.base = Some((current, here));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_seconds_round_trip() {
        let mut clock = Clock::new(48_000);
        clock.anchor(100.0, 48_000);
        assert_eq!(clock.station_at(96_000), Some(101.0));
        assert_eq!(clock.frame_at(102.5), Some(168_000));
        assert_eq!(clock.frame_at(99.0), Some(0));
    }

    #[test]
    fn a_small_error_is_eased_in_not_stepped() {
        let mut clock = Clock::new(48_000);
        clock.observe(10.0, 0.0, 0.002, 0);
        // The station now reads 20 ms ahead of what the console predicted.
        let moved = clock.observe(11.02, 48_000.0, 0.002, 48_000);
        assert_eq!(moved, Moved::Slewed);
        let right_away = clock.station_at(48_000).unwrap();
        assert!((right_away - 11.0).abs() < 1e-9, "the correction jumped: {right_away}");
        // Four seconds later, at 5 ms a second, all of it is in.
        let later = clock.station_at(48_000 + 48_000 * 5).unwrap();
        assert!((later - 16.02).abs() < 1e-6, "{later}");
        // And the mapping stays monotonic and invertible throughout.
        let mut last = 0.0;
        for step in 0..50 {
            let frame = 48_000 + step * 4_800;
            let t = clock.station_at(frame).unwrap();
            assert!(t > last);
            last = t;
            assert!((clock.frame_at_f(t).unwrap() - frame as f64).abs() < 1e-3);
        }
    }

    #[test]
    fn the_shortest_round_trip_wins_over_the_latest() {
        let mut clock = Clock::new(48_000);
        clock.observe(10.0, 0.0, 0.001, 0);
        // A slow answer whose midpoint is 40 ms out: the best sample is
        // still the first, which agrees with the mapping, so nothing moves.
        let moved = clock.observe(11.04, 48_000.0, 0.300, 48_000);
        assert_eq!(moved, Moved::Nothing);
        assert!((clock.station_at(48_000).unwrap() - 11.0).abs() < 1e-9);
    }

    #[test]
    fn a_real_jump_steps() {
        let mut clock = Clock::new(48_000);
        clock.observe(10.0, 0.0, 0.001, 0);
        assert_eq!(clock.observe(200.0, 48_000.0, 0.001, 48_000), Moved::Stepped);
        assert!((clock.station_at(48_000).unwrap() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn a_device_at_another_rate_keeps_the_time_it_was_noticed_at() {
        let mut clock = Clock::new(48_000);
        clock.anchor(0.0, 0);
        assert!(clock.device(44_100, 1, 48_000));
        assert_eq!(clock.station_at(48_000), Some(1.0));
        assert_eq!(clock.station_at(48_000 + 44_100), Some(2.0));
        assert!(!clock.device(44_100, 1, 50_000));
    }
}
