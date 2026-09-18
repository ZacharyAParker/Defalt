import json
import tempfile
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

import numpy as np

from radio import config, structure


class AcousticSections(unittest.TestCase):
    def tone(self, seconds=30, hz=100, amplitude=0.3):
        t = np.arange(round(seconds * structure.SAMPLE_RATE)) / structure.SAMPLE_RATE
        return (amplitude * np.sin(2 * np.pi * hz * t)).astype(np.float32)

    def test_detects_energy_change_without_inventing_vocals_or_choruses(self):
        signal = self.tone()
        signal[:10 * structure.SAMPLE_RATE] *= 0.1
        result = structure.analyse(signal)
        self.assertTrue(result["complete"])
        self.assertEqual(result["vocal_source"], "unknown")
        self.assertTrue(all(bin_["vocal"] is None for bin_ in result["bins"]))
        self.assertTrue(any(abs(p["at"] - 10) <= 0.5 for p in result["boundaries"]))
        self.assertLess(structure.at(result, 3)["energy"], 0.15)
        self.assertGreater(structure.at(result, 13)["energy"], 0.95)
        self.assertNotIn("chorus", json.dumps(result))

    def test_bass_change_is_independent_of_overall_energy(self):
        signal = np.concatenate([self.tone(10, hz=100), self.tone(10, hz=1000)])
        result = structure.analyse(signal)
        self.assertAlmostEqual(structure.at(result, 4)["energy"],
                               structure.at(result, 14)["energy"], places=3)
        self.assertGreater(structure.at(result, 4)["bass"], 0.95)
        self.assertLess(structure.at(result, 14)["bass"], 0.01)
        self.assertTrue(any(abs(p["at"] - 10) <= 0.5 for p in result["boundaries"]))

    def test_long_song_offers_deeper_entry_and_exit_regions(self):
        result = structure.analyse(self.tone(240))
        self.assertTrue(any(45 < p["at"] < 100 for p in result["entries"]))
        self.assertTrue(any(120 <= p["at"] < 150 for p in result["exits"]))
        self.assertTrue(all(p["at"] <= 240 * .49 for p in result["entries"]))

    def test_vocal_activity_requires_existing_stem_and_exposes_quiet_window(self):
        mixture = self.tone()
        vocal = self.tone(hz=600, amplitude=0.15)
        vocal[:8 * structure.SAMPLE_RATE] *= 0.001
        result = structure.analyse(mixture, vocals=vocal)
        self.assertEqual(result["vocal_source"], "existing_stem")
        self.assertEqual(structure.at(result, 4)["vocal"], 0)
        self.assertGreater(structure.at(result, 12)["vocal"], 0.9)
        quiet = next(p for p in result["entries"] if p["at"] == 4)
        singing = next(p for p in result["entries"] if p["at"] == 10)
        self.assertGreater(quiet["score"], singing["score"])

    def test_partial_stem_does_not_claim_missing_audio_is_instrumental(self):
        result = structure.analyse(self.tone(), vocals=self.tone(8))
        self.assertEqual(result["vocal_source"], "unknown")
        self.assertIsNone(structure.at(result, 25)["vocal"])

    def test_silence_has_no_cues_and_nonfinite_input_is_rejected(self):
        result = structure.analyse(np.zeros(structure.SAMPLE_RATE * 3))
        self.assertEqual(result["entries"], [])
        self.assertEqual(result["exits"], [])
        self.assertEqual(result["boundaries"], [])
        with self.assertRaises(ValueError):
            structure.analyse(np.array([0, np.nan, 1]))

    def test_long_record_is_explicitly_incomplete_and_bounded(self):
        with patch.object(structure, "MAX_SECONDS", 10):
            result = structure.analyse(self.tone(12))
            self.assertFalse(result["complete"])
            self.assertEqual(result["duration"], 10)
            self.assertEqual(len(result["bins"]), 20)
            self.assertEqual(structure.at(result, 10), {})

    def test_out_of_range_queries_preserve_unknown_data(self):
        result = structure.analyse(self.tone(3.25))
        self.assertEqual(result["bins"][-1]["end"], 3.25)
        for t in (-1, 3.25, 10, float("nan"), float("inf")):
            self.assertEqual(structure.at(result, t), {})

    def write_wav(self, path, seconds=5):
        with wave.open(str(path), "wb") as stream:
            stream.setnchannels(1)
            stream.setsampwidth(2)
            stream.setframerate(structure.SAMPLE_RATE)
            stream.writeframes((self.tone(seconds) * 32767).astype("<i2").tobytes())

    def test_real_decode_cache_reuse_and_source_invalidation(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "source.wav"
            self.write_wav(path)
            with patch.object(config, "CACHE_DIR", root / "cache"):
                result = structure.profile(path)
                self.assertEqual(result["duration"], 5)
                self.assertTrue(structure.cache_path(path).is_file())
                with patch.object(structure, "_decode", side_effect=AssertionError("cache decoded")):
                    self.assertEqual(structure.profile(path), result)
                    self.assertEqual(structure.profile_for({"file": str(path)}), result)
                    self.write_wav(path, 6)
                    self.assertEqual(structure.profile_for({"file": str(path)}), {})
                self.assertEqual(structure.profile(path)["duration"], 6)

    def test_added_or_changed_vocal_stem_invalidates_cache(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path, vocal = root / "source.wav", root / "vocals.wav"
            self.write_wav(path)
            self.write_wav(vocal)
            with patch.object(config, "CACHE_DIR", root / "cache"), \
                    patch("radio.stems.existing", return_value=None) as existing:
                structure.profile(path)
                existing.return_value = [vocal]
                self.assertEqual(structure.profile_for({"file": str(path)}), {})
                result = structure.profile(path)
                self.assertEqual(result["vocal_source"], "existing_stem")
                self.write_wav(vocal, 6)
                self.assertEqual(structure.profile_for({"file": str(path)}), {})

    def test_read_only_profile_never_decodes_and_corrupt_cache_falls_back(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "source.wav"
            self.write_wav(path)
            with patch.object(config, "CACHE_DIR", root / "cache"), \
                    patch.object(structure, "_decode", side_effect=AssertionError("reader decoded")):
                self.assertEqual(structure.profile_for({"file": str(path)}), {})
                cache = structure.cache_path(path)
                cache.parent.mkdir(parents=True)
                cache.write_text("{broken", encoding="utf-8")
                self.assertEqual(structure.profile_for({"file": str(path)}), {})
                identity, _ = structure._identity(path)
                result = structure.analyse(self.tone(5))
                result["bins"][0]["energy"] = float("nan")
                cache.write_text(json.dumps({"fingerprint": identity, "profile": result}), encoding="utf-8")
                self.assertEqual(structure.profile_for({"file": str(path)}), {})

    def test_decode_failure_is_safe_and_writes_no_cache(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "broken.wav"
            path.write_bytes(b"not audio")
            with patch.object(config, "CACHE_DIR", root / "cache"):
                self.assertEqual(structure.profile(path), {})
                self.assertFalse(structure.cache_path(path).exists())


if __name__ == "__main__":
    unittest.main()
