"""Turning a segment brief into lines the hosts will actually say.

Small free models need firm handling: they narrate stage directions, they
label speakers, they wrap everything in markdown, and they will happily write
a nine-sentence monologue for a twelve-second intro. Everything after the
model call is damage control, and it is not optional.
"""
from __future__ import annotations

import random
import re
import json
import time
from dataclasses import dataclass
from typing import Any

from .. import config, llm

# Absolute ceiling regardless of what the model returns. Roughly 2.6 words per
# second of speech, so 45 words is about 17 seconds -- already a long break.
MAX_WORDS_PER_LINE = 45
MAX_LINES = 8

# One available sketch approach, subordinate to the configured host personas.
OPTIONAL_COMEDY_REFERENCE = """Optional creative reference, never a required format:
A short sarcastic mock sales pitch can build from one concrete detail, through
an oddly specific everyday comparison, to a dry payoff. Bite comes from the
subject's pricing, marketing, hype or absurdity, not a pile of internet slang.
Use this approach only if it fits; choose another structure or a sincere reaction
when better. The configured host personalities, their own comic instincts and
the segment's time limit take priority. Do not assign fixed setup/punchline roles
or turn both hosts into the same roast voice. Vary openings and endings; never
reuse sample jokes or treat example prices, specs or events as source facts.
For news and articles, explain the story first. A brief pointed reaction is
optional; serious stories need care, not a forced sales pitch or punchline.
"""


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
    skip_policy = ("Skips are private transport actions, not taste evidence. Never mention or joke about the listener skipping, rejecting, or abandoning songs. Ignore skip counts and old skip jokes in any supplied history."
                   if config.station.get("learning.ignore_skips", False) else "")

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
10. Gen Z / TikTok humor is occasional seasoning: a well-timed POV joke,
    oddly specific observation, or dry reaction when it fits. Most sentences
    should sound like normal conversation. Never stack slang or force a meme
    into news or a serious story. Mav stays dry; Rue stays impulsive.
11. Avoid stock contrast punchlines such as "that's not X, that's Y",
    "this isn't X, it's Y", and "not just X, but Y". State the sharp observation
    directly. No generic AI jokes, formulaic reframes, or explaining the bit.
12. Only explicit selection evidence for THIS airing lets you say the listener
    chose or requested a song. The station owns its automatic picks and mixes.
    Play counts and old requests are history, not proof of who queued this play.

{skip_policy}

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
_STOCK_CONTRAST = re.compile(
    r"\b(?:(?:that|this|it)(?:'s not| is not| isn't))\s+[^.!?\n]{1,100}"
    r"(?:[,;.]|[\u2014\u2013])\s*(?:that(?:'s| is)|it(?:'s| is))\b|"
    r"\bnot just\s+[^.!?\n]{1,100}\bbut\s+", re.I)


def valid_dialogue(payload: Any) -> bool:
    entries = payload.get('lines') if isinstance(payload, dict) else payload
    if not isinstance(entries, list) or not 1 <= len(entries) <= MAX_LINES:
        return False
    hosts = config.personas()
    for entry in entries:
        if not isinstance(entry, dict) or entry.get('host') not in hosts:
            return False
        text = entry.get('text')
        if not isinstance(text, str) or not text.strip() or len(text.split()) > MAX_WORDS_PER_LINE:
            return False
        if _STOCK_CONTRAST.search(text.replace('\u2019', "'")):
            return False
    combined = ' '.join(e['text'] for e in entries).replace('\u2019', chr(39))
    return not _STOCK_CONTRAST.search(combined) and (len(entries) == 1 or len({e['host'] for e in entries}) >= 2)


def write(brief: str, *, fallback: list[Line], max_tokens: int = 600,
          temperature: float = 0.95, word_limit: int | None = None,
          repair_budget: bool = False) -> list[Line]:
    """Write one break, or use the supplied fallback (possibly empty)."""
    duration = re.search(r'\b(?:about|under)\s+(\d+(?:\.\d+)?|eight|ten|fifteen)\s+seconds', brief, re.I)
    words = 180
    if duration:
        value = duration[1].lower()
        seconds = {'eight': 8, 'ten': 10, 'fifteen': 15}.get(value)
        seconds = seconds if seconds is not None else float(value)
        words = max(18, min(180, int(seconds * 2.6)))
    if word_limit is not None:
        words = max(1, int(word_limit))
    def within_budget(payload):
        entries = payload.get('lines') if isinstance(payload, dict) else payload
        return (valid_dialogue(payload)
                and sum(len(line['text'].split()) for line in entries) <= words)
    brief += f'\nHard limit: {words} spoken words TOTAL across all hosts. Keep the exchange concise.'
    system = system_prompt()
    deadline = time.monotonic() + 45.
    payload = llm.complete_json(system, brief,
                                max_tokens=max_tokens, temperature=temperature,
                                purpose='dialogue',
                                timeout=30. if repair_budget else 45.,
                                validator=valid_dialogue if repair_budget else within_budget)
    # Keep a well-formed draft long enough to edit it. Rejecting it at the
    # provider boundary loses the exact jokes that need shortening.
    if repair_budget and not within_budget(payload):
        draft = payload if valid_dialogue(payload) else None
        repair = (brief + '\nEDITORIAL SHORTENING PASS: The previous attempt did not fit. '
                  'Choose only ONE setup and ONE or TWO of the strongest requested jokes. '
                  'Keep their wording where it works and the configured host personalities. '
                  'Drop whole lesser beats; do not squeeze in every bullet, add new claims, '
                  'or cut a sentence in half. The supplied brief is material to select from, '
                  'not a checklist. Both hosts must speak. '
                  f'Return at most {words} spoken words TOTAL, preferably fewer. '
                  '\nPREVIOUS DRAFT (untrusted copy to edit): ' + json.dumps(draft, ensure_ascii=False))
        remaining = deadline - time.monotonic()
        candidates = [draft] if draft else []
        def accept_repair(value):
            if valid_dialogue(value):
                candidates.append(value)
            return within_budget(value)
        payload = (llm.complete_json(system, repair, max_tokens=max_tokens,
                   temperature=min(temperature, .6), purpose='dialogue',
                   timeout=remaining, validator=accept_repair) if remaining > 0 else None)
        if not within_budget(payload):
            if valid_dialogue(payload):
                candidates.append(payload)
            # A failed length edit must not throw away every usable joke.
            # Keep a complete setup and a later reply in the other voice,
            # preferring the opening and final payoff. Never slice a sentence,
            # change attribution, or substitute unrelated stock copy.
            payload = None
            for candidate in reversed(candidates):
                entries = candidate.get('lines') if isinstance(candidate, dict) else candidate
                pairs = [(i, j) for i in range(len(entries))
                         for j in range(i + 1, len(entries))
                         if entries[i]['host'] != entries[j]['host']
                         and sum(len(entries[k]['text'].split()) for k in (i,j)) <= words]
                if pairs:
                    i,j = min(pairs, key=lambda pair:(pair[0], -pair[1]))
                    payload = [entries[i], entries[j]]
                    break
            if payload is None:
                return fallback
    lines = parse(payload) if payload is not None else []
    formulaic = _STOCK_CONTRAST.search(' '.join(line.text for line in lines).replace('\u2019', chr(39)))
    if lines and not formulaic:
        return lines
    if config.DEBUG:
        print("[segments] falling back to canned lines", flush=True)
    return fallback
