# Patch notes

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
Defalt v0.2.0 · © 2026 Zachary Parker
