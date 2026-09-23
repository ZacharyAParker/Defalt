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
    return _tracks(query)


def _tracks(query, limit=8, offset=0):
    global _token
    query = str(query or "").strip()
    if not 2 <= len(query) <= 240 or re.search(r"https?://|youtube\.com/|youtu\.be/", query, re.I):
        return []
    if not available():
        raise ValueError("Spotify suggestions need the same Spotify credentials used by Console.")
    limit = max(1, min(50, int(limit)))
    offset = max(0, min(950, int(offset)))
    cache_key = query if (limit, offset) == (8, 0) else (query, limit, offset)
    try:
        with _lock:
            cached = _cache.get(cache_key)
            if cached and cached[0] > time.monotonic():
                return cached[1]
            if not _token or _token[1] <= time.monotonic():
                body = _checked(httpx.post("https://accounts.spotify.com/api/token",
                    auth=(config.env("SPOTIFY_CLIENT_ID"), config.env("SPOTIFY_CLIENT_SECRET")),
                    data={"grant_type": "client_credentials"}, timeout=8))
                _token = (body["access_token"], time.monotonic() + max(1, int(body.get("expires_in", 3600)) - 60))
            token = _token[0]
        params = {"q": query, "type": "track", "limit": limit}
        if offset:
            params["offset"] = offset
        response = httpx.get("https://api.spotify.com/v1/search", params=params,
                            headers={"Authorization": "Bearer " + token}, timeout=8)
        if response.status_code == 401:
            with _lock:
                _token = None
        body = _checked(response)
        results = []
        for item in body.get("tracks", {}).get("items", [])[:limit]:
            if not isinstance(item, dict):
                continue
            artist = ", ".join(a["name"] for a in item.get("artists", [])
                               if isinstance(a, dict) and isinstance(a.get("name"), str))
            title = item.get("name")
            if not artist or not isinstance(title, str) or not title:
                continue
            album = item.get("album") or {}
            entry = {"artist": artist, "title": title, "album": album.get("name"),
                     "artwork": next((image.get("url") for image in album.get("images", [])
                                      if isinstance(image, dict) and str(image.get("url", "")).startswith("https://i.scdn.co/")), None),
                     "year": str(album.get("release_date") or "")[:4],
                     "duration_ms": item.get("duration_ms") or 0}
            if (limit, offset) != (8, 0):
                entry["explicit"] = bool(item.get("explicit"))
                entry["popularity"] = item.get("popularity") if type(item.get("popularity")) is int else None
            results.append(entry)
        with _lock:
            if len(_cache) >= 128:
                _cache.pop(next(iter(_cache)))
            _cache[cache_key] = (time.monotonic() + 300, results)
        return results
    except (httpx.HTTPError, KeyError, TypeError) as error:
        raise ValueError("Spotify search is unavailable. You can still type a request or paste a YouTube link.") from error


def _quoted(value):
    value = re.sub(r'\s+', " ", re.sub(r'["\\]', " ", str(value or ""))).strip()
    return f'"{value}"' if " " in value else value


def catalog(*, years=None, genre=None, artist=None, title=None, limit=20, offset=0):
    """Filtered catalog search: real recordings with year, album and length.

    Spotify's field filters do the narrowing (year:2010-2015, genre:"r&b",
    artist:, track:), so no free text can match a title by accident. Each
    result's `year` is an int or None; the caller still checks it.
    """
    parts = []
    if title:
        parts.append(f"track:{_quoted(title)}")
    if artist:
        parts.append(f"artist:{_quoted(artist)}")
    if genre:
        parts.append(f"genre:{_quoted(genre)}")
    if years:
        first, last = years
        parts.append(f"year:{first}" if first == last else f"year:{first}-{last}")
    if not parts:
        return []
    results = []
    for item in _tracks(" ".join(parts), limit=limit, offset=offset):
        year = item.get("year")
        results.append({**item, "year": int(year) if str(year or "").isdigit() else None})
    return results


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
