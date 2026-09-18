"""Turning a segment brief into lines the hosts will actually say.

Small free models need firm handling: they narrate stage directions, they
label speakers, they wrap everything in markdown, and they will happily write
a nine-sentence monologue for a twelve-second intro. Everything after the
model call is damage control, and it is not optional.
"""
from __future__ import annotations

import random
import re
from dataclasses import dataclass
from typing import Any

from .. import config, llm

# Absolute ceiling regardless of what the model returns. Roughly 2.6 words per
# second of speech, so 45 words is about 17 seconds -- already a long break.
MAX_WORDS_PER_LINE = 45
MAX_LINES = 8


@dataclass
class Line:
    host: str
    text: str
    reference: dict | None = None


# --------------------------------------------------------------------------
# Prompt assembly
# --------------------------------------------------------------------------
def _persona_block(persona: dict[str, Any]) -> str:
    parts = [f"### {persona.get('name', persona['id'])}  (id: {persona['id']})"]
    if persona.get("character"):
        parts.append(str(persona["character"]).strip())
    if persona.get("writing_rules"):
        parts.append("How they write:\n" + str(persona["writing_rules"]).strip())
    examples = persona.get("examples") or []
    if examples:
        sample = random.sample(list(examples), min(4, len(examples)))
        parts.append("Tone reference (do not reuse these lines):\n"
                     + "\n".join(f"- {line}" for line in sample))
    return "\n\n".join(parts)


def system_prompt() -> str:
    personas = config.personas()
    identity = config.station.get("identity", {}) or {}
    blocks = "\n\n".join(_persona_block(p) for p in personas.values())
    ids = ", ".join(personas.keys())

    return f"""You write dialogue for a two-host radio show on {identity.get('name', 'a small station')} \
({identity.get('call_sign', '')}), broadcasting to exactly one listener in {identity.get('city', 'nowhere')}.
Station tagline: {identity.get('tagline', '')}

{blocks}

HARD RULES -- every one of these matters, the output is fed straight to a
text-to-speech engine and then broadcast:

1. Output ONLY spoken words. No stage directions, no "(laughs)", no "*sighs*",
   no speaker labels inside the text, no emoji, no markdown.
2. Write numbers, times and symbols the way a person says them out loud.
   "twelve gigabytes", not "12GB". "two forty in the morning", not "2:40 AM".
3. Never invent facts. If you were given source material, stay inside it. If
   you were given nothing, talk about the station or each other instead.
4. Keep every single line under {MAX_WORDS_PER_LINE} words. Most lines should be
   far shorter than that. Radio breaks are quick.
5. The two hosts sound nothing alike. If a line would work in either mouth,
   it is a bad line.
6. Do not write applause, sound effects, jingles, or descriptions of audio.
7. The listener has opted into personal music roasts. Follow the brief's
   intensity: pointed jokes about song choices, artist loyalty and artistic
   output are welcome. No slurs, protected-trait attacks, threats, invented
   accusations or claims about the listener's private life. Exaggeration
   must sound like a joke. No sexual content.
8. Never trail or promise what comes next -- no "music next", no "back after
   this", no "coming up". You do not know what follows this break, and the
   director may well pick something else.
9. Do not read a station ident unless the brief asks for one.

Valid host ids: {ids}

Return a JSON array of objects, each with "host" and "text":
[{{"host": "{list(personas)[0] if personas else 'mav'}", "text": "..."}}]"""


# --------------------------------------------------------------------------
# Cleanup
# --------------------------------------------------------------------------
_LABEL = re.compile(r"^\s*(?:\*\*)?[A-Z][A-Za-z]{1,12}\s*(?:\*\*)?\s*[:：]\s*")
_DIRECTION = re.compile(r"[\(\[\*][^\)\]\*]{0,60}[\)\]\*]")
_EMOJI = re.compile(
    "[\U0001F000-\U0001FAFF\U00002600-\U000027BF\U0001F1E6-\U0001F1FF←-⇿]"
)
_WS = re.compile(r"\s+")


def clean(text: str) -> str:
    text = str(text or "")
    text = _LABEL.sub("", text)
    text = _DIRECTION.sub(" ", text)
    text = _EMOJI.sub("", text)
    text = text.replace("**", "").replace("__", "").replace("`", "")
    text = _WS.sub(" ", text).strip(" -–—\t")
    return text.strip()


def _trim_words(text: str) -> str:
    words = text.split()
    if len(words) <= MAX_WORDS_PER_LINE:
        return text
    # Cut at the last sentence boundary that fits, rather than mid-word.
    truncated = " ".join(words[:MAX_WORDS_PER_LINE])
    for mark in (". ", "! ", "? "):
        if mark in truncated:
            return truncated[:truncated.rindex(mark) + 1].strip()
    return truncated.rstrip(",;: ") + "."


def parse(payload: Any) -> list[Line]:
    """Coerce whatever the model returned into a clean list of lines."""
    personas = config.personas()
    if isinstance(payload, dict):
        # Models love to wrap the array in a key. Find it.
        for value in payload.values():
            if isinstance(value, list):
                payload = value
                break
    if not isinstance(payload, list):
        return []

    lines: list[Line] = []
    for entry in payload[:MAX_LINES]:
        if not isinstance(entry, dict):
            continue
        host = str(entry.get("host") or entry.get("speaker") or "").strip().lower()
        if host not in personas:
            # Sometimes it uses the display name instead of the id.
            match = next((pid for pid, p in personas.items()
                          if str(p.get("name", "")).lower() == host), None)
            if not match:
                continue
            host = match
        text = _trim_words(clean(entry.get("text") or entry.get("line") or ""))
        if len(text) < 2:
            continue
        lines.append(Line(host=host, text=text))

    # Two identical hosts in a row is fine occasionally, but a whole break in
    # one voice means the model ignored the format.
    if len({line.host for line in lines}) < 2 and len(lines) > 2:
        for index in range(1, len(lines), 2):
            others = [h for h in personas if h != lines[index].host]
            if others:
                lines[index].host = others[0]
    return lines


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------
def write(brief: str, *, fallback: list[Line], max_tokens: int = 600,
          temperature: float = 0.95) -> list[Line]:
    """Write one break. Never raises, never returns empty."""
    payload = llm.complete_json(system_prompt(), brief,
                                max_tokens=max_tokens, temperature=temperature)
    lines = parse(payload) if payload is not None else []
    if lines:
        return lines
    if config.DEBUG:
        print("[segments] falling back to canned lines", flush=True)
    return fallback
