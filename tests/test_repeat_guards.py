"""Native listening history and alias-aware cooldowns protect rotation."""
import threading
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from radio import config, director, library, taste, timeline


class RepeatGuards(unittest.TestCase):
    def setUp(self):
        self.settings = {"selection.title_separation_hours": 5, "selection.artist_separation": 6}
        for p in (patch.object(config.station, "get", side_effect=lambda k,d=None: self.settings.get(k,d)),
                  patch.object(taste, "affinity", return_value=0), patch.object(taste, "daypart_fit", return_value=1),
                  patch.object(taste, "_recent_artists", return_value={"artist"}),
                  patch.object(taste.vibe, "current", return_value=None),
                  patch.object(taste.time, "time", return_value=100000)):
            p.start(); self.addCleanup(p.stop)

    def track(self, key, last=None, **extra):
        return dict(key=key, title=key, artist="Artist", last_played=last, source="local", **extra)

    def pick(self, tracks, exclude=()):
        with patch.object(taste, "candidates", return_value=tracks):
            return taste.pick_next(set(exclude), history=[self.track("previous")])

    def test_artist_relaxation_does_not_also_relax_song_cooldown(self):
        result = self.pick([self.track("recent",99000),self.track("fresh")])
        self.assertEqual(result["key"],"fresh")

    def test_exhausted_library_uses_longest_rested_song(self):
        result = self.pick([self.track("recent",99000),self.track("oldest",85000)])
        self.assertEqual(result["key"],"oldest")

    def test_same_video_under_another_catalogue_key_is_not_queued_again(self):
        result = self.pick([self.track("one",video_id="abcdefghijk"),
                            self.track("alias",video_id="abcdefghijk")], ["one"])
        self.assertIsNone(result)

    def test_album_and_feature_credit_alias_share_cooldown(self):
        first = dict(self.track("first",99000),title="Song",artist="Artist")
        second = dict(self.track("second"),title="Song (feat. Guest)",artist="Artist, Guest")
        self.assertTrue(taste.recording_ids(first) & taste.recording_ids(second))
        self.assertEqual(self.pick([first,second,self.track("fresh")])["key"],"fresh")

    def test_browser_and_native_reports_count_same_airing_only_once(self):
        station=director.Station.__new__(director.Station)
        station.lock=threading.RLock()
        station.clock=SimpleNamespace(now=lambda:20)
        station.schedule=timeline.Schedule()
        item=station.schedule.add_music("a",dict(self.track("a"),duration=100))
        with patch.object(director.db,"one",return_value={"artist":"Artist"}), \
             patch.object(director.db,"log_event"), patch.object(taste,"mark_played") as mark:
            station.report("started","a",item_id=item.id)
            station.report("started","a",item_id=item.id)
            station.report("started","a",item_id="stale")
            self.assertEqual(mark.call_count,1)

    def test_video_only_search_never_falls_back_to_music_video(self):
        entry=dict(id="abcdefghijk",title="Artist - Song (Official Video)",duration=200)
        with patch.object(library.sourceio,"search",return_value={"entries":[entry]}), \
             patch.object(library,"_source_info",return_value=entry):
            self.assertIsNone(library.resolve("Artist","Song"))

    def test_gameplay_clip_is_not_a_recording_candidate(self):
        entry=dict(title="Why's this dealer? | BeamNG.drive",duration=130)
        self.assertIsNone(library._candidate_score(entry,"unknown","whys this dealer",0))
