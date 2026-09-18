import json
import subprocess
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import config, db, importer, intent, library, pull, sourceio, wishes, youtube

VIDEO = "dQw4w9WgXcQ"
OTHER = "abcdefghijk"


class Links(unittest.TestCase):
    def test_supported_links_always_select_one_exact_video(self):
        for text in (f"https://youtu.be/{VIDEO}?si=abc", f"https://music.youtube.com/watch?v={VIDEO}&list=foo",
                     f"play https://www.youtube.com/watch?v={VIDEO}&t=50 please",
                     f"https://m.youtube.com/shorts/{VIDEO}", f"youtube.com/watch?v={VIDEO}",
                     f"https://www.youtube-nocookie.com/embed/{VIDEO}", f"https://youtube.com/live/{VIDEO}"):
            with self.subTest(text=text):
                routed = intent.route(text)
                self.assertFalse(routed.error)
                self.assertEqual(routed.extra["video_id"], VIDEO)
                self.assertEqual(routed.extra["url"], f"https://www.youtube.com/watch?v={VIDEO}")

    def test_non_video_or_lookalike_urls_are_rejected(self):
        for text in (f"https://evil.example/watch?v={VIDEO}", f"https://youtube.com.evil/watch?v={VIDEO}",
                     f"https://youtube.com@evil.test/watch?v={VIDEO}", "https://youtube.com/playlist?list=test",
                     f"https://youtube.com/watch?v={VIDEO}EXTRA", "https://youtube.com/@channel",
                     f"https://youtu.be/{VIDEO} https://youtu.be/{OTHER}"):
            self.assertTrue(intent.route(text).error, text)

    def test_normal_title_is_not_a_link(self):
        self.assertIsNone(youtube.parse("Artist - Title"))


class Metadata(unittest.TestCase):
    def test_structured_credits_win_and_upload_date_is_not_release_year(self):
        with patch.object(sourceio, "guess_metadata") as guess:
            result = youtube.metadata({"track": "Real Title", "artist": "Real Artist", "genre": "Jazz",
                                       "title": "Uploader - Clickbait", "upload_date": "20260501",
                                       "album": "The Album", "duration": 123}, VIDEO)
        guess.assert_not_called()
        self.assertEqual(result["title"], "Real Title")
        self.assertEqual(result["artist"], "Real Artist")
        self.assertIsNone(result["year"])
        self.assertEqual(result["expected_ms"], 123000)

    def test_title_cleanup_preserves_versions_and_labels_inference(self):
        result = youtube.metadata({"title": "Artist - Song (Live Remix) (Official Video) (4K Remaster)"}, VIDEO, infer=False)
        self.assertEqual(result["title"], "Song (Live Remix) (4K Remaster)")
        self.assertEqual(result["artist"], "Artist")
        self.assertEqual(json.loads(result["source_metadata"])["fields"]["artist"]["source"], "title_parse")

    def test_topic_channel_and_director_guesses_fill_only_missing_fields(self):
        guess = {"title": {"value": "The Song", "confidence": .9, "evidence": "The Song"},
                 "artist": {"value": "Wrong", "confidence": 1, "evidence": "The Song"},
                 "genre": {"value": "Jazz", "confidence": .8, "evidence": "jazz piano"}, "year": 2026}
        with patch.object(sourceio, "guess_metadata", return_value=guess):
            result = youtube.metadata({"title": "The Song", "channel": "The Artist - Topic",
                                       "description": "A jazz piano recording."}, VIDEO)
        self.assertEqual(result["artist"], "The Artist")
        self.assertEqual(result["genre"], "Jazz")
        self.assertIsNone(result["year"])
        fields = json.loads(result["source_metadata"])["fields"]
        self.assertEqual(fields["title"]["source"], "director_inferred")
        self.assertEqual(fields["genre"]["source"], "director_inferred")

    def test_invented_evidence_and_low_confidence_are_not_saved(self):
        with patch.object(sourceio, "guess_metadata", return_value={
                "artist": {"value": "Famous Artist", "confidence": .99, "evidence": "not in the video"},
                "genre": {"value": "Jazz", "confidence": .1, "evidence": "mystery"}}):
            result = youtube.metadata({"title": "mystery", "uploader": "Upload Channel"}, VIDEO)
        self.assertEqual(result["artist"], "Upload Channel")
        self.assertEqual(result["genre"], "")
        self.assertEqual(json.loads(result["source_metadata"])["fields"]["artist"]["source"], "uploader_fallback")

    def test_failed_metadata_uses_oembed_then_safe_fallback(self):
        with patch.object(sourceio, "describe", side_effect=sourceio.SourceError("unavailable")), \
             patch.object(sourceio, "oembed", return_value={"title": "Artist - Song"}), \
             patch.object(sourceio, "guess_metadata", side_effect=sourceio.SourceError("timeout")):
            result = youtube.describe(VIDEO)
        self.assertEqual(result["title"], "Song")
        self.assertEqual(result["video_id"], VIDEO)

    def test_live_or_wrong_video_is_not_queued_as_finished_record(self):
        for info in ({"live_status": "is_live"}, {"live_status": "is_upcoming"}, {"id": OTHER}):
            with patch.object(sourceio, "describe", return_value=info):
                with self.assertRaises(sourceio.SourceError):
                    youtube.describe(VIDEO)

    def test_metadata_and_inference_have_wall_clock_limits(self):
        with patch.object(sourceio, "_run", return_value={}) as worker:
            sourceio.describe(VIDEO)
            self.assertEqual(worker.call_args.args[-1], 35)
            sourceio.guess_metadata({})
            self.assertEqual(worker.call_args.args[-1], 20)


class YouTubeRequests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        local = threading.local()
        for mock in (patch.object(db, "_DB_PATH", self.root / "test.db"), patch.object(db, "_LOCAL", local),
                     patch.object(library, "AUDIO_DIR", self.root / "cache"),
                     patch.object(config.station, "get", side_effect=lambda k, d=None: d),
                     patch("radio.taste.record")):
            mock.start()
            self.addCleanup(mock.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        db.connect()

    def test_different_links_are_distinct_and_same_link_is_deduplicated_without_network(self):
        with patch.object(youtube, "describe") as describe:
            a = wishes.submit(f"https://youtu.be/{VIDEO}")
            b = wishes.submit(f"https://youtu.be/{OTHER}")
            again = wishes.submit(f"https://youtube.com/watch?v={VIDEO}&list=ignored")
        describe.assert_not_called()
        self.assertNotEqual(a["key"], b["key"])
        self.assertEqual(a["key"], again["key"])
        self.assertEqual(len(db.query("SELECT * FROM requests")), 2)

    def test_radio_downloads_exact_link_even_when_clean_versions_are_avoided(self):
        result = wishes.submit(f"https://youtu.be/{VIDEO}")
        track = dict(db.one("SELECT * FROM tracks WHERE key=?", (result["key"],)))
        meta = youtube.metadata({"track": "Exact Version", "artist": "Artist", "genre": "Rock"}, VIDEO, infer=False)
        with patch.object(youtube, "describe", return_value=meta), \
             patch.object(library, "resolve") as resolve, \
             patch.object(library, "_download_raw", return_value=None) as download:
            library._ensure_locked(track)
        resolve.assert_not_called()
        self.assertEqual(download.call_args.args[0], VIDEO)
        row = db.one("SELECT * FROM tracks WHERE key=?", (result["key"],))
        self.assertEqual(row["title"], "Exact Version")
        self.assertEqual(row["video_id"], VIDEO)

    def test_console_uses_exact_video_and_does_not_search_title(self):
        meta = youtube.metadata({"title": "Artist - Track"}, VIDEO, infer=False)
        with patch.object(pull, "music_dir", return_value=self.root), patch.object(pull, "emit"), \
             patch.object(youtube, "describe", return_value=meta), \
             patch.object(library, "resolve") as resolve, \
             patch.object(library, "_download_raw", return_value=None) as download:
            self.assertEqual(pull.pull(f"https://youtu.be/{VIDEO}"), 1)
        resolve.assert_not_called()
        self.assertEqual(download.call_args.args[0], VIDEO)

    def test_metadata_is_resolved_once_and_identity_stays_stable(self):
        key = youtube.reserve(youtube.parse(f"https://youtu.be/{VIDEO}"))
        meta = youtube.metadata({"title": "Artist - Song"}, VIDEO, infer=False)
        with patch.object(youtube, "describe", return_value=meta) as describe:
            first = youtube.hydrate(dict(db.one("SELECT * FROM tracks WHERE key=?", (key,))))
            second = youtube.hydrate(dict(db.one("SELECT * FROM tracks WHERE key=?", (key,))))
        self.assertEqual(describe.call_count, 1)
        self.assertEqual(first["key"], second["key"])
        self.assertEqual(second["title"], "Song")

    def test_real_flac_tags_round_trip_and_imports_preserve_video_identity(self):
        path = self.root / "record.flac"
        subprocess.run([config.FFMPEG, "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
                        str(path)], check=True, capture_output=True)
        meta = youtube.metadata({"track": "Song", "artist": "Artist", "genre": "Rock", "album": "Album"}, VIDEO, infer=False)
        self.assertTrue(pull.tag(path, "Artist", "Song", meta))
        values = importer.metadata(path)
        self.assertEqual(values["video_id"], VIDEO)
        self.assertEqual(values["source_metadata"], meta["source_metadata"])
        imported = importer.import_file(path)
        self.assertEqual(imported["key"], youtube.key_for(VIDEO))
        self.assertEqual(importer.import_file(path)["key"], imported["key"])


if __name__ == "__main__":
    unittest.main()
