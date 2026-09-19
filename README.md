# Defalt

*named after the DJ. spelt wrong on purpose*

a DJ app for when i want to mix something myself, and a radio station for when
i just want to leave music on. two decks, a local library, and two hosts who
have way too much to say about what i'm listening to

the console is Rust with egui and cpal. the optional radio backend is Python.
it runs locally on Windows

## what works

Current release: **0.2.0**. Read the [patch notes](CHANGELOG.md),
[privacy policy](PRIVACY.md), [terms of use](TERMS.md), and
[copyright information](COPYRIGHT.md). The same pages open from the app footer
without leaving Console or Radio or stopping playback.

- Two decks with waveforms, cue points, tempo reset, pitch-preserving key lock,
  three-band EQ, filters, and stem controls.
- Automatic transitions with beat matching, bass handoffs, filter sweeps, and
  echo. The planner checks the overlap for competing vocals, bass, and quiet
  gaps. Timing and effects are configurable in **Mix settings**.
- Radio that uses those same decks. Load songs first and it plays them before
  continuing the station. Playback keeps going when the window is minimized.
- Transitions prepared ahead of time, with mix points marked on the decks.
  **Skip** jumps to just before the next prepared handoff.
- Song requests, YouTube video links, and Spotify catalog search. **Set vibe**
  keeps a mood or activity in mind until you change it.
- Two configurable hosts, song-specific jokes, optional meme references,
  speech ducking, and a transcript.
- Article requests: paste a news link or full text and the director writes a
  short, attributed host break. Source links stay with the transcript.

the hosts use a language model and text-to-speech. the deck controls don't
need either. Spotify is used for search and metadata; it isn't the playback
source

## run it

you'll need Python 3.11 or newer, a current Rust toolchain, the MSVC C++ build
tools, and `ffmpeg` / `ffprobe` on PATH

from the project folder:

```powershell
python -m venv .venv
.venv\Scripts\python.exe -m pip install -r requirements.txt
Copy-Item .env.example .env
```

put your own credentials in `.env`. OpenRouter is optional for host dialogue
and interpreting requests; without it the hosts use fallback lines. Spotify
search needs `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET`. Steam integration
needs `STEAM_API_KEY` and `STEAM_ID`

check the setup, import some music, then start the console:

```powershell
.venv\Scripts\python.exe -m radio.cli doctor
.venv\Scripts\python.exe -m radio.importer "E:\Music"
cargo run --release
```

choose **Refresh** after importing. select a record and load it onto A or B.
**Shortcuts** opens the keyboard reference; **Beat grid** and **Stems** reveal
their controls

open **Radio** to start the station. you can load one or two records before
going on air, or let it choose the opening tracks

to build the app and create or update its desktop shortcut:

```powershell
powershell -ExecutionPolicy Bypass -File tools\ship.ps1
```

close Defalt before replacing its executable

## radio and requests

**Request** is for songs, artists, topics, or a one-off change. **Set vibe** is
for something you want it to stick with, like "studying, calm and jazzy" or
"cooking dinner, upbeat funk"

YouTube links keep the exact video you selected. if music metadata is missing,
it uses the title and channel, then labelled estimates from the director when
needed. Spotify suggestions carry the selected song's title, credits, and
duration into the request

the station considers genre, artist, musical compatibility, and available
lyric text when choosing songs. those are preferences, so it can still change
direction. requests take priority over automatic picks

it now looks three songs ahead by default, adjustable from one to four. try
**wave** under energy direction if you want it to build for a few songs, then
ease back. that uses measured loudness as a rough clue; it isn't a mood detector

- [Radio controls and mixing](docs/RADIO.md)
- [Requests, queue behavior, and API](docs/REQUESTS.md)
- [Transition presets](docs/TRANSITIONS.md)
- [Song and artist references](docs/MEME-REFERENCES.md)

there's also a browser radio interface:

```powershell
.venv\Scripts\python.exe -m radio
```

open the local address printed by the server. `.env.example` sets port `8090`

## make it yours

**Mix settings** covers transition timing, EQ, filters, echo, speech levels,
song choice, and host commentary. changes to a planned mix wait for future
transitions

`config/station.yaml` holds station defaults. `config/personas/` has the hosts'
personalities and voices. `config/news.yaml` and `config/games.yaml` control
news and Steam segments. local overrides live in `config/overrides.yaml`

the listening database, cached audio, and Obsidian vault stay outside version
control. the vault has track history, artist preferences, sessions, and host
transcripts. notes under a `## Notes` heading survive regeneration

read [NOTICE.md](NOTICE.md) for audio handling and what gets sent to external
services. this is a local, single-listener app; the server has no public-facing
authentication

## still rough

- Loops are unfinished.
- Song structure is inferred from acoustic changes and available vocal stems.
  It doesn't reliably label verses, choruses, or drops.
- Lyric matching uses available embedded text, not a complete lyrics catalog.
- Native key lock preserves pitch; browser tempo changes still affect pitch.
- Host quality depends on the model and voice provider. Failed requests can
  mean fallback dialogue.
- Longer set planning and continuous audio feedback need more work.

## development

Python schedules the station and prepares audio. Rust loads the decks, follows
the schedule, and processes audio on the device callback. Background scheduling
runs separately from window drawing

```powershell
.venv\Scripts\python.exe -m unittest discover -s tests -t .
cargo test --bin defalt -- --skip soundcheck
node tests/test_browser_rates.js
node tests/test_browser_skip.js
node tests/test_browser_vibe.js
node tests/test_browser_spotify.js
```

Python audio tests need FFmpeg. soundcheck tests use an output device, so the
command above skips them. Node is only needed for the browser tests

third-party audio code and its licenses are listed in [vendor/README.md](vendor/README.md)

---
Defalt v0.2.0 · © 2026 Zachary Parker
