"""Bounded, cached acoustic sections for cue selection, without a model download.

These are changes in energy and bass, not named verses, choruses or verified
downbeats. Vocal activity is available only from an already separated vocal
stem. Scheduling reads the cache; decoding belongs to library preparation.
"""
from __future__ import annotations

import hashlib
import json
import math
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import numpy as np

from . import config

VERSION = 2
SAMPLE_RATE = 4000
STEP = 0.5
MAX_SECONDS = 1200


def cache_path(path: Path | str) -> Path:
    digest = hashlib.sha256(str(Path(path).resolve()).encode("utf-8")).hexdigest()
    return config.CACHE_DIR / "structure" / f"{digest[:32]}.json"


def _identity(path: Path) -> tuple[list, Path | None]:
    from . import stems
    stat = path.stat()
    parts = stems.existing(stems.cache_dir(path))
    vocal = next((part for part in parts or [] if part.stem == "vocals"), None)
    vocal_stat = vocal.stat() if vocal else None
    return [VERSION, stat.st_size, stat.st_mtime_ns,
            vocal_stat.st_size if vocal_stat else 0,
            vocal_stat.st_mtime_ns if vocal_stat else 0], vocal


def _cached(path: Path, identity: list) -> dict:
    try:
        cache = cache_path(path)
        if cache.stat().st_size > 2_000_000:
            return {}
        saved = json.loads(cache.read_text(encoding="utf-8"))
        if saved.get("fingerprint") != identity:
            return {}
        result = saved["profile"]
        if not _valid(result):
            return {}
        return result
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return {}


def _valid(result: Any) -> bool:
    if not isinstance(result, dict) or result.get("version") != VERSION:
        return False
    duration = result.get("duration", 0)
    if not isinstance(duration, (int, float)) or not 0 < duration <= MAX_SECONDS:
        return False
    bins = result.get("bins")
    if not isinstance(bins, list) or not 0 < len(bins) <= MAX_SECONDS / STEP:
        return False
    previous = -1.0
    for bin_ in bins:
        start, end = bin_["at"], bin_["end"]
        if not previous < start < end <= duration:
            return False
        previous = start
        for key in ("energy", "bass", "vocal"):
            value = bin_[key]
            if value is None and key == "vocal":
                continue
            if not isinstance(value, (int, float)) or not 0 <= value <= 1:
                return False
    for key in ("boundaries", "entries", "exits"):
        points = result.get(key)
        if not isinstance(points, list) or len(points) > len(bins):
            return False
        for point in points:
            if not 0 <= point["at"] <= duration:
                return False
            score = point["confidence" if key == "boundaries" else "score"]
            if not 0 <= score <= 1:
                return False
    return True


def profile_for(track: Any) -> dict:
    """Read-only, no decoder: safe to call while constructing a schedule.

    The optional attached profile is useful for prepared tracks and tests.
    Missing, stale or corrupt caches return {} so existing timing still works.
    """
    try:
        prepared = track.get("structure") if hasattr(track, "get") else None
        if prepared and _valid(prepared):
            return prepared
        value = track["file"]
        if not value:
            return {}
        path = Path(value).resolve(strict=True)
        identity, _ = _identity(path)
        return _cached(path, identity)
    except (OSError, ValueError, TypeError, KeyError, IndexError, AttributeError):
        return {}


def at(profile: dict, source_seconds: float) -> dict:
    """Acoustic bin at source time. Unknown/out-of-range data stays unknown."""
    if not math.isfinite(source_seconds) or source_seconds < 0:
        return {}
    bins = profile.get("bins") or []
    index = int(source_seconds / STEP)
    if index < len(bins) and bins[index]["at"] <= source_seconds < bins[index]["end"]:
        return bins[index]
    return {}


def _decode(path: Path) -> np.ndarray | None:
    try:
        result = subprocess.run(
            [config.FFMPEG, "-hide_banner", "-nostdin", "-v", "error",
             "-i", str(path), "-t", str(MAX_SECONDS + STEP),
             "-vn", "-f", "f32le", "-ac", "1", "-ar", str(SAMPLE_RATE), "-"],
            capture_output=True, timeout=60,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        if result.returncode or len(result.stdout) < 4:
            return None
        samples = np.frombuffer(result.stdout, dtype="<f4")
        return samples if np.isfinite(samples).all() else None
    except (OSError, subprocess.SubprocessError, ValueError):
        return None


def _powers(samples: np.ndarray, sample_rate: int) -> tuple[np.ndarray, np.ndarray]:
    width = max(1, round(sample_rate * STEP))
    energy, bass = [], []
    for start in range(0, len(samples), width):
        block = samples[start:start + width].astype(np.float64)
        energy.append(float(np.mean(block ** 2)))
        power = np.abs(np.fft.rfft(block)) ** 2 / len(block) ** 2
        power[1:-1 if len(block) % 2 == 0 else None] *= 2
        bass.append(float(power[np.fft.rfftfreq(len(block), 1 / sample_rate) < 250].sum()))
    return np.asarray(energy), np.asarray(bass)


def analyse(samples: np.ndarray, sample_rate: int = SAMPLE_RATE,
            vocals: np.ndarray | None = None) -> dict:
    """Pure signal analysis. Arrays must share a sample rate and source origin."""
    samples = np.asarray(samples, dtype=np.float32)
    if sample_rate <= 0 or samples.ndim != 1 or not np.isfinite(samples).all():
        raise ValueError("expected finite mono audio and a positive sample rate")
    if not len(samples):
        return {}
    complete = len(samples) <= MAX_SECONDS * sample_rate
    samples = samples[:MAX_SECONDS * sample_rate]
    energy, bass = _powers(samples, sample_rate)
    duration = len(samples) / sample_rate
    # Separate references retain a meaningful bass envelope for quiet tracks.
    e_ref = max(float(np.percentile(energy, 90)), 1e-8)
    b_ref = max(float(np.percentile(bass, 90)), 1e-8)
    e = np.sqrt(np.clip(energy / e_ref, 0, 1))
    b = np.sqrt(np.clip(bass / b_ref, 0, 1))
    vocal = None
    if vocals is not None:
        vocals = np.asarray(vocals, dtype=np.float32)
        # A partial or misaligned stem cannot establish that the missing part
        # is instrumental. Refuse that evidence entirely.
        if vocals.ndim == 1 and abs(len(vocals) - len(samples)) <= sample_rate * STEP \
                and np.isfinite(vocals).all():
            v_power, _ = _powers(vocals[:len(samples)], sample_rate)
            v_ref = max(float(np.percentile(v_power, 90)), 1e-8)
            vocal = np.sqrt(np.clip(v_power / v_ref, 0, 1))
            # Ignore a separated stem's quiet leakage and digital noise floor.
            vocal[(v_power < v_ref * 0.004) | (v_power < 10 ** (-48 / 10))] = 0
    bins = [{"at": round(i * STEP, 4), "end": round(min((i + 1) * STEP, duration), 4),
             "energy": round(float(e[i]), 4), "bass": round(float(b[i]), 4),
             "vocal": round(float(vocal[i]), 4) if vocal is not None and i < len(vocal) else None}
            for i in range(len(e))]

    # Contrast of adjacent two-second regions identifies practical texture
    # changes. Neither loudness changes nor bass entries prove a musical phrase.
    contrast = np.zeros(len(e))
    for i in range(4, len(e) - 4):
        contrast[i] = min(1.0, abs(float(e[i:i+4].mean() - e[i-4:i].mean())) * 0.7
                          + abs(float(b[i:i+4].mean() - b[i-4:i].mean())) * 0.3)
    chosen: list[int] = []
    for index in np.argsort(-contrast, kind="stable"):
        i = int(index)
        if contrast[i] < 0.18:
            break
        if all(abs(i - old) >= 8 for old in chosen):
            chosen.append(i)
    boundaries = [{"at": round(i * STEP, 4), "confidence": round(float(contrast[i]), 4),
                   "reason": "energy/bass change"} for i in sorted(chosen)]
    # Offer coarse candidates as well as distinct boundaries: a steady drum
    # intro/outro may have no contrast boundary at all.
    indexes = set(chosen) | set(range(0, len(e), 4))
    entries, exits = [], []
    for i in sorted(indexes):
        if e[i] < 0.04:
            continue
        v = float(vocal[i]) if vocal is not None and i < len(vocal) else None
        score = min(1.0, 0.35 + float(contrast[i]) * 0.4 + (0.25 * (1 - v) if v is not None else 0))
        point = {"at": round(i * STEP, 4), "score": round(score, 4),
                 "reason": "low vocal activity in existing stem" if v is not None and v < 0.2
                 else "energy/bass change" if i in chosen else "steady acoustic section"}
        if i * STEP <= duration * 0.49:
            entries.append(point)
        if i * STEP >= duration / 2:
            exits.append(point.copy())
    return {"version": VERSION, "duration": round(duration, 4), "step_sec": STEP,
            "complete": complete, "vocal_source": "existing_stem" if vocal is not None else "unknown",
            "bins": bins, "boundaries": boundaries, "entries": entries, "exits": exits}


def profile(path: Path | str) -> dict:
    """Prepare one track, max 20 minutes decoded at 4 kHz, cached atomically.

    Decoding has a 60-second timeout per source (at most mixture + vocal stem).
    Overlong tracks are marked incomplete; callers should keep normal timing.
    No stem separation is started here. Any failure returns a safe empty profile.
    """
    temporary = None
    try:
        path = Path(path).resolve(strict=True)
        identity, vocal_path = _identity(path)
        cached = _cached(path, identity)
        if cached:
            return cached
        samples = _decode(path)
        if samples is None:
            return {}
        vocals = _decode(vocal_path) if vocal_path else None
        result = analyse(samples, vocals=vocals)
        if not result or _identity(path)[0] != identity:
            return {}
        cache = cache_path(path)
        try:
            cache.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.NamedTemporaryFile(dir=cache.parent, mode="w", encoding="utf-8",
                                             suffix=".json", delete=False) as stream:
                temporary = stream.name
                json.dump({"fingerprint": identity, "profile": result}, stream,
                          allow_nan=False, separators=(",", ":"))
            os.replace(temporary, cache)
        except OSError:
            pass
        return result
    except (OSError, ValueError, TypeError):
        return {}
    finally:
        if temporary:
            try:
                os.unlink(temporary)
            except OSError:
                pass
