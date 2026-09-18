"""Edition preference applies to source fallback and automatic rotation."""
import unittest
from unittest.mock import patch

from radio import config, library, mixconfig, versions


def entry(title, **extra):
    return {"id": "abcdefghijk", "title": title, "duration": 200, **extra}


class EditionTests(unittest.TestCase):
    def setUp(self):
        self.settings = {}
        p = patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d))
        p.start()
        self.addCleanup(p.stop)

    def score(self, title, artist="Artist", song="Song", **extra):
        return library._candidate_score(entry(title, **extra), artist, song, 0)

    def test_censored_editions_rejected_even_on_official_audio(self):
        for label in ("Clean", "Clean Version", "Clean Edit", "Censored", "Non-Explicit", "Radio Friendly"):
            with self.subTest(label=label):
                self.assertIsNone(self.score(f"Artist - Song ({label})", channel="Artist - Topic"))
                self.assertTrue(versions.clean_track({"title": f"Song ({label})"}))
                self.assertTrue(versions.clean_track({"album": f"Album ({label})"}))

    def test_identity_words_are_not_edition_labels(self):
        for artist, title in (("Taylor Swift", "Clean"), ("Clean Bandit", "Rather Be"),
                              ("Artist", "So Fresh, So Clean"), ("Artist", "Censored"),
                              ("Artist", "Explicit")):
            with self.subTest(artist=artist, title=title):
                self.assertIsNotNone(self.score(f"{artist} - {title} (Official Audio)", artist, title))
                self.assertIsNotNone(self.score(f"{artist} - {title}", artist, title))
                self.assertIsNotNone(self.score(title, artist, title))
                self.assertFalse(versions.clean_track({"artist": artist, "title": title}))
        self.assertIsNone(self.score("Taylor Swift - Clean (Clean Version)", "Taylor Swift", "Clean"))

    def test_explicit_preference_preserves_source_quality(self):
        plain = self.score("Artist - Song (Official Audio)")
        self.assertGreater(self.score("Artist - Song (Explicit) (Official Audio)"), plain)
        self.assertGreater(self.score("Artist - Song (Uncensored) (Official Audio)"), plain)
        self.assertGreater(self.score("Artist - Song", channel="Artist - Topic"),
                           self.score("Artist - Song (Explicit Lyrics)"))
        self.assertIsNotNone(self.score("Artist - Song (Radio Edit)"))  # Shortened is not necessarily censored.

    def test_description_links_do_not_classify_the_wrong_edition(self):
        self.assertEqual(self.score("Artist - Song", description="Get the clean version at example.com"),
                         self.score("Artist - Song"))

    def test_explicit_clean_request_and_disabled_preference(self):
        self.assertIsNotNone(self.score("Artist - Song (Clean)", song="Song (Clean)"))
        self.assertIsNone(self.score("Artist - Song (Explicit)", song="Song (Clean)"))
        self.settings["selection.avoid_clean_versions"] = False
        self.assertIsNotNone(self.score("Artist - Song (Clean)"))

    def test_resolver_retries_explicit_search_before_video_fallback(self):
        with patch.object(library.sourceio, "search") as search:
            search.side_effect = [
                {"entries": [entry("Artist - Song (Clean)"), entry("Artist - Song (Official Video)")]},
                {"entries": [entry("Artist - Song (Explicit)", id="12345678901")]},
            ]
            self.assertEqual(library.resolve("Artist", "Song"), "12345678901")
            self.assertIn("explicit audio", search.call_args.args[0])

    def test_clean_video_cannot_leak_through_last_resort(self):
        with patch.object(library.sourceio, "search") as search:
            search.return_value = {"entries": [entry("Artist - Song (Clean) (Official Video)")]}
            self.assertIsNone(library.resolve("Artist", "Song"))
            self.assertEqual(search.call_count, 2)

    def test_normal_audio_does_not_need_extra_search(self):
        with patch.object(library.sourceio, "search") as search:
            search.return_value = {"entries": [entry("Artist - Song")]}
            self.assertEqual(library.resolve("Artist", "Song"), "abcdefghijk")
            self.assertEqual(search.call_count, 1)

    def test_setting_available_and_preserved_by_mix_profiles(self):
        key = "selection.avoid_clean_versions"
        self.assertEqual(mixconfig.validate({key: False}), {key: False})
        self.assertTrue(next(f["value"] for f in mixconfig.snapshot()["fields"] if f["key"] == key))
        self.assertTrue(all(key not in profile for profile in mixconfig.PROFILES.values()))
