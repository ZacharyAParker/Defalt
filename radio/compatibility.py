"""Bounded, explainable continuity hints, never catalogue admission rules.

Genre and artist evidence comes from tags/credits. Lyrics use only actual
embedded text: a deliberately small word/theme model, not guessed song lore.
Unknown evidence is neutral and no text is sent to an external service.
"""
from __future__ import annotations

import json
import math
import random
import re
import threading
import time
from functools import lru_cache
from typing import Any

from . import analysis, config, db, transitions

_THEMES = {
    "affection": {"love", "lover", "loving", "kiss", "kisses", "heart", "darling", "beloved"},
    "loss": {"lonely", "alone", "tears", "cry", "crying", "goodbye", "missing", "grief", "heartbreak"},
    "celebration": {"party", "dance", "dancing", "celebrate", "celebration", "club", "cheers"},
    "resilience": {"survive", "survivor", "strong", "stronger", "rise", "rising", "overcome", "courage"},
    "journey": {"road", "highway", "travel", "journey", "train", "miles", "horizon"},
    "struggle": {"fight", "fighting", "war", "battle", "chains", "prison", "freedom", "justice"},
    "reflection": {"remember", "memories", "memory", "yesterday", "childhood", "regret", "past"},
}
_STOP = set("the and that this with from your you you're i'm i've they them there their then when what where who why how would could should have has had been are was were not don't just yeah ooh oh hey la na baby gonna wanna can will all for but into its our out now one too let get got like it's she her him his he's she's did does ain't still very".split())
_ALIASES = {"hiphop": "hip hop", "hip-hop": "hip hop", "r&b": "rnb", "rhythm and blues": "rnb", "d&b": "drum and bass", "dnb": "drum and bass", "edm": "electronic"}
_FAMILIES = {
    "electronic": {"house", "techno", "trance", "dubstep", "drum and bass", "electronic", "garage"},
    "rock": {"rock", "metal", "punk", "grunge", "shoegaze"},
    "hip hop": {"hip hop", "rap", "trap", "drill"},
    "soul": {"soul", "rnb", "funk", "neo soul"},
    "folk": {"folk", "country", "bluegrass", "americana"},
}
_DEFAULT_SETTINGS = {
    "enabled": True, "genre_weight": .65, "artist_weight": .25,
    "lyrics_weight": .45, "mix_weight": .35, "energy_weight": .3,
    "energy_direction": "follow", "fatigue_after": 4, "variety_strength": .65,
    "explore_chance": .18, "history_size": 10, "lookahead_enabled": True,
    "lookahead_weight": .3, "lookahead_candidates": 16, "lookahead_depth": 3,
    "energy_arc_tracks": 3, "energy_step_lufs": 2.0,
    "energy_arc_enabled": True, "energy_arc_weight": .4, "energy_arc_break_build": .04,
    "energy_arc_hours": None,
}
# The hour-level shape a set follows when nothing else says otherwise: ease
# through the small hours, build across the morning, peak into the evening.
# Levels are perceived energy (0..1), not loudness.
DEFAULT_ARC = {0: .38, 1: .34, 2: .3, 3: .28, 4: .28, 5: .32, 6: .4, 7: .46, 8: .5, 9: .52,
               10: .55, 11: .56, 12: .56, 13: .56, 14: .58, 15: .6, 16: .62, 17: .64,
               18: .66, 19: .68, 20: .68, 21: .64, 22: .55, 23: .46}
# One LU of the old loudness step is this much perceived energy.
ENERGY_PER_LU = .05


def snapshot() -> dict:
    """Refresh configuration once per selection; never stat files per pair."""
    # Read the complete override generation once: separate get() calls could
    # straddle an Apply and combine old and new values in the same selection.
    data = config.station.data()
    selection = data.get("selection") or {}
    settings = selection.get("compatibility") or {}
    values = {key: settings.get(key, default) for key, default in _DEFAULT_SETTINGS.items()}
    values["artist_separation"] = selection.get("artist_separation", 6)
    # Mix facts the pair scoring needs, read once rather than per pair.
    mixing = data.get("transitions") or {}
    values["key_lock"] = bool(mixing.get("native_key_lock", False))
    values["tempo_match"] = bool(mixing.get("tempo_match", True))
    values["tempo_match_limit"] = mixing.get("tempo_match_limit", .06)
    values["key_shift_tempo_tolerance"] = mixing.get("key_shift_tempo_tolerance", .02)
    return values


def setting(name: str, default: float, low: float = 0, high: float = 1,
            settings: dict | None = None) -> float:
    try:
        value = float(settings.get(name, default) if settings is not None
                      else config.station.get(f"selection.compatibility.{name}", default))
        return min(high, max(low, value)) if math.isfinite(value) else default
    except (TypeError, ValueError):
        return default


@lru_cache(maxsize=4096)
def genres(text: str) -> frozenset[str]:
    tags = set()
    for tag in re.split(r"[;,/|]", text.lower()):
        tag = _ALIASES.get(tag.strip(), tag.strip())
        tag = re.sub(r"[-_]", " ", tag)
        if tag and tag not in {"unknown", "other", "none"}:
            tags.add(tag)
    return frozenset(tags)


@lru_cache(maxsize=8192)
def _genre_fit(first: str, second: str) -> float | None:
    left, right = genres(first), genres(second)
    if not left or not right:
        return None
    if left & right:
        return 1.0
    families = lambda tags: {family for family, words in _FAMILIES.items()
                             if any(re.search(r"\b" + re.escape(word) + r"\b", tag)
                                    for word in words for tag in tags)}
    return 0.7 if families(left) & families(right) else 0.0


def genre_fit(a: Any, b: Any) -> float | None:
    return _genre_fit(str(db.field(a, "genre") or ""), str(db.field(b, "genre") or ""))


@lru_cache(maxsize=4096)
def artists(text: str) -> frozenset[str]:
    primary = db.primary_artist(text)
    result = {db.norm(primary)} if primary else set()
    # Retain protected band names and comma suffixes such as Tyler, The Creator.
    remainder = text[len(primary):]
    for credit in re.split(r",|\s+(?:feat\.?|ft\.?|with|x|&)\s+", remainder, flags=re.I):
        credit = db.norm(credit.strip(" ()[]&"))
        if credit:
            result.add(credit)
    return frozenset(result - {"unknown artist", "unknown"})


@lru_cache(maxsize=16384)
def _primary(text: str) -> str:
    return db.norm(db.primary_artist(text))


@lru_cache(maxsize=16384)
def lyric_features(text: str) -> tuple[frozenset[str], frozenset[str]]:
    text = re.sub(r"\[[^\]]*\]", " ", text[:50000].lower())
    words = frozenset(w for w in re.findall(r"[^\W\d_]+", text, flags=re.UNICODE)
                      if len(w) > 2 and w not in _STOP)
    if len(words) < 8:
        return frozenset(), frozenset()
    # Two distinct cues avoids declaring a whole theme from a single 'heart'.
    themes = frozenset(name for name, cues in _THEMES.items() if len(words & cues) >= 2)
    return words, themes


def lyrics_fit(a: Any, b: Any) -> tuple[float | None, list[str]]:
    left, lt = lyric_features(str(db.field(a, "lyrics") or ""))
    right, rt = lyric_features(str(db.field(b, "lyrics") or ""))
    if not left or not right:
        return None, []
    shared = sorted(lt & rt)
    lexical = len(left & right) / math.sqrt(len(left) * len(right))
    thematic = len(lt & rt) / max(1, len(lt | rt))
    # A low word overlap is weak evidence, not a claim of opposed meaning.
    return min(1.0, 0.35 + 0.4 * lexical + 0.25 * thematic), shared


def _number(track: Any, key: str) -> float | None:
    try:
        value = float(track.get(key) if type(track) is dict else db.field(track, key))
        return value if math.isfinite(value) else None
    except (TypeError, ValueError):
        return None


def mix_fit(a: Any, b: Any, settings: dict | None = None) -> float | None:
    """How well two records mix: tempo, key as it will actually sound, energy
    and timbre. Unknown evidence is left out rather than guessed."""
    settings = settings or {}
    values = []
    ab, bb = _number(a, "bpm"), _number(b, "bpm")
    ac, bc = _number(a, "bpm_confidence"), _number(b, "bpm_confidence")
    rate = 1.0
    tempo_trusted = bool(ab and bb and ab > 0 and bb > 0 and min(ac or 0, bc or 0) >= .25)
    if tempo_trusted:
        distance = min(abs(ab / (bb * factor) - 1) for factor in (.5, 1, 2))
        values.append(max(0, 1 - distance / .25))
        limit = setting("tempo_match_limit", .06, 0, .2, settings)
        if settings.get("tempo_match", True):
            best = min((ab / (bb * factor) for factor in (.5, 1, 2)), key=lambda r: abs(r - 1))
            if abs(best - 1) <= limit:
                rate = best
    ak, bk = str(db.field(a, "camelot") or ""), str(db.field(b, "camelot") or "")
    if (re.fullmatch(r"(?:[1-9]|1[0-2])[AB]", ak) and re.fullmatch(r"(?:[1-9]|1[0-2])[AB]", bk)
            and min(_number(a, "key_confidence") or 0, _number(b, "key_confidence") or 0) >= .25):
        # A beat-matched pair plays the incoming record at `rate`, which
        # moves its key unless key lock is on. Judge the key it will have,
        # after the planner's own harmonic adjustment.
        key_lock = bool(settings.get("key_lock", False))
        played = rate
        if tempo_trusted and rate != 1.0:
            played, _ = transitions.harmonic_choice(
                ak, bk, rate, key_lock=key_lock,
                limit=setting("tempo_match_limit", .06, 0, .2, settings),
                tolerance=setting("key_shift_tempo_tolerance", .02, 0, .06, settings))
        heard = bk if key_lock else (transitions.shift_camelot(bk, transitions.semitones(played)) or bk)
        values.append(1.0 if analysis.keys_compatible(ak, heard) else .2)
    ae, be = _number(a, "energy"), _number(b, "energy")
    if ae is not None and be is not None:
        values.append(max(0, 1 - abs(ae - be) / .6))
    similar = similarity(a, b)
    if similar is not None:
        values.append(similar)
    return sum(values) / len(values) if values else None


# --------------------------------------------------------------------------
# Audio similarity
# --------------------------------------------------------------------------
_STATS_LOCK = threading.Lock()
_STATS: dict[str, Any] = {"at": 0.0, "mean": None, "scale": None}


@lru_cache(maxsize=8192)
def _vector(text: str) -> tuple[float, ...] | None:
    try:
        values = json.loads(text)
    except (TypeError, ValueError):
        return None
    if (not isinstance(values, list) or len(values) != analysis.EMBEDDING_SIZE
            or not all(isinstance(v, (int, float)) and math.isfinite(v) for v in values)):
        return None
    return tuple(float(v) for v in values)


def _library_stats() -> tuple[list[float], list[float]] | None:
    """Per-dimension centre and spread across the library, refreshed every
    ten minutes, so no single raw feature dominates the cosine."""
    with _STATS_LOCK:
        if _STATS["mean"] is not None and time.time() - _STATS["at"] < 600:
            return _STATS["mean"], _STATS["scale"]
    try:
        rows = db.query("SELECT embedding FROM tracks WHERE embedding IS NOT NULL LIMIT 5000")
    except Exception:  # noqa: BLE001 - an old database without the column
        rows = []
    vectors = [v for v in (_vector(row["embedding"]) for row in rows) if v]
    if len(vectors) < 8:
        mean, scale = [0.0] * analysis.EMBEDDING_SIZE, [1.0] * analysis.EMBEDDING_SIZE
    else:
        count = len(vectors)
        mean = [sum(v[i] for v in vectors) / count for i in range(analysis.EMBEDDING_SIZE)]
        scale = [max(.05, math.sqrt(sum((v[i] - mean[i]) ** 2 for v in vectors) / count))
                 for i in range(analysis.EMBEDDING_SIZE)]
    with _STATS_LOCK:
        _STATS.update(at=time.time(), mean=mean, scale=scale)
    return mean, scale


@lru_cache(maxsize=16384)
def _unit(text: str, generation: float) -> tuple[float, ...] | None:
    """A stored embedding standardised against the library and normalised."""
    vector = _vector(text)
    if not vector:
        return None
    mean, scale = _library_stats()
    values = [(v - m) / s for v, m, s in zip(vector, mean, scale)]
    norm = math.sqrt(sum(v * v for v in values))
    return tuple(v / norm for v in values) if norm > 1e-12 else None


def similarity(a: Any, b: Any) -> float | None:
    """0..1 timbral/rhythmic likeness from stored embeddings, else None."""
    first, second = db.field(a, "embedding"), db.field(b, "embedding")
    if not first or not second:
        return None
    _library_stats()
    generation = _STATS["at"]
    x, y = _unit(str(first), generation), _unit(str(second), generation)
    if not x or not y:
        return None
    cosine = sum(p * q for p, q in zip(x, y))
    return max(0.0, min(1.0, (cosine + 1) / 2))


def energy_target(history: list[dict], settings: dict) -> tuple[float, str]:
    """A bounded energy trajectory, recomputed for each hypothetical route.

    Energy is the analysed perceived-energy score (0..1); the configured step
    is still expressed in LU for existing settings, at ENERGY_PER_LU each.
    Wave changes direction after a measured rise/fall rather than counting
    songs with missing measurements as a completed arc. It never labels mood.
    """
    direction = settings.get("energy_direction", "follow")
    step = setting("energy_step_lufs", 2, .5, 4, settings) * ENERGY_PER_LU
    if direction != "wave":
        return {"build": step, "ease": -step}.get(direction, 0), direction
    span = int(setting("energy_arc_tracks", 3, 2, 6, settings))
    measured = []
    for track in history[-(span + 1):]:
        value = _number(track, "energy")
        if value is None:
            return 0.0, "wave awaiting energy history"
        measured.append(value)
    if len(measured) < 2:
        return 0.0, "wave awaiting energy history"
    changes = [b - a for a, b in zip(measured, measured[1:])]
    moving = [delta for delta in changes if abs(delta) >= .0125]
    if not moving:
        return step, "wave building from a plateau"
    sign = 1 if moving[-1] > 0 else -1
    run = 0
    for delta in reversed(changes):
        if delta * sign < .0125:
            break
        run += 1
    if run >= span:
        sign *= -1
    return step * sign, "wave building" if sign > 0 else "wave easing"


def energy_fit(a: Any, b: Any, settings: dict | None = None,
               history: list[dict] | None = None) -> float | None:
    """Configured energy trajectory from analysed perceived energy.

    Loudness is deliberately not used: stored LUFS mix pre- and post-
    normalisation measurements, and a quiet master is not a calm song.
    """
    outgoing, incoming = _number(a, "energy"), _number(b, "energy")
    if outgoing is None or incoming is None:
        return None
    settings = snapshot() if settings is None else settings
    direction = str(settings.get("energy_direction", "follow"))
    difference = incoming - outgoing
    target, _ = energy_target(history or [], settings)
    distance = abs(abs(difference) - .2) if direction == "surprise" else abs(difference - target)
    return max(0.0, 1.0 - distance / .4)


def arc_level(settings: dict, position: int = 0, hour: int | None = None) -> float | None:
    """The energy the set should sit at `position` songs from now.

    An hour-level shape (configurable per hour), plus a gentle build across
    the songs after each host break, capped so it never runs away.
    """
    if not settings.get("enabled", True) or not settings.get("energy_arc_enabled", True):
        return None
    hour = time.localtime().tm_hour if hour is None else hour
    hours = settings.get("energy_arc_hours") or {}
    try:
        level = float(hours.get(hour, hours.get(str(hour), DEFAULT_ARC[hour % 24])))
    except (TypeError, ValueError, AttributeError):
        level = DEFAULT_ARC[hour % 24]
    since = settings.get("break_position")
    if isinstance(since, int) and since >= 0:
        build = setting("energy_arc_break_build", .04, 0, .1, settings)
        level += min(3, since + position) * build - build
    return max(0.0, min(1.0, level))


def arc_penalty(route: list[dict], settings: dict, start: int = 0) -> float:
    """Mean distance of a route's known energies from the arc (0 if unknown)."""
    gaps = []
    for index, track in enumerate(route):
        level = arc_level(settings, start + index)
        energy = _number(track, "energy")
        if level is not None and energy is not None:
            gaps.append(abs(energy - level))
    return sum(gaps) / len(gaps) if gaps else 0.0


def evaluate(track: dict, previous: dict | None, history: list[dict],
             settings: dict | None = None, pair_cache: dict | None = None,
             explain: bool = True) -> dict:
    """A finite positive multiplier and honest evidence for a proposed pair."""
    settings = snapshot() if settings is None else settings
    if not previous or not settings.get("enabled", True):
        return {"multiplier": 1.0, "reason": "Taste and rotation", "evidence": {}}
    # Cache pair facts only within this batch. Fatigue/repetition below still
    # uses each route's full context, and the next selection sees fresh config.
    pair_key = (id(previous), id(track))
    facts = pair_cache.get(pair_key) if pair_cache is not None else None
    if facts is None:
        genre = genre_fit(previous, track)
        left, right = artists(str(previous.get("artist") or "")), artists(str(track.get("artist") or ""))
        artist = (1.0 if left & right else .5) if left and right else None
        lyric, themes = lyrics_fit(previous, track)
        mix = mix_fit(previous, track, settings)
        evidence = {"genre": genre, "artist": artist, "lyrics": lyric, "mix": mix}
        defaults = {"genre": .65, "artist": .25, "lyrics": .45, "mix": .35}
        signal = sum(setting(f"{name}_weight", defaults[name], 0, 2, settings) * (value - .5)
                     for name, value in evidence.items() if value is not None)
        facts = genre, left, right, lyric, themes, mix, evidence, signal
        if pair_cache is not None:
            pair_cache[pair_key] = facts
    genre, left, right, lyric, themes, mix, evidence, signal = facts
    # Energy depends on the route history, so it must not enter the pair cache.
    energy = energy_fit(previous, track, settings, history)
    evidence = {**evidence, "energy": energy}
    if energy is not None:
        signal += setting("energy_weight", .3, 0, 2, settings) * (energy - .5)

    # A sustained comparable run gradually changes the goal from continuity
    # toward contrast. Unknown tags never count as 'same vibe'. The run does
    # not depend on the candidate, so a batch computes it once per context.
    run_key = ("run", id(previous), tuple(id(t) for t in history))
    run = pair_cache.get(run_key) if pair_cache is not None else None
    if run is None:
        run = 0
        for recent in reversed(history):
            similar = genre_fit(previous, recent)
            same_artist = bool(left & artists(str(recent.get("artist") or "")))
            if (similar is not None and similar >= .7) or same_artist or lyrics_fit(previous, recent)[1]:
                run += 1
            else:
                break
        if pair_cache is not None:
            pair_cache[run_key] = run
    threshold = int(setting("fatigue_after", 4, 2, 20, settings))
    fatigue = min(1.0, max(0.0, (run - threshold + 1) / threshold))
    variety = setting("variety_strength", .65, settings=settings)
    signal *= 1 - fatigue * variety
    if genre is not None:
        signal += fatigue * variety * (.5 - genre)
    recent_credits = [artists(str(t.get("artist") or "")) for t in history[-4:]]
    repeat = sum(bool(right & credits) for credits in recent_credits)
    signal -= variety * .2 * repeat
    multiplier = math.exp(max(-1.5, min(1.5, signal)))
    if not explain:
        return {"multiplier": multiplier}
    reasons = []
    if genre is not None and genre >= .7:
        reasons.append("related genre tags")
    if left & right:
        reasons.append("shared artist credit")
    if themes:
        reasons.append("lyric cues: " + ", ".join(themes))
    elif lyric is not None and lyric > .55:
        reasons.append("shared lyric vocabulary")
    if mix is not None and mix >= .7:
        reasons.append("compatible tempo/key/energy evidence")
    if energy is not None and setting("energy_weight", .3, settings=settings) > 0:
        _, direction = energy_target(history, settings)
        reasons.append(f"{direction} energy")
    if fatigue:
        reasons.append("variety after a similar run")
    if repeat:
        reasons.append("recent artist penalty")
    return {"multiplier": multiplier,
            "reason": "; ".join(reasons) or "Taste and rotation; limited matching evidence",
            "evidence": evidence, "similar_run": run,
            "previous_key": previous.get("key")}


def lookahead(scored: list[tuple[float, dict]], history: list[dict],
              settings: dict | None = None) -> list[tuple[float, dict]]:
    """Soft multi-song route score over a bounded beam, with no database work.

    Half the shortlist follows taste scores; half explores the remaining
    catalogue. Tracks outside it retain their full original sampling weight.
    A route is a feasibility hint, never a reservation or queue rewrite.
    """
    settings = snapshot() if settings is None else settings
    if (len(scored) < 2 or not settings.get("enabled", True)
            or not settings.get("lookahead_enabled", True)):
        return scored
    strength = setting("lookahead_weight", .3, settings=settings)
    if strength <= 0:
        return scored
    limit = int(setting("lookahead_candidates", 16, 4, 32, settings))
    depth = int(setting("lookahead_depth", 3, 1, 4, settings))
    ranked = sorted(scored, key=lambda entry: entry[0], reverse=True)
    if len(ranked) > limit:
        keep = limit // 2
        ranked = ranked[:keep] + random.sample(ranked[keep:], limit - keep)
    shortlist = [track for _, track in ranked]
    strongest = max(weight for weight, _ in ranked)
    # A route through songs the listener is unlikely to want is a weak bridge.
    # Retain the taste/vibe weights already calculated for this selection.
    preference = {track["key"]: max(-.35, .25 * math.log(max(.001, weight) / max(.001, strongest)))
                  for weight, track in ranked}
    separation = max(0, int(settings.get("artist_separation", 6) or 0))

    def separated(candidate: dict, context: list[dict]) -> bool:
        if not separation:
            return True
        artist = _primary(candidate.get("artist") or "")
        return not artist or all(artist != _primary(t.get("artist") or "")
                                 for t in context[-separation:])

    updates = {}
    pair_cache = {}
    arc_weight = setting("energy_arc_weight", .4, 0, 2, settings) if arc_level(settings) is not None else 0.0
    levels = [arc_level(settings, position) for position in range(depth + 2)] if arc_weight else []

    def off_arc(track: dict, position: int) -> float:
        energy = _number(track, "energy")
        level = levels[position] if position < len(levels) else None
        return abs(energy - level) if energy is not None and level is not None else 0.0

    for current in shortlist:
        context = history + [current]
        # The candidate itself sits at arc position 0; its route follows.
        own_arc = arc_weight * off_arc(current, 0) if arc_weight else 0.0
        routes = [(0.0, [])]
        for _ in range(min(depth, len(shortlist) - 1)):
            expanded = []
            for cumulative, route in routes:
                previous = route[-1] if route else current
                route_context = context + route
                used = {current["key"], *(t["key"] for t in route)}
                for following in shortlist:
                    if following["key"] in used or not separated(following, route_context):
                        continue
                    score = math.log(evaluate(following, previous, route_context,
                                              settings, pair_cache, False)["multiplier"])
                    score += preference[following["key"]]
                    if arc_weight:
                        score -= arc_weight * off_arc(following, len(route) + 1)
                    expanded.append((cumulative + score, route + [following]))
            if not expanded:
                break
            expanded.sort(key=lambda entry: entry[0], reverse=True)
            routes = expanded[:3]
        routes = [(score / len(route), route) for score, route in routes if route]
        if not routes:
            continue
        routes.sort(key=lambda entry: entry[0], reverse=True)
        # Average several viable routes so one lucky bridge is not everything.
        outlook = sum(route[0] for route in routes[:3]) / min(3, len(routes)) - own_arc
        adjustment = math.exp(max(-.5, min(.5, strength * outlook)))
        best = routes[0]
        explanation = dict(current.get("selection") or {})
        explanation.update(lookahead=[{"key": t["key"], "title": t.get("title"),
                                      "artist": t.get("artist")} for t in best[1]],
                           lookahead_multiplier=adjustment)
        label = "two-song" if len(best[1]) == 2 else f"{len(best[1])}-song"
        explanation["reason"] = (explanation.get("reason", "Taste and rotation")
                                 + f"; {label} outlook: "
                                 + " → ".join(str(t.get("title") or t["key"]) for t in best[1]))
        updates[current["key"]] = (adjustment, explanation)
    result = []
    for weight, track in scored:
        if track["key"] in updates:
            adjustment, explanation = updates[track["key"]]
            result.append((max(.001, weight * adjustment), {**track, "selection": explanation}))
        else:
            result.append((weight, track))
    return result
