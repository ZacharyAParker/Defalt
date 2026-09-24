"""Synced lyrics: LRC, the LRCLIB client, the section map and where it is used."""
import json
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import httpx

from radio import (config, db, director, lyric_sections, lyrics, mixplanner, structure,
                   timeline, transitions)
from radio.segments import personal
from radio.segments.base import Line


VERSE_1 = ["walking down the empty road", "counting every passing car",
           "nothing here but me and dust", "wondering where you are"]
CHORUS = ["hold on hold on to the light", "we are burning through the night",
          "hold on hold on dont let go", "this is all we know"]
VERSE_2 = ["morning came without a sound", "coffee cold upon the stair",
           "every clock is running down", "and still you are not there"]
BRIDGE = ["maybe if the rain would stop", "maybe if the river turned",
          "i would tell you what i lost", "and all the things i learned"]


def song(start=14.0, step=3.5, pauses=False):
    """Verse, chorus, verse, chorus, bridge, chorus; (lines, last line end)."""
    t, lines = start, []
    for block in (VERSE_1, CHORUS, VERSE_2, CHORUS, BRIDGE, CHORUS):
        for text in block:
            lines.append({"t": t, "text": text})
            t += step
        if pauses:
            lines.append({"t": t, "text": ""})
            t += step
    return lines, t


def lrc(lines):
    def stamp(t):
        return f"[{int(t // 60):02d}:{t % 60:05.2f}]"
    return "\n".join(stamp(line["t"]) + line["text"] for line in lines)


class Response:
    def __init__(self, status, body=None, headers=None):
        self.status_code = status
        self._body = body
        self.headers = headers or {}

    def json(self):
        return self._body

    def raise_for_status(self):
        if self.status_code >= 400:
            raise httpx.HTTPStatusError("bad", request=None, response=None)


class Database(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.local = threading.local()
        for patcher in (patch.object(db, "_DB_PATH", self.root / "test.db"),
                        patch.object(db, "_LOCAL", self.local),
                        patch.object(config, "CACHE_DIR", self.root)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(lambda: getattr(self.local, "conn", None) and self.local.conn.close())
        self.settings = {}
        mocked = patch.object(config.station, "get",
                              side_effect=lambda key, default=None: self.settings.get(key, default))
        mocked.start()
        self.addCleanup(mocked.stop)
        for patcher in (patch.object(lyrics, "_sleep", lambda seconds: None),
                        patch.object(lyrics, "_backoff_until", 0.0),
                        patch.object(lyrics, "_last_call", 0.0),
                        patch.object(lyrics, "_failures", 0)):
            patcher.start()
            self.addCleanup(patcher.stop)
        lyrics._queue.clear()
        self.addCleanup(lyrics._queue.clear)

    def add_track(self, key="band|song", title="Song", artist="Band", duration=200.0, **extra):
        columns = {"key": key, "title": title, "artist": artist, "duration": duration,
                   "added_at": 0, **extra}
        db.write(f"INSERT INTO tracks ({','.join(columns)}) VALUES ({','.join('?' * len(columns))})",
                 tuple(columns.values()))
        return dict(db.one("SELECT * FROM tracks WHERE key=?", (key,)))


class LrcParsing(unittest.TestCase):
    def test_multiple_timestamps_offsets_and_junk(self):
        text = ("[ar:Band]\n[ti:Song]\n[offset:+500]\n"
                "[00:12.50][01:02.00]hello there\n"
                "[00:14.00]\n[00:14.50]\n"
                "[00:15.20]<00:15.20>word <00:16.00>two\n"
                "just words\n[xx:yy]nope\n[00:20:30]colon style")
        self.assertEqual(lyrics.parse_lrc(text), [
            {"t": 12.0, "text": "hello there"}, {"t": 13.5, "text": ""},
            {"t": 14.7, "text": "word two"}, {"t": 19.8, "text": "colon style"},
            {"t": 61.5, "text": "hello there"}])

    def test_negative_offset_delays_and_nothing_goes_below_zero(self):
        self.assertEqual(lyrics.parse_lrc("[offset:-1000]\n[00:01.00]a")[0]["t"], 2.0)
        self.assertEqual(lyrics.parse_lrc("[offset:3000]\n[00:01.00]a")[0]["t"], 0.0)

    def test_leading_pauses_and_duplicates_go(self):
        lines = lyrics.parse_lrc("[00:00.00]\n[00:05.00]one\n[00:05.00]one\n[00:09.00]two")
        self.assertEqual([line["text"] for line in lines], ["one", "two"])

    def test_garbage_is_empty(self):
        for value in (None, 12, "", "no lyrics here", "[00:ab]x"):
            self.assertEqual(lyrics.parse_lrc(value), [])


class Client(Database):
    def record(self, **extra):
        lines, _ = song()
        return {"id": 7, "trackName": "Song", "artistName": "Band", "albumName": "LP",
                "duration": 201.0, "instrumental": False, "plainLyrics": "hold on",
                "syncedLyrics": lrc(lines), **extra}

    def test_get_with_a_matching_length_is_stored_with_sections(self):
        track = self.add_track(album="LP", beat_period=0.5, downbeat_offset=0.0,
                               beat_residual_ms=5, bpm_confidence=0.9)
        calls = []

        def get(url, params=None, headers=None, timeout=None):
            calls.append((url, dict(params), headers))
            return Response(200, self.record())
        with patch.object(lyrics.httpx, "get", get):
            self.assertEqual(lyrics.fetch(track), "synced")
        self.assertEqual(len(calls), 1)
        url, params, headers = calls[0]
        self.assertTrue(url.endswith("/api/get"))
        self.assertEqual(params, {"track_name": "Song", "artist_name": "Band",
                                  "album_name": "LP", "duration": 200})
        self.assertTrue(headers["User-Agent"].startswith("Defalt/"))
        self.assertIn("github.com/ZacharyAParker/Defalt", headers["User-Agent"])
        found = lyrics.payload(track["key"])
        self.assertEqual(found["status"], "synced")
        self.assertEqual(found["lines"][0], {"t": 14.0, "text": VERSE_1[0]})
        labels = [s["label"] for s in found["sections"]]
        self.assertEqual(labels, ["intro", "verse", "chorus", "verse", "chorus", "bridge", "chorus", "outro"])
        self.assertTrue(found["vocal_spans"])
        # Every inner boundary landed on the bar grid (two-second bars).
        for section in found["sections"][1:]:
            self.assertAlmostEqual(section["start"] % 2.0, 0.0, places=2)

    def test_a_different_length_falls_back_to_search_and_picks_the_close_one(self):
        track = self.add_track()
        seen = []

        def get(url, params=None, headers=None, timeout=None):
            seen.append(url.rsplit("/", 1)[-1])
            if url.endswith("/get"):
                return Response(200, self.record(duration=260.0))
            return Response(200, [self.record(id=1, duration=240.0),
                                  self.record(id=2, duration=202.0, syncedLyrics=None),
                                  self.record(id=3, duration=199.0),
                                  self.record(id=4, trackName="Other Song", duration=200.0)])
        with patch.object(lyrics.httpx, "get", get):
            self.assertEqual(lyrics.fetch(track), "synced")
        self.assertEqual(seen, ["get", "search"])
        self.assertEqual(lyrics.row(track["key"])["lrclib_id"], 3)

    def test_plain_only_and_instrumental_answers(self):
        plain = self.add_track("band|plain", "Plain")
        quiet = self.add_track("band|quiet", "Quiet")

        def get(url, params=None, headers=None, timeout=None):
            if params["track_name"] == "Plain":
                return Response(200, self.record(trackName="Plain", syncedLyrics=None,
                                                 plainLyrics="one line\nanother line"))
            return Response(200, self.record(trackName="Quiet", syncedLyrics=None, plainLyrics=None,
                                             instrumental=True))
        with patch.object(lyrics.httpx, "get", get):
            self.assertEqual(lyrics.fetch(plain), "plain")
            self.assertEqual(lyrics.fetch(quiet), "instrumental")
        self.assertEqual(lyrics.payload("band|plain")["plain"], "one line\nanother line")
        self.assertEqual(lyrics.payload("band|plain")["lines"], [])
        self.assertTrue(lyrics.payload("band|quiet")["instrumental"])

    def test_a_miss_is_not_asked_again_for_a_fortnight(self):
        track = self.add_track()
        calls = []

        def get(url, params=None, headers=None, timeout=None):
            calls.append(url)
            return Response(404, {"statusCode": 404})
        with patch.object(lyrics.httpx, "get", get):
            now = time.time()
            self.assertEqual(lyrics.fetch(track, now=now), "missing")
            self.assertEqual(len(calls), 2)      # get, then search
            self.assertEqual(lyrics.fetch(track, now=now + 13 * 86400), "missing")
            self.assertEqual(len(calls), 2)
            self.assertEqual(lyrics.pending(10, now=now + 13 * 86400), [])
            self.assertEqual([t["key"] for t in lyrics.pending(10, now=now + 15 * 86400)], [track["key"]])
            lyrics.fetch(track, now=now + 15 * 86400)
            self.assertEqual(len(calls), 4)

    def test_network_errors_retry_sooner_and_never_wipe_found_lyrics(self):
        track = self.add_track()

        def broken(*args, **kwargs):
            raise httpx.ConnectError("offline")
        with patch.object(lyrics.httpx, "get", broken):
            now = time.time()
            self.assertEqual(lyrics.fetch(track, now=now), "error")
            self.assertFalse(lyrics.due(lyrics.row(track["key"]), now + 3600))
            self.assertTrue(lyrics.due(lyrics.row(track["key"]), now + 7 * 3600))
        self.assertIsNone(lyrics.payload(track["key"]))

    def test_rate_limit_backs_off_and_keeps_the_work_queued(self):
        first, second = self.add_track("band|a", "A"), self.add_track("band|b", "B")
        self.settings["lyrics.backfill"] = False

        def limited(url, params=None, headers=None, timeout=None):
            return Response(429, {}, {"Retry-After": "120"})
        lyrics.request(first)
        lyrics.request(second)
        lyrics.request(first)          # asked twice, queued once
        self.assertEqual(len(lyrics._queue), 2)
        with patch.object(lyrics.httpx, "get", limited):
            self.assertEqual(lyrics.run_batch(), {})
        self.assertGreater(lyrics._backoff_until, time.monotonic() + 100)
        self.assertEqual([t["key"] for t in lyrics._queue], ["band|a", "band|b"])
        self.assertIsNone(lyrics.row("band|a"))  # nothing learned, nothing stored

    def test_server_errors_back_off_exponentially(self):
        track = self.add_track()
        with patch.object(lyrics.httpx, "get", lambda *a, **k: Response(503)):
            with self.assertRaises(lyrics.Unavailable):
                lyrics.fetch(track)
            first = lyrics._backoff_until - time.monotonic()
            lyrics._backoff_until = 0.0
            with self.assertRaises(lyrics.Unavailable):
                lyrics.fetch(track)
            self.assertGreater(lyrics._backoff_until - time.monotonic(), first * 1.5)

    def test_disabled_never_queues_or_calls(self):
        self.settings["lyrics.enabled"] = False
        track = self.add_track()
        lyrics.request(track)
        self.assertEqual(len(lyrics._queue), 0)
        with patch.object(lyrics.httpx, "get", side_effect=AssertionError("called LRCLIB")):
            self.assertEqual(lyrics.run_batch(), {})

    def test_unknown_artists_are_not_looked_up(self):
        track = self.add_track("x", "Song", "Unknown Artist")
        with patch.object(lyrics.httpx, "get", side_effect=AssertionError("called LRCLIB")):
            self.assertEqual(lyrics.fetch(track), "skipped")

    def test_attach_carries_the_map_and_leaves_embedded_lyrics_alone(self):
        track = self.add_track(lyrics="embedded words")
        with patch.object(lyrics.httpx, "get", lambda *a, **k: Response(200, self.record())):
            lyrics.fetch(track)
        attached = lyrics.attach(track)
        self.assertEqual(attached["lyric_map"]["first_line"], 14.0)
        self.assertEqual(attached["lyric_map"]["status"], "synced")
        self.assertEqual(db.one("SELECT lyrics FROM tracks WHERE key=?", (track["key"],))["lyrics"],
                         "embedded words")
        self.assertNotIn("lyric_map", lyrics.attach(self.add_track("other|song", "Other")))

    def test_api_serves_lines_and_sections_and_404s_otherwise(self):
        from radio.app import app
        track = self.add_track()
        with patch.object(lyrics.httpx, "get", lambda *a, **k: Response(200, self.record())):
            lyrics.fetch(track)
        client = app.test_client()
        response = client.get("/api/lyrics/band%7Csong")
        self.assertEqual(response.status_code, 200)
        body = response.get_json()
        self.assertEqual(body["lines"][1]["text"], VERSE_1[1])
        self.assertIn("chorus", [s["label"] for s in body["sections"]])
        self.assertEqual(client.get("/api/lyrics/nobody%7Cnothing").status_code, 404)
        # Same protection as every other route.
        self.assertEqual(client.get("/api/lyrics/band%7Csong", headers={"Host": "evil.example"}).status_code, 403)


class Sections(unittest.TestCase):
    def test_verse_chorus_verse_chorus_bridge_chorus(self):
        for pauses in (False, True):
            lines, end = song(pauses=pauses)
            found = lyric_sections.sections(lines, end + 20)
            self.assertEqual([s["label"] for s in found],
                             ["intro", "verse", "chorus", "verse", "chorus", "bridge", "chorus", "outro"], pauses)
            self.assertEqual(found[0]["start"], 0.0)
            self.assertEqual(found[-1]["end"], round(end + 20, 2))
            for a, b in zip(found, found[1:]):
                self.assertEqual(a["end"], b["start"])
            self.assertTrue(all(0 < s["confidence"] <= 1 for s in found))

    def test_a_long_gap_between_lines_is_an_instrumental_break(self):
        lines, _ = song()
        moved = [{**line, "t": line["t"] + (30 if line["t"] >= 56 else 0)} for line in lines]
        labels = [s["label"] for s in lyric_sections.sections(moved, 200)]
        self.assertIn("instrumental", labels)
        self.assertEqual(labels.count("chorus"), 3)

    def test_too_few_lines_is_no_map(self):
        self.assertEqual(lyric_sections.sections([{"t": 1, "text": "a"}, {"t": 5, "text": "b"}], 60), [])

    def test_vocal_spans_run_line_to_line_capped_and_merged(self):
        lines = [{"t": 10, "text": "a b c"}, {"t": 13, "text": "d e f"}, {"t": 16, "text": ""},
                 {"t": 40, "text": "g h"}, {"t": 43, "text": "i j"}]
        self.assertEqual(lyric_sections.vocal_spans(lines, 60), [[10.0, 16.0], [40.0, 49.0]])
        self.assertTrue(lyric_sections.vocal_at([[10, 16]], 12))
        self.assertFalse(lyric_sections.vocal_at([[10, 16]], 20))
        self.assertIsNone(lyric_sections.vocal_at([], 20))
        self.assertAlmostEqual(lyric_sections.vocal_fraction([[10, 16]], 12, 20), 0.5)

    def test_boundaries_snap_to_the_phrase_line_or_the_downbeat(self):
        grid = {"beat_period": 0.5, "downbeat_offset": 0.25, "beat_residual_ms": 5, "bpm_confidence": 0.9}
        found = [{"start": 0.0, "end": 16.6, "label": "intro", "confidence": 0.7},
                 {"start": 16.6, "end": 21.1, "label": "verse", "confidence": 0.5},
                 {"start": 21.1, "end": 60.0, "label": "chorus", "confidence": 0.7}]
        snapped = lyric_sections.snap(found, grid)
        # 16.25 is a phrase line (16 s phrases from 0.25), within a bar.
        self.assertEqual(snapped[1]["start"], 16.25)
        self.assertEqual(snapped[0]["end"], 16.25)
        # 21.1 is nowhere near a phrase line; the nearest downbeat is 20.25.
        self.assertEqual(snapped[2]["start"], 20.25)
        # Without a grid, an acoustic boundary within two seconds wins.
        profile = {"boundaries": [{"at": 17.5, "confidence": 0.5}, {"at": 30, "confidence": 0.5}]}
        loose = lyric_sections.snap(found, None, profile)
        self.assertEqual(loose[1]["start"], 17.5)
        self.assertEqual(loose[2]["start"], 21.1)

    def test_exit_and_entry_scores(self):
        lines, end = song()
        found = lyric_sections.sections(lines, end + 20)
        chorus = [s for s in found if s["label"] == "chorus"]
        self.assertEqual(lyric_sections.exit_score(found, chorus[1]["start"] + 3), -1.0)
        self.assertEqual(lyric_sections.exit_score(found, chorus[-1]["end"]), 1.0)
        self.assertEqual(lyric_sections.exit_score([], 30), 0.0)
        verse = next(s for s in found if s["label"] == "verse")
        self.assertEqual(lyric_sections.entry_score(found, verse["start"]), 1.0)
        self.assertEqual(lyric_sections.entry_score(found, verse["start"] + 5), -0.5)
        self.assertEqual(lyric_sections.exit_points(found), [chorus[-1]["end"]])

    def test_overlay_replaces_vocal_evidence_only_where_there_is_a_profile(self):
        profile = {"bins": [{"at": i / 2, "end": (i + 1) / 2, "energy": 0.5, "bass": 0.5, "vocal": None}
                            for i in range(40)]}
        out = lyric_sections.overlay_vocals(profile, [[5, 8]])
        self.assertEqual(out["bins"][11]["vocal"], 0.9)
        self.assertEqual(out["bins"][2]["vocal"], 0.05)
        self.assertIsNone(profile["bins"][11]["vocal"], "the cached profile was changed")
        self.assertIs(lyric_sections.overlay_vocals(profile, []), profile)


class Planner(unittest.TestCase):
    """The same comparison as SmartCuePlanner, with and without lyric sections."""

    def setUp(self):
        self.settings = {
            "transitions.smart_cues": True, "transitions.mid_song_cues": False,
            "transitions.preset": "auto", "transitions.exit_search_seconds": 24,
            "transitions.max_intro_skip": 8, "transitions.minimum_play_fraction": 0.8,
            "transitions.phrase_beats": 0, "transitions.tempo_match": False,
            "transitions.overlap_scoring": False, "crossfade.detect_cold_end": False,
        }
        mocked = patch.object(config.station, "get", side_effect=lambda key, default=None:
                              self.settings.get(key, default))
        mocked.start()
        self.addCleanup(mocked.stop)

    def profile(self, duration=160, vocal=None, exits=(), entries=()):
        return {"version": structure.VERSION, "duration": duration, "step_sec": 0.5, "complete": True,
                "vocal_source": "unknown",
                "bins": [{"at": i / 2, "end": (i + 1) / 2, "energy": 0.7, "bass": 0.5, "vocal": vocal}
                         for i in range(duration * 2)],
                "boundaries": [], "entries": [{"at": t, "score": 1} for t in entries],
                "exits": [{"at": t, "score": 1} for t in exits]}

    def track(self, profile, sections=None, spans=None, **extra):
        result = {"key": "t", "title": "T", "artist": "A", "duration": 120, "bpm": 120,
                  "camelot": "8A", "intro_sec": 30, "structure": profile, **extra}
        if sections is not None:
            result["lyric_map"] = {"status": "synced", "sections": sections, "vocal_spans": spans or []}
        return result

    def refine(self, outgoing, incoming):
        return mixplanner.refine(outgoing, incoming, transitions.Plan(overlap=8),
                                 out_start=50, out_offset=0, out_duration=120, out_rate=1,
                                 in_offset=0, in_duration=120, in_rate=1)

    OUT_SECTIONS = [{"start": 0, "end": 10, "label": "intro", "confidence": .7},
                    {"start": 10, "end": 84, "label": "verse", "confidence": .5},
                    {"start": 84, "end": 112, "label": "chorus", "confidence": .8},
                    {"start": 112, "end": 120, "label": "outro", "confidence": .7}]

    def test_never_fades_out_in_the_middle_of_the_last_chorus(self):
        incoming = self.track(self.profile(vocal=0))
        plain = self.refine(self.track(self.profile(vocal=0, exits=(108,))), incoming)
        self.assertLess(plain.out_duration, 120)           # the acoustic cue wins alone
        lyrical = self.refine(self.track(self.profile(vocal=0, exits=(108,)), self.OUT_SECTIONS), incoming)
        self.assertGreaterEqual(lyrical.out_duration - lyrical.plan.overlap, 112 - 0.01)
        self.assertIn("lyric sections", lyrical.plan.reason)

    def test_zero_weight_ignores_the_sections(self):
        self.settings["transitions.section_weight"] = 0
        incoming = self.track(self.profile(vocal=0))
        choice = self.refine(self.track(self.profile(vocal=0, exits=(108,)), self.OUT_SECTIONS), incoming)
        self.assertLess(choice.out_duration, 120)

    def test_enters_where_the_singing_starts_once_lyrics_say_the_intro_is_instrumental(self):
        sections = [{"start": 0, "end": 6.5, "label": "intro", "confidence": .7},
                    {"start": 6.5, "end": 40, "label": "verse", "confidence": .5},
                    {"start": 40, "end": 160, "label": "chorus", "confidence": .8}]
        outgoing = self.track(self.profile(vocal=0))
        unknown = self.refine(outgoing, self.track(self.profile()))
        self.assertEqual(unknown.in_offset, 0)              # never skip an opening on a guess
        lyrical = self.refine(outgoing, self.track(self.profile(), sections, [[6.5, 150]]))
        self.assertEqual(lyrical.in_offset, 6.5)

    def test_lyric_vocals_stop_two_singers_stacking(self):
        spans = [[0, 160]]
        out = mixplanner.profile(self.track(self.profile(), [], spans))
        self.assertAlmostEqual(mixplanner._window(out, 100, 110, "vocal"), 0.9)
        self.assertEqual(mixplanner.profile(self.track(self.profile()))["bins"][0]["vocal"], None)

    def test_schedule_items_carry_a_small_section_map(self):
        schedule = timeline.Schedule()
        item = schedule.add_music("a", self.track(self.profile(), self.OUT_SECTIONS, [[10, 112]]))
        self.assertEqual(item.meta["lyrics"], "synced")
        self.assertEqual([s["label"] for s in item.meta["sections"]], ["intro", "verse", "chorus", "outro"])
        self.assertNotIn("confidence", item.meta["sections"][0])
        self.assertEqual(item.meta["vocal_spans"], [[10, 112]])
        plain = timeline.Schedule().add_music("b", self.track(self.profile()))
        self.assertNotIn("sections", plain.meta)


class TalkUp(unittest.TestCase):
    def test_the_post_is_the_first_sung_line_unless_you_set_one(self):
        base = {"intro_sec": 6.0, "lyric_map": {"first_line": 18.4}}
        self.assertEqual(director.talk_up_post(base, 12.0), 18.4)
        self.assertEqual(director.talk_up_post({**base, "intro_override": 9.0}, 12.0), 9.0)
        self.assertEqual(director.talk_up_post({"intro_sec": 6.0, "lyric_map": {"first_line": 0.5}}, 12.0), 6.0)
        self.assertEqual(director.talk_up_post({}, 12.0), 12.0)

    def test_a_break_over_a_long_intro_ends_just_before_the_first_line(self):
        post = director.talk_up_post({"intro_sec": 4.0, "lyric_map": {"first_line": 20.0}}, 12.0)
        start = timeline.break_start("over_intro", music_start=100.0, intro=post, safety=0.6,
                                     speech_length=9.0)
        self.assertAlmostEqual(start + 9.0, 100.0 + 20.0 - 0.6)
        self.assertEqual(timeline.choose_placement("over_intro", intro=post, safety=0.6, speech_length=9.0,
                                                   music_start=100.0, previous_end=104.0,
                                                   previous_start=0.0), "over_intro")


class HostQuotes(Database):
    def setUp(self):
        super().setUp()
        self.track = self.add_track()
        lines, _ = song()
        lyrics.save(self.track, {"status": "synced", "synced": lines, "plain": "", "lrclib_id": 1})
        self.context = {"next": {"key": self.track["key"], "title": "Song", "artist": "Band"}}

    class Rng:
        def __init__(self, values):
            self.values = list(values)

        def random(self):
            return self.values.pop(0)

        def choice(self, options):
            return options[0]

    def test_off_by_default_chance_and_once_per_song(self):
        self.settings["hosts.lyric_quote_chance"] = 0
        self.assertIsNone(personal.lyric_quote(self.context, self.Rng([0.0, 0.0])))
        self.settings["hosts.lyric_quote_chance"] = 0.1
        self.assertIsNone(personal.lyric_quote(self.context, self.Rng([0.5])))
        quote = personal.lyric_quote(self.context, self.Rng([0.01, 0.1]))
        self.assertIn(quote["line"], CHORUS)             # chorus lines come first
        self.assertLessEqual(len(quote["line"].split()), 10)
        self.assertIsNone(personal.lyric_quote(self.context, self.Rng([0.0, 0.0])))

    def test_any_more_than_the_one_line_is_a_leak(self):
        allowed = {"line": CHORUS[0]}
        credit = [Line("mav", f'As Band put it, "{CHORUS[0]}". Sure.')]
        self.assertFalse(personal.lyric_leak(credit, self.context, allowed))
        extended = [Line("mav", f"{CHORUS[0]}, {CHORUS[1]}")]
        self.assertTrue(personal.lyric_leak(extended, self.context, allowed))
        twice = [Line("mav", CHORUS[0]), Line("rue", CHORUS[0])]
        self.assertTrue(personal.lyric_leak(twice, self.context, allowed))
        self.assertTrue(personal.lyric_leak([Line("rue", VERSE_2[0])], self.context, None))
        self.assertFalse(personal.lyric_leak([Line("rue", "hold on, it's Band.")], self.context, None))


if __name__ == "__main__":
    unittest.main()
