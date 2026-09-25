"""THIRD-PARTY-NOTICES.md is generated from the lock files, never typed."""
import importlib.util
import subprocess
import unittest

from radio import about

ROOT = about.ROOT


def generator():
    spec = importlib.util.spec_from_file_location("third_party_notices", ROOT / "tools" / "third-party-notices.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ThirdPartyNotices(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tool = generator()
        try:
            cls.text = cls.tool.generate()
        except (FileNotFoundError, subprocess.CalledProcessError) as error:
            raise unittest.SkipTest(f"cargo metadata unavailable: {error}")

    def test_lists_the_packages_that_matter(self):
        crates = self.text.split("### Compiled into the console", 1)[1].split("## Python packages", 1)[0]
        for crate in ("egui", "eframe", "cpal", "symphonia", "ureq"):
            self.assertRegex(crates, rf"(?m)^- {crate} \d", crate)
        self.assertNotRegex(self.text, r"(?m)^- defalt ", "Defalt is not its own third party")
        python = self.text.split("## Python packages", 1)[1].split("## External tools", 1)[0]
        for package in ("Flask", "yt-dlp", "ytmusicapi", "edge-tts", "numpy"):
            self.assertRegex(python, rf"(?m)^- {package} ", package)
        self.assertRegex(self.text, r"(?m)^- Archivo — OFL-1.1 — Copyright 2020 The Archivo Project Authors")
        self.assertRegex(self.text, r"(?m)^- IBM Plex Mono — OFL-1.1")
        self.assertRegex(self.text, r"(?m)^- Signalsmith Stretch [\d.]+ — MIT")
        for tool in ("FFmpeg and ffprobe", "cloudflared", "Codex CLI"):
            self.assertIn(f"- {tool} — ", self.text)
        for heading in ("### MIT License", "### Apache License 2.0", "### BSD 3-Clause License",
                        "### SIL Open Font License 1.1"):
            self.assertEqual(self.text.count(heading), 1, heading)

    def test_output_is_deterministic_and_readable_in_the_app(self):
        self.assertEqual(self.text, self.tool.generate())
        body, footer = self.text.split("\n---\n")
        self.assertIn(f"Defalt v{about.VERSION} · {about.COPYRIGHT}", footer)
        # One paragraph per line: the in-app reader shows every line on its own.
        self.assertNotRegex(body, r"(?m)^[a-z]", "a wrapped line would show as a broken paragraph")
        self.assertNotIn("@", body.split("## License texts")[0], "authors are listed without email addresses")

    def test_committed_file_is_current(self):
        committed = (ROOT / "THIRD-PARTY-NOTICES.md").read_text(encoding="utf-8")
        # yt-dlp floats and some installs differ slightly; the Rust side is locked.
        rust = lambda text: text.split("## Rust crates", 1)[1].split("## Python packages", 1)[0]
        self.assertEqual(rust(committed), rust(self.text),
                         "run tools/third-party-notices.py after changing Cargo.lock")


if __name__ == "__main__":
    unittest.main()
