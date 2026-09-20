"""Small, infrequent collector for Kworb's public Spotify chart tables."""
import re
import time
from datetime import datetime, timezone
from html.parser import HTMLParser
from urllib.robotparser import RobotFileParser

import httpx

MAX_AGE = 72 * 3600  # Daily reporting dates can lag publication by two days.
SOURCE = 'Spotify via Kworb'
REGIONS = {'us': 'United States', 'gb': 'United Kingdom', 'ca': 'Canada',
           'au': 'Australia', 'de': 'Germany', 'fr': 'France', 'jp': 'Japan',
           'kr': 'South Korea', 'br': 'Brazil', 'mx': 'Mexico'}
AGENT = 'DefaltChartCollector/0.3 (+https://github.com/ZacharyAParker/Defalt)'


class ChartTable(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.heading = ''
        self.in_heading = self.in_table = self.in_cell = False
        self.rows, self.cells, self.links = [], [], []
        self.link = None

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'span' and 'pagetitle' in attrs.get('class', '').split():
            self.in_heading = True
        if tag == 'table' and attrs.get('id') == 'spotifydaily':
            self.in_table = True
        if not self.in_table:
            return
        if tag == 'tr':
            self.cells, self.links = [], []
        elif tag == 'td':
            self.in_cell = True
            self.cells.append('')
        elif tag == 'a' and self.in_cell and len(self.cells) == 3:
            self.link = [attrs.get('href', ''), '']

    def handle_data(self, data):
        if self.in_heading:
            self.heading += data
        if self.in_table and self.in_cell:
            self.cells[-1] += data
            if self.link is not None:
                self.link[1] += data

    def handle_endtag(self, tag):
        if tag == 'span':
            self.in_heading = False
        if not self.in_table:
            return
        if tag == 'a' and self.link is not None:
            self.links.append(self.link)
            self.link = None
        elif tag == 'td':
            self.in_cell = False
        elif tag == 'tr' and self.cells:
            self.rows.append((self.cells, self.links))
        elif tag == 'table':
            self.in_table = False


def parse(html, region, url):
    if region not in REGIONS or len(html) > 2_000_000:
        raise ValueError('Unsupported chart')
    table = ChartTable()
    table.feed(html)
    date = re.match(r'Spotify Daily Chart - ' + re.escape(REGIONS[region]) +
                    r' - (\d{4}/\d{2}/\d{2})(?:\s|$)', table.heading.strip())
    if not date:
        raise ValueError('Missing chart region or reporting date')
    updated = date[1].replace('/', '-')
    published = datetime.strptime(updated, '%Y-%m-%d').replace(tzinfo=timezone.utc).timestamp()
    if not -7200 <= time.time() - published < MAX_AGE:
        raise ValueError('Chart reporting date is stale')
    items, ranks = [], set()
    for cells, links in table.rows:
        if len(cells) < 3 or not cells[0].strip().isdigit():
            continue
        rank = int(cells[0].strip())
        artists = [label.strip() for href, label in links if re.fullmatch(r'\.\./artist/[A-Za-z0-9]{22}\.html', href)]
        songs = [(href, label.strip()) for href, label in links if re.fullmatch(r'\.\./track/[A-Za-z0-9]{22}\.html', href)]
        if (not 1 <= rank <= 50 or rank in ranks or not artists or len(songs) != 1 or
                not all(0 < len(value) <= 180 for value in (artists[0], songs[0][1]))):
            continue
        ranks.add(rank)
        items.append({'artist': artists[0], 'title': songs[0][1], 'genre': '', 'rank': rank,
                      'source': SOURCE, 'provider': 'Kworb', 'platform': 'Spotify',
                      'url': url, 'country': region, 'updated': updated, 'published_at': published,
                      'spotify_id': songs[0][0].split('/')[-1].removesuffix('.html')})
    if not items:
        raise ValueError('Chart contained no recognizable songs')
    return sorted(items, key=lambda item: item['rank'])


def fetch(region):
    if region not in REGIONS:
        return []
    url = f'https://kworb.net/spotify/country/{region}_daily.html'
    # Respect the site's current crawler instructions; no auth or browser cookies.
    with httpx.Client(timeout=10, headers={'User-Agent': AGENT}) as client:
        response = client.get('https://kworb.net/robots.txt')
        response.raise_for_status()
        robots = RobotFileParser()
        robots.parse(response.text.splitlines())
        if not robots.can_fetch(AGENT, url):
            return []
        response = client.get(url)
        response.raise_for_status()
        return parse(response.text, region, url)
