# Patch notes

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
Defalt v0.3.2 · © 2026 Zachary Parker
