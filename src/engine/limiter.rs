//! The last thing before the output: a look-ahead peak limiter.
//!
//! Two decks at unity with their bass up, plus the radio, plus a reverb tail,
//! is more than full scale, and the clamp after this used to be the only
//! thing stopping it -- a clamp is a square wave, which is the loudest kind
//! of distortion there is. This sees each peak 1.5 ms before it arrives and
//! has the gain already down by the time it does, so the clamp is left with
//! nothing to do.
//!
//! How it is guaranteed never to overshoot: every sample asks for the gain
//! that would put it exactly at the ceiling. That request is held as a
//! running minimum over the look-ahead window, recovered from with a 50 ms
//! release, and then averaged over the same window. The average of values
//! that are each no higher than a peak's request is itself no higher, and it
//! is applied to audio delayed by just enough that the window being averaged
//! is the one containing that peak. The average is also what makes the
//! attack a smooth ramp rather than a step.
//!
//! Stereo linked, so the image does not lean when one side is limited.
//! Everything is sized when it is built.

/// -1 dBFS. Leaves room for the inter-sample overs a DAC's reconstruction
/// filter adds to a sample-peak-limited signal.
pub const CEILING: f32 = 0.891_250_9;
const LOOKAHEAD_SECONDS: f32 = 0.0015;
const RELEASE_SECONDS: f32 = 0.050;

pub struct Limiter {
    enabled: bool,
    window: usize,
    /// The delayed audio, one window long.
    delay: Vec<[f32; 2]>,
    delay_index: usize,
    /// Monotonic queue of (sample number, requested gain) for the running
    /// minimum: requests only ever increase from head to tail.
    queue: Vec<(u64, f32)>,
    head: usize,
    count: usize,
    /// The window average, as a ring and its running sum.
    average: Vec<f32>,
    average_index: usize,
    sum: f64,
    envelope: f32,
    release: f32,
    sample: u64,
    /// Lowest gain applied since `take_reduction` last read it.
    lowest: f32,
}

impl Limiter {
    pub fn new(rate: u32) -> Self {
        let window = ((rate.max(1) as f32 * LOOKAHEAD_SECONDS).ceil() as usize).max(2);
        Limiter {
            enabled: true,
            window,
            delay: vec![[0.0; 2]; window],
            delay_index: 0,
            queue: vec![(0, 1.0); window + 1],
            head: 0,
            count: 0,
            average: vec![1.0; window],
            average_index: 0,
            sum: window as f64,
            envelope: 1.0,
            release: 1.0 - (-1.0 / (rate.max(1) as f32 * RELEASE_SECONDS)).exp(),
            sample: 0,
            lowest: 1.0,
        }
    }

    /// Off means the gain glides back to unity; the delay stays in, so
    /// switching it never moves the audio in time.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Gain reduction at its deepest since the last read, in dB (positive
    /// numbers: 3.0 means the output was pulled down 3 dB). Resets.
    pub fn take_reduction(&mut self) -> f32 {
        let lowest = self.lowest;
        self.lowest = 1.0;
        -20.0 * lowest.max(1e-6).log10()
    }

    #[inline]
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        let peak = frame[0].abs().max(frame[1].abs());
        let request = if self.enabled && peak > CEILING { CEILING / peak } else { 1.0 };

        // Running minimum over the last `window` requests.
        let capacity = self.queue.len();
        while self.count > 0 {
            let tail = (self.head + self.count - 1) % capacity;
            if self.queue[tail].1 >= request { self.count -= 1; } else { break; }
        }
        let tail = (self.head + self.count) % capacity;
        self.queue[tail] = (self.sample, request);
        self.count += 1;
        let oldest = self.sample.saturating_sub(self.window as u64 - 1);
        while self.queue[self.head].0 < oldest {
            self.head = (self.head + 1) % capacity;
            self.count -= 1;
        }
        let minimum = self.queue[self.head].1;
        self.sample += 1;

        self.envelope = minimum.min(self.envelope + (1.0 - self.envelope) * self.release);

        self.sum += self.envelope as f64 - self.average[self.average_index] as f64;
        self.average[self.average_index] = self.envelope;
        self.average_index += 1;
        if self.average_index == self.window {
            self.average_index = 0;
            // Re-add from scratch once a window, so rounding in the running
            // sum cannot creep over hours of playing.
            self.sum = self.average.iter().map(|&v| v as f64).sum();
        }
        let gain = (self.sum / self.window as f64) as f32;
        self.lowest = self.lowest.min(gain);

        // Delayed by one less than the window, which lines the peak up with
        // the window being averaged.
        self.delay[self.delay_index] = frame;
        self.delay_index += 1;
        if self.delay_index == self.window { self.delay_index = 0; }
        let out = self.delay[self.delay_index];
        [out[0] * gain, out[1] * gain]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_gets_past_the_ceiling() {
        let mut limiter = Limiter::new(48_000);
        let mut peak = 0.0f32;
        for i in 0..48_000 {
            // A loud sine with sudden spikes on top.
            let mut x = (i as f32 * 0.03).sin() * 1.6;
            if i % 5_000 == 0 { x = 3.0; }
            let [l, r] = limiter.process([x, -x * 0.5]);
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak <= CEILING * 1.0001, "limited output peaked at {peak}");
        assert!(limiter.take_reduction() > 3.0);
    }

    #[test]
    fn quiet_audio_goes_through_untouched_just_later() {
        let mut limiter = Limiter::new(48_000);
        let window = limiter.window;
        let input: Vec<f32> = (0..2_000).map(|i| (i as f32 * 0.07).sin() * 0.5).collect();
        let output: Vec<f32> = input.iter().map(|&x| limiter.process([x, x])[0]).collect();
        for i in window..2_000 {
            assert!((output[i] - input[i - (window - 1)]).abs() < 1e-6);
        }
        assert_eq!(limiter.take_reduction(), 0.0);
    }

    #[test]
    fn the_gain_ramps_down_before_a_peak_and_recovers_after() {
        let mut limiter = Limiter::new(48_000);
        let lag = limiter.window - 1;
        let input: Vec<f32> = (0..30_000).map(|i| if i == 2_000 { 2.0 } else { 0.1 }).collect();
        let mut gains = Vec::new();
        for i in 0..30_000 {
            let [l, _] = limiter.process([input[i], input[i]]);
            if i >= lag { gains.push(l / input[i - lag]); }
        }
        // A smooth dip, not a step: no gain change bigger than the window's
        // share of the reduction.
        let steps = gains.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.1).count();
        assert!(steps <= 1, "the gain stepped {steps} times");
        let last = gains[gains.len() - 1];
        assert!((last - 1.0).abs() < 0.01, "never recovered: {last}");
    }

    #[test]
    fn switching_off_lets_overs_through_again() {
        let mut limiter = Limiter::new(48_000);
        limiter.set_enabled(false);
        let mut peak = 0.0f32;
        for _ in 0..1_000 { peak = peak.max(limiter.process([1.5, 1.5])[0]); }
        assert!((peak - 1.5).abs() < 1e-6);
    }
}
