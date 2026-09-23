"""HTTP plumbing: request guard, event stream, local media, input checks, shutdown."""
import json
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import app as app_module
from radio import db
from radio.app import app

LOCAL = "http://127.0.0.1:8090"


class FakeClock:
    def __init__(self):
        self.value = 10.0

    def now(self):
        return self.value


class FakeStation:
    """Just enough station for the HTTP layer; no threads, no database."""

    def __init__(self):
        self.clock = FakeClock()
        self.heartbeats = 0
        self.shutdowns = 0
        self.reports = []
        self.items = [{"id": "a", "kind": "music"}]
        self.status_note = "on air"

    def heartbeat(self):
        self.heartbeats += 1

    def snapshot(self):
        return {"now": round(self.clock.now(), 4), "items": list(self.items), "epoch": 0}

    def report(self, *args):
        self.reports.append(args)

    def skip(self):
        return {"ok": True}

    def shutdown(self):
        self.shutdowns += 1

    def now_playing(self):
        return {"key": "a", "position": self.clock.now()}

    def transcript(self):
        return []

    def clear_lineup(self, keep_requests=False):
        return 0


class Base(unittest.TestCase):
    def setUp(self):
        self.station = FakeStation()
        for patcher in (patch("radio.app.director.station", return_value=self.station),):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.client = app.test_client()


class RequestGuard(Base):
    """Loopback Host on our port; same-origin browser writes only."""

    def test_host_matrix(self):
        for host, allowed in (("127.0.0.1:8090", True), ("localhost:8090", True),
                              ("[::1]:8090", True), ("LOCALHOST:8090", True),
                              ("evil.example:8090", False), ("127.0.0.1:9999", False),
                              ("127.0.0.1", False), ("127.0.0.1.nip.io:8090", False),
                              ("[::1]", False), ("", False), ("127.0.0.1:80x", False)):
            with self.subTest(host=host):
                response = self.client.get("/api/about", base_url=LOCAL, headers={"Host": host})
                self.assertEqual(response.status_code == 200, allowed, response.status_code)

    def test_default_port_host_matches_a_default_port_server(self):
        # Flask's test client: Host "localhost", SERVER_PORT 80.
        self.assertEqual(self.client.get("/api/about").status_code, 200)

    def test_origin_and_fetch_site_matrix_for_writes(self):
        for headers, allowed in (({}, True),
                                 ({"Origin": LOCAL}, True),
                                 ({"Origin": "http://evil.example"}, False),
                                 ({"Origin": "http://localhost:8090"}, False),
                                 ({"Origin": "https://127.0.0.1:8090"}, False),
                                 ({"Origin": "null"}, False),
                                 ({"Sec-Fetch-Site": "cross-site"}, False),
                                 ({"Sec-Fetch-Site": "same-origin", "Origin": LOCAL}, True),
                                 ({"Sec-Fetch-Site": "none"}, True)):
            with self.subTest(headers=headers):
                response = self.client.post("/api/heartbeat", base_url=LOCAL, headers=headers)
                self.assertEqual(response.status_code == 200, allowed, response.status_code)

    def test_reads_ignore_origin(self):
        response = self.client.get("/api/about", base_url=LOCAL,
                                   headers={"Origin": "http://evil.example", "Sec-Fetch-Site": "cross-site"})
        self.assertEqual(response.status_code, 200)

    def test_console_style_posts_still_work(self):
        # No Origin, and either no body at all or JSON -- what ureq sends.
        self.assertEqual(self.client.post("/api/queue/clear", base_url=LOCAL).status_code, 200)
        self.assertEqual(self.client.post("/api/queue/clear", base_url=LOCAL, json={}).status_code, 200)
        self.assertEqual(self.client.post("/api/queue/clear", base_url=LOCAL, json=[1]).status_code, 200)
        self.assertEqual(self.client.post("/api/report", base_url=LOCAL,
                                          data="not json").status_code, 400)  # unknown kind, not a 500


class StaticCaching(Base):
    def test_versioned_static_urls_are_immutable_and_plain_ones_are_not(self):
        with self.client.get("/static/radio.js?v=1.2.3") as response:
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response.headers["Cache-Control"], "public, max-age=31536000, immutable")
        with self.client.get("/static/radio.js") as response:
            self.assertNotIn("immutable", response.headers.get("Cache-Control", ""))
        with self.client.get("/api/about?v=1") as response:
            self.assertNotIn("immutable", response.headers.get("Cache-Control", ""))

    def test_woff2_has_a_font_type(self):
        import mimetypes
        self.assertEqual(mimetypes.guess_type("face.woff2")[0], "font/woff2")


class InputChecks(Base):
    def post_raw(self, path, body):
        return self.client.post(path, base_url=LOCAL, data=body, content_type="application/json")

    def test_config_refuses_non_finite_values(self):
        with patch.object(app_module.config.station, "set") as saved:
            for raw in ("NaN", "Infinity", "-Infinity"):
                response = self.post_raw("/api/config", '{"key":"crossfade.duration","value":%s}' % raw)
                self.assertEqual(response.status_code, 400, raw)
            self.assertEqual(self.post_raw("/api/config", '{"key":"crossfade.duration","value":"abc"}').status_code, 400)
            saved.assert_not_called()
            response = self.post_raw("/api/config", '{"key":"crossfade.duration","value":99}')
            self.assertEqual(response.json["value"], 20.0)
            saved.assert_called_once_with("crossfade.duration", 20.0)

    def test_intro_rejects_bad_seconds_with_400(self):
        with patch.object(app_module.db, "one", return_value={"1": 1}), \
                patch.object(app_module.db, "write") as write:
            for body in ('{"key":"k","seconds":"abc"}', '{"key":"k","seconds":NaN}',
                         '{"key":"k","seconds":[1]}', '{"key":"k","seconds":Infinity}'):
                self.assertEqual(self.post_raw("/api/intro", body).status_code, 400, body)
            write.assert_not_called()
            self.assertEqual(self.post_raw("/api/intro", '{"key":"k","seconds":"7.5"}').json["intro_override"], 7.5)
            self.assertIsNone(self.post_raw("/api/intro", '{"key":"k","seconds":null}').json["intro_override"])

    def test_report_rejects_bad_numbers_with_400(self):
        for body in ('{"kind":"played","key":"a","position":"x"}',
                     '{"kind":"played","key":"a","duration":NaN}',
                     '{"kind":"played","key":"a","position":[1]}'):
            self.assertEqual(self.post_raw("/api/report", body).status_code, 400, body)
        self.assertEqual(self.station.reports, [])
        self.assertEqual(self.post_raw("/api/report", '{"kind":"played","key":"a","position":"3"}').status_code, 200)
        self.assertEqual(self.station.reports, [("played", "a", 3.0, 0.0, "")])


class StatusAndSchedule(Base):
    def setUp(self):
        super().setUp()
        for patcher in (patch("radio.ads.for_station"),
                        patch("radio.llm.status", return_value={"configured": False}),
                        patch("radio.app.steam.status", return_value={}),
                        patch("radio.app.taste.summary", return_value={"top_tracks": []}),
                        patch("radio.mixconfig.snapshot", return_value={"fields": []}),
                        patch("radio.app.config.personas", return_value={"mav": {"name": "Mav"}}),
                        patch("radio.app.vibe.public", return_value={})):
            mocked = patcher.start()
            self.addCleanup(patcher.stop)
        from radio import ads
        ads.for_station.return_value.public.return_value = {"enabled": False}

    def test_lite_status_omits_heavy_fields_and_full_status_is_unchanged(self):
        full = self.client.get("/api/status").json
        lite = self.client.get("/api/status?lite=1").json
        for heavy in ("mix_config", "taste", "hosts"):
            self.assertIn(heavy, full)
            self.assertNotIn(heavy, lite)
        self.assertEqual({k: v for k, v in full.items() if k not in ("mix_config", "taste", "hosts")}, lite)

    def test_schedule_now_is_read_after_the_snapshot(self):
        original = self.station.snapshot

        def slow_snapshot():
            data = original()
            self.station.clock.value += 2.5  # time passes while serialising
            return data

        self.station.snapshot = slow_snapshot
        self.assertEqual(self.client.get("/api/schedule").json["now"], 12.5)
        self.assertEqual(self.station.heartbeats, 1)


class EventStreamTests(Base):
    def setUp(self):
        super().setUp()
        self.vibe = {"vibe": {}}
        self.queue = {"items": [{"id": "q", "eta": 1.0}], "now": 1.0}
        self.status = {"note": "on air", "now_playing": {"key": "a", "position": 1.0}}
        patcher = patch.dict(app_module.EVENT_TOPICS, {
            "schedule": app_module.schedule_payload,
            "queue": lambda station: json.loads(json.dumps(self.queue)),
            "status": lambda station: json.loads(json.dumps(self.status)),
            "vibe": lambda station: json.loads(json.dumps(self.vibe)),
        })
        patcher.start()
        self.addCleanup(patcher.stop)
        for patcher in (patch.object(app_module, "_closing", threading.Event()),
                        patch.object(app_module, "_streams", {})):
            patcher.start()
            self.addCleanup(patcher.stop)

    @staticmethod
    def parse(chunk):
        events = []
        for block in chunk.strip().split("\n\n"):
            lines = dict(line.split(": ", 1) for line in block.splitlines() if not line.startswith(":")
                         and ": " in line)
            if "event" in lines:
                events.append((lines["event"], json.loads(lines["data"])))
        return events

    def stream(self, topics):
        self.clock = [0.0]
        self.ticks = 0

        def wait(_seconds):
            self.ticks += 1
            self.clock[0] += 0.5
            return False

        ident = app_module._claim_stream()
        return app_module.EventStream(self.station, topics, ident, clock=lambda: self.clock[0], wait=wait)

    def test_connect_sends_every_topic_matching_the_polled_views(self):
        response = self.client.get("/api/events", base_url=LOCAL, buffered=False)
        try:
            self.assertEqual(response.mimetype, "text/event-stream")
            chunks = response.iter_encoded()
            self.assertEqual(next(chunks), b"retry: 3000\n\n")
            events = dict(self.parse(next(chunks).decode()))
        finally:
            response.close()
        self.assertEqual(list(events), ["schedule", "queue", "status", "vibe"])
        polled = self.client.get("/api/schedule").json
        self.assertEqual(events["schedule"], polled)
        self.assertEqual(events["vibe"], self.vibe)
        self.assertGreaterEqual(self.station.heartbeats, 1)

    def test_topics_filter_and_unknown_topic(self):
        response = self.client.get("/api/events?topics=vibe,vibe", base_url=LOCAL, buffered=False)
        try:
            chunks = response.iter_encoded()
            next(chunks)
            self.assertEqual([name for name, _ in self.parse(next(chunks).decode())], ["vibe"])
        finally:
            response.close()
        self.assertEqual(self.client.get("/api/events?topics=vibe,bogus").status_code, 400)

    def test_pushes_only_changes_resends_schedule_and_pings(self):
        body = self.stream(["schedule", "vibe", "queue", "status"])
        try:
            next(body)
            first = self.parse(next(body))
            self.assertEqual(len(first), 4)
            # Clock-driven fields alone are not a change.
            self.queue["items"][0]["eta"] = 0.5
            self.queue["now"] = 1.5
            self.status["now_playing"]["position"] = 1.5
            self.station.clock.value += 0.5
            self.vibe = {"vibe": {"mood": "calm"}}
            self.assertEqual(self.parse(next(body)), [("vibe", {"vibe": {"mood": "calm"}})])
            self.status["note"] = "changed"
            self.assertEqual([name for name, _ in self.parse(next(body))], ["status"])
            # Nothing changes for a while: the schedule still goes out every 5 s,
            # and a ping every 15 s.
            seen = []
            while self.clock[0] < 15.0:
                seen.append(next(body))
            names = [name for chunk in seen for name, _ in self.parse(chunk)]
            self.assertEqual(set(names), {"schedule"})
            self.assertGreaterEqual(names.count("schedule"), 2)
            self.assertTrue(any(": ping" in chunk for chunk in seen))
        finally:
            body.close()

    def test_close_ends_the_generator_and_frees_the_slot(self):
        body = self.stream(["vibe"])
        next(body)
        self.assertEqual(app_module.open_event_streams(), 1)
        body.close()
        body.close()  # idempotent
        self.assertEqual(app_module.open_event_streams(), 0)
        with self.assertRaises(StopIteration):
            next(body)
        self.assertEqual(app_module.open_event_streams(), 0)

    def test_closing_before_the_first_chunk_frees_the_slot(self):
        response = self.client.get("/api/events?topics=vibe", base_url=LOCAL, buffered=False)
        self.assertEqual(app_module.open_event_streams(), 1)
        response.close()
        self.assertEqual(app_module.open_event_streams(), 0)

    def test_an_abandoned_stream_is_freed_once_it_goes_stale(self):
        # The request thread died without close(): nothing pulls it any more.
        body = self.stream(["vibe"])
        next(body)
        with patch.object(app_module.time, "monotonic", return_value=1e12):
            self.assertIsNotNone(app_module._claim_stream())
        self.assertEqual(app_module.open_event_streams(), 1)  # only the new one
        del body
        import gc
        gc.collect()

    def test_shutdown_ends_open_streams(self):
        body = self.stream(["vibe"])
        next(body)
        next(body)
        app_module._closing.set()
        with self.assertRaises(StopIteration):
            next(body)
        body.close()
        self.assertEqual(self.client.get("/api/events", base_url=LOCAL).status_code, 503)

    def test_stream_count_is_capped(self):
        while app_module._claim_stream() is not None:
            pass
        self.assertEqual(app_module.open_event_streams(), app_module.MAX_EVENT_STREAMS)
        self.assertEqual(self.client.get("/api/events", base_url=LOCAL).status_code, 503)


class LocalMedia(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.local = threading.local()
        for patcher in (patch.object(db, "_DB_PATH", root / "test.db"),
                        patch.object(db, "_LOCAL", self.local)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(lambda: getattr(self.local, "conn", None) and self.local.conn.close())
        folder = root / "My Music" / "Alex G"
        folder.mkdir(parents=True)
        self.audio = folder / "01 Pretend (live) & more [x].flac"
        self.audio.write_bytes(bytes(range(256)) * 16)
        db.write("INSERT INTO tracks (key,title,artist,source,file,added_at) VALUES (?,?,?,?,?,0)",
                 ("alex g|pretend", "Pretend", "Alex G", "local", str(self.audio)))
        db.write("INSERT INTO tracks (key,title,artist,source,file,added_at) VALUES (?,?,?,?,?,0)",
                 ("gone|song", "Song", "Gone", "local", str(root / "missing.flac")))
        self.client = app.test_client()

    def test_serves_the_catalogued_file_with_ranges_and_validators(self):
        url = "/media/track/alex%20g%7Cpretend"
        with self.client.get(url) as response:
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response.data, self.audio.read_bytes())
            self.assertEqual(response.mimetype, "audio/flac")
            etag = response.headers["ETag"]
            self.assertTrue(response.headers.get("Last-Modified"))
        with self.client.get(url, headers={"Range": "bytes=10-19"}) as partial:
            self.assertEqual(partial.status_code, 206)
            self.assertEqual(partial.data, self.audio.read_bytes()[10:20])
        with self.client.get(url, headers={"If-None-Match": etag}) as cached:
            self.assertEqual(cached.status_code, 304)

    def test_unknown_keys_and_missing_files_are_404(self):
        for key in ("nope", "gone%7Csong", "..%2F..%2Fetc%2Fpasswd"):
            self.assertEqual(self.client.get(f"/media/track/{key}").status_code, 404, key)

    def test_cache_route_is_unchanged(self):
        self.assertEqual(self.client.get("/media/audio/..%2Fstation.db").status_code, 404)


class Shutdown(Base):
    def setUp(self):
        super().setUp()
        for patcher in (patch.object(app_module, "_closing", threading.Event()),
                        patch.object(app_module, "_shut_down", False),
                        patch.object(app_module, "_exit_process")):
            mocked = patcher.start()
            self.addCleanup(patcher.stop)
        self.exit = mocked

        class ImmediateTimer:
            def __init__(self, _delay, function):
                self.function = function

            def start(self):
                self.function()

        timer = patch("radio.app.threading.Timer", ImmediateTimer)
        timer.start()
        self.addCleanup(timer.stop)

    def test_shutdown_answers_then_exits_once(self):
        response = self.client.post("/api/shutdown", base_url=LOCAL)
        self.assertEqual(response.json, {"ok": True})
        response.close()
        self.assertEqual(self.station.shutdowns, 1)
        self.assertTrue(app_module._closing.is_set())
        self.exit.assert_called_once_with()
        again = self.client.post("/api/shutdown", base_url=LOCAL)
        again.close()
        self.assertEqual(again.json, {"ok": True})
        self.assertEqual(self.station.shutdowns, 1)
        self.exit.assert_called_once_with()

    def test_cross_site_shutdown_is_refused(self):
        response = self.client.post("/api/shutdown", base_url=LOCAL,
                                    headers={"Origin": "http://evil.example"})
        self.assertEqual(response.status_code, 403)
        self.assertEqual(self.station.shutdowns, 0)


if __name__ == "__main__":
    unittest.main()
