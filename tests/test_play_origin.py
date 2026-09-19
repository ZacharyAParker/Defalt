"""A catalogue request must never become ownership of every later airing."""
import threading
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from radio import director, library, timeline, versions
from radio.segments import base, personal


class PlayOriginTests(unittest.TestCase):
    def station(self):
        station = director.Station.__new__(director.Station)
        station.lock = threading.RLock()
        station._lineup = []
        station.schedule = timeline.Schedule()
        return station

    def test_automatic_replay_does_not_inherit_request_identity(self):
        station = self.station()
        track = dict(key="a", title="Song", artist="Artist", source="request", _request_id=7)
        station._enqueue(track, "auto")
        row = station._lineup[0]
        self.assertIsNone(row["request_id"])
        self.assertEqual(row["track"]["selection_origin"]["by"], "director")
        self.assertNotIn("_request_id", row["track"])
        self.assertEqual(track["_request_id"], 7)

    def test_removing_one_request_does_not_cancel_another_for_same_song(self):
        station = self.station()
        track = dict(key="a", title="Song", artist="Artist")
        first = station._enqueue(dict(track, _request_id=7), "request")
        station._enqueue(dict(track, _request_id=8), "request")
        with patch.object(director.db, "write") as write:
            self.assertTrue(station.remove(first))
        self.assertEqual(write.call_args.args[1][1], 7)
        self.assertEqual(station._lineup[0]["request_id"], 8)

    def test_request_finishes_at_airtime_once_and_only_for_its_id(self):
        station = self.station()
        items = [SimpleNamespace(start_at=t, end_at=t+30, meta={"key": "same", "selection_origin": {"request_id": i}})
                 for t, i in ((10, 7), (50, 8))]
        station.schedule = SimpleNamespace(music_items=lambda: items)
        with patch.object(director.db, "write") as write:
            station._finish_requests(9)
            write.assert_not_called()
            station._finish_requests(10)
            station._finish_requests(11)
            self.assertEqual(write.call_count, 1)
            self.assertEqual(write.call_args.args[1], (7,))
            station._finish_requests(100)
            self.assertIn("cancelled", write.call_args.args[0])
            self.assertEqual(write.call_args.args[1][1], 8)

    def test_current_origin_overrides_old_request_flag_in_dialogue(self):
        track = dict(key="a", title="Song", artist="Artist",
                     selection_origin={"by": "director", "method": "automatic"})
        with patch.object(personal, "facts", side_effect=lambda t: {"selected_for_this_play": t["selection_origin"]} if t else None):
            evidence = personal.evidence({"next": track, "was_request": True})
        self.assertFalse(evidence["incoming_is_listener_request"])

    def test_stock_contrast_is_replaced_but_ordinary_negation_is_kept(self):
        fallback = [base.Line("mav", "Our shuffle needs supervision.")]
        for text in ("That's not a playlist, that's a cry for help.",
                     "This isn't a song. It's a lifestyle.",
                     "Not just a song but a lifestyle."):
            with patch.object(base.llm, "complete_json", return_value=[{"host": "mav", "text": text}]):
                self.assertEqual(base.write("brief", fallback=fallback), fallback)
        with patch.object(base.llm, "complete_json", return_value=[{"host": "mav", "text": "That's not on our playlist today."}]):
            self.assertNotEqual(base.write("brief", fallback=fallback), fallback)


class OriginalSourceTests(unittest.TestCase):
    def test_cached_alternate_is_rechecked_without_deleting_old_audio(self):
        with tempfile.TemporaryDirectory() as folder:
            old = Path(folder) / "old.m4a"
            old.write_bytes(b"keep existing playback")
            track = dict(key="a", title="Song", artist="Artist", file=str(old),
                         video_id="abcdefghijk", source="seed", bpm=100)
            with patch.object(library.db, "one", return_value=track), \
                 patch.object(library, "AUDIO_DIR", Path(folder)), \
                 patch.object(library, "_source_info", return_value={"title": "Artist - Song (Official Audio)",
                     "description": "Official live audio", "duration": 193}), \
                 patch.object(library, "fetch_recording", side_effect=RuntimeError("refetch reached")) as fetch:
                with self.assertRaisesRegex(RuntimeError, "refetch reached"):
                    library._ensure_locked(track)
                self.assertIsNone(fetch.call_args.kwargs["video_id"])
                self.assertTrue(old.exists())

    def test_hidden_live_audio_is_rejected_even_with_official_title(self):
        entry = dict(title="Miguel - Sure Thing (Official Audio)", duration=193,
                     description="Miguel's official live audio for 'Sure Thing'.")
        self.assertIsNone(library._candidate_score(entry, "Miguel", "Sure Thing", 0))

    def test_explicit_alternate_request_is_respected(self):
        for edition in ("Acoustic", "Unplugged", "Live"):
            title = f"Song ({edition})"
            entry = dict(title=f"Artist - {title}", duration=193,
                         description=f"{edition} audio recording")
            self.assertIsNotNone(library._candidate_score(entry, "Artist", title, 0))
            self.assertTrue(versions.alternate_track({"title": title}))
        self.assertFalse(versions.alternate_track({"title": "Live Forever"}))

    def test_resolver_checks_description_before_accepting_search_title(self):
        entries = [dict(id="abcdefghijk", title="Artist - Song (Official Audio)", duration=193),
                   dict(id="12345678901", title="Artist - Song (Lyrics)", duration=193)]
        with patch.object(library.sourceio, "search", return_value={"entries": entries}), \
             patch.object(library, "_source_info", side_effect=lambda vid: {
                 "description": "Official live audio" if vid == "abcdefghijk" else "Studio recording"}):
            self.assertEqual(library.resolve("Artist", "Song"), "12345678901")
