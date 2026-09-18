"""Bounded, explainable continuity hints, never catalogue admission rules.

Genre and artist evidence comes from tags/credits. Lyrics use only actual
embedded text: a deliberately small word/theme model, not guessed song lore.
Unknown evidence is neutral and no text is sent to an external service.
"""
from __future__ import annotations

import math
import random
import re
from functools import lru_cache
from typing import Any

from . import analysis, config, db

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
    "lookahead_weight": .3, "lookahead_candidates": 16,
}


def snapshot() -> dict:
    """Refresh configuration once per selection; never stat files per pair."""
    # Read the complete override generation once: separate get() calls could
    # straddle an Apply and combine old and new values in the same selection.
    selection = config.station.data().get("selection") or {}
    settings = selection.get("compatibility") or {}
    values = {key: settings.get(key, default) for key, default in _DEFAULT_SETTINGS.items()}
    values["artist_separation"] = selection.get("artist_separation", 6)
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


@lru_cache(maxsize=4096)
def _primary(text: str) -> str:
    return db.norm(db.primary_artist(text))


@lru_cache(maxsize=2048)
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


def mix_fit(a: Any, b: Any) -> float | None:
    values = []
    def number(track, key):
        try:
            value = float(db.field(track, key))
            return value if math.isfinite(value) else None
        except (TypeError, ValueError):
            return None
    ab, bb = number(a, "bpm"), number(b, "bpm")
    ac, bc = number(a, "bpm_confidence"), number(b, "bpm_confidence")
    if ab and bb and ab > 0 and bb > 0 and min(ac or 0, bc or 0) >= .25:
        distance = min(abs(ab / (bb * factor) - 1) for factor in (.5, 1, 2))
        values.append(max(0, 1 - distance / .25))
    ak, bk = str(db.field(a, "camelot") or ""), str(db.field(b, "camelot") or "")
    if (re.fullmatch(r"(?:[1-9]|1[0-2])[AB]", ak) and re.fullmatch(r"(?:[1-9]|1[0-2])[AB]", bk)
            and min(number(a, "key_confidence") or 0, number(b, "key_confidence") or 0) >= .25):
        values.append(1.0 if analysis.keys_compatible(ak, bk) else .2)
    al, bl = number(a, "lufs"), number(b, "lufs")
    if al is not None and bl is not None:
        values.append(max(0, 1 - abs(al - bl) / 12))
    return sum(values) / len(values) if values else None


def energy_fit(a: Any, b: Any, settings: dict | None = None) -> float | None:
    """Configured loudness trajectory; LUFS is only a rough energy clue."""
    try:
        outgoing, incoming = float(db.field(a, "lufs")), float(db.field(b, "lufs"))
    except (TypeError, ValueError):
        return None
    if not all(math.isfinite(value) for value in (outgoing, incoming)):
        return None
    direction = str(settings.get("energy_direction", "follow") if settings is not None
                    else config.station.get("selection.compatibility.energy_direction", "follow"))
    difference = incoming - outgoing
    target = {"follow": 0, "build": 2, "ease": -2}.get(direction, 0)
    distance = abs(abs(difference) - 4) if direction == "surprise" else abs(difference - target)
    return max(0.0, 1.0 - distance / 8.0)


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
        mix = mix_fit(previous, track)
        energy = energy_fit(previous, track, settings)
        evidence = {"genre": genre, "artist": artist, "lyrics": lyric, "mix": mix, "energy": energy}
        defaults = {"genre": .65, "artist": .25, "lyrics": .45, "mix": .35, "energy": .3}
        signal = sum(setting(f"{name}_weight", defaults[name], 0, 2, settings) * (value - .5)
                     for name, value in evidence.items() if value is not None)
        facts = genre, left, right, lyric, themes, mix, energy, evidence, signal
        if pair_cache is not None:
            pair_cache[pair_key] = facts
    genre, left, right, lyric, themes, mix, energy, evidence, signal = facts

    # A sustained comparable run gradually changes the goal from continuity
    # toward contrast. Unknown tags never count as 'same vibe'.
    run = 0
    for recent in reversed(history):
        similar = genre_fit(previous, recent)
        _, recent_themes = lyrics_fit(previous, recent)
        same_artist = bool(left & artists(str(recent.get("artist") or "")))
        if (similar is not None and similar >= .7) or recent_themes or same_artist:
            run += 1
        else:
            break
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
        reasons.append("compatible tempo/key/level evidence")
    if energy is not None and setting("energy_weight", .3, settings=settings) > 0:
        direction = settings.get("energy_direction", "follow")
        reasons.append(f"{direction} energy (loudness clue)")
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
    """Soft two-song route score over a bounded beam, with no database work.

    Half the shortlist follows taste scores; half explores the remaining
    catalogue. Tracks outside it retain their full original sampling weight.
    A route is a feasibility hint, never a reservation or queue rewrite.
    """
    settings = snapshot() if settings is None else settings
    if (len(scored) < 3 or not settings.get("enabled", True)
            or not settings.get("lookahead_enabled", True)):
        return scored
    strength = setting("lookahead_weight", .3, settings=settings)
    if strength <= 0:
        return scored
    limit = int(setting("lookahead_candidates", 16, 4, 32, settings))
    ranked = sorted(scored, key=lambda entry: entry[0], reverse=True)
    if len(ranked) > limit:
        keep = limit // 2
        ranked = ranked[:keep] + random.sample(ranked[keep:], limit - keep)
    shortlist = [track for _, track in ranked]
    separation = max(0, int(settings.get("artist_separation", 6) or 0))

    def separated(candidate: dict, context: list[dict]) -> bool:
        if not separation:
            return True
        artist = _primary(candidate.get("artist") or "")
        return not artist or all(artist != _primary(t.get("artist") or "")
                                 for t in context[-separation:])

    updates = {}
    pair_cache = {}
    for current in shortlist:
        context = history + [current]
        # Rank one step first, then inspect two-step routes only from the
        # strongest three bridges. Work is bounded even for huge libraries.
        first_steps = []
        for following in shortlist:
            if following["key"] == current["key"] or not separated(following, context):
                continue
            score = math.log(evaluate(following, current, context, settings, pair_cache, False)["multiplier"])
            first_steps.append((score, following))
        first_steps.sort(key=lambda entry: entry[0], reverse=True)
        routes = []
        for first_score, following in first_steps[:3]:
            for final in shortlist:
                if (final["key"] in {current["key"], following["key"]}
                        or not separated(final, context + [following])):
                    continue
                second_score = math.log(evaluate(final, following, context + [following], settings, pair_cache, False)["multiplier"])
                routes.append(((first_score + second_score) / 2, following, final))
        if not routes:
            continue
        routes.sort(key=lambda entry: entry[0], reverse=True)
        # Average several viable routes so one lucky bridge is not everything.
        outlook = sum(route[0] for route in routes[:3]) / min(3, len(routes))
        adjustment = math.exp(max(-.5, min(.5, strength * outlook)))
        best = routes[0]
        explanation = dict(current.get("selection") or {})
        explanation.update(lookahead=[{"key": t["key"], "title": t.get("title"),
                                      "artist": t.get("artist")} for t in best[1:]],
                           lookahead_multiplier=adjustment)
        explanation["reason"] = (explanation.get("reason", "Taste and rotation")
                                 + "; two-song outlook: "
                                 + " → ".join(str(t.get("title") or t["key"]) for t in best[1:]))
        updates[current["key"]] = (adjustment, explanation)
    result = []
    for weight, track in scored:
        if track["key"] in updates:
            adjustment, explanation = updates[track["key"]]
            result.append((max(.001, weight * adjustment), {**track, "selection": explanation}))
        else:
            result.append((weight, track))
    return result
