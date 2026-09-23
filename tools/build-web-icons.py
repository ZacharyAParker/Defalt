"""Build the home-screen app's icons from icons/icon.png.

    web/static/icons/apple-touch-icon.png   180x180, iOS home screen (opaque)
    web/static/icons/icon-192.png           192x192, manifest "any"
    web/static/icons/icon-512.png           512x512, manifest "any"
    web/static/icons/maskable-512.png       512x512, manifest "maskable": the
                                            art inside the 80% safe circle on
                                            the page colour, so a launcher's
                                            mask never crops the record

Re-run after changing icons/icon.png:

    .venv\\Scripts\\python.exe tools\\build-web-icons.py
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "icons" / "icon.png"
OUT = ROOT / "web" / "static" / "icons"
# --ink-050 in radio.css: the page, and the manifest's background colour.
GROUND = (0x10, 0x0E, 0x0C)
SAFE = 0.8


def square(size: int, art: Image.Image) -> Image.Image:
    return art.resize((size, size), Image.LANCZOS)


def maskable(size: int, art: Image.Image) -> Image.Image:
    canvas = Image.new("RGB", (size, size), GROUND)
    inner = int(size * SAFE)
    offset = (size - inner) // 2
    canvas.paste(art.resize((inner, inner), Image.LANCZOS), (offset, offset))
    return canvas


def main() -> None:
    art = Image.open(SOURCE).convert("RGB")
    OUT.mkdir(parents=True, exist_ok=True)
    outputs = {
        "apple-touch-icon.png": square(180, art),
        "icon-192.png": square(192, art),
        "icon-512.png": square(512, art),
        "maskable-512.png": maskable(512, art),
    }
    for name, image in outputs.items():
        image.save(OUT / name, optimize=True)
        print(f"wrote {OUT / name} ({image.size[0]}x{image.size[1]})")


if __name__ == "__main__":
    main()
