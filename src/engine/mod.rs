//! The audio engine.
//!
//! One thread owns the output stream and the decks. Everything else talks to
//! it by pushing commands into a lock-free ring, and reads what it is doing
//! back out of atomics. Nothing on the far side of that boundary ever blocks
//! the audio callback -- no allocation, no free, no syscall, and no lock it
//! would wait for -- because a callback that misses its deadline is a click,
//! and enough of them is a drop out.
//!
//! The console (decks, radio bus, sends, limiter) lives behind a mutex that
//! only two parties ever touch: the callback, which `try_lock`s it and plays
//! silence in the vanishingly unlikely case it is held, and the housekeeping
//! thread, which only takes it while no stream exists -- after a device has
//! gone away and before its replacement is opened. That is what lets a
//! pulled USB interface or a change of default device come back with every
//! record, playhead and knob exactly where it was.

pub mod automation;
pub mod broadcast;
pub mod deck;
pub mod echo;
pub mod decode;
pub mod filters;
pub mod limiter;
pub mod playout;
pub mod resample;
pub mod reverb;
pub mod stretch;
pub mod visualizer;

#[cfg(test)]
mod audit;
#[cfg(test)]
mod soundcheck;

#[allow(unused_imports)]
pub use automation::{Curve, Lane, LANES};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig};

use deck::Deck;
use decode::Track;

/// Two for now. The layout is per-deck throughout so four is a constant change
/// rather than a rewrite.
pub const DECKS: usize = 2;

/// The most frames rendered in one pass. A driver asking for more (96 or
/// 192 kHz, Bluetooth, a large WASAPI period) gets several passes rather
/// than silence.
const CHUNK: usize = 2048;
/// Retired records, items and curves waiting to be freed off the audio
/// thread. A load retires up to six things (a record, four stems, an
/// outgoing tail); a busy radio break a handful of items. The housekeeping
/// thread empties it four times a second.
const GRAVEYARD: usize = 1024;
/// Where retirements wait if the graveyard is ever full, rather than being
/// dropped on the audio thread.
const OVERFLOW: usize = 256;

pub enum Command {
    Load { deck: usize, track: Arc<Track> },
    /// The same record, taken apart. Arrives later than the record does:
    /// separation takes seconds and a deck must be playable before it lands.
    Stems { deck: usize, parts: Box<[Arc<Track>; deck::STEMS]> },
    StemGain { deck: usize, stem: usize, value: f32 },
    StemMute { deck: usize, stem: usize, muted: bool },
    /// Fades in over 4 ms.
    Play { deck: usize },
    /// Fades out over 4 ms, then stops. `Telemetry::playing` reads false at
    /// once.
    Pause { deck: usize },
    /// Crossfades from the old place to the new while playing.
    Seek { deck: usize, seconds: f64 },
    Gain { deck: usize, value: f32 },
    Speed { deck: usize, value: f64 },
    KeyLock { deck: usize, enabled: bool },
    /// `mix` 0..0.5 is the return level (0.5 is repeats as loud as the
    /// record, 0 is off); the record itself is never turned down for it.
    Echo { deck: usize, mix: f32, feedback: f32, seconds: f32 },
    /// `None` puts the record back on its own speed.
    Scrub { deck: usize, rate: Option<f64> },
    /// Knob positions: bands 0..1 detented at 0.5, sweep -1..1 centred at 0.
    Tone { deck: usize, low: f32, mid: f32, high: f32, sweep: f32 },
    Master { value: f32 },

    /* -- Radio playout. The station schedules; these place what it decided. -- */
    /// Put a scheduled item on a playout channel, or take one off with `None`.
    Air { channel: usize, item: Option<Box<playout::Item>> },
    /// Everything off the air at once.
    OffAir,
    /// The radio's own level, kept apart from the decks' master so going on
    /// air does not move anything you set by hand. Glides over 5 ms.
    AirGain { value: f32 },

    /* -- Sample-accurate transitions. Frames are `Telemetry::frame`'s clock. -- */
    /// Run `curve` on one of a deck's lanes, replacing whatever curve was
    /// there. Build the curve off the audio thread; the old one comes back
    /// through the graveyard.
    Automate { deck: usize, lane: Lane, curve: Arc<Curve> },
    /// Cancel a lane's curve. The control stays where the curve left it.
    ClearAutomation { deck: usize, lane: Lane },
    /// The user has taken hold of a control: its lane lets go at the value it
    /// had reached, and commands move it again. Same effect as
    /// `ClearAutomation`, named for the moment it is sent.
    Detach { deck: usize, lane: Lane },
    /// Start a deck at an exact output frame, `source_seconds` into its
    /// record. On a deck already playing, it jumps there (crossfaded).
    PlayAt { deck: usize, frame: u64, source_seconds: f64 },
    /// Loop between two points of the record, in seconds, or stop looping.
    /// The seam is crossfaded, and key-locked decks loop without glitching.
    Loop { deck: usize, range: Option<(f64, f64)> },
    /// A roll: at output frame `frame`, loop the next `length_seconds` of
    /// record until output frame `until_frame`, then carry on from where the
    /// record would have been (slip). Rolls sent back to back share an in
    /// point, so 4 -> 2 -> 1 -> 1/2 beat rolls stutter one downbeat.
    LoopAt { deck: usize, frame: u64, length_seconds: f64, until_frame: u64 },
    /// The record's beat grid: a beat at `anchor_seconds`, one every
    /// `period_seconds`. A period of 0 clears it.
    Grid { deck: usize, anchor_seconds: f64, period_seconds: f64 },
    /// Seek on the deck's next beat boundary (a hot cue that keeps time).
    /// Without a grid, or stopped, an ordinary seek.
    SeekQuantized { deck: usize, seconds: f64 },
    /// Nudge `deck` so its beat phase matches `to_deck`'s, under a
    /// crossfade. Needs both grids.
    PhaseAlign { deck: usize, to_deck: usize },
    /// The transition level, 0..1, multiplied on top of `Gain`. Glides 5 ms.
    Level { deck: usize, value: f32 },
    /// Post-fader send into the shared reverb, 0..1.
    ReverbSend { deck: usize, value: f32 },
    /// The shared reverb: size 0..1, damping 0..1, pre-delay up to 0.25 s,
    /// return level 0..1 (0 by default, so nothing is heard until asked).
    Reverb { size: f32, damping: f32, predelay_seconds: f32, level: f32 },
    /// The master limiter (on by default, ceiling -1 dBFS).
    Limiter { enabled: bool },
}

impl Command {
    /// The deck a command is about, for sequence acknowledgements.
    fn deck(&self) -> Option<usize> {
        match *self {
            Command::Load { deck, .. } | Command::Stems { deck, .. }
            | Command::StemGain { deck, .. } | Command::StemMute { deck, .. }
            | Command::Play { deck } | Command::Pause { deck } | Command::Seek { deck, .. }
            | Command::Gain { deck, .. } | Command::Speed { deck, .. }
            | Command::KeyLock { deck, .. } | Command::Echo { deck, .. }
            | Command::Scrub { deck, .. } | Command::Tone { deck, .. }
            | Command::Automate { deck, .. } | Command::ClearAutomation { deck, .. }
            | Command::Detach { deck, .. } | Command::PlayAt { deck, .. }
            | Command::Loop { deck, .. } | Command::LoopAt { deck, .. }
            | Command::Grid { deck, .. } | Command::SeekQuantized { deck, .. }
            | Command::PhaseAlign { deck, .. } | Command::Level { deck, .. }
            | Command::ReverbSend { deck, .. } => Some(deck),
            Command::Master { .. } | Command::Air { .. } | Command::OffAir
            | Command::AirGain { .. } | Command::Reverb { .. } | Command::Limiter { .. } => None,
        }
    }
}

/// Everything the audio thread lets go of, on its way to being dropped on the
/// housekeeping thread. Each variant is held only to be dropped there.
#[allow(dead_code)]
enum Retired {
    Track(Arc<Track>),
    Item(Box<playout::Item>),
    Stems(Box<[Arc<Track>; deck::STEMS]>),
    Curve(Arc<Curve>),
}

/// What the audio thread is doing, readable from anywhere without a lock.
///
/// Positions are f64 bit patterns in an AtomicU64. Ugly, but a torn read of a
/// playhead would show the UI a position that never existed.
#[derive(Default)]
pub struct Telemetry {
    pub visualizer: visualizer::Capture,
    /// The mix as sent to the device, for the remote stream. Off until asked.
    pub broadcast: broadcast::Tap,
    position: [AtomicU64; DECKS],
    playing: [AtomicBool; DECKS],
    loaded: [AtomicBool; DECKS],
    separated: [AtomicBool; DECKS],
    peak: [AtomicU32; 2],
    deck_peak: [AtomicU32; DECKS],
    /// Callbacks that could not be filled in time. Non-zero means trouble.
    pub underruns: AtomicU64,
    /// Output frames since the stream opened, and the loudest thing the radio
    /// bus did in the last callback. The schedule is placed against the first
    /// of these, so the UI has to be able to read it.
    frame: AtomicU64,
    air_peak: AtomicU32,
    voice_peaks: [AtomicU32; playout::CHANNELS],
    /// Each voice channel's level (RMS) over the last callback, and its
    /// brightness: the energy of its sample-to-sample change over its energy.
    voice_rms: [AtomicU32; playout::CHANNELS],
    voice_tone: [AtomicU32; playout::CHANNELS],
    applied: [AtomicU64; DECKS],
    applied_any: AtomicU64,
    restarts: AtomicU64,
    device_rate: AtomicU32,
    limiter: AtomicU32,
}

impl Telemetry {
    fn set_position(&self, deck: usize, seconds: f64) {
        self.position[deck].store(seconds.to_bits(), Ordering::Relaxed);
    }

    pub fn position(&self, deck: usize) -> f64 {
        f64::from_bits(self.position[deck].load(Ordering::Relaxed))
    }

    pub fn playing(&self, deck: usize) -> bool {
        self.playing[deck].load(Ordering::Relaxed)
    }

    /// Whether the deck is playing separated stems rather than the record.
    #[allow(dead_code)]
    pub fn separated(&self, deck: usize) -> bool {
        self.separated[deck].load(Ordering::Relaxed)
    }

    /// Only the soundcheck reads this, which is enough reason to keep it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn loaded(&self, deck: usize) -> bool {
        self.loaded[deck].load(Ordering::Relaxed)
    }

    pub fn deck_peak(&self, deck: usize) -> f32 {
        f32::from_bits(self.deck_peak[deck].load(Ordering::Relaxed))
    }

    /// Output frames since the stream opened.
    ///
    /// The one clock the radio schedule is converted against: station seconds
    /// in, output frames out, converted once when a schedule arrives rather
    /// than every callback. It keeps counting across a device reopen, but at
    /// the new device's rate -- see `device_rate`.
    pub fn frame(&self) -> u64 {
        self.frame.load(Ordering::Relaxed)
    }

    pub fn air_peak(&self) -> f32 {
        f32::from_bits(self.air_peak.load(Ordering::Relaxed))
    }

    /// Each voice channel's loudest sample in the last callback. The booth
    /// reads `voice_rms` now; this stays for anything that wants a meter.
    #[allow(dead_code)]
    pub fn voice_peaks(&self) -> [f32; playout::CHANNELS] {
        std::array::from_fn(|i| f32::from_bits(self.voice_peaks[i].load(Ordering::Relaxed)))
    }

    pub fn voice_rms(&self) -> [f32; playout::CHANNELS] {
        std::array::from_fn(|i| f32::from_bits(self.voice_rms[i].load(Ordering::Relaxed)))
    }

    pub fn voice_tones(&self) -> [f32; playout::CHANNELS] {
        std::array::from_fn(|i| f32::from_bits(self.voice_tone[i].load(Ordering::Relaxed)))
    }

    /// Loudest sample of the last callback, measured before the final clamp,
    /// so anything over 1.0 is a real over (only possible with the limiter
    /// off).
    pub fn peak(&self) -> [f32; 2] {
        [
            f32::from_bits(self.peak[0].load(Ordering::Relaxed)),
            f32::from_bits(self.peak[1].load(Ordering::Relaxed)),
        ]
    }

    /// The sequence number (from `Engine::send_seq`) of the latest command
    /// for this deck that the audio thread has applied *and* whose effect is
    /// already in this telemetry.
    ///
    /// How the app should use it: keep the number `send_seq` gave you for
    /// the last Play/Pause/Seek/Load you sent a deck, and while
    /// `applied_seq(deck)` is still below it, trust your own idea of
    /// `playing` and `position` rather than what the telemetry says -- the
    /// telemetry is from before your command landed. Read this before
    /// reading the position; it is published after it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn applied_seq(&self, deck: usize) -> u64 {
        self.applied.get(deck).map_or(0, |seq| seq.load(Ordering::Acquire))
    }

    /// The latest sequence number applied for any command at all.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn applied_seq_any(&self) -> u64 {
        self.applied_any.load(Ordering::Acquire)
    }

    /// How many times the output has been reopened after a device went away
    /// or the default device changed. The app can show "audio device
    /// changed" when this moves.
    #[allow(dead_code)]
    pub fn device_restarts(&self) -> u64 {
        self.restarts.load(Ordering::Relaxed)
    }

    /// The rate the device is running at now. `Engine::sample_rate` is the
    /// rate it opened at; after a reopen on a device at another rate, frames
    /// (and so the radio schedule's frame arithmetic) are at this one.
    #[allow(dead_code)]
    pub fn device_rate(&self) -> u32 {
        self.device_rate.load(Ordering::Relaxed)
    }

    /// Deepest master limiter gain reduction in the last callback, in dB
    /// (0 = untouched, 3 = pulled down 3 dB). Worth a meter: a limiter that
    /// is always working is a mix that is too hot.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn limiter_reduction_db(&self) -> f32 {
        f32::from_bits(self.limiter.load(Ordering::Relaxed))
    }
}

pub struct Engine {
    commands: rtrb::Producer<(u64, Command)>,
    next_seq: u64,
    pub telemetry: Arc<Telemetry>,
    pub sample_rate: u32,
    pub device: String,
}

impl Engine {
    pub fn start() -> Result<Self, String> {
        // Deep enough that a burst of UI events cannot fill it, small enough
        // to notice if something is spraying commands.
        let (commands, inbox) = rtrb::RingBuffer::<(u64, Command)>::new(1024);
        // Things the audio thread has finished with, returned to be dropped
        // somewhere it is allowed to take its time.
        let (graveyard, undertaker) = rtrb::RingBuffer::<Retired>::new(GRAVEYARD);

        let telemetry = Arc::new(Telemetry::default());
        let (ready, started) = mpsc::channel();

        let thread_telemetry = telemetry.clone();
        std::thread::Builder::new()
            .name("defalt-audio".into())
            .spawn(move || audio_thread(inbox, graveyard, undertaker, thread_telemetry, ready))
            .map_err(|error| format!("could not start the audio thread: {error}"))?;

        let (sample_rate, device) = started
            .recv()
            .map_err(|_| "the audio thread died on the way up".to_string())??;

        Ok(Engine { commands, next_seq: 1, telemetry, sample_rate, device })
    }

    /// Commands are dropped rather than queued if the ring is full. A UI that
    /// has run 1024 commands behind is not going to be helped by the next one.
    #[allow(dead_code)]
    pub fn send(&mut self, command: Command) -> Result<(), String> {
        self.send_seq(command).map(|_| ())
    }

    /// `send`, returning the command's sequence number. Numbers only ever go
    /// up. Compare against `Telemetry::applied_seq(deck)` to know when the
    /// telemetry reflects this command.
    pub fn send_seq(&mut self, command: Command) -> Result<u64, String> {
        let seq = self.next_seq;
        self.commands
            .push((seq, command))
            .map_err(|_| "the audio thread is not keeping up".to_string())?;
        self.next_seq += 1;
        Ok(seq)
    }
}

type Started = Result<(u32, String), String>;
type Shared = Arc<Mutex<Option<Box<Console>>>>;

/// The whole of the audio side's state. Moves between streams when the
/// device changes; never allocates or frees once it is running.
struct Console {
    inbox: rtrb::Consumer<(u64, Command)>,
    graveyard: rtrb::Producer<Retired>,
    overflow: Vec<Retired>,
    decks: [Deck; DECKS],
    air: playout::Playout,
    master: f32,
    master_state: f32,
    smoothing: f32,
    reverb: reverb::Reverb,
    limiter: limiter::Limiter,
    limiter_on: bool,
    bus: Vec<f32>,
    send: Vec<f32>,
    rate: u32,
    applied: [u64; DECKS],
    applied_any: u64,
}

/// Hand something to the housekeeping thread to be dropped.
///
/// If the graveyard is full it waits in the overflow and is pushed again next
/// callback. Only if both are full -- over a thousand retirements without a
/// single 250 ms tick in between -- is it leaked rather than freed here: a
/// leak costs memory, a free in the callback costs a dropout.
fn bury(graveyard: &mut rtrb::Producer<Retired>, overflow: &mut Vec<Retired>, item: Retired) {
    if let Err(rtrb::PushError::Full(item)) = graveyard.push(item) {
        if overflow.len() < overflow.capacity() {
            overflow.push(item);
        } else {
            std::mem::forget(item);
        }
    }
}

/// Flush-to-zero and denormals-are-zero. A reverb or filter tail decaying
/// toward silence goes denormal, and denormal arithmetic on x86 is a hundred
/// times slower -- a quiet room can cost more CPU than a loud one.
#[inline]
fn denormals_off() {
    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }
}

impl Console {
    fn new(rate: u32, inbox: rtrb::Consumer<(u64, Command)>, graveyard: rtrb::Producer<Retired>) -> Self {
        Console {
            inbox,
            graveyard,
            overflow: Vec::with_capacity(OVERFLOW),
            decks: std::array::from_fn(|_| Deck::new(rate)),
            air: playout::Playout::default(),
            master: 1.0,
            master_state: 1.0,
            smoothing: 1.0 - (-1.0 / (rate.max(1) as f32 * 0.005)).exp(),
            reverb: reverb::Reverb::new(rate),
            limiter: limiter::Limiter::new(rate),
            limiter_on: true,
            bus: vec![0.0; CHUNK * 2],
            send: vec![0.0; CHUNK * 2],
            rate,
            applied: [0; DECKS],
            applied_any: 0,
        }
    }

    /// Rebuild everything that depends on the device rate. Allocates; only
    /// ever called with no stream running.
    fn set_rate(&mut self, rate: u32) {
        if rate == self.rate || rate == 0 {
            return;
        }
        for deck in self.decks.iter_mut() {
            deck.set_rate(rate);
        }
        let [size, damping, predelay, level] = self.reverb.settings();
        self.reverb = reverb::Reverb::new(rate);
        self.reverb.set(size, damping, predelay, level);
        self.limiter = limiter::Limiter::new(rate);
        self.limiter.set_enabled(self.limiter_on);
        self.smoothing = 1.0 - (-1.0 / (rate as f32 * 0.005)).exp();
        self.rate = rate;
    }

    fn retire(&mut self, item: Retired) {
        bury(&mut self.graveyard, &mut self.overflow, item);
    }

    fn flush_overflow(&mut self) {
        while let Some(item) = self.overflow.pop() {
            if let Err(rtrb::PushError::Full(item)) = self.graveyard.push(item) {
                self.overflow.push(item);
                break;
            }
        }
    }

    /// Drain the command ring. Runs at the top of every callback.
    fn apply(&mut self) {
        while let Ok((seq, command)) = self.inbox.pop() {
            let deck = command.deck();
            self.execute(command);
            if seq > 0 {
                self.applied_any = seq;
                if let Some(index) = deck.filter(|&d| d < DECKS) {
                    self.applied[index] = seq;
                }
            }
        }
    }

    fn source_rate(&self, deck: usize) -> f64 {
        self.decks[deck].track.as_ref().map_or(self.rate as f64, |t| t.sample_rate as f64)
    }

    fn execute(&mut self, command: Command) {
        let rate = self.rate;
        let valid = |deck: usize| deck < DECKS;
        match command {
            Command::Master { value } => self.master = value.clamp(0.0, 4.0),
            Command::Air { channel, item } => {
                if let Some(displaced) = self.air.set(channel, item) {
                    self.retire(Retired::Item(displaced));
                }
            }
            Command::OffAir => {
                let (graveyard, overflow) = (&mut self.graveyard, &mut self.overflow);
                self.air.clear(|item| bury(graveyard, overflow, Retired::Item(item)));
                for slot in self.decks.iter_mut() { slot.echo.clear(); }
                self.reverb.clear();
            }
            Command::AirGain { value } => self.air.gain = value.clamp(0.0, 4.0),
            Command::Load { deck, track } => {
                if !valid(deck) {
                    self.retire(Retired::Track(track));
                    return;
                }
                let (graveyard, overflow) = (&mut self.graveyard, &mut self.overflow);
                self.decks[deck].load(track, |old| bury(graveyard, overflow, Retired::Track(old)));
            }
            Command::Play { deck } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.play(); }
            }
            Command::Pause { deck } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.pause(); }
            }
            Command::Seek { deck, seconds } => {
                if valid(deck) {
                    let frames = seconds * self.source_rate(deck);
                    self.decks[deck].seek(frames);
                }
            }
            Command::Gain { deck, value } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.gain = value.clamp(0.0, 2.0); }
            }
            Command::Speed { deck, value } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.speed = value.clamp(-4.0, 4.0); }
            }
            Command::KeyLock { deck, enabled } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.key_lock = enabled; }
            }
            Command::Echo { deck, mix, feedback, seconds } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.echo.set(mix, feedback, seconds, rate); }
            }
            Command::Scrub { deck, rate } => {
                if let Some(slot) = self.decks.get_mut(deck) {
                    slot.scrub = rate.map(|value| value.clamp(-16.0, 16.0));
                }
            }
            Command::Stems { deck, mut parts } => {
                // The array moves out of its box by swapping references to
                // the record into the box's place: unboxing it would free the
                // box here. The box leaves through the graveyard.
                let track = self.decks.get(deck).and_then(|slot| slot.track.clone());
                if let Some(track) = track {
                    let placeholder: [Arc<Track>; deck::STEMS] = std::array::from_fn(|_| track.clone());
                    let fresh = std::mem::replace(&mut *parts, placeholder);
                    if let Some(old) = self.decks[deck].stems.replace(fresh) {
                        for part in old { self.retire(Retired::Track(part)); }
                    }
                }
                self.retire(Retired::Stems(parts));
            }
            Command::StemGain { deck, stem, value } => {
                if let Some(gain) = self.decks.get_mut(deck).and_then(|s| s.stem_gain.get_mut(stem)) {
                    *gain = value.clamp(0.0, 2.0);
                }
            }
            Command::StemMute { deck, stem, muted } => {
                if let Some(flag) = self.decks.get_mut(deck).and_then(|s| s.stem_muted.get_mut(stem)) {
                    *flag = muted;
                }
            }
            Command::Tone { deck, low, mid, high, sweep } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.strip.set(low, mid, high, sweep); }
            }
            Command::Automate { deck, lane, curve } => {
                if !valid(deck) {
                    self.retire(Retired::Curve(curve));
                    return;
                }
                if let Some(old) = self.decks[deck].automation.set(lane, curve) {
                    self.retire(Retired::Curve(old));
                }
            }
            Command::ClearAutomation { deck, lane } | Command::Detach { deck, lane } => {
                if let Some(old) = self.decks.get_mut(deck).and_then(|s| s.automation.clear(lane)) {
                    self.retire(Retired::Curve(old));
                }
            }
            Command::PlayAt { deck, frame, source_seconds } => {
                if valid(deck) {
                    let source = source_seconds.max(0.0) * self.source_rate(deck);
                    self.decks[deck].play_at(frame, source);
                }
            }
            Command::Loop { deck, range } => {
                if valid(deck) {
                    let source = self.source_rate(deck);
                    self.decks[deck].set_loop(range.map(|(a, b)| (a * source, b * source)));
                }
            }
            Command::LoopAt { deck, frame, length_seconds, until_frame } => {
                if let Some(slot) = self.decks.get_mut(deck) {
                    slot.schedule_roll(frame, length_seconds, until_frame);
                }
            }
            Command::Grid { deck, anchor_seconds, period_seconds } => {
                if let Some(slot) = self.decks.get_mut(deck) {
                    slot.grid = (period_seconds > 0.0 && period_seconds.is_finite()
                        && anchor_seconds.is_finite()).then_some((anchor_seconds, period_seconds));
                }
            }
            Command::SeekQuantized { deck, seconds } => {
                if valid(deck) {
                    let frames = seconds * self.source_rate(deck);
                    self.decks[deck].seek_quantized(frames);
                }
            }
            Command::PhaseAlign { deck, to_deck } => {
                if valid(deck) && valid(to_deck) && deck != to_deck {
                    self.align(deck, to_deck);
                }
            }
            Command::Level { deck, value } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.level = value.clamp(0.0, 1.0); }
            }
            Command::ReverbSend { deck, value } => {
                if let Some(slot) = self.decks.get_mut(deck) { slot.reverb_send = value.clamp(0.0, 1.0); }
            }
            Command::Reverb { size, damping, predelay_seconds, level } => {
                self.reverb.set(size, damping, predelay_seconds, level);
            }
            Command::Limiter { enabled } => {
                self.limiter_on = enabled;
                self.limiter.set_enabled(enabled);
            }
        }
    }

    /// Move `deck` by the smallest amount that puts it on the same point of
    /// its beat as `to`.
    fn align(&mut self, deck: usize, to: usize) {
        let (Some(target), Some(current), Some((_, period))) =
            (self.decks[to].phase(), self.decks[deck].phase(), self.decks[deck].grid) else { return };
        let mut delta = target - current;
        delta -= delta.round();
        let frames = delta * period * self.source_rate(deck);
        let position = self.decks[deck].position;
        self.decks[deck].seek(position + frames);
    }

    /// One callback's worth, into any sample format and channel count.
    fn render<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T], channels: usize, telemetry: &Telemetry) {
        denormals_off();
        self.flush_overflow();
        self.apply();

        let channels = channels.max(1);
        let frames = out.len() / channels;
        let visualizing = telemetry.visualizer.enabled();
        let mut peak = [0.0f32; 2];
        let mut done = 0;
        while done < frames {
            let count = (frames - done).min(CHUNK);
            let slice = &mut out[done * channels..(done + count) * channels];
            self.render_chunk(slice, channels, count, &mut peak, visualizing, telemetry);
            done += count;
        }
        // Anything past the last whole frame (a driver handing over a ragged
        // buffer) is silence rather than whatever was there.
        for sample in out[frames * channels..].iter_mut() {
            *sample = T::from_sample(0.0);
        }
        self.publish(telemetry, peak);
    }

    fn render_chunk<T: SizedSample + FromSample<f32>>(
        &mut self,
        out: &mut [T],
        channels: usize,
        frames: usize,
        peak: &mut [f32; 2],
        visualizing: bool,
        telemetry: &Telemetry,
    ) {
        let rate = self.rate;
        let bus = &mut self.bus[..frames * 2];
        let send = &mut self.send[..frames * 2];
        bus.fill(0.0);
        send.fill(0.0);
        let start = self.air.frame;
        let tap = telemetry.broadcast.begin();
        for deck in self.decks.iter_mut() {
            deck.render(bus, Some(send), rate, start);
            if let Some(done) = deck.take_finished_outgoing() {
                bury(&mut self.graveyard, &mut self.overflow, Retired::Track(done));
            }
        }
        // The radio goes through the master like everything else, so one
        // fader still takes the whole console down.
        let (graveyard, overflow) = (&mut self.graveyard, &mut self.overflow);
        self.air.mix_into(bus, rate, |item| bury(graveyard, overflow, Retired::Item(item)));

        for index in 0..frames {
            let [wet_l, wet_r] = self.reverb.process([send[index * 2], send[index * 2 + 1]]);
            self.master_state += (self.master - self.master_state) * self.smoothing;
            let left = (bus[index * 2] + wet_l) * self.master_state;
            let right = (bus[index * 2 + 1] + wet_r) * self.master_state;
            let [left, right] = self.limiter.process([left, right]);
            // Metered before the clamp, so an over shows as one.
            peak[0] = peak[0].max(left.abs());
            peak[1] = peak[1].max(right.abs());
            let left = if left.is_finite() { left.clamp(-1.0, 1.0) } else { 0.0 };
            let right = if right.is_finite() { right.clamp(-1.0, 1.0) } else { 0.0 };
            if visualizing { telemetry.visualizer.push(left, right); }
            // The stream gets exactly this; the speakers may get silence instead.
            let (left, right) = match &tap { Some(tap) => tap.put(index, left, right), None => (left, right) };

            let slot = &mut out[index * channels..(index + 1) * channels];
            if channels == 1 {
                // A mono device gets both sides, not just the left.
                slot[0] = T::from_sample(0.5 * (left + right));
                continue;
            }
            for (channel, sample) in slot.iter_mut().enumerate() {
                // Anything wider than two gets silence past the pair rather
                // than a duplicate.
                *sample = T::from_sample(match channel {
                    0 => left,
                    1 => right,
                    _ => 0.0,
                });
            }
        }
        if let Some(tap) = tap { tap.commit(frames, rate); }
    }

    fn publish(&mut self, telemetry: &Telemetry, peak: [f32; 2]) {
        telemetry.frame.store(self.air.frame, Ordering::Relaxed);
        telemetry.air_peak.store(self.air.peak.to_bits(), Ordering::Relaxed);
        self.air.peak = 0.0;
        for (i, peak) in self.air.channel_peaks.iter_mut().enumerate() {
            telemetry.voice_peaks[i].store(peak.to_bits(), Ordering::Relaxed);
            *peak = 0.0;
        }
        for i in 0..playout::CHANNELS {
            let (energy, count) = (self.air.channel_energy[i], self.air.channel_samples[i]);
            let rms = if count > 0 { (energy / count as f32).sqrt() } else { 0.0 };
            let tone = if energy > 1e-9 { self.air.channel_change[i] / energy } else { 0.0 };
            telemetry.voice_rms[i].store(rms.to_bits(), Ordering::Relaxed);
            telemetry.voice_tone[i].store(tone.to_bits(), Ordering::Relaxed);
            self.air.channel_energy[i] = 0.0;
            self.air.channel_change[i] = 0.0;
            self.air.channel_samples[i] = 0;
        }
        telemetry.peak[0].store(peak[0].to_bits(), Ordering::Relaxed);
        telemetry.peak[1].store(peak[1].to_bits(), Ordering::Relaxed);
        telemetry.limiter.store(self.limiter.take_reduction().to_bits(), Ordering::Relaxed);
        for (index, deck) in self.decks.iter_mut().enumerate() {
            telemetry.deck_peak[index].store(deck.peak.to_bits(), Ordering::Relaxed);
            // Read and reset: the meter shows this callback's loudest
            // moment, not the loudest since the record was loaded.
            deck.peak = 0.0;
            telemetry.set_position(index, deck.seconds());
            telemetry.playing[index].store(deck.transport_playing(), Ordering::Relaxed);
            telemetry.loaded[index].store(deck.track.is_some(), Ordering::Relaxed);
            telemetry.separated[index].store(deck.stems.is_some(), Ordering::Relaxed);
        }
        // Last, and released: a reader that sees these sees everything above.
        for (index, seq) in self.applied.iter().enumerate() {
            telemetry.applied[index].store(*seq, Ordering::Release);
        }
        telemetry.applied_any.store(self.applied_any, Ordering::Release);
    }
}

/// The whole of one callback.
fn fill<T: SizedSample + FromSample<f32>>(shared: &Shared, out: &mut [T], channels: usize, telemetry: &Telemetry) {
    // Never waits: the only other holder is the housekeeping thread, and it
    // only holds it while no stream is running.
    let mut guard = match shared.try_lock() {
        Ok(guard) => guard,
        // A render that panicked left the lock poisoned. The console is
        // still there; carrying on beats silence for the rest of the
        // session.
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            shared.clear_poison();
            poisoned.into_inner()
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            silence(out);
            telemetry.underruns.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    render_guarded(guard.as_deref_mut(), out, channels, telemetry);
}

/// One callback, with a panic in the render costing that buffer (silence,
/// counted as an underrun) rather than the stream.
fn render_guarded<T: SizedSample + FromSample<f32>>(
    console: Option<&mut Console>,
    out: &mut [T],
    channels: usize,
    telemetry: &Telemetry,
) {
    let Some(console) = console else {
        silence(out);
        return;
    };
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        console.render(out, channels, telemetry);
    }));
    if rendered.is_err() {
        silence(out);
        telemetry.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

fn silence<T: SizedSample + FromSample<f32>>(out: &mut [T]) {
    for sample in out.iter_mut() {
        *sample = T::from_sample(0.0);
    }
}

fn default_device_id(host: &cpal::Host) -> Option<String> {
    host.default_output_device().and_then(|device| device.id().ok()).map(|id| id.to_string())
}

/// Open the default output and start it, building the console first if this
/// is the first time. Returns the stream and the device's id and rate.
fn open(
    host: &cpal::Host,
    shared: &Shared,
    pending: &mut Option<(rtrb::Consumer<(u64, Command)>, rtrb::Producer<Retired>)>,
    telemetry: &Arc<Telemetry>,
    failed: &Arc<AtomicBool>,
) -> Result<(cpal::Stream, String, u32), String> {
    let device = host.default_output_device().ok_or("no output device")?;
    let name = device.id().map(|id| id.to_string()).unwrap_or_else(|_| "output".into());
    let config = device
        .default_output_config()
        .map_err(|error| format!("no usable output config: {error}"))?;
    let format = config.sample_format();
    let config: StreamConfig = config.into();
    let rate = config.sample_rate;

    {
        // No stream exists here, so nothing else can be holding this.
        let mut guard = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_mut() {
            Some(console) => console.set_rate(rate),
            None => {
                if let Some((inbox, graveyard)) = pending.take() {
                    *guard = Some(Box::new(Console::new(rate, inbox, graveyard)));
                }
            }
        }
    }
    decode::set_output_rate(rate);
    telemetry.device_rate.store(rate, Ordering::Relaxed);

    let (s, t, f) = (shared.clone(), telemetry.clone(), failed.clone());
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, s, t, f),
        SampleFormat::I16 => build::<i16>(&device, config, s, t, f),
        SampleFormat::I32 => build::<i32>(&device, config, s, t, f),
        SampleFormat::U16 => build::<u16>(&device, config, s, t, f),
        other => Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|error| format!("could not start the stream: {error}"))?;
    Ok((stream, name, rate))
}

fn audio_thread(
    inbox: rtrb::Consumer<(u64, Command)>,
    graveyard: rtrb::Producer<Retired>,
    mut undertaker: rtrb::Consumer<Retired>,
    telemetry: Arc<Telemetry>,
    ready: mpsc::Sender<Started>,
) {
    let host = cpal::default_host();
    let shared: Shared = Arc::new(Mutex::new(None));
    let failed = Arc::new(AtomicBool::new(false));
    let mut pending = Some((inbox, graveyard));

    let (stream, mut device, rate) = match open(&host, &shared, &mut pending, &telemetry, &failed) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok((rate, device.clone())));

    // The stream is not Send on Windows, so it has to live and die here. While
    // we are holding it anyway, this is also where retired records get freed,
    // and where a lost device is noticed and replaced.
    let mut stream = Some(stream);
    let mut tick = 0u64;
    loop {
        while undertaker.pop().is_ok() {
            // Dropping it here is the entire point.
        }
        tick += 1;
        let lost = failed.swap(false, Ordering::AcqRel);
        // cpal reports a default-device change on the stream, but checking
        // costs next to nothing every two seconds and covers hosts that do
        // not.
        let moved = stream.is_some() && tick % 8 == 0
            && default_device_id(&host).is_some_and(|id| id != device);
        let missing = stream.is_none() && tick % 4 == 0;
        if lost || moved || missing {
            // Dropping the stream stops its callbacks; after this the console
            // is ours to rebuild.
            drop(stream.take());
            match open(&host, &shared, &mut pending, &telemetry, &failed) {
                Ok((opened, name, rate)) => {
                    eprintln!("audio: reopened on {name} at {rate} Hz");
                    stream = Some(opened);
                    device = name;
                    telemetry.restarts.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    if lost || moved { eprintln!("audio: the output went away ({error}); retrying"); }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    shared: Shared,
    telemetry: Arc<Telemetry>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let errors = telemetry.clone();
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &OutputCallbackInfo| fill(&shared, out, channels, &telemetry),
            move |error: cpal::Error| {
                // Runs on cpal's thread, which may be the audio thread: set a
                // flag and let housekeeping do the talking and the reopening.
                match error.kind() {
                    cpal::ErrorKind::Xrun => { errors.underruns.fetch_add(1, Ordering::Relaxed); }
                    cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => {}
                    _ => failed.store(true, Ordering::Release),
                }
            },
            None,
        )
        .map_err(|error| format!("could not open the output: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rig {
        console: Console,
        commands: rtrb::Producer<(u64, Command)>,
        undertaker: rtrb::Consumer<Retired>,
        telemetry: Telemetry,
        seq: u64,
    }

    impl Rig {
        fn new(rate: u32) -> Self {
            let (commands, inbox) = rtrb::RingBuffer::new(1024);
            let (graveyard, undertaker) = rtrb::RingBuffer::new(GRAVEYARD);
            Rig { console: Console::new(rate, inbox, graveyard), commands, undertaker,
                  telemetry: Telemetry::default(), seq: 0 }
        }

        fn send(&mut self, command: Command) -> u64 {
            self.seq += 1;
            assert!(self.commands.push((self.seq, command)).is_ok());
            self.seq
        }

        /// One callback of `frames` frames into a device of `channels`.
        fn callback(&mut self, frames: usize, channels: usize) -> Vec<f32> {
            let mut out = vec![0.0f32; frames * channels];
            self.console.render(&mut out, channels, &self.telemetry);
            out
        }

        fn buried(&mut self) -> usize {
            let mut count = 0;
            while self.undertaker.pop().is_ok() { count += 1; }
            count
        }
    }

    fn constant(level: f32, seconds: f64, rate: u32) -> Arc<Track> {
        Arc::new(Track { samples: vec![level; (seconds * rate as f64) as usize * 2], sample_rate: rate })
    }

    fn sine(hz: f64, level: f32, seconds: f64, rate: u32) -> Arc<Track> {
        let samples = (0..(seconds * rate as f64) as usize).flat_map(|i| {
            let v = (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() as f32 * level;
            [v, v]
        }).collect();
        Arc::new(Track { samples, sample_rate: rate })
    }

    #[test]
    fn a_callback_bigger_than_the_scratch_bus_still_plays() {
        for frames in [2_048, 2_049, 4_096, 9_000] {
            let mut rig = Rig::new(192_000);
            rig.send(Command::Load { deck: 0, track: constant(0.25, 1.0, 192_000) });
            rig.send(Command::Play { deck: 0 });
            // Past the 4 ms fade-in and the limiter's look-ahead first.
            rig.callback(4_096, 2);
            let out = rig.callback(frames, 2);
            let bad = out.iter().position(|&s| (s - 0.25).abs() >= 1e-3);
            assert!(bad.is_none(), "{frames} frames came out as silence or garbage at {bad:?}: {:?}",
                    bad.map(|i| out[i]));
            assert_eq!(rig.telemetry.underruns.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn a_mono_device_hears_both_sides() {
        let mut rig = Rig::new(48_000);
        let track = Arc::new(Track {
            samples: (0..48_000).flat_map(|_| [0.4f32, 0.2]).collect(),
            sample_rate: 48_000,
        });
        rig.send(Command::Load { deck: 0, track });
        rig.send(Command::Play { deck: 0 });
        rig.callback(512, 1);
        let out = rig.callback(512, 1);
        assert!((out[100] - 0.3).abs() < 1e-4, "mono got {}", out[100]);
    }

    #[test]
    fn overs_are_metered_before_the_clamp_and_the_limiter_holds_the_ceiling() {
        let mut rig = Rig::new(48_000);
        rig.send(Command::Load { deck: 0, track: sine(100.0, 1.0, 2.0, 48_000) });
        rig.send(Command::Load { deck: 1, track: sine(100.0, 1.0, 2.0, 48_000) });
        rig.send(Command::Play { deck: 0 });
        rig.send(Command::Play { deck: 1 });
        rig.callback(2_048, 2);
        let limited = rig.callback(4_800, 2);
        let loudest = limited.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(loudest <= limiter::CEILING + 1e-4, "limited output reached {loudest}");
        assert!(rig.telemetry.limiter_reduction_db() > 5.0);

        rig.send(Command::Limiter { enabled: false });
        rig.callback(4_800, 2);
        rig.callback(4_800, 2);
        assert!(rig.telemetry.peak()[0] > 1.5, "the over was clamped out of the meter");
    }

    #[test]
    fn telemetry_acknowledges_commands_in_order_per_deck() {
        let mut rig = Rig::new(48_000);
        let load = rig.send(Command::Load { deck: 1, track: constant(0.1, 1.0, 48_000) });
        let play = rig.send(Command::Play { deck: 1 });
        assert_eq!(rig.telemetry.applied_seq(1), 0);
        rig.callback(256, 2);
        assert!(play > load);
        assert_eq!(rig.telemetry.applied_seq(1), play);
        assert_eq!(rig.telemetry.applied_seq(0), 0, "deck A never got a command");
        assert!(rig.telemetry.playing(1));
        let master = rig.send(Command::Master { value: 0.5 });
        rig.callback(256, 2);
        assert_eq!(rig.telemetry.applied_seq_any(), master);
        assert_eq!(rig.telemetry.applied_seq(1), play);
    }

    #[test]
    fn every_retirement_goes_through_the_graveyard_and_nothing_is_freed_in_the_callback() {
        let mut rig = Rig::new(48_000);
        rig.send(Command::Load { deck: 0, track: constant(0.1, 1.0, 48_000) });
        rig.send(Command::Play { deck: 0 });
        rig.callback(256, 2);
        // Built here, off the "audio thread", like the app builds them.
        let stems: Box<[Arc<Track>; deck::STEMS]> = Box::new(std::array::from_fn(|_| constant(0.025, 1.0, 48_000)));
        let stems_again: Box<[Arc<Track>; deck::STEMS]> = Box::new(std::array::from_fn(|_| constant(0.025, 1.0, 48_000)));
        let next = constant(0.2, 1.0, 48_000);
        let curve = Arc::new(Curve::new(0, vec![(0.0, 1.0)]));
        let curve_again = Arc::new(Curve::new(0, vec![(0.0, 0.9)]));
        let item = |seconds: f64| Box::new(playout::Item {
            track: constant(0.1, seconds, 48_000),
            start_frame: 0,
            offset: 0.0,
            duration: seconds,
            envelope: Arc::new(playout::Envelope::flat(1.0)),
        });
        let (short, long, longer) = (item(0.001), item(1.0), item(1.0));
        rig.send(Command::Stems { deck: 0, parts: stems });
        rig.send(Command::Stems { deck: 0, parts: stems_again });
        rig.send(Command::Automate { deck: 0, lane: Lane::Level, curve });
        rig.send(Command::Automate { deck: 0, lane: Lane::Level, curve: curve_again });
        rig.send(Command::Detach { deck: 0, lane: Lane::Level });
        rig.send(Command::Air { channel: 0, item: Some(short) });
        rig.send(Command::Air { channel: 1, item: Some(long) });
        rig.send(Command::Air { channel: 1, item: Some(longer) });
        rig.send(Command::Load { deck: 0, track: next });
        let mut out = vec![0.0f32; 512];
        let audit = audit::start();
        rig.console.render(&mut out, 2, &rig.telemetry);
        rig.console.render(&mut out, 2, &rig.telemetry);
        rig.send(Command::OffAir);
        rig.console.render(&mut out, 2, &rig.telemetry);
        let (allocations, frees) = audit.stop_with_frees();
        assert_eq!((allocations, frees), (0, 0), "the callback allocated or freed");
        // Two stem boxes, four stems replaced, two curves, three items, the
        // first record (after its fade) and the stems of the second load.
        assert!(rig.buried() >= 2 + 4 + 2 + 3 + 1 + 4);
    }

    #[test]
    fn the_whole_callback_path_does_not_allocate() {
        let mut rig = Rig::new(48_000);
        let track = sine(220.0, 0.3, 6.0, 48_000);
        rig.send(Command::Load { deck: 0, track: track.clone() });
        rig.send(Command::Load { deck: 1, track });
        rig.send(Command::Grid { deck: 0, anchor_seconds: 0.0, period_seconds: 0.5 });
        rig.send(Command::Grid { deck: 1, anchor_seconds: 0.1, period_seconds: 0.5 });
        rig.send(Command::Reverb { size: 0.8, damping: 0.4, predelay_seconds: 0.02, level: 0.5 });
        rig.callback(64, 2);
        let curves: Vec<(Lane, Arc<Curve>)> = vec![
            (Lane::Gain, Arc::new(Curve::new(1_000, vec![(0.0, 1.0), (20_000.0, 0.5)]))),
            (Lane::Low, Arc::new(Curve::new(0, vec![(0.0, 0.5), (30_000.0, 0.0)]))),
            (Lane::Sweep, Arc::new(Curve::new(0, vec![(0.0, 0.0), (30_000.0, 0.6)]))),
            (Lane::EchoSend, Arc::new(Curve::new(0, vec![(0.0, 1.0), (30_000.0, 0.0)]))),
            (Lane::EchoBeats, Arc::new(Curve::new(0, vec![(0.0, 0.5), (30_000.0, 0.25)]))),
            (Lane::ReverbSend, Arc::new(Curve::new(0, vec![(0.0, 0.0), (30_000.0, 1.0)]))),
            (Lane::Rate, Arc::new(Curve::new(40_000, vec![(0.0, 1.0), (10_000.0, 0.0)]))),
        ];
        let mut small = vec![0.0f32; 480 * 2];
        let mut large = vec![0.0f32; 3_000 * 2];
        // Queued before the audit starts: using up the list frees it, and
        // that is this thread tidying up, not the callback.
        for (lane, curve) in curves { rig.send(Command::Automate { deck: 0, lane, curve }); }
        let audit = audit::start();
        rig.send(Command::KeyLock { deck: 0, enabled: true });
        rig.send(Command::Speed { deck: 0, value: 1.04 });
        rig.send(Command::Echo { deck: 0, mix: 0.3, feedback: 0.5, seconds: 0.25 });
        rig.send(Command::PlayAt { deck: 0, frame: 300, source_seconds: 0.5 });
        rig.send(Command::Play { deck: 1 });
        rig.send(Command::Loop { deck: 1, range: Some((1.0, 1.75)) });
        rig.send(Command::LoopAt { deck: 0, frame: 20_000, length_seconds: 0.25, until_frame: 30_000 });
        rig.send(Command::LoopAt { deck: 0, frame: 25_000, length_seconds: 0.125, until_frame: 30_000 });
        for i in 0..120 {
            if i == 30 { rig.send(Command::SeekQuantized { deck: 1, seconds: 3.0 }); }
            if i == 40 { rig.send(Command::PhaseAlign { deck: 1, to_deck: 0 }); }
            if i == 50 { rig.send(Command::Pause { deck: 1 }); }
            if i == 60 { rig.send(Command::Limiter { enabled: false }); }
            if i == 70 { rig.send(Command::Seek { deck: 0, seconds: 1.0 }); }
            let out = if i % 2 == 0 { &mut small } else { &mut large };
            rig.console.render(out, 2, &rig.telemetry);
        }
        let (allocations, frees) = audit.stop_with_frees();
        assert_eq!(allocations, 0, "the callback allocated");
        assert_eq!(frees, 0, "the callback freed");
    }

    #[test]
    fn a_reopen_at_another_rate_keeps_the_record_and_the_playhead() {
        let mut rig = Rig::new(48_000);
        rig.send(Command::Load { deck: 0, track: sine(440.0, 0.3, 4.0, 48_000) });
        rig.send(Command::Play { deck: 0 });
        rig.send(Command::Tone { deck: 0, low: 0.2, mid: 0.5, high: 0.5, sweep: 0.0 });
        rig.callback(48_000, 2);
        let before = rig.telemetry.position(0);
        rig.console.set_rate(96_000);
        rig.callback(96_000, 2);
        let after = rig.telemetry.position(0);
        assert!((after - before - 1.0).abs() < 0.01, "{before} then {after}");
        assert!(rig.telemetry.playing(0));
        assert_eq!(rig.console.decks[0].strip.settings()[0], 0.2);
    }

    #[test]
    fn phase_align_puts_one_deck_on_the_other_decks_beat() {
        let mut rig = Rig::new(48_000);
        let track = constant(0.1, 8.0, 48_000);
        rig.send(Command::Load { deck: 0, track: track.clone() });
        rig.send(Command::Load { deck: 1, track });
        rig.send(Command::Grid { deck: 0, anchor_seconds: 0.0, period_seconds: 0.5 });
        rig.send(Command::Grid { deck: 1, anchor_seconds: 0.0, period_seconds: 0.5 });
        rig.send(Command::Seek { deck: 1, seconds: 0.2 });
        rig.send(Command::Play { deck: 0 });
        rig.send(Command::Play { deck: 1 });
        rig.callback(4_800, 2);
        rig.send(Command::PhaseAlign { deck: 1, to_deck: 0 });
        rig.callback(480, 2);
        let a = rig.console.decks[0].phase().unwrap();
        let b = rig.console.decks[1].phase().unwrap();
        assert!((a - b).abs() < 1e-6, "phases {a} and {b}");
    }

    #[test]
    fn the_reverb_tail_outlives_the_deck_that_fed_it() {
        let mut rig = Rig::new(48_000);
        rig.send(Command::Load { deck: 0, track: sine(300.0, 0.5, 2.0, 48_000) });
        rig.send(Command::Reverb { size: 0.8, damping: 0.3, predelay_seconds: 0.0, level: 1.0 });
        rig.send(Command::ReverbSend { deck: 0, value: 1.0 });
        rig.send(Command::Play { deck: 0 });
        rig.callback(24_000, 2);
        rig.send(Command::Gain { deck: 0, value: 0.0 });
        rig.send(Command::Pause { deck: 0 });
        rig.callback(4_800, 2);
        let tail = rig.callback(9_600, 2);
        let loud = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(loud > 0.005, "the reverb stopped with the deck: {loud}");
    }

    /// Everything on at once: two key-locked separated decks, EQ and sweep
    /// automated, echo, reverb, a roll, the limiter. Ignored because it
    /// measures time rather than correctness.
    ///
    ///     cargo test --bin defalt busy_console -- --ignored --nocapture
    #[test]
    #[ignore = "timing, not correctness"]
    fn a_busy_console_renders_far_faster_than_real_time() {
        let mut rig = Rig::new(48_000);
        for deck in 0..DECKS {
            let track = sine(220.0 + deck as f64 * 110.0, 0.3, 30.0, 48_000);
            rig.send(Command::Load { deck, track: track.clone() });
            rig.send(Command::Stems { deck, parts: Box::new(std::array::from_fn(|_| track.clone())) });
            rig.send(Command::KeyLock { deck, enabled: true });
            rig.send(Command::Speed { deck, value: 1.03 });
            rig.send(Command::Tone { deck, low: 0.3, mid: 0.6, high: 0.4, sweep: 0.2 });
            rig.send(Command::Echo { deck, mix: 0.3, feedback: 0.5, seconds: 0.3 });
            rig.send(Command::ReverbSend { deck, value: 0.4 });
            rig.send(Command::Automate { deck, lane: Lane::Mid,
                curve: Arc::new(Curve::new(0, vec![(0.0, 0.5), (480_000.0, 0.0)])) });
            rig.send(Command::Play { deck });
        }
        rig.send(Command::Reverb { size: 0.8, damping: 0.4, predelay_seconds: 0.02, level: 0.4 });
        rig.send(Command::LoopAt { deck: 0, frame: 96_000, length_seconds: 0.25, until_frame: 192_000 });
        let mut out = vec![0.0f32; 512 * 2];
        let began = std::time::Instant::now();
        let callbacks = 48_000 * 10 / 512;
        for _ in 0..callbacks { rig.console.render(&mut out, 2, &rig.telemetry); }
        let elapsed = began.elapsed().as_secs_f64();
        println!("10 s of a busy console in {elapsed:.3} s ({:.0}x real time)", 10.0 / elapsed);
        assert!(elapsed < 2.0, "the console is too slow to be safe: {elapsed:.3} s for 10 s");
    }

    #[test]
    fn a_poisoned_console_keeps_playing() {
        let rig = Rig::new(48_000);
        let (mut commands, telemetry) = (rig.commands, rig.telemetry);
        let shared: Shared = Arc::new(Mutex::new(Some(Box::new(rig.console))));
        assert!(commands.push((1, Command::Load { deck: 0, track: constant(0.25, 1.0, 48_000) })).is_ok());
        assert!(commands.push((2, Command::Play { deck: 0 })).is_ok());
        let holder = shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = holder.lock().unwrap();
            panic!("a render that went wrong");
        }).join();
        assert!(shared.is_poisoned());
        let mut out = vec![0.0f32; 8_192];
        fill(&shared, &mut out, 2, &telemetry);
        fill(&shared, &mut out, 2, &telemetry);
        assert!(!shared.is_poisoned(), "the poison was left for the next callback");
        assert!(out.iter().any(|s| s.abs() > 0.1), "a poisoned lock became permanent silence");
    }

    #[test]
    fn a_command_for_a_deck_that_does_not_exist_is_retired_not_dropped() {
        let mut rig = Rig::new(48_000);
        rig.send(Command::Load { deck: 7, track: constant(0.1, 0.1, 48_000) });
        rig.send(Command::Automate { deck: 9, lane: Lane::Gain, curve: Arc::new(Curve::new(0, vec![])) });
        rig.callback(64, 2);
        assert_eq!(rig.buried(), 2);
    }
}
