"""Early planning and skipping exercise the real schedule without network/audio."""
import random
import threading
import unittest
from unittest.mock import patch

from radio import config, director, timeline


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.settings = {"transitions.smart_cues": False, "transitions.tempo_match": False}
        p = patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d))
        p.start()
        self.addCleanup(p.stop)
        p = patch.object(director.taste, "record")
        self.record = p.start()
        self.addCleanup(p.stop)
        s = self.station = director.Station.__new__(director.Station)
        s.lock = threading.RLock()
        s.clock = director.Clock()
        s.schedule = timeline.Schedule()
        s.rng = random.Random(1)
        s._lineup = []
        s._stop = threading.Event()
        s._recent_keys = []
        s._songs_since_break = 0
        s._break_after = 10
        s._signed_on = True
        s._active_wish = None
        s._epoch = 0
        s.status_note = "on air"
        s._last_track = self.track("a")
        self.a = s.schedule.add_music("a", s._last_track)
        s.schedule.seal()
        s.clock.jump(20)

    def track(self, key):
        return dict(key=key, title=key, artist="Artist", duration=600,
                    file=f"{key}.m4a", intro_sec=15)

    def add(self, key):
        s = self.station
        s._enqueue(self.track(key), "auto")
        s._extend()
        return s.schedule.music_items()[-1]

    def test_long_record_gets_its_next_transition_immediately(self):
        s = self.station
        self.assertGreater(s.schedule.end_at - s.clock.now(), director.LOOKAHEAD)
        self.assertTrue(s._needs_extension(s.clock.now()))
        b = self.add("b")
        self.assertIn("transition", b.meta)
        self.assertFalse(s._needs_extension(s.clock.now()))
        self.settings["transitions.prepare_tracks_ahead"] = 2
        self.assertTrue(s._needs_extension(s.clock.now()))

    def test_skip_lands_before_original_mix_without_replanning(self):
        s = self.station
        b = self.add("b")
        before = s.schedule.as_dict()
        result = s.skip()
        self.assertEqual(result["mode"], "transition")
        self.assertAlmostEqual(s.clock.now(), b.start_at - 4)
        self.assertEqual(s.schedule.as_dict(), before)
        self.assertEqual(s._epoch, 1)

    def test_skip_uses_configured_lead_in(self):
        self.settings["skip.lead_in"] = 7
        b = self.add("b")
        self.station.skip()
        self.assertAlmostEqual(self.station.clock.now(), b.start_at - 7)

    def test_skip_waits_without_cutting_then_completes_once(self):
        s = self.station
        before = s.schedule.as_dict()
        self.assertEqual(s.skip()["mode"], "preparing")
        self.assertEqual(s.skip()["mode"], "preparing")
        self.assertEqual(s.schedule.as_dict(), before)
        self.assertEqual(s.clock.now(), 20)
        b = self.add("b")
        self.assertAlmostEqual(s.clock.now(), b.start_at - 4)
        self.assertIsNone(s._pending_skip)
        self.assertEqual(self.record.call_count, 1)

    def test_active_mix_is_never_skipped_or_cut_even_with_another_song_ahead(self):
        s = self.station
        b = self.add("b")
        self.add("c")
        s.clock.jump(b.start_at + 1 - s.clock.now())
        before = s.schedule.as_dict()
        now = s.clock.now()
        self.assertEqual(s.skip()["mode"], "already_mixing")
        self.assertEqual(s.clock.now(), now)
        self.assertEqual(s.schedule.as_dict(), before)
        self.assertFalse(self.record.called)

    def test_half_second_before_mix_does_not_cut_the_pair(self):
        s = self.station
        b = self.add("b")
        s.clock.jump(b.start_at - .2 - s.clock.now())
        self.assertEqual(s.skip()["mode"], "already_mixing")
        self.assertEqual(len(s.schedule.music_items()), 2)

    def test_expired_pending_skip_does_not_skip_a_different_song(self):
        s = self.station
        s.skip()
        s.clock.jump(self.a.end_at + 10)
        before = s.clock.now()
        self.add("b")
        self.assertEqual(s.clock.now(), before)
        self.assertIsNone(s._pending_skip)

    def test_voice_render_does_not_hold_schedule_lock(self):
        s = self.station
        entered, release = threading.Event(), threading.Event()
        errors = []
        def render(lines):
            entered.set()
            if not release.wait(3):
                raise RuntimeError("test render was not released")
            return []
        def build():
            try:
                self.add("b")
            except Exception as error:
                errors.append(error)
        with patch.object(s, "_render", side_effect=render):
            thread = threading.Thread(target=build)
            thread.start()
            try:
                self.assertTrue(entered.wait(1))
                acquired = s.lock.acquire(timeout=.3)
                self.assertTrue(acquired, "voice rendering locked out Skip and schedule polls")
                if acquired:
                    try:
                        self.assertEqual(s.skip()["mode"], "preparing")
                    finally:
                        s.lock.release()
            finally:
                release.set()
                thread.join(3)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertIsNone(s._pending_skip)

    def test_restarted_decks_discard_stale_transition_and_requeue_track(self):
        s = self.station
        def restart(lines):
            s.schedule = timeline.Schedule()
            return []
        with patch.object(s, "_render", side_effect=restart):
            s._enqueue(self.track("b"), "auto")
            s._extend()
        self.assertEqual(s.schedule.music_items(), [])
        self.assertEqual(s.lineup()[0]["key"], "b")

    def test_render_failure_does_not_lose_queued_record(self):
        s = self.station
        with patch.object(s, "_render", side_effect=RuntimeError("test")):
            s._enqueue(self.track("b"), "request")
            with self.assertRaises(RuntimeError):
                s._extend()
        self.assertEqual(s.lineup()[0]["key"], "b")
        self.assertIsNone(s._building_entry)

    def test_feeder_excludes_record_while_its_break_is_rendering(self):
        s = self.station
        s._building_entry = {"track": self.track("building")}
        with patch.object(director.db, "one", return_value=None), \
             patch.object(director.taste, "pick_next", return_value=None) as pick:
            s._next_candidate()
        self.assertIn("building", pick.call_args.args[0])
        self.assertEqual(pick.call_args.kwargs["previous"]["key"], "building")
