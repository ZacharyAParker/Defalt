"""Beat grid detection and beat matching.

Synthetic signals with a known grid, so these assert against ground truth
rather than against whatever the detector happened to produce.
"""
import unittest
from tests.station_defaults import StationDefaults

import numpy as np

from radio import analysis, config, transitions


def click_track(bpm: float, seconds: float = 20.0, jitter: float = 0.0,
                seed: int = 0) -> np.ndarray:
    """A pulse train at a known tempo, optionally with timing jitter."""
    rng = np.random.default_rng(seed)
    samples = np.zeros(int(analysis.SAMPLE_RATE * seconds), dtype=np.float32)
    period = 60.0 / bpm
    time = 0.1
    while time < seconds - 0.1:
        offset = rng.normal(0, jitter) if jitter else 0.0
        index = int((time + offset) * analysis.SAMPLE_RATE)
        if 0 <= index < len(samples) - 400:
            # A short burst of noise reads as a transient to the onset detector.
            samples[index:index + 400] += rng.normal(0, 1, 400).astype(np.float32)
        time += period
    return samples


class TestBeatTracking(unittest.TestCase):
    def grid(self, bpm, **kwargs):
        spectrum = analysis._spectrogram(click_track(bpm, **kwargs))
        return analysis.detect_beats(spectrum, bpm)

    def test_a_steady_click_track_gives_the_right_period(self):
        for bpm in (90.0, 120.0, 140.0):
            found = self.grid(bpm)["beat_period"]
            self.assertAlmostEqual(found, 60.0 / bpm, places=2,
                                   msg=f"{bpm} BPM")

    def test_a_steady_grid_has_a_small_residual(self):
        """Residual is the confidence measure, so it has to mean something."""
        self.assertLess(self.grid(120.0)["beat_residual_ms"], 30)

    def test_a_sloppy_grid_reports_a_large_residual(self):
        loose = self.grid(120.0, jitter=0.08)["beat_residual_ms"]
        tight = self.grid(120.0)["beat_residual_ms"]
        self.assertGreater(loose, tight)

    def test_silence_reports_no_grid(self):
        spectrum = analysis._spectrogram(
            np.zeros(analysis.SAMPLE_RATE * 5, dtype=np.float32))
        self.assertEqual(analysis.detect_beats(spectrum, 120.0)["beat_period"], 0.0)

    def test_no_tempo_means_no_grid(self):
        spectrum = analysis._spectrogram(click_track(120.0))
        self.assertEqual(analysis.detect_beats(spectrum, 0.0)["beat_period"], 0.0)


class TestTempoMatch(StationDefaults):
    def track(self, bpm, residual=10.0, period=None):
        return {"bpm": bpm, "beat_period": period or (60.0 / bpm if bpm else 0),
                "beat_offset": 0.0, "beat_residual_ms": residual}

    def test_a_small_difference_is_pitched_into_line(self):
        rate, why = transitions.tempo_match(self.track(128), self.track(124))
        self.assertEqual(why, "")
        self.assertAlmostEqual(rate, 128 / 124, places=4)

    def test_a_large_difference_is_refused(self):
        rate, why = transitions.tempo_match(self.track(90), self.track(140))
        self.assertEqual(rate, 1.0)
        self.assertIn("too far", why)

    def test_a_double_time_reading_is_matched_at_its_own_octave(self):
        """172 against 86 is the same pulse, so it needs almost no pitching."""
        rate, why = transitions.tempo_match(self.track(86), self.track(172))
        self.assertEqual(why, "")
        self.assertAlmostEqual(rate, 1.0, places=3)

    def test_an_unknown_tempo_is_left_alone(self):
        self.assertEqual(transitions.tempo_match(self.track(0), self.track(128))[0],
                         1.0)


class TestBeatNudge(StationDefaults):
    def track(self, bpm, offset=0.0, residual=10.0):
        return {"bpm": bpm, "beat_period": 60.0 / bpm, "beat_offset": offset,
                "beat_residual_ms": residual}

    def test_a_wandering_grid_is_refused(self):
        """Aligning to noise is worse than not aligning at all."""
        delta, why = transitions.beat_nudge(
            self.track(128, residual=400), self.track(128), 100.0, 6.0)
        self.assertEqual(delta, 0.0)
        self.assertIn("wander", why)

    def test_no_grid_is_refused(self):
        delta, why = transitions.beat_nudge(
            {"beat_period": 0}, self.track(128), 100.0, 6.0)
        self.assertEqual(delta, 0.0)
        self.assertIn("no grid", why)

    def test_mismatched_tempos_are_refused_without_a_rate(self):
        delta, why = transitions.beat_nudge(
            self.track(128), self.track(96), 100.0, 9.0)
        self.assertEqual(delta, 0.0)
        self.assertIn("drift", why)

    def test_the_nudge_never_exceeds_half_a_beat(self):
        period = 60.0 / 128
        for offset in (0.0, 0.05, 0.17, 0.4):
            delta, why = transitions.beat_nudge(
                self.track(128), self.track(128, offset=offset), 61.234, 6.0)
            self.assertEqual(why, "")
            self.assertLessEqual(abs(delta), period / 2 + 1e-6,
                                 f"offset {offset} moved {delta:.3f}s")

    def test_aligning_puts_the_incoming_beat_on_an_outgoing_beat(self):
        out = self.track(120)             # beats every 0.5s from 0
        incoming = self.track(120, offset=0.2)
        out_local = 61.3                  # mid-record, not on a beat
        delta, why = transitions.beat_nudge(out, incoming, out_local, 6.0)
        self.assertEqual(why, "")
        # Where the incoming record's first beat now lands, in outgoing time.
        landed = out_local + delta + 0.2
        remainder = (landed - out["beat_offset"]) % out["beat_period"]
        distance = min(remainder, out["beat_period"] - remainder)
        self.assertLess(distance, 0.005, f"landed {distance * 1000:.0f}ms off")

    def test_alignment_can_be_switched_off(self):
        config.station.set("transitions.beat_align", False)
        try:
            delta, why = transitions.beat_nudge(
                self.track(128), self.track(128), 100.0, 6.0)
            self.assertEqual(delta, 0.0)
            self.assertEqual(why, "disabled")
        finally:
            config.station.set("transitions.beat_align", True)


if __name__ == "__main__":
    unittest.main()
