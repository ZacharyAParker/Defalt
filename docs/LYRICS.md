# Lyrics

Defalt pulls time-synced lyrics from [LRCLIB](https://lrclib.net), a free,
open lyrics catalogue, and uses them for three things: the line being sung on
screen, verse/chorus bands on the waveforms, and better mix points. No speech
recognition, no models, nothing heavy. If LRCLIB doesn't have a song, that
song just doesn't get any of this.

## How it works

- **Lookup.** When the radio prepares a record, it asks LRCLIB in the
  background with the artist, title, album and length (`/api/get`), and falls
  back to `/api/search`. An answer only counts if the artist and title match
  and the length is within 3 seconds, because a different edit has different
  timing. A slow backfill walks the rest of the library a few songs every few
  minutes. Never on a request, never more than one call a second, and it backs
  off when LRCLIB says slow down or falls over.
- **Misses are remembered.** A song LRCLIB doesn't know isn't asked about
  again for 14 days. A network error retries after 6 hours.
- **Synced, plain or instrumental.** Synced lyrics (LRC) get parsed into
  timed lines. A plain-text-only answer is kept but has no timing, so no
  sections. Songs LRCLIB marks instrumental are remembered as such.
- **Sections.** `radio/lyric_sections.py` groups the lines into blocks (at
  pauses and blank lines), finds the block the song keeps coming back to
  (chorus), the blocks it sings once (verses), and a late one-off between
  choruses (bridge). No singing before the first line or after the last is
  intro and outro, and a long gap in the middle is an instrumental break.
  Boundaries snap to the record's own eight-bar phrase or downbeat when one is
  within a bar.

## Where it shows up

- **Mixing.** The planner prefers to start leaving after the last chorus or in
  the outro, never mid-chorus, and prefers bringing a record in where the
  singing starts or on a section's downbeat. The synced lines are also the
  vocal evidence for "never stack two singers" and for where echoes go.
  Tracks without lyrics mix exactly as before. `transitions.section_weight`
  (Mix settings) sets how much it cares, 0 turns it off.
- **Hosts.** A break over an intro is timed to end just before the first sung
  line ("talking up to the post"). Now and then (`hosts.lyric_quote_chance`,
  8% by default) a host may quote ONE short line, ten words at most, credited.
  Anything more than that one line gets the break thrown out.
- **Console.** Section bands along the bottom of each deck's overview (chorus
  amber, verse blue, bridge violet, breaks teal, intro/outro grey, hover for a
  key), and the current line under the deck title. **Lyrics** in the toolbar
  hides the line.
- **Radio view and browser.** The line being sung under now playing, the next
  one dim under it. It follows what you hear, including the stream delay in
  stream mode.
- **API.** `GET /api/lyrics/<track key>` returns the lines and sections, same
  protection as everything else.

## Limits

- Only as good as LRCLIB's timing. Most synced lyrics are within half a
  second, some aren't.
- Ad-libs and backing vocals usually aren't in the lyrics, so "not singing"
  between lines is treated as quiet, not silent.
- Verse/chorus comes from repeated words. A song with no repeated block gets
  verses and no chorus.
- Director chat can't "skip to the chorus" yet.

## Privacy

LRCLIB gets the artist, title, album and length of the songs it's asked about,
plus the usual network stuff (your IP). Lyrics stay in the local database and
are only ever shown to you: they're somebody else's work, so don't share
them. Switch **Synced lyrics from LRCLIB** off in Mix settings, or set
`lyrics.enabled: false`, to stop lookups. The `lyrics` column on tracks is
still only the text embedded in your own files.
