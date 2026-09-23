"""Verified references must stay attached to the right music and survive airing."""
import copy
import random
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import config, db, director, memes, mixconfig, timeline
from radio.segments import base, personal


class MusicMemes(unittest.TestCase):
    def setUp(self):
        lookup = patch("radio.song_context.prepare", return_value=None)
        lookup.start()
        self.addCleanup(lookup.stop)
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        local = threading.local()
        self.settings = {"hosts.meme_chance_percent": 100}
        self.now = 1000000.0
        for mock in (patch.object(db, "_DB_PATH", Path(temp.name) / "station.db"),
                     patch.object(db, "_LOCAL", local),
                     patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d)),
                     patch.object(memes.time, "time", side_effect=lambda: self.now)):
            mock.start()
            self.addCleanup(mock.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        db.connect()

    def data(self, artist="Kendrick Lamar", title="tv off", slot="incoming"):
        return {slot: {"artist": artist, "title": title}}

    def test_entire_reviewed_catalog_is_valid_and_has_unique_ids(self):
        refs = memes.references()
        self.assertEqual(len(refs), 11)
        self.assertEqual(len({r["id"] for r in refs}), 11)
        for ref in refs:
            self.assertTrue(ref["source"].startswith("https://"))

    def test_song_matching_normalizes_case_punctuation_and_credits(self):
        ref = memes.prepare(self.data("KENDRICK LAMAR feat. Lefty Gunplay", "TV OFF!"))
        self.assertEqual(ref["id"], "kendrick_mustard")
        self.assertEqual(ref["matched_slot"], "incoming")

    def test_title_alone_artist_substring_and_another_song_do_not_match(self):
        for artist, title in (("A Cover Band", "tv off"), ("Kendrick Lamar Tribute", "tv off"),
                              ("Kendrick Lamar", "Not Like Us"), ("Kendrick Lamar", "tv off live")):
            self.assertIsNone(memes.prepare(self.data(artist, title)))
        self.assertEqual(db.query("SELECT * FROM seen"), [])

    def test_artist_callback_keeps_origin_and_matched_record_separate(self):
        ref = memes.prepare(self.data("Tyler, The Creator", "See You Again", "outgoing"))
        self.assertEqual(ref["scope"], "artist")
        self.assertEqual(ref["matched_slot"], "outgoing")
        self.assertIn("interview", ref["context"])
        self.assertEqual(memes.provenance(ref)["matched_title"], "See You Again")

    def test_disabled_or_zero_chance_does_not_reserve(self):
        for settings in ({"hosts.meme_references": False}, {"hosts.meme_chance_percent": 0}):
            self.settings.update(settings)
            self.assertIsNone(memes.prepare(self.data()))
            self.settings.clear()
        self.assertEqual(db.query("SELECT * FROM seen"), [])

    def test_chance_is_applied_at_boundary(self):
        self.settings["hosts.meme_chance_percent"] = 30
        with patch.object(memes.random, "random", return_value=.30):
            self.assertIsNone(memes.prepare(self.data()))
        with patch.object(memes.random, "random", return_value=.299):
            self.assertIsNotNone(memes.prepare(self.data()))

    def test_quote_toggle_selects_original_unquoted_opening(self):
        self.settings["hosts.meme_quotes"] = False
        ref = memes.prepare(self.data())
        self.assertEqual(ref["opening"], ref["spoken"])
        self.assertNotIn(ref["quote"], ref["opening"])

    def test_global_gap_and_persistent_reference_cooldown(self):
        self.assertIsNotNone(memes.prepare(self.data()))
        self.now += 599
        self.assertIsNone(memes.prepare(self.data("Weezer", "Buddy Holly")))
        self.now += 1
        self.assertIsNotNone(memes.prepare(self.data("Weezer", "Buddy Holly")))
        # New connection simulates a backend restart, with the same database.
        db._LOCAL.conn.close()
        del db._LOCAL.conn
        self.now += 600
        self.assertIsNone(memes.prepare(self.data()))
        self.now = 1000000 + 48 * 3600
        self.assertIsNotNone(memes.prepare(self.data()))

    def test_only_one_reference_is_prepared_for_a_pair(self):
        data = {**self.data(), **self.data("Weezer", "Buddy Holly", "outgoing")}
        self.assertIsNotNone(memes.prepare(data))
        self.assertEqual(len(db.query("SELECT * FROM seen WHERE ident != '__last__'")), 1)
        self.assertIsNone(memes.prepare(data))

    def test_recent_identical_opening_is_not_prepared(self):
        ref = next(r for r in memes.references() if r["id"] == "kendrick_mustard")
        self.assertIsNone(memes.prepare(self.data(), [ref["spoken_quote"]]))

    def test_invalid_catalog_entries_are_ignored(self):
        valid = memes.references()[0]
        for changes in ({"source": "http://unreviewed"}, {"reviewed": "yesterday"},
                        {"artists": "Rick Astley"}, {"titles": []}, {"scope": "all"},
                        {"quote": "word " * 11}, {"spoken": "word " * 46},
                        {"spoken_quote": []}, {"id": "__last__"}, {"reviewed": None}):
            bad = {**copy.deepcopy(valid), **changes}
            with patch.object(memes._catalog, "get", return_value=[bad]):
                self.assertEqual(memes.references(), [])
        with patch.object(memes._catalog, "get", return_value=[None, valid, valid]):
            self.assertEqual(len(memes.references()), 1)
        with patch.object(memes._catalog, "get", return_value="broken"):
            self.assertEqual(memes.references(), [])

    def test_intro_has_grounded_opening_and_names_incoming_without_model_call(self):
        context = {"previous": self.data()["incoming"],
                   "next": {"title": "A Different Song", "artist": "Other Artist"}}
        with patch.object(personal, "write") as write:
            lines = personal.comment(context, "mav", "rue", introduce=True)
        write.assert_not_called()
        self.assertTrue(lines[0].text.startswith("Mustard!"))
        self.assertEqual(lines[0].reference["matched_slot"], "outgoing")
        self.assertEqual(lines[1].text, "A Different Song, by Other Artist.")
        self.assertEqual([line.host for line in lines], ["rue", "mav"])

    def test_model_failure_uses_sourced_two_host_exchange(self):
        with patch.object(base.llm, "complete_json", return_value=None):
            lines = personal.comment({"next": self.data()["incoming"]}, "mav", "rue")
        self.assertEqual(len(lines), 2)
        self.assertEqual([line.host for line in lines], ["rue", "mav"])
        self.assertIn("knowyourmeme.com", lines[0].reference["source"])
        self.assertNotIn("https://", lines[0].text)

    def test_model_cannot_replace_verified_opening_or_repeat_approved_quote(self):
        with patch.object(personal, "write", return_value=[base.Line("rue", "Made up quote"),
                                                          base.Line("mav", "Mustard! Mustard!")]) as write:
            lines = personal.comment({"next": self.data()["incoming"]}, "mav", "rue")
        self.assertNotIn("Made up", lines[0].text)
        self.assertNotIn("Mustard!", lines[1].text)
        self.assertIn("VERIFIED MEME", write.call_args.args[0])
        self.assertIn("never pretend they originated in this track", write.call_args.args[0])

    def test_unmatched_tracks_prompt_original_jokes_without_source_metadata(self):
        with patch.object(personal, "write", side_effect=lambda brief, **kw: kw["fallback"]) as write:
            lines = personal.comment({"next": {"title": "Unknown", "artist": "Unknown"}}, "mav", "rue")
        self.assertIn("No verified meme was selected", write.call_args.args[0])
        self.assertTrue(all(line.reference is None for line in lines))

    def test_source_survives_render_placement_and_transcript_history(self):
        ref = memes.provenance(memes.prepare(self.data()))
        station = director.Station.__new__(director.Station)
        station.lock = threading.RLock()
        station.clock = director.Clock()
        station.schedule = timeline.Schedule()
        station.rng = random.Random(0)
        with patch.object(director.tts, "say", return_value={"path": "meme.wav", "duration": 4}):
            rendered = station._render([base.Line("rue", "A sourced joke", ref)])
        track = {"key": "test", "artist": "Test", "title": "Test", "duration": 120, "file": "test.wav"}
        with patch.object(director.db, "intro_of", return_value=12),                 patch.object(director, "audio_present", return_value=True):
            station._place(track, rendered, "dry")
        voice = next(i for i in station.schedule.items if i.kind == "voice")
        station.clock.jump(voice.start_at + 1)
        self.assertEqual(station.transcript()[0]["reference"], ref)
        station.clock.jump(300)
        station.schedule.trim_before(station.clock.now())
        self.assertEqual(station.transcript()[0]["reference"], ref)

    def test_settings_schema_validates_controls(self):
        values = {"hosts.meme_references": True, "hosts.meme_quotes": False,
                  "hosts.meme_chance_percent": 100, "hosts.meme_gap_minutes": 0, "hosts.meme_repeat_hours": 1}
        self.assertEqual(mixconfig.validate(values), values)
        for values in ({"hosts.meme_chance_percent": 101}, {"hosts.meme_repeat_hours": 0},
                       {"hosts.meme_gap_minutes": -1}, {"hosts.meme_quotes": "yes"}):
            with self.assertRaises(ValueError):
                mixconfig.validate(values)


if __name__ == "__main__":
    unittest.main()
