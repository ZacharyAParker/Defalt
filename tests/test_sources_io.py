"""Feed and Steam fetching: parallel, bounded, cached, and quiet about secrets."""
import contextlib
import io
import json
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import httpx
import yaml

from radio import config, db, vault
from radio.sources import rss, steam

FEED = b"""<?xml version="1.0"?><rss version="2.0"><channel><title>Example</title>
<item><title>Headline one</title><link>https://example.com/1</link><description>Body.</description></item>
</channel></rss>"""


class Isolated(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        local = threading.local()
        for p in (patch.object(config, "CACHE_DIR", self.root),
                  patch.object(db, "_DB_PATH", self.root / "t.db"),
                  patch.object(db, "_LOCAL", local)):
            p.start()
            self.addCleanup(p.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        rss._CACHE.clear()
        steam._CACHE.clear()
        self.addCleanup(rss._CACHE.clear)
        self.addCleanup(steam._CACHE.clear)


class Feeds(Isolated):
    def test_feeds_are_fetched_in_parallel_within_one_budget(self):
        def fetch(url, **kwargs):
            if "slow" in url:
                time.sleep(3)
            else:
                time.sleep(.2)
            return httpx.Response(200, content=FEED, request=httpx.Request("GET", url))
        urls = [f"https://example.com/{i}" for i in range(4)] + ["https://slow.example.com/"]
        with patch.object(rss, "BATCH_GRACE", .5), patch.object(rss.httpx, "get", side_effect=fetch):
            started = time.monotonic()
            results = rss._fetch_all(urls, timeout=.3)
            elapsed = time.monotonic() - started
        self.assertLess(elapsed, 1.5)
        self.assertEqual(list(results), urls[:4])

    def test_feed_cache_is_on_disk_for_restarts_and_worker_processes(self):
        with patch.object(rss.httpx, "get", return_value=httpx.Response(
                200, content=FEED, request=httpx.Request("GET", "https://example.com/feed"))) as get:
            first = rss._fetch_feed("https://example.com/feed", 5)
            rss._CACHE.clear()  # a new process
            second = rss._fetch_feed("https://example.com/feed", 5)
        self.assertEqual(get.call_count, 1)
        self.assertEqual(first, second)
        self.assertEqual(first[0]["title"], "Headline one")

    def test_pruning_news_keeps_other_ledgers(self):
        old = time.time() - 400 * 86400
        for kind in ("news", "patch", "ad", "music_meme"):
            db.write("INSERT INTO seen(kind, ident, ts) VALUES(?,?,?)", (kind, "x", old))
        rss.mark_read([{"ident": "fresh"}])
        kinds = sorted({row["kind"] for row in db.query("SELECT kind FROM seen")})
        self.assertEqual(kinds, ["ad", "music_meme", "news", "patch"])
        self.assertFalse(db.one("SELECT 1 FROM seen WHERE kind='news' AND ident='x'"))

    def test_prune_uses_the_kind_filter_when_available(self):
        with patch.object(rss.db, "prune_seen") as prune, patch.object(rss.db, "mark_seen"):
            rss.mark_read([{"ident": "a"}])
        self.assertEqual(prune.call_args.kwargs, {"kind": "news"})


class Steam(Isolated):
    def test_errors_never_print_the_api_key(self):
        request = httpx.Request("GET", "https://api.steampowered.com/x?key=SECRET-KEY")
        error = httpx.HTTPStatusError("403 for https://api.steampowered.com/x?key=SECRET-KEY",
                                      request=request, response=httpx.Response(403, request=request))
        output = io.StringIO()
        with patch.object(config, "DEBUG", True), patch.object(steam.httpx, "get", side_effect=error), \
                contextlib.redirect_stdout(output):
            self.assertIsNone(steam._get("https://api.steampowered.com/x", {"key": "SECRET-KEY"}))
        self.assertNotIn("SECRET-KEY", output.getvalue())
        self.assertIn("HTTPStatusError HTTP 403", output.getvalue())

    def test_patch_news_is_cached_and_fetched_in_parallel(self):
        games = [{"appid": i, "name": f"Game {i}"} for i in range(1, 5)]
        def news(url, params=None, timeout=15.0):
            time.sleep(.2)
            return {"appnews": {"newsitems": []}}
        with patch.object(steam, "tracked_titles", return_value=games), \
                patch.object(steam, "_get", side_effect=news) as get:
            started = time.monotonic()
            self.assertIsNone(steam.latest_patch())
            self.assertLess(time.monotonic() - started, .7)
            steam.latest_patch()
        self.assertEqual(get.call_count, 4)  # the second lookup is served from cache


class SourceWorker(unittest.TestCase):
    def test_non_youtube_operations_do_not_import_yt_dlp(self):
        script = ("import io, json, sys\n"
                  "import radio.articles as a\n"
                  "a.fetch = lambda url: {'text': 'ok'}\n"
                  "sys.argv = ['sourceio', 'article']\n"
                  "sys.stdin = io.StringIO(json.dumps({'url': 'https://example.com'}))\n"
                  "import radio.sourceio as s\n"
                  "s.main()\n"
                  "print('yt_dlp' in sys.modules)\n")
        result = subprocess.run([sys.executable, "-c", script], cwd=config.ROOT, capture_output=True,
                                text=True, timeout=60)
        self.assertEqual(result.stdout.strip().splitlines()[-1], "False", result.stderr)


class Vault(unittest.TestCase):
    def test_reserved_and_long_names_are_safe_and_distinct(self):
        self.assertEqual(vault._safe("CON"), "_CON")
        self.assertEqual(vault._safe("nul.txt"), "_nul.txt")
        self.assertEqual(vault._safe("Conway"), "Conway")
        long_a, long_b = vault._safe("x" * 200 + "a"), vault._safe("x" * 200 + "b")
        self.assertLessEqual(len(long_a), vault.NAME_LIMIT)
        self.assertNotEqual(long_a, long_b)
        self.assertEqual(vault._safe("x" * 200 + "a"), long_a)

    def test_frontmatter_survives_quotes_colons_and_newlines(self):
        tricky = 'He said "hi": the\nsequel'
        text = f"---\nartist: {vault._yaml(tricky)}\n---"
        self.assertEqual(yaml.safe_load(text.split("---")[1])["artist"], tricky)

    def test_artist_note_matches_whole_credits_and_escapes_like(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        local = threading.local()
        written = {}
        with patch.object(db, "_DB_PATH", Path(temp.name) / "t.db"), patch.object(db, "_LOCAL", local), \
                patch.object(vault.taste, "affinity", return_value=0.0), \
                patch.object(vault, "_write", side_effect=lambda path, body: written.update({path.name: body})):
            for key, artist, title in (("1", "Ye", "Real"), ("2", "Yeat", "Not Ye"), ("3", "Kanye West", "Other"),
                                       ("4", "Ye feat. Someone", "Feature"), ("5", "100%_Band", "Pct")):
                db.write("INSERT INTO tracks(key,title,artist,play_count,added_at) VALUES(?,?,?,?,0)",
                         (key, title, artist, 1))
            vault.write_artist_note("Ye")
            vault.write_artist_note("100%_Band")
            local.conn.close()
        body = written["Ye.md"]
        self.assertIn("Real", body)
        self.assertIn("Feature", body)
        self.assertNotIn("Not Ye", body)
        self.assertNotIn("Other", body)
        self.assertIn("Pct", written["100%_Band.md"])


if __name__ == "__main__":
    unittest.main()
