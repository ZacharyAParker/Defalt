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
# Models whose provider rejects response_format=json_object.
_NO_JSON_FORMAT: set[str] = set()
# A timeout only benches a model when it had at least this long to answer.
BENCH_TIMEOUT_AFTER = 15.0


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
                         temperature: float = 0.9, timeout: float = 45.0,
                         json_mode: bool = False, validator=None,
                         json_object: bool = False) -> str | None:
    """Return generated text, or None if every model in the chain failed.

    A draft that fails the caller's validator moves on to the next model
    instead of ending the whole request. Models are benched only for signs
    of an unhealthy provider -- connection errors, 5xx, 429, or a timeout
    that had plenty of time -- never because the caller's deadline ran out.
    """
    key = config.env("OPENROUTER_API_KEY")
    if not key:
        return None

    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "HTTP-Referer": config.env("OPENROUTER_REFERER", "http://127.0.0.1:8090"),
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
    want_json_object = json_object and config.env_bool("OPENROUTER_JSON_OBJECT", True)

    deadline = time.monotonic() + max(0.0, timeout)
    for model in _models():
        if not _available(model):
            continue

        for attempt in range(3):
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
            # JSON mode where the provider supports it; learned per model.
            use_format = want_json_object and model not in _NO_JSON_FORMAT
            if use_format:
                payload["response_format"] = {"type": "json_object"}

            try:
                response = httpx.post(ENDPOINT, headers=headers, json=payload,
                                      timeout=remaining)
            except httpx.TimeoutException as error:
                if config.DEBUG:
                    print("[llm] timeout", model, type(error).__name__, flush=True)
                # Running out of the caller's short deadline says nothing
                # about the model's health; a long wait that still timed out does.
                if remaining >= BENCH_TIMEOUT_AFTER:
                    _benched(model, 60)
                break
            except httpx.HTTPError as error:
                if config.DEBUG:
                    print("[llm] transport error", model, type(error).__name__, flush=True)
                _benched(model, 60)
                break

            if response.status_code == 429:
                # Free tiers bench you for a while. Respect it and move on.
                if config.DEBUG:
                    print("[llm]", model, "429 rate limited", flush=True)
                _benched(model, 300)
                break

            if response.status_code >= 500:
                if config.DEBUG:
                    print("[llm]", model, response.status_code, flush=True)
                _benched(model, 120)
                break

            if response.status_code >= 400:
                # Some providers reject optional switches outright. Learn
                # that once, then stop sending them to this model.
                detail = response.text[:300].lower()
                if use_format and ("response_format" in detail or not use_flag):
                    _NO_JSON_FORMAT.add(model)
                    if config.DEBUG:
                        print("[llm]", model, "rejected response_format, retrying without", flush=True)
                    continue
                if use_flag:
                    _NO_REASONING_FLAG.add(model)
                    if config.DEBUG:
                        print("[llm]", model,
                              "rejected the reasoning flag, retrying without",
                              flush=True)
                    continue
                if use_format:
                    _NO_JSON_FORMAT.add(model)
                    continue
                if config.DEBUG:
                    print("[llm]", model, response.status_code,
                          response.text[:300], flush=True)
                # A request error is not an outage; try the next model.
                break

            try:
                data = response.json()
                choice = data["choices"][0]
                text = choice["message"].get("content")
            except (json.JSONDecodeError, KeyError, IndexError, TypeError):
                break

            if text and text.strip():
                text = text.strip()
                if json_mode or validator is not None:
                    parsed = _parse_json(text) if json_mode else text
                    if parsed is None or (validator is not None and not validator(parsed)):
                        # Unusable draft: another model may do better.
                        if config.DEBUG:
                            print("[llm]", model, "draft failed validation; trying next model", flush=True)
                        break
                return text

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
             validator=None, json_object: bool = False) -> str | None:
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
            temperature=temperature, timeout=remaining, json_mode=json_mode,
            validator=validator, json_object=json_object) if remaining > 0 else None)
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
