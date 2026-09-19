import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from radio import artwork


class Artwork(unittest.TestCase):
    def setUp(self):
        self.track = {"key":"artist|song", "artist":"Artist", "title":"Song", "video_id":"afRO9M6q0X4"}

    def test_thumbnail_ids_cannot_be_urls_or_paths(self):
        for value in [None,"","../private","https://other.test/image","abc?secret=1"]:
            self.assertEqual(artwork.youtube_thumbnail_urls(value), [])
        self.assertEqual(len(artwork.youtube_thumbnail_urls(self.track["video_id"])),2)

    def test_spotify_precedes_youtube_only_for_a_matching_record(self):
        result={"artist":"Artist", "title":"Song", "artwork":"https://i.scdn.co/image/test"}
        with patch.object(artwork.spotify,"available",return_value=True), patch.object(artwork.spotify,"search",return_value=[result]):
            self.assertEqual(list(artwork.candidates(self.track))[0][1],"Spotify album cover")
            result["title"]="Unrelated song"
            self.assertEqual(list(artwork.candidates(self.track))[0][1],"YouTube thumbnail")

    def test_missing_or_failing_spotify_still_uses_youtube(self):
        with patch.object(artwork.spotify,"available",return_value=False):
            self.assertEqual(list(artwork.candidates(self.track))[0][1],"YouTube thumbnail")
        with patch.object(artwork.spotify,"available",return_value=True), patch.object(artwork.spotify,"search",side_effect=ValueError("rate limit")):
            self.assertEqual(list(artwork.candidates(self.track))[0][1],"YouTube thumbnail")

    def test_missing_maxres_uses_hq_and_is_cached(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(artwork.config,"CACHE_DIR",Path(tmp)), \
             patch.object(artwork.db,"one",return_value=self.track), patch.object(artwork.spotify,"available",return_value=False), \
             patch.object(artwork,"_misses",{}), patch.object(artwork,"_download",side_effect=[None,b"png"]) as download:
            result=artwork.resolve(self.track["key"])
            self.assertEqual(result[1],"YouTube thumbnail")
            self.assertEqual(result[0].read_bytes(),b"png")
            self.assertEqual(artwork.resolve(self.track["key"]),result)
            self.assertEqual(download.call_count,2)
            self.assertTrue(download.call_args.args[0].endswith("hqdefault.jpg"))

    def test_unknown_tracks_and_external_thumbnail_hosts_are_rejected(self):
        with patch.object(artwork.db,"one",return_value=None):
            self.assertIsNone(artwork.resolve("missing"))
        with patch.object(artwork.httpx,"stream") as request:
            self.assertIsNone(artwork._download("https://localhost/private"))
            self.assertIsNone(artwork._download("http://i.ytimg.com/vi/id/hqdefault.jpg"))
            request.assert_not_called()
