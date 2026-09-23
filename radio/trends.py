"""Dated public charts, cached off the playback path. No guessed platform trends."""
import json
import math
import re
import threading
import time
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime

import httpx

from . import chart_scraper, config, db

_LOCK = threading.RLock()
_cache = None
MAX_AGE = 48 * 3600


def enabled():
    return bool(config.station.get('selection.trends_enabled', True))


def country():
    value = str(config.station.get('selection.trends_country', 'us')).lower()
    return value if re.fullmatch('[a-z]{2}', value) else 'us'


def timestamp(value):
    try:
        date = datetime.fromisoformat(value.replace('Z', '+00:00'))
    except (ValueError, TypeError, AttributeError):
        try:
            date = parsedate_to_datetime(value)
        except (ValueError, TypeError, AttributeError):
            return 0
    return date.replace(tzinfo=date.tzinfo or timezone.utc).timestamp()


def parse(payload, source, url, region):
    feed = payload.get('feed', {})
    updated = feed.get('updated', '')
    if isinstance(updated, dict):
        updated = updated.get('label', '')
    published = timestamp(updated)
    if not published or not -7200 <= time.time() - published < MAX_AGE:
        raise ValueError('Chart has no recent publication date')
    items = []
    for rank, item in enumerate(feed.get('results', feed.get('entry', []))[:50], 1):
        if not isinstance(item, dict):
            continue
        artist = item.get('artistName') or item.get('im:artist', {}).get('label')
        title = item.get('name') or item.get('im:name', {}).get('label')
        if not all(isinstance(value, str) and 0 < len(value) <= 180 for value in (artist, title)):
            continue
        genres = [g.get('name') for g in item.get('genres', []) if isinstance(g, dict) and g.get('name') != 'Music']
        if not genres:
            genre = item.get('category', {}).get('attributes', {}).get('label')
            genres = [genre] if genre else []
        items.append({'artist': artist, 'title': title, 'genre': ', '.join(genres), 'rank': rank,
                      'source': source, 'url': url, 'country': region, 'updated': updated,
                      'published_at': published})
    if not items:
        raise ValueError('Chart contained no songs')
    return items


def _stored():
    global _cache
    path = config.CACHE_DIR / f'trend-charts-{country()}.json'
    with _LOCK:
        if _cache is None or _cache.get('_path') != str(path):
            try:
                data = json.loads(path.read_text(encoding='utf-8'))
                if not isinstance(data, dict):
                    data = {}
            except (OSError, ValueError):
                data = {}
            _cache = {**data, '_path': str(path)}
        return dict(_cache)


def cached():
    if not enabled():
        return []
    items = _stored().get('items', [])
    if not isinstance(items, list):
        return []
    return [dict(item) for item in items if isinstance(item, dict)
            and isinstance(item.get('published_at'), (int, float))
            and isinstance(item.get('rank'), int) and 1 <= item['rank'] <= 50
            and all(isinstance(item.get(k), str) for k in ('title','artist','source','country'))
            and -7200 <= time.time() - item['published_at'] < (
                chart_scraper.MAX_AGE if item['source'] == chart_scraper.SOURCE else MAX_AGE)]


def refresh():
    global _cache
    if not enabled():
        return {'state': 'disabled'}
    old = _stored()
    region = country()
    previous = old.get('collectors', {})
    if not isinstance(previous, dict):
        previous = {}
    collectors = {}
    for name, fetcher, interval in [('apple', _apple, 7200), ('spotify_kworb', chart_scraper.fetch, 21600)]:
        saved = previous.get(name, {})
        if not isinstance(saved, dict):
            saved = {}
        next_check = saved.get('next_check', 0)
        if isinstance(next_check, (int, float)) and time.time() < next_check:
            collectors[name] = saved
            continue
        try:
            items = fetcher(region)
        except (httpx.HTTPError, ValueError, TypeError, AttributeError):
            items = []
        collectors[name] = {'next_check': time.time() + (interval if items else 900),
                            'items': items or saved.get('items', [])}
    value = {**old, 'collectors': collectors,
             'items': [item for saved in collectors.values() for item in
                       (saved.get('items', []) if isinstance(saved.get('items'), list) else [])]}
    with _LOCK:
        _cache = value
        try:
            path = config.CACHE_DIR / f'trend-charts-{region}.json'
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(value), encoding='utf-8')
        except OSError:
            pass
    available = cached()
    sources = list(dict.fromkeys(item['source'] for item in available))
    return {'state': 'ready' if available else 'unavailable',
            'source': sources[0] if sources else None, 'sources': sources}


def _apple(region):
    feeds = [('Apple Music', f'https://rss.marketingtools.apple.com/api/v2/{region}/music/most-played/50/songs.json'),
             ('iTunes', f'https://itunes.apple.com/{region}/rss/topsongs/limit=50/explicit=true/json')]
    for source, url in feeds:
        try:
            response = httpx.get(url, timeout=10, headers={'User-Agent': 'Defalt/0.3'})
            response.raise_for_status()
            return parse(response.json(), source, url, region)
        except (httpx.HTTPError, ValueError, TypeError, AttributeError):
            continue
    return []


def match(track, chart):
    # One boost per recording even when it appears on multiple charts.
    return min((item for item in chart if db.track_key(item['artist'], item['title']) ==
                 db.track_key(track.get('artist'), track.get('title'))),
               key=lambda item: item['rank'], default=None)


def influence(scored):
    chart = cached()
    try:
        value = float(config.station.get('selection.trend_strength', .3))
        strength = max(0., min(1., value)) if math.isfinite(value) else .3
    except (ValueError, TypeError):
        strength = .3
    # One boost per recording, keyed once per pass rather than per track.
    ranked = {}
    for item in chart:
        key = db.track_key(item['artist'], item['title'])
        if key not in ranked or item['rank'] < ranked[key]['rank']:
            ranked[key] = item
    result = []
    for weight, track in scored:
        evidence = ranked.get(db.track_key(track.get('artist'), track.get('title'))) if ranked else None
        if evidence and strength:
            multiplier = 1 + strength * (1 - (evidence['rank'] - 1) / 50)
            selection = {**track.get('selection', {}), 'trend': evidence,
                         'reason': track.get('selection', {}).get('reason', 'Taste and rotation') +
                         f"; {evidence['source']} {evidence['country'].upper()} chart #{evidence['rank']}"}
            track = {**track, 'selection': selection}
            weight *= multiplier
        result.append((weight, track))
    return result
