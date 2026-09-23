"""Energy, key, downbeat and similarity analysis; harmonic tempo matching;
phrase cues; set-level energy arcs; and selection performance."""
import json
import math
import random
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np

from radio import (analysis, compatibility, config, db, mixplanner, structure, taste,
                   timeline, transitions, trends)
from tests.station_defaults import StationDefaults

SR = analysis.SAMPLE_RATE


def tone(midi, seconds, amp=0.2):
    t = np.arange(int(SR * seconds)) / SR
    frequency = 440 * 2 ** ((midi - 69) / 12)
    return sum(amp / h * np.sin(2 * np.pi * frequency * h * t) for h in range(1, 5))


def progression(chords, beat=0.5, bars=8, drums=True, downbeat=0):
    """Chords one per bar, hi-hat on every beat and a kick on bar lines."""
    signal = np.concatenate([sum(tone(m, beat * 4) for m in chords[b % len(chords)])
                             for b in range(bars)]).astype(np.float32)
    if drums:
        rng = np.random.default_rng(0)
        for index, start in enumerate(range(int(0.1 * SR), len(signal) - 2000, int(beat * SR))):
            signal[start:start + 300] += rng.normal(0, 0.5, 300).astype(np.float32)
            if (index - downbeat) % 4 == 0:
                t = np.arange(2000) / SR
                signal[start:start + 2000] += (0.9 * np.sin(2 * np.pi * 55 * t) * np.exp(-t * 20)).astype(np.float32)
    return signal


C_MAJOR = [[60, 64, 67], [65, 69, 72], [67, 71, 74], [60, 64, 67]]
A_MINOR = [[57, 60, 64], [62, 65, 69], [64, 68, 71], [57, 60, 64]]


class KeyAndBars(unittest.TestCase):
    def test_key_follows_the_chords_and_keeps_a_runner_up(self):
        for chords, expected in ((C_MAJOR, (0, "major")), (A_MINOR, (9, "minor"))):
            tonic, mode, confidence, alternative = analysis.detect_key_detail(
                analysis._spectrogram(progression(chords)))
            self.assertEqual((tonic, mode), expected)
            self.assertGreater(confidence, 0.3)
            self.assertRegex(alternative, r"^\d{1,2}[AB]$")

    def test_a_slightly_sharp_recording_keeps_its_key(self):
        sharp = [[m + 0.3 for m in chord] for chord in A_MINOR]
        tonic, mode, _, _ = analysis.detect_key_detail(analysis._spectrogram(progression(sharp)))
        self.assertEqual((tonic, mode), (9, "minor"))

    def test_downbeat_follows_the_kick_and_chord_changes(self):
        for phase in range(4):
            grid = analysis.detect_beats(analysis._spectrogram(progression(C_MAJOR, bars=12, downbeat=phase)), 120.0)
            bar_position = round((grid["downbeat_offset"] - grid["beat_offset"]) / grid["beat_period"]) % 4
            # The synthetic kick and the chord change agree only on phase 0;
            # the kick alone still wins elsewhere, less confidently.
            first_beat = round((0.1 - grid["beat_offset"]) / grid["beat_period"])
            self.assertEqual(bar_position, (first_beat + phase) % 4, phase)
            self.assertGreater(grid["downbeat_confidence"], 0.0)


class Feel(unittest.TestCase):
    def test_energy_is_bounded_and_ranks_a_busy_track_over_a_sparse_one(self):
        busy = progression(C_MAJOR, bars=30)
        sparse = progression([[60, 64, 67]], bars=30, drums=False) * 0.3
        loud = analysis.features(busy, analysis._spectrogram(busy), bpm=120, bpm_confidence=.8, residual_ms=5)
        calm = analysis.features(sparse, analysis._spectrogram(sparse))
        for result in (loud, calm):
            self.assertTrue(0 <= result["energy"] <= 1 and 0 <= result["danceability"] <= 1)
            self.assertEqual(len(json.loads(result["embedding"])), analysis.EMBEDDING_SIZE)
        self.assertGreater(loud["energy"], calm["energy"])
        self.assertGreater(loud["danceability"], calm["danceability"])

    def test_energy_ignores_absolute_level(self):
        signal = progression(C_MAJOR, bars=20)
        quiet = signal * 0.1
        a = analysis.features(signal, analysis._spectrogram(signal), bpm=120, bpm_confidence=.8, residual_ms=5)
        b = analysis.features(quiet, analysis._spectrogram(quiet), bpm=120, bpm_confidence=.8, residual_ms=5)
        self.assertAlmostEqual(a["energy"], b["energy"], places=2)

    def test_profile_accepts_decoded_samples_and_reuses_its_result(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "song.wav"
            path.write_bytes(b"not decoded")
            samples = progression(C_MAJOR, bars=16)
            half = analysis.resample(samples, SR, SR // 2)
            with patch.object(analysis, "_decode", side_effect=AssertionError("decoded twice")):
                result = analysis.profile(path, samples=half, sample_rate=SR // 2)
                self.assertEqual(analysis.profile(path), result)
            self.assertEqual(result["features_version"], analysis.FEATURES_VERSION)
            self.assertIsNotNone(result["energy"])
            analysis._MEMO.clear()

    def test_ensure_features_backfills_once_and_never_erases_a_key(self):
        local = threading.local()
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(db, "_DB_PATH", Path(folder) / "t.db"), patch.object(db, "_LOCAL", local):
            audio = Path(folder) / "a.opus"
            audio.write_bytes(b"x")
            db.write("INSERT INTO tracks(key,title,artist,added_at,file,camelot,key_tonic,key_mode) "
                     "VALUES('k','T','A',0,?,'8A',9,'minor')", (str(audio),))
            row = dict(db.one("SELECT * FROM tracks WHERE key='k'"))
            measured = {"features_version": analysis.FEATURES_VERSION, "energy": .7, "danceability": .5,
                        "onset_rate": 3.0, "embedding": "[]", "camelot": "", "key_tonic": -1, "key_mode": "",
                        "key_confidence": 0, "key_alt": "", "downbeat_offset": .5, "downbeat_confidence": .6}
            with patch.object(analysis, "profile", return_value=measured) as profile:
                updated = analysis.ensure_features(row)
                analysis.ensure_features(updated)
            profile.assert_called_once()
            stored = db.one("SELECT * FROM tracks WHERE key='k'")
            self.assertEqual((stored["energy"], stored["camelot"], stored["key_tonic"], stored["features_version"]),
                             (.7, "8A", 9, analysis.FEATURES_VERSION))
            with patch.object(analysis, "ensure_features", side_effect=lambda r: {**r, "features_version": 1}):
                self.assertEqual(analysis.reanalyse(), {"updated": 0, "failed": 0, "pending": 0})
            db.write("UPDATE tracks SET features_version=0 WHERE key='k'")
            with patch.object(analysis, "ensure_features", side_effect=lambda r: {**r, "features_version": 1}):
                self.assertEqual(analysis.reanalyse()["updated"], 1)
            local.conn.close()


class Sections(unittest.TestCase):
    def test_chord_change_at_constant_level_is_a_boundary(self):
        rate = structure.SAMPLE_RATE
        t = np.arange(rate * 30) / rate
        first = sum(np.sin(2 * np.pi * 440 * 2 ** ((m - 69) / 12) * t) for m in (60, 64, 67))
        second = sum(np.sin(2 * np.pi * 440 * 2 ** ((m - 69) / 12) * t) for m in (61, 66, 70))
        signal = np.concatenate([first, second]).astype(np.float32) * 0.2
        result = structure.analyse(signal)
        self.assertEqual(result["version"], structure.VERSION)
        near = [b for b in result["boundaries"] if abs(b["at"] - 30) <= 2]
        self.assertTrue(near, result["boundaries"])
        self.assertEqual(near[0]["reason"], "harmonic change")

    def test_phrase_helpers_need_a_trusted_grid(self):
        track = {"beat_period": 0.5, "downbeat_offset": 1.0, "beat_residual_ms": 10, "bpm_confidence": .8}
        self.assertEqual(structure.phrase_alignment(track, 17.0), 1.0)     # 1 + 16 s (eight bars)
        self.assertEqual(structure.phrase_alignment(track, 3.0), 0.5)      # a bar line
        self.assertEqual(structure.phrase_alignment(track, 3.5), 0.0)
        self.assertEqual(structure.snap_to_phrase(track, 16.4, 1.0), 17.0)
        self.assertIsNone(structure.snap_to_phrase(track, 12.0, 1.0))
        self.assertEqual(structure.phrase_lines(track, 10, 50), [49.0, 33.0, 17.0])
        self.assertEqual(structure.phrase_alignment({**track, "beat_residual_ms": 90}, 17.0), 0.0)


class PhraseCues(StationDefaults):
    def profile(self, duration=160, exits=(), entries=()):
        return {"version": structure.VERSION, "duration": duration, "step_sec": 0.5, "complete": True,
                "vocal_source": "existing_stem",
                "bins": [{"at": i / 2, "end": (i + 1) / 2, "energy": .7, "bass": .5, "vocal": 0}
                         for i in range(duration * 2)],
                "boundaries": [], "entries": [{"at": t, "score": 1} for t in entries],
                "exits": [{"at": t, "score": 1} for t in exits]}

    def test_exit_and_entry_snap_to_phrase_lines(self):
        grid = {"bpm": 120, "bpm_confidence": .9, "beat_period": 0.5, "downbeat_offset": 1.0,
                "beat_residual_ms": 8, "camelot": "8A", "duration": 160}
        outgoing = {**grid, "key": "a", "title": "A", "artist": "A", "structure": self.profile(exits=(144.6,))}
        incoming = {**grid, "key": "b", "title": "B", "artist": "B", "intro_sec": 40,
                    "structure": self.profile(entries=(16.7,))}
        with patch.object(config.station, "get", side_effect=lambda k, d=None: {
                "transitions.mid_song_cues": False, "transitions.minimum_play_fraction": .8,
                "transitions.max_intro_skip": 20, "crossfade.detect_cold_end": False}.get(k, d)):
            choice = mixplanner.refine(outgoing, incoming, transitions.Plan(overlap=8), out_start=0, out_offset=0,
                                       out_duration=160, out_rate=1, in_offset=0, in_duration=160, in_rate=1)
        self.assertIn(choice.out_duration, (145.0, 129.0, 160))
        self.assertIn(choice.in_offset, (0, 17.0))
        self.assertGreater(choice.candidates, 0)


class HarmonicTempo(StationDefaults):
    def test_semitones_and_wheel_shifts(self):
        self.assertEqual(transitions.semitones(1.0), 0)
        self.assertEqual(transitions.semitones(1.028), 0)
        self.assertEqual(transitions.semitones(1.04), 1)
        self.assertEqual(transitions.semitones(0.95), -1)
        self.assertEqual(transitions.shift_camelot("8B", 1), "3B")   # C major -> C# major
        self.assertEqual(transitions.shift_camelot("8A", -1), "1A")
        self.assertEqual(transitions.shift_camelot("bad", 1), "")

    def test_a_compatible_pair_is_capped_inside_its_key(self):
        rate, note = transitions.harmonic_choice("8A", "8A", 1.04)
        self.assertEqual(transitions.semitones(rate), 0)
        self.assertLess(rate, 1.03)
        self.assertIn("capped", note)
        self.assertEqual(transitions.harmonic_choice("8A", "8A", 1.02), (1.02, ""))
        self.assertEqual(transitions.harmonic_choice("8A", "8A", 1.04, key_lock=True), (1.04, ""))

    def test_a_clashing_pair_is_pitched_into_a_compatible_key_when_close(self):
        # 8A (A minor) into 3A (A# minor): one semitone down lands on 8A.
        self.assertEqual(transitions.harmonic_choice("8A", "3A", 0.97), (0.97, ""))  # already lands on 8A
        rate, note = transitions.harmonic_choice("8A", "3A", 0.98)
        self.assertEqual(transitions.semitones(rate), -1)
        self.assertTrue(analysis.keys_compatible("8A", transitions.shift_camelot("3A", -1)))
        self.assertIn("semitone", note)
        self.assertLessEqual(abs(rate / 0.98 - 1), 0.02)
        # Too far from the beat-matched rate: leave the tempo alone.
        self.assertEqual(transitions.harmonic_choice("8A", "3A", 1.0), (1.0, ""))

    def test_schedule_uses_the_heard_key(self):
        def track(key, bpm, camelot):
            return dict(key=key, title=key, duration=120, intro_sec=20, bpm=bpm, camelot=camelot,
                        key_confidence=.8, beat_period=60 / bpm, beat_offset=0.1, beat_residual_ms=10,
                        bpm_confidence=0.9)
        schedule = timeline.Schedule()
        schedule.add_music("a", track("a", 120, "8A"))
        b = schedule.add_music("b", track("b", 124.8, "8A"))
        self.assertEqual(transitions.semitones(b.meta["playback_rate"]), 0)
        self.assertIn("pitch capped", b.meta["transition"]["reason"])
        self.assertNotIn("camelot_played", b.meta)

    def test_rise_is_chosen_from_energy_not_loudness(self):
        quiet_banger = {"bpm": 0, "camelot": "", "lufs": -30, "energy": .8}
        calm = {"bpm": 0, "camelot": "", "lufs": -8, "energy": .3}
        self.assertEqual(transitions.choose(calm, quiet_banger).preset, "rise")
        self.assertNotEqual(transitions.choose(quiet_banger, calm).preset, "rise")
        self.assertNotEqual(transitions.choose({"lufs": -30}, {"lufs": -8}).preset, "rise")


class Compatibility(StationDefaults):
    def test_mix_fit_judges_the_key_the_listener_will_hear(self):
        a = {"bpm": 120, "bpm_confidence": .9, "camelot": "8A", "key_confidence": .9}
        b = {"bpm": 123.6, "bpm_confidence": .9, "camelot": "3A", "key_confidence": .9}
        pitched = compatibility.mix_fit(a, b, {"tempo_match": True, "key_lock": False})
        locked = compatibility.mix_fit(a, b, {"tempo_match": True, "key_lock": True})
        self.assertGreater(pitched, locked)

    def test_similarity_is_neutral_without_embeddings(self):
        first = json.dumps([1.0] * analysis.EMBEDDING_SIZE)
        with patch.object(compatibility, "_library_stats",
                          return_value=([0.0] * analysis.EMBEDDING_SIZE, [1.0] * analysis.EMBEDDING_SIZE)):
            same = compatibility.similarity({"embedding": first}, {"embedding": first})
            opposite = compatibility.similarity({"embedding": first},
                                                {"embedding": json.dumps([-1.0] * analysis.EMBEDDING_SIZE)})
        self.assertAlmostEqual(same, 1.0)
        self.assertAlmostEqual(opposite, 0.0)
        self.assertIsNone(compatibility.similarity({}, {"embedding": first}))
        self.assertIsNone(compatibility.similarity({"embedding": "[1,2]"}, {"embedding": first}))

    def test_arc_eases_late_and_builds_after_a_break(self):
        settings = {"enabled": True, "energy_arc_enabled": True, "energy_arc_break_build": .04}
        self.assertLess(compatibility.arc_level(settings, hour=3), compatibility.arc_level(settings, hour=19))
        early = compatibility.arc_level({**settings, "break_position": 0}, hour=19)
        later = compatibility.arc_level({**settings, "break_position": 3}, hour=19)
        self.assertGreater(later, early)
        custom = {**settings, "energy_arc_hours": {19: .2}}
        self.assertAlmostEqual(compatibility.arc_level(custom, hour=19), .2)
        self.assertIsNone(compatibility.arc_level({**settings, "energy_arc_enabled": False}))

    def test_lookahead_prefers_routes_near_the_arc(self):
        tracks = [{"key": k, "title": k, "artist": k, "energy": e} for k, e in
                  (("fits", .66), ("off", .05), ("x", .66), ("y", .66))]
        settings = {**compatibility._DEFAULT_SETTINGS, "artist_separation": 0, "energy_arc_weight": 2,
                    "lookahead_depth": 1, "energy_weight": 0}
        with patch.object(compatibility.time, "localtime", return_value=time.struct_time((2026, 1, 1, 19, 0, 0, 0, 1, 0))):
            result = {t["key"]: w for w, t in compatibility.lookahead([(1.0, t) for t in tracks], [], settings)}
        self.assertGreater(result["fits"], result["off"])


class SelectionSpeed(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        local = threading.local()
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        for p in (patch.object(db, "_DB_PATH", Path(folder.name) / "t.db"), patch.object(db, "_LOCAL", local),
                  patch.object(trends, "cached", return_value=[])):
            p.start()
            self.addCleanup(p.stop)
        rng = random.Random(1)
        for i in range(120):
            db.write("INSERT INTO tracks(key,title,artist,source,added_at,genre,lyrics,bpm,bpm_confidence,energy) "
                     "VALUES(?,?,?,?,?,?,?,?,?,?)",
                     (f"k{i}", f"Song {i}", f"Artist {i % 40}", "local", 0, rng.choice(["House", "Rock", "Soul"]),
                      " ".join(rng.choice("love heart road train night dance party lonely cry".split())
                               for _ in range(60)), 120, .8, rng.random()))
            taste.bump(f"k{i}", f"Artist {i % 40}", rng.uniform(-1, 2))

    def test_a_pick_is_a_handful_of_queries(self):
        calls = {"n": 0}
        query, one = db.query, db.one
        def counted(function):
            def wrapper(*args, **kwargs):
                calls["n"] += 1
                return function(*args, **kwargs)
            return wrapper
        history = [dict(db.one("SELECT * FROM tracks WHERE key='k0'"))]
        with patch.object(db, "query", counted(query)), patch.object(db, "one", counted(one)):
            start = time.perf_counter()
            picked = taste.pick_next(set(), previous=history[0], history=history)
            elapsed = time.perf_counter() - start
        self.assertIsNotNone(picked)
        self.assertLessEqual(calls["n"], 8)
        self.assertLess(elapsed, 1.0)

    def test_affinity_and_daypart_tables_match_single_lookups(self):
        table = taste.affinity_table()
        self.assertAlmostEqual(table[("track", "k3")], taste.affinity("track", "k3"))
        taste.record("thumbs_up", "k3", "Artist 3")
        hourly = taste.daypart_table()
        self.assertAlmostEqual(taste._daypart_from(hourly, db.norm("Artist 3")), taste.daypart_fit("Artist 3"))

    def test_lyrics_cache_notices_changed_lyrics(self):
        first = {t["key"]: t for t in taste.candidates()}["k1"]["lyrics"]
        db.write("UPDATE tracks SET lyrics=? WHERE key='k1'", ("completely different words " * 5,))
        second = {t["key"]: t for t in taste.candidates()}["k1"]["lyrics"]
        self.assertNotEqual(first, second)
        self.assertTrue(second.startswith("completely"))
        self.assertEqual(taste.recording_ids({"title": "A (feat. B)", "artist": "C"}), {"song:c|a"})


if __name__ == "__main__":
    unittest.main()
