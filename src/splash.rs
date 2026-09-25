//! The startup ident: the OBBY STUDIO logo in a small borderless window,
//! then the console, maximized, coming up out of the ident's background.
//!
//! Two windows, not one that resizes. The console's window is made at its
//! full size from the start and kept cloaked (drawn, but not shown) while
//! the ident plays in a small window of its own; at the end both are the
//! ident's background colour, the console is uncloaked under the splash, the
//! splash closes, and the background lifts off the console. Resizing one
//! window from 800 points to the whole screen was tried first: on a
//! DirectX surface every resize is a frame or two of black (or of the old
//! picture in a corner) before the next frame lands at the new size, and
//! no ordering of the resize commands got rid of it.
//!
//! This is the part with no window in it -- the preference, which launches
//! get the ident, the frames blob, the clock and the order things happen in
//! -- so all of it can be tested without a screen. `ui::splash` draws it.
//!
//! The frames come from tools/build-splash.py: still WebP frames rather than
//! a video, because the image crate can read those and nothing here wants a
//! video decoder for six seconds of logo.

use std::path::Path;

/// The ident's frames, packed by tools/build-splash.py.
pub const FRAMES: &[u8] = include_bytes!("../assets/splash/frames.bin");
/// Its sound, 16-bit 48 kHz stereo, already at about -16 LUFS.
pub const SOUND: &[u8] = include_bytes!("../assets/splash/sound.wav");

/// The window the ident plays in, in points. The picture is 5:4-ish
/// (1344x1080 once the pillarbox is cropped off) and its frames are 1.5x
/// this, so a 150% screen gets them pixel for pixel.
pub const WIDTH: f32 = 800.0;

/// The ident's own background, sampled from its edges. Both windows are this
/// colour at the moment one replaces the other.
pub const BACKGROUND: [u8; 3] = [20, 24, 30];

/// How long the final logo shows on a launch that doesn't get the whole
/// ident (once a day, after the first; or reduced motion).
pub const FLASH: f64 = 1.0;
/// The ident fading into its background before the console comes up.
pub const OUTRO: f64 = 0.18;
/// The sound fading when the ident is skipped.
pub const SKIP_FADE: f64 = 0.15;
/// The longest the splash stays over the uncloaked console before it goes
/// anyway.
pub const GROW_WAIT: f64 = 0.8;
/// Frames the console draws, uncloaked but still under the splash, before
/// the splash closes: long enough for its first picture to be on screen.
pub const SETTLE_FRAMES: u32 = 2;
/// The cover lifting off the console.
pub const REVEAL: f64 = 0.25;
/// How long the final logo waits for the console to be ready before it goes
/// anyway. Nothing on the list takes this long unless something is wrong,
/// and a wrong thing is better seen in the console than behind a logo.
pub const HOLD_MAX: f64 = 4.0;

/* ── The preference ─────────────────────────────────────────────────── */

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Every,
    Daily,
    Off,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Every, Mode::Daily, Mode::Off];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Every => "Every launch",
            Mode::Daily => "Once a day",
            Mode::Off => "Off",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Mode::Every => "every",
            Mode::Daily => "daily",
            Mode::Off => "off",
        }
    }

    fn from_key(key: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.key() == key)
    }
}

/// Kept in cache/console-layout.json beside the library's share.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prefs {
    pub mode: Mode,
    pub sound: bool,
    /// The local day (days since 1970) the whole ident last played.
    pub last_day: Option<i64>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { mode: Mode::Every, sound: true, last_day: None }
    }
}

impl Prefs {
    pub fn load(root: &Path) -> Self {
        let saved = crate::ui::read_layout(root);
        Prefs {
            mode: saved["startup_video"].as_str().and_then(Mode::from_key).unwrap_or_default(),
            sound: saved["startup_sound"].as_bool().unwrap_or(true),
            last_day: saved["startup_day"].as_i64(),
        }
    }

    pub fn save(&self, root: &Path) {
        crate::ui::write_layout(root, serde_json::json!({
            "startup_video": self.mode.key(),
            "startup_sound": self.sound,
            "startup_day": self.last_day,
        }));
    }
}

/// Today, as days since 1970 in local time.
pub fn today() -> i64 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    (seconds + crate::local_offset()).div_euclid(86_400)
}

/// Whether reduced motion is on, read straight from the booth's own
/// preferences so the decision can be made before the window exists.
pub fn reduced_motion(root: &Path) -> bool {
    std::fs::read(root.join("cache/studio-preferences.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|saved| saved["reduced"].as_bool() == Some(true))
}

/* ── Which launches get what ────────────────────────────────────────── */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The whole ident, with its sound if that's on.
    Full,
    /// The final logo, still, for about a second. Silent.
    Flash,
    /// Straight to the console.
    Skip,
}

/// What this launch shows. `shot` is a screenshot run that didn't ask for
/// the splash: those capture the console exactly as they always have.
pub fn plan(prefs: &Prefs, today: i64, reduced: bool, shot: bool) -> Plan {
    if shot {
        return Plan::Skip;
    }
    match prefs.mode {
        Mode::Off => Plan::Skip,
        _ if reduced => Plan::Flash,
        Mode::Daily if prefs.last_day == Some(today) => Plan::Flash,
        Mode::Every | Mode::Daily => Plan::Full,
    }
}

/* ── The frames ─────────────────────────────────────────────────────── */

/// The blob's header and index. Frames are borrowed from it, not copied.
pub struct Frames {
    blob: &'static [u8],
    pub count: usize,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    index: Vec<(usize, usize)>,
}

impl Frames {
    pub fn parse(blob: &'static [u8]) -> Result<Frames, String> {
        let word = |at: usize| -> Result<u32, String> {
            blob.get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .ok_or_else(|| "the splash blob is cut short".to_string())
        };
        if blob.get(..4) != Some(b"DSPL".as_slice()) {
            return Err("not a splash blob".into());
        }
        if word(4)? != 1 {
            return Err(format!("splash blob version {}", word(4)?));
        }
        let count = word(8)? as usize;
        let (width, height) = (word(12)?, word(16)?);
        let (num, den) = (word(20)?, word(24)?);
        if count == 0 || width == 0 || height == 0 || num == 0 || den == 0 {
            return Err("the splash blob is empty".into());
        }
        let mut index = Vec::with_capacity(count);
        for n in 0..count {
            let (offset, length) = (word(28 + 8 * n)? as usize, word(32 + 8 * n)? as usize);
            if offset.checked_add(length).is_none_or(|end| end > blob.len()) {
                return Err(format!("splash frame {n} runs past the end"));
            }
            index.push((offset, length));
        }
        Ok(Frames { blob, count, width, height, fps: num as f64 / den as f64, index })
    }

    pub fn bytes(&self, n: usize) -> &'static [u8] {
        let (offset, length) = self.index[n.min(self.count - 1)];
        &self.blob[offset..offset + length]
    }

    pub fn seconds(&self) -> f64 {
        self.count as f64 / self.fps
    }

    pub fn last(&self) -> usize {
        self.count - 1
    }

    /// The frame showing `seconds` into the ident.
    pub fn at(&self, seconds: f64) -> usize {
        frame_index(seconds, self.fps, self.count)
    }

    /// The window's content size, in points.
    pub fn window_size(&self) -> [f32; 2] {
        [WIDTH, (WIDTH * self.height as f32 / self.width as f32).round()]
    }
}

pub fn frame_index(seconds: f64, fps: f64, count: usize) -> usize {
    if count == 0 || !seconds.is_finite() {
        return 0;
    }
    ((seconds.max(0.0) * fps).floor() as usize).min(count - 1)
}

/* ── The clock ──────────────────────────────────────────────────────── */

/// How far into the ident it is. With its sound playing through the engine,
/// that's the engine's own output clock, so the picture keeps time with
/// what you hear; without sound (or once the device stops answering), the
/// wall clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clock {
    /// The sound is on its way. The ident waits on its first frame, but only
    /// for `WAIT` seconds before it goes without it.
    Waiting { since: f64 },
    /// Output frame `start` is the ident's first sample.
    Engine { start: u64, rate: u32, restarts: u64, last_frame: u64, last_moved: f64 },
    /// Wall-clock second `origin` is the ident's start.
    Wall { origin: f64 },
}

/// What the engine says, read once a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineTime {
    pub frame: u64,
    pub rate: u32,
    pub restarts: u64,
}

impl Clock {
    /// The longest the ident waits for its sound to decode.
    pub const WAIT: f64 = 0.3;
    /// An output clock that hasn't moved in this long has stopped; the
    /// picture carries on without it.
    pub const STALL: f64 = 0.25;

    pub fn engine(start: u64, time: EngineTime, now: f64) -> Clock {
        Clock::Engine { start, rate: time.rate, restarts: time.restarts, last_frame: time.frame, last_moved: now }
    }

    /// Seconds into the ident: negative before it starts. `now` is wall
    /// seconds; `engine` is the engine's clock, if there is an engine.
    pub fn played(&mut self, now: f64, engine: Option<EngineTime>) -> f64 {
        match *self {
            Clock::Waiting { since } => {
                if now - since >= Self::WAIT {
                    *self = Clock::Wall { origin: now };
                }
                0.0
            }
            Clock::Wall { origin } => now - origin,
            Clock::Engine { start, rate, restarts, last_frame, last_moved } => {
                let played = engine_seconds(last_frame, start, rate);
                let Some(time) = engine else {
                    *self = Clock::Wall { origin: now - played.max(0.0) };
                    return played;
                };
                // A reopened device counts on at its own rate, and a stalled
                // one doesn't count at all: carry on from where it got to.
                let stalled = time.frame == last_frame && now - last_moved >= Self::STALL;
                if time.restarts != restarts || time.rate != rate || stalled {
                    *self = Clock::Wall { origin: now - played.max(0.0) };
                    return played;
                }
                if time.frame != last_frame {
                    *self = Clock::Engine { start, rate, restarts, last_frame: time.frame, last_moved: now };
                }
                engine_seconds(time.frame, start, rate)
            }
        }
    }
}

/// Seconds from output frame `start` to output frame `now`.
pub fn engine_seconds(now: u64, start: u64, rate: u32) -> f64 {
    if rate == 0 {
        return 0.0;
    }
    (now as f64 - start as f64) / rate as f64
}

/* ── The order of things ────────────────────────────────────────────── */

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// The ident playing (or the final logo, for a flash).
    Intro,
    /// Finished, and waiting on the console: the final logo and a line
    /// saying what it's waiting for.
    Hold,
    /// The ident fading into its background.
    Outro,
    /// The console uncloaked under the splash, both all background.
    Grow,
    /// The console drawn, and the background lifting off it.
    Reveal,
    Done,
}

/// What the machine wants done, once, when it moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Fade the sound out now (a skip).
    FadeSound,
    /// Uncloak the console's window, under the splash.
    Grow,
    /// Forget the splash.
    Finish,
}

pub struct Machine {
    pub phase: Phase,
    since: f64,
    /// Seconds the intro runs: the ident, or a flash.
    length: f64,
    grown_frames: u32,
    pub skipped: bool,
    /// Held on the moment it was skipped or finished, so a fade is a fade of
    /// the frame you were looking at.
    frozen: Option<f64>,
}

impl Machine {
    pub fn new(length: f64, now: f64) -> Machine {
        Machine { phase: Phase::Intro, since: now, length, grown_frames: 0, skipped: false, frozen: None }
    }

    fn enter(&mut self, phase: Phase, now: f64) {
        self.phase = phase;
        self.since = now;
    }

    /// Seconds the intro runs.
    pub fn length(&self) -> f64 {
        self.length
    }

    /// Seconds in the current phase.
    pub fn elapsed(&self, now: f64) -> f64 {
        (now - self.since).max(0.0)
    }

    /// Where the ident is for drawing: live while it plays, then held.
    pub fn position(&self, played: f64) -> f64 {
        self.frozen.unwrap_or(played)
    }

    /// Click, Esc, Space or Enter. Only the ident and the hold can be
    /// skipped; once the window is going, it's going.
    pub fn skip(&mut self, now: f64, played: f64, ready: bool) -> Vec<Action> {
        if !matches!(self.phase, Phase::Intro | Phase::Hold) {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if self.phase == Phase::Intro {
            actions.push(Action::FadeSound);
            self.skipped = true;
        }
        if ready {
            self.frozen.get_or_insert(played.clamp(0.0, self.length));
            self.enter(Phase::Outro, now);
        } else {
            // Not ready: skip to the final logo and wait there.
            self.frozen = Some(self.length);
            self.enter(Phase::Hold, now);
        }
        actions
    }

    /// Move on if it's time. `grown` is the console's window uncloaked and
    /// drawing.
    pub fn step(&mut self, now: f64, played: f64, ready: bool, grown: bool) -> Vec<Action> {
        let elapsed = self.elapsed(now);
        match self.phase {
            Phase::Intro if played >= self.length => {
                self.frozen = Some(self.length);
                self.enter(if ready { Phase::Outro } else { Phase::Hold }, now);
            }
            Phase::Hold if ready || elapsed >= HOLD_MAX => self.enter(Phase::Outro, now),
            Phase::Outro if elapsed >= OUTRO => {
                self.enter(Phase::Grow, now);
                return vec![Action::Grow];
            }
            Phase::Grow => {
                self.grown_frames = if grown { self.grown_frames + 1 } else { 0 };
                if self.grown_frames > SETTLE_FRAMES || elapsed >= GROW_WAIT {
                    self.enter(Phase::Reveal, now);
                }
            }
            Phase::Reveal if elapsed >= REVEAL => {
                self.enter(Phase::Done, now);
                return vec![Action::Finish];
            }
            _ => {}
        }
        Vec::new()
    }

    /// Whether the console's window is still just background: until it is
    /// uncloaked, there's no one to draw the console for.
    pub fn owns_window(&self) -> bool {
        matches!(self.phase, Phase::Intro | Phase::Hold | Phase::Outro)
    }

    /// Whether the splash's own window is up: until the console under it
    /// has had its first frames on screen.
    pub fn splash_window(&self) -> bool {
        matches!(self.phase, Phase::Intro | Phase::Hold | Phase::Outro | Phase::Grow)
    }

    /// How much of the ident shows over its background, 0..1.
    pub fn picture(&self, now: f64) -> f32 {
        match self.phase {
            Phase::Intro | Phase::Hold => 1.0,
            Phase::Outro => 1.0 - ease((self.elapsed(now) / OUTRO) as f32),
            _ => 0.0,
        }
    }

    /// How much the background covers the console, 0..1.
    pub fn cover(&self, now: f64) -> f32 {
        match self.phase {
            Phase::Reveal => 1.0 - ease((self.elapsed(now) / REVEAL) as f32),
            Phase::Done => 0.0,
            _ => 1.0,
        }
    }
}

/// Exponential-ish ease-out: quick to move, gentle to land.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/* ── Where the splash goes ────────────────────────────────────────── */

/// The splash window's top-left, in points: the middle of the (maximized,
/// cloaked) console window, which is the middle of the screen it will
/// open on. Failing that, the middle of the monitor; failing that, nowhere
/// yet, and the splash waits a frame for the console to know where it is.
pub fn centre(size: [f32; 2], console: Option<egui::Rect>, monitor: Option<egui::Vec2>) -> Option<egui::Pos2> {
    let middle = match (console, monitor) {
        (Some(rect), _) if rect.width() > 0.0 && rect.height() > 0.0 => rect.center(),
        (_, Some(monitor)) if monitor.x > 0.0 && monitor.y > 0.0 => (monitor / 2.0).to_pos2(),
        _ => return None,
    };
    Some((middle - egui::vec2(size[0], size[1]) / 2.0).round())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blob_parses_and_has_the_whole_ident() {
        let frames = Frames::parse(FRAMES).unwrap();
        assert_eq!(frames.count, 145);
        assert_eq!((frames.width, frames.height), (1200, 964));
        assert_eq!(frames.fps, 24.0);
        assert!((frames.seconds() - 6.0417).abs() < 0.001);
        for n in [0, 72, frames.last()] {
            let bytes = frames.bytes(n);
            assert_eq!(&bytes[..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WEBP");
        }
        let [w, h] = frames.window_size();
        assert_eq!(w, 800.0);
        assert!((w / h - 1200.0 / 964.0).abs() < 0.01, "{w}x{h}");
    }

    #[test]
    fn a_frame_decodes_to_its_size() {
        let frames = Frames::parse(FRAMES).unwrap();
        let image = image::load_from_memory_with_format(frames.bytes(frames.last()), image::ImageFormat::WebP)
            .unwrap();
        assert_eq!((image.width(), image.height()), (1200, 964));
        // The corner is the ident's background.
        let corner = image.to_rgb8().get_pixel(4, 4).0;
        for (got, want) in corner.iter().zip(BACKGROUND) {
            assert!((*got as i32 - want as i32).abs() <= 4, "{corner:?}");
        }
    }

    #[test]
    fn the_sound_is_as_long_as_the_picture() {
        let track = crate::engine::decode::load_bytes(SOUND, "wav", 0).unwrap();
        assert_eq!(track.sample_rate, 48_000);
        let frames = Frames::parse(FRAMES).unwrap();
        assert!((track.seconds() - frames.seconds()).abs() < 0.03, "{}", track.seconds());
    }

    #[test]
    fn a_damaged_blob_is_refused() {
        assert!(Frames::parse(b"nope").is_err());
        let short: &'static [u8] = &FRAMES[..40];
        assert!(Frames::parse(short).is_err());
        let mut lying = FRAMES[..28 + 8].to_vec();
        lying[8..12].copy_from_slice(&1u32.to_le_bytes());
        lying[28..32].copy_from_slice(&0u32.to_le_bytes());
        lying[32..36].copy_from_slice(&1_000u32.to_le_bytes());
        let lying: &'static [u8] = Box::leak(lying.into_boxed_slice());
        assert!(Frames::parse(lying).is_err());
    }

    #[test]
    fn frames_follow_the_clock() {
        assert_eq!(frame_index(-0.5, 24.0, 145), 0);
        assert_eq!(frame_index(0.0, 24.0, 145), 0);
        assert_eq!(frame_index(0.0417, 24.0, 145), 1);
        assert_eq!(frame_index(3.0, 24.0, 145), 72);
        assert_eq!(frame_index(60.0, 24.0, 145), 144);
        assert_eq!(frame_index(f64::NAN, 24.0, 145), 0);
        assert_eq!(engine_seconds(48_000 + 24_000, 48_000, 48_000), 0.5);
        assert!(engine_seconds(47_000, 48_000, 48_000) < 0.0, "before the start is before the start");
        assert_eq!(engine_seconds(10, 0, 0), 0.0);
    }

    #[test]
    fn the_engine_clock_drives_the_picture_and_hands_over_when_it_stops() {
        let time = |frame| Some(EngineTime { frame, rate: 48_000, restarts: 0 });
        let mut clock = Clock::engine(4_800, time(0).unwrap(), 0.0);
        assert!(clock.played(0.0, time(0)) < 0.0);
        assert_eq!(clock.played(0.2, time(4_800 + 48_000)), 1.0);
        // The wall clock says 2 s have gone, but the picture follows the sound.
        assert_eq!(clock.played(2.0, time(4_800 + 72_000)), 1.5);
        // The device stops answering: the picture carries on from 1.5 s.
        assert_eq!(clock.played(2.1, time(4_800 + 72_000)), 1.5);
        assert_eq!(clock.played(2.3, time(4_800 + 72_000)), 1.5);
        assert!(matches!(clock, Clock::Wall { .. }));
        assert!((clock.played(2.8, time(4_800 + 72_000)) - 2.0).abs() < 1e-9);

        // A reopened device hands over at once.
        let mut clock = Clock::engine(0, time(0).unwrap(), 0.0);
        clock.played(0.5, time(24_000));
        clock.played(0.6, Some(EngineTime { frame: 24_000, rate: 44_100, restarts: 1 }));
        assert!(matches!(clock, Clock::Wall { .. }));
        assert!((clock.played(1.1, None) - 1.0).abs() < 1e-9);

        // No sound in time: the wall clock, from when it gave up.
        let mut clock = Clock::Waiting { since: 0.0 };
        assert_eq!(clock.played(0.1, None), 0.0);
        assert_eq!(clock.played(Clock::WAIT, None), 0.0);
        assert!((clock.played(Clock::WAIT + 1.0, None) - 1.0).abs() < 1e-9);
    }

    /// Runs a machine on a fake clock at 60 frames a second until it
    /// finishes, and says what it did when.
    fn run(machine: &mut Machine, ready_at: f64, grown_at: f64, skip_at: Option<f64>) -> Vec<(Action, f64)> {
        let mut log = Vec::new();
        let mut now = 0.0;
        while machine.phase != Phase::Done && now < 30.0 {
            let ready = now >= ready_at;
            if skip_at.is_some_and(|at| now >= at && now < at + 1.0 / 60.0) {
                log.extend(machine.skip(now, now, ready).into_iter().map(|a| (a, now)));
            }
            let grown = log.iter().any(|(a, _)| *a == Action::Grow) && now >= grown_at;
            log.extend(machine.step(now, now, ready, grown).into_iter().map(|a| (a, now)));
            now += 1.0 / 60.0;
        }
        log
    }

    #[test]
    fn plays_then_grows_then_reveals() {
        let mut machine = Machine::new(6.04, 0.0);
        assert!(machine.owns_window() && machine.splash_window());
        let log = run(&mut machine, 1.0, 0.0, None);
        let grow = log.iter().find(|(a, _)| *a == Action::Grow).unwrap().1;
        assert!((grow - (6.04 + OUTRO)).abs() < 0.05, "grew at {grow}");
        let finish = log.iter().find(|(a, _)| *a == Action::Finish).unwrap().1;
        assert!(finish - grow < SETTLE_FRAMES as f64 / 60.0 + REVEAL + 0.1, "{finish}");
        assert!(!log.iter().any(|(a, _)| *a == Action::FadeSound), "nothing was skipped");
    }

    #[test]
    fn a_slow_start_holds_on_the_final_logo() {
        let mut machine = Machine::new(6.04, 0.0);
        let mut now = 0.0;
        while now < 7.0 {
            machine.step(now, now, false, false);
            now += 1.0 / 60.0;
        }
        assert_eq!(machine.phase, Phase::Hold);
        assert_eq!(machine.position(now), 6.04, "held on the last frame");
        assert_eq!(machine.picture(now), 1.0);
        machine.step(now, now, true, false);
        assert_eq!(machine.phase, Phase::Outro);

        // And never for ever.
        let mut machine = Machine::new(1.0, 0.0);
        let log = run(&mut machine, f64::INFINITY, 0.0, None);
        let grow = log.iter().find(|(a, _)| *a == Action::Grow).unwrap().1;
        assert!((grow - (1.0 + HOLD_MAX + OUTRO)).abs() < 0.05, "{grow}");
    }

    #[test]
    fn a_skip_fades_the_sound_and_goes() {
        let mut machine = Machine::new(6.04, 0.0);
        let log = run(&mut machine, 0.0, 0.0, Some(2.0));
        assert_eq!(log[0].0, Action::FadeSound);
        assert!((log[0].1 - 2.0).abs() < 0.02);
        let grow = log.iter().find(|(a, _)| *a == Action::Grow).unwrap().1;
        assert!((grow - (2.0 + OUTRO)).abs() < 0.05, "{grow}");
        assert!(machine.skipped);

        // Skipped while the console is still loading: the final logo, then
        // on as soon as it's ready.
        let mut machine = Machine::new(6.04, 0.0);
        assert_eq!(machine.skip(1.0, 1.0, false), vec![Action::FadeSound]);
        assert_eq!(machine.phase, Phase::Hold);
        assert_eq!(machine.position(1.0), 6.04);
        assert!(machine.skip(1.5, 1.5, false).is_empty(), "the sound fades once");
        machine.step(2.0, 2.0, true, false);
        assert_eq!(machine.phase, Phase::Outro);
        // Once the window is going, a skip does nothing.
        assert!(machine.skip(2.1, 2.1, true).is_empty());
    }

    #[test]
    fn the_cover_waits_for_the_window_to_grow() {
        let mut machine = Machine::new(0.5, 0.0);
        let log = run(&mut machine, 0.0, 1.2, None);
        let finish = log.iter().find(|(a, _)| *a == Action::Finish).unwrap().1;
        assert!(finish >= 1.2 + REVEAL - 0.02, "revealed before the window grew: {finish}");
        // A window that never says it grew is revealed anyway.
        let mut machine = Machine::new(0.5, 0.0);
        let log = run(&mut machine, 0.0, f64::INFINITY, None);
        let grow = log.iter().find(|(a, _)| *a == Action::Grow).unwrap().1;
        let finish = log.iter().find(|(a, _)| *a == Action::Finish).unwrap().1;
        assert!((finish - grow - GROW_WAIT - REVEAL).abs() < 0.05, "{}", finish - grow);
    }

    #[test]
    fn the_fades_run_the_right_way() {
        let mut machine = Machine::new(1.0, 0.0);
        assert_eq!((machine.picture(0.5), machine.cover(0.5)), (1.0, 1.0));
        machine.step(1.0, 1.0, true, false);
        assert_eq!(machine.phase, Phase::Outro);
        assert!(machine.picture(1.0 + OUTRO / 2.0) < 1.0);
        assert!(machine.owns_window());
        machine.step(1.0 + OUTRO + 1e-6, 1.0, true, false);
        assert_eq!(machine.phase, Phase::Grow);
        assert_eq!(machine.picture(1.3), 0.0);
        // The console draws under its cover from here, still under the splash.
        assert!(!machine.owns_window());
        assert!(machine.splash_window());
        assert_eq!(machine.cover(1.3), 1.0);
        for n in 0..=SETTLE_FRAMES {
            machine.step(1.3 + n as f64 * 0.016, 1.0, true, true);
        }
        assert_eq!(machine.phase, Phase::Reveal);
        assert!(!machine.splash_window(), "the splash closes once the console is on screen");
        let at = 1.4;
        machine.step(at, 1.0, true, true);
        assert!(machine.cover(at + REVEAL / 2.0) < 1.0 && machine.cover(at + REVEAL / 2.0) > 0.0);
    }

    #[test]
    fn which_launches_get_the_ident() {
        let every = Prefs::default();
        let daily = Prefs { mode: Mode::Daily, ..Prefs::default() };
        let off = Prefs { mode: Mode::Off, ..Prefs::default() };
        assert_eq!(plan(&every, 100, false, false), Plan::Full);
        assert_eq!(plan(&every, 100, false, true), Plan::Skip, "screenshot runs skip it");
        assert_eq!(plan(&off, 100, false, false), Plan::Skip);
        assert_eq!(plan(&off, 100, true, false), Plan::Skip);
        // Once a day: the whole thing the first time, a flash after that.
        assert_eq!(plan(&daily, 100, false, false), Plan::Full);
        let seen = Prefs { last_day: Some(100), ..daily };
        assert_eq!(plan(&seen, 100, false, false), Plan::Flash);
        assert_eq!(plan(&seen, 101, false, false), Plan::Full, "a new day");
        // Reduced motion: never the moving ident.
        assert_eq!(plan(&every, 100, true, false), Plan::Flash);
        assert_eq!(plan(&daily, 100, true, false), Plan::Flash);
    }

    #[test]
    fn the_preference_survives_the_library_saving_its_share() {
        let root = std::env::temp_dir().join(format!("defalt-splash-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(Prefs::load(&root), Prefs::default());
        let prefs = Prefs { mode: Mode::Daily, sound: false, last_day: Some(20_000) };
        prefs.save(&root);
        let mut library = crate::ui::Library::load(&root);
        library.set_share(0.5);
        library.save(&root);
        assert_eq!(Prefs::load(&root), prefs);
        let library = crate::ui::Library::load(&root);
        assert_eq!(library.share, 0.5, "and the other way round");
        std::fs::write(root.join("cache/console-layout.json"), r#"{"startup_video":"sideways"}"#).unwrap();
        assert_eq!(Prefs::load(&root).mode, Mode::Every);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_splash_opens_in_the_middle_of_where_the_console_will_be() {
        let size = [800.0, 643.0];
        let console = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2560.0, 1392.0));
        assert_eq!(centre(size, Some(console), None), Some(egui::pos2(880.0, 375.0)));
        // A second screen to the left: still the console's middle.
        let left = console.translate(egui::vec2(-1920.0, 0.0));
        assert_eq!(centre(size, Some(left), Some(egui::vec2(2560.0, 1440.0))), Some(egui::pos2(-1040.0, 375.0)));
        assert_eq!(centre(size, None, Some(egui::vec2(1920.0, 1080.0))), Some(egui::pos2(560.0, 219.0)));
        assert_eq!(centre(size, None, None), None);
    }
}
