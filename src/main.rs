// Defalt -- a DJ application.
//
// One process. The audio engine owns a thread and the output device; the
// console draws from its telemetry and pushes commands back down a lock-free
// ring. There is no IPC, no serialisation and no browser between a hand and a
// record, which is the whole reason this stopped being a web app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

mod airtime;
mod assist;
mod engine;
mod pull;
mod process;
mod keys;
mod library;
mod peaks;
mod spotify;
mod station;
mod ui;

use engine::{Command, Engine, DECKS};
use library::Record;
use peaks::Peaks;

/// A record being decoded, on its way to a deck.
struct Loaded {
    deck: usize,
    record: Record,
    track: Arc<engine::decode::Track>,
    peaks: Arc<Peaks>,
}

/// Generation tags prevent a slow, superseded decode from replacing a newer load.
type Delivery<T> = (u64, Result<T, (usize, String)>);

/// The two things this window can be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Console,
    Radio,
}

/// A control the station was driving that you have just taken.
#[derive(Clone, Copy)]
pub enum Take {
    /// Deck, and which of low/mid/high/sweep.
    Tone(usize, usize),
    Gain(usize),
    Crossfade,
}

#[derive(Default)]
pub struct DeckState {
    pub record: Option<Record>,
    pub peaks: Option<Arc<Peaks>>,
    pub length: f64,
    pub position: f64,
    pub playing: bool,
    pub loading: bool,
    /// Percent, as marked on the fader.
    pub pitch: f32,
    /// Channel fader, before the crossfader.
    pub gain: f32,
    /// low, mid, high, sweep.
    pub tone: [f32; 4],
    pub spin: f32,
    pub meter: f32,
    pub scrubbing: bool,
    pub error: Option<String>,
    /// Loop length in beats. Drawn, not yet honoured by the engine.
    pub loop_beats: u32,
    /// Auto-gain, from the record's measured loudness. Kept apart from the
    /// channel fader so assist can set it without moving anything you touched.
    pub trim: f32,
    /// Four cue points, in seconds.
    pub cues: [Option<f64>; 4],
    /// Which bands are killed, low to high. The knob keeps its own value so
    /// releasing a kill puts it back where you left it.
    pub killed: [bool; 3],
    /// The rate the record was written at. Kept because the deck reads its
    /// stems at the rate it reads the record, so parts that disagree are
    /// parts that play sharp.
    pub sample_rate: u32,
    /// Temporary pitch offset, while a bend key is held.
    pub bend: f32,
    pub reversed: bool,
    pub key_lock: bool,
}

impl DeckState {
    fn new() -> Self {
        DeckState {
            gain: 1.0,
            tone: [0.5, 0.5, 0.5, 0.0],
            loop_beats: 4,
            trim: 1.0,
            ..Default::default()
        }
    }

    pub fn tempo(&self) -> Option<f64> {
        self.record.as_ref().and_then(|r| r.tempo_at(self.pitch as f64))
    }
}

pub struct Defalt {
    root: PathBuf,
    engine: Option<Engine>,
    engine_error: Option<String>,

    pub records: Vec<Record>,
    /// The loudest the radio bus was last frame.
    pub air_peak: f32,
    pub host_levels: [f32; 2],
    pub studio: ui::studio::Studio,
    pub library_error: Option<String>,
    pub search: String,
    pub sort: (ui::Column, bool),

    pub decks: [DeckState; DECKS],
    pub crossfade: f32,
    pub master: f32,
    pub bars: u32,
    pub master_peak: [f32; 2],
    pub underruns: u64,
    pub device: String,
    pub sample_rate: u32,

    /// Which record the load buttons act on. Picking a record and choosing a
    /// deck are separate decisions.
    pub selected: Option<usize>,
    pub show_fx: bool,
    pub show_grid: bool,
    pub show_stems: bool,
    pub fx_manual: bool,
    pub clock: String,

    /// Assist: level matching on load, and the crate ordered by what mixes.
    pub assist: bool,
    /// The deck Space acts on: whichever you touched last.
    pub active_deck: usize,
    pub focus_search: bool,
    pub show_help: bool,
    pub info_page: Option<ui::about::Page>,
    /// A line for the user, and when it stops being worth showing.
    pub notice: Option<(String, std::time::Instant)>,

    /// A record being taken apart, per deck.
    pub splits: [Option<pull::Separation>; DECKS],
    /// Stem faders and mutes, mirrored here so the panel can draw them
    /// without asking the audio thread.
    pub stem_gain: [[f32; engine::deck::STEMS]; DECKS],
    pub stem_muted: [[bool; engine::deck::STEMS]; DECKS],
    pub separated: [bool; DECKS],

    /// Records being pulled in, newest first.
    pub pulls: Vec<pull::Job>,
    pub pull_query: String,
    /// Type-ahead against the catalogue, so a request is a real record.
    /// Named for what it searches: `search` is already the crate's filter.
    pub catalogue: spotify::Search,
    /// The radio, when it is running.
    pub station: station::Station,
    /// The station on this console's own output, rather than in a browser.
    pub airtime: airtime::Airtime,
    pub music_duck: f32,
    pub transcript_follow: bool,
    pub mix_settings_open: bool,
    pub mix_settings: serde_json::Value,
    /// Which of the two things this window is showing.
    pub view: View,
    /// The duration the catalogue gave for the chosen suggestion, which is
    /// what lets the resolver tell a record from a documentary about it.
    pub pull_duration_ms: Option<u64>,

    load_generation: [u64; DECKS],
    inbox: mpsc::Receiver<Delivery<Loaded>>,
    outbox: mpsc::Sender<Delivery<Loaded>>,
    #[allow(clippy::type_complexity)]
    stem_inbox: mpsc::Receiver<
        Delivery<(usize, Box<[Arc<engine::decode::Track>; engine::deck::STEMS]>)>,
    >,
    #[allow(clippy::type_complexity)]
    stem_outbox: mpsc::Sender<
        Delivery<(usize, Box<[Arc<engine::decode::Track>; engine::deck::STEMS]>)>,
    >,
    last_frame: std::time::Instant,
    pub frame_ms: f32,
    /// Seconds east of UTC, read once from the platform.
    utc_offset: i64,

    /// Self-portraits. `DEFALT_SHOT=<png>` in the environment takes one once
    /// the panel has settled and then quits; F12 takes one any time into
    /// `target/`. This exists because a console cannot be judged from a
    /// description of it, and nobody else can see this window.
    shot_on_launch: Option<PathBuf>,
    frames: u64,
    shots: u32,
    posed: bool,
    pose_frame: u64,
    asked_for_shot: bool,
    pub scroll_to_selection: bool,
}

impl Defalt {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::theme::apply(&cc.egui_ctx);
        Self::from_root(project_root(), true)
    }

    fn from_root(root: PathBuf, audio: bool) -> Self {
        let (outbox, inbox) = mpsc::channel();
        let (stem_outbox, stem_inbox) = mpsc::channel();

        let (engine, engine_error) = match if audio { Engine::start() } else { Err("Audio disabled".into()) } {
            Ok(engine) => {
                println!("audio: {} at {} Hz", engine.device, engine.sample_rate);
                (Some(engine), None)
            }
            // No device is not a reason to refuse to start: the library still
            // browses and the analysis is all still there to look at.
            Err(error) => {
                eprintln!("audio: {error}");
                (None, Some(error))
            }
        };
        // Taken before the struct literal moves the engine. Playout is
        // scheduled in output frames, so it needs the device's rate.
        let output_rate = engine.as_ref().map_or(48_000, |engine| engine.sample_rate);

        let (records, library_error) = match library::load(&root) {
            Ok(records) => (records, None),
            Err(error) => (Vec::new(), Some(error)),
        };

        let device = engine.as_ref().map_or_else(String::new, |e| e.device.clone());
        let sample_rate = engine.as_ref().map_or(0, |e| e.sample_rate);

        let catalogue = spotify::Search::new(&root);
        let root_for_station = root.clone();
        let mut app = Defalt {
            root,
            engine,
            engine_error,
            records,
            air_peak: 0.0,
            host_levels: [0.0; 2],
            studio: ui::studio::Studio::new(&root_for_station),
            library_error,
            search: String::new(),
            sort: (ui::Column::Artist, true),
            decks: [DeckState::new(), DeckState::new()],
            crossfade: 0.5,
            master: 0.85,
            bars: 8,
            master_peak: [0.0; 2],
            underruns: 0,
            device,
            sample_rate,
            selected: None,
            show_fx: false,
            show_grid: false,
            show_stems: false,
            fx_manual: true,
            clock: String::new(),
            assist: true,
            active_deck: 0,
            focus_search: false,
            show_help: false,
            info_page: None,
            notice: None,
            splits: [None, None],
            stem_gain: [[1.0; engine::deck::STEMS]; DECKS],
            stem_muted: [[false; engine::deck::STEMS]; DECKS],
            separated: [false; DECKS],
            pulls: Vec::new(),
            pull_query: String::new(),
            catalogue,
            airtime: airtime::Airtime::new(
                &root_for_station,
                station::port_of(&root_for_station),
                output_rate,
            ),
            station: station::Station::new(&root_for_station),
            view: View::Console,
            pull_duration_ms: None,
            inbox,
            outbox,
            load_generation: [0; DECKS],
            stem_inbox,
            stem_outbox,
            last_frame: std::time::Instant::now(),
            frame_ms: 0.0,
            utc_offset: local_offset(),
            shot_on_launch: std::env::var_os("DEFALT_SHOT").map(PathBuf::from),
            frames: 0,
            shots: 0,
            posed: false,
            music_duck: 1.0,
            transcript_follow: true,
            mix_settings_open: false,
            mix_settings: serde_json::Value::Null,
            pose_frame: 0,
            asked_for_shot: false,
            scroll_to_selection: false,
        };
        app.push_gains();
        app.send(Command::Master { value: app.master });
        app
    }

    fn send(&mut self, command: Command) {
        if let Some(engine) = self.engine.as_mut() {
            let _ = engine.send(command);
        }
    }

    /// For the panel, which has to be able to take the radio off the air.
    pub fn send_public(&mut self, command: Command) {
        self.send(command);
    }

    pub fn engine_ready(&self) -> bool {
        self.engine.is_some()
    }

    /// Give the schedule a turn, and perform whatever it asks for.
    ///
    /// Autopilot hands back a plan rather than touching anything itself, and
    /// this is where a plan becomes deck movement. Everything it writes goes
    /// through the same fields a hand would move, which is why the panel shows
    /// a transition happening instead of only hearing one.
    fn tick_airtime(&mut self) {
        // Ticking is not only for playing: the queue and the schedule are
        // worth following whenever the station is up, even when you are
        // listening in a browser instead.
        let running = self.station.running();
        if !self.airtime.on && !self.airtime.live() && !running {
            return;
        }
        let frame = match self.engine.as_ref() {
            Some(engine) => engine.telemetry.frame(),
            None => return,
        };
        let status: [airtime::DeckStatus; DECKS] = std::array::from_fn(|deck| {
            airtime::DeckStatus {
                loaded: self.decks[deck].record.is_some() && !self.decks[deck].loading,
                playing: self.decks[deck].playing,
                position: self.engine.as_ref().map(|engine| engine.telemetry.position(deck)),
                playback_rate: 1.0 + (self.decks[deck].pitch + self.decks[deck].bend) as f64 / 100.0,
            }
        });

        let records = std::mem::take(&mut self.records);
        let plan = self.airtime.tick(frame, &records, status, running);
        self.records = records;
        self.apply_airtime_plan(plan);
    }

    fn apply_airtime_plan(&mut self, plan: airtime::Plan) {
        for deck in plan.stop {
            self.send(Command::Pause { deck });
            self.send(Command::Echo { deck, mix: 0.0, feedback: 0.0, seconds: 0.25 });
            self.decks[deck].playing = false;
            self.load_generation[deck] = self.load_generation[deck].wrapping_add(1);
            self.decks[deck].loading = false;
        }
        for (deck, record) in plan.load {
            self.decks[deck].pitch = 0.0;
            self.decks[deck].bend = 0.0;
            self.apply_speed(deck);
            if !self.decks[deck].loading && self.decks[deck].record.as_ref()
                .is_some_and(|r| r.key == record.key && r.file == record.file) {
                self.airtime.deck_ready(deck);
            } else {
                self.load(deck, record);
            }
        }
        for deck in 0..DECKS {
            if let Some(enabled) = plan.key_lock[deck] {
                if self.decks[deck].key_lock != enabled {
                    self.decks[deck].key_lock = enabled;
                    self.send(Command::KeyLock { deck, enabled });
                }
            }
            if let Some(rate) = plan.speed[deck] {
                self.decks[deck].pitch = ((rate - 1.0) * 100.0) as f32;
                self.apply_speed(deck);
            }
        }
        for (deck, seconds) in plan.start {
            self.seek_audio(deck, seconds);
            self.send(Command::Play { deck });
            self.decks[deck].playing = true;
        }
        for (item_id, key) in plan.report_started {
            self.airtime.report_started(&item_id, &key);
        }

        let mut gains_moved = false;
        if let Some(duck) = plan.duck {
            if self.music_duck != duck { self.music_duck = duck; gains_moved = true; }
        }
        for deck in 0..DECKS {
            if let Some([mix, feedback, seconds]) = plan.echo[deck] {
                self.send(Command::Echo { deck, mix, feedback, seconds });
            }
            if let Some(mut tone) = plan.tone[deck] {
                for (band, value) in tone.iter_mut().enumerate() {
                    if self.airtime.held.tone[deck][band] {
                        *value = self.decks[deck].tone[band];
                    }
                }
                if self.decks[deck].tone != tone {
                    self.decks[deck].tone = tone;
                    self.push_tone(deck);
                }
            }
            if let Some(gain) = plan.gain[deck] {
                if self.decks[deck].gain != gain {
                    self.decks[deck].gain = gain;
                    gains_moved = true;
                }
            }
        }
        if let Some(crossfade) = plan.crossfade {
            if self.crossfade != crossfade {
                self.crossfade = crossfade;
                gains_moved = true;
            }
        }
        if gains_moved {
            self.push_gains();
        }

        for command in plan.voice {
            self.send(command);
        }
        if let Some(note) = plan.note {
            self.say(&note);
        }
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
            Take::Crossfade => std::mem::replace(&mut held.crossfade, true),
        };
        if !already {
            self.say("Yours. The rest is still on autopilot.");
        }
    }

    /// Hand everything back.
    pub fn return_to_auto(&mut self) {
        self.airtime.held.release();
        self.say("Back on autopilot.");
    }

    pub fn engine_error(&self) -> Option<&str> {
        self.engine_error.as_deref()
    }

    pub fn start_radio(&mut self) {
        if let Err(error) = self.station.start() {
            self.say(&error);
            return;
        }
        self.set_radio_playback(true);
    }

    pub fn stop_radio(&mut self) {
        self.set_radio_playback(false);
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
            self.music_duck = 1.0;
            self.send(Command::OffAir);
            for deck in 0..DECKS {
                self.decks[deck].gain = 1.0;
                self.send(Command::Echo { deck, mix: 0.0, feedback: 0.0, seconds: 0.25 });
            }
            self.push_gains();
            return;
        }
        if !self.engine_ready() {
            self.say("Radio needs an audio output to play through the decks.");
            return;
        }
        let mut order: Vec<usize> = (0..DECKS).collect();
        order.sort_by_key(|d| (!self.decks[*d].playing, *d));
        let tracks: Vec<_> = order.into_iter().filter_map(|deck| {
            let state = &self.decks[deck];
            if state.loading { return None; }
            let record = state.record.as_ref()?;
            // A finished record is an opening selection, not a zero-length item.
            let offset = if state.position < state.length - 1.0 { state.position } else { 0.0 };
            Some(serde_json::json!({"key": record.key, "deck": deck, "offset": offset}))
        }).collect();
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
            state.gain = 0.0;
            self.apply_speed(deck);
            self.send(Command::Scrub { deck, rate: None });
            for stem in 0..engine::deck::STEMS {
                self.stem_gain[deck][stem] = 1.0;
                self.stem_muted[deck][stem] = false;
                self.send(Command::StemGain { deck, stem, value: 1.0 });
                self.send(Command::StemMute { deck, stem, muted: false });
            }
            self.push_tone(deck);
        }
        self.push_gains();
        self.airtime.start_with(serde_json::json!(tracks));
        self.say("Setting up the opening decks and their transition.");
    }

    /// Decode off the UI thread. A five minute record takes a moment, and the
    /// console has to keep drawing while it happens.
    pub fn load(&mut self, deck: usize, record: Record) {
        if deck >= DECKS {
            return;
        }
        self.decks[deck].loading = true;
        self.decks[deck].error = None;
        self.load_generation[deck] = self.load_generation[deck].wrapping_add(1);
        let generation = self.load_generation[deck];
        self.splits[deck] = None;
        self.touch(deck);

        let outbox = self.outbox.clone();
        let path = record.file.clone();
        std::thread::spawn(move || {
            let message = match engine::decode::load(&path) {
                Ok(track) => {
                    let peaks = Arc::new(peaks::analyse(&track));
                    Ok(Loaded { deck, record, track, peaks })
                }
                Err(error) => Err((deck, error)),
            };
            let _ = outbox.send((generation, message));
        });
    }

    fn collect_loads(&mut self) {
        while let Ok((generation, message)) = self.inbox.try_recv() {
            let index = match &message { Ok(loaded) => loaded.deck, Err((deck, _)) => *deck };
            if generation != self.load_generation[index] { continue; }
            match message {
                Ok(loaded) => {
                    let deck = &mut self.decks[loaded.deck];
                    deck.length = loaded.track.seconds();
                    deck.record = Some(loaded.record);
                    deck.peaks = Some(loaded.peaks);
                    deck.position = 0.0;
                    deck.playing = false;
                    deck.loading = false;
                    deck.error = None;
                    deck.cues = [None; 4];
                    deck.killed = [false; 3];
                    deck.bend = 0.0;
                    deck.reversed = false;
                    deck.scrubbing = false;
                    // Levels matched before the record comes in, which is the
                    // whole of assist's first job.
                    let index_for_stems = loaded.deck;
                    deck.trim = if self.assist {
                        assist::trim_for(deck.record.as_ref().and_then(|r| r.lufs))
                    } else {
                        1.0
                    };
                    let index = loaded.deck;
                    deck.sample_rate = loaded.track.sample_rate;
                    self.separated[index_for_stems] = false;
                    self.splits[index_for_stems] = None;
                    self.stem_gain[index_for_stems] = [1.0; engine::deck::STEMS];
                    self.stem_muted[index_for_stems] = [false; engine::deck::STEMS];
                    self.send(Command::Load { deck: index, track: loaded.track });
                    self.apply_speed(index);
                    self.push_tone(index);
                    self.push_gains();
                    self.airtime.deck_ready(index);
                }
                Err((deck, error)) => {
                    self.decks[deck].loading = false;
                    self.decks[deck].error = Some(error);
                    self.airtime.deck_failed(deck);
                }
            }
        }
    }

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

    fn seek_audio(&mut self, deck: usize, seconds: f64) {
        let length = self.decks[deck].length;
        let seconds = seconds.clamp(0.0, length);
        self.decks[deck].position = seconds;
        self.send(Command::Seek { deck, seconds });
    }

    pub fn set_pitch(&mut self, deck: usize, percent: f32) {
        self.touch(deck);
        self.airtime.held.tempo[deck] = true;
        self.decks[deck].pitch = percent;
        self.apply_speed(deck);
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
    fn apply_speed(&mut self, deck: usize) {
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
        self.send(Command::Tone { deck, low, mid, high, sweep });
    }

    /// The channel faders and the crossfader are one number per deck by the
    /// time the engine sees them. Equal power, so the middle is not a dip.
    pub fn push_gains(&mut self) {
        let a = (self.crossfade * std::f32::consts::FRAC_PI_2).cos();
        let b = ((1.0 - self.crossfade) * std::f32::consts::FRAC_PI_2).cos();
        let curve = [a, b];
        for deck in 0..DECKS {
            let value = self.decks[deck].gain * self.decks[deck].trim * curve[deck] * self.music_duck;
            self.send(Command::Gain { deck, value });
        }
    }

    pub fn set_master(&mut self, value: f32) {
        self.master = value;
        self.send(Command::Master { value });
    }

    /// Tempo only, by moving this deck's pitch until it matches the other.
    /// Octave-aware, so a record detected at half time is matched rather than
    /// doubled into nonsense. Phase alignment wants a grid we trust on both
    /// records and is the next piece, not this one.
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
    }

    /// Back to what the analysis actually found.
    pub fn reset_grid(&mut self, deck: usize) {
        let Some(record) = self.decks[deck].record.as_ref() else { return };
        let key = record.key.clone();
        if let Some(original) = self.records.iter().find(|r| r.key == key).cloned() {
            self.decks[deck].record = Some(original);
        }
    }

    /// Everything the keyboard reaches. Kept together so the bindings stay a
    /// table of names rather than a second copy of the logic.
    pub fn say(&mut self, message: &str) {
        self.notice = Some((message.to_string(), std::time::Instant::now()));
    }

    /// Start a pull, and rescan when one finishes.
    pub fn begin_pull(&mut self) {
        let query = self.pull_query.trim().to_string();
        if query.is_empty() {
            return;
        }
        match pull::start(&self.root, &query, self.pull_duration_ms) {
            Ok(job) => {
                self.pulls.insert(0, job);
                self.pull_query.clear();
                self.pull_duration_ms = None;
                self.catalogue.clear();
            }
            Err(error) => self.say(&error),
        }
    }

    /// Take a suggestion: its exact name goes in the box, and its length goes
    /// to the resolver.
    pub fn take_suggestion(&mut self, at: usize) {
        let Some(found) = self.catalogue.showing.get(at).cloned() else { return };
        self.pull_query = found.query();
        self.pull_duration_ms = Some(found.duration_ms).filter(|ms| *ms > 0);
        self.catalogue.clear();
    }

    fn poll_pulls(&mut self) {
        let mut arrived = false;
        for job in self.pulls.iter_mut() {
            let was_over = job.stage.is_over();
            job.poll();
            if !was_over && matches!(job.stage, pull::Stage::Done { .. }) {
                arrived = true;
            }
        }
        if arrived {
            // The importer wrote a row; the crate has to be told.
            self.reload_library();
            self.say("Pulled in. It is in your music folder.");
        }
        // Finished jobs are worth keeping on screen for a moment, not
        // forever: the crate is where a record lives once it has arrived.
        self.pulls.retain(|job| {
            !job.stage.is_over() || job.started.elapsed().as_secs() < 25
        });
    }

    /// Take the record on a deck apart, if it is not already in pieces.
    pub fn begin_split(&mut self, deck: usize) {
        if self.decks[deck].loading || self.splits[deck].is_some() || self.separated[deck] {
            return;
        }
        let Some(record) = self.decks[deck].record.as_ref() else {
            self.say("Load a record first.");
            return;
        };
        let file = record.file.clone();
        match pull::separate(&self.root, deck, &file) {
            Ok(job) => self.splits[deck] = Some(job),
            Err(error) => self.say(&error),
        }
    }

    pub fn splitting(&self, deck: usize) -> Option<&pull::Separation> {
        self.splits[deck].as_ref()
    }

    pub fn set_stem_gain(&mut self, deck: usize, stem: usize, value: f32) {
        self.stem_gain[deck][stem] = value.clamp(0.0, 1.0);
        let value = self.stem_gain[deck][stem];
        self.send(Command::StemGain { deck, stem, value });
    }

    pub fn toggle_stem_mute(&mut self, deck: usize, stem: usize) {
        let muted = !self.stem_muted[deck][stem];
        self.stem_muted[deck][stem] = muted;
        self.send(Command::StemMute { deck, stem, muted });
    }

    fn poll_splits(&mut self) {
        for deck in 0..DECKS {
            let Some(job) = self.splits[deck].as_mut() else { continue };
            job.poll();
            match job.stage.clone() {
                pull::Split::Done { parts, .. } => {
                    self.splits[deck] = None;
                    self.load_parts(deck, parts);
                }
                pull::Split::Failed { error } => {
                    self.splits[deck] = None;
                    self.say(&error);
                }
                pull::Split::Working { .. } => {}
            }
        }
    }

    /// Decode the four parts off-thread, then hand them over together.
    ///
    /// Together, because half a separation is worse than none: three stems
    /// playing while the fourth is still decoding is the record with a hole
    /// in it.
    fn load_parts(&mut self, deck: usize, parts: pull::Parts) {
        let outbox = self.stem_outbox.clone();
        let generation = self.load_generation[deck];
        std::thread::spawn(move || {
            let mut decoded = Vec::with_capacity(engine::deck::STEMS);
            for path in parts.in_order() {
                match engine::decode::load(path) {
                    Ok(track) => decoded.push(track),
                    Err(error) => {
                        let _ = outbox.send((generation, Err((deck, error))));
                        return;
                    }
                }
            }
            let parts: [Arc<engine::decode::Track>; engine::deck::STEMS] =
                decoded.try_into().map_err(|_| ()).expect("four parts");
            let _ = outbox.send((generation, Ok((deck, Box::new(parts)))));
        });
    }

    fn collect_stems(&mut self) {
        while let Ok((generation, message)) = self.stem_inbox.try_recv() {
            let deck = match &message { Ok((deck, _)) | Err((deck, _)) => *deck };
            if generation != self.load_generation[deck] || self.decks[deck].loading { continue; }
            match message {
                Ok((deck, parts)) => {
                    // A deck reads its parts at the rate it reads the record,
                    // so parts written at another rate play sharp and drift.
                    // Separation is meant to hand back the same audio taken
                    // apart; anything else is not that, and is refused rather
                    // than played a semitone and a half up.
                    let expected = self.decks[deck].sample_rate;
                    if let Some(part) = parts.iter().find(|p| p.sample_rate != expected) {
                        self.separated[deck] = false;
                        self.say(&format!(
                            "Deck {}: the parts came back at {} Hz for a {} Hz record.                              Separate it again.",
                            label(deck), part.sample_rate, expected));
                        continue;
                    }
                    self.separated[deck] = true;
                    self.stem_gain[deck] = [1.0; engine::deck::STEMS];
                    self.stem_muted[deck] = [false; engine::deck::STEMS];
                    self.send(Command::Stems { deck, parts });
                    self.say("Separated.");
                }
                Err((deck, error)) => {
                    self.separated[deck] = false;
                    self.say(&format!("Deck {}: {error}", label(deck)));
                }
            }
        }
    }

    pub fn can_pull(&self) -> bool {
        pull::python(&self.root).is_some()
    }

    pub fn touch(&mut self, deck: usize) {
        self.active_deck = deck;
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
        let at = self.decks[deck].position;
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
        self.seek(deck, at);
        self.touch(deck);
    }

    /// Kill a band, or put it back exactly where it was.
    pub fn toggle_kill(&mut self, deck: usize, band: usize) {
        if band >= 3 {
            return;
        }
        self.decks[deck].killed[band] = !self.decks[deck].killed[band];
        self.push_tone(deck);
        self.touch(deck);
    }

    pub fn set_bend(&mut self, deck: usize, percent: f32) {
        if (self.decks[deck].bend - percent).abs() < f32::EPSILON {
            return;
        }
        self.decks[deck].bend = percent;
        self.airtime.held.tempo[deck] = true;
        self.apply_speed(deck);
    }

    pub fn toggle_reverse(&mut self, deck: usize) {
        if self.decks[deck].record.is_none() {
            return;
        }
        self.decks[deck].reversed = !self.decks[deck].reversed;
        self.airtime.held.tempo[deck] = true;
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

    pub fn set_crossfade(&mut self, value: f32) {
        self.crossfade = value.clamp(0.0, 1.0);
        self.push_gains();
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

    pub fn fit_for(&self, index: usize) -> Option<assist::Fit> {
        let deck = self.reference_deck()?;
        let playing = self.decks[deck].record.as_ref()?;
        let candidate = self.records.get(index)?;
        if playing.key == candidate.key {
            return None;
        }
        Some(assist::fit(playing, self.decks[deck].pitch, candidate))
    }

    pub fn scrub(&mut self, deck: usize, rate: Option<f64>) {
        self.touch(deck);
        self.airtime.held.tempo[deck] = true;
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

    pub fn reload_library(&mut self) {
        let selected_key = self.selected.and_then(|i| self.records.get(i)).map(|r| r.key.clone());
        match library::load(&self.root) {
            Ok(records) => {
                self.records = records;
                self.selected = selected_key.and_then(|key| self.records.iter().position(|r| r.key == key));
                self.library_error = None;
            }
            Err(error) => self.library_error = Some(error),
        }
    }

    fn screenshots(&mut self, ctx: &egui::Context) {
        self.frames += 1;

        // Capture the real local library without starting playback, network
        // searches, stem separation, or a station just to take a screenshot.
        let posing = self.shot_on_launch.is_some();
        if posing && self.frames == 5 {
            if std::env::var_os("DEFALT_SHOT_EMPTY").is_none() {
                for deck in 0..self.records.len().min(2) {
                    self.load(deck, self.records[deck].clone());
                }
            }
            if std::env::var_os("DEFALT_SHOT_RADIO").is_some() { self.view = View::Radio; }
            if let Ok(page) = std::env::var("DEFALT_SHOT_INFO") {
                self.info_page = ui::about::Page::from_name(&page);
            }
            if std::env::var_os("DEFALT_SHOT_ARTICLE").is_some() {
                self.airtime.request_is_article = true;
                self.airtime.article = "https://www.mindstudio.ai/blog/gemini-4-release-date-rumors".into();
                self.view = View::Radio;
            }
            if let Some(path) = std::env::var_os("DEFALT_SHOT_STATUS") {
                if let Ok(body) = std::fs::read(path).ok().and_then(|s| serde_json::from_slice(&s).ok()).ok_or(()) {
                    let status = station::on_air_from(&body);
                    self.mix_settings = status.mix_config.clone();
                    self.station.health = station::Health::Live(Box::new(status));
                    self.view = View::Radio;
                    self.mix_settings_open = std::env::var_os("DEFALT_SHOT_SETTINGS").is_some();
                }
            }
            if std::env::var_os("DEFALT_SHOT_RACKS").is_some() {
                self.show_grid = true;
                self.show_stems = true;
            }
        }
        let settled = self.decks.iter().all(|d| !d.loading);
        if posing && settled && self.frames > 5 && !self.posed {
            self.posed = true;
            self.pose_frame = self.frames;
            for deck in 0..2 { self.seek(deck, self.decks[deck].length * 0.25); }
            if std::env::var_os("DEFALT_SHOT_TRANSITIONS").is_some()
                && self.decks.iter().all(|d| d.length > 16.0) {
                let lengths = [self.decks[0].length, self.decks[1].length];
                self.airtime.pose_transition_pair(lengths);
                self.seek(0, lengths[0] - 12.0);
                self.seek(1, 0.0);
            }
        }
        // A missing decoder or empty library cannot hold capture open forever.
        let ready = posing && ((self.posed && self.frames >= self.pose_frame + 12) || self.frames >= 600);
        let launch_shot = ready && !self.asked_for_shot;
        if launch_shot {
            self.asked_for_shot = true;
        }
        let manual = ctx.input(|i| i.key_pressed(egui::Key::F12));
        if launch_shot || manual {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }

        let shots: Vec<Arc<egui::ColorImage>> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
                .collect()
        });

        for image in shots {
            let path = match self.shot_on_launch.take() {
                Some(path) => path,
                None => {
                    self.shots += 1;
                    self.root.join("target").join(format!("shot-{}.png", self.shots))
                }
            };
            match save_png(&image, &path) {
                Ok(()) => println!("screenshot: {}", path.display()),
                Err(error) => eprintln!("screenshot failed: {error}"),
            }
            if std::env::var_os("DEFALT_SHOT").is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Wall clock for the toolbar, formatted without pulling in a date crate.
    fn tick_clock(&mut self) {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let local = seconds as i64 + self.utc_offset;
        let minutes = (local / 60).rem_euclid(60);
        let hours24 = (local / 3600).rem_euclid(24);
        let hours = match hours24 % 12 { 0 => 12, h => h };
        let suffix = if hours24 < 12 { "AM" } else { "PM" };
        self.clock = format!("{hours}:{minutes:02} {suffix}");
    }

    fn read_telemetry(&mut self, elapsed: f32) {
        let Some(engine) = self.engine.as_ref() else { return };
        let telemetry = &engine.telemetry;

        self.master_peak = telemetry.peak();
        self.air_peak = telemetry.air_peak();
        self.host_levels = self.airtime.host_levels(&telemetry.voice_peaks());
        self.underruns = telemetry.underruns.load(std::sync::atomic::Ordering::Relaxed);

        for deck in 0..DECKS {
            let state = &mut self.decks[deck];
            if !state.scrubbing {
                state.position = telemetry.position(deck);
                state.playing = telemetry.playing(deck);
            }
            // The engine reports a rise; the fall belongs to the eye, so the
            // meter decays here rather than snapping to zero between peaks.
            state.meter = telemetry.deck_peak(deck).max(state.meter - elapsed * 1.9);

            if state.playing && !state.scrubbing {
                // 1.8 seconds a turn, near enough to 33rpm that muscle memory
                // transfers from a real platter.
                state.spin += elapsed * std::f32::consts::TAU / 1.8
                    * (1.0 + state.pitch / 100.0);
            }
        }
    }
}

impl eframe::App for Defalt {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = std::time::Instant::now();
        let elapsed = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.frame_ms = self.frame_ms * 0.9 + elapsed * 1000.0 * 0.1;

        self.collect_loads();
        self.collect_stems();
        self.poll_pulls();
        self.poll_splits();
        self.catalogue.tick();
        self.airtime.catalogue.tick();
        if self.shot_on_launch.is_none() {
            self.station.tick();
            self.tick_airtime();
        }
        self.read_telemetry(elapsed);
        self.tick_clock();

        // eframe skips `ui` for minimized/occluded windows. Keep scheduling,
        // station heartbeats, and completed deck loads alive without drawing.
        // Hidden windows are throttled by eframe to one logic tick per 100 ms;
        // the audio callback continues independently at the device sample rate.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.screenshots(ui.ctx());

        ui::draw(self, ui);

        // A mixer is never idle: meters fall, platters turn, waveforms move.
        ui.ctx().request_repaint();
    }
}

fn save_png(image: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(path, &rgba, width as u32, height as u32, image::ColorType::Rgba8)
        .map_err(|error| error.to_string())
}

fn label(deck: usize) -> &'static str {
    if deck == 0 { "A" } else { "B" }
}

/// Seconds east of UTC. Windows answers this without a date library.
#[cfg(windows)]
fn local_offset() -> i64 {
    use std::mem::zeroed;
    #[allow(non_snake_case)]
    #[repr(C)]
    struct TimeZoneInformation {
        Bias: i32,
        StandardName: [u16; 32],
        StandardDate: [u16; 8],
        StandardBias: i32,
        DaylightName: [u16; 32],
        DaylightDate: [u16; 8],
        DaylightBias: i32,
    }
    extern "system" {
        fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
    }
    unsafe {
        let mut info: TimeZoneInformation = zeroed();
        let result = GetTimeZoneInformation(&mut info);
        // 0 unknown, 1 standard, 2 daylight; the bias is minutes *west*.
        let extra = match result {
            2 => info.DaylightBias,
            _ => info.StandardBias,
        };
        -((info.Bias + extra) as i64) * 60
    }
}

#[cfg(not(windows))]
fn local_offset() -> i64 {
    0
}

/// Walk up from the executable for the project, so a debug build run from
/// anywhere still finds the library and the station.
fn project_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(current) = dir {
            if current.join("radio").join("__main__.py").is_file()
                || current.join("cache").join("station.db").is_file()
            {
                return current;
            }
            dir = current.parent().map(|p| p.to_path_buf());
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn main() -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("Defalt")
        .with_inner_size(if std::env::var_os("DEFALT_SHOT_COMPACT").is_some() { [1180.0, 720.0] } else { [1440.0, 900.0] })
        .with_min_inner_size([1180.0, 720.0])
        .with_decorations(false)
        .with_icon(Arc::new(window_icon().unwrap_or_default()));

    eframe::run_native(
        "Defalt",
        eframe::NativeOptions { viewport, ..Default::default() },
        Box::new(|cc| Ok(Box::new(Defalt::new(cc)))),
    )
}

/// The window icon.
///
/// Decoded rather than drawn: the artwork is a real asset now, and the image
/// crate is already here for screenshots, so this costs nothing new. The
/// executable gets the same icon stamped into its resource table by build.rs,
/// which is what Explorer and the taskbar read -- neither asks the running
/// process what it would like to look like.
fn window_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../icons/icon.png");
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData { rgba: image.into_raw(), width, height })
}

#[cfg(test)]
mod regressions {
    use super::*;

    fn app() -> Defalt {
        Defalt::from_root(std::env::temp_dir().join("defalt-no-fixture"), false)
    }

    fn loaded(deck: usize, key: &str) -> Loaded {
        let track = Arc::new(engine::decode::Track { samples: vec![0.1; 200], sample_rate: 48_000 });
        Loaded {
            deck,
            record: Record {
                key: key.into(), title: key.into(), artist: "Test artist".into(),
                album: None, duration: Some(1.0), bpm: Some(120.0), camelot: None,
                lufs: None, file: PathBuf::new(), beat_offset: None,
                beat_period: Some(0.5), downbeat_offset: None,
            },
            peaks: Arc::new(peaks::analyse(&track)), track,
        }
    }

    fn background_input(minimized: bool, occluded: bool) -> egui::RawInput {
        let mut input = egui::RawInput { focused: false, ..Default::default() };
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.focused = Some(false);
        viewport.minimized = Some(minimized);
        viewport.occluded = Some(occluded);
        input
    }

    #[test]
    fn background_logic_keeps_accepting_deck_loads_without_drawing() {
        // Exercise the same logic-only path eframe uses for hidden windows.
        // No UI pass or focus event is allowed to rescue a stalled update.
        for (minimized, occluded) in [(false, false), (true, false), (false, true)] {
            let mut app = app();
            let ctx = egui::Context::default();
            let mut frame = eframe::Frame::_new_kittest();
            let input = background_input(minimized, occluded);
            for generation in 1..=3 {
                let key = format!("background-track-{generation}");
                app.load_generation[0] = generation;
                app.decks[0].loading = true;
                app.outbox.send((generation, Ok(loaded(0, &key)))).unwrap();
                let _ = ctx.run_logic(&input, |ctx| eframe::App::logic(&mut app, ctx, &mut frame));
                assert_eq!(app.decks[0].record.as_ref().unwrap().key, key);
                assert!(!app.decks[0].loading);
            }
        }
    }

    #[test]
    fn background_logic_schedules_every_next_tick_without_a_ui_pass() {
        let mut app = app();
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let input = background_input(true, false);
        let (send, receive) = mpsc::channel();
        ctx.set_request_repaint_callback(move |info| { send.send(info.delay).unwrap(); });
        for _ in 0..5 {
            let _ = ctx.run_logic(&input, |ctx| eframe::App::logic(&mut app, ctx, &mut frame));
            let delays: Vec<_> = receive.try_iter().collect();
            assert!(delays.iter().any(|delay| *delay <= std::time::Duration::from_millis(100)),
                "background work failed to schedule its next wake");
        }
    }

    #[test]
    fn newer_track_wins_even_when_old_decode_finishes_last() {
        let mut app = app();
        app.load_generation[0] = 2;
        app.outbox.send((2, Ok(loaded(0, "new")))).ok().unwrap();
        app.outbox.send((1, Ok(loaded(0, "old")))).ok().unwrap();
        app.outbox.send((1, Err((0, "stale failure".into())))).ok().unwrap();
        app.collect_loads();
        assert_eq!(app.decks[0].record.as_ref().unwrap().key, "new");
        assert!(app.decks[0].error.is_none());
        assert!(!app.decks[0].loading);
    }

    #[test]
    fn old_stems_cannot_attach_to_a_replacement_track() {
        let mut app = app();
        app.load_generation[0] = 2;
        app.decks[0].sample_rate = 48_000;
        let track = loaded(0, "old").track;
        app.stem_outbox.send((1, Ok((0, Box::new(std::array::from_fn(|_| track.clone())))))).ok().unwrap();
        app.collect_stems();
        assert!(!app.separated[0]);
    }

    #[test]
    fn invalid_tempos_are_rejected_without_hanging_sync() {
        let mut app = app();
        app.decks[0].record = Some(loaded(0, "a").record);
        app.decks[1].record = Some(loaded(1, "b").record);
        for bpm in [0.0, -1.0, f64::INFINITY, f64::NAN, f64::MIN_POSITIVE] {
            app.decks[0].record.as_mut().unwrap().bpm = Some(bpm);
            assert!(app.sync(0).is_err());
        }
    }

    #[test]
    fn focusing_search_releases_held_pitch_bends() {
        let mut app = app();
        app.decks[0].bend = 4.0;
        let ctx = egui::Context::default();
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("search")));
        keys::handle(&mut app, &ctx);
        assert_eq!(app.decks[0].bend, 0.0);
    }

    #[test]
    fn disabled_chips_never_dispatch_clicks() {
        for live in [false, true] {
            let ctx = egui::Context::default();
            let mut hits = 0;
            let mut at = egui::Pos2::ZERO;
            for pressed in [None, Some(true), Some(false)] {
                let events = pressed.map_or_else(Vec::new, |pressed| vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
                ]);
                ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                    let response = ui::chip(ui, "Action", egui::vec2(80.0, 30.0), false, live);
                    at = response.rect.center();
                    hits += usize::from(response.clicked());
                }).drop_without_applying_deltas();
            }
            assert_eq!(hits, usize::from(live));
        }
    }


    #[test]
    fn radio_reuses_preloaded_audio_and_preserves_individually_held_eq() {
        let mut app = app();
        let record = loaded(0, "opening").record;
        app.decks[0].record = Some(record.clone());
        app.decks[0].tone = [0.25, 0.5, 0.5, 0.0];
        app.airtime.held.tone[0][0] = true;
        let mut plan = airtime::Plan::default();
        plan.load.push((0, record));
        plan.tone[0] = Some([0.5, 0.1, 0.5, -0.4]);
        app.apply_airtime_plan(plan);
        assert!(!app.decks[0].loading, "preloaded record was needlessly decoded again");
        assert_eq!(app.load_generation[0], 0);
        assert_eq!(app.decks[0].tone, [0.25, 0.1, 0.5, -0.4]);
    }

    #[test]
    fn tempo_reset_clears_pitch_and_bend_without_moving_the_playhead() {
        let mut app = app();
        app.decks[0].pitch = 5.0;
        app.decks[0].bend = -2.0;
        app.decks[0].position = 42.0;
        app.reset_tempo(0);
        assert_eq!(app.decks[0].pitch, 0.0);
        assert_eq!(app.decks[0].bend, 0.0);
        assert_eq!(app.decks[0].position, 42.0);
    }
}
