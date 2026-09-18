import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from radio.config import OverridableConfig


class SettingsAtomic(unittest.TestCase):
    def test_batch_preserves_unrelated_preferences_and_publishes_together(self):
        with tempfile.TemporaryDirectory() as folder:
            base, override = Path(folder) / "base.yaml", Path(folder) / "override.yaml"
            base.write_text("ducking:\n  target_gain: 0.1\n", encoding="utf-8")
            override.write_text("selection:\n  artist_separation: 6\n", encoding="utf-8")
            settings = OverridableConfig(base, override)
            settings.set_many({"selection.compatibility.genre_weight": 0.8,
                               "selection.compatibility.lyrics_weight": 0.3,
                               "transitions.native_key_lock": True})
            self.assertEqual(settings.get("selection.artist_separation"), 6)
            self.assertEqual(settings.get("ducking.target_gain"), 0.1)
            self.assertEqual(settings.get("selection.compatibility.genre_weight"), 0.8)
            self.assertTrue(settings.get("transitions.native_key_lock"))
            self.assertEqual(sorted(p.name for p in Path(folder).iterdir()), ["base.yaml", "override.yaml"])

    def test_failed_replace_keeps_file_and_cached_values_intact(self):
        with tempfile.TemporaryDirectory() as folder:
            base, override = Path(folder) / "base.yaml", Path(folder) / "override.yaml"
            base.write_text("{}", encoding="utf-8")
            override.write_text("selection:\n  compatibility:\n    genre_weight: 0.4\n", encoding="utf-8")
            settings = OverridableConfig(base, override)
            before = override.read_bytes()
            self.assertEqual(settings.get("selection.compatibility.genre_weight"), 0.4)
            with patch("radio.config.os.replace", side_effect=OSError("disk failure")):
                with self.assertRaises(OSError):
                    settings.set_many({"selection.compatibility.genre_weight": 0.9})
            self.assertEqual(override.read_bytes(), before)
            self.assertEqual(settings.get("selection.compatibility.genre_weight"), 0.4)
            self.assertFalse(list(Path(folder).glob("settings-*.yaml")))
