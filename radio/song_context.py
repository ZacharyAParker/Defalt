"""Optional, cached song background with a bounded remote lookup."""
import hashlib
import json
import re
import time
from urllib.parse import quote

import httpx

from . import config, sourceio


def normalized(text):
    return " ".join(re.findall(r"\w+", str(text).casefold()))


def fetch(track):
    title, artist = track["title"], track["artist"]
    response = httpx.get("https://en.wikipedia.org/w/api.php", params={
        "action": "query", "format": "json", "formatversion": 2,
        "generator": "search", "gsrsearch": f'"{title}" "{artist}"',
        "gsrnamespace": 0, "gsrlimit": 3, "prop": "extracts",
        "exintro": 1, "explaintext": 1, "exchars": 1200, "exlimit": 3,
    }, timeout=5, headers={"User-Agent": "Defalt/0.3 (https://github.com/ZacharyAParker/Defalt)"})
    response.raise_for_status()
    for page in response.json().get("query", {}).get("pages", []):
        text = page.get("extract", "")
        # Require the song title in the page name and both credits in its introduction.
        contains = lambda haystack, needle: f" {normalized(needle)} " in f" {normalized(haystack)} "
        if (contains(page.get("title", ""), title) and contains(text, title)
                and contains(text, artist) and len(text.split()) >= 35
                and "may refer to" not in text[:150].casefold()):
            return {"kind": "song_context", "source": "Wikipedia",
                    "url": "https://en.wikipedia.org/wiki/" + quote(page["title"].replace(" ", "_")),
                    "title": title, "artist": artist, "text": text[:1400]}
    return None


def prepare(track):
    if not config.station.get("hosts.song_context", True) or not track:
        return None
    title, artist = track.get("title"), track.get("artist")
    if not title or not artist or artist == "Unknown Artist":
        return None
    if track.get("metadata_sources", {}).get("artist") in {
            "director_inferred", "title_parse", "channel_parse", "uploader_fallback"}:
        return None
    key = hashlib.sha256(f"{normalized(artist)}\0{normalized(title)}".encode()).hexdigest()[:24]
    path = config.CACHE_DIR / "source-info" / f"song-context-{key}.json"
    try:
        cached = json.loads(path.read_text(encoding="utf-8"))
        if time.time() - cached["at"] < (604800 if cached["value"] else 21600):
            return cached["value"]
    except (OSError, ValueError, KeyError, TypeError):
        pass
    try:
        value = sourceio._run("song_context", {"title": title[:160], "artist": artist[:160]}, 7)
    except Exception:
        value = None
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"at": time.time(), "value": value}), encoding="utf-8")
    except OSError:
        pass
    return value
