//! Waveform peaks, computed from the samples we already decoded.
//!
//! The station computes these too, in Python, into a numpy sidecar. The
//! console does not read those: by the time a record is on a deck its samples
//! are already in memory, and walking them once is faster than finding,
//! parsing and validating a cache file.
//!
//! Each bucket carries the extremes of the waveform and where the energy sat,
//! so a drop is something you see rather than something you count bars to.

use crate::engine::decode::Track;
use crate::engine::filters::{Crossover, HIGH_SPLIT, LOW_SPLIT};

/// Mono samples per bucket at the finest level. 256 gives about 5ms of record
/// per bucket, which is finer than any zoom level can show and cheap to
/// reduce from.
const WINDOW: usize = 256;

#[derive(Clone, Copy, Default)]
pub struct Bucket {
    pub min: f32,
    pub max: f32,
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

pub struct Peaks {
    pub buckets: Vec<Bucket>,
    /// Seconds of record per bucket.
    pub seconds_each: f64,
    /// 1.0 / the loudest sample, so a quiet record still fills the display and
    /// a hot master does not clip out of the top of it.
    pub scale: f32,
}

impl Peaks {
    /// Reduce a span of the track to at most `want` buckets, for drawing.
    ///
    /// Taking extremes rather than averages: a waveform that averages its way
    /// through a transient is a waveform that hides the transient.
    pub fn window(&self, from_seconds: f64, to_seconds: f64, want: usize) -> Vec<Bucket> {
        if self.buckets.is_empty() || want == 0 || self.seconds_each <= 0.0 {
            return Vec::new();
        }
        let first = (from_seconds / self.seconds_each).floor().max(0.0) as usize;
        let last = ((to_seconds / self.seconds_each).ceil() as usize)
            .min(self.buckets.len());
        if last <= first {
            return Vec::new();
        }

        let span = last - first;
        let mut out = Vec::with_capacity(want);
        for i in 0..want {
            let start = first + span * i / want;
            let end = (first + span * (i + 1) / want).max(start + 1).min(last);
            out.push(fold(&self.buckets[start..end]));
        }
        out
    }
}

fn fold(group: &[Bucket]) -> Bucket {
    let mut out = Bucket { min: 0.0, max: 0.0, low: 0.0, mid: 0.0, high: 0.0 };
    if group.is_empty() {
        return out;
    }
    for bucket in group {
        out.min = out.min.min(bucket.min);
        out.max = out.max.max(bucket.max);
        out.low += bucket.low;
        out.mid += bucket.mid;
        out.high += bucket.high;
    }
    let count = group.len() as f32;
    out.low /= count;
    out.mid /= count;
    out.high /= count;
    out
}

/// Walk a decoded record once and reduce it to buckets.
pub fn analyse(track: &Track) -> Peaks {
    let rate = track.sample_rate.max(1);
    let frames = track.frames();

    // Bands by filter rather than by FFT: one pass, no windowing, and the
    // answer only has to be good enough to colour a few thousand pixels. The
    // same Linkwitz-Riley split the isolator uses, so the three bands really
    // are three bands that sum to the record -- and the colours line up with
    // what the EQ knobs take out.
    let mut crossover = Crossover::new(rate as f32, LOW_SPLIT, HIGH_SPLIT);

    let mut buckets = Vec::with_capacity(frames / WINDOW + 1);
    let mut current = Bucket { min: 0.0, max: 0.0, low: 0.0, mid: 0.0, high: 0.0 };
    let mut counted = 0usize;
    let mut loudest = 0.0f32;

    for frame in 0..frames {
        let left = track.samples[frame * 2];
        let right = track.samples[frame * 2 + 1];
        let mono = (left + right) * 0.5;

        let [low, mid, high] = crossover.split(0, mono);

        current.min = current.min.min(mono);
        current.max = current.max.max(mono);
        current.low += low * low;
        current.mid += mid * mid;
        current.high += high * high;
        loudest = loudest.max(mono.abs());
        counted += 1;

        if counted == WINDOW {
            finish(&mut current, counted);
            buckets.push(current);
            current = Bucket { min: 0.0, max: 0.0, low: 0.0, mid: 0.0, high: 0.0 };
            counted = 0;
        }
    }
    if counted > 0 {
        finish(&mut current, counted);
        buckets.push(current);
    }

    Peaks {
        buckets,
        seconds_each: WINDOW as f64 / rate as f64,
        scale: if loudest > 0.0 { 1.0 / loudest } else { 1.0 },
    }
}

fn finish(bucket: &mut Bucket, counted: usize) {
    let n = counted as f32;
    bucket.low = (bucket.low / n).sqrt();
    bucket.mid = (bucket.mid / n).sqrt();
    bucket.high = (bucket.high / n).sqrt();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn tone(freq: f32, rate: u32, seconds: f32) -> Track {
        let frames = (rate as f32 * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let value = (TAU * freq * i as f32 / rate as f32).sin() * 0.8;
            samples.push(value);
            samples.push(value);
        }
        Track { samples, sample_rate: rate }
    }

    #[test]
    fn a_bass_tone_reads_as_bass() {
        let peaks = analyse(&tone(60.0, 48_000, 1.0));
        // Skip the first buckets: the filters are still settling.
        let late = &peaks.buckets[40];
        assert!(late.low > late.mid && late.low > late.high,
                "low {} mid {} high {}", late.low, late.mid, late.high);
    }

    #[test]
    fn a_treble_tone_reads_as_treble() {
        let peaks = analyse(&tone(9_000.0, 48_000, 1.0));
        let late = &peaks.buckets[40];
        assert!(late.high > late.low && late.high > late.mid,
                "low {} mid {} high {}", late.low, late.mid, late.high);
    }

    #[test]
    fn a_vocal_range_tone_reads_as_mid() {
        // The old split subtracted two overlapping filters from the record,
        // which left a mid band that was mostly phase error.
        let peaks = analyse(&tone(800.0, 48_000, 1.0));
        let late = &peaks.buckets[40];
        assert!(late.mid > late.low * 10.0 && late.mid > late.high * 10.0,
                "low {} mid {} high {}", late.low, late.mid, late.high);
    }

    #[test]
    fn the_bands_carry_the_whole_record_between_them() {
        // Complementary Linkwitz-Riley bands: at a crossover the two
        // neighbours each carry half the amplitude, in phase, so between
        // them they are the whole of it.
        let rms = 0.8 / std::f32::consts::SQRT_2;
        let at = |freq: f32| {
            let b = analyse(&tone(freq, 48_000, 1.0)).buckets[60];
            [b.low / rms, b.mid / rms, b.high / rms]
        };
        let [low, mid, high] = at(250.0);
        assert!((low - 0.5).abs() < 0.05 && (mid - 0.5).abs() < 0.05 && high < 0.05,
                "250 Hz split {low} / {mid} / {high}");
        let [low, mid, high] = at(2_500.0);
        assert!(low < 0.05 && (mid - 0.5).abs() < 0.05 && (high - 0.5).abs() < 0.05,
                "2.5 kHz split {low} / {mid} / {high}");
    }

    #[test]
    fn the_scale_brings_a_quiet_record_up_to_full_height() {
        let peaks = analyse(&tone(1_000.0, 48_000, 0.5));
        assert!((peaks.scale - 1.25).abs() < 0.02, "got {}", peaks.scale);
    }

    #[test]
    fn a_window_returns_what_it_was_asked_for() {
        let peaks = analyse(&tone(1_000.0, 48_000, 4.0));
        let drawn = peaks.window(1.0, 3.0, 500);
        assert_eq!(drawn.len(), 500);
    }

    #[test]
    fn reducing_keeps_the_extremes_rather_than_averaging_them_away() {
        let mut peaks = Peaks {
            buckets: vec![Bucket::default(); 100],
            seconds_each: 0.01,
            scale: 1.0,
        };
        peaks.buckets[57].max = 0.95;
        // One loud bucket among a hundred quiet ones has to survive the
        // reduction, or every transient vanishes at low zoom.
        let drawn = peaks.window(0.0, 1.0, 10);
        assert!(drawn.iter().any(|b| b.max > 0.9), "the transient was averaged away");
    }

    #[test]
    fn a_window_past_the_end_is_empty_rather_than_a_panic() {
        let peaks = analyse(&tone(1_000.0, 48_000, 0.2));
        assert!(peaks.window(60.0, 70.0, 100).is_empty());
    }
}
