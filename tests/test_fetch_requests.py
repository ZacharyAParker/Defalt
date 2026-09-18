"""Requests must not be starved, lost on failure, or resurrected after cancel."""
import json
import random
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from radio import config, db, director, importer, library, pull, sourceio, timeline


class RequestFeederTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        local = threading.local()
        for p in (patch.object(db, "_DB_PATH", Path(folder.name) / "station.db"),
                  patch.object(db, "_LOCAL", local),
                  patch.object(config.station, "get", side_effect=lambda k, d=None: d)):
            p.start()
            self.addCleanup(p.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        db.write("INSERT INTO tracks(key,title,artist,source,added_at) VALUES ('verity','It is me Verity','Horror Skunx','request',0)")
        self.request_id = db.write("INSERT INTO requests(ts,query,track_key) VALUES (0,'Verity','verity')")
        s = self.station = director.Station.__new__(director.Station)
        s.lock = threading.RLock()
        s.clock = director.Clock()
        s.schedule = timeline.Schedule()
        s.rng = random.Random(1)
        s._recent_keys = []
        s._last_heartbeat = time.time()
        s._lineup = [{"track": {"key": str(i)}, "source": "auto"} for i in range(5)]
        self.ready = {"key": "verity", "title": "Verity", "duration": 123, "file": "verity.flac"}

    def state(self):
        return dict(db.one("SELECT * FROM requests WHERE id=?", (self.request_id,)))

    def test_request_bypasses_full_automatic_queue(self):
        with patch.object(library, "ensure", return_value=self.ready):
            self.assertTrue(self.station._feed_once())
        self.assertEqual(self.station._lineup[0]["track"]["key"], "verity")
        self.assertEqual(self.state()["status"], "queued")

    def test_failure_is_reported_and_another_request_can_be_processed(self):
        with patch.object(library, "ensure", side_effect=sourceio.SourceError("search timed out")):
            self.assertFalse(self.station._feed_once())
        self.assertEqual(self.state()["status"], "failed")
        self.assertIn("timed out", self.state()["note"])
        db.write("UPDATE requests SET status='pending' WHERE id=?", (self.request_id,))
        with patch.object(library, "ensure", return_value=self.ready):
            self.assertTrue(self.station._feed_once())
        self.assertEqual(self.state()["status"], "queued")
        self.assertIsNone(self.state()["note"])

    def test_request_stays_preparing_until_audio_is_ready(self):
        def prepare(track):
            self.assertEqual(self.state()["status"], "preparing")
            return self.ready
        with patch.object(library, "ensure", side_effect=prepare):
            self.station._feed_once()
        self.assertEqual(self.state()["status"], "queued")

    def test_cancel_during_download_does_not_enqueue_it_afterwards(self):
        def prepare(track):
            db.write("UPDATE requests SET status='cancelled' WHERE id=?", (self.request_id,))
            return self.ready
        with patch.object(library, "ensure", side_effect=prepare):
            self.station._feed_once()
        self.assertEqual(len(self.station._lineup), 5)
        self.assertEqual(self.state()["status"], "cancelled")

    def test_full_buffer_without_requests_does_not_download_more(self):
        db.write("UPDATE requests SET status='cancelled'")
        with patch.object(library, "ensure") as prepare:
            self.assertFalse(self.station._feed_once())
            prepare.assert_not_called()


class SourceWorkerTests(unittest.TestCase):
    def test_console_retry_recovers_saved_audio_without_another_download(self):
        with tempfile.TemporaryDirectory() as folder:
            saved = Path(folder) / 'Horror Skunx - Verity.flac'
            saved.write_bytes(b'saved audio')
            with patch.object(pull, 'music_dir', return_value=Path(folder)), \
                 patch.object(db, 'one', return_value=None), \
                 patch.object(importer, 'metadata', return_value={'artist': 'Horror Skunx', 'title': 'Verity'}), \
                 patch.object(importer, 'import_file', return_value={'key': 'horror skunx|verity'}) as imported, \
                 patch.object(library, 'resolve') as resolve, patch.object(pull, 'emit') as emit:
                self.assertEqual(pull.pull('Horror Skunx - Verity'), 0)
                imported.assert_called_once_with(saved)
                resolve.assert_not_called()
                self.assertEqual(emit.call_args.args[0], 'done')

    def test_blank_exception_has_a_visible_console_error(self):
        with patch.object(pull, 'pull', side_effect=ValueError()), patch.object(pull, 'emit') as emit:
            self.assertEqual(pull.main(['song']), 1)
            self.assertEqual(emit.call_args.kwargs['error'], 'ValueError')

    def test_timeout_stops_worker_and_reports_retryable_failure(self):
        child = MagicMock(pid=123, returncode=0)
        child.communicate.side_effect = [subprocess.TimeoutExpired('worker', 45), ('', '')]
        with patch.object(sourceio.subprocess, "Popen", return_value=child), \
             patch.object(sourceio.subprocess, "run") as kill:
            with self.assertRaisesRegex(sourceio.SourceError, "timed out.*retry"):
                sourceio.search("song", {})
        child.kill.assert_called_once()
        self.assertEqual(child.communicate.call_args_list[0].kwargs["timeout"], 45)

    def test_error_output_does_not_masquerade_as_no_matches(self):
        child = MagicMock(returncode=1)
        child.communicate.return_value = ('', 'ERROR: service unavailable')
        with patch.object(sourceio.subprocess, "Popen", return_value=child):
            with self.assertRaisesRegex(sourceio.SourceError, 'service unavailable'):
                sourceio.search('song', {})

    def test_simultaneous_fetches_get_distinct_files_and_partial_files_are_not_audio(self):
        with tempfile.TemporaryDirectory() as folder:
            base = Path(folder) / '.raw_video'
            def download(video_id, options):
                Path(options['outtmpl'].replace('%(ext)s', 'm4a')).write_bytes(b'audio')
            with patch.object(sourceio, 'download', side_effect=download):
                a = library._download_raw('video', base)
                b = library._download_raw('video', base)
            self.assertNotEqual(a, b)
            self.assertEqual(a.read_bytes(), b'audio')
            self.assertEqual(b.read_bytes(), b'audio')

    def test_failed_download_cleans_only_its_own_partial_file(self):
        with tempfile.TemporaryDirectory() as folder:
            base = Path(folder) / '.raw_video'
            other = Path(folder) / '.raw_video_other.m4a'
            other.write_bytes(b'other fetch')
            def fail(video_id, options):
                Path(options['outtmpl'].replace('%(ext)s', 'm4a.part')).write_bytes(b'partial')
                raise sourceio.SourceError('timed out')
            with patch.object(sourceio, 'download', side_effect=fail):
                with self.assertRaises(sourceio.SourceError):
                    library._download_raw('video', base)
            self.assertEqual(list(Path(folder).iterdir()), [other])
