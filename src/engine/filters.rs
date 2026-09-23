//! Per-deck tone controls: a three-band isolator and a sweep filter.
//!
//! The bands are a Linkwitz-Riley crossover rather than shelves and a bell.
//! Shelves cannot kill: a shelf at the bottom of its travel still drags the
//! next band down with it (the old bass kill took 18 dB out of 500 Hz) and
//! lets its own band leak through the far side of its slope. An isolator
//! splits the record into three bands that sum back to the record, turns each
//! one up or down, and adds them again -- so a kill removes that band and
//! nothing else, and flat really is flat.
//!
//! RBJ cookbook biquads, direct form 1, one set of state per channel. All of
//! this runs inside the audio callback: band gains and the sweep's
//! coefficients glide over 20 ms, without allocations or per-sample trig.

/// A band at the bottom of its travel is gone. The taper reaches -60 dB just
/// above the stop and the stop itself is silence, which is what a kill on an
/// isolator means.
const KILL_DB: f32 = -60.0;
const MAX_DB: f32 = 6.0;
/// The two crossover points. 250 Hz keeps kick and bass together in the low
/// band; 2.5 kHz puts vocals in the middle and hats and air on top.
pub const LOW_SPLIT: f32 = 250.0;
pub const HIGH_SPLIT: f32 = 2_500.0;
/// How long bringing the crossover into or out of circuit takes. The
/// crossover sums to an all-pass, not to a wire, so going in or out is a
/// crossfade rather than a switch.
const ENGAGE_SECONDS: f32 = 0.010;

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

    /// Forget the signal. Only ever called when the filter is out of circuit,
    /// so the next time it is heard it starts from silence rather than from
    /// whatever it was doing a minute ago.
    fn clear(&mut self) {
        self.channels = [State::default(); 2];
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

    /// Second-order all-pass. At Q = 1/sqrt(2) it is exactly the phase that
    /// a Linkwitz-Riley low and high pair sum to at the same frequency.
    fn all_pass(rate: f32, freq: f32, q: f32) -> Self {
        let freq = freq.min(rate * 0.45);
        let w = std::f32::consts::TAU * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        Self::from(
            1.0 - alpha,
            -2.0 * cos,
            1.0 + alpha,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }
}

const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// A fourth-order Linkwitz-Riley three-way split.
///
/// Each Linkwitz-Riley pair (two Butterworth sections squared) sums to an
/// all-pass at its crossover. The low band only goes through the first
/// split, so it is given the second split's all-pass as well; then all three
/// bands carry the same phase and `low + mid + high` is the input, delayed
/// in phase but flat in level at every frequency.
#[derive(Clone, Copy)]
pub struct Crossover {
    low: [Biquad; 2],
    rest: [Biquad; 2],
    mid: [Biquad; 2],
    high: [Biquad; 2],
    align: Biquad,
}

impl Crossover {
    pub fn new(rate: f32, low_split: f32, high_split: f32) -> Self {
        let lp = |f| Biquad::low_pass(rate, f, BUTTERWORTH_Q);
        let hp = |f| Biquad::high_pass(rate, f, BUTTERWORTH_Q);
        Crossover {
            low: [lp(low_split), lp(low_split)],
            rest: [hp(low_split), hp(low_split)],
            mid: [lp(high_split), lp(high_split)],
            high: [hp(high_split), hp(high_split)],
            align: Biquad::all_pass(rate, high_split, BUTTERWORTH_Q),
        }
    }

    /// One sample of one channel, as `[low, mid, high]`.
    #[inline]
    pub fn split(&mut self, channel: usize, x: f32) -> [f32; 3] {
        #[inline]
        fn twice(pair: &mut [Biquad; 2], channel: usize, x: f32) -> f32 {
            let once = pair[0].run(channel, x);
            pair[1].run(channel, once)
        }
        let low = self.align.run(channel, twice(&mut self.low, channel, x));
        let rest = twice(&mut self.rest, channel, x);
        let mid = twice(&mut self.mid, channel, rest);
        let high = twice(&mut self.high, channel, rest);
        [low, mid, high]
    }

    fn clear(&mut self) {
        for filter in self.low.iter_mut().chain(&mut self.rest).chain(&mut self.mid)
            .chain(&mut self.high) {
            filter.clear();
        }
        self.align.clear();
    }
}

/// A channel strip: three isolator bands and one sweep.
///
/// Knob positions are 0..1 with 0.5 as flat, which is what a detented EQ knob
/// actually is. The sweep is -1..1 with 0 meaning out of circuit entirely --
/// a filter that is always in the path colours the record even at "off".
pub struct Strip {
    rate: f32,
    crossover: Crossover,
    /// Band gains as they are now, where they are heading, and how far each
    /// frame moves them.
    gains: [f32; 3],
    target: [f32; 3],
    step: [f32; 3],
    remaining: u32,
    /// 0 is the dry record, 1 is the crossover's output. Moves over
    /// `ENGAGE_SECONDS` whenever the isolator comes into or out of circuit.
    engage: f32,
    engage_step: f32,
    sweep: Option<Biquad>,
    settings: [f32; 4],
    /// True when nothing here is doing anything and the deck can skip the
    /// strip entirely. Comes back on by itself once every knob is at its
    /// detent again and the last glide has finished.
    pub bypassed: bool,
}

impl Strip {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1) as f32;
        Strip {
            rate,
            crossover: Crossover::new(rate, LOW_SPLIT, HIGH_SPLIT),
            gains: [1.0; 3],
            target: [1.0; 3],
            step: [0.0; 3],
            remaining: 0,
            engage: 0.0,
            engage_step: 1.0 / (rate * ENGAGE_SECONDS).max(1.0),
            sweep: None,
            settings: [0.5, 0.5, 0.5, 0.0],
            bypassed: true,
        }
    }

    /// The knob positions last set, as `[low, mid, high, sweep]`. Read when a
    /// strip has to be rebuilt at another rate without the record noticing.
    pub fn settings(&self) -> [f32; 4] {
        self.settings
    }

    /// Returns true if anything changed, so the caller can skip the work.
    pub fn set(&mut self, low: f32, mid: f32, high: f32, sweep: f32) -> bool {
        let ramp = (self.rate * 0.020).ceil() as u32;
        self.set_over(low, mid, high, sweep, ramp)
    }

    /// `set`, gliding over `frames` instead of 20 ms. Automation calls this
    /// every few frames with a glide as long as the gap, so a curve comes out
    /// as a curve rather than as a staircase smoothed 20 ms late.
    pub fn set_over(&mut self, low: f32, mid: f32, high: f32, sweep: f32, frames: u32) -> bool {
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
        self.recompute(frames.max(1));
        true
    }

    fn flat(&self) -> bool {
        self.settings[..3].iter().all(|&v| v == 0.5)
    }

    fn recompute(&mut self, ramp: u32) {
        let [low, mid, high, sweep] = self.settings;
        let target = [gain(low), gain(mid), gain(high)];
        if target != self.target {
            self.target = target;
            self.remaining = ramp;
            self.step = std::array::from_fn(|i| (target[i] - self.gains[i]) / ramp as f32);
        }

        let next_sweep = sweep_cutoff(sweep).map(|(low_pass, freq)| if low_pass {
            Biquad::low_pass(self.rate, freq, 0.9)
        } else {
            Biquad::high_pass(self.rate, freq, 0.9)
        });
        if let Some(next) = next_sweep {
            self.sweep.get_or_insert_with(Biquad::bypass).retune(next, ramp);
        } else if let Some(current) = self.sweep.as_mut() {
            current.retune(Biquad::bypass(), ramp);
        }

        if !self.flat() || self.sweep.is_some() || self.remaining > 0 {
            self.bypassed = false;
        }
    }

    /// One stereo frame.
    #[inline]
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        [self.run(0, frame[0]), self.run(1, frame[1])]
    }

    /// One sample of one channel. Glides advance on channel 0, so a caller
    /// feeding stereo runs channel 0 then channel 1 for each frame.
    #[inline]
    pub fn run(&mut self, channel: usize, sample: f32) -> f32 {
        if channel == 0 {
            if self.remaining > 0 {
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.gains = self.target;
                } else {
                    for i in 0..3 { self.gains[i] += self.step[i]; }
                }
            }
            let wanted = if self.flat() && self.remaining == 0 { 0.0 } else { 1.0 };
            if self.engage < wanted {
                self.engage = (self.engage + self.engage_step).min(1.0);
            } else if self.engage > wanted {
                self.engage = (self.engage - self.engage_step).max(0.0);
            }
        }

        let mut value = sample;
        if self.engage > 0.0 {
            let [low, mid, high] = self.crossover.split(channel, sample);
            let wet = low * self.gains[0] + mid * self.gains[1] + high * self.gains[2];
            value = sample + (wet - sample) * self.engage;
        }
        if let Some(sweep) = self.sweep.as_mut() {
            value = sweep.run(channel, value);
        }

        if channel == 1 {
            // Finish the fade to bypass only after both channels saw the ramp.
            if self.settings[3].abs() < 0.02
                && self.sweep.as_ref().is_some_and(|s| s.remaining == 0) {
                self.sweep = None;
            }
            if self.engage == 0.0 && self.sweep.is_none() && self.remaining == 0 && self.flat() {
                self.crossover.clear();
                self.bypassed = true;
            }
        }
        value
    }
}

/// Knob position to band gain: a kill at the stop, the taper in between.
/// Where a sweep position puts the filter: `(low_pass, cutoff Hz)`, or
/// `None` when it is out of circuit. Public so the panel can say what the
/// fader is doing in the same numbers the filter uses.
pub fn sweep_cutoff(sweep: f32) -> Option<(bool, f32)> {
    if !sweep.is_finite() || sweep.abs() < 0.02 {
        None
    } else if sweep < 0.0 {
        // Left of centre sweeps a low-pass down from the top of the band.
        let t = (-sweep).clamp(0.0, 1.0);
        let freq = 20_000.0 * (1.0 - t).powf(2.4) + 120.0 * t;
        Some((true, freq.clamp(60.0, 20_000.0)))
    } else {
        let t = sweep.clamp(0.0, 1.0);
        let freq = 20.0 + 9_000.0 * t.powf(2.4);
        Some((false, freq.clamp(20.0, 12_000.0)))
    }
}

/// A band knob's gain in dB, as the isolator applies it; `None` at the
/// bottom of the travel, which is a kill rather than a number.
pub fn band_db(position: f32) -> Option<f32> {
    if position <= 0.001 { None } else { Some(decibels(position)) }
}

fn gain(position: f32) -> f32 {
    if position <= 0.001 {
        return 0.0;
    }
    10f32.powf(decibels(position) / 20.0)
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

    /// Level change at `freq` in dB for a strip set to these knobs.
    fn response(knobs: [f32; 4], freq: f32) -> f32 {
        let mut strip = Strip::new(48_000);
        strip.set(knobs[0], knobs[1], knobs[2], knobs[3]);
        20.0 * (energy(&mut strip, freq, 48_000.0) / std::f32::consts::FRAC_1_SQRT_2).log10()
    }

    #[test]
    fn flat_knobs_leave_the_record_alone() {
        let mut strip = Strip::new(48_000);
        assert!(strip.bypassed);
        let level = energy(&mut strip, 1_000.0, 48_000.0);
        assert!((level - 0.707).abs() < 0.02, "got {level}");
    }

    #[test]
    fn the_isolator_sums_flat_at_unity_across_the_band() {
        // Nudged off the detent so the crossover is really in circuit, then
        // brought back: the bands must add up to the record at every
        // frequency, crossovers included.
        for freq in [40.0, 120.0, 250.0, 700.0, 2_500.0, 6_000.0, 15_000.0] {
            let mut strip = Strip::new(48_000);
            strip.set(0.5, 0.5, 0.499_999, 0.0);
            assert!(!strip.bypassed);
            let level = 20.0 * (energy(&mut strip, freq, 48_000.0) / 0.707_106_8).log10();
            assert!(level.abs() < 0.1, "{freq} Hz came out at {level:.2} dB");
        }
    }

    #[test]
    fn a_bass_kill_takes_the_bass_and_leaves_the_band_above_it() {
        assert!(response([0.0, 0.5, 0.5, 0.0], 60.0) < -40.0);
        // The old shelf took 18 dB out of here.
        assert!(response([0.0, 0.5, 0.5, 0.0], 500.0) > -1.0);
        assert!(response([0.0, 0.5, 0.5, 0.0], 2_000.0) > -0.2);
    }

    #[test]
    fn a_mid_kill_takes_the_middle_and_leaves_both_edges() {
        assert!(response([0.5, 0.0, 0.5, 0.0], 800.0) < -30.0);
        assert!(response([0.5, 0.0, 0.5, 0.0], 60.0) > -0.5);
        // The old bell took 12 dB out of here.
        assert!(response([0.5, 0.0, 0.5, 0.0], 8_000.0) > -0.5);
    }

    #[test]
    fn a_high_kill_takes_the_top_and_leaves_the_middle() {
        assert!(response([0.5, 0.5, 0.0, 0.0], 12_000.0) < -40.0);
        assert!(response([0.5, 0.5, 0.0, 0.0], 800.0) > -0.5);
    }

    #[test]
    fn a_boost_lifts_only_its_band() {
        assert!((response([1.0, 0.5, 0.5, 0.0], 60.0) - 6.0).abs() < 0.3);
        assert!(response([1.0, 0.5, 0.5, 0.0], 5_000.0).abs() < 0.3);
    }

    #[test]
    fn the_strip_goes_back_to_bypass_once_the_knobs_are_home() {
        let mut strip = Strip::new(48_000);
        strip.set(0.2, 0.5, 0.5, 0.0);
        assert!(!strip.bypassed);
        for i in 0..4_800 { strip.process([(i as f32 * 0.05).sin(); 2]); }
        strip.set(0.5, 0.5, 0.5, 0.0);
        for i in 0..4_800 { strip.process([(i as f32 * 0.05).sin(); 2]); }
        assert!(strip.bypassed, "a knob touched once kept the filters in forever");
        // And in bypass the record really is untouched.
        assert_eq!(strip.process([0.25, -0.5]), [0.25, -0.5]);
    }

    #[test]
    fn engaging_and_releasing_the_isolator_does_not_click() {
        let mut strip = Strip::new(48_000);
        let mut previous = 0.0f32;
        let mut largest = 0.0f32;
        for i in 0..48_000 {
            if i == 10_000 { strip.set(0.5, 0.5, 0.45, 0.0); }
            if i == 30_000 { strip.set(0.5, 0.5, 0.5, 0.0); }
            let x = (i as f32 * std::f32::consts::TAU * 330.0 / 48_000.0).sin() * 0.5;
            let [y, _] = strip.process([x, x]);
            if i > 0 { largest = largest.max((y - previous).abs()); }
            previous = y;
        }
        // A 330 Hz sine at 0.5 moves at most 0.0216 per sample.
        assert!(largest < 0.03, "largest step {largest}");
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
