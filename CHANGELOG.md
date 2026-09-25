# Patch notes

## 0.4.4 — September 25, 2026

- added the legal stuff. there's a real LICENSE now (source available, all rights reserved), proper terms with the no warranty and liability parts, a security policy and a generated list of every third party package and font. License and Notices sit in the footer next to Terms, and the first time you open the app (or when the terms change) it asks you to accept them and warns about the flashing lights and volume before anything plays

## 0.4.3 — September 25, 2026

- songs stop playing the clean version when there's an explicit one. it checks youtube music's explicit tag now since the clean upload has the exact same title, and songs already downloaded clean get swapped in the background a couple a minute (`python -m radio.cli editions --dry-run` shows which ones)

## 0.4.2 — September 23, 2026

- korean, japanese, chinese, thai, russian and other non english lyrics and titles actually show up in the console now instead of empty boxes

## 0.4.1 — September 23, 2026

- the console got a djay style glow up. graphite panels, deck A is cyan and deck B is violet everywhere, real fonts, turntables that actually look like turntables, a green play and an amber CUE, and cue pads that match their markers on the waveform
- channel meters next to every volume fader and a master meter up top. VOL and FILTER have real scales now and show the value while you drag
- the library folds away. drag the divider or hit **Ctrl+L** and the decks get the room
- synced lyrics from LRCLIB. the current line shows under the deck and on the radio, the waveforms get verse/chorus bands, and the mixes stop cutting choruses in half. the hosts talk over intros and stop right before the singing starts
- the booth actually moves now. real mouth shapes and lip sync, blinks and glances, breathing, a cat that does stuff (yawns, grooms, stretches, perks up when you click it), rain that runs down the glass, city windows that flicker, steam off the mugs and an ON AIR sign that flickers on when you go live. there's a lightning toggle too
- the radio view got cleaned up. **Go on air** is the one button that matters, the controls are grouped, and the visualizer glows with the bass now
- notices don't squeeze into a skinny column anymore
- the browser booth runs at 30 fps and the app got about 4 MB smaller

## 0.4.0 — September 23, 2026

- Transitions got creative. Eleven new techniques join the six blends: echo out, loop rolls (4 → 2 → 1 → ½ beat), brake, spinback, echo freeze, reverb wash, stem swap, a cappella intro, filter ride, silence punch and drop swap. A selector chooses from the energy change, tempo gap, key fit, genre and time of day, avoids repeating the last three, and never stacks two vocals or puts effects under host speech. **Creativity** in Mix settings runs from smooth radio to show-off DJ; each technique can be switched off or pinned. Up next shows the technique, and the hosts occasionally mention a flashy one.
- Mixes land on 8-bar phrases. Downbeats, song sections, key detection and a new energy measure replace loudness guesses; pairs that clash can be pitched a little into a compatible key when key lock is off.
- The director understands eras. "Queue songs from 2010-2015", "play some 90s R&B" and "early 2000s pop punk" queue real catalog recordings; "keep it 80s" steers automatic picks. Missing years, albums and genres are filled in from Spotify in the background.
- The native engine plays every transition itself, sample-accurate even while minimized. New: loops and auto-loops, quantize, phase sync, a master limiter with a meter, reverb and a send/return echo whose tail rings out, and an FX rack. The EQ is now a true three-band isolator, and play, pause, cue and seek no longer click.
- Remote listening. With a Cloudflare Tunnel and Access configured, the console's own mix streams to `/listen`, and the radio page installs to a phone's home screen with lock-screen controls. The tunnel runs only while the radio is on, and every remote request must carry a valid Access token. See [Remote listening](docs/REMOTE.md).
- **Report a bug** (F8, or the footer) saves a report with the station context, a screenshot and the logs around that moment to `reports/`.
- The hosts know the day and time, keep a memory of running bits across restarts, trim long drafts instead of discarding them, and render voice lines in parallel with a consistent fallback voice. Optional Open-Meteo weather is off until you set a location.
- Reliability: audio recovers from device changes and high sample rates, one failed request no longer takes the radio off air, background processes stop with the console, the station restarts itself if it crashes, and queued songs are no longer cleaned out of the cache before they play.
- Other websites can no longer control the local server. The console and browser follow live updates over one event stream instead of constant polling, and both idle properly.
- The browser player keeps less audio in memory, corrects clock drift, uses fonts bundled with the app, and loads the studio art in about 0.4 MB instead of 10 MB.
- Run `python -m radio.cli reanalyse` once to give existing songs the new key, energy, phrase and similarity data.

## 0.3.9 — September 20, 2026

- Mav, Rue, Director briefs, and fictional ads now use the updated humor preferences: short shared bits, specific requested roasts, and occasional deliberately bad wordplay. Each host keeps their own personality. Private anecdotes stay private, factual claims still need source material, and stock contrast punchlines remain excluded.
- Fixed saved settings masking the rest of a configuration group. For example, saving the personal-comment toggle no longer drops the station-wide humor guidance from the writer's brief. Explicit overrides still win, while unchanged settings continue to use the current defaults.

## 0.3.8 — September 20, 2026

- Radio now discovers unfamiliar songs related to your favorites and current music direction. Suggestions are verified against the Spotify catalog in the background, with new artists and familiar artists' deep cuts mixed into the automatic pool.
- Added an adjustable unfamiliar-music share, defaulting to roughly one in three eligible automatic picks before strong vibe preferences. Library size no longer buries new discoveries. Requests and repeat guards retain priority; discovery does not manufacture listener requests or taste-score boosts.
- Added free, dated Apple Music country charts with an iTunes fallback, plus a public-page collector for Spotify daily charts via Kworb. Chart influence and country are configurable. Stale entries lose their influence; the actual source, date and rank stay attached to selection evidence. No paid chart API is required. TikTok trends remain pending an accessible source.
- Improved explicit-edition selection before accepting unlabelled sources. Clean-version checks now include recording and album metadata and apply to cached recordings even when other source preferences are off. Legacy ambiguous cache entries get a bounded recheck; failed probes preserve working audio with a retry cooldown. Exact YouTube links and explicit requests for clean editions retain their requested source.

## 0.3.7 — September 19, 2026

- Long ad briefs get one bounded shortening pass. The writer selects a setup and the strongest requested jokes, with room reserved for the unsponsored close. Failed custom ads report the failure instead of playing an unrelated pitch.
- Fixed repeated backup ads across different products. Backup sketches rotate across the station, repeated payoffs are checked, and preparation status identifies backup copy.
- Song commentary rotates its focus instead of favoring listening statistics every time. History jokes have an eight-break gap by default. Repeated lines fall back to a clean introduction.
- Optional song-background lookup supplies matched Wikipedia introductions for song facts. Reviewed memes remain available; unsupported trivia and claims that an old meme is currently trending are excluded from the writing brief. Both lookup and the history-joke gap are configurable in Mix settings.
- Director requests such as "give me some Laufey songs" now queue three catalog recordings by that artist, with one to five available on request. Artist batches preserve the music direction, avoid already requested recordings, and report exactly which songs were added.
- Stock "that's not X, that's Y" jokes remain rejected, including exchanges split between hosts. Plain unsponsored disclosures are allowed.

## 0.3.6 — September 19, 2026

- The Radio booth now uses the approved Mav and Rue artwork in the app and browser, with separate host and headphone layers, restored microphones, and fixed foreground mugs.
- Speaking mouths follow each host's audio independently, including overlapping speech. Subtle breathing, blinks, rain, city lights, and occasional cat routines bring the booth to life. The cat keeps its sleeping Zs; Reduced motion keeps the scene still while preserving speaking indicators.
- The new booth fits the existing layout at 1080p and 1440p. Artwork and animation remain separate from audio playback.
- The desktop shortcut uses the supplied gold record icon. Future installs retain that icon.

## 0.3.5 — September 19, 2026

- Ad briefs support fictional pitches about real products, games, DLC, patches, streaming and esports. Sarcastic sales pitches are one optional style; host personalities and short segment budgets take priority. Article and news reactions can use the same bite when appropriate, with factual claims tied to source material.
- Director chat can commission ads with a requested premise and humor direction. Ads prepare for the next host break, or the next safe opening when requested immediately. Recent-news requests use source material and report failures instead of substituting an unrelated stock ad.
- Long director drafts scroll inside the composer, keeping Send accessible. The native app, browser, and backend accept up to 12,000 characters; oversized drafts remain intact with a visible limit notice.
- Saying "go back to normal" resumes usual taste-based suggestions, bypassing music direction and Set vibe for the session. Only unprepared automatic picks refresh. Saved directions, explicit requests, prepared mixes, and talk settings remain intact. Save music direction makes the reset permanent; Undo restores the previous direction.

## 0.3.4 — September 19, 2026

- Added Director chat to Radio in the app and browser. Steer upcoming automatic picks with conversational follow-ups, ask about the current song choice, request a recording, or reduce automatic host breaks for a while.
- Chat directions apply to the current session unless **Save music direction** is selected. Messages stay off air unless **Send this to hosts** is selected. Undo restores the previous music direction or talk setting; already prepared songs, transitions, speech, and explicit requests retain their places.
- Added an explicit director playbook for supported controls, current playback context, follow-ups, and honest action receipts. Changing Set vibe supersedes private music direction. Retrying a chat submission cannot queue the same message twice.
- If the session writer returns prose where structured output is required, it gets one fresh format retry within the original deadline before provider fallback.
- Defalt now launches maximized. Spectrum bars and panel height scale with the Radio view, with layouts checked at 1080p and 1440p. The complete studio and footer remain visible.
- Removed the headline-only news fallback and its "whole story" exchange. Thin feed summaries can fetch article context with a deadline; stories without enough detail stay off air. When generation fails, a short attributed read uses complete source sentences. News requests use the same context checks.

## 0.3.3 — September 18, 2026

- Added an optional Codex writing backend with Luna medium for host dialogue and low reasoning for short request and metadata tasks.
- The hosts can use reviewed facts from a dedicated Obsidian vault. Only relevant, broadcast-enabled notes are included; source documents stay out of prompts.
- Dialogue sessions remember recent exchanges, reset when memory or personas change, and keep current requests separate from old conversation history.
- OpenRouter takes over if the session is unavailable, busy, late, or returns unusable output. Existing speech synthesis and audio scheduling stay separate from text generation.
- Added total spoken-word limits so host exchanges fit their segment budgets more closely.
- Session helpers run without a console window. Backend status reports which provider answered and whether fallback was needed.
- Ad copy now remembers recent scripts across restarts, rotates sketch premises, and replaces near-duplicate reads. Each fictional house product has six fallback sketches when generation is unavailable.
- Added a saved Audio visualizer toggle to Radio. The spectrum follows the local audio output with warm-to-cool frequency bars, smooth decay, and peak markers. It fits beneath the studio and respects Reduced motion.
- Added **Ignore skips for taste and host banter** under Mix settings > Song choice and variety. When enabled, future skips do not change taste scores or skip counts, and new host scripts omit skip history and skip-based roasts. Existing learned scores and already prepared speech are retained.

## 0.3.2 — September 18, 2026

- Fixed transitions collapsing into near-instant cuts when a song starts with vocals. Selected styles retain their overlap; automatic blends default to a three-second minimum, subject to available audio and song-length limits.
- Skip now leads into the mix by ten seconds, configurable from two to thirty. It includes the complete host exchange around the handoff and waits for active speech to finish.
- Native deck playback now updates listening history. Browser and native reports for the same airing count once, so song cooldowns work across both players.
- Automatic rotation excludes duplicate recordings under different catalogue entries. Artist spacing relaxes before song cooldowns; an exhausted small library returns to its longest-rested songs. Explicit requests still take priority.
- Normal searches and cached-source checks avoid music videos and labelled gameplay clips. Exact YouTube requests remain available. Added repeat and source preferences to Mix settings.

## 0.3.1 — September 18, 2026

- Added **Next host break** and **Play now** ad controls to Radio in the app and browser, with writing, voice preparation, queued, and on-air status.
- Ads now have fictional house products when Steam or a game watchlist cannot provide material. An empty source no longer silently becomes ordinary banter labeled as an ad.
- Added Gen Z and TikTok sketch directions, product-specific fallback jokes, and a short speech budget. Real-game claims stay tied to source material; the reads are unsponsored comedy.
- Forced ads preserve the song timeline, wait for existing host speech, and duck both decks. Browser gain automation updates when an ad is added during playback.
- Song searches prefer original recordings and check source descriptions for hidden live or acoustic versions. Cached recordings are checked too. Explicit edition requests and exact YouTube links still work.
- Each play now records whether the listener or station chose it. Hosts own automatic picks; old requests no longer count as current requests. Queue labels show **YOU** or **AUTO**.
- Requests leave the queue when played, removed, or replaced. Two requests for the same song are tracked separately.
- Both hosts use occasional Gen Z and TikTok humor across their dialogue. Stock “that's not X, that's Y” punchlines are rejected.
- Radio's studio now fits the window height, keeping the full scene, current record, and live transcript visible together. Only transcript history scrolls.
- Windows background helpers no longer open command prompt windows when starting Radio, downloading a song, separating stems, or opening the browser.

## 0.3.0 — September 18, 2026

### A room for the radio

- Added the cozy studio to Radio in the app and browser. Mav and Rue's mouths follow their own speech, including pauses and overlapping lines.
- Rain falls behind the hosts, city lights pulse gently, and reflections move on the water. Each effect can be switched off; reduced motion keeps the scene still.
- The cat keeps breathing and has floating Z's while asleep. Other antics happen between 90–180-second naps. Clicking the cat gets a greeting.
- The current record appears on a vinyl disc. Artwork uses a matching Spotify album cover, then the resolved YouTube video's thumbnail if Spotify is unavailable or has no reliable match. Missing images use a plain record label.
- The native rundown now includes upcoming host breaks, news, and ads alongside records. The transcript, requests, mix settings, and queue controls remain available.

### Host voices

- Mav and Rue now use Gemini 3.1 Flash TTS through OpenRouter, with separate voices and delivery instructions.
- The original Andrew and Ava voices remain the fallback. Failed renders cannot overwrite Gemini's cache entries.
- Speech output is validated, converted for playback, and levelled before airing.

Artwork is fetched and cached in the background. Studio animation does not control or interrupt playback.

## 0.2.0 — September 18, 2026

### Mixing and playback

- Radio plays preloaded decks first, prepares their transition, and keeps refilling the decks.
- Transitions prepare further ahead. Skip moves a few seconds before the planned handoff, and deck waveforms show MIX IN and MIX OUT points.
- Automatic cue selection can enter or leave within a song when the fit is better, with configurable limits on skipped intros and how much of the outgoing track must play. Manual cues stay in control.
- Mixing considers beat confidence, acoustic boundaries, energy, bass, and available vocal activity. Overlap scoring compares transition lengths and reduces clashes or energy dips.
- Added configurable bass swaps, vocal-aware mid EQ, selective echo, tempo matching, native key lock, tempo recovery, drift correction, and a tempo reset button.
- Added Clean radio, Smooth DJ, and Expressive club profiles, plus individual controls for timing, EQ, FX, and risk weights.
- Bounded lookahead considers future track pairings. Energy direction can follow, build, ease, surprise, or move in waves.
- Background scheduling keeps running when the native window loses focus or is minimized.
- Smoothed live EQ, filter, and gain changes to remove switching clicks. Improved speech ducking, voice levels, and recovery after host lines.
- Fixed toolbar clicks restoring a maximized window and pitch bends remaining held after focus changes.

### Requests and song choice

- Added persistent vibe and activity suggestions. Explicit song requests keep priority while the station adjusts future automatic picks.
- Added Spotify search in Radio and exact YouTube link requests, with metadata lookup and director fallback when source details are incomplete.
- Genre, artist, available lyric text, mix compatibility, repetition, and exploration now contribute to selection without making compatibility a hard gate.
- Song matching prefers original or explicit recordings over labeled clean edits when suitable sources are available.
- Added download deadlines and recovery so a stuck custom fetch cannot block requests indefinitely.
- Fixed requests such as family ties getting stuck on an unavailable upload. Normal song requests can try up to three matching sources and avoid failed sources for 24 hours. Exact YouTube links stay pinned to the requested video.
- Failed requests remain visible with an explanation and can be dismissed or requested again.

### Hosts and articles

- Added song- and artist-specific jokes using listening history, configurable roast intensity, and repeat avoidance.
- Added a reviewed meme reference list with quote, frequency, and reuse limits.
- Added a host transcript with timestamps, follow mode, and copy controls.
- Added an Article request mode for pasted text or a public article URL. The director writes a source-attributed script and flags suspicious dates or weak source material.
- Article preparation runs in the background, supports cancellation, and shows source references in the transcript.

### App and project

- Cleaned up deck controls, navigation labels, scrolling, and request feedback.
- Added atomic settings updates and expanded playback, mixing, request, article, and browser regression coverage.
- Published a clean repository snapshot and expanded setup, Radio, and request documentation.
- Added a shared version and copyright footer, plus Patches, Privacy, Terms, and Copyright windows in the native app and browser player. Reading them leaves playback running.
- Added repository patch notes, privacy policy, terms of use, and copyright information. This release is version 0.2.0.

### Still being worked on

- Structure detection infers acoustic changes; it does not reliably label verses, choruses, or drops.
- Lyric matching depends on available embedded text. Browser tempo changes still affect pitch.
- Loops, longer set planning, and continuous audio feedback need more work.

## Earlier builds

Earlier development builds used version 0.1.0. The 0.2.0 notes collect the current release changes; they are not a claim that every feature was first written on the release date.

---
Defalt v0.4.4 · © 2026 Zachary Parker
