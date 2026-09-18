//! Per-deck tone controls: three bands and a sweep filter.
//!
//! RBJ cookbook biquads, direct form 1, one set of state per channel. All of
//! this runs inside the audio callback. New coefficients retain filter history
//! and interpolate over 20 ms, without allocations or per-sample trig calls.

/// A knob at the bottom of its travel means gone, not merely quiet. Kills are
/// how anyone actually mixes: you take the bass out of one record and put the
/// other one's in. -60dB is inaudible under a full mix.
const KILL_DB: f32 = -60.0;
const MAX_DB: f32 = 6.0;

#[derive(Clone, Copy, Default)]
struct State {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

#[derive(Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    channels: [State; 2],
    target: [f32; 5],
    step: [f32; 5],
    remaining: u32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::bypass()
    }
}

impl Biquad {
    fn bypass() -> Self {
        Self::from(1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    }

    fn from(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            channels: [State::default(); 2],
            target: [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0],
            step: [0.0; 5],
            remaining: 0,
        }
    }

    fn retune(&mut self, next: Self, frames: u32) {
        let current = [self.b0, self.b1, self.b2, self.a1, self.a2];
        if next.target == self.target { return; }
        self.target = next.target;
        self.remaining = frames.max(1);
        self.step = std::array::from_fn(|i| (self.target[i] - current[i]) / self.remaining as f32);
    }

    #[inline]
    pub fn run(&mut self, channel: usize, x: f32) -> f32 {
        if channel == 0 && self.remaining > 0 {
            self.remaining -= 1;
            let current = [self.b0, self.b1, self.b2, self.a1, self.a2];
            let values = if self.remaining == 0 { self.target } else {
                std::array::from_fn(|i| current[i] + self.step[i])
            };
            [self.b0, self.b1, self.b2, self.a1, self.a2] = values;
        }
        let s = &mut self.channels[channel];
        let y = self.b0 * x + self.b1 * s.x1 + self.b2 * s.x2 - self.a1 * s.y1 - self.a2 * s.y2;
        s.x2 = s.x1;
        s.x1 = x;
        s.y2 = s.y1;
        s.y1 = y;
        y
    }

    fn low_shelf(rate: f32, freq: f32, gain_db: f32) -> Self {
        if gain_db.abs() < 0.0001 { return Self::bypass(); }
        let freq = freq.min(rate * 0.45);
        let a = 10f32.powf(gain_db / 40.0);
        let w = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w.sin_cos();
        // Shelf slope of 1, the value that gives the widest shelf with no
        // ripple -- anything steeper rings on a bass kill.
        let alpha = sin / 2.0 * ((a + 1.0 / a) * (1.0 / 1.0 - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Self::from(
            a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha,
        )
    }

    fn high_shelf(rate: f32, freq: f32, gain_db: f32) -> Self {
        if gain_db.abs() < 0.0001 { return Self::bypass(); }
        let freq = freq.min(rate * 0.45);
        let a = 10f32.powf(gain_db / 40.0);
        let w = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / 2.0 * ((a + 1.0 / a) * (1.0 / 1.0 - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Self::from(
            a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha,
        )
    }

    fn peaking(rate: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        if gain_db.abs() < 0.0001 { return Self::bypass(); }
        let freq = freq.min(rate * 0.45);
        let a = 10f32.powf(gain_db / 40.0);
        let w = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        Self::from(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    pub fn low_pass(rate: f32, freq: f32, q: f32) -> Self {
        let freq = freq.min(rate * 0.45);
        let w = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        Self::from(
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    pub fn high_pass(rate: f32, freq: f32, q: f32) -> Self {
        let freq = freq.min(rate * 0.45);
        let w = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        Self::from(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }
}

/// A channel strip: three bands and one sweep.
///
/// Knob positions are 0..1 with 0.5 as flat, which is what a detented EQ knob
/// actually is. The sweep is -1..1 with 0 meaning out of circuit entirely --
/// a filter that is always in the path colours the record even at "off".
pub struct Strip {
    rate: f32,
    low: Biquad,
    mid: Biquad,
    high: Biquad,
    sweep: Option<Biquad>,
    settings: [f32; 4],
    pub bypassed: bool,
}

impl Strip {
    pub fn new(rate: u32) -> Self {
        let mut strip = Strip {
            rate: rate.max(1) as f32,
            low: Biquad::bypass(),
            mid: Biquad::bypass(),
            high: Biquad::bypass(),
            sweep: None,
            settings: [0.5, 0.5, 0.5, 0.0],
            bypassed: true,
        };
        strip.recompute();
        strip
    }

    /// Returns true if anything changed, so the caller can skip the work.
    pub fn set(&mut self, low: f32, mid: f32, high: f32, sweep: f32) -> bool {
        if ![low, mid, high, sweep].iter().all(|v| v.is_finite()) { return false; }
        let next = [
            low.clamp(0.0, 1.0),
            mid.clamp(0.0, 1.0),
            high.clamp(0.0, 1.0),
            sweep.clamp(-1.0, 1.0),
        ];
        if next == self.settings {
            return false;
        }
        self.settings = next;
        self.recompute();
        true
    }

    fn recompute(&mut self) {
        let [low, mid, high, sweep] = self.settings;
        let ramp = (self.rate * 0.020).ceil() as u32;
        self.low.retune(Biquad::low_shelf(self.rate, 250.0, decibels(low)), ramp);
        self.mid.retune(Biquad::peaking(self.rate, 1_000.0, 0.9, decibels(mid)), ramp);
        self.high.retune(Biquad::high_shelf(self.rate, 4_000.0, decibels(high)), ramp);

        let next_sweep = if sweep.abs() < 0.02 {
            None
        } else if sweep < 0.0 {
            // Left of centre sweeps a low-pass down from the top of the band.
            let t = (-sweep).clamp(0.0, 1.0);
            let freq = 20_000.0 * (1.0 - t).powf(2.4) + 120.0 * t;
            Some(Biquad::low_pass(self.rate, freq.clamp(60.0, 20_000.0), 0.9))
        } else {
            let t = sweep.clamp(0.0, 1.0);
            let freq = 20.0 + 9_000.0 * t.powf(2.4);
            Some(Biquad::high_pass(self.rate, freq.clamp(20.0, 12_000.0), 0.9))
        };
        if let Some(next) = next_sweep {
            self.sweep.get_or_insert_with(Biquad::bypass).retune(next, ramp);
        } else if let Some(current) = self.sweep.as_mut() {
            current.retune(Biquad::bypass(), ramp);
        }

        self.bypassed = self.low.remaining == 0 && self.mid.remaining == 0
            && self.high.remaining == 0 && self.settings[0] == 0.5
            && self.settings[1] == 0.5
            && self.settings[2] == 0.5
            && self.sweep.is_none();
    }

    #[inline]
    pub fn run(&mut self, channel: usize, sample: f32) -> f32 {
        let mut value = self.low.run(channel, sample);
        value = self.mid.run(channel, value);
        value = self.high.run(channel, value);
        if let Some(sweep) = self.sweep.as_mut() {
            value = sweep.run(channel, value);
        }
        // Finish the fade to bypass only after both channels saw the ramp.
        if channel == 1 && self.settings[3].abs() < 0.02
            && self.sweep.as_ref().is_some_and(|s| s.remaining == 0) {
            self.sweep = None;
        }
        value
    }
}

/// Knob position to gain. The bottom of the travel is a kill, and the taper
/// below centre is stretched so the useful range is not crammed into the last
/// few degrees.
/// The taper, exposed so autopilot's inverse can be tested against the real
/// curve rather than against a copy of it that could drift.
#[cfg(test)]
pub fn decibels_for_test(position: f32) -> f32 {
    decibels(position)
}

fn decibels(position: f32) -> f32 {
    if position <= 0.001 {
        return KILL_DB;
    }
    if position >= 0.5 {
        (position - 0.5) / 0.5 * MAX_DB
    } else {
        let t = 1.0 - position / 0.5;
        -(t * t) * 30.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(strip: &mut Strip, freq: f32, rate: f32) -> f32 {
        // Run a sine through and measure what comes out, after letting the
        // filter settle so we are not measuring its startup transient.
        let mut sum = 0.0;
        let total = (rate / freq * 40.0).max(rate * 0.10) as usize;
        for i in 0..total {
            let x = (std::f32::consts::TAU * freq * i as f32 / rate).sin();
            let y = strip.run(0, x);
            if i > total / 2 {
                sum += y * y;
            }
        }
        (sum / (total / 2) as f32).sqrt()
    }

    #[test]
    fn flat_knobs_leave_the_record_alone() {
        let mut strip = Strip::new(48_000);
        assert!(strip.bypassed);
        let level = energy(&mut strip, 1_000.0, 48_000.0);
        assert!((level - 0.707).abs() < 0.02, "got {level}");
    }

    #[test]
    fn turning_eq_preserves_the_waveform_at_the_control_boundary() {
        let mut live = Strip::new(48_000);
        let mut reference = Strip::new(48_000);
        live.set(0.2, 0.5, 0.5, -0.3);
        reference.set(0.2, 0.5, 0.5, -0.3);
        for i in 0..9600 {
            let x = (i as f32 * std::f32::consts::TAU * 87.0 / 48_000.0).sin() * 0.4;
            for channel in 0..2 { live.run(channel, x); reference.run(channel, x); }
        }
        live.set(0.2, 0.1, 0.9, -0.4);
        let x = (9600.0f32 * std::f32::consts::TAU * 87.0 / 48_000.0).sin() * 0.4;
        for channel in 0..2 {
            let jump = (live.run(channel, x) - reference.run(channel, x)).abs();
            assert!(jump < 0.002, "control change caused a sample jump of {jump}");
        }
    }

    #[test]
    fn rapid_filter_moves_and_bypass_crossings_stay_finite_and_bounded() {
        for rate in [32_000, 44_100, 48_000, 96_000] {
            let mut strip = Strip::new(rate);
            let mut peak = 0.0f32;
            for i in 0..rate {
                if i % 240 == 0 {
                    let value = (i as f32 / 2000.0).sin();
                    strip.set(0.3, 0.5, 0.6, value);
                }
                let x = (i as f32 * std::f32::consts::TAU * 220.0 / rate as f32).sin() * 0.2;
                for channel in 0..2 {
                    let y = strip.run(channel, x);
                    assert!(y.is_finite());
                    peak = peak.max(y.abs());
                }
            }
            assert!(peak < 1.0, "filter modulation produced an excessive peak: {peak}");
        }
    }

    #[test]
    fn killing_the_bass_removes_the_bass_and_keeps_the_top() {
        let mut strip = Strip::new(48_000);
        strip.set(0.0, 0.5, 0.5, 0.0);
        let bass = energy(&mut strip, 60.0, 48_000.0);
        let mut strip = Strip::new(48_000);
        strip.set(0.0, 0.5, 0.5, 0.0);
        let top = energy(&mut strip, 8_000.0, 48_000.0);
        assert!(bass < 0.02, "bass survived at {bass}");
        assert!(top > 0.5, "the top went with it, at {top}");
    }

    #[test]
    fn killing_the_top_removes_the_top_and_keeps_the_bass() {
        let mut strip = Strip::new(48_000);
        strip.set(0.5, 0.5, 0.0, 0.0);
        let top = energy(&mut strip, 10_000.0, 48_000.0);
        let mut strip = Strip::new(48_000);
        strip.set(0.5, 0.5, 0.0, 0.0);
        let bass = energy(&mut strip, 80.0, 48_000.0);
        assert!(top < 0.05, "top survived at {top}");
        assert!(bass > 0.5, "the bass went with it, at {bass}");
    }

    #[test]
    fn the_sweep_is_out_of_circuit_at_centre() {
        let mut strip = Strip::new(48_000);
        strip.set(0.5, 0.5, 0.5, 0.0);
        assert!(strip.sweep.is_none());
        assert!(strip.bypassed);
    }

    #[test]
    fn sweeping_right_takes_the_bottom_out() {
        let mut strip = Strip::new(48_000);
        strip.set(0.5, 0.5, 0.5, 0.9);
        assert!(!strip.bypassed);
        let bass = energy(&mut strip, 60.0, 48_000.0);
        assert!(bass < 0.1, "bass survived a high-pass sweep at {bass}");
    }

    #[test]
    fn sweeping_left_takes_the_top_out() {
        let mut strip = Strip::new(48_000);
        strip.set(0.5, 0.5, 0.5, -0.9);
        let top = energy(&mut strip, 10_000.0, 48_000.0);
        assert!(top < 0.1, "top survived a low-pass sweep at {top}");
    }

    #[test]
    fn repeating_a_knob_position_does_not_recompute() {
        let mut strip = Strip::new(48_000);
        assert!(strip.set(0.2, 0.5, 0.5, 0.0));
        assert!(!strip.set(0.2, 0.5, 0.5, 0.0));
    }
}
