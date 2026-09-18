"""Compare bounded transition candidates using cached musical evidence.

No decoding or model work belongs here: the schedule lock calls this function.
Unknown vocals stay unknown, and absent analysis preserves the ordinary plan.
All times in profiles are source seconds; schedule durations are wall seconds.
"""
from __future__ import annotations

import math
from dataclasses import dataclass, replace
from typing import Any

from . import config, playback, structure, transitions


@dataclass
class Choice:
    plan: transitions.Plan
    out_duration: float
    in_offset: float
    in_duration: float
    score: float = 0.0
    candidates: int = 0


def _number(value, fallback=0.0):
    try:
        value = float(value)
        return value if math.isfinite(value) else fallback
    except (TypeError, ValueError):
        return fallback


def _window(profile, start, end, field):
    """Mean local evidence; missing samples never imply silence/no vocals."""
    values = [structure.at(profile, start + (end - start) * i / 8).get(field)
              for i in range(9)]
    values = [_number(v, math.nan) for v in values if v is not None]
    values = [v for v in values if math.isfinite(v)]
    return sum(values) / len(values) if len(values) >= 5 else None


def _safe_entry(profile, offset, cue):
    if cue <= offset + 0.01:
        return True
    # Never skip an opening lyric on a spectral guess. Known instrumental
    # lead-ins or actual near-silence are the only automatic entry skips.
    vocal = _window(profile, offset, cue, "vocal")
    energy = _window(profile, offset, cue, "energy")
    return ((vocal is not None and vocal < 0.12)
            or (energy is not None and energy < 0.025))


def refine(outgoing: dict, incoming: dict, plan: transitions.Plan, *,
           out_start: float, out_offset: float, out_duration: float,
           out_rate: float, in_offset: float, in_duration: float, in_rate: float,
           earliest_start: float = 0.0, protected_until: float = 0.0,
           entry_locked: bool = False, out_curve=None, out_initial_rate=None) -> Choice:
    """Keep the original candidate unless a supported alternative scores better."""
    baseline = Choice(plan, out_duration, in_offset, in_duration)
    cfg = config.station
    if not cfg.get("transitions.smart_cues", True):
        return baseline
    out_profile, in_profile = structure.profile_for(outgoing), structure.profile_for(incoming)
    if (not out_profile or not in_profile
            or not out_profile.get("complete") or not in_profile.get("complete")):
        return baseline

    initial_rate = out_initial_rate or out_rate
    def source_at(wall):
        return out_offset + playback.source_at(out_curve, wall, initial_rate)
    def wall_at(source):
        return playback.wall_at(out_curve, max(0, source - out_offset), initial_rate)
    out_end = source_at(out_duration)
    in_end = in_offset + in_duration * in_rate
    mid_song = bool(cfg.get("transitions.mid_song_cues", True))
    min_play = max(0.51, min(1.0, _number(cfg.get("transitions.minimum_play_fraction", 0.65), 0.65)))
    out_full = max(out_end, _number(outgoing.get("duration"), out_end))
    in_full = max(in_end, _number(incoming.get("duration"), in_end))
    max_early = max(0.0, _number(cfg.get("transitions.exit_search_seconds", 24.0)))
    max_skip = 0.0 if entry_locked else max(0.0, _number(cfg.get("transitions.max_intro_skip", 8.0)))
    if mid_song:
        max_early = max(0.0, out_end - out_offset - out_full * min_play) / out_rate
        entry_fraction = max(0.0, min(0.49, _number(cfg.get("transitions.max_entry_skip_fraction", 0.25), 0.25)))
        if not entry_locked:
            max_skip = max(0.0, min(in_full * entry_fraction - in_offset,
                                   in_end - in_offset - in_full * min_play))
    exits = [(out_end, 0.0)]
    entries = [(in_offset, 0.0)]
    for point in out_profile.get("exits", []):
        at = _number(point.get("at"), -1)
        minimum_end = out_offset + (out_full if mid_song else out_end - out_offset) * min_play
        if max(out_end - max_early * out_rate, minimum_end) <= at < out_end:
            exits.append((at, _number(point.get("score", point.get("confidence", 0.0)))))
    for point in in_profile.get("entries", []):
        at = _number(point.get("at"), -1)
        if in_offset < at <= min(in_end - 10 * in_rate, in_offset + max_skip):
            # A deeper cue may omit an earlier verse, but it must be a real
            # acoustic boundary. Do not mistake a loud bin for a chorus label.
            boundary = any(abs(_number(p.get("at"), -10) - at) <= 0.5
                           and _number(p.get("confidence")) >= 0.18
                           for p in in_profile.get("boundaries", []))
            if _safe_entry(in_profile, in_offset, at) or (mid_song and boundary):
                entries.append((at, _number(point.get("score", point.get("confidence", 0.0)))))
    # Bound work even for a damaged cache. Prefer strong nearby evidence.
    exits = exits[:1] + sorted(exits[1:], key=lambda p: (-p[1], -p[0]))[:5]
    entries = entries[:1] + sorted(entries[1:], key=lambda p: (-p[1], p[0]))[:3]
    forced = str(cfg.get("transitions.preset", "auto")) in transitions.PRESETS
    presets = [plan.preset]
    if not forced:
        presets += [p for p in ("fade", "blend", "melt", "slam") if p != plan.preset]

    best = None
    count = 0
    for end, exit_quality in exits:
        out_length = wall_at(end)
        for cue, entry_quality in entries:
            in_length = (in_end - cue) / in_rate
            for scale in (1.0, 0.5):
                # The compatibility planner already bounded beat drift and
                # the intro. Acoustic evidence may shorten that limit, never
                # expand it into a blend whose drums will drift apart.
                fraction = max(0.0, _number(cfg.get("crossfade.max_fraction_of_track", 0.25), 0.25))
                overlap = min(plan.overlap * scale, min(out_length, in_length) * fraction)
                if mid_song and out_end - out_offset >= out_full * min_play:
                    majority_at = wall_at(out_offset + out_full * min_play)
                    overlap = min(overlap, max(0.0, out_length - majority_at))
                # Respect a measured/overridden vocal entry after moving a cue.
                intro = incoming.get("intro_override")
                if intro is None:
                    intro = incoming.get("intro_sec")
                if intro is not None and cue <= _number(intro):
                    overlap = min(overlap, max(0.15, (_number(intro) - cue) / in_rate))
                elif cue > in_offset:
                    # At a deeper entry the original opening's vocal timestamp
                    # is no longer relevant. Use this cue's measured local gap.
                    vocals = [structure.at(in_profile, cue + i * 0.5).get("vocal")
                              for i in range(int(overlap * in_rate / 0.5) + 1)]
                    first_voice = next((i for i, v in enumerate(vocals) if v is not None and v > 0.2), None)
                    if first_voice is not None:
                        overlap = min(overlap, max(0.35, first_voice * 0.5 / in_rate))
                    elif any(v is None for v in vocals):
                        overlap = min(overlap, 2.0)
                if mid_song and end < out_end - 0.01:
                    # The majority must be heard BEFORE fading starts, using
                    # full-file source time. Intro and outro cuts share a budget.
                    if source_at(out_length - overlap) - out_offset < out_full * min_play:
                        continue
                if overlap < 0.15 or out_start + out_length - overlap < earliest_start:
                    continue
                if out_start + out_length < protected_until:
                    continue
                out_local = source_at(out_length - overlap)
                out_energy = _window(out_profile, out_local, end, "energy")
                in_energy = _window(in_profile, cue, cue + overlap * in_rate, "energy")
                out_vocal = _window(out_profile, out_local, end, "vocal")
                in_vocal = _window(in_profile, cue, cue + overlap * in_rate, "vocal")
                out_bass = _window(out_profile, out_local, end, "bass")
                in_bass = _window(in_profile, cue, cue + overlap * in_rate, "bass")
                for preset in presets:
                    candidate = replace(plan, overlap=overlap)
                    if preset != plan.preset:
                        candidate.preset = preset
                        candidate.volume, candidate.eq, candidate.effects = transitions.preset_spec(preset)
                        candidate.echo_mix = 0.0
                        transitions.configured(candidate)
                    # Start from compatibility-based preference. Structural
                    # evidence can outweigh it, but random stylistic churn cannot.
                    score = 0.35 if preset == plan.preset else 0.0
                    score += 0.35 * (exit_quality + entry_quality)
                    score -= 0.5 * (out_end - end) / max(1, max_early * out_rate)
                    score -= 0.35 * (cue - in_offset) / max(1, max_skip)
                    score -= 0.2 * abs(scale - 1.0)
                    if out_energy is not None and in_energy is not None:
                        score -= abs(out_energy - in_energy) * (0.3 if preset == "slam" else 0.8)
                    if out_vocal is not None and in_vocal is not None:
                        collision = out_vocal * in_vocal
                        score -= collision * overlap * (0.1 if preset == "slam" else 0.7)
                    if out_bass is not None and in_bass is not None and candidate.eq == "none":
                        score -= out_bass * in_bass * (0.1 if preset == "slam" else 0.8)
                    # A long unmatched drum blend remains unsafe even if both
                    # songs happen to have sparse sections at this instant.
                    gap = transitions.tempo_distance(_number(outgoing.get("bpm")) * out_rate,
                                                     _number(incoming.get("bpm")) * in_rate)
                    if gap > 0.06 and preset in ("blend", "wave", "rise"):
                        score -= 2.0
                    count += 1
                    choice = Choice(candidate, out_length, cue, in_length, score)
                    if best is None or score > best.score + 1e-6:
                        best = choice
    if best is None:
        return baseline
    best.candidates = count
    best.plan.reason = (f"compared {count} cue/style options; {best.plan.preset}"
                        + ("; earlier structural exit" if best.out_duration < out_duration - 0.01 else "")
                        + ("; structural entry cue" if best.in_offset > in_offset + 0.01 else "")
                        + f"; {plan.reason}")
    return best


def adapt(plan, outgoing, incoming, end, cue, out_rate, in_rate):
    """Use local evidence to position bass handoff and leave room for vocals."""
    overlap = plan.overlap
    if overlap <= 0:
        return
    bass = [structure.at(incoming, cue + overlap * in_rate * i / 10).get("bass")
            for i in range(11)]
    if all(v is not None and math.isfinite(_number(v, math.nan)) for v in bass):
        # A later incoming bass entrance earns a later handoff, bounded away
        # from transition edges so both sides still have time to ramp.
        top = max(bass)
        if top > 0.05:
            entry = next((i for i, value in enumerate(bass) if value >= top * 0.65), 5)
            plan.bass_swap = min(0.8, max(0.2, entry / 10))
    out_vocal = _window(outgoing, end - overlap * out_rate, end, "vocal")
    in_vocal = _window(incoming, cue, cue + overlap * in_rate, "vocal")
    if out_vocal is not None and in_vocal is not None:
        # Repeating a departing vocal under a new singer produces a clash.
        plan.echo_mix *= max(0.0, 1.0 - max(out_vocal, in_vocal))
        if max(out_vocal, in_vocal) > 0.35:
            plan.effects = tuple(effect for effect in plan.effects if not effect.endswith("_in"))
    # Unknown vocal activity is not a license for large wet effects.
    elif plan.echo_mix > 0:
        plan.echo_mix *= 0.6
