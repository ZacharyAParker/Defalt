"""Finite artist requests use catalog recordings, never a guessed mood."""
import re
import time

from . import db, spotify, versions


def detect(message):
    text = re.sub(r"^(?:no[,.]?\s+|please\s+)", "", message.strip(), flags=re.I)
    text = re.sub(r"\s+please[.!]?$", "", text, flags=re.I).rstrip(".!?")
    match = re.fullmatch(r"(?:give me|play|queue|add)(?:\s+some|\s+a few|\s+(?P<count>\d+|one|two|three|four|five))?\s+"
                         r"(?:songs (?:by|from) (?P<after>.+)|(?P<before>.+) songs)", text, re.I)
    if not match:
        return None
    artist = (match['after'] or match['before']).strip()
    if artist.casefold() in {'happy', 'sad', 'chill', 'romantic', 'upbeat', 'new', 'different', 'normal'}:
        return None
    raw_count = (match['count'] or '3').casefold()
    count = {'one': 1, 'two': 2, 'three': 3, 'four': 4, 'five': 5}.get(raw_count)
    return {'type': 'artist_request', 'artist': artist, 'count': count if count else int(raw_count)}


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
    candidates.sort(key=lambda item: (versions.clean_track(item), versions.alternate_track(item)))
    selected = []
    seen = set(protected_keys)
    for key in protected_keys:
        row = db.one('SELECT title,artist FROM tracks WHERE key=?', (key,))
        if row:
            seen.add(db.track_key(row['artist'], row['title']))
    for item in candidates:
        identity = db.track_key(item['artist'], item['title'])
        key = item.get('key') or identity
        if key in seen or identity in seen:
            continue
        seen.update((key, identity))
        selected.append(item)
    # Recheck capacity and duplicates together; another request may arrive during search.
    conn = db.connect()
    added = []
    with conn:
        conn.execute('BEGIN IMMEDIATE')
        room = max(0, 12 - conn.execute("SELECT COUNT(*) FROM requests WHERE status IN ('pending','preparing')").fetchone()[0])
        for item in selected:
            if len(added) >= min(count, room):
                break
            key = item.get('key') or db.track_key(item['artist'], item['title'])
            if conn.execute("SELECT 1 FROM requests WHERE track_key=? AND status IN ('pending','preparing','queued','scheduled')", (key,)).fetchone():
                continue
            if conn.execute('SELECT 1 FROM tracks WHERE key=? AND blocked=1', (key,)).fetchone():
                continue
            conn.execute('INSERT OR IGNORE INTO tracks(key,title,artist,source,expected_ms,added_at) VALUES(?,?,?,?,?,?)',
                         (key, item['title'], item['artist'], 'request', item.get('duration_ms') or 0, time.time()))
            conn.execute("INSERT INTO requests(ts,query,status,track_key) VALUES(?,?,'pending',?)",
                         (time.time(), f"{item['artist']} - {item['title']}", key))
            added.append(item)
    if not added:
        return (f'No new {artist} requests were added. I could not find an available matching recording '
                'outside the current queue, or the request queue is full. Name a song or use Spotify search. Your music direction is unchanged.')
    titles = '; '.join(f"{item['title']} by {item['artist']}" for item in added)
    return (f'Requested {len(added)} of {count} songs: {titles}. '
            'These will prepare after the planned songs. Your music direction and taste scores are unchanged.')
