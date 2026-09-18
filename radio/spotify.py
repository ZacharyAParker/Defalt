"""Public Spotify catalog search for the browser, using Console's credentials."""
import json
import re
import threading
import time

import httpx

from . import config

_lock = threading.Lock()
_token = None
_cache = {}


def available():
    return bool(config.env("SPOTIFY_CLIENT_ID") and config.env("SPOTIFY_CLIENT_SECRET"))


def _checked(response):
    if response.status_code == 429:
        raise ValueError("Spotify is rate limiting searches. Wait a moment and try again.")
    if response.status_code in (401, 403):
        raise ValueError("Spotify refused catalog access. Check the configured Spotify credentials.")
    response.raise_for_status()
    return response.json()


def search(query):
    global _token
    query = str(query or "").strip()
    if not 2 <= len(query) <= 240 or re.search(r"https?://|youtube\.com/|youtu\.be/", query, re.I):
        return []
    if not available():
        raise ValueError("Spotify suggestions need the same Spotify credentials used by Console.")
    try:
        with _lock:
            cached = _cache.get(query)
            if cached and cached[0] > time.monotonic():
                return cached[1]
            if not _token or _token[1] <= time.monotonic():
                body = _checked(httpx.post("https://accounts.spotify.com/api/token",
                    auth=(config.env("SPOTIFY_CLIENT_ID"), config.env("SPOTIFY_CLIENT_SECRET")),
                    data={"grant_type": "client_credentials"}, timeout=8))
                _token = (body["access_token"], time.monotonic() + max(1, int(body.get("expires_in", 3600)) - 60))
            token = _token[0]
        response = httpx.get("https://api.spotify.com/v1/search", params={"q": query, "type": "track", "limit": 8},
                            headers={"Authorization": "Bearer " + token}, timeout=8)
        if response.status_code == 401:
            with _lock:
                _token = None
        body = _checked(response)
        results = []
        for item in body.get("tracks", {}).get("items", [])[:8]:
            if not isinstance(item, dict):
                continue
            artist = ", ".join(a["name"] for a in item.get("artists", [])
                               if isinstance(a, dict) and isinstance(a.get("name"), str))
            title = item.get("name")
            if not artist or not isinstance(title, str) or not title:
                continue
            album = item.get("album") or {}
            results.append({"artist": artist, "title": title, "album": album.get("name"),
                            "year": str(album.get("release_date") or "")[:4],
                            "duration_ms": item.get("duration_ms") or 0})
        with _lock:
            if len(_cache) >= 128:
                _cache.pop(next(iter(_cache)))
            _cache[query] = (time.monotonic() + 300, results)
        return results
    except (httpx.HTTPError, KeyError, TypeError) as error:
        raise ValueError("Spotify search is unavailable. You can still type a request or paste a YouTube link.") from error


def selected(query, payload):
    """Validate a chosen record; edits must not inherit a stale duration hint."""
    if payload is None:
        return None
    if not isinstance(payload, dict):
        raise ValueError("Choose a Spotify result again.")
    artist, title = payload.get("artist"), payload.get("title")
    if (not isinstance(artist, str) or not artist.strip() or len(artist) > 120
            or not isinstance(title, str) or not title.strip() or len(title) > 160
            or query.strip() != f"{artist} - {title}"):
        raise ValueError("That Spotify result no longer matches the request. Choose it again.")
    duration = payload.get("duration_ms", 0)
    if type(duration) is not int or not 0 <= duration <= 86400000:
        raise ValueError("Invalid duration in Spotify result.")
    album = payload.get("album") or ""
    if not isinstance(album, str) or len(album) > 250:
        raise ValueError("Invalid album in Spotify result.")
    year = str(payload.get("year") or "")
    if year and not re.fullmatch(r"(?:19|20)\d{2}", year):
        raise ValueError("Invalid year in Spotify result.")
    fields = {name: {"value": value, "source": "spotify_catalog", "confidence": 1}
              for name, value in (("artist", artist), ("title", title), ("album", album), ("year", year)) if value}
    return {"artist": artist, "title": title, "album": album, "year": int(year) if year else None,
            "expected_ms": duration, "source_metadata": json.dumps({"version": 1, "fields": fields})}
