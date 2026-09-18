"""Overlap decisions use simultaneous audio evidence and bounded automation."""
import unittest
from unittest.mock import patch

from radio import config, mixconfig, mixplanner, structure, timeline, transitions
from tests.test_transitions import value_at


def profile(vocal=0, bass=.5, energy=.7):
    def sample(value, at):
        return value(at) if callable(value) else value
    return {"version": structure.VERSION, "duration": 120, "complete": True,
            "step_sec": .5, "entries": [], "exits": [], "boundaries": [],
            "bins": [{"at": i / 2, "end": (i + 1) / 2,
                      "vocal": sample(vocal, i / 2), "bass": sample(bass, i / 2),
                      "energy": sample(energy, i / 2)} for i in range(240)]}


class MixDynamics(unittest.TestCase):
    def setUp(self):
        self.settings = {}
        patcher = patch.object(config.station, "get",
                               side_effect=lambda key, default=None: self.settings.get(key, default))
        patcher.start()
        self.addCleanup(patcher.stop)

    def risk(self, left, right, plan=None):
        return mixplanner.overlap_risk(plan or transitions.Plan(overlap=8),
                                      left, right, lambda t: 112 + t, 0, 1)

    def test_alternating_singers_are_not_scored_as_simultaneous_singing(self):
        left = profile(vocal=lambda t: 1 if 112 <= t < 116 else 0)
        alternating = self.risk(left, profile(vocal=lambda t: 0 if t < 4 else 1))
        collision = self.risk(left, profile(vocal=lambda t: 1 if t < 4 else 0))
        self.assertEqual(alternating["vocal"], 0)
        self.assertGreater(collision["vocal"], .5)

    def test_eq_does_not_claim_to_isolate_or_remove_a_singer(self):
        plan = transitions.Plan(overlap=8, eq="three_band_fade", eq_strength=1)
        risk = self.risk(profile(vocal=1), profile(vocal=1), plan)
        self.assertGreater(risk["vocal"], .5)

    def test_unknown_stems_stay_unknown(self):
        self.assertIsNone(self.risk(profile(vocal=None), profile(vocal=1))["vocal"])

    def test_long_center_cut_has_a_larger_energy_hole_than_a_crossfade(self):
        left = right = profile()
        fade = self.risk(left, right)
        cut = self.risk(left, right, transitions.Plan(overlap=8, volume="center_cut", eq="none"))
        short = self.risk(left, right, transitions.Plan(overlap=2, volume="center_cut", eq="none"))
        self.assertLess(fade["dip"], .001)  # rendered curves round to four decimals
        self.assertGreater(cut["dip"], short["dip"])

    def test_bass_eq_reduces_collision_estimate(self):
        left = right = profile(bass=1)
        controlled = self.risk(left, right)
        raw = self.risk(left, right, transitions.Plan(overlap=8, eq="none"))
        self.assertLess(controlled["bass"], raw["bass"])

    def test_vocal_handoff_is_bounded_and_incoming_mids_finish_flat(self):
        plan = transitions.Plan(overlap=8, eq_strength=1)
        mixplanner.adapt(plan, profile(vocal=1), profile(vocal=1), 120, 0, 1, 1)
        self.assertIsNotNone(plan.vocal_swap)
        out, incoming = transitions.render(plan, 8)
        self.assertEqual(value_at(out.mid, 0), 0)
        self.assertEqual(value_at(incoming.mid, 8), 0)
        self.assertTrue(all(-3 <= db <= 0 for _, db in out.mid + incoming.mid))

    def test_vocal_handoff_respects_disabled_eq_or_missing_evidence(self):
        for eq, vocal, enabled in [("none", 1, True), ("center_bass", None, True),
                                   ("center_bass", 1, False)]:
            self.settings["transitions.vocal_handoff"] = enabled
            plan = transitions.Plan(overlap=8, eq=eq)
            mixplanner.adapt(plan, profile(vocal=vocal), profile(vocal=1), 120, 0, 1, 1)
            self.assertIsNone(plan.vocal_swap)

    def test_sparse_instrumental_exit_gets_a_late_bounded_echo(self):
        left = profile(energy=lambda t: .8 if t < 116 else .15)
        right = profile(energy=.3)
        plan = transitions.Plan(preset="blend", overlap=8)
        mixplanner.adapt(plan, left, right, 120, 0, 1, 1)
        self.assertGreater(plan.echo_start, .5)
        self.assertGreater(plan.echo_mix, 0)
        self.assertLessEqual(plan.echo_mix, .18)
        for key in ("transitions.echo_enabled", "transitions.echo_in_blends"):
            self.settings[key] = False
            disabled = transitions.Plan(preset="blend", overlap=8)
            mixplanner.adapt(disabled, left, right, 120, 0, 1, 1)
            self.assertEqual(disabled.echo_mix, 0)
            self.settings.clear()

    def test_unknown_vocals_do_not_enable_extra_echo(self):
        plan = transitions.Plan(preset="blend", overlap=8)
        mixplanner.adapt(plan, profile(vocal=None, energy=lambda t: .8 if t < 116 else .15),
                         profile(vocal=None, energy=.3), 120, 0, 1, 1)
        self.assertEqual(plan.echo_mix, 0)

    def test_short_vocal_at_the_exit_suppresses_echo_despite_a_quiet_average(self):
        plan = transitions.Plan(preset="melt", overlap=8, echo_mix=.3)
        mixplanner.adapt(plan, profile(vocal=lambda t: 1 if t >= 119 else 0),
                         profile(vocal=0), 120, 0, 1, 1)
        self.assertEqual(plan.echo_mix, 0)

    def test_resealing_keeps_the_late_echo_window_and_levels(self):
        self.settings.update({"transitions.smart_cues": False, "transitions.preset": "blend",
                              "crossfade.duration": 8, "transitions.phrase_beats": 0})
        def track(key, data):
            return {"key": key, "duration": 120, "bpm": 120, "intro_sec": 30, "structure": data}
        schedule = timeline.Schedule()
        first = schedule.add_music("/a", track("a", profile(energy=lambda t: .8 if t < 116 else .15)))
        schedule.add_music("/b", track("b", profile(energy=.3)))
        schedule.seal()
        echo = dict(first.meta["echo"])
        self.assertGreater(echo["start"], 116)
        self.assertEqual(echo["end"], first.duration)
        schedule.seal()
        self.assertEqual(first.meta["echo"], echo)

    def test_new_controls_reject_nonfinite_or_out_of_range_values(self):
        for values in ({"transitions.vocal_eq_depth": 7},
                       {"transitions.vocal_collision_weight": float("nan")},
                       {"selection.compatibility.lookahead_depth": 5}):
            with self.assertRaises(ValueError):
                mixconfig.validate(values)
        self.assertEqual(mixconfig.validate({"selection.compatibility.energy_direction": "wave"}),
                         {"selection.compatibility.energy_direction": "wave"})
