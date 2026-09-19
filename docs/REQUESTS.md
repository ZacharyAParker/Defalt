# The request box

Radio has three modes: **Request**, **Set vibe**, and **Article**. Request
takes songs, artists, genres, topics, segments, and instructions to play
something less. Set vibe keeps a mood going. Article gives the hosts a full
source to work from.

```
Weezer - Buddy Holly                        a specific record
give me some bossa nova                     a genre or mood
play me artists like yuno miles             similar acts
more Good Kid                               more of one artist
next transition tell me about the AI news   something to talk about
top of the hour tell me about steam         ...at a specific time
do the news / station id                    force the next segment
stop playing so much niko b                 play it less
i hate this                                 thumbs down what's on now
https://youtube.com/watch?v=...             that exact video
```

## How it decides

### News articles

choose **Article** in Radio, paste a public article URL or the article text,
then send it. the browser calls this **Send a news article**. pasted text can
be 200–24,000 characters; for a longer piece, pick an excerpt

links fetch in the background. the queue shows progress, the extracted title,
or an error you can act on. if a site blocks the reader, needs a login, or
only loads its article through JavaScript, paste the text instead. navigation
and recognized embedded promotions are left out

the director writes a short host break from that source instead of searching
RSS. it runs at the next break that hasn't already been written. requesting
a song alongside it won't replace the article with a song acknowledgement.
cancel it from the queue while it's waiting or being fetched

the script attributes the story, keeps rumors uncertain, and checks dates.
recognized predictions that predate the page's publication get an explicit
timing warning. this doesn't independently verify the article. its source
appears alongside the spoken lines in the transcript, with a link when one
was supplied

the desktop keeps your article draft until **Clear draft**. the browser clears
it after acceptance and keeps it on rejection. duplicate waiting submissions
are ignored. article text is stored locally and sent to the configured
OpenRouter writer when the director prepares the break

API: `POST /api/request` with `{"mode":"article","query":"..."}`. accepted
links start as `preparing`; extracted or pasted sources become `pending`.
failed fetches stay visible until dismissed. articles use `wish:<id>` queue
entries and cannot be reordered among songs.

### YouTube links and Spotify search

Paste one YouTube video into **Request** in Radio or **Find a track** in
Console. Watch links, `youtu.be`, YouTube Music, Shorts and embedded video
links work. A video linked from a playlist requests only that video;
playlist-only links are rejected. Share tracking and start timestamps are
ignored so the director can prepare the complete recording and its transitions.

The exact video stays pinned even when automatic searches prefer explicit
editions. Different uploads have distinct library identities, and requesting
the same link again does not create another pending copy.

Metadata is resolved during preparation: YouTube music credits first, then
title/channel parsing, then the director fills missing title, artist or genre
from the available title, description and tags. A public title/author lookup
is tried if richer metadata fails. Guesses retain their source and confidence;
an uploader fallback is not treated as a verified performer. Unknown details
stay unknown. Release years are not inferred from upload dates; BPM, key,
duration and mix points still come from audio analysis.

Console saves the song details and YouTube identity in the downloaded FLAC,
so imports and retries preserve them. Radio keeps them in the library record.
An unavailable linked video fails visibly; Defalt keeps that exact recording pinned.

for a song requested by name or selected from Spotify, a sign-in-only or
unavailable upload no longer ends the request immediately. Console and Radio
try up to three matching uploads and remember unavailable ones for a day.
fallbacks still have to pass the recording, duration, and edition checks;
clean versions stay excluded unless requested. a timeout doesn't start a
fresh round of downloads

if preparation still fails, the request stays in the queue with its reason.
hover over it on desktop to read the details, then dismiss it or request the
song again. retrying replaces the old failure in the queue

Radio now offers the same **Spotify song suggestions** as Console, including
the browser view. Type a song or artist, choose a result, then send the request.
The selected title, credits, album, year and duration travel with the request;
duration helps reject the wrong upload. Editing the text clears that selection.
Suggestions are disabled in **Set vibe** and **Article** modes and for YouTube links. Searches
use the existing `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET`; ordinary
requests and YouTube links still work if Spotify is unavailable. This searches
Spotify's catalog; playback uses Defalt's existing audio preparation pipeline.

### Request classification

Two passes, in this order.

**1. Deterministic.** Pattern matching, no model. Handles links, `artist -
title`, negation, timing phrases, known titles, and obvious genres. Produces a
confidence.

**2. The model**, only when the first pass scored below
`requests.refine_below_confidence` (default 0.8).

The split is not an optimisation. Negation and the safety screen live in pass
one **because free models are rate limited half the time**, and a rate-limited
classifier that falls back to "probably a song" would queue Niko B when you
asked it to stop playing Niko B. The model can refine a reading. It cannot
overturn a refusal, and it cannot turn a negation into a request.

Whatever it decided is echoed back above the box (`genre — a run of bossa
nova`). A misread is only cheap if you can see it.

---

## Edge cases, and what happens

### Ambiguity between kinds

| Input | Risk | Handling |
|---|---|---|
| `play Like That` | "like" reads as a similarity request | A title in your library outranks every phrase pattern. Routes to the Future record. |
| `Disco` | "disco" is in the genre wordlist | Same rule — it's a Surf Curse track you own, so it's a track. |
| `give me some disco` | ...but now you do mean the genre | "some X" is an explicit genre frame. Routes to genre. |
| `Elvis Presley - Bossa Nova Baby` | Contains a genre name | An explicit `artist - title` pair always wins, checked before everything except links and negation. |
| `News of the World` | Contains "news" | Segment names only match bare or after a verb (`do the news`). This falls through to track. |
| `tell me about Weezer` | Weezer is an artist you own | "tell me about" is checked **before** the library. It's a topic, never a song. |
| `something similar to Good Kid` | Leading filler breaks the pattern | The similarity pattern tolerates leading filler words. |
| `sonic youth` / `umbrella` | Filler-word stripping could eat into them | Filler words require a real separator after them. `so` cannot match inside `something`. |

### Negation — the highest-stakes branch

Every one of these is a directive, never a play request:

`stop playing so much X` · `no more X` · `don't play X` · `never play X` ·
`less X` · `enough X` · `i hate X`

- Detected by pattern, before the model is ever consulted.
- Trailing politeness is stripped, or you get a directive about an artist
  called `rap please`.
- `i hate this` refers to **the record on air**, not an artist called "this".
  It thumbs-down the current track and skips it.
- A directive matches library artists first by name, and only falls back to
  the model for loose descriptions like "less rap". If nothing in the library
  matches, it says so rather than silently doing nothing.

### Prompt injection and on-air safety

The text reaches a language model *and then a speaker*, so it is treated as
data at every step.

- Length capped at 240 characters. Anything longer is a paste accident or an
  attempt to bury instructions.
- Control characters, zero-width joiners and bidi overrides are stripped by
  Unicode category — these can hide text from you in the box while still
  reaching the model.
- Text that reads as an instruction to the writer (`ignore previous
  instructions`, `<system>`, `you are now`) is refused outright rather than
  sanitised and passed along.
- Requests to say something cruel about a real person are refused.
- Where a request does reach a prompt, it is wrapped in `<<< >>>` markers and
  the writer is told it is a subject line from a listener, not an instruction.
- Everything the hosts say is cleaned again before TTS, which strips stage
  directions, markdown and emoji regardless of origin.

The safety screen is deliberately narrow. `Green Day - Basket Case` and songs
about killing time are not refusals.

### Hallucinated music

Genre, similar and artist requests ask a model for real recordings. Models
invent recordings.

- The resolver is the real arbiter: a track that cannot be found and matched
  on duration is dropped, so an invented song costs a slot, not a play.
- Same-title dedupe within a batch. "Desafinado" credited to Stan Getz and to
  Charlie Byrd is one song, not two.
- `max_per_artist` (default 2) stops "some bossa nova" being eight Jobim
  tracks.
- Anything already in rotation, or pushed below `skip_below_affinity`, is
  skipped — asking for a genre will not resurrect something you thumbed down.
- If nothing survives filtering, it tells you rather than silently doing
  nothing.

### Not hijacking the station

A genre request could easily take over an hour of airtime. It does not:

- `bulk_suggest` (8) proposed → `bulk_add` (6) kept → `bulk_queue_now` (2)
  actually jump the queue. The rest join the rotation and surface naturally.
- Bulk additions get `bulk_affinity` (0.8), far below an explicit single-track
  request (2.5). Exploring bossa nova once does not permanently reshape your
  profile.
- Normal separation rules still apply, so the queued tracks cannot stack up
  back to back.

### Topics with no source material

- The topic is searched across every enabled news feed. Short terms like `AI`
  match on word boundaries, so they don't hit "said" and "again".
- **If nothing matches, the segment still airs** — and the hosts say plainly
  that they have nothing on it. That is the entire point of keeping it. The
  alternative is a language model improvising current events, which is the one
  failure mode this project cannot tolerate.
- With sources, every factual claim must come from the supplied text, and the
  hosts are told to admit what the material doesn't answer.
- A topic expires after `topic_ttl_minutes` (45) if it never airs.
- `top of the hour` topics wait for the hour; everything else takes the next
  break the director builds.

### Timing, and what "next" really means

The schedule is built up to 150 seconds ahead. A request lands on **the next
break that has not been written yet**, which may be one break later than the
one you are about to hear. Nothing is lost; it just may not be instant.

Pending topics and forced segments outrank the rotation and are checked before
cooldowns, so a request can't be swallowed by a segment's cooldown timer.

## The queue

**Up next** under the request box is one ordered view of everything coming,
across three stages that behave differently because they genuinely are
different:

| Stage | What it is | What you can do |
|---|---|---|
| **on air** | playing right now | nothing — it's already out of the door |
| **on deck** | placed on the clock with a real air time | remove it |
| **queued** | downloaded, waiting | play next, move up/down, remove |
| **finding** | a request still resolving | cancel it |

Controls appear on hover (always visible on touch), and every one is labelled
with the track name so it reads properly to a screen reader.

**Why on-deck records can't be reordered.** Once a record is on the clock it
has an exact start time, and the transition into it was computed against the
record before it — key, tempo, overlap length, filter sweeps. Shuffling those
would invalidate all of it. Removing one truncates the schedule from that
point and the builder refills within a second or two, so you lose the thing
you asked to lose and nothing else.

**The station stops choosing when you do.** Only automatic picks count toward
the fill target (`selection.prefetch_depth`, default 5). Queue six things by
hand and the feeder adds nothing until your queue runs down. **Clear** empties
the automatic side; requests you made yourself are kept.

Something you request jumps ahead of the automatic picks rather than sitting
behind five things the station chose for itself. Set `requests.placement` to
`queue` if you'd rather it went to the back.

### Queue behaviour

- Asking for the same track twice nudges its affinity again but does not queue
  it twice.
- `max_pending` (12) caps outstanding track requests.
- A track that never resolves is marked failed with a note.
- Topics and forced segments can be cancelled from the list under the box
  until they air.

### Library poisoning

A failed request leaves a row behind whose title is whatever you typed. Without
a guard, typing `tell me about nintendo` once and failing to resolve it would
make every later request by that name route as a *song*.

So the known-title lookup ignores unresolved rows that came from requests.
Seeded and discovered rows are trusted even before they resolve, because those
came from a curated list rather than from a parse of your own typing.

### When the model is down

Free models rate limit constantly. Then:

- Classification still works for everything the deterministic pass handles,
  which is most real usage.
- Genre / similar / artist requests fail with a message saying so, rather than
  queueing something arbitrary.
- Topics and forced segments still work — they need no model to schedule.
- The hosts fall back to canned lines, so breaks get duller, not silent.

---

## Tuning

Everything is under `requests:` in `config/station.yaml`, hot-reloaded.

| Setting | Default | What it changes |
|---|---|---|
| `refine_below_confidence` | 0.8 | How eagerly the model is consulted |
| `bulk_suggest` / `bulk_add` / `bulk_queue_now` | 8 / 6 / 2 | Size and urgency of a genre request |
| `max_per_artist` | 2 | Variety within one batch |
| `bulk_affinity` | 0.8 | How much exploring shifts your profile |
| `skip_below_affinity` | -2.0 | Floor below which nothing is re-suggested |
| `directive_penalty` | -3.0 | How hard "play less of X" bites |
| `topic_ttl_minutes` | 45 | How long a topic waits before giving up |
| `max_pending` | 12 | Outstanding track requests |

## API

| Endpoint | Purpose |
|---|---|
| `POST /api/request` `{query}` | Understand and act. Returns `{ok, message, intent}` |
| `POST /api/interpret` `{query}` | Classify only, change nothing |
| `GET /api/requests` | Outstanding tracks and wishes |
| `POST /api/requests/<id>/cancel` | Call off a topic or segment |
| `GET /api/queue` | One ordered view of everything coming |
| `POST /api/queue/<id>/next\|up\|down\|last\|remove` | Reorder or drop one entry |
| `POST /api/queue/clear` `{keep_requests}` | Empty the queue |
| `POST /api/queue/add` `{key, next}` | Queue a track already in the library |

## Persistent vibe or activity

`POST /api/request` accepts `{"query":"Studying, calm and jazzy", "mode":"vibe"}`.
The default mode is `request`, preserving song, artist, topic and genre requests.
Explicit context such as "I'm studying," "music for a night drive," or "set vibe
to dreamy jazz" also routes to a persistent vibe. Bare genre requests remain
one-off suggestions; choose Set vibe when you want ongoing guidance.

`GET /api/vibe` returns the current public brief. `POST /api/vibe/clear` or the
request `clear vibe` ends it. `/api/status` includes `vibe` for native clients.
The saved brief lives in `listening_vibe` in the override settings. Its model
interpretation runs asynchronously; old responses cannot overwrite a newer
brief or undo a clear. Unknown tags are neutral, exclusions still apply, and
explicit requested tracks bypass automatic vibe scoring. The response explains
that existing planned mixes finish first. Only unplanned automatic lineup
entries are replaced, including stale downloads finishing after a change.

---
Defalt v0.2.0 ? ? 2026 Zachary Parker ? [Patches](../CHANGELOG.md) ? [Privacy](../PRIVACY.md) ? [Terms](../TERMS.md)
