"""Opening decks must be real schedule items, followed by normal rotation."""
import math
import random
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import director, timeline, transitions
from radio.app import app
from tests.test_transitions import value_at


class DeckRadio(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        audio = Path(self.temp.name) / "track.wav"
        audio.touch()
        self.tracks = {key: dict(key=key, title=key, artist="test", file=str(audio),
                                duration=120.0, bpm=128, camelot="8A", intro_sec=12)
                       for key in ("a", "b", "c", "d")}
        s = self.station = director.Station.__new__(director.Station)
        s.clock = director.Clock()
        s.lock = threading.RLock()
        s.schedule = timeline.Schedule()
        s.rng = random.Random(0)
        s._lineup = []
        s._recent_keys = []
        s._songs_since_break = 0
        s._break_after = 10
        s._signed_on = True
        s._active_wish = None
        s._epoch = 0
        s.status_note = "test"
        self.addCleanup(patch.stopall)
        patch("radio.director.db.one", side_effect=lambda sql, args: self.tracks.get(args[0])).start()
        patch("radio.app.director.station", return_value=s).start()
        # Tests use fixed settings without changing the user's config files.
        patch.object(transitions.config.station, "get", side_effect=lambda k, default=None: default).start()
        self.client = app.test_client()

    def start(self, tracks=None, token="test"):
        return self.client.post("/api/decks/start", json={"session": token, "tracks": tracks if tracks is not None else [
            {"key": "a", "deck": 0, "offset": 20}, {"key": "b", "deck": 1, "offset": 0}]})

    def test_both_loaded_decks_get_a_transition_then_normal_rotation(self):
        s = self.station
        s._enqueue(self.tracks["a"], "auto")
        s._enqueue(self.tracks["c"], "auto")
        response = self.start()
        self.assertEqual(response.status_code, 200, response.json)
        a, b = s.schedule.music_items()
        self.assertEqual([a.meta["deck"], b.meta["deck"]], [0, 1])
        self.assertEqual(a.offset, 20)
        self.assertEqual(a.duration, 100)
        self.assertLess(b.start_at, a.end_at)
        self.assertEqual(b.meta["transition"]["preset"], "blend")
        self.assertIn("automation", a.meta)
        self.assertIn("automation", b.meta)
        s._extend()
        self.assertEqual([i.meta["key"] for i in s.schedule.music_items()], ["a", "b", "c"])
        self.assertLess(s.schedule.music_items()[2].start_at, b.end_at)

    def test_retry_does_not_reset_decks_or_duplicate_tracks(self):
        first = self.start().json
        second = self.start().json
        self.assertEqual(first["items"], second["items"])
        self.assertEqual(first["epoch"], second["epoch"])

    def test_playing_b_can_open_before_a_without_swapping_decks(self):
        response = self.start([{"key": "b", "deck": 1, "offset": 50},
                               {"key": "a", "deck": 0, "offset": 0}])
        self.assertEqual([i["meta"]["deck"] for i in response.json["items"]], [1, 0])

    def test_bad_second_deck_does_not_replace_existing_schedule(self):
        self.start()
        original = self.station.schedule
        for bad in ("missing", "bad_offset", "same_deck"):
            selection = [{"key": "a", "deck": 0}, {"key": "b", "deck": 1}]
            if bad == "missing": selection[1]["key"] = "missing"
            if bad == "bad_offset": selection[1]["offset"] = math.inf
            if bad == "same_deck": selection[1]["deck"] = 0
            self.assertEqual(self.start(selection, bad).status_code, 400)
            self.assertIs(self.station.schedule, original)

    def test_no_preloads_resumes_existing_station(self):
        self.start()
        original = self.station.schedule
        self.assertEqual(self.start([], "empty").status_code, 200)
        self.assertIs(self.station.schedule, original)

    def test_single_preloaded_deck_gets_next_station_track(self):
        self.start([{"key": "a", "deck": 1, "offset": 0}])
        self.station._enqueue(self.tracks["c"], "auto")
        self.station._extend()
        a, c = self.station.schedule.music_items()
        self.assertEqual(a.meta["deck"], 1)
        self.assertLess(c.start_at, a.end_at)

    def test_intro_and_cue_position_limit_overlap(self):
        s = timeline.Schedule()
        s.add_music("a", self.tracks["a"])
        b = s.add_music("b", self.tracks["b"], offset=10)
        self.assertAlmostEqual(b.meta["transition"]["overlap"], 3)

    def test_uncertain_analysis_does_not_trigger_long_blends(self):
        a, b = dict(self.tracks["a"]), dict(self.tracks["b"])
        a["bpm_confidence"] = b["key_confidence"] = 0.01
        plan = transitions.choose(a, b)
        self.assertEqual(plan.preset, "fade")
        self.assertLessEqual(plan.overlap, 6)

    def test_big_tempo_jump_outranks_harmonic_key_and_energy(self):
        a, b = dict(self.tracks["a"], bpm=92, lufs=-16), dict(self.tracks["b"], bpm=140, lufs=-12)
        self.assertEqual(transitions.choose(a, b).preset, "slam")

    def test_beat_drift_limits_unsynced_overlap(self):
        plan = transitions.choose(dict(self.tracks["a"], bpm=128), dict(self.tracks["b"], bpm=135))
        self.assertEqual(plan.overlap, 3)
        self.assertEqual(plan.preset, "melt")
        self.assertIn("drift", plan.reason)

    def test_eq_stays_neutral_between_transition_windows(self):
        points = timeline.build_parameter(120, [[0, -26], [6, 0]], 0,
                                          [[0, 0], [6, -26]], 114, 0)
        for t in (10, 60, 110, 114):
            self.assertEqual(value_at(points, t), 0, (t, points))
        self.assertLess(value_at(points, 117), -10)


if __name__ == "__main__":
    unittest.main()
