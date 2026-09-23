//! Getting records onto decks: decoding, stems, and the library itself.

use std::sync::Arc;

use crate::engine::{self, Command, DECKS};
use crate::library::{self, Record};
use crate::{assist, label, peaks, pull, Defalt, Loaded};

impl Defalt {
    /// Put a record you chose on a deck. If the station had that deck, it is
    /// yours now: its record there will not play, and every lane it was
    /// running lets go.
    pub fn load(&mut self, deck: usize, record: Record) {
        if deck >= DECKS {
            return;
        }
        if self.airtime.on_deck(deck).is_some() {
            self.airtime.release_deck(deck);
        }
        self.radio_trim[deck] = None;
        self.reset_lanes(deck, true);
        self.start_load(deck, record);
    }

    /// Decode off the UI thread. A five minute record takes a moment, and the
    /// console has to keep drawing while it happens. Returns the load's
    /// generation, which is what a finished decode is recognised by.
    pub(crate) fn start_load(&mut self, deck: usize, record: Record) -> u64 {
        self.decks[deck].loading = true;
        self.decks[deck].error = None;
        self.load_generation[deck] = self.load_generation[deck].wrapping_add(1);
        let generation = self.load_generation[deck];
        self.loading_key[deck] = Some(record.key.clone());
        self.splits[deck] = None;
        self.touch(deck);

        let outbox = self.outbox.clone();
        let path = record.file.clone();
        std::thread::spawn(move || {
            // A decoder that panics is a record that would not load, not a
            // deck that says "Loading..." until the window closes.
            let decoded = std::panic::catch_unwind(|| {
                engine::decode::load(&path).map(|track| {
                    let peaks = Arc::new(peaks::analyse(&track));
                    (track, peaks)
                })
            })
            .unwrap_or_else(|_| Err("decoding this record crashed".into()));
            let message = match decoded {
                Ok((track, peaks)) => Ok(Loaded { deck, record, track, peaks }),
                Err(error) => Err((deck, error)),
            };
            let _ = outbox.send((generation, message));
        });
        generation
    }

    pub(crate) fn collect_loads(&mut self) {
        while let Ok((generation, message)) = self.inbox.try_recv() {
            let index = match &message { Ok(loaded) => loaded.deck, Err((deck, _)) => *deck };
            if generation != self.load_generation[index] { continue; }
            match message {
                Ok(loaded) => {
                    let index = loaded.deck;
                    let key = loaded.record.key.clone();
                    let deck = &mut self.decks[index];
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
                    deck.loop_range = None;
                    deck.loop_in = None;
                    // Levels matched before the record comes in, which is the
                    // whole of assist's first job. A record the station put
                    // here carries the station's own trim instead: it has
                    // measured it already, and doing both would count the
                    // difference twice.
                    deck.trim = match self.radio_trim[index].take() {
                        Some(trim) => trim,
                        None if self.assist => assist::trim_for(deck.record.as_ref().and_then(|r| r.lufs)),
                        None => 1.0,
                    };
                    deck.sample_rate = loaded.track.sample_rate;
                    self.separated[index] = false;
                    self.splits[index] = None;
                    self.stem_gain[index] = [1.0; engine::deck::STEMS];
                    self.stem_muted[index] = [false; engine::deck::STEMS];
                    self.send(Command::Load { deck: index, track: loaded.track });
                    // A load clears the deck's grid; it goes back on at once.
                    self.send_grid(index);
                    self.apply_speed(index);
                    self.sent_tone[index] = None;
                    self.push_tone(index);
                    self.push_gains();
                    self.airtime.deck_ready(index, &key, generation);
                }
                Err((deck, error)) => {
                    self.decks[deck].loading = false;
                    self.decks[deck].error = Some(error);
                    if let Some(key) = self.loading_key[deck].take() {
                        self.airtime.deck_failed(deck, &key, generation);
                    }
                }
            }
        }
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
            Ok(job) => {
                self.splits[deck] = Some(job);
                self.split_cached_only[deck] = false;
            }
            Err(error) => self.say(&error),
        }
    }

    /// The same, for a technique that needs stems: only a separation already
    /// in the cache will do, because making one takes minutes and the
    /// transition is coming now.
    pub(crate) fn begin_cached_split(&mut self, deck: usize) {
        if self.decks[deck].loading || self.splits[deck].is_some() || self.separated[deck] {
            return;
        }
        let Some(file) = self.decks[deck].record.as_ref().map(|r| r.file.clone()) else { return };
        match pull::separate(&self.root, deck, &file) {
            Ok(job) => {
                self.splits[deck] = Some(job);
                self.split_cached_only[deck] = true;
            }
            Err(error) => crate::logfile::log!("stems: deck {} cannot look for cached stems: {error}", label(deck)),
        }
    }

    pub fn splitting(&self, deck: usize) -> Option<&pull::Separation> {
        self.splits[deck].as_ref()
    }

    pub fn set_stem_gain(&mut self, deck: usize, stem: usize, value: f32) {
        self.stem_gain[deck][stem] = value.clamp(0.0, 1.0);
        let value = self.stem_gain[deck][stem];
        // A technique's stem lane lets go of the part you took.
        if let Some(lane) = stem_lane(stem) {
            self.send(Command::Detach { deck, lane });
        }
        self.send(Command::StemGain { deck, stem, value });
    }

    pub fn toggle_stem_mute(&mut self, deck: usize, stem: usize) {
        let muted = !self.stem_muted[deck][stem];
        self.stem_muted[deck][stem] = muted;
        self.send(Command::StemMute { deck, stem, muted });
    }

    pub(crate) fn poll_splits(&mut self) {
        for deck in 0..DECKS {
            let Some(job) = self.splits[deck].as_mut() else { continue };
            job.poll();
            let cached_only = self.split_cached_only[deck];
            match job.stage.clone() {
                pull::Split::Done { parts, .. } => {
                    self.splits[deck] = None;
                    self.load_parts(deck, parts);
                }
                pull::Split::Failed { error } => {
                    self.splits[deck] = None;
                    if cached_only {
                        crate::logfile::log!("stems: deck {} has no cached stems ({error}); its stem lanes will do nothing", label(deck));
                    } else {
                        self.say(&error);
                    }
                }
                // Not in the cache: it has started separating, which a
                // radio transition cannot wait for. Dropping the job ends it.
                pull::Split::Working { device } if cached_only && !device.is_empty() => {
                    self.splits[deck] = None;
                    crate::logfile::log!("stems: deck {} is not separated yet; its stem lanes will do nothing", label(deck));
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
            let decoded = std::panic::catch_unwind(|| {
                let mut decoded = Vec::with_capacity(engine::deck::STEMS);
                for path in parts.in_order() {
                    decoded.push(engine::decode::load(path)?);
                }
                Ok::<_, String>(decoded)
            })
            .unwrap_or_else(|_| Err("decoding the stems crashed".into()));
            let message = match decoded.and_then(|parts| {
                <[Arc<engine::decode::Track>; engine::deck::STEMS]>::try_from(parts)
                    .map_err(|_| "a stem was missing".to_string())
            }) {
                Ok(parts) => Ok((deck, Box::new(parts))),
                Err(error) => Err((deck, error)),
            };
            let _ = outbox.send((generation, message));
        });
    }

    pub(crate) fn collect_stems(&mut self) {
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
                            "Deck {}: the parts came back at {} Hz for a {} Hz record. Separate it again.",
                            label(deck), part.sample_rate, expected));
                        continue;
                    }
                    self.separated[deck] = true;
                    self.stem_gain[deck] = [1.0; engine::deck::STEMS];
                    self.stem_muted[deck] = [false; engine::deck::STEMS];
                    self.send(Command::Stems { deck, parts });
                    if !self.split_cached_only[deck] {
                        self.say("Separated.");
                    }
                }
                Err((deck, error)) => {
                    self.separated[deck] = false;
                    self.say(&format!("Deck {}: {error}", label(deck)));
                }
            }
        }
    }

    /// Read the library again, off the UI thread.
    pub fn reload_library(&mut self) {
        self.library_inbox = Some(library::load_in_background(&self.root));
    }

    /// Take the library once it has been read.
    pub(crate) fn collect_library(&mut self) {
        let Some(inbox) = self.library_inbox.as_ref() else { return };
        let result = match inbox.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("reading the library stopped".into()),
        };
        self.library_inbox = None;
        match result {
            Ok(records) => {
                let selected_key = self.selected.and_then(|i| self.records.get(i)).map(|r| r.key.clone());
                self.records = records;
                self.view_state.records_generation += 1;
                self.selected = selected_key.and_then(|key| self.records.iter().position(|r| r.key == key));
                self.library_error = None;
            }
            Err(error) => self.library_error = Some(error),
        }
    }

    /// Block until the library is in, for the things that cannot go on
    /// without it: a posed screenshot, a test.
    pub(crate) fn wait_for_library(&mut self) {
        let began = std::time::Instant::now();
        while self.library_inbox.is_some() && began.elapsed().as_secs() < 30 {
            self.collect_library();
            if self.library_inbox.is_some() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

fn stem_lane(stem: usize) -> Option<engine::Lane> {
    engine::Lane::ALL.into_iter().find(|lane| lane.stem() == Some(stem))
}
