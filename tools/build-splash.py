"""Build the startup ident from assets/splash-src/Splash.mov.

The source is the OBBY STUDIO ident: 1920x1080, 24 fps, pillarboxed with
pure black bars either side of the real picture. Everything here starts by
finding the picture and cropping the bars off.

    assets/splash/frames.bin        the console's frames, lossy WebP, one blob
    assets/splash/sound.wav         the console's sound, 16-bit 48 kHz stereo
    assets/splash/manifest.json     what was built from what, for --check
    web/static/splash/intro.mp4     the browser's intro, H.264 + AAC, faststart
    web/static/splash/intro-poster.webp   its last frame

The console has no video decoder and doesn't want one, so it gets still
frames it can decode with the image crate, packed as:

    b"DSPL", u32 version, u32 count, u32 width, u32 height, u32 fps_num,
    u32 fps_den, then count x (u32 offset, u32 length), then the frames.
    Little-endian, offsets from the start of the blob.

Needs ffmpeg (FFMPEG_BIN, else ffmpeg on PATH) and Pillow. Re-run after
changing the source or anything in RECIPE:

    .venv\\Scripts\\python.exe tools\\build-splash.py
    .venv\\Scripts\\python.exe tools\\build-splash.py --check   # outputs current?

--check doesn't need ffmpeg: it compares hashes against the manifest and
reads every output back.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "splash-src" / "Splash.mov"
NATIVE = ROOT / "assets" / "splash"
WEB = ROOT / "web" / "static" / "splash"
MANIFEST = NATIVE / "manifest.json"

MAGIC = b"DSPL"
VERSION = 1

# Change anything here and the outputs are stale until rebuilt.
RECIPE = {
    # 1.5x an 800 point wide window: sharp at 150% scaling, a clean
    # downscale at 100% and still fine at 200%.
    "native_width": 1200,
    "native_quality": 85,
    # Where the sound should sit. The source is already about here; this
    # just keeps it there if the source is ever re-exported louder.
    "target_lufs": -16.0,
    "web_crf": 18,
    "web_audio": "128k",
    "poster_quality": 82,
}


def ffmpeg() -> str:
    found = os.environ.get("FFMPEG_BIN") or shutil.which("ffmpeg")
    if not found:
        raise SystemExit("ffmpeg not found: put it on PATH or set FFMPEG_BIN")
    return found


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([ffmpeg(), "-hide_banner", "-nostdin", *args],
                          check=True, capture_output=True, text=True)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def probe_frame(index: int) -> tuple[int, int, bytes]:
    """One source frame as raw RGB, full size."""
    result = subprocess.run(
        [ffmpeg(), "-hide_banner", "-nostdin", "-loglevel", "error", "-i", str(SOURCE),
         "-vf", f"select=eq(n\\,{index}),format=rgb24", "-frames:v", "1", "-f", "rawvideo", "-"],
        check=True, capture_output=True)
    width, height = 1920, 1080
    if len(result.stdout) != width * height * 3:
        raise SystemExit(f"unexpected frame size from the source ({len(result.stdout)} bytes)")
    return width, height, result.stdout


def detect_crop() -> tuple[int, int, int, int]:
    """The picture inside the pillarbox: (width, height, x, y).

    cropdetect doesn't see it, because the ident's background is a dark grey
    rather than black. A column belongs to the picture when it is at least
    half as bright, on average, as the middle of the frame; the one or two
    columns where the bars blend into it don't make that and are cut.
    """
    width, height, rgb = probe_frame(0)
    means = []
    for x in range(width):
        total = 0
        for y in range(0, height, 4):
            i = (y * width + x) * 3
            total += rgb[i] + rgb[i + 1] + rgb[i + 2]
        means.append(total / (3 * len(range(0, height, 4))))
    middle = sorted(means[width // 2 - 100:width // 2 + 100])[100]
    inside = [x for x, mean in enumerate(means) if mean >= middle * 0.5]
    left, right = inside[0], inside[-1] + 1
    # Even edges for 4:2:0.
    left += left % 2
    right -= right % 2
    return right - left, height, left, 0


def loudness(extra: list[str]) -> float:
    result = run("-i", str(SOURCE), "-map", "0:a:0", *extra, "-af", "ebur128", "-f", "null", "-")
    found = re.findall(r"I:\s+(-?[\d.]+) LUFS", result.stderr)
    if not found:
        raise SystemExit("couldn't measure the source's loudness")
    return float(found[-1])


def pack(frames: list[bytes], width: int, height: int, fps: tuple[int, int]) -> bytes:
    header = MAGIC + struct.pack("<6I", VERSION, len(frames), width, height, *fps)
    offset = len(header) + 8 * len(frames)
    index = b""
    for frame in frames:
        index += struct.pack("<2I", offset, len(frame))
        offset += len(frame)
    return header + index + b"".join(frames)


def unpack(blob: bytes) -> dict:
    if blob[:4] != MAGIC:
        raise ValueError("not a splash blob")
    version, count, width, height, fps_num, fps_den = struct.unpack_from("<6I", blob, 4)
    if version != VERSION:
        raise ValueError(f"blob version {version}")
    frames = []
    for n in range(count):
        offset, length = struct.unpack_from("<2I", blob, 28 + 8 * n)
        if offset + length > len(blob):
            raise ValueError(f"frame {n} runs past the end")
        frames.append(blob[offset:offset + length])
    return {"count": count, "width": width, "height": height, "fps": [fps_num, fps_den], "frames": frames}


def top_level_atoms(data: bytes) -> list[str]:
    atoms, at = [], 0
    while at + 8 <= len(data):
        size, kind = struct.unpack_from(">I4s", data, at)
        if size == 1:
            size = struct.unpack_from(">Q", data, at + 8)[0]
        atoms.append(kind.decode("latin-1"))
        if size < 8:
            break
        at += size
    return atoms


def build() -> None:
    from PIL import Image

    if not SOURCE.is_file():
        raise SystemExit(f"missing {SOURCE}")
    width, height, x, y = detect_crop()
    crop = f"crop={width}:{height}:{x}:{y}"
    # The source is BT.709, limited range, and says so only partly; say it
    # all, or the conversion to RGB uses BT.601 and the blue shifts.
    to_rgb = "in_color_matrix=bt709:in_range=tv:out_range=pc:flags=lanczos+accurate_rnd+full_chroma_int"
    measured = loudness([])
    gain = round(RECIPE["target_lufs"] - measured, 2)
    NATIVE.mkdir(parents=True, exist_ok=True)
    WEB.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as temp:
        temp = Path(temp)
        native_width = RECIPE["native_width"]
        run("-loglevel", "error", "-i", str(SOURCE), "-map", "0:v:0",
            "-vf", f"{crop},scale={native_width}:-2:{to_rgb},format=rgb24",
            str(temp / "n%04d.png"))
        run("-loglevel", "error", "-i", str(SOURCE), "-map", "0:v:0",
            "-vf", f"{crop},scale={width}:{height}:{to_rgb},format=rgb24",
            "-update", "1", str(temp / "last.png"))
        names = sorted(temp.glob("n*.png"))
        frames = []
        for name in names:
            out = io.BytesIO()
            with Image.open(name) as image:
                size = image.size
                image.convert("RGB").save(out, "WEBP", quality=RECIPE["native_quality"], method=6)
            frames.append(out.getvalue())
        (NATIVE / "frames.bin").write_bytes(pack(frames, *size, (24, 1)))
        with Image.open(temp / "last.png") as image:
            image.convert("RGB").save(WEB / "intro-poster.webp", "WEBP", quality=RECIPE["poster_quality"], method=6)

    run("-loglevel", "error", "-y", "-i", str(SOURCE), "-map", "0:a:0", "-af", f"volume={gain}dB",
        "-c:a", "pcm_s16le", "-ar", "48000", "-ac", "2", "-map_metadata", "-1", "-fflags", "+bitexact",
        str(NATIVE / "sound.wav"))
    run("-loglevel", "error", "-y", "-i", str(SOURCE), "-map", "0:v:0", "-map", "0:a:0",
        "-vf", f"{crop},format=yuv420p",
        "-c:v", "libx264", "-preset", "slow", "-crf", str(RECIPE["web_crf"]), "-profile:v", "high",
        "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv",
        "-af", f"volume={gain}dB", "-c:a", "aac", "-b:a", RECIPE["web_audio"], "-ar", "48000",
        "-movflags", "+faststart", "-map_metadata", "-1", "-map_chapters", "-1",
        str(WEB / "intro.mp4"))

    outputs = {
        "frames": NATIVE / "frames.bin",
        "sound": NATIVE / "sound.wav",
        "video": WEB / "intro.mp4",
        "poster": WEB / "intro-poster.webp",
    }
    manifest = {
        "source": {"file": SOURCE.relative_to(ROOT).as_posix(), "sha256": sha256(SOURCE)},
        "recipe": RECIPE,
        "crop": [width, height, x, y],
        "frames": len(frames),
        "fps": [24, 1],
        "native_size": list(size),
        "source_lufs": measured,
        "gain_db": gain,
        "outputs": {key: {"file": path.relative_to(ROOT).as_posix(), "bytes": path.stat().st_size,
                          "sha256": sha256(path)} for key, path in outputs.items()},
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    for key, entry in manifest["outputs"].items():
        print(f"{key:7} {entry['file']:40} {entry['bytes'] / 1e6:6.2f} MB")
    print(f"crop {width}x{height}+{x}+{y}, {len(frames)} frames at {size[0]}x{size[1]}, "
          f"sound {measured} LUFS {gain:+} dB")


def check() -> list[str]:
    """Why the outputs are stale, or nothing when they are current."""
    problems = []
    try:
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        return [f"no readable manifest: {error}"]
    if not SOURCE.is_file() or sha256(SOURCE) != manifest["source"]["sha256"]:
        problems.append("the source changed")
    if manifest.get("recipe") != RECIPE:
        problems.append("the recipe changed")
    for key, entry in manifest["outputs"].items():
        path = ROOT / entry["file"]
        if not path.is_file():
            problems.append(f"{entry['file']} is missing")
        elif sha256(path) != entry["sha256"]:
            problems.append(f"{entry['file']} doesn't match the manifest")
    if problems:
        return problems

    blob = unpack((NATIVE / "frames.bin").read_bytes())
    if blob["count"] != manifest["frames"] or [blob["width"], blob["height"]] != manifest["native_size"]:
        problems.append("frames.bin disagrees with the manifest")
    if any(frame[:4] != b"RIFF" or frame[8:12] != b"WEBP" for frame in blob["frames"]):
        problems.append("frames.bin holds something that isn't WebP")
    with wave.open(str(NATIVE / "sound.wav")) as sound:
        if (sound.getnchannels(), sound.getsampwidth(), sound.getframerate()) != (2, 2, 48000):
            problems.append("sound.wav isn't 16-bit 48 kHz stereo")
        seconds = sound.getnframes() / 48000
        if abs(seconds - blob["count"] / 24) > 0.05:
            problems.append(f"sound.wav runs {seconds:.3f} s against {blob['count'] / 24:.3f} s of picture")
    atoms = top_level_atoms((WEB / "intro.mp4").read_bytes())
    if "moov" not in atoms or "mdat" not in atoms or atoms.index("moov") > atoms.index("mdat"):
        problems.append("intro.mp4 isn't faststart")
    poster = (WEB / "intro-poster.webp").read_bytes()
    if poster[:4] != b"RIFF" or poster[8:12] != b"WEBP":
        problems.append("intro-poster.webp isn't WebP")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true", help="verify the outputs are current; build nothing")
    args = parser.parse_args()
    if args.check:
        problems = check()
        for problem in problems:
            print(f"stale: {problem}")
        if not problems:
            print("splash outputs are current")
        return 1 if problems else 0
    build()
    return 0


if __name__ == "__main__":
    sys.exit(main())
