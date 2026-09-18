//! Pitch-preserving tempo for fully decoded decks. Signalsmith's outputSeek
//! consumes source lookahead to prime the processor, so the first returned
//! sample belongs at the cue point, without inserting its algorithmic delay.
//! All buffers and the C++ processor are allocated when the audio engine opens.
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use super::decode::Track;
use super::deck::{sample_at, STEMS};

const BLOCK: usize = 128;

extern "C" {
    fn defalt_stretch_new(rate: u32) -> *mut c_void;
    fn defalt_stretch_delete(p: *mut c_void);
    fn defalt_stretch_seek_length(p: *mut c_void, rate: f32) -> i32;
    fn defalt_stretch_seek(p: *mut c_void, left: *const f32, right: *const f32, frames: i32);
    fn defalt_stretch_process(p: *mut c_void, left: *const f32, right: *const f32, frames: i32,
                              out_left: *mut f32, out_right: *mut f32, output_frames: i32);
}

pub struct Stretch {
    processor: NonNull<c_void>,
    input: [Vec<f32>; 2],
    output: [[f32; BLOCK]; 2],
    cursor: usize,
    input_position: f64,
    fraction: f64,
    ready: bool,
    device_rate: u32,
    rate: f64,
}

// Ownership moves once into the callback; it is never used concurrently.
unsafe impl Send for Stretch {}

impl Stretch {
    pub fn new(device_rate: u32) -> Option<Self> {
        let processor = NonNull::new(unsafe { defalt_stretch_new(device_rate) })?;
        // Supported tempo is 0.5..2.0. Reserve the largest possible pre-roll.
        let capacity = unsafe { defalt_stretch_seek_length(processor.as_ptr(), 2.0) } as usize;
        Some(Self { processor, input: [vec![0.0; capacity.max(BLOCK * 2 + 1)],
                                      vec![0.0; capacity.max(BLOCK * 2 + 1)]],
            output: [[0.0; BLOCK]; 2], cursor: BLOCK, input_position: 0.0,
            fraction: 0.0, ready: false, device_rate, rate: 1.0 })
    }

    pub fn reset(&mut self) { self.ready = false; self.cursor = BLOCK; }

    fn fill(&mut self, frames: usize, track: &Track,
            stems: Option<&[Arc<Track>; STEMS]>, levels: [f32; STEMS]) {
        let step = track.sample_rate as f64 / self.device_rate as f64;
        for i in 0..frames {
            let sample = if let Some(parts) = stems {
                let mut sum = [0.0; 2];
                for (part, level) in parts.iter().zip(levels) {
                    let frame = sample_at(part, self.input_position);
                    sum[0] += frame[0] * level;
                    sum[1] += frame[1] * level;
                }
                sum
            } else { sample_at(track, self.input_position) };
            self.input[0][i] = sample[0];
            self.input[1][i] = sample[1];
            self.input_position += step;
        }
    }

    /// Output one stereo frame and the source-frame advance it represents.
    pub fn next(&mut self, track: &Track, stems: Option<&[Arc<Track>; STEMS]>,
                levels: [f32; STEMS], position: f64, speed: f64) -> ([f32; 2], f64) {
        if !self.ready {
            self.rate = speed;
            self.input_position = position;
            self.fraction = 0.0;
            let count = unsafe { defalt_stretch_seek_length(self.processor.as_ptr(), speed as f32) } as usize;
            self.fill(count, track, stems, levels);
            unsafe { defalt_stretch_seek(self.processor.as_ptr(), self.input[0].as_ptr(),
                                         self.input[1].as_ptr(), count as i32); }
            self.ready = true;
        }
        if self.cursor == BLOCK {
            // Smooth ratio changes over about 20ms; this avoids abruptly
            // changing the analysis hop when a fader/tempo correction moves.
            self.rate += (speed - self.rate) * (1.0 - (-(BLOCK as f64) / (self.device_rate as f64 * 0.02)).exp());
            let wanted = BLOCK as f64 * self.rate + self.fraction;
            let count = wanted.floor() as usize;
            self.fraction = wanted - count as f64;
            self.fill(count, track, stems, levels);
            unsafe { defalt_stretch_process(self.processor.as_ptr(), self.input[0].as_ptr(),
                self.input[1].as_ptr(), count as i32, self.output[0].as_mut_ptr(),
                self.output[1].as_mut_ptr(), BLOCK as i32); }
            self.cursor = 0;
        }
        let frame = [self.output[0][self.cursor], self.output[1][self.cursor]];
        self.cursor += 1;
        (frame, self.rate * track.sample_rate as f64 / self.device_rate as f64)
    }
}

impl Drop for Stretch {
    fn drop(&mut self) { unsafe { defalt_stretch_delete(self.processor.as_ptr()); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::deck::Deck;

    struct AllocationAudit;
    thread_local! {
        static AUDIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    unsafe impl std::alloc::GlobalAlloc for AllocationAudit {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            if AUDIT.try_with(|flag| flag.get()).unwrap_or(false) {
                let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
            }
            std::alloc::GlobalAlloc::alloc(&std::alloc::System, layout)
        }
        unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
            std::alloc::GlobalAlloc::dealloc(&std::alloc::System, p, layout)
        }
        unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
            if AUDIT.try_with(|flag| flag.get()).unwrap_or(false) {
                let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
            }
            std::alloc::GlobalAlloc::realloc(&std::alloc::System, p, layout, size)
        }
    }
    #[global_allocator]
    static ALLOCATOR: AllocationAudit = AllocationAudit;

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
        assert!((deck.position - 7_600.0).abs() < 0.01);
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
        let jump = all.iter().step_by(2).zip(all.iter().skip(2).step_by(2))
            .map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(jump < 0.08, "mode/rate change clicked: largest jump {jump}");
    }

    #[test]
    fn deck_processing_and_recue_do_not_allocate_on_rust_callback_path() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(tone(220.0, 5.0, 48_000));
        deck.key_lock = true;
        deck.playing = true;
        let mut output = [0.0; 512];
        ALLOCATIONS.with(|count| count.set(0));
        AUDIT.with(|flag| flag.set(true));
        for i in 0..100 {
            if i == 50 { deck.position = 0.0; deck.reset_stretch(); }
            deck.speed = if i < 70 { 0.92 } else { 1.08 };
            output.fill(0.0);
            deck.mix_into(&mut output, 48_000);
        }
        AUDIT.with(|flag| flag.set(false));
        assert_eq!(ALLOCATIONS.with(|count| count.get()), 0);
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
        assert!(error < 1e-5, "summing stems changed the stereo phase: {error}");
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
