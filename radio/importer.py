"""Local files, read in place and never rewritten.

    python -m radio.importer "E:/Music"

Rescanning is cheap: a file whose path, size and modification time are
unchanged since last time is skipped without being opened. Anything that fails
is reported by name and retried on the next scan, because one unreadable file
should not cost you the rest of the folder.
"""
from __future__ import annotations

import json
import os
import re
import time
from pathlib import Path

import mutagen

from . import analysis, db, library

AUDIO_EXTENSIONS = {".mp3", ".flac", ".wav", ".wave", ".ogg", ".opus", ".m4a",
                    ".aac", ".aif", ".aiff", ".wma", ".ape", ".alac", ".mp4"}
METADATA_VERSION = 1

# Junk that downloaders leave on a filename. Only ever stripped from names --
# never from tags, and never from db.norm, which would silently re-key every
# record already in the library and orphan everything learned about them.
_HASH_PREFIX = re.compile(r"^[0-9a-f]{16,}", re.IGNORECASE)
_TRACK_NUMBER = re.compile(r"^\d{1,3}[ ._-]+")
_TRAILING_JUNK = re.compile(
    r"[\s\-_(\[]*\b(official\s+)?(music\s+)?(lyric|lyrics|"
    r"video|audio|visuali[sz]er|hd|hq|4k|full\s+song)\b[\s)\]]*$",
    re.IGNORECASE,
)


def from_filename(stem: str) -> tuple[str, str]:
    """Best guess at (artist, title) for a file with nothing useful in it.

    Untagged files are the normal case in a real folder, not the exception,
    and a key built from an unwashed name is a record the station can never
    learn anything about -- so this matters more than it looks.
    """
    name = _HASH_PREFIX.sub("", stem)
    name = name.replace("_", " ")
    name = _TRACK_NUMBER.sub("", name)
    name = re.sub(r"\s+", " ", name).strip().lstrip("@").strip()

    # Repeat: "... Official Music Video" sheds one word at a time.
    while True:
        shorter = _TRAILING_JUNK.sub("", name).strip()
        if shorter == name or not shorter:
            break
        name = shorter

    artist, separator, title = name.partition(" - ")
    if not separator:
        return "", name
    return artist.strip(), title.strip()


def metadata(path: Path) -> dict:
    """Tags where there are tags, and the filename where there aren't."""
    audio = mutagen.File(path, easy=True)
    tags = audio.tags if audio is not None and audio.tags else {}

    def tag(*names: str) -> str:
        for name in names:
            try:
                value = tags.get(name)
            except (KeyError, ValueError):
                # FLAC/Vorbis rejects MP4's non-ASCII tag names, even on get().
                # A format-specific alias being invalid is not corrupt audio.
                continue
            if value:
                if hasattr(value, "text"):
                    value = value.text
                first = value[0] if isinstance(value, (list, tuple)) else value
                return str(first).strip()
        return ""

    guessed_artist, guessed_title = from_filename(path.stem)
    info = getattr(audio, "info", None)
    year = re.search(r"\b(\d{4})\b", tag("date", "year", "TDRC", "TYER"))

    # EasyID3 deliberately hides USLT; native tags also cover MP4 ©lyr and
    # Vorbis LYRICS. Read text already in the user's file, never fetch lyrics.
    lyrics = tag("lyrics", "unsyncedlyrics", "©lyr")
    if not lyrics:
        native = mutagen.File(path, easy=False)
        native_tags = getattr(native, "tags", None) or {}
        for name, value in native_tags.items():
            if str(name).lower() not in {"lyrics", "unsyncedlyrics", "©lyr", "wm/lyrics"} and not str(name).startswith("USLT"):
                continue
            value = getattr(value, "text", value)
            if isinstance(value, (list, tuple)):
                value = "\n".join(str(part) for part in value)
            if isinstance(value, str) and value.strip():
                lyrics = value.strip()
                break

    values = {
        "title": tag("title", "TIT2") or guessed_title,
        "artist": tag("artist", "TPE1") or guessed_artist or "Unknown Artist",
        "album": tag("album", "TALB"),
        "genre": tag("genre", "TCON"),
        "lyrics": lyrics[:50000],
        "metadata_version": METADATA_VERSION,
        "year": int(year[1]) if year else None,
        "duration": getattr(info, "length", None),
        "sample_rate": getattr(info, "sample_rate", None),
    }
    source_url = tag("defalt_source_url")
    if source_url:
        from . import youtube
        try:
            link = youtube.parse(source_url)
        except ValueError:
            link = None
        if link:
            values.update(source_url=link["url"], video_id=link["video_id"])
            provenance = tag("defalt_source_metadata")
            try:
                decoded = json.loads(provenance)
                if isinstance(decoded, dict) and isinstance(decoded.get("fields"), dict):
                    values["source_metadata"] = json.dumps(decoded, ensure_ascii=False)
            except (TypeError, ValueError):
                pass
    return values


def refresh_tags(track: dict, path: Path) -> dict:
    """One inexpensive upgrade for existing files; preserve title/key and DSP."""
    if (track.get("metadata_version") or 0) >= METADATA_VERSION:
        return track
    try:
        values = metadata(path)
    except Exception:
        return track  # A tag failure must not make playable audio unavailable.
    genre = values.get("genre") or track.get("genre") or ""
    lyrics = values.get("lyrics") or track.get("lyrics") or ""
    db.write("UPDATE tracks SET genre=?, lyrics=?, metadata_version=? WHERE key=?",
             (genre, lyrics, METADATA_VERSION, track["key"]))
    return {**track, "genre": genre, "lyrics": lyrics, "metadata_version": METADATA_VERSION}


def import_file(path: Path | str) -> dict:
    """Analyse one file and put it in the library.

    Keyed on artist and title like everything else, not on the path. That
    matters more than it looks: affinity, the seed, skip counts, the vault and
    the intro override all key the same way, so a record keyed by its location
    would be one the station could never learn anything about. Two files that
    claim to be the same record collapse into one row, most recent import wins.
    Downloaded YouTube FLACs carry an explicit video identity instead, so two
    requested uploads with matching display titles remain separate recordings.
    """
    path = Path(path).resolve(strict=True)
    identity = os.path.normcase(str(path))
    stat = path.stat()

    seen = db.one(
        "SELECT * FROM tracks WHERE import_path=?",
        (identity,))
    if (seen and seen["import_mtime_ns"] == stat.st_mtime_ns
            and seen["import_size"] == stat.st_size):
        if (seen["metadata_version"] or 0) < METADATA_VERSION:
            refreshed = refresh_tags(dict(seen), path)
            if refreshed.get("metadata_version") == METADATA_VERSION:
                return {"key": seen["key"], "status": "updated"}
        return {"key": seen["key"], "status": "skipped"}

    values = metadata(path)
    if values.get("source_url") and values.get("video_id"):
        from . import youtube
        key = youtube.key_for(values["video_id"])
    else:
        key = db.track_key(values["artist"], values["title"])
    known = db.one("SELECT key FROM tracks WHERE key=?", (key,))

    # Everything is measured before anything is written, so a file that fails
    # halfway leaves no fingerprint behind and gets retried on the next scan.
    values["duration"] = values["duration"] or library._probe_duration(path)
    measured = library.measure(path)
    values.update(library.shape(measured["samples"], values["duration"]))
    values["lufs"] = measured["integrated"]
    values.update(analysis.profile(path))
    analysis.peaks(path)

    after = path.stat()
    if (stat.st_mtime_ns, stat.st_size) != (after.st_mtime_ns, after.st_size):
        raise OSError("file changed while it was being read; retry the scan")

    values.update(file=str(path), source="local", import_path=identity,
                  import_mtime_ns=stat.st_mtime_ns, import_size=stat.st_size)

    columns = list(values)
    db.write(
        f"INSERT INTO tracks (key, added_at, {', '.join(columns)}) "
        f"VALUES ({', '.join('?' for _ in range(len(columns) + 2))}) "
        "ON CONFLICT(key) DO UPDATE SET "
        + ", ".join(f"{column}=excluded.{column}" for column in columns),
        (key, time.time(), *values.values()))

    return {"key": key, "status": "updated" if known else "imported"}


def import_folder(directory: Path | str) -> dict:
    root = Path(directory).resolve(strict=True)
    if not root.is_dir():
        raise ValueError("expected a directory")

    report: dict = {"imported": 0, "updated": 0, "skipped": 0, "errors": []}

    def unreadable(error: OSError) -> None:
        report["errors"].append({"path": str(error.filename), "error": str(error)})

    for folder, directories, files in os.walk(root, onerror=unreadable):
        directories.sort()
        for name in sorted(files):
            path = Path(folder) / name
            if path.suffix.lower() not in AUDIO_EXTENSIONS:
                continue
            try:
                report[import_file(path)["status"]] += 1
            except Exception as error:  # one bad file, not one bad folder
                report["errors"].append({"path": str(path), "error": str(error)})

    return report


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="Import a folder of music.")
    parser.add_argument("directory", type=Path)
    report = import_folder(parser.parse_args().directory)
    print(json.dumps(report, indent=2, ensure_ascii=False))
    raise SystemExit(1 if report["errors"] else 0)
