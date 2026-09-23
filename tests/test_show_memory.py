"""Time awareness, weather, running bits and repetition memory across restarts."""
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import httpx

from radio import config, db, showclock
from radio.segments import base, show, writers


def at(hour, minute=0):
    return time.mktime((2026, 9, 22, hour, minute, 0, 0, 0, -1))


class Clock(unittest.TestCase):
    def test_spoken_time_is_words_a_host_would_say(self):
        self.assertEqual(showclock.spoken_time(at(19, 5)), "seven oh five in the evening")
        self.assertEqual(showclock.spoken_time(at(18)), "six o'clock in the evening")
        self.assertEqual(showclock.spoken_time(at(14, 40)), "two forty in the afternoon")
        self.assertEqual(showclock.spoken_time(at(2, 15)), "two fifteen at night")
        self.assertEqual(showclock.spoken_time(at(0)), "midnight")
        self.assertEqual(showclock.spoken_time(at(12)), "noon")

    def test_dayparts(self):
        self.assertEqual([showclock.daypart(at(h)) for h in (6, 13, 18, 22, 3)],
                         ["morning", "afternoon", "evening", "late night", "late night"])
        self.assertEqual(showclock.spoken_date(at(9)), "Tuesday, September 22")

    def test_time_check_no_longer_says_night_at_six_pm(self):
        with patch.object(writers.showclock, "time") as clock:
            clock.time.return_value = at(18)
            clock.localtime = time.localtime
            self.assertNotIn("night", writers._spoken_time())


class Weather(unittest.TestCase):
    def setUp(self):
        showclock._WEATHER.clear()
        self.addCleanup(showclock._WEATHER.clear)

    def settings(self, values):
        return patch.object(config.station, "get", side_effect=lambda k, d=None: values if k == "weather" else d)

    def test_off_unless_coordinates_are_configured(self):
        with self.settings({"latitude": None, "longitude": None}), patch.object(showclock.httpx, "get") as get:
            self.assertIsNone(showclock.weather())
        get.assert_not_called()

    def test_forecast_is_short_cached_and_failures_are_cached_too(self):
        response = httpx.Response(200, json={"current": {"temperature_2m": 11.6, "weather_code": 61}},
                                  request=httpx.Request("GET", showclock.FORECAST))
        with self.settings({"latitude": 51.5, "longitude": -0.1}), \
                patch.object(showclock.httpx, "get", return_value=response) as get:
            self.assertEqual(showclock.weather(1000), "light rain, 12 degrees celsius")
            self.assertEqual(showclock.weather(1000 + 1700), "light rain, 12 degrees celsius")
        self.assertEqual(get.call_count, 1)
        self.assertLessEqual(get.call_args.kwargs["timeout"], 5)
        showclock._WEATHER.clear()
        with self.settings({"latitude": 51.5, "longitude": -0.1}), \
                patch.object(showclock.httpx, "get", side_effect=httpx.ConnectTimeout("x")) as get:
            self.assertIsNone(showclock.weather(5000))
            self.assertIsNone(showclock.weather(5100))
        self.assertEqual(get.call_count, 1)


class Memory(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.local = threading.local()
        self.settings = {}
        for p in (patch.object(db, "_DB_PATH", Path(temp.name) / "t.db"),
                  patch.object(db, "_LOCAL", self.local),
                  patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d))):
            p.start()
            self.addCleanup(p.stop)
        self.addCleanup(self.close)

    def close(self):
        conn = getattr(self.local, "conn", None)
        if conn:
            conn.close()
            del self.local.conn

    def lines(self, *texts):
        return [base.Line("rue" if i % 2 == 0 else "mav", text) for i, text in enumerate(texts)]

    def test_recent_lines_survive_a_restart(self):
        show.remember("news", self.lines("First story line.", "Second story line."), now=100)
        self.close()  # a new connection, as after a restart
        self.assertEqual(show.recent_lines(), ["First story line.", "Second story line."])
        self.assertEqual(show.merged_recent(["Live line."]), ["First story line.", "Second story line.", "Live line."])

    def test_bits_are_offered_back_counted_and_retired(self):
        self.settings["hosts.bit_retire_after"] = 2
        show.remember("banter", self.lines("Anyway.", "The kazoo filed for custody of the bridge."), now=0)
        self.assertEqual(show.callbacks(now=60), [])  # too soon to call back
        offered = show.callbacks(now=3600)
        self.assertEqual(offered[0]["premise"], "The kazoo filed for custody of the bridge.")
        self.assertIn("RUNNING BITS", show.context_block("banter", now=3600))
        self.assertNotIn("RUNNING BITS", show.context_block("news", now=3600))
        # A later break that uses the premise counts as a callback, then retires it.
        show.remember("banter", self.lines("Custody hearing update.", "The kazoo won the bridge."), now=4000)
        self.assertEqual(show.callbacks(now=99999), [])

    def test_song_labels_are_not_premises(self):
        context = {"next": {"title": "Custody Bridge", "artist": "Kazoo Band"}}
        show.remember("track_intro", self.lines("Okay.", "Custody Bridge, by Kazoo Band."), context, now=0)
        self.assertEqual(show.callbacks(now=99999), [])

    def test_long_gap_is_mentioned_once_back_on_air(self):
        db.write("INSERT INTO aired(ts, kind) VALUES(?, 'banter')", (1000,))
        self.assertIn("first break in about 5 hours", show.context_block("banter", now=1000 + 5 * 3600))
        self.assertNotIn("first break", show.context_block("banter", now=1000 + 600))
        self.assertNotIn("first break", show.context_block("sign_on", now=1000 + 5 * 3600))

    def test_compose_records_the_break_and_restores_recent_lines(self):
        show.remember("news", self.lines("Earlier line from before a restart."), now=1)
        captured = {}
        def writer(context):
            captured.update(context)
            return self.lines("A brand new line.")
        with patch.dict(writers.WRITERS, {"banter": writer}):
            writers.compose("banter", {"recent_host_lines": []})
        self.assertIn("Earlier line from before a restart.", captured["recent_host_lines"])
        self.assertEqual(show.recent_lines()[-1], "A brand new line.")


if __name__ == "__main__":
    unittest.main()
