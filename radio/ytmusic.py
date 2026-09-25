"""YouTube Music's explicit badge, which yt-dlp never sees.

The auto-generated upload of a clean release carries the same title, album and
"Provided to YouTube by" description as the explicit one. The only place the
difference shows is the little E next to the song in YouTube Music. This asks
YouTube Music (signed out: no account, no cookies) which releases of a song
exist and which of them are explicit, so the resolver can take the uncensored
record when there is one.

Every call runs in a child process with a wall-clock deadline, calls are paced,
and answers are remembered on disk: a song for two weeks, a song YouTube Music
does not know for three days. A failure is remembered for an hour and pauses
every lookup for a few minutes. Anything that goes wrong returns None, and the
caller carries on exactly as it did before this existed. Background paths only,
never a request thread.
"""
from __future__ import annotations

import hashlib
import json
import re
import threading
import time
import unicodedata
from typing import Any

from . import config, db, sourceio, versions

TTL = 14 * 86400.0          # a song's releases rarely change
EMPTY_TTL = 3 * 86400.0     # not on YouTube Music, or not yet
FAILURE_COOLDOWN = 3600.0   # the same lookup, after a timeout or an error
OUTAGE_PAUSE = 300.0        # every lookup, after any failure
MIN_GAP = 1.5               # seconds between calls
SLACK = 4.0                 # seconds two editions of one recording may differ
ATV = "MUSIC_VIDEO_TYPE_ATV"

_ID = re.compile(r"[A-Za-z0-9_-]{11}")
# Takes YouTube Music files as songs even though they are not the record.
_ALTERNATE_ALBUM = r"(?:slowed|reverb|sped[\s-]?up|nightcore|8d|karaoke|instrumental|live|acoustic|remix(?:es)?)"

_lock = threading.Lock()
_last_call = 0.0
_paused_until = 0.0
_failed: dict[str, float] = {}


def _log(*parts: Any) -> None:
    if config.DEBUG:
        print("[ytmusic]", *parts, flush=True)


def _reset() -> None:
    """Forget pacing and failures. Tests only."""
    global _last_call, _paused_until
    with _lock:
        _last_call = _paused_until = 0.0
        _failed.clear()


def _pace() -> None:
    global _last_call
    with _lock:
        wait = _last_call + MIN_GAP - time.monotonic()
        _last_call = time.monotonic() + max(0.0, wait)
    if wait > 0:
        time.sleep(wait)


def _seconds(value: Any) -> int:
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return int(value) if 0 < value < 86400 else 0
    if isinstance(value, str) and re.fullmatch(r"\d{1,2}(?::\d\d){1,2}", value.strip()):
        total = 0
        for part in value.strip().split(":"):
            total = total * 60 + int(part)
        return total
    return 0


def _song(item: Any) -> dict[str, Any] | None:
    """One result, reduced to what the resolver reads. Anything odd is dropped."""
    if not isinstance(item, dict) or not _ID.fullmatch(str(item.get("videoId") or item.get("video_id") or "")):
        return None
    artists = item.get("artists") or []
    names = [a.get("name") if isinstance(a, dict) else a for a in artists if isinstance(a, (dict, str))]
    album = item.get("album")
    explicit = item.get("isExplicit", item.get("explicit"))
    return {
        "video_id": str(item.get("videoId") or item.get("video_id")),
        "title": str(item.get("title") or "")[:200],
        "artists": [str(name)[:120] for name in names if isinstance(name, str) and name][:8],
        "album": str((album.get("name") if isinstance(album, dict) else album) or "")[:200],
        "duration": _seconds(item.get("duration_seconds") or item.get("duration") or item.get("length")),
        "explicit": explicit if isinstance(explicit, bool) else None,
        "video_type": str(item.get("videoType") or item.get("video_type") or ""),
    }


# --------------------------------------------------------------------------
# The child side: runs inside `python -m radio.sourceio ytmusic`.
# --------------------------------------------------------------------------
def fetch(payload: dict[str, Any]) -> Any:
    from ytmusicapi import YTMusic
    client = YTMusic()
    if payload.get("op") == "songs":
        found = client.search(str(payload["query"])[:200], filter="songs", limit=20)
        return [song for song in map(_song, found or []) if song]
    if payload.get("op") == "edition":
        # A song's own page has no explicit flag. Its album does, per track.
        video_id = str(payload["video_id"])
        watch = client.get_watch_playlist(videoId=video_id, limit=1) or {}
        track = next((t for t in watch.get("tracks") or []
                      if isinstance(t, dict) and t.get("videoId") == video_id), None)
        song = _song(track)
        if not song:
            return {}
        album_id = ((track.get("album") or {}) if isinstance(track.get("album"), dict) else {}).get("id")
        song["explicit"] = None
        if isinstance(album_id, str) and album_id:
            album = client.get_album(album_id) or {}
            flags = {t.get("isExplicit") for t in album.get("tracks") or []
                     if isinstance(t, dict) and isinstance(t.get("isExplicit"), bool)
                     and db.norm(t.get("title") or "") == db.norm(song["title"])}
            if len(flags) == 1:
                song["explicit"] = flags.pop()
            elif isinstance(album.get("isExplicit"), bool):
                song["explicit"] = album["isExplicit"]
        return song
    raise ValueError("unknown YouTube Music lookup")


# --------------------------------------------------------------------------
# The station side: cached, paced, bounded.
# --------------------------------------------------------------------------
def _lookup(op: str, argument: str) -> Any:
    global _paused_until
    name = hashlib.sha256(json.dumps([op, argument], ensure_ascii=False).encode("utf-8")).hexdigest()
    path = config.CACHE_DIR / "source-info" / "ytmusic" / f"{name}.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        if (data.get("version") == 1 and time.time() - float(data["at"])
                < (TTL if data.get("result") else EMPTY_TTL)):
            return data["result"]
    except (OSError, ValueError, TypeError, KeyError):
        pass
    with _lock:
        now = time.monotonic()
        if now < _paused_until or _failed.get(name, 0.0) > now:
            return None
    _pace()
    try:
        payload = {"op": op, "query" if op == "songs" else "video_id": argument}
        raw = sourceio.ytmusic(payload)
        if op == "songs":
            if not isinstance(raw, list):
                raise sourceio.SourceError("unexpected answer")
            result: Any = [song for song in map(_song, raw) if song]
        else:
            if not isinstance(raw, dict):
                raise sourceio.SourceError("unexpected answer")
            result = _song(raw) or {}
    except Exception as error:  # noqa: BLE001 - never block playback over a badge
        _log("lookup failed", op, argument, error)
        with _lock:
            _failed[name] = time.monotonic() + FAILURE_COOLDOWN
            _paused_until = time.monotonic() + OUTAGE_PAUSE
        return None
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"version": 1, "at": time.time(), "result": result},
                                   ensure_ascii=False), encoding="utf-8")
    except OSError:
        pass
    return result


def songs(artist: str, title: str) -> list[dict[str, Any]] | None:
    """YouTube Music's song results for this record; None when it could not be asked."""
    primary = db.primary_artist(artist) or artist
    return _lookup("songs", f"{primary} {title}".strip())


def edition(video_id: str) -> dict[str, Any] | None:
    """One upload's release details, explicit flag included (None when unknown)."""
    if not _ID.fullmatch(video_id or ""):
        return {}
    return _lookup("edition", video_id)


def _near(a: float, b: float) -> bool:
    return bool(a and b and abs(a - b) <= SLACK)


def _album_base(album: str) -> str:
    # "V" and "V (Deluxe)" are one record; the deluxe tracklist just runs longer.
    return db.norm(re.sub(r"\s*[\(\[][^\)\]]*(?:deluxe|expanded|edition|version|remaster|anniversary|bonus)"
                          r"[^\)\]]*[\)\]]", "", album or "", flags=re.I))


def _same_album(a: str, b: str) -> bool:
    return bool(_album_base(a)) and _album_base(a) == _album_base(b)


_LETTERS = str.maketrans({"ø": "o", "æ": "ae", "œ": "oe", "ß": "ss", "đ": "d", "ł": "l", "ı": "i"})


def _fold(text: str) -> str:
    """ROSÉ and ROSE, BØRNS and BORNS, Run-D.M.C. and RUN DMC are one credit."""
    text = unicodedata.normalize("NFKD", db.norm(text).translate(_LETTERS))
    return "".join(c for c in text if not unicodedata.combining(c) and not c.isspace())


def matches(found: list[dict[str, Any]], artist: str, title: str) -> list[dict[str, Any]]:
    """Results that are this recording: same song and artist, not another take.

    Featured credits are ignored on both sides. Slowed, reverb, sped-up and
    live uploads come back as songs too; the title check drops most of them
    and the album check catches the rest, unless that take was asked for.
    """
    wanted = _fold(title)
    credit = _fold(artist)
    primary = _fold(db.primary_artist(artist))
    asked_alternate = versions.alternate_track({"title": title}) or bool(
        re.search(r"\b" + _ALTERNATE_ALBUM + r"\b", title or "", re.I))
    kept = []
    for song in found or []:
        if song.get("video_type") and song["video_type"] != ATV:
            continue  # a music video or a fan upload, not the release audio
        if not wanted or _fold(song["title"]) != wanted:
            continue
        names = [_fold(name) for name in song["artists"]]
        if primary and not (primary in names or any(name and name in credit for name in names)):
            continue
        album_alternate = versions.alternate_track({"album": song["album"]}) or versions._tagged(
            song["album"], _ALTERNATE_ALBUM)
        if album_alternate and not asked_alternate:
            continue
        if versions.is_clean_label(song["album"]) or versions.is_clean_label(song["title"]):
            song = {**song, "explicit": False}
        kept.append(song)
    return kept


def explicit_choice(artist: str, title: str, expected_ms: int = 0) -> dict[str, Any] | None:
    """Which releases of this song are explicit, best first.

    None: YouTube Music could not be asked, so behave as before. Otherwise
    `explicit` and `clean` list matching releases; an empty `explicit` with a
    non-empty `clean` means the song has no explicit version at all.
    """
    found = songs(artist, title)
    if found is None:
        return None
    matched = matches(found, artist, title)
    clean = [song for song in matched if song["explicit"] is False]
    expected = (expected_ms or 0) / 1000.0

    def fits(song: dict[str, Any]) -> bool:
        if expected and _near(song["duration"], expected):
            return True
        if clean:
            return any(_near(song["duration"], other["duration"]) or _same_album(song["album"], other["album"])
                       for other in clean)
        return not expected or not song["duration"]

    explicit = [song for song in matched if song["explicit"] and fits(song)]
    return {"explicit": explicit, "clean": clean, "matched": matched, "found": found}


def verdict(video_id: str, artist: str, title: str, duration: float = 0) -> dict[str, Any] | None:
    """Is this cached upload the explicit edition, and if not, which one is?

    status: explicit, upgrade (see `upgrade`: this is the clean release, or a
    video or re-upload, and an explicit release exists), no-explicit-version,
    unknown (explicit releases exist but this release upload's own flag could
    not be read) or not-found. None when YouTube Music could not be asked.
    """
    choice = explicit_choice(artist, title)
    if choice is None:
        return None
    if not choice["matched"]:
        return {"status": "not-found", "upgrade": None}
    if any(song["video_id"] == video_id for song in choice["explicit"]):
        return {"status": "explicit", "upgrade": None}
    if not any(song["explicit"] for song in choice["matched"]):
        return {"status": "no-explicit-version", "upgrade": None}
    current = next((song for song in choice["found"] if song["video_id"] == video_id), None)
    if current is None or current["explicit"] is None:
        looked_up = edition(video_id)
        if looked_up is None:
            return None
        current = {**(current or {}), **looked_up} if looked_up else current
    flag = current.get("explicit") if current else None
    if flag:
        return {"status": "explicit", "upgrade": None}
    reference = float(duration or 0) or float((current or {}).get("duration") or 0)
    album = (current or {}).get("album") or ""
    siblings = [song for song in choice["matched"] if song["explicit"] and song["video_id"] != video_id
                and (_near(song["duration"], reference) or _same_album(song["album"], album)
                     or not reference or not song["duration"])]
    if not siblings:
        return {"status": "no-explicit-version", "upgrade": None}
    siblings.sort(key=lambda song: (not _same_album(song["album"], album),
                                    abs((song["duration"] or reference) - reference)))
    if flag is None:
        kind = (current or {}).get("video_type") or ""
        if kind and kind != ATV:
            # A music video or somebody's "(Official Audio)" re-upload, with
            # no flag of its own. The explicit release audio beats it either way.
            return {"status": "upgrade", "upgrade": siblings[0]["video_id"], "album": siblings[0]["album"],
                    "reason": "not the release upload"}
        return {"status": "unknown", "upgrade": None, "explicit": siblings[0]["video_id"]}
    return {"status": "upgrade", "upgrade": siblings[0]["video_id"], "album": siblings[0]["album"],
            "reason": "clean release"}
