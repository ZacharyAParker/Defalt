import base64
import io
import json
import re
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from radio import feedback


def iso(moment, offset_hours=None):
    zone = timezone.utc if offset_hours is None else timezone(timedelta(hours=offset_hours))
    return datetime.fromtimestamp(moment, zone).isoformat(timespec="milliseconds")


class Folder(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.reports = self.root / "reports"
        self.logs = self.root / "logs"
        self.logs.mkdir()

    def tearDown(self):
        self._tmp.cleanup()


class Storage(Folder):
    def test_a_report_gets_a_folder_and_an_inbox_line(self):
        at = time.time()
        (self.logs / "station.log").write_text(
            f"{iso(at - 30)} [out] picked a record\n{iso(at - 3600)} [out] an hour ago\n", encoding="utf-8")
        result = feedback.submit(
            {"kind": "bug", "title": "Skip | lags", "description": "Pressed skip, waited.",
             "expected": "It skips.", "client": "browser", "client_context": {"page": "radio"},
             "client_logs": [f"{iso(at - 5)} audio stalled"]},
            context={"status_note": "on air", "now_playing": {"title": "One"}},
            reports_dir=self.reports, log_dir=self.logs, now=at, redactor=feedback.Redactor([]))
        self.assertRegex(result["id"], r"^\d{8}-\d{6}-skip-lags$")
        self.assertEqual(result["path"], f"reports/{result['id']}/")
        folder = self.reports / result["id"]
        self.assertEqual({p.name for p in folder.iterdir()}, {"report.md", "context.json", "logs.txt"})
        report = (folder / "report.md").read_text(encoding="utf-8")
        self.assertIn("# Bug: Skip / lags", report)
        self.assertIn("- status: open", report)
        self.assertIn("## What I expected", report)
        self.assertIn("- station: on air", report)
        context = json.loads((folder / "context.json").read_text(encoding="utf-8"))
        self.assertEqual(context["station"]["now_playing"]["title"], "One")
        self.assertEqual(context["browser"], {"page": "radio"})
        self.assertIn("version", context["environment"])
        logs = (folder / "logs.txt").read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(logs), 2)
        self.assertIn("station [out] picked a record", logs[0])
        self.assertIn("browser audio stalled", logs[1])
        inbox = (self.reports / "INBOX.md").read_text(encoding="utf-8")
        self.assertTrue(inbox.startswith("# Feedback inbox"))
        rows = feedback.entries(self.reports)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["status"], "open")
        self.assertEqual(rows[0]["title"], "Skip / lags")
        self.assertEqual(rows[0]["path"], result["path"])

    def test_two_reports_in_one_second_do_not_collide(self):
        at = time.time()
        first = feedback.submit({"title": "Same", "attach_logs": False}, reports_dir=self.reports,
                                log_dir=self.logs, now=at, redactor=feedback.Redactor([]))
        second = feedback.submit({"title": "Same", "attach_logs": False}, reports_dir=self.reports,
                                 log_dir=self.logs, now=at, redactor=feedback.Redactor([]))
        self.assertEqual(second["id"], first["id"] + "-2")
        self.assertFalse((self.reports / first["id"] / "logs.txt").exists())
        self.assertEqual(len(feedback.entries(self.reports)), 2)

    def test_a_title_can_come_from_the_description(self):
        result = feedback.submit({"kind": "Suggestion", "description": "More funk please\nand less talk"},
                                 reports_dir=self.reports, log_dir=self.logs, redactor=feedback.Redactor([]))
        self.assertTrue(result["id"].endswith("more-funk-please"))
        report = (self.reports / result["id"] / "report.md").read_text(encoding="utf-8")
        self.assertIn("# Suggestion: More funk please", report)

    def test_bad_reports_are_refused(self):
        for payload in (None, {"kind": "rant", "title": "x"}, {"title": ""}, {"title": 3},
                        {"title": "x", "client": "fax"}, {"title": "x", "attach_logs": "yes"},
                        {"title": "x", "client_context": []},
                        {"title": "x", "screenshot_png": base64.b64encode(b"GIF89a").decode()}):
            with self.assertRaises(ValueError):
                feedback.validate(payload)


class LogWindow(Folder):
    def test_rotated_files_and_mixed_stamps_merge_in_order(self):
        at = 1_790_000_000.0
        (self.logs / "station.log.1").write_text(
            f"{iso(at - 1200, -5)} [out] too early\n"
            f"{iso(at - 500, -5)} [err] Traceback (most recent call last):\n"
            "  File \"x.py\", line 1\n"
            "ValueError: boom\n", encoding="utf-8")
        (self.logs / "station.log").write_text(
            f"{iso(at - 100, 2)} [out] after rotation\n"
            f"{iso(at + 30, 2)} [out] just after\n"
            f"{iso(at + 120, 2)} [out] too late\n", encoding="utf-8")
        (self.logs / "console.log").write_text(
            f"{iso(at - 400)} [console] station started\n"
            f"{iso(at - 99.5)} [station] after rotation\n"
            f"{iso(at - 50)} [station:err] only the console saw this\n", encoding="utf-8")
        lines = feedback.log_window(at, self.logs)
        texts = [line.split(" ", 1)[1] for line in lines]
        self.assertEqual(texts, [
            "station [err] Traceback (most recent call last):",
            'station   File "x.py", line 1',
            "station ValueError: boom",
            "console [console] station started",
            "station [out] after rotation",
            "console [station:err] only the console saw this",
            "station [out] just after",
        ])
        stamps = [feedback.parse_stamp(line.split(" ", 1)[0]) for line in lines]
        self.assertEqual(stamps, sorted(stamps))

    def test_the_window_is_capped_to_the_newest_lines(self):
        at = time.time()
        (self.logs / "station.log").write_text(
            "".join(f"{iso(at - 300 + i)} [out] line {i}\n" for i in range(50)), encoding="utf-8")
        lines = feedback.log_window(at, self.logs, limit=10)
        self.assertEqual(len(lines), 11)
        self.assertIn("40 earlier lines left out", lines[0])
        self.assertTrue(lines[-1].endswith("line 49"))

    def test_rolling_log_rotates_and_keeps_a_fixed_number(self):
        log = feedback.RollingLog(self.logs / "station.log", max_bytes=200, backups=2)
        for i in range(40):
            log.write("out", f"line {i} " + "x" * 20)
        log.close()
        names = sorted(p.name for p in self.logs.iterdir())
        self.assertEqual(names, ["station.log", "station.log.1", "station.log.2"])
        for path in self.logs.iterdir():
            self.assertLessEqual(path.stat().st_size, 200)
            for line in path.read_text(encoding="utf-8").splitlines():
                self.assertIsNotNone(feedback.parse_stamp(line.split(" ", 1)[0]))
        self.assertIn("line 39", (self.logs / "station.log").read_text(encoding="utf-8"))

    def test_the_tee_passes_output_through_and_logs_whole_lines(self):
        log = feedback.RollingLog(self.logs / "station.log")
        original = io.StringIO()
        tee = feedback._Tee(original, log, "err")
        tee.write("half a ")
        tee.write("line\nprogress 10%\rprogress 100%\n\n")
        tee.flush()
        log.close()
        self.assertEqual(original.getvalue(), "half a line\nprogress 10%\rprogress 100%\n\n")
        lines = (self.logs / "station.log").read_text(encoding="utf-8").splitlines()
        self.assertEqual([line.split(" ", 1)[1] for line in lines], ["[err] half a line", "[err] progress 100%"])


class Redaction(Folder):
    def test_keys_and_tokens_are_scrubbed(self):
        (self.root / ".env").write_text(
            "OPENROUTER_API_KEY=abcd1234efgh5678\nPORT=8090\nHOST=127.0.0.1\nCACHE_DIR=cache\n"
            "SPOTIFY_CLIENT_SECRET='shh-its-a-secret'\n", encoding="utf-8")
        scrub = feedback.Redactor(feedback._env_secrets(self.root))
        text = scrub.text(
            "key abcd1234efgh5678 used; sk-or-v1-0123456789abcdef; Authorization: Bearer eyJhbGciOi.xyz; "
            "?api_key=zzzzzzzz&key=AbCdEfGhIjKlMnOpQrStUv&key=daft%20punk%7Cone; "
            "{\"access_token\": \"tok-value-1\"} secret shh-its-a-secret on 127.0.0.1:8090")
        for leaked in ("abcd1234efgh5678", "0123456789abcdef", "eyJhbGciOi", "zzzzzzzz",
                       "AbCdEfGhIjKlMnOpQrStUv", "tok-value-1", "shh-its-a-secret"):
            self.assertNotIn(leaked, text)
        self.assertIn("key=daft%20punk%7Cone", text)
        self.assertIn("127.0.0.1:8090", text)

    def test_context_values_under_secret_names_are_dropped(self):
        scrub = feedback.Redactor([])
        value = scrub.value({"key": "daft punk|one", "client_secret": "abc", "nested": [{"token": 12}],
                             "note": "Bearer abcdefghijkl", "llm": {"configured": True}})
        self.assertEqual(value["key"], "daft punk|one")
        self.assertEqual(value["client_secret"], "[redacted]")
        self.assertEqual(value["nested"][0]["token"], "[redacted]")
        self.assertEqual(value["note"], "Bearer [redacted]")
        self.assertEqual(value["llm"], {"configured": True})

    def test_a_report_never_carries_a_secret(self):
        at = time.time()
        (self.logs / "station.log").write_text(f"{iso(at - 1)} [out] using sk-live-abcdefghijklmnop\n",
                                               encoding="utf-8")
        result = feedback.submit({"title": "Leak", "description": "my key is sk-live-abcdefghijklmnop",
                                  "client_context": {"api_key": "sk-live-abcdefghijklmnop"}},
                                 reports_dir=self.reports, log_dir=self.logs, now=at,
                                 redactor=feedback.Redactor([]))
        folder = self.reports / result["id"]
        for path in folder.iterdir():
            self.assertNotIn("abcdefghijklmnop", path.read_text(encoding="utf-8"))


class Inbox(Folder):
    def test_close_marks_the_inbox_and_the_report(self):
        first = feedback.submit({"title": "One", "attach_logs": False}, reports_dir=self.reports,
                                redactor=feedback.Redactor([]))
        second = feedback.submit({"title": "Two", "attach_logs": False}, reports_dir=self.reports,
                                 redactor=feedback.Redactor([]))
        row = feedback.close(first["id"][:15] + "-one", "fixed in abc123", reports_dir=self.reports)
        self.assertEqual(row["status"], "closed")
        rows = {r["id"]: r for r in feedback.entries(self.reports)}
        self.assertEqual(rows[first["id"]]["status"], "closed")
        self.assertIn("fixed in abc123", rows[first["id"]]["note"])
        self.assertEqual(rows[second["id"]]["status"], "open")
        inbox = (self.reports / "INBOX.md").read_text(encoding="utf-8")
        self.assertIn(f"- [x] {first['id']} |", inbox)
        self.assertIn(f"- [ ] {second['id']} |", inbox)
        self.assertTrue(inbox.startswith("# Feedback inbox"))
        report = (self.reports / first["id"] / "report.md").read_text(encoding="utf-8")
        self.assertRegex(report, r"- status: closed \d{4}-\d\d-\d\d \d\d:\d\d: fixed in abc123")

    def test_ids_can_be_shortened_but_not_ambiguously(self):
        feedback.submit({"title": "Alpha", "attach_logs": False}, reports_dir=self.reports,
                        redactor=feedback.Redactor([]))
        feedback.submit({"title": "Alpha", "attach_logs": False}, reports_dir=self.reports,
                        redactor=feedback.Redactor([]))
        with self.assertRaises(KeyError):
            feedback.find("2", self.reports)
        with self.assertRaises(KeyError):
            feedback.find("nope", self.reports)

    def test_cli_lists_shows_and_closes(self):
        result = feedback.submit({"title": "From the CLI", "attach_logs": False}, reports_dir=self.reports,
                                 redactor=feedback.Redactor([]))
        with patch.object(feedback, "REPORTS_DIR", self.reports), \
                patch.object(feedback.config, "ROOT", self.root), \
                patch("sys.stdout", new_callable=io.StringIO) as out:
            self.assertEqual(feedback.main(["list"]), 0)
            self.assertEqual(feedback.main(["show", result["id"]]), 0)
            self.assertEqual(feedback.main(["close", result["id"], "done", "now"]), 0)
            self.assertEqual(feedback.main(["list", "--open"]), 0)
        text = out.getvalue()
        self.assertIn(f"open    {result['id']}", text)
        self.assertIn("# Bug: From the CLI", text)
        self.assertIn("closed", text)
        self.assertIn("done now", text)
        with patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual(feedback.main(["frobnicate"]), 2)


class FakeStation:
    status_note = "on air"

    def __init__(self, slow=False):
        import threading
        self.lock = threading.RLock()
        self.slow = slow

    def now_playing(self):
        if self.slow:
            time.sleep(2)
        raise RuntimeError("decoder fell over")

    def lineup(self):
        return [{"id": "q1", "title": "Next"}]


class Endpoint(Folder):
    def setUp(self):
        super().setUp()
        from radio.app import app
        self.client = app.test_client()
        self.patches = [patch.object(feedback, "REPORTS_DIR", self.reports),
                        patch.object(feedback, "LOG_DIR", self.logs),
                        patch.object(feedback, "station_context", return_value={"status_note": "testing"})]
        for p in self.patches:
            p.start()

    def tearDown(self):
        for p in self.patches:
            p.stop()
        super().tearDown()

    def test_post_files_and_get_lists(self):
        png = base64.b64encode(b"\x89PNG\r\n\x1a\nrest").decode()
        response = self.client.post("/api/feedback", json={
            "kind": "bug", "title": "Console froze", "description": "It froze.",
            "client": "console", "client_context": {"decks": []}, "screenshot_png": png,
            "client_logs": ["2026-01-01T00:00:00.000Z [console] ancient"]})
        self.assertEqual(response.status_code, 201)
        body = response.get_json()
        self.assertTrue(re.fullmatch(r"\d{8}-\d{6}-console-froze", body["id"]))
        folder = self.reports / body["id"]
        self.assertEqual((folder / "screenshot.png").read_bytes()[:4], b"\x89PNG")
        context = json.loads((folder / "context.json").read_text(encoding="utf-8"))
        self.assertEqual(context["station"], {"status_note": "testing"})
        self.assertEqual(context["console"], {"decks": []})
        listing = self.client.get("/api/feedback").get_json()["reports"]
        self.assertEqual([(r["id"], r["kind"], r["title"], r["status"]) for r in listing],
                         [(body["id"], "bug", "Console froze", "open")])
        self.assertIn("ts", listing[0])

    def test_bad_posts_are_refused_without_writing(self):
        response = self.client.post("/api/feedback", json={"kind": "complaint", "title": "x"})
        self.assertEqual(response.status_code, 400)
        self.assertIn("error", response.get_json())
        self.assertFalse(self.reports.exists())


class Context(unittest.TestCase):
    def test_one_broken_piece_does_not_lose_the_rest(self):
        with patch("radio.director.station", return_value=FakeStation()):
            context = feedback.station_context(timeout=2)
        self.assertEqual(context["status_note"], "on air")
        self.assertEqual(context["queue"], [{"id": "q1", "title": "Next"}])
        self.assertIn("decoder fell over", context["unavailable"]["now_playing"])

    def test_a_stuck_station_is_abandoned(self):
        with patch("radio.director.station", return_value=FakeStation(slow=True)):
            started = time.monotonic()
            context = feedback.station_context(timeout=0.2)
        self.assertLess(time.monotonic() - started, 1.5)
        self.assertEqual(context["status_note"], "on air")
        self.assertIn("timeout", context["unavailable"])


if __name__ == "__main__":
    unittest.main()
