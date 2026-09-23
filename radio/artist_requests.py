"""Finite catalog requests use real recordings, never a guessed mood.

Two shapes share one queueing path: an artist batch ("songs by Laufey") and a
criteria batch ("songs from 2010-2015", "90s r&b"). Both resolve against the
local library and, when configured, Spotify's catalog, then insert ordinary
pending requests the feeder prepares like any other.
"""
import random
import re
import time

from . import config, db, eras, spotify, versions

MAX_WAITING = 12
_WORD_COUNTS = {'one': 1, 'two': 2, 'three': 3, 'four': 4, 'five': 5, 'six': 6,
                'seven': 7, 'eight': 8, 'nine': 9, 'ten': 10, 'a couple of': 2, 'a couple': 2}


def detect(message):
    text = re.sub(r"^(?:no[,.]?\s+|please\s+)", "", message.strip(), flags=re.I)
    text = re.sub(r"\s+please[.!]?$", "", text, flags=re.I).rstrip(".!?")
    match = re.fullmatch(r"(?:give me|play|queue|add)(?:\s+some|\s+a few|\s+(?P<count>\d+|one|two|three|four|five))?\s+"
                         r"(?:songs (?P<prep>by|from) (?P<after>.+)|(?P<before>.+) songs)", text, re.I)
    if not match:
        return None
    artist = (match['after'] or match['before']).strip()
    if artist.casefold() in {'happy', 'sad', 'chill', 'romantic', 'upbeat', 'new', 'different', 'normal'}:
        return None
    # "songs from 2010-2015" names an era, not an act called "2010-2015";
    # "songs by Drake from 2016" is an era request for one act. Both belong
    # to the catalog resolver. "songs by the 1975" is still a band.
    if match['before'] and eras.parse(artist) and not re.search(r"[^\W\d_]{3,}", eras.strip(artist)):
        return None
    if (match['prep'] or '').lower() == 'from' and eras.parse(artist):
        return None
    if re.search(r"\s(?:from|in|during)\s+\S", artist, re.I) and eras.parse(
            re.split(r"\s(?:from|in|during)\s", artist, maxsplit=1, flags=re.I)[1]):
        return None
    raw_count = (match['count'] or '3').casefold()
    count = {'one': 1, 'two': 2, 'three': 3, 'four': 4, 'five': 5}.get(raw_count)
    return {'type': 'artist_request', 'artist': artist, 'count': count if count else int(raw_count)}


_CATALOG_VERB = re.compile(
    r"^(?:(?:can|could|would) you\s+|(?:i(?:'d| would)|let'?s)\s+(?:like|love|want|hear)?\s*)?"
    r"(?:queue(?:\s+up)?|play|put on|add|spin|give me|throw on|line up|get me|find me|i want|some|more|how about)\b",
    re.I)
_MUSIC_NOUN = r"(?:songs?|tracks?|music|tunes|hits|bangers|jams|records|classics|anthems|stuff|throwbacks|slow jams)"


def detect_catalog(message):
    """Deterministic era requests: 'queue songs from 2010-2015', '90s r&b'.

    Needs both a request shape and an era, so a title that merely contains a
    year ('1979', '22') is never mistaken for a decade request, and an act
    named like a year ('songs by the 1975') is left to the artist path.
    Anything with words it cannot place returns None for the model to read.
    """
    from .intent import GENRE_WORDS, clean
    text = clean(message).strip()
    text = re.sub(r"^(?:no[,.]?\s+|ok(?:ay)?[,.]?\s+|hey[,.]?\s+|please\s+)", "", text, flags=re.I)
    text = re.sub(r"[\s,]*\b(?:please|pls|thanks)\b[\s.!]*$", "", text, flags=re.I).rstrip(".!?")
    if not text or len(text) > 200 or re.search(r"\b(?:less|stop|no more|avoid|without|don'?t)\b", text, re.I):
        return None
    # An act credited with "by" is not part of the era: "by the 1975 from 2016".
    who = None
    body = text
    credit = re.search(r"\bby\s+(?P<act>.+?)(?=\s+(?:from|in|during)\s+\S|$)", text, re.I)
    if credit:
        who = credit['act'].strip(" ,.")
        body = (text[:credit.start()] + " " + text[credit.end():]).strip()
    years = eras.parse(body)
    if not years:
        return None
    lowered = body.lower()
    noun = bool(re.search(r"\b" + _MUSIC_NOUN + r"\b", lowered))
    if not (_CATALOG_VERB.match(lowered) or noun):
        return None
    rest = eras.strip(lowered)
    genres = [g for g in sorted(GENRE_WORDS, key=len, reverse=True)
              if re.search(r"(?<![\w&])" + re.escape(g) + r"(?![\w&])", rest)]
    # Keep the longest genre phrase: "neo soul" wins over "soul".
    genres = [g for g in genres if not any(g != other and g in other for other in genres)][:3]
    if years[0] == years[1] and not noun and not genres and not re.search(
            r"\b(?:from|in|of|since|circa|around|this|last)\s+(?:the\s+)?(?:year\s+)?(?:\d{4}|year)\b", lowered):
        return None  # "play 1979" is a song title, not a year.
    if years[0] == years[1] and re.search(rf"(?<!from )(?<!in )\bthe\s+{years[0]}\b", lowered):
        return None  # "some The 1975 songs" names a band
    count = None
    match = re.search(r"\b(?P<count>\d{1,2}|" + "|".join(sorted(_WORD_COUNTS, key=len, reverse=True)) + r")\s+"
                      r"(?:more\s+|[\w&'-]+\s+){0,3}?" + _MUSIC_NOUN, rest)
    if match:
        raw = match['count']
        count = int(raw) if raw.isdigit() else _WORD_COUNTS[raw]
    # "some Drake songs from 2016": the words before the noun name an act.
    leftover = rest
    for genre in genres:
        leftover = re.sub(r"(?<![\w&])" + re.escape(genre) + r"(?![\w&])", " ", leftover)
    leftover = re.sub(r"^\s*" + _CATALOG_VERB.pattern.lstrip("^"), " ", leftover, flags=re.I)
    leftover = re.sub(r"\b(?:" + "|".join(sorted(_WORD_COUNTS, key=len, reverse=True)) + r"|\d{1,2})\b", " ", leftover)
    leftover = re.sub(r"\b(?:" + _MUSIC_NOUN[3:-1] + r"|" + "|".join(_FILLERS) + r")\b", " ", leftover)
    leftover = re.sub(r"[^\w\s&'$-]", " ", leftover)
    leftover = re.sub(r"\s+", " ", leftover).strip()
    if leftover:
        named = re.search(r"(?P<act>\S.*?)\s+(?:songs?|tracks?|hits|records|bangers|classics)\b", text, re.I)
        if who or not named or db.norm(named['act']).split()[-len(leftover.split()):] != db.norm(leftover).split():
            return None  # words we cannot place: let the model read the whole thing
        found = re.search(re.escape(leftover), text, re.I)
        who = found.group(0) if found else leftover
    action = {'type': 'catalog_request', 'years': list(years), 'genres': genres,
              'count': max(1, min(10, count or 5))}
    if who:
        if not re.search(r"[^\W\d_]", who):
            return None
        action['artist'] = who[:120]
    return action


# Words that qualify a request without naming an act or a genre.
_FILLERS = ('some', 'a few', 'a couple of', 'a', 'few', 'couple', 'of', 'me', 'us', 'up', 'the', 'good', 'great',
            'best', 'popular', 'old', 'older', 'classic', 'big', 'biggest', 'more', 'any', 'random', 'different',
            'fun', 'party', 'summer', 'chill', 'sad', 'happy', 'hype', 'upbeat', 'feel good', 'nostalgic',
            'throwback', 'underground', 'banger', 'hit', 'jam', 'real', 'actual', 'top', 'era', 'year', 'years',
            'and', 'or', 'with', 'to', 'for', 'from', 'in', 'during', 'on', 'then', 'that', 'were', 'was', 'out',
            'released', 'came', 'early', 'late', 'mid', 'new', 'songs?')


def _protected(protected_keys):
    seen = set(k for k in protected_keys if k)
    for key in list(seen):
        row = db.one('SELECT title,artist FROM tracks WHERE key=?', (key,))
        if row:
            seen.add(db.track_key(row['artist'], row['title']))
    return seen


def _select(candidates, seen, per_artist=None):
    selected, artists = [], {}
    for item in candidates:
        identity = db.track_key(item['artist'], item['title'])
        key = item.get('key') or identity
        if key in seen or identity in seen:
            continue
        name = db.norm(db.primary_artist(item['artist']))
        if per_artist is not None and artists.get(name, 0) >= per_artist:
            continue
        artists[name] = artists.get(name, 0) + 1
        seen.update((key, identity))
        selected.append(item)
    return selected


def _commit(selected, count):
    """Insert pending requests; capacity and duplicates are checked together.

    Another request may arrive while a search runs, so both are rechecked
    inside one immediate transaction.
    """
    conn = db.connect()
    added = []
    with conn:
        conn.execute('BEGIN IMMEDIATE')
        room = max(0, MAX_WAITING - conn.execute(
            "SELECT COUNT(*) FROM requests WHERE status IN ('pending','preparing')").fetchone()[0])
        for item in selected:
            if len(added) >= min(count, room):
                break
            key = item.get('key') or db.track_key(item['artist'], item['title'])
            if conn.execute("SELECT 1 FROM requests WHERE track_key=? AND status IN "
                            "('pending','preparing','queued','scheduled')", (key,)).fetchone():
                continue
            if conn.execute('SELECT 1 FROM tracks WHERE key=? AND blocked=1', (key,)).fetchone():
                continue
            year = item.get('year') if type(item.get('year')) is int else None
            conn.execute('INSERT OR IGNORE INTO tracks(key,title,artist,source,expected_ms,added_at,album,year) '
                         'VALUES(?,?,?,?,?,?,?,?)',
                         (key, item['title'], item['artist'], 'request', item.get('duration_ms') or 0,
                          time.time(), item.get('album') or None, year))
            if year:
                # Catalog evidence fills a missing release year; it never
                # overwrites one read from the file's own tags.
                conn.execute('UPDATE tracks SET year=? WHERE key=? AND year IS NULL', (year, key))
            conn.execute("INSERT INTO requests(ts,query,status,track_key) VALUES(?,?,'pending',?)",
                         (time.time(), f"{item['artist']} - {item['title']}", key))
            added.append(item)
    return added


def _edition_order(item):
    return (versions.clean_track(item), versions.alternate_track(item))


def queue(artist, count, protected_keys=()):
    if not isinstance(artist, str) or not 1 <= len(artist.strip()) <= 120:
        raise ValueError('Name the artist whose songs you want.')
    if type(count) is not int or not 1 <= count <= 5:
        raise ValueError('Choose one to five songs for an artist request.')
    artist = artist.strip()
    matches = lambda item: db.norm(db.primary_artist(item.get('artist', ''))) == db.norm(artist)
    candidates = [dict(row) for row in db.query(
        'SELECT * FROM tracks WHERE blocked=0 ORDER BY COALESCE(last_played,0), play_count, added_at DESC')
        if matches(dict(row))]
    if spotify.available():
        try:
            candidates += [dict(item) for item in spotify.search(f'artist:"{artist.replace(chr(34), "")}"') if matches(item)]
        except ValueError:
            pass  # The local catalog remains usable when the remote catalog is unavailable.
    candidates.sort(key=_edition_order)
    added = _commit(_select(candidates, _protected(protected_keys)), count)
    if not added:
        return (f'No new {artist} requests were added. I could not find an available matching recording '
                'outside the current queue, or the request queue is full. Name a song or use Spotify search. Your music direction is unchanged.')
    titles = '; '.join(f"{item['title']} by {item['artist']}" for item in added)
    return (f'Requested {len(added)} of {count} songs: {titles}. '
            'These will prepare after the planned songs. Your music direction and taste scores are unchanged.')


# ---------------------------------------------------------------------------
# Criteria batches
# ---------------------------------------------------------------------------
def criteria(action):
    """Validate a catalog_request action into (years, genres, artist, description, count)."""
    if not isinstance(action, dict):
        raise ValueError('The catalog request was incomplete. Try again.')
    years = eras.coerce(action.get('years'))
    if years is None and isinstance(action.get('description'), str):
        years = eras.parse(action['description'])
    genres = action.get('genres') or []
    if not isinstance(genres, list) or len(genres) > 5 or any(not isinstance(g, str) for g in genres):
        raise ValueError('The genre list was invalid. Name up to five genres.')
    from .intent import clean
    genres = [clean(g)[:50] for g in genres if clean(g)]
    artist = action.get('artist')
    if artist is not None and (not isinstance(artist, str) or len(artist.strip()) > 120):
        raise ValueError('Name one artist, or leave the artist out.')
    artist = clean(artist or '') or None
    description = clean(action.get('description') or '')[:240] if isinstance(action.get('description'), str) else ''
    count = action.get('count', 5)
    if type(count) is not int or not 1 <= count <= 10:
        raise ValueError('Choose one to ten songs for a catalog request.')
    if not years and not genres and not artist:
        raise ValueError('Give me a year range, a genre or an artist so I can find real songs.')
    return years, genres, artist, description, count


def _local_candidates(years, genres, artist, recent_hours):
    from .compatibility import genre_fit
    sql, params = 'SELECT key,title,artist,album,genre,year,last_played,play_count,file FROM tracks WHERE blocked=0', []
    if years:
        sql += ' AND year BETWEEN ? AND ?'
        params += list(years)
    rows = [dict(row) for row in db.query(sql, params)]
    now = time.time()
    found = []
    for row in rows:
        if recent_hours and row['last_played'] and now - row['last_played'] < recent_hours * 3600:
            continue
        if artist and db.norm(db.primary_artist(row['artist'])) != db.norm(db.primary_artist(artist)):
            continue
        if genres and not any((genre_fit(row, {'genre': g}) or 0) >= .7 for g in genres):
            continue
        found.append(row)
    return found


# Spotify's genre filter uses its own spellings.
_SPOTIFY_GENRES = {'rnb': 'r&b', 'hip hop': 'hip-hop', 'hiphop': 'hip-hop', 'dnb': 'drum-and-bass',
                   'drum and bass': 'drum-and-bass', 'lofi': 'lo-fi', 'kpop': 'k-pop', 'jpop': 'j-pop',
                   'uk garage': 'uk-garage', 'pop punk': 'pop-punk', 'neo soul': 'neo-soul'}


def _remote_candidates(years, genres, artist):
    if not spotify.available():
        return [], None
    results, problem = [], None
    searches = [dict(genre=_SPOTIFY_GENRES.get(g.lower(), g)) for g in genres[:3]] or [{}]
    for search in searches:
        try:
            results += spotify.catalog(years=years, artist=artist, limit=20, **search)
        except ValueError as error:
            problem = str(error)
            break
    if artist:
        results = [r for r in results if db.norm(db.primary_artist(r['artist'])) == db.norm(db.primary_artist(artist))]
    if years:
        results = [r for r in results if r.get('year') and years[0] <= r['year'] <= years[1]]
    return results, problem


def resolve(years=None, genres=(), artist=None, protected_keys=(), rng=None):
    """Ordered, deduplicated candidates for a criteria batch, and any catalog problem."""
    rng = rng or random.Random()
    recent = float(config.station.get('selection.title_separation_hours', 5) or 0)
    local = _local_candidates(years, list(genres), artist, recent)
    remote, problem = _remote_candidates(years, list(genres), artist)
    avoid_clean = config.station.get('selection.avoid_clean_versions', True)
    # Popular catalog hits lead; the library's own rows keep their place too.
    rng.shuffle(local)
    remote.sort(key=lambda r: -(r.get('popularity') or 0))
    head = remote[:12]
    rng.shuffle(head)
    remote = head + remote[12:]
    if avoid_clean:
        remote.sort(key=lambda r: not r.get('explicit'))  # stable: explicit edition first
    merged = []
    for index in range(max(len(local), len(remote))):
        merged += local[index:index + 1] + remote[index:index + 1]
    merged.sort(key=_edition_order)
    if avoid_clean:
        merged = [m for m in merged if not versions.clean_track(m)] or merged
    seen = _protected(protected_keys)
    # One song per act first; relax only if the era is thin.
    first = _select(merged, set(seen), per_artist=None if artist else 1)
    extra = [m for m in _select(merged, set(seen), per_artist=None if artist else 2) if m not in first]
    return first + extra, problem


def catalog_request(action, protected_keys=(), rng=None):
    years, genres, artist, description, count = criteria(action)
    if not genres and description and not artist:
        # A mood alone cannot filter a catalog; its genre reading can.
        from . import vibe
        genres = vibe.fallback(description).get('genres', [])[:3]
    candidates, problem = resolve(years, genres, artist, protected_keys, rng)
    added = _commit(candidates, count)
    scope = ', '.join(part for part in (eras.label(years), ' / '.join(genres), f'by {artist}' if artist else '') if part)
    if not added:
        reason = (f' {problem}' if problem else
                  '' if spotify.available() else ' Only your library was searched; Spotify catalog search is not configured.')
        return (f'No new songs were added for {scope}. I could not find available matching recordings outside '
                f'the current queue, or the request queue is full.{reason} Your music direction is unchanged.')
    titles = '; '.join(f"{item['title']} by {item['artist']}"
                       + (f" ({item['year']})" if type(item.get('year')) is int else '') for item in added)
    return (f'Requested {len(added)} of {count} songs for {scope}: {titles}. '
            'These will prepare after the planned songs. Your music direction and taste scores are unchanged.')
