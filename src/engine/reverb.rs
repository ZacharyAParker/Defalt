//! One shared room: a small algorithmic reverb on a send and return.
//!
//! Every deck has a send into it (post-fader, like the echo), and the master
//! has its return. Because it sits outside the decks, a record washed out into
//! the reverb keeps its tail after the deck stops or its fader closes -- which
//! is the whole trick of a reverb-out transition.
//!
//! Freeverb's topology (eight damped combs into four all-passes per side,
//! the right side's delays spread a little longer than the left's) with its
//! delay lengths scaled from 44.1k to the device rate. Every buffer is sized
//! when the engine opens the device; a device at another rate gets a new
//! reverb built off the audio thread.

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;
/// Freeverb's fixed input attenuation: eight combs in parallel add up.
const INPUT_GAIN: f32 = 0.015;
const MAX_PREDELAY_SECONDS: f32 = 0.25;
const SILENCE: f32 = 3.2e-5;

struct Comb {
    buffer: Vec<f32>,
    index: usize,
    store: f32,
}

impl Comb {
    fn new(length: usize) -> Self {
        Comb { buffer: vec![0.0; length.max(1)], index: 0, store: 0.0 }
    }

    #[inline]
    fn run(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let output = self.buffer[self.index];
        self.store = output * (1.0 - damp) + self.store * damp;
        self.buffer[self.index] = input + self.store * feedback;
        self.index += 1;
        if self.index == self.buffer.len() { self.index = 0; }
        output
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.store = 0.0;
    }
}

struct AllPass {
    buffer: Vec<f32>,
    index: usize,
}

impl AllPass {
    fn new(length: usize) -> Self {
        AllPass { buffer: vec![0.0; length.max(1)], index: 0 }
    }

    #[inline]
    fn run(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.index];
        let output = delayed - input;
        self.buffer[self.index] = input + delayed * 0.5;
        self.index += 1;
        if self.index == self.buffer.len() { self.index = 0; }
        output
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
    }
}

pub struct Reverb {
    combs: [[Comb; 8]; 2],
    allpasses: [[AllPass; 4]; 2],
    predelay: Vec<f32>,
    predelay_index: usize,
    predelay_frames: usize,
    feedback: f32,
    damp: f32,
    /// Return level, and the smoothed version actually applied.
    level: f32,
    level_state: f32,
    smoothing: f32,
    /// Frames in a row with nothing going in and nothing coming out. Past
    /// half a second of that the room is empty and is not worth computing.
    quiet: usize,
    idle_after: usize,
    rate: u32,
}

impl Reverb {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1);
        let scale = rate as f64 / 44_100.0;
        let length = |base: usize, side: usize| ((base + side * SPREAD) as f64 * scale).round() as usize;
        let mut reverb = Reverb {
            combs: std::array::from_fn(|side| std::array::from_fn(|i| Comb::new(length(COMBS[i], side)))),
            allpasses: std::array::from_fn(|side| {
                std::array::from_fn(|i| AllPass::new(length(ALLPASSES[i], side)))
            }),
            predelay: vec![0.0; (rate as f32 * MAX_PREDELAY_SECONDS) as usize + 1],
            predelay_index: 0,
            predelay_frames: 0,
            feedback: 0.0,
            damp: 0.0,
            level: 0.0,
            level_state: 0.0,
            smoothing: 1.0 - (-1.0 / (rate as f32 * 0.01)).exp(),
            quiet: usize::MAX,
            idle_after: rate as usize / 2,
            rate,
        };
        reverb.set(0.7, 0.5, 0.02, 0.0);
        reverb
    }

    /// Room size 0..1 (how long the tail is: about a second at 0.5 and
    /// several at 1), damping 0..1 (how quickly the top dies away), a
    /// pre-delay of up to a quarter second, and the return level 0..1.
    pub fn set(&mut self, size: f32, damping: f32, predelay_seconds: f32, level: f32) {
        self.feedback = 0.7 + size.clamp(0.0, 1.0) * 0.28;
        self.damp = damping.clamp(0.0, 1.0) * 0.4;
        self.predelay_frames = ((predelay_seconds.clamp(0.0, MAX_PREDELAY_SECONDS) * self.rate as f32)
            as usize).min(self.predelay.len() - 1);
        self.level = level.clamp(0.0, 1.0);
    }

    /// The settings as last set: `[size, damping, predelay seconds, level]`,
    /// for rebuilding the room at another rate.
    pub fn settings(&self) -> [f32; 4] {
        [
            (self.feedback - 0.7) / 0.28,
            self.damp / 0.4,
            self.predelay_frames as f32 / self.rate as f32,
            self.level,
        ]
    }

    pub fn clear(&mut self) {
        for comb in self.combs.iter_mut().flatten() { comb.clear(); }
        for pass in self.allpasses.iter_mut().flatten() { pass.clear(); }
        self.predelay.fill(0.0);
        self.quiet = usize::MAX;
    }

    /// True while the room still has something in it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ringing(&self) -> bool {
        self.quiet < self.idle_after
    }

    /// One frame of what the decks sent, in; one frame of return, out.
    #[inline]
    pub fn process(&mut self, input: [f32; 2]) -> [f32; 2] {
        self.level_state += (self.level - self.level_state) * self.smoothing;
        let loud = input[0].abs() > SILENCE || input[1].abs() > SILENCE;
        if !loud && self.quiet >= self.idle_after {
            return [0.0; 2];
        }

        let mono = (input[0] + input[1]) * INPUT_GAIN;
        let delayed = if self.predelay_frames == 0 {
            mono
        } else {
            let length = self.predelay.len();
            let read = if self.predelay_index >= self.predelay_frames {
                self.predelay_index - self.predelay_frames
            } else {
                self.predelay_index + length - self.predelay_frames
            };
            let out = self.predelay[read];
            self.predelay[self.predelay_index] = mono;
            self.predelay_index += 1;
            if self.predelay_index == length { self.predelay_index = 0; }
            out
        };

        let mut out = [0.0f32; 2];
        for side in 0..2 {
            let mut sum = 0.0;
            for comb in self.combs[side].iter_mut() {
                sum += comb.run(delayed, self.feedback, self.damp);
            }
            for pass in self.allpasses[side].iter_mut() {
                sum = pass.run(sum);
            }
            out[side] = sum;
        }

        if loud || out[0].abs() > SILENCE || out[1].abs() > SILENCE {
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(1);
        }
        [out[0] * self.level_state, out[1] * self.level_state]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_impulse_rings_on_and_then_the_room_goes_quiet() {
        let mut reverb = Reverb::new(48_000);
        reverb.set(0.5, 0.5, 0.0, 1.0);
        for _ in 0..4_800 { reverb.process([0.0; 2]); }
        reverb.process([1.0, 1.0]);
        let mut energy_early = 0.0;
        let mut energy_late = 0.0;
        for i in 0..48_000 {
            let [l, r] = reverb.process([0.0; 2]);
            assert!(l.is_finite() && r.is_finite());
            if i < 4_800 { energy_early += l * l + r * r; }
            if (24_000..28_800).contains(&i) { energy_late += l * l + r * r; }
        }
        assert!(energy_early > 1e-4, "no reverb at all");
        assert!(energy_late < energy_early, "the tail grew");
        let mut frames = 0;
        while reverb.ringing() && frames < 48_000 * 20 {
            reverb.process([0.0; 2]);
            frames += 1;
        }
        assert!(!reverb.ringing(), "the room never emptied");
    }

    #[test]
    fn the_largest_room_at_192k_stays_bounded_under_constant_input() {
        let mut reverb = Reverb::new(192_000);
        reverb.set(1.0, 0.0, 0.25, 1.0);
        let mut peak = 0.0f32;
        for i in 0..192_000 {
            let x = (i as f32 * 0.01).sin();
            let [l, r] = reverb.process([x, x]);
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak.is_finite() && peak < 4.0, "runaway at {peak}");
    }

    #[test]
    fn pre_delay_holds_the_first_reflection_back() {
        let mut reverb = Reverb::new(10_000);
        reverb.set(0.5, 0.0, 0.1, 1.0);
        for _ in 0..2_000 { reverb.process([0.0; 2]); }
        reverb.process([1.0; 2]);
        let first = (0..5_000).position(|_| reverb.process([0.0; 2])[0].abs() > 1e-6).unwrap();
        // 1000 frames of pre-delay, then the shortest comb (~253 frames).
        assert!(first > 1_000 && first < 1_400, "first reflection at {first}");
    }
}
