"""Exact-video requests and evidence-labelled metadata, off the playback thread."""
from __future__ import annotations

import json
import math
import re
import time
import unicodedata
from urllib.parse import parse_qs, urlparse

from . import db, sourceio

_LINK = re.compile(r"https?://[^\s<>]+|(?<![\w.@-])(?:www\.|music\.|m\.)?(?:youtube\.com|youtu\.be)/[^\s<>]+", re.I)
_ID = re.compile(r"[A-Za-z0-9_-]{11}")
_HOSTS = {"youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com",
          "youtube-nocookie.com", "www.youtube-nocookie.com"}
_NOISE = re.compile(r"\s*[\[(](?:(?:official\s+)?(?:music\s+)?(?:video|audio|lyric video|lyrics|visualizer)|HD|HQ|4K)[\])]", re.I)


def parse(text: str) -> dict | None:
    """Return a canonical single video; never send arbitrary pasted URLs to yt-dlp."""
    found = _LINK.findall(text)
    if not found:
        return None
    if len(found) != 1:
        raise ValueError("Paste one YouTube video at a time.")
    raw = found[0].rstrip(".,!?;:)'\"]}")
    try:
        url = urlparse(raw if "://" in raw else "https://" + raw)
        if url.username or url.password or url.port not in (None, 443, 80):
            raise ValueError
        host = (url.hostname or "").lower()
        parts = url.path.strip("/").split("/")
        if host in {"youtu.be", "www.youtu.be"} and len(parts) == 1:
            ident = parts[0]
        elif host in _HOSTS:
            if url.path.rstrip("/") == "/watch":
                ids = parse_qs(url.query).get("v", [])
                ident = ids[0] if len(ids) == 1 else ""
            elif len(parts) == 2 and parts[0] in {"shorts", "embed", "live", "v"}:
                ident = parts[1]
            else:
                raise ValueError
        else:
            raise ValueError
        if not _ID.fullmatch(ident):
            raise ValueError
    except ValueError:
        raise ValueError("That link is not a YouTube video. Paste a video link, not a playlist or channel.") from None
    return {"video_id": ident, "url": "https://www.youtube.com/watch?v=" + ident}


def key_for(video_id):
    if not _ID.fullmatch(video_id):
        raise ValueError("Invalid YouTube video ID")
    return "youtube:" + video_id


def _text(value, limit=160):
    if not isinstance(value, str):
        return ""
    return " ".join("".join(c for c in value if not unicodedata.category(c).startswith("C")
                            or c in "\n\t").split())[:limit]


def _title(value):
    value = _text(value)
    while True:
        shorter = _NOISE.sub("", value).strip()
        if not shorter or shorter == value:
            return value
        value = shorter


def metadata(info, video_id, *, infer=True):
    """Music credits win; title/channel parsing and model guesses stay labelled."""
    fields = {}

    def set_field(name, value, source, confidence=1.0):
        value = _text(value, 120 if name == "artist" else 160)
        if value:
            fields[name] = {"value": value, "source": source, "confidence": confidence}

    set_field("title", info.get("track"), "youtube_music")
    artist = info.get("artist")
    if not artist and isinstance(info.get("artists"), list):
        artist = ", ".join(a for a in info["artists"] if isinstance(a, str))
    set_field("artist", artist, "youtube_music")
    for field in ("album", "genre"):
        set_field(field, info.get(field), "youtube_music")
    # Upload dates are not release dates. Never infer a release year from them.
    year = str(info.get("release_year") or info.get("release_date") or "")
    if re.fullmatch(r"(?:19|20)\d{2}(?:\d{4})?", year):
        set_field("year", year[:4], "youtube_music")

    upload_title = _text(info.get("title"), 300)
    channel = _text(info.get("channel") or info.get("uploader"), 120)
    pieces = re.split(r"\s+[-\u2013\u2014]\s+", _title(upload_title), maxsplit=1)
    if len(pieces) == 2 and all(pieces):
        if "artist" not in fields:
            set_field("artist", pieces[0], "title_parse", .8)
        if "title" not in fields:
            set_field("title", pieces[1], "title_parse", .8)
    if "artist" not in fields and channel.lower().endswith(" - topic"):
        set_field("artist", channel[:-8], "channel_parse", .9)

    evidence = {"title": upload_title, "channel": channel,
                "description": _text(info.get("description"), 3500),
                "tags": [_text(v, 60) for v in (info.get("tags") or [])[:20]
                         if isinstance(v, str)], "known_fields": fields}
    missing = [name for name in ("title", "artist", "genre") if name not in fields]
    if missing and infer:
        try:
            guessed = sourceio.guess_metadata({"evidence": evidence, "missing": missing})
        except sourceio.SourceError:
            guessed = None
        if isinstance(guessed, dict):
            for name in missing:
                guess = guessed.get(name)
                if not isinstance(guess, dict):
                    continue
                try:
                    confidence = float(guess.get("confidence", 0))
                except (ValueError, TypeError):
                    continue
                excerpt = _text(guess.get("evidence"), 300)
                evidence_text = " ".join([upload_title, channel, evidence["description"], *evidence["tags"]])
                if (not math.isfinite(confidence) or not .65 <= confidence <= 1
                        or len(excerpt) < 3 or excerpt.casefold() not in evidence_text.casefold()):
                    continue
                set_field(name, guess.get("value"), "director_inferred", confidence)
    if "title" not in fields:
        set_field("title", _title(upload_title) or f"YouTube video {video_id}",
                  "video_title" if upload_title else "fallback", .5 if upload_title else 0)
    if "artist" not in fields:
        set_field("artist", channel or "Unknown Artist", "uploader_fallback" if channel else "fallback", 0)
    try:
        duration = float(info.get("duration") or 0)
        expected_ms = int(duration * 1000) if math.isfinite(duration) and 0 < duration < 86400 else 0
    except (TypeError, ValueError):
        expected_ms = 0
    return {"video_id": video_id, "source_url": "https://www.youtube.com/watch?v=" + video_id,
            "title": fields["title"]["value"], "artist": fields["artist"]["value"],
            "album": fields.get("album", {}).get("value", ""),
            "genre": fields.get("genre", {}).get("value", ""),
            "year": int(fields["year"]["value"]) if "year" in fields else None,
            "expected_ms": expected_ms,
            "source_metadata": json.dumps({"version": 1, "fields": fields,
                "video_title": upload_title, "channel": channel, "checked_at": time.time()}, ensure_ascii=False)}


def describe(video_id):
    key_for(video_id)
    try:
        info = sourceio.describe(video_id)
    except sourceio.SourceError:
        # oEmbed often still exposes public title/author when richer metadata
        # fails. It does not grant access to unavailable audio.
        try:
            info = sourceio.oembed(video_id)
        except sourceio.SourceError:
            info = {}
    info = info if isinstance(info, dict) else {}
    if info.get("live_status") in {"is_live", "is_upcoming", "post_live"}:
        raise sourceio.SourceError("This video is live or not ready yet. Request a finished recording.")
    if info.get("id") and info["id"] != video_id:
        raise sourceio.SourceError("YouTube returned a different video. Please check the link.")
    return metadata(info, video_id)


def save(key, values):
    names = ("title", "artist", "album", "genre", "year", "expected_ms", "video_id", "source_url", "source_metadata")
    db.write("UPDATE tracks SET " + ", ".join(name + "=?" for name in names) + " WHERE key=?",
             (*[values.get(name) for name in names], key))


def hydrate(track):
    if not track.get("source_url") or track.get("source_metadata"):
        return track
    link = parse(track["source_url"])
    if not link:
        return track
    values = describe(link["video_id"])
    save(track["key"], values)
    return {**track, **values}


def reserve(link):
    """Cheap submission path: each video has its own identity before metadata exists."""
    key = key_for(link["video_id"])
    db.write("INSERT INTO tracks(key,title,artist,source,video_id,source_url,added_at) "
             "VALUES(?,?,?,'request',?,?,?) ON CONFLICT(key) DO UPDATE SET blocked=0",
             (key, f"YouTube video {link['video_id']}", "Unknown Artist",
              link["video_id"], link["url"], time.time()))
    return key
