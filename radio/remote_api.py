"""Remote listening, from the station's side.

The console at home encodes its own output (every stem swap, spinback and
reverb tail, exactly as the speakers play it) and serves it on a loopback
port. `/listen` passes that stream through, behind the same Host and
Cloudflare Access checks as everything else here, so a phone never needs
the browser mixer at all. `/api/remote/status` says whether any of it is
there to listen to.

Also here: the home-screen app's manifest and service worker, which have to
be served from the root to cover the whole site.
"""
from __future__ import annotations

import http.client
import json
import time
from typing import Any, Iterator

from flask import Blueprint, Response, jsonify, request

from . import config, director, remote_auth

blueprint = Blueprint("remote", __name__)

DEFAULT_BROADCAST_PORT = 8091
CHUNK = 16 * 1024
UPSTREAM_TIMEOUT = 15.0     # seconds without a byte before the proxy gives up
HEARTBEAT_EVERY = 5.0       # a stream listener keeps the station clock running
NOT_RUNNING = "The console isn't running at home — start Defalt to listen"


def broadcast_port() -> int:
    try:
        return int(config.env("BROADCAST_PORT", str(DEFAULT_BROADCAST_PORT)))
    except ValueError:
        return DEFAULT_BROADCAST_PORT


def _connect(timeout: float) -> http.client.HTTPConnection:
    return http.client.HTTPConnection("127.0.0.1", broadcast_port(), timeout=timeout)


def broadcast_status() -> dict[str, Any] | None:
    """The console's stream server's own status, or None if it is not there."""
    connection = _connect(1.5)
    try:
        connection.request("GET", "/status", headers={"Host": f"127.0.0.1:{broadcast_port()}"})
        response = connection.getresponse()
        if response.status != 200:
            return None
        body = json.loads(response.read(64 * 1024).decode("utf-8"))
        return body if isinstance(body, dict) else None
    except (OSError, ValueError, http.client.HTTPException):
        return None
    finally:
        connection.close()


def settings() -> dict[str, bool]:
    """station.yaml `remote.*`. On unless switched off, but only meaningful
    when a tunnel is configured at all."""
    return {
        "enabled": bool(config.station.get("remote.enabled", True)),
        "mute_local": bool(config.station.get("remote.mute_local", False)),
    }


def _standalone_tunnel() -> dict[str, Any] | None:
    try:
        from . import remote_tunnel
    except ImportError:
        return None
    return remote_tunnel.status()


def _not_running() -> Response:
    if "text/html" in (request.headers.get("Accept") or ""):
        page = ("<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>"
                "<title>Defalt</title><body style='background:#100e0c;color:#ece5d8;font:16px system-ui;"
                f"padding:2em'><p>{NOT_RUNNING}.</p>")
        return Response(page, status=503, mimetype="text/html", headers={"Cache-Control": "no-store"})
    response = jsonify(error=NOT_RUNNING, console_running=False)
    response.status_code = 503
    response.headers["Cache-Control"] = "no-store"
    return response


@blueprint.get("/api/remote/status")
def remote_status():
    broadcast = broadcast_status()
    tunnel = (broadcast or {}).get("tunnel") or _standalone_tunnel() or {"state": "off"}
    return jsonify({
        **settings(),
        "remote_hosts": sorted(remote_auth.remote_hosts()),
        "access_configured": remote_auth.configured(),
        "tunnel_configured": bool(config.env("REMOTE_TUNNEL")),
        "tunnel": tunnel,
        "console_running": broadcast is not None,
        "broadcast": ({k: v for k, v in broadcast.items() if k != "tunnel"} if broadcast else None),
        "remote": request.environ.get("defalt.remote") is not None,
    })


@blueprint.post("/api/remote/config")
def remote_config():
    payload = request.get_json(silent=True)
    if not isinstance(payload, dict):
        return jsonify(error="need a settings object"), 400
    values = {}
    for key in ("enabled", "mute_local"):
        if key in payload:
            if not isinstance(payload[key], bool):
                return jsonify(error=f"{key} must be true or false"), 400
            values[f"remote.{key}"] = payload[key]
    if not values:
        return jsonify(error="nothing to change"), 400
    config.station.set_many(values)
    return jsonify(ok=True, **settings())


class StreamProxy:
    """The body of one /listen: the console's bytes, as they come.

    A class for close(), which Werkzeug calls when the listener hangs up --
    the upstream connection is closed with it, so the console sees the
    listener leave at once rather than at its next failed write.
    """

    def __init__(self, connection: http.client.HTTPConnection, upstream: http.client.HTTPResponse,
                 station: Any) -> None:
        self._connection = connection
        self._upstream = upstream
        self._station = station
        self._beat = 0.0

    def __iter__(self) -> Iterator[bytes]:
        return self

    def __next__(self) -> bytes:
        now = time.monotonic()
        if now - self._beat >= HEARTBEAT_EVERY:
            self._beat = now
            try:
                self._station.heartbeat()
            except Exception:  # noqa: BLE001 - the audio matters more
                pass
        try:
            chunk = self._upstream.read1(CHUNK)
        except (OSError, http.client.HTTPException):
            chunk = b""
        if not chunk:
            self.close()
            raise StopIteration
        return chunk

    def close(self) -> None:
        try:
            self._upstream.close()
        finally:
            self._connection.close()


@blueprint.get("/listen")
def listen():
    connection = _connect(UPSTREAM_TIMEOUT)
    try:
        connection.request("GET", "/stream", headers={"Host": f"127.0.0.1:{broadcast_port()}"})
        upstream = connection.getresponse()
    except (OSError, http.client.HTTPException):
        connection.close()
        return _not_running()
    if upstream.status != 200:
        connection.close()
        return _not_running()
    station = director.station()
    headers = {
        "Cache-Control": "no-store",
        "X-Accel-Buffering": "no",
        "X-Content-Type-Options": "nosniff",
    }
    for name in ("X-Burst-Seconds", "X-Stream-Bitrate"):
        if upstream.getheader(name):
            headers[name] = upstream.getheader(name)
    return Response(StreamProxy(connection, upstream, station), status=200,
                    mimetype=upstream.getheader("Content-Type") or "audio/aac",
                    headers=headers, direct_passthrough=True)


# --------------------------------------------------------------------------
# The home-screen app
# --------------------------------------------------------------------------
@blueprint.get("/manifest.webmanifest")
def manifest():
    from flask import current_app
    path = config.ROOT / "web" / "manifest.webmanifest"
    response = current_app.response_class(path.read_bytes(), mimetype="application/manifest+json")
    response.headers["Cache-Control"] = "no-cache"
    return response


@blueprint.get("/sw.js")
def service_worker():
    from . import about
    source = (config.ROOT / "web" / "sw.js").read_text(encoding="utf-8")
    response = Response(source.replace("{{APP_VERSION}}", about.VERSION), mimetype="text/javascript")
    # Always checked: a stale worker would pin an old shell.
    response.headers["Cache-Control"] = "no-cache"
    response.headers["Service-Worker-Allowed"] = "/"
    return response
