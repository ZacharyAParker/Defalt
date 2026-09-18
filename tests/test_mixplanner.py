import copy
import unittest
from unittest.mock import patch

from radio import config, mixplanner, playback, structure, timeline, transitions
from tests.test_transitions import value_at


class SmartCuePlanner(unittest.TestCase):
    def setUp(self):
        self.settings = {
            "transitions.smart_cues": True, "transitions.mid_song_cues": False, "transitions.adaptive_eq_fx": True,
            "transitions.preset": "auto", "transitions.exit_search_seconds": 24,
            "transitions.max_intro_skip": 8, "transitions.minimum_play_fraction": 0.8,
            "transitions.phrase_beats": 0, "transitions.tempo_match": False,
            "crossfade.detect_cold_end": False,
        }
        mocked = patch.object(config.station, "get", side_effect=lambda key, default=None:
                             self.settings.get(key, default))
        mocked.start()
        self.addCleanup(mocked.stop)

    def profile(self, duration=160, *, vocal=None, bass=0.5, energy=0.7,
                entries=(), exits=()):
        def value(field, t):
            return field(t) if callable(field) else field
        return {"version": structure.VERSION, "duration": duration, "step_sec": 0.5,
                "complete": True, "vocal_source": "unknown" if vocal is None else "existing_stem",
                "bins": [{"at": i / 2, "end": (i + 1) / 2,
                          "energy": value(energy, i / 2), "bass": value(bass, i / 2),
                          "vocal": value(vocal, i / 2)} for i in range(duration * 2)],
                "boundaries": [], "entries": [{"at": t, "score": 1} for t in entries],
                "exits": [{"at": t, "score": 1} for t in exits]}

    def track(self, profile=None, **overrides):
        result = {"key": "track", "title": "Test", "artist": "Test", "duration": 120,
                  "bpm": 120, "camelot": "8A", "intro_sec": 30}
        if profile is not None:
            result["structure"] = profile
        return {**result, **overrides}

    def refine(self, outgoing, incoming, plan=None, **overrides):
        kwargs = dict(out_start=50, out_offset=0, out_duration=120, out_rate=1,
                      in_offset=0, in_duration=120, in_rate=1)
        kwargs.update(overrides)
        return mixplanner.refine(outgoing, incoming, plan or transitions.Plan(overlap=8), **kwargs)

    def test_missing_analysis_preserves_original_plan_and_positions(self):
        plan = transitions.Plan(overlap=7.2, reason="conservative fallback")
        with patch.object(structure, "_decode", side_effect=AssertionError("schedule decoded audio")):
            choice = self.refine(self.track(), self.track(self.profile()), plan,
                                 out_offset=12, out_duration=100, in_offset=9, in_duration=111)
        self.assertIs(choice.plan, plan)
        self.assertEqual((choice.out_duration, choice.in_offset, choice.in_duration), (100, 9, 111))
        self.assertEqual(choice.candidates, 0)

    def test_incomplete_analysis_is_not_used_to_guess_an_outro(self):
        profile = self.profile(exits=(118,))
        profile["complete"] = False
        plan = transitions.Plan(overlap=8)
        choice = self.refine(self.track(profile), self.track(self.profile()), plan)
        self.assertIs(choice.plan, plan)
        self.assertEqual(choice.candidates, 0)

    def test_source_positions_are_converted_once_for_different_deck_speeds(self):
        choice = self.refine(self.track(self.profile(vocal=0, exits=(138,))),
                             self.track(self.profile(vocal=0, entries=(8,))),
                             out_offset=20, out_duration=100, out_rate=1.2,
                             in_offset=4, in_duration=100, in_rate=0.96)
        self.assertAlmostEqual(choice.out_duration, (138 - 20) / 1.2)
        self.assertEqual(choice.in_offset, 8)
        self.assertAlmostEqual(choice.in_duration, (4 + 100 * .96 - 8) / .96)
        self.assertAlmostEqual(choice.in_offset + choice.in_duration * .96, 100)

    def test_existing_host_line_prevents_cutting_its_music_bed_short(self):
        outgoing = self.track(self.profile(vocal=0, exits=(118,)))
        incoming = self.track(self.profile(vocal=0))
        ordinary = self.refine(outgoing, incoming)
        protected = self.refine(outgoing, incoming, protected_until=169)
        self.assertLess(ordinary.out_duration, 120)
        self.assertGreaterEqual(50 + protected.out_duration, 169)

    def test_earliest_start_rejects_cues_inside_an_existing_transition(self):
        choice = self.refine(self.track(self.profile(vocal=0, exits=(118,))),
                             self.track(self.profile(vocal=0)), earliest_start=165)
        self.assertGreaterEqual(50 + choice.out_duration - choice.plan.overlap, 165)
        self.assertLessEqual(choice.plan.overlap, 5)

    def test_two_singing_tracks_choose_a_shorter_cut(self):
        choice = self.refine(self.track(self.profile(vocal=1)), self.track(self.profile(vocal=1)))
        self.assertEqual(choice.plan.preset, "slam")
        self.assertLess(choice.plan.overlap, 8)

    def test_candidate_search_never_expands_an_existing_drift_limit(self):
        # A low-energy opening followed by a matching section would otherwise
        # tempt the acoustic scorer to expand a deliberately capped overlap.
        plan = transitions.Plan(overlap=2)
        incoming = self.profile(energy=lambda t: .1 if t < 2 else .7)
        choice = self.refine(self.track(self.profile()), self.track(incoming), plan)
        self.assertLessEqual(choice.plan.overlap, 2)

    def test_configured_track_fraction_caps_candidate_overlap(self):
        self.settings["crossfade.max_fraction_of_track"] = .03
        choice = self.refine(self.track(self.profile()), self.track(self.profile()))
        self.assertLessEqual(choice.plan.overlap, 120 * .03)

    def test_forced_style_is_honored_even_when_vocals_require_shorter_overlap(self):
        self.settings["transitions.preset"] = "fade"
        choice = self.refine(self.track(self.profile(vocal=1)), self.track(self.profile(vocal=1)))
        self.assertEqual(choice.plan.preset, "fade")
        self.assertLess(choice.plan.overlap, 8)

    def test_unknown_vocals_do_not_invent_a_clash_or_skip_an_opening(self):
        choice = self.refine(self.track(self.profile()), self.track(self.profile(entries=(4,))))
        self.assertEqual(choice.plan.preset, "fade")
        self.assertEqual(choice.plan.overlap, 8)
        self.assertEqual(choice.in_offset, 0)

    def test_locked_preloaded_offset_is_preserved_despite_a_better_entry_candidate(self):
        outgoing = self.track(self.profile(vocal=0))
        incoming = self.track(self.profile(vocal=0, entries=(18,)))
        unlocked = self.refine(outgoing, incoming, in_offset=17, in_duration=103)
        locked = self.refine(outgoing, incoming, in_offset=17, in_duration=103, entry_locked=True)
        self.assertEqual(unlocked.in_offset, 18)
        self.assertEqual(locked.in_offset, 17)
        self.assertEqual(locked.in_duration, 103)

    def test_schedule_preserves_preloaded_cue_and_previously_scheduled_voice(self):
        schedule = timeline.Schedule(epoch_offset=50)
        first = schedule.add_music("a", self.track(self.profile(vocal=0, exits=(118,)), key="a"))
        speech = schedule.add_voice("voice", first.end_at - 3, 2)
        next_track = self.track(self.profile(vocal=0, entries=(18,)), key="b")
        second = schedule.add_music("b", next_track, offset=17, entry_locked=True)
        schedule.seal()
        self.assertEqual(second.offset, 17)
        self.assertAlmostEqual(second.offset + second.duration * second.meta["playback_rate"], 120)
        self.assertGreaterEqual(first.end_at, speech.end_at)
        self.assertEqual(speech.start_at, 167)
        self.assertTrue(second.meta["transition"]["reason"].startswith("compared "))

    def test_adaptive_bass_handoff_follows_actual_source_entry_at_double_speed(self):
        plan = transitions.Plan(overlap=8, eq="center_bass", echo_mix=.3,
                                effects=("lpf_in", "hpf_out"))
        incoming = self.profile(vocal=.5, bass=lambda t: 0 if t < 14 else 1)
        mixplanner.adapt(plan, self.profile(vocal=.8), incoming, 120, 4, 1, 2)
        self.assertAlmostEqual(plan.bass_swap, .7)
        self.assertAlmostEqual(plan.echo_mix, .06)
        self.assertNotIn("lpf_in", plan.effects)
        self.assertIn("hpf_out", plan.effects)
        outgoing_curve, incoming_curve = transitions.render(plan, plan.overlap)
        self.assertGreater(value_at(outgoing_curve.low, 3), value_at(incoming_curve.low, 3))
        self.assertLess(value_at(outgoing_curve.low, 7), value_at(incoming_curve.low, 7))

    def test_adaptation_does_not_mutate_analysis_profiles(self):
        profile = self.profile(vocal=.5)
        original = copy.deepcopy(profile)
        mixplanner.adapt(transitions.Plan(overlap=8), profile, profile, 120, 0, 1, 1)
        self.assertEqual(profile, original)

    def enable_mid_song(self):
        self.settings.update({"transitions.mid_song_cues": True,
                              "transitions.minimum_play_fraction": .65,
                              "transitions.max_entry_skip_fraction": .25})

    def test_better_exit_can_be_more_than_24_seconds_before_end(self):
        self.enable_mid_song()
        outgoing = self.profile(vocal=0, bass=0, exits=(90,),
                                energy=lambda t: .7 if t < 100 else .05)
        choice = self.refine(self.track(outgoing), self.track(self.profile(vocal=0, bass=0)))
        self.assertEqual(choice.out_duration, 90)
        self.assertGreaterEqual(choice.out_duration - choice.plan.overlap, 120 * .65)
        self.assertIn("earlier structural exit", choice.plan.reason)

    def test_skipped_entry_and_early_exit_share_full_song_budget(self):
        self.enable_mid_song()
        outgoing = self.profile(vocal=0, exits=(82, 96, 114),
                                energy=lambda t: .7 if t < 116 else .05)
        choice = self.refine(self.track(outgoing), self.track(self.profile(vocal=0)),
                             out_offset=30, out_duration=90)
        self.assertEqual(choice.out_duration, 84)  # source 114, after starting at 30
        self.assertGreaterEqual(choice.out_duration - choice.plan.overlap, 120 * .65)
        self.assertLessEqual(choice.plan.overlap, 6)

    def test_deep_entry_may_skip_earlier_vocals_only_at_supported_boundary(self):
        self.enable_mid_song()
        incoming = self.profile(vocal=lambda t: 1 if t < 20 else 0, entries=(28, 40))
        incoming["boundaries"] = [{"at": t, "confidence": .8} for t in (28, 40)]
        choice = self.refine(self.track(self.profile(vocal=0)), self.track(incoming, intro_sec=0))
        self.assertEqual(choice.in_offset, 28)
        self.assertEqual(choice.in_duration, 92)
        self.assertLessEqual(choice.in_offset, 120 * .25)
        self.assertIn("structural entry cue", choice.plan.reason)
        locked = self.refine(self.track(self.profile(vocal=0)), self.track(incoming), entry_locked=True)
        self.assertEqual(locked.in_offset, 0)

    def test_unknown_vocals_without_boundary_do_not_justify_deep_entry(self):
        self.enable_mid_song()
        choice = self.refine(self.track(self.profile(vocal=0)),
                             self.track(self.profile(entries=(28,))))
        self.assertEqual(choice.in_offset, 0)

    def test_unhelpful_mid_song_cut_is_not_selected_for_variety(self):
        self.enable_mid_song()
        choice = self.refine(self.track(self.profile(vocal=0, exits=(80,))),
                             self.track(self.profile(vocal=0)))
        self.assertEqual(choice.out_duration, 120)

    def test_tempo_recovery_uses_integrated_source_time_for_majority(self):
        self.enable_mid_song()
        curve = [[0, .95], [10, .95], [40, 1]]
        total = playback.wall_at(curve, 120, .95)
        profile = self.profile(vocal=0, exits=(90,), energy=lambda t: .7 if t < 100 else .05)
        choice = self.refine(self.track(profile), self.track(self.profile(vocal=0)),
                             out_duration=total, out_curve=curve, out_initial_rate=.95)
        self.assertAlmostEqual(playback.source_at(curve, choice.out_duration, .95), 90)
        self.assertGreaterEqual(playback.source_at(curve, choice.out_duration-choice.plan.overlap, .95), 78)

    def test_configuration_can_require_almost_all_of_song(self):
        self.enable_mid_song()
        self.settings["transitions.minimum_play_fraction"] = .98
        choice = self.refine(self.track(self.profile(vocal=0, exits=(90,))),
                             self.track(self.profile(vocal=0)))
        self.assertEqual(choice.out_duration, 120)
        self.assertLessEqual(choice.plan.overlap, 2.4 + 1e-6)


if __name__ == "__main__":
    unittest.main()
