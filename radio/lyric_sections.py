"""Verse, chorus and bridge, read off the synced lyrics themselves.

No model and no audio: a chorus is the block of lines a song keeps coming
back to, a verse is a block it sings once, and a bridge is the one late
block between choruses that never returns. Where nobody sings -- before the
first line, after the last, a long gap in the middle -- is intro, outro or
an instrumental break. Boundaries are then pulled onto the record's own bar
grid (an eight-bar phrase line when one is within a bar, else the nearest
downbeat), because a vocal pickup starts a beat early and a mix point should
not.

Everything here is pure and cheap. `radio/lyrics.py` stores the result; the
planner, the scheduler and the players read it. All times are source seconds.
"""
from __future__ import annotations

import math
import re
from difflib import SequenceMatcher
from typing import Any, Iterable

VERSION = 1
LABELS = ("intro", "verse", "chorus", "bridge", "instrumental", "outro")
# A pause this long between two sung lines is a break in the music, not a breath.
INSTRUMENTAL_GAP = 8.0
# Shortest intro/outro worth naming.
EDGE_MIN = 2.0

_WORD = re.compile(r"[^\w\s']+")


def _number(value: Any, fallback: float | None = None) -> float | None:
    try:
        value = float(value)
    except (TypeError, ValueError):
        return fallback
    return value if math.isfinite(value) else fallback


def norm_line(text: str) -> str:
    """A sung line with the punctuation and case that never change the words."""
    text = _WORD.sub(" ", (text or "").lower().replace("’", "'"))
    return " ".join(text.split())


def similar(a: str, b: str) -> float:
    if not a or not b:
        return 0.0
    if a == b:
        return 1.0
    return SequenceMatcher(None, a, b, autojunk=False).ratio()


def _sung(lines: Iterable[dict]) -> list[dict]:
    out = []
    for line in lines or []:
        t = _number(line.get("t") if isinstance(line, dict) else None)
        if t is None or t < 0:
            continue
        out.append({"t": t, "text": str(line.get("text") or "").strip()})
    out.sort(key=lambda line: line["t"])
    return out


def _median(values: list[float], fallback: float) -> float:
    values = sorted(v for v in values if v > 0)
    if not values:
        return fallback
    middle = len(values) // 2
    return values[middle] if len(values) % 2 else (values[middle - 1] + values[middle]) / 2


def line_cap(lines: list[dict]) -> float:
    """How long one sung line can be assumed to last, at most."""
    starts = [line["t"] for line in lines if line["text"]]
    typical = _median([b - a for a, b in zip(starts, starts[1:])], 3.5)
    return min(8.0, max(4.0, 2.0 * typical))


def vocal_spans(lines: Iterable[dict], duration: float | None = None, merge: float = 1.0) -> list[list[float]]:
    """Where somebody is singing: each line from its start to the next line's
    start, capped, with near-touching spans merged to keep the list short."""
    lines = _sung(lines)
    cap = line_cap(lines)
    end_of_song = _number(duration)
    spans: list[list[float]] = []
    for index, line in enumerate(lines):
        if not line["text"]:
            continue
        following = lines[index + 1]["t"] if index + 1 < len(lines) else line["t"] + cap
        end = min(following, line["t"] + cap)
        if end_of_song is not None:
            end = min(end, end_of_song)
        if end <= line["t"]:
            continue
        if spans and line["t"] - spans[-1][1] <= merge:
            spans[-1][1] = max(spans[-1][1], end)
        else:
            spans.append([line["t"], end])
    return [[round(a, 2), round(b, 2)] for a, b in spans]


def vocal_at(spans: Iterable[Iterable[float]] | None, seconds: float) -> bool | None:
    """True while a line is being sung, False between lines, None without lyrics."""
    if not spans:
        return None
    return any(float(a) <= seconds < float(b) for a, b in spans)


def vocal_fraction(spans: Iterable[Iterable[float]] | None, start: float, end: float) -> float | None:
    """Share of [start, end] covered by sung lines, or None without lyrics."""
    if not spans or not end > start:
        return None
    covered = sum(max(0.0, min(end, float(b)) - max(start, float(a))) for a, b in spans)
    return max(0.0, min(1.0, covered / (end - start)))


# --------------------------------------------------------------------------
# Blocks
# --------------------------------------------------------------------------
def _blocks(lines: list[dict]) -> list[list[dict]]:
    """Group sung lines into blocks at blank markers and long pauses, then
    split a block wherever the song turns from new lines to repeated ones."""
    starts = [line["t"] for line in lines if line["text"]]
    typical = _median([b - a for a, b in zip(starts, starts[1:])], 3.5)
    pause = max(2.0 * typical, typical + 4.0)
    blocks: list[list[dict]] = []
    current: list[dict] = []
    previous = None
    for line in lines:
        if not line["text"]:
            if current:
                blocks.append(current)
            current, previous = [], None
            continue
        if current and previous is not None and line["t"] - previous > pause:
            blocks.append(current)
            current = []
        current.append({**line, "norm": norm_line(line["text"])})
        previous = line["t"]
    if current:
        blocks.append(current)

    # Which lines the song sings again somewhere else. A line repeated only
    # right after itself ("oh oh / oh oh") does not count.
    flat = [line for block in blocks for line in block]
    for i, line in enumerate(flat):
        line["again"] = any(j not in (i - 1, i, i + 1) and similar(line["norm"], other["norm"]) >= 0.8
                            for j, other in enumerate(flat) if len(line["norm"]) > 2)
    refined: list[list[dict]] = []
    for block in blocks:
        # Split into runs of repeated / fresh lines, keeping any run of one
        # line with its neighbour: a single echoed word is not a new section.
        runs: list[list[dict]] = []
        for line in block:
            if runs and runs[-1][-1]["again"] == line["again"]:
                runs[-1].append(line)
            else:
                runs.append([line])
        merged: list[list[dict]] = []
        for run in runs:
            if merged and (len(run) < 2 or len(merged[-1]) < 2):
                merged[-1].extend(run)
            else:
                merged.append(list(run))
        refined.extend(merged)
    return refined


def _block_similarity(a: list[dict], b: list[dict]) -> float:
    short, long_ = (a, b) if len(a) <= len(b) else (b, a)
    if not short:
        return 0.0
    hits = sum(max((similar(x["norm"], y["norm"]) for y in long_), default=0.0) >= 0.75 for x in short)
    return hits / len(short) * min(1.0, len(short) / len(long_) + 0.35)


def _clusters(blocks: list[list[dict]]) -> list[int]:
    """A cluster id per block: blocks that are the same words share one."""
    ids = list(range(len(blocks)))

    def root(i: int) -> int:
        while ids[i] != i:
            ids[i] = ids[ids[i]]
            i = ids[i]
        return i

    for i in range(len(blocks)):
        for j in range(i + 1, len(blocks)):
            if _block_similarity(blocks[i], blocks[j]) >= 0.6:
                ids[root(j)] = root(i)
    return [root(i) for i in range(len(blocks))]


# --------------------------------------------------------------------------
# Sections
# --------------------------------------------------------------------------
def sections(lines: Iterable[dict], duration: float | None = None) -> list[dict]:
    """[{start, end, label, confidence}] from synced lines, unsnapped.

    Fewer than four sung lines is not enough to call anything a chorus, so
    that gives nothing rather than a guess.
    """
    lines = _sung(lines)
    sung = [line for line in lines if line["text"]]
    if len(sung) < 4:
        return []
    duration = _number(duration)
    last_line = sung[-1]["t"]
    if duration is None or duration <= last_line:
        duration = last_line + line_cap(lines)
    blocks = _blocks(lines)
    if not blocks:
        return []
    cluster = _clusters(blocks)

    # The chorus: the multi-line cluster sung most often, then the biggest.
    members: dict[int, list[int]] = {}
    for index, cid in enumerate(cluster):
        members.setdefault(cid, []).append(index)
    chorus_id, best = None, (0, 0)
    for cid, indexes in members.items():
        size = sum(len(blocks[i]) for i in indexes) / len(indexes)
        if len(indexes) >= 2 and size >= 2:
            rank = (len(indexes), size)
            if rank > best:
                chorus_id, best = cid, rank
    choruses = members.get(chorus_id, []) if chorus_id is not None else []

    cap = line_cap(lines)
    spans: list[dict] = []
    for index, block in enumerate(blocks):
        start = block[0]["t"]
        following = blocks[index + 1][0]["t"] if index + 1 < len(blocks) else None
        # The block runs until the next one starts, unless the song leaves
        # a real gap: then until its last line has had time to be sung.
        sung_until = min(block[-1]["t"] + cap, following if following is not None else duration)
        # A blank marker after the block is the song saying the words stop.
        marker = next((line["t"] for line in lines
                       if not line["text"] and block[-1]["t"] < line["t"] <= (following or duration)), None)
        if marker is not None:
            sung_until = min(sung_until, marker)
        end = following if following is not None and following - sung_until < INSTRUMENTAL_GAP else sung_until
        if index in choruses:
            label = "chorus"
            confidence = min(0.9, 0.55 + 0.1 * (len(choruses) - 1))
        else:
            label = "verse"
            confidence = 0.45 if len(members[cluster[index]]) > 1 else 0.5
            before = sum(1 for c in choruses if c < index)
            after = sum(1 for c in choruses if c > index)
            if (before >= 2 and after >= 1 and len(members[cluster[index]]) == 1):
                label, confidence = "bridge", 0.5
        spans.append({"start": start, "end": max(end, start + 0.5), "label": label,
                      "confidence": confidence})

    # Two blocks of the same kind back to back are one section: a verse
    # that pauses for breath, or a chorus sung twice.
    merged: list[dict] = []
    for span in spans:
        last = merged[-1] if merged else None
        if last and last["label"] == span["label"] and span["start"] - last["end"] < INSTRUMENTAL_GAP:
            last["end"] = span["end"]
            last["confidence"] = max(last["confidence"], span["confidence"])
        else:
            merged.append(span)
    spans = merged

    out: list[dict] = []
    first = spans[0]["start"]
    if first >= EDGE_MIN:
        out.append({"start": 0.0, "end": first, "label": "intro", "confidence": 0.75})
    elif first > 0:
        spans[0]["start"] = 0.0
    for index, span in enumerate(spans):
        out.append(span)
        following = spans[index + 1]["start"] if index + 1 < len(spans) else None
        if following is not None and following - span["end"] > 0.05:
            if following - span["end"] >= INSTRUMENTAL_GAP:
                out.append({"start": span["end"], "end": following, "label": "instrumental",
                            "confidence": 0.6})
            else:
                span["end"] = following
    tail = out[-1]["end"]
    if duration - tail >= EDGE_MIN:
        out.append({"start": tail, "end": duration, "label": "outro", "confidence": 0.7})
    elif duration > tail:
        out[-1]["end"] = duration
    return [{**s, "start": round(s["start"], 2), "end": round(s["end"], 2),
             "confidence": round(s["confidence"], 2)} for s in out]


def snap(found: list[dict], track: Any = None, profile: dict | None = None) -> list[dict]:
    """Pull each inner boundary onto the bar grid: the nearest eight-bar
    phrase line within a bar, else the nearest downbeat. Without a trusted
    grid, onto an acoustic boundary within two seconds. Never reorders."""
    if len(found) < 2:
        return [dict(s) for s in found]
    from . import structure
    grid = structure.phrase_grid(track) if track is not None else None
    marks = [_number(p.get("at")) for p in (profile or {}).get("boundaries", [])
             if _number(p.get("confidence"), 0.0) >= 0.18]
    marks = [m for m in marks if m is not None]

    def moved(at: float) -> float:
        if grid is not None:
            start, phrase = grid
            bar = phrase / 8
            line = start + round((at - start) / phrase) * phrase
            if abs(line - at) <= bar and line >= 0:
                return line
            downbeat = start + round((at - start) / bar) * bar
            return downbeat if downbeat >= 0 else at
        near = min(marks, key=lambda m: abs(m - at), default=None)
        return near if near is not None and abs(near - at) <= 2.0 else at

    out = [dict(s) for s in found]
    for index in range(1, len(out)):
        at = moved(out[index]["start"])
        low = out[index - 1]["start"] + 0.5
        high = out[index]["end"] - 0.5
        if low <= at <= high:
            out[index]["start"] = round(at, 2)
            out[index - 1]["end"] = round(at, 2)
    return out


def build(lines: Iterable[dict], duration: float | None = None, track: Any = None,
          profile: dict | None = None) -> tuple[list[dict], list[list[float]]]:
    """(sections snapped to the grid, vocal spans) for one song."""
    lines = _sung(lines)
    return snap(sections(lines, duration), track, profile), vocal_spans(lines, duration)


# --------------------------------------------------------------------------
# Reading them
# --------------------------------------------------------------------------
def lyric_map(track: Any) -> dict:
    """The lyric facts a prepared track carries (see lyrics.attach), or {}."""
    value = track.get("lyric_map") if hasattr(track, "get") else None
    return value if isinstance(value, dict) else {}


def at(found: list[dict] | None, seconds: float) -> dict | None:
    for span in found or []:
        if span["start"] <= seconds < span["end"]:
            return span
    return None


def last_chorus_end(found: list[dict] | None) -> float | None:
    ends = [s["end"] for s in found or [] if s.get("label") == "chorus"]
    return max(ends) if ends else None


def first_line(track: Any) -> float | None:
    """When the first sung line starts, if the synced lyrics say."""
    value = _number(lyric_map(track).get("first_line"))
    return value if value is not None and value >= 0 else None


def exit_score(found: list[dict] | None, blend_start: float, beat: float = 0.5) -> float:
    """-1..1 for a blend that starts at `blend_start` in the outgoing record.

    Leaving after the last chorus (or in the outro) is right; starting the
    fade in the middle of a chorus is the one thing never to do; a section
    line anywhere else is fine. 0 when there are no sections.
    """
    if not found:
        return 0.0
    bar = 4 * max(0.2, beat)
    last = last_chorus_end(found)
    here = at(found, blend_start)
    if here and here["label"] == "chorus" and here["end"] - blend_start > bar and blend_start - here["start"] > beat:
        return -1.0
    if last is not None and blend_start >= last - beat:
        return 1.0
    if here and here["label"] == "outro":
        return 1.0
    if any(abs(s["start"] - blend_start) <= beat for s in found):
        return 0.4
    if here and here["label"] in ("verse", "bridge") and here["end"] - blend_start > bar:
        return -0.3
    return 0.0


def entry_score(found: list[dict] | None, cue: float, beat: float = 0.5) -> float:
    """-0.5..1 for bringing a record in at `cue`: on the first sung section
    after the intro, or on a section's downbeat, rather than mid-line."""
    if not found:
        return 0.0
    for index, span in enumerate(found):
        if abs(span["start"] - cue) <= beat:
            if span["label"] in ("verse", "chorus") and (index == 0 or found[index - 1]["label"] in ("intro", "instrumental")):
                return 1.0
            return 0.6 if span["label"] in ("verse", "chorus", "bridge") else 0.3
    here = at(found, cue)
    if here and here["label"] in ("verse", "chorus", "bridge"):
        return -0.5
    return 0.0


def exit_points(found: list[dict] | None) -> list[float]:
    """Where a DJ would start leaving: after the last chorus, and the outro."""
    points = []
    last = last_chorus_end(found)
    if last is not None:
        points.append(last)
    points += [s["start"] for s in found or [] if s["label"] == "outro"]
    return sorted(set(round(p, 2) for p in points))


def entry_points(found: list[dict] | None) -> list[float]:
    """Section starts worth entering on: the first sung section and each chorus."""
    points = []
    for index, span in enumerate(found or []):
        if span["label"] in ("verse", "chorus") and (index == 0 or found[index - 1]["label"] in ("intro", "instrumental")):
            points.append(span["start"])
        elif span["label"] == "chorus":
            points.append(span["start"])
    return sorted(set(round(p, 2) for p in points))


def overlay_vocals(profile: dict, spans: list | None) -> dict:
    """A copy of an acoustic profile whose vocal evidence is the lyrics.

    Synced lines say exactly when somebody sings, which a spectral guess or
    a stem's leakage cannot. Between lines the value is low rather than zero:
    ad-libs and backing vocals are not in the lyrics.
    """
    bins = profile.get("bins") if isinstance(profile, dict) else None
    if not spans or not bins:
        return profile
    out = dict(profile)
    out["bins"] = [{**b, "vocal": 0.9 if any(float(a) < b["end"] and float(e) > b["at"] for a, e in spans) else 0.05}
                   for b in bins]
    out["vocal_source"] = "synced_lyrics"
    return out
