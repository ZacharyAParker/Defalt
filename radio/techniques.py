"""Transition techniques: the tricks a DJ reaches for beyond a crossfade.

The presets in transitions.py are all the same move -- two faders and a bass
swap -- in different clothes, which is why the station sounded the same every
time the same kind of pair came up. A technique is a different move: an echo
that rings out over the next record's drop, a roll that stutters into its
first beat, a turntable brake, a spinback.

Each technique is a function of the pair. It gets the beat, where the
incoming record's first downbeat lands (`one`), how much of the outgoing
record is free before that, the keys, the energy step, the vocal evidence,
and returns a shape: two volume curves (baked into the ordinary gain
envelopes, so ducking still multiplies in) plus automation lanes and events
for each deck. Times in a shape are seconds from the start of the overlap;
the schedule turns them into station time when it seals.

Choosing one is `select`: every technique that is *technically* safe for the
pair gets a weight from the context, the creativity setting decides how much
of the probability goes to effects at all, recently used techniques are
penalised, and a seed derived from the two records makes the choice stable,
so a preview never flickers between two answers.
"""
from __future__ import annotations

import hashlib
import json
import math
import random
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from . import config

SILENT = 0.0001

# Lanes rest here. Every lane a technique writes ends at its resting value,
# because a deck keeps a lane's last value once the curve is done.
NEUTRAL = {"gain": 1.0, "level": 1.0, "low": 0.5, "mid": 0.5, "high": 0.5,
           "sweep": 0.0, "echo_send": 0.0, "echo_feedback": 0.3, "reverb_send": 0.0,
           "stem_drums": 1.0, "stem_bass": 1.0, "stem_harmonic": 1.0, "stem_vocals": 1.0}

# Engine ranges, per docs/TRANSITIONS.md "Automation lanes (protocol)".
RANGES = {"gain": (0.0, 2.0), "level": (0.0, 1.0), "low": (0.0, 1.0), "mid": (0.0, 1.0),
          "high": (0.0, 1.0), "sweep": (-1.0, 1.0), "echo_send": (0.0, 1.0),
          "echo_feedback": (0.0, 1.0), "echo_beats": (0.0, 16.0), "reverb_send": (0.0, 1.0),
          "rate": (-4.0, 4.0), "stem_drums": (0.0, 2.0), "stem_bass": (0.0, 2.0),
          "stem_harmonic": (0.0, 2.0), "stem_vocals": (0.0, 2.0)}

LEGACY = ("fade", "rise", "blend", "wave", "melt", "slam")


@dataclass
class Context:
    """What a technique may know about one boundary. Seconds are wall time."""
    overlap: float
    beat: float | None = None           # outgoing beat, at its playing rate
    grid: bool = False                  # both grids trusted and the beats aligned
    one: float = 0.0                    # incoming first downbeat, from overlap start
    lead_room: float = 0.0              # outgoing time free before `one`
    out_rate: float = 1.0               # deck speed of the outgoing tail
    in_rate: float = 1.0
    harmonic: bool | None = None        # keys compatible; None when unknown
    energy_step: float | None = None    # incoming minus outgoing, 0..1 scale
    tempo_gap: float = 0.0              # after tempo matching, octave-invariant
    tempo_known: bool = False
    matched: bool = False               # within the compatible tempo tolerance
    out_vocal: float | None = None
    in_vocal: float | None = None
    stems: bool = False                 # both records have cached separations
    speech: bool = False                # a host is already on the clock here
    base: str = "fade"
    genre: str = ""
    danceability: float | None = None
    hour: int | None = None
    pace: str = ""

    @property
    def b(self) -> float:
        """A beat to quantize to: the real one, or half a second without."""
        return self.beat if self.beat and self.beat > 0 else 0.5


@dataclass
class Technique:
    name: str
    label: str
    describe: str                   # what the hosts may say it was
    build: Callable[[Context], dict] | None
    flashy: bool = True
    needs_stems: bool = False
    needs_grid: bool = False
    needs_tempo: bool = False       # a measured tempo to time the effect
    needs_matched: bool = False     # long blends need tempos that agree
    stacks: bool = False            # both records full-band at once for a while
    lead_beats: float = 0.0         # outgoing beats needed before `one`
    min_tail_beats: float = 0.0     # overlap needed after `one`
    weight: float = 1.0


# --------------------------------------------------------------------------
# Small builders
# --------------------------------------------------------------------------
def _points(*pairs) -> list[list[float]]:
    out: list[list[float]] = []
    for t, v in pairs:
        t = round(float(t), 4)
        if out and t < out[-1][0]:
            t = out[-1][0]
        out.append([t, round(float(v), 4)])
    return out


def _cut_in(ctx: Context) -> list[list[float]]:
    """The incoming record lands on its one. Silent before it, if later."""
    one = ctx.one
    if one <= 0.02:
        return _points((0, SILENT), (0.015, 1.0), (ctx.overlap, 1.0))
    return _points((0, SILENT), (one, SILENT), (one + 0.015, 1.0), (ctx.overlap, 1.0))


def _held_out(until: float, ctx: Context, fade: float = 0.0) -> list[list[float]]:
    """Outgoing envelope: up until `until`, then gone (optionally over `fade`)."""
    until = max(0.0, min(until, ctx.overlap))
    gone = min(ctx.overlap, until + max(0.02, fade))
    return _points((0, 1.0), (until, 1.0), (gone, SILENT), (ctx.overlap, SILENT))


def _cut_level(ctx: Context, at: float, ramp: float = 0.02) -> list[list[float]]:
    """The outgoing fader closing at `at`, and resting again as the deck ends."""
    end = ctx.overlap
    at = min(at, end - 0.03)
    return _points((at - 0.001, 1.0), (at + ramp, 0.0), (end - 0.01, 0.0), (end, 1.0))


def _reset(points: list[list[float]], lane: str, end: float) -> list[list[float]]:
    """End a lane at rest by `end` (the deck's last moment)."""
    rest = NEUTRAL.get(lane)
    if rest is None or not points:
        return points
    last = points[-1]
    if abs(last[1] - rest) > 1e-6:
        if last[0] < end - 0.01:
            points.append([round(end - 0.01, 4), last[1]])
        points.append([round(max(end, last[0]), 4), rest])
    return points


def _q(ctx: Context, t: float) -> float:
    """Snap a moment to the beat grid anchored on the incoming one."""
    b = ctx.b
    return ctx.one + round((t - ctx.one) / b) * b


def _bar_after(ctx: Context, fraction: float, minimum_beats: float = 4) -> float:
    """A bar line about `fraction` of the way from the one to the end."""
    b = ctx.b
    span = max(0.0, ctx.overlap - ctx.one)
    bars = max(minimum_beats / 4, math.floor(span * fraction / (4 * b)))
    at = ctx.one + bars * 4 * b
    if at > ctx.overlap - 2 * b:
        at = ctx.one + max(1, math.floor((span - 2 * b) / b)) * b
    return at


# --------------------------------------------------------------------------
# The techniques
# --------------------------------------------------------------------------
def echo_out(ctx: Context) -> dict:
    """Catch the last beat in the echo, close the fader on the one, and let the
    repeats ring over the incoming drop while a high-pass thins them away."""
    b, one, end = ctx.b, ctx.one, ctx.overlap
    clash = ctx.harmonic is False
    tail = min((2 if clash else 4) * b, end - one - 0.5 * b)
    delay = 0.5 if clash else 0.75
    feedback = 0.4 if clash else 0.55
    out = {
        "echo_beats": _points((one - 2 * b, delay), (end, delay)),
        "echo_send": _points((one - b, 0.0), (one - 0.25 * b, 0.6), (one, 0.6), (one + 0.02, 0.0)),
        "echo_feedback": _points((one - b, 0.3), (one - 0.25 * b, feedback),
                                 (one + tail * 0.5, feedback), (one + tail, 0.0)),
        "level": _cut_level(ctx, one, ramp=b / 8),
        "sweep": _points((one, 0.0), (one + tail, 0.75 if clash else 0.55)),
    }
    return {"out_volume": _held_out(one + tail, ctx, fade=0.5 * b), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": [],
            "window": (one - b, one + tail),
            "note": f"echo out, {tail / b:.0f}-beat tail" + (" kept short for the key clash" if clash else "")}


def loop_roll(ctx: Context) -> dict:
    """Rolls on the outgoing record halving towards the incoming downbeat:
    4, 2, 1, then half a beat, with a high-pass rising and the bass leaving."""
    b, one = ctx.b, ctx.one
    if ctx.lead_room >= 16 * b:
        steps = ((4, -16, -8), (2, -8, -4), (1, -4, -2), (0.5, -2, 0))
    else:
        steps = ((2, -8, -4), (1, -4, -2), (0.5, -2, -1), (0.25, -1, 0))
    lead = -steps[0][1] * b
    events = [{"type": "roll", "deck": "out", "at": round(one + a * b, 4),
               "length_seconds": round(length * b, 4), "until": round(one + z * b, 4)}
              for length, a, z in steps]
    out = {
        "sweep": _points((one - lead, 0.0), (one - 0.02, 0.5)),
        "low": _points((one - lead / 2, 0.5), (one - 0.02, 0.15)),
        "level": _cut_level(ctx, one),
    }
    return {"out_volume": _held_out(one, ctx), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": events,
            "window": (one - lead, one + 0.1),
            "note": f"loop roll over {lead / b:.0f} beats"}


def brake(ctx: Context) -> dict:
    """Tape-stop the outgoing record over a beat or two; the next slams in on
    the one."""
    b, one, rate = ctx.b, ctx.one, ctx.out_rate
    length = 2 * b if b < 0.5 else b
    start = one - length
    curve = [(start + length * x, rate * (1 - x) ** 1.6) for x in (0, 0.25, 0.5, 0.75)]
    out = {"rate": _points(*curve, (one, 0.0), (ctx.overlap - 0.01, 0.0), (ctx.overlap, rate)),
           "level": _cut_level(ctx, one)}
    return {"out_volume": _held_out(one, ctx), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": [],
            "window": (start, one + 0.1), "note": f"brake over {length / b:.0f} beats"}


def spinback(ctx: Context) -> dict:
    """A backwards burst on the outgoing record, smeared into the reverb."""
    b, one, rate = ctx.b, ctx.one, ctx.out_rate
    spin = max(0.6, 1.5 * b)
    start = one - spin
    out = {"rate": _points((start, rate), (start + 0.06, -3.0), (one - spin * 0.35, -1.2),
                           (one, -0.3), (one + 0.02, 0.0), (ctx.overlap - 0.01, 0.0),
                           (ctx.overlap, rate)),
           "level": _points((one - spin * 0.5, 1.0), (one, 0.0), (ctx.overlap - 0.01, 0.0),
                            (ctx.overlap, 1.0)),
           "reverb_send": _points((start, 0.0), (start + 0.1, 0.35), (one, 0.35), (one + 0.05, 0.0))}
    return {"out_volume": _held_out(one, ctx), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": [],
            "window": (start, one + 0.5), "note": "spinback"}


def echo_freeze(ctx: Context) -> dict:
    """Freeze the outgoing record's last beat in the echo and filter it away
    under the incoming one."""
    b, one, end = ctx.b, ctx.one, ctx.overlap
    clash = ctx.harmonic is False
    beats = 1.0 if (not clash and b <= 0.95) else 0.5
    hold = (1 if clash else 2) * b
    fade = (1 if clash else 2) * b
    hold = min(hold, max(0.0, (end - one) * 0.45))
    fade = min(fade, max(0.1, end - one - hold - 0.05))
    out = {
        "echo_beats": _points((one - 2 * b, beats), (end, beats)),
        "echo_send": _points((one - beats * b - 0.25 * b, 0.0), (one - beats * b, 0.9), (one, 0.9),
                             (one + 0.02, 0.0)),
        "echo_feedback": _points((one - beats * b, 0.3), (one, 0.97), (one + hold, 0.97),
                                 (one + hold + fade, 0.0)),
        "level": _cut_level(ctx, one),
        "sweep": _points((one, 0.0), (one + hold, 0.3), (one + hold + fade, 0.8)),
    }
    return {"out_volume": _held_out(one + hold + fade, ctx, fade=0.3 * b), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": [],
            "window": (one - beats * b - 0.25 * b, one + hold + fade),
            "note": f"echo freeze, {beats:g}-beat loop"}


def reverb_wash(ctx: Context) -> dict:
    """The outgoing record washes into reverb as a high-pass climbs; the
    incoming one emerges from under a low-pass."""
    end = ctx.overlap
    arrive = max(0.0, _q(ctx, end * 0.15))
    gone = max(arrive + ctx.b, _q(ctx, end * 0.6))
    gone = min(gone, end - 0.5)
    # The dry record and the incoming one trade places at equal power; the
    # send is post-fader, so what went into the reverb keeps ringing after.
    fall = [(arrive + (gone - arrive) * i / 8, math.cos(i / 8 * math.pi / 2)) for i in range(9)]
    out = {"reverb_send": _points((0, 0.0), (arrive, 0.5), (gone, 0.8), (gone + 0.05, 0.0)),
           "sweep": _points((0, 0.0), (gone, 0.7)),
           "level": _points(*fall, (end - 0.01, 0.0), (end, 1.0))}
    rise = [(arrive + (gone - arrive) * i / 8, math.sin(i / 8 * math.pi / 2)) for i in range(9)]
    in_volume = _points((0, SILENT), *rise, (end, 1.0)) if arrive > 0 else _points(*rise, (end, 1.0))
    if in_volume[0][0] > 0:
        in_volume.insert(0, [0.0, SILENT])
    lanes_in = {"sweep": _points((0, -0.6), (arrive, -0.6), (gone, 0.0))}
    return {"out_volume": _points((0, 1.0), (max(gone, end - ctx.b), 1.0), (end, SILENT)),
            "in_volume": in_volume, "lanes": {"out": out, "in": lanes_in}, "events": [],
            "window": (0.0, end), "note": "reverb wash"}


def stem_swap(ctx: Context) -> dict:
    """Incoming drums and bass under the outgoing vocals for a phrase, then the
    vocals and harmony hand over on a bar line."""
    b, one, end = ctx.b, ctx.one, ctx.overlap
    swap = _bar_after(ctx, 0.5)
    out = {"stem_drums": _points((one - 0.01, 1.0), (one + 0.02, 0.0)),
           "stem_bass": _points((one - 0.01, 1.0), (one + 0.02, 0.0)),
           "stem_vocals": _points((swap, 1.0), (swap + b, 0.0)),
           "stem_harmonic": _points((swap, 1.0), (swap + b, 0.0))}
    lanes_in = {"stem_vocals": _points((0, 0.0), (swap, 0.0), (swap + b, 1.0)),
                "stem_harmonic": _points((0, 0.0), (swap, 0.0), (swap + b, 1.0))}
    return {"out_volume": _held_out(swap + b, ctx, fade=b), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": lanes_in}, "events": [],
            "window": (one, swap + b), "requires": ["stems"], "note": "stem swap on the bar"}


def acapella_intro(ctx: Context) -> dict:
    """The incoming vocal over the outgoing instrumental, then the rest of the
    incoming record drops in on a bar line."""
    b, one = ctx.b, ctx.one
    swap = _bar_after(ctx, 0.5)
    out = {"stem_vocals": _points((max(0.0, one - b), 1.0), (one, 0.0))}
    lanes_in = {name: _points((0, 0.0), (swap - 0.02, 0.0), (swap, 1.0))
                for name in ("stem_drums", "stem_bass", "stem_harmonic")}
    return {"out_volume": _held_out(swap, ctx, fade=0.05), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": lanes_in}, "events": [],
            "window": (one, swap), "requires": ["stems"], "note": "acapella intro"}


def filter_ride(ctx: Context) -> dict:
    """A long filter ride with the bass swapped on a bar line. Rising energy
    opens the incoming record from a low-pass while the outgoing one thins
    upwards; falling energy does the opposite."""
    b, end = ctx.b, ctx.overlap
    swap = _bar_after(ctx, 0.5)
    rising = ctx.energy_step is None or ctx.energy_step >= -0.05
    settle = max(swap + b, end - b)
    if rising:
        lanes_in = {"sweep": _points((0, -0.8), (swap, -0.25), (settle, 0.0))}
        out_sweep = _points((0, 0.0), (end - 0.02, 0.65))
    else:
        lanes_in = {"sweep": _points((0, 0.6), (swap, 0.2), (settle, 0.0))}
        out_sweep = _points((0, 0.0), (end - 0.02, -0.8))
    ramp = b / 8
    out = {"sweep": out_sweep, "low": _points((swap - ramp, 0.5), (swap + ramp, 0.0))}
    lanes_in["low"] = _points((0, 0.0), (swap - ramp, 0.0), (swap + ramp, 0.5))
    fade_from = max(swap + b, end - 4 * b)
    out_volume = _points((0, 1.0), (fade_from, 1.0),
                         *[(fade_from + (end - fade_from) * i / 6, max(SILENT, math.cos(i / 6 * math.pi / 2)))
                           for i in range(1, 7)])
    in_volume = _points(*[(2 * b * i / 6, max(SILENT, math.sin(i / 6 * math.pi / 2))) for i in range(7)],
                        (end, 1.0))
    return {"out_volume": out_volume, "in_volume": in_volume,
            "lanes": {"out": out, "in": lanes_in}, "events": [],
            "window": (0.0, end), "note": ("rising" if rising else "falling") + " filter ride, bass swap on the bar"}


def silence_punch(ctx: Context) -> dict:
    """The outgoing record gated to silence for a beat, so the drop lands
    into nothing."""
    b, one = ctx.b, ctx.one
    gap = b if b <= 0.55 else 0.5 * b
    out = {"level": _cut_level(ctx, one - gap, ramp=0.01),
           "low": _points((one - gap - 2 * b, 0.5), (one - gap, 0.2))}
    return {"out_volume": _held_out(one - gap + 0.01, ctx), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": [],
            "window": (one - gap - 2 * b, one), "note": f"{gap / b:g}-beat silence before the drop"}


def drop_swap(ctx: Context) -> dict:
    """A hard cut exactly on the downbeat, the outgoing bass already gone."""
    b, one = ctx.b, ctx.one
    out = {"low": _points((one - 4 * b, 0.5), (one - b, 0.1)),
           "level": _cut_level(ctx, one, ramp=0.01)}
    return {"out_volume": _held_out(one, ctx), "in_volume": _cut_in(ctx),
            "lanes": {"out": out, "in": {}}, "events": [],
            "window": (one - 4 * b, one), "note": "drop swap on the downbeat"}


TECHNIQUES: dict[str, Technique] = {t.name: t for t in (
    Technique("fade", "Fade", "a plain crossfade", None, flashy=False),
    Technique("rise", "Rise", "a filtered rise", None, flashy=False),
    Technique("blend", "Blend", "a long blend", None, flashy=False),
    Technique("wave", "Wave", "an underwater blend", None, flashy=False),
    Technique("melt", "Melt", "a thin-out blend", None, flashy=False),
    Technique("slam", "Slam", "a hard cut", None, flashy=False),
    Technique("echo_out", "Echo out",
              "an echo-out: the old record's last beat echoed away as the new one dropped",
              echo_out, needs_tempo=True, stacks=True, lead_beats=1, min_tail_beats=2.5),
    Technique("loop_roll", "Loop roll",
              "a loop roll: the old record stuttered in shrinking loops into the new one's first beat",
              loop_roll, needs_grid=True, needs_tempo=True, lead_beats=8, weight=0.9),
    Technique("brake", "Brake",
              "a brake: the old record slowed to a stop like a turntable powering down",
              brake, needs_tempo=True, lead_beats=2, weight=0.7),
    Technique("spinback", "Spinback",
              "a spinback: the old record was spun backwards out of the mix",
              spinback, needs_tempo=True, lead_beats=1.5, weight=0.45),
    Technique("echo_freeze", "Echo freeze",
              "an echo freeze: the old record's last beat froze and was filtered away",
              echo_freeze, needs_tempo=True, stacks=True, lead_beats=1.5, min_tail_beats=2.5, weight=0.8),
    Technique("reverb_wash", "Reverb wash",
              "a reverb wash: the old record dissolved into reverb",
              reverb_wash, stacks=True, min_tail_beats=6, weight=0.9),
    Technique("stem_swap", "Stem swap",
              "a stem swap: the new drums and bass played under the old vocals before the vocals handed over",
              stem_swap, needs_stems=True, needs_grid=True, needs_matched=True, min_tail_beats=8, weight=1.2),
    Technique("acapella_intro", "Acapella intro",
              "an acapella intro: the new vocals came in over the old instrumental",
              acapella_intro, needs_stems=True, needs_grid=True, needs_matched=True, min_tail_beats=8),
    Technique("filter_ride", "Filter ride", "a long filter ride",
              filter_ride, flashy=False, needs_matched=True, stacks=True, min_tail_beats=8, weight=1.1),
    Technique("silence_punch", "Silence punch",
              "a silence punch: a beat of silence right before the new record dropped",
              silence_punch, needs_grid=True, needs_tempo=True, lead_beats=3, weight=0.6),
    Technique("drop_swap", "Drop swap",
              "a drop swap: a hard cut on the downbeat with the bass already gone",
              drop_swap, needs_grid=True, needs_tempo=True, lead_beats=4),
)}

FX = tuple(name for name, t in TECHNIQUES.items() if t.build is not None)
FLASHY = tuple(name for name, t in TECHNIQUES.items() if t.flashy)


# --------------------------------------------------------------------------
# Choosing
# --------------------------------------------------------------------------
def _setting(key: str, default):
    return config.station.get(key, default)


def _number(value, fallback=0.0):
    try:
        value = float(value)
    except (TypeError, ValueError):
        return fallback
    return value if math.isfinite(value) else fallback


def eligible(name: str, ctx: Context) -> str:
    """Why `name` cannot play this boundary, or "" when it can.

    These are the rules that make a transition sound broken rather than
    merely unsuitable, so they are never weighed against anything.
    """
    technique = TECHNIQUES.get(name)
    if technique is None:
        return "unknown technique"
    if technique.build is None:
        return "" if name == ctx.base else "not the planned blend"
    if ctx.speech:
        return "a host is talking over this mix"
    if technique.needs_stems and not ctx.stems:
        return "needs separated stems on both records"
    if technique.needs_grid and not ctx.grid:
        return "needs aligned beat grids"
    if technique.needs_tempo and not ctx.tempo_known:
        return "needs a measured tempo"
    if technique.needs_matched and not ctx.matched:
        return "tempos too far apart for a long blend"
    b = ctx.b
    if technique.lead_beats and ctx.lead_room < technique.lead_beats * b + 0.05:
        return "not enough of the outgoing record before the drop"
    if technique.min_tail_beats and ctx.overlap - ctx.one < technique.min_tail_beats * b:
        return "overlap too short"
    if name == "acapella_intro" and ctx.harmonic is not True:
        return "a vocal over another record needs compatible keys"
    if name == "acapella_intro" and ctx.in_vocal is not None and ctx.in_vocal < 0.2:
        return "no incoming vocal to feature"
    if technique.stacks and not technique.needs_stems:
        # Never stack two singers: an echo of a departing vocal under a new
        # one is exactly the clash a DJ avoids.
        if (ctx.out_vocal is not None and ctx.in_vocal is not None
                and ctx.out_vocal > 0.3 and ctx.in_vocal > 0.3):
            return "both records have vocals here"
    return ""


ELECTRONIC = ("house", "techno", "edm", "electronic", "dance", "trance", "dubstep", "drum and bass",
              "dnb", "garage", "hip hop", "hip-hop", "rap", "trap", "breakbeat", "electro", "disco")
ACOUSTIC = ("acoustic", "folk", "jazz", "classical", "ambient", "singer-songwriter", "country",
            "soul", "blues", "lofi", "lo-fi", "bossa")


def suitability(name: str, ctx: Context) -> float:
    """How well an eligible technique fits this pair, around 1.0."""
    technique = TECHNIQUES[name]
    weight = technique.weight
    step = ctx.energy_step
    if step is not None and step > 0.1:          # building up: rolls and risers
        weight *= {"loop_roll": 2.2, "silence_punch": 1.8, "drop_swap": 1.6, "filter_ride": 1.4,
                   "stem_swap": 1.2, "echo_out": 0.6, "reverb_wash": 0.4, "brake": 0.7,
                   "echo_freeze": 0.8, "spinback": 0.8}.get(name, 1.0)
    elif step is not None and step < -0.1:       # cooling down: let it ring
        weight *= {"echo_out": 2.0, "reverb_wash": 2.2, "echo_freeze": 1.6, "brake": 1.3,
                   "filter_ride": 1.1, "loop_roll": 0.4, "silence_punch": 0.5,
                   "drop_swap": 0.6}.get(name, 1.0)
    if ctx.tempo_known and not ctx.matched:      # a big tempo gap: cut, don't blend
        weight *= {"brake": 2.0, "spinback": 1.6, "echo_out": 1.8, "drop_swap": 1.5,
                   "echo_freeze": 1.5, "reverb_wash": 1.2}.get(name, 1.0)
    if ctx.harmonic is False:                    # clashing keys: short tails, clean cuts
        weight *= {"echo_out": 0.7, "echo_freeze": 0.6, "reverb_wash": 0.7, "filter_ride": 0.6,
                   "brake": 1.3, "drop_swap": 1.3, "silence_punch": 1.3, "loop_roll": 1.2,
                   "spinback": 1.2}.get(name, 1.0)
    elif ctx.harmonic is True:
        weight *= {"filter_ride": 1.3, "stem_swap": 1.3, "acapella_intro": 1.5,
                   "echo_out": 1.1}.get(name, 1.0)
    genre = (ctx.genre or "").lower()
    if any(word in genre for word in ELECTRONIC):
        weight *= {"loop_roll": 1.4, "drop_swap": 1.4, "silence_punch": 1.3, "stem_swap": 1.4,
                   "filter_ride": 1.3}.get(name, 1.15)
    elif any(word in genre for word in ACOUSTIC):
        weight *= {"echo_out": 0.9, "reverb_wash": 1.0, "filter_ride": 0.8}.get(name, 0.55)
    if ctx.danceability is not None and ctx.danceability > 0.6 and name in ("loop_roll", "drop_swap"):
        weight *= 1.2
    if technique.stacks and (ctx.out_vocal is None or ctx.in_vocal is None):
        weight *= 0.7          # unknown vocals: prefer moves that never overlap
    return max(0.0, weight)


def creativity(ctx: Context) -> float:
    """The creativity setting, eased for the hour and the listener's brief.

    A missing setting means an old configuration: keep the smooth radio it
    was written for. The shipped station.yaml turns it up.
    """
    value = max(0.0, min(1.0, _number(_setting("transitions.creativity", 0.0))))
    if ctx.hour is not None and 0 <= ctx.hour < 6:
        value *= 0.7               # the small hours want smoother radio
    pace = (ctx.pace or "").lower()
    if pace == "slow":
        value *= 0.6
    elif pace == "fast":
        value = min(1.0, value * 1.25)
    return value


def banned(name: str) -> bool:
    return name in FX and not bool(_setting(f"transitions.allow_{name}", True))


def seed_for(out_key: str, in_key: str) -> int:
    digest = hashlib.sha1(f"{out_key}|{in_key}".encode("utf-8")).hexdigest()
    return int(digest[:12], 16)


def select(ctx: Context, recent: list[str] | tuple[str, ...] = (), seed: int = 0) -> tuple[str, str]:
    """(technique, why). The base preset whenever nothing else should play."""
    pinned = str(_setting("transitions.preset", "auto") or "auto").lower()
    if pinned in FX:
        why = eligible(pinned, ctx)
        if not why:
            return pinned, "selected technique"
        fallback_note = f"{pinned} not possible here ({why})"
    else:
        fallback_note = ""
    amount = creativity(ctx)
    if amount <= 1e-3 or ctx.speech:
        why = "talk-over keeps it clean" if ctx.speech else "smooth radio"
        return ctx.base, "; ".join(filter(None, [fallback_note, why]))

    memory = max(0, int(_number(_setting("transitions.technique_memory", 3), 3)))
    recently = list(recent)[-memory:] if memory else []
    fx = {}
    for name in FX:
        if banned(name) or eligible(name, ctx):
            continue
        weight = suitability(name, ctx)
        if name in recently:
            weight *= 0.03         # do not repeat a trick within the last few mixes
        if weight > 0:
            fx[name] = weight
    base_weight = 1.6 * (1 - amount) + 0.15
    if recently and recently[-1] == ctx.base:
        base_weight *= 0.7
    options = {ctx.base: base_weight}
    if fx:
        # Creativity sets how much probability effects get as a whole; the
        # context sets how it is shared. A pair that suits nothing flashy
        # gets fewer effects, not the same number spread thinner.
        total = sum(fx.values())
        fresh = [w for name, w in fx.items() if name not in recently]
        mean = (sum(fresh) / len(fresh)) if fresh else total / len(fx) * 0.1
        mass = 3.0 * amount ** 1.3 * min(1.5, mean)
        for name, weight in fx.items():
            options[name] = mass * weight / total
    rng = random.Random(seed)
    roll = rng.random() * sum(options.values())
    for name, weight in sorted(options.items()):
        roll -= weight
        if roll <= 0:
            break
    note = "creative pick" if name != ctx.base else "kept the blend"
    return name, "; ".join(filter(None, [fallback_note, f"{note} at creativity {amount:.2f}"]))


def build(name: str, ctx: Context) -> dict | None:
    """The technique's shape for this context, clamped into engine ranges."""
    technique = TECHNIQUES.get(name)
    if technique is None or technique.build is None:
        return None
    shape = technique.build(ctx)
    end = ctx.overlap
    for deck, lanes in shape["lanes"].items():
        for lane, points in list(lanes.items()):
            low, high = RANGES.get(lane, (-4.0, 4.0))
            for point in points:
                point[1] = round(min(high, max(low, point[1])), 4)
            if deck == "out":
                _reset(points, lane, end)
            else:
                _reset(points, lane, max(points[-1][0], 0.0))
    for key in ("out_volume", "in_volume"):
        for point in shape[key]:
            point[0] = round(min(max(point[0], 0.0), end), 4)
            point[1] = round(min(1.0, max(SILENT, point[1])), 5)
    shape["technique"] = name
    shape["flashy"] = technique.flashy
    return shape


def absolute(shape: dict, overlap_start: float, out_bounds: tuple[float, float],
             in_bounds: tuple[float, float]) -> tuple[dict, list[dict]]:
    """Lanes and events on the station clock, clipped to each deck's item."""
    lanes: dict[str, dict[str, list[list[float]]]] = {"out": {}, "in": {}}
    for deck, bounds in (("out", out_bounds), ("in", in_bounds)):
        for lane, points in (shape.get("lanes", {}).get(deck) or {}).items():
            moved = [[round(overlap_start + t, 4), v] for t, v in points
                     if bounds[0] - 1e-3 <= overlap_start + t <= bounds[1] + 1e-3]
            if moved:
                lanes[deck][lane] = moved
    events = []
    for event in shape.get("events", []):
        moved = dict(event)
        moved["at"] = round(overlap_start + event["at"], 4)
        moved["until"] = round(overlap_start + event["until"], 4)
        bounds = out_bounds if event.get("deck") == "out" else in_bounds
        if bounds[0] - 1e-3 <= moved["at"] and moved["until"] <= bounds[1] + 1e-3:
            events.append(moved)
    return lanes, events


# --------------------------------------------------------------------------
# Evidence the schedule gathers
# --------------------------------------------------------------------------
_STEM_CACHE: dict[str, tuple[float, bool]] = {}
_STEM_LOCK = threading.Lock()


def has_stems(track: dict[str, Any]) -> bool:
    """Whether this record's four parts are already separated on disk.

    Cheap -- a stat per part, remembered for a minute -- and never starts a
    separation: a stem technique is only offered when the parts exist.
    """
    value = track.get("stems") if hasattr(track, "get") else None
    if isinstance(value, bool):
        return value
    path = track.get("file") if hasattr(track, "get") else None
    if not path:
        return False
    now = time.monotonic()
    with _STEM_LOCK:
        cached = _STEM_CACHE.get(str(path))
        if cached and now - cached[0] < 60:
            return cached[1]
    try:
        from . import stems
        found = stems.existing(stems.cache_dir(Path(path))) is not None
    except (OSError, ValueError, TypeError):
        found = False
    with _STEM_LOCK:
        if len(_STEM_CACHE) > 256:
            _STEM_CACHE.clear()
        _STEM_CACHE[str(path)] = (now, found)
    return found


# --------------------------------------------------------------------------
# What aired, for the recency penalty and the hosts
# --------------------------------------------------------------------------
class History:
    """Aired techniques, kept in the events table so they outlive a restart.

    One row per transition as it airs (kind 'transition'). The selector reads
    the last few; the hosts read the newest flashy one.
    """
    KIND = "transition"

    def recent(self, limit: int = 8) -> list[str]:
        try:
            from . import db
            rows = db.query("SELECT meta FROM events WHERE kind=? ORDER BY id DESC LIMIT ?",
                            (self.KIND, int(limit)))
        except Exception:  # noqa: BLE001 - history is a nicety, never a failure
            return []
        names = []
        for row in rows:
            try:
                names.append(str(json.loads(row["meta"] or "{}").get("technique") or ""))
            except (TypeError, ValueError):
                continue
        return [name for name in reversed(names) if name]

    def record(self, technique: str, track_key: str | None, **meta: Any) -> None:
        try:
            from . import db
            db.log_event(self.KIND, track_key, None, technique=technique, **meta)
        except Exception:  # noqa: BLE001
            pass

    def latest(self) -> dict | None:
        try:
            from . import db
            row = db.one("SELECT id, ts, track_key, meta FROM events WHERE kind=? "
                         "ORDER BY id DESC LIMIT 1", (self.KIND,))
        except Exception:  # noqa: BLE001
            return None
        if not row:
            return None
        try:
            meta = json.loads(row["meta"] or "{}")
        except (TypeError, ValueError):
            return None
        return {"id": row["id"], "ts": row["ts"], "track_key": row["track_key"], **meta}


history = History()


def host_note(now: float | None = None, rng: random.Random | None = None) -> str:
    """A factual line about the last flashy transition, now and then.

    Only what actually aired, only within a few minutes, only once, and only
    at the configured chance -- the hosts must never invent a mix move.
    """
    chance = max(0.0, min(1.0, _number(_setting("hosts.transition_note_chance", 0.3), 0.3)))
    if chance <= 0:
        return ""
    latest = history.latest()
    if not latest or latest.get("technique") not in FLASHY:
        return ""
    now = time.time() if now is None else now
    if now - _number(latest.get("ts"), 0) > 15 * 60:
        return ""
    try:
        from . import db
        ident = str(latest["id"])
        if db.is_seen("transition_note", ident):
            return ""
        if (rng or random).random() >= chance:
            db.mark_seen("transition_note", ident)   # rolled once; do not re-roll every break
            return ""
        db.mark_seen("transition_note", ident)
    except Exception:  # noqa: BLE001
        return ""
    technique = TECHNIQUES[latest["technique"]]
    into = latest.get("title") or "the current record"
    artist = latest.get("artist")
    into = json.dumps(f"{into} by {artist}" if artist else into, ensure_ascii=False)
    return (f"MIX NOTE (fact, optional): the mix into {into} was {technique.describe}. "
            "At most a few words about it, only if it fits; do not describe any other transition.")
