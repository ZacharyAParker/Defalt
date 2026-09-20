"""Enough sourced detail for a news read, including a useful offline fallback."""
import html
import re
import time

from .. import sourceio
from .base import Line

_CACHE = {}


def clean(text):
    return re.sub(r'\s+', ' ', html.unescape(re.sub(r'<[^>]+>', ' ', str(text or '')))).strip()


def sentences(text):
    return [s.strip() for s in re.findall(r'.+?[.!?](?=\s|$)', clean(text)) if s.strip()]


def substantial(text, title=''):
    words = re.findall(r'\w+', clean(text).lower())
    headline = set(re.findall(r'\w+', title.lower()))
    return len(words) >= 35 and len(set(words) - headline) >= 14 and len(sentences(text)) >= 2


def prepare(stories):
    """Try at most two sources. Never invent detail or block playback on a page."""
    ready = []
    for original in stories[:2]:
        item = dict(original)
        text = clean(item.get('summary'))
        if not substantial(text, item.get('title', '')) and item.get('link'):
            url = item['link']
            cached = _CACHE.get(url)
            if cached is None or time.monotonic() - cached[0] > 1800:
                try:
                    article = sourceio._run('article', {'url': url}, 8)
                    expanded = clean(article.get('text'))[:5000]
                except Exception:
                    expanded = ''
                if len(_CACHE) >= 100:
                    _CACHE.pop(next(iter(_CACHE)))
                cached = _CACHE[url] = (time.monotonic(), expanded)
            if substantial(cached[1], item.get('title', '')):
                text = cached[1]
        if substantial(text, item.get('title', '')):
            item['summary'] = text
            ready.append(item)
    return ready


def fallback(story, anchor, budget=28):
    """Read complete source sentences, with attribution and no filler exchange."""
    source = clean(story.get('source'))[:100] or 'the report'
    prefix = f'According to {source}, '
    limit = max(35, min(95, int(budget * 2.3)))
    selected = []
    count = len(prefix.split())
    for sentence in sentences(story['summary']):
        size = len(sentence.split())
        if count + size > limit:
            break
        selected.append(sentence)
        count += size
    text = ' '.join(selected)
    if not substantial(text, story.get('title', '')):
        return []
    return [Line(anchor, prefix + text)]


def usable(lines):
    text = ' '.join(line.text for line in lines)
    return (len(text.split()) >= 35 and
            not re.search(r"(?:that.s (?:it|the whole story)\?|that is the whole story)", text, re.I))
