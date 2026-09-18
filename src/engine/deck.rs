//! One deck: a record, a position, and a rate.
//!
//! Everything here runs on the audio thread, so nothing here allocates, locks,
//! or touches the filesystem. The only thing a deck ever does with an
//! `Arc<Track>` is read from it -- when a record is replaced the old one is
//! handed back to another thread to be dropped, because releasing the last
//! reference would free several hundred megabytes inside the callback.

use std::sync::Arc;

use super::decode::Track;
use super::filters::Strip;

/// The four parts a record separates into. Order is fixed and matches what
/// the separator writes: drums, bass, everything harmonic, vocals.
pub const STEMS: usize = 4;
pub const STEM_NAMES: [&str; STEMS] = ["DRUMS", "BASS", "HARMONIC", "VOCALS"];

pub struct Deck {
    pub track: Option<Arc<Track>>,
    /// The same record, taken apart. When present it is played instead of
    /// `track` -- summed at unity the two are the same recording, so the
    /// swap is inaudible until you move a stem fader.
    pub stems: Option<[Arc<Track>; STEMS]>,
    /// Per-stem level, and whether it is silenced outright.
    pub stem_gain: [f32; STEMS],
    pub stem_muted: [bool; STEMS],
    /// Playhead, in source frames. Fractional: the whole point is that it
    /// moves at a rate we choose rather than one frame per output frame.
    pub position: f64,
    pub playing: bool,
    pub gain: f32,
    gain_state: Option<f32>,
    pub echo: super::echo::Echo,
    /// Multiplier on the record's own speed. 1.0 is as it was cut.
    pub speed: f64,
    pub key_lock: bool,
    stretch: Option<super::stretch::Stretch>,
    stretch_mix: f32,
    stretch_active: bool,
    /// Set while a hand is on the platter. Replaces `speed` outright, and can
    /// be negative, which is the whole difference between a deck and a player.
    pub scrub: Option<f64>,
    /// Three bands and a sweep, per deck, before the sum.
    pub strip: Strip,
    /// Loudest sample this deck put on the bus since it was last read. The
    /// meter belongs to the channel, not the master -- you need to see a
    /// record's level before you bring it in, which is exactly when it is not
    /// on the master yet.
    pub peak: f32,
}

impl Deck {
    pub fn new(rate: u32) -> Self {
        Deck {
            track: None,
            stems: None,
            stem_gain: [1.0; STEMS],
            stem_muted: [false; STEMS],
            position: 0.0,
            playing: false,
            gain: 1.0,
            gain_state: None,
            echo: super::echo::Echo::new(rate),
            speed: 1.0,
            key_lock: false,
            stretch: super::stretch::Stretch::new(rate),
            stretch_mix: 0.0,
            stretch_active: false,
            scrub: None,
            strip: Strip::new(rate),
            peak: 0.0,
        }
    }

    pub fn seconds(&self) -> f64 {
        match &self.track {
            Some(track) if track.sample_rate > 0 => {
                self.position / track.sample_rate as f64
            }
            _ => 0.0,
        }
    }

    pub fn reset_stretch(&mut self) {
        if let Some(stretch) = self.stretch.as_mut() { stretch.reset(); }
        self.stretch_mix = 0.0;
        self.stretch_active = false;
    }

    /// Frames of source consumed per frame of output.
    ///
    /// Sample-rate conversion and speed are the same operation, so they are
    /// the same number. A 44.1k record on a 48k device reads at 0.919 even
    /// when it is playing "normally".
    #[inline]
    fn rate(&self, device_rate: u32) -> f64 {
        let Some(track) = &self.track else { return 0.0 };
        if device_rate == 0 {
            return 0.0;
        }
        let conversion = track.sample_rate as f64 / device_rate as f64;
        self.scrub.unwrap_or(self.speed) * conversion
    }

    /// Add this deck's contribution to an interleaved stereo output buffer.
    pub fn mix_into(&mut self, out: &mut [f32], device_rate: u32) {
        if !self.playing {
            self.gain_state = None;
            self.reset_stretch();
            return;
        }
        let Some(track) = self.track.clone() else { return };

        let rate = self.rate(device_rate);
        if rate == 0.0 {
            return;
        }
        let last = track.frames() as f64;
        let mut gain = *self.gain_state.get_or_insert(self.gain);
        let smoothing = 1.0 - (-1.0 / (device_rate.max(1) as f32 * 0.005)).exp();

        // Read the four parts where we have them. Cloning the Arcs is a
        // refcount bump, not an allocation, and the deck keeps its own
        // reference so none of these can reach zero here.
        let stems = self.stems.clone();
        let stem_levels: [f32; STEMS] = std::array::from_fn(|i| {
            if self.stem_muted[i] { 0.0 } else { self.stem_gain[i] }
        });

        let mut peak = self.peak;
        for frame in out.chunks_exact_mut(2) {
            // Running off either end stops the deck rather than wrapping. A
            // record that silently restarted would be worse than silence.
            if self.position < 0.0 || self.position >= last {
                self.playing = false;
                self.position = self.position.clamp(0.0, last);
                break;
            }
            let dry = match &stems {
                Some(parts) => {
                    let mut sum = [0.0f32; 2];
                    for (part, level) in parts.iter().zip(stem_levels) {
                        if level <= 0.0 {
                            continue;
                        }
                        let [l, r] = sample_at(part, self.position);
                        sum[0] += l * level;
                        sum[1] += r * level;
                    }
                    sum
                }
                None => sample_at(&track, self.position),
            };
            let use_stretch = self.key_lock && self.scrub.is_none()
                && (0.5..=2.0).contains(&self.speed) && self.stretch.is_some();
            if use_stretch && !self.stretch_active {
                self.reset_stretch();
            }
            self.stretch_active = use_stretch;
            let mut advance = rate;
            let mut signal = dry;
            if use_stretch {
                let (wet, step) = self.stretch.as_mut().unwrap()
                    .next(&track, stems.as_ref(), stem_levels, self.position, self.speed);
                self.stretch_mix = (self.stretch_mix + 1.0 / (device_rate as f32 * 0.02)).min(1.0);
                signal = std::array::from_fn(|i| dry[i] + (wet[i] - dry[i]) * self.stretch_mix);
                advance = step;
            } else if self.stretch_mix > 0.0 {
                // Leaving key lock (including a scratch) fades back to the
                // original direct path; old spectral history is then discarded.
                let (wet, _) = self.stretch.as_mut().unwrap()
                    .next(&track, stems.as_ref(), stem_levels, self.position, self.speed.clamp(0.5, 2.0));
                self.stretch_mix = (self.stretch_mix - 1.0 / (device_rate as f32 * 0.02)).max(0.0);
                signal = std::array::from_fn(|i| dry[i] + (wet[i] - dry[i]) * self.stretch_mix);
                if self.stretch_mix == 0.0 { self.reset_stretch(); }
            }
            let [mut left, mut right] = signal;
            // Skipped entirely when every knob is at its detent, so a flat
            // channel costs nothing rather than four biquads per sample.
            if !self.strip.bypassed {
                left = self.strip.run(0, left);
                right = self.strip.run(1, right);
            }
            // Meter before the channel/crossfader gain. A silent channel
            // still advances and can be lined up before bringing it in.
            peak = peak.max(left.abs()).max(right.abs());
            [left, right] = self.echo.run([left, right]);
            gain += (self.gain - gain) * smoothing;
            left *= gain;
            right *= gain;
            frame[0] += left;
            frame[1] += right;
            self.position += advance;
        }
        self.peak = peak;
        self.gain_state = Some(gain);
    }
}

/// Catmull-Rom between the four frames around a fractional position.
///
/// Linear interpolation is cheap and audibly wrong once the rate is anything
/// but 1.0 -- it low-passes on the way down and aliases on the way up. Cubic
/// is a large improvement for four multiplies. A windowed sinc is the real
/// answer for hard scrubbing and is the intended upgrade here.
#[inline]
pub(super) fn sample_at(track: &Track, position: f64) -> [f32; 2] {
    let index = position.floor();
    let t = (position - index) as f32;
    let index = index as isize;

    let a = track.frame(index - 1);
    let b = track.frame(index);
    let c = track.frame(index + 1);
    let d = track.frame(index + 2);

    [
        catmull_rom(a[0], b[0], c[0], d[0], t),
        catmull_rom(a[1], b[1], c[1], d[1], t),
    ]
}

#[inline]
fn catmull_rom(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * b)
        + (-a + c) * t
        + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
        + (-a + 3.0 * b - 3.0 * c + d) * t3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize, sample_rate: u32) -> Arc<Track> {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            samples.push(i as f32);
            samples.push(i as f32);
        }
        Arc::new(Track { samples, sample_rate })
    }

    #[test]
    fn interpolation_is_exact_on_whole_frames() {
        let track = ramp(8, 48_000);
        assert_eq!(sample_at(&track, 3.0), [3.0, 3.0]);
    }

    #[test]
    fn muted_deck_keeps_time_and_its_pre_fader_meter() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(ramp(100, 48_000));
        deck.playing = true;
        deck.gain = 0.0;
        let mut output = [0.0; 20];
        deck.mix_into(&mut output, 48_000);
        assert_eq!(deck.position, 10.0);
        assert!(deck.playing);
        assert!(deck.peak > 0.0);
        assert_eq!(output, [0.0; 20]);
        deck.gain = 1.0;
        deck.mix_into(&mut output, 48_000);
        assert_eq!(deck.position, 20.0, "unmuting must keep the current beat");
        assert!(output[0] > 0.0 && output[0] < 0.1, "unmuting must ramp without a click");
    }

    #[test]
    fn channel_gain_changes_ramp_instead_of_jumping() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(Arc::new(Track { samples: vec![0.2; 10_000], sample_rate: 48_000 }));
        deck.playing = true;
        deck.mix_into(&mut [0.0; 100], 48_000);
        deck.gain = 0.1;
        let mut output = [0.0; 4800];
        deck.mix_into(&mut output, 48_000);
        assert!((output[0] - 0.2).abs() < 0.001);
        assert!((output[4798] - 0.02).abs() < 0.0001);
        assert!(output.chunks_exact(2).map(|f| f[0]).collect::<Vec<_>>().windows(2)
            .all(|p| (p[1] - p[0]).abs() < 0.001));
    }

    #[test]
    fn muted_deck_still_stops_at_the_end() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(ramp(4, 48_000));
        deck.playing = true;
        deck.gain = 0.0;
        deck.mix_into(&mut [0.0; 20], 48_000);
        assert!(!deck.playing);
        assert_eq!(deck.position, 4.0);
    }

    #[test]
    fn interpolation_is_linear_along_a_ramp() {
        // Catmull-Rom reproduces a straight line exactly, which makes a ramp
        // the one case where the right answer is obvious.
        let track = ramp(8, 48_000);
        let [left, _] = sample_at(&track, 3.25);
        assert!((left - 3.25).abs() < 1e-4, "got {left}");
    }

    #[test]
    fn a_44k_record_on_a_48k_device_reads_slower_than_real_time() {
        let deck = Deck { track: Some(ramp(4, 44_100)), ..Deck::new(48_000) };
        let rate = deck.rate(48_000);
        assert!((rate - 44_100.0 / 48_000.0).abs() < 1e-9, "got {rate}");
    }

    #[test]
    fn speed_and_sample_rate_conversion_multiply() {
        let mut deck = Deck { track: Some(ramp(4, 48_000)), ..Deck::new(48_000) };
        deck.speed = 2.0;
        assert!((deck.rate(48_000) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn scrubbing_overrides_speed_and_may_run_backwards() {
        let mut deck = Deck { track: Some(ramp(4, 48_000)), ..Deck::new(48_000) };
        deck.speed = 1.0;
        deck.scrub = Some(-2.0);
        assert!((deck.rate(48_000) + 2.0).abs() < 1e-9);
    }

    #[test]
    fn running_off_the_end_stops_rather_than_wrapping() {
        let mut deck = Deck { track: Some(ramp(4, 48_000)), ..Deck::new(48_000) };
        deck.playing = true;
        deck.position = 3.0;
        let mut out = vec![0.0; 16];
        deck.mix_into(&mut out, 48_000);
        assert!(!deck.playing);
        assert_eq!(deck.position, 4.0);
    }

    #[test]
    fn running_off_the_front_while_scrubbing_backwards_also_stops() {
        let mut deck = Deck { track: Some(ramp(8, 48_000)), ..Deck::new(48_000) };
        deck.playing = true;
        deck.position = 1.0;
        deck.scrub = Some(-1.0);
        let mut out = vec![0.0; 16];
        deck.mix_into(&mut out, 48_000);
        assert!(!deck.playing);
        assert_eq!(deck.position, 0.0);
    }

    #[test]
    fn a_silent_deck_adds_nothing_to_the_bus() {
        let mut deck = Deck { track: Some(ramp(8, 48_000)), ..Deck::new(48_000) };
        deck.playing = true;
        deck.gain = 0.0;
        let mut out = vec![0.0; 8];
        deck.mix_into(&mut out, 48_000);
        assert!(out.iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn decks_sum_onto_the_same_bus_rather_than_replacing_it() {
        let mut one = Deck { track: Some(ramp(8, 48_000)), ..Deck::new(48_000) };
        let mut two = Deck { track: Some(ramp(8, 48_000)), ..Deck::new(48_000) };
        one.playing = true;
        two.playing = true;
        one.position = 2.0;
        two.position = 2.0;
        let mut out = vec![0.0; 2];
        one.mix_into(&mut out, 48_000);
        two.mix_into(&mut out, 48_000);
        assert_eq!(out[0], 4.0);
    }

    #[test]
    fn stems_at_unity_sum_to_the_same_record() {
        // Four parts that add up to the mix: playing them is the mix.
        let quarter = |value: f32| {
            let mut samples = Vec::new();
            for _ in 0..8 {
                samples.push(value);
                samples.push(value);
            }
            Arc::new(Track { samples, sample_rate: 48_000 })
        };
        let mixed = quarter(0.4);
        let parts = [quarter(0.1), quarter(0.1), quarter(0.1), quarter(0.1)];

        let mut plain = Deck { track: Some(mixed.clone()), ..Deck::new(48_000) };
        plain.playing = true;
        let mut split = Deck {
            track: Some(mixed),
            stems: Some(parts),
            ..Deck::new(48_000)
        };
        split.playing = true;

        let mut a = vec![0.0; 8];
        let mut b = vec![0.0; 8];
        plain.mix_into(&mut a, 48_000);
        split.mix_into(&mut b, 48_000);
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-5, "{x} vs {y}");
        }
    }

    #[test]
    fn muting_a_stem_removes_exactly_that_stem() {
        let part = |value: f32| {
            let samples = vec![value; 16];
            Arc::new(Track { samples, sample_rate: 48_000 })
        };
        let mut deck = Deck {
            track: Some(part(0.4)),
            stems: Some([part(0.1), part(0.2), part(0.4), part(0.8)]),
            ..Deck::new(48_000)
        };
        deck.playing = true;
        deck.stem_muted[3] = true;

        let mut out = vec![0.0; 2];
        deck.mix_into(&mut out, 48_000);
        // 0.1 + 0.2 + 0.4, with the 0.8 gone.
        assert!((out[0] - 0.7).abs() < 1e-5, "got {}", out[0]);
    }

    #[test]
    fn a_deck_with_no_stems_still_plays_the_record() {
        let track = Arc::new(Track { samples: vec![0.5; 16], sample_rate: 48_000 });
        let mut deck = Deck { track: Some(track), ..Deck::new(48_000) };
        deck.playing = true;
        let mut out = vec![0.0; 2];
        deck.mix_into(&mut out, 48_000);
        assert!((out[0] - 0.5).abs() < 1e-5, "got {}", out[0]);
    }
}
