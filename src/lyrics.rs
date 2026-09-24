//! Synced lyrics and the verse/chorus map, read straight out of SQLite.
//!
//! The station fetches them from LRCLIB and works out the sections
//! (radio/lyrics.py); the console only reads the `lyrics` table, on a thread
//! of its own, and keeps what it found for a while. A record with nothing in
//! the table simply has no lyric line and no section bands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Source seconds.
    pub t: f64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Intro,
    Verse,
    Chorus,
    Bridge,
    Instrumental,
    Outro,
}

impl Label {
    fn named(name: &str) -> Option<Label> {
        Some(match name {
            "intro" => Label::Intro,
            "verse" => Label::Verse,
            "chorus" => Label::Chorus,
            "bridge" => Label::Bridge,
            "instrumental" => Label::Instrumental,
            "outro" => Label::Outro,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Label::Intro => "Intro",
            Label::Verse => "Verse",
            Label::Chorus => "Chorus",
            Label::Bridge => "Bridge",
            Label::Instrumental => "Break",
            Label::Outro => "Outro",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    pub start: f64,
    pub end: f64,
    pub label: Label,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub sections: Vec<Section>,
}

impl Lyrics {
    pub fn section_at(&self, seconds: f64) -> Option<&Section> {
        self.sections.iter().find(|s| s.start <= seconds && seconds < s.end)
    }
}

/// What to show at a moment: the line being sung and the one after it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Showing<'a> {
    pub current: Option<&'a str>,
    pub next: Option<&'a str>,
}

/// A line is not still being sung this long after it started.
const HOLD: f64 = 10.0;
/// Before the singing starts, the first line is shown this far ahead.
const LEAD: f64 = 6.0;

/// The line at `seconds` of the source, karaoke style.
pub fn showing(lines: &[Line], seconds: f64) -> Showing<'_> {
    if lines.is_empty() || !seconds.is_finite() {
        return Showing::default();
    }
    let index = lines.partition_point(|line| line.t <= seconds);
    let upcoming = |from: usize| lines[from..].iter().find(|l| !l.text.is_empty());
    match index.checked_sub(1) {
        None => Showing {
            current: None,
            next: upcoming(0).filter(|l| l.t - seconds <= LEAD).map(|l| l.text.as_str()),
        },
        Some(at) => {
            let line = &lines[at];
            let sung = !line.text.is_empty() && seconds - line.t < HOLD;
            let next = upcoming(index);
            Showing {
                current: sung.then_some(line.text.as_str()),
                // Between lines, the next one is only teased when it is near.
                next: next
                    .filter(|l| sung || l.t - seconds <= LEAD)
                    .map(|l| l.text.as_str()),
            }
        }
    }
}

/// One record's lyrics from the station's database, or None.
pub fn load(root: &Path, key: &str) -> Option<Lyrics> {
    let path = crate::library::database(root);
    if !path.is_file() {
        return None;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = rusqlite::Connection::open_with_flags(&path, flags).ok()?;
    connection.busy_timeout(Duration::from_secs(2)).ok()?;
    // An older station has no such table; that is no lyrics, not an error.
    let (status, synced, sections): (String, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT status, synced, sections FROM lyrics WHERE track_key = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()?;
    if !matches!(status.as_str(), "synced" | "plain" | "instrumental") {
        return None;
    }
    let lyrics = parse(synced.as_deref().unwrap_or(""), sections.as_deref().unwrap_or(""));
    (!lyrics.lines.is_empty() || !lyrics.sections.is_empty()).then_some(lyrics)
}

/// The stored JSON, read forgivingly: a malformed entry is skipped.
pub fn parse(synced: &str, sections: &str) -> Lyrics {
    let lines = serde_json::from_str::<serde_json::Value>(synced).ok();
    let mut lines: Vec<Line> = lines
        .as_ref()
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|line| {
            let t = line["t"].as_f64().filter(|t| t.is_finite() && *t >= 0.0)?;
            Some(Line { t, text: line["text"].as_str().unwrap_or("").trim().to_string() })
        })
        .collect();
    lines.sort_by(|a, b| a.t.total_cmp(&b.t));
    let found = serde_json::from_str::<serde_json::Value>(sections).ok();
    let sections = found
        .as_ref()
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let start = s["start"].as_f64().filter(|v| v.is_finite())?;
            let end = s["end"].as_f64().filter(|v| v.is_finite() && *v > start)?;
            Some(Section { start, end, label: Label::named(s["label"].as_str()?)? })
        })
        .collect();
    Lyrics { lines, sections }
}

/* ── The cache ───────────────────────────────────────────────────────── */

enum Entry {
    /// A read under way, and what was known before it (shown meanwhile).
    Loading(Receiver<Option<Lyrics>>, Option<Arc<Lyrics>>),
    Ready(Option<Arc<Lyrics>>, Instant),
}

/// Records' lyrics, read in the background and kept. A miss is asked again
/// now and then, since the station may have fetched them meanwhile.
#[derive(Default)]
pub struct Cache {
    entries: HashMap<String, Entry>,
}

const RETRY_MISS: Duration = Duration::from_secs(30);
const REFRESH: Duration = Duration::from_secs(600);

impl Cache {
    /// What is known for `key` now. Starts a read when there is nothing
    /// fresh, and never waits for one.
    pub fn get(&mut self, root: &Path, key: &str) -> Option<Arc<Lyrics>> {
        if key.is_empty() {
            return None;
        }
        let stale = match self.entries.get_mut(key) {
            Some(Entry::Loading(inbox, kept)) => match inbox.try_recv() {
                Ok(found) => {
                    let found = found.map(Arc::new);
                    self.entries.insert(key.to_string(), Entry::Ready(found.clone(), Instant::now()));
                    return found;
                }
                Err(TryRecvError::Empty) => return kept.clone(),
                Err(TryRecvError::Disconnected) => true,
            },
            Some(Entry::Ready(found, at)) => {
                let age = at.elapsed();
                if (found.is_none() && age < RETRY_MISS) || (found.is_some() && age < REFRESH) {
                    return found.clone();
                }
                // Keep showing what we had while it is read again.
                let kept = found.clone();
                self.start(root, key, kept.clone());
                return kept;
            }
            None => true,
        };
        if stale {
            self.start(root, key, None);
        }
        None
    }

    fn start(&mut self, root: &Path, key: &str, kept: Option<Arc<Lyrics>>) {
        if self.entries.len() > 64 {
            self.entries.retain(|_, entry| matches!(entry, Entry::Loading(..)));
        }
        let (sender, inbox) = mpsc::channel();
        let (root, wanted): (PathBuf, String) = (root.to_path_buf(), key.to_string());
        let spawned = std::thread::Builder::new()
            .name("lyrics".into())
            .spawn(move || {
                let found = std::panic::catch_unwind(|| load(&root, &wanted)).ok().flatten();
                let _ = sender.send(found);
            });
        if spawned.is_ok() {
            self.entries.insert(key.to_string(), Entry::Loading(inbox, kept));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<Line> {
        [(12.0, "first line"), (15.0, "second line"), (18.0, ""), (40.0, "after the break"), (43.0, "last")]
            .iter()
            .map(|(t, text)| Line { t: *t, text: text.to_string() })
            .collect()
    }

    #[test]
    fn the_line_being_sung_is_current_and_the_next_one_waits() {
        let lines = lines();
        assert_eq!(showing(&lines, 0.0), Showing::default(), "nothing this early");
        assert_eq!(showing(&lines, 7.0).next, Some("first line"), "teased just before");
        assert_eq!(showing(&lines, 7.0).current, None);
        let singing = showing(&lines, 13.5);
        assert_eq!((singing.current, singing.next), (Some("first line"), Some("second line")));
        assert_eq!(showing(&lines, 15.0).current, Some("second line"));
        // A blank marker ends the singing; the next line is not teased from afar.
        assert_eq!(showing(&lines, 20.0), Showing::default());
        assert_eq!(showing(&lines, 36.0).next, Some("after the break"));
        assert_eq!(showing(&lines, 44.0).current, Some("last"));
        assert_eq!(showing(&lines, 44.0).next, None);
        assert_eq!(showing(&lines, 60.0).current, None, "a line is not held forever");
        assert_eq!(showing(&lines, f64::NAN), Showing::default());
    }

    #[test]
    fn the_stored_json_is_read_forgivingly() {
        let lyrics = parse(
            r#"[{"t": 5.0, "text": "b"}, {"t": 1.5, "text": " a "}, {"t": "x"}, {"text": "no time"}]"#,
            r#"[{"start": 0, "end": 1.5, "label": "intro"}, {"start": 1.5, "end": 9, "label": "chorus"},
                {"start": 9, "end": 8, "label": "verse"}, {"start": 9, "end": 12, "label": "polka"}]"#,
        );
        assert_eq!(lyrics.lines, vec![Line { t: 1.5, text: "a".into() }, Line { t: 5.0, text: "b".into() }]);
        assert_eq!(lyrics.sections.len(), 2);
        assert_eq!(lyrics.section_at(3.0).map(|s| s.label), Some(Label::Chorus));
        assert_eq!(parse("not json", ""), Lyrics::default());
    }

    #[test]
    fn a_database_without_the_table_or_the_track_is_no_lyrics() {
        let root = std::env::temp_dir().join(format!("defalt-lyrics-{}", std::process::id()));
        let _ = std::fs::create_dir_all(root.join("cache"));
        let path = crate::library::database(&root);
        let _ = std::fs::remove_file(&path);
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE tracks (key TEXT);").unwrap();
        }
        assert_eq!(load(&root, "a|b"), None);
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE lyrics (track_key TEXT PRIMARY KEY, status TEXT, synced TEXT, sections TEXT);
                 INSERT INTO lyrics VALUES ('a|b', 'synced', '[{\"t\": 1, \"text\": \"hi\"}]',
                                            '[{\"start\": 0, \"end\": 1, \"label\": \"intro\"}]');
                 INSERT INTO lyrics VALUES ('c|d', 'missing', NULL, NULL);",
            )
            .unwrap();
        }
        let found = load(&root, "a|b").expect("stored lyrics");
        assert_eq!(found.lines[0].text, "hi");
        assert_eq!(found.sections[0].label, Label::Intro);
        assert_eq!(load(&root, "c|d"), None);
        let mut cache = Cache::default();
        let mut got = None;
        for _ in 0..200 {
            got = cache.get(&root, "a|b");
            if got.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(got.map(|l| l.lines.len()), Some(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
