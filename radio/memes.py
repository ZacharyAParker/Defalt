"""Reviewed music memes, matched locally and rationed across prepared breaks.

This is a curated catalog, not a live search or a claim that model memory is a
source. Reserve on preparation: even a discarded break consumes its cooldown,
so queue rebuilds cannot spam the same joke. No network work on this path.
"""
import math
import random
import re
import threading
import time
import unicodedata
from datetime import date
from urllib.parse import urlparse

from . import config, db
from .compatibility import artists

_catalog = config.ConfigFile(config.CONFIG_DIR / "memes.yaml")
_lock = threading.Lock()


def _norm(value):
    return re.sub(r"[^\w]+", "", unicodedata.normalize("NFKC", value).casefold())


def references():
    """Ignore incomplete entries rather than let a bad edit interrupt radio."""
    result, ids = [], set()
    entries = _catalog.get("references", [])
    for entry in entries if isinstance(entries, list) else []:
        if not isinstance(entry, dict):
            continue
        required = ("id", "scope", "context", "source", "reviewed", "spoken")
        if any(not isinstance(entry.get(k), str) or not entry[k].strip() for k in required):
            continue
        try:
            source = urlparse(entry["source"])
            date.fromisoformat(entry["reviewed"])
        except ValueError:
            continue
        if source.scheme != "https" or not source.hostname or entry["scope"] not in {"song", "artist"}:
            continue
        if entry["id"] in ids or entry["id"] == "__last__":
            continue
        if any(not isinstance(entry.get(k), list) or not entry[k]
               or any(not isinstance(v, str) or not _norm(v) for v in entry[k])
               for k in (("artists", "titles") if entry["scope"] == "song" else ("artists",))):
            continue
        quote = entry.get("quote", "")
        if not isinstance(quote, str) or len(quote.split()) > 10:
            continue
        quoted = entry.get("spoken_quote", "")
        if not isinstance(quoted, str) or (quote and (not quoted or quote not in quoted)):
            continue
        if any(len(text.split()) > 45 for text in (entry["spoken"], quoted)):
            continue
        ids.add(entry["id"])
        result.append(dict(entry))
    return result


def _number(key, default, low, high):
    try:
        value = float(config.station.get("hosts." + key, default))
        return max(low, min(high, value)) if math.isfinite(value) else default
    except (TypeError, ValueError):
        return default


def prepare(data, recent=()):
    if not config.station.get("hosts.meme_references", True):
        return None
    if random.random() * 100 >= _number("meme_chance_percent", 30, 0, 100):
        return None
    quotes = config.station.get("hosts.meme_quotes", True)
    candidates = []
    for ref in references():
        for slot in ("incoming", "outgoing"):
            track = data.get(slot)
            if not track:
                continue
            credits = {_norm(artist) for artist in artists(track.get("artist") or "")}
            if not credits.intersection(_norm(artist) for artist in ref["artists"]):
                continue
            if ref["scope"] == "song" and _norm(track.get("title") or "") not in {
                    _norm(title) for title in ref["titles"]}:
                continue
            spoken = ref.get("spoken_quote") if quotes and ref.get("quote") else ref["spoken"]
            if any(_norm(spoken) == _norm(line) for line in recent):
                continue
            candidates.append({**ref, "opening": spoken, "matched_slot": slot,
                               "matched_title": track.get("title"), "matched_artist": track.get("artist")})
            break
    if not candidates:
        return None
    gap = _number("meme_gap_minutes", 10, 0, 120) * 60
    repeat = _number("meme_repeat_hours", 48, 1, 168) * 3600
    with _lock:
        # One transaction protects reservations even from a second backend.
        conn = db.connect()
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            now = time.time()
            history = {row["ident"]: row["ts"] for row in conn.execute(
                "SELECT ident, ts FROM seen WHERE kind='music_meme'")}
            if "__last__" in history and now - history["__last__"] < gap:
                return None
            fresh = [ref for ref in candidates if ref["id"] not in history
                     or now - history[ref["id"]] >= repeat]
            if not fresh:
                return None
            chosen = random.choice(fresh)
            conn.executemany("INSERT OR REPLACE INTO seen(kind, ident, ts) VALUES('music_meme', ?, ?)",
                             [(chosen["id"], now), ("__last__", now)])
    return chosen


def provenance(ref):
    return {key: ref[key] for key in ("id", "scope", "context", "source", "reviewed",
                                     "matched_slot", "matched_title", "matched_artist")}
