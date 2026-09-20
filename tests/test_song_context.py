import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from radio import song_context


class SongContext(unittest.TestCase):
    def test_requires_song_and_artist_match(self):
        text = '"Royals" is a song by Lorde. ' + 'Background detail about the release. ' * 10
        response = Mock()
        response.json.return_value = {"query": {"pages": [{"title": "Royals (song)", "extract": text}]}}
        with patch.object(song_context.httpx, "get", return_value=response):
            self.assertIsNotNone(song_context.fetch({"title": "Royals", "artist": "Lorde"}))
            self.assertIsNone(song_context.fetch({"title": "Royals", "artist": "Someone Else"}))
            self.assertIsNone(song_context.fetch({"title": "Royal", "artist": "Lorde"}))

    def test_deadline_failure_is_cached_and_disable_never_looks_up(self):
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(song_context.config, "CACHE_DIR", Path(folder)), \
                patch.object(song_context.config.station, "get", return_value=True), \
                patch.object(song_context.sourceio, "_run", side_effect=RuntimeError("offline")) as lookup:
            track = {"title": "Royals", "artist": "Lorde"}
            self.assertIsNone(song_context.prepare(track))
            self.assertIsNone(song_context.prepare(track))
            lookup.assert_called_once_with("song_context", track, 7)
            with patch.object(song_context.config.station, "get", return_value=False):
                self.assertIsNone(song_context.prepare(track))
            self.assertEqual(lookup.call_count, 1)

    def test_inferred_artist_is_not_promoted_to_music_trivia(self):
        with patch.object(song_context.sourceio, "_run") as lookup:
            self.assertIsNone(song_context.prepare({"title": "Example", "artist": "Uploader",
                "metadata_sources": {"artist": "uploader_fallback"}}))
            lookup.assert_not_called()
