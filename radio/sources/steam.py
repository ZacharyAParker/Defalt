"""Steam: what you play, what got patched, what you want next.

Drives two segment types -- real patch notes for games you actually play, and
the (entirely unsponsored, entirely fake) ad reads for things on your wishlist.

Needs STEAM_API_KEY and STEAM_ID in .env, and a public profile with public
game details. Without them every function here degrades to the manual
watchlist in config/games.yaml.
"""
from __future__ import annotations

import html
import re
import time
from typing import Any

import httpx

from .. import config, db

API = "https://api.steampowered.com"
STORE = "https://store.steampowered.com"
_CACHE: dict[str, tuple[float, Any]] = {}


def _cached(key: str, ttl: float, producer: Any) -> Any:
    hit = _CACHE.get(key)
    if hit and time.time() - hit[0] < ttl:
        return hit[1]
    value = producer()
    _CACHE[key] = (time.time(), value)
    return value


def configured() -> bool:
    return bool(config.env("STEAM_API_KEY") and config.env("STEAM_ID"))


def _get(url: str, params: dict[str, Any] | None = None,
         timeout: float = 15.0) -> dict[str, Any] | None:
    try:
        response = httpx.get(url, params=params, timeout=timeout,
                             follow_redirects=True,
                             headers={"User-Agent": "personal-radio/1.0"})
        response.raise_for_status()
        return response.json()
    except (httpx.HTTPError, ValueError) as error:
        if config.DEBUG:
            print("[steam] request failed", url, error, flush=True)
        return None


# --------------------------------------------------------------------------
# Library
# --------------------------------------------------------------------------
def _owned_games() -> list[dict[str, Any]]:
    if not configured():
        return []
    data = _get(f"{API}/IPlayerService/GetOwnedGames/v1/", {
        "key": config.env("STEAM_API_KEY"),
        "steamid": config.env("STEAM_ID"),
        "include_appinfo": 1,
        "include_played_free_games": 1,
    })
    return ((data or {}).get("response") or {}).get("games") or []


def _recently_played() -> list[dict[str, Any]]:
    if not configured():
        return []
    data = _get(f"{API}/IPlayerService/GetRecentlyPlayedGames/v1/", {
        "key": config.env("STEAM_API_KEY"),
        "steamid": config.env("STEAM_ID"),
    })
    return ((data or {}).get("response") or {}).get("games") or []


def tracked_titles() -> list[dict[str, Any]]:
    """Games the station considers 'yours', most relevant first."""
    cfg = config.games
    manual = [
        {"appid": entry.get("appid"), "name": entry.get("name", ""),
         "playtime": 10**6 if entry.get("priority") == "high" else 10**5,
         "manual": True, "notes": entry.get("notes", "")}
        for entry in (cfg.get("watchlist") or [])
        if isinstance(entry, dict) and entry.get("name")
    ]

    if not cfg.get("steam.enabled", True) or not configured():
        return manual

    def build() -> list[dict[str, Any]]:
        by_app: dict[int, dict[str, Any]] = {}

        if cfg.get("steam.use_recently_played", True):
            window = float(cfg.get("steam.recently_played_days", 21) or 21)
            for game in _recently_played():
                by_app[game["appid"]] = {
                    "appid": game["appid"], "name": game.get("name", ""),
                    # Recent play is the strongest possible relevance signal.
                    "playtime": (game.get("playtime_2weeks") or 0) * 100 + window,
                    "recent": True,
                }

        minimum = float(cfg.get("steam.min_playtime_minutes", 120) or 0)
        for game in _owned_games():
            if game["appid"] in by_app:
                continue
            minutes = game.get("playtime_forever") or 0
            if minutes < minimum:
                continue
            by_app[game["appid"]] = {
                "appid": game["appid"], "name": game.get("name", ""),
                "playtime": minutes, "recent": False,
            }

        ordered = sorted(by_app.values(), key=lambda g: -g["playtime"])
        cap = int(cfg.get("steam.max_tracked_titles", 25) or 25)
        return ordered[:cap]

    blocked = {str(n).lower() for n in (cfg.get("blocklist") or [])}
    combined = manual + _cached("tracked", 3600, build)
    return [g for g in combined if g.get("name", "").lower() not in blocked]


# --------------------------------------------------------------------------
# Patch notes
# --------------------------------------------------------------------------
_BBCODE = re.compile(r"\[/?[^\]]{1,40}\]")
_TAGS = re.compile(r"<[^>]+>")
_WS = re.compile(r"[ \t]*\n[ \t]*")


def _clean_body(text: str) -> str:
    text = _BBCODE.sub("", text or "")
    text = _TAGS.sub(" ", text)
    text = html.unescape(text)
    text = _WS.sub("\n", text)
    return re.sub(r"\n{3,}", "\n\n", text).strip()


def _news_for_app(appid: int, count: int = 8) -> list[dict[str, Any]]:
    data = _get(f"{API}/ISteamNews/GetNewsForApp/v2/", {
        "appid": appid, "count": count, "maxlength": 0,
    })
    return ((data or {}).get("appnews") or {}).get("newsitems") or []


def _looks_like_patch(item: dict[str, Any], keywords: list[str]) -> bool:
    title = (item.get("title") or "").lower()
    return any(word in title for word in keywords)


def latest_patch() -> dict[str, Any] | None:
    """Find one unread, recent, substantial patch for a game you play."""
    cfg = config.games
    max_age = float(cfg.get("patch_notes.max_age_hours", 96) or 96) * 3600
    min_chars = int(cfg.get("patch_notes.min_body_chars", 240) or 0)
    accepted = {str(f) for f in (cfg.get("patch_notes.accepted_feeds") or [])}
    keywords = [str(k).lower() for k in (cfg.get("patch_notes.patch_keywords") or [])]
    cooldown = float(cfg.get("patch_notes.per_game_cooldown_hours", 20) or 0) * 3600
    now = time.time()

    for game in tracked_titles():
        appid = game.get("appid")
        if not appid:
            continue
        if cooldown and db.is_seen("patch_game", f"{appid}:{int(now // cooldown)}"):
            continue

        for item in _news_for_app(int(appid)):
            if accepted and item.get("feedname") not in accepted:
                continue
            if item.get("date") and now - float(item["date"]) > max_age:
                continue
            if keywords and not _looks_like_patch(item, keywords):
                continue
            ident = str(item.get("gid") or item.get("url") or "")
            if not ident or db.is_seen("patch", ident):
                continue

            body = _clean_body(item.get("contents", ""))
            if len(body) < min_chars:
                continue

            return {
                "ident": ident,
                "game": game.get("name", "a game"),
                "appid": appid,
                "title": item.get("title", ""),
                "body": body[:3500],
                "url": item.get("url", ""),
                "published": float(item.get("date") or 0),
                "recent": game.get("recent", False),
            }
    return None


def mark_patch_read(patch: dict[str, Any]) -> None:
    db.mark_seen("patch", patch["ident"])
    cooldown = float(config.games.get("patch_notes.per_game_cooldown_hours", 20) or 0)
    if cooldown:
        bucket = int(time.time() // (cooldown * 3600))
        db.mark_seen("patch_game", f"{patch['appid']}:{bucket}")


# --------------------------------------------------------------------------
# Wishlist -> ad reads
# --------------------------------------------------------------------------
def wishlist() -> list[dict[str, Any]]:
    """Public wishlist. Returns [] if the profile is private, which is fine."""
    if not configured() or not config.games.get("steam.use_wishlist_for_ads", True):
        return []

    def build() -> list[dict[str, Any]]:
        data = _get(f"{API}/IWishlistService/GetWishlist/v1/", {
            "key": config.env("STEAM_API_KEY"),
            "steamid": config.env("STEAM_ID"),
        })
        entries = ((data or {}).get("response") or {}).get("items") or []
        return [{"appid": e.get("appid")} for e in entries if e.get("appid")]

    return _cached("wishlist", 7200, build)


def app_details(appid: int) -> dict[str, Any] | None:
    """Store page data: name, blurb, genres, release date."""
    def build() -> dict[str, Any] | None:
        data = _get(f"{STORE}/api/appdetails", {"appids": appid, "l": "en"})
        node = (data or {}).get(str(appid)) or {}
        if not node.get("success"):
            return None
        payload = node.get("data") or {}
        return {
            "appid": appid,
            "name": payload.get("name", ""),
            "blurb": _clean_body(payload.get("short_description", "")),
            "genres": [g.get("description") for g in (payload.get("genres") or [])],
            "developer": ", ".join(payload.get("developers") or []),
            "release": (payload.get("release_date") or {}).get("date", ""),
            "coming_soon": bool((payload.get("release_date") or {}).get("coming_soon")),
            "free": bool(payload.get("is_free")),
        }

    return _cached(f"app:{appid}", 86400, build)


def ad_subject() -> dict[str, Any] | None:
    """Pick something to write a fake ad about: wishlist first, then library."""
    if not config.games.get("ads.enabled", True):
        return None

    pool: list[int] = [w["appid"] for w in wishlist()]
    if not pool:
        pool = [g["appid"] for g in tracked_titles() if g.get("appid")]
    import random
    random.shuffle(pool)
    for appid in pool[:8]:
        if db.is_seen("ad", str(appid)):
            continue
        details = app_details(int(appid))
        if details and details.get("name"):
            return details
    manual = [dict(name=g["name"], blurb=g.get("notes", ""), ad_key="manual:" + g["name"])
              for g in (config.games.get("watchlist") or [])
              if isinstance(g, dict) and g.get("name") and not g.get("appid")]
    if manual:
        return random.choice(manual)
    if not config.games.get("ads.house_fallback", True):
        return None
    products = config.games.get("ads.house_products") or [
        {"name": "Queue Insurance", "blurb": "A fictional policy covering the embarrassment of being handed the aux."},
        {"name": "Grass Touch Simulator", "blurb": "An imaginary game about finally going outside, played indoors."},
        {"name": "One More Song Alarm", "blurb": "A fake alarm clock that accepts one more song as a valid sleep schedule."},
    ]
    products = [{**p, "fictional": True, "ad_key": "house:" + str(p["name"])} for p in products
                if isinstance(p, dict) and p.get("name")]
    random.shuffle(products)
    # Rotate the least recently used house product instead of exhausting the pool.
    def last_used(product):
        row = db.one("SELECT ts FROM seen WHERE kind='ad' AND ident=?", (product["ad_key"],))
        return row["ts"] if row else 0
    return min(products, key=last_used) if products else None


def mark_ad_used(details: dict[str, Any]) -> None:
    db.mark_seen("ad", str(details.get("ad_key") or details.get("appid") or details["name"]))


def status() -> dict[str, Any]:
    return {
        "configured": configured(),
        "tracked": len(tracked_titles()),
        "wishlist": len(wishlist()),
    }
