"""Config loading with hot reload.

Every YAML file under config/ is watched by mtime. Ask for a value and you get
whatever is on disk right now, so editing station.yaml mid-broadcast takes
effect on the next segment the director builds.
"""
from __future__ import annotations

import os
import copy
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

import yaml
from dotenv import load_dotenv

ROOT = Path(__file__).resolve().parent.parent
CONFIG_DIR = ROOT / "config"

load_dotenv(ROOT / ".env")


def env(key: str, default: str = "") -> str:
    return os.getenv(key, default) or default


def env_bool(key: str, default: bool = False) -> bool:
    raw = os.getenv(key)
    if raw is None:
        return default
    return raw.strip().lower() in {"1", "true", "yes", "on"}


def env_list(key: str, default: str = "") -> list[str]:
    return [p.strip() for p in env(key, default).split(",") if p.strip()]


def _project_path(value: str, fallback: str) -> Path:
    raw = value or fallback
    path = Path(raw)
    return path if path.is_absolute() else ROOT / path


VAULT_DIR = _project_path(env("VAULT_DIR"), "vault")
CACHE_DIR = _project_path(env("CACHE_DIR"), "cache")
DEBUG = env_bool("RADIO_DEBUG")
FFMPEG = env("FFMPEG_BIN", "ffmpeg")


def _ffprobe() -> str:
    """ffprobe lives beside a configured ffmpeg far more often than on PATH."""
    explicit = env("FFPROBE_BIN")
    if explicit:
        return explicit
    ffmpeg = Path(FFMPEG)
    if ffmpeg.parent != Path("."):
        return str(ffmpeg.with_name("ffprobe" + ffmpeg.suffix))
    return "ffprobe"


FFPROBE = _ffprobe()

# How often a config file is looked at on disk. Every request reads settings
# dozens of times; a stat per read was most of what a status poll cost.
STAT_INTERVAL = 1.0


class ConfigFile:
    """A single YAML file that reloads itself when it changes on disk."""

    def __init__(self, path: Path):
        self.path = path
        self._lock = threading.Lock()
        self._mtime: float | None = None
        self._checked = 0.0
        self._data: dict[str, Any] = {}
        # Bumped on every reload, so anything derived from it knows to redo.
        self.version = 0

    def data(self) -> dict[str, Any]:
        with self._lock:
            now = time.monotonic()
            # A cleared _mtime means "look now" (set_many does that after a
            # write); otherwise the disk is asked at most once a second.
            if self._mtime is not None and now - self._checked < STAT_INTERVAL:
                return self._data
            self._checked = now
            try:
                mtime = self.path.stat().st_mtime
            except OSError:
                # Absent (an overrides file usually is). Remember that we
                # looked, so the next read inside the interval does not.
                if self._mtime is None:
                    self._mtime = -1.0
                return self._data
            if mtime != self._mtime:
                try:
                    loaded = yaml.safe_load(self.path.read_text(encoding="utf-8"))
                    self._data = loaded if isinstance(loaded, dict) else {}
                    self._mtime = mtime
                    self.version += 1
                except (OSError, yaml.YAMLError):
                    # Keep serving the last good copy. A typo mid-edit should
                    # never take the station off the air.
                    pass
            return self._data

    def get(self, dotted: str, default: Any = None) -> Any:
        """Fetch a nested value: get("ducking.target_gain", 0.2)."""
        node: Any = self.data()
        for part in dotted.split("."):
            if not isinstance(node, dict) or part not in node:
                return default
            node = node[part]
        return node


class OverridableConfig:
    """A base YAML file plus a machine-written override layer.

    The UI writes to overrides.yaml so that station.yaml keeps its comments and
    stays yours. An override always wins; delete overrides.yaml to reset.
    """

    def __init__(self, base: Path, override: Path):
        self.base = ConfigFile(base)
        self.override = ConfigFile(override)
        self._write_lock = threading.Lock()
        self._merged: tuple[Any, Any, dict[str, Any]] | None = None

    def version(self) -> tuple[int, int]:
        """Changes whenever either layer is reloaded from disk."""
        self.base.data()
        self.override.data()
        return self.base.version, self.override.version

    def data(self) -> dict[str, Any]:
        base, top = self.base.data(), self.override.data()
        merged = self._merged
        if merged is None or merged[0] is not base or merged[1] is not top:
            merged = (base, top, _deep_merge(base, top))
            self._merged = merged
        return merged[2]

    def get(self, dotted: str, default: Any = None) -> Any:
        sentinel = object()
        value = self.override.get(dotted, sentinel)
        if value is not sentinel:
            # A saved child setting must not hide its unchanged siblings when
            # a caller reads the whole group (for example hosts.humour).
            if isinstance(value, dict):
                base = self.base.get(dotted, sentinel)
                if isinstance(base, dict):
                    return _deep_merge(base, value)
            return value
        return self.base.get(dotted, default)

    def set(self, dotted: str, value: Any) -> None:
        self.set_many({dotted: value})

    def set_many(self, values: dict[str, Any]) -> None:
        """Persist one complete settings change without exposing partial YAML."""
        with self._write_lock:
            data = copy.deepcopy(self.override.data())
            for dotted, value in values.items():
                node = data
                parts = dotted.split(".")
                for part in parts[:-1]:
                    child = node.get(part)
                    if not isinstance(child, dict):
                        child = {}
                        node[part] = child
                    node = child
                node[parts[-1]] = value
            self.override.path.parent.mkdir(parents=True, exist_ok=True)
            content = "# Written by the station UI. station.yaml is the commented\n" \
                "# master; anything here overrides it. Delete to reset.\n"
            content += yaml.safe_dump(data, sort_keys=False, allow_unicode=True)
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=self.override.path.parent,
                                                 prefix="settings-", suffix=".yaml", delete=False) as output:
                    temporary = Path(output.name)
                    output.write(content)
                os.replace(temporary, self.override.path)
            finally:
                if temporary is not None:
                    temporary.unlink(missing_ok=True)
            self.override._mtime = None  # force a reload on next read


def _deep_merge(base: dict[str, Any], top: dict[str, Any]) -> dict[str, Any]:
    merged = dict(base)
    for key, value in (top or {}).items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


station = OverridableConfig(CONFIG_DIR / "station.yaml",
                            CONFIG_DIR / "overrides.yaml")
news = ConfigFile(CONFIG_DIR / "news.yaml")
games = ConfigFile(CONFIG_DIR / "games.yaml")


_PERSONAS_LOCK = threading.Lock()
_personas: dict[str, Any] = {"checked": 0.0, "signature": None, "directory": None, "value": {}}


def personas() -> dict[str, dict[str, Any]]:
    """Load every persona file. Filename is irrelevant; the `id` field wins.

    Parsed once and kept until a file in the folder changes. Callers get
    their own copy, so nothing one of them does leaks into the next.
    """
    directory = CONFIG_DIR / "personas"
    with _PERSONAS_LOCK:
        now = time.monotonic()
        cache = _personas
        if (cache["signature"] is None or cache["directory"] != directory
                or now - cache["checked"] >= STAT_INTERVAL):
            cache["checked"] = now
            signature = []
            try:
                for path in directory.glob("*.yaml"):
                    stat = path.stat()
                    signature.append((path.name, stat.st_mtime_ns, stat.st_size))
            except OSError:
                pass
            signature = tuple(sorted(signature))
            if signature != cache["signature"] or cache["directory"] != directory:
                cache.update(signature=signature, directory=directory,
                             value=_load_personas(directory))
        return copy.deepcopy(cache["value"])


def _load_personas(directory: Path) -> dict[str, dict[str, Any]]:
    found: dict[str, dict[str, Any]] = {}
    if not directory.is_dir():
        return found
    for path in sorted(directory.glob("*.yaml")):
        try:
            data = yaml.safe_load(path.read_text(encoding="utf-8"))
        except (OSError, yaml.YAMLError):
            continue
        if isinstance(data, dict) and data.get("id"):
            found[str(data["id"])] = data
    return found


def ensure_dirs() -> None:
    for path in (VAULT_DIR, CACHE_DIR, CACHE_DIR / "audio", CACHE_DIR / "voice"):
        path.mkdir(parents=True, exist_ok=True)
