//! A non-blocking audio tap. The UI performs the spectrum analysis.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

pub const SAMPLES: usize = 1024;
pub const BANDS: usize = 48;

pub struct Capture {
    enabled: AtomicBool,
    cursor: AtomicUsize,
    samples: [[AtomicU32; SAMPLES]; 2],
}
impl Default for Capture {
    fn default() -> Self {
        Self { enabled: AtomicBool::new(false), cursor: AtomicUsize::new(0),
            samples: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))) }
    }
}
impl Capture {
    pub fn set_enabled(&self, enabled: bool) { self.enabled.store(enabled, Ordering::Relaxed); }
    pub fn enabled(&self) -> bool { self.enabled.load(Ordering::Relaxed) }
    pub fn push(&self, left: f32, right: f32) {
        let cursor = self.cursor.load(Ordering::Relaxed);
        self.samples[0][cursor % SAMPLES].store(left.to_bits(), Ordering::Relaxed);
        self.samples[1][cursor % SAMPLES].store(right.to_bits(), Ordering::Relaxed);
        self.cursor.store(cursor.wrapping_add(1), Ordering::Release);
    }
    pub fn snapshot(&self) -> [[f32; SAMPLES]; 2] {
        let cursor = self.cursor.load(Ordering::Acquire);
        std::array::from_fn(|channel| std::array::from_fn(|i| {
            f32::from_bits(self.samples[channel][cursor.wrapping_add(i) % SAMPLES].load(Ordering::Relaxed))
        }))
    }
}

pub fn analyze(samples: &[[f32; SAMPLES]; 2], rate: u32) -> [f32; BANDS] {
    if rate == 0 { return [0.; BANDS]; }
    let mut power = [0.0_f32; SAMPLES / 2];
    for channel in samples {
        let mut real = [0.0_f32; SAMPLES];
        let mut imag = [0.0_f32; SAMPLES];
        for i in 0..SAMPLES {
            let sample = if channel[i].is_finite() { channel[i] } else { 0. };
            real[i] = sample * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / SAMPLES as f32).cos());
        }
        let mut j = 0;
        for i in 1..SAMPLES {
            let mut bit = SAMPLES >> 1;
            while j & bit != 0 { j ^= bit; bit >>= 1; }
            j ^= bit;
            if i < j { real.swap(i, j); }
        }
        let mut size = 2;
        while size <= SAMPLES {
            let angle = -std::f32::consts::TAU / size as f32;
            let (step_i, step_r) = angle.sin_cos();
            for start in (0..SAMPLES).step_by(size) {
                let (mut wr, mut wi) = (1., 0.);
                for offset in 0..size / 2 {
                    let a = start + offset;
                    let b = a + size / 2;
                    let tr = wr * real[b] - wi * imag[b];
                    let ti = wr * imag[b] + wi * real[b];
                    real[b] = real[a] - tr;
                    imag[b] = imag[a] - ti;
                    real[a] += tr;
                    imag[a] += ti;
                    (wr, wi) = (wr * step_r - wi * step_i, wr * step_i + wi * step_r);
                }
            }
            size *= 2;
        }
        for i in 1..SAMPLES / 2 { power[i] += (real[i] * real[i] + imag[i] * imag[i]) * 0.5; }
    }
    let high = (rate as f32 * 0.45).min(16000.).max(41.);
    std::array::from_fn(|band| {
        let low_hz = 40. * (high / 40.).powf(band as f32 / BANDS as f32);
        let high_hz = 40. * (high / 40.).powf((band + 1) as f32 / BANDS as f32);
        let lo = ((low_hz * SAMPLES as f32 / rate as f32) as usize).clamp(1, SAMPLES / 2 - 1);
        let hi = ((high_hz * SAMPLES as f32 / rate as f32).ceil() as usize).clamp(lo + 1, SAMPLES / 2);
        let amplitude = power[lo..hi].iter().copied().fold(0., f32::max).sqrt() * 4. / SAMPLES as f32;
        ((20. * amplitude.max(0.000001).log10() + 66.) / 66.).clamp(0., 1.)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_is_silent_and_nonfinite_samples_are_safe() {
        assert_eq!(analyze(&[[0.; SAMPLES]; 2], 48000), [0.; BANDS]);
        assert_eq!(analyze(&[[f32::NAN; SAMPLES]; 2], 48000), [0.; BANDS]);
    }
    #[test]
    fn tone_lands_in_its_band_and_antiphase_stereo_survives() {
        let left = std::array::from_fn(|i| (std::f32::consts::TAU * 1000. * i as f32 / 48000.).sin() * 0.5);
        let right = left.map(|x| -x);
        let bands = analyze(&[left, right], 48000);
        let top = bands.iter().enumerate().max_by(|a,b| a.1.total_cmp(b.1)).unwrap().0;
        let frequency = 40. * (16000.0_f32 / 40.).powf((top as f32 + 0.5) / BANDS as f32);
        assert!((800. ..1200.).contains(&frequency));
        assert!(bands[top] > 0.8);
        let same_phase = analyze(&[left, left], 48000);
        assert_eq!(bands, same_phase);
    }
    #[test]
    fn ring_returns_newest_window_in_order() {
        let capture = Capture::default();
        assert!(!capture.enabled());
        capture.set_enabled(true);
        for i in 0..SAMPLES + 7 { capture.push(i as f32, -(i as f32)); }
        let samples = capture.snapshot();
        assert_eq!(samples[0][0], 7.);
        assert_eq!(samples[1][SAMPLES - 1], -((SAMPLES + 6) as f32));
    }
}
