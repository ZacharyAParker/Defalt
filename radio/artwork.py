"""Cached cover art: matched Spotify albums, then the resolved YouTube video."""
from __future__ import annotations

import hashlib
import io
import json
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
    with _lock:
        if path.is_file() and note.is_file():
            try:
                return path, json.loads(note.read_text(encoding="utf-8"))["source"]
            except (ValueError, KeyError, OSError):
                pass
        if _misses.get(digest, 0) > time.monotonic():
            return None
        for url, source in candidates(track):
            try:
                data = _download(url)
                if data:
                    folder.mkdir(parents=True, exist_ok=True)
                    temporary = path.with_suffix(".tmp")
                    temporary.write_bytes(data)
                    temporary.replace(path)
                    note.write_text(json.dumps({"source": source}), encoding="utf-8")
                    return path, source
            except (httpx.HTTPError, OSError, ValueError, Image.DecompressionBombError):
                continue
        if len(_misses) >= 256:
            _misses.clear()
        _misses[digest] = time.monotonic() + 300
    return None
