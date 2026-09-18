import random
import unittest
from unittest.mock import patch

from radio import config, playback, timeline, transitions


class PlaybackTime(unittest.TestCase):
    def test_linear_recovery_has_exact_integral_and_inverse(self):
        curve = [[0, 1.06], [10, 1.06], [40, 1]]
        self.assertAlmostEqual(playback.source_at(curve, 10), 10.6)
        self.assertAlmostEqual(playback.source_at(curve, 25), 10.6 + 15 * 1.045)
        self.assertAlmostEqual(playback.source_at(curve, 40), 41.5)
        self.assertAlmostEqual(playback.source_at(curve, 100), 101.5)
        self.assertAlmostEqual(playback.wall_at(curve, 120), 118.5)
        self.assertAlmostEqual(playback.rate_at(curve, 25), 1.03)

    def test_round_trip_through_accelerating_and_decelerating_segments(self):
        rng = random.Random(42)
        for initial in (.92, .97, 1.0, 1.04, 1.08):
            curve = [[0, initial], [12, initial], [42, 1], [62, .96], [82, 1.02]]
            for wall in [0, 12, 42, 62, 82, 120] + [rng.uniform(0, 300) for _ in range(100)]:
                source = playback.source_at(curve, wall, initial)
                self.assertAlmostEqual(playback.wall_at(curve, source, initial), wall, places=9)

    def test_absent_or_invalid_curve_uses_constant_rate(self):
        invalid = [None, [], [[10, 1]], [[0, 1], [0, .9]], [[0, 0]],
                   [[0, 1], [2, float("nan")]], [[0, 1], [float("inf"), 1]], "broken"]
        for curve in invalid:
            with self.subTest(curve=curve):
                self.assertAlmostEqual(playback.source_at(curve, 10, 1.04), 10.4)
                self.assertAlmostEqual(playback.wall_at(curve, 10.4, 1.04), 10)
                self.assertEqual(playback.rate_at(curve, 10, 1.04), 1.04)

    def test_recovery_requires_room_after_ramp_and_preserves_overlap(self):
        self.assertEqual(playback.recovery(1.04, 25, 10, 30), [])
        curve = playback.recovery(1.04, 120, 10, 30)
        self.assertEqual(curve, [[0, 1.04], [10, 1.04], [40, 1]])
        self.assertEqual(playback.rate_at(curve, 9.99), 1.04)
        self.assertEqual(playback.rate_at(curve, 40), 1)
        self.assertEqual(playback.recovery(1, 120, 10, 30), [])


class RecoveryScheduling(unittest.TestCase):
    def setUp(self):
        self.settings = {
            "transitions.tempo_recovery": True, "transitions.recovery_seconds": 30,
            "transitions.tempo_match": True, "transitions.tempo_match_limit": .06,
            "transitions.preset": "fade", "transitions.phrase_beats": 4,
            "transitions.smart_cues": True, "crossfade.duration": 8,
            "crossfade.detect_cold_end": False,
        }
        mocked = patch.object(config.station, "get", side_effect=lambda key, default=None:
                             self.settings.get(key, default))
        mocked.start()
        self.addCleanup(mocked.stop)

    def track(self, key, bpm=120, **values):
        return dict(key=key, title=key, artist=key, duration=180, bpm=bpm,
                    beat_period=60 / bpm, beat_offset=.1, beat_residual_ms=5,
                    bpm_confidence=.9, camelot="8A", intro_sec=30, **values)

    def test_recovery_off_retains_fixed_speed_duration(self):
        self.settings["transitions.tempo_recovery"] = False
        schedule = timeline.Schedule()
        schedule.add_music("a", self.track("a", 120))
        second = schedule.add_music("b", self.track("b", 116), offset=9, entry_locked=True)
        self.assertNotIn("rate_curve", second.meta)
        self.assertAlmostEqual(second.duration * second.meta["playback_rate"], 171)

    def test_recovery_preserves_source_end_and_pitched_intro_from_a_preloaded_cue(self):
        schedule = timeline.Schedule()
        schedule.add_music("a", self.track("a", 120))
        second = schedule.add_music("b", self.track("b", 116), offset=9, entry_locked=True)
        curve = second.meta["rate_curve"]
        self.assertEqual(second.offset, 9)
        self.assertAlmostEqual(second.offset + playback.source_at(curve, second.duration), 180)
        self.assertAlmostEqual(playback.source_at(curve, second.meta["intro_sec"]), 21)
        self.assertAlmostEqual(curve[1][0], schedule._plans[second.id].overlap + 2)
        self.assertEqual(playback.rate_at(curve, second.duration), 1)

    def test_following_transition_matches_recovered_bpm_and_integrated_beat_phase(self):
        schedule = timeline.Schedule()
        schedule.add_music("a", self.track("a", 120))
        second = schedule.add_music("b", self.track("b", 116), offset=9, entry_locked=True)
        with patch.object(transitions, "beat_nudge", wraps=transitions.beat_nudge) as nudge:
            third = schedule.add_music("c", self.track("c", 118))
        self.assertAlmostEqual(third.meta["playback_rate"], 116 / 118, places=5)
        self.assertIn("stable tail after tempo recovery", third.meta["transition"]["reason"])
        outgoing_grid = nudge.call_args.args[0]
        phase_shift = playback.source_at(second.meta["rate_curve"], second.duration) - second.duration
        self.assertAlmostEqual(outgoing_grid["beat_offset"], .1 - 9 - phase_shift)
        self.assertAlmostEqual(outgoing_grid["beat_period"], 60 / 116)
        self.assertGreaterEqual(third.start_at - second.start_at, second.meta["rate_curve"][-1][0])
        for item in (second, third):
            self.assertAlmostEqual(item.offset + playback.source_at(item.meta["rate_curve"], item.duration), 180)
        schedule.seal()
        self.assertEqual(second.meta["rate_curve"][-1][1], 1)

    def test_short_tracks_do_not_start_a_recovery_they_cannot_finish(self):
        schedule = timeline.Schedule()
        schedule.add_music("a", self.track("a", 120))
        short = self.track("short", 116)
        short["duration"] = 25
        second = schedule.add_music("b", short)
        self.assertNotIn("rate_curve", second.meta)
        self.assertAlmostEqual(second.duration * second.meta["playback_rate"], 25)

    def test_large_outgoing_mix_is_confined_to_constant_tail(self):
        schedule = timeline.Schedule()
        schedule.add_music("a", self.track("a", 116))
        second = schedule.add_music("b", self.track("b", 120))
        self.settings["crossfade.duration"] = 150
        self.settings["crossfade.max_fraction_of_track"] = .95
        self.settings["transitions.phrase_beats"] = 0
        third = schedule.add_music("c", self.track("c", 118))
        self.assertGreaterEqual(third.start_at, second.start_at + second.meta["rate_curve"][-1][0])
        self.assertLessEqual(third.meta["transition"]["overlap"], third.duration * .95)


if __name__ == "__main__":
    unittest.main()
