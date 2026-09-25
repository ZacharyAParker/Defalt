"""Cached songs that came from a clean upload, swapped for the explicit one.

Audio downloaded before the resolver could read YouTube Music's explicit flag
may be the censored edition: the auto-generated uploads of a clean and an
explicit release look identical to yt-dlp. This walks the cache in the
background, a couple of songs a minute, and asks YouTube Music about each.
Only an upload it reports as not explicit, with an explicit release of the same
recording beside it, is re-downloaded. The old file plays until the new one is
ready, anything scheduled or queued is left alone, and every answer is recorded
so a song is not asked about again for a month.

Exact links and local files are never touched. A failure keeps the audio that
works and tries again in an hour.
"""
from __future__ import annotations

import json
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterable

from . import config, db, library, versions, ytmusic

PER_PASS = 2          # songs looked up per pass
PASS_GAP = 60.0       # seconds between passes
SWAPS_PER_PASS = 1    # re-downloads per pass

_thread: threading.Thread | None = None


def _log(*parts: Any) -> None:
    if config.DEBUG:
        print("[editions]", *parts, flush=True)


def enabled() -> bool:
    return bool(config.station.get("selection.avoid_clean_versions", True)
                and config.station.get("selection.recheck_cached_editions", True))


def cached(limit: int | None = None, *, unchecked_only: bool = True) -> list[dict[str, Any]]:
    """Downloaded tracks whose source could be the clean edition, most played first."""
    found = []
    for row in db.query(
            "SELECT * FROM tracks WHERE file IS NOT NULL AND video_id IS NOT NULL AND blocked=0 "
            "AND (source IS NULL OR source != 'local') AND (source_url IS NULL OR source_url = '') "
            "ORDER BY play_count DESC, COALESCE(last_played, 0) DESC, key"):
        track = dict(row)
        # Somebody asked for the clean edition by name: that is what it is.
        if versions.is_clean_label(track["title"] or "") or not Path(track["file"]).is_file():
            continue
        if unchecked_only and library._edition_checked(track["video_id"], track["artist"], track["title"]):
            continue
        found.append(track)
        if limit and len(found) >= limit:
            break
    return found


def _labels(video_id: str) -> dict[str, Any]:
    # Only what is already on disk: a report must not wait on yt-dlp.
    try:
        info = json.loads((config.CACHE_DIR / "source-info" / f"{video_id}.json").read_text(encoding="utf-8"))
        return info if isinstance(info, dict) and info.get("id") == video_id else {}
    except (OSError, ValueError):
        return {}


def check(track: dict[str, Any]) -> dict[str, Any]:
    """What YouTube Music says about this cached upload. See ytmusic.verdict."""
    clean, explicit = versions.metadata_edition(_labels(track["video_id"]), track["artist"], track["title"])
    if explicit:
        return {"status": "explicit", "upgrade": None, "labelled": True}
    found = ytmusic.verdict(track["video_id"], track["artist"], track["title"], track.get("duration") or 0)
    if found is None:
        return {"status": "unreachable", "upgrade": None}
    if clean and found["status"] == "unknown":
        # Its own labels say clean; YouTube Music only lacked the flag.
        return {**found, "status": "upgrade", "upgrade": found.get("explicit")}
    return found


def _idle(track: dict[str, Any], keep: set[str]) -> bool:
    """Not scheduled, queued, being prepared, or prepared moments ago."""
    grace = float(config.station.get("cache.fresh_grace_minutes", 15) or 0) * 60
    return (library._same_file_key(track["file"]) not in keep
            and track["key"] not in library._INFLIGHT
            and time.time() - float(track.get("cache_used_at") or 0) >= grace)


def run_batch(limit: int = PER_PASS, protect: Callable[[], Iterable[str]] | None = None, *,
              dry_run: bool = False, swaps: int = SWAPS_PER_PASS, unchecked_only: bool = True,
              progress: Callable[[dict[str, Any], dict[str, Any]], None] | None = None
              ) -> list[tuple[dict[str, Any], dict[str, Any]]]:
    """Look up to `limit` cached songs; swap at most `swaps` of them.

    `protect` returns the audio files still wanted on air. A dry run looks
    and reports only: no downloads and no records.
    """
    results = []
    keep: set[str] = set()
    if not dry_run and protect:
        keep = {library._same_file_key(p) for p in (protect() or ()) if p}
    # Busy songs wait for a later pass instead of holding up the rest.
    tracks = [track for track in cached(None, unchecked_only=unchecked_only)
              if dry_run or _idle(track, keep)]
    for track in tracks[:limit] if limit else tracks:
        result = check(track)
        if result["status"] == "unreachable":
            # YouTube Music is down or slow: stop, and pick this up next pass.
            results.append((track, result))
            if progress:
                progress(track, result)
            break
        if not dry_run:
            artist, title, video_id = track["artist"], track["title"], track["video_id"]
            if result["status"] == "upgrade" and result.get("upgrade"):
                if protect:
                    keep = {library._same_file_key(p) for p in (protect() or ()) if p}
                if swaps <= 0 or not _idle(dict(db.one("SELECT * FROM tracks WHERE key=?", (track["key"],))
                                                 or track), keep):
                    result = {**result, "deferred": True}  # on air or busy; next pass
                else:
                    swaps -= 1
                    swapped = library.upgrade(track, result["upgrade"], protect)
                    result = {**result, "swapped": swapped}
                    if not swapped:
                        library._record_edition_check(video_id, artist, title, success=False)
            else:
                # "unknown" is answered again sooner: the next preparation of
                # this song may still settle it with a fresh search.
                library._record_edition_check(video_id, artist, title,
                                              flagged=result["status"] != "unknown",
                                              status=result["status"])
        results.append((track, result))
        if progress:
            progress(track, result)
    return results


def start(stop: threading.Event, protect: Callable[[], Iterable[str]] | None = None) -> threading.Thread:
    """The background worker: a couple of songs a minute, never on a request thread."""
    global _thread
    if _thread is not None and _thread.is_alive():
        return _thread

    def loop() -> None:
        stop.wait(120)  # let the station come up and start feeding first
        while not stop.is_set():
            try:
                if enabled():
                    for track, result in run_batch(protect=protect):
                        if result.get("swapped") or result["status"] == "upgrade":
                            _log(track["artist"], "-", track["title"], result)
            except Exception as error:  # noqa: BLE001 - never kill the worker
                _log("pass failed", error)
            stop.wait(PASS_GAP)

    _thread = threading.Thread(target=loop, daemon=True, name="editions")
    _thread.start()
    return _thread
