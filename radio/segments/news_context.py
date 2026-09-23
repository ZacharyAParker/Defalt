"""Enough sourced detail for a news read, including a useful offline fallback."""
import html
import re
import time

from .. import config, sourceio
from .base import Line, MAX_WORDS_PER_LINE

_CACHE = {}


def for_ad(category):
    """News comedy needs a dated recent report, not an undated or future headline."""
    from ..sources import rss
    _, stories=rss.stories(category,limit=4)
    max_age=float(config.news.get('defaults.max_age_hours',30) or 30)*3600
    now=time.time()
    stories=[s for s in stories if s.get('published') and 0 <= now-float(s['published']) <= max_age]
    stories.sort(key=lambda s:s['published'],reverse=True)
    return prepare(stories)


def clean(text):
    return re.sub(r'\s+', ' ', html.unescape(re.sub(r'<[^>]+>', ' ', str(text or '')))).strip()


def sentences(text):
    return [s.strip() for s in re.findall(r'.+?[.!?](?=\s|$)', clean(text)) if s.strip()]


def substantial(text, title=''):
    words = re.findall(r'\w+', clean(text).lower())
    headline = set(re.findall(r'\w+', title.lower()))
    return len(words) >= 35 and len(set(words) - headline) >= 14 and len(sentences(text)) >= 2


def prepare(stories, limit=2):
    """Try at most two sources, stopping once `limit` stories are usable.

    A news break airs one story, so the news writer asks for one: a second
    article fetch would cost up to eight seconds for text nobody hears.
    Never invent detail or block playback on a page.
    """
    ready = []
    for original in stories[:2]:
        if len(ready) >= limit:
            break
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


FALLBACK_WORDS = 40


def fallback(story, anchor, budget=28):
    """Headline plus one complete source sentence, attributed. Nothing more.

    This airs when the writer failed, so it is a short bulletin rather than a
    recitation of the feed: about forty words, one line, never a cut sentence.
    """
    source = clean(story.get('source'))[:100] or 'the report'
    title = clean(story.get('title')).rstrip('.!?:;, ')
    limit = min(FALLBACK_WORDS, MAX_WORDS_PER_LINE, max(18, int(budget * 2.3)))
    headline = f'According to {source}: {title}.' if title else f'According to {source}:'
    heading = set(re.findall(r'\w+', title.lower()))
    for sentence in sentences(story['summary']):
        words = set(re.findall(r'\w+', sentence.lower()))
        # Skip a first sentence that only restates the headline.
        if len(words - heading) < 5:
            continue
        text = f'{headline} {sentence}'
        if len(text.split()) <= limit:
            return [Line(anchor, text)]
        break
    return []


def usable(lines):
    text = ' '.join(line.text for line in lines)
    return (len(text.split()) >= 35 and
            not re.search(r"(?:that.s (?:it|the whole story)\?|that is the whole story)", text, re.I))
