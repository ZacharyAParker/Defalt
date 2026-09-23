"""Fill in missing year, album and genre from the Spotify catalogue.

Most of the library arrives with a title and an artist and nothing else, so a
request like "songs from 2010 to 2015" had almost nothing to go on. This runs
in the background, a few tracks at a time, and only ever fills a blank: a tag
from your own file, or anything you or an import already set, always wins.

Rate limited on purpose -- one catalogue call at a time with a gap between,
a pause when Spotify says slow down, and a per-track cooldown so a record the
catalogue does not know is not asked about again for a month.
"""
from __future__ import annotations

import json
import re
import threading
import time
from typing import Any, Callable

import httpx

from . import config, db, spotify

COOLDOWN = 30 * 86400.0      # per track, after an attempt that got an answer
ERROR_COOLDOWN = 86400.0     # after a network or catalogue failure
MIN_GAP = 1.2                # seconds between catalogue calls
SOURCE = "spotify_enrichment"

_lock = threading.Lock()
_token: tuple[str, float] | None = None
_last_call = 0.0
_backoff_until = 0.0
_thread: threading.Thread | None = None


def _log(*parts: Any) -> None:
    if config.DEBUG:
        print("[enrich]", *parts, flush=True)


def enabled() -> bool:
    return bool(config.station.get("enrichment.enabled", True)) and spotify.available()


def _pace(sleep: Callable[[float], None] = time.sleep) -> None:
    """Hold the caller until the next catalogue call is allowed."""
    global _last_call
    with _lock:
        wait = max(_last_call + MIN_GAP, _backoff_until) - time.monotonic()
        _last_call = time.monotonic() + max(0.0, wait)
    if wait > 0:
        sleep(wait)


class RateLimited(Exception):
    pass


def _get(url: str, params: dict[str, Any]) -> dict:
    """One authenticated catalogue GET. Raises RateLimited on 429."""
    global _token, _backoff_until
    _pace()
    with _lock:
        token = _token
    if not token or token[1] <= time.monotonic():
        response = httpx.post("https://accounts.spotify.com/api/token",
                              auth=(config.env("SPOTIFY_CLIENT_ID"), config.env("SPOTIFY_CLIENT_SECRET")),
                              data={"grant_type": "client_credentials"}, timeout=8)
        response.raise_for_status()
        body = response.json()
        token = (body["access_token"], time.monotonic() + max(1, int(body.get("expires_in", 3600)) - 60))
        with _lock:
            _token = token
    response = httpx.get(url, params=params, headers={"Authorization": "Bearer " + token[0]}, timeout=8)
    if response.status_code == 429:
        try:
            wait = float(response.headers.get("Retry-After", 60))
        except ValueError:
            wait = 60.0
        with _lock:
            _backoff_until = time.monotonic() + min(max(wait, 1.0), 3600.0)
        raise RateLimited(wait)
    if response.status_code == 401:
        with _lock:
            _token = None
    response.raise_for_status()
    return response.json()


def artist_genres(artist: str) -> list[str]:
    """Spotify's genres for this exact artist name, or [] if it is not sure."""
    wanted = db.norm(db.primary_artist(artist))
    if not wanted:
        return []
    body = _get("https://api.spotify.com/v1/search",
                {"q": f'artist:"{artist.replace(chr(34), "")}"', "type": "artist", "limit": 5})
    for item in (body.get("artists") or {}).get("items") or []:
        if isinstance(item, dict) and db.norm(str(item.get("name") or "")) == wanted:
            return [str(g) for g in item.get("genres") or [] if isinstance(g, str)][:3]
    return []


def _match(track: dict) -> dict | None:
    global _backoff_until
    artist, title = track["artist"], track["title"]
    query = f'artist:"{artist.replace(chr(34), "")}" track:"{title.replace(chr(34), "")}"'
    _pace()
    try:
        results = spotify.search(query)
    except ValueError as error:
        if "rate limit" in str(error).lower():
            with _lock:
                _backoff_until = time.monotonic() + 60.0
            raise RateLimited(60.0) from error
        raise
    for item in results:
        if (db.norm(item.get("title", "")) == db.norm(title) and
                db.norm(db.primary_artist(item.get("artist", ""))) == db.norm(db.primary_artist(artist))):
            return item
    return None


def _blank(value: Any) -> bool:
    return value is None or (isinstance(value, str) and not value.strip())


def enrich_track(track: dict) -> str:
    """Look one track up and fill whatever is still blank. Returns a status."""
    wants = {name for name in ("year", "album", "genre") if _blank(track.get(name))}
    if not wants:
        return "nothing_new"
    found: dict[str, Any] = {}
    match = _match(track) if wants & {"year", "album"} else None
    if match:
        year = str(match.get("year") or "")
        if "year" in wants and re.fullmatch(r"(?:19|20)\d{2}", year):
            found["year"] = int(year)
        if "album" in wants and isinstance(match.get("album"), str) and match["album"].strip():
            found["album"] = match["album"].strip()[:250]
    if "genre" in wants:
        genres = artist_genres(track["artist"])
        if genres:
            found["genre"] = ", ".join(genres)
    if not found:
        return "no_match"

    # Fill blanks in SQL, so a tag written meanwhile by an import or an edit
    # is never overwritten by what the catalogue thinks.
    sets, params = [], []
    for name, value in found.items():
        sets.append(f"{name}=CASE WHEN {name} IS NULL OR TRIM({name})='' THEN ? ELSE {name} END")
        params.append(value)
    db.write(f"UPDATE tracks SET {', '.join(sets)} WHERE key=?", (*params, track["key"]))
    _note_provenance(track["key"], found)
    _log("filled", track["artist"], "-", track["title"], found)
    return "filled"


def _note_provenance(key: str, found: dict[str, Any]) -> None:
    """Say where the new fields came from, in provenance the track already has.

    Hosts read these sources before claiming a fact. A track with no
    provenance yet is left without: creating it would change how YouTube
    links are hydrated, which keys on the column being empty.
    """
    row = db.one("SELECT source_metadata FROM tracks WHERE key=?", (key,))
    try:
        data = json.loads(row["source_metadata"]) if row and row["source_metadata"] else None
    except (TypeError, ValueError):
        return
    if not isinstance(data, dict) or not isinstance(data.get("fields"), dict):
        return
    for name, value in found.items():
        data["fields"].setdefault(name, {"value": value, "source": SOURCE, "confidence": 0.8})
    db.write("UPDATE tracks SET source_metadata=? WHERE key=?", (json.dumps(data, ensure_ascii=False), key))


def pending(limit: int, now: float | None = None) -> list[dict]:
    now = time.time() if now is None else now
    return [dict(row) for row in db.query(
        "SELECT t.* FROM tracks t LEFT JOIN enrichment e ON e.track_key=t.key "
        "WHERE (t.year IS NULL OR t.album IS NULL OR TRIM(t.album)='' "
        "       OR t.genre IS NULL OR TRIM(t.genre)='') "
        "AND t.blocked=0 AND t.artist != 'Unknown Artist' AND TRIM(t.title) != '' "
        "AND (e.attempted_at IS NULL OR e.attempted_at < ? "
        "     OR (e.status='error' AND e.attempted_at < ?)) "
        "ORDER BY t.play_count DESC, t.added_at DESC LIMIT ?",
        (now - COOLDOWN, now - ERROR_COOLDOWN, limit))]


def run_batch(limit: int | None = None, cancelled: Callable[[], bool] = lambda: False) -> dict[str, int]:
    """Enrich up to `limit` tracks. Stops early on a rate limit or cancel."""
    if not enabled():
        return {}
    limit = int(limit or config.station.get("enrichment.batch", 12) or 12)
    counts: dict[str, int] = {}
    for track in pending(limit):
        if cancelled() or time.monotonic() < _backoff_until:
            break
        try:
            status = enrich_track(track)
        except RateLimited:
            break  # the track is not marked; it is next in line after the pause
        except (ValueError, httpx.HTTPError, KeyError, TypeError) as error:
            _log("lookup failed", track.get("key"), error)
            status = "error"
        db.write("INSERT INTO enrichment (track_key, attempted_at, status) VALUES (?,?,?) "
                 "ON CONFLICT(track_key) DO UPDATE SET attempted_at=excluded.attempted_at, "
                 "status=excluded.status", (track["key"], time.time(), status))
        counts[status] = counts.get(status, 0) + 1
    return counts


def start(stop: threading.Event) -> threading.Thread:
    """The background worker: one small batch every few minutes."""
    global _thread
    if _thread is not None and _thread.is_alive():
        return _thread

    def loop() -> None:
        stop.wait(60)  # let the station come up first
        while not stop.is_set():
            try:
                run_batch(cancelled=stop.is_set)
            except Exception as error:  # noqa: BLE001 - never kill the worker
                _log("batch failed", error)
            minutes = float(config.station.get("enrichment.interval_minutes", 5) or 5)
            stop.wait(max(60.0, minutes * 60))

    _thread = threading.Thread(target=loop, daemon=True, name="enrichment")
    _thread.start()
    return _thread
