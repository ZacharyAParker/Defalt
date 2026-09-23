"""Tempo and key, so the station can choose a transition that makes sense.

Deliberately numpy-only. librosa would do this in three lines but drags in
scipy and numba for a job that is a couple of FFTs, and this runs once per
track at download time where a second either way costs nothing.

  tempo -- spectral flux onset envelope, then autocorrelation over a
           plausible BPM range, with octave correction so 140 does not get
           reported as 70.
  key   -- tuning-corrected, log-compressed chroma from the harmonic part
           of the spectrum, correlated against the Krumhansl-Schmuckler
           major and minor profiles. The runner-up is kept, because a
           relative major/minor pair is the classic confusion.
  bars  -- downbeats from where the kick, the harmony and the accents
           agree, with a confidence.
  feel  -- a perceived-energy score, a danceability hint and a small audio
           embedding for "more like this", from the same decode.

Both return a confidence. Low confidence is normal and useful: a spoken-word
track or a free-tempo ballad genuinely has no BPM, and the transition picker
falls back to a safe crossfade rather than pretending it knows.
"""
from __future__ import annotations

import hashlib
import json
import subprocess
import os
import tempfile
import zipfile
from pathlib import Path
from typing import Any

import numpy as np

from . import config

SAMPLE_RATE = 22050
WINDOW = 2048
HOP = 512

# Krumhansl-Schmuckler key profiles: how strongly each scale degree is used
# in a major and a minor key. Correlating a track's chroma against all twelve
# rotations of each gives the key.
MAJOR_PROFILE = np.array([6.35, 2.23, 3.48, 2.33, 4.38, 4.09,
                          2.52, 5.19, 2.39, 3.66, 2.29, 2.88])
MINOR_PROFILE = np.array([6.33, 2.68, 3.52, 5.38, 2.60, 3.53,
                          2.54, 4.75, 3.98, 2.69, 3.34, 3.17])

NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F",
              "F#", "G", "G#", "A", "A#", "B"]

# Analysis is capped: three minutes is plenty to establish tempo and key, and
# it keeps a ten-minute mix from costing ten times as much.
MAX_SECONDS = 180

# Bump when the stored descriptors change meaning; older rows are refreshed
# lazily when prepared (ensure_features) or in bulk (reanalyse).
FEATURES_VERSION = 1


def peak_levels(samples: np.ndarray, sample_rate: int = SAMPLE_RATE) -> dict:
    """Three resolutions of [min, max, low, mid, high] float32 buckets.

    Energy is mean-square power in <250 Hz, 250–2000 Hz, and >=2000 Hz.
    Positions refer to mono samples at the returned sample_rate, not native
    track samples. The last bucket may be shorter than samples_per_bucket.
    """
    samples = np.asarray(samples, dtype=np.float32)
    if samples.ndim != 1 or sample_rate <= 0 or not np.isfinite(samples).all():
        raise ValueError('expected finite mono samples and a positive sample rate')
    width = 512
    count = (len(samples) + width - 1) // width
    base = np.empty((count, 5), dtype=np.float32)
    weights = np.empty(count, dtype=np.int64)
    for index in range(count):
        block = samples[index * width:(index + 1) * width]
        weights[index] = len(block)
        power = np.abs(np.fft.rfft(block)) ** 2 / len(block) ** 2
        power[1: -1 if len(block) % 2 == 0 else None] *= 2
        frequencies = np.fft.rfftfreq(len(block), 1 / sample_rate)
        base[index] = (block.min(), block.max(),
                       power[frequencies < 250].sum(),
                       power[(frequencies >= 250) & (frequencies < 2000)].sum(),
                       power[frequencies >= 2000].sum())
    levels = {width: base}
    for factor in (8, 64):
        coarse = np.empty(((count + factor - 1) // factor, 5), dtype=np.float32)
        for index, start in enumerate(range(0, count, factor)):
            group = base[start:start + factor]
            coarse[index, :2] = (group[:, 0].min(), group[:, 1].max())
            coarse[index, 2:] = np.average(group[:, 2:], axis=0,
                                           weights=weights[start:start + factor])
        levels[width * factor] = coarse
    return {'sample_rate': sample_rate, 'sample_count': len(samples), 'levels': levels}


def peaks_cache(path: Path | str) -> Path:
    """Where a record's waveform cache lives.

    Under the project's own cache, never beside the audio: this runs over a
    folder the user curates, and a scattering of .npz files through it is
    litter.
    """
    digest = hashlib.sha256(str(Path(path).resolve()).encode('utf-8')).hexdigest()
    return config.CACHE_DIR / 'peaks' / f'{digest[:32]}.npz'


def peaks(path: Path | str) -> dict:
    """Full-track waveform, cached under the project as a compressed NPZ.

    Cache identity is the audio path plus nanosecond mtime and size. NPZ arrays
    are float32 and load without pickle. Atomic replacement tolerates readers
    and concurrent writers; an unwritable cache does not prevent analysis.
    """
    path = Path(path).resolve(strict=True)
    stat = path.stat()
    fingerprint = np.array([1, stat.st_mtime_ns, stat.st_size], dtype=np.int64)
    cache = peaks_cache(path)
    cache_dir = cache.parent
    try:
        with np.load(cache, allow_pickle=False) as saved:
            if np.array_equal(saved['fingerprint'], fingerprint):
                levels = {width: saved[f'level_{width}'] for width in (512, 4096, 32768)}
                count = int(saved['sample_count'])
                if all(a.shape == ((count + w - 1) // w, 5)
                       and np.isfinite(a).all() for w, a in levels.items()):
                    return {'sample_rate': SAMPLE_RATE, 'sample_count': count, 'levels': levels}
    except (OSError, ValueError, KeyError, EOFError, zipfile.BadZipFile):
        pass
    decoded = subprocess.run(
        [config.FFMPEG, '-hide_banner', '-nostdin', '-v', 'error', '-i', str(path),
         '-f', 'f32le', '-ac', '1', '-ar', str(SAMPLE_RATE), '-'],
        capture_output=True, timeout=600,
        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    if decoded.returncode:
        raise ValueError(f'cannot decode audio: {path}')
    result = peak_levels(np.frombuffer(decoded.stdout, dtype='<f4'))
    cache_dir.mkdir(parents=True, exist_ok=True)
    after = path.stat()
    if (after.st_mtime_ns, after.st_size) != (stat.st_mtime_ns, stat.st_size):
        raise OSError('audio changed during waveform analysis')
    temporary = None
    try:
        # Alongside its destination, not alongside the audio: os.replace
        # cannot cross a filesystem, and a music folder on another drive
        # would silently defeat the cache entirely.
        with tempfile.NamedTemporaryFile(dir=cache_dir, suffix='.npz', delete=False) as stream:
            temporary = stream.name
            np.savez_compressed(stream, fingerprint=fingerprint,
                                sample_count=result['sample_count'],
                                **{f'level_{w}': a for w, a in result['levels'].items()})
        os.replace(temporary, cache)
    except OSError:
        pass
    finally:
        if temporary and os.path.exists(temporary):
            os.unlink(temporary)
    return result


def _decode(path: Path) -> np.ndarray | None:
    """Mono float32 at SAMPLE_RATE, straight out of ffmpeg."""
    try:
        result = subprocess.run(
            [config.FFMPEG, "-hide_banner", "-nostdin", "-v", "error",
             "-t", str(MAX_SECONDS), "-i", str(path),
             "-f", "f32le", "-ac", "1", "-ar", str(SAMPLE_RATE), "-"],
            capture_output=True, timeout=180,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
    except (subprocess.SubprocessError, OSError):
        return None
    if result.returncode != 0 or len(result.stdout) < WINDOW * 4:
        return None
    return np.frombuffer(result.stdout, dtype=np.float32)


def _spectrogram(samples: np.ndarray) -> np.ndarray:
    """Magnitude STFT. Frames are columns."""
    frames = 1 + (len(samples) - WINDOW) // HOP
    if frames < 4:
        return np.zeros((WINDOW // 2 + 1, 0))
    window = np.hanning(WINDOW).astype(np.float32)
    # Strided view avoids copying the signal once per frame.
    shape = (frames, WINDOW)
    strides = (samples.strides[0] * HOP, samples.strides[0])
    blocks = np.lib.stride_tricks.as_strided(samples, shape, strides)
    return np.abs(np.fft.rfft(blocks * window, axis=1)).T


# --------------------------------------------------------------------------
# Tempo
# --------------------------------------------------------------------------
def _onset_envelope(spectrum: np.ndarray) -> np.ndarray:
    """Spectral flux: how much energy appeared since the last frame."""
    log_spec = np.log1p(spectrum)
    flux = np.diff(log_spec, axis=1)
    # Only increases matter. Energy dying away is not an onset.
    return np.maximum(flux, 0).sum(axis=0)


_MEL_BANDS = 40


def _band_edges(bins: int) -> np.ndarray:
    """Log-spaced band edges across the usable spectrum."""
    freqs = np.fft.rfftfreq(WINDOW, 1.0 / SAMPLE_RATE)[:bins]
    low, high = 40.0, min(SAMPLE_RATE / 2, 8000.0)
    edges = np.geomspace(low, high, _MEL_BANDS + 1)
    return np.searchsorted(freqs, edges).clip(0, bins - 1)


def _beat_envelope(spectrum: np.ndarray) -> np.ndarray:
    """An onset function actually good enough to find beats with.

    Summing raw flux across every FFT bin is dominated by whatever is loudest
    and broadband, which buries the transients. Two changes fix it:

    banding      energy is collapsed into log-spaced bands first, so a snare
                 in the mids counts as much as a wall of guitar,
    whitening    the result is measured against its own local average, so a
                 quiet intro and a loud chorus contribute equally.

    Without both, the grid confidence sits around 1.0 -- indistinguishable
    from no grid at all.
    """
    if spectrum.shape[1] < 4:
        return np.zeros(0)

    edges = _band_edges(spectrum.shape[0])
    banded = np.empty((_MEL_BANDS, spectrum.shape[1]), dtype=np.float32)
    for index in range(_MEL_BANDS):
        start, stop = edges[index], max(edges[index + 1], edges[index] + 1)
        banded[index] = spectrum[start:stop].mean(axis=0)

    flux = np.maximum(np.diff(np.log1p(banded), axis=1), 0).sum(axis=0)
    if len(flux) < 8:
        return flux

    # Adaptive whitening against a local mean, roughly half a second wide.
    span = max(3, int(SAMPLE_RATE / HOP * 0.5) | 1)
    kernel = np.ones(span) / span
    local = np.convolve(flux, kernel, mode="same")
    return np.maximum(flux - local, 0)


def detect_tempo(spectrum: np.ndarray) -> tuple[float, float]:
    """Return (bpm, confidence 0..1). bpm is 0 when there is no clear pulse."""
    onset = _onset_envelope(spectrum)
    if len(onset) < 32:
        return (0.0, 0.0)

    onset = onset - onset.mean()
    if not np.any(onset):
        return (0.0, 0.0)

    correlation = np.correlate(onset, onset, mode="full")[len(onset) - 1:]
    if correlation[0] <= 0:
        return (0.0, 0.0)
    correlation = correlation / correlation[0]

    frames_per_second = SAMPLE_RATE / HOP
    # 60-200 BPM covers everything this station plays.
    low_lag = max(1, int(frames_per_second * 60 / 200))
    high_lag = min(len(correlation) - 1, int(frames_per_second * 60 / 60))
    if high_lag <= low_lag:
        return (0.0, 0.0)

    window = correlation[low_lag:high_lag]
    best = int(np.argmax(window)) + low_lag
    strength = float(window.max())
    bpm = 60.0 * frames_per_second / best

    # Octave correction. Autocorrelation is just as happy to lock onto half
    # or double the real tempo, so pull the answer into the range where most
    # music actually sits.
    while bpm < 70:
        bpm *= 2
    while bpm > 180:
        bpm /= 2

    return (round(bpm, 1), max(0.0, min(1.0, strength * 2.5)))


# --------------------------------------------------------------------------
# Beat grid
# --------------------------------------------------------------------------
def detect_beats(spectrum: np.ndarray, bpm: float) -> dict[str, float]:
    """Where the beats actually fall, not just how fast they come.

    Tempo alone is not enough to mix on. Two records at the same BPM whose
    beats are a tenth of a second apart produce a flam through the whole
    overlap -- audibly two records rather than one. This finds the phase of
    the grid so the incoming record can be nudged onto it.

    A comb filter over the onset envelope: for every candidate offset within
    one beat, sum the onset strength at every beat that offset implies. The
    offset that collects the most energy is where the beats are.
    """
    blank = {"beat_offset": 0.0, "beat_period": 0.0, "beat_residual_ms": 999.0,
             "downbeat_offset": 0.0}
    if bpm <= 0:
        return blank

    onset = _beat_envelope(spectrum)
    if len(onset) < 32:
        return blank

    fps = SAMPLE_RATE / HOP
    period_frames = 60.0 / bpm * fps
    if period_frames < 2 or period_frames >= len(onset):
        return blank

    beats = track_beats(onset, period_frames)
    grid = fit_grid(beats, fps)
    if grid["beat_period"] <= 0:
        return blank

    phase, confidence = detect_downbeat(spectrum, beats, onset)
    # The grid offset is the first grid beat; the tracked beat list may
    # start later, so express the phase relative to the grid itself.
    first = int(round((beats[0] / fps - grid["beat_offset"]) / grid["beat_period"])) if len(beats) else 0
    downbeat = (phase + first) % 4

    return {
        "beat_offset": grid["beat_offset"],
        "beat_period": grid["beat_period"],
        "beat_residual_ms": grid["beat_residual_ms"],
        "downbeat_offset": round(
            grid["beat_offset"] + downbeat * grid["beat_period"], 4),
        "downbeat_confidence": confidence,
    }


def _zscore(values: np.ndarray) -> np.ndarray:
    spread = values.std()
    return (values - values.mean()) / spread if spread > 1e-9 else np.zeros_like(values)


def detect_downbeat(spectrum: np.ndarray, beats: np.ndarray,
                    onset: np.ndarray | None = None) -> tuple[int, float]:
    """Which beat of four starts the bar, and how sure that is (0..1).

    The loudest beat is often the snare on two and four. Bars are marked
    by the kick and by harmony: chords tend to change on the one. So each
    beat is scored on low-frequency onset (the kick), chroma change across
    it (the harmony) and overall accent, and the four bar positions compete.
    Confidence is how clearly the winner beats the runner-up.
    """
    if len(beats) < 8 or spectrum.shape[1] < 8:
        return 0, 0.0
    onset = _beat_envelope(spectrum) if onset is None else onset
    freqs = np.fft.rfftfreq(WINDOW, 1.0 / SAMPLE_RATE)[:spectrum.shape[0]]
    low = np.log1p(spectrum[(freqs >= 30) & (freqs < 150)].sum(axis=0))
    kick = np.maximum(np.diff(low, prepend=low[:1]), 0)
    chroma = _chroma_frames(spectrum, step=1, harmonic=False)
    frames = np.clip(np.rint(beats).astype(int), 0, spectrum.shape[1] - 1)

    def near(signal, frame):
        return float(signal[max(0, frame - 1):frame + 2].max()) if len(signal) else 0.0

    change = np.zeros(len(frames))
    for index in range(1, len(frames) - 1):
        before = chroma[:, frames[index - 1]:frames[index]].mean(axis=1)
        after = chroma[:, frames[index]:frames[index + 1]].mean(axis=1)
        norm = np.linalg.norm(before) * np.linalg.norm(after)
        change[index] = 1 - float(before @ after / norm) if norm > 1e-12 else 0.0
    kicks = np.array([near(kick, f) for f in frames])
    accents = np.array([near(onset, min(f, len(onset) - 1)) for f in frames]) if len(onset) else np.zeros(len(frames))
    score = _zscore(kicks) + _zscore(change) + 0.5 * _zscore(accents)
    means = np.array([score[p::4].mean() if len(score[p::4]) else -np.inf for p in range(4)])
    order = np.argsort(means)[::-1]
    best, second = means[order[0]], means[order[1]]
    spread = float(means.max() - means.min())
    confidence = float(np.clip((best - second) / (spread + 0.25), 0.0, 1.0)) if np.isfinite(second) else 0.0
    return int(order[0]), round(confidence, 3)


def track_beats(onset: np.ndarray, period_frames: float,
                tightness: float = 100.0) -> np.ndarray:
    """Dynamic-programming beat tracking, after Ellis (2007).

    A comb filter asks "which single phase collects the most onsets", which
    fails on anything with syncopation or a bar or two of rubato. This instead
    finds the best *sequence* of beats: every frame scores its own onset
    strength plus the best score reachable from a plausible previous beat,
    penalised for landing at the wrong distance. Backtracking the winner gives
    a beat sequence that follows the music instead of averaging over it.
    """
    if len(onset) < 8 or period_frames < 2:
        return np.zeros(0)

    peak = onset.max()
    if peak <= 0:
        return np.zeros(0)
    strength = onset / peak

    # Candidate gaps between consecutive beats, half to double the period.
    low = max(1, int(round(period_frames * 0.5)))
    high = max(low + 1, int(round(period_frames * 2.0)))
    gaps = np.arange(low, high + 1)
    # Cost of an unlikely gap: log-squared, so double or half time is heavily
    # discouraged but a little drift is nearly free.
    penalty = -tightness * (np.log(gaps / period_frames) ** 2)

    score = np.full(len(strength), -np.inf)
    back = np.zeros(len(strength), dtype=int)
    score[0] = strength[0]

    for frame in range(1, len(strength)):
        previous = frame - gaps
        valid = previous >= 0
        if not np.any(valid):
            score[frame] = strength[frame]
            back[frame] = -1
            continue
        candidates = score[previous[valid]] + penalty[valid]
        best = int(np.argmax(candidates))
        score[frame] = strength[frame] + candidates[best]
        back[frame] = previous[valid][best]

    # Start from the best score in the final stretch, then walk back.
    tail = max(0, len(score) - int(period_frames * 2))
    position = int(np.argmax(score[tail:])) + tail
    beats = []
    while position >= 0:
        beats.append(position)
        nxt = back[position]
        if nxt >= position:
            break
        position = nxt
    return np.array(sorted(beats), dtype=float)


def fit_grid(beats: np.ndarray, fps: float) -> dict[str, float]:
    """Fit a steady grid to a beat sequence, and say how well it fits.

    Mixing needs a *grid*, not a beat list: a constant offset and period the
    other record can be nudged onto. The residual of that fit, in
    milliseconds, is the honest confidence measure -- it is literally how far
    the beats wander from a steady pulse, and therefore how far off an
    alignment built on it would be.
    """
    blank = {"beat_offset": 0.0, "beat_period": 0.0,
             "beat_residual_ms": 999.0, "beat_count": 0}
    if len(beats) < 8:
        return blank

    index = np.arange(len(beats))
    period, offset = np.polyfit(index, beats / fps, 1)
    if period <= 0:
        return blank

    predicted = offset + period * index
    residual = float(np.sqrt(np.mean((beats / fps - predicted) ** 2)) * 1000)

    return {
        "beat_offset": round(float(offset % period), 4),
        "beat_period": round(float(period), 5),
        "beat_residual_ms": round(residual, 1),
        "beat_count": len(beats),
    }


def grid_score(spectrum: np.ndarray, offset: float, period: float) -> float:
    """Onset energy on the grid, relative to everywhere else.

    Used to check a detected grid is real rather than to build one. Above
    about 1.3 the beats line up with actual transients.
    """
    onset = _beat_envelope(spectrum)
    if len(onset) < 16 or period <= 0:
        return 0.0
    fps = SAMPLE_RATE / HOP
    picks = np.arange(offset * fps, len(onset), period * fps).astype(int)
    picks = picks[picks < len(onset)]
    if not len(picks) or onset.mean() <= 0:
        return 0.0
    return float(onset[picks].mean() / onset.mean())


# --------------------------------------------------------------------------
# Key
# --------------------------------------------------------------------------
# Harmony lives between roughly G2 and D#8: below it the kick and the bass
# smear across semitones at this resolution, above it cymbals and air.
CHROMA_LOW, CHROMA_HIGH = 100.0, 5000.0


def _median_filter(values: np.ndarray, width: int, axis: int) -> np.ndarray:
    """A running median along one axis, in bounded chunks of memory."""
    pad = width // 2
    padded = np.pad(values, [(pad, pad) if a == axis else (0, 0) for a in range(values.ndim)], mode="edge")
    out = np.empty_like(values)
    other = 1 - axis
    step = max(1, 2_000_000 // max(1, values.shape[axis] * width))
    for start in range(0, values.shape[other], step):
        block = padded[start:start + step] if other == 0 else padded[:, start:start + step]
        windows = np.lib.stride_tricks.sliding_window_view(block, width, axis=axis)
        filtered = np.median(windows, axis=-1)
        if other == 0:
            out[start:start + step] = filtered
        else:
            out[:, start:start + step] = filtered
    return out


def _chroma_frames(spectrum: np.ndarray, step: int = 2, harmonic: bool = True) -> np.ndarray:
    """Per-frame twelve-class chroma, shape (12, frames/step).

    Magnitudes are log-compressed so one loud bass note cannot outvote a
    chord. Each bin shares its energy between neighbouring pitch classes by
    its distance in semitones (after estimating the recording's tuning), and
    bins too coarse to resolve a semitone count for less. With `harmonic`,
    a median filter along time and frequency keeps sustained partials and
    drops drum hits, a cheap harmonic/percussive separation.
    """
    bins, frames = spectrum.shape
    freqs = np.fft.rfftfreq(WINDOW, 1.0 / SAMPLE_RATE)[:bins]
    usable = (freqs >= CHROMA_LOW) & (freqs <= CHROMA_HIGH)
    if not np.any(usable) or frames == 0:
        return np.zeros((12, 0))
    magnitude = spectrum[usable][:, ::max(1, step)].astype(np.float32)
    peak = float(magnitude.max())
    if peak <= 0:
        return np.zeros((12, magnitude.shape[1]))
    # Gentle compression: stronger would lift overtones (a fifth above every
    # note) level with the notes themselves and pull the key to the dominant.
    compressed = np.log1p(5.0 * magnitude / peak)
    if harmonic and compressed.shape[1] >= 9:
        sustained = _median_filter(compressed, 9, axis=1)
        percussive = _median_filter(compressed, 9, axis=0)
        mask = sustained ** 2 / (sustained ** 2 + percussive ** 2 + 1e-9)
        compressed = compressed * mask
    midi = 69 + 12 * np.log2(freqs[usable] / 440.0)
    # Tuning: the energy-weighted circular mean of each bin's offset from
    # the nearest equal-tempered semitone, so a record mastered a little
    # sharp does not straddle two pitch classes.
    weights = compressed.sum(axis=1)
    angle = 2 * np.pi * (midi - np.rint(midi))
    tuning = float(np.angle(np.sum(weights * np.exp(1j * angle))) / (2 * np.pi)) if weights.sum() > 0 else 0.0
    position = (midi - tuning) % 12
    distance = np.abs(position[None, :] - np.arange(12)[:, None])
    distance = np.minimum(distance, 12 - distance)
    share = np.exp(-0.5 * (distance / 0.35) ** 2)
    share *= np.minimum(1.0, (freqs[usable] * (2 ** (1 / 12) - 1)) / (SAMPLE_RATE / WINDOW))[None, :]
    return share @ compressed


def _chroma(spectrum: np.ndarray) -> np.ndarray:
    """Fold the spectrum into twelve pitch classes, summed over the track."""
    frames = _chroma_frames(spectrum)
    if not frames.size:
        return np.zeros(12)
    chroma = frames.sum(axis=1)
    total = chroma.sum()
    return chroma / total if total > 0 else chroma


def _key_scores(chroma: np.ndarray) -> list[tuple[float, int, str]]:
    scores: list[tuple[float, int, str]] = []
    for tonic in range(12):
        rotated = np.roll(chroma, -tonic)
        for profile, mode in ((MAJOR_PROFILE, "major"), (MINOR_PROFILE, "minor")):
            correlation = np.corrcoef(rotated, profile)[0, 1]
            if np.isfinite(correlation):
                scores.append((float(correlation), tonic, mode))
    scores.sort(reverse=True)
    return scores


def detect_key(spectrum: np.ndarray) -> tuple[int, str, float]:
    """Return (pitch class 0-11, 'major'|'minor', confidence 0..1)."""
    tonic, mode, confidence, _ = detect_key_detail(spectrum)
    return (tonic, mode, confidence)


def detect_key_detail(spectrum: np.ndarray) -> tuple[int, str, float, str]:
    """(tonic, mode, confidence, runner-up Camelot code).

    Confidence is how far clear of the runner-up the winner is, except that
    a relative major/minor runner-up costs less: both share every note, and
    either is a safe mixing neighbour on the Camelot wheel.
    """
    chroma = _chroma(spectrum)
    if not np.any(chroma):
        return (-1, "", 0.0, "")
    scores = _key_scores(chroma)
    if not scores:
        return (-1, "", 0.0, "")
    best, tonic, mode = scores[0]
    runner_up, alt_tonic, alt_mode = scores[1] if len(scores) > 1 else (0.0, -1, "")
    relative = (alt_mode != mode and camelot(alt_tonic, alt_mode)[:-1] == camelot(tonic, mode)[:-1])
    # A track that fits two unrelated keys equally well has not really told
    # us its key. A relative pair is ambiguity about the mode, not the notes.
    margin = max(0.0, best - runner_up)
    if relative and len(scores) > 2:
        margin = max(margin, 0.5 * max(0.0, best - scores[2][0]))
    confidence = max(0.0, min(1.0, best * 0.6 + margin * 2.0))
    return (tonic, mode, round(confidence, 3), camelot(alt_tonic, alt_mode))


# --------------------------------------------------------------------------
# Camelot
# --------------------------------------------------------------------------
# The Camelot wheel: keys a step apart on it mix without clashing. Number is
# position round the wheel, letter is A for minor and B for major.
_CAMELOT_MAJOR = {0: 8, 1: 3, 2: 10, 3: 5, 4: 12, 5: 7,
                  6: 2, 7: 9, 8: 4, 9: 11, 10: 6, 11: 1}
_CAMELOT_MINOR = {0: 5, 1: 12, 2: 7, 3: 2, 4: 9, 5: 4,
                  6: 11, 7: 6, 8: 1, 9: 8, 10: 3, 11: 10}


def camelot(tonic: int, mode: str) -> str:
    """'8B' for C major, '8A' for A minor. Empty when the key is unknown."""
    if tonic is None or tonic < 0 or mode not in ("major", "minor"):
        return ""
    if mode == "major":
        return f"{_CAMELOT_MAJOR[tonic % 12]}B"
    return f"{_CAMELOT_MINOR[tonic % 12]}A"


def keys_compatible(first: str, second: str) -> bool:
    """Do these two Camelot codes mix cleanly?

    Same key, one step around the wheel, or the relative major/minor swap.
    That is the standard DJ rule and it holds up well in practice.
    """
    if not first or not second:
        return False
    try:
        first_number, first_letter = int(first[:-1]), first[-1]
        second_number, second_letter = int(second[:-1]), second[-1]
    except (ValueError, IndexError):
        return False

    if first == second:
        return True
    if first_number == second_number:          # relative major / minor
        return True
    if first_letter == second_letter:
        step = abs(first_number - second_number)
        return step == 1 or step == 11         # the wheel wraps 12 -> 1
    return False


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------
# --------------------------------------------------------------------------
# Feel: energy, danceability, and an embedding for similarity
# --------------------------------------------------------------------------
EMBEDDING_SIZE = 32


def _mfcc(spectrum: np.ndarray, count: int = 13) -> np.ndarray:
    """Cepstral coefficients over log-spaced bands; shape (count, frames)."""
    power = spectrum.astype(np.float64) ** 2
    edges = _band_edges(spectrum.shape[0])
    bands = np.empty((_MEL_BANDS, spectrum.shape[1]))
    for index in range(_MEL_BANDS):
        start, stop = edges[index], max(edges[index + 1], edges[index] + 1)
        bands[index] = power[start:stop].mean(axis=0)
    logged = np.log(bands + 1e-10)
    n = np.arange(_MEL_BANDS)
    basis = np.cos(np.pi / _MEL_BANDS * (n + 0.5)[None, :] * np.arange(count)[:, None])
    return basis @ logged / _MEL_BANDS


def features(samples: np.ndarray, spectrum: np.ndarray, *, bpm: float = 0.0,
             bpm_confidence: float = 0.0, residual_ms: float = 999.0,
             mode: str = "") -> dict[str, Any]:
    """Perceived energy, a danceability hint, and a small audio embedding.

    Energy here is what a listener means by it: how sustained and dense the
    sound is (loudness relative to the record's own peaks), how much low end
    drives it, how busy the onsets are, how bright it is, and how fast. It
    ignores absolute level on purpose -- a quiet master of a banger is still
    a banger, and stored LUFS mix source and normalised measurements.
    Danceability is a steady, confident pulse in a dance tempo with weight in
    the low end. Both are bounded 0..1 hints, not genre or mood labels.
    """
    blank = {"energy": None, "danceability": None, "onset_rate": None, "embedding": None}
    if spectrum.shape[1] < 16 or len(samples) < SAMPLE_RATE:
        return blank
    duration = len(samples) / SAMPLE_RATE
    # Sustain: half-second RMS against the record's own loud passages, the
    # same measure the cue planner's structure bins use.
    width = SAMPLE_RATE // 2
    blocks = len(samples) // width
    rms = np.sqrt(np.mean(samples[:blocks * width].reshape(blocks, width).astype(np.float64) ** 2, axis=1))
    reference = max(float(np.percentile(rms, 90)), 1e-9)
    sustain = float(np.mean(np.clip(rms / reference, 0, 1)))
    loud = rms[rms > 1e-5]
    dynamics = float(np.percentile(20 * np.log10(loud), 95) - np.percentile(20 * np.log10(loud), 10)) if len(loud) > 4 else 0.0

    power = spectrum.astype(np.float64) ** 2
    freqs = np.fft.rfftfreq(WINDOW, 1.0 / SAMPLE_RATE)[:spectrum.shape[0]]
    total = power.sum(axis=0) + 1e-12
    bass = float(np.mean(power[freqs < 150].sum(axis=0) / total))
    centroid = float(np.mean((freqs[:, None] * power).sum(axis=0) / total))
    cumulative = np.cumsum(power, axis=0) / total
    rolloff = float(np.mean(freqs[np.argmax(cumulative >= 0.85, axis=0)]))
    audible = power[(freqs > 60) & (freqs < 8000)] + 1e-12
    flatness = float(np.mean(np.exp(np.mean(np.log(audible), axis=0)) / np.mean(audible, axis=0)))

    onset = _beat_envelope(spectrum)
    peaks = 0
    if len(onset) > 2:
        threshold = onset.mean() + onset.std()
        peaks = int(np.sum((onset[1:-1] > threshold) & (onset[1:-1] >= onset[:-2]) & (onset[1:-1] > onset[2:])))
    onset_rate = peaks / duration

    pace = min(1.0, max(0.0, (bpm - 60) / 120)) if bpm > 0 else 0.4
    energy = (0.35 * sustain + 0.2 * min(1.0, onset_rate / 6.0) + 0.15 * pace
              + 0.15 * min(1.0, bass / 0.45) + 0.15 * min(1.0, centroid / 3000.0))
    steadiness = max(0.0, 1.0 - residual_ms / 40.0) if bpm > 0 else 0.0
    in_pocket = max(0.0, 1.0 - abs(bpm - 118) / 45.0) if bpm > 0 else 0.0
    danceability = (min(1.0, bpm_confidence * 1.5) * (0.4 + 0.6 * steadiness)
                    * (0.35 + 0.65 * in_pocket) * (0.6 + 0.4 * min(1.0, bass / 0.35)))

    mfcc = _mfcc(spectrum)
    chroma = _chroma(spectrum)
    entropy = float(-np.sum(chroma * np.log(chroma + 1e-12)) / np.log(12)) if chroma.any() else 1.0
    vector = [*mfcc[1:13].mean(axis=1), *mfcc[1:9].std(axis=1),
              centroid / 1000.0, rolloff / 2000.0, flatness * 10.0, bass * 4.0,
              onset_rate / 2.0, float(np.log2(bpm / 120.0)) * 2.0 if bpm > 0 else 0.0,
              bpm_confidence * 2.0, dynamics / 10.0, energy * 4.0, danceability * 4.0,
              {"major": 1.0, "minor": -1.0}.get(mode, 0.0), entropy * 4.0]
    return {"energy": round(float(np.clip(energy, 0, 1)), 4),
            "danceability": round(float(np.clip(danceability, 0, 1)), 4),
            "onset_rate": round(onset_rate, 3),
            "embedding": json.dumps([round(float(v), 4) for v in vector], separators=(",", ":"))}


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------
_MEMO: dict[tuple, dict[str, Any]] = {}


def resample(samples: np.ndarray, rate: int, target: int) -> np.ndarray:
    """Mono float32 at `target` Hz by linear interpolation.

    Adequate for envelopes, onsets and chroma below a few kHz; not a
    listening-quality resampler.
    """
    samples = np.asarray(samples, dtype=np.float32)
    if rate == target or not len(samples):
        return samples
    if rate <= 0:
        raise ValueError("sample rate must be positive")
    count = int(len(samples) * target / rate)
    positions = np.arange(count, dtype=np.float64) * (rate / target)
    return np.interp(positions, np.arange(len(samples)), samples).astype(np.float32)


def profile(path: Path, samples: np.ndarray | None = None,
            sample_rate: int = SAMPLE_RATE) -> dict[str, Any]:
    """Everything the transition picker needs to know about a recording.

    `samples`, when given, is the already decoded audio: mono float32 at
    `sample_rate` Hz (default SAMPLE_RATE, 22050), from the start of the
    file. Only the first MAX_SECONDS are used; other rates are resampled.
    This lets one decode serve several analyses.

    The most recent results are remembered by path, size and mtime: the
    feature backfill right after a download reuses them instead of decoding
    the same file twice.
    """
    blank = {"bpm": 0.0, "bpm_confidence": 0.0, "key_tonic": -1,
             "key_mode": "", "key_confidence": 0.0, "camelot": "", "key_alt": "",
             "beat_offset": 0.0, "beat_period": 0.0, "beat_residual_ms": 999.0,
             "downbeat_offset": 0.0, "downbeat_confidence": 0.0,
             "energy": None, "danceability": None, "onset_rate": None, "embedding": None,
             "features_version": FEATURES_VERSION}
    try:
        stat = Path(path).stat()
        memo_key = (str(Path(path).resolve()), stat.st_size, stat.st_mtime_ns)
    except OSError:
        memo_key = None
    if memo_key and memo_key in _MEMO:
        return dict(_MEMO[memo_key])

    if samples is not None:
        samples = resample(np.asarray(samples, dtype=np.float32)[:int(MAX_SECONDS * sample_rate)],
                          sample_rate, SAMPLE_RATE)
        if not np.isfinite(samples).all():
            return blank
    else:
        samples = _decode(path)
    if samples is None or len(samples) < SAMPLE_RATE:
        return blank

    spectrum = _spectrogram(samples)
    if spectrum.shape[1] < 8:
        return blank

    bpm, bpm_confidence = detect_tempo(spectrum)
    tonic, mode, key_confidence, alternative = detect_key_detail(spectrum)
    beats = detect_beats(spectrum, bpm)

    result = {
        "bpm": bpm,
        "bpm_confidence": round(bpm_confidence, 3),
        "key_tonic": tonic,
        "key_mode": mode,
        "key_confidence": key_confidence,
        "camelot": camelot(tonic, mode),
        "key_alt": alternative,
        "downbeat_confidence": 0.0,
        **beats,
        **features(samples, spectrum, bpm=bpm, bpm_confidence=bpm_confidence,
                   residual_ms=beats["beat_residual_ms"], mode=mode),
        "features_version": FEATURES_VERSION,
    }
    if memo_key:
        if len(_MEMO) >= 8:
            _MEMO.pop(next(iter(_MEMO)))
        _MEMO[memo_key] = dict(result)
    return result


# Columns refreshed when a stored row predates FEATURES_VERSION. Tempo and the
# beat grid are unchanged, so they are left alone; key, downbeat and feel are
# what improved.
_REFRESHED = ("key_tonic", "key_mode", "key_confidence", "camelot", "key_alt",
              "downbeat_offset", "downbeat_confidence", "energy", "danceability",
              "onset_rate", "embedding", "features_version")


def needs_features(track: Any) -> bool:
    from . import db
    version = db.field(track, "features_version")
    return not version or int(version) < FEATURES_VERSION


def ensure_features(track: dict[str, Any]) -> dict[str, Any]:
    """Bring one prepared track's descriptors up to date; returns the row.

    Blocking (one decode), so call it from the feeder or a CLI, never while
    holding the schedule lock. A missing file or failed decode leaves the
    row as it was.
    """
    from . import db
    if not needs_features(track) or not track.get("file") or not Path(track["file"]).is_file():
        return track
    measured = profile(Path(track["file"]))
    if measured.get("features_version") != FEATURES_VERSION or measured.get("energy") is None:
        return track
    values = {name: measured.get(name) for name in _REFRESHED}
    if not values.get("camelot"):
        # A failed key read must not erase a usable earlier one.
        for name in ("key_tonic", "key_mode", "key_confidence", "camelot", "key_alt"):
            values.pop(name)
    db.write(f"UPDATE tracks SET {', '.join(f'{name}=?' for name in values)} WHERE key=?",
             (*values.values(), track["key"]))
    return {**track, **values}


def reanalyse(limit: int | None = None, progress=None) -> dict[str, int]:
    """Refresh descriptors for every cached/local track that predates them.

    Hook for `python -m radio.cli reanalyse`. Returns counts.
    """
    from . import db
    rows = [dict(row) for row in db.query(
        "SELECT * FROM tracks WHERE file IS NOT NULL AND blocked=0 "
        "AND (features_version IS NULL OR features_version < ?) ORDER BY last_played DESC",
        (FEATURES_VERSION,))]
    done = failed = 0
    for row in rows[:limit] if limit else rows:
        updated = ensure_features(row)
        if needs_features(updated):
            failed += 1
        else:
            done += 1
        if progress:
            progress(row, not needs_features(updated))
    return {"updated": done, "failed": failed, "pending": max(0, len(rows) - done - failed)}


def describe(track: Any) -> str:
    """'128 BPM · A minor (8A)', for logs and the UI."""
    from . import db
    bpm = db.field(track, "bpm") or 0
    tonic = db.field(track, "key_tonic")
    mode = db.field(track, "key_mode") or ""
    parts = []
    if bpm:
        parts.append(f"{bpm:.0f} BPM")
    if tonic is not None and tonic >= 0 and mode:
        code = camelot(int(tonic), mode)
        parts.append(f"{NOTE_NAMES[int(tonic)]} {mode} ({code})")
    return " · ".join(parts) or "unanalysed"
