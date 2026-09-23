"""Obsidian vault writer -- the station's memory, in a form you can read.

The SQLite database is the source of truth. This mirrors it into linked
markdown so you can open the vault, see what the station thinks it knows about
you, and correct it by hand. Anything you write under the `## Notes` heading of
a note is preserved across rewrites.
"""
from __future__ import annotations

import hashlib
import json
import re
import time
from datetime import datetime
from pathlib import Path
from typing import Any

from . import config, db, taste

NOTES_MARKER = "## Notes"
_UNSAFE = re.compile(r'[<>:"/\\|?*\x00-\x1f]')
# Device names Windows refuses as a file name, with or without an extension.
_RESERVED = {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$",
             *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}
NAME_LIMIT = 80


def _root() -> Path:
    return config.VAULT_DIR


def _safe(name: str, limit: int = NAME_LIMIT) -> str:
    """A file-name-safe note name that is stable for the same input.

    Long names are shortened with a short hash so two long titles sharing a
    prefix do not collide and the full path stays well under Windows' limit.
    """
    original = (name or "unknown").strip()
    cleaned = _UNSAFE.sub("-", original).rstrip(". ") or "unknown"
    if len(cleaned) > limit:
        digest = hashlib.sha1(original.encode("utf-8")).hexdigest()[:8]
        cleaned = cleaned[:limit - 9].rstrip(". ") + "~" + digest
    if cleaned.split(".", 1)[0].strip().upper() in _RESERVED:
        cleaned = "_" + cleaned
    return cleaned


def _yaml(value: object) -> str:
    """A YAML-safe scalar. JSON strings are valid YAML double-quoted scalars."""
    return json.dumps(str(value), ensure_ascii=False)


def artist_note(artist: str) -> str:
    """The note name for an artist. Filenames and links both go through here."""
    return _safe(db.primary_artist(artist))


def track_note(artist: str, title: str) -> str:
    """The note name for a track. Uses the full credit, as the file does."""
    return f"{_safe(artist)} - {_safe(title)}"


def _preserve_notes(path: Path) -> str:
    """Keep whatever the human wrote under the Notes heading."""
    if not path.exists():
        return ""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return ""
    if NOTES_MARKER not in text:
        return ""
    return text.split(NOTES_MARKER, 1)[1].strip()


def _write(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    kept = _preserve_notes(path)
    content = body.rstrip() + f"\n\n{NOTES_MARKER}\n\n" + (kept or "")
    try:
        path.write_text(content.rstrip() + "\n", encoding="utf-8")
    except OSError as error:
        if config.DEBUG:
            print("[vault] write failed", path, error, flush=True)


def ensure_scaffold() -> None:
    root = _root()
    for folder in ("Sessions", "Artists", "Tracks", "Breaks"):
        (root / folder).mkdir(parents=True, exist_ok=True)

    readme = root / "README.md"
    if not readme.exists():
        identity = config.station.get("identity", {}) or {}
        _write(readme, f"""# {identity.get('name', 'Station')} — memory

This vault is written by the station. Open the folder in Obsidian.

- **Taste Profile** — what it currently believes you like, and why.
- **Sessions/** — one note per listening session: what aired, what you skipped.
- **Artists/** and **Tracks/** — a note per artist and per song, with running
  affinity scores and play counts.
- **Breaks/** — transcripts of what the hosts actually said.

Everything here is regenerated from the database, with one exception: anything
you type under a `{NOTES_MARKER}` heading is preserved. Use that to correct the
station. Write what you actually think and it will not be overwritten.""")


# --------------------------------------------------------------------------
# Notes
# --------------------------------------------------------------------------
def write_taste_profile() -> None:
    profile = taste.summary(limit=25)
    lines = [
        "---", "type: taste-profile",
        f"updated: {datetime.now().isoformat(timespec='seconds')}", "---", "",
        "# Taste Profile", "",
        f"- Library: **{profile['library_size']}** tracks",
        f"- Played: **{profile['total_plays']}**  ·  Skipped: **{profile['total_skips']}**",
        "",
        "## Top artists", "",
    ]
    for entry in profile["top_artists"]:
        lines.append(f"- [[{artist_note(entry['artist'])}]] — {entry['score']:+.2f}")
    lines += ["", "## Top tracks", ""]
    for entry in profile["top_tracks"]:
        lines.append(f"- [[{track_note(entry['artist'], entry['title'])}]] "
                     f"— {entry['score']:+.2f}")

    lines += ["", "## By hour", "",
              "When you actually listen to whom.", ""]
    rows = db.query(
        "SELECT hour, artist, weight FROM daypart ORDER BY hour, weight DESC")
    by_hour: dict[int, list[str]] = {}
    for row in rows:
        by_hour.setdefault(row["hour"], []).append(row["artist"])
    for hour in sorted(by_hour):
        top = ", ".join(f"[[{artist_note(a)}]]" for a in by_hour[hour][:4])
        lines.append(f"- **{hour:02d}:00** — {top}")

    _write(_root() / "Taste Profile.md", "\n".join(lines))


def write_track_note(key: str) -> None:
    row = db.one("SELECT * FROM tracks WHERE key=?", (key,))
    if not row:
        return
    artist, title = row["artist"], row["title"]
    score = taste.affinity("track", key)
    last = (datetime.fromtimestamp(row["last_played"]).isoformat(timespec="minutes")
            if row["last_played"] else "never")
    intro = db.intro_of(row, 12.0)
    override = "  (your override)" if db.field(row, "intro_override") else ""

    body = f"""---
type: track
artist: {_yaml(artist)}
title: {_yaml(title)}
affinity: {score:.3f}
plays: {row['play_count']}
skips: {row['skip_count']}
---

# {title}

**Artist:** [[{artist_note(artist)}]]
**Affinity:** {score:+.2f}
**Plays:** {row['play_count']}  ·  **Skips:** {row['skip_count']}
**Last played:** {last}
**Intro length:** {intro:.1f}s{override} (how long the hosts can talk over it)
**Duration:** {(row['duration'] or 0):.0f}s

> If the hosts keep talking into the vocal on this one, set an override:
> `curl -X POST localhost:8080/api/intro -H "Content-Type: application/json" -d '{{"key":"{key}","seconds":8}}'`"""
    _write(_root() / "Tracks" / f"{track_note(artist, title)}.md", body)


def _like(text: str) -> str:
    return re.sub(r"([\\%_])", r"\\\1", text)


def write_artist_note(artist: str) -> None:
    from .compatibility import artists as credits
    primary = db.primary_artist(artist)
    key = db.norm(primary)
    score = taste.affinity("artist", key)
    candidates = db.query(
        "SELECT artist, title, play_count, skip_count FROM tracks "
        "WHERE artist LIKE ? ESCAPE '\\' ORDER BY play_count DESC LIMIT 400",
        (f"%{_like(primary)}%",))
    # LIKE is only a prefilter: "Ye" must not collect every "Yeat" record.
    tracks = [row for row in candidates if key in credits(row["artist"])][:40]

    lines = [
        "---", "type: artist", f"name: {_yaml(artist)}",
        f"affinity: {score:.3f}", "---", "",
        f"# {db.primary_artist(artist)}", "",
        f"**Affinity:** {score:+.2f}", "", "## Tracks in rotation", "",
    ]
    for row in tracks:
        # Link with the row's own credit -- that is what names its file.
        lines.append(
            f"- [[{track_note(row['artist'], row['title'])}|{row['title']}]] "
            f"— {row['play_count']} plays, {row['skip_count']} skips")
    _write(_root() / "Artists" / f"{artist_note(artist)}.md", "\n".join(lines))


def write_break(kind: str, lines: list[dict[str, str]],
                context: str = "") -> None:
    """Transcript of one talk break, so you can see what they actually said."""
    if not lines:
        return
    stamp = datetime.now()
    body = [
        "---", "type: break", f"kind: {_yaml(kind)}",
        f"aired: {stamp.isoformat(timespec='seconds')}", "---", "",
        f"# {kind.replace('_', ' ').title()} — {stamp.strftime('%H:%M')}", "",
    ]
    if context:
        body += [f"> {context}", ""]
    for line in lines:
        body.append(f"**{line.get('host', '?').upper()}:** {line.get('text', '')}")
        body.append("")
    path = (_root() / "Breaks" / stamp.strftime("%Y-%m-%d") /
            f"{stamp.strftime('%H%M%S')}-{_safe(kind, 40)}.md")
    _write(path, "\n".join(body))


def write_session_note() -> None:
    """Called on shutdown. Summarises everything that aired this session."""
    ensure_scaffold()
    write_taste_profile()

    since = time.time() - 12 * 3600
    events = db.query(
        "SELECT e.ts, e.kind, e.position, t.title, t.artist FROM events e "
        "LEFT JOIN tracks t ON t.key = e.track_key "
        "WHERE e.ts > ? ORDER BY e.ts", (since,))
    if not events:
        return

    played = [e for e in events if e["kind"] in ("played", "completed")]
    skipped = [e for e in events if e["kind"].startswith("skipped")]
    aired = db.query("SELECT kind, ts FROM aired WHERE ts > ? ORDER BY ts", (since,))

    stamp = datetime.now()
    lines = [
        "---", "type: session",
        f"date: {stamp.strftime('%Y-%m-%d')}",
        f"ended: {stamp.isoformat(timespec='seconds')}", "---", "",
        f"# Session — {stamp.strftime('%A %d %B %Y, %H:%M')}", "",
        f"**{len(played)}** songs played · **{len(skipped)}** skipped · "
        f"**{len(aired)}** talk breaks", "",
    ]

    if skipped:
        lines += ["## Skipped", "",
                  "The station is treating these as rejections.", ""]
        for event in skipped:
            if event["title"]:
                lines.append(
                f"- [[{track_note(event['artist'], event['title'])}"
                f"|{event['title']}]] at {(event['position'] or 0):.0f}s")
        lines.append("")

    if played:
        lines += ["## Played", ""]
        seen: set[str] = set()
        for event in played:
            if not event["title"] or event["title"] in seen:
                continue
            seen.add(event["title"])
            lines.append(
                f"- [[{track_note(event['artist'], event['title'])}"
                f"|{event['title']}]] — [[{artist_note(event['artist'])}]]")
        lines.append("")

    if aired:
        counts: dict[str, int] = {}
        for row in aired:
            counts[row["kind"]] = counts.get(row["kind"], 0) + 1
        lines += ["## Segments aired", ""]
        for kind, count in sorted(counts.items(), key=lambda kv: -kv[1]):
            lines.append(f"- {kind.replace('_', ' ')} — {count}")
        lines.append("")

    _write(_root() / "Sessions" / f"{stamp.strftime('%Y-%m-%d %H%M')}.md",
           "\n".join(lines))

    # Refresh notes for everything touched this session.
    touched = {(e["artist"], e["title"]) for e in events if e["title"]}
    for artist, title in list(touched)[:120]:
        write_track_note(db.track_key(artist, title))
    for artist in {a for a, _ in touched}:
        write_artist_note(artist)


def rebuild_all() -> dict[str, int]:
    """Regenerate every note from the database.

    Covers the whole library, not just what has played. The vault is the
    station's memory, and a memory full of unresolved links is not much of one
    -- every artist the taste profile mentions should have a note behind it.
    """
    ensure_scaffold()
    tracks = db.query("SELECT key, artist FROM tracks")
    for row in tracks:
        write_track_note(row["key"])
    artists = {db.primary_artist(r["artist"]) for r in tracks}
    for artist in artists:
        write_artist_note(artist)
    write_taste_profile()   # last, so its links point at notes that now exist
    return {"tracks": len(tracks), "artists": len(artists)}
