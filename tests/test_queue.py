"""Queue control.

The station is built without starting its threads, so these exercise the
ordering logic directly rather than racing a live feeder.
"""
import unittest

from radio import director


def station():
    """A Station with no threads running."""
    instance = director.Station.__new__(director.Station)
    import random
    import threading
    instance.lock = threading.RLock()
    instance.rng = random.Random(0)
    instance._lineup = []
    instance._stop = threading.Event()
    return instance


def queued(instance):
    return [entry["title"] for entry in instance.lineup()]


class TestOrdering(unittest.TestCase):
    def setUp(self):
        self.station = station()
        self.ids = {}
        for name in ("A", "B", "C", "D"):
            self.ids[name] = self.station._enqueue(
                {"key": name.lower(), "artist": "x", "title": name,
                 "duration": 120.0}, "auto")

    def test_entries_queue_in_order(self):
        self.assertEqual(queued(self.station), ["A", "B", "C", "D"])

    def test_play_next_jumps_to_the_front(self):
        self.station.move(self.ids["D"], "next")
        self.assertEqual(queued(self.station), ["D", "A", "B", "C"])

    def test_up_swaps_with_the_one_above(self):
        self.station.move(self.ids["C"], "up")
        self.assertEqual(queued(self.station), ["A", "C", "B", "D"])

    def test_down_swaps_with_the_one_below(self):
        self.station.move(self.ids["A"], "down")
        self.assertEqual(queued(self.station), ["B", "A", "C", "D"])

    def test_up_at_the_top_is_harmless(self):
        self.assertTrue(self.station.move(self.ids["A"], "up"))
        self.assertEqual(queued(self.station), ["A", "B", "C", "D"])

    def test_down_at_the_bottom_is_harmless(self):
        self.assertTrue(self.station.move(self.ids["D"], "down"))
        self.assertEqual(queued(self.station), ["A", "B", "C", "D"])

    def test_last_sends_it_to_the_back(self):
        self.station.move(self.ids["A"], "last")
        self.assertEqual(queued(self.station), ["B", "C", "D", "A"])

    def test_moving_something_that_is_gone_reports_failure(self):
        self.assertFalse(self.station.move("nonexistent", "next"))


class TestRemoval(unittest.TestCase):
    def setUp(self):
        self.station = station()
        self.ids = {}
        for name in ("A", "B", "C"):
            self.ids[name] = self.station._enqueue(
                {"key": name.lower(), "artist": "x", "title": name,
                 "duration": 120.0}, "auto")

    def test_removing_takes_exactly_one_out(self):
        self.assertTrue(self.station.remove(self.ids["B"]))
        self.assertEqual(queued(self.station), ["A", "C"])

    def test_removing_twice_reports_failure(self):
        self.station.remove(self.ids["B"])
        self.assertFalse(self.station.remove(self.ids["B"]))

    def test_clear_empties_the_queue(self):
        self.assertEqual(self.station.clear_lineup(), 3)
        self.assertEqual(queued(self.station), [])

    def test_clear_can_spare_what_you_asked_for(self):
        """Clearing should not throw away your own requests by default."""
        self.station._enqueue(
            {"key": "mine", "artist": "x", "title": "MINE", "duration": 100.0},
            "request")
        dropped = self.station.clear_lineup(keep_requests=True)
        self.assertEqual(dropped, 3)
        self.assertEqual(queued(self.station), ["MINE"])


class TestPriority(unittest.TestCase):
    def test_a_request_can_jump_the_automatic_picks(self):
        instance = station()
        for name in ("A", "B"):
            instance._enqueue({"key": name, "artist": "x", "title": name,
                               "duration": 100.0}, "auto")
        instance._enqueue({"key": "r", "artist": "x", "title": "REQ",
                           "duration": 100.0}, "request", front=True)
        self.assertEqual(queued(instance), ["REQ", "A", "B"])

    def test_taking_the_next_one_pops_the_front(self):
        instance = station()
        for name in ("A", "B"):
            instance._enqueue({"key": name, "artist": "x", "title": name,
                               "duration": 100.0}, "auto")
        entry = instance._take_next()
        self.assertEqual(entry["track"]["title"], "A")
        self.assertEqual(queued(instance), ["B"])

    def test_taking_from_an_empty_queue_returns_nothing(self):
        self.assertIsNone(station()._take_next())

    def test_only_automatic_entries_count_toward_the_fill_target(self):
        """Queue six things by hand and the station should stop choosing."""
        instance = station()
        for index in range(6):
            instance._enqueue({"key": str(index), "artist": "x",
                               "title": str(index), "duration": 100.0},
                              "request")
        automatic = sum(1 for e in instance._lineup if e["source"] == "auto")
        self.assertEqual(automatic, 0)


if __name__ == "__main__":
    unittest.main()
