//! Bounded, preallocated stereo echo for outgoing transition tails.
pub struct Echo {
    buffer: Vec<[f32; 2]>,
    cursor: usize,
    delay: usize,
    pub mix: f32,
    pub feedback: f32,
    wet: f32,
    smoothing: f32,
}

impl Echo {
    pub fn new(rate: u32) -> Self {
        Self { buffer: vec![[0.0; 2]; rate.max(1) as usize * 2], cursor: 0,
            delay: (rate / 4).max(1) as usize, mix: 0.0, feedback: 0.3,
            wet: 0.0, smoothing: 1.0 - (-1.0 / (rate.max(1) as f32 * 0.01)).exp() }
    }
    pub fn set(&mut self, mix: f32, feedback: f32, seconds: f32, rate: u32) {
        self.mix = mix.clamp(0.0, 0.5);
        self.feedback = feedback.clamp(0.0, 0.65);
        self.delay = ((seconds * rate as f32) as usize).clamp(1, self.buffer.len() - 1);
    }
    pub fn clear(&mut self) {
        self.buffer.fill([0.0; 2]);
        self.wet = 0.0;
        self.mix = 0.0;
    }
    pub fn run(&mut self, dry: [f32; 2]) -> [f32; 2] {
        self.wet += (self.mix - self.wet) * self.smoothing;
        let tap = self.buffer[(self.cursor + self.buffer.len() - self.delay) % self.buffer.len()];
        self.buffer[self.cursor] = std::array::from_fn(|i| (dry[i] + tap[i] * self.feedback).clamp(-2.0, 2.0));
        self.cursor = (self.cursor + 1) % self.buffer.len();
        std::array::from_fn(|i| dry[i] * (1.0 - self.wet) + tap[i] * self.wet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn echo_repeats_at_the_selected_time_and_decays() {
        let mut echo = Echo::new(1000);
        echo.set(0.25, 0.4, 0.1, 1000);
        for _ in 0..200 { echo.run([0.0; 2]); }
        let dry = echo.run([1.0; 2]);
        assert!((dry[0] - 0.75).abs() < 0.001);
        let mut first = 0.0;
        let mut second = 0.0;
        for i in 1..=200 {
            let y = echo.run([0.0; 2]);
            if i == 100 { first = y[0]; }
            if i == 200 { second = y[0]; }
        }
        assert!((first - 0.25).abs() < 0.001);
        assert!((second - 0.1).abs() < 0.001);
        echo.clear();
        assert_eq!(echo.run([0.0; 2]), [0.0; 2]);
    }
}
