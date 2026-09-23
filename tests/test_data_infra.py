"""Database, config and metadata plumbing: schema once per process, retention,
normalised lookups, import bookkeeping, config caching and enrichment."""
import json
import os
import tempfile
import threading
import time
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

import numpy as np

from radio import analysis, config, db, discovery, enrich, importer, library, mixconfig


class TempDatabase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.local = threading.local()
        for patcher in (patch.object(db, "_DB_PATH", self.root / "test.db"),
                        patch.object(db, "_LOCAL", self.local)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(lambda: getattr(self.local, "conn", None) and self.local.conn.close())
        db.connect()

    def track(self, key, **values):
        values = {"title": key, "artist": "A", "added_at": 0, **values}
        db.write(f"INSERT INTO tracks (key, {', '.join(values)}) VALUES (?{', ?' * len(values)})",
                 (key, *values.values()))


class Schema(TempDatabase):
    def test_schema_runs_once_per_process_not_per_thread(self):
        results = []
        main = db.connect()

        def other_thread():
            conn = db.connect()
            results.append(conn is not main)
            results.append(conn.execute("PRAGMA busy_timeout").fetchone()[0])
            conn.close()

        with patch.object(db, "_migrate", side_effect=AssertionError("ran again")):
            thread = threading.Thread(target=other_thread)
            thread.start()
            thread.join()
        self.assertEqual(results, [True, 15000])

    def test_a_new_database_file_still_gets_its_schema(self):
        other = threading.local()
        with patch.object(db, "_DB_PATH", self.root / "second.db"), patch.object(db, "_LOCAL", other):
            db.write("INSERT INTO import_paths VALUES ('p', 1, 2, 'k', 0)")
            other.conn.close()

    def test_hot_query_indexes_exist(self):
        names = {row["name"] for row in db.query("SELECT name FROM sqlite_master WHERE type='index'")}
        self.assertTrue({"idx_events_track", "idx_events_kind_ts", "idx_requests_status",
                         "idx_tracks_title_norm", "idx_tracks_file"} <= names)

    def test_normalised_columns_follow_title_and_artist(self):
        self.track("k", title="Pretend (Remastered 2011)", artist="Alex G")
        self.assertEqual([r["key"] for r in db.tracks_titled("pretend", "alex g")], ["k"])
        db.write("UPDATE tracks SET title='Change' WHERE key='k'")
        self.assertEqual(db.tracks_titled("pretend"), [])
        self.assertEqual([r["key"] for r in db.tracks_titled("CHANGE!")], ["k"])

    def test_local_levels_backfill(self):
        self.track("l", source="local", lufs=-9.5)
        self.track("d", source="request", lufs=-14.0)
        db._migrate(db.connect())
        rows = {r["key"]: r for r in db.query("SELECT key, source_lufs, applied_gain_db FROM tracks")}
        self.assertEqual((rows["l"]["source_lufs"], rows["l"]["applied_gain_db"]), (-9.5, 0))
        self.assertIsNone(rows["d"]["source_lufs"])


class Retention(TempDatabase):
    def age(self, table, days):
        db.write(f"UPDATE {table} SET ts=?", (time.time() - days * 86400,))

    def test_prune_seen_by_kind(self):
        db.mark_seen("news", "old-story")
        db.mark_seen("ad", "old-ad")
        self.age("seen", 30)
        db.mark_seen("news", "new-story")
        db.prune_seen(7, kind="news")
        self.assertEqual({(r["kind"], r["ident"]) for r in db.query("SELECT * FROM seen")},
                         {("ad", "old-ad"), ("news", "new-story")})
        db.prune_seen(7)
        self.assertEqual([r["ident"] for r in db.query("SELECT * FROM seen")], ["new-story"])

    def test_history_prune_keeps_listening_signals_and_latest_rows(self):
        for kind in ("played", "thumbs_up", "skipped_early", "discovery_attempt", "discovery_attempt"):
            db.log_event(kind, "k")
        db.mark_aired("news")
        db.mark_aired("news")
        db.mark_aired("sign_on")
        for status in ("aired", "pending", "failed"):
            db.write("INSERT INTO requests (ts, query, status) VALUES (0, 'q', ?)", (status,))
        for status in ("done", "pending", "active"):
            db.write("INSERT INTO wishes (ts, raw, kind, status) VALUES (0, 'w', 'topic', ?)", (status,))
        for table in ("events", "aired"):
            self.age(table, 400)
        db.prune_history(180)
        kinds = sorted(r["kind"] for r in db.query("SELECT kind FROM events"))
        self.assertEqual(kinds, ["discovery_attempt", "played", "skipped_early", "thumbs_up"])
        self.assertEqual(sorted(r["kind"] for r in db.query("SELECT kind FROM aired")), ["news", "sign_on"])
        self.assertIsNotNone(db.last_aired("sign_on"))
        self.assertEqual([r["status"] for r in db.query("SELECT status FROM requests")], ["pending"])
        self.assertEqual(sorted(r["status"] for r in db.query("SELECT status FROM wishes")),
                         ["active", "pending"])


def wav(path, seconds=0.1):
    samples = (np.sin(np.arange(int(44100 * seconds)) * 2 * np.pi * 440 / 44100) * 10000).astype("<i2")
    with wave.open(str(path), "wb") as output:
        output.setparams((1, 2, 44100, 0, "NONE", "not compressed"))
        output.writeframes(samples.tobytes())


class ImportPaths(TempDatabase):
    def setUp(self):
        super().setUp()
        for target, kwargs in ((library, dict(target="measure", return_value={
                "samples": [], "integrated": -20.0, "true_peak": -3.0})),
                               (analysis, dict(target="profile", return_value={"bpm": 120.0})),
                               (analysis, dict(target="peaks", return_value={}))):
            name = kwargs.pop("target")
            patcher = patch.object(target, name, **kwargs)
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)
        self.first = self.root / "a" / "Artist - Song.wav"
        self.second = self.root / "b" / "Artist - Song.wav"
        for path in (self.first, self.second):
            path.parent.mkdir()
            wav(path)

    def test_twin_files_are_read_once_each_then_skipped(self):
        self.assertEqual(importer.import_file(self.first)["status"], "imported")
        self.assertEqual(importer.import_file(self.second)["status"], "updated")
        self.assertEqual(self.measure.call_count, 2)
        for _ in range(2):
            self.assertEqual(importer.import_file(self.first)["status"], "skipped")
            self.assertEqual(importer.import_file(self.second)["status"], "skipped")
        self.assertEqual(self.measure.call_count, 2)
        self.assertEqual(len(db.query("SELECT * FROM import_paths")), 2)
        row = db.one("SELECT * FROM tracks")
        self.assertEqual((row["lufs"], row["source_lufs"], row["applied_gain_db"], row["true_peak"]),
                         (-20.0, -20.0, 0.0, -3.0))

    def test_a_changed_file_is_read_again(self):
        importer.import_file(self.first)
        stamp = time.time_ns() + 5_000_000_000
        os.utime(self.first, ns=(stamp, stamp))
        self.assertEqual(importer.import_file(self.first)["status"], "updated")
        self.assertEqual(self.measure.call_count, 2)

    def test_if_the_twin_the_row_points_at_is_gone_this_copy_is_read(self):
        importer.import_file(self.first)
        importer.import_file(self.second)
        self.second.unlink()
        self.assertEqual(importer.import_file(self.first)["status"], "updated")
        self.assertEqual(Path(db.one("SELECT file FROM tracks")["file"]), self.first.resolve())

    def test_rows_from_before_the_table_are_recognised_and_levels_filled(self):
        importer.import_file(self.first)
        db.write("DELETE FROM import_paths")
        db.write("UPDATE tracks SET lufs=NULL, true_peak=NULL")
        self.assertEqual(importer.import_file(self.first)["status"], "updated")  # levels only
        self.assertEqual(self.measure.call_count, 2)
        self.profile.assert_called_once()
        self.assertEqual(db.one("SELECT lufs FROM tracks")["lufs"], -20.0)
        self.assertEqual(importer.import_file(self.first)["status"], "skipped")
        self.assertEqual(len(db.query("SELECT * FROM import_paths")), 1)

    def test_folder_scan_ignores_renders_in_progress(self):
        wav(self.root / "a" / ".tmp_abc_Artist - Other.wav")
        report = importer.import_folder(self.root / "a")
        self.assertEqual((report["imported"], report["errors"]), (1, []))


class ConfigCache(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.base = self.root / "station.yaml"
        self.base.write_text("crossfade:\n  duration: 6\n", encoding="utf-8")
        self.settings = config.OverridableConfig(self.base, self.root / "overrides.yaml")

    def rewrite(self, text):
        self.base.write_text(text, encoding="utf-8")
        stamp = time.time() + 10
        os.utime(self.base, (stamp, stamp))

    def test_files_are_looked_at_once_per_interval(self):
        self.assertEqual(self.settings.get("crossfade.duration"), 6)
        self.rewrite("crossfade:\n  duration: 9\n")
        with patch.object(Path, "stat", side_effect=AssertionError("stat inside the interval")):
            self.assertEqual(self.settings.get("crossfade.duration"), 6)
        with patch.object(config, "STAT_INTERVAL", 0.0):
            self.assertEqual(self.settings.get("crossfade.duration"), 9)

    def test_missing_override_file_is_not_stat_every_read(self):
        self.settings.get("crossfade.duration")
        with patch.object(Path, "stat", side_effect=AssertionError("stat inside the interval")):
            self.settings.get("crossfade.duration")
            self.settings.data()

    def test_saving_is_visible_immediately_and_bumps_the_version(self):
        before = self.settings.version()
        self.settings.set("crossfade.duration", 3.5)
        self.assertEqual(self.settings.get("crossfade.duration"), 3.5)
        self.assertNotEqual(self.settings.version(), before)
        self.assertEqual(self.settings.data()["crossfade"]["duration"], 3.5)

    def test_mix_snapshot_is_reused_until_settings_change(self):
        with patch.object(config, "station", self.settings):
            first = mixconfig.snapshot()
            self.assertIs(mixconfig.snapshot(), first)
            self.settings.set("crossfade.duration", 4.0)
            second = mixconfig.snapshot()
        self.assertIsNot(second, first)
        value = next(f["value"] for f in second["fields"] if f["key"] == "crossfade.duration")
        self.assertEqual(value, 4.0)

    def test_personas_are_parsed_once_and_handed_out_as_copies(self):
        folder = self.root / "personas"
        folder.mkdir()
        (folder / "a.yaml").write_text("id: mav\nname: Mav\nvoice: {name: x}\n", encoding="utf-8")
        with patch.object(config, "CONFIG_DIR", self.root):
            first = config.personas()
            first["mav"]["voice"]["name"] = "mutated"
            with patch.object(config.yaml, "safe_load", side_effect=AssertionError("parsed again")):
                self.assertEqual(config.personas()["mav"]["voice"]["name"], "x")
            (folder / "b.yaml").write_text("id: rue\nname: Rue\n", encoding="utf-8")
            with patch.object(config, "STAT_INTERVAL", 0.0):
                self.assertEqual(sorted(config.personas()), ["mav", "rue"])

    def test_ffprobe_is_found_beside_a_configured_ffmpeg(self):
        with patch.dict(os.environ, {"FFPROBE_BIN": ""}), \
                patch.object(config, "FFMPEG", r"C:\tools\ffmpeg\bin\ffmpeg.exe"):
            self.assertEqual(config._ffprobe(), str(Path(r"C:\tools\ffmpeg\bin\ffprobe.exe")))
        with patch.dict(os.environ, {"FFPROBE_BIN": "D:/probe.exe"}):
            self.assertEqual(config._ffprobe(), "D:/probe.exe")
        with patch.dict(os.environ, {"FFPROBE_BIN": ""}), patch.object(config, "FFMPEG", "ffmpeg"):
            self.assertEqual(config._ffprobe(), "ffprobe")


class DiscoveryOrder(TempDatabase):
    def test_cooldown_and_full_pool_answer_before_the_profile_is_built(self):
        settings = {"selection.exploration_rate": .35}
        with patch.object(config.station, "get", side_effect=lambda k, d=None: settings.get(k, d)), \
                patch.object(discovery, "profile", side_effect=AssertionError("profile built")):
            db.log_event("discovery_attempt")
            self.assertEqual(discovery.refresh()["state"], "cooldown")
            for index in range(12):
                self.track(f"d{index}", source="auto_discovery")
            self.track("played", source="auto_discovery", play_count=1)
            result = discovery.refresh()
        self.assertEqual((result["state"], result["available"]), ("ready", 12))


class FakeResponse:
    def __init__(self, status, body=None, headers=None):
        self.status_code = status
        self._body = body or {}
        self.headers = headers or {}

    def json(self):
        return self._body

    def raise_for_status(self):
        if self.status_code >= 400:
            import httpx
            raise httpx.HTTPStatusError("bad", request=None, response=None)


class Enrichment(TempDatabase):
    def setUp(self):
        super().setUp()
        self.settings = {}
        self.calls = []
        for patcher in (patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d)),
                        patch.object(enrich.spotify, "available", return_value=True),
                        patch.object(enrich, "MIN_GAP", 0.0),
                        patch.object(enrich, "_backoff_until", 0.0),
                        patch.object(enrich, "_token", None),
                        patch.object(enrich.httpx, "post", return_value=FakeResponse(
                            200, {"access_token": "t", "expires_in": 3600})),
                        patch.object(enrich.httpx, "get", side_effect=self.artist_search)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.artist_response = FakeResponse(200, {"artists": {"items": [
            {"name": "Someone Else", "genres": ["polka"]},
            {"name": "Alex G", "genres": ["indie rock", "lo-fi", "bedroom pop", "fourth"]}]}})

    def artist_search(self, url, params=None, headers=None, timeout=None):
        self.calls.append(params)
        return self.artist_response

    def search(self, query):
        self.calls.append(query)
        return [{"artist": "Alex G", "title": "Pretend", "album": "Trick", "year": "2012"}]

    def test_fills_only_blanks_and_never_overwrites_tags(self):
        self.track("alex g|pretend", title="Pretend", artist="Alex G", genre="Indie")
        with patch.object(enrich.spotify, "search", side_effect=self.search):
            self.assertEqual(enrich.run_batch(), {"filled": 1})
        row = db.one("SELECT year, album, genre FROM tracks")
        self.assertEqual((row["year"], row["album"], row["genre"]), (2012, "Trick", "Indie"))
        self.assertEqual(len(self.calls), 1)  # genre was known: no artist lookup

    def test_genres_come_from_the_exact_artist_and_provenance_is_noted(self):
        meta = json.dumps({"version": 1, "fields": {"title": {"value": "Pretend", "source": "youtube"}}})
        self.track("alex g|pretend", title="Pretend", artist="Alex G", year=2012, album="Trick",
                   source_metadata=meta)
        self.assertEqual(enrich.run_batch(), {"filled": 1})
        row = db.one("SELECT genre, source_metadata FROM tracks")
        self.assertEqual(row["genre"], "indie rock, lo-fi, bedroom pop")
        fields = json.loads(row["source_metadata"])["fields"]
        self.assertEqual(fields["genre"]["source"], enrich.SOURCE)
        self.assertEqual(fields["title"]["source"], "youtube")

    def test_cooldown_after_an_attempt(self):
        self.track("x|y", title="Y", artist="X")
        with patch.object(enrich.spotify, "search", return_value=[]):
            self.assertEqual(enrich.run_batch(), {"no_match": 1})
            self.assertEqual(enrich.run_batch(), {})
        self.assertEqual(db.one("SELECT status FROM enrichment")["status"], "no_match")
        self.assertEqual(enrich.pending(10, now=time.time() + enrich.COOLDOWN + 1)[0]["key"], "x|y")

    def test_rate_limit_stops_the_batch_without_marking_the_track(self):
        self.track("alex g|pretend", title="Pretend", artist="Alex G")
        self.track("b|c", title="C", artist="B")
        self.artist_response = FakeResponse(429, headers={"Retry-After": "30"})
        with patch.object(enrich.spotify, "search", side_effect=self.search):
            self.assertEqual(enrich.run_batch(), {})
        self.assertIsNone(db.one("SELECT * FROM enrichment"))
        self.assertGreater(enrich._backoff_until, time.monotonic() + 20)

    def test_spotify_rate_limit_message_also_backs_off(self):
        self.track("alex g|pretend", title="Pretend", artist="Alex G")
        with patch.object(enrich.spotify, "search", side_effect=ValueError(
                "Spotify is rate limiting searches. Wait a moment and try again.")):
            self.assertEqual(enrich.run_batch(), {})
        self.assertIsNone(db.one("SELECT * FROM enrichment"))

    def test_disabled_without_credentials_or_by_setting(self):
        self.track("alex g|pretend", title="Pretend", artist="Alex G")
        self.settings["enrichment.enabled"] = False
        self.assertEqual(enrich.run_batch(), {})
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
