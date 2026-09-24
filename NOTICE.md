# Notice on audio, and what this thing actually does

Read this before you run it. It matters.

## What happens to audio here

Defalt does not ship with music or provide a public broadcast service. What it
does is:

1. pick a track it thinks you want to hear
2. find a matching source for it and download the audio to a local cache folder
3. normalise the level, measure where the song starts, play it back on this
   machine
4. delete the file when the cache budget or age limit says to

The cache lives in `cache/audio/` by default and is excluded from version
control. The local player serves cached media over its local HTTP interface.
Keep that interface on loopback; the app has no public-facing authentication.

## The rules this project holds itself to

**Local only.** The server binds to `127.0.0.1` by default. Do not change
`HOST` to put it on a public address. The one supported way to reach it from
elsewhere is the optional Cloudflare Tunnel in [Remote listening](docs/REMOTE.md),
which runs only while the radio is on and refuses every request that lacks a
valid Cloudflare Access token for your own application.

**Personal use only.** One listener: you. Remote listening streams the console's
mix to your own devices behind your own Access policy. There is no multi-user
mode, no sharing and no public stream. Do not give anyone else access to the
tunnel or the `/listen` address.

**Never redistributed.** Nothing in `cache/` should ever be uploaded, shared,
committed, published, or handed to another person. That is the whole line, and
it is not a soft one.

**Transient.** Set `cache.mode: ephemeral` in `config/station.yaml` and files are
deleted the moment they finish playing. The default (`lru`, 6 GB) keeps recent
tracks so replays and requests are instant. Either way it is a cache, not a
library, and `cache.purge_on_exit: true` will empty it on shutdown.

## On the "I have Spotify Premium" reasoning

A paid streaming subscription is a licence to stream from that service. It is
not a general licence to obtain the same recordings from somewhere else, and
downloading from a third-party source is very likely against that source's terms
of service regardless of what you pay Spotify.

I am not going to pretend otherwise, and this file is not legal advice. What I
will say plainly:

- Do not distribute anything this produces.
- Do not run it as a service for other people.
- If you want the safest version of this, put your own files in a folder and
  point the station at them instead. Everything else in the project works the
  same way.

The design deliberately makes the risky part small and replaceable:
`radio/library.py` is the only file that fetches audio. Swap it for a local-file
scanner and nothing else in the station needs to change.

## The fake adverts

The "ads" the hosts read are a comedy bit. Nothing is sponsored, nobody is paid,
and the station has no commercial relationship with any game, studio or
publisher. `config/games.yaml` requires a disclaimer line on every ad read for
exactly this reason. Leave `require_disclaimer: true` alone.

## News and patch notes

News segments summarise public RSS feeds and link back to the source. Patch
notes come from Steam's public news API for games on your own account. The hosts
are instructed to stay inside the supplied text and not invent facts, but they
are language models: if a segment says something that matters to you, go read
the original. Every story keeps its source URL.

## Third-party services

| Service | What it gets | Optional |
|---|---|---|
| OpenRouter | The segment brief: track titles, headlines, patch text, submitted articles | Yes — canned lines without it |
| Microsoft Edge TTS or configured OpenRouter speech provider | The written host lines, to synthesise speech | Provider is configurable |
| Steam Web API | Your SteamID, to read your library and wishlist | Yes |
| RSS and article publishers | Page/feed requests, IP address, and request headers | Yes |
| YouTube | Searches, metadata requests, and requests for selected audio | When fetching sources |
| Spotify | Search queries, catalog lookups for missing years and genres, and application credentials | Yes |
| LRCLIB | Artist, title, album and length of songs in your library, to find synced lyrics | Yes — `lyrics.enabled` / Mix settings |
| Open-Meteo | The latitude and longitude you configure, for weather | Yes — off until a location is set |
| Cloudflare | Tunnelled page, control and stream traffic, and Access sign-in | Yes — remote listening only |

The database and vault stay on this machine. When host dialogue or request
interpretation uses OpenRouter, the prompt can include song details, your vibe
brief, selected listening-history facts, and the full text of articles you
submit for a host break. Article links are fetched from their publisher.
Voice providers receive the lines they are asked to read.

The browser player bundles Archivo (© The Archivo Project Authors) and IBM Plex
Mono (© IBM Corp.), both under the SIL Open Font License 1.1; the licence texts
are in `web/static/fonts/`.

The [privacy policy](PRIVACY.md) covers storage, external providers, retention,
and deletion limits. See the [terms](TERMS.md), [copyright notice](COPYRIGHT.md),
and [patch notes](CHANGELOG.md) for the rest of the release information.

---
Defalt v0.4.2 · © 2026 Zachary Parker
