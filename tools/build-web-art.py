"""Build the browser's studio art from web/static/studio-v2.

The desktop console embeds the PNG layers in studio-v2 as they are (see
src/ui/studio.rs), so those stay untouched. This converts each layer the
browser draws to WebP beside the originals (the animation frames are a
separate atlas; run tools/build-studio-frames.py first):

    web/static/studio-v2/web/*.webp
    web/static/studio-v2/web/sprites.json   what was cut from where, for the tests

The layers must match the layout in web/static/studio-scene.js
(tests/test_browser_studio.js checks that they do). Re-run after changing either:

    .venv\\Scripts\\python.exe tools\\build-web-art.py
"""
from __future__ import annotations

import json
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "web" / "static" / "studio-v2"
OUT = SOURCE / "web"

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

    # Everything that moves (mouths, eyes, the cat, the city's lights, the
    # sign) is one atlas, built by tools/build-studio-frames.py; listed here
    # so the tests see every file the booth loads.
    atlas = Image.open(OUT / "atlas.webp")
    manifest["atlas"] = {"file": "atlas.webp", "size": list(atlas.size), "bytes": (OUT / "atlas.webp").stat().st_size,
                         "from": "atlas.png"}

    (OUT / "sprites.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    before = sum((SOURCE / f).stat().st_size for f in {v["from"] for v in manifest.values()})
    after = sum(v["bytes"] for v in manifest.values())
    print(f"{len(manifest)} sprites: {before / 1e6:.1f} MB of source art -> {after / 1e6:.2f} MB")


if __name__ == "__main__":
    main()
