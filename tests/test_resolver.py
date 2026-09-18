"""Picking the right upload.

A search for a song returns the record, the music video, three live takes, a
karaoke backing and a lyric video that has been pitched up to dodge a content
match. Getting this wrong is not subtle -- Weezer's Buddy Holly video runs
4:02 against a 2:40 record, because the first eighty seconds are a sitcom.

No network: these classify fabricated search results, shaped exactly like the
ones yt-dlp returns.
"""
import unittest

from radio import library


def result(title, description="", channel="", duration=200, **extra):
    return {"title": title, "description": description, "channel": channel,
            "duration": duration, "id": "abcdefghijk", **extra}


def kind(title, artist, song, **extra):
    return library.classify(result(title, **extra), artist, song)


def score(title, artist, song, allow_video=False, expected_ms=0, **extra):
    return library._candidate_score(result(title, **extra), artist, song,
                                    expected_ms, allow_video=allow_video)


class TestLive(unittest.TestCase):
    def test_live_takes_are_refused(self):
        for title in (
            "Nirvana - Smells Like Teen Spirit (Live at Reading 1992)",
            "The Cranberries - Zombie 1999 Live Video",
            "Tower of Power - Soul Vaccination - In Concert",
            "Weezer - Buddy Holly (AOL Sessions)",
            "Radiohead - Creep [Live]",
            "Arctic Monkeys - 505 (Live at Glastonbury Festival)",
            "Some Band - Some Song (Unplugged)",
        ):
            self.assertIsNone(score(title, "Some Band", "Some Song"),
                              f"should refuse: {title}")

    def test_a_live_description_is_caught_even_with_a_clean_title(self):
        self.assertIsNone(score("Some Band - Some Song", "Some Band", "Some Song",
                                description="Recorded live at the Apollo, 1974."))

    def test_a_livestream_is_refused(self):
        self.assertIsNone(score("Some Band - Some Song", "Some Band", "Some Song",
                                live_status="was_live"))

    def test_songs_whose_titles_contain_live_still_resolve(self):
        """The bug that rejected every candidate for AC/DC's Live Wire."""
        self.assertIsNotNone(score("AC/DC - Live Wire (Official Audio)",
                                   "AC/DC", "Live Wire"))
        self.assertIsNotNone(score("Oasis - Live Forever (Remastered)",
                                   "Oasis", "Live Forever"))

    def test_but_a_genuine_live_take_of_such_a_song_is_still_refused(self):
        self.assertIsNone(score("AC/DC - Live Wire (Live at Donington)",
                                "AC/DC", "Live Wire"))


class TestMusicVideos(unittest.TestCase):
    def test_music_videos_are_refused_on_the_first_pass(self):
        for title in (
            "Weezer - Buddy Holly (Official Music Video)",
            "The Cranberries - Zombie - Official Music Video",
            "Some Band - Some Song (Official Video)",
            "Some Band - Some Song (Visualizer)",
        ):
            self.assertIsNone(score(title, "Some Band", "Some Song"),
                              f"should refuse: {title}")

    def test_a_music_video_description_is_caught_too(self):
        self.assertIsNone(score(
            "Weezer - Buddy Holly", "Weezer", "Buddy Holly",
            description="Music video by Weezer performing Buddy Holly."))

    def test_a_video_is_allowed_when_nothing_else_exists(self):
        """Better a video than losing the song entirely."""
        allowed = score("Some Band - Some Song (Official Music Video)",
                        "Some Band", "Some Song", allow_video=True)
        self.assertIsNotNone(allowed)

    def test_but_a_video_still_loses_to_clean_audio(self):
        video = score("Some Band - Some Song (Official Music Video)",
                      "Some Band", "Some Song", allow_video=True)
        clean = score("Some Band - Some Song", "Some Band", "Some Song",
                      allow_video=True)
        self.assertLess(video, clean)


class TestPreference(unittest.TestCase):
    def test_an_art_track_wins(self):
        """"Provided to YouTube by" marks the actual release upload."""
        art = score("Zombie", "The Cranberries", "Zombie",
                    description="Provided to YouTube by Universal Music Group")
        plain = score("The Cranberries - Zombie", "The Cranberries", "Zombie")
        self.assertGreater(art, plain)

    def test_a_topic_channel_also_counts_as_an_art_track(self):
        self.assertTrue(kind("Zombie", "The Cranberries", "Zombie",
                             channel="The Cranberries - Topic")["art_track"])

    def test_lyric_videos_are_demoted_not_refused(self):
        lyrics = score("Nirvana - Smells Like Teen Spirit (Lyrics)",
                       "Nirvana", "Smells Like Teen Spirit")
        plain = score("Nirvana - Smells Like Teen Spirit",
                      "Nirvana", "Smells Like Teen Spirit")
        self.assertIsNotNone(lyrics)
        self.assertLess(lyrics, plain)

    def test_a_different_take_loses_to_the_plain_record(self):
        """An art-track acoustic must not outrank the actual single."""
        acoustic = score("Creep (Acoustic)", "Radiohead", "Creep",
                         description="Provided to YouTube by XL Recordings")
        plain = score("Radiohead - Creep", "Radiohead", "Creep")
        self.assertLess(acoustic, plain)

    def test_asking_for_a_variant_gets_you_the_variant(self):
        self.assertFalse(kind("Creep (Acoustic)", "Radiohead",
                              "Creep - Acoustic")["variant"])
        self.assertFalse(kind("overtonight - mirrors demo", "overtonight",
                              "mirrors demo")["variant"])


class TestTampering(unittest.TestCase):
    def test_ai_remasters_are_refused(self):
        self.assertIsNone(score(
            "Radiohead - Creep (Remastered) (Audio)", "Radiohead", "Creep",
            description="Remastered With Artificial Intelligence"))

    def test_pitched_and_boosted_uploads_are_refused(self):
        for title in ("Some Band - Some Song (sped up)",
                      "Some Band - Some Song [Bass Boosted]"):
            self.assertIsNone(score(title, "Some Band", "Some Song"),
                              f"should refuse: {title}")

    def test_an_ordinary_remaster_is_fine(self):
        self.assertIsNotNone(score("Weezer - Buddy Holly (2024 Remaster)",
                                   "Weezer", "Buddy Holly"))


class TestDuration(unittest.TestCase):
    def test_a_wildly_wrong_length_is_refused(self):
        self.assertIsNone(score("Some Band - Some Song", "Some Band",
                                "Some Song", expected_ms=160_000, duration=600))

    def test_the_right_length_scores_higher(self):
        close = score("Some Band - Some Song", "Some Band", "Some Song",
                      expected_ms=200_000, duration=200)
        loose = score("Some Band - Some Song", "Some Band", "Some Song",
                      expected_ms=200_000, duration=215)
        self.assertGreater(close, loose)

    def test_absurd_lengths_are_refused_without_an_expectation(self):
        self.assertIsNone(score("Some Band - Some Song", "Some Band",
                                "Some Song", duration=20))
        self.assertIsNone(score("Some Band - Some Song", "Some Band",
                                "Some Song", duration=4000))


if __name__ == "__main__":
    unittest.main()
