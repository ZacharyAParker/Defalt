import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import httpx

from radio import config, tts


class SpeechFallback(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        # Failures bench a model process-wide; keep tests independent.
        tts._COOLDOWN.clear()
        self.addCleanup(tts._COOLDOWN.clear)
        self.root = Path(self.temp.name)
        self.voice = {"engine": "openrouter", "model": "google/gemini-3.1-flash-tts-preview",
                      "openrouter_voice": "Charon", "instructions": "Dry, deadpan.",
                      "name": "en-US-AndrewMultilingualNeural", "rate": "-6%",
                      "fallback": {"engine": "edge", "name": "en-US-AndrewMultilingualNeural", "rate": "-6%"}}

    def test_fallback_does_not_poison_primary_cache_and_primary_recovers(self):
        paths = []
        async def edge(text, path, voice):
            paths.append((path, dict(voice), text))
            path.write_bytes(b"e" * 1024)
        def hosted(text, path, voice):
            path.write_bytes(b"g" * 1024)
            return True
        with patch.object(tts, "VOICE_DIR", self.root), \
             patch.object(tts, "_duration", return_value=2), \
             patch.object(tts, "levelled", side_effect=lambda p: {"path": str(p), "duration": 2}), \
             patch.object(tts, "_synthesise", side_effect=edge), \
             patch.object(tts, "_say_openrouter", return_value=False) as render:
            first = tts.say("Hello.", self.voice)
            self.assertTrue(first["fallback"])
            self.assertEqual(first["voice"], self.voice["name"])
            self.assertEqual(paths[0][1]["rate"], "-6%")
            self.assertIsNone(paths[0][1]["instructions"])
            self.assertEqual(paths[0][2], "Hello.")
            self.assertFalse((self.root / f'{tts._key("Hello.", self.voice)}.mp3').exists())
            render.side_effect = hosted
            second = tts.say("Hello.", self.voice)
            self.assertFalse(second["fallback"])
            self.assertNotEqual(first["path"], second["path"])
            tts.say("Hello.", self.voice)
            self.assertEqual(render.call_count, 2)

    def test_hosted_previous_model_can_be_fallback(self):
        voice = {**self.voice, "fallback": {"engine": "openrouter", "model": "old/model", "openrouter_voice": "old"}}
        def render(text, path, settings):
            if settings["model"] != "old/model":
                return False
            path.write_bytes(b"a" * 1024)
            return True
        with patch.object(tts, "VOICE_DIR", self.root), patch.object(tts, "_duration", return_value=2), \
             patch.object(tts, "levelled", return_value={"duration": 2}), \
             patch.object(tts, "_say_openrouter", side_effect=render):
            result = tts.say("Hello.", voice)
        self.assertEqual(result["model"], "old/model")
        self.assertTrue(result["fallback"])

    def test_direction_and_station_model_changes_invalidate_cache(self):
        self.assertNotEqual(tts._key("Hello", self.voice),
                            tts._key("Hello", {**self.voice, "instructions": "Excited"}))
        with patch.object(config.station, "get", side_effect=lambda key, default=None:
                          {"tts.backend": "openrouter", "tts.openrouter.model": "one", "tts.openrouter.voice": "a"}.get(key, default)):
            first = tts._key("Hello", {})
        with patch.object(config.station, "get", side_effect=lambda key, default=None:
                          {"tts.backend": "openrouter", "tts.openrouter.model": "two", "tts.openrouter.voice": "a"}.get(key, default)):
            self.assertNotEqual(first, tts._key("Hello", {}))

    def test_json_errors_and_http_failures_are_not_audio(self):
        for response in [httpx.Response(200, json={"error": "x" * 1024}),
                         httpx.Response(429, content=b"x" * 1024)]:
            with patch.object(config, "env", return_value="test-key"), \
                 patch.object(tts.httpx, "post", return_value=response):
                output = self.root / "bad.mp3"
                self.assertFalse(tts._say_openrouter("Hello", output, self.voice))
                self.assertFalse(output.exists())

    def test_gemini_pcm_is_converted_to_playable_mp3(self):
        import math
        import struct
        pcm = b"".join(struct.pack("<h", int(3000 * math.sin(i * 2 * math.pi * 440 / 24000)))
                       for i in range(24000))
        response = httpx.Response(200, content=pcm, headers={"content-type": "audio/pcm"})
        with patch.object(config, "env", return_value="test-key"), \
             patch.object(tts.httpx, "post", return_value=response) as post:
            output = self.root / "good.mp3"
            self.assertTrue(tts._say_openrouter("Hello", output, self.voice))
            self.assertAlmostEqual(tts._duration(output), 1, delta=.1)
            payload = post.call_args.kwargs["json"]
            self.assertEqual(payload["response_format"], "pcm")
            self.assertIn("Transcript:\nHello", payload["input"])
            self.assertNotIn("speed", payload)

    def test_corrupt_mp3_is_not_cached(self):
        response = httpx.Response(200, content=b"not audio" * 1000, headers={"content-type": "audio/mpeg"})
        with patch.object(config, "env", return_value="test-key"), \
             patch.object(tts.httpx, "post", return_value=response):
            output = self.root / "bad.mp3"
            self.assertFalse(tts._say_openrouter("Hello", output, self.voice))
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
