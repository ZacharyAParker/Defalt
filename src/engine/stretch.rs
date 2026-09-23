//! Pitch-preserving tempo for fully decoded decks. Signalsmith's outputSeek
//! consumes source lookahead to prime the processor, so the first returned
//! sample belongs at the cue point, without inserting its algorithmic delay.
//! All buffers and the C++ processors are allocated when the audio engine
//! opens.
//!
//! Two processors per deck: a stereo one for a record, and an eight channel
//! one for a separated record, one stereo pair per stem. Stretching the stems
//! apart (together, in one processor, so they stay phase-locked) means their
//! levels are applied to what comes out rather than what goes in, and a stem
//! mute is heard now rather than one look-ahead window later.
//!
//! The stretcher reads ahead of the playhead, so it has to know about loops:
//! its input wraps at the loop's out point with the same crossfade the deck
//! uses, and a loop never feeds it audio from past the seam.
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use super::decode::Track;
use super::deck::{sample_at, STEMS};

const BLOCK: usize = 128;
const CHANNELS: usize = STEMS * 2;
/// Input frames over which a loop seam in the look-ahead is crossfaded.
pub const WRAP: usize = 128;

extern "C" {
    fn defalt_stretch_new_channels(rate: u32, channels: i32) -> *mut c_void;
    fn defalt_stretch_delete(p: *mut c_void);
    fn defalt_stretch_seek_length(p: *mut c_void, rate: f32) -> i32;
    fn defalt_stretch_seek_n(p: *mut c_void, inputs: *const *const f32, frames: i32);
    fn defalt_stretch_process_n(p: *mut c_void, inputs: *const *const f32, frames: i32,
                                outputs: *const *mut f32, output_frames: i32);
}

pub struct Stretch {
    stereo: NonNull<c_void>,
    separated_processor: NonNull<c_void>,
    input: [Vec<f32>; CHANNELS],
    output: [[f32; BLOCK]; CHANNELS],
    cursor: usize,
    input_position: f64,
    fraction: f64,
    ready: bool,
    /// Which processor is primed: the stems one or the stereo one.
    separated: bool,
    device_rate: u32,
    rate: f64,
    /// What the stretcher was saying before a jump, faded out under what it
    /// says after one.
    tail: [[f32; 2]; BLOCK],
    tail_left: usize,
    loop_range: Option<(f64, f64)>,
    wrap_from: f64,
    wrap_left: usize,
}

// Ownership moves once into the callback; it is never used concurrently.
unsafe impl Send for Stretch {}

impl Stretch {
    pub fn new(device_rate: u32) -> Option<Self> {
        let stereo = NonNull::new(unsafe { defalt_stretch_new_channels(device_rate, 2) })?;
        let Some(separated_processor) =
            NonNull::new(unsafe { defalt_stretch_new_channels(device_rate, CHANNELS as i32) })
        else {
            unsafe { defalt_stretch_delete(stereo.as_ptr()) };
            return None;
        };
        // Supported tempo is 0.5..2.0. Reserve the largest possible pre-roll.
        let capacity = unsafe { defalt_stretch_seek_length(separated_processor.as_ptr(), 2.0) }
            .max(unsafe { defalt_stretch_seek_length(stereo.as_ptr(), 2.0) }) as usize;
        let capacity = capacity.max(BLOCK * 2 + 1);
        Some(Self {
            stereo,
            separated_processor,
            input: std::array::from_fn(|_| vec![0.0; capacity]),
            output: [[0.0; BLOCK]; CHANNELS],
            cursor: BLOCK,
            input_position: 0.0,
            fraction: 0.0,
            ready: false,
            separated: false,
            device_rate,
            rate: 1.0,
            tail: [[0.0; 2]; BLOCK],
            tail_left: 0,
            loop_range: None,
            wrap_from: 0.0,
            wrap_left: 0,
        })
    }

    pub fn reset(&mut self) {
        self.ready = false;
        self.cursor = BLOCK;
        self.tail_left = 0;
        self.wrap_left = 0;
    }

    /// Where the stretcher's input has read up to, in source frames. Ahead
    /// of the deck's playhead by the look-ahead.
    pub fn input_position(&self) -> f64 {
        self.input_position
    }

    pub fn primed(&self) -> bool {
        self.ready
    }

    /// The loop the input should wrap at, in source frames. The deck calls
    /// this whenever its loop (or roll) changes.
    pub fn set_loop(&mut self, range: Option<(f64, f64)>) {
        self.loop_range = range.filter(|(a, b)| b > a);
    }

    fn processor(&self) -> *mut c_void {
        if self.separated { self.separated_processor.as_ptr() } else { self.stereo.as_ptr() }
    }

    #[inline]
    fn read(&self, track: &Track, stems: Option<&[Arc<Track>; STEMS]>, position: f64) -> [f32; CHANNELS] {
        let mut out = [0.0; CHANNELS];
        if self.separated {
            if let Some(parts) = stems {
                for (stem, part) in parts.iter().enumerate() {
                    let [l, r] = sample_at(part, position);
                    out[stem * 2] = l;
                    out[stem * 2 + 1] = r;
                }
            }
        } else {
            let [l, r] = sample_at(track, position);
            out[0] = l;
            out[1] = r;
        }
        out
    }

    fn fill(&mut self, frames: usize, track: &Track, stems: Option<&[Arc<Track>; STEMS]>) {
        let step = track.sample_rate as f64 / self.device_rate as f64;
        let channels = if self.separated { CHANNELS } else { 2 };
        for i in 0..frames {
            let mut sample = self.read(track, stems, self.input_position);
            if self.wrap_left > 0 {
                let old = self.read(track, stems, self.wrap_from);
                let w = 1.0 - self.wrap_left as f32 / (WRAP + 1) as f32;
                for c in 0..channels { sample[c] = old[c] + (sample[c] - old[c]) * w; }
                self.wrap_from += step;
                self.wrap_left -= 1;
            }
            for c in 0..channels { self.input[c][i] = sample[c]; }
            let before = self.input_position;
            self.input_position += step;
            if let Some((start, end)) = self.loop_range {
                if before < end && self.input_position >= end {
                    // The same rule as the deck's playhead: carry on past the
                    // seam under a fade, come in again from the loop's start.
                    self.wrap_from = self.input_position;
                    self.input_position = start + (self.input_position - end) % (end - start);
                    self.wrap_left = WRAP;
                }
            }
        }
    }

    fn pointers(&mut self) -> ([*const f32; CHANNELS], [*mut f32; CHANNELS]) {
        (std::array::from_fn(|c| self.input[c].as_ptr()),
         std::array::from_fn(|c| self.output[c].as_mut_ptr()))
    }

    fn prime(&mut self, track: &Track, stems: Option<&[Arc<Track>; STEMS]>, position: f64, speed: f64) {
        self.separated = stems.is_some();
        self.rate = speed;
        self.input_position = position;
        self.fraction = 0.0;
        self.wrap_left = 0;
        let count = unsafe { defalt_stretch_seek_length(self.processor(), speed as f32) } as usize;
        let count = count.min(self.input[0].len());
        self.fill(count, track, stems);
        let (inputs, _) = self.pointers();
        unsafe { defalt_stretch_seek_n(self.processor(), inputs.as_ptr(), count as i32); }
        self.ready = true;
        self.cursor = BLOCK;
    }

    fn block(&mut self, track: &Track, stems: Option<&[Arc<Track>; STEMS]>, speed: f64) {
        // Smooth ratio changes over about 20ms; this avoids abruptly
        // changing the analysis hop when a fader/tempo correction moves.
        self.rate += (speed - self.rate) * (1.0 - (-(BLOCK as f64) / (self.device_rate as f64 * 0.02)).exp());
        let wanted = BLOCK as f64 * self.rate + self.fraction;
        let count = (wanted.floor() as usize).min(self.input[0].len());
        self.fraction = wanted - count as f64;
        self.fill(count, track, stems);
        let (inputs, outputs) = self.pointers();
        unsafe { defalt_stretch_process_n(self.processor(), inputs.as_ptr(), count as i32,
                                          outputs.as_ptr(), BLOCK as i32); }
        self.cursor = 0;
    }

    #[inline]
    fn mixed(&self, levels: [f32; STEMS]) -> [f32; 2] {
        let at = self.cursor;
        if self.separated {
            let mut sum = [0.0f32; 2];
            for (stem, level) in levels.iter().enumerate() {
                sum[0] += self.output[stem * 2][at] * level;
                sum[1] += self.output[stem * 2 + 1][at] * level;
            }
            sum
        } else {
            [self.output[0][at], self.output[1][at]]
        }
    }

    /// About to jump: keep one block of what the stretcher would have said
    /// next, then let it prime again wherever the deck lands. The kept block
    /// fades out under the new one, so a seek, a hot cue or a roll's release
    /// under key lock is a 128-frame crossfade rather than a hole -- and the
    /// deck's key-lock mix never drops back to the vinyl path to cover it.
    pub fn crossfade(&mut self, track: &Track, stems: Option<&[Arc<Track>; STEMS]>,
                     levels: [f32; STEMS], speed: f64) {
        if !self.ready {
            self.tail_left = 0;
            return;
        }
        let speed = speed.clamp(0.5, 2.0);
        for i in 0..BLOCK {
            if self.cursor == BLOCK { self.block(track, stems, speed); }
            self.tail[i] = self.mixed(levels);
            self.cursor += 1;
        }
        self.tail_left = BLOCK;
        self.ready = false;
        self.cursor = BLOCK;
        self.wrap_left = 0;
    }

    /// Output one stereo frame and the source-frame advance it represents.
    ///
    /// With stems, `levels` are applied here, to the stretched stems, so a
    /// level change is heard on this frame.
    pub fn next(&mut self, track: &Track, stems: Option<&[Arc<Track>; STEMS]>,
                levels: [f32; STEMS], position: f64, speed: f64) -> ([f32; 2], f64) {
        if self.ready && stems.is_some() != self.separated {
            // The parts arrived (or went) mid-play: switch processors under
            // a crossfade rather than a jolt.
            self.crossfade(track, stems, levels, speed);
        }
        if !self.ready {
            self.prime(track, stems, position, speed);
        }
        if self.cursor == BLOCK {
            self.block(track, stems, speed);
        }
        let mut frame = self.mixed(levels);
        self.cursor += 1;
        if self.tail_left > 0 {
            let done = BLOCK - self.tail_left;
            let w = (done + 1) as f32 / (BLOCK + 1) as f32;
            let old = self.tail[done];
            frame = [old[0] + (frame[0] - old[0]) * w, old[1] + (frame[1] - old[1]) * w];
            self.tail_left -= 1;
        }
        (frame, self.rate * track.sample_rate as f64 / self.device_rate as f64)
    }
}

impl Drop for Stretch {
    fn drop(&mut self) {
        unsafe {
            defalt_stretch_delete(self.stereo.as_ptr());
            defalt_stretch_delete(self.separated_processor.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::audit;
    use super::super::deck::Deck;

    fn tone(hz: f64, seconds: f64, rate: u32) -> Arc<Track> {
        let samples = (0..(seconds * rate as f64) as usize).flat_map(|i| {
            let sample = (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() as f32 * 0.2;
            [sample, sample]
        }).collect();
        Arc::new(Track { samples, sample_rate: rate })
    }

    fn render(deck: &mut Deck, seconds: f64, rate: u32) -> Vec<f32> {
        let mut output = vec![0.0; (seconds * rate as f64) as usize * 2];
        for chunk in output.chunks_mut(512) { deck.mix_into(chunk, rate); }
        output
    }

    fn frequency(samples: &[f32], rate: u32) -> f64 {
        let signal: Vec<f32> = samples.iter().step_by(2).copied().collect();
        let crossings: Vec<usize> = signal.windows(2).enumerate()
            .filter_map(|(i, p)| (p[0] <= 0.0 && p[1] > 0.0).then_some(i)).collect();
        (crossings.len() - 1) as f64 * rate as f64 /
            (crossings.last().unwrap() - crossings.first().unwrap()) as f64
    }

    fn largest_step(samples: &[f32]) -> f32 {
        samples.iter().step_by(2).zip(samples.iter().skip(2).step_by(2))
            .map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max)
    }

    #[test]
    fn key_lock_preserves_pitch_at_both_tempo_limits_and_device_rates() {
        for rate in [44_100, 48_000, 96_000] {
            for speed in [0.92, 1.08] {
                let mut deck = Deck::new(rate);
                deck.track = Some(tone(440.0, 4.0, 44_100));
                deck.speed = speed;
                deck.key_lock = true;
                deck.playing = true;
                let output = render(&mut deck, 2.0, rate);
                let hz = frequency(&output[rate as usize..], rate);
                assert!((hz - 440.0).abs() < 1.0, "{rate}Hz at {speed}: pitch {hz}");
                assert!((deck.seconds() - 2.0 * speed).abs() < 0.001);
                assert!(output.iter().all(|v| v.is_finite() && v.abs() < 0.5));
            }
        }
    }

    #[test]
    fn key_lock_impulse_keeps_its_timeline_position() {
        for speed in [0.92, 1.08] {
            let mut samples = vec![0.0; 48_000 * 2 * 2];
            samples[24_000 * 2] = 1.0;
            samples[24_000 * 2 + 1] = 1.0;
            let mut deck = Deck::new(48_000);
            deck.track = Some(Arc::new(Track { samples, sample_rate: 48_000 }));
            deck.speed = speed;
            deck.key_lock = true;
            deck.playing = true;
            let output = render(&mut deck, 1.5, 48_000);
            let peak = output.iter().step_by(2).enumerate()
                .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs())).unwrap().0;
            let error = (peak as f64 / 48_000.0 - 0.5 / speed).abs();
            assert!(error < 0.02, "tempo {speed}, impulse error {error}s");
        }
    }

    #[test]
    fn reset_removes_previously_buffered_audio_and_scrub_bypasses_key_lock() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(tone(440.0, 4.0, 48_000));
        deck.key_lock = true;
        deck.speed = 1.05;
        deck.playing = true;
        render(&mut deck, 0.4, 48_000);
        deck.track = Some(Arc::new(Track { samples: vec![0.0; 96_000], sample_rate: 48_000 }));
        deck.position = 0.0;
        deck.reset_stretch();
        let silence = render(&mut deck, 0.2, 48_000);
        assert!(silence.iter().all(|s| s.abs() < 1e-5));
        deck.position = 10_000.0;
        deck.scrub = Some(-1.0);
        render(&mut deck, 0.05, 48_000);
        // 2400 frames backwards at a full -1, less the few milliseconds the
        // hand takes to swing the platter from +1.05 round to -1.
        assert!(deck.position < 8_600.0 && deck.position > 7_600.0, "at {}", deck.position);
    }

    #[test]
    fn tempo_changes_and_key_lock_toggles_remain_bounded_and_stereo_coherent() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(tone(220.0, 5.0, 48_000));
        deck.playing = true;
        let mut all = Vec::new();
        for (locked, speed) in [(false, 1.0), (true, 0.92), (true, 1.08), (false, 1.0), (true, 1.0)] {
            deck.key_lock = locked;
            deck.speed = speed;
            all.extend(render(&mut deck, 0.3, 48_000));
        }
        assert!(all.iter().all(|s| s.is_finite() && s.abs() < 0.5));
        let difference = all.chunks_exact(2).map(|s| (s[0] - s[1]).powi(2)).sum::<f32>();
        let energy = all.iter().map(|s| s * s).sum::<f32>();
        assert!(difference / energy < 0.0001, "stereo difference energy {}", difference / energy);
        let jump = largest_step(&all);
        assert!(jump < 0.08, "mode/rate change clicked: largest jump {jump}");
    }

    #[test]
    fn deck_processing_and_recue_do_not_allocate_on_rust_callback_path() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(tone(220.0, 5.0, 48_000));
        deck.stems = Some(std::array::from_fn(|_| tone(220.0, 5.0, 48_000)));
        deck.key_lock = true;
        deck.playing = true;
        let mut output = [0.0; 512];
        let audit = audit::start();
        for i in 0..100 {
            if i == 50 { deck.position = 0.0; deck.reset_stretch(); }
            if i == 60 { deck.seek(48_000.0); }
            if i == 65 { deck.set_loop(Some((50_000.0, 60_000.0))); }
            deck.speed = if i < 70 { 0.92 } else { 1.08 };
            output.fill(0.0);
            deck.mix_into(&mut output, 48_000);
        }
        assert_eq!(audit.stop(), 0);
    }

    #[test]
    fn separated_stems_share_one_phase_coherent_stretcher() {
        let original = tone(440.0, 3.0, 48_000);
        let quarter = Arc::new(Track { samples: original.samples.iter().map(|s| s * 0.25).collect(), sample_rate: 48_000 });
        let mut plain = Deck::new(48_000);
        plain.track = Some(original.clone());
        plain.key_lock = true;
        plain.playing = true;
        plain.speed = 0.92;
        let mut separated = Deck::new(48_000);
        separated.track = Some(original);
        separated.stems = Some(std::array::from_fn(|_| quarter.clone()));
        separated.key_lock = true;
        separated.playing = true;
        separated.speed = 0.92;
        let direct = render(&mut plain, 1.0, 48_000);
        let stems = render(&mut separated, 1.0, 48_000);
        let error = direct.iter().zip(stems).map(|(a,b)| (a-b).abs()).fold(0.0f32, f32::max);
        // Not bit-exact: eight channels and two round differently inside
        // the stretcher. -60 dB of difference is the same sound.
        assert!(error < 2e-4, "summing stems changed the stereo phase: {error}");
    }

    #[test]
    fn a_stem_mute_under_key_lock_is_heard_now_not_a_window_later() {
        let silent = Arc::new(Track { samples: vec![0.0; 48_000 * 6], sample_rate: 48_000 });
        let loud = tone(300.0, 3.0, 48_000);
        let mut deck = Deck::new(48_000);
        deck.track = Some(loud.clone());
        deck.stems = Some([loud, silent.clone(), silent.clone(), silent]);
        deck.key_lock = true;
        deck.speed = 0.95;
        deck.playing = true;
        render(&mut deck, 0.5, 48_000);
        deck.stem_muted[0] = true;
        let after = render(&mut deck, 0.05, 48_000);
        // The level glides over about 5 ms; 30 ms in, the drums are gone.
        let late = &after[48 * 30 * 2..];
        let peak = late.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.002, "the muted stem was still sounding at {peak}");
    }

    #[test]
    fn a_seek_under_key_lock_crossfades_instead_of_dropping_to_vinyl() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(tone(220.0, 6.0, 48_000));
        deck.key_lock = true;
        deck.speed = 0.9;
        deck.playing = true;
        let mut all = render(&mut deck, 0.5, 48_000);
        deck.seek(3.3 * 48_000.0);
        let after = render(&mut deck, 0.3, 48_000);
        // Pitch straight after the jump: a vinyl blip would read 198 Hz.
        let hz = frequency(&after[..48_000 / 10 * 2], 48_000);
        assert!((hz - 220.0).abs() < 3.0, "the seek played {hz} Hz");
        all.extend(after);
        assert!(largest_step(&all) < 0.03, "the seek clicked: {}", largest_step(&all));
    }

    #[test]
    fn disabled_key_lock_keeps_vinyl_pitch_and_locked_playback_stops_at_source_end() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(tone(440.0, 2.0, 48_000));
        deck.speed = 0.92;
        deck.playing = true;
        let pitched = render(&mut deck, 1.0, 48_000);
        assert!((frequency(&pitched, 48_000) - 440.0 * 0.92).abs() < 1.0);
        deck.position = 0.0;
        deck.reset_stretch();
        deck.key_lock = true;
        render(&mut deck, 3.0, 48_000);
        assert!(!deck.playing);
        assert!((deck.seconds() - 2.0).abs() < 0.0001);
    }
}
