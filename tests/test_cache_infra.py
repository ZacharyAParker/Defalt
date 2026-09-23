"""The audio cache and everything else under cache/: atomic renders, eviction,
janitor protection and the housekeeping passes."""
import os
import random
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from radio import config, db, director, housekeeping, library, timeline


class TempCache(unittest.TestCase):
    """A private database and cache/ tree, and settings from a dict."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.audio = self.root / "audio"
        self.audio.mkdir()
        self.local = threading.local()
        self.settings = {}
        for patcher in (patch.object(db, "_DB_PATH", self.root / "test.db"),
                        patch.object(db, "_LOCAL", self.local),
                        patch.object(config, "CACHE_DIR", self.root),
                        patch.object(library, "AUDIO_DIR", self.audio),
                        patch.object(config.station, "get",
                                     side_effect=lambda k, d=None: self.settings.get(k, d))):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(lambda: getattr(self.local, "conn", None) and self.local.conn.close())

    def file(self, name, size=2048, age=0.0, folder=None):
        path = (folder or self.audio) / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"x" * size)
        if age:
            stamp = time.time() - age
            os.utime(path, (stamp, stamp))
        return path

    def track(self, key, **values):
        values = {"title": key, "artist": "A", "added_at": 0, **values}
        db.write(f"INSERT INTO tracks (key, {', '.join(values)}) VALUES (?{', ?' * len(values)})",
                 (key, *values.values()))


class AtomicRender(TempCache):
    def fake_ffmpeg(self, write=b"", code=0, raises=None):
        def run(args, timeout=300):
            if raises:
                Path(args[-1]).write_bytes(b"partial")
                raise raises
            if write:
                Path(args[-1]).write_bytes(write)
            return SimpleNamespace(returncode=code, stderr="boom")
        return patch.object(library, "_ffmpeg", side_effect=run)

    def leftovers(self):
        return [p.name for p in self.audio.iterdir()]

    def test_failed_render_leaves_no_partial_file_under_the_final_name(self):
        target = self.audio / "abc.m4a"
        with self.fake_ffmpeg(write=b"half a record", code=1):
            self.assertFalse(library._render(self.root / "raw.webm", target, 0.0, "m4a"))
        self.assertEqual(self.leftovers(), [])

    def test_timeout_and_tiny_output_are_failures_too(self):
        target = self.audio / "abc.m4a"
        with self.fake_ffmpeg(raises=subprocess.TimeoutExpired("ffmpeg", 1)):
            self.assertFalse(library._render(self.root / "raw.webm", target, 0.0, "m4a"))
        with self.fake_ffmpeg(write=b"tiny"):
            self.assertFalse(library._render(self.root / "raw.webm", target, 0.0, "m4a"))
        self.assertEqual(self.leftovers(), [])

    def test_success_publishes_by_rename_and_keeps_the_extension_for_ffmpeg(self):
        target = self.audio / "abc.m4a"
        seen = []
        with self.fake_ffmpeg(write=b"y" * 4096) as run:
            self.assertTrue(library._render(self.root / "raw.webm", target, 1.5, "m4a"))
            seen = run.call_args[0][0][-1]
        self.assertTrue(Path(seen).name.startswith(library.STAGING_PREFIX))
        self.assertTrue(seen.endswith(".m4a"))
        self.assertEqual(self.leftovers(), ["abc.m4a"])
        self.assertEqual(target.read_bytes(), b"y" * 4096)

    def test_raw_download_is_removed_even_when_measuring_times_out(self):
        self.track("a|song")
        raw = self.file(".raw_abcdefghijk_1234.webm")
        with patch.object(library, "fetch_recording", return_value=("abcdefghijk", raw)), \
                patch.object(library, "measure", side_effect=subprocess.TimeoutExpired("ffmpeg", 240)):
            with self.assertRaises(subprocess.TimeoutExpired):
                library._ensure_locked({"key": "a|song", "artist": "A", "title": "song"})
        self.assertFalse(raw.exists())
        self.assertFalse(library._cache_path("abcdefghijk").exists())

    def test_staging_sweep_only_takes_old_leftovers(self):
        old_raw = self.file(".raw_x_1.webm", age=7200)
        old_tmp = self.file(".tmp_1_abc.m4a", age=7200)
        young = self.file(".tmp_2_abc.m4a")
        record = self.file("abc.m4a", age=7200)
        self.assertEqual(library.sweep_staging(1.0), 2)
        self.assertFalse(old_raw.exists() or old_tmp.exists())
        self.assertTrue(young.exists() and record.exists())


class EnsureReuse(TempCache):
    def test_cache_hit_on_the_same_recording_skips_every_decode(self):
        final = library._cache_path("abcdefghijk")
        final.write_bytes(b"z" * 4096)
        self.track("a|song", video_id="abcdefghijk", duration=200.0, intro_sec=9.0,
                   outro_sec=190.0, lufs=-14.1, source_lufs=-9.0, applied_gain_db=-5.1,
                   bpm=120.0, camelot="8A")
        with patch.object(library, "fetch_recording", return_value=("abcdefghijk", final)), \
                patch.object(library, "measure") as measure, \
                patch.object(library.analysis, "profile") as profile, \
                patch.object(library, "_probe_duration") as probe:
            ready = library._ensure_locked({"key": "a|song", "artist": "A", "title": "song"})
        measure.assert_not_called()
        profile.assert_not_called()
        probe.assert_not_called()
        self.assertEqual(ready["file"], str(final))
        self.assertEqual((ready["bpm"], ready["lufs"], ready["source_lufs"]), (120.0, -14.1, -9.0))
        self.assertIsNotNone(ready["cache_used_at"])

    def test_fresh_render_stores_playing_level_and_source_level_separately(self):
        self.track("a|song")
        raw = self.file(".raw_abcdefghijk_1.webm")

        def render(source, target, gain, fmt=None):
            target.write_bytes(b"r" * 4096)
            return True

        tonal = dict.fromkeys(library.TONAL_FIELDS, None)
        tonal.update(bpm=100.0)
        with patch.object(library, "fetch_recording", return_value=("abcdefghijk", raw)), \
                patch.object(library, "measure", return_value={
                    "integrated": -8.0, "true_peak": -0.5, "samples": [], "duration": 181.5}), \
                patch.object(library, "_render", side_effect=render), \
                patch.object(library.analysis, "profile", return_value=tonal), \
                patch.object(library, "_probe_duration") as probe:
            ready = library._ensure_locked({"key": "a|song", "artist": "A", "title": "song"})
        probe.assert_not_called()  # the loudness pass already reported the length
        gain = library.gain_for(-8.0, -0.5)
        self.assertAlmostEqual(ready["source_lufs"], -8.0)
        self.assertAlmostEqual(ready["applied_gain_db"], gain)
        self.assertAlmostEqual(ready["lufs"], -8.0 + gain)
        self.assertEqual(ready["duration"], 181.5)
        self.assertFalse(raw.exists())


class Measurements(unittest.TestCase):
    def test_decoded_seconds_prefers_the_final_progress_line(self):
        stderr = ("Duration: 00:03:30.00, start: 0\n"
                  "size=N/A time=00:01:00.00 bitrate=N/A\r"
                  "size=N/A time=00:03:21.44 bitrate=N/A speed=90x\n")
        self.assertAlmostEqual(library._decoded_seconds(stderr), 201.44)
        self.assertAlmostEqual(library._decoded_seconds("  Duration: 01:00:01.50, start"), 3601.5)
        self.assertEqual(library._decoded_seconds("nothing useful"), 0.0)

    def test_trim_is_zero_for_renders_and_levels_local_files(self):
        with patch.object(config.station, "get", side_effect=lambda k, d=None: d):
            self.assertEqual(library.trim_db({"source": "request", "lufs": -8.0}), 0.0)
            self.assertEqual(library.trim_db({"source": "local", "lufs": None}), 0.0)
            self.assertEqual(library.trim_db({"source": "local", "lufs": -8.0, "true_peak": -0.2}), -6.0)
            # Quiet file with room: boosted, but never past the peak ceiling.
            self.assertEqual(library.trim_db({"source": "local", "lufs": -20.0, "true_peak": -6.0}), 4.5)
            # No peak measured: only ever turned down.
            self.assertEqual(library.trim_db({"source": "local", "lufs": -20.0}), 0.0)
            self.assertEqual(library.trim_db({"source": "local", "lufs": -10.0}), -4.0)


class Eviction(TempCache):
    def test_lru_orders_by_last_play_not_file_age(self):
        self.settings.update({"cache.max_size_gb": 3000 / 1024 ** 3, "cache.fresh_grace_minutes": 0})
        played = self.file("played.m4a", age=90 * 86400 / 2)
        idle = self.file("idle.m4a", age=3600)
        self.track("p", file=str(played), last_played=time.time() - 60)
        self.track("i", file=str(idle))
        library.evict()
        self.assertTrue(played.exists())
        self.assertFalse(idle.exists())
        self.assertIsNone(db.one("SELECT file FROM tracks WHERE key='i'")["file"])

    def test_ephemeral_never_deletes_protected_or_freshly_prepared_files(self):
        self.settings.update({"cache.mode": "ephemeral"})
        queued = self.file("queued.m4a", age=7200)
        fresh = self.file("fresh.m4a")
        done = self.file("done.m4a", age=7200)
        staging = self.file(".tmp_1_x.m4a", age=7200)
        library.evict(protect={str(queued)})
        self.assertTrue(queued.exists())
        self.assertTrue(fresh.exists())
        self.assertTrue(staging.exists())  # the sweep's job, not eviction's
        self.assertFalse(done.exists())

    def test_prepared_recently_counts_as_used(self):
        self.settings.update({"cache.max_age_hours": 1, "cache.fresh_grace_minutes": 0})
        old = self.file("old.m4a", age=5 * 3600)
        self.track("o", file=str(old), cache_used_at=time.time() - 60)
        library.evict()
        self.assertTrue(old.exists())


class JanitorProtection(TempCache):
    def station(self):
        s = director.Station.__new__(director.Station)
        s.clock = director.Clock()
        s.lock = threading.RLock()
        s.schedule = timeline.Schedule()
        s.rng = random.Random(0)
        s._lineup = []
        s._building_entry = None
        return s

    def test_queue_and_the_entry_being_built_are_protected(self):
        self.settings.update({"cache.mode": "ephemeral", "cache.fresh_grace_minutes": 0})
        queued = self.file("queued.m4a", age=7200)
        building = self.file("building.m4a", age=7200)
        spare = self.file("spare.m4a", age=7200)
        s = self.station()
        s._lineup = [{"id": "q", "track": {"key": "q", "file": str(queued)}, "source": "auto"}]
        s._building_entry = {"id": "b", "track": {"key": "b", "file": str(building)}}
        keep_audio, _ = s._janitor_protected()
        self.assertIn(str(queued), keep_audio)
        self.assertIn(str(building), keep_audio)
        library.evict(protect=keep_audio)
        self.assertTrue(queued.exists() and building.exists())
        self.assertFalse(spare.exists())

    def test_scheduled_urls_are_still_protected(self):
        s = self.station()
        s.schedule.items.append(SimpleNamespace(url="/media/audio/abc.m4a", kind="music", meta={},
                                                start_at=0, end_at=1e9))
        with patch.object(s.schedule, "trim_before"), patch.object(s.schedule, "music_items", return_value=[]):
            keep_audio, _ = s._janitor_protected()
        self.assertIn(str(self.audio / "abc.m4a"), keep_audio)


class Housekeeping(TempCache):
    def test_stems_budget_keeps_protected_and_recent_separations(self):
        from radio import stems
        record = self.file("queued.m4a")
        keep = stems.cache_dir(record)
        self.settings.update({"cache.stems_max_gb": 1 / 1024 ** 3})  # effectively zero
        folders = {name: self.root / "stems" / name for name in (keep.name, "old", "recent")}
        for name, folder in folders.items():
            self.file("drums.flac", folder=folder)
            stamp = time.time() - (0 if name == "recent" else 3 * 86400)
            os.utime(folder, (stamp, stamp))
        partial = self.root / "stems" / "dead.partial"
        partial.mkdir()
        stamp = time.time() - 7200
        os.utime(partial, (stamp, stamp))
        housekeeping.prune_stems([record])
        self.assertTrue(folders[keep.name].exists())
        self.assertTrue(folders["recent"].exists())
        self.assertFalse(folders["old"].exists())
        self.assertFalse(partial.exists())

    def test_director_sessions_of_dead_processes_go(self):
        mine = self.root / "director-sessions" / f"{os.getpid()}-abc"
        dead = self.root / "director-sessions" / "999999991-def"
        for folder in (mine, dead):
            self.file("schema.json", folder=folder)
            stamp = time.time() - 7200
            os.utime(folder, (stamp, stamp))
        with patch.object(housekeeping, "_alive", side_effect=lambda pid: pid == os.getpid()):
            housekeeping.prune_director_sessions()
        self.assertTrue(mine.exists())
        self.assertFalse(dead.exists())

    def test_alive_recognises_this_process(self):
        self.assertTrue(housekeeping._alive(os.getpid()))

    def test_source_info_and_analysis_orphans_age_out(self):
        info = self.root / "source-info"
        self.track("k", video_id="abcdefghijk")
        kept = self.file("abcdefghijk.json", age=90 * 86400, folder=info)
        orphan = self.file("zzzzzzzzzzz.json", age=90 * 86400, folder=info)
        context = self.file("song-context-abc.json", age=9 * 86400, folder=info)
        check = self.file("x.json", age=40 * 86400, folder=info / "edition-checks")
        housekeeping.prune_source_info()
        self.assertTrue(kept.exists())
        self.assertFalse(orphan.exists() or context.exists() or check.exists())

        record = self.file("known.m4a")
        self.track("r", file=str(record))
        from radio import analysis
        known = self.file(analysis.peaks_cache(record).name, age=90 * 86400, folder=self.root / "peaks")
        stale = self.file("0" * 32 + ".json", age=90 * 86400, folder=self.root / "structure")
        housekeeping.prune_analysis()
        self.assertTrue(known.exists())
        self.assertFalse(stale.exists())

    def test_sweep_runs_the_slow_passes_at_most_hourly(self):
        with patch.object(housekeeping, "_last_full", 0.0), \
                patch.object(housekeeping, "prune_artwork", return_value=0) as artwork:
            housekeeping.sweep()
            housekeeping.sweep()
            self.assertEqual(artwork.call_count, 1)
            housekeeping.sweep(force=True)
            self.assertEqual(artwork.call_count, 2)


class ArtworkLocking(TempCache):
    class Forbidden:
        def __enter__(self):
            raise AssertionError("global lock taken")

        def __exit__(self, *args):
            return False

    def test_a_cached_cover_is_served_without_any_lock(self):
        from radio import artwork
        self.track("k", video_id="abcdefghijk")
        with patch.object(artwork, "candidates", return_value=iter([("https://i.ytimg.com/x.jpg", "YouTube")])), \
                patch.object(artwork, "_download", return_value=b"png-bytes"):
            path, source = artwork.resolve("k")
        self.assertEqual(path.read_bytes(), b"png-bytes")
        self.assertEqual(artwork._digest_locks, {})
        with patch.object(artwork, "_lock", self.Forbidden()), \
                patch.object(artwork, "candidates", side_effect=AssertionError("network")):
            self.assertEqual(artwork.resolve("k"), (path, "YouTube"))

    def test_different_covers_do_not_wait_on_each_other(self):
        from radio import artwork
        self.track("slow", video_id="aaaaaaaaaaa")
        self.track("fast", video_id="bbbbbbbbbbb")
        started, release = threading.Event(), threading.Event()

        def download(url):
            if "aaaaaaaaaaa" in url:
                started.set()
                release.wait(5)
            return b"img"

        def candidates(track):
            yield f"https://i.ytimg.com/vi/{track['video_id']}/x.jpg", "YouTube"

        with patch.object(artwork, "_download", side_effect=download), \
                patch.object(artwork, "candidates", side_effect=candidates):
            def resolve_slow():
                artwork.resolve("slow")
                db.connect().close()  # this thread's own connection

            slow = threading.Thread(target=resolve_slow)
            slow.start()
            self.assertTrue(started.wait(5))
            try:
                self.assertIsNotNone(artwork.resolve("fast"))  # would deadlock under one lock
            finally:
                release.set()
                slow.join(5)


if __name__ == "__main__":
    unittest.main()
