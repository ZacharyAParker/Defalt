"""Edition preference applies to source fallback and automatic rotation."""
import unittest
import tempfile
from pathlib import Path
from unittest.mock import patch

from radio import config, library, mixconfig, versions


def entry(title, **extra):
    return {"id": "abcdefghijk", "title": title, "duration": 200, **extra}


class EditionTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        p = patch.object(config, "CACHE_DIR", self.root)
        p.start()
        self.addCleanup(p.stop)
        self.settings = {}
        p = patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d))
        p.start()
        self.addCleanup(p.stop)
        p = patch.object(library, "_source_info", return_value={})
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
        self.assertIsNone(self.score("Artist - Song (Radio Edit)"))
        self.assertIsNotNone(self.score("Artist - Song (Radio Edit)", song="Song (Radio Edit)"))

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

    def test_unlabelled_audio_is_compared_with_explicit_search(self):
        with patch.object(library.sourceio, "search") as search:
            search.return_value = {"entries": [entry("Artist - Song")]}
            self.assertEqual(library.resolve("Artist", "Song"), "abcdefghijk")
            self.assertEqual(search.call_count, 2)

    def test_setting_available_and_preserved_by_mix_profiles(self):
        key = "selection.avoid_clean_versions"
        self.assertEqual(mixconfig.validate({key: False}), {key: False})
        self.assertTrue(next(f["value"] for f in mixconfig.snapshot()["fields"] if f["key"] == key))
        self.assertTrue(all(key not in profile for profile in mixconfig.PROFILES.values()))

    def test_hidden_clean_metadata_is_rejected(self):
        for extra in ({"album": "Record (Clean)"}, {"track": "Song (Censored)"},
                      {"description": "Provided to YouTube by Label\nClean Version"},
                      {"description": "Album: Record (Clean)"}):
            with self.subTest(extra=extra):
                self.assertIsNone(self.score("Artist - Song", **extra))
        self.assertIsNotNone(self.score("Artist - Song", album="Clean"))
        self.assertIsNotNone(self.score("Artist - Song", description="Listen to the clean version here: example.com"))

    def test_explicit_result_beats_unlabelled_topic_and_wrong_song(self):
        plain = entry("Artist - Song", channel="Artist - Topic")
        explicit = entry("Artist - Song (Explicit)", id="12345678901")
        wrong = entry("Artist - Other Song (Explicit)", id="12345678902")
        with patch.object(library.sourceio, "search", side_effect=[{"entries": [plain]}, {"entries": [wrong, explicit]}]):
            self.assertEqual(library.resolve("Artist", "Song"), explicit["id"])
        self.assertTrue(library._edition_checked(explicit["id"], "Artist", "Song"))

    def test_details_can_reject_unlabelled_clean_album(self):
        plain = entry("Artist - Song", channel="Artist - Topic")
        explicit = entry("Artist - Song (Explicit)", id="12345678901")
        with patch.object(library.sourceio, "search", return_value={"entries": [plain, explicit]}), \
             patch.object(library, "_source_info", side_effect=lambda vid: {"album": "Record (Clean)"} if vid == plain["id"] else {}):
            self.assertEqual(library.resolve("Artist", "Song"), explicit["id"])

    def test_clean_request_does_not_search_for_explicit(self):
        with patch.object(library.sourceio, "search", return_value={"entries": [entry("Artist - Song (Clean)")]} ) as search:
            self.assertEqual(library.resolve("Artist", "Song (Clean)"), "abcdefghijk")
            self.assertEqual(search.call_count, 1)

    def cached(self):
        old = self.root / "old.opus"
        old.write_bytes(b"working audio")
        return dict(key="song", artist="Artist", title="Song", video_id="abcdefghijk",
                    file=str(old), source="seed", bpm=100)

    def test_clean_cache_rechecked_when_other_preferences_disabled(self):
        self.settings.update({"selection.prefer_original_recording": False, "selection.avoid_music_videos": False})
        track = self.cached()
        with patch.object(library.db, "one", return_value=track), patch.object(library, "AUDIO_DIR", self.root), \
             patch.object(library, "_source_info", return_value={"title": "Song", "album": "Record (Clean)"}), \
             patch.object(library, "fetch_recording", side_effect=RuntimeError("replacement reached")) as fetch:
            with self.assertRaisesRegex(RuntimeError, "replacement reached"):
                library._ensure_locked(track)
            self.assertIsNone(fetch.call_args.kwargs["video_id"])
            self.assertTrue(Path(track["file"]).exists())

    def test_legacy_cache_search_failure_keeps_audio_and_cools_down(self):
        from radio import importer
        track = self.cached()
        with patch.object(library.db, "one", return_value=track), patch.object(library, "AUDIO_DIR", self.root), \
             patch.object(library, "resolve", side_effect=library.sourceio.SourceError("offline")) as resolve, \
             patch.object(importer, "refresh_tags", side_effect=lambda row, path: row):
            self.assertEqual(library._ensure_locked(track)["file"], track["file"])
            self.assertEqual(library._ensure_locked(track)["file"], track["file"])
            self.assertEqual(resolve.call_count, 1)
            self.assertTrue(Path(track["file"]).exists())

    def test_legacy_cache_replacement_download_failure_keeps_audio(self):
        track = self.cached()
        with patch.object(library.db, "one", return_value=track), patch.object(library, "AUDIO_DIR", self.root), \
             patch.object(library, "resolve", return_value="12345678901"), \
             patch.object(library, "fetch_recording", side_effect=library.sourceio.SourceError("offline")):
            self.assertEqual(library._ensure_locked(track)["file"], track["file"])
            self.assertTrue(Path(track["file"]).exists())

    def test_failed_probe_cooldown_expires(self):
        with patch.object(library.time, "time", return_value=1000):
            library._record_edition_check("abcdefghijk", "Artist", "Song", success=False)
        with patch.object(library.time, "time", return_value=1100):
            self.assertTrue(library._edition_checked("abcdefghijk", "Artist", "Song"))
        with patch.object(library.time, "time", return_value=4700):
            self.assertFalse(library._edition_checked("abcdefghijk", "Artist", "Song"))

    def test_rejecting_hidden_clean_results_still_reaches_later_audio(self):
        candidates = [entry("Artist - Song", id=f"source{i:05d}", channel="Artist - Topic") for i in range(5)]
        with patch.object(library.sourceio, "search", return_value={"entries": candidates}), \
             patch.object(library, "_source_info", side_effect=lambda vid: {"album": "Record (Clean)"} if vid != candidates[-1]["id"] else {}):
            self.assertEqual(library.resolve("Artist", "Song"), candidates[-1]["id"])
