# Transitions

Every record is analysed when it downloads — tempo, musical key, loudness —
and the station picks how to get from one to the next based on what the two
actually are: a base blend from the presets below, and often a technique on
top of it -- an echo out, a loop roll, a brake -- chosen with some variety.

## The presets

A transition is three independent choices. The named presets are just
combinations of them, which is why they're easy to reason about.

| Preset | Volume | Bass | Filters |
|---|---|---|---|
| **fade** | equal-power crossfade | swaps at the midpoint | — |
| **rise** | both stay up | swaps at the end | low-pass in, high-pass out |
| **blend** | equal-power crossfade | three-band fade | — |
| **wave** | both stay up | swaps at the centre | low-pass on both |
| **melt** | linear crossfade | swaps at the centre | high-pass on both |
| **slam** | hard centred swap | untouched | — |

What they sound like:

- **fade** — the safe one. Constant loudness across the overlap, and only one
  bassline at a time.
- **rise** — the new record arrives muffled and opens up while the old one
  thins out and keeps its low end until the last moment. Good for stepping up.
- **blend** — highs hand over first, mids next, lows last. Over nine seconds
  it reads as one record *becoming* another rather than two playing at once.
- **wave** — both tracks low-passed through the middle, so the mix goes
  underwater and surfaces again on the new track.
- **melt** — both high-passed, so the bottom drops out through the middle and
  comes back. The opposite feel to wave.
- **slam** — no blend at all. For pairs that were never going to mix.

## How `auto` picks the base

```
tempo close AND keys compatible  ->  blend   (long, 1.5× overlap)
tempo close, keys clash          ->  wave
keys compatible, tempo apart     ->  melt
stepping up in loudness          ->  rise
tempo far apart                  ->  slam    (short, 0.55× overlap)
nothing to go on                 ->  fade
```

**Keys** use the Camelot wheel. Same key, one step around it, or the relative
major/minor swap counts as compatible — the standard DJ rule.

**Tempo** is compared octave-invariantly. A detector that reports 172 BPM for
an 86 BPM track hasn't found a different tempo, it's found the same pulse
counted twice, and treating that as a mismatch would pick a hard cut for two
records that would have beat-matched perfectly. So 86 and 172 are zero apart.

That folding also means the distance scale only runs to about 0.33, which is
why `slam_distance` defaults to 0.18 rather than something that looks bigger.

## Techniques

The presets are all one move -- two faders and a bass swap -- in different
clothes, so the same kind of pair always sounded the same. A technique is a
different move, the kind a DJ reaches for on purpose. It rides on top of the
preset `auto` picked, which stays as the *base*: whenever a technique cannot
or should not play, the base blend is what airs.

| Technique | What happens | Needs |
|---|---|---|
| **echo_out** | The outgoing record's last beat is caught in the echo (3/4-beat delay), the fader closes on the incoming one, and the repeats ring over the drop while a high-pass thins them away. Four beats of tail, two with clashing keys. | a tempo |
| **loop_roll** | Rolls on the outgoing record halving towards the incoming downbeat -- 4, 2, 1, then ½ beat (2, 1, ½, ¼ when there is less room) -- with a high-pass rising and the bass leaving. Hard cut on the one. | aligned grids |
| **brake** | Tape stop: the outgoing record slows to nothing over one or two beats and the next one slams in on the one. | a tempo |
| **spinback** | A backwards burst on the outgoing record, smeared into the reverb, then the drop. | a tempo |
| **echo_freeze** | The last beat is frozen in the echo (feedback ~1, send closed) and filtered away under the incoming record. | a tempo |
| **reverb_wash** | The outgoing record washes into reverb as a high-pass climbs; the incoming one emerges from under a low-pass. The dry records trade at equal power. | -- |
| **stem_swap** | The incoming drums and bass under the outgoing vocals for about half the overlap, then vocals and harmony hand over on a bar line. | separated stems on both records, aligned grids, matched tempos |
| **acapella_intro** | The incoming vocal alone over the outgoing instrumental, then the rest of the incoming record drops in on a bar. | stems, compatible keys, an incoming vocal |
| **filter_ride** | A long filter ride with the bass swapped on the bar: rising energy opens the incoming record from a low-pass while the outgoing one thins upwards, falling energy the other way round. | matched tempos |
| **silence_punch** | The outgoing record gated to silence for a beat, so the drop lands into nothing. | aligned grids |
| **drop_swap** | A hard cut exactly on the downbeat, the outgoing bass already gone for the last bar. | aligned grids |

Plus the six presets, which are the smooth half of the menu.

Everything is quantized. The schedule already starts the incoming record on
an outgoing beat and, with phrase cues on, on a phrase of both records. A
technique then finds **the one** -- the incoming record's first downbeat,
at most a bar in -- and times every roll, brake, gate and cut from it in whole
beats of the outgoing record at its playing rate. Lead-in effects (rolls,
brakes, gates) happen on the outgoing record *before* the one, only in the
part of it that is free: after its own incoming transition and after any
tempo recovery has settled.

## How a technique is chosen

`radio/techniques.py`, `select()`. Two stages.

**What is possible.** These are rules, not weights, because breaking them
sounds broken:

- a host already talking over the mix: nothing flashy, only the base blend
- stem techniques only when *both* records have cached separations
- rolls, gates and drop swaps only on trusted, aligned beat grids
- long blends (filter ride, stems) only when the tempos agree after matching
- never two singers at once: echo tails, washes and rides are out when both
  records show vocals in the overlap
- an acapella needs compatible keys and an incoming vocal to feature
- a lead-in needs enough of the outgoing record before the one, and a tail
  needs enough overlap after it

**What fits.** Every possible technique gets a weight from the context:

| Signal | Leans towards |
|---|---|
| energy stepping up | loop roll, silence punch, drop swap, a rising filter ride |
| energy stepping down | echo out, reverb wash, echo freeze, brake |
| a big tempo gap | brake, spinback, echo out, drop swap -- never a long blend |
| clashing keys | clean cuts; echo tails kept short and filtered harder |
| compatible keys | filter ride, stem swap, acapella |
| electronic / hip hop genres | rolls, drops, gates, stems |
| acoustic, jazz, folk, soul... | far fewer effects, mostly echo and wash |
| unknown vocal evidence | moves that never overlap the two records |

**Creativity** (0..1) decides how much of the probability goes to effects at
all; the weights decide how it is shared. The base blend keeps
`1.6 × (1 − c) + 0.15`, effects share `3 × c^1.3` scaled by how well they fit
this pair on average -- a pair that suits nothing flashy gets fewer effects,
not the same number spread thinner. Roughly: 0 is the presets only, 0.2 about
one mix in five, 0.5 about half, 1 nearly always. The hour and the listener's
brief ease it: the small hours (0-6) and a slow brief turn it down, a fast
brief up.

**Variety.** A technique used within the last `technique_memory` transitions
(default 3) keeps 3% of its weight. The memory is the aired history (one
`transition` row per mix in the events table, written as each mix airs) plus
everything already planned on the clock. The dice are seeded from the two
records' keys, so the same pair in the same situation always gets the same
answer and a preview never flickers.

**Pinning and banning.** `transitions.preset` accepts a technique name as well
as a preset: it plays wherever it is possible, and the reason says why when it
is not. Each technique has an `allow_<name>` switch.

## Speech comes first

Breaks are usually placed *after* the record they talk over, so the schedule
checks again when it seals: a technique whose effect window has a host in it
(plus the duck's attack and release) airs as its base blend instead, and the
reason says so. If the break later goes away, the technique comes back.
Ducking multiplies into the technique's volume curves exactly as it does into
a crossfade's, so speech stays on top even of an echo tail.

## The hosts notice

When a flashy technique airs, the next comedy break is sometimes told so, as a
fact it may use: `hosts.transition_note_chance` (default 30%), within fifteen
minutes of the mix, once per mix. The note comes from the aired log, never
from the plan, so the hosts cannot mention a spinback that was dropped for
speech.

## Analysis

`radio/analysis.py`, numpy only — librosa would do it in three lines but drags
in scipy and numba for what is a couple of FFTs.

- **Tempo**: spectral flux onset envelope, autocorrelated, with octave
  correction pulling the answer into 70–180 BPM.
- **Key**: chroma folded to twelve pitch classes, correlated against the
  Krumhansl-Schmuckler major and minor profiles.

Both return a confidence, and low confidence is useful rather than a failure:
a spoken-word track genuinely has no BPM, and the picker falls back to a plain
crossfade instead of pretending it knows something.

Spot-checked against the library: Better Off Alone 136 (actual 137), Just the
Two of Us 95.7 (actual ~96), Teenage Dirtbag 92.3 (actual 92), Us and Them
71.8 (actual ~70). Octave errors happen — Basket Case reads 89 rather than
171 — which is exactly what the octave-invariant comparison is there for.

## Automation lanes (protocol)

A technique reaches both players on the **incoming** music item's
`meta.transition`. Everything the presets already sent is still there and
still means the same thing -- `preset`, `overlap`, `reason`, `envelope`,
`deck_envelope`, `automation` (dB bands, Hz filters), `echo`, `rate_curve` --
so an old preset transition is byte-for-byte what it was. A technique adds:

```jsonc
"transition": {
  "preset": "loop_roll",        // the technique's name, for older readers
  "technique": "loop_roll",     // what airs: a technique, or the preset name
  "base": "blend",              // the preset underneath (what airs under speech)
  "overlap": 7.5, "reason": "...",
  "switch_at": 1234.5,          // station seconds: the incoming one
  "flashy": true,               // worth a host mention
  "requires": ["stems"],        // only on stem techniques
  "lanes": {
    "out": { "<lane>": [[t, v], ...], ... },   // the record before this one
    "in":  { "<lane>": [[t, v], ...], ... }    // this record
  },
  "events": [
    {"type": "roll", "deck": "out", "at": t, "length_seconds": s, "until": t2},
    {"type": "loop", "deck": "in",  "at": t, "length_seconds": s, "until": t2}
  ]
}
```

- **Times** are absolute station-clock seconds, the clock `start_at` is on.
  Out lanes lie inside the outgoing item, in lanes inside the incoming one;
  events likewise.
- **Lanes**, their values, and resting values:

  | lane | value | rests at |
  |---|---|---|
  | `gain` | channel gain 0..2 | 1 (not emitted by the station; volume rides the envelope) |
  | `level` | transition level 0..1, multiplied on gain | 1 |
  | `low` `mid` `high` | isolator knob 0..1, 0.5 flat, 0 kill | 0.5 |
  | `sweep` | -1..1: below 0 low-pass, above 0 high-pass, 0 off | 0 |
  | `echo_send` | into the echo, post-fader, 0..1 | 0 |
  | `echo_feedback` | 0..1; ~1 with the send closed is a freeze | 0.3 |
  | `echo_beats` | delay in beats of the deck's grid | (left) |
  | `reverb_send` | into the shared reverb, post-fader, 0..1 | 0 |
  | `rate` | the deck's **absolute** speed -4..4: through 0 is a brake, below 0 backwards. The station bakes the scheduled rate in, so 1.02 on a deck pitched +2% is "normal" | the scheduled rate |
  | `stem_drums` `stem_bass` `stem_harmonic` `stem_vocals` | per-stem level 0..2; no-op without stems | 1 |

  Linear between breakpoints, the value of the first point before it and of
  the last after it. **Every lane the station writes ends at its resting
  value**, because a deck keeps a lane's last value; unknown lanes are
  ignored.
- **Events.** `roll`: from `at` the deck repeats the `length_seconds` slice
  that starts where it is at `at` (wall seconds at its playing rate), until
  `until`, then carries on from where it would have been (slip). `loop` is
  the same, for a loop. The station only emits them on beats.
- **Volume** is not a lane: a technique's fader moves are baked into the
  item's `envelope`/`deck_envelope`, so ducking still multiplies in and a
  player that ignores lanes still hears a sensible (if plain) cut or blend.
- **Signal order** the lanes assume, per deck:
  `source (rate, rolls) -> stems -> low/mid/high -> sweep -> gain x level ->`
  `[post-fader echo and reverb sends] -> dry + returns -> envelope -> master`.
  Closing `level` stops feeding the sends and leaves what is in them to ring.
  In the browser the envelope comes after the returns; on the console it
  drives the channel gain and crossfader. Either way a technique that wants
  its tail heard keeps the envelope up and closes `level` instead, and the
  duck still turns the whole deck down under speech.
- **Echo return.** There is no return lane: while a transition's echo lanes
  run, the player opens its echo return (the browser uses a fixed 0.8).
- **Speech.** A technique the schedule dropped for a host carries no lanes and
  `technique == base`; its reason says why.

The console runs the lanes on its engine's automation curves (a lane drives
the same control its command sets). The browser runs what Web Audio can, see
below.

### On the console

`src/airtime/automation.rs` builds one curve per lane per deck from
everything above, on the station clock; `src/airtime/mod.rs` converts it to
output frames and sends it to the engine once -- when the item arrives, and
again only when the plan, the deck's situation (the other half of the mix
missing, a late start), a control you took, or the clock mapping (by more than
15 ms) changes. Records start with `PlayAt` on their exact frame. Nothing is
stepped per UI frame, so a minimised window mixes exactly as a watched one.

- `out` lanes (and events) go to the deck playing the previous music item,
  `in` lanes to this item's deck; a deck's own curves are its item's `in` lanes
  plus the next item's `out` lanes.
- Lane names map through `Lane::from_name`; an unknown lane, role or event
  type is dropped with one line in `logs/console.log`.
- `level` multiplies the envelope (it rests at 1), then the speech duck
  multiplies in. `low`/`mid`/`high`/`sweep`/`echo_send`/`rate` replace the
  legacy curve for that control over the span their points cover and leave
  it alone elsewhere. Every other lane (`reverb_send`, `echo_feedback`,
  `echo_beats`, stems, `gain`) is run as written and cleared 0.1 s after its
  last point, so the control is the console's again; a `gain` lane is scaled
  by the channel's own fader times trim.
- Any echo lane opens that deck's echo return at 0.8 of dry.
- `roll` and `loop` events both become the engine's `LoopAt` (a slip roll):
  `length_seconds` is heard time, converted to record time at the deck's
  scheduled rate; `length_beats` uses the record's `beat_period`.
- `requires: ["stems"]` loads the record's stems from the separation cache
  when the deck is ready; with none cached the stem lanes do nothing (logged).
- A control you take detaches its lane at once and is left out of every
  curve sent after, until the record ends or you hand it back.

## How it reaches the speakers

The server renders every transition as parameter automation — lists of
`[time, value]` breakpoints, the same shape as the gain envelope — covering
gain, three EQ bands in dB, and low-pass and high-pass frequencies in Hz.
A technique adds the lanes above.

The browser builds a chain per record:

```
source -> [gate] -> [low shelf] -> [mid peak] -> [high shelf] -> [low-pass] -> [high-pass]
       -> [technique bands] -> [technique sweep] -> [level] -> (echo / reverb sends) -> gain -> master
```

Filter nodes are only created when that record's transition actually automates
them, so a plain crossfade costs one gain node. A parameter that never leaves
its resting value is dropped server-side rather than shipped and ignored.

In the browser, technique lanes map as follows:

| lane | Web Audio |
|---|---|
| `level` | a gain node before the sends |
| `low` `mid` `high` | shelf / peak / shelf, knob mapped to dB the way the console maps it (0.5 = 0 dB, 1 = +6, 0 = kill) |
| `sweep` | a low-pass and a high-pass, fader mapped to Hz as on the console's strip |
| `echo_send` `echo_feedback` `echo_beats` | a DelayNode feedback loop (feedback held just under 1 so a freeze cannot creep), return fixed at 0.8 |
| `reverb_send` | a ConvolverNode with a generated 2.4 s impulse |
| `rate` | `playbackRate`; **negative rates are not possible**, so a spinback becomes a brake to a stop where the backwards burst would have ended |
| `roll` / `loop` events | a looping AudioBufferSourceNode of the slice, over the gated record (slip) |
| `stem_vocals` `stem_bass` | stood in by a mid cut (to -12 dB) and a low-shelf cut; drums and harmony are ignored -- there is no honest filter for them. The selector only offers stem techniques when both records are separated, so this only matters for a browser listening to a console session |

A technique planned after its outgoing record was already handed to Web Audio
is wired in behind the gate, without stopping the source, as long as none of
its lanes has started yet.

Ducking multiplies into the same gain curve, so a host talking through a
transition ducks correctly at every instant instead of fighting whatever the
crossfade is doing.

## Tuning

`config/station.yaml` under `transitions:`, hot-reloaded. Or use the
**Transition** picker on the Board to force a preset or a technique and hear
the difference.

| Setting | Default | What it changes |
|---|---|---|
| `preset` | `auto` | Force a style, or let it choose |
| `tempo_tolerance` | 0.06 | How close counts as beat-matched |
| `slam_distance` | 0.18 | How far apart before it stops trying to blend |
| `lpf_floor_hz` | 380 | How far a low-pass closes |
| `hpf_ceiling_hz` | 900 | How far a high-pass climbs |
| `long_multiplier` | 1.5 | Overlap for a matched pair |
| `short_multiplier` | 0.55 | Overlap for a mismatched one |
| `creativity` | 0.5 | 0 smooth radio (presets only) .. 1 show-off DJ |
| `technique_memory` | 3 | Mixes before a technique may repeat |
| `allow_<technique>` | true | Switch one technique off |
| `hosts.transition_note_chance` | 0.3 | How often a break hears about a flashy mix |

All of these are in the Mix settings, under **Transitions** (the host note
under **Hosts and speech**); the browser's Board has the picker (presets and
techniques) and a Creativity fader. `transitions.creativity` missing from a
configuration means 0, the smooth radio that configuration was written for;
the shipped `station.yaml` sets 0.5.

`crossfade.duration` is still the base length everything scales from, and
`crossfade.max_fraction_of_track` still caps it against the shorter record.

---
Defalt v0.2.0 ? ? 2026 Zachary Parker ? [Patches](../CHANGELOG.md) ? [Privacy](../PRIVACY.md) ? [Terms](../TERMS.md)
