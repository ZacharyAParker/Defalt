//! Which deck gets what, and how each deck is playing its part.
//!
//! The station decides the running order; this decides where it goes on the
//! desk. Records go onto whichever deck is free -- never one you are using --
//! and speech is fetched for the voice bus. And for the transitions, it
//! works out what each deck is really doing: the mix as planned, or a deck
//! carrying on alone because the other half of the mix is not there.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::library::Record;
use crate::DECKS;

use super::automation::{duck_at, merge_windows, Duck, Mode};
use super::protocol::{MixWindow, Scheduled};
use super::{Airtime, Assigned, DeckStatus, Plan, VOICE_LEAD_IN};

/// How long a record's audio may be missing before it is given up on.
const MISSING_GRACE: Duration = Duration::from_secs(5);
/// How often a missing record's audio is looked for again.
const MISSING_RETRY: Duration = Duration::from_secs(1);

impl Airtime {
    /// The deck to put the next record on.
    ///
    /// Whichever one is not spoken for -- and never one you are using. A deck
    /// playing something you loaded yourself is yours, autopilot or not, so it
    /// waits for the other one rather than pulling the record out from under
    /// you. Among free decks it takes the silent one, so a record is cued up
    /// against the one that is still playing.
    pub(super) fn free_deck(&self, decks: [DeckStatus; DECKS]) -> Option<usize> {
        let spare = (0..DECKS)
            .filter(|deck| self.decks[*deck].is_none())
            .filter(|deck| !decks[*deck].playing);
        let mut first = None;
        for deck in spare {
            if !decks[deck].loaded {
                return Some(deck);
            }
            first.get_or_insert(deck);
        }
        first
    }

    /// Give upcoming items a deck (or a voice channel) and start fetching.
    pub(super) fn assign(&mut self, records: &[Record], decks: [DeckStatus; DECKS], plan: &mut Plan) {
        let now = self.station_now;
        // Sorted, so the earliest record takes the first free deck. The
        // station sends them in order, but nothing here should depend on
        // that. Indices, not copies: this runs every frame.
        let mut due: Vec<usize> = (0..self.schedule.len())
            .filter(|&i| {
                let item = &self.schedule[i];
                item.ends_at() > now
                    && !self.failed.contains(&item.id)
                    && (item.is_music() || (item.start_at < now + VOICE_LEAD_IN && self.voices.wanted(&item.id)))
            })
            .collect();
        due.sort_by(|a, b| self.schedule[*a].start_at.total_cmp(&self.schedule[*b].start_at));

        for index in due {
            let item = &self.schedule[index];
            if item.is_music() {
                if self.decks.iter().flatten().any(|d| d.id == item.id) {
                    continue;
                }
                let Some(deck) = item.preferred_deck.filter(|d| *d < DECKS && self.decks[*d].is_none() && !decks[*d].playing)
                    .or_else(|| self.free_deck(decks)) else {
                    continue; // Both spoken for; it gets one when a record ends.
                };
                let Some(path) = self.find(index, records) else {
                    self.missing(index);
                    continue;
                };
                let item = &self.schedule[index];
                // Straight into the plan: `Defalt::load` already decodes off
                // the UI thread, so there is nothing here worth a thread of
                // its own.
                let record = records.iter().find(|r| r.key == item.key && r.file == path)
                    .cloned().unwrap_or_else(|| item.record(path));
                self.decks[deck] = Some(Assigned::new(&item.id, &record.key));
                self.sent[deck] = None;
                plan.load.push((deck, record, item.trim_db as f32));
            } else {
                let Some(path) = self.find(index, records) else { continue };
                let id = self.schedule[index].id.clone();
                self.voices.fetch(&id, path);
            }
        }
    }

    /// Where an item's audio is, remembered once found: this is asked every
    /// frame, and each answer is a trip to the disk.
    fn find(&mut self, index: usize, records: &[Record]) -> Option<PathBuf> {
        let item = &self.schedule[index];
        if let Some(path) = self.resolved.get(&item.id) {
            return Some(path.clone());
        }
        if let Some((_, tried)) = self.missing.get(&item.id) {
            if tried.elapsed() < MISSING_RETRY {
                return None;
            }
        }
        let found = self.resolve(item, records);
        let id = item.id.clone();
        match &found {
            Some(path) => {
                self.missing.remove(&id);
                self.resolved.insert(id, path.clone());
            }
            None => {
                let first = self.missing.get(&id).map_or_else(Instant::now, |(first, _)| *first);
                self.missing.insert(id, (first, Instant::now()));
            }
        }
        found
    }

    /// A record whose audio cannot be found: say so once, give it a moment
    /// (a download may be landing), then treat it like a record that would
    /// not load -- skipped if it is due, taken out of the queue if not.
    fn missing(&mut self, index: usize) {
        let item = &self.schedule[index];
        let Some((first, _)) = self.missing.get(&item.id).copied() else { return };
        let due = item.start_at <= self.station_now + 2.0;
        if first.elapsed() < MISSING_GRACE && !due {
            if self.said_missing.insert(item.id.clone()) {
                self.note = Some(format!("Cannot find the audio for {} yet.", item.title));
            }
            return;
        }
        let (id, title, started) = (item.id.clone(), item.title.clone(), item.start_at <= self.station_now);
        crate::logfile::log!("airtime: no audio for {id} ({title}); giving up on it");
        self.failed.insert(id.clone());
        self.missing.remove(&id);
        self.give_up(&id, started);
        self.note = Some(format!("Could not find the audio for {title}, so it was skipped."));
    }

    /// A record that will not play: the station moves past it if it is due,
    /// or drops it from the queue if not.
    pub(super) fn give_up(&mut self, id: &str, due: bool) {
        if due {
            self.post("/api/skip", None);
        } else {
            self.queue_action(id, "remove");
        }
    }

    /// Where an item's audio actually is on this machine.
    ///
    /// The station says, these days: `meta.file` is the path it read. Older
    /// stations only serve it by basename under a media root, which is right
    /// for a browser and wrong here -- so then it is the cache when the
    /// station downloaded it, and your own library when it did not.
    pub(super) fn resolve(&self, item: &Scheduled, records: &[Record]) -> Option<PathBuf> {
        if let Some(file) = item.file.as_ref().filter(|f| f.is_absolute() && f.is_file()) {
            return Some(file.clone());
        }
        let url = &item.url;
        let name = super::percent_decode(url.rsplit('/').next()?);

        if url.contains("/media/voice/") {
            let path = self.root.join("cache").join("voice").join(&name);
            return path.is_file().then_some(path);
        }
        // A local track is served by its key.
        if url.contains("/media/track/") {
            if let Some(record) = records.iter().find(|r| r.key == name || r.key == item.key) {
                return Some(record.file.clone());
            }
        }

        let cached = self.root.join("cache").join("audio").join(&name);
        if cached.is_file() {
            return Some(cached);
        }
        records
            .iter()
            .find(|record| {
                record.file.file_name().and_then(|n| n.to_str()) == Some(name.as_str())
            })
            .map(|record| record.file.clone())
    }

    /* ── The mix ─────────────────────────────────────────────────────── */

    /// The music item that starts after this one and before it ends: the
    /// record this one mixes into.
    pub(super) fn next_music(&self, item: &Scheduled) -> Option<&Scheduled> {
        self.schedule.iter()
            .filter(|next| next.is_music() && next.id != item.id && next.start_at > item.start_at)
            .min_by(|a, b| a.start_at.total_cmp(&b.start_at))
            .filter(|next| next.start_at < item.ends_at())
    }

    /// The music item this one mixes in from.
    pub(super) fn previous_music(&self, item: &Scheduled) -> Option<&Scheduled> {
        self.schedule.iter()
            .filter(|prev| prev.is_music() && prev.id != item.id && prev.start_at < item.start_at)
            .max_by(|a, b| a.start_at.total_cmp(&b.start_at))
            .filter(|prev| prev.ends_at() > item.start_at)
    }

    pub(super) fn deck_of(&self, id: &str) -> Option<usize> {
        (0..DECKS).find(|deck| self.decks[*deck].as_ref().is_some_and(|a| a.id == id))
    }

    fn armed(&self, deck: usize) -> bool {
        self.decks[deck].as_ref().is_some_and(|a| a.ready && a.started)
    }

    /// When a deck's record was started late, if it was.
    fn late(&self, deck: usize) -> Option<f64> {
        let assigned = self.decks[deck].as_ref()?;
        let item = self.item(&assigned.id)?;
        assigned.armed_at.filter(|at| at - item.start_at > 0.25)
    }

    /// Is this deck's record actually sounding (or about to, on its frame)?
    pub(super) fn sounding(&self, deck: usize, decks: [DeckStatus; DECKS], now: f64) -> bool {
        let Some(assigned) = self.decks[deck].as_ref() else { return false };
        let Some(item) = self.item(&assigned.id) else { return false };
        let starts = assigned.armed_at.unwrap_or(item.start_at).max(item.start_at);
        assigned.ready && assigned.started && (decks[deck].playing || now < starts + 0.5)
    }

    /// How each deck is playing its part.
    pub(super) fn modes(&mut self, decks: [DeckStatus; DECKS]) -> [Mode; DECKS] {
        let now = self.station_now;
        let mut modes = [Mode::Normal; DECKS];
        for deck in 0..DECKS {
            let Some(item) = self.on_deck(deck) else {
                self.solo_since[deck] = None;
                continue;
            };
            // Going out: into a record that is not ready means playing on
            // alone rather than fading into nothing; into one that started
            // late means holding on until it has arrived.
            let outgoing = self.next_music(item).map(|next| match self.deck_of(&next.id) {
                Some(other) if self.armed(other) => match self.late(other) {
                    Some(started) => Mode::RecoveryOut {
                        started, window: (item.ends_at() - started).clamp(0.01, 0.5),
                    },
                    None => Mode::Normal,
                },
                _ => Mode::Solo { from: next.start_at },
            });
            // Coming in: if what it is mixing out of is not sounding, there
            // is nothing to blend with.
            let mut solo = None;
            let incoming = self.previous_music(item).filter(|prev| now < prev.ends_at()).map(|prev| {
                let there = self.deck_of(&prev.id).is_some_and(|other| self.sounding(other, decks, now));
                if now >= item.start_at && self.sounding(deck, decks, now) && !there {
                    solo = Some(());
                    return Mode::Normal;
                }
                match self.late(deck) {
                    Some(started) => Mode::RecoveryIn {
                        started, window: (prev.ends_at() - started).clamp(0.01, 0.5),
                    },
                    None => Mode::Normal,
                }
            });
            modes[deck] = if solo.is_some() {
                // Latched: the moment it went alone is the moment it stays.
                Mode::Solo { from: *self.solo_since[deck].get_or_insert(now) }
            } else {
                self.solo_since[deck] = None;
                match (incoming, outgoing) {
                    (Some(mode), _) if mode != Mode::Normal => mode,
                    (_, Some(mode)) => mode,
                    _ => Mode::Normal,
                }
            };
        }
        modes
    }

    /// The transition happening at `now`, as (outgoing deck, incoming deck,
    /// how far through it is).
    pub(super) fn overlap(&self, now: f64) -> Option<(usize, usize, f32)> {
        let mut sounding: Vec<(usize, &Scheduled)> = (0..DECKS)
            .filter_map(|deck| self.on_deck(deck).map(|item| (deck, item)))
            .filter(|(_, item)| item.start_at <= now && now < item.ends_at())
            .collect();
        if sounding.len() < 2 {
            return None;
        }
        sounding.sort_by(|a, b| a.1.start_at.total_cmp(&b.1.start_at));
        let (out_deck, out_item) = sounding[0];
        let (in_deck, in_item) = sounding[1];

        // The overlap runs from the incoming record's start to the outgoing
        // one's end, which is what the station means by it.
        let length = (out_item.ends_at() - in_item.start_at).max(0.001);
        let progress = ((now - in_item.start_at) / length).clamp(0.0, 1.0);
        Some((out_deck, in_deck, progress as f32))
    }

    /// How far the music is ducked under speech at `now`, for the panel.
    pub(super) fn speech_duck(&self, now: f64) -> f32 {
        let Some(item) = (0..DECKS).filter_map(|d| self.on_deck(d))
            .find(|i| !i.deck_envelope.is_empty()) else { return 1.0 };
        let duck = Duck::of(item);
        duck_at(&merge_windows(&self.speech, duck), duck, now)
    }

    pub fn transition_windows(&self, deck: usize) -> Vec<MixWindow> {
        let Some(item) = self.on_deck(deck) else { return Vec::new() };
        let mut music: Vec<_> = self.schedule.iter().filter(|i| i.is_music()).collect();
        music.sort_by(|a, b| a.start_at.total_cmp(&b.start_at));
        music.windows(2).filter_map(|pair| {
            let (outgoing, incoming) = (pair[0], pair[1]);
            if item.id != outgoing.id && item.id != incoming.id { return None; }
            if incoming.transition.as_ref()?.overlap <= 0.0 { return None; }
            let start = incoming.start_at;
            let end = outgoing.ends_at().min(incoming.ends_at());
            if end <= start { return None; }
            Some(MixWindow {
                start: item.source_at(start - item.start_at),
                end: item.source_at(end - item.start_at),
                incoming: item.id == incoming.id,
            })
        }).collect()
    }
}
