"""Bug reports and suggestions, filed where you can work through them later.

reports/INBOX.md is the list. Each report is a folder beside it holding
report.md (what was said), context.json (what the station was doing) and
logs.txt (every log line from ten minutes before to a minute after). The
console writes the same layout itself when the station is down; see
src/reports.rs, and docs/FEEDBACK.md for the format.

A report is most wanted when something is already broken, so nothing here is
allowed to depend on the station being healthy: every piece of context is
optional, and a slow one is abandoned rather than waited on.

    python -m radio.feedback list
    python -m radio.feedback show <id>
    python -m radio.feedback close <id> [note]

The rolling log lives here too: logs/station.log, everything the station
prints, with a timestamp on every line. `install_log_tee` is called once from
radio/__main__.py.
"""
from __future__ import annotations

import base64
import binascii
import json
import os
import platform
import re
import subprocess
import sys
import threading
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Iterable

from . import config

LOG_DIR = config.ROOT / "logs"
REPORTS_DIR = config.ROOT / "reports"
LOG_MAX_BYTES = 5 * 1024 * 1024
LOG_BACKUPS = 5

# What a report attaches, around the moment it was filed.
WINDOW_BEFORE = 10 * 60
WINDOW_AFTER = 60
MAX_LOG_LINES = 2000

KINDS = ("bug", "suggestion")
CLIENTS = ("browser", "console", "cli")
MAX_TITLE = 200
MAX_TEXT = 20_000
MAX_CONTEXT_BYTES = 1_000_000
MAX_SCREENSHOT_BYTES = 12 * 1024 * 1024
CONTEXT_TIMEOUT = 3.0

INBOX_HEADER = """# Feedback inbox

Bug reports and suggestions filed from the console and the radio page, oldest
first. Each line points at a folder holding report.md, context.json and
logs.txt (and screenshot.png when one was taken). Close an item with
`python -m radio.feedback close <id> "what was done"`.

"""

_WRITE = threading.Lock()


# --------------------------------------------------------------------------
# Timestamps
# --------------------------------------------------------------------------
def stamp(moment: float | None = None) -> str:
    """ISO 8601, local, with the offset and milliseconds. Every log line
    starts with one; the console writes the same shape."""
    when = datetime.fromtimestamp(time.time() if moment is None else moment).astimezone()
    return when.isoformat(timespec="milliseconds")


_STAMP = re.compile(r"^(\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(?:\.\d{1,9})?(?:Z|[+-]\d\d:?\d\d)?)\s(.*)$")


def parse_stamp(text: str) -> float | None:
    """Seconds since the epoch, or None. A stamp with no offset is local."""
    text = text.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    # fromisoformat takes at most microseconds.
    text = re.sub(r"(\.\d{6})\d+", r"\1", text)
    try:
        when = datetime.fromisoformat(text)
    except ValueError:
        return None
    if when.tzinfo is None:
        when = when.astimezone()
    return when.timestamp()


# --------------------------------------------------------------------------
# The rolling log
# --------------------------------------------------------------------------
class RollingLog:
    """A text log that keeps `backups` old files of `max_bytes` each.

    Same scheme as the logging module's rotating handler (station.log.1 is
    the newest old one), written by hand so a line is always exactly one
    line with its own stamp, whatever wrote it.
    """

    def __init__(self, path: Path, max_bytes: int = LOG_MAX_BYTES, backups: int = LOG_BACKUPS):
        self.path = Path(path)
        self.max_bytes = max_bytes
        self.backups = backups
        self.lock = threading.Lock()
        self._file = None
        self._size = 0

    def _open(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._file = open(self.path, "a", encoding="utf-8", errors="replace", newline="\n")
        self._size = self._file.tell()

    def _rotate(self) -> None:
        if self._file is not None:
            self._file.close()
            self._file = None
        for index in range(self.backups - 1, 0, -1):
            older = self.path.with_name(f"{self.path.name}.{index}")
            if older.exists():
                os.replace(older, self.path.with_name(f"{self.path.name}.{index + 1}"))
        if self.path.exists():
            if self.backups > 0:
                os.replace(self.path, self.path.with_name(f"{self.path.name}.1"))
            else:
                self.path.unlink()

    def write(self, tag: str, text: str, moment: float | None = None) -> None:
        prefix = f"{stamp(moment)} [{tag}] "
        lines = [prefix + line for line in str(text).splitlines() if line.strip()]
        if not lines:
            return
        data = "\n".join(lines) + "\n"
        with self.lock:
            try:
                if self._file is None:
                    self._open()
                size = len(data.encode("utf-8", errors="replace"))
                if self._size and self._size + size > self.max_bytes:
                    self._rotate()
                    self._open()
                self._file.write(data)
                self._file.flush()
                self._size += size
            except OSError:
                # A full disk or a locked file must never stop the station.
                self._file = None

    def close(self) -> None:
        with self.lock:
            if self._file is not None:
                self._file.close()
                self._file = None


class _Tee:
    """Stands in for stdout or stderr: passes everything through, and copies
    each finished line into the log."""

    def __init__(self, original: Any, log: RollingLog, tag: str):
        self._original = original
        self._log = log
        self._tag = tag
        self._partial = ""
        self._lock = threading.Lock()

    def write(self, text: str) -> int:
        if self._original is not None:
            try:
                self._original.write(text)
            except (OSError, ValueError, UnicodeError):
                pass
        with self._lock:
            pending = self._partial + str(text)
            *finished, self._partial = pending.split("\n")
        for line in finished:
            # A carriage return redraws the line on a terminal; keep what the
            # terminal would have ended up showing.
            self._log.write(self._tag, line.rsplit("\r", 1)[-1])
        return len(text)

    def flush(self) -> None:
        if self._original is not None:
            try:
                self._original.flush()
            except (OSError, ValueError):
                pass

    def isatty(self) -> bool:
        return False

    def __getattr__(self, name: str) -> Any:
        if self._original is None:
            raise AttributeError(name)
        return getattr(self._original, name)


_installed: RollingLog | None = None


def install_log_tee(log_dir: Path | None = None) -> RollingLog:
    """Copy everything printed from here on into logs/station.log."""
    global _installed
    if _installed is not None:
        return _installed
    log = RollingLog(Path(log_dir or LOG_DIR) / "station.log")
    sys.stdout = _Tee(sys.stdout, log, "out")
    sys.stderr = _Tee(sys.stderr, log, "err")
    _installed = log
    from .about import VERSION
    log.write("out", f"station starting, v{VERSION}, pid {os.getpid()}")
    return log


# --------------------------------------------------------------------------
# Secrets
# --------------------------------------------------------------------------
_SECRET_NAME = re.compile(
    r"(api[_-]?key|secret|token|passw(or)?d|authorization|credential|cookie|private[_-]?key|sid$)",
    re.IGNORECASE)
_PATTERNS = [
    (re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(-----END [A-Z ]*PRIVATE KEY-----|$)", re.S),
     "[redacted private key]"),
    (re.compile(r"\bsk-[A-Za-z0-9_\-]{8,}"), "sk-[redacted]"),
    (re.compile(r"\bAIza[0-9A-Za-z_\-]{20,}"), "[redacted]"),
    (re.compile(r"\b(gh[pousr]_[A-Za-z0-9]{20,})"), "[redacted]"),
    (re.compile(r"\b(xox[abpr]-[A-Za-z0-9\-]{10,})"), "[redacted]"),
    (re.compile(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=\-]{8,}"), r"\1 [redacted]"),
    # name=value and "name": "value", where the name says it is a secret.
    (re.compile(r"(?i)([\w\-]*(?:api[_-]?key|secret|token|passw(?:or)?d|authorization|client[_-]?id)[\w\-]*)"
                r"(\"?'?\s*[:=]\s*\"?'?)([^\s\"'&,;}]{4,})"),
     r"\1\2[redacted]"),
    # A bare key= only when the value looks machine-made; track keys are
    # "artist|title" and must survive.
    (re.compile(r"(?i)(?<![\w])(key=)([A-Za-z0-9_\-\.]{20,})(?=$|[&\s\"',;])"), r"\1[redacted]"),
]


def _env_secrets(root: Path | None = None) -> list[str]:
    """Values from .env (and the environment) that must never be written.

    Anything whose name says secret, plus anything long enough to be a key
    rather than a port or a folder."""
    found: set[str] = set()
    pairs: list[tuple[str, str]] = []
    try:
        text = (Path(root or config.ROOT) / ".env").read_text(encoding="utf-8", errors="replace")
        for line in text.splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            name, value = line.split("=", 1)
            pairs.append((name.strip().removeprefix("export ").strip(), value.strip().strip("'\"")))
    except OSError:
        pass
    pairs.extend((name, value) for name, value in os.environ.items() if _SECRET_NAME.search(name))
    for name, value in pairs:
        if len(value) < 6:
            continue
        looks_like_path = any(mark in value for mark in ("/", "\\", " ")) and not _SECRET_NAME.search(name)
        if _SECRET_NAME.search(name) or (len(value) >= 16 and not looks_like_path):
            found.add(value)
    return sorted(found, key=len, reverse=True)


class Redactor:
    def __init__(self, secrets: Iterable[str] | None = None):
        self.secrets = [s for s in (secrets if secrets is not None else _env_secrets()) if s]

    def text(self, value: str) -> str:
        value = str(value)
        for secret in self.secrets:
            if secret in value:
                value = value.replace(secret, "[redacted]")
        for pattern, replacement in _PATTERNS:
            value = pattern.sub(replacement, value)
        return value

    def value(self, value: Any) -> Any:
        if isinstance(value, dict):
            return {str(k): ("[redacted]" if _SECRET_NAME.search(str(k)) and v not in (None, "", True, False)
                             and not isinstance(v, (dict, list)) else self.value(v))
                    for k, v in value.items()}
        if isinstance(value, (list, tuple)):
            return [self.value(v) for v in value]
        if isinstance(value, str):
            return self.text(value)
        return value


def redact(text: str) -> str:
    return Redactor().text(text)


# --------------------------------------------------------------------------
# Log window
# --------------------------------------------------------------------------
_ECHO = re.compile(r"^\[station(?::err)?\] (.*)$")
_OWN = re.compile(r"^\[(?:out|err)\] (.*)$")


def _log_files(log_dir: Path) -> list[Path]:
    if not log_dir.is_dir():
        return []
    return sorted(p for p in log_dir.iterdir()
                  if p.is_file() and re.fullmatch(r"[\w.\-]+\.log(\.\d+)?", p.name))


def _source(path: Path) -> str:
    return path.name.split(".log", 1)[0]


def read_entries(path: Path, source: str | None = None) -> list[tuple[float, str, str]]:
    """(time, source, text) for every line. A line with no stamp of its own
    -- a traceback's middle, say -- takes the one before it."""
    source = source or _source(path)
    entries: list[tuple[float, str, str]] = []
    last: float | None = None
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            for raw in handle:
                line = raw.rstrip("\r\n")
                if not line.strip():
                    continue
                match = _STAMP.match(line)
                moment = parse_stamp(match.group(1)) if match else None
                if moment is not None:
                    last = moment
                    entries.append((moment, source, match.group(2)))
                elif last is not None:
                    entries.append((last, source, line))
    except OSError:
        pass
    return entries


def log_window(at: float, log_dir: Path | None = None, before: float = WINDOW_BEFORE,
               after: float = WINDOW_AFTER, limit: int = MAX_LOG_LINES,
               extra: Iterable[tuple[float, str, str]] = ()) -> list[str]:
    """Every log line near `at`, from every log, oldest first.

    The console copies the station's output into its own log when it started
    the station, so those copies are dropped wherever the station's own line
    is there too."""
    log_dir = Path(log_dir or LOG_DIR)
    start, end = at - before, at + after
    entries: list[tuple[float, int, str, str]] = []
    order = 0
    for path in _log_files(log_dir):
        try:
            if path.stat().st_mtime < start:
                continue
        except OSError:
            continue
        for moment, source, text in read_entries(path):
            if start <= moment <= end:
                entries.append((moment, order, source, text))
                order += 1
    for moment, source, text in extra:
        if start <= moment <= end:
            entries.append((moment, order, source, text))
            order += 1

    own: dict[str, list[float]] = {}
    for moment, _, source, text in entries:
        match = _OWN.match(text)
        if source == "station" and match:
            own.setdefault(match.group(1).strip(), []).append(moment)

    seen: set[tuple[float, str, str]] = set()
    kept: list[tuple[float, int, str, str]] = []
    for entry in sorted(entries):
        moment, _, source, text = entry
        echo = _ECHO.match(text) if source != "station" else None
        if echo and any(abs(t - moment) <= 10 for t in own.get(echo.group(1).strip(), ())):
            continue
        key = (round(moment, 3), source, text)
        if key in seen:
            continue
        seen.add(key)
        kept.append(entry)

    lines = [f"{stamp(moment)} {source} {text}" for moment, _, source, text in kept]
    if len(lines) > limit:
        dropped = len(lines) - limit
        lines = [f"... {dropped} earlier lines left out ..."] + lines[-limit:]
    return lines


def _client_entries(lines: Any, source: str) -> list[tuple[float, str, str]]:
    """Log lines a client sent along, in the same stamped shape."""
    if isinstance(lines, str):
        lines = lines.splitlines()
    if not isinstance(lines, list):
        return []
    entries: list[tuple[float, str, str]] = []
    last = time.time()
    for raw in lines[-MAX_LOG_LINES:]:
        if not isinstance(raw, str) or not raw.strip():
            continue
        line = raw.rstrip()[:4000]
        match = _STAMP.match(line)
        moment = parse_stamp(match.group(1)) if match else None
        if moment is not None:
            last = moment
            text = match.group(2)
            # Console lines already name their file; keep that as the source.
            named = re.match(r"^(console|station)\s(.*)$", text)
            if named:
                entries.append((moment, named.group(1), named.group(2)))
                continue
            entries.append((moment, source, text))
        else:
            entries.append((last, source, line))
    return entries


# --------------------------------------------------------------------------
# Context
# --------------------------------------------------------------------------
def _item(item: Any, now: float) -> dict[str, Any]:
    meta = getattr(item, "meta", {}) or {}
    row = {"id": getattr(item, "id", None), "kind": getattr(item, "kind", None),
           "starts_in": round(getattr(item, "start_at", 0.0) - now, 1),
           "duration": round(getattr(item, "duration", 0.0) or 0.0, 1)}
    for key in ("title", "artist", "key", "host", "segment", "bpm", "camelot", "transition", "selection_origin"):
        if meta.get(key) not in (None, ""):
            row[key] = meta.get(key)
    if getattr(item, "kind", "") == "voice" and meta.get("text"):
        row["text"] = str(meta["text"])[:300]
    return row


def station_context(timeout: float = CONTEXT_TIMEOUT) -> dict[str, Any]:
    """What the station was doing, as far as it will say within `timeout`.

    Each piece is taken separately, so a lock held by a stuck thread costs
    only the pieces behind it."""
    found: dict[str, Any] = {}
    problems: dict[str, str] = {}

    def gather() -> None:
        from . import director
        station = director.station()

        def piece(name: str, fn):
            try:
                found[name] = fn()
            except Exception as error:  # noqa: BLE001 -- a report must survive anything
                problems[name] = f"{type(error).__name__}: {error}"[:300]

        piece("status_note", lambda: station.status_note)
        piece("clock", lambda: {"now": round(station.clock.now(), 2), "running": station.clock.running})
        piece("now_playing", station.now_playing)

        def coming() -> list[dict[str, Any]]:
            with station.lock:
                now = station.clock.now()
                items = sorted((i for i in station.schedule.items if i.end_at > now),
                               key=lambda i: i.start_at)[:16]
                return [_item(i, now) for i in items]

        piece("schedule", coming)
        piece("queue", lambda: station.lineup()[:20])
        piece("transcript", lambda: [
            {k: row.get(k) for k in ("host", "text", "start_at", "active")}
            for row in station.transcript()[-12:]])

        def vibe_now():
            from . import vibe
            return {"public": vibe.public(), "direction": vibe.selection_direction()}

        piece("vibe", vibe_now)

        def chat():
            from .director_chat import for_station
            state = for_station(station).state()
            return {"messages": [{k: m.get(k) for k in ("role", "text")}
                                 for m in state.get("messages", [])[-10:]],
                    "direction": state.get("direction"), "busy": state.get("busy"),
                    "quiet_minutes": state.get("quiet_minutes")}

        piece("director_chat", chat)

        def mix():
            from . import mixconfig
            return {field["key"]: field["value"] for field in mixconfig.snapshot().get("fields", [])}

        piece("mix_settings", mix)

        def llm_state():
            from . import llm
            status = llm.status()
            return {"configured": status.get("configured"), "models": status.get("models"),
                    "benched": status.get("benched")}

        piece("llm", llm_state)
        piece("discovery", lambda: getattr(station, "discovery_status", None))
        piece("trends", lambda: getattr(station, "trend_status", None))
        piece("ads", lambda: station.ads.public() if hasattr(station, "ads") else None)

    worker = threading.Thread(target=gather, daemon=True, name="feedback-context")
    worker.start()
    worker.join(timeout)
    result = dict(found)
    if worker.is_alive():
        problems["timeout"] = f"gave up after {timeout:.0f}s; the rest was not collected"
    if problems:
        result["unavailable"] = dict(problems)
    return result


def _git(*args: str) -> str:
    try:
        done = subprocess.run(["git", *args], cwd=config.ROOT, capture_output=True, text=True,
                              timeout=3, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        return done.stdout.strip() if done.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def environment() -> dict[str, Any]:
    from .about import VERSION
    commit = _git("rev-parse", "--short", "HEAD")
    dirty = bool(_git("status", "--porcelain", "--untracked-files=no")) if commit else False
    return {"app": f"Defalt v{VERSION}", "version": VERSION,
            "commit": (commit + (" (uncommitted changes)" if dirty else "")) or "unknown",
            "branch": _git("rev-parse", "--abbrev-ref", "HEAD") or "unknown",
            "os": platform.platform(), "python": platform.python_version()}


# --------------------------------------------------------------------------
# Filing
# --------------------------------------------------------------------------
def slug(title: str) -> str:
    words = re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")
    return words[:40].rstrip("-") or "report"


def _clean_line(text: str, limit: int) -> str:
    return re.sub(r"\s+", " ", str(text)).replace("|", "/").strip()[:limit]


def validate(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        raise ValueError("Send a report object.")
    kind = str(payload.get("kind") or "bug").strip().lower()
    if kind not in KINDS:
        raise ValueError("Choose bug or suggestion.")
    title = payload.get("title") or ""
    description = payload.get("description") or ""
    expected = payload.get("expected") or ""
    for name, value, limit in (("title", title, MAX_TITLE * 4), ("description", description, MAX_TEXT),
                               ("expected", expected, MAX_TEXT)):
        if not isinstance(value, str):
            raise ValueError(f"The {name} must be text.")
        if len(value) > limit:
            raise ValueError(f"The {name} is too long.")
    title = _clean_line(title, MAX_TITLE)
    if not title:
        title = _clean_line(description.strip().split("\n", 1)[0], 80)
    if not title:
        raise ValueError("Give the report a title or a description.")
    client = str(payload.get("client") or "browser")
    if client not in CLIENTS:
        raise ValueError("Unknown client.")
    attach = payload.get("attach_logs", True)
    if not isinstance(attach, bool):
        raise ValueError("attach_logs must be true or false.")
    context = payload.get("client_context")
    context = {} if context is None else context
    if not isinstance(context, dict):
        raise ValueError("client_context must be an object.")
    if len(json.dumps(context, default=str)) > MAX_CONTEXT_BYTES:
        raise ValueError("client_context is too large.")
    screenshot = None
    if payload.get("screenshot_png"):
        try:
            screenshot = base64.b64decode(str(payload["screenshot_png"]), validate=True)
        except (binascii.Error, ValueError):
            raise ValueError("The screenshot could not be read.") from None
        if len(screenshot) > MAX_SCREENSHOT_BYTES or not screenshot.startswith(b"\x89PNG"):
            raise ValueError("The screenshot must be a PNG under 12 MB.")
    return {"kind": kind, "title": title, "description": description.strip(),
            "expected": expected.strip(), "client": client, "attach_logs": attach,
            "client_context": context, "client_logs": payload.get("client_logs"),
            "screenshot": screenshot}


def _folder(reports: Path, when: datetime, title: str) -> Path:
    base = f"{when:%Y%m%d-%H%M%S}-{slug(title)}"
    for attempt in range(1, 100):
        folder = reports / (base if attempt == 1 else f"{base}-{attempt}")
        try:
            folder.mkdir(parents=True)
            return folder
        except FileExistsError:
            continue
    raise OSError("could not find a free report folder")


def _report_md(report: dict[str, Any], ident: str, when: datetime, env: dict[str, Any],
               state: str, attachments: list[str]) -> str:
    heading = "Bug" if report["kind"] == "bug" else "Suggestion"
    lines = [
        f"# {heading}: {report['title']}", "",
        f"- id: {ident}",
        f"- filed: {when:%Y-%m-%d %H:%M:%S} ({when.isoformat(timespec='seconds')})",
        f"- type: {report['kind']}",
        "- status: open",
        f"- from: {report['client']}",
        f"- app: {env.get('app')}",
        f"- commit: {env.get('commit')} on {env.get('branch')}",
        f"- os: {env.get('os')}",
        f"- python: {env.get('python')}",
        f"- station: {state}",
        "", "## What happened" if report["kind"] == "bug" else "## Suggestion", "",
        report["description"] or "(no description)", "",
    ]
    if report["expected"]:
        lines += ["## What I expected", "", report["expected"], ""]
    lines += ["## Attached", ""] + [f"- {name}" for name in attachments] + [""]
    return "\n".join(lines)


def _inbox_line(ident: str, when: datetime, kind: str, title: str, status: str = "open",
                note: str = "") -> str:
    box = "x" if status == "closed" else " "
    line = (f"- [{box}] {ident} | {when:%Y-%m-%d %H:%M:%S} | {kind} | {status} | "
            f"{_clean_line(title, MAX_TITLE)} | {ident}/report.md")
    return line + (f" | {_clean_line(note, 500)}" if note else "")


def _append_inbox(reports: Path, line: str) -> None:
    inbox = reports / "INBOX.md"
    with _WRITE:
        fresh = not inbox.exists()
        with open(inbox, "a", encoding="utf-8", newline="\n") as handle:
            if fresh:
                handle.write(INBOX_HEADER)
            handle.write(line + "\n")


def submit(payload: Any, *, context: dict[str, Any] | None = None, reports_dir: Path | None = None,
           log_dir: Path | None = None, now: float | None = None,
           redactor: Redactor | None = None) -> dict[str, Any]:
    """File one report. Returns {id, path}. Raises ValueError on a bad payload."""
    report = validate(payload)
    reports = Path(reports_dir or REPORTS_DIR)
    moment = time.time() if now is None else now
    when = datetime.fromtimestamp(moment).astimezone()
    scrub = redactor or Redactor()
    for field in ("title", "description", "expected"):
        report[field] = scrub.text(report[field])

    folder = _folder(reports, when, report["title"])
    ident = folder.name
    env = environment()

    context = dict(context or {})
    body: dict[str, Any] = {
        "id": ident, "filed": when.isoformat(timespec="seconds"), "kind": report["kind"],
        "client": report["client"], "environment": env, "station": context,
    }
    if report["client_context"]:
        body[report["client"]] = report["client_context"]
    body = scrub.value(body)
    (folder / "context.json").write_text(json.dumps(body, indent=2, ensure_ascii=False, default=str),
                                         encoding="utf-8")
    attachments = ["context.json"]

    if report["attach_logs"]:
        extra = _client_entries(report["client_logs"], report["client"])
        lines = log_window(moment, log_dir, extra=extra)
        text = "\n".join(scrub.text(line) for line in lines)
        (folder / "logs.txt").write_text((text + "\n") if text else "(no log lines in the window)\n",
                                         encoding="utf-8")
        attachments.append(f"logs.txt ({len(lines)} lines, {WINDOW_BEFORE // 60} min before to "
                           f"{WINDOW_AFTER // 60} min after)")
    if report["screenshot"]:
        (folder / "screenshot.png").write_bytes(report["screenshot"])
        attachments.append("screenshot.png")

    state = context.get("status_note") or ("not collected" if not context else "unknown")
    (folder / "report.md").write_text(_report_md(report, ident, when, env, str(state), attachments),
                                      encoding="utf-8")
    _append_inbox(reports, _inbox_line(ident, when, report["kind"], report["title"]))
    return {"id": ident, "path": f"reports/{ident}/"}


# --------------------------------------------------------------------------
# The inbox
# --------------------------------------------------------------------------
_LINE = re.compile(r"^- \[( |x)\] (\S+) \| ([^|]*) \| (\w+) \| (\w+) \| ([^|]*) \| (\S+)(?: \| (.*))?$")


def entries(reports_dir: Path | None = None) -> list[dict[str, Any]]:
    inbox = Path(reports_dir or REPORTS_DIR) / "INBOX.md"
    try:
        text = inbox.read_text(encoding="utf-8")
    except OSError:
        return []
    rows = []
    for line in text.splitlines():
        match = _LINE.match(line.strip())
        if match:
            rows.append({"id": match.group(2), "ts": match.group(3).strip(), "kind": match.group(4),
                         "status": match.group(5), "title": match.group(6).strip(),
                         "path": f"reports/{match.group(7).rsplit('/', 1)[0]}/",
                         "note": (match.group(8) or "").strip()})
    return rows


def find(ident: str, reports_dir: Path | None = None) -> dict[str, Any]:
    rows = entries(reports_dir)
    exact = [row for row in rows if row["id"] == ident]
    matches = exact or [row for row in rows if row["id"].startswith(ident)]
    if not matches:
        raise KeyError(f"no report {ident!r}")
    if len(matches) > 1:
        raise KeyError(f"{ident!r} matches {len(matches)} reports; give more of the id")
    return matches[0]


def close(ident: str, note: str = "", reports_dir: Path | None = None,
          now: float | None = None) -> dict[str, Any]:
    """Mark a report closed in INBOX.md and in its report.md."""
    reports = Path(reports_dir or REPORTS_DIR)
    row = find(ident, reports)
    when = datetime.fromtimestamp(time.time() if now is None else now).astimezone()
    closing = f"closed {when:%Y-%m-%d %H:%M}" + (f": {note.strip()}" if note.strip() else "")
    inbox = reports / "INBOX.md"
    with _WRITE:
        lines = inbox.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            match = _LINE.match(line.strip())
            if match and match.group(2) == row["id"]:
                filed = datetime.strptime(match.group(3).strip(), "%Y-%m-%d %H:%M:%S")
                lines[index] = _inbox_line(row["id"], filed, match.group(4), match.group(6).strip(),
                                           "closed", closing)
        temporary = inbox.with_suffix(".tmp")
        temporary.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
        os.replace(temporary, inbox)
    report = reports / row["id"] / "report.md"
    try:
        text = report.read_text(encoding="utf-8")
        report.write_text(re.sub(r"(?m)^- status: .*$", lambda _: f"- status: {closing}", text, count=1),
                          encoding="utf-8")
    except OSError:
        pass
    return {**row, "status": "closed", "note": closing}


# --------------------------------------------------------------------------
# HTTP
# --------------------------------------------------------------------------
def register(app: Any) -> None:
    """POST/GET /api/feedback on the station's Flask app."""
    from flask import jsonify, request

    @app.post("/api/feedback")
    def file_feedback():
        payload = request.get_json(silent=True)
        try:
            validate(payload)
        except ValueError as error:
            return jsonify(error=str(error)), 400
        context = station_context()
        try:
            result = submit(payload, context=context)
        except ValueError as error:
            return jsonify(error=str(error)), 400
        except OSError as error:
            return jsonify(error=f"Could not write the report: {error}"), 500
        print(f"[feedback] filed {result['id']}", flush=True)
        return jsonify(ok=True, **result), 201

    @app.get("/api/feedback")
    def list_feedback():
        return jsonify(reports=entries())


# --------------------------------------------------------------------------
# python -m radio.feedback
# --------------------------------------------------------------------------
def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    command = args.pop(0) if args else "list"
    try:
        if command == "list":
            rows = entries()
            if "--open" in args:
                rows = [row for row in rows if row["status"] == "open"]
            if not rows:
                print("No reports." if not (REPORTS_DIR / "INBOX.md").exists() else "Nothing to show.")
            for row in rows:
                print(f"{row['status']:<6}  {row['id']}  {row['kind']:<10}  {row['title']}")
            return 0
        if command == "show" and args:
            row = find(args[0])
            folder = config.ROOT / row["path"]
            print((folder / "report.md").read_text(encoding="utf-8"))
            for name in ("context.json", "logs.txt", "screenshot.png"):
                if (folder / name).exists():
                    print(f"  {folder / name}")
            return 0
        if command == "close" and args:
            row = close(args[0], " ".join(args[1:]))
            print(f"{row['id']}: {row['note']}")
            return 0
    except KeyError as error:
        print(str(error).strip("'\""), file=sys.stderr)
        return 1
    print("usage: python -m radio.feedback list [--open] | show <id> | close <id> [note]",
          file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
