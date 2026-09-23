//! The decks: transport, tempo, tone, the mixer, cues and loops.

use crate::engine::{Command, Curve, Lane, DECKS};
use crate::{assist, keys, label, ui, Defalt, Take};

/// Seconds a beat lasts on a record with no grid, for loops: 120 BPM.
const FALLBACK_BEAT: f64 = 0.5;

impl Defalt {
    pub fn play_pause(&mut self, deck: usize) {
        if self.decks[deck].record.is_none() || !self.engine_ready() {
            return;
        }
        self.touch(deck);
        self.airtime.held.tempo[deck] = true;
        let playing = !self.decks[deck].playing;
        self.decks[deck].playing = playing;
        self.send(if playing { Command::Play { deck } } else { Command::Pause { deck } });
    }

    pub fn cue(&mut self, deck: usize) {
        self.touch(deck);
        self.airtime.held.tempo[deck] = true;
        self.decks[deck].position = 0.0;
        self.send(Command::Seek { deck, seconds: 0.0 });
    }

    pub fn seek(&mut self, deck: usize, seconds: f64) {
        self.airtime.held.tempo[deck] = true;
        self.seek_audio(deck, seconds);
    }

    pub(crate) fn seek_audio(&mut self, deck: usize, seconds: f64) {
        let length = self.decks[deck].length;
        let seconds = seconds.clamp(0.0, length);
        self.decks[deck].position = seconds;
        self.send(Command::Seek { deck, seconds });
    }

    pub fn set_pitch(&mut self, deck: usize, percent: f32) {
        self.touch(deck);
        self.hold_tempo(deck);
        self.decks[deck].pitch = percent;
        self.apply_speed(deck);
    }

    /// Your hand on the tempo: the station's rate lane lets go of it.
    fn hold_tempo(&mut self, deck: usize) {
        if !std::mem::replace(&mut self.airtime.held.tempo[deck], true) && self.airtime.on {
            self.send(Command::Detach { deck, lane: Lane::Rate });
        }
    }

    pub fn reset_tempo(&mut self, deck: usize) {
        self.decks[deck].bend = 0.0;
        self.set_pitch(deck, 0.0);
        self.say("Tempo reset to the record's original speed.");
    }

    pub fn toggle_key_lock(&mut self, deck: usize) {
        self.decks[deck].key_lock = !self.decks[deck].key_lock;
        self.airtime.held.key_lock[deck] = true;
        self.send(Command::KeyLock { deck, enabled: self.decks[deck].key_lock });
    }

    /// The fader plus whatever a held bend is adding.
    pub(crate) fn apply_speed(&mut self, deck: usize) {
        let percent = self.decks[deck].pitch + self.decks[deck].bend;
        self.send(Command::Speed { deck, value: 1.0 + percent as f64 / 100.0 });
    }

    pub fn push_tone(&mut self, deck: usize) {
        let [mut low, mut mid, mut high, sweep] = self.decks[deck].tone;
        // A kill sits on top of the knob rather than moving it, so letting go
        // puts the band back exactly where you had it.
        let killed = self.decks[deck].killed;
        if killed[0] { low = 0.0; }
        if killed[1] { mid = 0.0; }
        if killed[2] { high = 0.0; }
        let tone = [low, mid, high, sweep];
        if self.sent_tone[deck] == Some(tone) {
            return;
        }
        self.sent_tone[deck] = Some(tone);
        self.send(Command::Tone { deck, low, mid, high, sweep });
    }

    /// The channel faders and the crossfader are one number per deck by the
    /// time the engine sees them. Equal power, so the middle is not a dip.
    ///
    /// On the radio the station's transition rides each deck's level lane,
    /// so the crossfader is left out of it -- until you take hold of it.
    pub fn push_gains(&mut self) {
        let a = (self.crossfade * std::f32::consts::FRAC_PI_2).cos();
        let b = ((1.0 - self.crossfade) * std::f32::consts::FRAC_PI_2).cos();
        let neutral = self.airtime.on && !self.airtime.held.crossfade;
        let curve = if neutral { [1.0, 1.0] } else { [a, b] };
        for deck in 0..DECKS {
            let value = self.decks[deck].gain * self.decks[deck].trim * curve[deck];
            if self.sent_gain[deck] == Some(value) {
                continue;
            }
            self.sent_gain[deck] = Some(value);
            self.send(Command::Gain { deck, value });
        }
    }

    pub fn set_master(&mut self, value: f32) {
        self.master = value;
        self.send(Command::Master { value });
    }

    pub fn toggle_limiter(&mut self) {
        self.limiter_on = !self.limiter_on;
        let enabled = self.limiter_on;
        self.send(Command::Limiter { enabled });
        self.say(if enabled { "Limiter on." } else { "Limiter off: the master can clip." });
    }

    /// The record's beat grid, as the engine should know it.
    pub(crate) fn send_grid(&mut self, deck: usize) {
        let (anchor, period) = self.grid(deck).unwrap_or((0.0, 0.0));
        self.send(Command::Grid { deck, anchor_seconds: anchor, period_seconds: period });
    }

    /// A beat at the first return, one every second: in seconds of record.
    pub fn grid(&self, deck: usize) -> Option<(f64, f64)> {
        let record = self.decks[deck].record.as_ref()?;
        let period = record.beat_period.filter(|p| p.is_finite() && *p > 0.05)?;
        Some((record.beat_offset.filter(|o| o.is_finite()).unwrap_or(0.0), period))
    }

    /// Tempo only, by moving this deck's pitch until it matches the other.
    /// Octave-aware, so a record detected at half time is matched rather than
    /// doubled into nonsense. Both grids go to the engine as well, so phase
    /// and quantize have something to work from.
    pub fn sync(&mut self, deck: usize) -> Result<(), String> {
        let other = 1 - deck;
        let mine = self.decks[deck].record.as_ref().and_then(|r| r.bpm)
            .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
            .ok_or("this deck has no detected tempo")?;
        let target = self.decks[other].tempo()
            .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
            .ok_or("the other deck has no detected tempo")?;

        let mut ratio = target / mine;
        if !ratio.is_finite() || ratio <= 0.0 {
            return Err("the detected tempos cannot be matched".into());
        }
        while ratio > 1.35 { ratio /= 2.0; }
        while ratio < 0.74 { ratio *= 2.0; }

        let percent = ((ratio - 1.0) * 100.0) as f32;
        if percent.abs() > 8.0 {
            return Err(format!("{percent:.1}% is past the end of the fader"));
        }
        self.set_pitch(deck, percent);
        self.send_grid(deck);
        self.send_grid(other);
        Ok(())
    }

    /// Put this deck on the same point of its beat as the other one: the
    /// half of sync that tempo alone cannot do.
    pub fn phase_sync(&mut self, deck: usize) -> Result<(), String> {
        let other = 1 - deck;
        if self.grid(deck).is_none() || self.grid(other).is_none() {
            return Err("phase needs a beat grid on both decks".into());
        }
        if !self.decks[deck].playing || !self.decks[other].playing {
            return Err("phase lines up two playing decks".into());
        }
        self.touch(deck);
        self.hold_tempo(deck);
        self.send_grid(deck);
        self.send_grid(other);
        self.send(Command::PhaseAlign { deck, to_deck: other });
        Ok(())
    }

    /// Halve or double a detected tempo.
    ///
    /// The commonest analysis error there is: a record detected at double
    /// time mixes at half speed and every sync against it is nonsense. The
    /// beat period moves with the tempo, or the grid would drift away from
    /// the number beside it.
    pub fn scale_tempo(&mut self, deck: usize, factor: f64) {
        let Some(record) = self.decks[deck].record.as_mut() else { return };
        if let Some(bpm) = record.bpm {
            record.bpm = Some(bpm * factor);
        }
        if let Some(period) = record.beat_period {
            record.beat_period = Some(period / factor);
        }
        self.send_grid(deck);
    }

    /// Back to what the analysis actually found.
    pub fn reset_grid(&mut self, deck: usize) {
        let Some(record) = self.decks[deck].record.as_ref() else { return };
        let key = record.key.clone();
        if let Some(original) = self.records.iter().find(|r| r.key == key).cloned() {
            self.decks[deck].record = Some(original);
        }
        self.send_grid(deck);
    }

    /// Skip by beats where there is a grid, and by a fixed slice where there
    /// is not -- a key that does nothing on an unanalysed record reads as
    /// broken rather than as unavailable.
    pub fn skip(&mut self, deck: usize, beats: f64) {
        let period = self.decks[deck]
            .record
            .as_ref()
            .and_then(|r| r.beat_period)
            .filter(|p| *p > 0.02);
        let by = match period {
            Some(period) => beats * period / (1.0 + self.decks[deck].pitch as f64 / 100.0),
            None => beats.signum() * keys::SKIP_FALLBACK,
        };
        let to = self.decks[deck].position + by;
        self.seek(deck, to);
        self.touch(deck);
    }

    pub fn set_cue(&mut self, deck: usize, slot: usize) {
        if self.decks[deck].record.is_none() || slot >= 4 {
            return;
        }
        let at = self.snap(deck, self.decks[deck].position);
        self.decks[deck].cues[slot] = Some(at);
        self.say(&format!("Deck {} cue {} set", label(deck), slot + 1));
        self.touch(deck);
    }

    pub fn jump_to_cue(&mut self, deck: usize, slot: usize) {
        let Some(at) = self.decks[deck].cues.get(slot).copied().flatten() else {
            // An unset cue is not an error, but silence would look like one.
            self.say(&format!("Deck {} cue {} is not set", label(deck), slot + 1));
            return;
        };
        if self.quantize && self.decks[deck].playing && self.grid(deck).is_some() {
            // On the next beat, landing as far past the cue as the beat was
            // passed: a hot cue that keeps time.
            self.airtime.held.tempo[deck] = true;
            self.send(Command::SeekQuantized { deck, seconds: at });
        } else {
            self.seek(deck, at);
        }
        self.touch(deck);
    }

    /// With quantize on, the nearest beat to `seconds`; otherwise `seconds`.
    fn snap(&self, deck: usize, seconds: f64) -> f64 {
        match self.grid(deck) {
            Some((anchor, period)) if self.quantize => anchor + ((seconds - anchor) / period).round() * period,
            _ => seconds,
        }
    }

    pub fn toggle_quantize(&mut self) {
        self.quantize = !self.quantize;
        self.say(if self.quantize { "Quantize on: cues and loops land on the beat." } else { "Quantize off." });
    }

    /* ── Loops ───────────────────────────────────────────────────────── */

    pub fn set_loop_in(&mut self, deck: usize) {
        if self.decks[deck].record.is_none() {
            return;
        }
        let at = self.snap(deck, self.decks[deck].position);
        self.decks[deck].loop_in = Some(at);
        self.touch(deck);
        self.say(&format!("Deck {} loop in", label(deck)));
    }

    pub fn set_loop_out(&mut self, deck: usize) {
        let Some(start) = self.decks[deck].loop_in else {
            self.say(&format!("Deck {}: set a loop in first", label(deck)));
            return;
        };
        let end = self.snap(deck, self.decks[deck].position);
        if end <= start + 0.01 {
            self.say(&format!("Deck {}: the loop out has to come after the loop in", label(deck)));
            return;
        }
        self.decks[deck].loop_in = None;
        self.enter_loop(deck, start, end);
    }

    /// Loop on or off: out of a loop if in one, else into an auto-loop of
    /// the current length.
    pub fn toggle_loop(&mut self, deck: usize) {
        if self.decks[deck].loop_range.is_some() {
            self.exit_loop(deck);
        } else {
            let beats = self.decks[deck].loop_beats;
            self.auto_loop(deck, beats);
        }
    }

    /// An auto-loop of `beats`, from the beat the playhead is in. Always on
    /// the grid when there is one: a loop that does not start on a beat
    /// stutters.
    pub fn auto_loop(&mut self, deck: usize, beats: u32) {
        if self.decks[deck].record.is_none() {
            return;
        }
        let beats = beats.clamp(1, 16);
        self.decks[deck].loop_beats = beats;
        let position = self.decks[deck].position;
        let (start, period) = match self.grid(deck) {
            Some((anchor, period)) => (anchor + ((position - anchor) / period).floor() * period, period),
            None => (position, FALLBACK_BEAT),
        };
        self.enter_loop(deck, start.max(0.0), start.max(0.0) + beats as f64 * period);
    }

    pub fn halve_loop(&mut self, deck: usize) {
        self.resize_loop(deck, 0.5);
    }

    pub fn double_loop(&mut self, deck: usize) {
        self.resize_loop(deck, 2.0);
    }

    fn resize_loop(&mut self, deck: usize, factor: f64) {
        let beats = if factor < 1.0 { self.decks[deck].loop_beats / 2 } else { self.decks[deck].loop_beats * 2 };
        let beats = beats.clamp(1, 16);
        let changed = beats != self.decks[deck].loop_beats;
        self.decks[deck].loop_beats = beats;
        if let Some((start, end)) = self.decks[deck].loop_range {
            if changed {
                let length = ((end - start) * factor).max(0.01);
                self.enter_loop(deck, start, start + length);
            }
        }
    }

    pub fn exit_loop(&mut self, deck: usize) {
        self.decks[deck].loop_range = None;
        self.send(Command::Loop { deck, range: None });
        self.touch(deck);
    }

    fn enter_loop(&mut self, deck: usize, start: f64, end: f64) {
        let length = self.decks[deck].length;
        let end = if length > 0.0 { end.min(length) } else { end };
        if end <= start {
            return;
        }
        self.decks[deck].loop_range = Some((start, end));
        // A loop takes the record off the station's clock.
        self.hold_tempo(deck);
        self.send(Command::Loop { deck, range: Some((start, end)) });
        self.touch(deck);
    }

    /// Kill a band, or put it back exactly where it was.
    pub fn toggle_kill(&mut self, deck: usize, band: usize) {
        if band >= 3 {
            return;
        }
        self.take_over(Take::Tone(deck, band));
        self.decks[deck].killed[band] = !self.decks[deck].killed[band];
        self.push_tone(deck);
        self.touch(deck);
    }

    pub fn set_bend(&mut self, deck: usize, percent: f32) {
        if (self.decks[deck].bend - percent).abs() < f32::EPSILON {
            return;
        }
        self.decks[deck].bend = percent;
        self.hold_tempo(deck);
        self.apply_speed(deck);
    }

    pub fn toggle_reverse(&mut self, deck: usize) {
        if self.decks[deck].record.is_none() {
            return;
        }
        self.decks[deck].reversed = !self.decks[deck].reversed;
        self.hold_tempo(deck);
        let reversed = self.decks[deck].reversed;
        // Reverse is a scrub rate, which is the same machinery a hand on the
        // platter uses -- there is no second way to run a record backwards.
        let rate = reversed.then(|| -(1.0 + self.decks[deck].pitch as f64 / 100.0));
        self.send(Command::Scrub { deck, rate });
        self.touch(deck);
    }

    pub fn nudge_crossfade(&mut self, direction: f32) {
        let next = (self.crossfade + direction * 0.02).clamp(0.0, 1.0);
        self.set_crossfade(next);
    }

    /// From the keyboard: the crossfader is yours from the first key.
    pub fn set_crossfade(&mut self, value: f32) {
        self.take_over(Take::Crossfade);
        self.crossfade = value.clamp(0.0, 1.0);
        self.push_gains();
    }

    /// The beat echo from the effects rack. The send is opened with it: a
    /// transition may have left the deck's send lane closed.
    pub fn set_echo(&mut self, deck: usize, [mix, feedback, seconds]: [f32; 3]) {
        if let Some(frame) = self.engine.as_ref().map(|e| e.telemetry.frame()) {
            let curve = std::sync::Arc::new(Curve::new(frame, vec![(0.0, 1.0)]));
            self.send(Command::Automate { deck, lane: Lane::EchoSend, curve });
        }
        self.send(Command::Echo { deck, mix, feedback, seconds });
    }

    pub fn set_reverb_send(&mut self, deck: usize, value: f32) {
        self.send(Command::Detach { deck, lane: Lane::ReverbSend });
        self.send(Command::ReverbSend { deck, value: value.clamp(0.0, 1.0) });
    }

    pub fn move_selection(&mut self, by: i32) {
        let rows = ui::filtered(self);
        if rows.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|index| rows.iter().position(|r| *r == index))
            .map_or(0, |position| {
                (position as i32 + by).rem_euclid(rows.len() as i32) as usize
            });
        self.selected = Some(rows[at]);
        self.scroll_to_selection = true;
    }

    pub fn load_selected(&mut self, deck: usize) {
        let Some(index) = self.selected else {
            self.say("Nothing selected in the crate");
            return;
        };
        if let Some(record) = self.records.get(index).cloned() {
            self.load(deck, record);
            self.touch(deck);
        }
    }

    /// How well every record follows what is playing, for the crate.
    ///
    /// The reference deck is whichever one is playing; with both going it is
    /// the one you are mixing *out of*, which is the one the next record has
    /// to follow.
    pub fn reference_deck(&self) -> Option<usize> {
        if let Some((deck, key, _)) = &self.match_reference {
            if self.decks[*deck].record.as_ref().is_some_and(|r| &r.key == key) {
                return Some(*deck);
            }
        }
        self.live_reference_deck()
    }

    /// The reference as things stand this instant, with nothing held.
    fn live_reference_deck(&self) -> Option<usize> {
        let playing: Vec<usize> = (0..DECKS)
            .filter(|d| self.decks[*d].playing && self.decks[*d].record.is_some())
            .collect();
        match playing.as_slice() {
            [only] => Some(*only),
            // Both going: the one the crossfader is favouring is the one on
            // air, so the next record follows it.
            [a, b] => Some(if self.crossfade <= 0.5 { *a } else { *b }),
            _ => (0..DECKS).find(|d| self.decks[*d].record.is_some()),
        }
    }

    /// Hold the match reference through a mix.
    ///
    /// It moves only when its deck stops or takes another record -- never
    /// because the crossfader crossed the middle with both decks running,
    /// which is exactly when you are reading the crate for the next one. Its
    /// tempo follows the pitch fader only once that has moved a real
    /// distance, so an autopilot tempo ramp does not reorder the crate every
    /// frame.
    pub(crate) fn hold_match_reference(&mut self) {
        let held = self.match_reference.as_ref().and_then(|(deck, key, pitch)| {
            let state = &self.decks[*deck];
            let same = state.record.as_ref().is_some_and(|r| &r.key == key);
            (same && (state.playing || !self.decks.iter().any(|d| d.playing)))
                .then_some((*deck, *pitch))
        });
        self.match_reference = match held {
            Some((deck, pitch)) => {
                let now = self.decks[deck].pitch;
                let pitch = if (now - pitch).abs() > 1.0 { now } else { pitch };
                self.decks[deck].record.as_ref().map(|r| (deck, r.key.clone(), pitch))
            }
            None => self.live_reference_deck().and_then(|deck| {
                self.decks[deck].record.as_ref().map(|r| (deck, r.key.clone(), self.decks[deck].pitch))
            }),
        };
    }

    /// What the crate's match column is scored against, for its cache.
    pub fn match_reference_key(&self) -> Option<(usize, String, i64)> {
        let deck = self.reference_deck()?;
        let key = self.decks[deck].record.as_ref()?.key.clone();
        Some((deck, key, (self.reference_pitch(deck) * 10.0).round() as i64))
    }

    fn reference_pitch(&self, deck: usize) -> f32 {
        match &self.match_reference {
            Some((held, _, pitch)) if *held == deck => *pitch,
            _ => self.decks[deck].pitch,
        }
    }

    pub fn fit_for(&self, index: usize) -> Option<assist::Fit> {
        let deck = self.reference_deck()?;
        let playing = self.decks[deck].record.as_ref()?;
        let candidate = self.records.get(index)?;
        if playing.key == candidate.key {
            return None;
        }
        Some(assist::fit(playing, self.reference_pitch(deck), candidate))
    }

    pub fn scrub(&mut self, deck: usize, rate: Option<f64>) {
        self.touch(deck);
        self.hold_tempo(deck);
        self.decks[deck].scrubbing = rate.is_some();
        self.send(Command::Scrub { deck, rate });
    }

    /// Seconds of record across the beat view, from this deck's own tempo, so
    /// one zoom setting means the same number of bars on both decks even when
    /// they are running at different speeds.
    pub fn window_seconds(&self, deck: usize) -> f64 {
        let bpm = self.decks[deck].tempo().unwrap_or(120.0).max(20.0);
        self.bars as f64 * 4.0 * (60.0 / bpm)
    }
}
