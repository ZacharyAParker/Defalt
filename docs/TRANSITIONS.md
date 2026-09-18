# Transitions

Every record is analysed when it downloads — tempo, musical key, loudness —
and the station picks how to get from one to the next based on what the two
actually are.

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

## How `auto` decides

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

## How it reaches the speakers

The server renders every transition as parameter automation — lists of
`[time, value]` breakpoints, the same shape as the gain envelope — covering
gain, three EQ bands in dB, and low-pass and high-pass frequencies in Hz.

The browser builds a chain per record:

```
source -> [low shelf] -> [mid peak] -> [high shelf] -> [low-pass] -> [high-pass] -> gain -> master
```

Filter nodes are only created when that record's transition actually automates
them, so a plain crossfade costs one gain node. A parameter that never leaves
its resting value is dropped server-side rather than shipped and ignored.

Ducking multiplies into the same gain curve, so a host talking through a
transition ducks correctly at every instant instead of fighting whatever the
crossfade is doing.

## Tuning

`config/station.yaml` under `transitions:`, hot-reloaded. Or use the
**Transition** picker on the Board to force one and hear the difference.

| Setting | Default | What it changes |
|---|---|---|
| `preset` | `auto` | Force a style, or let it choose |
| `tempo_tolerance` | 0.06 | How close counts as beat-matched |
| `slam_distance` | 0.18 | How far apart before it stops trying to blend |
| `lpf_floor_hz` | 380 | How far a low-pass closes |
| `hpf_ceiling_hz` | 900 | How far a high-pass climbs |
| `long_multiplier` | 1.5 | Overlap for a matched pair |
| `short_multiplier` | 0.55 | Overlap for a mismatched one |

`crossfade.duration` is still the base length everything scales from, and
`crossfade.max_fraction_of_track` still caps it against the shorter record.
