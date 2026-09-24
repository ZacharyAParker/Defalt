"""Time-synced lyrics from LRCLIB, and from nowhere else.

LRCLIB (https://lrclib.net) is a free, open catalogue of synced lyrics. For a
song we ask /api/get with the artist, title, album and length, and fall back
to /api/search when that misses, then keep only an answer whose artist and
title match ours and whose length is within a few seconds -- a different
edit of a song has different timing, which is worse than no timing at all.

Everything runs on a background worker, never on a request: a track is
queued when the feeder prepares it, and a slow backfill walks the library
the way enrich.py does. At most one call a second, a pause when LRCLIB says
slow down or falls over, and a miss is remembered for a fortnight so a song
it does not know is not asked about on every pass. What comes back stays in
the local database and is only ever shown to you.

The tracks.lyrics column is left alone: that is text embedded in your own
files, and song compatibility reads it as such.
"""
from __future__ import annotations

import json
import re
import threading
import time
from collections import deque
from typing import Any, Callable

import httpx

from . import about, config, db, lyric_sections

API = "https://lrclib.net/api"
SOURCE = "lrclib"
MISS_COOLDOWN = 14 * 86400.0      # LRCLIB had nothing (or nothing that matched)
ERROR_COOLDOWN = 6 * 3600.0       # the network or LRCLIB failed; try again sooner
MIN_GAP = 1.05                    # seconds between calls
DURATION_SLACK = 3.0              # seconds a match may differ in length
MAX_LINES = 1500

_lock = threading.Lock()
_last_call = 0.0
_backoff_until = 0.0
_failures = 0
_sleep: Callable[[float], None] = time.sleep
_queue: deque[dict] = deque(maxlen=64)
_wake = threading.Event()
_thread: threading.Thread | None = None


def _log(*parts: Any) -> None:
    if config.DEBUG:
        print("[lyrics]", *parts, flush=True)


def enabled() -> bool:
    return bool(config.station.get("lyrics.enabled", True))


def user_agent() -> str:
    return f"Defalt/{about.VERSION} (https://github.com/ZacharyAParker/Defalt)"


# --------------------------------------------------------------------------
# LRC
# --------------------------------------------------------------------------
_STAMP = re.compile(r"\[(\d{1,3}):(\d{1,2}(?:[.:]\d{1,3})?)\]")
_OFFSET = re.compile(r"^\s*\[offset:\s*([+-]?\d+)\s*\]\s*$", re.IGNORECASE)
_WORD_STAMP = re.compile(r"<\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?>")


def _seconds(minutes: str, rest: str) -> float | None:
    rest = rest.replace(":", ".")
    try:
        value = int(minutes) * 60 + float(rest)
    except ValueError:
        return None
    return value if value >= 0 else None


def parse_lrc(text: str) -> list[dict]:
    """[{t, text}] from LRC, sorted by time.

    A line may carry several timestamps ("[00:12.00][01:30.00]chorus line");
    each becomes its own entry. [offset:+ms] moves every line earlier by that
    many milliseconds, as the format says. A timestamp with no words is kept
    with empty text: it is the song saying the singing stops there. Tags
    like [ar:] and anything that is not a timestamped line are ignored.
    """
    if not isinstance(text, str):
        return []
    offset = 0.0
    found: list[tuple[float, str]] = []
    for raw in text.splitlines():
        match = _OFFSET.match(raw)
        if match:
            offset = int(match.group(1)) / 1000.0
            continue
        line = raw.strip()
        stamps = []
        while True:
            stamp = _STAMP.match(line)
            if not stamp:
                break
            seconds = _seconds(stamp.group(1), stamp.group(2))
            if seconds is not None:
                stamps.append(seconds)
            line = line[stamp.end():].lstrip()
        if not stamps:
            continue
        words = " ".join(_WORD_STAMP.sub("", line).split())[:300]
        for seconds in stamps:
            found.append((seconds, words))
    lines: list[dict] = []
    seen = set()
    for seconds, words in sorted(found, key=lambda pair: pair[0]):
        t = round(max(0.0, seconds - offset), 2)
        if (t, words) in seen:
            continue
        seen.add((t, words))
        # A run of blank stamps is one pause.
        if not words and lines and not lines[-1]["text"]:
            continue
        lines.append({"t": t, "text": words})
        if len(lines) >= MAX_LINES:
            break
    while lines and not lines[0]["text"]:
        lines.pop(0)
    return lines


# --------------------------------------------------------------------------
# LRCLIB
# --------------------------------------------------------------------------
class Unavailable(Exception):
    """LRCLIB is rate limiting or failing; nothing was learned about the song."""


def _pace() -> None:
    global _last_call
    with _lock:
        wait = max(_last_call + MIN_GAP, _backoff_until) - time.monotonic()
        _last_call = time.monotonic() + max(0.0, wait)
    if wait > 0:
        _sleep(wait)


def _timeout() -> httpx.Timeout:
    seconds = float(config.station.get("lyrics.timeout_seconds", 8) or 8)
    return httpx.Timeout(max(1.0, seconds), connect=min(4.0, max(1.0, seconds)))


def _get(path: str, params: dict[str, Any]) -> Any:
    """One polite GET. None for a 404; Unavailable on 429 or a server error."""
    global _backoff_until, _failures
    _pace()
    response = httpx.get(API + path, params=params, timeout=_timeout(),
                         headers={"User-Agent": user_agent(), "Accept": "application/json"})
    if response.status_code == 404:
        return None
    if response.status_code == 429 or response.status_code >= 500:
        with _lock:
            _failures += 1
            try:
                wait = float(response.headers.get("Retry-After", ""))
            except ValueError:
                wait = 30.0 * 2 ** min(_failures - 1, 7)
            _backoff_until = time.monotonic() + min(max(wait, 5.0), 3600.0)
        raise Unavailable(response.status_code)
    response.raise_for_status()
    with _lock:
        _failures = 0
    return response.json()


def _same_artist(a: str, b: str) -> bool:
    left, right = db.norm(db.primary_artist(a)), db.norm(db.primary_artist(b))
    if not left or not right:
        return False
    return left == right or db.norm(a) == db.norm(b) or left in db.norm(b) or right in db.norm(a)


def match_score(record: Any, track: dict) -> float | None:
    """How well one LRCLIB record fits our track, or None if it is not it."""
    if not isinstance(record, dict):
        return None
    if db.norm(str(record.get("trackName") or "")) != db.norm(str(track.get("title") or "")):
        return None
    if not _same_artist(str(record.get("artistName") or ""), str(track.get("artist") or "")):
        return None
    ours, theirs = track.get("duration"), record.get("duration")
    gap = 0.0
    try:
        if ours and theirs:
            gap = abs(float(ours) - float(theirs))
            if gap > DURATION_SLACK:
                return None
    except (TypeError, ValueError):
        return None
    score = 1.0 - gap / (DURATION_SLACK * 4)
    if isinstance(record.get("syncedLyrics"), str) and record["syncedLyrics"].strip():
        score += 2.0
    elif isinstance(record.get("plainLyrics"), str) and record["plainLyrics"].strip():
        score += 1.0
    elif record.get("instrumental"):
        score += 0.5
    else:
        return None
    return score


def lookup(track: dict) -> dict | None:
    """The best matching LRCLIB record for a track, or None. Raises Unavailable."""
    params: dict[str, Any] = {"track_name": track["title"], "artist_name": track["artist"]}
    if str(track.get("album") or "").strip():
        params["album_name"] = track["album"]
    duration = track.get("duration")
    if duration:
        params["duration"] = int(round(float(duration)))
        record = _get("/get", params)
        if match_score(record, track) is not None:
            return record
    results = _get("/search", {"track_name": track["title"], "artist_name": track["artist"]})
    scored = [(match_score(record, track), record) for record in (results or [])[:20]
              if isinstance(results, list)]
    scored = [(score, record) for score, record in scored if score is not None]
    if not scored:
        return None
    return max(scored, key=lambda pair: pair[0])[1]


def result_from(record: dict | None) -> dict:
    """What to store for one lookup answer (or a miss)."""
    if not record:
        return {"status": "missing"}
    synced = parse_lrc(record.get("syncedLyrics") or "")
    plain = record.get("plainLyrics") if isinstance(record.get("plainLyrics"), str) else ""
    ident = record.get("id") if isinstance(record.get("id"), int) else None
    if sum(1 for line in synced if line["text"]) >= 2:
        return {"status": "synced", "synced": synced, "plain": plain, "lrclib_id": ident}
    if plain.strip():
        return {"status": "plain", "synced": [], "plain": plain, "lrclib_id": ident}
    if record.get("instrumental"):
        return {"status": "instrumental", "synced": [], "plain": "", "lrclib_id": ident,
                "instrumental": True}
    return {"status": "missing"}


# --------------------------------------------------------------------------
# Storage
# --------------------------------------------------------------------------
def _analysis(lines: list[dict], track: dict) -> tuple[list, list]:
    from . import structure
    # The grid and the file live on the track row; a queued request only
    # carries enough to look the song up.
    stored = db.one("SELECT * FROM tracks WHERE key=?", (track.get("key"),)) if track.get("key") else None
    track = {**dict(stored), **track} if stored else track
    try:
        duration = float(track.get("duration") or 0) or None
    except (TypeError, ValueError):
        duration = None
    return lyric_sections.build(lines, duration, track, structure.profile_for(track))


def save(track: dict, result: dict, now: float | None = None) -> None:
    now = time.time() if now is None else now
    synced = result.get("synced") or []
    sections, spans = _analysis(synced, track) if synced else ([], [])
    db.write(
        "INSERT INTO lyrics (track_key, status, synced, plain, sections, vocal_spans, source, "
        "lrclib_id, instrumental, fetched_at, sections_version) VALUES (?,?,?,?,?,?,?,?,?,?,?) "
        "ON CONFLICT(track_key) DO UPDATE SET status=excluded.status, synced=excluded.synced, "
        "plain=excluded.plain, sections=excluded.sections, vocal_spans=excluded.vocal_spans, "
        "source=excluded.source, lrclib_id=excluded.lrclib_id, instrumental=excluded.instrumental, "
        "fetched_at=excluded.fetched_at, sections_version=excluded.sections_version",
        (track["key"], result["status"], json.dumps(synced, ensure_ascii=False) if synced else None,
         (result.get("plain") or None), json.dumps(sections) if sections else None,
         json.dumps(spans) if spans else None, SOURCE, result.get("lrclib_id"),
         1 if result.get("instrumental") else 0, now, lyric_sections.VERSION))


def row(key: str) -> dict | None:
    found = db.one("SELECT * FROM lyrics WHERE track_key=?", (key,))
    return dict(found) if found else None


def _loads(value: Any) -> list:
    try:
        data = json.loads(value) if value else []
    except (TypeError, ValueError):
        return []
    return data if isinstance(data, list) else []


def due(existing: dict | None, now: float | None = None) -> bool:
    """Whether a track should be looked up (again)."""
    if existing is None:
        return True
    now = time.time() if now is None else now
    status = existing.get("status")
    age = now - float(existing.get("fetched_at") or 0)
    if status == "missing":
        return age >= MISS_COOLDOWN
    if status == "error":
        return age >= ERROR_COOLDOWN
    return False


def lookable(track: dict) -> bool:
    return bool(track.get("key") and str(track.get("title") or "").strip()
                and str(track.get("artist") or "").strip()
                and track.get("artist") != "Unknown Artist")


def fetch(track: dict, now: float | None = None) -> str:
    """Look one track up if it is due, store the answer, return its status.

    Unavailable (a rate limit, LRCLIB down) is re-raised without storing,
    so the track stays first in line once the pause is over.
    """
    if not lookable(track):
        return "skipped"
    existing = row(track["key"])
    if not due(existing, now):
        return str(existing["status"])
    try:
        result = result_from(lookup(track))
    except Unavailable:
        raise
    except (httpx.HTTPError, ValueError, KeyError, TypeError) as error:
        _log("lookup failed", track.get("key"), repr(error))
        result = {"status": "error"}
    if result["status"] == "error" and existing and existing.get("status") in ("synced", "plain"):
        return str(existing["status"])
    save(track, result, now)
    _log(result["status"], track.get("artist"), "-", track.get("title"))
    return result["status"]


def attach(track: dict) -> dict:
    """The track, carrying its lyric map when there is one. Database only.

    lyric_map: {status, sections, vocal_spans, first_line, last_line}. The
    planner and the scheduler read this; nothing here touches the network.
    """
    if not isinstance(track, dict) or not track.get("key"):
        return track
    try:
        found = row(track["key"])
    except Exception as error:  # noqa: BLE001 - lyrics are a nicety
        _log("read failed", repr(error))
        return track
    if not found or found.get("status") not in ("synced", "plain", "instrumental"):
        return track
    lines = _loads(found.get("synced"))
    sections, spans = _loads(found.get("sections")), _loads(found.get("vocal_spans"))
    if lines and found.get("sections_version") != lyric_sections.VERSION:
        sections, spans = _analysis(lines, track)
        db.write("UPDATE lyrics SET sections=?, vocal_spans=?, sections_version=? WHERE track_key=?",
                 (json.dumps(sections) if sections else None, json.dumps(spans) if spans else None,
                  lyric_sections.VERSION, track["key"]))
    sung = [line["t"] for line in lines if isinstance(line, dict) and line.get("text")]
    lyric_map = {"status": found["status"], "sections": sections, "vocal_spans": spans}
    if sung:
        lyric_map["first_line"] = sung[0]
        lyric_map["last_line"] = sung[-1]
    return {**track, "lyric_map": lyric_map}


def payload(key: str) -> dict | None:
    """GET /api/lyrics/<key>: the lines and the sections, or None."""
    found = row(key)
    if not found or found.get("status") not in ("synced", "plain", "instrumental"):
        return None
    return {"key": key, "status": found["status"], "source": found.get("source") or SOURCE,
            "lines": _loads(found.get("synced")), "plain": found.get("plain") or "",
            "sections": _loads(found.get("sections")), "vocal_spans": _loads(found.get("vocal_spans")),
            "instrumental": bool(found.get("instrumental")), "fetched_at": found.get("fetched_at")}


def quotable(key: str, words: int = 10) -> list[str]:
    """Short sung lines a host could quote, chorus lines first."""
    found = row(key)
    if not found:
        return []
    lines = _loads(found.get("synced"))
    texts = [str(line.get("text") or "") for line in lines if isinstance(line, dict)]
    if not texts and found.get("plain"):
        texts = [part.strip() for part in str(found["plain"]).splitlines()]
    sections = _loads(found.get("sections"))
    in_chorus = set()
    for line in lines:
        span = lyric_sections.at(sections, float(line.get("t") or 0)) if isinstance(line, dict) else None
        if span and span.get("label") == "chorus":
            in_chorus.add(str(line.get("text") or ""))
    usable = [t for t in dict.fromkeys(texts) if 3 <= len(t.split()) <= words]
    return sorted(usable, key=lambda t: t not in in_chorus)


def all_lines(key: str) -> list[str]:
    found = row(key)
    if not found:
        return []
    texts = [str(line.get("text") or "") for line in _loads(found.get("synced")) if isinstance(line, dict)]
    if found.get("plain"):
        texts += [part.strip() for part in str(found["plain"]).splitlines()]
    return [t for t in dict.fromkeys(texts) if t]


# --------------------------------------------------------------------------
# The worker
# --------------------------------------------------------------------------
def request(track: dict) -> None:
    """Ask for this track's lyrics soon. Never blocks and never calls out."""
    if not enabled() or not isinstance(track, dict) or not lookable(track):
        return
    wanted = {name: track.get(name) for name in ("key", "title", "artist", "album", "duration")}
    with _lock:
        if all(item["key"] != wanted["key"] for item in _queue):
            _queue.append(wanted)
    _wake.set()


def pending(limit: int, now: float | None = None) -> list[dict]:
    """Library tracks with no answer yet, or a miss old enough to retry."""
    now = time.time() if now is None else now
    return [dict(r) for r in db.query(
        "SELECT t.key, t.title, t.artist, t.album, t.duration FROM tracks t "
        "LEFT JOIN lyrics l ON l.track_key=t.key "
        "WHERE t.blocked=0 AND t.artist != 'Unknown Artist' AND TRIM(t.title) != '' "
        "AND t.duration > 0 AND (l.track_key IS NULL "
        "     OR (l.status='missing' AND l.fetched_at < ?) OR (l.status='error' AND l.fetched_at < ?)) "
        "ORDER BY t.play_count DESC, t.added_at DESC LIMIT ?",
        (now - MISS_COOLDOWN, now - ERROR_COOLDOWN, limit))]


def _take(work: list[dict], rest: int) -> None:
    """Put unfinished work back at the front of the queue."""
    with _lock:
        _queue.extendleft(reversed(work[rest:]))


def run_batch(limit: int | None = None, cancelled: Callable[[], bool] = lambda: False,
              backfill: bool = True) -> dict[str, int]:
    """Queued tracks first, then (when `backfill`) up to `limit` from the library.

    Stops at a rate limit or an outage and leaves what is left queued, so the
    next pass carries on from the same place once the pause is over.
    """
    counts: dict[str, int] = {}
    if not enabled():
        return counts
    work: list[dict] = []
    with _lock:
        while _queue:
            work.append(_queue.popleft())
    if backfill and bool(config.station.get("lyrics.backfill", True)):
        size = int(limit or config.station.get("lyrics.batch", 10) or 10)
        known = {item["key"] for item in work}
        work += [track for track in pending(size) if track["key"] not in known]
    for index, track in enumerate(work):
        if cancelled() or time.monotonic() < _backoff_until:
            _take(work, index)
            break
        try:
            status = fetch(track)
        except Unavailable:
            _take(work, index)
            break
        counts[status] = counts.get(status, 0) + 1
    return counts


def start(stop: threading.Event) -> threading.Thread:
    """The worker: requested tracks as they come in, and a small backfill
    batch from the library every few minutes."""
    global _thread
    if _thread is not None and _thread.is_alive():
        return _thread

    def loop() -> None:
        stop.wait(20)  # let the station come up first
        next_backfill = 0.0
        while not stop.is_set():
            _wake.clear()
            backfill = time.monotonic() >= next_backfill
            try:
                if backfill or _queue:
                    run_batch(cancelled=stop.is_set, backfill=backfill)
            except Exception as error:  # noqa: BLE001 - never kill the worker
                _log("batch failed", repr(error))
            if backfill:
                minutes = float(config.station.get("lyrics.interval_minutes", 4) or 4)
                next_backfill = time.monotonic() + max(60.0, minutes * 60)
            pause = max(2.0, _backoff_until - time.monotonic())
            _wake.wait(min(pause, max(2.0, next_backfill - time.monotonic())))

    _thread = threading.Thread(target=loop, daemon=True, name="lyrics")
    _thread.start()
    return _thread

