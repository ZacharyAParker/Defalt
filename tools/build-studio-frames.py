"""Build the booth's animation frames: mouths, eyelids and glances for both
hosts, every pose of the cat, and the pieces of the room that move (the city's
lit windows, the rain's window frame, the lightning, the sign, the steam).

Everything is derived from the booth's own art. The hosts' mouths and eyes
and the cat's poses are edits of tight crops of the base picture, accepted
into assets/studio-src/ (see `accept` below); the rest is cut from
background.png here. The output is one atlas and one manifest that both
booths read:

    web/static/studio-v2/atlas.png          lossless, embedded by the desktop app
    web/static/studio-v2/web/atlas.webp     the same atlas for the browser
    web/static/studio-v2/web/frames.json    where every frame sits, in both

Re-run after changing any source frame:

    .venv\\Scripts\\python.exe tools\\build-studio-frames.py

Accepting a new edit of a crop (an image the size of the crop, or a whole
multiple of it): it is scaled to the crop, aligned against the base by a small
translation search, checked -- nothing outside the part meant to change may
have moved or changed colour -- and saved as the source frame:

    .venv\\Scripts\\python.exe tools\\build-studio-frames.py accept face mav ah edit.png
    .venv\\Scripts\\python.exe tools\\build-studio-frames.py accept cat yawn-a edit.png
"""
from __future__ import annotations

import hashlib
import json
import math
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageStat

ROOT = Path(__file__).resolve().parent.parent
ART = ROOT / "web" / "static" / "studio-v2"
SRC = ROOT / "assets" / "studio-src"
NATIVE_ATLAS = ART / "atlas.png"
WEB_ATLAS = ART / "web" / "atlas.webp"
MANIFEST = ART / "web" / "frames.json"
W, H = 1728, 1152

HOSTS = {
    "mav": {"layer": "man.png", "at": (101, 183), "phones": ("headphones-mav.png", (383, 185))},
    "rue": {"layer": "woman.png", "at": (910, 213), "phones": ("headphones-rue.png", (1077, 216))},
}
# The crop each face edit is made on, in scene units, and inside it the parts
# that may change: the mouth and jaw, and the eyes.
FACE_CROP = {"mav": (480, 300, 260, 260), "rue": (1150, 320, 260, 260)}
FACE_AREAS = {
    "mav": {"mouth": (80, 135, 140, 110), "eyes": (50, 45, 175, 95)},
    "rue": {"mouth": (78, 132, 134, 108), "eyes": (55, 50, 185, 100)},
}
MOUTHS = ["slight", "ah", "wide", "oo", "ee"]
LIDS = ["half", "closed"]
LOOKS = {"mav": ["look-front", "look-down"], "rue": ["look-side", "look-down"]}

# The cat's crop: the sleeping sprite sits in it at CAT_SPRITE, on the shelf.
CAT_CROP = (20, 182, 312, 208)
CAT_SPRITE = (48, 248)
CAT_FRAMES = [
    "sleep-0", "sleep-1", "sleep-2", "sleep-3", "sleep-4", "sleep-5", "sleep-6", "sleep-7",
    "ear-flick", "ear-back", "tail-up", "tail-flick", "tail-down",
    "drowsy", "awake", "look", "yawn-a", "yawn-b", "squint",
    "groom-paw", "groom-lick", "groom-wipe", "stretch-a", "stretch-b", "perk",
]
# What stands in for a pose that has no accepted source frame yet.
CAT_FALLBACK = {
    "ear-back": "ear-flick", "tail-flick": "tail-up", "tail-down": "tail-up", "look": "awake",
    "yawn-a": "awake", "yawn-b": "yawn-a", "squint": "drowsy", "groom-paw": "awake",
    "groom-lick": "groom-paw", "groom-wipe": "groom-paw", "stretch-b": "stretch-a",
    "stretch-a": "awake", "perk": "drowsy", "awake": "drowsy",
}

PANES = [(463, 0, 218, 606), (722, 0, 387, 606)]
WINDOW = (455, 0, 663, 614)
SIGN = (1236, 72, 262, 132)
LAMP = (110, 185)
MUGS = [(505, 906), (1170, 944)]
Z_POS = (150, 262)


# ── helpers ──────────────────────────────────────────────────────────────

def load(name: str) -> Image.Image:
    return Image.open(ART / name).convert("RGBA")


def scene_background() -> Image.Image:
    return load("background.png").resize((W, H), Image.LANCZOS)


def scene_composite() -> Image.Image:
    """The booth as drawn, at rest, in scene units: what every edit starts from."""
    scene = scene_background()
    cat = load("sleeping-cat.png")
    scene.alpha_composite(cat, CAT_SPRITE)
    for host in HOSTS.values():
        scene.alpha_composite(load(host["layer"]), host["at"])
        phones, at = host["phones"]
        scene.alpha_composite(load(phones), at)
    for name, at in (("black-mug.png", (416, 892)), ("white-mug.png", (1095, 921))):
        scene.alpha_composite(load(name), at)
    scene.alpha_composite(load("microphones.png").resize((W, H), Image.LANCZOS))
    return scene


def crop(image: Image.Image, box) -> Image.Image:
    x, y, w, h = box
    return image.crop((x, y, x + w, y + h))


def fit(candidate: Image.Image, size) -> Image.Image:
    """An edit back at crop size. Whole multiples are box-filtered, which
    undoes a nearest-neighbour enlargement exactly."""
    candidate = candidate.convert("RGBA")
    if candidate.size == size:
        return candidate
    return candidate.resize(size, Image.BOX if candidate.width % size[0] == 0 else Image.LANCZOS)


def difference(a: Image.Image, b: Image.Image) -> Image.Image:
    """Per-pixel largest channel difference, as an L image."""
    diff = ImageChops.difference(a.convert("RGB"), b.convert("RGB"))
    r, g, bb = diff.split()
    return ImageChops.lighter(ImageChops.lighter(r, g), bb)


def mean(image: Image.Image, mask: Image.Image | None = None) -> float:
    """Average of an L image, over the pixels `mask` selects."""
    if mask is not None and not mask.getbbox():
        return 0.0
    return ImageStat.Stat(image, mask).mean[0]


def box_mask(size, box, inside=255) -> Image.Image:
    mask = Image.new("L", size, 255 - inside)
    x, y, w, h = box
    ImageDraw.Draw(mask).rectangle((x, y, x + w - 1, y + h - 1), fill=inside)
    return mask


def align(candidate: Image.Image, base: Image.Image, keep: Image.Image, reach: int = 4):
    """The small shift that best lines `candidate` up with `base` over the
    pixels in `keep` (the ones that are meant not to change)."""
    best = None
    for dy in range(-reach, reach + 1):
        for dx in range(-reach, reach + 1):
            shifted = ImageChops.offset(candidate, dx, dy)
            error = mean(difference(shifted, base), keep)
            if best is None or error < best[0]:
                best = (error, dx, dy, shifted)
    return best


def trim(patch: Image.Image, at):
    """Cut a patch down to its visible pixels; move `at` (x, y) with it."""
    bbox = patch.getchannel("A").point(lambda a: 255 if a > 2 else 0).getbbox()
    if not bbox:
        return None
    return patch.crop(bbox), (at[0] + bbox[0], at[1] + bbox[1])


# ── accept: an edit becomes a source frame ───────────────────────────────

def accept(kind: str, args: list[str]) -> None:
    scene = scene_composite()
    if kind == "face":
        host, name, path = args
        box = FACE_CROP[host]
        area = FACE_AREAS[host]["mouth" if name in MOUTHS else "eyes"]
        base = crop(scene, box)
        out = SRC / "faces" / f"{host}-{name}.png"
    elif kind == "cat":
        name, path = args
        box = CAT_CROP
        base = crop(scene, box)
        area = None
        out = SRC / "cat" / f"{name}.png"
    else:
        raise SystemExit(f"accept face|cat, not {kind}")
    candidate = fit(Image.open(path), base.size)
    if area is None:
        # The cat may move anywhere in its crop; line up on the wall above
        # and the shelf's far ends, which it never reaches.
        keep = Image.new("L", base.size, 0)
        draw = ImageDraw.Draw(keep)
        draw.rectangle((0, 0, base.width, 40), fill=255)
        draw.rectangle((base.width - 18, 0, base.width, base.height), fill=255)
    else:
        keep = box_mask(base.size, area, inside=0)
    error, dx, dy, aligned = align(candidate, base, keep)
    changed = mean(difference(aligned, base), ImageChops.invert(keep))
    print(f"{out.name}: shifted {dx:+d},{dy:+d}; outside the edit off by {error:.2f}/255, inside changed {changed:.1f}/255")
    if error > 4.0:
        raise SystemExit("rejected: the edit moved or recoloured what should have stayed put")
    # A face edit is judged over its own small area; a cat's over most of its crop.
    if changed < (0.5 if area else 0.15):
        raise SystemExit("rejected: nothing changed where it should have")
    out.parent.mkdir(parents=True, exist_ok=True)
    aligned.convert("RGB").save(out)


# ── patches from the accepted frames ─────────────────────────────────────

def face_patch(host: str, name: str, scene: Image.Image):
    """What a face frame adds over the base face: the changed pixels, with a
    soft edge, inside the part meant to change, never over the headphones."""
    path = SRC / "faces" / f"{host}-{name}.png"
    if not path.exists():
        print(f"  missing face frame {host}-{name}")
        return None
    box = FACE_CROP[host]
    base = crop(scene, box)
    frame = Image.open(path).convert("RGBA")
    area = FACE_AREAS[host]["mouth" if name in MOUTHS else "eyes"]
    diff = difference(frame, base)
    changed = diff.point(lambda d: 255 if d > 9 else 0)
    changed = ImageChops.multiply(changed, box_mask(base.size, area))
    # Close small holes, grow a little past the change, then soften.
    mask = changed.filter(ImageFilter.MaxFilter(5)).filter(ImageFilter.MinFilter(3))
    mask = mask.filter(ImageFilter.MaxFilter(3)).filter(ImageFilter.GaussianBlur(1.6))
    # Soften toward the edge of the allowed area too, so nothing ends in a line.
    edge = box_mask(base.size, (area[0] + 3, area[1] + 3, area[2] - 6, area[3] - 6)).filter(ImageFilter.GaussianBlur(2.5))
    mask = ImageChops.multiply(mask, edge)
    phones, at = HOSTS[host]["phones"]
    over = Image.new("L", (W, H), 0)
    over.paste(load(phones).getchannel("A"), at)
    over = crop(over, box).point(lambda a: 255 if a > 8 else 0).filter(ImageFilter.MaxFilter(3))
    mask = ImageChops.multiply(mask, ImageChops.invert(over))
    patch = frame.copy()
    patch.putalpha(mask)
    return trim(patch, box[:2])


def cat_frame(name: str, scene: Image.Image, backdrop: Image.Image, sprite: Image.Image):
    """A whole cat, cut from an accepted frame by what differs from the bare
    wall; where the frame matches the base exactly the sprite's own edge is kept."""
    path = SRC / "cat" / f"{name}.png"
    if not path.exists():
        return None
    frame = Image.open(path).convert("RGBA")
    base = crop(scene, CAT_CROP)
    wall = crop(backdrop, CAT_CROP)
    same = difference(frame, base).point(lambda d: 255 if d <= 3 else 0)
    cat = Image.new("RGBA", base.size, (0, 0, 0, 0))
    cat.alpha_composite(sprite, (CAT_SPRITE[0] - CAT_CROP[0], CAT_SPRITE[1] - CAT_CROP[1]))
    away = difference(frame, wall)
    alpha = away.point(lambda d: max(0, min(255, (d - 14) * 255 // 30)))
    alpha = alpha.filter(ImageFilter.MedianFilter(3))
    cut = unmix(frame, wall, alpha)
    # Keep the sprite exactly where the frame did not change.
    whole = Image.composite(cat, cut, same)
    return only_the_cat(whole)


def only_the_cat(image: Image.Image) -> Image.Image:
    """Drop specks the cut picked up away from the cat: anything not joined
    to the biggest shape (the cat) by visible pixels."""
    a = image.getchannel("A").load()
    w, h = image.size
    seen = [[False] * w for _ in range(h)]
    shapes = []
    for y0 in range(h):
        for x0 in range(w):
            if seen[y0][x0] or a[x0, y0] < 40:
                continue
            stack, shape = [(x0, y0)], []
            seen[y0][x0] = True
            while stack:
                x, y = stack.pop()
                shape.append((x, y))
                for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
                    if 0 <= nx < w and 0 <= ny < h and not seen[ny][nx] and a[nx, ny] >= 40:
                        seen[ny][nx] = True
                        stack.append((nx, ny))
            shapes.append(shape)
    if not shapes:
        return image
    keep = max(shapes, key=len)
    mask = Image.new("L", image.size, 0)
    m = mask.load()
    for x, y in keep:
        m[x, y] = 255
    # Let the faint edge around the kept shape stay.
    mask = mask.filter(ImageFilter.MaxFilter(3))
    out = image.copy()
    out.putalpha(ImageChops.multiply(image.getchannel("A"), mask))
    return out


def unmix(frame: Image.Image, wall: Image.Image, alpha: Image.Image) -> Image.Image:
    f, w, a = frame.load(), wall.load(), alpha.load()
    out = Image.new("RGBA", frame.size)
    o = out.load()
    for y in range(frame.height):
        for x in range(frame.width):
            k = a[x, y]
            if k == 0:
                o[x, y] = (0, 0, 0, 0)
                continue
            t = k / 255
            o[x, y] = tuple(max(0, min(255, round((f[x, y][c] - (1 - t) * w[x, y][c]) / t))) for c in range(3)) + (k,)
    return out


def breathing(sprite: Image.Image, depth: float) -> Image.Image:
    """The sleeping sprite with its back risen `depth` pixels: a smooth lift
    that is strongest over the middle of the body and nothing at the shelf.
    Worked at 4x and filtered back down, so every frame is a clean picture."""
    k = 4
    big = sprite.resize((sprite.width * k, sprite.height * k), Image.NEAREST)
    src = big.load()
    out = Image.new("RGBA", big.size, (0, 0, 0, 0))
    dst = out.load()
    w, h = big.size
    bottom = h - 1
    for x in range(w):
        u = x / w
        # Mostly the curled back (right two thirds), a little at the head.
        along = 0.25 + 0.75 * math.exp(-((u - 0.68) / 0.24) ** 2)
        lift = depth * k * along
        for y in range(h):
            # Nothing moves at the shelf; the top of the back moves most.
            v = y / bottom
            dy = lift * (1 - v) ** 0.8
            sy = y + dy
            y0 = int(sy)
            if y0 >= h - 1:
                dst[x, y] = src[x, h - 1]
                continue
            t = sy - y0
            a, b = src[x, y0], src[x, y0 + 1]
            dst[x, y] = tuple(round(a[c] * (1 - t) + b[c] * t) for c in range(4))
    return out.resize(sprite.size, Image.BOX)


# ── pieces of the room ───────────────────────────────────────────────────

def glass_mask(background: Image.Image) -> Image.Image:
    """Glass, in background pixels: inside the panes and not ivy."""
    k = background.width / W
    mask = Image.new("L", background.size, 0)
    draw = ImageDraw.Draw(mask)
    for x, y, w, h in PANES:
        draw.rectangle((round(x * k), round(y * k), round((x + w) * k) - 1, round((y + h) * k) - 1), fill=255)
    px = background.load()
    ivy = Image.new("L", background.size, 0)
    leaf = ivy.load()
    for y in range(round(340 * k)):
        for x in range(round(WINDOW[0] * k), round((WINDOW[0] + WINDOW[2]) * k)):
            sx = x / k
            if (sx < 575 and y / k < 270) or (sx > 1010 and y / k < 340):
                r, g, b = px[x, y][:3]
                if g > b + 6 and g > r - 4:
                    leaf[x, y] = 255
    # A leaf's dark edges and shadow count as leaf too.
    ivy = ivy.filter(ImageFilter.MaxFilter(5))
    return ImageChops.multiply(mask, ImageChops.invert(ivy))


def cut_background(background: Image.Image, box):
    """A piece of the background in its own pixels, and the scene area it covers."""
    k = background.width / W
    x, y, w, h = box
    left, top = math.floor(x * k), math.floor(y * k)
    right, bottom = math.ceil((x + w) * k), math.ceil((y + h) * k)
    piece = background.crop((left, top, right, bottom))
    return piece, (left / k, top / k, (right - left) / k, (bottom - top) / k)


def window_front(background: Image.Image, glass: Image.Image):
    piece, at = cut_background(background, WINDOW)
    k = background.width / W
    mask = glass.crop((round(at[0] * k), round(at[1] * k), round(at[0] * k) + piece.width, round(at[1] * k) + piece.height))
    front = piece.copy()
    front.putalpha(ImageChops.invert(mask.filter(ImageFilter.GaussianBlur(0.6))))
    return front, at


def flash(background: Image.Image, glass: Image.Image):
    """Where lightning shows: all of the glass a little, the sky most."""
    piece, at = cut_background(background, WINDOW)
    k = background.width / W
    mask = glass.crop((round(at[0] * k), round(at[1] * k), round(at[0] * k) + piece.width, round(at[1] * k) + piece.height))
    px, m = piece.load(), mask.load()
    out = Image.new("RGBA", piece.size, (0, 0, 0, 0))
    o = out.load()
    for y in range(piece.height):
        for x in range(piece.width):
            if not m[x, y]:
                continue
            r, g, b = px[x, y][:3]
            sky = max(0.0, min(1.0, (b - max(r, g) - 8) / 40)) * max(0.0, min(1.0, (r + g + b) / 3 / 70))
            lit = max(0.0, min(1.0, (r - b - 40) / 60))
            a = (0.18 + 0.82 * sky) * (1 - lit) * m[x, y] / 255
            o[x, y] = (214, 222, 255, round(255 * a))
    half = out.resize((piece.width // 2, piece.height // 2), Image.BOX)
    return half, at


def lit_windows(background: Image.Image, glass: Image.Image):
    """Every lit window in the city: warm, bright pixels on the glass,
    grouped. Each gets a patch of its own light (drawn added, to brighten it)
    and one with the light put out (drawn over it, to darken it)."""
    px, m = background.load(), glass.load()
    k = background.width / W
    x0, x1 = round(WINDOW[0] * k), round((WINDOW[0] + WINDOW[2]) * k)
    y1 = round(WINDOW[3] * k)
    hot = set()
    for y in range(y1):
        for x in range(x0, x1):
            if m[x, y] < 250:
                continue
            r, g, b = px[x, y][:3]
            if r > 150 and g > 85 and r - b > 75 and r + g > 300:
                hot.add((x, y))
    groups = []
    seen = set()
    for start in sorted(hot):
        if start in seen:
            continue
        stack, members = [start], []
        seen.add(start)
        while stack:
            x, y = stack.pop()
            members.append((x, y))
            for dx in (-2, -1, 0, 1, 2):
                for dy in (-2, -1, 0, 1, 2):
                    n = (x + dx, y + dy)
                    if n in hot and n not in seen:
                        seen.add(n)
                        stack.append(n)
        if len(members) >= 5:
            xs, ys = [p[0] for p in members], [p[1] for p in members]
            groups.append((min(xs), min(ys), max(xs) + 1, max(ys) + 1, len(members)))
    groups.sort(key=lambda g: -g[4])
    windows = []
    for left, top, right, bottom, _ in groups[:64]:
        left, top, right, bottom = left - 2, top - 2, right + 2, bottom + 2
        piece = background.crop((left, top, right, bottom))
        p = piece.load()
        lit = Image.new("RGBA", piece.size)
        dim = Image.new("RGBA", piece.size)
        lp, dp = lit.load(), dim.load()
        for y in range(piece.height):
            for x in range(piece.width):
                r, g, b = p[x, y][:3]
                amount = max(0.0, min(1.0, (r - b - 45) / 70)) * max(0.0, min(1.0, (r + g - 180) / 120))
                lp[x, y] = (r, g, b, round(255 * amount))
                # The same window unlit: dark glass with a hint of blue night.
                dp[x, y] = (round(r * 0.18 + 22), round(g * 0.18 + 20), round(b * 0.3 + 34), round(255 * amount))
        at = (left / k, top / k, piece.width / k, piece.height / k)
        windows.append({"lit": (lit, at), "dim": (dim, at)})
    return windows


def unlit_sign(background: Image.Image):
    """The sign with the power off, and its light on its own (for the glow).
    The same sums as the desktop app's `unlit`."""
    piece, at = cut_background(background, SIGN)
    off = piece.copy()
    glow = Image.new("RGBA", piece.size, (0, 0, 0, 0))
    p, o, g = piece.load(), off.load(), glow.load()
    wall, feather_px = 45.0, 18.0
    for y in range(piece.height):
        for x in range(piece.width):
            r, gg, b, a = p[x, y]
            edge = min(x, y, piece.width - 1 - x, piece.height - 1 - y)
            feather = max(0.0, min(1.0, edge / feather_px))
            light = max(0.0, r - max(gg, b) - wall)
            lit = max(0.0, min(1.0, light / 90)) * feather
            if lit <= 0:
                continue
            dim = 1 - 0.72 * lit
            rr = (r - light * 0.85 * feather) * dim
            o[x, y] = (round(max(0, min(255, rr))), round(gg * dim), round(b * dim), a)
            g[x, y] = (r, gg, b, round(255 * lit))
    return (off, at), (glow, at)


def z_glyph() -> Image.Image:
    """A soft, hand-drawn looking z: rounded strokes, a faint halo."""
    k, size = 4, 22
    big = Image.new("RGBA", (size * k, size * k), (0, 0, 0, 0))
    d = ImageDraw.Draw(big)
    pts = [(5, 6), (16, 5.5), (6, 16), (17, 15.5)]
    pts = [(x * k, y * k) for x, y in pts]
    # White: the booth tints it the snore colour as it draws it.
    colour = (255, 255, 255, 255)
    d.line(pts, fill=colour, width=int(2.4 * k), joint="curve")
    for x, y in (pts[0], pts[-1]):
        r = 1.2 * k
        d.ellipse((x - r, y - r, x + r, y + r), fill=colour)
    halo = big.getchannel("A").filter(ImageFilter.GaussianBlur(2.2 * k)).point(lambda a: a * 0.45)
    glow = Image.new("RGBA", big.size, (255, 255, 255, 0))
    glow.putalpha(halo)
    glow.alpha_composite(big)
    return glow.resize((size, size), Image.LANCZOS)


def soft_dot() -> Image.Image:
    """A round, soft-edged dot: steam, a lamp's glow, a bead of rain."""
    n = 64
    dot = Image.new("RGBA", (n, n), (255, 255, 255, 0))
    p = dot.load()
    for y in range(n):
        for x in range(n):
            r = math.hypot(x + 0.5 - n / 2, y + 0.5 - n / 2) / (n / 2)
            a = max(0.0, 1 - r) ** 1.6
            p[x, y] = (255, 255, 255, round(255 * a))
    return dot


# ── the atlas ────────────────────────────────────────────────────────────

def pack(images: list[Image.Image], width: int = 1024):
    """Shelf-pack images (tallest first) with clear pixels between them, so
    filtering at a frame's edge fades out instead of picking up a neighbour."""
    gap = 3
    order = sorted(range(len(images)), key=lambda i: -images[i].height)
    spots = [None] * len(images)
    x = y = shelf = 0
    for i in order:
        w, h = images[i].size
        if x + w + gap > width:
            x, y, shelf = 0, y + shelf + gap, 0
        spots[i] = (x + gap, y + gap)
        x += w + gap
        shelf = max(shelf, h)
    height = y + shelf + 2 * gap
    atlas = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    for image, spot in zip(images, spots):
        atlas.paste(image, spot)
    return atlas, spots


def build() -> None:
    scene = scene_composite()
    backdrop = scene_background()
    background = load("background.png")
    sprite = load("sleeping-cat.png")
    entries: list[tuple[str, Image.Image, tuple]] = []

    def add(key, image, at):
        entries.append((key, image, tuple(round(v, 3) for v in at)))

    faces = {}
    for host in HOSTS:
        names = MOUTHS + LIDS + LOOKS[host]
        for name in names:
            patch = face_patch(host, name, scene)
            if patch:
                image, (x, y) = patch
                add(f"face:{host}:{name}", image, (x, y, image.width, image.height))
        faces[host] = names

    cats = {}
    wall_cat = Image.new("RGBA", CAT_CROP[2:], (0, 0, 0, 0))
    wall_cat.alpha_composite(sprite, (CAT_SPRITE[0] - CAT_CROP[0], CAT_SPRITE[1] - CAT_CROP[1]))
    for i, name in enumerate(CAT_FRAMES):
        if name.startswith("sleep-"):
            depth = 1.7 * int(name[6:]) / 7
            frame = Image.new("RGBA", CAT_CROP[2:], (0, 0, 0, 0))
            frame.alpha_composite(breathing(sprite, depth), (CAT_SPRITE[0] - CAT_CROP[0], CAT_SPRITE[1] - CAT_CROP[1]))
            cats[name] = frame
        else:
            cats[name] = cat_frame(name, scene, backdrop, sprite)
    for name in CAT_FRAMES:
        seen = name
        while cats[seen] is None:
            seen = CAT_FALLBACK.get(seen, "sleep-0")
        if seen != name:
            print(f"  cat {name}: no source frame yet, standing in with {seen}")
            cats[name] = cats[seen]
    for name in CAT_FRAMES:
        patch = trim(cats[name], CAT_CROP[:2])
        image, (x, y) = patch
        add(f"cat:{name}", image, (x, y, image.width, image.height))

    glass = glass_mask(background)
    front, at = window_front(background, glass)
    add("window", front, at)
    lightning, at = flash(background, glass)
    add("flash", lightning, at)
    windows = lit_windows(background, glass)
    for i, window in enumerate(windows):
        for kind in ("lit", "dim"):
            image, at = window[kind]
            add(f"light:{i}:{kind}", image, at)
    (off, at), (glow, glow_at) = unlit_sign(background)
    add("sign-off", off, at)
    add("sign-glow", glow, glow_at)
    add("z", z_glyph(), (0, 0, 22, 22))
    add("soft", soft_dot(), (0, 0, 64, 64))

    # A pose standing in for another is the same picture: packed once.
    unique, index = [], {}
    keys = [(image.size, hashlib.sha1(image.tobytes()).hexdigest()) for _, image, _ in entries]
    for (_, image, _), key in zip(entries, keys):
        if key not in index:
            index[key] = len(unique)
            unique.append(image)
    atlas, placed = pack(unique)
    spots = [placed[index[key]] for key in keys]
    where = {key: (list(spot) + list(image.size), list(at)) for (key, image, at), spot in zip(entries, spots)}

    def item(key):
        src, at = where[key]
        return {"src": src, "at": at}

    manifest = {
        "atlas": list(atlas.size),
        "faces": {
            host: {
                "mouths": [item(f"face:{host}:{n}") if f"face:{host}:{n}" in where else None for n in MOUTHS],
                "lids": [item(f"face:{host}:{n}") if f"face:{host}:{n}" in where else None for n in LIDS],
                "looks": [item(f"face:{host}:{n}") if f"face:{host}:{n}" in where else None for n in LOOKS[host]],
            }
            for host in HOSTS
        },
        "cat": [dict(name=n, **item(f"cat:{n}")) for n in CAT_FRAMES],
        "lights": [{"lit": item(f"light:{i}:lit"), "dim": item(f"light:{i}:dim")} for i in range(len(windows))],
        "window": item("window"),
        "flash": item("flash"),
        "sign_off": item("sign-off"),
        "sign_glow": item("sign-glow"),
        "z": item("z"),
        "soft": item("soft"),
        "panes": [list(p) for p in PANES],
        "lamp": list(LAMP),
        "mugs": [list(m) for m in MUGS],
        "snore": list(Z_POS),
    }
    atlas.save(NATIVE_ATLAS, optimize=True)
    WEB_ATLAS.parent.mkdir(parents=True, exist_ok=True)
    atlas.save(WEB_ATLAS, "WEBP", quality=90, method=6, alpha_quality=100, exact=False)
    MANIFEST.write_text(json.dumps(manifest, separators=(",", ":")) + "\n", encoding="utf-8", newline="\n")
    print(f"{len(entries)} frames in a {atlas.width}x{atlas.height} atlas: "
          f"{NATIVE_ATLAS.stat().st_size / 1e3:.0f} kB png, {WEB_ATLAS.stat().st_size / 1e3:.0f} kB webp, "
          f"{len(windows)} lit windows")


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "accept":
        accept(sys.argv[2], sys.argv[3:])
    else:
        build()
