"""Exact source/wall-time conversion for piecewise-linear playback rates.

Curve points are [seconds since item start, source seconds per wall second].
Rate stays constant outside the points. Source positions exclude item.offset.
"""
from __future__ import annotations

import math


def points(curve, initial=1.0) -> list[list[float]]:
    """Reject malformed automation as a whole instead of producing a bad seek."""
    if not math.isfinite(initial) or initial <= 0:
        initial = 1.0
    fallback = [[0.0, initial]]
    if not curve:
        return fallback
    try:
        result = [[float(t), float(rate)] for t, rate in curve]
        if (len(result) > 64 or result[0][0] != 0
                or any(not math.isfinite(t) or not math.isfinite(rate) or rate <= 0
                       for t, rate in result)
                or any(a[0] >= b[0] for a, b in zip(result, result[1:]))):
            return fallback
        return result
    except (ValueError, TypeError, IndexError):
        return fallback


def rate_at(curve, wall_seconds: float, initial=1.0) -> float:
    values = points(curve, initial)
    t = max(0.0, wall_seconds)
    for (start, a), (end, b) in zip(values, values[1:]):
        if t <= end:
            return a + (b - a) * (t - start) / (end - start)
    return values[-1][1]


def source_at(curve, wall_seconds: float, initial=1.0) -> float:
    """Integral of rate from zero through wall_seconds."""
    values = points(curve, initial)
    t, source = max(0.0, wall_seconds), 0.0
    for (start, a), (end, b) in zip(values, values[1:]):
        elapsed = min(t, end) - start
        if elapsed <= 0:
            return source
        slope = (b - a) / (end - start)
        source += a * elapsed + slope * elapsed * elapsed / 2
        if t <= end:
            return source
    return source + max(0, t - values[-1][0]) * values[-1][1]


def wall_at(curve, source_seconds: float, initial=1.0) -> float:
    """Inverse integral, using a stable quadratic root within each rate ramp."""
    values = points(curve, initial)
    remaining = max(0.0, source_seconds)
    for (start, a), (end, b) in zip(values, values[1:]):
        span = end - start
        consumed = (a + b) * span / 2
        if remaining <= consumed:
            slope = (b - a) / span
            if abs(slope) < 1e-12:
                return start + remaining / a
            # Rationalized positive root avoids cancellation for gentle ramps.
            elapsed = 2 * remaining / (a + math.sqrt(max(0, a * a + 2 * slope * remaining)))
            return start + min(span, max(0, elapsed))
        remaining -= consumed
    return values[-1][0] + remaining / values[-1][1]


def recovery(initial: float, source_length: float, hold: float, seconds: float,
             tail_reserve: float = 12.0) -> list[list[float]]:
    """Hold through the blend, then recover; short tracks keep a stable tempo.

    Reserve stable tail time for the next mix. The timeline additionally caps
    that next overlap so a recovery ramp never runs under a beat-matched blend.
    """
    if (not all(math.isfinite(v) for v in (initial, source_length, hold, seconds, tail_reserve))
            or not .92 <= initial <= 1.08 or abs(initial - 1) < 1e-5 or source_length <= 0):
        return []
    hold, seconds = max(0.0, hold), max(5.0, seconds)
    curve = [[0.0, initial]]
    if hold > 0:
        curve.append([hold, initial])
    curve.append([hold + seconds, 1.0])
    if source_at(curve, hold + seconds) + max(0.0, tail_reserve) > source_length:
        return []
    return curve
