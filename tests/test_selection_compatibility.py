"""Soft continuity, actual queue context and grounded embedded-lyric evidence."""
import tempfile
import math
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from mutagen.id3 import USLT, TCON
from mutagen.wave import WAVE

from radio import compatibility, config, db, director, importer, mixconfig, taste


def record(key, genre="", artist=None, lyrics="", **extra):
    return {"key": key, "title": key, "artist": artist or key, "genre": genre,
            "lyrics": lyrics, "last_played": None, "source": "local", **extra}


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.settings = {"selection.compatibility.explore_chance": 0,
                         "selection.compatibility.lookahead_depth": 2,
                         "selection.artist_separation": 0,
                         "selection.title_separation_hours": 0}
        patcher = patch.object(config.station, "get", side_effect=lambda k, d=None: self.settings.get(k, d))
        patcher.start()
        self.addCleanup(patcher.stop)
        def data():
            values = {}
            for key, value in self.settings.items():
                node = values
                parts = key.split(".")
                for part in parts[:-1]:
                    node = node.setdefault(part, {})
                node[parts[-1]] = value
            return values
        patcher = patch.object(config.station, "data", side_effect=data)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_genre_aliases_and_families_are_soft_and_unknown_is_neutral(self):
        current = record("a", "Hip-Hop")
        same = compatibility.evaluate(record("b", "hip hop"), current, [current])
        different = compatibility.evaluate(record("c", "Country"), current, [current])
        missing = compatibility.evaluate(record("d"), current, [current])
        self.assertGreater(same["multiplier"], missing["multiplier"])
        self.assertGreater(missing["multiplier"], different["multiplier"])
        self.assertGreater(different["multiplier"], 0)
        self.assertIsNone(missing["evidence"]["genre"])
        self.assertEqual(compatibility.genre_fit(record("x", "deep house"), record("y", "techno")), .7)

    def test_similar_run_eventually_favours_a_vibe_change(self):
        history = [record(str(i), "House") for i in range(8)]
        same, contrast = record("same", "House"), record("change", "Rock")
        before = [compatibility.evaluate(t, history[-1], history[-1:])["multiplier"] for t in (same, contrast)]
        after = [compatibility.evaluate(t, history[-1], history)["multiplier"] for t in (same, contrast)]
        self.assertGreater(before[0], before[1])
        self.assertGreater(after[1], after[0])

    def test_artist_credit_bridge_and_repetition_are_separate_signals(self):
        current = record("a", artist="Kendrick Lamar feat. SZA")
        matching = record("b", artist="SZA")
        other = record("c", artist="Another Artist")
        self.assertEqual(compatibility.evaluate(matching, current, [])["evidence"]["artist"], 1)
        self.assertEqual(compatibility.evaluate(other, current, [])["evidence"]["artist"], .5)
        self.assertLess(compatibility.evaluate(matching, current, [matching] * 4)["multiplier"],
                        compatibility.evaluate(matching, current, [])["multiplier"])
        self.assertEqual(compatibility.artists("Earth, Wind & Fire"), {"earth wind fire"})
        self.assertEqual(compatibility.artists("Tyler, The Creator"), {"tyler the creator"})

    def test_lyrics_need_actual_text_not_a_suggestive_title(self):
        current = record("love and heartbreak")
        self.assertIsNone(compatibility.evaluate(record("love again"), current, [])["evidence"]["lyrics"])
        a = record("a", lyrics="Love kisses darling heart across oceans mountains winter summer candlelight forever")
        b = record("b", lyrics="Loving lover kisses beloved sunlight garden roses starlight promise forever")
        c = record("c", lyrics="Highway train miles journey travel diesel engine station departure landscape")
        score, themes = compatibility.lyrics_fit(a, b)
        self.assertIn("affection", themes)
        self.assertGreater(score, compatibility.lyrics_fit(a, c)[0])
        self.assertIsNone(compatibility.lyrics_fit(a, record("d", lyrics="love love love"))[0])

    def test_untrusted_mix_metadata_cannot_influence_compatibility(self):
        self.assertIsNone(compatibility.mix_fit(record("a", bpm=120, camelot="8A"),
                                                record("b", bpm=120, camelot="8A")))
        good = record("a", bpm=120, bpm_confidence=.8, lufs=-12)
        self.assertGreater(compatibility.mix_fit(good, record("b", bpm=120, bpm_confidence=.8, lufs=-12)),
                           compatibility.mix_fit(good, record("c", bpm=150, bpm_confidence=.8, lufs=-22)))

    def test_energy_direction_is_soft_and_uses_only_measured_energy(self):
        # Perceived energy (0..1) replaced LUFS: a loudness value alone is
        # no longer evidence (stored LUFS mixed pre/post-normalisation).
        current = record("current", energy=.5)
        higher, lower = record("up", energy=.6), record("down", energy=.4)
        self.settings["selection.compatibility.energy_direction"] = "build"
        self.assertGreater(compatibility.energy_fit(current, higher), compatibility.energy_fit(current, lower))
        self.settings["selection.compatibility.energy_direction"] = "ease"
        self.assertGreater(compatibility.energy_fit(current, lower), compatibility.energy_fit(current, higher))
        self.settings["selection.compatibility.energy_direction"] = "follow"
        self.assertGreater(compatibility.energy_fit(current, current), compatibility.energy_fit(current, lower))
        self.settings["selection.compatibility.energy_direction"] = "surprise"
        self.assertGreater(compatibility.energy_fit(current, record("jump", energy=.7)),
                           compatibility.energy_fit(current, current))
        self.assertIsNone(compatibility.energy_fit(current, record("unknown", genre="Metal")))
        self.assertIsNone(compatibility.energy_fit(record("a", lufs=-14), record("b", lufs=-8)))

    def test_wave_reverses_after_a_measured_run_and_unknown_history_stays_neutral(self):
        settings = {"energy_direction": "wave", "energy_arc_tracks": 3, "energy_step_lufs": 2}
        rising = [record(str(i), energy=value) for i, value in enumerate([.2, .3, .4, .5])]
        falling = [record(str(i), energy=value) for i, value in enumerate([.5, .4, .3, .2])]
        step = 2 * compatibility.ENERGY_PER_LU
        self.assertEqual(compatibility.energy_target(rising, settings), (-step, "wave easing"))
        self.assertEqual(compatibility.energy_target(falling, settings), (step, "wave building"))
        self.assertEqual(compatibility.energy_target(rising[:2], settings), (step, "wave building"))
        self.assertEqual(compatibility.energy_target([record("unknown")], settings)[0], 0)
        plateau = [record(str(i), energy=.3) for i in range(4)]
        self.assertEqual(compatibility.energy_target(plateau, settings)[0], step)

    def test_cached_pair_does_not_reuse_energy_direction_from_another_route(self):
        settings = {**compatibility.snapshot(), "energy_direction": "wave", "variety_strength": 0}
        current, next_track = record("current", energy=.5), record("next", energy=.6)
        rising = [record(str(i), energy=value) for i, value in enumerate([.2, .3, .4])] + [current]
        falling = [record(str(i), energy=value) for i, value in enumerate([.8, .7, .6])] + [current]
        cache = {}
        ease = compatibility.evaluate(next_track, current, rising, settings, cache)
        build = compatibility.evaluate(next_track, current, falling, settings, cache)
        self.assertGreater(build["multiplier"], ease["multiplier"])
        self.assertEqual(build, compatibility.evaluate(next_track, current, falling, settings))
        self.assertIn("wave building", build["reason"])

    def test_four_song_outlook_uses_unique_routes_and_bounded_work(self):
        self.settings["selection.compatibility.lookahead_depth"] = 4
        pool = [(1, record(key)) for key in "ABCDEF"]
        preferred = {("A", "B"), ("B", "C"), ("C", "D"), ("D", "E")}
        def score(track, previous, *_):
            return {"multiplier": math.exp(1 if (previous["key"], track["key"]) in preferred else -1)}
        with patch.object(compatibility, "evaluate", side_effect=score) as evaluate:
            result = {track["key"]: (weight, track) for weight, track in compatibility.lookahead(pool, [])}
        route = result["A"][1]["selection"]["lookahead"]
        self.assertEqual([track["key"] for track in route], list("BCDE"))
        self.assertLessEqual(evaluate.call_count, 6 * (5 + 3 * 3 * 4))
        self.assertGreater(result["A"][0], result["F"][0])
        self.assertTrue(all(weight > 0 for weight, _ in result.values()))
        self.assertTrue(all("selection" not in track for _, track in pool))

    def test_short_catalogue_uses_the_available_outlook_without_repeating_tracks(self):
        self.settings["selection.compatibility.lookahead_depth"] = 4
        result = compatibility.lookahead([(1, record("a")), (1, record("b"))], [])
        for _, track in result:
            route = track["selection"]["lookahead"]
            self.assertEqual(len(route), 1)
            self.assertNotEqual(route[0]["key"], track["key"])

    def test_default_outlook_is_three_future_songs_and_profiles_keep_wave_preferences(self):
        self.settings.pop("selection.compatibility.lookahead_depth")
        self.assertEqual(compatibility.snapshot()["lookahead_depth"], 3)
        for profile in mixconfig.PROFILES.values():
            self.assertNotIn("selection.compatibility.energy_arc_tracks", profile)

    def test_lookahead_rewards_viable_two_song_routes_without_reserving_them(self):
        pool = [(1.0, record(key)) for key in "ABCD"]
        def evaluate(track, previous, history, *_):
            good = (previous["key"], track["key"]) in {("A", "B"), ("B", "C")}
            return {"multiplier": math.exp(1 if good else -1)}
        with patch.object(compatibility, "evaluate", side_effect=evaluate), \
             patch.object(db, "query", side_effect=AssertionError("no DB inside route search")):
            result = dict((track["key"], (weight, track))
                          for weight, track in compatibility.lookahead(pool, []))
        self.assertGreater(result["A"][0], result["D"][0])
        route = result["A"][1]["selection"]["lookahead"]
        self.assertEqual([track["key"] for track in route], ["B", "C"])
        self.assertIn("two-song outlook", result["A"][1]["selection"]["reason"])
        self.assertTrue(all(weight > 0 for weight, _ in result.values()))
        self.assertNotIn("selection", pool[0][1])  # no mutation/reservation

    def test_lookahead_is_bounded_and_unexamined_tracks_keep_their_weight(self):
        self.settings["selection.compatibility.lookahead_candidates"] = 8
        pool = [(1.0, record(str(index))) for index in range(100)]
        with patch.object(compatibility, "evaluate", return_value={"multiplier": 1}) as evaluate:
            result = compatibility.lookahead(pool, [])
        self.assertLessEqual(evaluate.call_count, 8 * (7 + 3 * 6))
        self.assertEqual(len(result), 100)
        self.assertTrue(all(weight == 1 for weight, _ in result))
        self.assertEqual(sum("selection" in track for _, track in result), 8)

    def test_forward_routes_do_not_break_artist_separation(self):
        self.settings["selection.artist_separation"] = 6
        pool = [(1.0, record(key, artist="Same Artist")) for key in "ABC"]
        result = compatibility.lookahead(pool, [])
        self.assertTrue(all("selection" not in track for _, track in result))

    def test_route_batch_reads_settings_once_and_next_batch_sees_changes(self):
        pool = [(1.0, record(str(i), "House")) for i in range(16)]
        with patch.object(config.station, "data", wraps=config.station.data) as read:
            compatibility.lookahead(pool, [])
        self.assertEqual(read.call_count, 1)
        settings = compatibility.snapshot()
        self.settings["selection.compatibility.energy_direction"] = "ease"
        self.assertEqual(settings["energy_direction"], "follow")
        self.assertEqual(compatibility.snapshot()["energy_direction"], "ease")

    def test_cached_pair_facts_preserve_different_history_fatigue(self):
        same = record("same", "House")
        current = record("current", "House")
        settings, cache = compatibility.snapshot(), {}
        short = compatibility.evaluate(same, current, [current], settings, cache)
        long = compatibility.evaluate(same, current, [record(str(i), "House") for i in range(8)], settings, cache)
        self.assertGreater(short["multiplier"], long["multiplier"])
        self.assertEqual(long, compatibility.evaluate(same, current,
            [record(str(i), "House") for i in range(8)], settings))

    def test_style_profiles_preserve_user_selection_preferences(self):
        preferences = {"selection.compatibility.genre_weight": .19,
                       "selection.compatibility.energy_direction": "ease",
                       "selection.compatibility.lookahead_enabled": False}
        for profile in mixconfig.PROFILES.values():
            draft = {**preferences, **profile}
            self.assertEqual({key: draft[key] for key in preferences}, preferences)
            self.assertFalse(any(key.startswith("selection.") for key in profile))

    def pick(self, pool, previous, history=None, roll=0):
        with patch.object(taste, "candidates", return_value=pool), \
             patch.object(taste, "affinity", return_value=0), \
             patch.object(taste, "daypart_fit", return_value=.5), \
             patch.object(taste.random, "random", return_value=.5), \
             patch.object(taste.random, "uniform", side_effect=lambda lo, hi: roll * hi), \
             patch.object(db, "query", return_value=[]):
            return taste.pick_next(previous=previous, history=history or [previous])

    def test_every_genre_remains_reachable_in_weighted_sampling(self):
        previous = record("current", "House")
        pool = [record("same", "House"), record("different", "Metal")]
        self.assertEqual(self.pick(pool, previous, roll=0)["key"], "same")
        self.assertEqual(self.pick(pool, previous, roll=.99999)["key"], "different")

    def test_clean_editions_do_not_return_when_rotation_relaxes(self):
        self.settings["selection.artist_separation"] = 6
        self.settings["selection.compatibility.explore_chance"] = 1
        previous = record("current", artist="Shared")
        clean = record("clean", title="Song (Clean)", artist="Shared")
        normal = record("normal", artist="Shared")
        self.assertIsNone(self.pick([clean], previous))
        chosen = self.pick([clean, normal], previous)
        self.assertEqual(chosen["key"], "normal")
        self.assertTrue(chosen["selection"]["separation_relaxed"])
        self.settings["selection.avoid_clean_versions"] = False
        self.assertEqual(self.pick([clean], previous)["key"], "clean")

    def test_missing_lyrics_candidate_retains_a_positive_sampling_chance(self):
        text = "Love kisses darling heart across oceans mountains winter summer candlelight forever"
        previous = record("current", lyrics=text)
        pool = [record("tagged", lyrics=text), record("missing")]
        selected = self.pick(pool, previous, roll=.99999)
        self.assertEqual(selected["key"], "missing")
        self.assertIsNone(selected["selection"]["evidence"]["lyrics"])

    def test_exploration_relaxes_only_compatibility_preferences(self):
        self.settings["selection.compatibility.explore_chance"] = 1
        chosen = self.pick([record("different", "Metal")], record("current", "House"))
        self.assertEqual(chosen["selection"]["multiplier"], 1)
        self.assertTrue(chosen["selection"]["exploration"])

    def test_artist_separation_considers_upcoming_queue(self):
        self.settings["selection.artist_separation"] = 6
        previous = record("current", "House", artist="Shared")
        chosen = self.pick([record("repeat", "House", artist="Shared"),
                            record("fresh", "Rock", artist="Other")], previous)
        self.assertEqual(chosen["key"], "fresh")


class SelectionDatabaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.root = Path(temporary.name)
        self.addCleanup(temporary.cleanup)
        local = threading.local()
        for patcher in (patch.object(db, "_DB_PATH", self.root / "test.db"),
                        patch.object(db, "_LOCAL", local)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        db.connect()

    def insert(self, key, blocked=0):
        db.write("INSERT INTO tracks(key,title,artist,added_at,blocked) VALUES (?,?,?,0,?)",
                 (key, key, key, blocked))

    def test_exclusions_and_blocks_survive_separation_fallback(self):
        self.insert("excluded")
        self.insert("blocked", 1)
        self.assertIsNone(taste.pick_next({"excluded"}))
        self.insert("only")
        db.write("UPDATE tracks SET last_played=9999999999 WHERE key='only'")
        self.assertEqual(taste.pick_next({"excluded"})["key"], "only")

    def test_queue_tail_is_actual_predecessor_and_requests_still_win(self):
        from radio.timeline import Schedule
        instance = director.Station.__new__(director.Station)
        instance.lock = threading.RLock()
        instance._recent_keys = ["recent"]
        instance._lineup = [{"track": record("queued", "Rock")}]
        instance.schedule = Schedule()
        self.insert("recent")
        with patch.object(taste, "pick_next", return_value=record("picked")) as pick:
            instance._next_candidate()
            self.assertEqual(pick.call_args.kwargs["previous"]["key"], "queued")
            self.assertEqual(pick.call_args.args[0], {"queued"})
        self.insert("request")
        db.write("INSERT INTO requests(ts,query,track_key) VALUES (0,'request','request')")
        with patch.object(taste, "pick_next") as pick:
            track, requested = instance._next_candidate()
            self.assertEqual(track["key"], "request")
            self.assertTrue(requested)
            pick.assert_not_called()

    def test_repeated_recent_track_keeps_its_latest_chronological_position(self):
        from radio.timeline import Schedule
        instance = director.Station.__new__(director.Station)
        instance.lock = threading.RLock()
        instance._recent_keys = ["repeat", "between", "repeat"]
        instance._lineup = []
        instance.schedule = Schedule()
        self.insert("repeat")
        self.insert("between")
        with patch.object(taste, "pick_next", return_value=None) as pick:
            instance._next_candidate()
        self.assertEqual(pick.call_args.kwargs["previous"]["key"], "repeat")
        self.assertEqual([t["key"] for t in pick.call_args.kwargs["history"]], ["between", "repeat"])

    def test_embedded_lyrics_refresh_without_reanalysing_audio(self):
        import wave
        path = self.root / "Artist - Song.wav"
        with wave.open(str(path), "wb") as output:
            output.setparams((1, 2, 8000, 0, "NONE", "not compressed"))
            output.writeframes(b"\x00\x00" * 800)
        audio = WAVE(path)
        audio.add_tags()
        lyrics = "Love kisses darling heart across oceans mountains winter summer candlelight forever"
        audio.tags.add(USLT(encoding=3, lang="eng", desc="", text=lyrics))
        audio.tags.add(TCON(text=["Soul"]))
        audio.save()
        self.insert("song")
        updated = importer.refresh_tags(dict(db.one("SELECT * FROM tracks WHERE key='song'")), path)
        self.assertEqual(updated["lyrics"], lyrics)
        self.assertEqual(db.one("SELECT genre FROM tracks WHERE key='song'")["genre"], "Soul")
        self.assertIn("affection", compatibility.lyric_features(updated["lyrics"])[1])
        with patch.object(importer, "metadata") as read:
            importer.refresh_tags(updated, path)
            read.assert_not_called()


if __name__ == "__main__":
    unittest.main()
