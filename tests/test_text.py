"""Name matching and model-output cleanup.

Small free models narrate stage directions, label speakers, and wrap output in
markdown. Everything here is the damage control that stops that reaching TTS.
"""
import unittest

from radio import db
from radio.segments import base


class TestArtistNames(unittest.TestCase):
    def test_collaborations_credit_the_lead_artist(self):
        self.assertEqual(db.primary_artist("Kendrick Lamar, SZA"), "Kendrick Lamar")
        self.assertEqual(db.primary_artist("Baby Keem, Kendrick Lamar"), "Baby Keem")
        self.assertEqual(db.primary_artist("Logic feat. Eminem"), "Logic")
        self.assertEqual(db.primary_artist("Shawn Wasabi & YDG"), "Shawn Wasabi")

    def test_commas_inside_a_real_name_are_not_a_split(self):
        """The bug that turned Tyler, The Creator into 'tyler'."""
        self.assertEqual(db.primary_artist("Tyler, The Creator"), "Tyler, The Creator")
        self.assertEqual(db.primary_artist("Grover Washington, Jr."),
                         "Grover Washington, Jr.")

    def test_bands_whose_names_contain_separators_survive(self):
        self.assertEqual(db.primary_artist("Earth, Wind & Fire"), "Earth, Wind & Fire")
        self.assertEqual(db.primary_artist("Crosby, Stills & Nash"),
                         "Crosby, Stills & Nash")

    def test_plain_names_are_untouched(self):
        for name in ("Weezer", "Run-D.M.C.", "bbno$", "BENEE", "Fred again.."):
            self.assertEqual(db.primary_artist(name), name)


class TestTrackKeys(unittest.TestCase):
    def test_the_same_song_hashes_the_same_despite_decoration(self):
        base_key = db.track_key("Radiohead", "Creep")
        self.assertEqual(db.track_key("radiohead", "Creep!"), base_key)
        self.assertEqual(db.track_key("Radiohead", "Creep (Remastered)"), base_key)
        self.assertEqual(db.track_key("Radiohead", "Creep - 2011 Remaster"), base_key)

    def test_different_songs_do_not_collide(self):
        self.assertNotEqual(db.track_key("Weezer", "Buddy Holly"),
                            db.track_key("Weezer", "Undone"))


class TestCleanup(unittest.TestCase):
    def test_speaker_labels_are_stripped(self):
        self.assertEqual(base.clean("MAV: that was Good Kid"), "that was Good Kid")
        self.assertEqual(base.clean("**Rue:** okay but listen"), "okay but listen")

    def test_stage_directions_are_removed(self):
        self.assertEqual(base.clean("(laughs) that's the one"), "that's the one")
        self.assertEqual(base.clean("*sighs* fine"), "fine")
        self.assertEqual(base.clean("[static] we're back"), "we're back")

    def test_markdown_and_emoji_do_not_reach_the_voice(self):
        self.assertEqual(base.clean("**huge** track"), "huge track")
        self.assertNotIn("\U0001F525", base.clean("this one goes hard \U0001F525"))

    def test_ordinary_speech_survives_untouched(self):
        line = "Kendrick Lamar, tv off. The song is called tv off."
        self.assertEqual(base.clean(line), line)


class TestParsing(unittest.TestCase):
    def setUp(self):
        self.hosts = list(base.config.personas())
        if len(self.hosts) < 2:
            self.skipTest("needs two personas configured")

    def test_a_clean_array_parses(self):
        lines = base.parse([
            {"host": self.hosts[0], "text": "One."},
            {"host": self.hosts[1], "text": "Two."},
        ])
        self.assertEqual([l.host for l in lines], self.hosts[:2])

    def test_an_array_wrapped_in_an_object_is_unwrapped(self):
        """Models constantly return {"lines": [...]} instead of the array."""
        lines = base.parse({"lines": [{"host": self.hosts[0], "text": "Hello."}]})
        self.assertEqual(len(lines), 1)

    def test_display_names_are_mapped_back_to_ids(self):
        name = base.config.personas()[self.hosts[0]].get("name")
        lines = base.parse([{"host": name, "text": "Hello."}])
        self.assertEqual(lines[0].host, self.hosts[0])

    def test_unknown_hosts_are_dropped(self):
        lines = base.parse([{"host": "narrator", "text": "Meanwhile..."}])
        self.assertEqual(lines, [])

    def test_a_single_voice_monologue_is_split_between_hosts(self):
        payload = [{"host": self.hosts[0], "text": f"Line {i}."} for i in range(4)]
        lines = base.parse(payload)
        self.assertGreater(len({l.host for l in lines}), 1)

    def test_overlong_lines_are_trimmed(self):
        payload = [{"host": self.hosts[0], "text": "word " * 200}]
        lines = base.parse(payload)
        self.assertLessEqual(len(lines[0].text.split()), base.MAX_WORDS_PER_LINE)

    def test_junk_returns_nothing_rather_than_raising(self):
        for junk in (None, "not json", 12, [], [{}], [{"host": "x"}]):
            self.assertEqual(base.parse(junk), [])


if __name__ == "__main__":
    unittest.main()
