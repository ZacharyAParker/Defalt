"""HTTP surface.

Deliberately thin. The browser asks for the schedule and reports back what it
did; every decision of consequence lives in the director. Binds to loopback --
this serves cached audio and is not safe to expose.
"""
from __future__ import annotations

import re
import secrets
from pathlib import Path
from typing import Any

from flask import Flask, jsonify, request, send_file

from . import config, db, director, intent, library, taste, tts, vault, wishes, vibe, spotify
from .sources import steam

app = Flask(__name__, static_folder=str(config.ROOT / "web" / "static"))
app.secret_key = config.env("FLASK_SECRET_KEY") or secrets.token_hex(32)
# Local files on a local disk. Caching them only ever means editing radio.js
# and wondering why nothing changed.
app.config["SEND_FILE_MAX_AGE_DEFAULT"] = 0

from .library_api import blueprint as library_blueprint
app.register_blueprint(library_blueprint)

SAFE_NAME = re.compile(r"^[A-Za-z0-9._-]{1,120}$")
MEDIA_ROOTS = {"audio": library.AUDIO_DIR, "voice": tts.VOICE_DIR}


@app.get("/")
def index():
    """The same page the desktop app serves, for running this in a browser."""
    return send_file(config.ROOT / "web" / "index.html", max_age=0)


# --------------------------------------------------------------------------
# Playout
# --------------------------------------------------------------------------
@app.get("/media/<kind>/<name>")
def media(kind: str, name: str):
    root = MEDIA_ROOTS.get(kind)
    if root is None or not SAFE_NAME.match(name):
        return jsonify(error="not found"), 404
    path = (root / name).resolve()
    try:
        path.relative_to(root.resolve())
    except ValueError:
        return jsonify(error="not found"), 404
    if not path.is_file():
        return jsonify(error="not found"), 404
    # conditional=True gives us range requests, which the browser wants when
    # it seeks or re-fetches a partially decoded file.
    return send_file(path, conditional=True, max_age=3600)


@app.get("/api/schedule")
def schedule():
    station = director.station()
    station.heartbeat()
    return jsonify(station.snapshot())


@app.post("/api/heartbeat")
def heartbeat():
    director.station().heartbeat()
    return jsonify(ok=True)


@app.post("/api/decks/start")
def start_decks():
    payload = request.get_json(silent=True)
    if not isinstance(payload, dict):
        return jsonify(error="need a deck startup object"), 400
    try:
        return jsonify(director.station().start_decks(
            payload.get("tracks"), payload.get("session")))
    except (ValueError, TypeError) as error:
        return jsonify(error=str(error)), 400


@app.post("/api/report")
def report():
    payload = request.get_json(silent=True) or {}
    kind = str(payload.get("kind") or "")
    if kind not in {"started", "played", "skipped", "replayed"}:
        return jsonify(error="unknown report kind"), 400
    director.station().report(
        kind,
        str(payload.get("key") or ""),
        float(payload.get("position") or 0),
        float(payload.get("duration") or 0),
    )
    return jsonify(ok=True)


@app.post("/api/skip")
def skip():
    return jsonify(director.station().skip())


@app.post("/api/rate")
def rate():
    payload = request.get_json(silent=True) or {}
    key = str(payload.get("key") or "")
    value = str(payload.get("value") or "")
    if not key or value not in {"up", "down"}:
        return jsonify(error="need key and value of up or down"), 400
    row = db.one("SELECT artist FROM tracks WHERE key=?", (key,))
    if not row:
        return jsonify(error="unknown track"), 404
    taste.record(f"thumbs_{value}", key, row["artist"])
    if value == "down":
        # A thumbs down also buys separation -- do not queue it again tonight.
        db.write("UPDATE tracks SET last_played=strftime('%s','now') WHERE key=?",
                 (key,))
    return jsonify(ok=True, score=round(taste.affinity("track", key), 2))


# --------------------------------------------------------------------------
# Requests
# --------------------------------------------------------------------------
@app.post("/api/request")
def make_request():
    """The one text box. Takes a song, an artist, a genre, a topic, or an
    instruction to play something less. See docs/REQUESTS.md."""
    if not config.station.get("requests.enabled", True):
        return jsonify(error="requests are switched off in station.yaml"), 403

    payload = request.get_json(silent=True) or {}
    raw = str(payload.get("query") or "").strip()

    # The old shape still works, and skips classification entirely.
    artist = str(payload.get("artist") or "").strip()
    title = str(payload.get("title") or "").strip()
    if artist and title and not raw:
        raw = f"{artist} - {title}"

    station = director.station()
    result = wishes.submit(raw, rate_current=station.rate_current, mode=payload.get("mode", "request"),
                           vibe_changed=station.refresh_vibe, selection=payload.get("selection"))
    if result.get("ok") and result.get("kind") in ("vibe", "clear_vibe"):
        station.refresh_vibe()
    status = 200 if result.get("ok") else 400
    return jsonify(result), status


@app.get("/api/spotify/search")
def spotify_search():
    try:
        return jsonify(available=spotify.available(), tracks=spotify.search(request.args.get("q", "")))
    except ValueError as error:
        return jsonify(available=spotify.available(), tracks=[], message=str(error)), 503


@app.get("/api/requests")
def list_requests():
    """Everything outstanding: queued tracks and open wishes, newest first."""
    tracks = db.query(
        "SELECT r.id, r.ts, r.query, r.status, r.note, t.title, t.artist "
        "FROM requests r LEFT JOIN tracks t ON t.key = r.track_key "
        "ORDER BY r.ts DESC LIMIT 20")
    wish_rows = db.query(
        "SELECT id, ts, CASE WHEN kind='article' THEN subject ELSE raw END AS raw, kind, subject, timing, status, note "
        "FROM wishes ORDER BY ts DESC LIMIT 20")
    return jsonify({
        "tracks": [dict(row) for row in tracks],
        "wishes": [dict(row) for row in wish_rows],
    })


# --------------------------------------------------------------------------
# The queue
# --------------------------------------------------------------------------
@app.get("/api/queue")
def get_queue():
    """One ordered view of what is coming, across all three stages.

    on_deck  already placed on the clock with a real air time. Can be
             dropped, but not reordered -- the times and transitions of
             everything after it were computed against it.
    queued   downloaded and waiting. Fully reorderable.
    finding  a request still being resolved. Can be cancelled.
    """
    station = director.station()
    now = station.clock.now()

    rows: list[dict[str, Any]] = []
    with station.lock:
        for item in sorted(station.schedule.music_items(),
                           key=lambda i: i.start_at):
            if item.end_at <= now:
                continue
            rows.append({
                "id": item.id,
                "stage": "on_deck",
                "playing": item.start_at <= now < item.end_at,
                "eta": round(max(0.0, item.start_at - now), 1),
                "artist": item.meta.get("artist"),
                "title": item.meta.get("title"),
                "key": item.meta.get("key"),
                "bpm": item.meta.get("bpm"),
                "camelot": item.meta.get("camelot"),
                "transition": item.meta.get("transition"),
                "selection": item.meta.get("selection"),
                "can_move": False,
                "can_remove": item.start_at > now,
            })

    for entry in station.lineup():
        rows.append({**entry, "stage": "queued", "playing": False, "eta": None,
                     "can_move": True, "can_remove": True})

    for row in db.query(
            "SELECT r.id, r.query, r.status, r.note, t.artist, t.title FROM requests r "
            "LEFT JOIN tracks t ON t.key = r.track_key "
            "WHERE r.status IN ('pending','preparing') OR (r.status='failed' AND NOT EXISTS "
            "(SELECT 1 FROM requests newer WHERE newer.track_key=r.track_key AND newer.id>r.id)) "
            "ORDER BY (r.status='failed'), CASE WHEN r.status='failed' THEN -r.ts ELSE r.ts END LIMIT 30"):
        rows.append({
            "id": f"req:{row['id']}", "stage": "failed" if row['status'] == 'failed' else "finding", "playing": False,
            "eta": None, "artist": row["artist"], "title": row["title"] or row["query"],
            "note": row['note'] if row['status'] == 'failed' else None,
            "source": "request", "can_move": False, "can_remove": True,
        })

    wishes.pending()  # Retire interrupted/expired article preparation too.
    for row in db.query("SELECT id,subject,status,note FROM wishes WHERE kind='article' "
                        "AND status IN ('pending','preparing','failed') ORDER BY ts DESC LIMIT 20"):
        rows.append({'id': f"wish:{row['id']}", 'stage': 'article', 'playing': False,
                     'status': row['status'],
                     'eta': None, 'title': row['subject'], 'artist': row['note'] or
                     ('Fetching article' if row['status'] == 'preparing' else
                      'Article failed' if row['status'] == 'failed' else 'Next unwritten host break'),
                     'source': 'article', 'can_move': False, 'can_remove': True})
    return jsonify({"items": rows, "now": round(now, 2)})


@app.post("/api/queue/<entry_id>/<action>")
def queue_action(entry_id: str, action: str):
    """next | up | down | last | remove, on any queue entry."""
    station = director.station()

    # A request that has not resolved yet lives in the database, not the queue.
    if entry_id.startswith('wish:'):
        try:
            wish_id = int(entry_id.split(':', 1)[1])
        except ValueError:
            return jsonify(error='Not an article request'), 400
        if action != 'remove':
            return jsonify(error='Articles play during host breaks and cannot be reordered with songs.'), 400
        with station.lock:
            removed = wishes.cancel(wish_id)
        return jsonify(ok=removed), 200 if removed else 409
    if entry_id.startswith("req:"):
        if action != "remove":
            return jsonify(error="still being found -- it cannot be reordered "
                                 "until it is downloaded"), 400
        try:
            request_id = int(entry_id.split(":", 1)[1])
        except ValueError:
            return jsonify(error="not a request"), 400
        with station.lock:
            db.write("UPDATE requests SET status='cancelled' WHERE id=? "
                     "AND status IN ('pending','preparing','failed')", (request_id,))
        return jsonify(ok=True)

    if action == "remove":
        # Either it is waiting in the queue, or already on the clock.
        if station.remove(entry_id):
            return jsonify(ok=True, dropped="queued")
        if station.drop_scheduled(entry_id):
            return jsonify(ok=True, dropped="on_deck")
        return jsonify(error="too late -- that one is already playing"), 409

    if action not in ("next", "up", "down", "last"):
        return jsonify(error="unknown action"), 400
    if not station.move(entry_id, action):
        return jsonify(error="that is not in the queue any more"), 404
    return jsonify(ok=True)


@app.post("/api/queue/clear")
def clear_queue():
    payload = request.get_json(silent=True) or {}
    dropped = director.station().clear_lineup(
        keep_requests=bool(payload.get("keep_requests")))
    return jsonify(ok=True, dropped=dropped)


@app.post("/api/queue/add")
def queue_add():
    """Queue a track already in the library, by key. Used by the history list."""
    payload = request.get_json(silent=True) or {}
    key = str(payload.get("key") or "")
    if director.station().enqueue_key(key, front=bool(payload.get("next"))):
        return jsonify(ok=True)
    return jsonify(error="that one is not downloaded yet -- request it by "
                         "name instead"), 400


@app.post("/api/requests/<int:wish_id>/cancel")
def cancel_wish(wish_id: int):
    """Call off a topic or forced segment before it airs."""
    return jsonify(ok=wishes.cancel(wish_id))


@app.post("/api/interpret")
def interpret():
    """Classify without acting. Lets the box show what it understood first."""
    payload = request.get_json(silent=True) or {}
    parsed = intent.understand(str(payload.get("query") or ""))
    return jsonify(parsed.as_dict())


# --------------------------------------------------------------------------
# Status + tweaking
# --------------------------------------------------------------------------
@app.get("/api/vibe")
def get_vibe():
    return jsonify(vibe=vibe.public())


@app.post("/api/vibe/clear")
def clear_vibe():
    vibe.clear()
    director.station().refresh_vibe()
    return jsonify(ok=True, vibe={}, message="Vibe cleared. Returning to normal rotation after planned mixes.")


@app.get("/api/status")
def status():
    from . import llm
    from . import mixconfig
    station = director.station()
    return jsonify({
        "identity": config.station.get("identity", {}) or {},
        "now_playing": station.now_playing(),
        "transcript": station.transcript(),
        "mix_config": mixconfig.snapshot(),
        "vibe": vibe.public(),
        "note": station.status_note,
        "llm": llm.status(),
        "steam": steam.status(),
        "taste": taste.summary(limit=8),
        "hosts": [
            {"id": pid, "name": p.get("name", pid),
             "role": p.get("role", ""), "voice": (p.get("voice") or {}).get("name")}
            for pid, p in config.personas().items()
        ],
    })


# Only these may be changed from the browser. Everything else is a text edit,
# on purpose -- the config file is the real control surface.
TWEAKABLE: dict[str, tuple[type, float, float]] = {
    "crossfade.duration": (float, 0.0, 20.0),
    "ducking.target_gain": (float, 0.02, 1.0),
    "ducking.attack": (float, 0.05, 3.0),
    "ducking.release": (float, 0.1, 6.0),
    "talk_placement.assumed_intro": (float, 0.0, 45.0),
    "talk_placement.post_safety_margin": (float, 0.0, 5.0),
    "selection.exploration_rate": (float, 0.0, 1.0),
    "clock.segment_weights.news": (float, 0.0, 30.0),
    "clock.segment_weights.patch_notes": (float, 0.0, 30.0),
    "clock.segment_weights.game_ad": (float, 0.0, 30.0),
    "clock.segment_weights.banter": (float, 0.0, 30.0),
    "clock.segment_weights.track_intro": (float, 0.0, 30.0),
}


@app.get("/api/config")
def get_config():
    return jsonify({
        "values": {key: config.station.get(key) for key in TWEAKABLE},
        "bounds": {key: {"min": lo, "max": hi}
                   for key, (_, lo, hi) in TWEAKABLE.items()},
    })


@app.post("/api/mix/config")
def set_mix_config():
    from . import mixconfig
    try:
        values = mixconfig.validate(request.get_json(silent=True))
    except (ValueError, TypeError) as error:
        return jsonify(error=str(error)), 400
    station = director.station()
    with station.lock:
        config.station.set_many(values)
        # Preserve existing transition timing; update speech protection now.
        station.schedule.seal()
    return jsonify(ok=True)


@app.post("/api/config")
def set_config():
    payload = request.get_json(silent=True) or {}
    key = str(payload.get("key") or "")
    if key not in TWEAKABLE:
        return jsonify(error="that setting is only editable in station.yaml"), 400
    caster, low, high = TWEAKABLE[key]
    try:
        value = caster(payload.get("value"))
    except (TypeError, ValueError):
        return jsonify(error="bad value"), 400
    value = min(max(value, low), high)
    config.station.set(key, value)
    return jsonify(ok=True, key=key, value=value)


@app.post("/api/intro")
def set_intro():
    """Override where a track's vocal comes in, so the hosts stop talking into it."""
    payload = request.get_json(silent=True) or {}
    key = str(payload.get("key") or "")
    if not db.one("SELECT 1 FROM tracks WHERE key=?", (key,)):
        return jsonify(error="unknown track"), 404
    raw = payload.get("seconds")
    seconds = None if raw in (None, "") else min(max(float(raw), 0.0), 60.0)
    db.write("UPDATE tracks SET intro_override=? WHERE key=?", (seconds, key))
    return jsonify(ok=True, key=key, intro_override=seconds)


@app.get("/api/transition")
def get_transition():
    from . import transitions
    return jsonify({
        "preset": str(config.station.get("transitions.preset", "auto")),
        "options": ["auto", *transitions.PRESETS],
    })


@app.post("/api/transition")
def set_transition():
    """Force a transition style, or hand it back to the automatic picker."""
    from . import transitions
    payload = request.get_json(silent=True) or {}
    preset = str(payload.get("preset") or "").strip().lower()
    if preset not in ("auto", *transitions.PRESETS):
        return jsonify(error=f"not a transition. Try one of: auto, "
                             f"{', '.join(transitions.PRESETS)}"), 400
    config.station.set("transitions.preset", preset)
    return jsonify(ok=True, preset=preset)


@app.get("/api/history")
def history():
    rows = db.query(
        "SELECT t.title, t.artist, t.key, e.kind, e.ts FROM events e "
        "JOIN tracks t ON t.key = e.track_key "
        "WHERE e.kind IN ('played','completed','skipped_early','skipped_late') "
        "ORDER BY e.ts DESC LIMIT 40")
    return jsonify([dict(row) for row in rows])


@app.post("/api/vault/rebuild")
def rebuild_vault():
    return jsonify(vault.rebuild_all())


@app.errorhandler(500)
def server_error(_error: Any):
    return jsonify(error="something broke -- check the server log"), 500


def create_app() -> Flask:
    config.ensure_dirs()
    vault.ensure_scaffold()
    director.station()  # start the threads
    return app
