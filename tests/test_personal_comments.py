"""Personal host bits use actual records, with no invented repeat history."""
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import config, db, mixconfig
from radio.segments import base, personal, writers


class PersonalComments(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        local = threading.local()
        self.settings = {"hosts.personal_comments": True, "hosts.roast_level": "sharp",
                         "hosts.song_comment_chance": 1.0}
        for p in (patch.object(db, "_DB_PATH", Path(temporary.name) / "test.db"),
                  patch.object(db, "_LOCAL", local),
                  patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d))):
            p.start()
            self.addCleanup(p.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        db.connect()

    def track(self, key="a", title="Repeat Offender", artist="The Alibis", plays=0):
        db.write("INSERT INTO tracks(key,title,artist,play_count,added_at) VALUES(?,?,?,?,0)",
                 (key, title, artist, plays))
        return {"key": key, "title": title, "artist": artist}

    def event(self, key, kind):
        db.write("INSERT INTO events(ts,track_key,kind) VALUES(0,?,?)", (key, kind))

    def test_evidence_reads_fresh_counts_and_separates_requests_from_plays(self):
        track = self.track(plays=4)
        for kind in ("request", "request", "skipped_early", "thumbs_up"):
            self.event("a", kind)
        result = personal.facts(dict(track, play_count=99))
        self.assertEqual(result["recorded_plays"], 4)
        self.assertEqual(result["requests"], 2)
        self.assertEqual(result["early_skips"], 1)
        self.assertEqual(result["thumbs_up"], 1)
        self.assertEqual(result["late_skips"], 0)

    def test_artist_history_only_contains_other_actually_played_titles(self):
        track = self.track(plays=8)
        self.track("b", "Old Favourite", "the alibis", plays=3)
        self.track("c", "Never Played", plays=0)
        self.track("d", "Different Artist", "Someone Else", plays=30)
        result = personal.facts(track)["other_frequently_played_titles_by_artist"]
        self.assertEqual(result, [{"title": "Old Favourite", "play_count": 3}])

    def test_unknown_track_does_not_inherit_unverified_repeat_history(self):
        track = {"key": "missing", "title": "New Arrival", "artist": "New Artist", "play_count": 99}
        data = personal.evidence({"next": track})
        self.assertIsNone(data["incoming"]["recorded_plays"])
        for _ in range(10):
            line = personal.fallback(data, "mav", "rue", [], False)[0].text
            self.assertNotIn("again", line)
            self.assertNotIn("Another round", line)

    def test_request_fallback_names_song_and_avoids_recent_punchline(self):
        track = self.track()
        data = personal.evidence({"next": track, "was_request": True})
        first = personal.fallback(data, "mav", "rue", [], True)
        second = personal.fallback(data, "mav", "rue", [first[0].text], True)
        self.assertNotEqual(first[0].text, second[0].text)
        self.assertIn(track["title"], second[-1].text)
        self.assertIn(track["artist"], second[-1].text)
        self.assertEqual(second[-1].host, "mav")

    def test_model_prompt_gets_pair_evidence_recent_lines_and_selected_intensity(self):
        outgoing = self.track(plays=6)
        incoming = self.track("b", "Fresh Trouble", plays=0)
        self.settings["hosts.roast_level"] = "savage"
        with patch.object(personal, "write", side_effect=lambda brief, **kw: kw["fallback"]) as write:
            personal.comment({"previous": outgoing, "next": incoming, "was_request": True,
                              "recent_host_lines": ["Retired punchline"], "speech_budget": 9}, "mav", "rue")
        brief = write.call_args.args[0]
        for text in ("Repeat Offender", "Fresh Trouble", '"recorded_plays": 6',
                     '"recorded_plays": 0', "Retired punchline", "Go hard", "9 seconds",
                     "Requests are not plays", "No fake music trivia"):
            self.assertIn(text, brief)

    def test_model_failure_still_delivers_contextual_two_host_exchange(self):
        track = self.track(plays=5)
        with patch.object(base.llm, "complete_json", return_value=None):
            lines = writers.track_intro({"next": track})
        self.assertEqual({line.host for line in lines}, {"mav", "rue"})
        self.assertIn(track["title"], lines[-1].text)
        self.assertTrue(any(word in lines[0].text for word in ("again", "Another round")))

    def test_song_comment_frequency_and_disable_controls_route_writers(self):
        context = {"next": self.track()}
        with patch.object(personal, "comment", return_value=[base.Line("mav", "Personal")]) as comment, \
                patch.object(writers, "write", return_value=[base.Line("mav", "Generic")]), \
                patch.object(writers.taste, "summary", return_value={"top_artists": []}):
            self.assertEqual(writers.banter(context)[0].text, "Personal")
            self.settings["hosts.song_comment_chance"] = 0
            self.assertEqual(writers.banter(context)[0].text, "Generic")
            self.settings["hosts.personal_comments"] = False
            self.assertEqual(writers.track_intro(context)[0].text, "Generic")
            self.assertEqual(comment.call_count, 1)

    def test_new_settings_validate_and_invalid_intensity_is_rejected(self):
        values = {"hosts.roast_level": "savage", "hosts.song_comment_chance": .9,
                  "transitions.mid_song_cues": True, "transitions.minimum_play_fraction": .65,
                  "transitions.max_entry_skip_fraction": .25}
        self.assertEqual(mixconfig.validate(values), values)
        for invalid in ({"hosts.roast_level": "nonsense"}, {"transitions.minimum_play_fraction": .5},
                        {"transitions.max_entry_skip_fraction": .6}, {"hosts.song_comment_chance": 1.1}):
            with self.assertRaises(ValueError):
                mixconfig.validate(invalid)


if __name__ == "__main__":
    unittest.main()
