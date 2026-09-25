//! The startup ident on screen. `crate::splash` decides what happens when;
//! this draws it, plays its sound through the engine and hands over to the
//! console.
//!
//! The ident plays in a small window of its own. The console's window is
//! already open behind it at full size, cloaked, and is uncloaked under the
//! splash once both are nothing but the ident's background, so the swap
//! can't be seen (see `crate::splash` for why it isn't one window growing).
//!
//! One texture, replaced frame by frame from a worker that decodes a couple
//! of frames ahead, so the ident costs one frame of memory rather than 145.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use egui::{Align2, Color32, ColorImage, FontId, Rect, Stroke, TextureHandle, TextureOptions, ViewportId};

use super::theme;
use crate::engine::decode::Track;
use crate::engine::playout::{self, Envelope, Item};
use crate::engine::Command;
use crate::splash::{self, Action, Clock, EngineTime, Frames, Machine, Phase, Plan};
use crate::Defalt;

/// The playout channel the ident's sound goes out on: past the hosts' three,
/// so the station never hands it to a voice line.
const CHANNEL: usize = playout::CHANNELS - 1;
/// How far ahead the sound is placed, so the command is on the audio thread
/// before its first sample is due.
const LEAD: f64 = 0.06;
/// A line about what the console is waiting for shows only once it has
/// waited this long; quick work doesn't get a caption that flashes.
const STATUS_AFTER: f64 = 0.3;
/// The splash window's title: what Alt+Tab says, and how it's found to
/// round its corners.
const TITLE: &str = "Defalt \u{2014} starting";

/// A worker's decoded frame, with its number.
type Decoded = (usize, ColorImage);

enum Sound {
    Off,
    Decoding(mpsc::Receiver<Result<Arc<Track>, String>>),
    Playing { track: Arc<Track>, start: u64, rate: u32 },
    Done,
}

/// A screenshot run's frozen moment. Those draw the splash in the console's
/// own window (sized to match), because eframe can't screenshot a second one.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pin {
    Frame(usize),
    End,
}

pub struct Splash {
    frames: Frames,
    machine: Machine,
    clock: Clock,
    began: Instant,
    texture: Option<TextureHandle>,
    shown: Option<usize>,
    /// The frame the worker should be on. It decodes forward from here.
    want: Arc<AtomicUsize>,
    decoded: Option<mpsc::Receiver<Decoded>>,
    /// A frame the worker got to before the clock did.
    ahead: Option<Decoded>,
    sound: Sound,
    /// What the console is waiting for, and since when.
    status: Option<(&'static str, f64)>,
    /// The console's window, cloaked until the swap.
    console: Option<isize>,
    cloaked: bool,
    /// Where the splash window goes, worked out once.
    place: Option<egui::Pos2>,
    icon: Arc<egui::IconData>,
    /// The console's window told to maximize (behind its cloak).
    maximizing: bool,
    /// Frames the splash window has drawn while still hidden; it's shown
    /// after the first, so it never appears blank.
    painted: u32,
    /// Click, Esc, Space or Enter in the splash window, since last frame.
    skip: bool,
    pin: Option<Pin>,
    /// The final logo only: a flash, reduced motion, or a shot of the end.
    still: bool,
    asked_for_shot: bool,
    /// When the splash window was shown.
    visible_at: Option<f64>,
    /// The ident's clock is running (from when it could be seen).
    started: bool,
}

/// A screenshot run that asked for the splash: which moment, if any.
fn pin_from_env(last: usize) -> Option<Pin> {
    std::env::var_os("DEFALT_SHOT")?;
    let pin = std::env::var("DEFALT_SHOT_SPLASH").ok()?;
    Some(match pin.trim().parse::<usize>() {
        Ok(n) => Pin::Frame(n.min(last)),
        Err(_) => Pin::End,
    })
}

/// Whether this run draws the splash in the console's own window (a
/// screenshot of it) rather than a window of its own.
pub fn in_console_window() -> bool {
    Frames::parse(splash::FRAMES).is_ok_and(|frames| pin_from_env(frames.last()).is_some())
}

impl Splash {
    /// `sound` is the ident's sound on the engine, when that's wanted and
    /// there's an engine to play it. `console` is the console's window, to
    /// be cloaked until the ident is done.
    pub fn new(plan: Plan, sound: bool, console: Option<isize>) -> Option<Splash> {
        if plan == Plan::Skip {
            return None;
        }
        let frames = match Frames::parse(splash::FRAMES) {
            Ok(frames) => frames,
            Err(error) => {
                crate::logfile::log!("splash: {error}");
                return None;
            }
        };
        let pin = pin_from_env(frames.last());
        let length = if plan == Plan::Flash { splash::FLASH } else { frames.seconds() };
        let still = plan == Plan::Flash || pin == Some(Pin::End);
        let first = match pin {
            Some(Pin::Frame(n)) => n,
            _ if still => frames.last(),
            _ => 0,
        };
        let sound = if sound && plan == Plan::Full && pin.is_none() {
            let (send, receive) = mpsc::channel();
            std::thread::Builder::new()
                .name("defalt-splash-sound".into())
                .spawn(move || {
                    let rate = crate::engine::decode::output_rate();
                    let _ = send.send(crate::engine::decode::load_bytes(splash::SOUND, "wav", rate));
                })
                .map_or(Sound::Off, |_| Sound::Decoding(receive))
        } else {
            Sound::Off
        };
        let clock = match sound {
            Sound::Decoding(_) => Clock::Waiting { since: 0.0 },
            _ => Clock::Wall { origin: 0.0 },
        };
        let cloaked = pin.is_none() && console.is_some_and(|hwnd| crate::platform::cloak(hwnd, true));
        Some(Splash {
            frames,
            machine: Machine::new(length, 0.0),
            clock,
            began: Instant::now(),
            texture: None,
            shown: None,
            want: Arc::new(AtomicUsize::new(first)),
            decoded: None,
            ahead: None,
            sound,
            status: None,
            console,
            cloaked,
            place: None,
            icon: Arc::new(crate::platform::window_icon().unwrap_or_default()),
            maximizing: false,
            painted: 0,
            skip: false,
            pin,
            still,
            asked_for_shot: false,
            visible_at: None,
            started: false,
        })
    }

    fn now(&self) -> f64 {
        self.began.elapsed().as_secs_f64()
    }

    fn played(&mut self, engine: Option<EngineTime>) -> f64 {
        let now = self.now();
        match self.pin {
            Some(Pin::Frame(n)) => (n as f64 + 0.5) / self.frames.fps,
            Some(Pin::End) => self.frames.seconds(),
            None => self.clock.played(now, engine),
        }
    }

    /// Put the frame for this moment on the texture, if it's arrived.
    fn show_frame(&mut self, ctx: &egui::Context, target: usize) {
        if self.shown.is_none() {
            // The first frame is decoded here, so the window never opens
            // on an empty background.
            if let Some(image) = decode(&self.frames, target) {
                self.upload(ctx, target, image);
            }
            if self.pin.is_none() && !self.still && target < self.frames.last() {
                self.decoded = Some(spawn_decoder(self.want.clone(), target + 1, ctx.clone()));
            }
            return;
        }
        self.want.store(target, Ordering::Relaxed);
        let mut best = None;
        loop {
            if let Some((n, _)) = &self.ahead {
                if *n > target {
                    break;
                }
                best = self.ahead.take();
                continue;
            }
            match self.decoded.as_ref().map(|receive| receive.try_recv()) {
                Some(Ok(frame)) => self.ahead = Some(frame),
                _ => break,
            }
        }
        if let Some((n, image)) = best {
            if Some(n) != self.shown {
                self.upload(ctx, n, image);
            }
        }
    }

    fn upload(&mut self, ctx: &egui::Context, n: usize, image: ColorImage) {
        match &mut self.texture {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("splash", image, TextureOptions::LINEAR)),
        }
        self.shown = Some(n);
    }

    /// Start the sound once it has decoded, lined up with where the picture
    /// is. Too late for the ident (skipped, or over): not at all.
    fn start_sound(&mut self, app: &mut Defalt, played: f64) {
        let Sound::Decoding(receive) = &self.sound else { return };
        let Ok(result) = receive.try_recv() else { return };
        let track = match result {
            Ok(track) => track,
            Err(error) => {
                crate::logfile::log!("splash: couldn't decode the ident's sound: {error}");
                self.sound = Sound::Off;
                return;
            }
        };
        let Some(time) = engine_time(app) else {
            self.sound = Sound::Off;
            return;
        };
        let offset = played.max(0.0) + LEAD;
        if self.machine.phase != Phase::Intro || offset >= track.seconds() {
            self.sound = Sound::Done;
            return;
        }
        let rate = time.rate as f64;
        let at = time.frame + (LEAD * rate).round() as u64;
        // The output frame the ident's first sample would have played at.
        let start = at.saturating_sub((played.max(0.0) * rate).round() as u64);
        app.send(Command::Air {
            channel: CHANNEL,
            item: Some(Box::new(Item {
                track: track.clone(),
                start_frame: at,
                offset,
                duration: track.seconds(),
                envelope: Arc::new(Envelope::flat(1.0)),
            })),
        });
        self.clock = Clock::engine(start, time, self.now());
        self.sound = Sound::Playing { track, start, rate: time.rate };
    }

    /// Skipped: the sound goes quickly rather than being cut. Replaces the
    /// playing item with the same sound from the same sample, fading out.
    fn fade_sound(&mut self, app: &mut Defalt) {
        let Sound::Playing { track, start, rate } = &self.sound else {
            self.sound = Sound::Done;
            return;
        };
        let item = engine_time(app).and_then(|time| {
            let offset = splash::engine_seconds(time.frame, *start, *rate);
            (offset >= 0.0 && offset < track.seconds()).then(|| Box::new(Item {
                track: track.clone(),
                start_frame: time.frame,
                offset,
                duration: splash::SKIP_FADE,
                envelope: Arc::new(Envelope::new(vec![[0.0, 1.0], [splash::SKIP_FADE as f32, 0.0]])),
            }))
        });
        app.send(Command::Air { channel: CHANNEL, item });
        self.sound = Sound::Done;
    }

    /// The console's window comes out from behind its cloak, under the
    /// splash, and takes the keyboard.
    fn uncloak(&mut self, ctx: &egui::Context) {
        if let Some(console) = self.console.filter(|_| self.cloaked) {
            crate::platform::cloak(console, false);
        }
        self.cloaked = false;
        ctx.send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Focus);
    }

    fn paint(&self, ctx: &egui::Context, now: f64) {
        let rect = ctx.viewport_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(rect, 0.0, background());
        let picture = self.machine.picture(now);
        if let Some(texture) = self.texture.as_ref().filter(|_| picture > 0.0) {
            let size = egui::vec2(self.frames.width as f32, self.frames.height as f32);
            let scale = (rect.width() / size.x).min(rect.height() / size.y);
            let image = Rect::from_center_size(rect.center(), size * scale);
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(texture.id(), image, uv, Color32::WHITE.gamma_multiply(picture));
        }
        if let Some((text, since)) = self.status {
            let shown = ((now - since - STATUS_AFTER) / 0.2).clamp(0.0, 1.0) as f32 * picture;
            if shown > 0.0 {
                painter.text(
                    egui::pos2(rect.center().x, rect.bottom() - 40.0),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(theme::SIZE_S),
                    theme::TEXT_MUTE.gamma_multiply(shown),
                );
            }
        }
        // A hairline edge, so the window reads as a window on a dark desktop
        // (Windows 11 rounds the corners to match). It fades with the ident,
        // so over the console at the swap there's no outline left to see.
        painter.rect_stroke(rect.shrink(0.5), 8.0,
                            Stroke::new(1.0, Color32::from_white_alpha(14).gamma_multiply(picture)),
                            egui::StrokeKind::Inside);
    }

    /// The splash's own window, for one frame.
    fn window(&mut self, ctx: &egui::Context, now: f64) {
        let size = self.frames.window_size();
        if self.place.is_none() {
            // In the middle of the maximized console, once it has maximized
            // (a frame or two); the monitor's middle if that's slow.
            let (maximized, console, monitor) =
                ctx.input(|i| (i.viewport().maximized, i.viewport().outer_rect, i.viewport().monitor_size));
            if maximized == Some(true) || now > 0.5 {
                self.place = splash::centre(size, console.filter(|_| maximized == Some(true)), monitor);
            }
        }
        let Some(place) = self.place else { return };
        let builder = egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_inner_size(size)
            .with_position(place)
            .with_icon(self.icon.clone());
        ctx.show_viewport_immediate(ViewportId::from_hash_of("defalt-splash"), builder, |ui, _| {
            let ctx = ui.ctx().clone();
            if self.painted == 1 {
                if let Some(hwnd) = crate::platform::find_window(TITLE) {
                    crate::platform::round_corners(hwnd, true);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.visible_at = Some(now);
                crate::logfile::log!("startup: splash on screen {:.0} ms after launch", launched_ms());
            }
            self.painted += 1;
            self.paint(&ctx, now);
            self.skip |= ctx.input(|i| i.pointer.any_click()) || skip_key(&ctx);
            // Alt+F4 on the splash is quitting Defalt.
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Close);
            }
        });
    }

    /// A screenshot run: once the pinned frame is up, take the picture;
    /// once it's taken, save it and quit.
    fn screenshot(&mut self, ctx: &egui::Context, target: usize) {
        if !self.asked_for_shot && self.shown == Some(target) && self.now() > 0.6 {
            self.asked_for_shot = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let images: Vec<Arc<ColorImage>> = ctx.input(|i| i.events.iter().filter_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        }).collect());
        for image in images {
            let path = std::env::var_os("DEFALT_SHOT").map(std::path::PathBuf::from).unwrap_or_default();
            match crate::shots::save_png(&image, &path) {
                Ok(()) => println!("screenshot: {}", path.display()),
                Err(error) => eprintln!("screenshot failed: {error}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// The console window's frame while there's a splash. True while the
/// console shouldn't be drawn: it's still cloaked (or, for a screenshot of
/// the splash, it's the splash).
pub fn before(app: &mut Defalt, ctx: &egui::Context) -> bool {
    let Some(mut splash) = app.splash.take() else { return false };
    let now = splash.now();
    if splash.pin.is_none() && !splash.started {
        // The ident starts when it can be seen, not when the window was
        // asked for: until then, the first frame, no sound, no clock. A
        // window that never shows (it should take a frame or two) doesn't
        // hold the console up for long.
        if splash.visible_at.is_none() && now < 2.0 {
            let first = if splash.still { splash.frames.last() } else { 0 };
            splash.show_frame(ctx, first);
            if !splash.maximizing {
                splash.maximizing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            splash.window(ctx, now);
            ctx.layer_painter(egui::LayerId::background()).rect_filled(ctx.viewport_rect(), 0.0, background());
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            app.splash = Some(splash);
            return true;
        }
        splash.started = true;
        splash.machine = Machine::new(splash.machine.length(), now);
        splash.clock = match splash.clock {
            Clock::Waiting { .. } => Clock::Waiting { since: now },
            _ => Clock::Wall { origin: now },
        };
    }

    // What the console is still doing, for the caption and for knowing
    // when it can be shown.
    app.studio.preload(ctx);
    let pending = if app.library_inbox.is_some() {
        Some("warming up the decks\u{2026}")
    } else if !app.studio.art_ready(ctx) {
        Some("waking Mav and Rue\u{2026}")
    } else {
        None
    };
    splash.status = match (pending, splash.status) {
        (Some(text), Some((was, since))) if was == text => Some((text, since)),
        (Some(text), _) => Some((text, now)),
        (None, _) => None,
    };
    let ready = pending.is_none();

    let played = splash.played(engine_time(app));
    splash.start_sound(app, played);
    let played = splash.played(engine_time(app));

    if splash.pin.is_some() {
        let target = if splash.still { splash.frames.last() } else { splash.frames.at(played) };
        splash.show_frame(ctx, target);
        splash.paint(ctx, now);
        splash.screenshot(ctx, target);
        app.splash = Some(splash);
        return true;
    }

    // The keyboard can be with either window: the splash normally, but the
    // console's (cloaked) window if Windows kept focus there.
    splash.skip |= skip_key(ctx);
    let mut actions = Vec::new();
    if std::mem::take(&mut splash.skip) && matches!(splash.machine.phase, Phase::Intro | Phase::Hold) {
        crate::logfile::log!("startup: splash skipped {played:.2} s in");
        actions = splash.machine.skip(now, played, ready);
    }
    actions.extend(splash.machine.step(now, played, ready, !splash.cloaked));
    for action in actions {
        match action {
            Action::FadeSound => splash.fade_sound(app),
            Action::Grow => splash.uncloak(ctx),
            Action::Finish => {
                crate::logfile::log!("startup: console ready {:.0} ms after launch", launched_ms());
                ctx.request_repaint();
                return false;
            }
        }
    }

    let target = if splash.still { splash.frames.last() } else { splash.frames.at(splash.machine.position(played)) };
    splash.show_frame(ctx, target);
    if splash.machine.splash_window() {
        splash.window(ctx, now);
    }
    let owns = splash.machine.owns_window();
    if owns {
        // Cloaked, so nobody sees this; it's what shows if cloaking isn't
        // there to hide it.
        ctx.layer_painter(egui::LayerId::background()).rect_filled(ctx.viewport_rect(), 0.0, background());
    }
    // The ident moves at 24 frames a second; drawing at up to 60 keeps a
    // new frame from waiting most of a frame to be seen, and the fades smooth.
    ctx.request_repaint_after(std::time::Duration::from_millis(16));
    app.splash = Some(splash);
    owns
}

/// Over the console while it's being revealed: the ident's background,
/// lifting off.
pub fn after(app: &Defalt, ctx: &egui::Context) {
    let Some(splash) = app.splash.as_ref() else { return };
    let cover = splash.machine.cover(splash.now());
    if cover <= 0.0 {
        return;
    }
    ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("splash_cover")))
        .rect_filled(ctx.viewport_rect(), 0.0, background().gamma_multiply(cover));
}

fn skip_key(ctx: &egui::Context) -> bool {
    ctx.input(|i| [egui::Key::Escape, egui::Key::Space, egui::Key::Enter].iter().any(|key| i.key_pressed(*key)))
}

fn background() -> Color32 {
    let [r, g, b] = splash::BACKGROUND;
    Color32::from_rgb(r, g, b)
}

fn engine_time(app: &Defalt) -> Option<EngineTime> {
    let engine = app.engine.as_ref()?;
    let rate = match engine.telemetry.device_rate() {
        0 => engine.sample_rate,
        rate => rate,
    };
    Some(EngineTime { frame: engine.telemetry.frame(), rate, restarts: engine.telemetry.device_restarts() })
}

fn decode(frames: &Frames, n: usize) -> Option<ColorImage> {
    let image = image::load_from_memory_with_format(frames.bytes(n), image::ImageFormat::WebP)
        .map_err(|error| crate::logfile::log!("splash: frame {n}: {error}"))
        .ok()?
        .into_rgb8();
    let size = [image.width() as usize, image.height() as usize];
    Some(ColorImage::from_rgb(size, image.as_raw()))
}

/// Decodes forward from `from`, jumping ahead whenever the clock has got
/// further than it has, and stops after the last frame or when nobody is
/// listening. Two frames of slack: enough to ride out a slow decode.
fn spawn_decoder(want: Arc<AtomicUsize>, from: usize, ctx: egui::Context) -> mpsc::Receiver<Decoded> {
    let (send, receive) = mpsc::sync_channel(2);
    let _ = std::thread::Builder::new().name("defalt-splash".into()).spawn(move || {
        let Ok(frames) = Frames::parse(splash::FRAMES) else { return };
        let mut next = from;
        while next <= frames.last() {
            let n = next.max(want.load(Ordering::Relaxed)).min(frames.last());
            let Some(image) = decode(&frames, n) else { break };
            if send.send((n, image)).is_err() {
                break;
            }
            ctx.request_repaint();
            next = n + 1;
        }
    });
    receive
}

/// Milliseconds since the process started, for the startup log.
fn launched_ms() -> f64 {
    crate::platform::since_launch().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_arrive_in_order_and_catch_up_with_the_clock() {
        let want = Arc::new(AtomicUsize::new(0));
        let receive = spawn_decoder(want.clone(), 1, egui::Context::default());
        let (first, image) = receive.recv().unwrap();
        assert_eq!(first, 1);
        assert_eq!(image.size, [1200, 964]);
        // The clock jumps ahead: the worker follows rather than decoding
        // every frame in between.
        want.store(100, Ordering::Relaxed);
        let got: Vec<usize> = receive.iter().map(|(n, _)| n).collect();
        assert!(got.windows(2).all(|w| w[0] < w[1]), "{got:?}");
        assert_eq!(*got.last().unwrap(), 144);
        assert!(got.contains(&100) && got.len() < 60, "{got:?}");
    }

    #[test]
    fn nothing_is_shown_for_a_launch_that_skips_it() {
        assert!(Splash::new(Plan::Skip, true, None).is_none());
        let flash = Splash::new(Plan::Flash, true, None).unwrap();
        assert!(matches!(flash.sound, Sound::Off), "a flash is silent");
        assert!(flash.still, "a flash is the final logo, not the ident's first second");
        assert_eq!(flash.machine.phase, Phase::Intro);
        let full = Splash::new(Plan::Full, false, None).unwrap();
        assert!(!full.still);
        assert!(!full.cloaked, "no window, nothing cloaked");
        assert!(matches!(full.clock, Clock::Wall { .. }), "no sound: the wall clock from the start");
    }

    #[test]
    fn a_skip_in_the_splash_window_reaches_the_machine() {
        let mut app = Defalt::from_root(std::env::temp_dir().join("defalt-no-fixture"), false);
        app.splash = Splash::new(Plan::Full, false, None);
        let ctx = egui::Context::default();
        let frame = |app: &mut Defalt| ctx.run_ui(egui::RawInput::default(), |ui| { before(app, ui.ctx()); })
            .drop_without_applying_deltas();
        // Nothing runs before the splash can be seen: the first frame, no clock.
        app.splash.as_mut().unwrap().skip = true;
        frame(&mut app);
        let splash = app.splash.as_mut().unwrap();
        assert!(!splash.started && !splash.machine.skipped);
        assert_eq!(splash.shown, Some(0));
        // Seen: now a skip lands, once.
        splash.visible_at = Some(0.0);
        frame(&mut app);
        let splash = app.splash.as_ref().unwrap();
        assert!(splash.started);
        assert!(splash.machine.skipped);
        assert!(!splash.skip, "a skip is taken once");
    }
}
