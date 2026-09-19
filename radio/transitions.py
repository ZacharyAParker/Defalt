"""Transitions: how one record becomes the next.

A transition is three independent choices, which is what makes the named
presets easy to reason about rather than magic:

  volume  what each track's level does across the overlap
  eq      what happens to the bass, because two basslines at once is mud
  fx      filter sweeps layered on top

`Fade` is an equal-power crossfade with the bass swapping at the midpoint.
`Wave` is the same bass swap but with both tracks low-passed through the
middle, so the mix goes muffled and opens up again. Same machinery, one
different choice.

Everything is emitted as parameter automation -- lists of [time, value]
breakpoints, exactly like the gain envelope -- so the browser applies it with
the same code path it already uses and nothing has to be scheduled live.
"""
from __future__ import annotations

import math
import random
from dataclasses import dataclass, field
from typing import Any, Callable

from . import analysis, config

# Effectively bypassed filter positions. Web Audio has no "off" switch, so a
# filter that should not be doing anything sits at the edge of the spectrum.
LPF_OPEN = 20000.0
HPF_OPEN = 20.0

# How many breakpoints a swept curve gets. Enough to read as continuous
# without bloating the schedule payload.
STEPS = 20

PRESETS = ("fade", "rise", "blend", "wave", "melt", "slam")


def minimum_overlap(requested: float) -> float:
    """A vocal timestamp is not a request for a near-instant cut."""
    return min(max(0.0, requested), max(0.5, min(12.0, float(
        config.station.get("transitions.minimum_blend_seconds", 3.0)))))

VOLUME_MODES = ("smooth_crossfade", "crossfade", "overlap", "fade_in_fade_out",
                "cut_in_fade_out", "fade_in_cut_out", "center_cut")

EQ_MODES = ("center_bass", "end_bass_swap", "three_band_fade", "none")


@dataclass
class Plan:
    """One boundary between two records."""
    preset: str = "fade"
    volume: str = "smooth_crossfade"
    eq: str = "center_bass"
    effects: tuple[str, ...] = ()
    overlap: float = 6.0
    reason: str = ""
    eq_strength: float = 1.0
    echo_mix: float = 0.0
    echo_feedback: float = 0.3
    echo_beats: float = 0.5
    bass_swap: float | None = None
    vocal_swap: float | None = None
    vocal_depth: float = 0.0
    echo_start: float = 0.0

    def as_dict(self) -> dict[str, Any]:
        return {"preset": self.preset, "volume": self.volume, "eq": self.eq,
                "effects": list(self.effects), "overlap": round(self.overlap, 2),
                "reason": self.reason, "eq_strength": self.eq_strength,
                "echo_mix": self.echo_mix, "echo_feedback": self.echo_feedback,
                "echo_beats": self.echo_beats, "bass_swap": self.bass_swap,
                "vocal_swap": self.vocal_swap, "vocal_depth": self.vocal_depth,
                "echo_start": self.echo_start}


def configured(plan: Plan) -> Plan:
    cfg = config.station
    mode = cfg.get("transitions.eq_mode", "auto")
    if mode in EQ_MODES:
        plan.eq = mode
    if not cfg.get("transitions.eq_enabled", True):
        plan.eq = "none"
    if not cfg.get("transitions.filters_enabled", True):
        plan.effects = ()
    plan.eq_strength = float(cfg.get("transitions.eq_strength", 1.0))
    if cfg.get("transitions.echo_enabled", True) and plan.preset in ("melt", "slam"):
        plan.echo_mix = float(cfg.get("transitions.echo_mix", 0.18))
    plan.echo_feedback = float(cfg.get("transitions.echo_feedback", 0.30))
    plan.echo_beats = float(cfg.get("transitions.echo_beats", 0.5))
    return plan


@dataclass
class Automation:
    """Parameter curves for one side of one transition."""
    gain: list[list[float]] = field(default_factory=list)
    low: list[list[float]] = field(default_factory=list)
    mid: list[list[float]] = field(default_factory=list)
    high: list[list[float]] = field(default_factory=list)
    lpf: list[list[float]] = field(default_factory=list)
    hpf: list[list[float]] = field(default_factory=list)


# --------------------------------------------------------------------------
# Volume shapes. Each returns (outgoing, incoming) as functions of x in 0..1.
# --------------------------------------------------------------------------
def _volume_curves(mode: str) -> tuple[Callable[[float], float],
                                       Callable[[float], float]]:
    if mode == "crossfade":
        return (lambda x: 1.0 - x, lambda x: x)

    if mode == "overlap":
        # Both tracks stay up. Only the EQ separates them, which is why the
        # presets that use this always pair it with a bass swap.
        return (lambda x: 1.0 if x < 0.97 else (1.0 - x) / 0.03,
                lambda x: 1.0 if x > 0.03 else x / 0.03)

    if mode == "fade_in_fade_out":
        # The old one is most of the way gone before the new one arrives, so
        # there is a brief dip between them.
        return (lambda x: max(0.0, 1.0 - x / 0.65),
                lambda x: max(0.0, (x - 0.35) / 0.65))

    if mode == "cut_in_fade_out":
        return (lambda x: 1.0 - x, lambda x: 1.0 if x > 0.02 else x / 0.02)

    if mode == "fade_in_cut_out":
        return (lambda x: 1.0 if x < 0.98 else 0.0, lambda x: x)

    if mode == "center_cut":
        # Both duck toward the middle and swap there. Reads as a deliberate
        # edit rather than a blend.
        return (lambda x: max(0.0, 1.0 - x * 2) if x < 0.5 else 0.0,
                lambda x: 0.0 if x < 0.5 else min(1.0, (x - 0.5) * 2))

    # smooth_crossfade -- equal power, constant perceived loudness.
    return (lambda x: math.cos(x * math.pi / 2),
            lambda x: math.sin(x * math.pi / 2))


# --------------------------------------------------------------------------
# EQ shapes. Values are dB applied to a shelf or peaking band.
# --------------------------------------------------------------------------
KILL_DB = -26.0     # far enough down that a bassline is gone, not quiet


def _bass_swap(point: float) -> tuple[Callable[[float], float],
                                      Callable[[float], float]]:
    """Outgoing keeps the low end until `point`, then the incoming takes it.

    The swap is a short ramp rather than a step. Two basslines fighting is
    the single ugliest thing a crossfade can do, and a hard cut on the low
    shelf is audible as a thump.
    """
    ramp = 0.12

    def outgoing(x: float) -> float:
        if x <= point - ramp:
            return 0.0
        if x >= point + ramp:
            return KILL_DB
        return KILL_DB * ((x - (point - ramp)) / (2 * ramp))

    def incoming(x: float) -> float:
        if x <= point - ramp:
            return KILL_DB
        if x >= point + ramp:
            return 0.0
        return KILL_DB * (1.0 - (x - (point - ramp)) / (2 * ramp))

    return (outgoing, incoming)


def _eq_curves(mode: str) -> dict[str, tuple[Callable[[float], float],
                                             Callable[[float], float]]]:
    flat = (lambda x: 0.0, lambda x: 0.0)

    if mode == "center_bass":
        return {"low": _bass_swap(0.5), "mid": flat, "high": flat}

    if mode == "end_bass_swap":
        return {"low": _bass_swap(0.85), "mid": flat, "high": flat}

    if mode == "three_band_fade":
        # Highs hand over first, mids next, lows last. That ordering is what
        # makes a long blend sound like one record becoming another rather
        # than two records playing at once.
        def band(point: float):
            width = 0.45

            def outgoing(x: float) -> float:
                return KILL_DB * min(1.0, max(0.0, (x - point + width) / (2 * width)))

            def incoming(x: float) -> float:
                return KILL_DB * (1.0 - min(1.0, max(0.0, (x - point + width) / (2 * width))))
            return (outgoing, incoming)

        return {"high": band(0.35), "mid": band(0.5), "low": band(0.7)}

    return {"low": flat, "mid": flat, "high": flat}


# --------------------------------------------------------------------------
# Filter sweeps
# --------------------------------------------------------------------------
def _sweep(start: float, end: float) -> Callable[[float], float]:
    """Logarithmic sweep -- linear in Hz sounds wrong to an ear."""
    log_start, log_end = math.log(max(start, 20.0)), math.log(max(end, 20.0))
    return lambda x: math.exp(log_start + (log_end - log_start) * x)


def _effect_curves(effects: tuple[str, ...]) -> dict[str, Any]:
    cfg = config.station
    lpf_floor = float(cfg.get("transitions.lpf_floor_hz", 380.0) or 380.0)
    hpf_ceiling = float(cfg.get("transitions.hpf_ceiling_hz", 900.0) or 900.0)

    out: dict[str, Any] = {}
    if "lpf_in" in effects:
        # Incoming arrives muffled and opens up.
        out["in_lpf"] = _sweep(lpf_floor, LPF_OPEN)
    if "lpf_out" in effects:
        # Outgoing closes down as it leaves.
        out["out_lpf"] = _sweep(LPF_OPEN, lpf_floor)
    if "hpf_in" in effects:
        out["in_hpf"] = _sweep(hpf_ceiling, HPF_OPEN)
    if "hpf_out" in effects:
        out["out_hpf"] = _sweep(HPF_OPEN, hpf_ceiling)
    return out


# --------------------------------------------------------------------------
# Preset definitions
# --------------------------------------------------------------------------
def preset_spec(name: str) -> tuple[str, str, tuple[str, ...]]:
    """(volume mode, eq mode, effects) for a named preset."""
    return {
        # A standard crossfade where the bass swaps around the midpoint.
        "fade":  ("smooth_crossfade", "center_bass", ()),
        # Overlap, bass swap at the end, low-pass in and high-pass out.
        "rise":  ("overlap", "end_bass_swap", ("lpf_in", "hpf_out")),
        # Overlap with a smooth three-band fade.
        "blend": ("smooth_crossfade", "three_band_fade", ()),
        # Overlap, bass swap at the centre, low-pass on both sides.
        "wave":  ("overlap", "center_bass", ("lpf_in", "lpf_out")),
        # Fade across, bass swap at the centre, high-pass on both sides.
        "melt":  ("crossfade", "center_bass", ("hpf_in", "hpf_out")),
        # A hard, centred volume swap.
        "slam":  ("center_cut", "none", ()),
    }.get(name, ("smooth_crossfade", "center_bass", ()))


# --------------------------------------------------------------------------
# Choosing one
# --------------------------------------------------------------------------
def tempo_distance(first: float, second: float) -> float:
    """Relative tempo difference, immune to octave errors.

    A detector that reports 172 for an 86 BPM track has not found a different
    tempo, it has found the same one counted twice. Comparing at face value
    would call that a huge mismatch and pick a hard cut for two tracks that
    would have beat-matched perfectly.
    """
    if not math.isfinite(first) or not math.isfinite(second) or first <= 0 or second <= 0:
        return 1.0
    best = 1.0
    for multiplier in (0.5, 1.0, 2.0):
        candidate = second * multiplier
        best = min(best, abs(first - candidate) / max(first, candidate))
    return best


def tempo_match(outgoing: Any, incoming: Any) -> tuple[float, str]:
    """Playback rate for the incoming record so its beats keep pace.

    Phase alignment alone is nearly useless in a rotation picked by taste
    rather than tempo: line the first beats up and two records a few BPM
    apart have drifted a whole beat by the middle of a nine-second blend.
    Matching the tempo is what makes alignment hold.

    This is the pitch fader, not a time-stretcher. Web Audio's playbackRate
    moves pitch with speed, exactly like riding the pitch control on a deck,
    which is why the range is capped: a few percent is inaudible as pitch and
    is what DJs use, while ten would be a semitone and a half and obvious.
    """
    cfg = config.station
    if not cfg.get("transitions.tempo_match", True):
        return (1.0, "disabled")

    from . import db
    out_bpm = float(db.field(outgoing, "bpm") or 0)
    in_bpm = float(db.field(incoming, "bpm") or 0)
    if not math.isfinite(out_bpm) or not math.isfinite(in_bpm) or out_bpm <= 0 or in_bpm <= 0:
        return (1.0, "no tempo")

    limit = float(cfg.get("transitions.tempo_match_limit", 0.06) or 0.06)

    # Consider the octave that needs the least correction: pulling 172 down to
    # 88 is not a tempo match, but treating it as 86 and nudging is.
    best_rate, best_cost = 1.0, 1.0
    for multiplier in (0.5, 1.0, 2.0):
        rate = out_bpm / (in_bpm * multiplier)
        cost = abs(rate - 1.0)
        if cost < best_cost:
            best_rate, best_cost = rate, cost

    if best_cost > limit:
        return (1.0, f"{best_cost * 100:.0f}% apart, too far to pitch")
    return (round(best_rate, 5), "")


def beat_nudge(outgoing: Any, incoming: Any, out_local: float,
               overlap: float, rate: float = 1.0) -> tuple[float, str]:
    """How far to shift the incoming record so its beats land on the outgoing
    one's. Returns (seconds, why-not) -- seconds is 0 when alignment is off.

    Matching tempo is not the same as matching beats. Two records at 129 BPM
    whose grids sit a tenth of a second apart flam through the entire overlap.
    This nudges the incoming start, by at most half a beat, so the grids
    coincide. Nothing is skipped or stretched -- the record simply starts a
    fraction of a beat earlier or later.

    Refuses in three cases, because a wrong nudge is worse than none:
      - either grid is weak, meaning we would be aligning to noise
      - the tempos are far enough apart that the grids diverge audibly
        before the overlap is over
      - there is no measured grid at all
    """
    cfg = config.station
    if not cfg.get("transitions.beat_align", True):
        return (0.0, "disabled")

    from . import db
    ceiling = float(cfg.get("transitions.beat_max_residual_ms", 30.0) or 30.0)

    out_period = float(db.field(outgoing, "beat_period") or 0)
    in_period = float(db.field(incoming, "beat_period") or 0)
    if out_period <= 0 or in_period <= 0:
        return (0.0, "no grid")

    out_residual = db.field(outgoing, "beat_residual_ms")
    in_residual = db.field(incoming, "beat_residual_ms")
    out_residual = float(out_residual) if out_residual is not None else 999
    in_residual = float(in_residual) if in_residual is not None else 999
    if out_residual > ceiling or in_residual > ceiling:
        return (0.0, f"beats wander too much "
                     f"({out_residual:.0f}ms/{in_residual:.0f}ms)")

    # After tempo matching the incoming record's beats arrive at
    # in_period / rate, which is the whole point of doing it.
    in_period = in_period / rate if rate > 0 else in_period
    in_period = min((in_period * scale for scale in (0.5, 1.0, 2.0)), key=lambda p: abs(p - out_period))

    # Beats drift apart at the rate the periods differ. Over the overlap that
    # drift must stay under a fraction of a beat or aligning the start just
    # moves the flam later.
    drift = abs(out_period - in_period) / out_period * overlap
    budget = float(cfg.get("transitions.beat_max_drift", 0.25) or 0.25)
    if drift > budget * out_period:
        return (0.0, f"grids drift {drift * 1000:.0f}ms over the blend")

    out_offset = float(db.field(outgoing, "beat_offset") or 0)
    in_offset = float(db.field(incoming, "beat_offset") or 0) / (rate or 1.0)

    # The next outgoing beat at or after the moment the incoming record
    # currently starts.
    elapsed = out_local - out_offset
    next_beat = out_offset + math.ceil(elapsed / out_period) * out_period

    # Shift so the incoming record's first beat lands on it, then wrap into
    # the nearest half beat so the schedule barely moves.
    delta = (next_beat - out_local) - in_offset
    delta = math.remainder(delta, in_period)
    return (round(delta, 4), "")


def choose(outgoing: Any, incoming: Any, rng: random.Random | None = None
           ) -> Plan:
    """Pick a transition from what the two records actually are."""
    from . import db
    cfg = config.station
    rng = rng or random

    forced = str(cfg.get("transitions.preset", "auto") or "auto").lower()
    base = max(0.0, float(cfg.get("crossfade.duration", 6.0)))

    if forced in PRESETS:
        volume, eq, effects = preset_spec(forced)
        return configured(Plan(forced, volume, eq, effects, base, "selected transition style"))

    out_bpm = float(db.field(outgoing, "bpm") or 0)
    in_bpm = float(db.field(incoming, "bpm") or 0)
    out_key = db.field(outgoing, "camelot") or ""
    in_key = db.field(incoming, "camelot") or ""
    out_loud = float(db.field(outgoing, "lufs") or 0)
    in_loud = float(db.field(incoming, "lufs") or 0)

    def trusted(track, field, minimum):
        confidence = db.field(track, field)
        return confidence is None or (math.isfinite(float(confidence)) and float(confidence) >= minimum)

    tempo_ok = (math.isfinite(out_bpm) and math.isfinite(in_bpm)
                and out_bpm > 0 and in_bpm > 0
                and trusted(outgoing, "bpm_confidence", 0.25)
                and trusted(incoming, "bpm_confidence", 0.25))
    close_tempo = tempo_ok and tempo_distance(out_bpm, in_bpm) <= float(
        cfg.get("transitions.tempo_tolerance", 0.06) or 0.06)
    keys_known = (bool(out_key and in_key)
                  and trusted(outgoing, "key_confidence", 0.15)
                  and trusted(incoming, "key_confidence", 0.15))
    harmonic = keys_known and analysis.keys_compatible(out_key, in_key)
    rising = (math.isfinite(out_loud) and math.isfinite(in_loud)
              and in_loud and out_loud and (in_loud - out_loud) > 1.5)

    long_mix = base * float(cfg.get("transitions.long_multiplier", 1.5) or 1.5)
    short_mix = base * float(cfg.get("transitions.short_multiplier", 0.55) or 0.55)

    if tempo_ok and tempo_distance(out_bpm, in_bpm) > float(
            cfg.get("transitions.slam_distance", 0.18) or 0.18):
        name, reason = "slam", f"{out_bpm:.0f} to {in_bpm:.0f}, too far to blend"
        overlap = short_mix
    elif close_tempo and harmonic:
        name, reason = "blend", f"{out_key} into {in_key}, tempos within reach"
        overlap = long_mix
    elif close_tempo and keys_known:
        name, reason = "wave", "tempos match but the keys clash"
        overlap = long_mix
    elif harmonic and not tempo_ok:
        name, reason = "fade", "keys agree, no tempo read"
        overlap = base
    elif harmonic:
        name, reason = "melt", f"{out_key} into {in_key}, tempos apart"
        overlap = base
    elif rising:
        name, reason = "rise", "stepping up in energy"
        overlap = base
    else:
        name, reason = "fade", "conservative fade; incomplete or uncertain analysis"
        overlap = base

    # Decks run at their original speed. A small BPM gap still accumulates
    # during a long overlap; finish before it becomes an audible double beat.
    if close_tempo and name != "slam":
        distance = tempo_distance(out_bpm, in_bpm)
        if distance > 0:
            safe = (60.0 / out_bpm) * 0.25 / distance
            if overlap > safe:
                overlap = safe
                reason += "; shortened to limit beat drift"

    intro = db.field(incoming, "intro_override")
    if intro is None:
        intro = db.field(incoming, "intro_sec")
    if intro is not None and math.isfinite(float(intro)):
        safe = max(minimum_overlap(base), float(intro))
        if overlap > safe:
            overlap = safe
            reason += "; clear before the incoming vocal"

    if overlap < minimum_overlap(base):
        overlap = minimum_overlap(base)
        name = "melt"
        reason += "; use a controlled fade instead of a sub-second drum blend"
    volume, eq, effects = preset_spec(name)
    return configured(Plan(name, volume, eq, effects, max(0.0, overlap), reason))


# --------------------------------------------------------------------------
# Rendering to automation
# --------------------------------------------------------------------------
def _sample(curve: Callable[[float], float], length: float, offset: float,
            steps: int = STEPS) -> list[list[float]]:
    points = []
    for index in range(steps + 1):
        x = index / steps
        points.append([round(offset + x * length, 4), round(curve(x), 4)])
    return points


def render(plan: Plan, length: float) -> tuple[Automation, Automation]:
    """Automation for (outgoing tail, incoming head), times relative to the
    start of the overlap window."""
    out_volume, in_volume = _volume_curves(plan.volume)
    eq = _eq_curves(plan.eq)
    if plan.bass_swap is not None and plan.eq != "none":
        eq["low"] = _bass_swap(max(0.2, min(0.8, plan.bass_swap)))
    fx = _effect_curves(plan.effects)

    outgoing = Automation(
        gain=_sample(out_volume, length, 0.0),
        low=_sample(eq["low"][0], length, 0.0),
        mid=_sample(eq["mid"][0], length, 0.0),
        high=_sample(eq["high"][0], length, 0.0),
    )
    incoming = Automation(
        gain=_sample(in_volume, length, 0.0),
        low=_sample(eq["low"][1], length, 0.0),
        mid=_sample(eq["mid"][1], length, 0.0),
        high=_sample(eq["high"][1], length, 0.0),
    )
    if plan.vocal_swap is not None and plan.eq != "none":
        # A modest mid-band handoff gives one singer the foreground. This is
        # broad EQ, not vocal isolation; the incoming band returns to unity.
        swap = max(0.2, min(0.8, plan.vocal_swap))
        depth = max(0.0, min(6.0, plan.vocal_depth))
        for i, (out_point, in_point) in enumerate(zip(outgoing.mid, incoming.mid)):
            x = i / STEPS
            turn = min(1.0, max(0.0, (x - swap + 0.15) / 0.3))
            out_point[1] -= depth * turn
            in_point[1] -= depth * (1.0 - turn)
    for automation in (outgoing, incoming):
        for band in (automation.low, automation.mid, automation.high):
            for point in band:
                point[1] *= max(0.0, min(1.0, plan.eq_strength))

    if "out_lpf" in fx:
        outgoing.lpf = _sample(fx["out_lpf"], length, 0.0)
    if "out_hpf" in fx:
        outgoing.hpf = _sample(fx["out_hpf"], length, 0.0)
    if "in_lpf" in fx:
        incoming.lpf = _sample(fx["in_lpf"], length, 0.0)
    if "in_hpf" in fx:
        incoming.hpf = _sample(fx["in_hpf"], length, 0.0)

    return (outgoing, incoming)
