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


class ConfigFile:
    """A single YAML file that reloads itself when it changes on disk."""

    def __init__(self, path: Path):
        self.path = path
        self._lock = threading.Lock()
        self._mtime: float | None = None
        self._data: dict[str, Any] = {}

    def data(self) -> dict[str, Any]:
        with self._lock:
            try:
                mtime = self.path.stat().st_mtime
            except OSError:
                return self._data
            if mtime != self._mtime:
                try:
                    loaded = yaml.safe_load(self.path.read_text(encoding="utf-8"))
                    self._data = loaded if isinstance(loaded, dict) else {}
                    self._mtime = mtime
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

    def data(self) -> dict[str, Any]:
        return _deep_merge(self.base.data(), self.override.data())

    def get(self, dotted: str, default: Any = None) -> Any:
        sentinel = object()
        value = self.override.get(dotted, sentinel)
        if value is not sentinel:
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


def personas() -> dict[str, dict[str, Any]]:
    """Load every persona file. Filename is irrelevant; the `id` field wins."""
    found: dict[str, dict[str, Any]] = {}
    directory = CONFIG_DIR / "personas"
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
