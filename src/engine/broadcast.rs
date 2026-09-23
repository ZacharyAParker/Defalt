//! The broadcast tap: exactly what the device is sent, copied out for the
//! stream.
//!
//! Taken after the master, the limiter and the final clamp, so a listener on
//! a phone hears every stem swap, spinback, roll and reverb tail the console
//! plays -- not an approximation of it. The callback side is a handful of
//! relaxed atomic stores into a ring that was allocated before the broadcast
//! began: no allocation, no lock, no syscall, and never a wait. A reader that
//! falls behind loses the oldest audio (and counts it) rather than holding
//! the callback up.
//!
//! The ring is a seqlock-style overwrite ring rather than rtrb because the
//! callback only ever sees the telemetry by shared reference, and because a
//! producer here must never fail: when the reader lags it is the reader who
//! skips ahead.

use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

/// Stereo frames the ring holds: about five seconds at 48 kHz, 1.3 at 192.
pub const CAPACITY: usize = 1 << 18;
/// Frames of headroom a reader keeps from the writer. The callback writes a
/// chunk (at most `super::CHUNK` frames) before it publishes it, so anything
/// this close to being lapped may already be half overwritten.
const SLACK: usize = 4 * super::CHUNK;

#[derive(Default)]
pub struct Tap {
    active: AtomicBool,
    mute_local: AtomicBool,
    ring: OnceLock<Box<[AtomicU32]>>,
    /// Frames ever committed. Only the callback writes it.
    written: AtomicU64,
    rate: AtomicU32,
}

impl Tap {
    /// Allocate the ring (once) and start copying. Call off the audio thread.
    pub fn start(&self) {
        self.ring.get_or_init(|| (0..CAPACITY * 2).map(|_| AtomicU32::new(0)).collect());
        self.active.store(true, Ordering::Release);
    }

    pub fn stop(&self) {
        self.active.store(false, Ordering::Release);
    }

    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Local speakers get silence while the tap is active; the stream still
    /// gets the mix. Has no effect while nothing is being broadcast.
    pub fn set_mute_local(&self, muted: bool) {
        self.mute_local.store(muted, Ordering::Relaxed);
    }

    pub fn mute_local(&self) -> bool {
        self.mute_local.load(Ordering::Relaxed)
    }

    /// The rate of the most recently committed audio, 0 before any.
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Acquire)
    }

    /// The callback's handle for one chunk, or None while not broadcasting.
    #[inline]
    pub fn begin(&self) -> Option<Writer<'_>> {
        if !self.active.load(Ordering::Relaxed) {
            return None;
        }
        let ring = self.ring.get()?;
        Some(Writer {
            ring,
            base: self.written.load(Ordering::Relaxed),
            mute_local: self.mute_local.load(Ordering::Relaxed),
            tap: self,
        })
    }

    /// A reader that starts at the live edge.
    pub fn reader(&self) -> Reader {
        Reader { cursor: self.written.load(Ordering::Acquire), overruns: 0, dropped: 0 }
    }
}

pub struct Writer<'a> {
    ring: &'a [AtomicU32],
    base: u64,
    mute_local: bool,
    tap: &'a Tap,
}

impl Writer<'_> {
    /// Frame `index` of this chunk. Returns what the device should play.
    #[inline]
    pub fn put(&self, index: usize, left: f32, right: f32) -> (f32, f32) {
        let slot = ((self.base as usize).wrapping_add(index) % CAPACITY) * 2;
        self.ring[slot].store(left.to_bits(), Ordering::Relaxed);
        self.ring[slot + 1].store(right.to_bits(), Ordering::Relaxed);
        if self.mute_local { (0.0, 0.0) } else { (left, right) }
    }

    /// Publish the chunk's `frames`, rendered at `rate`.
    #[inline]
    pub fn commit(self, frames: usize, rate: u32) {
        self.tap.rate.store(rate, Ordering::Relaxed);
        self.tap.written.store(self.base + frames as u64, Ordering::Release);
    }
}

/// The encoder's end. Not shared: one reader per broadcast.
pub struct Reader {
    cursor: u64,
    /// Times the reader was lapped, and the frames that cost.
    pub overruns: u64,
    pub dropped: u64,
}

impl Reader {
    /// Append everything new to `out` as interleaved stereo f32 and return
    /// the number of frames appended.
    pub fn read(&mut self, tap: &Tap, out: &mut Vec<f32>) -> usize {
        let Some(ring) = tap.ring.get() else { return 0 };
        let written = tap.written.load(Ordering::Acquire);
        if written < self.cursor {
            // Only a fresh tap counts from zero again; start over at its edge.
            self.cursor = written;
            return 0;
        }
        let reach = (CAPACITY - SLACK) as u64;
        if written - self.cursor > reach {
            self.lapped(written - reach);
        }
        let from = self.cursor;
        let start = out.len();
        for frame in from..written {
            let slot = (frame as usize % CAPACITY) * 2;
            out.push(f32::from_bits(ring[slot].load(Ordering::Relaxed)));
            out.push(f32::from_bits(ring[slot + 1].load(Ordering::Relaxed)));
        }
        // Anything the writer may have reached while we copied is suspect.
        fence(Ordering::Acquire);
        let now = tap.written.load(Ordering::Relaxed);
        let safe = (now + SLACK as u64).saturating_sub(CAPACITY as u64);
        if safe > from {
            let torn = (safe - from).min(written - from) as usize;
            out.drain(start..start + torn * 2);
            self.overruns += 1;
            self.dropped += torn as u64;
        }
        self.cursor = written;
        (out.len() - start) / 2
    }

    fn lapped(&mut self, to: u64) {
        self.overruns += 1;
        self.dropped += to - self.cursor;
        self.cursor = to;
    }
}

#[cfg(test)]
mod tests {
    use super::super::{audit, decode::Track, Command, Console, Telemetry, GRAVEYARD};
    use super::*;
    use std::sync::Arc;

    fn console(rate: u32) -> (Console, rtrb::Producer<(u64, Command)>, rtrb::Consumer<super::super::Retired>) {
        let (commands, inbox) = rtrb::RingBuffer::new(1024);
        let (graveyard, undertaker) = rtrb::RingBuffer::new(GRAVEYARD);
        (Console::new(rate, inbox, graveyard), commands, undertaker)
    }

    fn tone(rate: u32) -> Arc<Track> {
        let samples = (0..rate as usize * 2).flat_map(|i| {
            let v = (std::f64::consts::TAU * 330.0 * i as f64 / rate as f64).sin() as f32 * 0.3;
            [v, -v]
        }).collect();
        Arc::new(Track { samples, sample_rate: rate })
    }

    #[test]
    fn the_tap_hears_exactly_what_the_device_plays_without_allocating() {
        let (mut console, mut commands, _undertaker) = console(48_000);
        let telemetry = Telemetry::default();
        let _ = commands.push((1, Command::Load { deck: 0, track: tone(48_000) }));
        let _ = commands.push((2, Command::Play { deck: 0 }));
        let mut out = vec![0.0f32; 512 * 2];
        console.render(&mut out, 2, &telemetry);
        telemetry.broadcast.start();
        let mut reader = telemetry.broadcast.reader();
        let mut heard = Vec::with_capacity(8 * 512 * 2);
        let audit = audit::start();
        for _ in 0..8 {
            console.render(&mut out, 2, &telemetry);
            heard.extend_from_slice(&out);
        }
        let (allocations, frees) = audit.stop_with_frees();
        assert_eq!((allocations, frees), (0, 0), "the tap allocated or freed in the callback");
        let mut tapped = Vec::new();
        assert_eq!(reader.read(&telemetry.broadcast, &mut tapped), 8 * 512);
        assert_eq!(tapped, heard);
        assert!(heard.iter().any(|s| s.abs() > 0.1));
        assert_eq!(telemetry.broadcast.rate(), 48_000);
        assert_eq!(reader.overruns, 0);
    }

    #[test]
    fn muting_the_speakers_keeps_the_stream() {
        let (mut console, mut commands, _undertaker) = console(48_000);
        let telemetry = Telemetry::default();
        let _ = commands.push((1, Command::Load { deck: 0, track: tone(48_000) }));
        let _ = commands.push((2, Command::Play { deck: 0 }));
        telemetry.broadcast.start();
        telemetry.broadcast.set_mute_local(true);
        let mut reader = telemetry.broadcast.reader();
        let mut out = vec![0.0f32; 1024 * 2];
        console.render(&mut out, 2, &telemetry);
        console.render(&mut out, 2, &telemetry);
        assert!(out.iter().all(|&s| s == 0.0), "the speakers were not muted");
        let mut tapped = Vec::new();
        reader.read(&telemetry.broadcast, &mut tapped);
        assert!(tapped.iter().any(|s| s.abs() > 0.1), "the stream went quiet too");

        // Off the air, muting means nothing: the room hears the console.
        telemetry.broadcast.stop();
        console.render(&mut out, 2, &telemetry);
        assert!(out.iter().any(|s| s.abs() > 0.1));
    }

    #[test]
    fn nothing_is_copied_while_no_one_is_listening() {
        let tap = Tap::default();
        assert!(tap.begin().is_none());
        tap.start();
        tap.stop();
        assert!(tap.begin().is_none());
    }

    #[test]
    fn a_lagging_reader_loses_the_oldest_audio_and_counts_it() {
        let tap = Tap::default();
        tap.start();
        let mut reader = tap.reader();
        let chunk = 2048;
        let total = CAPACITY + 8 * chunk;
        let mut frame = 0usize;
        while frame < total {
            let writer = tap.begin().unwrap();
            for i in 0..chunk {
                writer.put(i, (frame + i) as f32, 0.0);
            }
            writer.commit(chunk, 44_100);
            frame += chunk;
        }
        let mut out = Vec::new();
        let got = reader.read(&tap, &mut out);
        assert_eq!(reader.overruns, 1);
        assert_eq!(got as u64 + reader.dropped, total as u64);
        assert!(got <= CAPACITY - SLACK);
        // What survived is the newest audio, in order, ending at the edge.
        assert_eq!(out[out.len() - 2], (total - 1) as f32);
        assert!(out.chunks(2).zip(out.chunks(2).skip(1)).all(|(a, b)| b[0] == a[0] + 1.0));
        assert_eq!(tap.rate(), 44_100);

        // Caught up, it carries on with nothing lost.
        let writer = tap.begin().unwrap();
        writer.put(0, -1.0, -1.0);
        writer.commit(1, 44_100);
        out.clear();
        assert_eq!(reader.read(&tap, &mut out), 1);
        assert_eq!(reader.overruns, 1);
    }
}
