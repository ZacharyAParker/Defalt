"""The startup ident's generated files match their source and recipe."""
import importlib.util
import io
import json
import re
import shutil
import struct
import unittest

from radio import about

ROOT = about.ROOT
# How far a decoded pixel may be from the colour it was rendered as; the
# console's own test (src/splash.rs) allows the same.
TOLERANCE = 4


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
        frames = [b"RIFF\x00\x00\x00\x00WEBPone", b"RIFF\x00\x00\x00\x00WEBPtwo!", b"RIFF\x00\x00\x00\x00WEBPthree"]
        blob = self.tool.pack(frames, 12, 10, (30, 1), 1)
        self.assertEqual(blob[:4], b"DSPL")
        self.assertEqual(struct.unpack_from("<I", blob, 8)[0], 3)
        back = self.tool.unpack(blob)
        self.assertEqual((back["count"], back["width"], back["height"], back["fps"]), (3, 12, 10, [30, 1]))
        self.assertEqual(back["logo"], 1)
        self.assertEqual(back["frames"], frames)
        with self.assertRaises(ValueError):
            self.tool.unpack(b"NOPE" + blob[4:])
        # The logo has to be one of the frames, going in and coming out.
        with self.assertRaises(ValueError):
            self.tool.pack(frames, 12, 10, (30, 1), 3)
        lost = bytearray(blob)
        struct.pack_into("<I", lost, 28, 3)
        with self.assertRaises(ValueError):
            self.tool.unpack(bytes(lost))
        # Last release's blob had no logo in it and isn't read as if it did.
        old = bytearray(blob)
        struct.pack_into("<I", old, 4, 1)
        with self.assertRaises(ValueError):
            self.tool.unpack(bytes(old))

    def decoded(self):
        """The built blob, and a way to get one of its frames as a picture."""
        from PIL import Image
        blob = self.tool.unpack((self.tool.NATIVE / "frames.bin").read_bytes())

        def frame(n):
            return Image.open(io.BytesIO(blob["frames"][n])).convert("RGB")
        return blob, frame

    def background(self):
        """The splash colour, from the one place it's written."""
        source = (ROOT / "src" / "splash.rs").read_text(encoding="utf-8")
        found = re.search(r"BACKGROUND: \[u8; 3\] = \[(\d+), (\d+), (\d+)\]", source)
        return tuple(int(value) for value in found.groups())

    def test_the_logo_frame_is_the_same_one_everywhere(self):
        # The recipe names it, the manifest records it and the blob carries
        # it to the console, which reads nothing else.
        manifest = json.loads(self.tool.MANIFEST.read_text(encoding="utf-8"))
        blob, frame = self.decoded()
        logo = self.tool.RECIPE["logo_frame"]
        self.assertEqual(manifest["recipe"]["logo_frame"], logo)
        self.assertEqual(blob["logo"], logo)
        self.assertEqual(manifest["frames"], blob["count"])
        self.assertEqual(manifest["fps"], blob["fps"])
        self.assertLess(logo, blob["count"] - 1, "the ident plays on past its logo")
        # It has the logo on it: a fair share of it isn't background, and
        # some of that is the lit lettering.
        from PIL import Image, ImageChops, ImageStat
        picture = frame(logo)
        red, green, blue = ImageChops.difference(picture, Image.new("RGB", picture.size, self.background())).split()
        furthest = ImageChops.lighter(ImageChops.lighter(red, green), blue)
        drawn = sum(furthest.histogram()[TOLERANCE + 1:])
        self.assertGreater(drawn, picture.width * picture.height // 20)
        red, green, blue = picture.split()
        dimmest = ImageChops.darker(ImageChops.darker(red, green), blue)
        self.assertGreater(dimmest.getextrema()[1], 200, "nothing bright on the logo frame")
        # And the browser's poster is that frame, not some other.
        with Image.open(ROOT / "web" / "static" / "splash" / "intro-poster.webp") as poster:
            small = poster.convert("RGB").resize((150, 120))
        differences = ImageStat.Stat(ImageChops.difference(small, picture.resize((150, 120)))).mean
        self.assertLess(max(differences), 1, "the poster isn't the logo frame")

    def test_it_opens_and_closes_on_the_splash_colour(self):
        # The window opens on the first frame and the console takes over on
        # the last, so both are the one colour all over.
        background = self.background()
        blob, frame = self.decoded()
        for n in (0, blob["count"] - 1):
            for (low, high), want in zip(frame(n).getextrema(), background):
                self.assertLessEqual(abs(low - want), TOLERANCE, f"frame {n}")
                self.assertLessEqual(abs(high - want), TOLERANCE, f"frame {n}")
        # The browser's intro sits on the same colour.
        css = (ROOT / "web" / "static" / "intro.css").read_text(encoding="utf-8")
        self.assertIn("--intro-ground: #%02x%02x%02x;" % background, css)

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
