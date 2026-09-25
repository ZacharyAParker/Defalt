"""HTTP surface.

Deliberately thin. The browser asks for the schedule and reports back what it
did; every decision of consequence lives in the director. Binds to loopback --
this serves cached audio and is not safe to expose.
"""
from __future__ import annotations

import itertools
import json
import math
import mimetypes
import os
import re
import secrets
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterator
from urllib.parse import urlsplit

from flask import Flask, jsonify, request, send_file, Response

from . import about, config, db, director, intent, library, taste, tts, vault, wishes, vibe, spotify
from . import remote_auth
from .sources import steam

app = Flask(__name__, static_folder=str(config.ROOT / "web" / "static"))
app.secret_key = config.env("FLASK_SECRET_KEY") or secrets.token_hex(32)
# Local files on a local disk. Caching them only ever means editing radio.js
# and wondering why nothing changed.
app.config["SEND_FILE_MAX_AGE_DEFAULT"] = 0

from .library_api import blueprint as library_blueprint
app.register_blueprint(library_blueprint)
from . import feedback as _feedback
_feedback.register(app)
from .remote_api import blueprint as remote_blueprint
app.register_blueprint(remote_blueprint)

# Self-hosted fonts. Windows' registry does not always know the type, and a
# font served as text/plain is refused by the browser.
mimetypes.add_type("font/woff2", ".woff2")
# Same for the startup intro: a video the registry calls something else
# won't play, and WebP isn't registered at all on older Windows.
mimetypes.add_type("video/mp4", ".mp4")
mimetypes.add_type("image/webp", ".webp")


@app.after_request
def cache_versioned_static(response: Response) -> Response:
    """Static URLs carrying ?v=<version> never change: cache them for good.

    Unversioned ones keep the no-cache default above, so a plain edit to
    radio.js is still picked up on the next load.
    """
    if (request.path.startswith("/static/") and "v" in request.args
            and response.status_code in (200, 304)):
        response.headers["Cache-Control"] = "public, max-age=31536000, immutable"
    return response

SAFE_NAME = re.compile(r"^[A-Za-z0-9._-]{1,120}$")
MEDIA_ROOTS = {"audio": library.AUDIO_DIR, "voice": tts.VOICE_DIR}
AUDIO_TYPES = {".mp3": "audio/mpeg", ".flac": "audio/flac", ".wav": "audio/wav",
               ".wave": "audio/wav", ".ogg": "audio/ogg", ".opus": "audio/ogg",
               ".m4a": "audio/mp4", ".mp4": "audio/mp4", ".aac": "audio/aac",
               ".aif": "audio/aiff", ".aiff": "audio/aiff"}


def _log(*parts: Any) -> None:
    if config.DEBUG:
        print("[app]", *parts, flush=True)


# --------------------------------------------------------------------------
# Request protection
# --------------------------------------------------------------------------
# Loopback, plus the public names in REMOTE_HOSTS that a Cloudflare Tunnel
# brings here -- and those only with a valid Cloudflare Access token (see
# remote_auth). A loopback server is still reachable from any web page
# the listener has open. The Host check stops DNS rebinding (a hostile name
# resolved to 127.0.0.1); the Origin / Sec-Fetch-Site check stops a page
# elsewhere from posting skips, requests or a shutdown. A request without an
# Origin -- the console, curl -- is not a browser acting for another site.
LOOPBACK_NAMES = {"127.0.0.1", "localhost", "::1"}
SAFE_METHODS = {"GET", "HEAD", "OPTIONS"}


def _split_host(value: str) -> tuple[str, int] | None:
    """'127.0.0.1:8090' / '[::1]:8090' / 'localhost' -> (name, port)."""
    value = (value or "").strip().lower()
    if value.startswith("["):
        end = value.find("]")
        if end == -1:
            return None
        name, rest = value[1:end], value[end + 1:]
        if rest and not rest.startswith(":"):
            return None
        port = rest[1:]
    else:
        name, _, port = value.partition(":")
        if ":" in port:
            return None
    if port and not port.isdigit():
        return None
    return name, int(port) if port else 80


def _served_port() -> int:
    try:
        return int(request.environ.get("SERVER_PORT") or 0)
    except ValueError:
        return 0


@app.before_request
def guard_request():
    host = _split_host(request.headers.get("Host", ""))
    if host is None:
        return jsonify(error="unexpected host"), 403
    loopback = host[0] in LOOPBACK_NAMES and host[1] == _served_port()
    # Anything Cloudflare delivered came from the internet, whatever Host it
    # names; a public name in REMOTE_HOSTS always did.
    through_cloudflare = bool(request.headers.get("Cf-Connecting-Ip") or request.headers.get("Cf-Ray"))
    remote = remote_auth.is_remote_host(host[0]) and host[1] in (80, 443)
    if remote or (loopback and through_cloudflare):
        try:
            claims = remote_auth.check(request.headers, request.cookies)
        except remote_auth.Refused as why:
            _log("remote request refused:", why)
            return jsonify(error="Cloudflare Access sign-in required"), 403
        request.environ["defalt.remote"] = remote_auth.identity(claims)
        remote = True
    elif not loopback:
        return jsonify(error="unexpected host"), 403
    if request.method in SAFE_METHODS:
        return None
    origin = request.headers.get("Origin")
    if origin is not None:
        try:
            parts = urlsplit(origin)
            scheme = "https" if remote else "http"
            same = (parts.scheme == scheme and parts.netloc.lower() == request.host.lower())
        except ValueError:
            same = False
        if not same:
            return jsonify(error="cross-origin request refused"), 403
    if request.headers.get("Sec-Fetch-Site", "").lower() == "cross-site":
        return jsonify(error="cross-site request refused"), 403
    return None


@app.get("/")
def index():
    """The same page the desktop app serves, for running this in a browser."""
    html = (config.ROOT / "web" / "index.html").read_text(encoding="utf-8")
    html = html.replace("{{APP_VERSION}}", about.VERSION).replace("{{TERMS_VERSION}}", about.TERMS_VERSION)
    return Response(html, mimetype="text/html",
                    headers={"Cache-Control": "no-store"})


@app.get("/api/about")
def release_info():
    return jsonify(about.release_info())


@app.get("/api/artwork")
def track_artwork():
    from . import artwork
    key = request.args.get("key", "")
    if not key or len(key) > 500:
        return jsonify(error="Track key required"), 400
    result = artwork.resolve(key)
    if result is None:
        return jsonify(error="No artwork available"), 404
    path, source = result
    response = send_file(path, mimetype="image/png", conditional=True, max_age=3600)
    response.headers["X-Artwork-Source"] = source
    return response


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


@app.get("/media/track/<path:key>")
def media_track(key: str):
    """A library track, by key, from wherever the tracks table says it lives.

    Local records sit in the listener's own folders, under any name at all,
    so they cannot go through the name-checked cache route. Only a path the
    catalogue already holds is ever served; the URL never names a file.
    """
    row = db.one("SELECT file FROM tracks WHERE key=?", (key,))
    if row is None or not row["file"]:
        return jsonify(error="not found"), 404
    path = Path(row["file"])
    if not path.is_file():
        return jsonify(error="not found"), 404
    return send_file(path, mimetype=AUDIO_TYPES.get(path.suffix.lower()),
                     conditional=True, etag=True, max_age=3600)


def schedule_payload(station: Any) -> dict[str, Any]:
    """GET /api/schedule's body. `now` is read last, after everything else."""
    data = station.snapshot()
    try:
        data["now"] = round(float(station.clock.now()), 4)
    except (TypeError, ValueError, AttributeError):
        pass
    return data


@app.get("/api/schedule")
def schedule():
    station = director.station()
    station.heartbeat()
    return jsonify(schedule_payload(station))


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
    payload = request.get_json(silent=True)
    if not isinstance(payload, dict):
        payload = {}
    kind = str(payload.get("kind") or "")
    if kind not in {"started", "played", "skipped", "replayed"}:
        return jsonify(error="unknown report kind"), 400
    try:
        position = _finite(payload.get("position") or 0)
        duration = _finite(payload.get("duration") or 0)
    except ValueError:
        return jsonify(error="position and duration must be numbers"), 400
    director.station().report(
        kind,
        str(payload.get("key") or ""),
        position,
        duration,
        str(payload.get("item_id") or ""),
    )
    return jsonify(ok=True)


def _json_object() -> dict[str, Any]:
    """The JSON body if it is an object. An empty body, form data or a bare
    array reads as {} -- the console posts JSON or nothing at all."""
    payload = request.get_json(silent=True)
    return payload if isinstance(payload, dict) else {}


def _finite(value: Any) -> float:
    """A real, finite number, or ValueError. JSON can carry NaN and Infinity."""
    if isinstance(value, (dict, list)):
        raise ValueError("not a number")
    try:
        number = float(value)
    except (TypeError, ValueError) as error:
        raise ValueError("not a number") from error
    if not math.isfinite(number):
        raise ValueError("not a finite number")
    return number


@app.post("/api/skip")
def skip():
    return jsonify(director.station().skip())


@app.get('/api/director/chat')
def director_chat_state():
    from .director_chat import for_station
    return jsonify(for_station(director.station()).state())


@app.post('/api/director/chat')
def director_chat_message():
    from .director_chat import for_station
    try:
        return jsonify(for_station(director.station()).submit(request.get_json(silent=True))), 202
    except ValueError as error:
        return jsonify(error=str(error)), 400


@app.post("/api/ads")
def request_ad():
    from . import ads
    payload = request.get_json(silent=True)
    if not isinstance(payload, dict) or payload.get("timing") not in ("next_break", "now"):
        return jsonify(error="Choose next_break or now."), 400
    try:
        result = ads.for_station(director.station()).queue(payload["timing"])
        return jsonify(ok=True, ad=result, message=result["message"]), 202
    except ValueError as error:
        return jsonify(error=str(error)), 409


@app.post("/api/rate")
def rate():
    payload = _json_object()
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

    payload = _json_object()
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
    return jsonify(queue_payload(director.station()))


def queue_payload(station: Any) -> dict[str, Any]:
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
                "selection_origin": item.meta.get("selection_origin", {"by": "unknown"}),
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
    return {"items": rows, "now": round(now, 2)}


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
    payload = _json_object()
    dropped = director.station().clear_lineup(
        keep_requests=bool(payload.get("keep_requests")))
    return jsonify(ok=True, dropped=dropped)


@app.post("/api/queue/add")
def queue_add():
    """Queue a track already in the library, by key. Used by the history list."""
    payload = _json_object()
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
    payload = _json_object()
    parsed = intent.understand(str(payload.get("query") or ""))
    return jsonify(parsed.as_dict())


# --------------------------------------------------------------------------
# Status + tweaking
# --------------------------------------------------------------------------
def vibe_payload(_station: Any = None) -> dict[str, Any]:
    return {"vibe": vibe.public()}


@app.get("/api/vibe")
def get_vibe():
    return jsonify(vibe_payload())


@app.post("/api/vibe/clear")
def clear_vibe():
    vibe.clear()
    director.station().refresh_vibe()
    return jsonify(ok=True, vibe={}, message="Vibe cleared. Returning to normal rotation after planned mixes.")


def status_payload(station: Any, lite: bool = False) -> dict[str, Any]:
    """GET /api/status. `lite` leaves out what rarely changes and costs most:
    the settings schema, the taste summary and the host roster."""
    from . import llm
    from . import ads
    payload = {
        "identity": config.station.get("identity", {}) or {},
        "now_playing": station.now_playing(),
        "transcript": station.transcript(),
        "ad": ads.for_station(station).public(),
        "vibe": vibe.public(),
        "note": station.status_note,
        "llm": llm.status(),
        "steam": steam.status(),
    }
    if lite:
        return payload
    from . import mixconfig
    payload["mix_config"] = mixconfig.snapshot()
    payload["taste"] = taste.summary(limit=8)
    payload["hosts"] = [
        {"id": pid, "name": p.get("name", pid),
         "role": p.get("role", ""), "voice": (p.get("voice") or {}).get("name")}
        for pid, p in config.personas().items()
    ]
    return payload


@app.get("/api/status")
def status():
    lite = request.args.get("lite", "").lower() in ("1", "true", "yes")
    return jsonify(status_payload(director.station(), lite=lite))


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
    payload = _json_object()
    key = str(payload.get("key") or "")
    if key not in TWEAKABLE:
        return jsonify(error="that setting is only editable in station.yaml"), 400
    caster, low, high = TWEAKABLE[key]
    try:
        value = caster(payload.get("value"))
    except (TypeError, ValueError):
        return jsonify(error="bad value"), 400
    # NaN slips through min/max untouched and would be written to disk.
    if not math.isfinite(value):
        return jsonify(error="bad value"), 400
    value = min(max(value, low), high)
    config.station.set(key, value)
    return jsonify(ok=True, key=key, value=value)


@app.post("/api/intro")
def set_intro():
    """Override where a track's vocal comes in, so the hosts stop talking into it."""
    payload = _json_object()
    key = str(payload.get("key") or "")
    if not db.one("SELECT 1 FROM tracks WHERE key=?", (key,)):
        return jsonify(error="unknown track"), 404
    raw = payload.get("seconds")
    try:
        seconds = None if raw in (None, "") else min(max(_finite(raw), 0.0), 60.0)
    except ValueError:
        return jsonify(error="seconds must be a number"), 400
    db.write("UPDATE tracks SET intro_override=? WHERE key=?", (seconds, key))
    return jsonify(ok=True, key=key, intro_override=seconds)


@app.get("/api/lyrics/<path:key>")
def get_lyrics(key: str):
    """A track's synced lines and the sections read off them, if LRCLIB had them."""
    from . import lyrics
    if not key or len(key) > 500:
        return jsonify(error="Track key required"), 400
    found = lyrics.payload(key)
    if found is None:
        return jsonify(error="No lyrics for this track"), 404
    return jsonify(found)


@app.get("/api/transition")
def get_transition():
    from . import transitions
    return jsonify({
        "preset": str(config.station.get("transitions.preset", "auto")),
        "options": ["auto", *transitions.PRESETS, *transitions.TECHNIQUES],
    })


@app.post("/api/transition")
def set_transition():
    """Force a transition style, or hand it back to the automatic picker."""
    from . import transitions
    payload = _json_object()
    preset = str(payload.get("preset") or "").strip().lower()
    if preset not in ("auto", *transitions.PRESETS, *transitions.TECHNIQUES):
        return jsonify(error=f"not a transition. Try one of: auto, "
                             f"{', '.join((*transitions.PRESETS, *transitions.TECHNIQUES))}"), 400
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


# --------------------------------------------------------------------------
# Events: the polled views, pushed when they change
# --------------------------------------------------------------------------
# One Server-Sent Events stream in place of four polling loops. Each topic's
# data is exactly what its GET returns. Every connection is a generator on its
# own server thread, so connections are counted and capped. A stream ends when
# the station shuts down, or when Werkzeug closes it after a write to a
# vanished client fails -- within a few seconds, because the schedule is
# re-sent that often.
EVENT_TOPICS: dict[str, Callable[[Any], dict[str, Any]]] = {
    "schedule": schedule_payload,
    "queue": queue_payload,
    "status": lambda station: status_payload(station, lite=True),
    "vibe": vibe_payload,
}
EVENT_TICK = 0.5          # how often each topic is checked for a change
SCHEDULE_REFRESH = 5.0    # `now` moves on, so the schedule goes out this often anyway
PING_EVERY = 15.0
MAX_EVENT_STREAMS = 8
# A live stream is pulled every tick. One not pulled for this long belongs to
# a request thread that is gone: on Windows, Werkzeug can die draining a reset
# socket before it ever calls close(), so the count cannot rest on close().
STALE_STREAM = 30.0
_streams: dict[int, float] = {}          # stream id -> last pulled (monotonic)
_streams_lock = threading.Lock()
_stream_ids = itertools.count(1)
_closing = threading.Event()   # set once, by shutdown


def open_event_streams() -> int:
    with _streams_lock:
        return len(_streams)


def _claim_stream() -> int | None:
    now = time.monotonic()
    with _streams_lock:
        for ident, pulled in list(_streams.items()):
            if now - pulled > STALE_STREAM:
                del _streams[ident]
        if len(_streams) >= MAX_EVENT_STREAMS:
            return None
        ident = next(_stream_ids)
        _streams[ident] = now
        return ident


def _release_stream(ident: int) -> None:
    with _streams_lock:
        _streams.pop(ident, None)


def _fingerprint(topic: str, payload: dict[str, Any]) -> str:
    """What counts as a change. Fields that move with the clock alone do not:
    the clients interpolate those, and resending them would be every tick."""
    if topic in ("schedule", "queue"):
        payload = {k: v for k, v in payload.items() if k != "now"}
    if topic == "queue":
        payload = {**payload, "items": [{k: v for k, v in item.items() if k != "eta"}
                                        for item in payload.get("items") or []]}
    if topic == "status" and isinstance(payload.get("now_playing"), dict):
        payload = {**payload, "now_playing": {k: v for k, v in payload["now_playing"].items()
                                              if k != "position"}}
    return json.dumps(payload, sort_keys=True, default=str)


class EventStream:
    """The response body of one /api/events connection.

    A class rather than a bare generator for close(): Werkzeug calls it when
    the client goes away, including before the first chunk -- when a
    generator's own finally would never run. The body generator holds no
    reference back to this object, so when Werkzeug drops the response
    without closing it, both are freed at once and the generator's finally
    still releases the stream; the staleness check is the last resort.
    """

    def __init__(self, station: Any, topics: list[str], ident: int, *,
                 clock: Callable[[], float] = time.monotonic,
                 wait: Callable[[float], bool] | None = None) -> None:
        self._ident = ident
        self._done = False
        self._body = _event_body(station, topics, ident, clock, wait or _closing.wait)

    def __iter__(self) -> Iterator[str]:
        return self

    def __next__(self) -> str:
        if not self._done:
            with _streams_lock:
                # Also re-registers a stream that was only slow, never gone.
                _streams[self._ident] = time.monotonic()
        try:
            return next(self._body)
        except StopIteration:
            self._done = True
            raise

    def close(self) -> None:
        self._done = True
        try:
            self._body.close()
        finally:
            _release_stream(self._ident)


def _event_body(station: Any, topics: list[str], ident: int,
                clock: Callable[[], float], wait: Callable[[float], bool]) -> Iterator[str]:
    sent: dict[str, str] = {}
    sent_at: dict[str, float] = {}
    last_ping = clock()
    try:
        yield "retry: 3000\n\n"
        while not _closing.is_set():
            # An open stream is a listener, exactly like a schedule poll.
            station.heartbeat()
            now = clock()
            chunks = []
            for topic in topics:
                try:
                    payload = EVENT_TOPICS[topic](station)
                except Exception as error:  # noqa: BLE001 - skip one tick, keep the stream
                    _log("event topic failed", topic, repr(error))
                    continue
                mark = _fingerprint(topic, payload)
                due = topic == "schedule" and now - sent_at.get(topic, -1e9) >= SCHEDULE_REFRESH
                if mark != sent.get(topic) or due:
                    sent[topic], sent_at[topic] = mark, now
                    chunks.append(f"event: {topic}\ndata: {json.dumps(payload, default=str)}\n\n")
            if now - last_ping >= PING_EVERY:
                chunks.append(": ping\n\n")
                last_ping = now
            if chunks:
                yield "".join(chunks)
            if wait(EVENT_TICK):
                return
    finally:
        _release_stream(ident)


@app.get("/api/events")
def events():
    raw = request.args.get("topics", "")
    topics = list(dict.fromkeys(t.strip() for t in raw.split(",") if t.strip())) or list(EVENT_TOPICS)
    unknown = [t for t in topics if t not in EVENT_TOPICS]
    if unknown:
        return jsonify(error=f"unknown topics: {', '.join(unknown)}"), 400
    if _closing.is_set():
        return jsonify(error="the station is shutting down"), 503
    ident = _claim_stream()
    if ident is None:
        return jsonify(error="too many event streams; poll instead"), 503
    try:
        body = EventStream(director.station(), topics, ident)
    except BaseException:
        _release_stream(ident)
        raise
    return Response(body, mimetype="text/event-stream",
                    headers={"Cache-Control": "no-store", "X-Accel-Buffering": "no"})


# --------------------------------------------------------------------------
# Shutdown
# --------------------------------------------------------------------------
_shutdown_lock = threading.Lock()
_shut_down = False


def _exit_process() -> None:
    os._exit(0)  # noqa: SLF001 - the station's own state is already put away


def shut_down_station() -> bool:
    """Put the station away once: session note, cache purge, clock stop.

    Returns False when it had already been done (a second request, or the
    signal handler after a request).
    """
    global _shut_down
    with _shutdown_lock:
        if _shut_down:
            return False
        _shut_down = True
    _closing.set()
    # The tunnel first: nothing should reach a station on its way down.
    from . import remote_tunnel
    remote_tunnel.stop()
    director.station().shutdown()
    return True


@app.post("/api/shutdown")
def shutdown():
    """The console's way to stop the station: answer, then leave."""
    if request.environ.get("defalt.remote") is not None:
        # The console at home owns the station; a phone does not turn it off.
        return jsonify(error="the station can only be shut down at home"), 403
    first = False
    try:
        first = shut_down_station()
    except Exception as error:  # noqa: BLE001 - still leave; that was the request
        _log("shutdown failed", repr(error))
        first = True
    response = jsonify(ok=True)
    if first:
        # After the response has been written, not before: the caller is
        # waiting on this answer to know the station went down cleanly.
        response.call_on_close(lambda: threading.Timer(0.25, _exit_process).start())
    return response


@app.errorhandler(500)
def server_error(_error: Any):
    return jsonify(error="something broke -- check the server log"), 500


def create_app() -> Flask:
    config.ensure_dirs()
    vault.ensure_scaffold()
    director.station()  # start the threads
    from . import enrich
    enrich.start(_closing)
    from . import lyrics
    lyrics.start(_closing)
    from . import editions
    editions.start(_closing, lambda: director.station()._janitor_protected()[0])
    return app
