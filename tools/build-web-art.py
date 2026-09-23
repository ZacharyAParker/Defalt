"""Build the browser's studio art from web/static/studio-v2.

The desktop console embeds the PNG layers in studio-v2 as they are (see
src/ui/studio.rs), so those stay untouched. The browser only ever draws small
parts of some of them -- two mouths out of a full-frame photo, four eyes out of
another, a cat out of a mostly empty canvas -- and was downloading about 10 MB
to do it. This cuts each piece out once, at roughly twice the size it is drawn
on screen, and writes everything as WebP beside the originals:

    web/static/studio-v2/web/*.webp
    web/static/studio-v2/web/sprites.json   what was cut from where, for the tests

The rectangles below are in the scene's 1728x1152 canvas space and must match
the layout in web/static/studio-scene.js (tests/test_browser_studio.js checks
that they do). Re-run after changing either:

    .venv\\Scripts\\python.exe tools\\build-web-art.py
"""
from __future__ import annotations

import json
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "web" / "static" / "studio-v2"
OUT = SOURCE / "web"
CANVAS = (1728, 1152)

# Whole layers: converted, not cropped. They are drawn at (or near) their own
# resolution across a large part of the scene.
LAYERS = {
    "background": "background.png",
    "microphones": "microphones.png",
    "mav": "man.png",
    "rue": "woman.png",
    "mav-phones": "headphones-mav.png",
    "rue-phones": "headphones-rue.png",
    "black-mug": "black-mug.png",
    "white-mug": "white-mug.png",
}

# Face patches, cut from full-frame art that is scaled to the canvas: the
# rectangle is where the patch is drawn, in canvas space, and is also where it
# lives in the source once the source is scaled to the canvas.
FACES = {
    "mav-mouth": ("speaking.jpg", (585, 463, 89, 51)),
    "rue-mouth": ("speaking.jpg", (1252, 477, 84, 55)),
    "mav-eye-0": ("blink.png", (550, 374, 64, 40)),
    "mav-eye-1": ("blink.png", (634, 372, 48, 41)),
    "rue-eye-0": ("blink.png", (1224, 391, 71, 47)),
    "rue-eye-1": ("blink.png", (1323, 405, 54, 48)),
}

# Cat poses: a crop of the source image (source pixels), drawn into CAT_RECT.
CAT_RECT = (49, 251, 264, 137)
CATS = {
    "cat-sleep": ("sleeping-cat.png", (1, 3, 264, 137)),
    "cat-awake": ("cat-awake.png", (58, 33, 1635, 848)),
    "cat-yawn": ("cat-yawn.png", (19, 10, 1678, 888)),
    "cat-groom": ("cat-groom.png", (40, 14, 1641, 878)),
}

# The scene is shown at about half its canvas size (a 1728-wide canvas in a
# column well under 900 CSS pixels), so canvas-space pixels are already about
# twice the drawn size. Cats get double that: their source is much sharper.
FACE_SCALE = 1
CAT_SCALE = 2


def save(image: Image.Image, name: str, quality: int) -> dict:
    OUT.mkdir(parents=True, exist_ok=True)
    path = OUT / f"{name}.webp"
    options = {"quality": quality, "method": 6}
    if image.mode == "RGBA":
        # Transparent pixels keep whatever colour they had; zeroing them lets
        # the encoder spend nothing on them.
        options["exact"] = False
        options["alpha_quality"] = 95
    image.save(path, "WEBP", **options)
    return {"file": path.name, "size": list(image.size), "bytes": path.stat().st_size}


def main() -> None:
    manifest: dict[str, dict] = {}
    for name, filename in LAYERS.items():
        image = Image.open(SOURCE / filename)
        image = image.convert("RGBA" if image.mode in ("RGBA", "LA", "P") else "RGB")
        manifest[name] = {**save(image, name, 84 if image.mode == "RGB" else 88), "from": filename}

    for name, (filename, rect) in FACES.items():
        image = Image.open(SOURCE / filename).convert("RGB")
        sx, sy = image.width / CANVAS[0], image.height / CANVAS[1]
        x, y, w, h = rect
        box = (round(x * sx), round(y * sy), round((x + w) * sx), round((y + h) * sy))
        size = (round(w * FACE_SCALE), round(h * FACE_SCALE))
        sprite = image.crop(box).resize(size, Image.LANCZOS)
        manifest[name] = {**save(sprite, name, 90), "from": filename, "rect": list(rect)}

    for name, (filename, crop) in CATS.items():
        image = Image.open(SOURCE / filename).convert("RGBA")
        x, y, w, h = crop
        size = (CAT_RECT[2] * CAT_SCALE, CAT_RECT[3] * CAT_SCALE)
        sprite = image.crop((x, y, x + w, y + h)).resize(size, Image.LANCZOS)
        manifest[name] = {**save(sprite, name, 90), "from": filename, "rect": list(CAT_RECT)}

    (OUT / "sprites.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    before = sum((SOURCE / f).stat().st_size for f in {v["from"] for v in manifest.values()})
    after = sum(v["bytes"] for v in manifest.values())
    print(f"{len(manifest)} sprites: {before / 1e6:.1f} MB of source art -> {after / 1e6:.2f} MB")


if __name__ == "__main__":
    main()
