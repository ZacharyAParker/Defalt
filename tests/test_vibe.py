"""Persistent brief, bounded song guidance, queue ownership, and request routing."""
import copy
import tempfile
import threading
from pathlib import Path
from unittest.mock import Mock, patch

from radio import config, db, director, intent, library, taste, vibe, wishes
from radio.app import app
from radio.segments import personal
from tests.station_defaults import StationDefaults


class ListeningVibeTests(StationDefaults):
    def setUp(self):
        super().setUp()
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        local = threading.local()
        for p in (patch.object(db, "_DB_PATH", Path(folder.name) / "test.db"),
                  patch.object(db, "_LOCAL", local)):
            p.start()
            self.addCleanup(p.stop)
        self.addCleanup(lambda: getattr(local, "conn", None) and local.conn.close())
        for key, genre in (("quiet", "ambient"), ("loud", "metal"), ("unknown", "")):
            db.write("INSERT INTO tracks(key,title,artist,genre,added_at) VALUES(?,?,?,?,0)",
                     (key, key, key, genre))

    def test_activity_routes_without_a_model_and_explicit_tracks_stay_tracks(self):
        for text in ("I'm studying", "I am gaming but keep it calm", "set vibe to dreamy jazz",
                     "vibe: cooking dinner", "music for a late night drive"):
            with self.subTest(text=text), patch.object(intent.llm, "complete_json", side_effect=AssertionError("network")):
                self.assertEqual(intent.understand(text).kind, "vibe")
        self.assertEqual(intent.route("Artist - Calm").kind, "track")
        self.assertEqual(intent.route("play less metal").kind, "directive")
        self.assertEqual(intent.route("clear vibe").kind, "clear_vibe")

    def test_setting_persists_until_clear_without_changing_taste_or_requests(self):
        before = config.station.data()
        profile = vibe.set_current("Studying, calm and jazzy", enrich=False)
        fresh = config.OverridableConfig(config.station.base.path, config.station.override.path)
        self.assertEqual(fresh.get("listening_vibe.description"), profile["description"])
        self.assertFalse(db.query("SELECT * FROM requests"))
        self.assertFalse(db.query("SELECT * FROM affinity"))
        vibe.clear()
        self.assertEqual(vibe.current(), {})
        self.assertEqual({k: v for k, v in config.station.data().items() if k != "listening_vibe"}, before)

    def test_activity_fallback_and_explicit_negative_preferences(self):
        quiet = vibe.fallback("gaming but calm, no metal")
        self.assertEqual(quiet["pace"], "slow")
        self.assertNotIn("metal", quiet["genres"])
        self.assertIn("metal", quiet["avoid_genres"])
        self.assertGreater(vibe.fit({"genre": "ambient"}, quiet), vibe.fit({"genre": "metal"}, quiet))
        self.assertEqual(vibe.fallback("I am working out at the gym")["pace"], "fast")

    def test_unknown_metadata_is_neutral_and_vibe_is_a_soft_weight(self):
        profile = vibe.fallback("calm")
        self.assertEqual(vibe.fit({"genre": "unknown"}, profile), 1)
        self.assertGreater(vibe.fit({"genre": "metal"}, profile), 0)
        self.assertGreater(vibe.fit({"genre": "ambient"}, profile), 1)
        tempo_only = vibe.fit({"bpm": 85, "bpm_confidence": .9}, profile)
        self.assertGreater(tempo_only, 1)
        self.assertLess(tempo_only, 1.5)
        self.assertNotEqual(vibe.fallback("not calm, keep it energetic")["pace"], "slow")

    def test_many_untagged_tracks_cannot_drown_the_known_matches(self):
        rows = [(4, {"selection": {"vibe_weight": 4}})] + [
            (1, {"selection": {"vibe_weight": 1}}) for _ in range(100)]
        weighted = vibe.focus(rows, {"description": "focus"})
        self.assertAlmostEqual(weighted[0][0] / sum(w for w, _ in weighted), .8)
        self.assertTrue(all(w > 0 for w, _ in weighted))
        self.assertEqual(vibe.focus(rows, {}), rows)

    def test_model_results_only_score_existing_keys_with_finite_bounds(self):
        vibe.set_current("a strange spacey evening", enrich=False)
        profile = copy.deepcopy(vibe.current())
        callback = Mock()
        with patch.object(vibe.llm, "complete_json", return_value={"genres": ["ambient"], "pace": "slow",
                 "fits": {"quiet": .9, "loud": float("nan"), "unknown": 9, "invented": 1}}):
            vibe._enrich(profile, callback)
        self.assertEqual(vibe.current()["fits"], {"quiet": .9})
        self.assertNotIn("fits", vibe.public())
        callback.assert_called_once()

    def test_clear_or_new_brief_wins_over_old_model_response(self):
        for replacement in (None, "party"):
            vibe.set_current("calm", enrich=False)
            profile = copy.deepcopy(vibe.current())
            callback = Mock()
            def respond(*args, **kwargs):
                vibe.set_current(replacement, enrich=False) if replacement else vibe.clear()
                return {"genres": ["ambient"], "fits": {"quiet": 1}}
            with patch.object(vibe.llm, "complete_json", side_effect=respond):
                vibe._enrich(profile, callback)
            self.assertEqual(vibe.current().get("description"), replacement)
            callback.assert_not_called()

    def test_enrichment_callback_does_not_hold_selection_lock(self):
        vibe.set_current('calm', enrich=False)
        observed=[]
        def callback():
            worker=threading.Thread(target=lambda:observed.append(vibe.selection_revision()))
            worker.start()
            worker.join(timeout=1)
            self.assertFalse(worker.is_alive(),'station callback must allow a concurrent selection snapshot')
        with patch.object(vibe.llm,'complete_json',return_value=None):
            vibe._enrich(copy.deepcopy(vibe.current()),callback)
        self.assertEqual(len(observed),1)

    def test_submission_returns_without_waiting_for_model(self):
        with patch.object(vibe.threading.Thread, "start") as start, \
                patch.object(vibe.llm, "complete_json", side_effect=AssertionError("blocked request")):
            result = wishes.submit("Cooking, upbeat funk", mode="vibe")
        self.assertTrue(result["ok"])
        self.assertEqual(vibe.current()["interpretation"], "refining")
        start.assert_called_once()

    def test_model_failure_keeps_the_saved_basic_brief(self):
        vibe.set_current("calm studying", enrich=False)
        with patch.object(vibe.llm, "complete_json", side_effect=RuntimeError("offline")):
            vibe._enrich(copy.deepcopy(vibe.current()))
        self.assertEqual(vibe.public()["interpretation"], "basic")
        self.assertEqual(vibe.public()["pace"], "slow")

    def test_api_sets_reports_and_clears_vibe_and_refreshes_pending_picks(self):
        station = Mock()
        client = app.test_client()
        with patch.object(director, "station", return_value=station), patch.object(vibe.threading.Thread, "start"):
            response = client.post("/api/request", json={"query": "night drive", "mode": "vibe"})
            self.assertEqual(response.status_code, 200)
            self.assertEqual(client.get("/api/vibe").json["vibe"]["description"], "night drive")
            self.assertEqual(client.post("/api/vibe/clear").json["vibe"], {})
            self.assertEqual(station.refresh_vibe.call_count, 2)
            self.assertEqual(client.post("/api/request", json={"query": "", "mode": "vibe"}).status_code, 400)

    def test_refresh_keeps_requests_and_preloaded_music(self):
        station = director.Station.__new__(director.Station)
        station.lock = threading.RLock()
        station._lineup = [{"source": s} for s in ("auto", "request", "deck")]
        station.schedule = object()
        original = station.schedule
        station.refresh_vibe()
        self.assertEqual([e["source"] for e in station._lineup], ["request", "deck"])
        self.assertIs(station.schedule, original)

    def test_automatic_download_finishing_after_vibe_change_is_not_enqueued(self):
        station = director.Station.__new__(director.Station)
        station.lock = threading.RLock()
        station._lineup = []
        def prepare(track):
            vibe.set_current("party", enrich=False)
            return {**track, "duration": 120}
        with patch.object(station, "_listening", return_value=True), \
                patch.object(station, "_lineup_target", return_value=2), \
                patch.object(station, "_next_candidate", return_value=({"key": "quiet"}, False)), \
                patch.object(library, "ensure", side_effect=prepare):
            station._feed_once()
        self.assertEqual(station._lineup, [])

    def test_hosts_receive_the_actual_activity_without_invented_context(self):
        vibe.set_current("building a factory in Satisfactory", enrich=False)
        with patch.object(personal, "write", return_value=[]) as writer:
            personal.comment({"next": {"key": "quiet"}}, "mav", "rue")
        self.assertIn("building a factory in Satisfactory", writer.call_args.args[0])
        self.assertIn("Do not repeat the activity every break", writer.call_args.args[0])

    def test_selection_keeps_vibe_during_exploration_and_does_not_call_model(self):
        vibe.set_current("calm", enrich=False)
        config.station.set_many({"selection.compatibility.explore_chance": 1,
                                 "selection.artist_separation": 0})
        with patch.object(vibe.llm, "complete_json", side_effect=AssertionError("network in selection")), \
                patch.object(taste.random, "uniform", return_value=0):
            selected = taste.pick_next()
        self.assertEqual(selected["selection"]["vibe"], "calm")
        self.assertTrue(selected["selection"]["exploration"])
