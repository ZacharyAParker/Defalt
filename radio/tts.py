"""Voices -- turns written lines into host audio.

Two engines. Edge TTS is the default: free, fast, 300+ voices. OpenRouter's
/audio/speech endpoint is the alternative, for hosted models like Fish Audio.
The engine is settable per persona, because hosted speech models often expose
only one voice, and two hosts sharing a voice is not a two-host show -- so you
may well want one host on each.

Each line is rendered separately rather than one blob per break. That costs a
few extra requests but buys precise control: the director can overlap Rue on
top of Mav to make an interruption land, and it can back-time an individual
line so the last word hits the post. say_many() renders a break's lines in
parallel and keeps each host on one voice for the whole break.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import math
import re
import subprocess
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

import edge_tts
import httpx

from . import config, showclock

VOICE_DIR = config.CACHE_DIR / "voice"

# If a persona names a voice Edge does not have, fall back to these rather
# than dropping the line entirely.
FALLBACK_VOICES = ["en-US-GuyNeural", "en-US-JennyNeural", "en-US-AriaNeural"]

# Short clips are padded to this length before loudness is measured, so a
# two-second "okay" lands at the same loudness as a fifteen-second read.
MIN_MEASURE_SECONDS = 3.5

_COOLDOWN: dict[str, float] = {}
_COOLDOWN_LOCK = threading.Lock()


def backend() -> str:
    return str(config.station.get("tts.backend", "edge") or "edge").lower()


def _ffprobe() -> str:
    return getattr(config, "FFPROBE", "ffprobe")


def _timeout() -> float:
    """Per-request ceiling. A slow line should fall back, not stall a break."""
    try:
        value = float(config.station.get("tts.timeout_seconds", 25) or 25)
    except (TypeError, ValueError):
        value = 25.0
    return max(5.0, min(60.0, value)) if math.isfinite(value) else 25.0


# --------------------------------------------------------------------------
# Circuit breaker: a failing model is skipped for a while, like llm._benched.
# --------------------------------------------------------------------------
def _bench_key(voice: dict[str, Any]) -> str:
    if voice.get("engine") == "openrouter":
        return "openrouter:" + str(voice.get("model") or "")
    return "edge"


def _available(key: str) -> bool:
    with _COOLDOWN_LOCK:
        return _COOLDOWN.get(key, 0) < time.monotonic()


def _bench(key: str, seconds: float) -> None:
    with _COOLDOWN_LOCK:
        _COOLDOWN[key] = time.monotonic() + seconds
    if config.DEBUG:
        print(f"[tts] {key} benched for {seconds:.0f}s", flush=True)


def benched() -> dict[str, float]:
    now = time.monotonic()
    with _COOLDOWN_LOCK:
        return {key: round(until - now, 1) for key, until in _COOLDOWN.items() if until > now}


# --------------------------------------------------------------------------
# Text and voice preparation
# --------------------------------------------------------------------------
# Real acronyms stay spelled out. Everything else shouted in capitals is
# emphasis ("MAV. MAV."), which speech engines otherwise read letter by letter.
_ACRONYMS = {"AI", "DJ", "TV", "PC", "UK", "US", "USA", "FM", "AM", "PM", "OK",
             "NASA", "RPG", "FPS", "MMO", "GPU", "CPU", "EP", "LP", "BPM", "RSS",
             "NPC", "DLC", "PS", "EU", "NBA", "NFL", "MLB", "BBC", "CNN", "HBO",
             "GTA", "RGB", "SSD", "RAM", "VR", "AR", "UI", "API", "FAQ", "CEO", "MVP"}
_EDGE_PUNCT = re.compile(r"^[^\w']+|[^\w']+$")


def speakable(text: str) -> str:
    """TTS input only: shouted words in normal case, acronyms untouched.

    A capitalised word of three or more letters is emphasis; a two-letter one
    only when it sits next to such a word ("IT'S SO LOUD"). The transcript
    keeps the capitals -- this only changes what the speech engine reads.
    """
    tokens = re.split(r"(\s+)", text)
    words = [i for i, token in enumerate(tokens) if token.strip()]

    def core(token: str) -> str:
        return _EDGE_PUNCT.sub("", token)

    def capitals(token: str) -> bool:
        word = core(token)
        letters = word.replace("'", "")
        return (len(letters) >= 2 and letters.isalpha() and word.upper() == word
                and word not in _ACRONYMS)

    shouted = {i for i in words if capitals(tokens[i])}
    strong = {i for i in shouted if len(core(tokens[i]).replace("'", "")) >= 3}
    for position, index in enumerate(words):
        if index not in shouted:
            continue
        neighbours = words[max(0, position - 1):position + 2]
        if index in strong or any(n in strong for n in neighbours if n != index):
            word = core(tokens[index])
            tokens[index] = tokens[index].replace(word, word.capitalize(), 1)
    return "".join(tokens)


def _for_daypart(voice: dict[str, Any], when: float | None = None) -> dict[str, Any]:
    """Performance directions follow the clock: "late-night" only at night."""
    instructions = voice.get("instructions")
    if not instructions:
        return voice
    voice = dict(voice)
    by_daypart = voice.get("instructions_by_daypart")
    part = showclock.daypart(when)
    if isinstance(by_daypart, dict) and by_daypart.get(part):
        voice["instructions"] = str(by_daypart[part])
    elif part != "late night":
        voice["instructions"] = re.sub(r"\blate[- ]night\b", showclock.voice_daypart(when),
                                       str(instructions), flags=re.I)
    return voice


def _resolved_voice(voice: dict[str, Any]) -> dict[str, Any]:
    voice = dict(voice)
    voice["engine"] = str(voice.get("engine") or backend()).lower()
    if voice["engine"] == "openrouter":
        voice["model"] = voice.get("model") or config.station.get(
            "tts.openrouter.model", "fish-audio/s2.1-pro-free:free")
        voice["openrouter_voice"] = voice.get("openrouter_voice") or config.station.get(
            "tts.openrouter.voice")
    return voice


def _key(text: str, voice: dict[str, Any]) -> str:
    voice = _resolved_voice(voice)
    blob = json.dumps([voice["engine"], text,
                       voice.get("name"), voice.get("rate"), voice.get("pitch"),
                       voice.get("volume"), voice.get("model"),
                       voice.get("openrouter_voice"), voice.get("speed"),
                       voice.get("instructions"), "speech-v2"],
                      sort_keys=True)
    return hashlib.sha1(blob.encode("utf-8")).hexdigest()[:20]


# --------------------------------------------------------------------------
# Edge
# --------------------------------------------------------------------------
async def _synthesise(text: str, path: Path, voice: dict[str, Any]) -> None:
    timeout = _timeout()
    communicate = edge_tts.Communicate(
        text,
        voice.get("name") or FALLBACK_VOICES[0],
        rate=str(voice.get("rate") or "+0%"),
        pitch=str(voice.get("pitch") or "+0Hz"),
        volume=str(voice.get("volume") or "+0%"),
        connect_timeout=int(min(10, timeout)),
        receive_timeout=int(timeout),
    )
    await asyncio.wait_for(communicate.save(str(path)), timeout)


def _say_edge(text: str, path: Path, voice: dict[str, Any]) -> bool:
    """Synthesise to a .part file and only then move it into the cache.

    An interrupted stream leaves a playable-looking but truncated MP3; caching
    that under the final name would air a clipped line forever.
    """
    part = path.with_name(path.name + ".part")
    try:
        asyncio.run(_synthesise(text, part, voice))
        if part.exists() and part.stat().st_size > 512 and _duration(part) > 0:
            part.replace(path)
            return True
        return False
    except (asyncio.TimeoutError, TimeoutError):
        _bench("edge", 30)
        return False
    except Exception as error:  # noqa: BLE001 - providers raise broadly
        if config.DEBUG:
            print(f"[tts] edge failed: {type(error).__name__}", flush=True)
        return False
    finally:
        part.unlink(missing_ok=True)


# --------------------------------------------------------------------------
# Duration, cached beside the file so each clip is probed once
# --------------------------------------------------------------------------
def _probe(path: Path) -> float:
    try:
        result = subprocess.run(
            [_ffprobe(), "-v", "error", "-show_entries", "format=duration",
             "-of", "json", str(path)],
            capture_output=True, text=True, timeout=20,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        return float(json.loads(result.stdout)["format"]["duration"])
    except Exception:  # noqa: BLE001
        return 0.0


def _sidecar(path: Path) -> Path:
    return path.with_name(path.stem + ".duration.json")


def _duration(path: Path) -> float:
    try:
        stat = path.stat()
    except OSError:
        return 0.0
    if path.suffix != ".mp3":
        return _probe(path)
    sidecar = _sidecar(path)
    try:
        cached = json.loads(sidecar.read_text(encoding="utf-8"))
        if cached["size"] == stat.st_size and cached["mtime"] == stat.st_mtime:
            return float(cached["duration"])
    except (OSError, ValueError, KeyError, TypeError):
        pass
    value = _probe(path)
    if value > 0:
        try:
            sidecar.write_text(json.dumps({"size": stat.st_size, "mtime": stat.st_mtime,
                                           "duration": value}), encoding="utf-8")
        except OSError:
            pass
    return value


# --------------------------------------------------------------------------
# Levelling
# --------------------------------------------------------------------------
def _measure(path: Path, chain: str, target: float, peak: float) -> dict[str, float] | None:
    """First loudnorm pass: what the clip measures, for an exact linear gain."""
    try:
        result = subprocess.run(
            [config.FFMPEG, "-nostdin", "-hide_banner", "-i", str(path), "-af",
             f"{chain}loudnorm=I={target}:TP={peak}:LRA=7:print_format=json",
             "-f", "null", "-"],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        block = result.stderr[result.stderr.rindex("{"):result.stderr.rindex("}") + 1]
        data = json.loads(block)
        values = {name: float(data[name]) for name in
                  ("input_i", "input_tp", "input_lra", "input_thresh", "target_offset")}
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired):
        return None
    return values if all(math.isfinite(v) for v in values.values()) else None


def levelled(path: Path) -> dict[str, Any]:
    """Cache broadcast-level speech without replacing a file being played.

    Two passes: measure, then apply one linear gain with those measurements,
    so short and long lines land at the same loudness and the dynamics of a
    single line are left alone. Clips under a few seconds are padded with
    silence while measured (silence is gated out of integrated loudness) and
    trimmed back afterwards. The versioned name upgrades old cache entries.
    """
    target = float(config.station.get("tts.target_lufs", -16.0))
    peak = float(config.station.get("tts.true_peak_db", -1.5))
    output = path.with_name(f"{path.stem}-voice-v2-{abs(target):g}-{abs(peak):g}.mp3")
    if not output.exists() or output.stat().st_mtime < path.stat().st_mtime:
        with tempfile.NamedTemporaryFile(suffix=".mp3", dir=path.parent, delete=False) as tmp:
            temporary = Path(tmp.name)
        try:
            length = _duration(path)
            short = 0 < length < MIN_MEASURE_SECONDS
            pad = f"apad=whole_dur={MIN_MEASURE_SECONDS}," if short else ""
            measured = _measure(path, pad, target, peak)
            loudnorm = f"loudnorm=I={target}:TP={peak}:LRA=7"
            if measured:
                loudnorm += (f":measured_I={measured['input_i']}:measured_TP={measured['input_tp']}"
                             f":measured_LRA={measured['input_lra']}"
                             f":measured_thresh={measured['input_thresh']}"
                             f":offset={measured['target_offset']}:linear=true")
            chain = pad + loudnorm + (f",atrim=0:{length:.3f}" if short else "")
            result = subprocess.run(
                [config.FFMPEG, "-nostdin", "-v", "error", "-y", "-i", str(path),
                 "-af", chain, "-ar", "48000",
                 "-codec:a", "libmp3lame", "-b:a", "128k", str(temporary)],
                capture_output=True, timeout=60,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            if result.returncode or temporary.stat().st_size <= 512:
                raise RuntimeError("speech levelling failed")
            temporary.replace(output)
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            print(f"[tts] could not level speech: {error}", flush=True)
            output = path
        finally:
            temporary.unlink(missing_ok=True)
    return {"path": str(output), "duration": _duration(output)}


# --------------------------------------------------------------------------
# OpenRouter
# --------------------------------------------------------------------------
SPEECH_ENDPOINT = "https://openrouter.ai/api/v1/audio/speech"


def _say_openrouter(text: str, path: Path, voice: dict[str, Any]) -> bool:
    """Render one host; Gemini accepts performance direction in its prompt.

    Speech models are discoverable with /models?output_modalities=speech.
    Keep directions separate from the transcript and from fallback engines.
    Audio is converted in a temporary directory and moved into place only
    once it has been validated, so a failure never leaves a cached file.
    """
    key = config.env("OPENROUTER_API_KEY")
    if not key:
        return False

    model = str(voice.get("model")
                or config.station.get("tts.openrouter.model",
                                      "fish-audio/s2.1-pro-free:free"))
    bench_key = "openrouter:" + model
    gemini = model.startswith("google/gemini-") and "tts" in model
    instructions = str(voice.get("instructions") or "").strip()
    prompt = text
    if gemini and instructions:
        prompt = ("Generate speech for the transcript below. Speak only the transcript, "
                  "not the performance directions. Do not add any words.\n\n"
                  f"Performance directions:\n{instructions}\n\nTranscript:\n{text}")
    payload: dict[str, Any] = {
        "model": model,
        "input": prompt,
        "response_format": "pcm" if gemini else "mp3",
    }
    name = voice.get("openrouter_voice") or config.station.get(
        "tts.openrouter.voice")
    if name:
        payload["voice"] = str(name)
    if voice.get("speed") and not gemini:
        payload["speed"] = float(voice["speed"])

    try:
        response = httpx.post(
            SPEECH_ENDPOINT,
            headers={
                "Authorization": f"Bearer {key}",
                "Content-Type": "application/json",
                "HTTP-Referer": config.env("OPENROUTER_REFERER",
                                           "http://127.0.0.1:8090"),
                "X-OpenRouter-Title": str(
                    config.station.get("identity.name", "radio")),
            },
            json=payload, timeout=_timeout())
        if response.status_code >= 400:
            print(f"[tts] {model}: HTTP {response.status_code}; trying fallback", flush=True)
            _bench(bench_key, 300 if response.status_code == 429 else 120)
            return False
        if not response.content or len(response.content) < 512:
            return False
        content_type = response.headers.get("content-type", "").split(";", 1)[0].lower()
        if content_type not in {"audio/mpeg", "audio/mp3", "audio/pcm", "audio/l16", "audio/wav", "audio/x-wav"}:
            return False
        with tempfile.TemporaryDirectory(dir=path.parent) as directory:
            raw = Path(directory) / "speech"
            converted = Path(directory) / "speech.mp3"
            raw.write_bytes(response.content)
            args = [config.FFMPEG, "-nostdin", "-v", "error", "-y"]
            if content_type in {"audio/pcm", "audio/l16"}:
                # Gemini's native output is little-endian 24 kHz, mono PCM.
                if not gemini or len(response.content) % 2:
                    return False
                args += ["-f", "s16le", "-ar", "24000", "-ac", "1"]
            result = subprocess.run(
                [*args, "-i", str(raw), "-vn", "-ar", "48000", "-ac", "1",
                 "-codec:a", "libmp3lame", "-b:a", "128k", str(converted)],
                capture_output=True, timeout=60,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            if result.returncode or not converted.exists() or _probe(converted) <= 0:
                return False
            converted.replace(path)
    except httpx.TimeoutException:
        print(f"[tts] {model}: timed out after {_timeout():.0f}s; trying fallback", flush=True)
        _bench(bench_key, 60)
        return False
    except httpx.TransportError as error:
        if config.DEBUG:
            print("[tts] openrouter unreachable", type(error).__name__, flush=True)
        _bench(bench_key, 60)
        return False
    except Exception as error:  # noqa: BLE001
        if config.DEBUG:
            print("[tts] openrouter failed", type(error).__name__, flush=True)
        return False
    return path.exists() and path.stat().st_size > 512


# --------------------------------------------------------------------------
# Public API
# --------------------------------------------------------------------------
def _candidates(voice: dict[str, Any]) -> list[dict[str, Any]]:
    """The primary voice, then hosted fallbacks, then Edge voices, in order."""
    primary = _resolved_voice(voice)
    candidates = [primary] if primary["engine"] == "openrouter" else []
    fallback = voice.get("fallback")
    edge = dict(voice)
    if isinstance(fallback, dict):
        previous = _resolved_voice(fallback)
        if previous["engine"] == "openrouter":
            candidates.append(previous)
        edge.update(fallback)
    # Preserve each host's previous Edge settings as the final fallback.
    for name in dict.fromkeys([edge.get("name"), *FALLBACK_VOICES]):
        if name:
            candidates.append({**edge, "engine": "edge", "name": name,
                               "model": None, "openrouter_voice": None,
                               "instructions": None, "speed": None})
    return candidates


def say(text: str, voice: dict[str, Any], *, start: int = 0) -> dict[str, Any] | None:
    """Render one line. Returns {path, duration, ...} or None on failure.

    Cached by (backend, text, voice settings), so a repeated station ident
    costs nothing the second time. `start` skips earlier candidates, which is
    how a break stays on one fallback voice once a line needed it.
    """
    text = speakable((text or "").strip())
    if not text:
        return None

    VOICE_DIR.mkdir(parents=True, exist_ok=True)
    voice = _for_daypart(voice)
    for index, candidate in enumerate(_candidates(voice)):
        if index < start:
            continue
        path = VOICE_DIR / f"{_key(text, candidate)}.mp3"
        try:
            cached = path.exists() and path.stat().st_size > 512 and _duration(path) > 0
            if not cached:
                if not _available(_bench_key(candidate)):
                    continue
                if candidate["engine"] == "openrouter":
                    if not _say_openrouter(text, path, candidate):
                        continue
                elif not _say_edge(text, path, candidate):
                    continue
            if path.exists() and path.stat().st_size > 512 and _duration(path) > 0:
                return {**levelled(path), "engine": candidate["engine"],
                        "model": candidate.get("model"),
                        "voice": candidate.get("openrouter_voice") if candidate["engine"] == "openrouter" else candidate["name"],
                        "fallback": index > 0, "candidate": index}
            path.unlink(missing_ok=True)
        except Exception as error:  # noqa: BLE001 - providers raise broadly
            if config.DEBUG:
                print(f"[tts] {candidate['engine']} failed: {type(error).__name__}", flush=True)
    return None


def _voice_identity(voice: dict[str, Any]) -> str:
    return json.dumps(voice, sort_keys=True, default=str)


def say_many(jobs: list[tuple[str, dict[str, Any]]], workers: int = 3) -> list[dict[str, Any] | None]:
    """Render a break's lines in parallel. Results line up with `jobs`.

    Once any of a host's lines needed a fallback voice, that host's other
    lines are re-rendered on the same fallback, so nobody changes voice
    mid-break. A line that fails outright comes back as None; see
    required_failed().
    """
    jobs = list(jobs)
    results: list[dict[str, Any] | None] = [None] * len(jobs)
    if not jobs:
        return results

    def run(index: int, start: int = 0) -> dict[str, Any] | None:
        text, voice = jobs[index]
        try:
            return say(text, voice, start=start)
        except Exception as error:  # noqa: BLE001 - one line never sinks the batch
            if config.DEBUG:
                print(f"[tts] line failed: {type(error).__name__}", flush=True)
            return None

    with ThreadPoolExecutor(max_workers=max(1, min(workers, len(jobs))),
                            thread_name_prefix="tts") as pool:
        for index, result in enumerate(pool.map(run, range(len(jobs)))):
            results[index] = result

        groups: dict[str, list[int]] = {}
        for index, (_, voice) in enumerate(jobs):
            groups.setdefault(_voice_identity(voice), []).append(index)
        redo: list[tuple[int, int]] = []
        for indexes in groups.values():
            used = [results[i].get("candidate", 0) for i in indexes if results[i]]
            if used and min(used) != max(used):
                floor = max(used)
                redo += [(i, floor) for i in indexes if results[i] and results[i].get("candidate", 0) < floor]
        for (index, _), result in zip(redo, pool.map(lambda job: run(*job), redo)):
            if result:
                results[index] = result
    return results


def required_failed(lines: list[Any], results: list[dict[str, Any] | None]) -> bool:
    """True when a line marked `required` (the song-naming line) has no audio.

    The caller should then drop the whole break rather than air it without
    the line that makes it make sense.
    """
    return any(getattr(line, "required", False) and not result
               for line, result in zip(lines, results))


def list_voices() -> list[dict[str, str]]:
    """Every installed Edge voice. Used by `python -m radio.cli voices`."""
    async def fetch() -> list[dict[str, Any]]:
        return await edge_tts.list_voices()

    try:
        voices = asyncio.run(fetch())
    except Exception:  # noqa: BLE001
        return []
    return [
        {
            "name": v.get("ShortName", ""),
            "gender": v.get("Gender", ""),
            "locale": v.get("Locale", ""),
            "personality": ", ".join(
                (v.get("VoiceTag") or {}).get("VoicePersonalities") or []),
        }
        for v in voices
    ]


def evict(keep: set[str] | None = None, max_files: int = 600) -> None:
    """Voice lines are tiny but they accumulate. Trim oldest-first."""
    keep = {str(Path(p)) for p in (keep or set())}
    files = [p for p in VOICE_DIR.glob("*.mp3") if str(p) not in keep]
    if len(files) <= max_files:
        return
    files.sort(key=lambda p: p.stat().st_atime)
    for path in files[:len(files) - max_files]:
        try:
            path.unlink()
            _sidecar(path).unlink(missing_ok=True)
        except OSError:
            pass
