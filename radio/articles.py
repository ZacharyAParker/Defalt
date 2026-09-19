"""Listener-supplied news sources, kept separate from song and vibe requests."""
from __future__ import annotations

import hashlib
import ipaddress
import json
import re
import socket
import threading
import time
from html.parser import HTMLParser
from urllib.parse import urljoin, urlsplit

import httpx

from . import db

MAX_TEXT = 24000
MAX_BYTES = 2_000_000
_SLOTS = threading.BoundedSemaphore(2)


class ArticleHTML(HTMLParser):
    """Prefer editorial body markup; ignore navigation and embedded promotions."""
    VOID = {'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'wbr'}
    SKIP = {'script', 'style', 'nav', 'footer', 'aside', 'form', 'button', 'noscript', 'svg'}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.stack = []
        self.parts = {0: [], 1: [], 2: [], 3: []}
        self.meta = {}
        self.title = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'meta':
            self.meta[attrs.get('property', attrs.get('name', ''))] = attrs.get('content', '')
        if tag in self.VOID:
            if tag in ('br', 'hr'):
                self.handle_data('\n')
            return
        parent_skip, parent_level = self.stack[-1][1:] if self.stack else (False, 0)
        classes = attrs.get('class', '').split()
        skip = parent_skip or tag in self.SKIP or 'not-prose' in classes or 'hidden' in attrs
        level = max(parent_level, 1 if tag == 'main' else 2 if tag == 'article' else 0)
        if any(c in ('prose', 'article-body', 'entry-content', 'post-content', 'ms-blog-content') for c in classes):
            level = 3
        self.stack.append((tag, skip, level))
        if tag in ('p', 'div', 'section', 'li', 'h1', 'h2', 'h3'):
            self.handle_data('\n')

    def handle_endtag(self, tag):
        if tag in self.VOID:
            return
        self.handle_data('\n' if tag in ('p', 'div', 'li', 'h1', 'h2', 'h3', 'section') else '')
        for index in range(len(self.stack) - 1, -1, -1):
            if self.stack[index][0] == tag:
                del self.stack[index:]
                break

    def handle_data(self, text):
        if self.stack and self.stack[-1][1]:
            return
        if any(tag == 'title' for tag, _, _ in self.stack):
            self.title.append(text)
            return
        level = self.stack[-1][2] if self.stack else 0
        self.parts[level].append(text)

    def article(self, url):
        text = next((''.join(self.parts[n]) for n in (3, 2, 1, 0)
                     if len(''.join(self.parts[n]).strip()) >= 200), '')
        return source(text, title=self.meta.get('og:title') or ''.join(self.title),
                      publisher=self.meta.get('og:site_name') or urlsplit(url).hostname,
                      url=url, published=self.meta.get('article:published_time', ''))


def source(text, *, title='', publisher='Pasted article', url='', published=''):
    text = '\n'.join(re.sub(r'\s+', ' ', line).strip() for line in text.splitlines())
    text = re.sub(r'\n{3,}', '\n\n', text).strip()
    if len(text) < 200:
        raise ValueError('Could not find enough article text. Paste the article itself instead.')
    if len(text) > MAX_TEXT:
        raise ValueError('This article is too long. Paste an excerpt of up to 24,000 characters.')
    warnings = []
    year_match = re.match(r'(20\d\d)', published)
    if year_match:
        publication_year = int(year_match[1])
        for sentence in re.split(r'(?<=[.!?])\s+', text):
            years = [int(y) for y in re.findall(r'\b20\d\d\b', sentence)]
            if (years and max(years) < publication_year and
                    re.search(r'\b(will|expected|predict\w*|signs point|set to|likely to)\b', sentence, re.I)):
                warnings.append('The page is dated ' + str(publication_year) +
                                ', but includes a prediction for ' + str(max(years)) +
                                '. Treat its timing as inconsistent, not a current release forecast.')
                break
    return {'text': text, 'title': (title.strip() or text.splitlines()[0])[:180],
            'source': str(publisher or 'Pasted article')[:100], 'url': url,
            'published': published[:80], 'retrieved_at': time.strftime('%Y-%m-%d'), 'warnings': warnings}


def public_url(url):
    parsed = urlsplit(url)
    if (parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username
            or parsed.password or parsed.port not in (None, 80, 443)):
        raise ValueError('Use a public http or https article link, or paste the article text.')
    addresses = socket.getaddrinfo(parsed.hostname, parsed.port or (443 if parsed.scheme == 'https' else 80))
    if not addresses or any(not ipaddress.ip_address(a[4][0]).is_global for a in addresses):
        raise ValueError('Use a public article link, or paste the article text.')
    return addresses[0][4][0]


def fetch(url):
    # sourceio runs this in a process with a total deadline, including DNS and redirects.
    with httpx.Client(timeout=8, trust_env=False, limits=httpx.Limits(max_keepalive_connections=0),
                      headers={'User-Agent': 'Defalt/1.0 article reader'}) as client:
        for _ in range(4):
            address = public_url(url)
            original = httpx.URL(url)
            # Connect to the validated address; do not resolve the hostname a
            # second time after checking it. Preserve TLS hostname verification.
            target = original.copy_with(host=address)
            client.cookies.clear()
            with client.stream('GET', target, headers={'Host': original.netloc.decode('ascii')},
                               extensions={'sni_hostname': original.raw_host.decode('ascii')}) as response:
                if response.is_redirect:
                    url = urljoin(url, response.headers.get('location', ''))
                    continue
                response.raise_for_status()
                content_type = response.headers.get('content-type', '').lower()
                if not any(t in content_type for t in ('text/html', 'application/xhtml+xml', 'text/plain')):
                    raise ValueError('That link is not an article page. Paste the article text instead.')
                data = bytearray()
                for chunk in response.iter_bytes():
                    data.extend(chunk)
                    if len(data) > MAX_BYTES:
                        raise ValueError('That page is too large. Paste the article text instead.')
                text = data.decode(response.encoding or 'utf-8', errors='replace')
                if 'text/plain' in content_type:
                    return source(text, publisher=urlsplit(url).hostname, url=url)
                parser = ArticleHTML()
                parser.feed(text)
                return parser.article(url)
    raise ValueError('Too many redirects. Paste the article text instead.')


def _prepare(wish_id, raw):
    try:
        from . import sourceio
        article = sourceio._run('article', {'url': raw}, 20)
        db.write("UPDATE wishes SET status='pending', subject=?, payload=?, note=NULL, expires_at=? "
                 "WHERE id=? AND status='preparing'",
                 (article['title'], json.dumps(article), time.time() + 2700, wish_id))
    except Exception as error:
        note = str(error).replace('Song article', 'Article')[:350]
        db.write("UPDATE wishes SET status='failed', note=? WHERE id=? AND status='preparing'",
                 (note + ' You can paste the article text instead.', wish_id))
    finally:
        _SLOTS.release()


def submit(raw):
    raw = raw.strip()
    if not raw or len(raw) > MAX_TEXT:
        return {'ok': False, 'message': 'Paste an article link or 200–24,000 characters of article text.'}
    linked = bool(re.match(r'^https?://\S+$', raw, re.I))
    if not linked and re.match(r'^\w+://\S+$', raw):
        return {'ok': False, 'message': 'Use an http or https article link.'}
    fingerprint = hashlib.sha256(raw.encode()).hexdigest()
    existing = db.one("SELECT id FROM wishes WHERE kind='article' AND raw=? "
                      "AND status IN ('pending','preparing','active') AND expires_at>?", (fingerprint, time.time()))
    if existing:
        return {'ok': True, 'kind': 'article', 'id': existing['id'], 'message': 'That article is already waiting.'}
    try:
        article = {} if linked else source(raw)
    except ValueError as error:
        return {'ok': False, 'message': str(error)}
    if linked and not _SLOTS.acquire(blocking=False):
        return {'ok': False, 'message': 'Two articles are already being fetched. Try again shortly.'}
    try:
        wish_id = db.write("INSERT INTO wishes (ts,raw,kind,subject,payload,timing,status,expires_at) "
                           "VALUES (?,?,'article',?,?,'next',?,?)",
                           (time.time(), fingerprint, raw[:180] if linked else article['title'],
                            json.dumps(article), 'preparing' if linked else 'pending',
                            time.time() + (120 if linked else 2700)))
        if linked:
            threading.Thread(target=_prepare, args=(wish_id, raw), daemon=True, name='article-fetch').start()
    except Exception:
        if linked:
            _SLOTS.release()
        raise
    return {'ok': True, 'kind': 'article', 'id': wish_id,
            'message': ('Fetching the article. Progress appears in the queue.' if linked else
                        'Article queued for the next unwritten host break.')}
