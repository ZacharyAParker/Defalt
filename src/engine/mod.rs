//! The audio engine.
//!
//! One thread owns the output stream and the decks. Everything else talks to
//! it by pushing commands into a lock-free ring, and reads what it is doing
//! back out of atomics. Nothing on the far side of that boundary ever blocks
//! the audio callback -- no mutex, no allocation, no syscall -- because a
//! callback that misses its deadline is a click, and enough of them is a drop
//! out.

pub mod deck;
pub mod echo;
pub mod decode;
pub mod filters;
pub mod stretch;
pub mod playout;

#[cfg(test)]
mod soundcheck;

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig};

use deck::Deck;
use decode::Track;

/// Two for now. The layout is per-deck throughout so four is a constant change
/// rather than a rewrite.
pub const DECKS: usize = 2;

pub enum Command {
    Load { deck: usize, track: Arc<Track> },
    /// The same record, taken apart. Arrives later than the record does:
    /// separation takes seconds and a deck must be playable before it lands.
    Stems { deck: usize, parts: Box<[Arc<Track>; deck::STEMS]> },
    StemGain { deck: usize, stem: usize, value: f32 },
    StemMute { deck: usize, stem: usize, muted: bool },
    Play { deck: usize },
    Pause { deck: usize },
    Seek { deck: usize, seconds: f64 },
    Gain { deck: usize, value: f32 },
    Speed { deck: usize, value: f64 },
    KeyLock { deck: usize, enabled: bool },
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
    /// air does not move anything you set by hand.
    AirGain { value: f32 },
}

/// What the audio thread is doing, readable from anywhere without a lock.
///
/// Positions are f64 bit patterns in an AtomicU64. Ugly, but a torn read of a
/// playhead would show the UI a position that never existed.
#[derive(Default)]
pub struct Telemetry {
    position: [AtomicU64; DECKS],
    playing: [AtomicBool; DECKS],
    loaded: [AtomicBool; DECKS],
    separated: [AtomicBool; DECKS],
    length: [AtomicU64; DECKS],
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

    /// Only the soundcheck reads this, which is enough reason to keep it:
    /// "the deck has a record" is exactly what that test needs to assert.
    #[cfg_attr(not(test), allow(dead_code))]
    #[cfg_attr(not(test), allow(dead_code))]
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
    /// than every callback.
    pub fn frame(&self) -> u64 {
        self.frame.load(Ordering::Relaxed)
    }

    pub fn air_peak(&self) -> f32 {
        f32::from_bits(self.air_peak.load(Ordering::Relaxed))
    }

    pub fn voice_peaks(&self) -> [f32; playout::CHANNELS] {
        std::array::from_fn(|i| f32::from_bits(self.voice_peaks[i].load(Ordering::Relaxed)))
    }

    pub fn peak(&self) -> [f32; 2] {
        [
            f32::from_bits(self.peak[0].load(Ordering::Relaxed)),
            f32::from_bits(self.peak[1].load(Ordering::Relaxed)),
        ]
    }
}

pub struct Engine {
    commands: rtrb::Producer<Command>,
    pub telemetry: Arc<Telemetry>,
    pub sample_rate: u32,
    pub device: String,
}

impl Engine {
    pub fn start() -> Result<Self, String> {
        // Deep enough that a burst of UI events cannot fill it, small enough
        // to notice if something is spraying commands.
        let (commands, inbox) = rtrb::RingBuffer::<Command>::new(1024);
        // Records the audio thread has finished with, returned to be dropped
        // somewhere it is allowed to take its time.
        // Five per load now, not one: a record and its four stems.
        let (graveyard, undertaker) = rtrb::RingBuffer::<Arc<Track>>::new(256);

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

        Ok(Engine { commands, telemetry, sample_rate, device })
    }

    /// Commands are dropped rather than queued if the ring is full. A UI that
    /// has run 1024 commands behind is not going to be helped by the next one.
    pub fn send(&mut self, command: Command) -> Result<(), String> {
        self.commands
            .push(command)
            .map_err(|_| "the audio thread is not keeping up".to_string())
    }
}

type Started = Result<(u32, String), String>;

fn audio_thread(
    inbox: rtrb::Consumer<Command>,
    graveyard: rtrb::Producer<Arc<Track>>,
    mut undertaker: rtrb::Consumer<Arc<Track>>,
    telemetry: Arc<Telemetry>,
    ready: mpsc::Sender<Started>,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        let _ = ready.send(Err("no output device".into()));
        return;
    };
    let name = device.id().map(|id| id.to_string()).unwrap_or_else(|_| "output".into());

    let config = match device.default_output_config() {
        Ok(config) => config,
        Err(error) => {
            let _ = ready.send(Err(format!("no usable output config: {error}")));
            return;
        }
    };
    let format = config.sample_format();
    let config: StreamConfig = config.into();
    let sample_rate = config.sample_rate;

    let built = match format {
        SampleFormat::F32 => build::<f32>(&device, config, inbox, graveyard, telemetry),
        SampleFormat::I16 => build::<i16>(&device, config, inbox, graveyard, telemetry),
        SampleFormat::I32 => build::<i32>(&device, config, inbox, graveyard, telemetry),
        SampleFormat::U16 => build::<u16>(&device, config, inbox, graveyard, telemetry),
        other => Err(format!("unsupported sample format {other}")),
    };

    let stream = match built {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if let Err(error) = stream.play() {
        let _ = ready.send(Err(format!("could not start the stream: {error}")));
        return;
    }

    let _ = ready.send(Ok((sample_rate, name)));

    // The stream is not Send on Windows, so it has to live and die here. While
    // we are holding it anyway, this is also where retired records get freed.
    loop {
        while undertaker.pop().is_ok() {
            // Dropping the Arc here is the entire point.
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut inbox: rtrb::Consumer<Command>,
    mut graveyard: rtrb::Producer<Arc<Track>>,
    telemetry: Arc<Telemetry>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate;

    let mut decks: [Deck; DECKS] = std::array::from_fn(|_| Deck::new(sample_rate));
    let mut air = playout::Playout::default();
    let mut master = 1.0_f32;
    // Scratch bus, allocated once here and never inside the callback.
    let mut bus = vec![0.0_f32; 4096];

    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &OutputCallbackInfo| {
                apply(&mut inbox, &mut decks, &mut air, &mut master, &mut graveyard,
                      sample_rate);

                let frames = out.len() / channels.max(1);
                if bus.len() < frames * 2 {
                    // Only reachable if the driver hands us a bigger buffer
                    // than it advertised. Silence beats a reallocation here.
                    for sample in out.iter_mut() {
                        *sample = T::from_sample(0.0);
                    }
                    telemetry.underruns.fetch_add(1, Ordering::Relaxed);
                    return;
                }

                let bus = &mut bus[..frames * 2];
                bus.fill(0.0);
                for deck in decks.iter_mut() {
                    deck.mix_into(bus, sample_rate);
                }
                // The radio goes through the master like everything else, so
                // one fader still takes the whole console down.
                air.mix_into(bus, sample_rate, |track| {
                    let _ = graveyard.push(track);
                });

                let mut peak = [0.0_f32; 2];
                for (index, frame) in bus.chunks_exact_mut(2).enumerate() {
                    let left = (frame[0] * master).clamp(-1.0, 1.0);
                    let right = (frame[1] * master).clamp(-1.0, 1.0);
                    peak[0] = peak[0].max(left.abs());
                    peak[1] = peak[1].max(right.abs());

                    let slot = &mut out[index * channels..(index + 1) * channels];
                    for (channel, sample) in slot.iter_mut().enumerate() {
                        // Mono outputs get the left; anything wider than two
                        // gets silence past the pair rather than a duplicate.
                        *sample = T::from_sample(match channel {
                            0 => left,
                            1 => right,
                            _ => 0.0,
                        });
                    }
                }

                telemetry.frame.store(air.frame, Ordering::Relaxed);
                telemetry.air_peak.store(air.peak.to_bits(), Ordering::Relaxed);
                air.peak = 0.0;
                for (i, peak) in air.channel_peaks.iter_mut().enumerate() {
                    telemetry.voice_peaks[i].store(peak.to_bits(), Ordering::Relaxed);
                    *peak = 0.0;
                }
                telemetry.peak[0].store(peak[0].to_bits(), Ordering::Relaxed);
                telemetry.peak[1].store(peak[1].to_bits(), Ordering::Relaxed);
                for (index, deck) in decks.iter_mut().enumerate() {
                    telemetry.deck_peak[index]
                        .store(deck.peak.to_bits(), Ordering::Relaxed);
                    // Read and reset: the meter shows this callback's loudest
                    // moment, not the loudest since the record was loaded.
                    deck.peak = 0.0;
                    telemetry.set_position(index, deck.seconds());
                    telemetry.playing[index].store(deck.playing, Ordering::Relaxed);
                    telemetry.loaded[index]
                        .store(deck.track.is_some(), Ordering::Relaxed);
                    telemetry.separated[index]
                        .store(deck.stems.is_some(), Ordering::Relaxed);
                    let length = deck.track.as_ref().map_or(0.0, |track| track.seconds());
                    telemetry.length[index].store(length.to_bits(), Ordering::Relaxed);
                }
            },
            |error| eprintln!("audio: {error}"),
            None,
        )
        .map_err(|error| format!("could not open the output: {error}"))
}

/// Drain the command ring. Runs at the top of every callback.
fn apply(
    inbox: &mut rtrb::Consumer<Command>,
    decks: &mut [Deck; DECKS],
    air: &mut playout::Playout,
    master: &mut f32,
    graveyard: &mut rtrb::Producer<Arc<Track>>,
    sample_rate: u32,
) {
    while let Ok(command) = inbox.pop() {
        match command {
            Command::Master { value } => *master = value.clamp(0.0, 4.0),
            Command::Air { channel, item } => {
                let displaced = air.set(channel, item.map(|item| *item));
                retire(displaced, graveyard);
            }
            Command::OffAir => {
                air.clear(|track| { let _ = graveyard.push(track); });
                for slot in decks.iter_mut() { slot.echo.clear(); }
            },
            Command::AirGain { value } => air.gain = value.clamp(0.0, 4.0),
            Command::Load { deck, track } => {
                if let Some(slot) = decks.get_mut(deck) {
                    retire(slot.track.take(), graveyard);
                    // The old stems belong to the old record.
                    for old in slot.stems.take().into_iter().flatten() {
                        retire(Some(old), graveyard);
                    }
                    slot.position = 0.0;
                    slot.playing = false;
                    slot.scrub = None;
                    slot.stem_gain = [1.0; deck::STEMS];
                    slot.stem_muted = [false; deck::STEMS];
                    slot.track = Some(track);
                    slot.echo.clear();
                    slot.strip = filters::Strip::new(sample_rate);
                    slot.reset_stretch();
                }
            }
            Command::Play { deck } => {
                if let Some(slot) = decks.get_mut(deck) {
                    slot.playing = slot.track.is_some();
                }
            }
            Command::Pause { deck } => {
                if let Some(slot) = decks.get_mut(deck) {
                    slot.playing = false;
                }
            }
            Command::Seek { deck, seconds } => {
                if let Some(slot) = decks.get_mut(deck) {
                    let rate = slot
                        .track
                        .as_ref()
                        .map_or(sample_rate, |track| track.sample_rate);
                    let frames = slot.track.as_ref().map_or(0, |track| track.frames());
                    slot.position =
                        (seconds * rate as f64).clamp(0.0, frames as f64);
                    slot.reset_stretch();
                }
            }
            Command::Gain { deck, value } => {
                if let Some(slot) = decks.get_mut(deck) {
                    slot.gain = value.clamp(0.0, 2.0);
                }
            }
            Command::Speed { deck, value } => {
                if let Some(slot) = decks.get_mut(deck) {
                    slot.speed = value.clamp(-4.0, 4.0);
                }
            }
            Command::KeyLock { deck, enabled } => {
                if let Some(slot) = decks.get_mut(deck) { slot.key_lock = enabled; }
            }
            Command::Echo { deck, mix, feedback, seconds } => {
                if let Some(slot) = decks.get_mut(deck) { slot.echo.set(mix, feedback, seconds, sample_rate); }
            }
            Command::Scrub { deck, rate } => {
                if let Some(slot) = decks.get_mut(deck) {
                    slot.scrub = rate.map(|value| value.clamp(-16.0, 16.0));
                }
            }
            Command::Stems { deck, parts } => {
                if let Some(slot) = decks.get_mut(deck) {
                    for old in slot.stems.take().into_iter().flatten() {
                        retire(Some(old), graveyard);
                    }
                    slot.stems = Some(*parts);
                }
            }
            Command::StemGain { deck, stem, value } => {
                if let Some(slot) = decks.get_mut(deck) {
                    if let Some(gain) = slot.stem_gain.get_mut(stem) {
                        *gain = value.clamp(0.0, 2.0);
                    }
                }
            }
            Command::StemMute { deck, stem, muted } => {
                if let Some(slot) = decks.get_mut(deck) {
                    if let Some(flag) = slot.stem_muted.get_mut(stem) {
                        *flag = muted;
                    }
                }
            }
            Command::Tone { deck, low, mid, high, sweep } => {
                if let Some(slot) = decks.get_mut(deck) {
                    slot.strip.set(low, mid, high, sweep);
                }
            }
        }
    }
}

/// Hand a finished record to another thread to be dropped.
///
/// If the graveyard is full we let it go here and eat the deallocation. That
/// is a real glitch, but it takes sixty-four evictions without a single 250ms
/// tick in between, and leaking would be worse.
fn retire(track: Option<Arc<Track>>, graveyard: &mut rtrb::Producer<Arc<Track>>) {
    if let Some(track) = track {
        let _ = graveyard.push(track);
    }
}
