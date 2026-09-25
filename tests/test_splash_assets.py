"""The startup ident's generated files match their source and recipe."""
import importlib.util
import shutil
import struct
import unittest

from radio import about

ROOT = about.ROOT


def builder():
    spec = importlib.util.spec_from_file_location("build_splash", ROOT / "tools" / "build-splash.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SplashAssets(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tool = builder()

    def test_outputs_are_current(self):
        # --check reads the outputs back without ffmpeg; the skip is for a
        # checkout that never had the ident committed alongside the tool.
        if not self.tool.MANIFEST.is_file() and not shutil.which("ffmpeg"):
            raise unittest.SkipTest("no splash manifest and no ffmpeg to build one")
        self.assertEqual(self.tool.check(), [])

    def test_the_blob_round_trips(self):
        frames = [b"RIFF\x00\x00\x00\x00WEBPone", b"RIFF\x00\x00\x00\x00WEBPtwo!"]
        blob = self.tool.pack(frames, 12, 10, (24, 1))
        self.assertEqual(blob[:4], b"DSPL")
        self.assertEqual(struct.unpack_from("<I", blob, 8)[0], 2)
        back = self.tool.unpack(blob)
        self.assertEqual((back["count"], back["width"], back["height"], back["fps"]), (2, 12, 10, [24, 1]))
        self.assertEqual(back["frames"], frames)
        with self.assertRaises(ValueError):
            self.tool.unpack(b"NOPE" + blob[4:])

    def test_the_web_intro_is_faststart_and_small(self):
        data = (ROOT / "web" / "static" / "splash" / "intro.mp4").read_bytes()
        atoms = self.tool.top_level_atoms(data)
        self.assertLess(atoms.index("moov"), atoms.index("mdat"))
        self.assertLess(len(data), 2_500_000, "the intro should stay a light download")

    def test_the_intro_is_served_with_its_type_and_in_ranges(self):
        from radio.app import app
        client = app.test_client()
        local = "http://127.0.0.1:8090"
        response = client.get("/static/splash/intro.mp4?v=1", base_url=local, headers={"Range": "bytes=0-1023"})
        self.assertEqual(response.status_code, 206)
        self.assertEqual(response.mimetype, "video/mp4")
        self.assertEqual(len(response.data), 1024)
        self.assertTrue(response.headers["Content-Range"].startswith("bytes 0-1023/"))
        response.close()
        poster = client.get("/static/splash/intro-poster.webp", base_url=local)
        self.assertEqual(poster.mimetype, "image/webp")
        poster.close()

    def test_the_service_worker_leaves_the_video_alone(self):
        worker = (ROOT / "web" / "sw.js").read_text(encoding="utf-8")
        shell = worker.split("const SHELL = [", 1)[1].split("];", 1)[0]
        self.assertNotIn("splash", shell)


if __name__ == "__main__":
    unittest.main()
