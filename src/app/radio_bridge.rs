//! The radio, on the decks: the station's plan becomes deck movement here.

use std::sync::Arc;

use crate::engine::{self, Command, Curve, Lane, DECKS};
use crate::{airtime, Defalt, Take};

impl Defalt {
    /// Keep the station watched, and hand a restarted one the decks.
    pub(crate) fn tick_station(&mut self) {
        self.station.tick();
        if self.station.recoveries != self.recoveries {
            self.recoveries = self.station.recoveries;
            if self.airtime.on {
                // The records on the decks kept playing through the restart;
                // the new session is told what they are and carries on from
                // them.
                let tracks = self.deck_lineup(|app, deck| app.airtime.on_deck(deck).is_some());
                self.airtime.resume_with(serde_json::json!(tracks));
                self.say("The station restarted; the music carried on.");
            }
        }
        // An open settings window picks up the whole settings once they
        // arrive, unless you have started editing.
        if self.mix_settings_open && !self.view_state.mix_dirty
            && self.station.mix_generation != self.mix_generation {
            self.mix_generation = self.station.mix_generation;
            self.mix_settings = self.station.mix_config.clone();
        }
    }

    /// What is on the decks, for the station: key, deck, and how far in.
    fn deck_lineup(&self, include: impl Fn(&Defalt, usize) -> bool) -> Vec<serde_json::Value> {
        let mut order: Vec<usize> = (0..DECKS).collect();
        order.sort_by_key(|d| (!self.decks[*d].playing, *d));
        order.into_iter().filter(|deck| include(self, *deck)).filter_map(|deck| {
            let state = &self.decks[deck];
            if state.loading { return None; }
            let record = state.record.as_ref()?;
            // A finished record is an opening selection, not a zero-length item.
            let offset = if state.position < state.length - 1.0 { state.position } else { 0.0 };
            Some(serde_json::json!({"key": record.key, "deck": deck, "offset": offset}))
        }).collect()
    }

    /// Give the schedule a turn, and perform whatever it asks for.
    ///
    /// Autopilot hands back a plan rather than touching anything itself, and
    /// this is where a plan becomes deck movement: curves for the engine,
    /// starts on exact frames, and the knob positions for the panel to draw.
    pub(crate) fn tick_airtime(&mut self) {
        // Ticking is not only for playing: the queue and the schedule are
        // worth following whenever the station is up, even when you are
        // listening in a browser instead.
        let running = self.station.running();
        if !self.airtime.on && !self.airtime.live() && !running {
            return;
        }
        let Some(telemetry) = self.engine.as_ref().map(|engine| engine.telemetry.clone()) else { return };
        let frame = telemetry.frame();
        self.airtime.station_ready(self.station.ready());
        self.airtime.device(telemetry.device_rate(), telemetry.device_restarts(), frame);
        let status: [airtime::DeckStatus; DECKS] = std::array::from_fn(|deck| {
            // A position read before the last command landed is from before
            // it: feedback must not chase a seek that has not happened yet.
            let fresh = self.telemetry_fresh(deck);
            airtime::DeckStatus {
                loaded: self.decks[deck].record.is_some() && !self.decks[deck].loading,
                playing: self.decks[deck].playing,
                position: fresh.then(|| telemetry.position(deck)),
                playback_rate: 1.0 + (self.decks[deck].pitch + self.decks[deck].bend) as f64 / 100.0,
                base_gain: self.decks[deck].gain * self.decks[deck].trim,
            }
        });

        let records = std::mem::take(&mut self.records);
        let plan = self.airtime.tick(frame, &records, status, running);
        self.records = records;
        self.apply_airtime_plan(plan);
    }

    pub(crate) fn apply_airtime_plan(&mut self, plan: airtime::Plan) {
        for deck in plan.stop {
            // Paused, not reset: whatever it was sending to the echo and
            // the reverb rings out on its own.
            self.send(Command::Pause { deck });
            self.decks[deck].playing = false;
            self.load_generation[deck] = self.load_generation[deck].wrapping_add(1);
            self.decks[deck].loading = false;
        }
        for (deck, record, trim_db) in plan.load {
            self.decks[deck].pitch = 0.0;
            self.decks[deck].bend = 0.0;
            self.apply_speed(deck);
            let trim = 10f32.powf(trim_db / 20.0).clamp(0.05, 4.0);
            let same = !self.decks[deck].loading && self.decks[deck].record.as_ref()
                .is_some_and(|r| r.key == record.key && r.file == record.file);
            if same {
                // Already on the deck: only its level changes hands.
                self.decks[deck].trim = trim;
                self.push_gains();
                let generation = self.load_generation[deck];
                self.airtime.loading(deck, generation);
                self.airtime.deck_ready(deck, &record.key, generation);
            } else {
                self.radio_trim[deck] = Some(trim);
                let generation = self.start_load(deck, record);
                self.airtime.loading(deck, generation);
            }
        }
        for deck in 0..DECKS {
            if let Some(enabled) = plan.key_lock[deck] {
                if self.decks[deck].key_lock != enabled {
                    self.decks[deck].key_lock = enabled;
                    self.send(Command::KeyLock { deck, enabled });
                }
            }
            // The rate lane has the speed already; this is the readout.
            if let Some(rate) = plan.speed[deck] {
                self.decks[deck].pitch = ((rate - 1.0) * 100.0) as f32;
            }
        }
        for (deck, seconds) in plan.start {
            match plan.start_frame[deck] {
                Some(frame) => {
                    self.send(Command::PlayAt { deck, frame, source_seconds: seconds });
                }
                None => {
                    self.seek_audio(deck, seconds);
                    self.send(Command::Play { deck });
                }
            }
            self.decks[deck].position = seconds;
        }
        for (item_id, key) in plan.report_started {
            self.airtime.report_started(&item_id, &key);
        }
        for command in plan.automation {
            self.send(command);
        }
        for (deck, lane) in plan.released {
            self.restore_lane(deck, lane);
        }
        for deck in plan.stems {
            self.begin_cached_split(deck);
        }

        // What the curves are doing, for the panel to draw. The engine is
        // already doing it.
        if let Some(duck) = plan.duck {
            self.music_duck = duck;
        }
        for deck in 0..DECKS {
            if let Some(tone) = plan.tone[deck] {
                for band in 0..4 {
                    if !self.airtime.held.tone[deck][band] {
                        self.decks[deck].tone[band] = tone[band];
                    }
                }
                // The lanes moved the engine's knobs; what was last pushed
                // by hand is no longer what it has.
                self.sent_tone[deck] = None;
            }
            if let Some(level) = plan.level[deck] {
                self.decks[deck].level = level;
            }
        }
        if let Some(crossfade) = plan.crossfade {
            self.crossfade = crossfade;
        }
        self.push_gains();

        // Off air clears every deck's echo line, the rack's included: the
        // rack must send its echo again rather than show one that is gone.
        if plan.voice.iter().any(|c| matches!(c, Command::OffAir)) {
            for fx in self.view_state.fx.iter_mut() {
                fx.forget_sent();
            }
        }
        for command in plan.voice {
            self.send(command);
        }
        if plan.restore {
            self.hand_back();
        }
        if let Some(note) = plan.note {
            self.say(&note);
        }
    }

    /// A lane a written transition has finished with: the control goes back
    /// to where the console has it.
    pub(crate) fn restore_lane(&mut self, deck: usize, lane: Lane) {
        match lane {
            Lane::Gain => {
                self.sent_gain[deck] = None;
                self.push_gains();
            }
            Lane::Level => { self.send(Command::Level { deck, value: 1.0 }); }
            Lane::ReverbSend => {
                let value = self.view_state.fx[deck].reverb;
                self.send(Command::ReverbSend { deck, value });
            }
            Lane::Low | Lane::Mid | Lane::High | Lane::Sweep => {
                self.sent_tone[deck] = None;
                self.push_tone(deck);
            }
            Lane::Rate => self.apply_speed(deck),
            Lane::EchoSend => {
                if let Some(frame) = self.engine.as_ref().map(|e| e.telemetry.frame()) {
                    let curve = Arc::new(Curve::new(frame, vec![(0.0, 1.0)]));
                    self.send(Command::Automate { deck, lane: Lane::EchoSend, curve });
                }
            }
            Lane::EchoFeedback | Lane::EchoBeats => {
                // The rack's own echo if it has one going; otherwise the
                // return stays open, so the tail rings, but at a feedback
                // that lets it die away -- a freeze left behind would ring
                // for ever.
                let seconds = self.decks[deck].record.as_ref().and_then(|r| r.beat_period)
                    .filter(|p| *p > 0.05).unwrap_or(0.5).clamp(0.03, 1.8) as f32;
                let [mix, feedback, seconds] = self.view_state.fx[deck].last_sent()
                    .unwrap_or([airtime::ECHO_RETURN, 0.3, seconds]);
                self.send(Command::Echo { deck, mix, feedback: feedback.min(0.65), seconds });
            }
            _ => {
                if let Some(stem) = lane.stem() {
                    let (value, muted) = (self.stem_gain[deck][stem], self.stem_muted[deck][stem]);
                    self.send(Command::StemGain { deck, stem, value });
                    self.send(Command::StemMute { deck, stem, muted });
                }
            }
        }
    }

    /// Every lane off a deck, and its controls back where the console has
    /// them. `cut_echo` also closes the echo return, which ends a tail --
    /// right for a deck being handed back, wrong for one merely stopping.
    pub(crate) fn reset_lanes(&mut self, deck: usize, cut_echo: bool) {
        for lane in Lane::ALL {
            self.send(Command::ClearAutomation { deck, lane });
        }
        self.send(Command::Level { deck, value: 1.0 });
        let reverb = self.view_state.fx[deck].reverb;
        self.send(Command::ReverbSend { deck, value: reverb });
        // The echo send has no command of its own; a flat curve puts it
        // back open, which is where the effects rack expects it.
        if let Some(frame) = self.engine.as_ref().map(|e| e.telemetry.frame()) {
            let curve = Arc::new(Curve::new(frame, vec![(0.0, 1.0)]));
            self.send(Command::Automate { deck, lane: Lane::EchoSend, curve });
        }
        if cut_echo {
            self.send(Command::Echo { deck, mix: 0.0, feedback: 0.3, seconds: 0.25 });
            self.view_state.fx[deck] = Default::default();
        }
        for stem in 0..engine::deck::STEMS {
            let (value, muted) = (self.stem_gain[deck][stem], self.stem_muted[deck][stem]);
            self.send(Command::StemGain { deck, stem, value });
            self.send(Command::StemMute { deck, stem, muted });
        }
        self.sent_tone[deck] = None;
        self.push_tone(deck);
        self.sent_gain[deck] = None;
        self.decks[deck].level = 1.0;
        self.apply_speed(deck);
    }

    /// The radio has let go of the decks: all of it back to the console.
    fn hand_back(&mut self) {
        self.music_duck = 1.0;
        for deck in 0..DECKS {
            self.decks[deck].gain = 1.0;
            self.reset_lanes(deck, true);
        }
        self.push_gains();
    }

    /// You reached for something the station was driving. It is yours now.
    ///
    /// Only that control: taking the filter mid-transition leaves the bass
    /// swap and the crossfader running, which is the difference between a
    /// console you can play and a switch that says auto or manual.
    pub fn take_over(&mut self, what: Take) {
        match what {
            Take::Tone(deck, _) | Take::Gain(deck) => self.touch(deck),
            Take::Crossfade => {},
        }
        if !self.airtime.on { return; }
        let held = &mut self.airtime.held;
        let already = match what {
            Take::Tone(deck, band) => std::mem::replace(&mut held.tone[deck][band], true),
            Take::Gain(deck) => std::mem::replace(&mut held.gain[deck], true),
            Take::Crossfade => held.crossfade,
        };
        if already {
            return;
        }
        // The lane lets go at once, where it had got to; the next plan
        // leaves it out.
        match what {
            Take::Tone(deck, band) => {
                let lane = [Lane::Low, Lane::Mid, Lane::High, Lane::Sweep][band];
                self.send(Command::Detach { deck, lane });
                self.sent_tone[deck] = None;
            }
            Take::Gain(deck) => {
                self.send(Command::Detach { deck, lane: Lane::Gain });
                self.sent_gain[deck] = None;
            }
            Take::Crossfade => {
                // Each deck keeps the level it has, and the crossfader --
                // sitting where it shows the balance -- carries the balance
                // from here, so nothing jumps.
                let levels: [f32; DECKS] = std::array::from_fn(|deck| self.decks[deck].level);
                self.airtime.take_crossfader(levels);
                let (_, gain) = airtime_levels(levels);
                for deck in 0..DECKS {
                    self.send(Command::Detach { deck, lane: Lane::Level });
                    self.send(Command::Level { deck, value: gain.min(1.0) });
                }
                self.push_gains();
            }
        }
        self.say("Yours. The rest is still on autopilot.");
    }

    /// Hand everything back.
    pub fn return_to_auto(&mut self) {
        self.airtime.return_to_auto();
        self.push_gains();
        self.say("Back on autopilot.");
    }

    pub fn start_radio(&mut self) {
        if let Err(error) = self.station.start() {
            self.say(&error);
            return;
        }
        self.set_radio_playback(true);
    }

    /// Off the decks, and the station asked to shut down properly -- on a
    /// thread of its own, so the panel does not wait for it.
    pub fn stop_radio(&mut self) {
        self.set_radio_playback(false);
        self.remote.stop_tunnel(); // the tunnel goes first
        self.station.stop();
    }

    pub fn set_radio_playback(&mut self, on: bool) {
        if on == self.airtime.on { return; }
        if !on {
            for deck in 0..DECKS {
                if self.airtime.on_deck(deck).is_some() {
                    self.send(Command::Pause { deck });
                    self.decks[deck].playing = false;
                    self.load_generation[deck] = self.load_generation[deck].wrapping_add(1);
                    self.decks[deck].loading = false;
                }
            }
            self.airtime.set_on(false);
            self.send(Command::OffAir);
            self.hand_back();
            return;
        }
        if !self.engine_ready() {
            self.say("Radio needs an audio output to play through the decks.");
            return;
        }
        let tracks = self.deck_lineup(|_, _| true);
        for deck in 0..DECKS {
            self.send(Command::Pause { deck });
            let state = &mut self.decks[deck];
            state.playing = false;
            state.pitch = 0.0;
            state.bend = 0.0;
            state.reversed = false;
            state.scrubbing = false;
            state.killed = [false; 3];
            state.tone = [0.5, 0.5, 0.5, 0.0];
            state.gain = 1.0;
            state.loop_range = None;
            self.send(Command::Loop { deck, range: None });
            self.send(Command::Scrub { deck, rate: None });
            for stem in 0..engine::deck::STEMS {
                self.stem_gain[deck][stem] = 1.0;
                self.stem_muted[deck][stem] = false;
            }
            self.reset_lanes(deck, true);
        }
        self.airtime.start_with(serde_json::json!(tracks));
        self.push_gains();
        self.say("Setting up the opening decks and their transition.");
    }
}

/// The balance and overall level two deck levels come to.
fn airtime_levels(levels: [f32; DECKS]) -> (f32, f32) {
    let gain = levels[0].hypot(levels[1]);
    let crossfade = if gain > 0.000001 { levels[1].atan2(levels[0]) / std::f32::consts::FRAC_PI_2 } else { 0.5 };
    (crossfade, gain)
}
