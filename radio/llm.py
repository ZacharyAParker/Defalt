"""Text generation through a local session or OpenRouter, with fallback.

Free models rate-limit constantly, so every call walks a fallback chain before
giving up. Nothing here is allowed to take the station off the air: on total
failure the caller gets None and falls back to a canned line.
"""
from __future__ import annotations

import json
import re
import threading
import time
from typing import Any

import httpx

from . import config

ENDPOINT = "https://openrouter.ai/api/v1/chat/completions"
_COOLDOWN: dict[str, float] = {}
_LOCK = threading.Lock()

# Models that 400 on the reasoning switch. Learned once at runtime, so a
# provider that does not support it costs one retry per process, not per call.
_NO_REASONING_FLAG: set[str] = set()


def _suppress_reasoning() -> bool:
    return config.env_bool("OPENROUTER_DISABLE_REASONING", True)


class LLMUnavailable(RuntimeError):
    pass


def _models() -> list[str]:
    primary = config.env("OPENROUTER_MODEL",
                         "deepseek/deepseek-v4-flash-0731")
    chain = [primary, *config.env_list("OPENROUTER_FALLBACKS")]
    seen: list[str] = []
    for model in chain:
        if model and model not in seen:
            seen.append(model)
    return seen


def _available(model: str) -> bool:
    with _LOCK:
        return _COOLDOWN.get(model, 0) < time.time()


def _benched(model: str, seconds: float) -> None:
    with _LOCK:
        _COOLDOWN[model] = time.time() + seconds


def _openrouter_complete(system: str, user: str, *, max_tokens: int = 700,
                         temperature: float = 0.9, timeout: float = 45.0) -> str | None:
    """Return generated text, or None if every model in the chain failed."""
    key = config.env("OPENROUTER_API_KEY")
    if not key:
        return None

    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "HTTP-Referer": config.env("OPENROUTER_REFERER", "http://127.0.0.1:8080"),
        "X-Title": str(config.station.get("identity.name", "radio")),
    }
    # Reasoning models spend the token budget thinking before they answer, so
    # a budget sized for the answer alone comes back empty or truncated. The
    # headroom is added to every request: on a non-reasoning model it costs
    # nothing, because it stops when it is finished.
    headroom = int(config.env("OPENROUTER_REASONING_HEADROOM", "1200") or 0)

    payload_base = {
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens + headroom,
        "temperature": temperature,
    }

    deadline = time.monotonic() + max(0.0, timeout)
    for model in _models():
        if not _available(model):
            continue

        for attempt in (0, 1):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            payload = {**payload_base, "model": model}
            # Nothing the hosts say needs a chain of thought, and a reasoning
            # model will happily spend the entire budget thinking and return
            # an empty message. Turn it off where the model allows it.
            use_flag = _suppress_reasoning() and model not in _NO_REASONING_FLAG
            if use_flag:
                payload["reasoning"] = {"enabled": False}

            try:
                response = httpx.post(ENDPOINT, headers=headers, json=payload,
                                      timeout=remaining)
            except httpx.HTTPError as error:
                if config.DEBUG:
                    print("[llm] transport error", model, error, flush=True)
                _benched(model, 60)
                break

            if response.status_code == 429:
                # Free tiers bench you for a while. Respect it and move on.
                if config.DEBUG:
                    print("[llm]", model, "429 rate limited", flush=True)
                _benched(model, 300)
                break

            if response.status_code >= 400:
                # Some providers reject the reasoning switch outright. Learn
                # that once, then stop sending it to this model.
                if use_flag and attempt == 0:
                    _NO_REASONING_FLAG.add(model)
                    if config.DEBUG:
                        print("[llm]", model,
                              "rejected the reasoning flag, retrying without",
                              flush=True)
                    continue
                if config.DEBUG:
                    print("[llm]", model, response.status_code,
                          response.text[:300], flush=True)
                _benched(model, 120)
                break

            try:
                data = response.json()
                choice = data["choices"][0]
                text = choice["message"].get("content")
            except (json.JSONDecodeError, KeyError, IndexError, TypeError):
                _benched(model, 60)
                break

            if text and text.strip():
                return text.strip()

            # Empty content almost always means the budget went on reasoning.
            if config.DEBUG:
                spent = (data.get("usage") or {}).get(
                    "completion_tokens_details", {}).get("reasoning_tokens")
                print(f"[llm] {model} returned nothing "
                      f"(finish={choice.get('finish_reason')}, "
                      f"reasoning_tokens={spent}) -- raise "
                      f"OPENROUTER_REASONING_HEADROOM", flush=True)
            break

    return None


_FENCE = re.compile(r"^```(?:json)?\s*|\s*```$", re.MULTILINE)


def _parse_json(raw: str) -> Any | None:
    if not isinstance(raw, str) or not raw:
        return None

    cleaned = _FENCE.sub("", raw).strip()
    try:
        return json.loads(cleaned)
    except json.JSONDecodeError:
        pass

    for opener, closer in (("[", "]"), ("{", "}")):
        start, end = cleaned.find(opener), cleaned.rfind(closer)
        if start != -1 and end > start:
            try:
                return json.loads(cleaned[start:end + 1])
            except json.JSONDecodeError:
                continue
    if config.DEBUG:
        print("[llm] unparseable response:", cleaned[:400], flush=True)
    return None


def complete(system: str, user: str, *, max_tokens: int = 700,
             temperature: float = 0.9, timeout: float = 45.0,
             purpose: str = 'utility', json_mode: bool = False,
             validator=None) -> str | None:
    from . import session_backend
    deadline = time.monotonic() + max(0.0, timeout)
    use_session = session_backend.enabled()
    failure = None
    if use_session:
        memory, fingerprint, memory_error = session_backend.prepare(user, purpose)
        try:
            session_limit = float(session_backend.setting('timeout_seconds', 25))
        except (TypeError, ValueError):
            session_limit = 25.0
        budget = max(0.0, min(session_limit, timeout * .65))
        result = session_backend.complete(system, user, purpose=purpose, memory=memory,
            memory_fingerprint=fingerprint, timeout=budget, json_mode=json_mode,
            validator=validator, memory_warning=memory_error) if budget > 0 else None
        if result is not None:
            return result
        failure = session_backend.status()['last_result'].get('error') or memory_error
        # Keep the exact same approved facts and current task when falling back.
        system = session_backend.augment(system, memory)
    remaining = deadline - time.monotonic()
    raw = (_openrouter_complete(system, user, max_tokens=max_tokens,
            temperature=temperature, timeout=remaining) if remaining > 0 else None)
    if raw and (json_mode or validator):
        parsed = _parse_json(raw) if json_mode else raw
        if parsed is None or (validator is not None and not validator(parsed)):
            raw = None
    if use_session:
        session_backend.record('openrouter' if raw else 'canned', fallback_reason=failure)
    return raw


def complete_json(system: str, user: str, **kwargs: Any) -> Any | None:
    system += "\n\nRespond with valid JSON only. No prose, no code fences."
    raw = complete(system, user, json_mode=True, **kwargs)
    return _parse_json(raw) if raw else None


def status() -> dict[str, Any]:
    from . import session_backend
    with _LOCK:
        benched = {m: round(t - time.time(), 1)
                   for m, t in _COOLDOWN.items() if t > time.time()}
    return {
        "configured": bool(config.env("OPENROUTER_API_KEY")) or (session_backend.enabled() and bool(session_backend.executable())),
        "models": _models(),
        "benched": benched,
        "director": session_backend.status(),
    }
