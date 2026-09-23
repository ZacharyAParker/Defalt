//! The hosts' voices, on the console's small speech bus.
//!
//! Speech is the one thing that cannot go on a deck, because a deck holds a
//! record. Host lines are decoded here -- the bus takes a track, not a path
//! -- and placed on a channel at the exact output frame their line airs.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use crate::engine::decode::Track;
use crate::engine::playout::{Envelope, Item};
use crate::engine::Command;

use super::clock::Clock;
use super::protocol::Scheduled;

/// The voice bus is small: hosts talk one at a time, and two is enough for one
/// line to tail out under the next.
pub const VOICE_CHANNELS: usize = 3;

pub enum Fetched {
    Voice { id: String, track: Arc<Track> },
    Failed { id: String, error: String },
}

pub struct Voices {
    /// Lines on the bus, and the channel each took.
    pub on_air: HashMap<String, usize>,
    pub free: Vec<usize>,
    /// Lines being decoded.
    pub pending: HashSet<String>,
    fetch_out: Sender<Fetched>,
    fetches: Receiver<Fetched>,
}

impl Default for Voices {
    fn default() -> Self {
        let (fetch_out, fetches) = channel();
        Voices {
            on_air: HashMap::new(),
            free: (0..VOICE_CHANNELS).collect(),
            pending: HashSet::new(),
            fetch_out,
            fetches,
        }
    }
}

impl Voices {
    /// Everything back: nothing on the bus, nothing waiting.
    pub fn clear(&mut self) {
        self.on_air.clear();
        self.free = (0..VOICE_CHANNELS).collect();
        self.pending.clear();
    }

    /// Off the bus, but lines still decoding stay decoding.
    pub fn clear_air(&mut self) {
        self.on_air.clear();
        self.free = (0..VOICE_CHANNELS).collect();
    }

    pub fn wanted(&self, id: &str) -> bool {
        !self.on_air.contains_key(id) && !self.pending.contains(id)
    }

    /// Decode a line off the UI thread. A decoder that panics still reports
    /// back, or the line would be pending for ever.
    pub fn fetch(&mut self, id: &str, path: PathBuf) {
        self.pending.insert(id.to_string());
        let sender = self.fetch_out.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let decoded = std::panic::catch_unwind(|| crate::engine::decode::load(&path))
                .unwrap_or_else(|_| Err("decoding a host line crashed".into()));
            let message = match decoded {
                Ok(track) => Fetched::Voice { id, track },
                Err(error) => Fetched::Failed { id, error },
            };
            let _ = sender.send(message);
        });
    }

    pub fn arrived(&mut self) -> Vec<Fetched> {
        let mut out = Vec::new();
        while let Ok(message) = self.fetches.try_recv() {
            match &message {
                Fetched::Voice { id, .. } | Fetched::Failed { id, .. } => { self.pending.remove(id); }
            }
            out.push(message);
        }
        out
    }

    /// Put a decoded line on a channel, at the frame its item airs -- or, if
    /// that has passed, now and as far into the line as it should be.
    pub fn air(&mut self, item: &Scheduled, track: Arc<Track>, clock: &Clock, frame: u64)
        -> Result<Option<Command>, String> {
        let Some(target) = clock.frame_at_f(item.start_at) else { return Ok(None) };
        let Some(channel) = self.free.pop() else {
            return Err("More hosts talking than the voice bus can hold.".into());
        };
        let rate = clock.rate().max(1) as f64;
        let (start_frame, offset) = if target < frame as f64 {
            let late = (frame as f64 - target) / rate;
            (frame, item.offset + late)
        } else {
            (target.round() as u64, item.offset)
        };
        if offset >= track.seconds() {
            self.free.push(channel);
            return Ok(None);
        }
        let envelope = if item.envelope.is_empty() {
            Envelope::flat(1.0)
        } else {
            Envelope::new(item.envelope.clone())
        };
        self.on_air.insert(item.id.clone(), channel);
        Ok(Some(Command::Air {
            channel,
            item: Some(Box::new(Item {
                track,
                start_frame,
                offset,
                duration: (item.duration - (offset - item.offset)).max(0.0),
                envelope: Arc::new(envelope),
            })),
        }))
    }

    /// Give back the channels of lines that have finished.
    pub fn release_ended(&mut self, still: impl Fn(&str) -> bool) {
        let ended: Vec<String> = self.on_air.keys().filter(|id| !still(id)).cloned().collect();
        for id in ended {
            if let Some(channel) = self.on_air.remove(&id) {
                self.free.push(channel);
            }
        }
    }
}
