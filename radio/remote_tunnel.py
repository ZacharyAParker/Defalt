"""The Cloudflare tunnel, for a station run on its own (`python -m radio`).

When the console starts the station it runs the tunnel itself (and says so
with DEFALT_CONSOLE=1), so this only ever runs standalone. Same rules as the
console's: up only while the station is, started once the server is
actually listening, stopped first on the way out, restarted with backoff if
cloudflared dies, and never started without Cloudflare Access configured.

Nothing can be streamed from here -- the stream is the console's output --
but the page, requests and the rest of the controls work remotely.
"""
from __future__ import annotations

import atexit
import os
import shutil
import socket
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Callable

from . import config

DEFAULT_BIN = r"C:\Program Files (x86)\cloudflared\cloudflared.exe"
REGISTERED = "Registered tunnel connection"
UNREGISTERED = ("Unregistered tunnel connection", "Connection terminated", "Retrying connection")
BACKOFF_MAX = 60.0


class NotConfigured(Exception):
    pass


def tunnel_command(env: Callable[[str], str] = config.env,
                   exists: Callable[[Path], bool] = lambda p: p.is_file(),
                   which: Callable[[str], str | None] = shutil.which) -> list[str]:
    """cloudflared's argv, or NotConfigured with the reason. Fails closed."""
    name = env("REMOTE_TUNNEL").strip()
    if not name:
        raise NotConfigured("no REMOTE_TUNNEL in .env")
    for required in ("REMOTE_HOSTS", "CF_ACCESS_TEAM_DOMAIN", "CF_ACCESS_AUD"):
        if not env(required).strip():
            raise NotConfigured(f"{required} is not set; the tunnel stays down")
    binary = Path(env("CLOUDFLARED_BIN").strip() or which("cloudflared") or DEFAULT_BIN)
    explicit = env("CLOUDFLARED_CONFIG").strip()
    if explicit:
        tunnel_config = Path(explicit)
    else:
        home = env("USERPROFILE").strip() or env("HOME").strip()
        if not home:
            raise NotConfigured("no home folder for the cloudflared config")
        tunnel_config = Path(home) / ".cloudflared" / f"{name}.yml"
    if not exists(binary):
        raise NotConfigured(f"cloudflared not found ({binary})")
    if not exists(tunnel_config):
        raise NotConfigured(f"no tunnel config at {tunnel_config}")
    return [str(binary), "tunnel", "--config", str(tunnel_config), "run", name]


def follow(connections: int, line: str) -> int:
    """Edge connections up after this log line."""
    if REGISTERED in line:
        return connections + 1
    if connections and any(marker in line for marker in UNREGISTERED):
        return connections - 1
    return connections


def _listening(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return True
    except OSError:
        return False


class Tunnel:
    def __init__(self, port: int, *, command: Callable[[], list[str]] = tunnel_command,
                 spawn: Callable[..., Any] = subprocess.Popen,
                 enabled: Callable[[], bool] = lambda: bool(config.station.get("remote.enabled", True)),
                 listening: Callable[[int], bool] = _listening) -> None:
        self.port = port
        self._command = command
        self._spawn = spawn
        self._enabled = enabled
        self._is_listening = listening
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self._process: Any = None
        self._thread: threading.Thread | None = None
        self.state = "off"
        self.connections = 0
        self.note = ""

    def status(self) -> dict[str, Any]:
        return {"state": self.state, "connections": self.connections, "note": self.note or None,
                "configured": not self.note.startswith("not configured")}

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name="remote-tunnel", daemon=True)
        self._thread.start()

    def _run(self) -> None:
        # Only once the station answers: a tunnel to nothing is a 502 page.
        while not self._stop.is_set() and not self._is_listening(self.port):
            self._stop.wait(0.25)
        backoff = 2.0
        while not self._stop.is_set():
            if not self._enabled():
                self._kill()
                self.state, self.note = "off", "switched off in settings"
                self._stop.wait(5.0)
                continue
            try:
                argv = self._command()
            except NotConfigured as why:
                self.state, self.note = "off", f"not configured: {why}"
                return
            began = time.monotonic()
            if not self._launch(argv):
                self._stop.wait(backoff)
                backoff = min(backoff * 2, BACKOFF_MAX)
                continue
            process = self._process
            while not self._stop.is_set() and process.poll() is None and self._enabled():
                self._stop.wait(1.0)
            if self._stop.is_set() or not self._enabled():
                continue
            print(f"[tunnel] cloudflared exited ({process.returncode}); retrying in {backoff:.0f}s", flush=True)
            self.state, self.connections = "connecting", 0
            if time.monotonic() - began > 120:
                backoff = 2.0
            self._stop.wait(backoff)
            backoff = min(backoff * 2, BACKOFF_MAX)

    def _launch(self, argv: list[str]) -> bool:
        flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
        try:
            process = self._spawn(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                  stdin=subprocess.DEVNULL, creationflags=flags,
                                  text=True, encoding="utf-8", errors="replace")
        except OSError as error:
            print(f"[tunnel] could not start cloudflared: {error}", flush=True)
            self.state, self.note = "connecting", str(error)
            return False
        with self._lock:
            if self._stop.is_set():
                process.kill()
                return False
            self._process = process
        self.state, self.connections, self.note = "connecting", 0, ""
        print(f"[tunnel] starting: {' '.join(argv[1:])}", flush=True)
        threading.Thread(target=self._pump, args=(process,), daemon=True).start()
        return True

    def _pump(self, process: Any) -> None:
        for line in process.stdout or ():
            line = line.strip()
            if not line:
                continue
            print(f"[tunnel] {line}", flush=True)
            if process is self._process:
                self.connections = follow(self.connections, line)
                self.state = "online" if self.connections else "connecting"

    def _kill(self) -> None:
        with self._lock:
            process, self._process = self._process, None
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        self.connections = 0

    def stop(self) -> None:
        """Tunnel down. Safe to call more than once, from anywhere."""
        self._stop.set()
        self._kill()
        self.state = "off"


_tunnel: Tunnel | None = None


def status() -> dict[str, Any] | None:
    return _tunnel.status() if _tunnel is not None else None


def start_standalone(port: int) -> Tunnel | None:
    """Run the tunnel around this server, unless the console runs it."""
    global _tunnel
    if os.environ.get("DEFALT_CONSOLE") or not config.env("REMOTE_TUNNEL"):
        return None
    _tunnel = Tunnel(port)
    atexit.register(_tunnel.stop)
    _tunnel.start()
    return _tunnel


def stop() -> None:
    if _tunnel is not None:
        _tunnel.stop()
