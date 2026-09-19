"""Pull one record into your music folder.

    python -m radio.pull "Alex G - Pretend"

Prints one JSON object per line so the console can follow along. Anything the
library logs on the way goes to stdout too, so the reader ignores any line
that is not JSON rather than trying to keep the two apart.

Where it lands: ``MUSIC_DIR`` from ``.env``, defaulting to your Music folder.
The file is tagged with what we know, then imported the same way any file
already in that folder would be -- it becomes a local record you own, not a
special category. Nothing here ever deletes it: the radio's cache is evicted
under a size budget, and this is deliberately not that.

Read NOTICE.md before using this at all. It covers what happens to audio and
where the lines are.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import sys
from pathlib import Path
from typing import Any

from . import config, db, importer, library, youtube

#: Characters Windows will not have in a filename, plus the ones that make a
#: path miserable to type.
_UNSAFE = re.compile(r'[<>:"/\\|?*\x00-\x1f]')


def emit(stage: str, **rest: Any) -> None:
    print(json.dumps({"stage": stage, **rest}, ensure_ascii=False), flush=True)


def music_dir() -> Path:
    configured = config.env("MUSIC_DIR") or ""
    if configured.strip():
        return Path(configured.strip()).expanduser()
    return Path(os.path.expanduser("~")) / "Music"


def split_query(text: str) -> tuple[str, str]:
    """"Artist - Title" into its two halves.

    An em dash is what you get when a title is copied off a web page, and a
    plain hyphen with no spaces is usually part of a name rather than a
    separator, so only the spaced forms split.
    """
    text = " ".join(text.split())
    for separator in (" - ", " – ", " — ", " by "):
        if separator in text:
            left, _, right = text.partition(separator)
            if separator == " by ":
                # "Pretend by Alex G" is the other way round.
                return right.strip(), left.strip()
            return left.strip(), right.strip()
    # No separator: treat the lot as a title and let the resolver work it out.
    return "", text.strip()


#: What a pulled record is saved as.
#:
#: FLAC, and not the station's own cache format. The station caches to Opus,
#: which is the right choice for something it will throw away -- but the
#: console decodes with symphonia, and symphonia has no Opus decoder at all.
#: A pulled record that cannot be put on a deck is not a pulled record.
#:
#: Lossless rather than re-encoded, because the source is already lossy and a
#: second generation buys nothing. It costs disk and keeps what we were given.
PULL_FORMAT = "flac"
PULL_SUFFIX = ".flac"


def safe_name(artist: str, title: str, suffix: str) -> str:
    stem = f"{artist} - {title}" if artist else title
    stem = _UNSAFE.sub("", stem).strip().strip(".")
    # Windows caps a path component at 255; leave room for the extension and
    # for the " (2)" a collision adds.
    return (stem[:200] or "untitled") + suffix


def unique(path: Path) -> Path:
    """Never overwrite. Two different masters of one song are two records."""
    if not path.exists():
        return path
    stem, suffix = path.stem, path.suffix
    for n in range(2, 100):
        candidate = path.with_name(f"{stem} ({n}){suffix}")
        if not candidate.exists():
            return candidate
    raise OSError(f"too many files named like {path.name}")


def tag(path: Path, artist: str, title: str, metadata: dict | None = None) -> bool:
    """Write what we know into the file itself.

    Without this the importer would fall back to parsing the filename, which
    works but throws away the fact that we already know the answer.
    """
    try:
        import mutagen
    except ImportError:
        return False
    try:
        audio = mutagen.File(path, easy=True)
        if audio is None:
            return False
        if audio.tags is None:
            audio.add_tags()
        if title:
            audio["title"] = title
        if artist:
            audio["artist"] = artist
        if metadata:
            for field, tag_name in (("album", "album"), ("genre", "genre"), ("year", "date"),
                                    ("source_url", "defalt_source_url"),
                                    ("source_metadata", "defalt_source_metadata")):
                if metadata.get(field):
                    audio[tag_name] = str(metadata[field])
        audio.save()
        return True
    except Exception:
        # A file we cannot tag is still a file we can play, and the importer
        # reads the filename as a fallback anyway.
        return False


def pull(query: str, expected_ms: int = 0) -> int:
    try:
        link = youtube.parse(query)
    except ValueError as error:
        emit("failed", error=str(error))
        return 2
    artist, title = ("", "YouTube video " + link["video_id"]) if link else split_query(query)
    metadata = None
    if not title:
        emit("failed", error="nothing to look for")
        return 2

    folder = music_dir()
    try:
        folder.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        emit("failed", error=f"cannot write to {folder}: {error}")
        return 1

    key = youtube.key_for(link["video_id"]) if link else db.track_key(artist, title)
    existing = db.one("SELECT * FROM tracks WHERE key=?", (key,))
    if existing is not None:
        path = db.field(existing, "file")
        if db.field(existing, "source") == "local" and path and Path(path).exists():
            emit("done", key=key, artist=existing["artist"], title=existing["title"], file=path,
                 note="already in your library")
            return 0

    emit("resolving", key=key, artist=artist, title=title)
    if link:
        metadata = youtube.describe(link["video_id"])
        artist, title = metadata["artist"], metadata["title"]
        emit("resolving", key=key, artist=artist, title=title,
             note="Using the linked video; song details identified.")

    # A previous download may have finished before its import failed. Recover
    # that tagged file instead of creating another numbered copy on every retry.
    # Keep two uploads of the same song distinct, and retries recover the
    # exact linked recording rather than a similarly titled local file.
    filename_title = f"{title} [{link['video_id']}]" if link else title
    saved = folder / safe_name(artist, filename_title, PULL_SUFFIX)
    if saved.is_file():
        values = importer.metadata(saved)
        if link and not values.get("video_id") and tag(saved, artist, title, metadata):
            values = importer.metadata(saved)
        if (values.get("video_id") == link["video_id"] if link else
                db.track_key(values["artist"], values["title"]) == key):
            emit("analysing", key=key, file=str(saved))
            result = importer.import_file(saved)
            emit("done", key=result["key"], artist=artist, title=title, file=str(saved),
                 note="recovered the downloaded song")
            return 0

    # The duration is the whole reason a suggestion is worth taking: the
    # resolver scores against it, which is what separates a record from a
    # featurette about the record.
    video_id, raw = library.fetch_recording(artist, title, expected_ms,
        video_id=link['video_id'] if link else None, pinned=bool(link),
        on_attempt=lambda video, attempt: emit('fetching', key=key, video_id=video,
            note='Trying another matching upload.' if attempt else 'Downloading audio.'))
    if not video_id:
        emit("failed", key=key, error="nothing usable found")
        return 1
    # The station's own downloader gets the audio and levels it; we then take
    # the file out of its cache rather than leaving it there to be evicted.
    if not raw:
        emit("failed", key=key, error="the download failed")
        return 1

    try:
        measured = library.measure(raw)
        gain = library.gain_for(measured["integrated"], measured["true_peak"])
        destination = unique(folder / safe_name(artist, filename_title, PULL_SUFFIX))
        if not library._render(raw, destination, gain, PULL_FORMAT):
            # ffmpeg leaves a zero-byte file behind when it refuses. Left
            # there it would take the name, and the next attempt would come
            # out as "... (2)" forever.
            try:
                destination.unlink(missing_ok=True)
            except OSError:
                pass
            emit("failed", key=key, error="could not normalise the audio")
            return 1
    finally:
        try:
            raw.unlink()
        except OSError:
            pass

    tagged = tag(destination, artist, title, metadata)
    if link and not tagged:
        emit("failed", key=key, error=f"Audio is saved at {destination}, but its YouTube tags could not be written. Retry to recover it.")
        return 1

    emit("analysing", key=key, file=str(destination))
    try:
        # Imported exactly as if you had put it in the folder yourself, which
        # is the point: from here on it is an ordinary local record.
        result = importer.import_file(destination)
    except Exception as error:
        emit("failed", key=key, error=f"Could not import audio ({str(error) or type(error).__name__}). "
             f"The download is saved at {destination}; retry to recover it.")
        return 1

    row = db.one("SELECT * FROM tracks WHERE key=?", (result["key"],))
    if metadata:
        youtube.save(result["key"], metadata)
    emit(
        "done",
        key=result["key"],
        artist=db.field(row, "artist") or artist,
        title=db.field(row, "title") or title,
        file=str(destination),
        bpm=db.field(row, "bpm"),
        camelot=db.field(row, "camelot"),
        duration=db.field(row, "duration"),
    )
    return 0


def main(argv: list[str]) -> int:
    # The console reads this; accented titles must not die on a cp1252 pipe.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass

    expected_ms = 0
    if "--duration-ms" in argv:
        at = argv.index("--duration-ms")
        try:
            expected_ms = int(argv[at + 1])
        except (IndexError, ValueError):
            expected_ms = 0
        argv = argv[:at] + argv[at + 2:]

    if not argv:
        emit("failed", error='usage: python -m radio.pull "Artist - Title"')
        return 2
    try:
        return pull(" ".join(argv), expected_ms)
    except Exception as error:  # one bad pull, not a dead console
        emit("failed", error=str(error) or type(error).__name__)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
