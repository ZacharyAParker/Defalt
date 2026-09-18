"""The separator's child process, and what it can print.

Demucs announces the file it is working on. On Windows that announcement is
where a record with a fullwidth quote in its name used to die -- not in the
audio, not in the path, but in the child's idea of what stdout can carry.
"""
import os
import unittest

from radio import stems


class ChildEnvironment(unittest.TestCase):
    def test_the_child_is_told_to_speak_utf8(self):
        env = stems.child_env()
        self.assertEqual(env["PYTHONIOENCODING"], "utf-8")
        self.assertEqual(env["PYTHONUTF8"], "1")

    def test_the_rest_of_the_environment_survives(self):
        # It has to inherit PATH and CUDA_VISIBLE_DEVICES, or the separator is
        # either not found or quietly on the wrong device.
        os.environ["DEFALT_TEST_MARKER"] = "kept"
        try:
            self.assertEqual(stems.child_env()["DEFALT_TEST_MARKER"], "kept")
        finally:
            del os.environ["DEFALT_TEST_MARKER"]

    def test_a_fullwidth_quote_survives_that_encoding(self):
        # The exact character that killed it, through the exact codec the
        # child is now told to use.
        name = "The Larping Tombstone - \uff02Thick Of It\uff02 (Remix).wav"
        codec = stems.child_env()["PYTHONIOENCODING"]
        self.assertEqual(name.encode(codec).decode(codec), name)
        # And the proof that it was never going to survive the old one.
        with self.assertRaises(UnicodeEncodeError):
            name.encode("cp1252")


if __name__ == "__main__":
    unittest.main()
