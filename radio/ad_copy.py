"""Rotate ad premises and reject repeated copy across manual and automatic breaks."""
import difflib
import json
import random
import re
import threading
import time

from . import db

_LOCK = threading.RLock()
ANGLES = [
    'A short sarcastic mock sales pitch built around one specific detail and a dry payoff.',
    'A suspiciously specific customer-service complaint.',
    'An influencer tries to hide how little they understand the product.',
    'A job interview for someone spectacularly unqualified.',
    'A comment-section argument that escapes into the studio.',
    'A dramatic apology for an extremely minor inconvenience.',
    'A product demonstration where the hosts become the test subjects.',
]
BITS = {
    'Queue Insurance': [
        ['The aux has been passed. Queue Insurance would like a word.', 'Your deductible is one normal song.', 'Can we pay in skips?'],
        ['Queue Insurance. I filed a claim for emotional damage from shuffle.', 'You selected shuffle.', 'Victim blaming from my own insurer.'],
        ['Queue Insurance has reviewed our application.', 'They heard one transition and put us on hold.', 'The hold music is better than our set.'],
        ['Queue Insurance. For the friend who says trust me before every song.', 'I asked for references.', 'They sent a playlist and blocked me.'],
        ['Queue Insurance. I listed the aux cord as a dependent.', 'It has been supporting you emotionally.', 'Finally, somebody recognizes unpaid labor.'],
        ['Queue Insurance. My premium went up after one song.', 'You appealed with a voice note.', 'It had a beat drop. I stand by the evidence.'],
    ],
    'Grass Touch Simulator': [
        ['Grass Touch Simulator. Going outside has graphics settings.', 'You lowered the grass quality.', 'For performance.'],
        ['Grass Touch Simulator. I finally logged off to go outside.', 'You opened another game.', 'The loading screen had a tree.'],
        ['Grass Touch Simulator. My character has achieved fresh air.', 'You have achieved chair.', 'We all progress at our own pace.'],
        ['Grass Touch Simulator. I made eye contact with a shrub.', 'How did the conversation go?', 'I am waiting for it to accept my friend request.'],
        ['Grass Touch Simulator. I put hiking on my resume.', 'You walked to the settings menu.', 'Uphill. The font was tiny.'],
        ['Grass Touch Simulator. My lawn has more social plans than me.', 'It goes outside every day.', 'Okay, nobody likes a show-off.'],
    ],
    'One More Song Alarm': [
        ['One More Song Alarm. Bedtime has a terms and conditions loophole.', 'Your sleep schedule is in early access.', 'The roadmap looks incredible.'],
        ['One More Song Alarm. I asked for five more minutes.', 'You submitted an album.', 'It is a very coherent application.'],
        ['One More Song Alarm. I have negotiated a bedtime.', 'With whom?', 'The ceiling. It has been very flexible.'],
        ['One More Song Alarm. My morning self left a complaint.', 'Did you read it?', 'Marked it as spam. Terrible energy.'],
        ['One More Song Alarm. I set a reminder to stop listening.', 'And then?', 'I gave the reminder a theme song.'],
        ['One More Song Alarm. I am practicing a consistent routine.', 'You say one more every night.', 'Consistency. Thank you.'],
    ],
}
CLOSES = ['Fictional product. No sponsors. Keep your money.',
          'Unsponsored. We invented the product and the problem.',
          'Nobody paid for this. That probably explains a lot.']
REAL_CLOSES = ['Unsponsored. Nobody paid for this bit.',
               'No sponsorship. Just us committing to the presentation.',
               'Nobody paid us. Please judge the sales pitch accordingly.']


def recent():
    rows = db.query("SELECT meta FROM events WHERE kind='ad_prepared' ORDER BY id DESC LIMIT 24")
    result = []
    for row in rows:
        try:
            item = json.loads(row['meta'])
            if isinstance(item, dict) and isinstance(item.get('lines'), list):
                result.append(item)
        except (ValueError, TypeError):
            continue
    return result


def plan(subject, styles):
    with _LOCK:
        history = recent()
        same = [h for h in history if h.get('product') == subject['name']]
        last = same[0] if same else {}
        style_pool = [str(s) for s in styles if str(s) != last.get('style')] or list(styles)
        angle_pool = [a for a in ANGLES if a != last.get('angle')]
        return {'style': random.choice(style_pool), 'angle': random.choice(angle_pool),
                'history': history}


def normalized(lines):
    # Branding and the unsponsored disclaimer must not hide repeated punchlines.
    body = ' '.join(str(line) for line in lines[:3])
    return ' '.join(re.findall(r'\w+', body.lower()))


def repeated(lines, history):
    text = normalized(lines)
    return any(difflib.SequenceMatcher(None, text, normalized(old['lines'])).ratio() >= .82
               for old in history[:18])


def fallback(subject, history):
    name = subject['name']
    options = BITS.get(name) if subject.get('fictional') else None
    if not options:
        options = [
            [f'{name}. I prepared a very professional sales pitch.', 'You wrote please in three fonts.', 'Typography is persuasion.'],
            [f'{name}. My presentation has a slide deck.', 'Every slide says hear me out.', 'The transitions are doing a lot of work.'],
            [f'{name}. I have appointed myself brand ambassador.', 'They do not know you exist.', 'A refreshingly hands-off partnership.'],
            [f'{name}. I rehearsed this in the mirror.', 'The mirror asked to unsubscribe.', 'It has always been a difficult audience.'],
            [f'{name}. My marketing strategy is mysterious.', 'You forgot to write one.', 'That is the mysterious part.'],
            [f'{name}. I brought supporting evidence.', 'That is a sticky note saying vibes.', 'Exhibit A.'],
        ]
    # Least recently used premise; stable rotation also works without an LLM.
    def distance(lines):
        return next((i for i, old in enumerate(history)
                     if normalized(lines) == normalized(old['lines'])), len(history) + 1)
    chosen = max(options, key=distance)
    closes = CLOSES if subject.get('fictional') else REAL_CLOSES
    last_close = history[0]['lines'][-1] if history and history[0]['lines'] else None
    close = closes[(closes.index(last_close) + 1) % len(closes)] if last_close in closes else closes[0]
    return list(chosen) + [close]


def finish(subject, proposal, lines, anchor, wildcard, *, strict=False):
    from .segments.base import Line
    with _LOCK:
        history = recent()
        if repeated([line.text for line in lines], history):
            if strict:
                raise ValueError('The requested ad repeated an earlier read. Nothing was scheduled; please retry.')
            lines = [Line(wildcard if i % 2 == 0 else anchor, text)
                     for i, text in enumerate(fallback(subject, history))]
        db.write('INSERT INTO events(ts,kind,meta) VALUES(?,?,?)',
                 (time.time(), 'ad_prepared', json.dumps({
                     'product': subject['name'], 'style': proposal['style'],
                     'angle': proposal['angle'], 'lines': [line.text for line in lines]})))
        return lines
