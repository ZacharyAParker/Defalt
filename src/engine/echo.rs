//! Bounded, preallocated stereo echo, wired as a send and a return.
//!
//! The dry record never goes through here. The deck adds what comes back
//! (`tap * return`) on top of its own signal, and feeds the delay line with
//! `signal * send + tap * feedback`. So turning the echo up never dips the
//! record, and -- because the deck keeps running the echo after its fader
//! closes or its transport stops -- an echo out actually echoes out: the
//! repeats carry on after the record has gone, until they die away.
//!
//! All state is allocated when the deck is built. Nothing here allocates,
//! locks or divides on the audio thread.

/// Longest delay the line can hold. Also bounds how long a tail can run.
const MAX_SECONDS: usize = 2;
/// Below this a repeat is inaudible under anything (-90 dBFS).
const SILENCE: f32 = 3.2e-5;
/// How long a change of delay time takes. Two read heads, one at the old
/// time and one at the new, crossfaded -- rather than one head sliding, which
/// is a tape machine changing pitch.
const RETIME_SECONDS: f32 = 0.030;
/// Feedback the transition planner may ask for, and feedback the automation
/// lane may ask for. 1.0 is a freeze: the line repeats forever with no loss,
/// and the clamp on what is written keeps even that bounded.
const MAX_FEEDBACK: f32 = 0.65;
pub const MAX_LANE_FEEDBACK: f32 = 1.0;
/// Hard bound on anything written into the line, so feedback at the limit
/// with signal still arriving cannot run away.
const CEILING: f32 = 1.5;

pub struct Echo {
    buffer: Vec<[f32; 2]>,
    cursor: usize,
    /// The read head being faded in, and the one being faded out.
    delay: usize,
    old_delay: usize,
    fade: f32,
    fade_step: f32,
    /// A retime that arrived mid-fade, applied when the fade finishes.
    queued: Option<usize>,
    /// Knob values. `mix` is kept as the planner set it; `ret` is the return
    /// level derived from it.
    pub mix: f32,
    pub feedback: f32,
    pub send: f32,
    ret: f32,
    /// Smoothed versions of the three levels, moved a little every frame.
    send_state: f32,
    ret_state: f32,
    feedback_state: f32,
    smoothing: f32,
    /// Consecutive frames written below `SILENCE`. Once a whole buffer's
    /// worth has gone by, nothing audible is left in the line.
    quiet: usize,
    rate: u32,
}

impl Echo {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1);
        let length = rate as usize * MAX_SECONDS;
        Self {
            buffer: vec![[0.0; 2]; length],
            cursor: 0,
            delay: (rate / 4).max(1) as usize,
            old_delay: (rate / 4).max(1) as usize,
            fade: 1.0,
            fade_step: 1.0 / (rate as f32 * RETIME_SECONDS).max(1.0),
            queued: None,
            mix: 0.0,
            feedback: 0.3,
            send: 1.0,
            ret: 0.0,
            send_state: 1.0,
            ret_state: 0.0,
            feedback_state: 0.3,
            smoothing: 1.0 - (-1.0 / (rate as f32 * 0.01)).exp(),
            quiet: length,
            rate,
        }
    }

    /// What the transition planner sends: a wet level 0..0.5, feedback
    /// 0..0.65 and a delay time.
    ///
    /// `mix` used to be a dry/wet balance. With the dry path left alone it
    /// becomes a return level that keeps the same wet-to-dry ratio: the old
    /// 0.5 (equal parts) is a return of 1.0, the old 0.25 is a third.
    pub fn set(&mut self, mix: f32, feedback: f32, seconds: f32, rate: u32) {
        self.mix = mix.clamp(0.0, 0.5);
        self.ret = self.mix / (1.0 - self.mix);
        self.feedback = feedback.clamp(0.0, MAX_FEEDBACK);
        let rate = if rate == 0 { self.rate } else { rate };
        self.retime(((seconds.max(0.0) * rate as f32) as usize).clamp(1, self.buffer.len() - 1));
    }

    /// How much of the channel goes into the line, 0..1. The automation
    /// lane's `EchoSend`; 1 is the default, so a planner that only ever
    /// sets `mix` hears the whole channel.
    pub fn set_send(&mut self, send: f32) {
        self.send = send.clamp(0.0, 1.0);
    }

    /// Feedback up to a freeze. The automation lane's `EchoFeedback`.
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback.clamp(0.0, MAX_LANE_FEEDBACK);
    }

    /// A new delay time, in seconds. The automation lane's `EchoBeats`
    /// arrives here as beats times the deck's beat length.
    pub fn set_seconds(&mut self, seconds: f32) {
        let frames = ((seconds.max(0.0) * self.rate as f32) as usize).clamp(1, self.buffer.len() - 1);
        self.retime(frames);
    }

    /// The delay time being faded to, in seconds.
    pub fn seconds(&self) -> f32 {
        self.queued.unwrap_or(self.delay) as f32 / self.rate as f32
    }

    fn retime(&mut self, frames: usize) {
        if frames == self.delay && self.queued.is_none() {
            return;
        }
        if self.fade < 1.0 {
            // Mid-crossfade: let it land first. A continuously moving lane
            // becomes a series of short crossfades instead of a smear.
            self.queued = Some(frames);
            return;
        }
        if frames == self.delay { self.queued = None; return; }
        self.old_delay = self.delay;
        self.delay = frames;
        self.fade = 0.0;
    }

    pub fn clear(&mut self) {
        self.buffer.fill([0.0; 2]);
        self.ret_state = 0.0;
        self.ret = 0.0;
        self.mix = 0.0;
        self.quiet = self.buffer.len();
    }

    /// True while something audible could still come out. The deck keeps
    /// calling `tick` with silence after it stops until this goes false.
    pub fn ringing(&self) -> bool {
        (self.ret > 0.0 || self.ret_state > 1e-6) && self.quiet < self.buffer.len()
    }

    #[inline]
    fn read(&self, delay: usize) -> [f32; 2] {
        let index = if self.cursor >= delay {
            self.cursor - delay
        } else {
            self.cursor + self.buffer.len() - delay
        };
        self.buffer[index]
    }

    /// Feed one frame of the channel and get back what the return adds.
    #[inline]
    pub fn tick(&mut self, input: [f32; 2]) -> [f32; 2] {
        self.send_state += (self.send - self.send_state) * self.smoothing;
        self.ret_state += (self.ret - self.ret_state) * self.smoothing;
        self.feedback_state += (self.feedback - self.feedback_state) * self.smoothing;

        let new = self.read(self.delay);
        let tap = if self.fade < 1.0 {
            let old = self.read(self.old_delay);
            let t = self.fade;
            self.fade = (self.fade + self.fade_step).min(1.0);
            if self.fade >= 1.0 {
                if let Some(next) = self.queued.take() {
                    self.fade = 1.0;
                    self.retime(next);
                }
            }
            [old[0] + (new[0] - old[0]) * t, old[1] + (new[1] - old[1]) * t]
        } else {
            new
        };

        let written: [f32; 2] = std::array::from_fn(|i| {
            (input[i] * self.send_state + tap[i] * self.feedback_state).clamp(-CEILING, CEILING)
        });
        self.buffer[self.cursor] = written;
        self.cursor += 1;
        if self.cursor == self.buffer.len() {
            self.cursor = 0;
        }
        if written[0].abs() < SILENCE && written[1].abs() < SILENCE {
            self.quiet = (self.quiet + 1).min(self.buffer.len());
        } else {
            self.quiet = 0;
        }
        [tap[0] * self.ret_state, tap[1] * self.ret_state]
    }

    /// The record with its echo on top, for callers that do not need the two
    /// apart.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn run(&mut self, dry: [f32; 2]) -> [f32; 2] {
        let wet = self.tick(dry);
        [dry[0] + wet[0], dry[1] + wet[1]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(mix: f32, feedback: f32, seconds: f32) -> Echo {
        let mut echo = Echo::new(1000);
        echo.set(mix, feedback, seconds, 1000);
        for _ in 0..200 { echo.run([0.0; 2]); }
        echo
    }

    #[test]
    fn echo_repeats_at_the_selected_time_and_decays() {
        let mut echo = settled(0.25, 0.4, 0.1);
        let dry = echo.run([1.0; 2]);
        let ret = 0.25 / 0.75;
        let mut first = 0.0;
        let mut second = 0.0;
        for i in 1..=200 {
            let y = echo.run([0.0; 2]);
            if i == 100 { first = y[0]; }
            if i == 200 { second = y[0]; }
        }
        assert_eq!(dry[0], 1.0, "the dry signal was touched");
        assert!((first - ret).abs() < 0.001, "{first}");
        assert!((second - ret * 0.4).abs() < 0.001, "{second}");
        echo.clear();
        assert_eq!(echo.run([0.0; 2]), [0.0; 2]);
    }

    #[test]
    fn turning_the_echo_up_never_dips_the_record() {
        let mut echo = Echo::new(1000);
        for i in 0..400 {
            if i == 100 { echo.set(0.5, 0.3, 0.3, 1000); }
            // Before the first repeat arrives, only the dry signal is heard.
            let y = echo.run([0.5; 2]);
            if i < 399 { assert!(y[0] >= 0.5 - 1e-6, "dry dipped to {} at {i}", y[0]); }
        }
    }

    #[test]
    fn the_tail_keeps_ringing_after_the_input_stops_and_then_ends() {
        let mut echo = settled(0.5, 0.5, 0.05);
        for _ in 0..50 { echo.tick([0.8; 2]); }
        let mut heard = 0.0f32;
        let mut frames = 0;
        while echo.ringing() && frames < 10_000 {
            heard = heard.max(echo.tick([0.0; 2])[0].abs());
            frames += 1;
            if frames == 500 { assert!(heard > 0.01, "nothing came back after the stop"); }
        }
        assert!(!echo.ringing(), "the tail never ended");
        assert!(frames > 500, "the tail was cut short at {frames} frames");
    }

    #[test]
    fn a_frozen_line_repeats_forever_but_stays_bounded() {
        let mut echo = settled(0.5, 0.0, 0.05);
        echo.set_feedback(1.0);
        for _ in 0..50 { echo.tick([2.0; 2]); }
        echo.set_send(0.0);
        let mut peak = 0.0f32;
        for i in 0..5_000 {
            let y = echo.tick([2.0; 2]);
            if i > 100 { peak = peak.max(y[0].abs()); }
        }
        assert!(peak > 0.5, "the freeze decayed: {peak}");
        assert!(peak <= CEILING, "the freeze ran away: {peak}");
    }

    #[test]
    fn changing_the_delay_time_crossfades_instead_of_jumping() {
        let mut echo = settled(0.5, 0.6, 0.1);
        let mut previous = 0.0f32;
        let mut largest = 0.0f32;
        for i in 0..3_000 {
            if i == 1_000 { echo.set_seconds(0.37); }
            if i == 1_010 { echo.set_seconds(0.2); }
            let x = (i as f32 * 0.02).sin() * 0.5;
            let y = echo.tick([x; 2])[0];
            if i > 1 { largest = largest.max((y - previous).abs()); }
            previous = y;
        }
        assert!(largest < 0.1, "retime clicked by {largest}");
    }
}
