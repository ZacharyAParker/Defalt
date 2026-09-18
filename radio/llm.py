"""OpenRouter client -- writes everything the hosts say.

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


def complete(system: str, user: str, *, max_tokens: int = 700,
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

    for model in _models():
        if not _available(model):
            continue

        for attempt in (0, 1):
            payload = {**payload_base, "model": model}
            # Nothing the hosts say needs a chain of thought, and a reasoning
            # model will happily spend the entire budget thinking and return
            # an empty message. Turn it off where the model allows it.
            use_flag = _suppress_reasoning() and model not in _NO_REASONING_FLAG
            if use_flag:
                payload["reasoning"] = {"enabled": False}

            try:
                response = httpx.post(ENDPOINT, headers=headers, json=payload,
                                      timeout=timeout)
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


def complete_json(system: str, user: str, **kwargs: Any) -> Any | None:
    """Ask for JSON and actually get JSON back.

    Small free models wrap output in prose or code fences roughly half the
    time, so we strip fences and fall back to grabbing the outermost bracketed
    span before giving up.
    """
    system = (system + "\n\nRespond with valid JSON only. No prose, no code "
                       "fences, no explanation before or after.")
    raw = complete(system, user, **kwargs)
    if not raw:
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


def status() -> dict[str, Any]:
    with _LOCK:
        benched = {m: round(t - time.time(), 1)
                   for m, t in _COOLDOWN.items() if t > time.time()}
    return {
        "configured": bool(config.env("OPENROUTER_API_KEY")),
        "models": _models(),
        "benched": benched,
    }
