"""Voices -- turns written lines into host audio.

Two engines. Edge TTS is the default: free, fast, 300+ voices. OpenRouter's
/audio/speech endpoint is the alternative, for hosted models like Fish Audio.
The engine is settable per persona, because hosted speech models often expose
only one voice, and two hosts sharing a voice is not a two-host show -- so you
may well want one host on each.

Each line is rendered separately rather than one blob per break. That costs a
few extra requests but buys precise control: the director can overlap Rue on
top of Mav to make an interruption land, and it can back-time an individual
line so the last word hits the post.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import edge_tts
import httpx

from . import config

VOICE_DIR = config.CACHE_DIR / "voice"

# If a persona names a voice Edge does not have, fall back to these rather
# than dropping the line entirely.
FALLBACK_VOICES = ["en-US-GuyNeural", "en-US-JennyNeural", "en-US-AriaNeural"]


def backend() -> str:
    return str(config.station.get("tts.backend", "edge") or "edge").lower()


def _key(text: str, voice: dict[str, Any]) -> str:
    blob = json.dumps([voice.get("engine") or backend(), text,
                       voice.get("name"), voice.get("rate"), voice.get("pitch"),
                       voice.get("volume"), voice.get("model"),
                       voice.get("openrouter_voice"), voice.get("speed")],
                      sort_keys=True)
    return hashlib.sha1(blob.encode("utf-8")).hexdigest()[:20]


async def _synthesise(text: str, path: Path, voice: dict[str, Any]) -> None:
    communicate = edge_tts.Communicate(
        text,
        voice.get("name") or FALLBACK_VOICES[0],
        rate=str(voice.get("rate") or "+0%"),
        pitch=str(voice.get("pitch") or "+0Hz"),
        volume=str(voice.get("volume") or "+0%"),
    )
    await communicate.save(str(path))


def _duration(path: Path) -> float:
    try:
        result = subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries", "format=duration",
             "-of", "json", str(path)],
            capture_output=True, text=True, timeout=20,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        return float(json.loads(result.stdout)["format"]["duration"])
    except Exception:  # noqa: BLE001
        return 0.0


def levelled(path: Path) -> dict[str, Any]:
    """Cache broadcast-level speech without replacing a file being played.

    The versioned name also upgrades existing TTS cache entries on first use.
    Loudnorm controls loudness and peaks; a raw gain boost would clip shouts.
    """
    target = float(config.station.get("tts.target_lufs", -16.0))
    peak = float(config.station.get("tts.true_peak_db", -1.5))
    output = path.with_name(f"{path.stem}-voice-v1-{abs(target):g}-{abs(peak):g}.mp3")
    if not output.exists() or output.stat().st_mtime < path.stat().st_mtime:
        with tempfile.NamedTemporaryFile(suffix=".mp3", dir=path.parent, delete=False) as tmp:
            temporary = Path(tmp.name)
        try:
            result = subprocess.run(
                [config.FFMPEG, "-nostdin", "-v", "error", "-y", "-i", str(path),
                 "-af", f"loudnorm=I={target}:TP={peak}:LRA=7", "-ar", "48000",
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


SPEECH_ENDPOINT = "https://openrouter.ai/api/v1/audio/speech"


def _say_openrouter(text: str, path: Path, voice: dict[str, Any]) -> bool:
    """Speak via OpenRouter's OpenAI-compatible /audio/speech endpoint.

    Note this is a different endpoint from the chat models -- speech models do
    not appear in /models, so `cli tts-models` cannot enumerate them. You have
    to know the id, e.g. fish-audio/s2.1-pro-free:free.

    Voice support varies sharply by model: the free Fish model accepts only
    `alloy`, which is why `engine` is settable per persona. Two hosts on one
    voice is not a two-host show.
    """
    key = config.env("OPENROUTER_API_KEY")
    if not key:
        return False

    model = str(voice.get("model")
                or config.station.get("tts.openrouter.model",
                                      "fish-audio/s2.1-pro-free:free"))
    payload: dict[str, Any] = {
        "model": model,
        "input": text,
        # Only mp3 and pcm are accepted. mp3 saves us a conversion pass.
        "response_format": "mp3",
    }
    name = voice.get("openrouter_voice") or config.station.get(
        "tts.openrouter.voice")
    if name:
        payload["voice"] = str(name)
    if voice.get("speed"):
        payload["speed"] = float(voice["speed"])

    try:
        response = httpx.post(
            SPEECH_ENDPOINT,
            headers={
                "Authorization": f"Bearer {key}",
                "Content-Type": "application/json",
                "HTTP-Referer": config.env("OPENROUTER_REFERER",
                                           "http://127.0.0.1:8080"),
                "X-OpenRouter-Title": str(
                    config.station.get("identity.name", "radio")),
            },
            json=payload, timeout=120)
        if response.status_code >= 400:
            if config.DEBUG:
                print("[tts] openrouter", response.status_code,
                      response.text[:300], flush=True)
            return False
        if not response.content or len(response.content) < 512:
            return False
        path.write_bytes(response.content)
    except Exception as error:  # noqa: BLE001
        if config.DEBUG:
            print("[tts] openrouter failed", error, flush=True)
        return False
    return path.exists() and path.stat().st_size > 512


def say(text: str, voice: dict[str, Any]) -> dict[str, Any] | None:
    """Render one line. Returns {path, duration} or None on failure.

    Cached by (backend, text, voice settings), so a repeated station ident
    costs nothing the second time.
    """
    text = (text or "").strip()
    if not text:
        return None

    VOICE_DIR.mkdir(parents=True, exist_ok=True)
    path = VOICE_DIR / f"{_key(text, voice)}.mp3"

    if path.exists() and path.stat().st_size > 512:
        return levelled(path)

    # A persona may name its own engine, so one host can be on a hosted voice
    # and the other on Edge. Falls back to the station-wide setting.
    if str(voice.get("engine") or backend()).lower() == "openrouter":
        if _say_openrouter(text, path, voice):
            duration = _duration(path)
            if duration > 0:
                return levelled(path)
        # Never lose a line because a hosted engine was unavailable.
        if config.DEBUG:
            print("[tts] openrouter unavailable, falling back to edge", flush=True)

    attempts = [voice.get("name"), *FALLBACK_VOICES]
    for name in attempts:
        if not name:
            continue
        try:
            asyncio.run(_synthesise(text, path, {**voice, "name": name}))
        except Exception as error:  # noqa: BLE001 - edge_tts raises broadly
            if config.DEBUG:
                print("[tts] failed", name, error, flush=True)
            continue
        if path.exists() and path.stat().st_size > 512:
            duration = _duration(path)
            if duration > 0:
                return levelled(path)
        try:
            path.unlink()
        except OSError:
            pass
    return None


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
        except OSError:
            pass
