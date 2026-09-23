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
import re
import threading
import time
from typing import Any

from . import config, db, intent as intent_mod, llm, taste, youtube, spotify

SEGMENT_KINDS = {"news", "patch_notes", "game_ad", "station_id",
                 "time_check", "banter", "listener_message"}


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


def _direct_artists(subject: str) -> list[str]:
    """Library artists named by the subject, matched on whole words only.

    'less rap' must not match Trapt, and 'less Weezer' needs no model.
    """
    rows = db.query("SELECT DISTINCT artist FROM tracks")
    names = sorted({db.primary_artist(r["artist"]) for r in rows})
    target = db.norm(subject)
    if not target:
        return []
    pattern = re.compile(r"(?<!\w)" + re.escape(target) + r"(?!\w)")
    return [n for n in names if db.norm(n) == target or pattern.search(db.norm(n))]


def _artists_matching(subject: str) -> list[str]:
    """Which artists in the library fit a loose description like 'rap'."""
    rows = db.query("SELECT DISTINCT artist FROM tracks")
    names = sorted({db.primary_artist(r["artist"]) for r in rows})
    if not names:
        return []

    # Cheap whole-word pass first -- no model needed for "less Weezer".
    direct = _direct_artists(subject)
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


def apply_directive(subject: str, artists: list[str] | None = None) -> dict[str, Any]:
    """Reduce how often something plays. Returns what was actually affected."""
    penalty = float(_cfg("directive_penalty", -3.0))
    artists = _artists_matching(subject) if artists is None else artists
    if not artists:
        return {"artists": [], "tracks": 0}

    from .compatibility import artists as credits
    wanted = {db.norm(artist) for artist in artists}
    touched = 0
    # Exact credit match, never a LIKE pattern: a wildcard or a substring
    # ("Rap" inside "Trapt") would turn down the wrong act.
    for row in db.query("SELECT key, artist FROM tracks"):
        if db.norm(db.primary_artist(row["artist"])) in wanted or credits(row["artist"]) & wanted:
            taste.bump(row["key"], row["artist"], penalty)
            touched += 1
    for artist in artists:
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
def _spawn(target, *args) -> None:
    """Run slow request work off the web thread. Tests replace this."""
    threading.Thread(target=target, args=args, daemon=True, name="request-worker").start()


def _open_job(parsed: intent_mod.Intent, kind: str | None = None) -> int:
    """A visible progress row for work that finishes after the reply."""
    return db.write(
        "INSERT INTO wishes (ts, raw, kind, subject, payload, timing, status, expires_at) "
        "VALUES (?,?,?,?,?,?, 'preparing', ?)",
        (time.time(), parsed.raw, kind or parsed.kind, parsed.subject or parsed.title or parsed.raw,
         json.dumps({"job": True}), parsed.timing, time.time() + 900))


def _close_job(job_id: int, result: dict[str, Any]) -> None:
    payload = {"job": True, **{k: result[k] for k in ("added", "queued", "sources", "artists") if k in result}}
    db.write("UPDATE wishes SET status=?, note=?, payload=? WHERE id=? AND status='preparing'",
             ("done" if result.get("ok") else "failed", str(result.get("message") or "")[:400],
              json.dumps(payload), job_id))


def _cancelled(job_id: int | None) -> bool:
    if not job_id:
        return False
    row = db.one("SELECT status FROM wishes WHERE id=?", (job_id,))
    return not row or row["status"] == "cancelled"


def submit(raw: str, *, rate_current: Any = None, mode: str = "request", vibe_changed=None, selection=None) -> dict[str, Any]:
    """Understand a request and act on it. Never raises.

    Answers quickly: the deterministic router runs here, and anything that
    needs the model, a catalog search or the news feeds runs as a background
    job whose progress shows in the request list.

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
              if mode == "vibe" else intent_mod.route(raw))
    if selected and not parsed.error:
        parsed = intent_mod.Intent(kind="track", artist=selected["artist"], title=selected["title"],
                                   raw=raw, confidence=1, reason="selected from Spotify")
    threshold = float(config.station.get("requests.refine_below_confidence", 0.8))
    if (mode == "request" and not selected and not parsed.error and not parsed.negate
            and parsed.confidence < threshold):
        # The model's second opinion takes seconds. Say so now and finish
        # the request in the background.
        job = _open_job(parsed, "request")
        _spawn(_refined_job, job, parsed, rate_current, vibe_changed)
        return {"intent": parsed.as_dict(), "ok": True, "kind": "working", "id": job,
                "message": "working out what you meant; it will appear in the queue when it is placed"}
    if not parsed.reason:
        parsed.reason = intent_mod._describe(parsed)
    return _act(parsed, rate_current=rate_current, vibe_changed=vibe_changed, selected=selected)


def _refined_job(job: int, parsed: intent_mod.Intent, rate_current: Any, vibe_changed: Any) -> None:
    try:
        refined = intent_mod._refine(parsed)
        if not refined.reason:
            refined.reason = intent_mod._describe(refined)
        if _cancelled(job):
            return
        result = _act(refined, rate_current=rate_current, vibe_changed=vibe_changed, inline=True)
        if result.get("ok") and result.get("kind") in ("vibe", "clear_vibe") and callable(vibe_changed):
            vibe_changed()
    except Exception as error:  # noqa: BLE001 - a job must always close its row
        result = {"ok": False, "message": f"could not finish that request: {str(error)[:200]}"}
    _close_job(job, result)


def _bulk_job(job: int, parsed: intent_mod.Intent) -> None:
    try:
        result = _bulk(parsed, job)
    except Exception as error:  # noqa: BLE001
        result = {"ok": False, "message": f"could not find songs for {parsed.subject}: {str(error)[:200]}"}
    _close_job(job, result)


def _bulk(parsed: intent_mod.Intent, job: int | None = None) -> dict[str, Any]:
    kind = parsed.kind
    criteria = parsed.extra.get("catalog")
    if criteria:
        # Eras and decades resolve against real catalog recordings with
        # release years, never a model's list.
        from . import artist_requests
        if _cancelled(job):
            return {"ok": False, "message": "cancelled"}
        message = artist_requests.catalog_request(criteria)
        return {"ok": message.startswith("Requested"), "kind": kind, "message": message}
    count = int(_cfg("bulk_suggest", 8))
    pairs = suggest(kind, parsed.subject, count)
    if not pairs:
        return {"ok": False, "message": ("could not think of anything for that. The writer may "
                                         "be rate limited -- try again in a minute.")}
    if _cancelled(job):
        return {"ok": False, "message": "cancelled"}
    outcome = _ingest(pairs, parsed.subject, kind)
    if not outcome["added"]:
        return {"ok": False, "message": (f"everything it suggested for {parsed.subject} is "
                                         f"already in rotation")}
    return {"ok": True, "kind": kind, "added": outcome["added"], "queued": outcome["queued"],
            "message": (f"added {len(outcome['added'])} for "
                        f"{parsed.subject}, {outcome['queued']} up next")}


def _topic_job(wish_id: int, subject: str) -> None:
    from .sources import rss
    try:
        stories = rss.search(subject, limit=3)
    except Exception:  # noqa: BLE001 - the hosts can still say they found nothing
        stories = []
    note = (f"{len(stories)} stories found" if stories
            else "nothing in the feeds; the hosts will say so rather than make it up")
    db.write("UPDATE wishes SET payload=?, note=? WHERE id=? AND status IN ('pending','active')",
             (json.dumps({"stories": [s["ident"] for s in stories], "found": len(stories)}), note, wish_id))


def _directive_job(job: int, subject: str) -> None:
    try:
        outcome = apply_directive(subject)
        result = _directive_result(subject, outcome)
    except Exception as error:  # noqa: BLE001
        result = {"ok": False, "message": f"could not turn that down: {str(error)[:200]}"}
    _close_job(job, result)


def _directive_result(subject: str, outcome: dict[str, Any]) -> dict[str, Any]:
    if not outcome["artists"]:
        return {"ok": False, "message": (f"nothing in the library matched {subject}, so "
                                         f"there was nothing to turn down")}
    names = ", ".join(outcome["artists"][:4])
    more = "" if len(outcome["artists"]) <= 4 else f" and {len(outcome['artists']) - 4} more"
    return {"ok": True, "kind": "directive", "artists": outcome["artists"],
            "message": f"easing off {names}{more}"}


def _act(parsed: intent_mod.Intent, *, rate_current: Any = None, vibe_changed: Any = None,
         selected: dict[str, Any] | None = None, inline: bool = False) -> dict[str, Any]:
    """Carry out an understood request. `inline` runs slow work right here."""
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
            "('pending','preparing','queued','scheduled')", (key,))
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
        if inline:
            result.update(_bulk(parsed))
            return result
        job = _open_job(parsed)
        _spawn(_bulk_job, job, parsed)
        what = ("songs from " + parsed.subject) if parsed.extra.get("catalog") else parsed.subject
        result.update(ok=True, kind=kind, id=job, added=[], queued=0,
                      message=f"finding {what}; they appear in the queue as they are found")
        return result

    # --- something for the hosts to talk about ---------------------------
    if kind == "topic":
        # Kept whether or not the feeds have anything: the hosts saying they
        # have nothing on it is a valid and honest segment, and far better
        # than inventing coverage. The source check only reports progress.
        wish_id = add_wish(parsed, {"stories": [], "found": 0})
        if inline:
            _topic_job(wish_id, parsed.subject)
            found = json.loads(db.one("SELECT payload FROM wishes WHERE id=?", (wish_id,))["payload"])["found"]
            result.update(ok=True, kind="topic", id=wish_id, sources=found,
                          message=(f"they will cover {parsed.subject} ({found} stories found)" if found else
                                   f"nothing in the feeds about {parsed.subject}. They will say so rather than make it up."))
            return result
        _spawn(_topic_job, wish_id, parsed.subject)
        result.update(ok=True, kind="topic", id=wish_id,
                      message=f"they will cover {parsed.subject}; checking the feeds for sources")
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

        direct = _direct_artists(parsed.subject)
        if direct or inline:
            result.update(_directive_result(parsed.subject, apply_directive(parsed.subject, direct or None)))
            return result
        # A loose description ("less rap") needs the model to read the
        # library's artist list. Do that off the web thread.
        job = _open_job(parsed)
        _spawn(_directive_job, job, parsed.subject)
        result.update(ok=True, kind="directive", id=job,
                      message=f"working out which artists count as {parsed.subject}")
        return result

    result.update(ok=False, message="not sure what that was. Try a song, an "
                                    "artist, a genre, or 'tell me about X'.")
    return result
