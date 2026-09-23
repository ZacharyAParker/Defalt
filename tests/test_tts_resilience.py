"""Speech rendering: atomic cache, circuit breaker, parallel breaks, levelling."""
import math
import shutil
import struct
import subprocess
import tempfile
import threading
import time
import unittest
import wave
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import httpx

from radio import config, showclock, tts

HOSTED = {"engine": "openrouter", "model": "google/gemini-3.1-flash-tts-preview",
          "openrouter_voice": "Charon", "instructions": "You are Mav, a male late-night radio host.",
          "name": "en-US-AndrewMultilingualNeural",
          "fallback": {"engine": "edge", "name": "en-US-AndrewMultilingualNeural"}}


class Isolated(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        tts._COOLDOWN.clear()
        self.addCleanup(tts._COOLDOWN.clear)
        for p in (patch.object(tts, "VOICE_DIR", self.root),
                  patch.object(tts, "levelled", side_effect=lambda p: {"path": str(p), "duration": 2})):
            p.start()
            self.addCleanup(p.stop)


class AtomicCache(Isolated):
    def test_interrupted_edge_stream_is_never_cached(self):
        async def partial(text, path, voice):
            path.write_bytes(b"x" * 4096)  # half a file, then the socket dies
            raise ConnectionResetError
        with patch.object(tts, "_synthesise", side_effect=partial), \
                patch.object(tts, "_duration", return_value=3):
            self.assertIsNone(tts.say("Hello there.", {"engine": "edge", "name": "en-US-GuyNeural"}))
        self.assertEqual([p for p in self.root.iterdir() if p.suffix in {".mp3", ".part"}], [])

    def test_edge_timeout_benches_edge_and_leaves_nothing_behind(self):
        async def slow(text, path, voice):
            path.write_bytes(b"x" * 4096)
            raise TimeoutError
        with patch.object(tts, "_synthesise", side_effect=slow) as synth, \
                patch.object(tts, "_duration", return_value=3):
            self.assertIsNone(tts.say("Hello.", {"engine": "edge", "name": "en-US-GuyNeural"}))
            self.assertIn("edge", tts.benched())
            calls = synth.call_count
            self.assertIsNone(tts.say("Another line.", {"engine": "edge", "name": "en-US-GuyNeural"}))
            self.assertEqual(synth.call_count, calls)  # benched: no second wait
        self.assertFalse(list(self.root.glob("*.mp3")))

    def test_complete_edge_render_is_moved_into_place(self):
        async def good(text, path, voice):
            self.assertTrue(path.name.endswith(".part"))
            path.write_bytes(b"x" * 4096)
        with patch.object(tts, "_synthesise", side_effect=good), \
                patch.object(tts, "_duration", return_value=3):
            result = tts.say("Hello.", {"engine": "edge", "name": "en-US-GuyNeural"})
        self.assertTrue(Path(result["path"]).exists())
        self.assertFalse(list(self.root.glob("*.part")))


class CircuitBreaker(Isolated):
    def post(self, status=None, error=None):
        def respond(*args, **kwargs):
            if error:
                raise error
            return httpx.Response(status, content=b"x" * 1024)
        return patch.object(tts.httpx, "post", side_effect=respond)

    def test_server_errors_and_timeouts_bench_the_model_and_use_short_timeout(self):
        for status, error in ((503, None), (429, None), (400, None),
                              (None, httpx.ReadTimeout("slow"))):
            tts._COOLDOWN.clear()
            with patch.object(config, "env", return_value="key"), self.post(status, error) as post:
                self.assertFalse(tts._say_openrouter("Hi", self.root / "a.mp3", HOSTED))
            self.assertLessEqual(post.call_args.kwargs["timeout"], 30)
            self.assertIn("openrouter:" + HOSTED["model"], tts.benched())

    def test_benched_model_is_skipped_straight_to_fallback(self):
        tts._bench("openrouter:" + HOSTED["model"], 60)
        async def edge(text, path, voice):
            path.write_bytes(b"e" * 1024)
        with patch.object(tts, "_say_openrouter") as hosted, \
                patch.object(tts, "_synthesise", side_effect=edge), \
                patch.object(tts, "_duration", return_value=2):
            result = tts.say("Hello.", HOSTED)
        hosted.assert_not_called()
        self.assertTrue(result["fallback"])

    def test_timeout_setting_is_bounded(self):
        for value, expected in ((120, 60), (1, 5), ("junk", 25), (20, 20)):
            with patch.object(config.station, "get", side_effect=lambda k, d=None: value if k == "tts.timeout_seconds" else d):
                self.assertEqual(tts._timeout(), expected)


class Batches(Isolated):
    def test_lines_render_in_parallel_and_keep_their_order(self):
        active, peak, lock = [0], [0], threading.Lock()
        def slow(text, voice, *, start=0):
            with lock:
                active[0] += 1
                peak[0] = max(peak[0], active[0])
            time.sleep(.05)
            with lock:
                active[0] -= 1
            return {"path": text, "duration": 1, "candidate": 0}
        with patch.object(tts, "say", side_effect=slow):
            results = tts.say_many([(f"line {i}", {}) for i in range(6)])
        self.assertEqual([r["path"] for r in results], [f"line {i}" for i in range(6)])
        self.assertEqual(peak[0], 3)

    def test_a_host_that_falls_back_once_stays_on_the_fallback_all_break(self):
        calls = []
        def render(text, voice, *, start=0):
            calls.append((text, start))
            if text == "second" and start == 0:
                return {"path": "edge-second", "duration": 1, "candidate": 1}
            return {"path": f"{text}-{start}", "duration": 1, "candidate": max(start, 0)}
        other = {"engine": "edge", "name": "rue"}
        with patch.object(tts, "say", side_effect=render):
            results = tts.say_many([("first", HOSTED), ("second", HOSTED), ("third", other), ("fourth", HOSTED)])
        self.assertEqual([r["candidate"] for r in results], [1, 1, 0, 1])
        self.assertIn(("first", 1), calls)
        self.assertIn(("fourth", 1), calls)
        self.assertNotIn(("third", 1), calls)  # the other host keeps its own voice

    def test_required_line_failure_is_reported(self):
        lines = [SimpleNamespace(text="joke", required=False), SimpleNamespace(text="Song, by Artist.", required=True)]
        self.assertTrue(tts.required_failed(lines, [{"path": "a"}, None]))
        self.assertFalse(tts.required_failed(lines, [None, {"path": "b"}]))

    def test_one_crashing_line_does_not_sink_the_batch(self):
        def render(text, voice, *, start=0):
            if text == "bad":
                raise RuntimeError("boom")
            return {"path": text, "duration": 1, "candidate": 0}
        with patch.object(tts, "say", side_effect=render):
            self.assertEqual([bool(r) for r in tts.say_many([("ok", {}), ("bad", {})])], [True, False])


class Preparation(unittest.TestCase):
    def test_shouting_is_spoken_as_words_but_acronyms_survive(self):
        self.assertEqual(tts.speakable("MAV. MAV. IT'S THE SAME BASSLINE"),
                         "Mav. Mav. It's The Same Bassline")
        self.assertEqual(tts.speakable("AI on TV. NASA, PS5, OK."), "AI on TV. NASA, PS5, OK.")
        self.assertEqual(tts.speakable("I said no."), "I said no.")

    def test_transcript_keeps_capitals_but_cache_key_uses_spoken_form(self):
        seen = []
        async def edge(text, path, voice):
            seen.append(text)
            path.write_bytes(b"e" * 1024)
        with tempfile.TemporaryDirectory() as folder, patch.object(tts, "VOICE_DIR", Path(folder)), \
                patch.object(tts, "_synthesise", side_effect=edge), \
                patch.object(tts, "_duration", return_value=2), \
                patch.object(tts, "levelled", side_effect=lambda p: {"path": str(p), "duration": 2}):
            tts.say("MAV. MAV.", {"engine": "edge", "name": "x"})
        self.assertEqual(seen, ["Mav. Mav."])

    def test_late_night_direction_only_at_night(self):
        evening = time.mktime((2026, 9, 22, 19, 0, 0, 0, 0, -1))
        night = time.mktime((2026, 9, 22, 23, 30, 0, 0, 0, -1))
        self.assertIn("evening radio host", tts._for_daypart(HOSTED, evening)["instructions"])
        self.assertIn("late-night radio host", tts._for_daypart(HOSTED, night)["instructions"])
        custom = {**HOSTED, "instructions_by_daypart": {"morning": "Bright breakfast energy."}}
        morning = time.mktime((2026, 9, 22, 8, 0, 0, 0, 0, -1))
        self.assertEqual(tts._for_daypart(custom, morning)["instructions"], "Bright breakfast energy.")
        self.assertEqual(showclock.voice_daypart(night), "late-night")


class Levelling(unittest.TestCase):
    def test_duration_is_probed_once_and_cached_beside_the_file(self):
        with tempfile.TemporaryDirectory() as folder:
            clip = Path(folder) / "line.mp3"
            clip.write_bytes(b"x" * 2048)
            with patch.object(tts, "_probe", return_value=4.25) as probe:
                self.assertEqual(tts._duration(clip), 4.25)
                self.assertEqual(tts._duration(clip), 4.25)
            self.assertEqual(probe.call_count, 1)
            clip.write_bytes(b"y" * 4096)  # a replaced file is probed again
            with patch.object(tts, "_probe", return_value=1.5):
                self.assertEqual(tts._duration(clip), 1.5)

    def test_uses_configured_ffprobe(self):
        with patch.object(config, "FFPROBE", "custom-ffprobe", create=True), \
                patch.object(tts.subprocess, "run", side_effect=OSError) as run:
            tts._probe(Path("x.mp3"))
        self.assertEqual(run.call_args.args[0][0], "custom-ffprobe")

    def test_two_pass_linear_loudnorm_pads_short_clips(self):
        calls = []
        measured = ('{"input_i" : "-30.1", "input_tp" : "-12.0", "input_lra" : "2.0", '
                    '"input_thresh" : "-40.5", "target_offset" : "0.4"}')
        def run(args, **kwargs):
            calls.append(args)
            if "-f" in args and args[args.index("-f") + 1] == "null":
                return subprocess.CompletedProcess(args, 0, "", "[loudnorm]\n" + measured)
            Path(args[-1]).write_bytes(b"z" * 2048)
            return subprocess.CompletedProcess(args, 0, b"", b"")
        with tempfile.TemporaryDirectory() as folder:
            clip = Path(folder) / "short.mp3"
            clip.write_bytes(b"x" * 2048)
            with patch.object(tts.subprocess, "run", side_effect=run), \
                    patch.object(tts, "_probe", return_value=1.2):
                result = tts.levelled(clip)
        first, second = calls
        self.assertIn("apad=whole_dur=", first[first.index("-af") + 1])
        chain = second[second.index("-af") + 1]
        self.assertIn("linear=true", chain)
        self.assertIn("measured_I=-30.1", chain)
        self.assertIn("atrim=0:1.200", chain)
        self.assertIn("-voice-v2-", result["path"])

    @unittest.skipUnless(shutil.which(config.FFMPEG), "ffmpeg not installed")
    def test_short_and_long_lines_come_out_equally_loud(self):
        with tempfile.TemporaryDirectory() as folder:
            outputs = []
            for seconds in (1.0, 8.0):
                source = Path(folder) / f"tone-{seconds}.wav"
                with wave.open(str(source), "wb") as out:
                    out.setnchannels(1)
                    out.setsampwidth(2)
                    out.setframerate(24000)
                    out.writeframes(b"".join(struct.pack("<h", int(6000 * math.sin(i * 2 * math.pi * 300 / 24000)))
                                             for i in range(int(24000 * seconds))))
                mp3 = source.with_suffix(".mp3")
                subprocess.run([config.FFMPEG, "-nostdin", "-v", "error", "-y", "-i", str(source), str(mp3)], check=True)
                outputs.append(Path(tts.levelled(mp3)["path"]))
            loudness = [tts._measure(path, f"apad=whole_dur={tts.MIN_MEASURE_SECONDS},", -16, -1.5)["input_i"]
                        for path in outputs]
        self.assertTrue(all(p.name.count("-voice-v2-") == 1 for p in outputs))
        self.assertLess(abs(loudness[0] - loudness[1]), 1.5)
        self.assertLess(abs(loudness[1] + 16), 1.5)


if __name__ == "__main__":
    unittest.main()
