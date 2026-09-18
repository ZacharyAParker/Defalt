"""Transition selection and the automation it renders.

A transition is three independent choices -- volume, EQ, effects -- and the
named presets are combinations of those. These tests pin the behaviour that
would be audible if it broke: two basslines at once, a gain curve that dips in
the middle, or a filter left closed after the transition ends.
"""
import unittest
from tests.station_defaults import StationDefaults

from radio import analysis, transitions


def value_at(points, moment):
    if not points:
        return None
    if moment <= points[0][0]:
        return points[0][1]
    for (t0, v0), (t1, v1) in zip(points, points[1:]):
        if t0 <= moment <= t1:
            return v1 if t1 == t0 else v0 + (v1 - v0) * ((moment - t0) / (t1 - t0))
    return points[-1][1]


class TestCamelot(unittest.TestCase):
    def test_c_major_and_a_minor_are_the_same_wheel_position(self):
        self.assertEqual(analysis.camelot(0, "major"), "8B")
        self.assertEqual(analysis.camelot(9, "minor"), "8A")

    def test_relative_major_and_minor_mix(self):
        self.assertTrue(analysis.keys_compatible("8A", "8B"))

    def test_neighbours_on_the_wheel_mix(self):
        self.assertTrue(analysis.keys_compatible("8A", "9A"))
        self.assertTrue(analysis.keys_compatible("8A", "7A"))

    def test_the_wheel_wraps(self):
        self.assertTrue(analysis.keys_compatible("12A", "1A"))

    def test_distant_keys_do_not_mix(self):
        self.assertFalse(analysis.keys_compatible("8A", "2A"))
        self.assertFalse(analysis.keys_compatible("8A", "3B"))

    def test_an_unknown_key_is_never_compatible(self):
        self.assertFalse(analysis.keys_compatible("", "8A"))
        self.assertFalse(analysis.keys_compatible("8A", ""))


class TestTempoDistance(unittest.TestCase):
    def test_identical_tempos_are_zero_apart(self):
        self.assertAlmostEqual(transitions.tempo_distance(128, 128), 0.0)

    def test_a_double_time_reading_is_not_a_mismatch(self):
        """A detector reporting 172 for an 86 BPM track found the same pulse
        counted twice. Treating that as a huge gap would pick a hard cut for
        two records that would have beat-matched."""
        self.assertAlmostEqual(transitions.tempo_distance(86, 172), 0.0, places=6)
        self.assertAlmostEqual(transitions.tempo_distance(140, 70), 0.0, places=6)

    def test_genuinely_different_tempos_are_far_apart(self):
        self.assertGreater(transitions.tempo_distance(92, 136), 0.25)

    def test_an_unknown_tempo_is_maximally_distant(self):
        self.assertEqual(transitions.tempo_distance(0, 128), 1.0)


class TestChoosing(StationDefaults):
    def track(self, bpm, camelot, lufs=-14.0):
        return {"bpm": bpm, "camelot": camelot, "lufs": lufs}

    def test_matched_tempo_and_key_gets_a_long_blend(self):
        plan = transitions.choose(self.track(128, "8A"), self.track(129, "8A"))
        self.assertEqual(plan.preset, "blend")
        self.assertGreater(plan.overlap, 6.0)

    def test_matched_tempo_but_clashing_key_avoids_a_harmonic_blend(self):
        plan = transitions.choose(self.track(128, "8A"), self.track(129, "3B"))
        self.assertEqual(plan.preset, "wave")

    def test_a_big_tempo_jump_gets_a_short_hard_transition(self):
        plan = transitions.choose(self.track(92, "8A"), self.track(140, "3B"))
        self.assertEqual(plan.preset, "slam")
        self.assertLess(plan.overlap, 6.0)

    def test_no_analysis_still_produces_a_usable_plan(self):
        plan = transitions.choose(self.track(0, ""), self.track(0, ""))
        self.assertIn(plan.preset, transitions.PRESETS)
        self.assertGreater(plan.overlap, 0)

    def test_every_preset_is_renderable(self):
        for name in transitions.PRESETS:
            volume, eq, effects = transitions.preset_spec(name)
            self.assertIn(volume, transitions.VOLUME_MODES, name)
            self.assertIn(eq, transitions.EQ_MODES, name)
            plan = transitions.Plan(name, volume, eq, effects, 6.0)
            outgoing, incoming = transitions.render(plan, 6.0)
            self.assertTrue(outgoing.gain, name)
            self.assertTrue(incoming.gain, name)


class TestRendering(unittest.TestCase):
    def render(self, preset, length=6.0):
        volume, eq, effects = transitions.preset_spec(preset)
        plan = transitions.Plan(preset, volume, eq, effects, length)
        return transitions.render(plan, length)

    def test_a_crossfade_hands_over_completely(self):
        outgoing, incoming = self.render("fade")
        self.assertAlmostEqual(value_at(outgoing.gain, 0.0), 1.0, places=2)
        self.assertAlmostEqual(value_at(outgoing.gain, 6.0), 0.0, places=2)
        self.assertAlmostEqual(value_at(incoming.gain, 0.0), 0.0, places=2)
        self.assertAlmostEqual(value_at(incoming.gain, 6.0), 1.0, places=2)

    def test_equal_power_does_not_dip_in_the_middle(self):
        outgoing, incoming = self.render("fade")
        middle = (value_at(outgoing.gain, 3.0) ** 2
                  + value_at(incoming.gain, 3.0) ** 2)
        self.assertAlmostEqual(middle, 1.0, places=2)

    def test_only_one_track_has_bass_at_any_point(self):
        """Two basslines at once is the ugliest thing a crossfade can do."""
        for preset in ("fade", "rise", "wave", "melt"):
            outgoing, incoming = self.render(preset)
            for moment in (0.5, 1.5, 3.0, 4.5, 5.5):
                out_low = value_at(outgoing.low, moment)
                in_low = value_at(incoming.low, moment)
                both_up = out_low > -6 and in_low > -6
                self.assertFalse(both_up,
                                 f"{preset} at {moment}s: both basses up "
                                 f"({out_low:.1f} / {in_low:.1f} dB)")

    def test_the_bass_swap_lands_where_the_preset_says(self):
        _, fade_in = self.render("fade")
        _, rise_in = self.render("rise")
        # fade swaps at the midpoint, rise holds the old bass until the end
        self.assertGreater(value_at(fade_in.low, 4.0), value_at(rise_in.low, 4.0))

    def test_filters_end_open(self):
        """A sweep that does not return leaves the next record filtered."""
        outgoing, incoming = self.render("rise")
        self.assertGreater(value_at(incoming.lpf, 6.0), 15000)
        outgoing, incoming = self.render("melt")
        self.assertLess(value_at(incoming.hpf, 6.0), 40)

    def test_slam_is_a_swap_not_a_blend(self):
        outgoing, incoming = self.render("slam")
        self.assertEqual(value_at(incoming.gain, 1.0), 0.0)
        self.assertAlmostEqual(value_at(outgoing.gain, 5.0), 0.0, places=2)

    def test_blend_hands_the_highs_over_before_the_lows(self):
        outgoing, _ = self.render("blend")
        # Partway through, the outgoing highs are further down than its lows.
        self.assertLess(value_at(outgoing.high, 3.0), value_at(outgoing.low, 3.0))


if __name__ == "__main__":
    unittest.main()
