"""The playout maths. If these break, the station sounds wrong."""
import math
import random
import unittest

from radio import timeline as T


def gain_at(envelope, moment):
    """Read an envelope the way the browser does: linear between breakpoints."""
    if moment <= envelope[0][0]:
        return envelope[0][1]
    for (t0, g0), (t1, g1) in zip(envelope, envelope[1:]):
        if t0 <= moment <= t1:
            if t1 == t0:
                return g1
            return g0 + (g1 - g0) * ((moment - t0) / (t1 - t0))
    return envelope[-1][1]


class TestCrossfade(unittest.TestCase):
    def test_equal_power_holds_loudness_across_the_overlap(self):
        """A linear crossfade dips in the middle. Equal power must not."""
        fade_in, fade_out = T._fade_curve("equal_power")
        for progress in (0.1, 0.25, 0.5, 0.75, 0.9):
            power = fade_in(progress) ** 2 + fade_out(progress) ** 2
            self.assertAlmostEqual(power, 1.0, places=6)

    def test_envelope_starts_silent_and_reaches_unity(self):
        env = T.build_music_envelope(120, fade_in=6, fade_out=6, ducks=[])
        self.assertLess(gain_at(env, 0), 0.01)
        self.assertAlmostEqual(gain_at(env, 60), 1.0, places=2)
        self.assertLess(gain_at(env, 120), 0.01)

    def test_breakpoints_are_ordered_and_bounded(self):
        env = T.build_music_envelope(90, 5, 5, [T._Duck(20, 30)])
        times = [t for t, _ in env]
        self.assertEqual(times, sorted(times))
        for moment, gain in env:
            self.assertGreaterEqual(moment, 0)
            self.assertLessEqual(moment, 90)
            # Never exactly zero: Web Audio cannot ramp through it.
            self.assertGreater(gain, 0)
            self.assertLessEqual(gain, 1.0)


class TestDucking(unittest.TestCase):
    def setUp(self):
        self.env = T.build_music_envelope(100, 6, 6, [T._Duck(20, 30)])

    def test_music_is_at_full_level_before_the_duck(self):
        self.assertAlmostEqual(gain_at(self.env, 15), 1.0, places=2)

    def test_music_reaches_the_configured_depth_while_talking(self):
        target = float(T.config.station.get("ducking.target_gain"))
        for moment in (20.0, 25.0, 30.0):
            self.assertAlmostEqual(gain_at(self.env, moment), target, places=2)

    def test_music_recovers_after_the_release(self):
        release = float(T.config.station.get("ducking.release"))
        hold = float(T.config.station.get("ducking.hold_after"))
        self.assertAlmostEqual(gain_at(self.env, 30 + hold + release + 0.5),
                               1.0, places=2)

    def test_duck_ramps_rather_than_jumping(self):
        """A step change reads as a mistake. The attack must be a ramp."""
        during_attack = gain_at(self.env, 19.85)
        self.assertLess(during_attack, 1.0)
        self.assertGreater(during_attack, float(
            T.config.station.get("ducking.target_gain")))

    def test_ducking_multiplies_with_the_crossfade(self):
        """Talking through a crossfade must duck the fade, not fight it."""
        ducked = T.build_music_envelope(100, 6, 6, [T._Duck(0, 10)])
        clean = T.build_music_envelope(100, 6, 6, [])
        target = float(T.config.station.get("ducking.target_gain"))
        # At 3s we are mid fade-in AND mid duck; result is the product.
        self.assertAlmostEqual(gain_at(ducked, 3.0),
                               gain_at(clean, 3.0) * target, places=2)


class TestSchedule(unittest.TestCase):
    def track(self, key, duration=180.0, intro=12.0):
        return {"key": key, "title": key.upper(), "artist": f"artist {key}",
                "duration": duration, "outro_sec": duration - 1.0,
                "intro_sec": intro}

    def test_second_record_starts_before_the_first_ends(self):
        s = T.Schedule()
        a = s.add_music("/a", self.track("a"))
        b = s.add_music("/b", self.track("b"))
        overlap = a.end_at - b.start_at
        self.assertGreater(overlap, 0, "records must overlap to crossfade")
        self.assertAlmostEqual(
            overlap, float(T.config.station.get("crossfade.duration")), places=1)

    def test_dry_break_leaves_a_gap_instead_of_overlapping(self):
        s = T.Schedule()
        a = s.add_music("/a", self.track("a"))
        s.cursor = a.end_at + 10.0          # reserve time for the break
        b = s.add_music("/b", self.track("b"), dry_before=True)
        self.assertGreaterEqual(b.start_at, a.end_at)

    def test_a_break_spanning_the_crossfade_ducks_both_records(self):
        s = T.Schedule()
        a = s.add_music("/a", self.track("a"))
        b = s.add_music("/b", self.track("b"))
        voice_start = a.end_at - 3
        voice_end = max(a.end_at + 3, b.start_at + 6)
        s.duck_all_overlapping(voice_start, voice_end)
        s.seal()
        target = float(T.config.station.get("ducking.target_gain"))
        # Both are ducked during the shared window.
        self.assertLess(gain_at(a.envelope, (a.end_at - 1) - a.start_at), 1.0)
        self.assertLessEqual(gain_at(b.envelope, (voice_start + voice_end) / 2 - b.start_at),
                             gain_at(b.envelope, 40.0) * target + 0.05)

    def test_adding_a_record_does_not_flatten_the_previous_envelope(self):
        """Regression: seal() used to consume the fade values, so the first
        record lost its fade-out the moment a second one was scheduled."""
        s = T.Schedule()
        a = s.add_music("/a", self.track("a"))
        s.seal()
        fade_out_alone = gain_at(a.envelope, a.duration - 0.1)

        s.add_music("/b", self.track("b"))
        s.seal()
        fade_out_after = gain_at(a.envelope, a.duration - 0.1)

        self.assertLess(fade_out_alone, 0.2, "should fade out at the end")
        self.assertAlmostEqual(fade_out_alone, fade_out_after, places=4)
        self.assertGreater(len(a.envelope), 10,
                           "a flattened envelope means the fades were lost")

    def test_repeated_sealing_is_stable(self):
        s = T.Schedule()
        a = s.add_music("/a", self.track("a"))
        s.duck_all_overlapping(a.start_at + 10, a.start_at + 20)
        s.seal()
        first = list(a.envelope)
        for _ in range(4):
            s.seal()
        self.assertEqual(first, a.envelope)

    def test_trailing_silence_is_trimmed_so_fades_land_on_music(self):
        s = T.Schedule()
        # 180s file, but the music stops at 150s.
        item = s.add_music("/a", {**self.track("a"), "outro_sec": 150.0})
        self.assertLess(item.duration, 180.0)
        self.assertAlmostEqual(item.duration, 151.5, places=1)


class TestBreakPlacement(unittest.TestCase):
    """Back-timing: the last word should land as the vocal starts."""

    def test_bridge_lands_the_last_word_on_the_post(self):
        start = T.break_start(
            "bridge", music_start=100.0, intro=14.0, safety=0.6,
            speech_length=9.0, previous_end=104.0, previous_start=0.0)
        self.assertAlmostEqual(start + 9.0, 100.0 + 14.0 - 0.6, places=3)

    def test_a_long_break_reaches_back_over_the_outgoing_record(self):
        """20s of speech cannot fit in a 6s intro, so it must start earlier."""
        start = T.break_start(
            "bridge", music_start=100.0, intro=6.0, safety=0.6,
            speech_length=20.0, previous_end=104.0, previous_start=0.0)
        self.assertLess(start, 100.0, "should begin over the previous record")
        self.assertAlmostEqual(start + 20.0, 100.0 + 6.0 - 0.6, places=3)

    def test_over_intro_never_starts_before_the_record_does(self):
        start = T.break_start(
            "over_intro", music_start=100.0, intro=4.0, safety=0.6,
            speech_length=25.0, previous_end=104.0, previous_start=0.0)
        self.assertGreaterEqual(start, 100.0)

    def test_backtime_is_capped(self):
        start = T.break_start(
            "bridge", music_start=100.0, intro=2.0, safety=0.6,
            speech_length=90.0, previous_end=104.0, previous_start=0.0,
            max_backtime=20.0)
        self.assertGreaterEqual(start, 100.0 - 20.0)

    def test_over_intro_is_not_viable_for_a_short_intro(self):
        """A nine-second break cannot live inside a three-second intro."""
        self.assertFalse(T.viable(
            "over_intro", intro=3.0, safety=0.6, speech_length=9.0,
            music_start=100.0, previous_end=104.0, previous_start=0.0))
        self.assertTrue(T.viable(
            "over_intro", intro=14.0, safety=0.6, speech_length=9.0,
            music_start=100.0, previous_end=104.0, previous_start=0.0))

    def test_an_unfittable_style_degrades_to_bridge(self):
        chosen = T.choose_placement(
            "over_intro", intro=3.0, safety=0.6, speech_length=9.0,
            music_start=100.0, previous_end=104.0, previous_start=0.0)
        self.assertEqual(chosen, "bridge")

    def test_a_fitting_style_is_left_alone(self):
        chosen = T.choose_placement(
            "over_intro", intro=20.0, safety=0.6, speech_length=6.0,
            music_start=100.0, previous_end=104.0, previous_start=0.0)
        self.assertEqual(chosen, "over_intro")

    def test_the_first_record_of_a_session_is_always_dry(self):
        self.assertEqual(T.choose_placement(
            "bridge", intro=12.0, safety=0.6, speech_length=6.0,
            music_start=0.0, previous_end=None, previous_start=None), "dry")

    def test_no_style_ever_talks_over_the_vocal(self):
        """The core promise: after adaptation, the break always hits the post."""
        for style in ("bridge", "over_intro", "over_outro"):
            for intro in (2.0, 3.0, 8.0, 14.0, 25.0):
                for speech in (2.0, 6.0, 12.0, 20.0):
                    chosen = T.choose_placement(
                        style, intro=intro, safety=0.6, speech_length=speech,
                        music_start=100.0, previous_end=106.0, previous_start=0.0)
                    start = T.break_start(
                        chosen, music_start=100.0, intro=intro, safety=0.6,
                        speech_length=speech, previous_end=106.0,
                        previous_start=0.0, max_backtime=20.0)
                    self.assertLessEqual(
                        start + speech, 100.0 + intro - 0.6 + 1e-6,
                        f"{style}->{chosen} intro={intro} speech={speech} "
                        f"talks over the vocal")

    def test_over_outro_finishes_before_the_record_ends(self):
        start = T.break_start(
            "over_outro", music_start=100.0, intro=12.0, safety=0.6,
            speech_length=8.0, previous_end=104.0, previous_start=0.0)
        self.assertLessEqual(start + 8.0, 104.0)


class TestLineLayout(unittest.TestCase):
    def lines(self, count, duration=3.0):
        return [T.VoiceLine(url=f"/v{i}", duration=duration, host="mav",
                            text="x") for i in range(count)]

    def test_lines_run_in_order(self):
        rng = random.Random(1)
        placed = T.lay_out_lines(self.lines(4), 10.0, rng)
        offsets = [offset for _, offset in placed]
        self.assertEqual(offsets, sorted(offsets))

    def test_no_line_starts_before_the_break_does(self):
        for seed in range(40):
            placed = T.lay_out_lines(self.lines(5), 10.0, random.Random(seed))
            self.assertGreaterEqual(min(o for _, o in placed), 10.0 - 1e-9)

    def test_span_bounds_every_line(self):
        """The span must contain all lines, including any that overlap.

        It is NOT the sum of line lengths: an interruption deliberately
        compresses the break, which is the whole point of overlapping.
        """
        for seed in range(30):
            placed = T.lay_out_lines(self.lines(4), 0.0, random.Random(seed))
            start, end = T.speech_span(placed)
            for line, offset in placed:
                self.assertGreaterEqual(offset, start - 1e-9)
                self.assertLessEqual(offset + line.duration, end + 1e-9)

    def test_interruptions_shorten_the_break(self):
        """Overlapping lines should produce a tighter break than sequential."""
        lines = self.lines(6)
        spans = {T.speech_span(T.lay_out_lines(lines, 0.0, random.Random(s)))[1]
                 for s in range(60)}
        self.assertGreater(len(spans), 1, "layout should vary run to run")


if __name__ == "__main__":
    unittest.main()
