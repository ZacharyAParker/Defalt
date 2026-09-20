"""Taste: what gets played next, and how the station learns.

Seeded once from Spotify, then driven entirely by what you do -- skips,
replays, requests, thumbs. Affinity decays toward neutral so a phase you grew
out of stops dominating the rotation a year later.
"""
from __future__ import annotations

import json
import math
import random
import re
import time
from pathlib import Path
from typing import Any

from . import compatibility, config, db, versions, vibe

SEED_FILE = config.ROOT / "seed" / "spotify_seed.json"


# --------------------------------------------------------------------------
# Seeding
# --------------------------------------------------------------------------
def _parse_duration(text: str) -> int:
    """'3:41' -> ms. Tolerates the odd '5:60' Spotify sometimes emits."""
    try:
        minutes, seconds = text.split(":")
        return (int(minutes) * 60 + int(seconds)) * 1000
    except (ValueError, AttributeError):
        return 0


def import_seed(force: bool = False) -> int:
    """Load the Spotify bootstrap. Idempotent -- existing tracks are left alone."""
    if not SEED_FILE.exists():
        return 0
    if not force and db.one("SELECT 1 FROM tracks WHERE source='seed' LIMIT 1"):
        return 0

    try:
        payload = json.loads(SEED_FILE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return 0

    # Long-term favourites are the backbone; short-term gets a boost because
    # it is what you are actually into right now; recent is a light nudge.
    buckets = [
        ("top_tracks_long_term", 2.0),
        ("top_tracks_short_term", 3.0),
        ("recently_played", 1.2),
    ]

    now = time.time()
    added = 0
    for field, base_weight in buckets:
        rows = payload.get(field) or []
        total = max(len(rows), 1)
        for index, row in enumerate(rows):
            if not isinstance(row, list) or len(row) < 2:
                continue
            title, artist = str(row[0]), str(row[1])
            expected = _parse_duration(row[2]) if len(row) > 2 else 0
            key = db.track_key(artist, title)

            existing = db.one("SELECT key FROM tracks WHERE key=?", (key,))
            if not existing:
                db.write(
                    "INSERT INTO tracks (key,title,artist,source,expected_ms,added_at) "
                    "VALUES (?,?,?,?,?,?)",
                    (key, title, artist, "seed", expected, now),
                )
                added += 1

            # Rank within the bucket matters: #1 is a stronger signal than #48.
            rank_factor = 1.0 - (index / total) * 0.6
            bump(key, artist, base_weight * rank_factor)

    return added


# --------------------------------------------------------------------------
# Affinity
# --------------------------------------------------------------------------
def _decayed(score: float, updated_at: float) -> float:
    half_life = float(config.station.get("learning.half_life_days", 90) or 90)
    if half_life <= 0:
        return score
    elapsed_days = max(0.0, (time.time() - updated_at) / 86400.0)
    return score * (0.5 ** (elapsed_days / half_life))


def affinity(entity_type: str, entity_key: str) -> float:
    row = db.one(
        "SELECT score, updated_at FROM affinity WHERE entity_type=? AND entity_key=?",
        (entity_type, entity_key),
    )
    if not row:
        return 0.0
    return _decayed(row["score"], row["updated_at"])


def bump(track_key: str, artist: str, delta: float) -> None:
    """Move a track's affinity, bleeding a fraction onto its primary artist."""
    _apply("track", track_key, delta)
    bleed = float(config.station.get("learning.artist_bleed", 0.4) or 0.0)
    if bleed:
        _apply("artist", db.norm(db.primary_artist(artist)), delta * bleed)


def _apply(entity_type: str, entity_key: str, delta: float) -> None:
    if not entity_key:
        return
    current = affinity(entity_type, entity_key)
    db.write(
        "INSERT INTO affinity (entity_type, entity_key, score, updated_at) "
        "VALUES (?,?,?,?) ON CONFLICT(entity_type, entity_key) "
        "DO UPDATE SET score=excluded.score, updated_at=excluded.updated_at",
        (entity_type, entity_key, current + delta, time.time()),
    )


def ignore_skips() -> bool:
    return bool(config.station.get("learning.ignore_skips", False))


def record(signal: str, track_key: str, artist: str = "",
           position: float | None = None, duration: float | None = None) -> None:
    """Fold a listening signal into the profile.

    `signal` is one of the keys under learning.signal_weights, or the raw
    'skipped' / 'played' which get classified by position first.
    """
    if not config.station.get("learning.enabled", True):
        return
    if ignore_skips() and signal in {"skipped", "skipped_early", "skipped_late"}:
        return

    weights = config.station.get("learning.signal_weights", {}) or {}

    if signal == "skipped":
        cutoff = float(config.station.get("learning.skip_is_rejection_before", 30) or 30)
        signal = "skipped_early" if (position or 0) < cutoff else "skipped_late"
        db.write("UPDATE tracks SET skip_count = skip_count + 1 WHERE key=?", (track_key,))
    elif signal == "played":
        threshold = float(config.station.get("learning.completion_is_approval_at", 0.85) or 0.85)
        if duration and position and position >= duration * threshold:
            signal = "completed"
        else:
            return  # partial play with no clear verdict teaches us nothing

    delta = float(weights.get(signal, 0.0) or 0.0)
    if not artist:
        row = db.one("SELECT artist FROM tracks WHERE key=?", (track_key,))
        artist = row["artist"] if row else ""

    if delta:
        bump(track_key, artist, delta)
    db.log_event(signal, track_key, position)

    # Daypart: remember which hour you were in when this landed well.
    if delta > 0 and artist:
        hour = time.localtime().tm_hour
        db.write(
            "INSERT INTO daypart (hour, artist, weight) VALUES (?,?,?) "
            "ON CONFLICT(hour, artist) DO UPDATE SET weight = weight + excluded.weight",
            (hour, db.norm(db.primary_artist(artist)), delta),
        )


def daypart_fit(artist: str, hour: int | None = None) -> float:
    """0..1 -- how much this artist belongs at this hour, per your history."""
    hour = time.localtime().tm_hour if hour is None else hour
    key = db.norm(db.primary_artist(artist))
    # Look at a three-hour window so 2am and 3am reinforce each other.
    hours = [(hour - 1) % 24, hour, (hour + 1) % 24]
    placeholders = ",".join("?" * len(hours))
    row = db.one(
        f"SELECT SUM(weight) AS w FROM daypart WHERE artist=? AND hour IN ({placeholders})",
        (key, *hours),
    )
    mine = (row["w"] if row and row["w"] else 0.0)
    row = db.one(
        f"SELECT SUM(weight) AS w FROM daypart WHERE hour IN ({placeholders})",
        tuple(hours),
    )
    total = (row["w"] if row and row["w"] else 0.0)
    if total <= 0:
        return 0.5  # no data yet, stay neutral
    return min(1.0, (mine / total) * 8.0)


# --------------------------------------------------------------------------
# Selection
# --------------------------------------------------------------------------
def _recent_artists(limit: int) -> set[str]:
    rows = db.query(
        "SELECT artist FROM tracks WHERE last_played IS NOT NULL "
        "ORDER BY last_played DESC LIMIT ?", (limit,))
    return {db.norm(db.primary_artist(r["artist"])) for r in rows}


def candidates() -> list[dict[str, Any]]:
    rows = db.query("SELECT * FROM tracks WHERE blocked = 0")
    return [dict(r) for r in rows]


def recording_ids(track):
    title = re.sub(r"[\[(]\s*(?:feat\.?|ft\.?|with)\s+[^\])]+[\])]", "", track.get("title") or "", flags=re.I)
    title = db.norm(title.replace("'", "").replace("\u2019", ""))
    artist = db.norm(db.primary_artist(track.get("artist") or ""))
    ids = {f"song:{artist}|{title}"}
    if track.get("video_id"):
        ids.add(f"video:{track['video_id']}")
    return ids


def pick_next(exclude_keys: set[str] | None = None,
              previous: dict[str, Any] | None = None,
              history: list[dict[str, Any]] | None = None) -> dict[str, Any] | None:
    """Choose the next track. Weighted sampling, not argmax -- a station that
    always plays its single favourite song is not a station."""
    exclude = set(exclude_keys or ())
    catalogue = candidates()
    excluded_ids = set().union(*(recording_ids(t) for t in catalogue if t["key"] in exclude))
    latest = {}
    for track in catalogue:
        for identity in recording_ids(track):
            latest[identity] = max(latest.get(identity, 0), track.get("last_played") or 0)
    pool, seen = [], set()
    for track in catalogue:
        identities = recording_ids(track)
        if track["key"] in exclude or identities & (excluded_ids | seen):
            continue
        seen.update(identities)
        pool.append({**track, "last_played": max(latest[i] for i in identities) or None})
    if config.station.get("selection.avoid_clean_versions", True):
        pool = [t for t in pool if not versions.clean_track(t)]
    if config.station.get("selection.prefer_original_recording", True):
        pool = [t for t in pool if not versions.alternate_track(t)]
    if not pool:
        return None

    cfg = config.station
    selection_settings = compatibility.snapshot()
    listening_vibe = vibe.for_selection()
    weights = cfg.get("selection.weights", {}) or {}
    separation = int(cfg.get("selection.artist_separation", 6) or 0)
    title_hours = float(cfg.get("selection.title_separation_hours", 5) or 0)
    now = time.time()

    blocked_artists = _recent_artists(separation) if separation else set()
    history = list(history or [])
    if not history:
        history = [dict(row) for row in reversed(db.query(
            "SELECT * FROM tracks WHERE last_played IS NOT NULL "
            "ORDER BY last_played DESC LIMIT ?",
            (int(compatibility.setting("history_size", 10, 2, 30, selection_settings)),)))]
    previous = previous or (history[-1] if history else None)
    if separation:
        blocked_artists.update(db.norm(db.primary_artist(t.get("artist") or ""))
                               for t in history[-separation:])
    rested = [t for t in pool if not (title_hours and t["last_played"]
                                    and now - t["last_played"] < title_hours * 3600)]
    eligible = [t for t in rested
                if db.norm(db.primary_artist(t["artist"])) not in blocked_artists
                ]
    relaxed = not eligible
    if eligible or rested:
        pool = eligible or rested  # Relax artist spacing before song cooldowns.
    else:
        # A small library eventually has to repeat. Give the longest-rested
        # recordings their turn instead of repeatedly drawing the favourite.
        oldest = min(t["last_played"] or 0 for t in pool)
        pool = [t for t in pool if (t["last_played"] or 0) <= oldest + 1.0]
    explore = (bool(selection_settings.get("enabled", True))
               and random.random() < compatibility.setting("explore_chance", .18, settings=selection_settings))

    scored: list[tuple[float, dict[str, Any]]] = []
    for track in pool:
        artist_norm = db.norm(db.primary_artist(track["artist"]))
        last = track["last_played"] or 0

        track_aff = affinity("track", track["key"])
        artist_aff = affinity("artist", artist_norm)
        combined = track_aff + artist_aff * 0.5

        # Squash affinity into 0..1 so one beloved song can't swamp everything.
        affinity_score = 1.0 / (1.0 + math.exp(-max(-60, min(60, combined / 4.0))))

        age_days = (now - last) / 86400.0 if last else 30.0
        freshness = min(1.0, age_days / 14.0)

        fit = daypart_fit(track["artist"])
        seed_recency = 1.0 if track["source"] == "seed" else 0.5
        noise = random.random()

        total = (
            float(weights.get("affinity", 1.0)) * affinity_score
            + float(weights.get("freshness", 0.6)) * freshness
            + float(weights.get("daypart_fit", 0.5)) * fit
            + float(weights.get("recency_seed", 0.4)) * seed_recency
            + float(weights.get("exploration", 0.35)) * noise
        )
        continuity = compatibility.evaluate(track, previous, history, selection_settings)
        if explore:
            continuity = {**continuity, "multiplier": 1.0,
                          "reason": "Exploration pick; continuity preference relaxed"}
        total *= continuity["multiplier"]
        vibe_weight = vibe.fit(track, listening_vibe)
        total *= vibe_weight
        if listening_vibe:
            label = 'private music direction' if listening_vibe.get('private') else listening_vibe['description']
            continuity = {**continuity,
                          "vibe": label, "vibe_weight": round(vibe_weight, 3),
                          "reason": continuity["reason"] + "; vibe: " + label}
        if relaxed:
            # A small library must still play. Keep rotation pressure while
            # relaxing only separation, never explicit exclusions/blocks.
            if artist_norm in blocked_artists:
                total *= .25
            if title_hours and last and now - last < title_hours * 3600:
                total *= max(.05, (now - last) / (title_hours * 3600))
        selected = {**track, "selection": {**continuity, "exploration": explore,
                                           "separation_relaxed": relaxed}}
        scored.append((max(total, 0.001), selected))

    if not explore:
        scored = compatibility.lookahead(scored, history, selection_settings)
    scored = vibe.focus(scored, listening_vibe)
    total_weight = sum(w for w, _ in scored)
    roll = random.uniform(0, total_weight)
    for weight, track in scored:
        roll -= weight
        if roll <= 0:
            return track
    return scored[-1][1]


def mark_played(track_key: str) -> None:
    db.write(
        "UPDATE tracks SET last_played=?, play_count=play_count+1 WHERE key=?",
        (time.time(), track_key),
    )


def add_track(title: str, artist: str, source: str = "discovery",
              expected_ms: int = 0) -> str:
    key = db.track_key(artist, title)
    if not db.one("SELECT 1 FROM tracks WHERE key=?", (key,)):
        db.write(
            "INSERT INTO tracks (key,title,artist,source,expected_ms,added_at) "
            "VALUES (?,?,?,?,?,?)",
            (key, title, artist, source, expected_ms, time.time()),
        )
    return key


def summary(limit: int = 12) -> dict[str, Any]:
    """Human-facing snapshot, used by the UI and the vault writer."""
    top_tracks = db.query(
        "SELECT t.title, t.artist, a.score, a.updated_at FROM affinity a "
        "JOIN tracks t ON t.key = a.entity_key "
        "WHERE a.entity_type='track' ORDER BY a.score DESC LIMIT ?", (limit,))
    top_artists = db.query(
        "SELECT entity_key, score, updated_at FROM affinity "
        "WHERE entity_type='artist' ORDER BY score DESC LIMIT ?", (limit,))

    # Affinity is keyed on the normalised name ("run d m c"). Map it back to
    # how the artist is actually written, or the vault fills with broken links
    # and the UI shows lowercase mush.
    display: dict[str, str] = {}
    for row in db.query("SELECT DISTINCT artist FROM tracks"):
        display.setdefault(db.norm(db.primary_artist(row["artist"])),
                           db.primary_artist(row["artist"]))
    counts = db.one(
        "SELECT COUNT(*) AS tracks, SUM(play_count) AS plays, "
        "SUM(skip_count) AS skips FROM tracks")
    return {
        "top_tracks": [
            {"title": r["title"], "artist": r["artist"],
             "score": round(_decayed(r["score"], r["updated_at"]), 2)}
            for r in top_tracks
        ],
        "top_artists": [
            {"artist": display.get(r["entity_key"], r["entity_key"]),
             "key": r["entity_key"],
             "score": round(_decayed(r["score"], r["updated_at"]), 2)}
            for r in top_artists
        ],
        "library_size": counts["tracks"] if counts else 0,
        "total_plays": (counts["plays"] or 0) if counts else 0,
        "total_skips": (counts["skips"] or 0) if counts else 0,
    }
