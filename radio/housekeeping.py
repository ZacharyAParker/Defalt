"""Everything under cache/ that is not the audio cache, kept to a budget.

The audio cache has always had a size budget. The rest of cache/ -- stems,
cover art, waveform and structure analyses, source lookups, the director's
scratch folders -- only ever grew. The janitor calls `sweep` every pass; the
cheap part runs every time and the rest at most once an hour.

Nothing here deletes something the station still means to play: the caller
passes the files that are scheduled, queued or being built, and their
separations and analyses stay.
"""
from __future__ import annotations

import hashlib
import os
import shutil
import sys
import threading
import time
from pathlib import Path
from typing import Any, Iterable

from . import config, db, library

HOUR = 3600.0
DAY = 86400.0
_LOCK = threading.Lock()
_last_full = 0.0


def _setting(name: str, default: float) -> float:
    try:
        value = float(config.station.get(f"cache.{name}", default))
    except (TypeError, ValueError):
        return default
    return value if value == value and value >= 0 else default  # NaN guard


def sweep(protect: Iterable[str] = (), *, force: bool = False) -> dict[str, Any]:
    """One janitor pass. `protect` is audio paths still wanted on air."""
    global _last_full
    report: dict[str, Any] = {"staging": library.sweep_staging(1.0)}
    with _LOCK:
        now = time.time()
        if not force and now - _last_full < HOUR:
            return report
        _last_full = now
    wanted = [Path(p) for p in protect if p]
    for name, task in (("stems", lambda: prune_stems(wanted)),
                       ("artwork", prune_artwork),
                       ("analysis", prune_analysis),
                       ("source_info", prune_source_info),
                       ("director_sessions", prune_director_sessions),
                       ("history", lambda: db.prune_history(_setting("history_retention_days", 180)))):
        try:
            report[name] = task()
        except Exception as error:  # noqa: BLE001 - one bad folder, not the janitor
            report[name] = f"failed: {error}"
    return report


def _remove(path: Path) -> int:
    """Delete a file or folder; returns bytes freed (0 if it would not go)."""
    try:
        if path.is_dir():
            size = sum(p.stat().st_size for p in path.rglob("*") if p.is_file())
            shutil.rmtree(path)
            return size
        size = path.stat().st_size
        path.unlink()
        return size
    except OSError:
        return 0


def _age(path: Path, now: float) -> float:
    try:
        return now - path.stat().st_mtime
    except OSError:
        return 0.0


# --------------------------------------------------------------------------
# Stems: large (four FLACs per record), so they get a budget of their own.
# --------------------------------------------------------------------------
def prune_stems(protect: Iterable[Path] = ()) -> int:
    from . import stems
    root = config.CACHE_DIR / "stems"
    if not root.is_dir():
        return 0
    keep = set()
    for path in protect:
        try:
            keep.add(stems.cache_dir(path).name)
        except OSError:
            continue
    now = time.time()
    budget = _setting("stems_max_gb", 8.0) * 1024 ** 3
    max_age = _setting("stems_max_age_days", 60) * DAY
    freed = 0
    folders = []
    for folder in root.iterdir():
        if folder.name.endswith(".partial"):
            # A separation that died mid-run. A live one is younger than this.
            if _age(folder, now) > HOUR:
                freed += _remove(folder)
            continue
        if not folder.is_dir():
            continue
        size = sum(p.stat().st_size for p in folder.glob("*") if p.is_file())
        folders.append((folder.stat().st_mtime, size, folder))
    folders.sort()  # least recently used first; loading a cached split touches it
    total = sum(size for _, size, _ in folders)
    for mtime, size, folder in folders:
        if folder.name in keep or now - mtime < DAY:
            continue
        if not ((max_age and now - mtime > max_age) or (budget and total > budget)):
            continue
        gone = _remove(folder)
        freed += gone
        total -= size if gone else 0
    return freed


# --------------------------------------------------------------------------
# Cover art: small, but one per track ever looked at.
# --------------------------------------------------------------------------
def prune_artwork() -> int:
    root = config.CACHE_DIR / "artwork"
    if not root.is_dir():
        return 0
    now = time.time()
    max_age = _setting("artwork_max_age_days", 90) * DAY
    freed = 0
    for path in root.iterdir():
        if not path.is_file():
            continue
        stale_tmp = path.suffix == ".tmp" and _age(path, now) > HOUR
        if stale_tmp or (max_age and _age(path, now) > max_age):
            freed += _remove(path)
    return freed


# --------------------------------------------------------------------------
# Waveform peaks and structure profiles: keyed by the audio path's digest, so
# an entry whose audio is still known is kept and an orphan ages out.
# --------------------------------------------------------------------------
def _path_digests() -> set[str]:
    paths = {row["file"] for row in db.query("SELECT file FROM tracks WHERE file IS NOT NULL")}
    if library.AUDIO_DIR.is_dir():
        paths.update(str(p) for p in library.AUDIO_DIR.iterdir())
    digests = set()
    for raw in paths:
        try:
            resolved = str(Path(raw).resolve())
        except OSError:
            continue
        digests.add(hashlib.sha256(resolved.encode("utf-8")).hexdigest()[:32])
    return digests


def prune_analysis() -> int:
    known = _path_digests()
    now = time.time()
    max_age = _setting("analysis_max_age_days", 30) * DAY
    freed = 0
    for folder in (config.CACHE_DIR / "peaks", config.CACHE_DIR / "structure"):
        if not folder.is_dir():
            continue
        for path in folder.iterdir():
            if not path.is_file():
                continue
            age = _age(path, now)
            if path.name.split(".", 1)[0] in known and not path.name.endswith(".tmp"):
                continue
            # Orphans age out; a crashed writer's temp file after an hour.
            if (max_age and age > max_age) or (".tmp" in path.name and age > HOUR):
                freed += _remove(path)
    return freed


# --------------------------------------------------------------------------
# Source lookups: per-video descriptions, song context, edition checks.
# --------------------------------------------------------------------------
def prune_source_info() -> int:
    root = config.CACHE_DIR / "source-info"
    if not root.is_dir():
        return 0
    now = time.time()
    videos = {row["video_id"] for row in
              db.query("SELECT video_id FROM tracks WHERE video_id IS NOT NULL")}
    freed = 0
    for path in root.iterdir():
        if not path.is_file():
            continue
        age = _age(path, now)
        if path.name.startswith("song-context-"):
            # Trusted for a week at most by the reader; past that it is dead.
            if age > 8 * DAY:
                freed += _remove(path)
        elif path.suffix == ".json" and path.stem not in videos and age > 30 * DAY:
            freed += _remove(path)
    checks = root / "edition-checks"
    if checks.is_dir():
        for path in checks.iterdir():
            # Honoured for 30 days by library._edition_checked; then noise.
            if path.is_file() and _age(path, now) > 31 * DAY:
                freed += _remove(path)
    return freed


# --------------------------------------------------------------------------
# Director session scratch folders: <pid>-<uuid>, one set per process.
# --------------------------------------------------------------------------
def _alive(pid: int) -> bool:
    if pid == os.getpid():
        return True
    if sys.platform == "win32":
        import ctypes
        # os.kill(pid, 0) would *terminate* the process on Windows. Ask.
        kernel = ctypes.windll.kernel32
        handle = kernel.OpenProcess(0x1000, False, pid)  # QUERY_LIMITED_INFORMATION
        if not handle:
            return False
        try:
            code = ctypes.c_ulong()
            if not kernel.GetExitCodeProcess(handle, ctypes.byref(code)):
                return True
            return code.value == 259  # STILL_ACTIVE
        finally:
            kernel.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def prune_director_sessions() -> int:
    root = config.CACHE_DIR / "director-sessions"
    if not root.is_dir():
        return 0
    now = time.time()
    freed = 0
    for folder in root.iterdir():
        pid_text = folder.name.split("-", 1)[0]
        if not folder.is_dir() or not pid_text.isdigit():
            continue
        # A process that has gone away cannot come back for its folder. The
        # hour covers a pid reused by something unrelated in the meantime.
        if not _alive(int(pid_text)) and _age(folder, now) > HOUR:
            freed += _remove(folder)
    return freed
