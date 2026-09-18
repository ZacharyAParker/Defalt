"""News: fetch feeds, filter them, hand the hosts something worth reading.

Categories, feeds, weights and blocklists all live in config/news.yaml and are
hot-reloaded, so adding a category is a text edit and nothing more.
"""
from __future__ import annotations

import hashlib
import random
import re
import time
from calendar import timegm
from typing import Any

import feedparser
import httpx

from .. import config, db

_TAGS = re.compile(r"<[^>]+>")
_WS = re.compile(r"\s+")
_CACHE: dict[str, tuple[float, list[dict[str, Any]]]] = {}
CACHE_TTL = 600.0


def _clean(text: str) -> str:
    return _WS.sub(" ", _TAGS.sub(" ", text or "")).strip()


def _published(entry: Any) -> float:
    for field in ("published_parsed", "updated_parsed"):
        value = getattr(entry, field, None)
        if value:
            try:
                return timegm(value)
            except (TypeError, ValueError):
                continue
    return 0.0


def _fetch_feed(url: str, timeout: float) -> list[dict[str, Any]]:
    cached = _CACHE.get(url)
    if cached and time.time() - cached[0] < CACHE_TTL:
        return cached[1]

    try:
        response = httpx.get(url, timeout=timeout, follow_redirects=True,
                             headers={"User-Agent": "personal-radio/1.0"})
        response.raise_for_status()
        parsed = feedparser.parse(response.content)
    except (httpx.HTTPError, ValueError) as error:
        if config.DEBUG:
            print("[news] feed failed", url, error, flush=True)
        return []

    items = []
    for entry in parsed.entries[:25]:
        title = _clean(getattr(entry, "title", ""))
        if not title:
            continue
        summary = _clean(getattr(entry, "summary", "") or
                         getattr(entry, "description", ""))
        link = getattr(entry, "link", "") or ""
        items.append({
            "title": title,
            "summary": summary[:600],
            "link": link,
            "published": _published(entry),
            "source": _clean(getattr(parsed.feed, "title", "")) or url,
            "ident": hashlib.sha1((link or title).encode()).hexdigest()[:16],
        })

    _CACHE[url] = (time.time(), items)
    return items


def _blocked(title: str, blocklist: list[str]) -> bool:
    lowered = title.lower()
    return any(word and word.lower() in lowered for word in blocklist)


def pick_category() -> tuple[str, dict[str, Any]] | None:
    """Weighted choice among enabled categories that have feeds."""
    categories = config.news.get("categories", {}) or {}
    pool = [
        (name, data) for name, data in categories.items()
        if isinstance(data, dict) and data.get("enabled") and data.get("feeds")
    ]
    if not pool:
        return None
    weights = [max(0.01, float(data.get("weight", 1) or 1)) for _, data in pool]
    return random.choices(pool, weights=weights, k=1)[0]


def stories(category: str | None = None, limit: int | None = None
            ) -> tuple[str, list[dict[str, Any]]]:
    """Return (category label, unread stories). Marks nothing as read -- the
    segment writer does that only once a story actually makes it to air."""
    defaults = config.news.get("defaults", {}) or {}
    timeout = float(defaults.get("timeout", 12) or 12)
    max_age = float(defaults.get("max_age_hours", 30) or 30) * 3600
    blocklist = [str(w) for w in (defaults.get("global_blocklist") or [])]

    if category:
        data = (config.news.get("categories", {}) or {}).get(category)
        if not data:
            return ("", [])
        chosen = (category, data)
    else:
        picked = pick_category()
        if not picked:
            return ("", [])
        chosen = picked

    name, data = chosen
    limit = limit or int(defaults.get("items_per_segment", 2) or 2)
    blocklist = blocklist + [str(w) for w in (data.get("blocklist") or [])]

    collected: list[dict[str, Any]] = []
    now = time.time()
    for url in data.get("feeds") or []:
        for item in _fetch_feed(str(url), timeout):
            if item["published"] and now - item["published"] > max_age:
                continue
            if _blocked(item["title"], blocklist):
                continue
            if db.is_seen("news", item["ident"]):
                continue
            item["category"] = name
            item["category_label"] = data.get("label", name)
            item["tone"] = data.get("tone", "")
            collected.append(item)

    collected.sort(key=lambda i: i["published"], reverse=True)
    # Shuffle within the freshest handful so the same feed doesn't dominate.
    head = collected[:limit * 4]
    random.shuffle(head)
    return (str(data.get("label", name)), head[:limit])


def search(topic: str, limit: int = 3) -> list[dict[str, Any]]:
    """Find stories about a topic across every enabled category.

    Returns [] rather than something loosely related when nothing matches.
    That is deliberate: an empty result makes the hosts say they have nothing,
    which is far better than handing them an unrelated article and letting
    them improvise around a headline that does not answer the question.
    """
    words = {w for w in re.split(r"[^\w]+", (topic or "").lower())
             if len(w) > 1 and w not in _STOPWORDS}
    if not words:
        return []

    # Short terms must match as whole words. "AI" is a real topic, but as a
    # substring it hits "said", "again" and "maintain", which would hand the
    # hosts a pile of unrelated stories and let them invent a connection.
    matchers = []
    for word in words:
        if len(word) <= 3:
            matchers.append(re.compile(rf"\b{re.escape(word)}\b").search)
        else:
            matchers.append(lambda text, w=word: w in text)

    defaults = config.news.get("defaults", {}) or {}
    timeout = float(defaults.get("timeout", 12) or 12)
    # A topic request is allowed to reach further back than a rolling bulletin.
    max_age = float(defaults.get("max_age_hours", 30) or 30) * 3600 * 4
    now = time.time()

    scored: list[tuple[float, dict[str, Any]]] = []
    for name, data in (config.news.get("categories", {}) or {}).items():
        if not isinstance(data, dict) or not data.get("enabled"):
            continue
        for url in data.get("feeds") or []:
            for item in _fetch_feed(str(url), timeout):
                if item["published"] and now - item["published"] > max_age:
                    continue
                haystack = f"{item['title']} {item['summary']}".lower()
                hits = sum(1 for match in matchers if match(haystack))
                if not hits:
                    continue
                # Title matches count double -- a word in the headline is what
                # the story is about, one in the body may be an aside.
                title = item["title"].lower()
                title_hits = sum(1 for match in matchers if match(title))
                score = hits + title_hits
                if score < max(1, len(words) * 0.5):
                    continue
                item = dict(item)
                item["category"] = name
                item["category_label"] = data.get("label", name)
                item["tone"] = data.get("tone", "")
                scored.append((score, item))

    scored.sort(key=lambda pair: (-pair[0], -pair[1]["published"]))
    seen: set[str] = set()
    out: list[dict[str, Any]] = []
    for _, item in scored:
        if item["ident"] in seen:
            continue
        seen.add(item["ident"])
        out.append(item)
        if len(out) >= limit:
            break
    return out


_STOPWORDS = {
    "the", "and", "for", "with", "about", "what", "whats", "that", "this",
    "any", "some", "news", "tell", "talk", "cover", "give", "show", "please",
    "latest", "happening", "going", "update", "updates", "story", "stories",
}


def mark_read(items: list[dict[str, Any]]) -> None:
    for item in items:
        db.mark_seen("news", item["ident"])
    days = int((config.news.get("defaults", {}) or {}).get("dedupe_days", 5) or 5)
    db.prune_seen(max(days, 1) * 4)
