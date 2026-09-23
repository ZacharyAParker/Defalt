"""Cached cover art: matched Spotify albums, then the resolved YouTube video."""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import re
import threading
import time
from pathlib import Path
from urllib.parse import urlparse

import httpx
from PIL import Image

from . import config, db, spotify

_lock = threading.Lock()
_misses: dict[str, float] = {}
MAX_BYTES = 4 * 1024 * 1024


def youtube_thumbnail_urls(video_id: str) -> list[str]:
    if not re.fullmatch(r"[A-Za-z0-9_-]{11}", str(video_id or "")):
        return []
    return [f"https://i.ytimg.com/vi/{video_id}/{size}.jpg" for size in ("maxresdefault", "hqdefault")]


def _normal(text):
    return re.sub(r"[^a-z0-9]+", "", str(text).casefold())


def candidates(track):
    if spotify.available():
        try:
            for item in spotify.search(f'{track["artist"]} {track["title"]}'):
                # Artwork from an unrelated search result is worse than a thumbnail.
                if (_normal(item["title"]) == _normal(track["title"])
                        and _normal(str(track["artist"]).split(",")[0]) == _normal(item["artist"].split(",")[0])
                        and item.get("artwork")):
                    yield item["artwork"], "Spotify album cover"
                    break
        except ValueError:
            pass
    for url in youtube_thumbnail_urls(track.get("video_id")):
        yield url, "YouTube thumbnail"


def _download(url: str) -> bytes | None:
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.hostname not in {"i.scdn.co", "i.ytimg.com"}:
        return None
    with httpx.stream("GET", url, timeout=8, follow_redirects=False) as response:
        if response.status_code != 200:
            return None
        data = bytearray()
        for chunk in response.iter_bytes():
            data.extend(chunk)
            if len(data) > MAX_BYTES:
                return None
    with Image.open(io.BytesIO(data)) as image:
        if image.width < 200 or image.height < 90 or image.width * image.height > 16_000_000:
            return None
        image.thumbnail((512, 512))
        output = io.BytesIO()
        image.convert("RGB").save(output, format="PNG")
        return output.getvalue()


def resolve(key: str) -> tuple[Path, str] | None:
    row = db.one("SELECT * FROM tracks WHERE key=?", (key,))
    if row is None:
        return None
    track = dict(row)
    digest = hashlib.sha256(json.dumps([key, track.get("video_id"), track.get("artist"), track.get("title")]).encode()).hexdigest()
    folder = config.CACHE_DIR / "artwork"
    path = folder / f"{digest}.png"
    note = path.with_suffix(".json")
    # The common case is a hit, and it needs no lock at all: files are only
    # ever published whole, by rename.
    hit = _cached(path, note)
    if hit:
        return hit
    with _lock:
        if _misses.get(digest, 0) > time.monotonic():
            return None
    # One download per cover at a time; different covers never wait on each
    # other's network round trips.
    with _digest_lock(digest):
        hit = _cached(path, note)
        if hit:
            return hit
        with _lock:
            if _misses.get(digest, 0) > time.monotonic():
                return None
        for url, source in candidates(track):
            try:
                data = _download(url)
                if data:
                    folder.mkdir(parents=True, exist_ok=True)
                    temporary = path.with_suffix(".tmp")
                    temporary.write_bytes(data)
                    note.write_text(json.dumps({"source": source}), encoding="utf-8")
                    temporary.replace(path)
                    return path, source
            except (httpx.HTTPError, OSError, ValueError, Image.DecompressionBombError):
                continue
        with _lock:
            if len(_misses) >= 256:
                _misses.clear()
            _misses[digest] = time.monotonic() + 300
    return None


def _cached(path: Path, note: Path) -> tuple[Path, str] | None:
    if not (path.is_file() and note.is_file()):
        return None
    try:
        source = json.loads(note.read_text(encoding="utf-8"))["source"]
    except (ValueError, KeyError, OSError):
        return None
    try:
        # Housekeeping ages covers out by mtime; one still being shown is not
        # old. Touched at most daily, so a busy page is not a write per view.
        if time.time() - path.stat().st_mtime > 86400:
            os.utime(path)
    except OSError:
        pass
    return path, source


_digest_locks: dict[str, list] = {}


@contextlib.contextmanager
def _digest_lock(digest: str):
    with _lock:
        slot = _digest_locks.setdefault(digest, [threading.Lock(), 0])
        slot[1] += 1
    try:
        with slot[0]:
            yield
    finally:
        with _lock:
            slot[1] -= 1
            if slot[1] == 0:
                _digest_locks.pop(digest, None)
