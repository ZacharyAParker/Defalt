"""The editable radio mix settings and three starting points."""
import math
from . import config

# key, label, default, bounds/options. One schema serves validation and the UI.
FIELDS = [
    ("transitions.preset", "Transition", "auto", ["auto", "fade", "rise", "blend", "wave", "melt", "slam"]),
    ("crossfade.duration", "Base overlap (seconds)", 6.0, [0.5, 20.0]),
    ("transitions.minimum_blend_seconds", "Shortest automatic blend (seconds)", 3.0, [0.5, 12.0]),
    ("transitions.long_multiplier", "Compatible tracks: length multiplier", 1.5, [1.0, 2.5]),
    ("transitions.short_multiplier", "Mismatched tracks: length multiplier", 0.55, [0.15, 1.0]),
    ("transitions.tempo_match", "Match tempo on reliable grids", True, None),
    ("transitions.tempo_match_limit", "Maximum pitch change (fraction)", 0.06, [0.0, 0.08]),
    ("transitions.native_key_lock", "Preserve pitch on console decks", False, None),
    ("transitions.tempo_recovery", "Return toward original tempo after mixing", False, None),
    ("transitions.recovery_seconds", "Tempo recovery ramp (seconds)", 30.0, [10.0, 90.0]),
    ("transitions.beat_align", "Align beats", True, None),
    ("transitions.phrase_beats", "Phrase length (beats; 0 disables)", 4, [0, 16]),
    ("transitions.beat_max_residual_ms", "Maximum grid error (ms)", 30.0, [5.0, 60.0]),
    ("transitions.beat_max_drift", "Maximum drift (beats)", 0.25, [0.05, 0.5]),
    ("transitions.tempo_tolerance", "Compatible tempo gap (fraction)", 0.06, [0.01, 0.15]),
    ("transitions.slam_distance", "Short handoff above tempo gap", 0.18, [0.08, 0.33]),
    ("transitions.smart_cues", "Compare musical cue and transition options", True, None),
    ("transitions.overlap_scoring", "Check vocals, bass and dips throughout each mix", True, None),
    ("transitions.vocal_collision_weight", "Avoid overlapping singers", 1.0, [0.0, 2.0]),
    ("transitions.energy_dip_weight", "Avoid empty spots during a mix", 0.6, [0.0, 2.0]),
    ("transitions.bass_collision_weight", "Avoid competing basslines", 0.5, [0.0, 2.0]),
    ("transitions.prepare_tracks_ahead", "Minimum songs planned ahead", 1, [1, 3]),
    ("skip.lead_in", "Skip: seconds before the transition", 10.0, [2.0, 30.0]),
    ("selection.avoid_music_videos", "Use audio recordings instead of music videos", True, None),
    ("selection.title_separation_hours", "Automatic song repeat cooldown (hours)", 5.0, [0.5, 48.0]),
    ("selection.artist_separation", "Songs between the same artist", 6, [0, 20]),
    ("learning.ignore_skips", "Ignore skips for taste and host banter", False, None),
    ("transitions.exit_search_seconds", "End search when deeper cues are off (seconds)", 24.0, [0.0, 60.0]),
    ("transitions.max_intro_skip", "Opening skip when deeper cues are off (seconds)", 8.0, [0.0, 20.0]),
    ("transitions.mid_song_cues", "Use deeper structural mix points", True, None),
    ("transitions.max_entry_skip_fraction", "Maximum opening skipped", 0.25, [0.0, 0.49]),
    ("transitions.minimum_play_fraction", "Full song heard before mixing out", 0.65, [0.51, 1.0]),
    ("hosts.personal_comments", "Personal song commentary", True, None),
    ("hosts.song_context", "Look up sourced song background", True, None),
    ("hosts.listening_stats_gap", "Breaks between listening-history jokes", 8, [3, 30]),
    ("hosts.song_comment_chance", "Banter focused on your songs", 0.85, [0.0, 1.0]),
    ("hosts.roast_level", "Roast intensity", "sharp", ["gentle", "sharp", "savage"]),
    ("hosts.meme_references", "Verified song and artist memes", True, None),
    ("hosts.meme_chance_percent", "Meme chance per personal break (%)", 30, [0, 100]),
    ("hosts.meme_gap_minutes", "Minimum gap between memes (minutes)", 10, [0, 120]),
    ("hosts.meme_repeat_hours", "Same meme cooldown (hours)", 48, [1, 168]),
    ("hosts.meme_quotes", "Allow short verified meme quotes", True, None),
    ("transitions.feedback_enabled", "Correct small playback drift", True, None),
    ("transitions.feedback_max_adjustment", "Maximum live speed correction", 0.005, [0.0, 0.01]),
    ("transitions.feedback_tolerance_ms", "Playback drift tolerance (ms)", 30.0, [10.0, 150.0]),
    ("transitions.eq_enabled", "Automate EQ", True, None),
    ("transitions.adaptive_eq_fx", "Adapt bass handoff and effects to local audio", True, None),
    ("transitions.vocal_handoff", "Make room for the incoming singer with mid EQ", True, None),
    ("transitions.vocal_eq_depth", "Maximum vocal handoff cut (dB)", 3.0, [0.0, 6.0]),
    ("transitions.echo_in_blends", "Add late echo to sparse instrumental blends", True, None),
    ("transitions.eq_mode", "EQ handoff", "auto", ["auto", "center_bass", "end_bass_swap", "three_band_fade", "none"]),
    ("transitions.eq_strength", "EQ depth", 0.8, [0.0, 1.0]),
    ("transitions.filters_enabled", "Automate filter sweeps", True, None),
    ("transitions.lpf_floor_hz", "Low-pass floor (Hz)", 380.0, [120.0, 5000.0]),
    ("transitions.hpf_ceiling_hz", "High-pass ceiling (Hz)", 900.0, [100.0, 4000.0]),
    ("transitions.echo_enabled", "Echo on outgoing melt/slam transitions", True, None),
    ("transitions.echo_mix", "Echo wet level", 0.18, [0.0, 0.5]),
    ("transitions.echo_feedback", "Echo feedback", 0.30, [0.0, 0.65]),
    ("transitions.echo_beats", "Echo delay (beats)", 0.5, [0.125, 2.0]),
    ("ducking.target_gain", "Music level under speech", 0.10, [0.02, 0.5]),
    ("ducking.attack", "Ducking attack (seconds)", 0.35, [0.05, 2.0]),
    ("ducking.hold_after", "Hold after speech (seconds)", 0.40, [0.0, 2.0]),
    ("ducking.release", "Music return (seconds)", 1.2, [0.2, 5.0]),
    ("tts.target_lufs", "Voice loudness target (LUFS)", -16.0, [-24.0, -12.0]),
    ("tts.true_peak_db", "Voice peak ceiling (dB)", -1.5, [-6.0, -1.0]),
    ("selection.avoid_clean_versions", "Avoid clean/censored song versions", True, None),
    ("selection.prefer_original_recording", "Prefer original recordings unless a version is requested", True, None),
    ("selection.compatibility.enabled", "Consider how consecutive songs fit", True, None),
    ("selection.compatibility.genre_weight", "Genre connection", 0.65, [0.0, 1.0]),
    ("selection.compatibility.artist_weight", "Artist connection", 0.25, [0.0, 1.0]),
    ("selection.compatibility.lyrics_weight", "Lyrical connection when text is available", 0.45, [0.0, 1.0]),
    ("selection.compatibility.mix_weight", "Tempo, key and level compatibility", 0.35, [0.0, 1.0]),
    ("selection.compatibility.variety_strength", "Encourage a change after similar songs", 0.65, [0.0, 1.0]),
    ("selection.compatibility.explore_chance", "Chance to explore outside the current vibe", 0.18, [0.0, 1.0]),
    ("selection.compatibility.fatigue_after", "Ease similarity after this many songs", 4, [2, 12]),
    ("selection.compatibility.history_size", "Recent songs considered", 10, [3, 20]),
    ("selection.compatibility.lookahead_enabled", "Consider several possible next songs", True, None),
    ("selection.compatibility.lookahead_depth", "Future songs considered per route", 3, [1, 4]),
    ("selection.compatibility.lookahead_weight", "Future mix opportunities", 0.3, [0.0, 1.0]),
    ("selection.compatibility.lookahead_candidates", "Candidates considered ahead", 16, [4, 32]),
    ("selection.compatibility.energy_direction", "Energy direction (loudness is a rough clue)", "follow", ["follow", "build", "ease", "surprise", "wave"]),
    ("selection.compatibility.energy_arc_tracks", "Wave: songs before changing direction", 3, [2, 6]),
    ("selection.compatibility.energy_step_lufs", "Build/ease target step (LUFS)", 2.0, [0.5, 4.0]),
    ("selection.compatibility.energy_weight", "Energy direction influence", 0.3, [0.0, 1.0]),
]

BASE = {key: default for key, _, default, _ in FIELDS if not key.startswith("selection.")}
PROFILES = {
    "Smooth DJ": dict(BASE),
    "Clean radio": {**BASE, "crossfade.duration": 3.0, "transitions.tempo_match": False,
                    "transitions.eq_strength": 0.5, "transitions.filters_enabled": False,
                    "transitions.echo_enabled": False, "transitions.phrase_beats": 0},
    "Expressive club": {**BASE, "crossfade.duration": 10.0, "transitions.eq_strength": 1.0,
                        "transitions.echo_mix": 0.28, "transitions.phrase_beats": 8},
}


def snapshot():
    return {"profiles": PROFILES, "fields": [
        {"key": key, "label": label, "value": config.station.get(key, default),
         "kind": "bool" if isinstance(default, bool) else "choice" if isinstance(default, str) else "int" if isinstance(default, int) else "number",
         "bounds": bounds}
        for key, label, default, bounds in FIELDS]}


def validate(values):
    if not isinstance(values, dict) or not values:
        raise ValueError("expected mix settings")
    fields = {key: (default, bounds) for key, _, default, bounds in FIELDS}
    checked = {}
    for key, value in values.items():
        if key not in fields:
            raise ValueError(f"unknown mix setting: {key}")
        default, bounds = fields[key]
        if isinstance(default, bool):
            if type(value) is not bool:
                raise ValueError(f"{key} must be true or false")
        elif isinstance(default, str):
            if value not in bounds:
                raise ValueError(f"invalid option for {key}")
        else:
            if type(value) not in (float, int) or not math.isfinite(value) or not bounds[0] <= value <= bounds[1]:
                raise ValueError(f"{key} is outside its range")
            if isinstance(default, int):
                if value != int(value):
                    raise ValueError(f"{key} must be a whole number")
                value = int(value)
        checked[key] = value
    return checked
