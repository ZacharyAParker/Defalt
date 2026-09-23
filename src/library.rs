//! The record library, read straight out of SQLite.
//!
//! No HTTP and no Python. The station writes this database when it imports or
//! fetches something; the console only ever reads it, so the two can be open
//! at once without either waiting on the other.

use std::path::{Path, PathBuf};

/// One record, as far as the console cares.
#[derive(Clone, Debug)]
pub struct Record {
    pub key: String,
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration: Option<f64>,
    pub bpm: Option<f64>,
    pub camelot: Option<String>,
    pub lufs: Option<f64>,
    pub file: PathBuf,
    /// Measured beat grid, where there is one: seconds to the first beat, and
    /// seconds between beats.
    pub beat_offset: Option<f64>,
    pub beat_period: Option<f64>,
    pub downbeat_offset: Option<f64>,
}

impl Record {
    pub fn label(&self) -> String {
        format!("{} - {}", self.artist, self.title)
    }

    /// Tempo with a pitch adjustment applied, which is the number that
    /// actually matters when you are matching two records.
    pub fn tempo_at(&self, pitch_percent: f64) -> Option<f64> {
        self.bpm.map(|bpm| bpm * (1.0 + pitch_percent / 100.0))
    }
}

pub fn database(root: &Path) -> PathBuf {
    root.join("cache").join("station.db")
}

/// Read the library on a thread of its own. A few thousand rows with a file
/// check each is a noticeable pause on a slow disk, and the console must be
/// drawing while it happens -- at startup most of all.
pub fn load_in_background(root: &Path) -> std::sync::mpsc::Receiver<Result<Vec<Record>, String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("library".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(|| load(&root))
                .unwrap_or_else(|_| Err("reading the library crashed".into()));
            let _ = sender.send(result);
        })
        .ok();
    receiver
}

/// Every local record with a file still on disk.
///
/// The radio's own fetched records are deliberately left out: they are cache,
/// evicted under a size budget when the station is done with them, which is
/// not something a deck should discover halfway through a mix. Anything
/// pulled lands in your music folder and is imported as local, so it arrives
/// here by the ordinary route.
pub fn load(root: &Path) -> Result<Vec<Record>, String> {
    let path = database(root);
    if !path.is_file() {
        return Ok(Vec::new());
    }

    // Read-only, and it must not create anything: the station owns this file.
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = rusqlite::Connection::open_with_flags(&path, flags)
        .map_err(|error| format!("could not open the library: {error}"))?;
    // The station may be mid-write (an import, a fetch). Wait a moment for
    // it rather than failing the whole crate on SQLITE_BUSY.
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|error| format!("could not open the library: {error}"))?;

    let mut statement = connection
        .prepare(
            "SELECT key, artist, title, album, duration, bpm, camelot, lufs, file, \
                    beat_offset, beat_period, downbeat_offset \
             FROM tracks \
             WHERE source = 'local' AND file IS NOT NULL \
             ORDER BY artist COLLATE NOCASE, title COLLATE NOCASE",
        )
        .map_err(|error| format!("library query failed: {error}"))?;

    let rows = statement
        .query_map([], |row| {
            Ok(Record {
                key: row.get(0)?,
                artist: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                album: row.get(3)?,
                duration: row.get(4)?,
                bpm: row.get(5)?,
                camelot: row.get(6)?,
                lufs: row.get(7)?,
                file: PathBuf::from(row.get::<_, String>(8)?),
                beat_offset: row.get(9)?,
                beat_period: row.get(10)?,
                downbeat_offset: row.get(11)?,
            })
        })
        .map_err(|error| format!("library read failed: {error}"))?;

    let mut records = Vec::new();
    for row in rows {
        match row {
            // A record whose file has been moved or deleted is not an error
            // worth stopping for, it is just one row that cannot be loaded.
            Ok(record) if record.file.is_file() => records.push(record),
            Ok(_) => continue,
            Err(error) => return Err(format!("library row failed: {error}")),
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_database_is_an_empty_library_rather_than_an_error() {
        let empty = std::env::temp_dir().join("defalt-no-such-project");
        assert_eq!(load(&empty).unwrap().len(), 0);
    }

    #[test]
    fn tempo_follows_the_pitch_fader() {
        let record = Record {
            key: "a|b".into(), artist: "a".into(), title: "b".into(),
            album: None, duration: None, bpm: Some(120.0), camelot: None,
            lufs: None, file: PathBuf::new(),
            beat_offset: None, beat_period: None, downbeat_offset: None,
        };
        assert_eq!(record.tempo_at(0.0), Some(120.0));
        assert_eq!(record.tempo_at(5.0), Some(126.0));
    }
}
