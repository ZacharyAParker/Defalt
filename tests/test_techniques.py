"""Transition techniques: the shapes they render and how one gets chosen.

What would be audible if these broke: an effect that lands off the beat, a
lane left parked at a kill or a freeze after the mix, two singers stacked, an
effect buried under a host, or the same trick three mixes running.
"""
import collections
import json
import math
import unittest
from unittest.mock import patch

from radio import config, mixconfig, techniques, timeline, transitions
from radio.segments import show


def context(**overrides):
    values = dict(overlap=8.0, beat=0.5, grid=True, one=0.0, lead_room=20.0, out_rate=1.02,
                  in_rate=0.99, harmonic=True, energy_step=0.0, tempo_gap=0.01, tempo_known=True,
                  matched=True, out_vocal=0.1, in_vocal=0.5, stems=True, speech=False, base="blend")
    values.update(overrides)
    return techniques.Context(**values)


def value_at(points, moment):
    if moment <= points[0][0]:
        return points[0][1]
    for (t0, v0), (t1, v1) in zip(points, points[1:]):
        if t0 <= moment <= t1:
            return v1 if t1 == t0 else v0 + (v1 - v0) * (moment - t0) / (t1 - t0)
    return points[-1][1]


class Settings(unittest.TestCase):
    """Fixed settings; nothing reads or writes the listener's history."""
    def setUp(self):
        self.settings = {"transitions.creativity": 0.5}
        for p in (patch.object(config.station, "get",
                               side_effect=lambda key, default=None: self.settings.get(key, default)),
                  patch.object(techniques.history, "recent", return_value=[]),
                  patch.object(techniques.history, "record")):
            p.start()
            self.addCleanup(p.stop)


class Shapes(Settings):
    def shapes(self):
        for name in techniques.FX:
            for ctx in (context(), context(one=1.0, harmonic=False, energy_step=0.3),
                        context(overlap=5.0, beat=0.45, energy_step=-0.3, lead_room=5.0)):
                if techniques.eligible(name, ctx):
                    continue
                yield name, ctx, techniques.build(name, ctx)

    def test_every_technique_builds_for_a_friendly_pair(self):
        ctx = context()
        for name in techniques.FX:
            self.assertEqual(techniques.eligible(name, ctx), "", name)
            self.assertIsNotNone(techniques.build(name, ctx), name)

    def test_lanes_stay_inside_engine_ranges(self):
        for name, ctx, shape in self.shapes():
            for deck, lanes in shape["lanes"].items():
                for lane, points in lanes.items():
                    self.assertIn(lane, techniques.RANGES, (name, lane))
                    low, high = techniques.RANGES[lane]
                    times = [t for t, _ in points]
                    self.assertEqual(times, sorted(times), (name, lane))
                    for _, value in points:
                        self.assertTrue(low <= value <= high, (name, deck, lane, value))

    def test_every_lane_comes_back_to_rest(self):
        """A deck keeps a lane's last value: a kill or a freeze left behind
        would follow the record, or the next record on that deck."""
        for name, ctx, shape in self.shapes():
            for deck, lanes in shape["lanes"].items():
                for lane, points in lanes.items():
                    if lane == "rate":
                        rest = ctx.out_rate if deck == "out" else ctx.in_rate
                    elif lane == "echo_beats":
                        continue
                    else:
                        rest = techniques.NEUTRAL[lane]
                    self.assertAlmostEqual(points[-1][1], rest, 4, (name, deck, lane))
                    self.assertLessEqual(points[-1][0], ctx.overlap + 1e-6, (name, deck, lane))

    def test_outgoing_effects_start_inside_the_free_part_of_the_record(self):
        for name, ctx, shape in self.shapes():
            for lane, points in shape["lanes"]["out"].items():
                self.assertGreaterEqual(points[0][0], ctx.one - ctx.lead_room - 1e-6, (name, lane))
            for lane, points in shape["lanes"]["in"].items():
                self.assertGreaterEqual(points[0][0], -1e-6, (name, lane))

    def test_volumes_are_envelopes_over_the_overlap(self):
        for name, ctx, shape in self.shapes():
            for key in ("out_volume", "in_volume"):
                points = shape[key]
                self.assertEqual(points[0][0], 0.0, (name, key))
                self.assertAlmostEqual(points[-1][0], ctx.overlap, 4, (name, key))
                self.assertEqual([t for t, _ in points], sorted(t for t, _ in points))
                for _, value in points:
                    self.assertTrue(techniques.SILENT <= value <= 1.0, (name, key, value))
            self.assertLess(shape["out_volume"][-1][1], 0.01, name)
            self.assertAlmostEqual(shape["in_volume"][-1][1], 1.0, 4, name)

    def test_gain_staging_never_doubles_the_level(self):
        """Both decks at full, full band, for any length is how the limiter
        ends up doing the mixing."""
        for name, ctx, shape in self.shapes():
            level = shape["lanes"]["out"].get("level")
            for i in range(81):
                t = ctx.overlap * i / 80
                out = value_at(shape["out_volume"], t) * (value_at(level, t) if level else 1.0)
                incoming = value_at(shape["in_volume"], t)
                self.assertLessEqual(out + incoming, 2.0 + 1e-6, (name, t))
                if name not in ("filter_ride", "stem_swap", "acapella_intro"):
                    # Only moves that split the spectrum or the stems keep
                    # both records fully up together.
                    self.assertLessEqual(out ** 2 + incoming ** 2, 1.3, (name, t))

    def test_cut_style_moves_close_the_outgoing_fader_on_the_one(self):
        for name in ("loop_roll", "brake", "drop_swap", "echo_out", "echo_freeze"):
            ctx = context(one=1.0)
            shape = techniques.build(name, ctx)
            level = shape["lanes"]["out"]["level"]
            self.assertAlmostEqual(value_at(level, ctx.one - 0.01), 1.0, 2, name)
            self.assertLess(value_at(level, ctx.one + ctx.b / 8 + 0.01), 0.01, name)
            self.assertLess(value_at(shape["in_volume"], ctx.one - 0.05), 0.01, name)
            self.assertAlmostEqual(value_at(shape["in_volume"], ctx.one + 0.05), 1.0, 3, name)

    def test_loop_roll_halves_on_the_beat_into_the_downbeat(self):
        ctx = context(one=1.0)
        events = techniques.build("loop_roll", ctx)["events"]
        self.assertEqual([e["length_seconds"] / ctx.b for e in events], [4, 2, 1, 0.5])
        for first, second in zip(events, events[1:]):
            self.assertAlmostEqual(first["until"], second["at"])
        self.assertAlmostEqual(events[-1]["until"], ctx.one)
        for event in events:
            self.assertEqual((event["type"], event["deck"]), ("roll", "out"))
            self.assertAlmostEqual(((event["at"] - ctx.one) / ctx.b) % 1, 0.0, 6)
        short = techniques.build("loop_roll", context(lead_room=5.0))["events"]
        self.assertEqual([e["length_seconds"] / 0.5 for e in short], [2, 1, 0.5, 0.25])

    def test_brake_reaches_a_stop_on_the_one(self):
        ctx = context(one=1.0)
        rate = techniques.build("brake", ctx)["lanes"]["out"]["rate"]
        self.assertAlmostEqual(rate[0][1], ctx.out_rate)
        self.assertAlmostEqual(value_at(rate, ctx.one), 0.0)
        self.assertAlmostEqual(((ctx.one - rate[0][0]) / ctx.b) % 1, 0.0, 6)
        values = [v for t, v in rate if t <= ctx.one]
        self.assertEqual(values, sorted(values, reverse=True), "a brake only slows down")

    def test_spinback_goes_backwards(self):
        rate = techniques.build("spinback", context())["lanes"]["out"]["rate"]
        self.assertLess(min(v for _, v in rate), -1.0)

    def test_clashing_keys_get_short_filtered_tails(self):
        friendly = techniques.build("echo_out", context())
        clash = techniques.build("echo_out", context(harmonic=False))
        tail = lambda shape: next(t for t, v in shape["lanes"]["out"]["echo_feedback"] if t > 0 and v == 0)
        self.assertLess(tail(clash), tail(friendly))
        self.assertGreater(max(v for _, v in clash["lanes"]["out"]["sweep"]),
                           max(v for _, v in friendly["lanes"]["out"]["sweep"]))

    def test_echo_freeze_actually_freezes_then_lets_go(self):
        lanes = techniques.build("echo_freeze", context())["lanes"]["out"]
        self.assertGreaterEqual(max(v for _, v in lanes["echo_feedback"]), 0.95)
        self.assertLess(lanes["echo_feedback"][-2][1], 0.5)

    def test_stem_moves_hand_over_on_a_bar(self):
        ctx = context(overlap=10.0)
        shape = techniques.build("stem_swap", ctx)
        swap = shape["lanes"]["in"]["stem_vocals"][1][0]
        self.assertAlmostEqual(((swap - ctx.one) / (4 * ctx.b)) % 1, 0.0, 6)
        self.assertEqual(shape["requires"], ["stems"])


class Eligibility(Settings):
    def test_stem_techniques_need_stems_on_both_records(self):
        for name in ("stem_swap", "acapella_intro"):
            self.assertIn("stems", techniques.eligible(name, context(stems=False)))
            self.assertEqual(techniques.eligible(name, context(stems=True)), "")

    def test_nothing_flashy_under_speech(self):
        ctx = context(speech=True)
        for name in techniques.FX:
            self.assertTrue(techniques.eligible(name, ctx), name)
        self.settings["transitions.creativity"] = 1.0
        for seed in range(50):
            self.assertEqual(techniques.select(ctx, (), seed)[0], "blend")

    def test_two_singers_are_never_stacked(self):
        ctx = context(out_vocal=0.7, in_vocal=0.6, stems=False)
        for name in ("echo_out", "echo_freeze", "reverb_wash", "filter_ride"):
            self.assertIn("vocals", techniques.eligible(name, ctx), name)
        # Moves that never overlap the two records are still fine.
        self.assertEqual(techniques.eligible("brake", ctx), "")

    def test_beat_moves_need_a_grid_and_long_blends_need_matched_tempos(self):
        self.assertTrue(techniques.eligible("loop_roll", context(grid=False)))
        self.assertTrue(techniques.eligible("filter_ride", context(matched=False)))
        self.assertTrue(techniques.eligible("brake", context(tempo_known=False, beat=None)))
        self.assertEqual(techniques.eligible("reverb_wash", context(tempo_known=False, beat=None)), "")

    def test_an_acapella_needs_compatible_keys(self):
        self.assertTrue(techniques.eligible("acapella_intro", context(harmonic=False)))
        self.assertTrue(techniques.eligible("acapella_intro", context(harmonic=None)))

    def test_no_lead_in_room_means_no_lead_in_effect(self):
        ctx = context(lead_room=0.4)
        self.assertTrue(techniques.eligible("loop_roll", ctx))
        self.assertTrue(techniques.eligible("brake", ctx))


class Selector(Settings):
    def picks(self, ctx, seeds=400, recent=()):
        return collections.Counter(techniques.select(ctx, recent, seed)[0] for seed in range(seeds))

    def test_creativity_zero_is_smooth_radio(self):
        self.settings["transitions.creativity"] = 0.0
        self.assertEqual(set(self.picks(context())), {"blend"})

    def test_more_creativity_means_more_effects(self):
        shares = []
        for amount in (0.2, 0.5, 1.0):
            self.settings["transitions.creativity"] = amount
            counts = self.picks(context())
            shares.append(1 - counts["blend"] / sum(counts.values()))
        self.assertEqual(shares, sorted(shares))
        self.assertGreater(shares[-1], 0.8)
        self.assertTrue(0.25 < shares[1] < 0.8, shares)

    def test_the_choice_is_varied(self):
        self.settings["transitions.creativity"] = 1.0
        counts = self.picks(context())
        self.assertGreaterEqual(len([n for n in counts if n in techniques.FX]), 7, counts)
        self.assertLess(max(counts.values()) / sum(counts.values()), 0.35, counts)

    def test_the_same_pair_gets_the_same_answer(self):
        self.settings["transitions.creativity"] = 0.7
        seed = techniques.seed_for("a|one", "b|two")
        self.assertEqual({techniques.select(context(), ["brake"], seed) for _ in range(5)}.__len__(), 1)
        self.assertNotEqual(seed, techniques.seed_for("b|two", "a|one"))

    def test_recent_techniques_are_rarely_repeated(self):
        self.settings["transitions.creativity"] = 1.0
        fresh = self.picks(context())
        for name in ("echo_out", "loop_roll"):
            repeated = self.picks(context(), recent=["fade", name])
            self.assertLess(repeated[name], max(3, fresh[name] * 0.15), name)
        self.settings["transitions.technique_memory"] = 1
        self.assertGreater(self.picks(context(), recent=["echo_out", "fade"])["echo_out"],
                           fresh["echo_out"] * 0.5, "outside the memory it is fair game again")

    def test_energy_direction_steers_the_choice(self):
        self.settings["transitions.creativity"] = 1.0
        up = self.picks(context(energy_step=0.4))
        down = self.picks(context(energy_step=-0.4))
        self.assertGreater(up["loop_roll"], down["loop_roll"])
        self.assertGreater(down["reverb_wash"] + down["echo_out"], up["reverb_wash"] + up["echo_out"])

    def test_a_big_tempo_gap_reaches_for_cuts_not_long_blends(self):
        self.settings["transitions.creativity"] = 1.0
        counts = self.picks(context(matched=False, grid=False, tempo_gap=0.2))
        for name in ("filter_ride", "stem_swap", "acapella_intro", "loop_roll"):
            self.assertEqual(counts[name], 0, name)
        self.assertGreater(counts["brake"] + counts["echo_out"] + counts["spinback"], 0)

    def test_banned_and_pinned(self):
        self.settings["transitions.creativity"] = 1.0
        self.settings["transitions.allow_echo_out"] = False
        self.assertEqual(self.picks(context())["echo_out"], 0)
        self.settings["transitions.preset"] = "spinback"
        self.assertEqual(set(self.picks(context(), seeds=20)), {"spinback"})
        # Pinned but impossible here: the normal choice, and the reason says why.
        name, why = techniques.select(context(stems=False), (), 1)
        self.settings["transitions.preset"] = "stem_swap"
        name, why = techniques.select(context(stems=False), (), 1)
        self.assertNotEqual(name, "stem_swap")
        self.assertIn("stem_swap not possible", why)

    def test_the_small_hours_and_a_slow_brief_calm_it_down(self):
        self.settings["transitions.creativity"] = 0.8
        self.assertLess(techniques.creativity(context(hour=3)), techniques.creativity(context(hour=15)))
        self.assertLess(techniques.creativity(context(pace="slow")), techniques.creativity(context()))


def row(key, **extra):
    base = {"key": key, "title": key.title(), "artist": "Artist", "duration": 180.0, "bpm": 120.0,
            "beat_period": 0.5, "beat_offset": 0.0, "downbeat_offset": 0.0, "beat_residual_ms": 8,
            "bpm_confidence": 0.9, "camelot": "8A", "key_confidence": 0.9, "intro_sec": 20.0}
    base.update(extra)
    return base


class Schedule(Settings):
    def setUp(self):
        super().setUp()
        self.settings.update({"transitions.creativity": 1.0, "transitions.smart_cues": False})
        # Mid-afternoon: the small hours calm the selector down on purpose.
        afternoon = patch.object(timeline.time, "localtime", return_value=type("T", (), {"tm_hour": 15})())
        afternoon.start()
        self.addCleanup(afternoon.stop)

    def build(self, count=4, **extra):
        schedule = timeline.Schedule()
        for i in range(count):
            schedule.add_music(f"/media/audio/{i}.opus", row(f"k{i}", **extra))
        schedule.seal()
        return schedule

    def test_technique_reaches_the_schedule_on_the_station_clock(self):
        self.settings["transitions.preset"] = "loop_roll"
        schedule = self.build(3)
        first, second, _ = schedule.music_items()
        transition = second.meta["transition"]
        self.assertEqual((transition["technique"], transition["preset"]), ("loop_roll", "loop_roll"))
        self.assertIn(transition["base"], transitions.PRESETS)
        self.assertGreaterEqual(transition["switch_at"], second.start_at)
        for lane, points in transition["lanes"]["out"].items():
            for t, _ in points:
                self.assertTrue(first.start_at - 1e-3 <= t <= first.end_at + 1e-3, lane)
        self.assertEqual(len(transition["events"]), 4)
        for event in transition["events"]:
            self.assertTrue(first.start_at <= event["at"] < event["until"] <= first.end_at + 1e-3)
            # On the outgoing record's beat grid, in station time.
            beats = (event["at"] - first.start_at - first.offset) / 0.5
            self.assertAlmostEqual(beats, round(beats), 3)
        # The outgoing envelope closes on the one; the incoming lands there.
        switch = transition["switch_at"] - second.start_at
        self.assertLess(timeline_gain(first.envelope, first.duration - transition["overlap"] + switch + 0.1), 0.01)
        self.assertGreater(timeline_gain(second.envelope, switch + 0.1), 0.99)

    def test_protocol_serializes(self):
        self.settings["transitions.preset"] = "echo_out"
        schedule = self.build(2)
        payload = json.loads(json.dumps(schedule.as_dict()))
        transition = payload[1]["meta"]["transition"]
        self.assertEqual(set(transition["lanes"]), {"out", "in"})
        for deck in transition["lanes"].values():
            for lane, points in deck.items():
                self.assertTrue(all(len(p) == 2 and all(isinstance(x, float) or isinstance(x, int) for x in p)
                                    for p in points), lane)
        for key in ("preset", "base", "technique", "overlap", "reason", "events", "switch_at", "flashy"):
            self.assertIn(key, transition)

    def test_old_presets_keep_their_fields_and_carry_no_lanes(self):
        self.settings["transitions.creativity"] = 0.0
        schedule = self.build(2)
        transition = schedule.music_items()[1].meta["transition"]
        self.assertEqual(transition["technique"], transition["preset"])
        self.assertNotIn("lanes", transition)
        self.assertIn("automation", schedule.music_items()[1].meta)

    def test_speech_over_the_mix_drops_back_to_the_blend(self):
        self.settings["transitions.preset"] = "echo_out"
        schedule = self.build(2)
        first, second = schedule.music_items()
        self.assertEqual(second.meta["transition"]["technique"], "echo_out")
        schedule.add_voice("/media/voice/a.opus", second.start_at - 1, 5.0)
        schedule.seal()
        transition = second.meta["transition"]
        self.assertEqual(transition["technique"], transition["base"])
        self.assertNotIn("lanes", transition)
        self.assertIn("host talks over", transition["reason"])
        plain = transitions.render(schedule._plans[second.id], transition["overlap"])[1]
        self.assertAlmostEqual(timeline_gain(second.meta["deck_envelope"], transition["overlap"] / 2),
                               value_at(plain.gain, transition["overlap"] / 2), 2)
        # The host leaves again: the technique is back.
        schedule.items = [i for i in schedule.items if i.kind == "music"]
        schedule.seal()
        self.assertEqual(second.meta["transition"]["technique"], "echo_out")

    def test_speech_already_on_the_clock_is_never_given_an_effect(self):
        schedule = timeline.Schedule()
        schedule.add_music("/a/0.opus", row("k0"))
        schedule.add_voice("/v/0.opus", 150.0, 40.0)
        schedule.add_music("/a/1.opus", row("k1"))
        plan = schedule._plans[schedule.music_items()[1].id]
        self.assertEqual(plan.technique, "")
        self.assertIn("talk-over", plan.reason)

    def test_consecutive_mixes_vary(self):
        schedule = self.build(24, energy=0.5)
        names = [i.meta["transition"]["technique"] for i in schedule.music_items()[1:]]
        self.assertGreaterEqual(len(set(names)), 5, names)
        repeats = sum(1 for i, name in enumerate(names)
                      if name in techniques.FX and name in names[max(0, i - 3):i])
        self.assertLessEqual(repeats, 2, names)

    def test_aired_transitions_are_logged_once(self):
        self.settings["transitions.preset"] = "brake"
        schedule = self.build(2)
        second = schedule.music_items()[1]
        schedule.trim_before(second.start_at - 5)
        techniques.history.record.assert_not_called()
        schedule.trim_before(second.meta["transition"]["switch_at"] + 0.1)
        schedule.trim_before(second.meta["transition"]["switch_at"] + 1)
        techniques.history.record.assert_called_once()
        args, kwargs = techniques.history.record.call_args
        self.assertEqual(args[:2], ("brake", "k1"))
        self.assertTrue(kwargs["flashy"])

    def test_stems_only_when_both_records_are_separated(self):
        self.settings["transitions.preset"] = "stem_swap"
        without = self.build(2)
        self.assertNotEqual(without.music_items()[1].meta["transition"]["technique"], "stem_swap")
        with_stems = self.build(2, stems=True)
        transition = with_stems.music_items()[1].meta["transition"]
        self.assertEqual(transition["technique"], "stem_swap")
        self.assertEqual(transition["requires"], ["stems"])


def timeline_gain(envelope, moment):
    return value_at(envelope, moment)


class HostNote(Settings):
    def setUp(self):
        super().setUp()
        self.seen = set()
        for p in (patch("radio.db.is_seen", side_effect=lambda kind, ident: (kind, ident) in self.seen),
                  patch("radio.db.mark_seen", side_effect=lambda kind, ident: self.seen.add((kind, ident)))):
            p.start()
            self.addCleanup(p.stop)
        self.settings["hosts.transition_note_chance"] = 1.0

    def latest(self, **values):
        entry = {"id": 7, "ts": 1000.0, "track_key": "k", "technique": "spinback",
                 "title": "Song", "artist": "Band"}
        entry.update(values)
        return patch.object(techniques.history, "latest", return_value=entry)

    def test_a_flashy_mix_is_offered_once_as_a_fact(self):
        with self.latest():
            note = techniques.host_note(now=1100.0)
            self.assertIn("spinback", note)
            self.assertIn('"Song by Band"', note)
            self.assertEqual(techniques.host_note(now=1101.0), "", "never twice")

    def test_plain_old_or_unlucky_mixes_are_not_mentioned(self):
        with self.latest(technique="blend"):
            self.assertEqual(techniques.host_note(now=1100.0), "")
        with self.latest(id=8):
            self.assertEqual(techniques.host_note(now=1000.0 + 3600), "")
        self.settings["hosts.transition_note_chance"] = 0.0
        with self.latest(id=9):
            self.assertEqual(techniques.host_note(now=1100.0), "")

    def test_the_note_reaches_comedy_briefs_only(self):
        with self.latest(id=10), patch.object(show, "_gap_hours", return_value=None), \
                patch.object(show.showclock, "weather", return_value=""), \
                patch.object(show, "callbacks", return_value=[]):
            self.assertNotIn("MIX NOTE", show.context_block("news", now=1100.0))
            self.assertIn("MIX NOTE", show.context_block("banter", now=1100.0))


class Schema(unittest.TestCase):
    def test_transition_settings_are_grouped_with_units(self):
        fields = {f["key"]: f for f in mixconfig._build_snapshot()["fields"]}
        self.assertEqual(fields["transitions.creativity"]["group"], "Transitions")
        self.assertEqual(fields["transitions.creativity"]["unit"], "percent")
        for name in techniques.FX:
            field = fields[f"transitions.allow_{name}"]
            self.assertEqual((field["kind"], field["group"]), ("bool", "Transitions"))
            self.assertIn(name, fields["transitions.preset"]["bounds"])
        self.assertEqual(set(transitions.TECHNIQUES), set(techniques.FX))

    def test_validation(self):
        self.assertEqual(mixconfig.validate({"transitions.preset": "loop_roll", "transitions.creativity": 0.8,
                                             "transitions.allow_brake": False}),
                         {"transitions.preset": "loop_roll", "transitions.creativity": 0.8,
                          "transitions.allow_brake": False})
        with self.assertRaises(ValueError):
            mixconfig.validate({"transitions.creativity": 1.5})
        with self.assertRaises(ValueError):
            mixconfig.validate({"transitions.preset": "scratch"})


if __name__ == "__main__":
    unittest.main()
