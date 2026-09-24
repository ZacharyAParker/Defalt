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
mod app;
mod assist;
mod broadcast; // remote listening
mod engine;
mod pull;
mod platform;
mod process;
mod keys;
mod library;
mod lyrics;
mod logfile;
mod reports;
mod peaks;
mod shots;
mod spotify;
mod station;
mod tunnel; // remote listening
mod ui;

use engine::{Command, Engine, DECKS};
use library::Record;
use peaks::Peaks;

pub use platform::local_offset;

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
    /// Auto-loop length in beats: 1, 2, 4, 8 or 16.
    pub loop_beats: u32,
    /// The loop the deck is in, in seconds of record, if it is in one.
    pub loop_range: Option<(f64, f64)>,
    /// A loop-in point set and waiting for its loop-out.
    pub loop_in: Option<f64>,
    /// Auto-gain, from the record's measured loudness -- or, on a record the
    /// station put here, the station's own trim. Kept apart from the channel
    /// fader so neither moves anything you touched.
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
    /// Where the station has this deck's level, for the panel.
    pub level: f32,
}

impl DeckState {
    fn new() -> Self {
        DeckState {
            gain: 1.0,
            tone: [0.5, 0.5, 0.5, 0.0],
            loop_beats: 4,
            trim: 1.0,
            level: 1.0,
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
    /// Remote listening: the stream server and the Cloudflare tunnel.
    pub remote: tunnel::Remote,

    pub records: Vec<Record>,
    /// The library, while it is being read on its own thread.
    library_inbox: Option<mpsc::Receiver<Result<Vec<Record>, String>>>,
    /// The loudest the radio bus was last frame.
    pub air_peak: f32,
    pub host_levels: [f32; 2],
    /// How bright each host's voice is right now (a hiss high, a vowel low).
    pub host_tones: [f32; 2],
    pub studio: ui::studio::Studio,
    pub library_error: Option<String>,
    pub search: String,
    pub sort: (ui::Column, bool),

    pub decks: [DeckState; DECKS],
    pub crossfade: f32,
    pub master: f32,
    pub bars: u32,
    pub master_peak: [f32; 2],
    /// When the master last went over full scale (possible with the limiter
    /// off), for the meter to hold its warning.
    pub over_at: Option<std::time::Instant>,
    pub underruns: u64,
    /// Commands the engine's ring had no room for.
    pub dropped: u64,
    pub device: String,
    pub sample_rate: u32,
    device_restarts: u64,
    /// The master limiter, and how hard it is working (decaying, for the eye).
    pub limiter_on: bool,
    pub limiter_db: f32,
    /// Hot cues and cue jumps land on the next beat.
    pub quantize: bool,

    /// Which record the load buttons act on. Picking a record and choosing a
    /// deck are separate decisions.
    pub selected: Option<usize>,
    pub show_fx: bool,
    pub show_grid: bool,
    pub show_stems: bool,
    pub clock: String,
    /// The minute `clock` was last written for; the string is rebuilt only
    /// when that changes.
    clock_minute: i64,
    /// The deck the crate's match column is scored against, the record on
    /// it, and the pitch it was playing at. Held until that deck stops or
    /// changes record, so the crate does not reshuffle as the crossfader
    /// passes the middle.
    match_reference: Option<(usize, String, f32)>,
    /// The panel's own caches and control state.
    pub view_state: ui::ViewState,

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
    /// How far speech has the music down, for the panel.
    pub music_duck: f32,
    pub transcript_follow: bool,
    pub mix_settings_open: bool,
    pub mix_settings: serde_json::Value,
    /// The station's settings generation the open window was filled from.
    mix_generation: u64,
    /// Which of the two things this window is showing.
    pub view: View,
    /// The duration the catalogue gave for the chosen suggestion, which is
    /// what lets the resolver tell a record from a documentary about it.
    pub pull_duration_ms: Option<u64>,

    load_generation: [u64; DECKS],
    /// The record each deck's load is for, so a failure can say which.
    loading_key: [Option<String>; DECKS],
    /// A separation that may only come from the cache (a technique's
    /// stems), never be made.
    split_cached_only: [bool; DECKS],
    /// The station's trim for a record it is loading onto a deck.
    radio_trim: [Option<f32>; DECKS],
    /// Sequence number of the last transport command per deck. Telemetry
    /// older than it is from before the command, and is not believed.
    pending_seq: [u64; DECKS],
    /// What the engine was last told, so an unchanged value is not sent
    /// again every frame.
    sent_gain: [Option<f32>; DECKS],
    sent_tone: [Option<[f32; 4]>; DECKS],
    recoveries: u64,
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
    /// Seconds east of UTC, asked again each minute so a clock change is
    /// noticed.
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
    /// Booth frames saved so far by a `DEFALT_SHOT_FRAMES` run.
    shot_frames: usize,
    pub scroll_to_selection: bool,
    pub feedback: ui::feedback::Feedback,
    /// Every command sent, by name, for tests that have no audio device.
    #[cfg(test)]
    sent_names: Vec<&'static str>,
}

impl Defalt {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::theme::apply(&cc.egui_ctx);
        Self::from_root(platform::project_root(), true)
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

        let device = engine.as_ref().map_or_else(String::new, |e| e.device.clone());
        let sample_rate = engine.as_ref().map_or(0, |e| e.sample_rate);

        let catalogue = spotify::Search::new(&root);
        let port = station::port_of(&root);
        // One connection pool for everything that talks to the station.
        let client = station::client::Client::new(port);
        let mut app = Defalt {
            engine,
            engine_error,
            remote: Default::default(),
            records: Vec::new(),
            library_inbox: Some(library::load_in_background(&root)),
            air_peak: 0.0,
            host_levels: [0.0; 2],
            host_tones: [0.0; 2],
            studio: ui::studio::Studio::new(&root),
            library_error: None,
            search: String::new(),
            sort: (ui::Column::Artist, true),
            decks: [DeckState::new(), DeckState::new()],
            crossfade: 0.5,
            master: 0.85,
            bars: 8,
            master_peak: [0.0; 2],
            over_at: None,
            underruns: 0,
            dropped: 0,
            device,
            sample_rate,
            device_restarts: 0,
            limiter_on: true,
            limiter_db: 0.0,
            quantize: false,
            selected: None,
            show_fx: false,
            show_grid: false,
            show_stems: false,
            clock: String::new(),
            clock_minute: i64::MIN,
            match_reference: None,
            view_state: ui::ViewState::default(),
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
            airtime: airtime::Airtime::with_client(&root, client.clone(), port, output_rate),
            station: station::Station::with_client(&root, client),
            view: View::Console,
            pull_duration_ms: None,
            inbox,
            outbox,
            load_generation: [0; DECKS],
            loading_key: [None, None],
            split_cached_only: [false; DECKS],
            radio_trim: [None; DECKS],
            pending_seq: [0; DECKS],
            sent_gain: [None; DECKS],
            sent_tone: [None; DECKS],
            recoveries: 0,
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
            mix_generation: 0,
            pose_frame: 0,
            asked_for_shot: false,
            shot_frames: 0,
            scroll_to_selection: false,
            feedback: Default::default(),
            #[cfg(test)]
            sent_names: Vec::new(),
            root,
        };
        app.push_gains();
        app.send(Command::Master { value: app.master });
        // The reverb's return starts at nothing, which makes every send to it
        // silent. A modest room, audible only where something is sent.
        app.send(Command::Reverb { size: 0.72, damping: 0.45, predelay_seconds: 0.02, level: 0.6 });
        app
    }

    /// Hand the engine a command. A full ring drops it; that is counted, and
    /// logged, because a command that silently never arrived looks exactly
    /// like a bug in whatever sent it.
    fn send(&mut self, command: Command) -> Option<u64> {
        let transport = match command {
            Command::Load { deck, .. } | Command::Play { deck } | Command::Pause { deck }
            | Command::Seek { deck, .. } | Command::PlayAt { deck, .. }
            | Command::SeekQuantized { deck, .. } | Command::Loop { deck, .. } => Some(deck),
            _ => None,
        };
        #[cfg(test)]
        self.sent_names.push(match &command {
            Command::Loop { range: Some(_), .. } => "loop",
            Command::Loop { range: None, .. } => "loop off",
            Command::Seek { .. } => "seek",
            Command::SeekQuantized { .. } => "seek quantized",
            Command::Grid { .. } => "grid",
            Command::PhaseAlign { .. } => "phase",
            Command::Detach { .. } => "detach",
            Command::Limiter { .. } => "limiter",
            Command::Echo { .. } => "echo",
            _ => "other",
        });
        let engine = self.engine.as_mut()?;
        match engine.send_seq(command) {
            Ok(seq) => {
                if let Some(deck) = transport.filter(|d| *d < DECKS) {
                    self.pending_seq[deck] = seq;
                }
                Some(seq)
            }
            Err(error) => {
                self.dropped += 1;
                if self.dropped == 1 || self.dropped % 100 == 0 {
                    logfile::log!("engine: dropped a command ({} so far): {error}", self.dropped);
                }
                None
            }
        }
    }

    /// For the panel, which has to be able to take the radio off the air.
    pub fn send_public(&mut self, command: Command) {
        self.send(command);
    }

    pub fn engine_ready(&self) -> bool {
        self.engine.is_some()
    }

    pub fn engine_error(&self) -> Option<&str> {
        self.engine_error.as_deref()
    }

    /// Everything the keyboard reaches. Kept together so the bindings stay a
    /// table of names rather than a second copy of the logic.
    pub fn say(&mut self, message: &str) {
        self.notice = Some((message.to_string(), std::time::Instant::now()));
    }

    pub fn touch(&mut self, deck: usize) {
        self.active_deck = deck;
    }

    /// Wall clock for the toolbar, formatted without pulling in a date crate.
    fn tick_clock(&mut self) {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let local = seconds as i64 + self.utc_offset;
        if local.div_euclid(60) == self.clock_minute {
            return;
        }
        // A new minute: ask the platform again, so a daylight-saving change
        // moves the clock with it.
        self.utc_offset = local_offset();
        let local = seconds as i64 + self.utc_offset;
        self.clock_minute = local.div_euclid(60);
        let minutes = (local / 60).rem_euclid(60);
        let hours24 = (local / 3600).rem_euclid(24);
        let hours = match hours24 % 12 { 0 => 12, h => h };
        let suffix = if hours24 < 12 { "AM" } else { "PM" };
        self.clock = format!("{hours}:{minutes:02} {suffix}");
    }

    fn read_telemetry(&mut self, elapsed: f32) {
        let Some(engine) = self.engine.as_ref() else { return };
        let telemetry = engine.telemetry.clone();

        self.master_peak = telemetry.peak();
        // Measured before the final clamp, so anything past full scale is a
        // real over -- possible only with the limiter off.
        if self.master_peak.iter().any(|p| *p > 1.0) {
            self.over_at = Some(std::time::Instant::now());
        }
        self.limiter_db = telemetry.limiter_reduction_db().max(self.limiter_db - elapsed * 12.0).max(0.0);
        self.air_peak = telemetry.air_peak();
        self.host_levels = self.airtime.host_levels(&telemetry.voice_rms());
        self.host_tones = self.airtime.host_levels(&telemetry.voice_tones());
        self.underruns = telemetry.underruns.load(std::sync::atomic::Ordering::Relaxed);

        let restarts = telemetry.device_restarts();
        if restarts != self.device_restarts {
            self.device_restarts = restarts;
            let rate = telemetry.device_rate();
            if rate > 0 {
                self.sample_rate = rate;
            }
            let khz = if rate % 1000 == 0 { format!("{}", rate / 1000) } else { format!("{:.1}", rate as f64 / 1000.0) };
            logfile::log!("audio: device changed; reconnected at {rate} Hz");
            self.say(&format!("Audio device changed \u{2014} reconnected at {khz} kHz"));
        }

        for deck in 0..DECKS {
            // Read the acknowledgement first: it is published after the
            // position, so a position read after it is at least that new.
            let fresh = telemetry.applied_seq(deck) >= self.pending_seq[deck];
            let state = &mut self.decks[deck];
            if !state.scrubbing && fresh {
                state.position = telemetry.position(deck);
                state.playing = telemetry.playing(deck);
            }
            // The engine reports a rise; the fall belongs to the eye, so the
            // meter decays here rather than snapping to zero between peaks.
            state.meter = telemetry.deck_peak(deck).max(state.meter - elapsed * 1.9);

            if state.playing && !state.scrubbing {
                state.spin = advance_spin(state.spin, elapsed, state.pitch);
            }
        }
    }

    /// Whether this deck's telemetry has caught up with the last command.
    fn telemetry_fresh(&self, deck: usize) -> bool {
        self.engine.as_ref().is_some_and(|engine| engine.telemetry.applied_seq(deck) >= self.pending_seq[deck])
    }
}

impl eframe::App for Defalt {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = std::time::Instant::now();
        let elapsed = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.frame_ms = self.frame_ms * 0.9 + elapsed * 1000.0 * 0.1;

        self.collect_library();
        self.collect_loads();
        self.collect_stems();
        self.poll_pulls();
        self.poll_splits();
        self.catalogue.tick();
        self.airtime.catalogue.tick();
        if self.shot_on_launch.is_none() {
            self.tick_station();
            self.remote.tick(&self.root, self.engine.as_ref().map(|e| &e.telemetry), &self.station);
            self.tick_airtime();
        }
        self.read_telemetry(elapsed);
        self.hold_match_reference();
        self.tick_clock();

        // eframe skips `ui` for minimized/occluded windows. Keep scheduling,
        // station heartbeats, and completed deck loads alive without drawing.
        // Hidden windows are throttled by eframe to one logic tick per 100 ms;
        // the audio callback continues independently at the device sample
        // rate, and so does every transition, which the engine performs from
        // curves sent ahead of time. The tick only has to keep up with the
        // schedule.
        let radio = self.airtime.on || self.airtime.live() || self.station.running();
        ctx.request_repaint_after(std::time::Duration::from_millis(if radio { 16 } else { 100 }));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.screenshots(ui.ctx());
        ui::feedback::show(self, ui.ctx());

        ui::draw(self, ui);

        // Links clicked anywhere on the panel open through the console's own
        // opener, which starts the browser outside the console's job --
        // otherwise closing Defalt would close the browser with it.
        ui.ctx().output_mut(|output| {
            output.commands.retain(|command| match command {
                egui::OutputCommand::OpenUrl(open) => {
                    process::open_url(&open.url);
                    false
                }
                _ => true,
            });
        });

        // Drawn as often as something on it is moving, and no more.
        ui.ctx().request_repaint_after(ui::repaint_after(self, ui.ctx()));
    }

    /// Put the station away properly: its session note, its cache, its
    /// clock. Waiting here is fine; the window is already going.
    fn on_exit(&mut self) {
        self.remote.stop_tunnel(); // the tunnel goes first
        self.station.stop_blocking();
    }
}

/// 1.8 seconds a turn, near enough to 33rpm that muscle memory transfers
/// from a real platter. Kept as an angle inside one turn: an f32 that only
/// ever grows runs out of precision after a few hours of play and the strobe
/// starts to stutter.
pub fn advance_spin(spin: f32, elapsed: f32, pitch: f32) -> f32 {
    (spin + elapsed * std::f32::consts::TAU / 1.8 * (1.0 + pitch / 100.0))
        .rem_euclid(std::f32::consts::TAU)
}

fn label(deck: usize) -> &'static str {
    if deck == 0 { "A" } else { "B" }
}

fn main() -> eframe::Result<()> {
    // Before anything is started, so everything started is contained.
    process::contain_self();
    logfile::init(&platform::project_root());
    let shot = std::env::var_os("DEFALT_SHOT").is_some();
    let size = std::env::var("DEFALT_SHOT_SIZE").ok().and_then(|size| {
        let (w, h) = size.split_once('x')?;
        Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])
    }).filter(|size| shot && size.iter().all(|v| v.is_finite() && *v >= 640. && *v <= 4096.))
        .unwrap_or(if std::env::var_os("DEFALT_SHOT_COMPACT").is_some() { [1024., 640.] } else { [1440., 900.] });
    let viewport = egui::ViewportBuilder::default()
        .with_title("Defalt")
        .with_inner_size(size)
        .with_maximized(!shot || std::env::var_os("DEFALT_SHOT_MAXIMIZED").is_some())
        // Fits a 1080p screen at 150% scaling with the taskbar showing; the
        // bands give up height before anything clips.
        .with_min_inner_size([1024.0, 640.0])
        .with_decorations(false)
        .with_icon(Arc::new(platform::window_icon().unwrap_or_default()));

    eframe::run_native(
        "Defalt",
        eframe::NativeOptions { viewport, ..Default::default() },
        Box::new(|cc| Ok(Box::new(Defalt::new(cc)))),
    )
}

#[cfg(test)]
mod regressions {
    use super::*;

    fn app() -> Defalt {
        let mut app = Defalt::from_root(std::env::temp_dir().join("defalt-no-fixture"), false);
        app.wait_for_library();
        app
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
    fn a_decoder_that_panics_is_a_failed_load_not_a_deck_loading_for_ever() {
        let mut app = app();
        app.start_load(0, Record { file: PathBuf::from("\u{0}not a path"), ..loaded(0, "x").record });
        let began = std::time::Instant::now();
        while app.decks[0].loading && began.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.collect_loads();
        }
        assert!(!app.decks[0].loading);
        assert!(app.decks[0].error.is_some());
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
        plan.load.push((0, record, 0.0));
        plan.tone[0] = Some([0.5, 0.1, 0.5, -0.4]);
        app.apply_airtime_plan(plan);
        assert!(!app.decks[0].loading, "preloaded record was needlessly decoded again");
        assert_eq!(app.load_generation[0], 0);
        assert_eq!(app.decks[0].tone, [0.25, 0.1, 0.5, -0.4]);
    }

    #[test]
    fn the_stations_trim_replaces_assist_rather_than_stacking_on_it() {
        let mut app = app();
        let mut record = loaded(0, "loud").record;
        record.lufs = Some(-8.0);
        let mut plan = airtime::Plan::default();
        plan.load.push((0, record.clone(), -6.0));
        app.apply_airtime_plan(plan);
        let generation = app.load_generation[0];
        app.outbox.send((generation, Ok(Loaded { record, ..loaded(0, "loud") }))).unwrap();
        app.collect_loads();
        assert!((app.decks[0].trim - 10f32.powf(-6.0 / 20.0)).abs() < 1e-4,
                "trim was {} rather than the station's -6 dB", app.decks[0].trim);
        // Loaded by hand, assist measures it itself.
        let mut record = loaded(0, "loud").record;
        record.lufs = Some(-8.0);
        app.load(0, record.clone());
        let generation = app.load_generation[0];
        app.outbox.send((generation, Ok(Loaded { record, ..loaded(0, "loud") }))).unwrap();
        app.collect_loads();
        assert!((app.decks[0].trim - assist::trim_for(Some(-8.0))).abs() < 1e-4);
    }

    #[test]
    fn the_platter_angle_stays_inside_one_turn_after_hours_of_play() {
        let mut spin = 0.0f32;
        // Four hours at 60 frames a second, a little fast.
        for _ in 0..(4 * 3600 * 60) {
            spin = advance_spin(spin, 1.0 / 60.0, 3.0);
        }
        assert!((0.0..std::f32::consts::TAU).contains(&spin), "{spin}");
        // And a frame still turns it by a frame's worth.
        let next = advance_spin(spin, 1.0 / 60.0, 0.0);
        let moved = (next - spin).rem_euclid(std::f32::consts::TAU);
        assert!((moved - std::f32::consts::TAU / 1.8 / 60.0).abs() < 1e-4, "{moved}");
    }

    #[test]
    fn the_match_reference_holds_through_a_mix() {
        let mut app = app();
        app.decks[0].record = Some(loaded(0, "outgoing").record);
        app.decks[1].record = Some(loaded(1, "incoming").record);
        app.decks[0].playing = true;
        app.hold_match_reference();
        assert_eq!(app.reference_deck(), Some(0));
        // Both running, and the crossfader passes the middle: still A.
        app.decks[1].playing = true;
        app.crossfade = 0.9;
        app.hold_match_reference();
        assert_eq!(app.reference_deck(), Some(0), "the crate reshuffled mid-mix");
        // A small tempo ride does not move the scoring tempo; a real one does.
        app.decks[0].pitch = 0.4;
        app.hold_match_reference();
        assert_eq!(app.match_reference_key().unwrap().2, 0);
        app.decks[0].pitch = 2.0;
        app.hold_match_reference();
        assert_eq!(app.match_reference_key().unwrap().2, 20);
        // Once A stops, B is what the next record follows.
        app.decks[0].playing = false;
        app.hold_match_reference();
        assert_eq!(app.reference_deck(), Some(1));
    }

    #[test]
    fn a_tabbed_to_control_leaves_the_keyboard_working() {
        let mut app = app();
        app.decks[0].record = Some(loaded(0, "a").record);
        let ctx = egui::Context::default();
        // A knob has focus, not a text field.
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("some-knob")));
        let press = |key| egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::ALT,
        };
        let input = egui::RawInput { events: vec![press(egui::Key::W)], ..Default::default() };
        ctx.run_ui(input, |ui| keys::handle(&mut app, ui.ctx())).drop_without_applying_deltas();
        assert!(app.decks[0].cues[0].is_some(), "Alt+W did nothing with a control focused");
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

    #[test]
    fn the_keyboard_crossfader_is_yours_even_on_autopilot() {
        let mut app = app();
        app.airtime.set_on(true);
        app.nudge_crossfade(1.0);
        assert!(app.airtime.held.crossfade, "the next autopilot tick would undo the key");
        app.toggle_kill(1, 0);
        assert!(app.airtime.held.tone[1][0], "a kill was overwritten by the station's EQ");
    }

    #[test]
    fn an_auto_loop_lands_on_the_grid_and_halves_and_doubles() {
        let mut app = app();
        app.decks[0].record = Some(Record { beat_offset: Some(0.1), ..loaded(0, "a").record });
        app.decks[0].length = 60.0;
        app.decks[0].position = 10.33;
        app.auto_loop(0, 4);
        let (start, end) = app.decks[0].loop_range.expect("no loop");
        assert!((start - 10.1).abs() < 1e-9, "the loop did not start on a beat: {start}");
        assert!((end - start - 2.0).abs() < 1e-9, "four beats at 120 is two seconds: {}", end - start);
        app.halve_loop(0);
        let (_, end) = app.decks[0].loop_range.unwrap();
        assert!((end - 10.1 - 1.0).abs() < 1e-9);
        assert_eq!(app.decks[0].loop_beats, 2);
        app.double_loop(0);
        app.double_loop(0);
        assert_eq!(app.decks[0].loop_beats, 8);
        app.toggle_loop(0);
        assert!(app.decks[0].loop_range.is_none(), "the toggle did not exit the loop");
    }

    #[test]
    fn a_manual_loop_is_in_then_out_and_quantize_snaps_both() {
        let mut app = app();
        app.decks[0].record = Some(loaded(0, "a").record);
        app.decks[0].length = 60.0;
        app.quantize = true;
        app.decks[0].position = 4.1;
        app.set_loop_in(0);
        app.decks[0].position = 5.9;
        app.set_loop_out(0);
        assert_eq!(app.decks[0].loop_range, Some((4.0, 6.0)));
    }

    #[test]
    fn quantize_makes_a_playing_cue_jump_wait_for_the_beat() {
        let mut app = app();
        app.decks[0].record = Some(loaded(0, "a").record);
        app.decks[0].length = 60.0;
        app.decks[0].cues[0] = Some(8.0);
        app.decks[0].playing = true;
        app.jump_to_cue(0, 0);
        assert_eq!(app.sent_names.last(), Some(&"seek"));
        app.toggle_quantize();
        app.jump_to_cue(0, 0);
        assert_eq!(app.sent_names.last(), Some(&"seek quantized"));
        app.sent_names.clear();
        app.auto_loop(0, 4);
        assert!(app.sent_names.contains(&"loop"));
        app.exit_loop(0);
        assert_eq!(app.sent_names.last(), Some(&"loop off"));
    }

    #[test]
    fn sync_and_phase_give_the_engine_both_grids() {
        let mut app = app();
        app.decks[0].record = Some(loaded(0, "a").record);
        app.decks[1].record = Some(loaded(1, "b").record);
        app.sync(0).unwrap();
        assert_eq!(app.sent_names.iter().filter(|n| **n == "grid").count(), 2);
        assert!(app.phase_sync(0).is_err(), "phase with stopped decks");
        app.decks[0].playing = true;
        app.decks[1].playing = true;
        app.phase_sync(0).unwrap();
        assert_eq!(app.sent_names.last(), Some(&"phase"));
        app.toggle_limiter();
        assert_eq!(app.sent_names.last(), Some(&"limiter"));
        assert!(!app.limiter_on);
    }

    #[test]
    fn off_air_makes_the_rack_send_its_echo_again() {
        let mut app = app();
        app.view_state.fx[1] = ui::racks::Echo::sent_for_test([0.3, 0.3, 0.5]);
        let mut plan = airtime::Plan::default();
        plan.voice.push(Command::OffAir);
        app.apply_airtime_plan(plan);
        assert!(app.view_state.fx[1].last_sent().is_none(), "the rack still thinks its echo is on the deck");
    }

    #[test]
    fn a_finished_freeze_is_let_down_rather_than_left_ringing() {
        let mut app = app();
        app.sent_names.clear();
        app.restore_lane(0, engine::Lane::EchoFeedback);
        assert!(app.sent_names.contains(&"echo"), "the echo was left as the transition had it");
    }

    #[test]
    fn a_manual_load_takes_a_deck_back_from_the_station() {
        let mut app = app();
        app.airtime.set_on(true);
        app.airtime.pose_transition_pair([100.0, 100.0]);
        assert!(app.airtime.on_deck(1).is_some());
        app.load(1, loaded(1, "mine").record);
        assert!(app.airtime.on_deck(1).is_none(), "the station still thinks it has the deck");
    }
}
