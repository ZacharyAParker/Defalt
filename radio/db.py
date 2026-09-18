"""SQLite store: the station's memory.

Holds the track catalogue, every listening event, rolling affinity scores, and
the dedupe ledgers that stop the hosts reading the same story twice. The
Obsidian vault is a human-readable mirror of this; this file is the source of
truth.
"""
from __future__ import annotations

import json
import re
import sqlite3
import threading
import time
from pathlib import Path
from typing import Any, Iterable

from . import config

_LOCAL = threading.local()
_DB_PATH = config.CACHE_DIR / "station.db"

SCHEMA = """
CREATE TABLE IF NOT EXISTS hot_cues (
    track_key TEXT NOT NULL REFERENCES tracks(key) ON DELETE CASCADE,
    slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 7),
    position_samples INTEGER NOT NULL CHECK(position_samples >= 0),
    colour TEXT NOT NULL DEFAULT '#ffffff',
    label TEXT NOT NULL DEFAULT '',
    PRIMARY KEY(track_key, slot)
);
CREATE TABLE IF NOT EXISTS crates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0)
);
CREATE TABLE IF NOT EXISTS crate_tracks (
    crate_id INTEGER NOT NULL REFERENCES crates(id) ON DELETE CASCADE,
    track_key TEXT NOT NULL REFERENCES tracks(key) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position >= 0),
    PRIMARY KEY(crate_id, track_key),
    UNIQUE(crate_id, position)
);
CREATE TABLE IF NOT EXISTS tracks (
    key           TEXT PRIMARY KEY,   -- normalised "artist|title"
    title         TEXT NOT NULL,
    artist        TEXT NOT NULL,
    source        TEXT,               -- 'seed' | 'request' | 'discovery' | 'local'
    video_id      TEXT,               -- resolved youtube id, once known
    file          TEXT,               -- cached audio path, once downloaded
    duration      REAL,
    intro_sec     REAL,               -- measured vocal entry, rewritten on analysis
    intro_override REAL,              -- yours, if you set one. always wins.
    outro_sec     REAL,
    lufs          REAL,
    bpm            REAL,            -- detected tempo, 0 when there is no pulse
    bpm_confidence REAL,
    key_tonic      INTEGER,         -- 0-11 pitch class, -1 unknown
    key_mode       TEXT,            -- major | minor
    key_confidence REAL,
    camelot        TEXT,            -- '8A' -- what the transition picker uses
    beat_offset      REAL,          -- seconds to the first beat
    beat_period      REAL,          -- seconds per beat
    beat_residual_ms REAL,          -- how far beats wander from a steady grid
    downbeat_offset  REAL,
    expected_ms   INTEGER,            -- length per the seed, for match checking
    added_at      REAL NOT NULL,
    last_played   REAL,
    play_count    INTEGER NOT NULL DEFAULT 0,
    skip_count    INTEGER NOT NULL DEFAULT 0,
    blocked       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        REAL NOT NULL,
    track_key TEXT,
    kind      TEXT NOT NULL,          -- played | skipped | thumbs_up | ...
    position  REAL,                   -- seconds into the track
    meta      TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_ts   ON events (ts);
CREATE INDEX IF NOT EXISTS idx_events_kind ON events (kind);

CREATE TABLE IF NOT EXISTS affinity (
    entity_type TEXT NOT NULL,        -- 'track' | 'artist'
    entity_key  TEXT NOT NULL,
    score       REAL NOT NULL DEFAULT 0,
    updated_at  REAL NOT NULL,
    PRIMARY KEY (entity_type, entity_key)
);

-- Dedupe ledger for anything the hosts read on air: news URLs, Steam patch
-- ids, ad reads. Stops repeats across restarts.
CREATE TABLE IF NOT EXISTS seen (
    kind  TEXT NOT NULL,
    ident TEXT NOT NULL,
    ts    REAL NOT NULL,
    PRIMARY KEY (kind, ident)
);

CREATE TABLE IF NOT EXISTS requests (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        REAL NOT NULL,
    query     TEXT NOT NULL,
    status    TEXT NOT NULL DEFAULT 'pending',  -- pending|queued|aired|failed
    track_key TEXT,
    note      TEXT
);

-- Anything you ask the station for that is not a single track: a topic to
-- cover, a genre to explore, a standing "stop playing so much X".
CREATE TABLE IF NOT EXISTS wishes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         REAL NOT NULL,
    raw        TEXT NOT NULL,      -- exactly what you typed, never edited
    kind       TEXT NOT NULL,      -- topic | genre | similar | artist | segment | directive
    subject    TEXT,               -- the cleaned subject of the wish
    payload    TEXT,               -- json, kind-specific
    timing     TEXT DEFAULT 'next',
    status     TEXT DEFAULT 'pending',  -- pending|active|done|failed|cancelled
    note       TEXT,
    expires_at REAL
);
CREATE INDEX IF NOT EXISTS idx_wishes ON wishes (status, ts);

CREATE TABLE IF NOT EXISTS aired (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    ts   REAL NOT NULL,
    kind TEXT NOT NULL,               -- segment kind, for cooldown checks
    meta TEXT
);
CREATE INDEX IF NOT EXISTS idx_aired ON aired (kind, ts);

-- Daypart histogram: which hours you actually play which artists at.
CREATE TABLE IF NOT EXISTS daypart (
    hour       INTEGER NOT NULL,
    artist     TEXT NOT NULL,
    weight     REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, artist)
);
"""


def connect() -> sqlite3.Connection:
    """One connection per thread. The director and Flask both touch this."""
    conn = getattr(_LOCAL, "conn", None)
    if conn is None:
        _DB_PATH.parent.mkdir(parents=True, exist_ok=True)
        conn = sqlite3.connect(_DB_PATH, timeout=15, check_same_thread=False)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA foreign_keys=ON")
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA synchronous=NORMAL")
        conn.executescript(SCHEMA)
        _migrate(conn)
        conn.commit()
        _LOCAL.conn = conn
    return conn


# Columns added after the first release. CREATE TABLE IF NOT EXISTS will not
# add them to a database that already exists, so do it by hand.
_ADDED_COLUMNS = {"tracks": {
    "source": "TEXT", "album": "TEXT", "year": "INTEGER", "genre": "TEXT",
    "lyrics": "TEXT", "metadata_version": "INTEGER",
    "source_url": "TEXT", "source_metadata": "TEXT",
    "import_path": "TEXT", "import_mtime_ns": "INTEGER", "import_size": "INTEGER",
    "sample_rate": "INTEGER",
    "intro_override": "REAL",
    "bpm": "REAL", "bpm_confidence": "REAL",
    "key_tonic": "INTEGER", "key_mode": "TEXT", "key_confidence": "REAL",
    "camelot": "TEXT",
    "beat_offset": "REAL", "beat_period": "REAL",
    "beat_residual_ms": "REAL", "downbeat_offset": "REAL",
}}


# Indexes over columns that arrive by migration. These cannot live in SCHEMA:
# it runs before the columns exist.
_ADDED_INDEXES = (
    "CREATE INDEX IF NOT EXISTS idx_tracks_import ON tracks (import_path)",
)


def _migrate(conn: sqlite3.Connection) -> None:
    for table, columns in _ADDED_COLUMNS.items():
        existing = {row["name"] for row in
                    conn.execute(f"PRAGMA table_info({table})").fetchall()}
        for name, kind in columns.items():
            if name not in existing:
                conn.execute(f"ALTER TABLE {table} ADD COLUMN {name} {kind}")
    for statement in _ADDED_INDEXES:
        conn.execute(statement)


def field(track: Any, name: str) -> Any:
    """Read a column from either a dict or a sqlite3.Row, missing-safe."""
    if isinstance(track, sqlite3.Row):
        return track[name] if name in track.keys() else None
    return (track or {}).get(name)


def intro_of(track: Any, fallback: float) -> float:
    """The talk-over window for a track: your override, else what we measured."""
    return float(field(track, "intro_override")
                 or field(track, "intro_sec")
                 or fallback)


def query(sql: str, params: Iterable[Any] = ()) -> list[sqlite3.Row]:
    return connect().execute(sql, tuple(params)).fetchall()


def one(sql: str, params: Iterable[Any] = ()) -> sqlite3.Row | None:
    return connect().execute(sql, tuple(params)).fetchone()


def write(sql: str, params: Iterable[Any] = ()) -> int:
    conn = connect()
    cursor = conn.execute(sql, tuple(params))
    conn.commit()
    return cursor.lastrowid or 0


_PUNCT = re.compile(r"[^\w\s]+")
_SPACE = re.compile(r"\s+")
# Parenthetical noise that changes the string but not the song.
_NOISE = re.compile(
    r"\s*[\(\[-]\s*(feat\.?|ft\.?|with|remaster(ed)?|\d{4} remaster|"
    r"radio edit|original version|deluxe edition|from .+)\b.*$",
    re.IGNORECASE,
)


def norm(text: str) -> str:
    """Normalise a title or artist so the same song always hashes the same."""
    text = _NOISE.sub("", text or "")
    text = _PUNCT.sub(" ", text.lower())
    return _SPACE.sub(" ", text).strip()


def track_key(artist: str, title: str) -> str:
    return f"{norm(artist)}|{norm(title)}"


# Band names that contain the very separators we split on. Checked whole, so
# they survive intact. Add to this if the station keeps mangling an act.
_ATOMIC_NAMES = {
    "earth, wind & fire", "crosby, stills & nash", "crosby, stills, nash & young",
    "blood, sweat & tears", "emerson, lake & palmer", "peter, paul and mary",
}

# Comma-separated credits usually mean "several artists", but plenty of acts
# have a comma in their actual name. Never split before one of these.
_NAME_CONTINUATIONS = ("the ", "jr", "sr", "ii", "iii")


def _splits_here(remainder: str) -> bool:
    """Is the text after a comma a new artist, or the rest of this one?"""
    tail = remainder.strip().lower()
    return not any(tail.startswith(word) for word in _NAME_CONTINUATIONS)


def primary_artist(artist: str) -> str:
    """First credited artist -- collaborations shouldn't fragment affinity.

    "Kendrick Lamar, SZA" -> "Kendrick Lamar", but
    "Tyler, The Creator" -> "Tyler, The Creator".
    """
    artist = (artist or "").strip()
    lowered = artist.lower()
    if lowered in _ATOMIC_NAMES:
        return artist

    for sep in (" & ", " feat", " ft.", " with ", " x "):
        if sep in lowered:
            artist = artist[: lowered.index(sep)].strip()
            lowered = artist.lower()

    index = artist.find(",")
    while index != -1:
        if _splits_here(artist[index + 1:]):
            return artist[:index].strip()
        index = artist.find(",", index + 1)
    return artist.strip()


def log_event(kind: str, track_key_: str | None = None,
              position: float | None = None, **meta: Any) -> None:
    write(
        "INSERT INTO events (ts, track_key, kind, position, meta) VALUES (?,?,?,?,?)",
        (time.time(), track_key_, kind, position, json.dumps(meta) if meta else None),
    )


def mark_aired(kind: str, **meta: Any) -> None:
    write("INSERT INTO aired (ts, kind, meta) VALUES (?,?,?)",
          (time.time(), kind, json.dumps(meta) if meta else None))


def last_aired(kind: str) -> float | None:
    row = one("SELECT MAX(ts) AS ts FROM aired WHERE kind = ?", (kind,))
    return row["ts"] if row and row["ts"] else None


def is_seen(kind: str, ident: str) -> bool:
    return one("SELECT 1 FROM seen WHERE kind=? AND ident=?", (kind, ident)) is not None


def mark_seen(kind: str, ident: str) -> None:
    write("INSERT OR REPLACE INTO seen (kind, ident, ts) VALUES (?,?,?)",
          (kind, ident, time.time()))


def prune_seen(days: int) -> None:
    write("DELETE FROM seen WHERE ts < ?", (time.time() - days * 86400,))


def hot_cues(key: str) -> list[dict]:
    return [dict(row) for row in query(
        'SELECT * FROM hot_cues WHERE track_key=? ORDER BY slot', (key,))]


def set_hot_cue(key: str, slot: int, position_samples: int,
                colour: str = '#ffffff', label: str = '') -> None:
    if type(slot) is not int or not 0 <= slot < 8:
        raise ValueError('slot must be an integer from 0 to 7')
    if type(position_samples) is not int or not 0 <= position_samples <= 2**63 - 1:
        raise ValueError('position_samples must be a nonnegative 64-bit integer')
    if not isinstance(colour, str) or not re.fullmatch(r'#[0-9a-fA-F]{6}', colour):
        raise ValueError('colour must be #RRGGBB')
    if not isinstance(label, str) or len(label) > 256:
        raise ValueError('label must be a string of at most 256 characters')
    write('INSERT INTO hot_cues VALUES (?,?,?,?,?) ON CONFLICT(track_key,slot) '
          'DO UPDATE SET position_samples=excluded.position_samples, '
          'colour=excluded.colour, label=excluded.label',
          (key, slot, position_samples, colour, label))


def delete_hot_cue(key: str, slot: int) -> None:
    write('DELETE FROM hot_cues WHERE track_key=? AND slot=?', (key, slot))


def crate(crate_id: int) -> dict | None:
    row = one('SELECT * FROM crates WHERE id=?', (crate_id,))
    if row is None:
        return None
    return {**dict(row), 'tracks': [dict(track) for track in query(
        'SELECT t.*, ct.position FROM crate_tracks ct JOIN tracks t '
        'ON t.key=ct.track_key WHERE crate_id=? ORDER BY ct.position', (crate_id,))]}


def save_crate(name: str, track_keys: list[str], crate_id: int | None = None) -> int:
    """Create or atomically rename/replace membership; list order is play order."""
    if not isinstance(name, str) or not name.strip() or len(name) > 256:
        raise ValueError('name must contain 1 to 256 characters')
    if (not isinstance(track_keys, list) or any(not isinstance(k, str) for k in track_keys)
            or len(set(track_keys)) != len(track_keys)):
        raise ValueError('tracks must be a list of unique track keys')
    conn = connect()
    with conn:
        if crate_id is None:
            crate_id = conn.execute('INSERT INTO crates(name) VALUES (?)', (name.strip(),)).lastrowid
        elif conn.execute('UPDATE crates SET name=? WHERE id=?',
                          (name.strip(), crate_id)).rowcount == 0:
            raise LookupError('unknown crate')
        conn.execute('DELETE FROM crate_tracks WHERE crate_id=?', (crate_id,))
        conn.executemany('INSERT INTO crate_tracks VALUES (?,?,?)',
                         ((crate_id, key, index) for index, key in enumerate(track_keys)))
    return crate_id


def delete_crate(crate_id: int) -> None:
    write('DELETE FROM crates WHERE id=?', (crate_id,))
