"""Acting on what you asked for.

`intent.py` decides what you meant. This decides what the station does about
it, and it is where the awkward realities live: a genre request must not
hijack the rotation for an hour, a model that invents songs must not poison
the library, a topic with no source material must make the hosts say so rather
than improvise, and asking for less of something has to actually reduce it.

Nothing here blocks on the network. Suggested tracks are added to the library
and queued; the feeder resolves and downloads them on its own thread, and the
wish reports progress as it goes.
"""
from __future__ import annotations

import json
import time
from typing import Any

from . import config, db, intent as intent_mod, llm, taste, youtube, spotify

SEGMENT_KINDS = {"news", "patch_notes", "game_ad", "station_id",
                 "time_check", "banter"}


def _cfg(key: str, default: Any) -> Any:
    value = config.station.get(f"requests.{key}")
    return default if value is None else value


# ---------------------------------------------------------------------------
# Suggestion
# ---------------------------------------------------------------------------
_SUGGEST_SYSTEM = """You name real, released recordings. Nothing else.

Return a JSON array of objects with "artist" and "title". No commentary.

Absolute rules:
- Every entry must be a recording that actually exists and was released.
  A plausible-sounding invention is worse than a short list.
- Use the artist name and track title as they appear on the release.
- No live versions, remixes, covers or edits unless asked for.
- Spread the list across different artists. Never more than two by one act.
- If you are not confident a recording exists, leave it out and return fewer."""


def _prompt_for(kind: str, subject: str, count: int) -> str:
    if kind == "genre":
        return (f"List {count} recordings that are unmistakably "
                f"<<<{subject}>>>. Mix well-known and less obvious, but every "
                f"one must genuinely belong to that description.")
    if kind == "similar":
        return (f"List {count} recordings by artists whose music resembles "
                f"<<<{subject}>>>. Do NOT include {subject} themselves. One "
                f"or two tracks per artist, each a good representative.")
    return (f"List {count} well-known recordings by <<<{subject}>>>. "
            f"Only tracks actually by that artist.")


def suggest(kind: str, subject: str, count: int) -> list[tuple[str, str]]:
    """Ask the model for candidate recordings. Returns [] if it cannot."""
    payload = llm.complete_json(
        _SUGGEST_SYSTEM, _prompt_for(kind, subject, count),
        max_tokens=700, temperature=0.7)

    if isinstance(payload, dict):
        for value in payload.values():
            if isinstance(value, list):
                payload = value
                break
    if not isinstance(payload, list):
        return []

    out: list[tuple[str, str]] = []
    for entry in payload:
        if not isinstance(entry, dict):
            continue
        artist = intent_mod.clean(str(entry.get("artist") or ""))[:120]
        title = intent_mod.clean(str(entry.get("title") or ""))[:160]
        if artist and title:
            out.append((artist, title))
    return out


def _ingest(pairs: list[tuple[str, str]], subject: str, kind: str) -> dict[str, Any]:
    """Filter suggestions, add them to the library, queue a couple to air."""
    # The per-artist cap exists to stop "some bossa nova" being eight Jobim
    # tracks. It must not apply when you named the artist yourself -- asking
    # for more Tower of Power and getting two is not what you asked for.
    per_artist = 10**6 if kind == "artist" else int(_cfg("max_per_artist", 2))
    keep = int(_cfg("bulk_add", 6))
    queue_now = int(_cfg("bulk_queue_now", 2))
    boost = float(_cfg("bulk_affinity", 0.8))
    dislike_floor = float(_cfg("skip_below_affinity", -2.0))

    seen_artists: dict[str, int] = {}
    seen_titles: set[str] = set()
    added: list[dict[str, str]] = []
    skipped_known = 0

    for artist, title in pairs:
        if len(added) >= keep:
            break
        key = db.track_key(artist, title)

        # The same standard under two different credits is still the same
        # song. "Desafinado" by Stan Getz and by Charlie Byrd would otherwise
        # both survive the per-artist cap and play back to back.
        song = db.norm(title)
        if song in seen_titles:
            continue
        seen_titles.add(song)

        # Already in the library and already played recently: no point.
        existing = db.one("SELECT key, play_count FROM tracks WHERE key=?", (key,))
        if existing and existing["play_count"]:
            skipped_known += 1
            continue
        # Something you have already told the station you dislike.
        if taste.affinity("track", key) < dislike_floor:
            continue
        bucket = db.norm(db.primary_artist(artist))
        if seen_artists.get(bucket, 0) >= per_artist:
            continue

        seen_artists[bucket] = seen_artists.get(bucket, 0) + 1
        taste.add_track(title, artist, source="discovery")
        # A deliberately smaller nudge than an explicit single-track request.
        # Asking to hear a genre once should not permanently reshape taste.
        taste.bump(key, artist, boost)
        added.append({"artist": artist, "title": title, "key": key})

    queued = 0
    for track in added[:queue_now]:
        db.write("INSERT INTO requests (ts, query, status, track_key) "
                 "VALUES (?,?,'pending',?)",
                 (time.time(), f"{kind}: {subject}", track["key"]))
        queued += 1

    return {"added": added, "queued": queued, "already_known": skipped_known}


# ---------------------------------------------------------------------------
# Directives
# ---------------------------------------------------------------------------
_MATCH_SYSTEM = """You are given a description and a list of artist names.

Return a JSON array of the names from the list that fit the description.
Copy names exactly as given. Return [] if none fit. Never invent a name."""


def _artists_matching(subject: str) -> list[str]:
    """Which artists in the library fit a loose description like 'rap'."""
    rows = db.query("SELECT DISTINCT artist FROM tracks")
    names = sorted({db.primary_artist(r["artist"]) for r in rows})
    if not names:
        return []

    # Cheap exact/substring pass first -- no model needed for "less Weezer".
    target = db.norm(subject)
    direct = [n for n in names if db.norm(n) == target or target in db.norm(n)]
    if direct:
        return direct

    payload = llm.complete_json(
        _MATCH_SYSTEM,
        f"Description: <<<{subject}>>>\n\nArtists:\n"
        + "\n".join(names[:200]),
        max_tokens=500, temperature=0.1)
    if not isinstance(payload, list):
        return []
    valid = {n.lower(): n for n in names}
    return [valid[str(x).lower()] for x in payload
            if isinstance(x, str) and str(x).lower() in valid]


def apply_directive(subject: str) -> dict[str, Any]:
    """Reduce how often something plays. Returns what was actually affected."""
    penalty = float(_cfg("directive_penalty", -3.0))
    artists = _artists_matching(subject)
    if not artists:
        return {"artists": [], "tracks": 0}

    touched = 0
    for artist in artists:
        rows = db.query("SELECT key, artist FROM tracks WHERE artist LIKE ?",
                        (f"%{artist}%",))
        for row in rows:
            taste.bump(row["key"], row["artist"], penalty)
            touched += 1
        db.log_event("directive", None, None, subject=subject, artist=artist)
    return {"artists": artists, "tracks": touched}


# ---------------------------------------------------------------------------
# Wishes (topics and forced segments)
# ---------------------------------------------------------------------------
def add_wish(intent: intent_mod.Intent, payload: dict[str, Any] | None = None
             ) -> int:
    ttl = float(_cfg("topic_ttl_minutes", 45)) * 60
    return db.write(
        "INSERT INTO wishes (ts, raw, kind, subject, payload, timing, status, "
        "expires_at) VALUES (?,?,?,?,?,?, 'pending', ?)",
        (time.time(), intent.raw, intent.kind, intent.subject,
         json.dumps(payload or {}), intent.timing, time.time() + ttl),
    )


def pending(kind: str | None = None) -> list[dict[str, Any]]:
    """Live wishes, oldest first, with expired ones retired on the way past."""
    db.write("UPDATE wishes SET status='failed', note='expired' "
             "WHERE status IN ('pending','preparing') AND expires_at IS NOT NULL "
             "AND expires_at < ?", (time.time(),))
    sql = "SELECT * FROM wishes WHERE status='pending'"
    params: list[Any] = []
    if kind:
        sql += " AND kind=?"
        params.append(kind)
    sql += " ORDER BY ts"
    return [dict(r) for r in db.query(sql, params)]


def next_topic() -> dict[str, Any] | None:
    """The topic the hosts should cover next, if one is due."""
    for wish in pending():
        if wish['kind'] not in ('topic', 'article'):
            continue
        if wish["timing"] == "hour" and time.localtime().tm_min > 6:
            continue        # asked for top of the hour, not yet
        return wish
    return None


def next_segment() -> dict[str, Any] | None:
    for wish in pending("segment"):
        payload = json.loads(wish["payload"] or "{}")
        if payload.get("segment") in SEGMENT_KINDS:
            return wish
    return None


def close(wish_id: int, status: str = "done", note: str = "") -> None:
    db.write("UPDATE wishes SET status=?, note=? WHERE id=?",
             (status, note or None, wish_id))


def cancel(wish_id: int) -> bool:
    row = db.one("SELECT status FROM wishes WHERE id=?", (wish_id,))
    if not row or row["status"] not in ("pending", "active", "preparing", "failed"):
        return False
    close(wish_id, "cancelled")
    return True


# ---------------------------------------------------------------------------
# The front door
# ---------------------------------------------------------------------------
def submit(raw: str, *, rate_current: Any = None, mode: str = "request", vibe_changed=None, selection=None) -> dict[str, Any]:
    """Understand a request and act on it. Never raises.

    `rate_current` is the station's thumbs-down callback, passed in so this
    module does not have to import the director and create a cycle.
    """
    if mode == "article":
        from . import articles
        return articles.submit(raw)
    if mode not in ("request", "vibe"):
        return {"ok": False, "message": "Choose request, vibe, or article."}
    try:
        selected = spotify.selected(raw, selection) if mode == "request" else None
    except ValueError as error:
        return {"ok": False, "message": str(error)}
    parsed = (intent_mod.Intent(kind="vibe", subject=intent_mod.clean(raw), raw=raw)
              if mode == "vibe" else intent_mod.understand(raw))
    if selected and not parsed.error:
        parsed = intent_mod.Intent(kind="track", artist=selected["artist"], title=selected["title"],
                                   raw=raw, confidence=1, reason="selected from Spotify")
    result: dict[str, Any] = {"intent": parsed.as_dict()}

    if parsed.error:
        result["ok"] = False
        result["message"] = parsed.error
        return result

    kind = parsed.kind

    if kind in ("vibe", "clear_vibe"):
        from . import vibe
        try:
            if kind == "clear_vibe":
                vibe.clear()
                state = {}
            else:
                state = vibe.set_current(parsed.subject or parsed.raw, on_change=vibe_changed)
        except ValueError as error:
            result.update(ok=False, message=str(error))
            return result
        result.update(ok=True, kind=kind, vibe=state,
                      message=("Vibe saved: " + state["description"] +
                               ". Keeping it until you change or clear it. Already planned mixes finish first."
                               if state else "Vibe cleared. Returning to normal rotation after planned mixes."))
        return result

    # --- a single track --------------------------------------------------
    if kind == "track":
        title = parsed.title or parsed.subject
        artist = parsed.artist
        if not title and not artist:
            result.update(ok=False, message="could not tell what song that was")
            return result

        limit = int(_cfg("max_pending", 12))
        waiting = db.one("SELECT COUNT(*) AS n FROM requests WHERE status IN ('pending','preparing')")
        if waiting and waiting["n"] >= limit:
            result.update(ok=False,
                          message=f"there are already {limit} requests waiting")
            return result

        linked = bool(parsed.extra.get("video_id"))
        key = (youtube.reserve(parsed.extra) if linked else
               taste.add_track(title, artist or "unknown", source="request"))
        if linked:
            row = db.one("SELECT title, artist FROM tracks WHERE key=?", (key,))
            title, artist = row["title"], row["artist"]
        if selected:
            db.write("UPDATE tracks SET expected_ms=?, album=?, year=?, source_metadata=? WHERE key=?",
                     (selected["expected_ms"], selected["album"], selected["year"], selected["source_metadata"], key))

        # Asking twice should not play it twice. Nudge the affinity again --
        # you clearly want it -- but do not add a second queue entry.
        already = db.one(
            "SELECT id FROM requests WHERE track_key=? AND status IN "
            "('pending','preparing','queued')", (key,))
        if already:
            taste.record("request", key, artist)
            result.update(ok=True, kind="track", key=key,
                          message=f"{title} is already in the queue")
            return result

        taste.record("request", key, artist)
        db.write("INSERT INTO requests (ts, query, status, track_key) "
                 "VALUES (?,?,'pending',?)", (time.time(), parsed.raw, key))
        result.update(ok=True, kind="track", key=key,
                      message=("Queued the linked video. Finding song details while it prepares." if linked else f"queued {title}"
                              + (f" by {artist}" if artist else "")))
        return result

    # --- bulk: an artist, a genre, or something similar ------------------
    if kind in ("artist", "similar", "genre"):
        count = int(_cfg("bulk_suggest", 8))
        pairs = suggest(kind, parsed.subject, count)
        if not pairs:
            result.update(
                ok=False,
                message=("could not think of anything for that. The writer may "
                         "be rate limited -- try again in a minute."))
            return result

        outcome = _ingest(pairs, parsed.subject, kind)
        if not outcome["added"]:
            result.update(
                ok=False,
                message=(f"everything it suggested for {parsed.subject} is "
                         f"already in rotation"))
            return result

        result.update(ok=True, kind=kind, added=outcome["added"],
                      queued=outcome["queued"],
                      message=(f"added {len(outcome['added'])} for "
                               f"{parsed.subject}, {outcome['queued']} up next"))
        return result

    # --- something for the hosts to talk about ---------------------------
    if kind == "topic":
        from .sources import rss
        stories = rss.search(parsed.subject, limit=3)
        wish_id = add_wish(parsed, {"stories": [s["ident"] for s in stories],
                                    "found": len(stories)})
        if stories:
            result.update(
                ok=True, kind="topic", id=wish_id, sources=len(stories),
                message=(f"they will cover {parsed.subject} "
                         f"({len(stories)} stories found)"))
        else:
            # Kept anyway: the hosts saying they have nothing on it is a valid
            # and honest segment, and far better than inventing coverage.
            result.update(
                ok=True, kind="topic", id=wish_id, sources=0,
                message=(f"nothing in the feeds about {parsed.subject}. "
                         f"They will say so rather than make it up."))
        return result

    # --- run a specific segment ------------------------------------------
    if kind == "segment":
        if parsed.segment not in SEGMENT_KINDS:
            result.update(ok=False, message="not a segment the station runs")
            return result
        wish_id = add_wish(parsed, {"segment": parsed.segment})
        result.update(ok=True, kind="segment", id=wish_id,
                      message=f"next break will be {parsed.segment.replace('_', ' ')}")
        return result

    # --- play that less ---------------------------------------------------
    if kind == "directive":
        if parsed.extra.get("current"):
            if callable(rate_current):
                rated = rate_current()
                result.update(ok=bool(rated), kind="directive",
                              message="noted, marking this one down"
                                      if rated else "nothing playing to mark down")
            else:
                result.update(ok=False, message="nothing playing to mark down")
            return result

        outcome = apply_directive(parsed.subject)
        if not outcome["artists"]:
            result.update(
                ok=False,
                message=(f"nothing in the library matched {parsed.subject}, so "
                         f"there was nothing to turn down"))
            return result
        names = ", ".join(outcome["artists"][:4])
        more = "" if len(outcome["artists"]) <= 4 else f" and {len(outcome['artists']) - 4} more"
        result.update(ok=True, kind="directive",
                      artists=outcome["artists"],
                      message=f"easing off {names}{more}")
        return result

    result.update(ok=False, message="not sure what that was. Try a song, an "
                                    "artist, a genre, or 'tell me about X'.")
    return result
