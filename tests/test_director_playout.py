"""Playout bookkeeping: completions, contract fields, restored endings, the
monotonic clock, vanished audio, bounded ledgers, and request-box hygiene."""
import random
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from radio import config, db, director, director_chat, intent, library, taste, timeline, vibe, wishes


class Base(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.folder = Path(folder.name)
        local = threading.local()
        self.settings = {}
        for p in (patch.object(config, "CACHE_DIR", self.folder),
                  patch.object(db, "_DB_PATH", self.folder / "station.db"),
                  patch.object(db, "_LOCAL", local),
                  patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d)),
                  patch.object(config.station, "set_many", side_effect=self.settings.update)):
            p.start()
            self.addCleanup(p.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())

    def station(self):
        s = director.Station.__new__(director.Station)
        s.lock = threading.RLock()
        s.clock = director.Clock()
        s.schedule = timeline.Schedule()
        s.rng = random.Random(1)
        s._lineup = []
        s._recent_keys = []
        s._stop = threading.Event()
        s._songs_since_break = 0
        s._break_after = 10
        s._signed_on = True
        s._active_wish = None
        s._epoch = 0
        s.status_note = "on air"
        s._last_track = None
        return s

    def audio(self, name):
        path = self.folder / name
        path.write_bytes(b"audio")
        return str(path)

    def track(self, key, **extra):
        return dict(key=key, title=key.title(), artist="Artist", duration=120, intro_sec=10,
                    file=self.audio(f"{key}.opus"), **extra)


class Completions(Base):
    def test_a_record_heard_to_its_end_counts_once_and_a_skipped_one_never(self):
        s = self.station()
        a = s.schedule.add_music("a", self.track("a"))
        b = s.schedule.add_music("b", self.track("b"))
        s.schedule.seal()
        with patch.object(director.taste, "record") as record:
            s._finish_requests(a.end_at - 1)
            record.assert_not_called()
            s._finish_requests(a.end_at + 0.1)
            s._finish_requests(a.end_at + 5)
            self.assertEqual(record.call_count, 1)
            signal, key = record.call_args.args[:2]
            self.assertEqual((signal, key), ("played", "a"))
            self.assertEqual(record.call_args.kwargs["position"], record.call_args.kwargs["duration"])
            s._skipped_items = {b.id}
            s._finish_requests(b.end_at + 1)
            self.assertEqual(record.call_count, 1)

    def test_client_played_reports_share_the_ledger(self):
        s = self.station()
        a = s.schedule.add_music("a", self.track("a"))
        s.schedule.seal()
        s.clock.jump(a.end_at - 0.5)
        db.write("INSERT INTO tracks(key,title,artist,added_at) VALUES('a','A','Artist',0)")
        with patch.object(director.taste, "record") as record:
            s.report("played", "a", position=119, duration=120)
            s.report("played", "a", position=119, duration=120)
            s._finish_requests(a.end_at + 1)
        self.assertEqual(record.call_count, 1)

    def test_completion_really_trains_taste(self):
        s = self.station()
        db.write("INSERT INTO tracks(key,title,artist,added_at) VALUES('a','A','Artist',0)")
        a = s.schedule.add_music("a", self.track("a"))
        self.settings["learning.signal_weights"] = {"completed": 1.0}
        s._finish_requests(a.end_at + 1)
        self.assertGreater(taste.affinity("track", "a"), 0)
        self.assertTrue(db.one("SELECT 1 FROM events WHERE kind='completed' AND track_key='a'"))


class Contract(Base):
    def test_music_meta_carries_playout_facts(self):
        s = self.station()
        local = self.track("local", source="local", lufs=-20.0, true_peak=-10.0, year=2014, bpm=120, camelot="8A",
                           beat_offset=.1, beat_period=.5, downbeat_offset=.6)
        downloaded = self.track("dl", source="request", lufs=-9.0)
        self.settings["audio.target_lufs"] = -14.0
        self.assertTrue(s._place(local, [], "none"))
        self.assertTrue(s._place(downloaded, [], "none"))
        first, second = s.schedule.music_items()
        self.assertEqual(first.url, "/media/track/local")
        self.assertTrue(second.url.startswith("/media/audio/"))
        self.assertEqual(first.meta["file"], local["file"])
        self.assertEqual(first.meta["trim_db"], 6.0)
        self.assertEqual(second.meta["trim_db"], 0.0)
        for name, value in (("lufs", -20.0), ("year", 2014), ("bpm", 120), ("camelot", "8A"),
                            ("beat_offset", .1), ("beat_period", .5), ("downbeat_offset", .6)):
            self.assertEqual(first.meta[name], value, name)
        self.assertEqual(director.media_url({"key": "a b|c/d", "source": "local"}), "/media/track/a%20b%7Cc%2Fd")

    def test_trim_follows_the_library_or_applied_gain(self):
        self.settings["audio.target_lufs"] = -14.0
        self.assertEqual(timeline.trim_db({"source": "local", "lufs": -10}), -4.0)
        # Boosts need a measured peak; the library never lifts a file blind.
        self.assertEqual(timeline.trim_db({"source": "local", "lufs": -40}), 0.0)
        self.assertEqual(timeline.trim_db({"source": "local", "lufs": -40, "true_peak": -8.0}), 6.5)
        # applied_gain_db=0 on a local file means nothing was baked in: still trim.
        self.assertEqual(timeline.trim_db({"source": "local", "lufs": -10, "applied_gain_db": 0}), -4.0)
        with patch.object(library, "trim_db", create=True, return_value=-2.5):
            self.assertEqual(timeline.trim_db({"source": "local", "lufs": -10}), -2.5)


class Placement(Base):
    def test_removing_the_next_record_restores_the_natural_ending(self):
        s = self.station()
        a = s.schedule.add_music("a", self.track("a", outro_sec=None))
        natural = a.duration
        s.schedule.seal()
        b = s.schedule.add_music("b", self.track("b"))
        s.schedule.seal()
        self.assertEqual(s.schedule._natural[a.id], natural)
        a.duration = natural - 20  # as if b's cue planner chose an earlier exit
        self.assertTrue(s.drop_scheduled(b.id))
        self.assertEqual(a.duration, natural)
        self.assertEqual([i.id for i in s.schedule.music_items()], [a.id])
        self.assertIs(s.schedule._last_music, a)

    def test_vanished_audio_is_never_scheduled_and_requests_prepare_again(self):
        s = self.station()
        track = self.track("gone")
        Path(track["file"]).unlink()
        db.write("INSERT INTO tracks(key,title,artist,added_at) VALUES('gone','Gone','Artist',0)")
        request = db.write("INSERT INTO requests(ts,query,status,track_key) VALUES(0,'x','queued','gone')")
        self.assertFalse(s._place(track, [], "none"))
        self.assertFalse(s.schedule.items)
        s._enqueue({**track, "_request_id": request}, "request")
        with patch.object(director.writers, "compose", return_value=[]):
            s._extend()
        self.assertFalse(s.schedule.items)
        self.assertEqual(db.one("SELECT status FROM requests WHERE id=?", (request,))["status"], "pending")

    def test_ledgers_are_pruned_to_the_live_schedule(self):
        s = self.station()
        a = s.schedule.add_music("a", self.track("a"))
        s._reported_plays = {a.id, "old"}
        s._skipped_speech = {"old-voice"}
        s._skipped_items = {"old"}
        s._credited_items = {"old": "x", a.id: "a"}
        s._finished_request_ids = {1, 2}
        s._prune_memory()
        self.assertEqual((s._reported_plays, s._skipped_speech, s._skipped_items, set(s._credited_items),
                          s._finished_request_ids), ({a.id}, set(), set(), {a.id}, set()))


class StationClock(unittest.TestCase):
    def test_clock_ignores_wall_clock_jumps(self):
        clock = director.Clock()
        with patch.object(director.time, "monotonic", return_value=100.0), \
                patch.object(director.time, "time", return_value=1e9):
            clock.start()
        with patch.object(director.time, "monotonic", return_value=105.0), \
                patch.object(director.time, "time", return_value=5.0):
            self.assertAlmostEqual(clock.now(), 5.0)


class RequestBoxHygiene(Base):
    def test_less_rap_turns_down_rap_acts_not_trapt(self):
        for key, artist in (("t", "Trapt"), ("r", "Rap Collective"), ("x", "50% Off")):
            db.write("INSERT INTO tracks(key,title,artist,added_at) VALUES(?,?,?,0)", (key, key, artist))
        self.assertEqual(wishes._direct_artists("rap"), ["Rap Collective"])
        outcome = wishes.apply_directive("50%", ["50% Off"])
        self.assertEqual(outcome["tracks"], 1)
        self.assertLess(taste.affinity("track", "x"), 0)
        self.assertEqual(taste.affinity("track", "t"), 0)

    def test_a_scheduled_request_is_not_queued_twice(self):
        key = taste.add_track("Song", "Artist", source="request")
        db.write("INSERT INTO requests(ts,query,status,track_key) VALUES(0,'x','scheduled',?)", (key,))
        result = wishes.submit("Artist - Song")
        self.assertIn("already", result["message"])
        self.assertEqual(db.one("SELECT COUNT(*) AS n FROM requests")["n"], 1)

    def test_slow_paths_answer_immediately(self):
        spawned = []
        with patch.object(wishes, "_spawn", side_effect=lambda fn, *a: spawned.append((fn, a))), \
                patch.object(wishes.llm, "complete_json", side_effect=AssertionError("model on the web thread")), \
                patch("radio.sources.rss.search", side_effect=AssertionError("feeds on the web thread")):
            unclear = wishes.submit("something nobody can parse")
            topic = wishes.submit("tell me about the moon landing")
            bulk = wishes.submit("play some bossa nova")
        self.assertEqual(unclear["kind"], "working")
        self.assertEqual(db.one("SELECT status FROM wishes WHERE id=?", (unclear["id"],))["status"], "preparing")
        self.assertTrue(topic["ok"] and bulk["ok"])
        self.assertEqual(len(spawned), 3)
        def model(system, *args, **kwargs):
            if system == wishes._SUGGEST_SYSTEM:
                return [{"artist": "Joao Gilberto", "title": "Desafinado"}]
            return {"kind": "track", "title": "Nobody", "artist": "Someone"}
        with patch.object(wishes.llm, "complete_json", side_effect=model), \
                patch("radio.sources.rss.search", return_value=[{"ident": "s1"}]):
            for fn, args in spawned:
                fn(*args)
        self.assertEqual(db.one("SELECT status FROM wishes WHERE id=?", (unclear["id"],))["status"], "done")
        self.assertIn("1 stories", db.one("SELECT note FROM wishes WHERE id=?", (topic["id"],))["note"])
        self.assertEqual(db.one("SELECT status FROM wishes WHERE id=?", (bulk["id"],))["status"], "done")
        self.assertEqual(db.one("SELECT status FROM wishes WHERE id=?", (topic["id"],))["status"], "pending")

    def test_a_cancelled_job_does_not_queue(self):
        spawned = []
        with patch.object(wishes, "_spawn", side_effect=lambda fn, *a: spawned.append((fn, a))):
            bulk = wishes.submit("play some bossa nova")
        wishes.cancel(bulk["id"])
        with patch.object(wishes.llm, "complete_json", return_value=[{"artist": "A", "title": "B"}]):
            spawned[0][0](*spawned[0][1])
        self.assertFalse(db.query("SELECT * FROM requests"))
        self.assertEqual(db.one("SELECT status FROM wishes WHERE id=?", (bulk["id"],))["status"], "cancelled")

    def test_known_titles_use_the_indexed_lookup_when_present(self):
        rows = [{"title": "Like That", "artist": "Future", "blocked": 0, "source": "seed"}]
        with patch.object(db, "tracks_titled", create=True, return_value=rows) as lookup:
            self.assertEqual(intent._known_title("like that"), ("Future", "Like That"))
        lookup.assert_called_once_with("like that")


class ChatLocking(Base):
    def test_direction_changes_happen_under_the_chat_lock(self):
        station = SimpleNamespace(lock=threading.RLock(), clock=Mock(), schedule=timeline.Schedule(), _lineup=[])
        station.clock.now.return_value = 0
        chat = director_chat.Chat(station)
        held = []
        station.refresh_vibe = lambda: held.append(chat.lock._is_owned())
        self.addCleanup(lambda: vibe.set_session_selection(None))
        chat.apply({"type": "steer", "profile": {"description": "Soul", "genres": ["soul"], "avoid_genres": [],
                                                 "pace": "any"}}, chat.snapshot())
        chat.apply({"type": "quiet", "minutes": 5}, chat.snapshot())
        chat.apply({"type": "undo"}, chat.snapshot())
        self.assertEqual(held, [True, True])


if __name__ == "__main__":
    unittest.main()
