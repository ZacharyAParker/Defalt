"""The sign-on, and the house style that shapes every break.

These do not call a model. What is worth pinning down is that the segment
exists, that it can be switched off, that it is not in the rotation, and that
the one setting deciding whether this station is funny or unbearable actually
reaches the briefs.
"""
import unittest
from unittest import mock

from radio import config
from radio.segments import writers


class Stub:
    """Stands in for the config so a test never writes to overrides.yaml,
    which is the user's file and outlives the test run."""

    def __init__(self, values):
        self.values = values

    def get(self, dotted, default=None):
        return self.values.get(dotted, default)


class Registered(unittest.TestCase):
    def test_the_sign_on_is_a_segment_like_any_other(self):
        self.assertIn("sign_on", writers.WRITERS)
        self.assertIn("sign_on", writers.SEGMENT_KINDS)

    def test_it_is_not_in_the_rotation_weights(self):
        # It fires once when the station comes up, so putting it in the
        # weighted pool would have the station introducing itself mid-shift.
        weights = config.station.get("clock.segment_weights", {}) or {}
        self.assertNotIn("sign_on", weights)

    def test_it_can_be_switched_off(self):
        # Present and defaulting on. A station that cannot be told to stop
        # introducing itself is one you turn off instead.
        self.assertTrue(config.station.get("clock.sign_on", True))


class HouseStyle(unittest.TestCase):
    def humour_with(self, **hosts):
        stub = Stub({"hosts": hosts})
        with mock.patch.object(writers.config, "station", stub):
            return writers._humour()

    def test_the_humour_setting_reaches_the_brief(self):
        self.assertIn(
            "MARKER-PHRASE",
            self.humour_with(humour="MARKER-PHRASE", self_aware=False),
        )

    def test_self_awareness_is_what_makes_them_know_what_they_are(self):
        aware = self.humour_with(humour="", self_aware=True)
        self.assertIn("one person", aware)
        self.assertEqual(self.humour_with(humour="", self_aware=False).strip(), "")

    def test_they_are_meaner_about_themselves_than_about_him(self):
        # The instruction that keeps self-deprecation funny rather than
        # turning it into a station that insults its only listener.
        self.assertIn(
            "meaner about themselves",
            self.humour_with(humour="", self_aware=True),
        )

    def test_they_still_do_the_show(self):
        # Self-aware is not the same as refusing to be a radio station.
        self.assertIn("still doing the show", self.humour_with(self_aware=True))

    def test_self_awareness_is_on_unless_it_is_turned_off(self):
        # Missing key means on: the default should be the interesting one.
        self.assertNotEqual(self.humour_with(humour="").strip(), "")

    def test_the_shipped_style_is_not_empty(self):
        self.assertGreater(len(writers._humour()), 80,
                           "the house style is effectively blank")


class Identity(unittest.TestCase):
    def test_the_station_knows_who_it_is_for(self):
        # The sign-on names the listener, so there has to be one to name.
        identity = config.station.get("identity", {}) or {}
        self.assertTrue(str(identity.get("listener") or "").strip())


if __name__ == "__main__":
    unittest.main()
