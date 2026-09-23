import json
import math
import random
import re
import struct
import subprocess
import tempfile
import threading
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

from radio import config, director, mixconfig, timeline, transitions, tts
from tests.test_transitions import value_at


class SpeechAndMix(unittest.TestCase):
    def setUp(self):
        self.settings = dict(mixconfig.PROFILES["Smooth DJ"])
        self.mock = patch.object(config.station, "get", side_effect=lambda key, default=None:
            ({k.split(".", 1)[1]: v for k, v in self.settings.items() if k.startswith("ducking.")}
             if key == "ducking" else self.settings.get(key, default)))
        self.mock.start()
        self.addCleanup(self.mock.stop)

    def track(self, key, bpm=120):
        return dict(key=key, title=key, duration=120, intro_sec=20, bpm=bpm,
                    camelot="8A", beat_period=60/bpm, beat_offset=0.1,
                    beat_residual_ms=10, bpm_confidence=0.9)

    def test_later_appended_music_ducks_under_existing_speech(self):
        s = timeline.Schedule()
        a = s.add_music("a", self.track("a"))
        s.add_voice("voice", 114, 12, meta={"text": "Host line", "host": "Mav"})
        s.duck_all_overlapping(114, 126)
        b = s.add_music("b", self.track("b"))
        s.seal()
        self.assertLessEqual(value_at(b.envelope, 122-b.start_at), 0.101)
        self.assertGreater(value_at(b.meta["deck_envelope"], 122-b.start_at), 0.9)
        self.assertLessEqual(value_at(a.envelope, 115), 0.101)

    def test_long_break_keeps_exact_attack_hold_and_release(self):
        points = timeline.build_gain_envelope(120, None, 0, None, 0, [timeline._Duck(10, 70)])
        self.assertAlmostEqual(value_at(points, 10), 0.1, places=3)
        self.assertAlmostEqual(value_at(points, 70.4), 0.1, places=3)
        self.assertAlmostEqual(value_at(points, 71.6), 1.0, places=3)
        self.assertGreater(value_at(points, 9.65), 0.99)

    def test_short_gaps_between_hosts_do_not_pump_the_music(self):
        s = timeline.Schedule()
        a = s.add_music("a", self.track("a"))
        s.add_voice("v1", 10, 5)
        s.add_voice("v2", 15.5, 5)
        s.seal()
        self.assertAlmostEqual(value_at(a.envelope, 15.45), 0.1, places=3)

    def test_tempo_matching_changes_duration_and_preserves_phase(self):
        s = timeline.Schedule()
        a = s.add_music("a", self.track("a", 120))
        # 122 BPM stays inside the pitch band that keeps the shared key; a
        # wider gap is capped for harmony (see test_timeline).
        b = s.add_music("b", self.track("b", 122))
        rate = b.meta["playback_rate"]
        self.assertAlmostEqual(rate, 120/122, places=4)
        self.assertAlmostEqual(b.duration * rate, 120, places=4)
        phase = (b.start_at + 0.1/rate - (a.start_at + 0.1)) % 0.5
        self.assertLess(min(phase, 0.5-phase), 0.001)
        self.assertLess(b.start_at, a.end_at)
        self.assertIn("beats aligned", b.meta["transition"]["reason"])

    def test_unreliable_grids_and_disabled_matching_keep_original_speed(self):
        for weak in (True, False):
            s = timeline.Schedule()
            s.add_music("a", self.track("a"))
            track = self.track("b", 124)
            if weak: track["beat_residual_ms"] = 100
            else: self.settings["transitions.tempo_match"] = False
            b = s.add_music("b", track)
            self.assertEqual(b.meta["playback_rate"], 1.0)

    def test_every_profile_validates_and_effect_switches_are_respected(self):
        for profile in mixconfig.PROFILES.values():
            self.assertEqual(mixconfig.validate(profile), profile)
        self.settings.update(mixconfig.PROFILES["Clean radio"])
        plan = transitions.choose(self.track("a", 92), self.track("b", 140))
        self.assertFalse(plan.effects)
        self.assertEqual(plan.echo_mix, 0)
        self.settings["transitions.eq_enabled"] = False
        plan = transitions.choose(self.track("a"), self.track("b"))
        out, incoming = transitions.render(plan, 6)
        self.assertTrue(all(point[1] == 0 for point in out.low + incoming.low))

    def test_invalid_config_is_rejected_before_writes(self):
        for invalid in ({"transitions.echo_feedback": 0.99}, {"ducking.target_gain": math.nan},
                        {"transitions.eq_enabled": "false"}, {"unknown": 1},
                        {"transitions.phrase_beats": 4.5}):
            with self.assertRaises(ValueError): mixconfig.validate(invalid)

    def test_actual_quiet_audio_is_levelled_and_cached_without_replacing_source(self):
        with tempfile.TemporaryDirectory() as folder:
            source = Path(folder) / "quiet.wav"
            with wave.open(str(source), "wb") as wav:
                wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
                wav.writeframes(b"".join(struct.pack("<h", int(32767 * 0.02 * math.sin(i * 2*math.pi*440/48000)))
                                       for i in range(48000*4)))
            original = source.read_bytes()
            result = tts.levelled(source)
            output = Path(result["path"])
            self.assertNotEqual(source, output)
            measured = subprocess.run([config.FFMPEG, "-nostdin", "-hide_banner", "-i", str(output),
                "-af", "loudnorm=I=-16:TP=-1.5:LRA=7:print_format=json", "-f", "null", "-"],
                capture_output=True, text=True, check=True)
            report = json.loads(re.findall(r"\{[^{}]+\}", measured.stderr)[-1])
            self.assertLess(abs(float(report["input_i"]) + 16), 1.0)
            self.assertLess(float(report["input_tp"]), -1)
            mtime = output.stat().st_mtime_ns
            self.assertEqual(tts.levelled(source)["path"], str(output))
            self.assertEqual(output.stat().st_mtime_ns, mtime)
            self.assertEqual(source.read_bytes(), original)


class Transcript(unittest.TestCase):
    def test_current_lines_are_exposed_future_lines_hidden_and_history_survives_trim(self):
        s = director.Station.__new__(director.Station)
        s.lock = threading.RLock()
        s.clock = director.Clock()
        s.schedule = timeline.Schedule()
        s.schedule.add_voice("one", 10, 4, meta={"host": "Mav", "text": "First line"})
        s.schedule.add_voice("two", 20, 4, meta={"host": "Rue", "text": "Next line"})
        s.clock.jump(11)
        rows = s.transcript()
        self.assertEqual(len(rows), 1)
        self.assertTrue(rows[0]["active"])
        self.assertEqual(rows[0]["text"], "First line")
        s.clock.jump(10)
        self.assertEqual(len(s.transcript()), 2)
        s.clock.jump(60)
        s.schedule.trim_before(s.clock.now())
        self.assertEqual(len(s.transcript()), 2)
        self.assertFalse(any(row["active"] for row in s.transcript()))


if __name__ == "__main__":
    unittest.main()
