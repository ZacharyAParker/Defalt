"""Compare bounded transition candidates using cached musical evidence.

No decoding or model work belongs here: the schedule lock calls this function.
Unknown vocals stay unknown, and absent analysis preserves the ordinary plan.
All times in profiles are source seconds; schedule durations are wall seconds.
"""
from __future__ import annotations

import math
from dataclasses import dataclass, replace
from typing import Any

from . import config, lyric_sections, playback, structure, transitions


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


def profile(track: Any) -> dict:
    """The cached acoustic profile, with the synced lyrics as its vocal
    evidence when the track has them. Lyrics never invent a profile."""
    found = structure.profile_for(track)
    spans = lyric_sections.lyric_map(track).get("vocal_spans")
    return lyric_sections.overlay_vocals(found, spans) if found and spans else found


def _window(profile, start, end, field):
    """Mean local evidence; missing samples never imply silence/no vocals."""
    values = [structure.at(profile, start + (end - start) * i / 8).get(field)
              for i in range(9)]
    values = [_number(v, math.nan) for v in values if v is not None]
    values = [v for v in values if math.isfinite(v)]
    return sum(values) / len(values) if len(values) >= 5 else None


def _safe_entry(profile, offset, cue, spans=None):
    if cue <= offset + 0.01:
        return True
    if spans:
        # Synced lyrics say exactly where the first words are: skipping up
        # to them loses no lyric. A beat of slack for a pickup.
        return not any(float(a) < cue - 0.25 and float(b) > offset for a, b in spans)
    # Never skip an opening lyric on a spectral guess. Known instrumental
    # lead-ins or actual near-silence are the only automatic entry skips.
    vocal = _window(profile, offset, cue, "vocal")
    energy = _window(profile, offset, cue, "energy")
    return ((vocal is not None and vocal < 0.12)
            or (energy is not None and energy < 0.025))


def _options():
    defaults = {"overlap_scoring": True, "vocal_collision_weight": 1.0,
                "energy_dip_weight": 0.6, "bass_collision_weight": 0.5,
                "adaptive_eq_fx": True, "vocal_handoff": True, "vocal_eq_depth": 3.0,
                "echo_in_blends": True, "echo_enabled": True, "echo_mix": 0.18}
    return {key: config.station.get(f"transitions.{key}", value)
            for key, value in defaults.items()}


def overlap_risk(plan, outgoing, incoming, out_at, cue, in_rate):
    """Estimate audible clashes using the gain/EQ curves we will actually send.

    Cached amplitudes are relative within each song, not calibrated loudness.
    This is a bounded ranking heuristic, not an audio render or vocal detector.
    Simultaneous samples matter: alternating singers are not a vocal clash.
    """
    out_auto, in_auto = transitions.render(plan, plan.overlap)
    totals = {"vocal": [], "bass": [], "dip": []}
    for i in range(transitions.STEPS + 1):
        x = i / transitions.STEPS
        t = min(plan.overlap - 1e-6, plan.overlap * x)
        left = structure.at(outgoing, out_at(max(0.0, t)))
        right = structure.at(incoming, cue + max(0.0, t) * in_rate)
        a, b = out_auto.gain[i][1], in_auto.gain[i][1]
        for field, band in (("vocal", "mid"), ("bass", "low")):
            av, bv = left.get(field), right.get(field)
            if av is not None and bv is not None:
                ga = a * 10 ** (getattr(out_auto, band)[i][1] / 20)
                gb = b * 10 ** (getattr(in_auto, band)[i][1] / 20)
                if field == "vocal":
                    # A mid cut cannot remove a singer's full spectrum. Never
                    # score broad EQ as though it were isolated stem muting.
                    ga, gb = .6 * a + .4 * ga, .6 * b + .4 * gb
                totals[field].append(av * bv * ga * gb)
        av, bv = left.get("energy"), right.get("energy")
        if av is not None and bv is not None:
            expected = (1 - x) * av + x * bv
            actual = math.hypot(av * a, bv * b)
            totals["dip"].append(max(0.0, expected - actual))
    return {key: sum(values) / len(values) * plan.overlap if len(values) >= 11 else None
            for key, values in totals.items()}


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
    out_profile, in_profile = profile(outgoing), profile(incoming)
    if (not out_profile or not in_profile
            or not out_profile.get("complete") or not in_profile.get("complete")):
        return baseline
    options = _options()

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
    minimum_end = out_offset + (out_full if mid_song else out_end - out_offset) * min_play
    earliest_exit = max(out_end - max_early * out_rate, minimum_end)
    phrasing = bool(cfg.get("transitions.phrase_cues", True))
    out_sections = lyric_sections.lyric_map(outgoing).get("sections") or []
    in_sections = lyric_sections.lyric_map(incoming).get("sections") or []
    section_weight = max(0.0, min(1.0, _number(cfg.get("transitions.section_weight", 0.4), 0.4)))
    if not section_weight:
        out_sections = in_sections = []
    for point in out_profile.get("exits", []):
        at = _number(point.get("at"), -1)
        if earliest_exit <= at < out_end:
            quality = _number(point.get("score", point.get("confidence", 0.0)))
            # An acoustic change a beat or two off the bar grid is almost
            # always the phrase line itself, measured coarsely.
            snapped = structure.snap_to_phrase(outgoing, at, 2 * _number(outgoing.get("beat_period"), 0.5)) \
                if phrasing else None
            if snapped is not None and earliest_exit <= snapped < out_end:
                at = snapped
            exits.append((at, quality))
    if phrasing:
        # Phrase lines are exits in their own right: DJs leave on an eight.
        for at in structure.phrase_lines(outgoing, earliest_exit, out_end - 0.01, limit=4):
            if all(abs(at - other) > 0.25 for other, _ in exits):
                exits.append((at, 0.45))
    # Leaving after the last chorus, or on the outro: the fade starts there,
    # so the record ends one planned overlap later.
    for line in lyric_sections.exit_points(out_sections):
        at = line + plan.overlap * out_rate
        if earliest_exit <= at < out_end and all(abs(at - other) > 0.25 for other, _ in exits):
            exits.append((at, 0.5))
    lyric_entries = [{"at": at, "score": 0.5} for at in lyric_sections.entry_points(in_sections)]
    in_spans = lyric_sections.lyric_map(incoming).get("vocal_spans") or []
    for point in in_profile.get("entries", []) + lyric_entries:
        at = _number(point.get("at"), -1)
        if in_offset < at <= min(in_end - 10 * in_rate, in_offset + max_skip):
            # A deeper cue may omit an earlier verse, but it must be a real
            # acoustic boundary. Do not mistake a loud bin for a chorus label.
            boundary = any(abs(_number(p.get("at"), -10) - at) <= 0.5
                           and _number(p.get("confidence")) >= 0.18
                           for p in in_profile.get("boundaries", [])) or any(
                abs(s["start"] - at) <= 0.5 for s in in_sections)
            if _safe_entry(in_profile, in_offset, at, in_spans) or (mid_song and boundary):
                quality = _number(point.get("score", point.get("confidence", 0.0)))
                snapped = structure.snap_to_phrase(incoming, at, 2 * _number(incoming.get("beat_period"), 0.5)) \
                    if phrasing else None
                if (snapped is not None and in_offset < snapped <= min(in_end - 10 * in_rate, in_offset + max_skip)
                        and _safe_entry(in_profile, in_offset, snapped, in_spans)):
                    at = snapped
                entries.append((at, quality))
    # Bound work even for a damaged cache. Prefer strong nearby evidence.
    exits = exits[:1] + sorted(exits[1:], key=lambda p: (-p[1], -p[0]))[:5]
    entries = entries[:1] + sorted(entries[1:], key=lambda p: (-p[1], p[0]))[:3]
    forced = str(cfg.get("transitions.preset", "auto")) in transitions.PRESETS
    presets = [plan.preset]
    if not forced:
        presets += [p for p in ("fade", "blend", "melt", "slam") if p != plan.preset]
    # Settings and preset construction are invariant across cue combinations.
    templates = {}
    for preset in presets:
        template = replace(plan)
        if preset != plan.preset:
            template.preset = preset
            template.volume, template.eq, template.effects = transitions.preset_spec(preset)
            template.echo_mix = 0.0
            transitions.configured(template)
        templates[preset] = template

    # Invariant across every cue combination: read once, not per candidate.
    fraction = max(0.0, _number(cfg.get("crossfade.max_fraction_of_track", 0.25), 0.25))
    phrase_weight = max(0.0, min(1.0, _number(cfg.get("transitions.phrase_weight", 0.3), 0.3))) if phrasing else 0.0
    minimum = transitions.minimum_overlap(plan.overlap)
    intro = incoming.get("intro_override")
    if intro is None:
        intro = incoming.get("intro_sec")
    out_beat = _number(outgoing.get("beat_period"), 0.5) or 0.5
    in_beat = _number(incoming.get("beat_period"), 0.5) or 0.5
    best = None
    count = 0
    for end, exit_quality in exits:
        out_length = wall_at(end)
        for cue, entry_quality in entries:
            in_length = (in_end - cue) / in_rate
            for scale in ((1.0,) if forced else ((1.0, 0.75, 0.5, 0.25) if options["overlap_scoring"] else (1.0, 0.5))):
                # The compatibility planner already bounded beat drift and
                # the intro. Acoustic evidence may shorten that limit, never
                # expand it into a blend whose drums will drift apart.
                overlap = min(plan.overlap * scale, min(out_length, in_length) * fraction)
                effective_minimum = min(minimum, min(out_length, in_length) * fraction)
                if mid_song and out_end - out_offset >= out_full * min_play:
                    majority_at = wall_at(out_offset + out_full * min_play)
                    overlap = min(overlap, max(0.0, out_length - majority_at))
                    effective_minimum = min(effective_minimum, max(0.0, out_length - majority_at))
                # Respect a measured/overridden vocal entry after moving a cue.
                if not forced and intro is not None and cue <= _number(intro):
                    overlap = min(overlap, max(minimum, (_number(intro) - cue) / in_rate))
                elif not forced and cue > in_offset:
                    # At a deeper entry the original opening's vocal timestamp
                    # is no longer relevant. Use this cue's measured local gap.
                    vocals = [structure.at(in_profile, cue + i * 0.5).get("vocal")
                              for i in range(int(overlap * in_rate / 0.5) + 1)]
                    first_voice = next((i for i, v in enumerate(vocals) if v is not None and v > 0.2), None)
                    if first_voice is not None:
                        overlap = min(overlap, max(minimum, first_voice * 0.5 / in_rate))
                    elif any(v is None for v in vocals):
                        overlap = min(overlap, max(minimum, 2.0))
                if mid_song and end < out_end - 0.01:
                    # The majority must be heard BEFORE fading starts, using
                    # full-file source time. Intro and outro cuts share a budget.
                    if source_at(out_length - overlap) - out_offset < out_full * min_play:
                        continue
                if overlap < max(0.15, effective_minimum) or out_start + out_length - overlap < earliest_start:
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
                    candidate = replace(templates[preset], overlap=overlap)
                    # Start from compatibility-based preference. Structural
                    # evidence can outweigh it, but random stylistic churn cannot.
                    score = 0.35 if preset == plan.preset else 0.0
                    score += 0.35 * (exit_quality + entry_quality)
                    score -= 0.5 * (out_end - end) / max(1, max_early * out_rate)
                    score -= 0.35 * (cue - in_offset) / max(1, max_skip)
                    score -= 0.6 * abs(scale - 1.0)
                    if phrase_weight:
                        # Start the blend on a phrase line of the outgoing
                        # record and bring the incoming one in on its own:
                        # the eights line up and the mix breathes with both.
                        score += phrase_weight * (structure.phrase_alignment(outgoing, out_local)
                                                  + structure.phrase_alignment(incoming, cue)) / 2
                    if out_sections or in_sections:
                        # Verse and chorus from the synced lyrics: never fade
                        # out mid-chorus, and come in where a section starts.
                        score += section_weight * (
                            lyric_sections.exit_score(out_sections, out_local, out_beat)
                            + (lyric_sections.entry_score(in_sections, cue, in_beat) if cue > in_offset + 0.01 else 0.0))
                    if out_energy is not None and in_energy is not None:
                        score -= abs(out_energy - in_energy) * (0.3 if preset == "slam" else 0.8)
                    if options["overlap_scoring"]:
                        preview = replace(candidate)
                        if options["adaptive_eq_fx"]:
                            adapt(preview, out_profile, in_profile, end, cue, out_rate, in_rate,
                                  options=options)
                        risk = overlap_risk(preview, out_profile, in_profile,
                            lambda t: source_at(out_length - overlap + t), cue, in_rate)
                        for field, setting in (("vocal", "vocal_collision_weight"),
                                               ("bass", "bass_collision_weight"),
                                               ("dip", "energy_dip_weight")):
                            if risk[field] is not None:
                                score -= risk[field] * max(0, min(2, _number(options[setting]))) * (2 if field == "vocal" else 1)
                    elif out_vocal is not None and in_vocal is not None:
                        collision = out_vocal * in_vocal
                        score -= collision * overlap * (0.1 if preset == "slam" else 0.7)
                    if not options["overlap_scoring"] and out_bass is not None and in_bass is not None and candidate.eq == "none":
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
                        + ("; checked overlap dynamics" if options["overlap_scoring"] else "")
                        + ("; earlier structural exit" if best.out_duration < out_duration - 0.01 else "")
                        + ("; structural entry cue" if best.in_offset > in_offset + 0.01 else "")
                        + ("; lyric sections" if out_sections or in_sections else "")
                        + f"; {plan.reason}")
    return best


def adapt(plan, outgoing, incoming, end, cue, out_rate, in_rate, *, options=None):
    """Use local evidence to position bass handoff and leave room for vocals."""
    overlap = plan.overlap
    if overlap <= 0:
        return
    options = _options() if options is None else options
    bass = [structure.at(incoming, cue + overlap * in_rate * i / 10).get("bass")
            for i in range(11)]
    if all(v is not None and math.isfinite(_number(v, math.nan)) for v in bass):
        # A later incoming bass entrance earns a later handoff, bounded away
        # from transition edges so both sides still have time to ramp.
        top = max(bass)
        if top > 0.05:
            entry = next((i for i, value in enumerate(bass) if value >= top * 0.65), 5)
            plan.bass_swap = min(0.8, max(0.2, entry / 10))
            out_bass = [structure.at(outgoing, max(0, end - overlap * out_rate)
                                    + min(overlap * i / 10, overlap - 1e-6) * out_rate).get("bass")
                        for i in range(11)]
            if all(v is not None for v in out_bass):
                preferred = plan.bass_swap if entry > 0 else 0.5
                def cost(point):
                    out_eq, in_eq = transitions._bass_swap(point)
                    loss = sum(max(0.0, max(a, b) - math.hypot(
                        a * 10 ** (out_eq(i / 10) * plan.eq_strength / 20),
                        b * 10 ** (in_eq(i / 10) * plan.eq_strength / 20)))
                        for i, (a, b) in enumerate(zip(out_bass, bass))) / 11
                    return loss + 0.08 * abs(point - preferred)
                plan.bass_swap = min((i / 20 for i in range(4, 17)), key=cost)
    out_vocal = _window(outgoing, end - overlap * out_rate, end, "vocal")
    in_vocal = _window(incoming, cue, cue + overlap * in_rate, "vocal")
    if out_vocal is not None and in_vocal is not None:
        pairs = [(i / 20,
                  structure.at(outgoing, end - overlap * out_rate + overlap * out_rate * i / 20).get("vocal"),
                  structure.at(incoming, cue + overlap * in_rate * i / 20).get("vocal"))
                 for i in range(20)]
        known = [(x, a, b) for x, a, b in pairs if a is not None and b is not None]
        if options["vocal_handoff"] and plan.eq != "none" and any(a * b > .12 for _, a, b in known):
            # Handoff near the strongest incoming vocal entrance, preferring
            # a point where the outgoing voice leaves a gap.
            points = [i / 20 for i in range(4, 17)]
            def vocal_cost(point):
                mismatch = sum((b if x < point else a) for x, a, b in known) / max(1, len(known))
                return mismatch + .15 * abs(point - .5)
            plan.vocal_swap = min(points, key=vocal_cost)
            plan.vocal_depth = max(0, min(6, _number(options["vocal_eq_depth"], 3)))
        if (options["echo_enabled"] and options["echo_in_blends"] and plan.echo_mix == 0
                and plan.preset in ("fade", "blend") and len(known) == 20
                and max(max(a, b) for _, a, b in known) < .12):
            out_head = _window(outgoing, end - overlap * out_rate, end - overlap * out_rate * .5, "energy")
            out_tail = _window(outgoing, end - overlap * out_rate * .25, end - 1e-6, "energy")
            in_tail = _window(incoming, cue + overlap * in_rate * .5, cue + overlap * in_rate, "energy")
            if (out_head is not None and out_tail is not None and in_tail is not None
                    and .04 < out_tail < out_head * .7 and in_tail < .55):
                plan.echo_mix = min(.18, max(0, _number(options["echo_mix"], .18)))
                plan.echo_start = .55
        # Repeating a departing vocal under a new singer produces a clash.
        peak_voice = max([out_vocal, in_vocal] + [max(a, b) for _, a, b in known])
        plan.echo_mix *= max(0.0, 1.0 - peak_voice)
        if peak_voice > 0.35:
            plan.effects = tuple(effect for effect in plan.effects if not effect.endswith("_in"))
    # Unknown vocal activity is not a license for large wet effects.
    elif plan.echo_mix > 0:
        plan.echo_mix *= 0.6
