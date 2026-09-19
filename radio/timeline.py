"""The playout timeline: every fade, duck and overlap, computed here.

The browser is deliberately dumb. It receives items with an absolute start
time and a gain envelope, and schedules them on the Web Audio clock. All the
judgement -- how long to crossfade, how far to duck, when a host has to start
talking so the last word lands exactly as the vocal comes in -- happens in
this file, where it can be reasoned about and tested.

Envelope format is a list of [time_from_item_start, gain] breakpoints. Curves
are baked into the breakpoints rather than described, so the browser only ever
has to ramp linearly between two numbers.
"""
from __future__ import annotations

import math
import uuid
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable

from . import config, playback, transitions

# Web Audio cannot ramp to or through exact zero on an exponential curve, and
# a true zero also makes de-duplication ambiguous. This is silence.
SILENT = 0.0001


@dataclass
class Item:
    """One thing that makes sound, placed on the station clock."""
    kind: str                       # "music" | "voice"
    url: str
    start_at: float                 # absolute station seconds
    duration: float                 # seconds of this item that will play
    envelope: list[list[float]] = field(default_factory=list)
    offset: float = 0.0             # start this far into the source file
    meta: dict[str, Any] = field(default_factory=dict)
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])

    @property
    def end_at(self) -> float:
        return self.start_at + self.duration

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "kind": self.kind, "url": self.url,
            "start_at": round(self.start_at, 4),
            "duration": round(self.duration, 4),
            "offset": round(self.offset, 4),
            "envelope": self.envelope,
            "meta": self.meta,
        }


# --------------------------------------------------------------------------
# Curves
# --------------------------------------------------------------------------
def _fade_curve(name: str) -> tuple[Callable[[float], float], Callable[[float], float]]:
    """Return (fade_in, fade_out) as functions of progress 0..1."""
    if name == "linear":
        return (lambda x: x, lambda x: 1.0 - x)
    if name == "exponential":
        # Holds the outgoing track up longer, then drops it away quickly.
        return (lambda x: x * x, lambda x: 1.0 - x * x)
    # equal_power: constant perceived loudness across the overlap. The default
    # for a reason -- linear crossfades audibly dip in the middle.
    return (lambda x: math.sin(x * math.pi / 2), lambda x: math.cos(x * math.pi / 2))


def _duck_curve(name: str, target: float) -> tuple[Callable[[float], float],
                                                   Callable[[float], float]]:
    """Return (attack, release) ramps between 1.0 and `target`."""
    target = max(target, SILENT)
    if name == "linear":
        return (lambda x: 1.0 + (target - 1.0) * x,
                lambda x: target + (1.0 - target) * x)
    # Exponential reads as a compressor rather than a fader move.
    return (lambda x: target ** x, lambda x: target ** (1.0 - x))


# --------------------------------------------------------------------------
# Envelope assembly
# --------------------------------------------------------------------------
@dataclass
class _Duck:
    start: float
    end: float


def _curve_from(points: list[list[float]] | None, default: float
                ) -> Callable[[float], float]:
    """Turn a breakpoint list into a function, flat at `default` outside it."""
    if not points:
        return lambda t: default
    first, last = points[0], points[-1]

    def value(t: float) -> float:
        if t <= first[0]:
            return first[1]
        if t >= last[0]:
            return last[1]
        for (t0, v0), (t1, v1) in zip(points, points[1:]):
            if t0 <= t <= t1:
                if t1 == t0:
                    return v1
                return v0 + (v1 - v0) * ((t - t0) / (t1 - t0))
        return last[1]
    return value


def build_parameter(duration: float, head: list[list[float]] | None,
                    head_at: float, tail: list[list[float]] | None,
                    tail_at: float, default: float) -> list[list[float]]:
    """One non-gain parameter across a whole item.

    A record is the incoming side of one transition and the outgoing side of
    the next, so its EQ and filter automation is two windows with a flat
    stretch of untouched audio in between.
    """
    if not head and not tail:
        return []

    head_curve = _curve_from(head, default)
    tail_curve = _curve_from(tail, default)
    head_end = head_at + (head[-1][0] if head else 0.0)
    tail_start = tail_at

    times: set[float] = {0.0, duration}
    for point in head or []:
        times.add(head_at + point[0])
    for point in tail or []:
        times.add(tail_at + point[0])
    if head:
        times.add(head_end + 0.01)
    if tail:
        times.add(max(0.0, tail_start - 0.01))

    points: list[list[float]] = []
    for moment in sorted(t for t in times if -0.001 <= t <= duration + 0.001):
        moment = min(max(moment, 0.0), duration)
        if head and moment <= head_end:
            value = head_curve(moment - head_at)
        elif tail and moment >= tail_start:
            value = tail_curve(moment - tail_at)
        else:
            value = default
        # Keep plateau endpoints: removing the last flat point turns a short
        # EQ move at the outro into a ramp across the entire song.
        points.append([round(moment, 4), round(value, 4)])

    if points and points[-1][0] < duration - 0.01:
        points.append([round(duration, 4), points[-1][1]])

    # A parameter that never leaves its resting value is not automation. Drop
    # it so the browser does not build a filter node to do nothing.
    if all(abs(value - default) < 1e-4 for _, value in points):
        return []
    return points


def build_music_envelope(duration: float, fade_in: float, fade_out: float,
                         ducks: Iterable[_Duck]) -> list[list[float]]:
    """Combine crossfade shape and duck windows into one breakpoint list.

    The two are multiplied rather than sequenced, so a host talking through a
    crossfade ducks the correct amount at every instant instead of fighting
    the fader.
    """
    cfg = config.station
    curve_in, curve_out = _fade_curve(str(cfg.get("crossfade.curve", "equal_power")))
    attack_len = float(cfg.get("ducking.attack", 0.35) or 0.35)
    release_len = float(cfg.get("ducking.release", 1.2) or 1.2)
    hold = float(cfg.get("ducking.hold_after", 0.40) or 0.0)
    target = float(cfg.get("ducking.target_gain", 0.10))
    duck_attack, duck_release = _duck_curve(
        str(cfg.get("ducking.curve", "exponential")), target)

    windows = sorted(ducks, key=lambda d: d.start)

    def fade_at(t: float) -> float:
        if fade_in > 0 and t < fade_in:
            return curve_in(max(0.0, t) / fade_in)
        if fade_out > 0 and t > duration - fade_out:
            return curve_out(min(1.0, (t - (duration - fade_out)) / fade_out))
        return 1.0

    def duck_at(t: float) -> float:
        gain = 1.0
        for window in windows:
            attack_from = window.start - attack_len
            release_from = window.end + hold
            release_to = release_from + release_len
            if t <= attack_from or t >= release_to:
                continue
            if t < window.start:
                value = duck_attack((t - attack_from) / attack_len)
            elif t <= release_from:
                value = target
            else:
                value = duck_release((t - release_from) / release_len)
            gain = min(gain, value)
        return gain

    # Sample densely inside every curved region, sparsely everywhere else.
    regions: list[tuple[float, float]] = []
    if fade_in > 0:
        regions.append((0.0, fade_in))
    if fade_out > 0:
        regions.append((duration - fade_out, duration))
    for window in windows:
        regions.append((window.start - attack_len, window.start))
        regions.append((window.end + hold, window.end + hold + release_len))

    times = {0.0, duration}
    for start, end in regions:
        span = end - start
        if span <= 0:
            continue
        steps = max(2, min(24, int(span / 0.12)))
        for index in range(steps + 1):
            times.add(start + span * index / steps)
    for window in windows:
        times.add(window.start)
        times.add(window.end)
        times.add(window.end + hold)

    points: list[list[float]] = []
    for moment in sorted(t for t in times if -0.001 <= t <= duration + 0.001):
        moment = min(max(moment, 0.0), duration)
        gain = max(SILENT, fade_at(moment) * duck_at(moment))
        # Drop breakpoints that add nothing -- keeps the payload small.
        if points and abs(points[-1][1] - gain) < 0.004 \
                and abs(points[-1][0] - moment) < 0.5:
            continue
        points.append([round(moment, 4), round(gain, 5)])

    if not points or points[-1][0] < duration - 0.01:
        points.append([round(duration, 4), round(max(SILENT, fade_at(duration)), 5)])
    return points


def build_gain_envelope(duration: float,
                        head: list[list[float]] | None, head_at: float,
                        tail: list[list[float]] | None, tail_at: float,
                        ducks: Iterable[_Duck]) -> list[list[float]]:
    """Gain across a whole item: transition curves times the duck.

    The two are multiplied rather than sequenced, so a host talking through a
    transition ducks the correct amount at every instant instead of fighting
    whatever the crossfade is doing.
    """
    cfg = config.station
    attack_len = float(cfg.get("ducking.attack", 0.35) or 0.35)
    release_len = float(cfg.get("ducking.release", 1.2) or 1.2)
    hold = float(cfg.get("ducking.hold_after", 0.40) or 0.0)
    target = float(cfg.get("ducking.target_gain", 0.10))
    duck_attack, duck_release = _duck_curve(
        str(cfg.get("ducking.curve", "exponential")), target)
    windows = sorted(ducks, key=lambda d: d.start)

    head_curve = _curve_from(head, 1.0)
    tail_curve = _curve_from(tail, 1.0)
    head_end = head_at + (head[-1][0] if head else 0.0)

    def transition_at(t: float) -> float:
        value = 1.0
        if head and t <= head_end:
            value = min(value, head_curve(t - head_at))
        if tail and t >= tail_at:
            value = min(value, tail_curve(t - tail_at))
        return value

    def duck_at(t: float) -> float:
        gain = 1.0
        for window in windows:
            attack_from = window.start - attack_len
            release_from = window.end + hold
            release_to = release_from + release_len
            if t <= attack_from or t >= release_to:
                continue
            if t < window.start:
                value = duck_attack((t - attack_from) / attack_len)
            elif t <= release_from:
                value = target
            else:
                value = duck_release((t - release_from) / release_len)
            gain = min(gain, value)
        return gain

    times: set[float] = {0.0, duration}
    for point in head or []:
        times.add(head_at + point[0])
    for point in tail or []:
        times.add(tail_at + point[0])
    for window in windows:
        span = (window.end + hold + release_len) - (window.start - attack_len)
        steps = max(2, min(24, int(span / 0.12)))
        for index in range(steps + 1):
            times.add(window.start - attack_len + span * index / steps)
        times.add(window.start)
        times.add(window.end)
        times.add(window.end + hold)
        # Sample the attack and release themselves, even for long speeches.
        # Sampling the whole break at 24 points could miss a 350 ms attack.
        for index in range(13):
            times.add(window.start - attack_len + attack_len * index / 12)
            times.add(window.end + hold + release_len * index / 12)

    points: list[list[float]] = []
    for moment in sorted(t for t in times if -0.001 <= t <= duration + 0.001):
        moment = min(max(moment, 0.0), duration)
        gain = max(SILENT, transition_at(moment) * duck_at(moment))
        points.append([round(moment, 4), round(gain, 5)])

    if not points or points[-1][0] < duration - 0.01:
        points.append([round(duration, 4),
                       round(max(SILENT, transition_at(duration)), 5)])
    return points


def flat_envelope(duration: float, gain: float = 1.0,
                  edge: float = 0.06) -> list[list[float]]:
    """A voice line: brief edges so nothing clicks, full level in between."""
    gain = max(SILENT, gain)
    edge = min(edge, duration / 3.0) if duration > 0 else 0.0
    if edge <= 0:
        return [[0.0, gain], [round(duration, 4), gain]]
    return [
        [0.0, SILENT],
        [round(edge, 4), round(gain, 5)],
        [round(max(edge, duration - edge), 4), round(gain, 5)],
        [round(duration, 4), SILENT],
    ]


# --------------------------------------------------------------------------
# The schedule
# --------------------------------------------------------------------------
class Schedule:
    """An append-only run of items on the station clock."""

    def __init__(self, epoch_offset: float = 0.0):
        self.items: list[Item] = []
        self.cursor: float = epoch_offset       # next clear point
        self._last_music: Item | None = None
        self._pending_ducks: dict[str, list[_Duck]] = {}
        # Fade shape per item, kept out of meta so re-sealing cannot lose it.
        self._fades: dict[str, tuple[float, float]] = {}
        # The transition INTO each item, and the track row it came from, so a
        # re-seal rebuilds the same automation rather than inventing new.
        self._plans: dict[str, transitions.Plan] = {}
        self._rows: dict[str, dict[str, Any]] = {}
        self._last_row: dict[str, Any] | None = None
        self.rng = __import__("random").Random()

    # -- queries ---------------------------------------------------------
    @property
    def end_at(self) -> float:
        return max((item.end_at for item in self.items), default=self.cursor)

    def music_items(self) -> list[Item]:
        return [i for i in self.items if i.kind == "music"]

    def as_dict(self) -> list[dict[str, Any]]:
        return [item.as_dict() for item in sorted(self.items, key=lambda i: i.start_at)]

    def trim_before(self, station_time: float) -> None:
        """Drop items that have finished, so the payload stays bounded."""
        self.items = [i for i in self.items if i.end_at > station_time - 30]
        live = {i.id for i in self.items}
        self._fades = {k: v for k, v in self._fades.items() if k in live}
        self._plans = {k: v for k, v in self._plans.items() if k in live}
        self._rows = {k: v for k, v in self._rows.items() if k in live}
        self._pending_ducks = {k: v for k, v in self._pending_ducks.items()
                               if k in live}

    # -- placement -------------------------------------------------------
    def crossfade_for(self, outgoing: Item | None, incoming_duration: float) -> float:
        cfg = config.station
        length = float(cfg.get("crossfade.duration", 6.0) or 0.0)
        if outgoing is None or length <= 0:
            return 0.0
        cap = float(cfg.get("crossfade.max_fraction_of_track", 0.25) or 0.25)
        shortest = min(outgoing.duration, incoming_duration)
        return max(0.0, min(length, shortest * cap))

    def add_music(self, url: str, track: dict[str, Any], *,
                  dry_before: bool = False, offset: float = 0.0,
                  entry_locked: bool = False, earliest_start: float = 0.0) -> Item:
        """Place a song, crossfading into whatever is already playing."""
        cfg = config.station
        duration = float(track.get("duration") or 0.0)
        if not math.isfinite(duration) or duration <= 0:
            raise ValueError("track has no measured duration")
        if not math.isfinite(offset) or not 0 <= offset < duration:
            raise ValueError("track offset is outside the source")

        # Trim trailing dead air so the crossfade lands on music, not silence.
        if cfg.get("crossfade.detect_cold_end", True):
            outro = float(track.get("outro_sec") or 0.0)
            if outro > offset and duration - outro > 2.0:
                duration = min(duration, outro + 1.5)
        duration -= offset
        effective_track = dict(track)
        intro = track.get("intro_override")
        if intro is None:
            intro = track.get("intro_sec")
        if intro is not None:
            effective_track["intro_override"] = max(0.0, float(intro) - offset)

        previous = self._last_music
        plan: transitions.Plan | None = None
        rate = 1.0
        out_row = dict(self._last_row or {})
        previous_curve = []
        previous_rate = 1.0
        if previous is not None:
            previous_curve = previous.meta.get("rate_curve") or []
            initial = previous.meta.get("playback_rate", 1.0)
            previous_rate = playback.rate_at(previous_curve, previous.duration, initial)
            # On the constant tail, source = wall * tail_rate + accumulated
            # phase shift. Carry that shift into the next beat alignment.
            phase_shift = (playback.source_at(previous_curve, previous.duration, initial)
                           - previous.duration * previous_rate)
            out_row["bpm"] = float(out_row.get("bpm") or 0) * previous_rate
            out_row["beat_period"] = float(out_row.get("beat_period") or 0) / previous_rate
            out_row["beat_offset"] = (float(out_row.get("beat_offset") or 0)
                                      - previous.offset - phase_shift) / previous_rate
        def grid_ok(row):
            period = float(row.get("beat_period") or 0)
            residual = row.get("beat_residual_ms")
            confidence = row.get("bpm_confidence")
            return (math.isfinite(period) and period > 0 and residual is not None
                    and math.isfinite(float(residual))
                    and float(residual) <= float(cfg.get("transitions.beat_max_residual_ms", 30))
                    and (confidence is None or float(confidence) >= 0.25))
        trusted_grids = previous is not None and grid_ok(out_row) and grid_ok(track)
        if trusted_grids and not dry_before:
            rate, _ = transitions.tempo_match(out_row, track)
            rate = min(1.08, max(0.92, rate))
        duration /= rate
        effective_track["bpm"] = float(track.get("bpm") or 0) * rate
        if effective_track.get("intro_override") is not None:
            effective_track["intro_override"] /= rate

        if previous is not None and not dry_before:
            plan = transitions.choose(out_row, effective_track, self.rng)
            from . import mixplanner
            protected_until = max((voice.end_at for voice in self.items
                                   if voice.kind == "voice" and voice.start_at < previous.end_at
                                   and voice.end_at > previous.start_at), default=previous.start_at)
            # Never move a later transition into the already planned head of
            # this track, or through a host break that is already on the clock.
            previous_head = self._plans.get(previous.id)
            safe_start = max(earliest_start, previous.start_at +
                             (previous_head.overlap if previous_head else 0.35))
            if previous_curve:
                safe_start = max(safe_start, previous.start_at + previous_curve[-1][0])
            choice = mixplanner.refine(self._last_row or {}, track, plan,
                out_start=previous.start_at, out_offset=previous.offset,
                out_duration=previous.duration, out_rate=previous_rate,
                in_offset=offset, in_duration=duration, in_rate=rate,
                earliest_start=safe_start, protected_until=protected_until,
                entry_locked=entry_locked, out_curve=previous_curve,
                out_initial_rate=previous.meta.get("playback_rate", 1.0))
            plan = choice.plan
            previous.duration = choice.out_duration
            offset, duration = choice.in_offset, choice.in_duration
            if intro is not None:
                effective_track["intro_override"] = max(0.0, float(intro) - offset) / rate
            cap = float(cfg.get("crossfade.max_fraction_of_track", 0.25) or 0.25)
            # A slowed track that later recovers is shorter than its initial
            # fixed-rate estimate. Source length is a conservative lower bound
            # until the exact recovery curve is known below.
            incoming_cap_duration = (min(duration, duration * rate)
                                     if cfg.get("transitions.tempo_recovery", False) else duration)
            plan.overlap = max(0.0, min(plan.overlap,
                                        min(previous.duration, incoming_cap_duration) * cap,
                                        previous.end_at - earliest_start))
            if previous_curve:
                plan.overlap = min(plan.overlap, max(0.0, previous.duration - previous_curve[-1][0]))
                plan.reason += "; stable tail after tempo recovery"
            overlap = plan.overlap
            phrase_beats = int(cfg.get("transitions.phrase_beats", 4) or 0)
            if trusted_grids and phrase_beats > 0:
                phrase = out_row["beat_period"] * phrase_beats
                if overlap >= phrase:
                    overlap = math.floor(overlap / phrase) * phrase
                    plan.reason += f"; {phrase_beats}-beat phrasing"
            plan.overlap = overlap
        else:
            overlap = 0.0

        if previous is None:
            start = max(self.cursor, 0.0)
            fade_in = 0.35
        elif overlap > 0:
            start = previous.end_at - overlap
            if trusted_grids:
                in_grid = dict(track, beat_offset=float(track.get("beat_offset") or 0) - offset)
                nudge, why = transitions.beat_nudge(out_row, in_grid, start - previous.start_at, overlap, rate)
                # Never lengthen past the intro/track cap chosen above.
                if nudge < 0:
                    nudge += out_row["beat_period"]
                aligned = not why and nudge <= overlap * 0.25
                if aligned:
                    start += nudge
                overlap = previous.end_at - start
                plan.overlap = overlap
                if aligned:
                    plan.reason += "; beats aligned"
                if abs(rate - 1) > 0.00001:
                    plan.reason += f"; pitch {(rate - 1) * 100:+.1f}%"
            fade_in = overlap
        else:
            start = max(self.cursor, previous.end_at + 0.35, earliest_start)
            fade_in = 0.6
            plan = None

        # Beat quantization can change the overlap. Adapt the material-aware
        # controls only after that final timing, and only once (echo attenuation
        # must not compound each time the schedule is sealed).
        if plan is not None and cfg.get("transitions.adaptive_eq_fx", True):
            from . import mixplanner, structure
            outgoing_profile = structure.profile_for(self._last_row or {})
            incoming_profile = structure.profile_for(track)
            if (outgoing_profile.get("complete") and incoming_profile.get("complete")):
                source_end = previous.offset + playback.source_at(
                    previous_curve, previous.duration, previous.meta.get("playback_rate", 1.0))
                mixplanner.adapt(plan, outgoing_profile, incoming_profile,
                                 source_end, offset, previous_rate, rate)

        # Recover only after the overlap and a short stable hold. Recompute
        # duration by the integral so the source ends exactly on the clock.
        rate_curve = []
        if cfg.get("transitions.tempo_recovery", False):
            source_length = duration * rate
            rate_curve = playback.recovery(rate, source_length, overlap + 2.0,
                float(cfg.get("transitions.recovery_seconds", 30.0)))
            if rate_curve:
                duration = playback.wall_at(rate_curve, source_length, rate)
                if intro is not None:
                    effective_track["intro_override"] = playback.wall_at(
                        rate_curve, max(0.0, float(intro) - offset), rate)
                if plan is not None:
                    plan.reason += "; gradual tempo recovery after blend"

        fade_out = float(cfg.get("crossfade.duration", 6.0) or 0.0)
        fade_out = max(0.0, min(fade_out, duration * 0.4))

        item = Item(kind="music", url=url, start_at=start, duration=duration,
                    envelope=[], offset=offset, meta={
                        "title": track.get("title"), "artist": track.get("artist"),
                        "key": track.get("key"),
                        "selection_origin": dict(track.get("selection_origin") or {"by": "unknown"}),
                        "intro_sec": effective_track.get("intro_override"),
                        "playback_rate": rate,
                        "key_lock": bool(cfg.get("transitions.native_key_lock", False)),
                    })
        if rate_curve:
            item.meta["rate_curve"] = rate_curve
        if track.get("selection"):
            item.meta["selection"] = dict(track["selection"])
        # Envelope is finalised in `seal()` once we know what ducks it.
        self._fades[item.id] = (fade_in, fade_out)
        self._pending_ducks[item.id] = []
        self._rows[item.id] = dict(track)
        if plan is not None:
            self._plans[item.id] = plan
            item.meta["transition"] = plan.as_dict()
        item.meta["bpm"] = track.get("bpm") or 0
        item.meta["camelot"] = track.get("camelot") or ""

        self.items.append(item)
        self._last_music = item
        self._last_row = dict(track)
        self.cursor = item.end_at
        return item

    def add_voice(self, url: str, start_at: float, duration: float,
                  gain: float = 1.0, meta: dict[str, Any] | None = None) -> Item:
        item = Item(kind="voice", url=url, start_at=start_at, duration=duration,
                    envelope=flat_envelope(duration, gain), meta=meta or {})
        self.items.append(item)
        return item

    def duck(self, music: Item, start_at: float, end_at: float) -> None:
        """Register that something is talking over this music item."""
        window = _Duck(start=max(0.0, start_at - music.start_at),
                       end=min(music.duration, end_at - music.start_at))
        if window.end > window.start:
            self._pending_ducks.setdefault(music.id, []).append(window)

    def duck_all_overlapping(self, start_at: float, end_at: float) -> None:
        """Duck every music item that overlaps this speech window.

        A break that spans a crossfade is talking over two songs at once, and
        both of them have to come down.
        """
        for item in self.items:
            if item.kind != "music":
                continue
            if item.start_at < end_at and item.end_at > start_at:
                self.duck(item, start_at, end_at)

    def seal(self) -> None:
        """Bake every music envelope.

        Safe to call repeatedly: fades and ducks are held separately from the
        item, so re-sealing after a later record is added rebuilds the same
        envelope rather than flattening the earlier one.
        """
        music = sorted([i for i in self.items if i.kind == "music"],
                       key=lambda i: i.start_at)

        for index, item in enumerate(music):
            ducks = list(self._pending_ducks.get(item.id, []))
            # A later append can overlap speech already on the clock. Rebuild
            # from voice items so BOTH decks duck, regardless of placement order.
            hold = float(config.station.get("ducking.hold_after", 0.4))
            attack = float(config.station.get("ducking.attack", 0.35))
            release = float(config.station.get("ducking.release", 1.2))
            ducks.extend(_Duck(v.start_at - item.start_at, v.end_at - item.start_at)
                         for v in self.items if v.kind == "voice"
                         and v.start_at - attack < item.end_at
                         and v.end_at + hold + release > item.start_at)
            merged = []
            for window in sorted(ducks, key=lambda d: d.start):
                if merged and window.start <= merged[-1].end + hold + attack:
                    merged[-1].end = max(merged[-1].end, window.end)
                else:
                    merged.append(_Duck(window.start, window.end))
            ducks = merged
            plan_in = self._plans.get(item.id)
            # The transition INTO the next record is the one this record is
            # the outgoing side of.
            plan_out = (self._plans.get(music[index + 1].id)
                        if index + 1 < len(music) else None)

            head = tail = None
            head_at = tail_at = 0.0

            if plan_in:
                _, incoming = transitions.render(plan_in, plan_in.overlap)
                head, head_at = incoming, 0.0
            item.meta.pop("echo", None)
            if plan_out:
                outgoing, _ = transitions.render(plan_out, plan_out.overlap)
                tail = outgoing
                tail_at = max(0.0, item.duration - plan_out.overlap)
                if plan_out.echo_mix > 0:
                    bpm = float(item.meta.get("bpm") or 120) * playback.rate_at(
                        item.meta.get("rate_curve"), tail_at, item.meta.get("playback_rate", 1))
                    item.meta["echo"] = {"start": tail_at + plan_out.overlap * plan_out.echo_start, "end": item.duration,
                                         "seconds": 60 / max(40, bpm) * plan_out.echo_beats,
                                         "mix": plan_out.echo_mix, "feedback": plan_out.echo_feedback}
                else:
                    item.meta.pop("echo", None)

            if head or tail:
                item.meta["deck_envelope"] = build_gain_envelope(
                    item.duration, head.gain if head else None, head_at,
                    tail.gain if tail else None, tail_at, [])
                item.envelope = build_gain_envelope(
                    item.duration,
                    head.gain if head else None, head_at,
                    tail.gain if tail else None, tail_at,
                    ducks)
                automation: dict[str, list[list[float]]] = {}
                for band, default in (("low", 0.0), ("mid", 0.0), ("high", 0.0)):
                    points = build_parameter(
                        item.duration,
                        getattr(head, band) if head else None, head_at,
                        getattr(tail, band) if tail else None, tail_at, default)
                    if points:
                        automation[band] = points
                for filt, default in (("lpf", transitions.LPF_OPEN),
                                      ("hpf", transitions.HPF_OPEN)):
                    points = build_parameter(
                        item.duration,
                        getattr(head, filt) if head else None, head_at,
                        getattr(tail, filt) if tail else None, tail_at, default)
                    if points:
                        automation[filt] = points
                item.meta["automation"] = automation
            else:
                # First or last record of a run, or a dry break either side of
                # it: no transition to render, just the plain fade.
                fade_in, fade_out = self._fades.get(item.id, (0.0, 0.0))
                item.envelope = build_music_envelope(
                    item.duration, fade_in, fade_out, ducks)
                item.meta["deck_envelope"] = build_music_envelope(
                    item.duration, fade_in, fade_out, [])
                item.meta.pop("automation", None)
            item.meta["ducking"] = config.station.get("ducking", {}) or {}
            item.meta["playback_feedback"] = {
                "enabled": bool(config.station.get("transitions.feedback_enabled", True)),
                "max_adjustment": float(config.station.get("transitions.feedback_max_adjustment", 0.005)),
                "tolerance_ms": float(config.station.get("transitions.feedback_tolerance_ms", 30.0)),
            }


# --------------------------------------------------------------------------
# Break placement
# --------------------------------------------------------------------------
@dataclass
class VoiceLine:
    """One rendered host line, waiting to be placed."""
    url: str
    duration: float
    host: str
    text: str
    gain: float = 1.0
    reference: dict | None = None


def lay_out_lines(lines: list[VoiceLine], start_at: float,
                  rng: Any) -> list[tuple[VoiceLine, float]]:
    """Assign each line a start time, occasionally overlapping for interruptions."""
    cfg = config.station
    gap = float(cfg.get("talk_placement.line_gap.default", 0.18) or 0.0)
    chance = float(cfg.get("talk_placement.line_gap.interrupt_chance", 0.22) or 0.0)
    overlap = float(cfg.get("talk_placement.line_gap.interrupt_overlap", 0.35) or 0.0)

    placed: list[tuple[VoiceLine, float]] = []
    cursor = start_at
    for index, line in enumerate(lines):
        if index > 0 and rng.random() < chance:
            # Cut in over the tail of the previous line, but never so far back
            # that we start before that line has meaningfully begun.
            previous_start = placed[-1][1]
            cursor = max(cursor - (gap + overlap), previous_start + 0.25)
        placed.append((line, cursor))
        cursor += line.duration + gap
    return placed


def viable(placement: str, *, intro: float, safety: float, speech_length: float,
           music_start: float, previous_end: float | None,
           previous_start: float | None) -> bool:
    """Can this break style hold this much speech without hitting the vocal?

    `over_intro` is confined to the intro, so a nine-second break cannot go
    over a three-second intro. `over_outro` has to finish before the outgoing
    record ends AND before the incoming vocal, which during a crossfade is
    often the tighter of the two. `bridge` and `dry` can always be made to fit,
    which is why they are the fallbacks.
    """
    if placement == "over_intro":
        return speech_length <= max(0.0, intro - safety)

    if placement == "over_outro":
        if previous_end is None:
            return False
        deadline = min(previous_end - 1.0, music_start + intro - safety)
        earliest = (previous_start + 5.0) if previous_start is not None \
            else deadline - speech_length
        return (deadline - earliest) >= speech_length

    return True


def choose_placement(preferred: str, *, intro: float, safety: float,
                     speech_length: float, music_start: float,
                     previous_end: float | None,
                     previous_start: float | None) -> str:
    """Take the rolled style, or the nearest one that actually fits.

    A DJ picks the treatment the record allows. So does this.
    """
    if previous_end is None:
        return "dry"
    if viable(preferred, intro=intro, safety=safety, speech_length=speech_length,
              music_start=music_start, previous_end=previous_end,
              previous_start=previous_start):
        return preferred
    # bridge can back-time, so it absorbs anything the tighter styles cannot.
    return "bridge"


def break_start(placement: str, *, music_start: float, intro: float,
                safety: float, speech_length: float,
                previous_end: float | None = None,
                previous_start: float | None = None,
                allow_backtime: bool = True,
                max_backtime: float = 20.0) -> float:
    """When the hosts must start talking, for this break to land correctly.

    The rule that matters is "hit the post": the last word should finish just
    as the vocal arrives. Since we know the speech length and the measured
    intro, the start time is simply back-timed from the vocal entry. If the
    break is longer than the intro can hold, it reaches back over the outgoing
    record rather than talking through the singer.
    """
    post = music_start + intro - safety   # the moment the vocal arrives

    if placement == "over_outro" and previous_end is not None:
        # Land the break before the outgoing record is gone -- but the records
        # overlap during a crossfade, so the incoming vocal can arrive first.
        # Whichever comes sooner is the real deadline.
        target_end = min(previous_end - 1.0, post)
        start = target_end - speech_length
        floor = previous_start + 5.0 if previous_start is not None else start
        return max(start, floor)

    if placement == "dry":
        return (previous_end + 0.6) if previous_end is not None else music_start

    # bridge / over_intro
    start = post - speech_length

    floor = music_start
    if placement == "bridge" and allow_backtime:
        floor = music_start - max_backtime
        if previous_start is not None:
            # Never reach so far back that we talk over the previous intro too.
            floor = max(floor, previous_start + 5.0)
    return max(start, floor)


def speech_span(placed: list[tuple[VoiceLine, float]]) -> tuple[float, float]:
    if not placed:
        return (0.0, 0.0)
    start = min(offset for _, offset in placed)
    end = max(offset + line.duration for line, offset in placed)
    return (start, end)
