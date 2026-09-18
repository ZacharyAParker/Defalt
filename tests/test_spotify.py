import json
import unittest
from unittest.mock import patch, MagicMock

from radio import db, spotify, wishes
from tests import test_youtube


class Catalog(unittest.TestCase):
    def setUp(self):
        self.env = patch.object(spotify.config, "env", side_effect=lambda k, d="": "test" if k.startswith("SPOTIFY_") else d)
        self.env.start()
        self.addCleanup(self.env.stop)
        for mock in (patch.object(spotify, "_token", None), patch.object(spotify, "_cache", {})):
            mock.start()
            self.addCleanup(mock.stop)

    def response(self, data, status=200):
        return MagicMock(status_code=status, json=lambda: data)

    def test_search_preserves_credits_duration_and_caches_token_and_results(self):
        result = {"tracks": {"items": [{"name": "Song", "artists": [{"name": "One"}, {"name": "Two"}],
                   "album": {"name": "Album", "release_date": "2024-03-02"}, "duration_ms": 123456}]}}
        with patch.object(spotify.httpx, "post", return_value=self.response({"access_token": "test", "expires_in": 3600})) as post, \
             patch.object(spotify.httpx, "get", return_value=self.response(result)) as get:
            tracks = spotify.search("Song")
            self.assertEqual(spotify.search("Song"), tracks)
            spotify.search("Song two")
        self.assertEqual(post.call_count, 1)
        self.assertEqual(get.call_count, 2)
        self.assertEqual(tracks[0], {"artist": "One, Two", "title": "Song", "album": "Album", "year": "2024", "duration_ms": 123456})

    def test_short_queries_and_links_never_reach_spotify(self):
        with patch.object(spotify.httpx, "post") as post:
            for query in ("", "a", "https://youtu.be/dQw4w9WgXcQ", "youtube.com/watch?v=dQw4w9WgXcQ"):
                self.assertEqual(spotify.search(query), [])
        post.assert_not_called()

    def test_missing_credentials_and_rate_limit_are_readable(self):
        with patch.object(spotify, "available", return_value=False):
            with self.assertRaisesRegex(ValueError, "credentials"):
                spotify.search("Song")
        with patch.object(spotify.httpx, "post", return_value=self.response({}, 429)):
            with self.assertRaisesRegex(ValueError, "rate limiting"):
                spotify.search("Song")

    def test_stale_selection_and_invalid_duration_are_rejected(self):
        for query, selection in (("Artist - Other", {"artist": "Artist", "title": "Song"}),
                                  ("Artist - Song", {"artist": "Artist", "title": "Song", "duration_ms": -1})):
            with self.assertRaises(ValueError):
                spotify.selected(query, selection)


class SelectionQueue(unittest.TestCase):
    setUp = test_youtube.YouTubeRequests.setUp

    def test_chosen_record_stores_duration_and_metadata_before_queueing(self):
        selection = {"artist": "Artist", "title": "Song", "album": "Album", "year": "2024", "duration_ms": 123000}
        result = wishes.submit("Artist - Song", selection=selection)
        self.assertTrue(result["ok"])
        row = db.one("SELECT * FROM tracks WHERE key=?", (result["key"],))
        self.assertEqual(row["expected_ms"], 123000)
        self.assertEqual(row["album"], "Album")
        self.assertEqual(row["year"], 2024)
        self.assertEqual(json.loads(row["source_metadata"])["fields"]["title"]["source"], "spotify_catalog")

    def test_stale_selection_does_not_queue_another_song(self):
        result = wishes.submit("Artist - New Song", selection={"artist": "Artist", "title": "Old Song", "duration_ms": 120000})
        self.assertFalse(result["ok"])
        self.assertEqual(db.query("SELECT * FROM requests"), [])


if __name__ == "__main__":
    unittest.main()
