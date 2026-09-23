//! Sample-rate conversion, done once, at load, off the audio thread.
//!
//! A deck can read a record at any rate -- its cubic reader is what makes
//! pitch and scrubbing work -- but cubic is a poor converter: a 44.1k record
//! on a 48k device picks up images a few kHz under the top of the band on
//! every sample, and a 24k voice line sounds like a telephone. So records and
//! voice lines are converted to the device rate when they are decoded, with a
//! proper filter, and the deck only has to interpolate when someone actually
//! moves the pitch.
//!
//! Polyphase windowed sinc. For the rates anyone uses (44.1/48/88.2/96/192,
//! 22.05/24 for speech) the ratio reduces to at most a few hundred phases, so
//! every phase gets its own exact kernel and the inner loop is a plain dot
//! product. Anything stranger falls back to interpolating a finely sampled
//! kernel, which is slower and just as clean.

/// Zero crossings of the sinc on each side of the centre, in the lower rate's
/// samples. 32 with a Kaiser window of beta 9 is ~90 dB of stopband, which
/// is below the noise floor of anything that will ever be played through this.
const ZERO_CROSSINGS: usize = 32;
const BETA: f64 = 9.0;
/// Where the passband ends, as a fraction of the lower Nyquist. The window's
/// transition band is centred here, so images and aliases are down by the
/// full stopband by the time they reach the Nyquist.
const CUTOFF: f64 = 0.91;
/// Above this many distinct phases the exact bank is too big to be worth
/// building and the interpolated kernel is used instead.
const MAX_BANK_PHASES: u64 = 4096;
/// Resolution of the interpolated kernel, per zero crossing.
const TABLE_STEPS: usize = 512;

/// Convert interleaved stereo from one rate to another.
///
/// Returns the input unchanged (copied) when the rates already agree or
/// either is zero. The output is `ceil(frames * to / from)` frames long and
/// is time-aligned with the input: output frame `j` is the input at
/// `j * from / to`, so a playhead in seconds means the same thing in both.
pub fn stereo(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || to == 0 || samples.len() < 2 {
        return samples.to_vec();
    }
    let frames = samples.len() / 2;
    let kernel = Kernel::new(from, to);
    let out_frames = ((frames as u128 * to as u128).div_ceil(from as u128)) as usize;

    // Planar and zero padded, so the inner loop never has to ask whether it
    // is reading off either end of the record.
    let pad = kernel.taps;
    let planar: [Vec<f32>; 2] = std::array::from_fn(|channel| {
        let mut plane = vec![0.0f32; frames + pad * 2];
        for (i, frame) in samples.chunks_exact(2).enumerate() {
            plane[pad + i] = frame[channel];
        }
        plane
    });

    // One channel per thread: conversion is the slowest part of a load and
    // the two channels have nothing to say to each other.
    let [left, right] = std::thread::scope(|scope| {
        let handles = planar.each_ref().map(|plane| {
            let kernel = &kernel;
            scope.spawn(move || kernel.run(plane, pad, out_frames))
        });
        handles.map(|handle| handle.join().expect("resampler thread"))
    });

    let mut out = Vec::with_capacity(out_frames * 2);
    for (l, r) in left.iter().zip(&right) {
        out.push(*l);
        out.push(*r);
    }
    out
}

struct Kernel {
    /// Output frames advance the input by `step / phases` input frames.
    step: u64,
    phases: u64,
    taps: usize,
    /// First tap's offset from the integer input position.
    first: isize,
    /// `phases * taps` exact weights, or empty when too many phases.
    bank: Vec<f32>,
    /// Finely sampled half kernel for the fallback, indexed by |distance| in
    /// scaled units * TABLE_STEPS.
    table: Vec<f32>,
    scale: f64,
}

impl Kernel {
    fn new(from: u32, to: u32) -> Self {
        let divisor = gcd(from as u64, to as u64);
        let step = from as u64 / divisor;
        let phases = to as u64 / divisor;
        // Distances are measured in input samples. Downsampling narrows the
        // passband, which widens the kernel in input samples by the ratio.
        let scale = CUTOFF * (to as f64 / from as f64).min(1.0);
        let half = (ZERO_CROSSINGS as f64 / scale).ceil() as usize;
        let taps = half * 2;
        let first = -(half as isize) + 1;

        let weight = |distance: f64| -> f64 {
            let x = distance * scale;
            if x.abs() >= ZERO_CROSSINGS as f64 {
                return 0.0;
            }
            scale * sinc(x) * kaiser(x / ZERO_CROSSINGS as f64)
        };

        let mut bank = Vec::new();
        let mut table = Vec::new();
        if phases <= MAX_BANK_PHASES {
            bank.reserve(phases as usize * taps);
            for phase in 0..phases {
                let fraction = phase as f64 / phases as f64;
                let start = bank.len();
                let mut sum = 0.0;
                for k in 0..taps {
                    let w = weight((first + k as isize) as f64 - fraction);
                    sum += w;
                    bank.push(w as f32);
                }
                // Every phase passes DC at exactly unity. Otherwise the gain
                // wobbles with the phase, and a wobble at the phase rate is a
                // tone.
                if sum.abs() > 1e-9 {
                    for w in &mut bank[start..] {
                        *w = (*w as f64 / sum) as f32;
                    }
                }
            }
        } else {
            let size = ZERO_CROSSINGS * TABLE_STEPS + 2;
            table = (0..size)
                .map(|i| {
                    let x = i as f64 / TABLE_STEPS as f64;
                    if x >= ZERO_CROSSINGS as f64 { 0.0 } else {
                        (scale * sinc(x) * kaiser(x / ZERO_CROSSINGS as f64)) as f32
                    }
                })
                .collect();
        }
        Kernel { step, phases, taps, first, bank, table, scale }
    }

    fn run(&self, plane: &[f32], pad: usize, out_frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(out_frames);
        let mut weights = vec![0.0f32; self.taps];
        for j in 0..out_frames as u64 {
            let numerator = j * self.step;
            let base = (numerator / self.phases) as isize;
            let phase = numerator % self.phases;
            let start = (pad as isize + base + self.first) as usize;
            let window = &plane[start..start + self.taps];
            let weights: &[f32] = if self.bank.is_empty() {
                let fraction = phase as f64 / self.phases as f64;
                let mut sum = 0.0f32;
                for (k, w) in weights.iter_mut().enumerate() {
                    let distance = ((self.first + k as isize) as f64 - fraction).abs() * self.scale;
                    let position = distance * TABLE_STEPS as f64;
                    let index = position as usize;
                    if index + 1 >= self.table.len() {
                        *w = 0.0;
                    } else {
                        let t = (position - index as f64) as f32;
                        *w = self.table[index] + (self.table[index + 1] - self.table[index]) * t;
                    }
                    sum += *w;
                }
                if sum.abs() > 1e-9 {
                    for w in weights.iter_mut() { *w /= sum; }
                }
                &weights
            } else {
                let at = phase as usize * self.taps;
                &self.bank[at..at + self.taps]
            };
            out.push(dot(window, weights));
        }
        out
    }
}

/// Written as eight independent sums so the compiler can keep them in
/// vector registers rather than serialising on one accumulator.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let chunks = a.len() / 8;
    for i in 0..chunks {
        for lane in 0..8 {
            lanes[lane] += a[i * 8 + lane] * b[i * 8 + lane];
        }
    }
    let mut sum: f32 = lanes.iter().sum();
    for i in chunks * 8..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Kaiser window over -1..1.
fn kaiser(x: f64) -> f64 {
    if x.abs() >= 1.0 { return 0.0; }
    bessel_i0(BETA * (1.0 - x * x).sqrt()) / bessel_i0(BETA)
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        term *= half / k as f64;
        let add = term * term;
        sum += add;
        if add < sum * 1e-17 { break; }
    }
    sum
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 { (a, b) = (b, a % b); }
    a.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn tone(hz: f64, rate: u32, seconds: f64) -> Vec<f32> {
        (0..(rate as f64 * seconds) as usize)
            .flat_map(|i| {
                let v = (TAU * hz * i as f64 / rate as f64).sin() as f32 * 0.5;
                [v, v]
            })
            .collect()
    }

    /// Error against the ideal tone at the new rate, in dB relative to the
    /// tone, over the middle of the output (the edges are a record starting
    /// and stopping, which is not the resampler's fault).
    fn error_db(out: &[f32], hz: f64, rate: u32) -> f64 {
        let frames = out.len() / 2;
        let (mut signal, mut error) = (0.0f64, 0.0f64);
        for j in frames / 4..frames * 3 / 4 {
            let ideal = (TAU * hz * j as f64 / rate as f64).sin() * 0.5;
            signal += ideal * ideal;
            error += (out[j * 2] as f64 - ideal).powi(2);
        }
        10.0 * (error / signal).log10()
    }

    #[test]
    fn a_1khz_tone_converts_44k_to_48k_with_the_images_far_down() {
        let out = stereo(&tone(1_000.0, 44_100, 0.5), 44_100, 48_000);
        assert_eq!(out.len() / 2, 24_000);
        let db = error_db(&out, 1_000.0, 48_000);
        assert!(db < -80.0, "residual (images + distortion) at {db:.1} dB");
    }

    #[test]
    fn a_high_tone_survives_downsampling_48k_to_44k() {
        let out = stereo(&tone(15_000.0, 48_000, 0.5), 48_000, 44_100);
        let db = error_db(&out, 15_000.0, 44_100);
        assert!(db < -60.0, "15 kHz came through at {db:.1} dB of error");
    }

    #[test]
    fn content_above_the_new_nyquist_is_removed_rather_than_folded() {
        // 23 kHz at 48k has nowhere to go at 44.1k except down to 21.1 kHz
        // as an alias. It has to be filtered out instead.
        let out = stereo(&tone(23_000.0, 48_000, 0.5), 48_000, 44_100);
        let frames = out.len() / 2;
        let rms = (out[frames / 2..frames * 3 / 2].iter().map(|s| s * s).sum::<f32>()
            / frames as f32).sqrt();
        assert!(20.0 * (rms / 0.354).log10() < -70.0, "alias at {rms}");
    }

    #[test]
    fn speech_at_24k_doubles_cleanly() {
        let out = stereo(&tone(3_000.0, 24_000, 0.5), 24_000, 48_000);
        assert_eq!(out.len(), 48_000);
        assert!(error_db(&out, 3_000.0, 48_000) < -80.0);
    }

    #[test]
    fn an_awkward_ratio_uses_the_interpolated_kernel_and_is_still_clean() {
        let kernel = Kernel::new(44_100, 47_999);
        assert!(kernel.bank.is_empty());
        let out = stereo(&tone(1_000.0, 44_100, 0.3), 44_100, 47_999);
        assert!(error_db(&out, 1_000.0, 47_999) < -70.0);
    }

    #[test]
    fn matching_rates_are_left_alone() {
        let input = tone(440.0, 48_000, 0.01);
        assert_eq!(stereo(&input, 48_000, 48_000), input);
    }
}
