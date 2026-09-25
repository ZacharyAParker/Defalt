# radio

**Background playback.** Deck loading, radio scheduling, and station keep-alives
continue when Defalt is unfocused, covered, or minimized. These updates run in
the window framework's logic callback, independently of UI drawing; audio stays
on its own device callback.

**Start from your decks.** Load one or two tracks, then choose **Go on air**.
Radio starts through the console output automatically. Loaded tracks open the
station in order (a playing deck goes first; otherwise A then B), keeping their
cue positions. With two tracks loaded, it prepares their transition before
playback, then refills each freed deck from the station queue. The channel
faders, crossfader, EQ and filters perform the scheduled mix, including ducking
under speech. **Playing here** pauses console playback; **Stop** also stops
the station. Touching a mixer control holds that control until you return it
to autopilot.

Automatic transitions use analysis confidence, tempo/key compatibility and
the incoming intro. Reliable beat grids allow bounded tempo matching, beat
alignment and beat-group overlap lengths. Cached local energy/bass changes
supply alternative cue points and transition candidates. Existing vocal stems
provide vocal activity when available; unknown vocals remain unknown.
Deeper cues are enabled by default: a stronger measured section can replace
an opening or end-of-track handoff. The planner keeps at least **65% of the full
song before the mix-out starts**; skipping an opening reduces the remaining
exit budget. The opening skip limit defaults to **25%**. Both are adjustable
under **Musical timing and playback**. Preloaded manual cues stay locked, and
manually loaded short tails or explicit skips can bypass the automatic listening
minimum. Unanalysed tracks retain conventional timing. These are acoustic cues,
not verified chorus or drop labels.
Each deck's **RESET** button restores original tempo; **KEY** preserves pitch
while changing speed. Key lock is optional and native-console only.

The next transition is prepared as soon as the next track is ready, even
when the current song has several minutes left. **Minimum songs planned ahead**
controls how many future records are reserved (1-3). **Skip** waits for the
incoming console deck to finish loading, then jumps to four seconds before
the existing handoff; **Skip: seconds before the transition** adjusts that
lead-in (0-10 seconds). An active mix is allowed to finish. If preparation is
still running, the current record keeps playing.

Both deck overview waveforms and the scrolling beat view show **MIX IN** and
**MIX OUT** ranges, with lines at the start and end of each planned overlap.
Hover an overview for exact source timestamps. These marks follow the actual
schedule, including cue offsets and tempo recovery; they appear once the pair
is planned and are removed if that pair is dropped.

**Mix settings** in the radio view offers Clean radio, Smooth DJ and Expressive
club starting points, plus individual controls for timing, pitch limits, grid
confidence, EQ handoffs/depth, filter sweeps, and tempo-timed outgoing echo.
Optional tempo recovery gradually returns toward original speed after the
blend, with source positions and the next transition recalculated consistently.
Small playback drift is corrected without seeking a playing record; touching
tempo or transport takes ownership until **Back to auto** or the next record.
Changes apply to future transitions; speech ducking updates when settings are
applied. The defaults normalize host speech to -16 LUFS with a -1.5 dB peak
ceiling and lower music to 10% under speech, with a smooth attack and return.
Manual channel-fader holds do not disable speech protection.

**Hosts and speech** controls personal song commentary, the share of banter
focused on songs (85% by default), and gentle / sharp / savage roast intensity.
Song intros and request acknowledgements use the current pair's titles, artists,
and actual station plays, requests, votes and skips. Recent host lines help avoid
recycled punchlines. Unknown listening history stays unknown, and model failures
use contextual fallback jokes. The default is sharp; changes affect newly written
breaks. The transcript still shows the resulting dialogue.

Verified song/artist memes are optional in the same panel, with a 30% chance
per personal break, a 10-minute minimum gap, a 48-hour repeat cooldown and a
short-quote toggle. The initial [reviewed catalog](../config/memes.yaml) contains
11 references; unmatched tracks get ordinary song jokes. Song memes match
both artist and title, while artist callbacks identify their original source.
See [meme references](MEME-REFERENCES.md) for coverage and cooldown rules.

Use **Set vibe** beside the request box to describe a mood or what you are
doing: "Studying, calm and jazzy," "Building a factory in Satisfactory," or
"Cooking dinner, upbeat funk." The brief stays active across restarts until
you replace it or press **Clear vibe**. The browser has the same mode and
current-vibe display. You can also type `set vibe to ...` or `clear vibe` in
Request mode; explicit song requests still take priority.

Both Console and Radio accept **YouTube video links** and keep the exact
linked recording. Song details come from YouTube metadata, title/channel
clues, and the director's labelled estimates when needed. Radio also has
**Spotify search suggestions**: choose a result, then send it with its exact
title, credits and duration. See [requests](REQUESTS.md) for details.

The station weights future automatic choices toward suitable songs in your
catalogue, using genre tags, cautious tempo clues and a background interpretation
of the brief. Known matches receive at least 80% of the selection probability
when available; other music stays possible. Normal artist separation still
applies. If there is no useful evidence, it keeps playing rather than inventing
matches. This does not permanently change your taste scores or bulk-download
suggestions. Waiting automatic picks refresh; already planned mixes and loaded
decks finish first. Hosts can refer to the activity you supplied, without
repeating it every break. If interpretation is unavailable, basic mood/activity
rules still work.

**Song choice and variety** adds soft genre, credited-artist, embedded-lyric
and musical compatibility weights. No genre is a gate. Repetition fatigue,
exploration and an optional one-to-four-song outlook make room for changes in mood.
The default looks three songs ahead and considers taste/vibe weights as well
as the possible joins. It does not reserve those songs or replace requests.
Energy direction can follow, build, ease, surprise or wave; loudness is only a rough
energy clue. Missing tags or lyrics stay neutral. Hover a queue row for why it
was picked; that explanation is historical if you later reorder the queue.
Style profiles leave these song-choice preferences alone.

**Wave** builds or eases until the configured number of consecutive measured
changes, then favors the other direction. **Wave: songs before changing
direction** defaults to three; **Build/ease target step** defaults to 2 LUFS.
Missing loudness history does not count as completing an arc. Every preference
is still a sampling weight, so a request or a change of vibe can steer the set.

**Check vocals, bass and dips throughout each mix** compares simultaneous
samples of the cached audio profile with the planned gain and EQ curves.
Singers taking turns are treated differently from singers overlapping. The
search also compares shorter handoffs when a long blend would leave a hole.
The vocal, bass and dip weights are separately adjustable. These are estimates
from coarse audio evidence, not a rendered preview or verified phrase analysis.

With **Adapt bass handoff and effects to local audio**, both bass envelopes
influence the swap point. **Make room for the incoming singer with mid EQ**
adds a mild handoff when existing vocal stems show a clash; its default maximum
cut is 3 dB. This is broad EQ, not stem isolation. Manual EQ holds still win.

**Add late echo to sparse instrumental blends** can put a short echo on the
back half of a fading instrumental exit. It needs known vocal-free evidence
and room in the incoming track. Brief vocals near the exit reduce existing
echo too. Echo level, feedback, beat length and the main enable switch still
apply. Unknown vocal activity never enables this extra effect.

**Avoid clean/censored song versions** is on by default in Song choice settings.
Automatic rotation skips tracks labeled clean/censored in their title or album;
new source searches reject those editions and favor labeled explicit/uncensored
audio. The clean and explicit releases of a song are usually uploaded with the
exact same title, so source searches also ask YouTube Music (signed out) which
releases carry its explicit flag, and take the explicit one when it exists. A
song with no explicit release keeps its normal source. If YouTube Music doesn't
answer, a second search looks specifically for explicit audio. Songs
without edition labels remain eligible, and a radio edit alone is not treated
as censored. An explicitly requested clean edition is still allowed, and exact
YouTube links always play the linked video. Detection uses labels and that
flag, not listening for muted words. Cached songs are rechecked in the
background a couple a minute; one that came from the clean upload is
re-downloaded from the explicit one when it isn't scheduled, and the old file
plays until the new one is ready. `python -m radio.cli editions --dry-run`
lists the affected songs. Manually loaded decks are preserved.

Lyrics currently use embedded text and conservative word/theme cues, not an
external lyrics service or semantic interpretation. Structure analysis finds
acoustic boundaries, not verified chorus/drop labels. Longer set planning, more detailed song structure, and browser key lock are
still unfinished.

The radio **Transcript** shows the last 200 host lines for the station session,
with timestamps, the active speaker, a Follow live toggle and Copy. Future
lines stay hidden until their airtime. This is the hosts' script, not speech
recognition of songs or microphone audio.

**Two hosts.** Mav is dry and slightly over it. Rue is loud and completely
sincere. They interrupt each other. Both live in `config/personas/` as plain
YAML — rewrite them, change their voices, add a third, delete one.

**It talks over the music properly.** The station works out where the song
actually starts, then back-times the break so the hosts stop talking on the
post. If the break is too long to fit in the intro, it starts earlier over the
outgoing record instead of talking through the singer. Ducking is a real
sidechain shape — fast attack, slow release — multiplied against the crossfade
rather than fighting it.

**Segments.** Track intros, banter, news, Steam patch notes, fake ad reads,
station idents, time checks, and bits where they go through your listening
history and draw conclusions. Weights and cooldowns are in
`config/station.yaml`.

**News.** Gaming, music, and tech/AI out of the box. Add a category by pasting
a few feed URLs into `config/news.yaml`. Nothing gets read twice.

**Patch notes and adverts.** Points at your Steam library, finds real patches
for things you've played recently, and reads them. Wishlist becomes ad reads.
Every ad closes on a line making it obvious it's a bit.

**It learns.** Seeded once from your Spotify stats, then it's on its own —
skips, replays, requests and thumbs move affinity scores, which decay so old
phases fade instead of ruling forever. It tracks which artists you play at
which hour. Everything it believes gets mirrored into an Obsidian vault so you
can read it and argue with it.

**Requests.** One box, and it takes more than song titles:

```
Weezer - Buddy Holly                        a specific record
give me some bossa nova                     a genre or a mood
play me artists like yuno miles             similar acts
next transition tell me about the AI news   something to talk about
do the news                                 force the next segment
stop playing so much niko b                 play it less
i hate this                                 thumbs down what's on now
```

It tells you what it understood before it acts, because a misread request is
otherwise invisible until something odd turns up on air twenty minutes later.
Negation and the safety screen are pattern-matched rather than left to the
model — a rate-limited classifier that guessed "probably a song" would queue
the exact thing you asked it to stop playing.

**A queue you can actually move.** Play next, up, down, remove, clear. Records
already on the clock can be dropped but not reordered — their start times and
transitions were computed against the record before them, so shuffling would
invalidate all of it. The station stops picking for itself while your own queue
has anything in it.

**Skip lands on the mix, not on silence.** Skip doesn't cut — it winds the
clock forward to a couple of seconds before the next transition, so the
crossfade and whatever the hosts were going to say over it still happen. You
lose the rest of the record, not the join. If there's genuinely nothing on deck
it cuts and the builder refills, which is the only case where you'll hear a gap.

[requests](REQUESTS.md) has every edge case it handles.

---
Defalt v0.2.0 ? ? 2026 Zachary Parker ? [Patches](../CHANGELOG.md) ? [Privacy](../PRIVACY.md) ? [Terms](../TERMS.md)
