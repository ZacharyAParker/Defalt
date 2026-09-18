"""Take a record apart into drums, bass, harmonic and vocals.

    python -m radio.stems "E:/Music/Alex G - Pretend.mp3"

Prints one JSON object per line, like the puller, so the console can follow
along and ignore anything that is not JSON.

Separation is slow and the results are worth keeping, so they are cached under
the project by the same identity the waveform cache uses: the audio path plus
its size and modification time. Move or re-encode the file and it separates
again; leave it alone and it never does.

Why this is not real-time: it does not need to be. The reference application
separates as it plays because it has to. A deck has the whole record in memory
before the first bar, so the parts can be prepared once and read like any
other audio.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

from . import config

#: Demucs writes these four, in this order, and the engine expects that order.
PARTS = ("drums", "bass", "other", "vocals")

#: The four-source model. `htdemucs` is the current default and the one whose
#: output names match PARTS.
MODEL = "htdemucs"

#: Bumped whenever separation starts producing different files for the same
#: input. Part of the cache identity, so a fix reaches records that were
#: already separated instead of only new ones.
CACHE_VERSION = 2


def child_env() -> dict[str, str]:
    """The environment the separator runs in.

    Demucs prints the file it is working on. On Windows a Python child
    inherits the console's legacy code page, so a record whose name carries
    anything outside it -- a fullwidth quote out of a YouTube title, an
    accent, a dash -- kills the separator on its own progress line before it
    has read a sample:

        UnicodeEncodeError: 'charmap' codec can't encode character '＂'

    The audio was never the problem, and neither was the path: it was the
    child's idea of what stdout can carry. Say it explicitly.
    """
    return dict(os.environ, PYTHONUTF8="1", PYTHONIOENCODING="utf-8")


def sample_rate_of(path: Path) -> int | None:
    """The rate a file is actually written at, or None if it cannot be read."""
    try:
        import mutagen

        audio = mutagen.File(path)
        rate = getattr(getattr(audio, "info", None), "sample_rate", None)
        return int(rate) if rate else None
    except Exception:
        return None


def match_rate(part: Path, target: int) -> None:
    """Put a separated part back on the record's own sample rate.

    Demucs works at its model's rate -- 44100 for htdemucs -- and writes what
    it worked at, whatever went in. A deck reads the parts at the rate it read
    the record at, because they are meant to be the same audio taken apart, so
    parts written at 44100 under a 48000 record play 8.8% fast: about a
    semitone and a half sharp, and drifting further out of time every bar.

    Resampling here rather than teaching the deck four more rates keeps the
    parts sample-for-sample aligned with the record, which is the property
    every other thing that touches them relies on.
    """
    rate = sample_rate_of(part)
    if rate is None or rate == target:
        return

    beside = part.with_name(part.name + ".matched.flac")
    command = [
        config.FFMPEG, "-hide_banner", "-nostdin", "-y",
        "-i", str(part),
        "-ar", str(target),
        "-c:a", "flac",
        str(beside),
    ]
    result = subprocess.run(
        command, capture_output=True, text=True, encoding="utf-8", errors="replace",
        env=child_env(),
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    if result.returncode != 0 or not beside.is_file():
        beside.unlink(missing_ok=True)
        tail = (result.stderr or "").strip().splitlines()
        raise RuntimeError(
            f"could not put {part.name} back on {target} Hz: "
            + (tail[-1] if tail else "ffmpeg failed"))
    beside.replace(part)


def emit(stage: str, **rest: Any) -> None:
    print(json.dumps({"stage": stage, **rest}, ensure_ascii=False), flush=True)


def cache_dir(path: Path) -> Path:
    """Where a record's parts live.

    Keyed the same way the waveform cache is: path plus size plus mtime. Two
    different records never collide, and the same record re-encoded is a
    different record as far as this is concerned.
    """
    stat = path.stat()
    identity = f"v{CACHE_VERSION}|{path.resolve()}|{stat.st_size}|{stat.st_mtime_ns}"
    digest = hashlib.sha256(identity.encode("utf-8")).hexdigest()[:32]
    return config.CACHE_DIR / "stems" / digest


def existing(folder: Path) -> list[Path] | None:
    """The four parts, if all four are there. Three is not a separation."""
    found = [folder / f"{name}.flac" for name in PARTS]
    return found if all(p.is_file() and p.stat().st_size > 1024 for p in found) else None


def available() -> str | None:
    """Why separation cannot run, or None if it can."""
    try:
        import torch  # noqa: F401
    except ImportError:
        return "PyTorch is not installed"
    try:
        import demucs.separate  # noqa: F401
    except ImportError:
        return "Demucs is not installed"
    return None


def device() -> str:
    try:
        import torch

        if torch.cuda.is_available():
            return "cuda"
    except Exception:
        pass
    return "cpu"


def separate(source: Path | str) -> int:
    source = Path(source).resolve(strict=True)
    folder = cache_dir(source)

    if (found := existing(folder)) is not None:
        emit("done", cached=True, **{name: str(p) for name, p in zip(PARTS, found)})
        return 0

    if (missing := available()) is not None:
        emit("failed", error=missing)
        return 1

    where = device()
    emit("separating", device=where, model=MODEL)

    # Demucs writes into <out>/<model>/<track stem>/. Given a temporary
    # directory we can move the four files where we want them and not care
    # what it called the folder.
    staging = folder.with_name(folder.name + ".partial")
    if staging.exists():
        shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True, exist_ok=True)

    command = [
        sys.executable, "-m", "demucs.separate",
        "-n", MODEL,
        "-d", where,
        "--flac",
        "-o", str(staging),
        str(source),
    ]
    result = subprocess.run(
        command, capture_output=True, text=True, encoding="utf-8", errors="replace",
        env=child_env(),
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    if result.returncode != 0:
        shutil.rmtree(staging, ignore_errors=True)
        tail = (result.stderr or result.stdout or "").strip().splitlines()
        emit("failed", error=tail[-1] if tail else "the separator failed")
        return 1

    produced = {p.stem: p for p in staging.rglob("*.flac")}
    if not all(name in produced for name in PARTS):
        shutil.rmtree(staging, ignore_errors=True)
        emit("failed", error=f"expected {len(PARTS)} parts, got {sorted(produced)}")
        return 1

    # Match the record before anything is published, so a half-resampled
    # separation never becomes the cached answer.
    target = sample_rate_of(source)
    if target:
        try:
            for name in PARTS:
                match_rate(produced[name], target)
        except RuntimeError as error:
            shutil.rmtree(staging, ignore_errors=True)
            emit("failed", error=str(error))
            return 1

    folder.mkdir(parents=True, exist_ok=True)
    for name in PARTS:
        shutil.move(str(produced[name]), str(folder / f"{name}.flac"))
    shutil.rmtree(staging, ignore_errors=True)

    emit("done", cached=False, device=where,
         **{name: str(folder / f"{name}.flac") for name in PARTS})
    return 0


def main(argv: list[str]) -> int:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass

    if not argv:
        emit("failed", error='usage: python -m radio.stems "<audio file>"')
        return 2
    try:
        return separate(" ".join(argv))
    except Exception as error:  # one bad record, not a dead console
        emit("failed", error=str(error))
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
