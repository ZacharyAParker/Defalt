//! One deck: a record, a position, and a rate.
//!
//! Everything here runs on the audio thread, so nothing here allocates, locks,
//! or touches the filesystem. The only thing a deck ever does with an
//! `Arc<Track>` is read from it -- when a record is replaced the old one is
//! handed back to another thread to be dropped, because releasing the last
//! reference would free several hundred megabytes inside the callback.
//!
//! Nothing a deck does is allowed to be a step. Play fades in over a few
//! milliseconds and pause fades out before the transport stops; a seek, a hot
//! cue, a loop seam and a roll's release all crossfade from where the record
//! was to where it is going; a record that runs out fades on its last frames.
//! A step in a waveform is a click, and a click on a PA is a pop.

use std::sync::Arc;

use super::automation::{Automation, Lane};
use super::decode::Track;
use super::filters::Strip;

/// The four parts a record separates into. Order is fixed and matches what
/// the separator writes: drums, bass, everything harmonic, vocals.
pub const STEMS: usize = 4;
pub const STEM_NAMES: [&str; STEMS] = ["DRUMS", "BASS", "HARMONIC", "VOCALS"];

/// How long play takes to come up and pause takes to go down.
const FADE_SECONDS: f32 = 0.004;
/// Jumps crossfade over this long, held to 128..256 frames.
const JUMP_SECONDS: f32 = 0.004;
/// Stem levels, the transition level and the reverb send glide this fast.
const SMOOTH_SECONDS: f32 = 0.005;
/// The platter's inertia: a scrub rate is reached over about this long, so a
/// hand moving in UI-frame steps does not zipper.
const SCRUB_SECONDS: f64 = 0.008;
/// Frames between isolator updates when EQ is automated. The strip glides
/// across each gap, so the curve is followed to well under a millisecond.
const EQ_BLOCK: u64 = 32;
/// Rolls that can be waiting at once.
const PENDING_ROLLS: usize = 8;
/// What a beat is when the deck has no grid: 120 BPM.
const DEFAULT_BEAT_SECONDS: f64 = 0.5;

/// A jump under way: the old read head, still sounding, fading out.
#[derive(Clone, Copy)]
struct Jump {
    from: f64,
    left: u32,
}

/// The record a `Load` replaced, playing its last few milliseconds out.
pub struct Outgoing {
    pub track: Arc<Track>,
    position: f64,
    step: f64,
    gain: f32,
    left: u32,
    total: u32,
}

#[derive(Clone, Copy)]
struct ScheduledRoll {
    frame: u64,
    seconds: f64,
    until: u64,
}

#[derive(Clone, Copy)]
struct Roll {
    start: f64,
    end: f64,
    until: u64,
}

pub struct Deck {
    pub track: Option<Arc<Track>>,
    /// The same record, taken apart. When present it is played instead of
    /// `track` -- summed at unity the two are the same recording, so the
    /// swap is inaudible until you move a stem fader.
    pub stems: Option<[Arc<Track>; STEMS]>,
    /// Per-stem level, and whether it is silenced outright.
    pub stem_gain: [f32; STEMS],
    pub stem_muted: [bool; STEMS],
    stem_state: Option<[f32; STEMS]>,
    /// Playhead, in source frames. Fractional: the whole point is that it
    /// moves at a rate we choose rather than one frame per output frame.
    pub position: f64,
    pub playing: bool,
    pub gain: f32,
    gain_state: Option<f32>,
    /// A second level on top of `gain`, for transitions: the planner's
    /// crossfade lives here so it never fights the channel fader.
    pub level: f32,
    level_state: f32,
    pub echo: super::echo::Echo,
    /// Post-fader send into the console's shared reverb, 0..1.
    pub reverb_send: f32,
    reverb_state: f32,
    /// Multiplier on the record's own speed. 1.0 is as it was cut.
    pub speed: f64,
    pub key_lock: bool,
    stretch: Option<super::stretch::Stretch>,
    stretch_mix: f32,
    stretch_active: bool,
    /// Set while a hand is on the platter. Replaces `speed` outright, and can
    /// be negative, which is the whole difference between a deck and a player.
    pub scrub: Option<f64>,
    /// The rate actually being read at, gliding toward the wanted one.
    rate_state: Option<f64>,
    rate_automated: bool,
    /// Three bands and a sweep, per deck, before the sum.
    pub strip: Strip,
    /// Loudest sample this deck put on the bus since it was last read. The
    /// meter belongs to the channel, not the master -- you need to see a
    /// record's level before you bring it in, which is exactly when it is not
    /// on the master yet.
    pub peak: f32,
    /// Transport fade: where it is, where it is going, and whether reaching
    /// zero stops the deck.
    fade: f32,
    fade_target: f32,
    stopping: bool,
    jump: Option<Jump>,
    pub outgoing: Option<Outgoing>,
    /// A loop in source frames, and a roll (a loop that remembers where the
    /// record would have been) on top of it.
    loop_range: Option<(f64, f64)>,
    roll: Option<Roll>,
    shadow: f64,
    rolls: [Option<ScheduledRoll>; PENDING_ROLLS],
    next_roll: u64,
    /// Beat grid in seconds: where a beat falls and how long one is.
    pub grid: Option<(f64, f64)>,
    quantized: Option<f64>,
    play_at: Option<(u64, f64)>,
    pub automation: Automation,
    /// Output frame the next render starts at, for callers that do not pass
    /// one.
    clock: u64,
    device_rate: u32,
    smoothing: f32,
    fade_step: f32,
    fade_frames: u32,
    jump_frames: u32,
    scrub_smoothing: f64,
    mix_step: f32,
}

impl Deck {
    pub fn new(rate: u32) -> Self {
        let mut deck = Deck {
            track: None,
            stems: None,
            stem_gain: [1.0; STEMS],
            stem_muted: [false; STEMS],
            stem_state: None,
            position: 0.0,
            playing: false,
            gain: 1.0,
            gain_state: None,
            level: 1.0,
            level_state: 1.0,
            echo: super::echo::Echo::new(rate),
            reverb_send: 0.0,
            reverb_state: 0.0,
            speed: 1.0,
            key_lock: false,
            stretch: super::stretch::Stretch::new(rate),
            stretch_mix: 0.0,
            stretch_active: false,
            scrub: None,
            rate_state: None,
            rate_automated: false,
            strip: Strip::new(rate),
            peak: 0.0,
            fade: 1.0,
            fade_target: 1.0,
            stopping: false,
            jump: None,
            outgoing: None,
            loop_range: None,
            roll: None,
            shadow: 0.0,
            rolls: [None; PENDING_ROLLS],
            next_roll: u64::MAX,
            grid: None,
            quantized: None,
            play_at: None,
            automation: Automation::default(),
            clock: 0,
            device_rate: rate,
            smoothing: 0.0,
            fade_step: 0.0,
            fade_frames: 1,
            jump_frames: 128,
            scrub_smoothing: 0.0,
            mix_step: 0.0,
        };
        deck.set_timing(rate);
        deck
    }

    fn set_timing(&mut self, rate: u32) {
        let rate = rate.max(1);
        self.device_rate = rate;
        self.smoothing = 1.0 - (-1.0 / (rate as f32 * SMOOTH_SECONDS)).exp();
        self.fade_frames = ((rate as f32 * FADE_SECONDS) as u32).max(1);
        self.fade_step = 1.0 / self.fade_frames as f32;
        self.jump_frames = ((rate as f32 * JUMP_SECONDS) as u32).clamp(128, 256);
        self.scrub_smoothing = 1.0 - (-1.0 / (rate as f64 * SCRUB_SECONDS)).exp();
        self.mix_step = 1.0 / (rate as f32 * 0.02);
    }

    /// Rebuild everything that depends on the device rate, keeping the
    /// record, the playhead and every setting. Allocates: call it off the
    /// audio thread, with the stream stopped.
    pub fn set_rate(&mut self, rate: u32) {
        self.set_timing(rate);
        let [low, mid, high, sweep] = self.strip.settings();
        self.strip = Strip::new(rate);
        self.strip.set(low, mid, high, sweep);
        let (mix, feedback, seconds, send) =
            (self.echo.mix, self.echo.feedback, self.echo.seconds(), self.echo.send);
        self.echo = super::echo::Echo::new(rate);
        self.echo.set(mix, feedback.min(0.65), seconds, rate);
        self.echo.set_feedback(feedback);
        self.echo.set_send(send);
        self.stretch = super::stretch::Stretch::new(rate);
        self.stretch_mix = 0.0;
        self.stretch_active = false;
        self.sync_loop();
    }

    pub fn seconds(&self) -> f64 {
        match &self.track {
            Some(track) if track.sample_rate > 0 => {
                self.position / track.sample_rate as f64
            }
            _ => 0.0,
        }
    }

    /// Playing as the listener hears it: false from the moment a pause is
    /// asked for, even though the fade-out still has a few ms to run.
    pub fn transport_playing(&self) -> bool {
        self.playing && !self.stopping
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
    #[cfg_attr(not(test), allow(dead_code))]
    fn rate(&self, device_rate: u32) -> f64 {
        let Some(track) = &self.track else { return 0.0 };
        if device_rate == 0 {
            return 0.0;
        }
        let conversion = track.sample_rate as f64 / device_rate as f64;
        self.scrub.unwrap_or(self.speed) * conversion
    }

    fn source_rate(&self) -> f64 {
        self.track.as_ref().map_or(self.device_rate as f64, |t| t.sample_rate as f64)
    }

    fn last_frame(&self) -> f64 {
        self.track.as_ref().map_or(0.0, |t| t.frames() as f64)
    }

    /* -- transport -- */

    /// Start, fading in from silence.
    pub fn play(&mut self) {
        if self.track.is_none() {
            return;
        }
        if !self.playing {
            self.playing = true;
            self.fade = 0.0;
        }
        self.stopping = false;
        self.fade_target = 1.0;
    }

    /// Fade out, then stop. The playhead stops where the fade ends.
    pub fn pause(&mut self) {
        if self.playing {
            self.stopping = true;
            self.fade_target = 0.0;
        }
        // Anything waiting on the transport waits no longer.
        self.play_at = None;
        self.quantized = None;
        self.rolls = [None; PENDING_ROLLS];
        self.next_roll = u64::MAX;
    }

    /// Start at an exact output frame, from `source` frames into the record.
    /// A deck already playing jumps there instead, under a crossfade.
    pub fn play_at(&mut self, frame: u64, source: f64) {
        if self.track.is_some() {
            self.play_at = Some((frame, source));
        }
    }

    /// Put a record on, handing back everything it displaces. A record that
    /// was playing is kept for a few milliseconds more to fade out.
    pub fn load(&mut self, track: Arc<Track>, mut retire: impl FnMut(Arc<Track>)) {
        if let Some(old) = self.outgoing.take() {
            retire(old.track);
        }
        if let Some(old) = self.track.take() {
            let audible = self.playing && self.fade > 0.0;
            if audible && self.device_rate > 0 {
                let step = self.rate_state.unwrap_or(self.speed) * old.sample_rate as f64
                    / self.device_rate as f64;
                let gain = self.gain_state.unwrap_or(self.gain) * self.level_state * self.fade;
                self.outgoing = Some(Outgoing {
                    track: old,
                    position: self.position,
                    step,
                    gain,
                    left: self.fade_frames,
                    total: self.fade_frames,
                });
            } else {
                retire(old);
            }
        }
        // The old stems belong to the old record.
        for old in self.stems.take().into_iter().flatten() {
            retire(old);
        }
        self.position = 0.0;
        self.playing = false;
        self.stopping = false;
        self.fade = 1.0;
        self.fade_target = 1.0;
        self.scrub = None;
        self.rate_state = None;
        self.stem_gain = [1.0; STEMS];
        self.stem_muted = [false; STEMS];
        self.stem_state = None;
        self.track = Some(track);
        self.strip = Strip::new(self.device_rate);
        self.jump = None;
        self.loop_range = None;
        self.roll = None;
        self.rolls = [None; PENDING_ROLLS];
        self.next_roll = u64::MAX;
        self.quantized = None;
        self.play_at = None;
        self.grid = None;
        self.reset_stretch();
        self.sync_loop();
    }

    /// The outgoing record, once it has finished fading, for retiring.
    pub fn take_finished_outgoing(&mut self) -> Option<Arc<Track>> {
        if self.outgoing.as_ref().is_some_and(|o| o.left == 0) {
            return self.outgoing.take().map(|o| o.track);
        }
        None
    }

    /* -- jumps -- */

    fn current_levels(&self) -> [f32; STEMS] {
        self.stem_state.unwrap_or_else(|| self.stem_targets())
    }

    fn stem_targets(&self) -> [f32; STEMS] {
        std::array::from_fn(|i| if self.stem_muted[i] { 0.0 } else { self.stem_gain[i] })
    }

    fn stretch_crossfade(&mut self) {
        let (Some(track), Some(stretch)) = (self.track.clone(), self.stretch.as_mut()) else { return };
        let stems = self.stems.clone();
        let levels = self.stem_state.unwrap_or([1.0; STEMS]);
        stretch.crossfade(&track, stems.as_ref(), levels, self.speed);
    }

    /// Jump the playhead, in source frames. Playing, it crossfades from the
    /// old place to the new one (key-locked, the stretcher crossfades its
    /// own output and stays key-locked); stopped, it just moves.
    pub fn seek(&mut self, target: f64) {
        let target = target.clamp(0.0, self.last_frame());
        self.roll = None;
        self.sync_loop();
        if !self.playing || self.track.is_none() {
            self.position = target;
            self.jump = None;
            self.reset_stretch();
            return;
        }
        self.jump_to(target);
    }

    /// Crossfaded move while playing, without touching loops or rolls.
    fn jump_to(&mut self, target: f64) {
        self.jump = Some(Jump { from: self.position, left: self.jump_frames });
        self.position = target;
        if self.stretch_mix > 0.0 {
            self.stretch_crossfade();
        }
    }

    /// Jump on the next beat boundary of this deck's own grid, landing as far
    /// past `target` as the boundary was passed -- so a hot cue on a beat
    /// keeps the beat. Without a grid, or stopped, it is an ordinary seek.
    pub fn seek_quantized(&mut self, target: f64) {
        if self.playing && self.grid.is_some() {
            self.quantized = Some(target);
        } else {
            self.seek(target);
        }
    }

    /// Where in its beat the playhead is, 0..1, or `None` without a grid.
    pub fn phase(&self) -> Option<f64> {
        let (anchor, period) = self.grid?;
        if period <= 0.0 { return None; }
        let beats = (self.seconds() - anchor) / period;
        Some(beats - beats.floor())
    }

    /// The length of a beat as heard, in seconds.
    fn beat_seconds(&self) -> f64 {
        match self.grid {
            Some((_, period)) if period > 0.0 => period / self.speed.abs().max(0.05),
            _ => DEFAULT_BEAT_SECONDS,
        }
    }

    /* -- loops -- */

    /// Loop between two points in source frames, or stop looping. The loop
    /// takes effect when the playhead next crosses its end, so setting one
    /// ahead of the playhead waits for it and jumping out of one leaves it.
    ///
    /// Stopping looping stops rolling too: a roll still waiting is dropped,
    /// and one running lets go to where the record would have been. That is
    /// how a planned roll that is no longer wanted -- a skip, a record taken
    /// back by hand -- is called off.
    pub fn set_loop(&mut self, range: Option<(f64, f64)>) {
        self.loop_range = range.filter(|(a, b)| a.is_finite() && b.is_finite() && b > a && *a >= 0.0);
        if range.is_none() {
            self.cancel_rolls();
        }
        self.sync_loop();
    }

    /// Drop every waiting roll, and release a running one.
    pub fn cancel_rolls(&mut self) {
        self.rolls = [None; PENDING_ROLLS];
        self.next_roll = u64::MAX;
        if self.roll.is_some() {
            self.release_roll();
        }
    }

    /// Schedule a roll: at output frame `frame` the deck loops the next
    /// `seconds` of record from wherever it is, until output frame `until`,
    /// then carries on from where it would have been had it never looped.
    ///
    /// A roll that starts while another is running keeps that roll's in
    /// point and where-it-would-have-been, and only changes the length --
    /// so 4, 2, 1, 1/2 beat rolls in a row stutter the same downbeat
    /// tighter and tighter, then drop back in on time.
    pub fn schedule_roll(&mut self, frame: u64, seconds: f64, until: u64) -> bool {
        if !(seconds > 0.0) { return false; }
        let Some(slot) = self.rolls.iter_mut().find(|slot| slot.is_none()) else { return false };
        *slot = Some(ScheduledRoll { frame, seconds, until });
        self.next_roll = self.next_roll.min(frame);
        true
    }

    /// True while a roll is holding the playhead.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn rolling(&self) -> bool {
        self.roll.is_some()
    }

    fn effective_loop(&self) -> Option<(f64, f64)> {
        self.roll.map(|roll| (roll.start, roll.end)).or(self.loop_range)
    }

    /// Tell the stretcher about the loop. If it has already read past the
    /// loop's end, it is primed again from the playhead so it never plays
    /// audio from beyond the seam.
    fn sync_loop(&mut self) {
        let range = self.effective_loop();
        let Some(stretch) = self.stretch.as_mut() else { return };
        stretch.set_loop(range);
        if let Some((_, end)) = range {
            if stretch.primed() && stretch.input_position() >= end && self.position < end
                && self.stretch_mix > 0.0 {
                self.stretch_crossfade();
            }
        }
    }

    fn start_due_rolls(&mut self, now: u64) {
        let mut due: Option<ScheduledRoll> = None;
        let mut next = u64::MAX;
        for slot in self.rolls.iter_mut() {
            let Some(roll) = *slot else { continue };
            if roll.frame <= now {
                if due.map_or(true, |d| roll.frame >= d.frame) { due = Some(roll); }
                *slot = None;
            } else {
                next = next.min(roll.frame);
            }
        }
        self.next_roll = next;
        let Some(scheduled) = due else { return };
        let length = (scheduled.seconds * self.source_rate()).max(16.0);
        match self.roll.as_mut() {
            Some(roll) => {
                roll.end = roll.start + length;
                roll.until = scheduled.until;
            }
            None => {
                self.roll = Some(Roll {
                    start: self.position,
                    end: self.position + length,
                    until: scheduled.until,
                });
                self.shadow = self.position;
            }
        }
        if let Some(roll) = self.roll {
            if self.position >= roll.end {
                // Shortened past the playhead: come round now.
                let target = roll.start + (self.position - roll.start) % (roll.end - roll.start);
                self.jump_to(target);
            }
        }
        self.sync_loop();
    }

    fn release_roll(&mut self) {
        let target = self.shadow;
        self.roll = None;
        self.sync_loop();
        if self.playing {
            self.jump_to(target.clamp(0.0, self.last_frame()));
        } else {
            self.position = target;
        }
    }

    /* -- automation -- */

    fn automate(&mut self, now: u64, eq_tick: bool, gain: &mut f32) {
        self.rate_automated = false;
        let mut eq = None;
        for lane in Lane::ALL {
            let Some(value) = self.automation.value(lane, now) else { continue };
            match lane {
                Lane::Gain => {
                    self.gain = value.clamp(0.0, 2.0);
                    *gain = self.gain;
                }
                Lane::Level => {
                    self.level = value.clamp(0.0, 1.0);
                    self.level_state = self.level;
                }
                Lane::Low | Lane::Mid | Lane::High | Lane::Sweep => {
                    if eq_tick {
                        let settings = eq.get_or_insert(self.strip.settings());
                        let band = match lane { Lane::Low => 0, Lane::Mid => 1, Lane::High => 2, _ => 3 };
                        settings[band] = value;
                    }
                }
                Lane::EchoSend => self.echo.set_send(value),
                Lane::EchoFeedback => self.echo.set_feedback(value),
                Lane::EchoBeats => {
                    let seconds = value.max(0.0) as f64 * self.beat_seconds();
                    self.echo.set_seconds(seconds as f32);
                }
                Lane::ReverbSend => {
                    self.reverb_send = value.clamp(0.0, 1.0);
                    self.reverb_state = self.reverb_send;
                }
                Lane::Rate => {
                    self.speed = (value as f64).clamp(-4.0, 4.0);
                    self.rate_automated = true;
                }
                Lane::StemDrums | Lane::StemBass | Lane::StemHarmonic | Lane::StemVocals => {
                    // Only a separated record has stems to move.
                    if self.stems.is_some() {
                        let stem = lane.stem().unwrap_or(0);
                        let level = value.clamp(0.0, 2.0);
                        self.stem_gain[stem] = level;
                        let mut state = self.current_levels();
                        state[stem] = if self.stem_muted[stem] { 0.0 } else { level };
                        self.stem_state = Some(state);
                    }
                }
            }
        }
        if let Some([low, mid, high, sweep]) = eq {
            self.strip.set_over(low, mid, high, sweep, EQ_BLOCK as u32);
        }
    }

    /* -- rendering -- */

    /// Add this deck's contribution to an interleaved stereo output buffer.
    ///
    /// Uses the deck's own frame count as the clock for automation and
    /// scheduled starts; the console calls `render` with the real one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn mix_into(&mut self, out: &mut [f32], device_rate: u32) {
        let clock = self.clock;
        self.render(out, None, device_rate, clock);
    }

    #[inline]
    fn read(track: &Track, stems: Option<&[Arc<Track>; STEMS]>, levels: [f32; STEMS], position: f64) -> [f32; 2] {
        match stems {
            Some(parts) => {
                let mut sum = [0.0f32; 2];
                for (part, level) in parts.iter().zip(levels) {
                    if level <= 0.0 {
                        continue;
                    }
                    let [l, r] = sample_at(part, position);
                    sum[0] += l * level;
                    sum[1] += r * level;
                }
                sum
            }
            None => sample_at(track, position),
        }
    }

    fn mix_outgoing(&mut self, out: &mut [f32]) {
        let Some(outgoing) = self.outgoing.as_mut() else { return };
        for frame in out.chunks_exact_mut(2) {
            if outgoing.left == 0 { break; }
            let gain = outgoing.gain * outgoing.left as f32 / (outgoing.total + 1) as f32;
            let [l, r] = sample_at(&outgoing.track, outgoing.position);
            frame[0] += l * gain;
            frame[1] += r * gain;
            outgoing.position += outgoing.step;
            outgoing.left -= 1;
        }
    }

    /// Echo repeats with nothing new going in: a stopped deck's tail.
    #[inline]
    fn ring(&mut self, frame: &mut [f32]) {
        if self.echo.ringing() {
            let [l, r] = self.echo.tick([0.0; 2]);
            frame[0] += l;
            frame[1] += r;
        }
    }

    /// Render `out.len() / 2` frames starting at output frame `frame`, adding
    /// the deck to `out` and its reverb send to `send`.
    pub fn render(&mut self, out: &mut [f32], mut send: Option<&mut [f32]>, device_rate: u32, frame: u64) {
        let frames = out.len() / 2;
        self.clock = frame + frames as u64;
        self.mix_outgoing(out);

        if !self.playing && self.play_at.is_none() {
            self.gain_state = None;
            self.stem_state = None;
            self.rate_state = None;
            self.reset_stretch();
            if self.automation.any() && frames > 0 {
                // Nothing to hear, but a lane still moves its control, so
                // whatever starts next starts from the curve's value.
                let mut gain = self.gain;
                self.automate(frame + frames as u64 - 1, true, &mut gain);
            }
            if self.echo.ringing() {
                for pair in out.chunks_exact_mut(2) { self.ring(pair); }
            }
            return;
        }
        let Some(track) = self.track.clone() else { return };
        if device_rate == 0 {
            return;
        }
        let conversion = track.sample_rate as f64 / device_rate as f64;
        let last = track.frames() as f64;
        // A record's last few ms fade out, but never more than an eighth of
        // it: a jingle a hundred frames long is still mostly heard.
        let end_fade = (self.fade_frames as f64).min(last / 8.0).max(1.0);

        // Cloning the Arcs is a refcount bump, not an allocation, and the
        // deck keeps its own reference so none of these can reach zero here.
        let stems = self.stems.clone();
        let mut gain = self.gain_state.unwrap_or(self.gain);
        let mut peak = self.peak;

        for (index, pair) in out.chunks_exact_mut(2).enumerate() {
            let now = frame + index as u64;

            if let Some((at, source)) = self.play_at {
                if now >= at {
                    self.play_at = None;
                    // Late only if the command itself arrived late: join
                    // where the record would be by now.
                    let late = (now - at) as f64 * self.speed * conversion;
                    let target = (source + late).clamp(0.0, last);
                    if self.playing && !self.stopping {
                        self.seek(target);
                    } else {
                        self.position = target;
                        self.playing = true;
                        self.stopping = false;
                        self.jump = None;
                        self.fade = 0.0;
                        self.fade_target = 1.0;
                        self.stretch_mix = 0.0;
                        self.stretch_active = false;
                        if let Some(stretch) = self.stretch.as_mut() { stretch.reset(); }
                    }
                }
            }
            if !self.playing {
                self.ring(pair);
                continue;
            }

            if self.automation.any() {
                self.automate(now, now % EQ_BLOCK == 0 || index == 0, &mut gain);
            } else {
                self.rate_automated = false;
            }
            if now >= self.next_roll {
                self.start_due_rolls(now);
            }
            if self.roll.is_some_and(|roll| now >= roll.until) {
                self.release_roll();
            }

            // Running off either end stops the deck rather than wrapping. A
            // record that silently restarted would be worse than silence.
            if self.position < 0.0 || self.position >= last {
                self.playing = false;
                self.stopping = false;
                self.fade = 1.0;
                self.fade_target = 1.0;
                self.jump = None;
                self.position = self.position.clamp(0.0, last);
                self.ring(pair);
                continue;
            }

            if self.fade < self.fade_target {
                self.fade = (self.fade + self.fade_step).min(self.fade_target);
            } else if self.fade > self.fade_target {
                self.fade = (self.fade - self.fade_step).max(self.fade_target);
            }

            // The wanted rate, reached through the platter's inertia unless
            // automation is placing it exactly.
            let wanted = self.scrub.unwrap_or(self.speed);
            let rate = match self.rate_state {
                Some(state) if !self.rate_automated => state + (wanted - state) * self.scrub_smoothing,
                _ => wanted,
            };
            self.rate_state = Some(rate);
            let read_rate = rate * conversion;

            let targets = self.stem_targets();
            let levels = match self.stem_state.as_mut() {
                Some(state) => {
                    for i in 0..STEMS { state[i] += (targets[i] - state[i]) * self.smoothing; }
                    *state
                }
                None => {
                    self.stem_state = Some(targets);
                    targets
                }
            };

            // Key lock is a stretcher, and a stretcher at exactly 1.0 is an
            // expensive way of doing nothing, so at 1.0 it steps out (under
            // the same crossfade as anything else leaving it).
            let use_stretch = self.key_lock && self.scrub.is_none()
                && (0.5..=2.0).contains(&self.speed) && (self.speed - 1.0).abs() > 1e-6
                && self.stretch.is_some();
            if use_stretch && !self.stretch_active && self.stretch_mix == 0.0 {
                if let Some(stretch) = self.stretch.as_mut() { stretch.reset(); }
            }
            self.stretch_active = use_stretch;
            let mut advance = read_rate;
            let mut wet = [0.0f32; 2];
            if use_stretch {
                let (frame, step) = self.stretch.as_mut().unwrap()
                    .next(&track, stems.as_ref(), levels, self.position, self.speed);
                wet = frame;
                advance = step;
                self.stretch_mix = (self.stretch_mix + self.mix_step).min(1.0);
            } else if self.stretch_mix > 0.0 {
                // Leaving key lock (including a scratch) fades back to the
                // original direct path; old spectral history is then discarded.
                let (frame, _) = self.stretch.as_mut().unwrap()
                    .next(&track, stems.as_ref(), levels, self.position, self.speed.clamp(0.5, 2.0));
                wet = frame;
                self.stretch_mix = (self.stretch_mix - self.mix_step).max(0.0);
                if self.stretch_mix == 0.0 {
                    if let Some(stretch) = self.stretch.as_mut() { stretch.reset(); }
                }
            }

            // The direct path, only when some of it is being heard.
            let signal = if self.stretch_mix >= 1.0 {
                if let Some(jump) = self.jump.as_mut() {
                    jump.left = jump.left.saturating_sub(1);
                    if jump.left == 0 { self.jump = None; }
                }
                wet
            } else {
                let mut dry = Self::read(&track, stems.as_ref(), levels, self.position);
                if let Some(jump) = self.jump.as_mut() {
                    let old = Self::read(&track, stems.as_ref(), levels, jump.from);
                    let w = 1.0 - jump.left as f32 / (self.jump_frames + 1) as f32;
                    dry = [old[0] + (dry[0] - old[0]) * w, old[1] + (dry[1] - old[1]) * w];
                    jump.from += advance;
                    jump.left -= 1;
                    if jump.left == 0 { self.jump = None; }
                }
                let mix = self.stretch_mix;
                [dry[0] + (wet[0] - dry[0]) * mix, dry[1] + (wet[1] - dry[1]) * mix]
            };

            let [mut left, mut right] = signal;
            // Skipped entirely when every knob is at its detent, so a flat
            // channel costs nothing.
            if !self.strip.bypassed {
                [left, right] = self.strip.process([left, right]);
            }
            // Meter before the channel/crossfader gain. A silent channel
            // still advances and can be lined up before bringing it in.
            peak = peak.max(left.abs()).max(right.abs());

            gain += (self.gain - gain) * self.smoothing;
            self.level_state += (self.level - self.level_state) * self.smoothing;
            let looping_ahead = advance > 0.0
                && self.effective_loop().is_some_and(|(_, end)| self.position < end && end < last);
            let edge = if looping_ahead {
                f64::INFINITY
            } else if advance >= 0.0 {
                last - self.position
            } else {
                self.position
            };
            let end_gain = (edge / advance.abs().max(1e-9) / end_fade).min(1.0) as f32;
            let fader = gain * self.level_state * self.fade * end_gain;
            left *= fader;
            right *= fader;

            // Post-fader sends: pulling the fader stops feeding the echo and
            // the reverb, and leaves what is already in them to ring.
            let [echo_l, echo_r] = self.echo.tick([left, right]);
            pair[0] += left + echo_l;
            pair[1] += right + echo_r;
            self.reverb_state += (self.reverb_send - self.reverb_state) * self.smoothing;
            if let Some(bus) = send.as_deref_mut() {
                bus[index * 2] += left * self.reverb_state;
                bus[index * 2 + 1] += right * self.reverb_state;
            }

            let before = self.position;
            self.position += advance;
            if self.roll.is_some() {
                self.shadow += advance;
            }
            if advance > 0.0 {
                if let Some((start, end)) = self.effective_loop() {
                    if before < end && self.position >= end {
                        let target = start + (self.position - end) % (end - start);
                        // Key-locked, the stretcher's input already wrapped
                        // under its own crossfade; only the direct path
                        // needs one here.
                        self.jump = Some(Jump { from: self.position, left: self.jump_frames });
                        self.position = target;
                    }
                }
            }
            if let (Some(target), Some((anchor, period))) = (self.quantized, self.grid) {
                let source_rate = track.sample_rate as f64;
                let beat = |position: f64| ((position / source_rate - anchor) / period).floor();
                if period > 0.0 && advance > 0.0 && beat(self.position) != beat(before) {
                    let boundary = (anchor + beat(self.position) * period) * source_rate;
                    let past = (self.position - boundary).max(0.0);
                    self.quantized = None;
                    self.seek(target + past);
                }
            }
            if self.stopping && self.fade <= 0.0 {
                self.playing = false;
                self.stopping = false;
                self.fade = 1.0;
                self.fade_target = 1.0;
                self.jump = None;
            }
        }
        self.peak = peak;
        self.gain_state = Some(gain);
    }
}

/// Catmull-Rom between the four frames around a fractional position.
///
/// Records arrive already converted to the device rate, so at speed 1.0 the
/// read head lands on whole frames and this returns them untouched. It only
/// interpolates when the pitch or a hand moves the rate, which is where a
/// cheap cubic is the right tool.
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
    use super::super::automation::Curve;

    fn ramp(frames: usize, sample_rate: u32) -> Arc<Track> {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            samples.push(i as f32);
            samples.push(i as f32);
        }
        Arc::new(Track { samples, sample_rate })
    }

    fn sine(hz: f64, seconds: f64, rate: u32) -> Arc<Track> {
        let samples = (0..(seconds * rate as f64) as usize).flat_map(|i| {
            let v = (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() as f32 * 0.5;
            [v, v]
        }).collect();
        Arc::new(Track { samples, sample_rate: rate })
    }

    fn constant(level: f32, seconds: f64, rate: u32) -> Arc<Track> {
        Arc::new(Track { samples: vec![level; (seconds * rate as f64) as usize * 2], sample_rate: rate })
    }

    fn playing(track: Arc<Track>) -> Deck {
        let mut deck = Deck::new(48_000);
        deck.track = Some(track);
        deck.playing = true;
        deck
    }

    /// Render in callback-sized pieces on the deck's own clock.
    fn render(deck: &mut Deck, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(512) { deck.mix_into(chunk, 48_000); }
        out
    }

    fn left(out: &[f32]) -> Vec<f32> {
        out.iter().step_by(2).copied().collect()
    }

    fn largest_step(signal: &[f32]) -> f32 {
        signal.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
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

    #[test]
    fn muting_a_stem_glides_rather_than_stepping() {
        let mut deck = Deck {
            track: Some(constant(0.4, 1.0, 48_000)),
            stems: Some(std::array::from_fn(|_| constant(0.1, 1.0, 48_000))),
            ..Deck::new(48_000)
        };
        deck.playing = true;
        render(&mut deck, 100);
        deck.stem_muted[0] = true;
        let after = left(&render(&mut deck, 2_400));
        assert!(largest_step(&after) < 0.01, "the mute stepped");
        assert!((after[2_399] - 0.3).abs() < 1e-3, "the mute never landed: {}", after[2_399]);
    }

    /* -- declicking -- */

    #[test]
    fn play_fades_in_and_pause_fades_out_before_stopping() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(constant(0.5, 1.0, 48_000));
        deck.play();
        let start = left(&render(&mut deck, 1_000));
        assert!(start[0] > 0.0 && start[0] < 0.01, "play started at {}", start[0]);
        assert!((start[999] - 0.5).abs() < 1e-4);
        assert!(largest_step(&start) < 0.01);
        deck.pause();
        assert!(!deck.transport_playing(), "a paused deck still reports playing");
        let stop = left(&render(&mut deck, 1_000));
        assert!(largest_step(&stop) < 0.01, "pause clicked");
        assert!(!deck.playing);
        assert_eq!(stop[999], 0.0);
        let parked = deck.position;
        render(&mut deck, 1_000);
        assert_eq!(deck.position, parked, "a paused deck kept moving");
    }

    #[test]
    fn a_seek_while_playing_crossfades_between_the_two_places() {
        let mut deck = playing(sine(200.0, 4.0, 48_000));
        let mut all = left(&render(&mut deck, 10_000));
        // Half a cycle out of phase: the worst possible cut.
        deck.seek(10_000.0 + 48_000.0 * 1.0 + 120.0);
        all.extend(left(&render(&mut deck, 10_000)));
        // A 200 Hz sine at 0.5 moves at most 0.013 a sample.
        assert!(largest_step(&all) < 0.025, "the seek clicked: {}", largest_step(&all));
    }

    #[test]
    fn a_record_replaced_while_playing_fades_out_instead_of_cutting() {
        let mut deck = playing(constant(0.5, 1.0, 48_000));
        let before = left(&render(&mut deck, 500));
        let mut retired = 0;
        deck.load(constant(0.0, 1.0, 48_000), |_| retired += 1);
        let after = left(&render(&mut deck, 500));
        assert!((after[0] - before[499]).abs() < 0.01, "the old record cut off");
        assert_eq!(after[499], 0.0);
        assert!(deck.take_finished_outgoing().is_some());
        assert_eq!(retired, 0, "the old record was retired before it finished sounding");
    }

    #[test]
    fn the_last_frames_of_a_record_fade_rather_than_stop_dead() {
        let mut deck = playing(constant(0.5, 0.1, 48_000));
        deck.position = 4_800.0 - 500.0;
        let tail = left(&render(&mut deck, 600));
        assert!(largest_step(&tail) < 0.01, "the end clicked");
        assert!(!deck.playing);
    }

    /* -- echo out -- */

    #[test]
    fn the_echo_keeps_ringing_after_the_fader_closes_and_the_deck_stops() {
        let mut deck = playing(sine(300.0, 2.0, 48_000));
        deck.echo.set(0.5, 0.5, 0.1, 48_000);
        render(&mut deck, 9_600);
        deck.gain = 0.0;
        deck.pause();
        render(&mut deck, 2_400);
        assert!(!deck.playing);
        let tail = left(&render(&mut deck, 9_600));
        let loud = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(loud > 0.02, "the echo died with the record: {loud}");
    }

    /* -- loops and rolls -- */

    #[test]
    fn a_loop_seam_is_continuous() {
        // A loop whose seam lands mid-cycle: without the crossfade this is a
        // step of the whole waveform.
        let mut deck = playing(sine(220.0, 3.0, 48_000));
        deck.set_loop(Some((10_000.0, 19_931.0)));
        let out = left(&render(&mut deck, 48_000));
        assert!(deck.position < 19_931.0 && deck.position >= 10_000.0, "left the loop: {}", deck.position);
        assert!(largest_step(&out) < 0.025, "the seam clicked: {}", largest_step(&out));
    }

    #[test]
    fn a_key_locked_loop_stays_in_its_loop_and_does_not_click() {
        let mut deck = playing(sine(220.0, 4.0, 48_000));
        deck.key_lock = true;
        deck.speed = 0.94;
        render(&mut deck, 4_800);
        deck.set_loop(Some((12_000.0, 36_111.0)));
        let out = left(&render(&mut deck, 96_000));
        assert!(deck.position < 36_111.0 && deck.position >= 12_000.0, "left the loop: {}", deck.position);
        let late = &out[48_000..];
        assert!(largest_step(late) < 0.03, "the key-locked seam clicked: {}", largest_step(late));
    }

    #[test]
    fn a_roll_loops_then_drops_back_in_where_the_record_would_have_been() {
        let mut deck = playing(ramp(96_000, 48_000));
        deck.gain = 1.0;
        // Roll 1000 frames of record from frame 2000 until 12000.
        assert!(deck.schedule_roll(2_000, 1_000.0 / 48_000.0, 12_000));
        render(&mut deck, 6_000);
        assert!(deck.rolling());
        assert!(deck.position >= 2_000.0 && deck.position < 3_000.0, "{}", deck.position);
        render(&mut deck, 7_000);
        assert!(!deck.rolling());
        assert_eq!(deck.position, 13_000.0, "slip lost its place");
    }

    #[test]
    fn shorter_rolls_in_a_row_keep_the_same_in_point() {
        let mut deck = playing(ramp(96_000, 48_000));
        let beat = 4_800.0 / 48_000.0;
        deck.schedule_roll(1_000, beat, 20_000);
        deck.schedule_roll(10_000, beat / 2.0, 20_000);
        render(&mut deck, 9_000);
        let first = deck.effective_loop().unwrap();
        render(&mut deck, 2_000);
        let second = deck.effective_loop().unwrap();
        assert_eq!(first.0, second.0);
        assert!((second.1 - second.0 - 2_400.0).abs() < 1e-6);
        assert!(deck.position < second.1);
        render(&mut deck, 10_000);
        assert_eq!(deck.position, 21_000.0);
    }

    /* -- scheduling and automation -- */

    #[test]
    fn stopping_the_loop_calls_off_waiting_and_running_rolls() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(constant(0.5, 4.0, 48_000));
        deck.playing = true;
        assert!(deck.schedule_roll(10_000, 0.1, 40_000));
        deck.set_loop(None);
        render(&mut deck, 20_000);
        assert!(!deck.rolling(), "a called-off roll still started");
        assert!(deck.schedule_roll(deck.clock + 100, 0.1, deck.clock + 30_000));
        render(&mut deck, 2_000);
        assert!(deck.rolling());
        deck.set_loop(None);
        assert!(!deck.rolling(), "a running roll kept holding the playhead");
        assert!(deck.schedule_roll(deck.clock + 100, 0.1, deck.clock + 30_000));
        deck.pause();
        deck.play();
        render(&mut deck, 2_000);
        assert!(!deck.rolling(), "a pause left a roll waiting");
    }

    #[test]
    fn play_at_starts_on_its_exact_output_frame() {
        let mut deck = Deck::new(48_000);
        deck.track = Some(constant(0.5, 1.0, 48_000));
        deck.play_at(1_234, 4_800.0);
        let out = left(&render(&mut deck, 3_000));
        assert!(out[..1_234].iter().all(|&s| s == 0.0), "started early");
        assert!(out[1_234] > 0.0, "did not start on its frame");
        assert!((deck.position - (4_800.0 + (3_000 - 1_234) as f64)).abs() < 1e-9);
    }

    #[test]
    fn a_gain_gate_is_exact_to_the_sample() {
        let mut deck = playing(constant(0.5, 1.0, 48_000));
        let curve = Curve::new(0, vec![(0.0, 1.0), (1_000.0, 1.0), (1_000.0, 0.0),
                                       (1_048.0, 0.0), (1_048.0, 1.0)]);
        deck.automation.set(Lane::Gain, Arc::new(curve));
        let out = left(&render(&mut deck, 2_000));
        assert_eq!(out[999], 0.5);
        assert!(out[1_000..1_048].iter().all(|&s| s == 0.0), "the gate leaked");
        assert_eq!(out[1_048], 0.5, "the gate did not reopen on its sample");
    }

    #[test]
    fn a_level_curve_is_followed_sample_for_sample() {
        let mut deck = playing(constant(0.5, 1.0, 48_000));
        deck.automation.set(Lane::Level, Arc::new(Curve::new(500, vec![(0.0, 1.0), (1_000.0, 0.0)])));
        let out = left(&render(&mut deck, 2_000));
        for i in [500usize, 750, 1_000, 1_499] {
            let expected = 0.5 * (1.0 - (i - 500) as f32 / 1_000.0);
            assert!((out[i] - expected).abs() < 1e-6, "frame {i}: {} vs {expected}", out[i]);
        }
        assert_eq!(out[1_600], 0.0);
        deck.automation.clear(Lane::Level);
        assert_eq!(deck.level, 0.0, "detaching must keep the curve's last value");
    }

    #[test]
    fn a_brake_slows_to_a_stop_over_one_beat() {
        let mut deck = playing(sine(440.0, 4.0, 48_000));
        deck.position = 48_000.0;
        let beat = 24_000.0;
        deck.automation.set(Lane::Rate, Arc::new(Curve::new(0, vec![(0.0, 1.0), (beat, 0.0)])));
        let out = left(&render(&mut deck, 30_000));
        // The area under a straight line from 1 to 0.
        assert!((deck.position - (48_000.0 + beat / 2.0)).abs() < 2.0, "{}", deck.position);
        assert!(largest_step(&out) < 0.06, "the brake zippered");
        assert!(out[29_000..].iter().all(|s| s.abs() < 0.5));
    }

    #[test]
    fn a_spinback_runs_the_record_backwards_and_comes_to_rest() {
        let mut deck = playing(sine(440.0, 4.0, 48_000));
        deck.key_lock = true;
        deck.position = 96_000.0;
        let length = 24_000.0;
        deck.automation.set(Lane::Rate, Arc::new(Curve::new(0, vec![(0.0, -3.0), (length, 0.0)])));
        let out = left(&render(&mut deck, 26_000));
        assert!((deck.position - (96_000.0 - 1.5 * length)).abs() < 3.0, "{}", deck.position);
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 0.5 + 1e-3));
        assert!(largest_step(&out[100..]) < 0.2);
    }

    #[test]
    fn a_stem_lane_moves_one_part_and_is_ignored_without_stems() {
        let mut plain = playing(constant(0.4, 1.0, 48_000));
        plain.automation.set(Lane::StemVocals, Arc::new(Curve::new(0, vec![(0.0, 0.0)])));
        assert_eq!(left(&render(&mut plain, 10))[9], 0.4);
        let mut split = Deck {
            stems: Some(std::array::from_fn(|_| constant(0.1, 1.0, 48_000))),
            ..playing(constant(0.4, 1.0, 48_000))
        };
        split.automation.set(Lane::StemVocals, Arc::new(Curve::new(0, vec![(0.0, 0.0)])));
        assert!((left(&render(&mut split, 10))[9] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn a_quantized_jump_waits_for_the_beat() {
        let mut deck = playing(ramp(96_000, 48_000));
        deck.grid = Some((0.0, 0.1));
        deck.position = 1_000.0;
        deck.seek_quantized(40_000.0);
        render(&mut deck, 3_000);
        // The beat at 4800 is passed at output frame 3800; not yet.
        assert_eq!(deck.position, 4_000.0);
        render(&mut deck, 1_000);
        // Jumped on frame 4800 of the record, keeping the 200 frames since.
        assert_eq!(deck.position, 40_200.0);
    }
}
