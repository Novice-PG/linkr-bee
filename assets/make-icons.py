#!/usr/bin/env python3
"""Regenerate every derived Linkr Bee icon from the one master.

`assets/linkr-bee-icon.svg` is the master artwork: a green hexagonal terminal
mark. Everything else — the Windows `.ico`, the iOS and Android app icons and
splashes, the HarmonyOS app icons, the Android vector foreground and the web
favicon — is the same drawing at a size and on a ground each platform asks
for, and this script is what makes that true. Run it from the repository root:

    python3 assets/make-icons.py

It needs Pillow and numpy (Pillow for the tiles and the files, numpy for the
stroke rasteriser). The rasteriser draws the SVG's own geometry with an
analytic 1-pixel edge, so the small sizes stay sharp without a super-sampled
downscale. Nothing here reads the previous icons: the sizes and positions are
the ones the old artwork used, written down as constants so a re-run is a
reproduction rather than a redraw.

The icon-only SVG at the repository root (`assets/linkr-bee-mark.svg`) and the
logo files are not touched: the mark keeps the same artwork, and the logo is a
logo.
"""

import os
import re
import struct
import sys
from io import BytesIO

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MASTER = os.path.join(ROOT, "assets", "linkr-bee-icon.svg")

# The white rounded tile the launchers draw the mark on, as the old artwork
# had it: tile width and corner radius as fractions of the canvas, and the
# soft shadow under it.
TILE_W = 0.7917
TILE_R = 0.0526
ROUND_W = 0.9167
SHADOW_ALPHA = 60
SHADOW_BLUR = 0.012
SHADOW_DROP = 0.005

# How much of a canvas each family gives the mark, as the old artwork did:
# height of the mark's own bounding box over the canvas height.
APPICON_ART = 0.57  # iOS app icon, opaque white ground
LAUNCHER_ART = 0.4375  # Android's legacy rounded tile
ROUND_ART = 0.51  # Android's legacy round tile
FOREGROUND_ART = 0.60  # Android's adaptive foreground (inside the safe zone)
ICO_ART = 0.78  # Windows .ico frames, transparent
HARMONY_ART = 0.78  # HarmonyOS tile, white ground
FAVICON_ART = 0.78  # the web favicon, 24-unit canvas

# The Android splash screens carry the mark at the height the old ones did;
# the key is the file, so a re-run reproduces the set exactly.
ANDROID_SPLASH_ART = {
    "drawable/splash.png": 64,
    "drawable-port-mdpi/splash.png": 64,
    "drawable-port-hdpi/splash.png": 96,
    "drawable-port-xhdpi/splash.png": 96,
    "drawable-port-xxhdpi/splash.png": 128,
    "drawable-port-xxxhdpi/splash.png": 128,
    "drawable-land-mdpi/splash.png": 64,
    "drawable-land-hdpi/splash.png": 96,
    "drawable-land-xhdpi/splash.png": 96,
    "drawable-land-xxhdpi/splash.png": 128,
    "drawable-land-xxxhdpi/splash.png": 128,
}
IOS_SPLASH_ART = 160  # 2732×2732, the size the old mark had
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)


def parse_master():
    """The strokes of the master: points in its 200×200 space, half widths."""
    text = open(MASTER, encoding="utf-8").read()
    color = re.search(r'stroke="(#[0-9A-Fa-f]{6})"', text).group(1)
    rgb = tuple(int(color[i : i + 2], 16) for i in (1, 3, 5))

    def points(raw):
        nums = [float(v) for v in re.findall(r"-?\d+(?:\.\d+)?", raw)]
        return list(zip(nums[0::2], nums[1::2]))

    shapes = []
    for tag, closed in (("polygon", True), ("polyline", False)):
        m = re.search(rf"<{tag}\b([^>]*)>", text, re.S)
        body = m.group(1)
        pts = points(re.search(r'points="([^"]+)"', body).group(1))
        width = float(re.search(r'stroke-width="([\d.]+)"', body).group(1))
        shapes.append((pts, width / 2, closed))
    m = re.search(r"<line\b([^>]*)>", text, re.S)
    body = m.group(1)

    def number(name):
        return float(re.search(rf'\b{name}="(-?[\d.]+)"', body).group(1))

    width = number("stroke-width")
    shapes.append(([(number("x1"), number("y1")), (number("x2"), number("y2"))], width / 2, False))

    xs, ys = [], []
    for pts, r, _ in shapes:
        xs += [x - r for x, _ in pts] + [x + r for x, _ in pts]
        ys += [y - r for _, y in pts] + [y + r for _, y in pts]
    return shapes, rgb, (min(xs), min(ys), max(xs), max(ys))


SHAPES, COLOR, ART = parse_master()
ART_W, ART_H = ART[2] - ART[0], ART[3] - ART[1]


def render_mark(height):
    """The mark alone, transparent, its own box `height` pixels tall."""
    scale = height / ART_H
    size = int(np.ceil(ART_W * scale)) + 4
    tall = int(np.ceil(height)) + 4
    ys, xs = np.mgrid[0:tall, 0:size]
    px, py = xs + 0.5, ys + 0.5
    cx = (ART[0] + ART[2]) / 2
    cy = (ART[1] + ART[3]) / 2
    coverage = np.zeros((tall, size), np.float32)
    for pts, half, closed in SHAPES:
        pixel = [((x - cx) * scale + size / 2, (y - cy) * scale + tall / 2) for x, y in pts]
        edges = list(zip(pixel, pixel[1:]))
        if closed:
            edges.append((pixel[-1], pixel[0]))
        for (ax, ay), (bx, by) in edges:
            vx, vy = bx - ax, by - ay
            wx, wy = px - ax, py - ay
            length = vx * vx + vy * vy
            t = np.clip((wx * vx + wy * vy) / length, 0, 1) if length else 0
            dx, dy = wx - t * vx, wy - t * vy
            dist = np.sqrt(dx * dx + dy * dy)
            coverage = np.maximum(coverage, np.clip(0.5 + (half * scale - dist), 0, 1))
    out = np.zeros((tall, size, 4), np.uint8)
    out[..., 0], out[..., 1], out[..., 2] = (*COLOR,)
    out[..., 3] = (coverage * 255 + 0.5).astype(np.uint8)
    return Image.fromarray(out, "RGBA")


def on_ground(size, art_height, ground, mode="RGBA"):
    """The mark centered on a canvas, `ground` a fill or `None` (transparent)."""
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0) if ground is None else ground)
    mark = render_mark(art_height)
    canvas.alpha_composite(mark, ((size - mark.width) // 2, (size - mark.height) // 2))
    return canvas.convert(mode)


def on_tile(size, art_height, round_tile=False):
    """The Android legacy tile: a white shape with the old soft shadow."""
    tile_w = round(size * (ROUND_W if round_tile else TILE_W))
    radius = tile_w // 2 if round_tile else max(1, round(tile_w * TILE_R))
    off = (size - tile_w) // 2
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    drop = max(1, round(size * SHADOW_DROP))
    ImageDraw.Draw(shadow).rounded_rectangle(
        [off, off + drop, off + tile_w, off + tile_w + drop],
        radius=radius,
        fill=(16, 23, 34, SHADOW_ALPHA),
    )
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(max(1, size * SHADOW_BLUR))))
    tile = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    ImageDraw.Draw(tile).rounded_rectangle(
        [off, off, off + tile_w, off + tile_w], radius=radius, fill=(255, 255, 255, 255)
    )
    canvas.alpha_composite(tile)
    mark = render_mark(art_height)
    canvas.alpha_composite(mark, ((size - mark.width) // 2, (size - mark.height) // 2))
    return canvas


def write_ico(path):
    frames = [on_ground(size, size * ICO_ART, None) for size in ICO_SIZES]
    pngs = []
    for frame in frames:
        buffer = BytesIO()
        frame.save(buffer, "PNG")
        pngs.append(buffer.getvalue())
    header = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    entries = b""
    for frame, png in zip(frames, pngs):
        side = 0 if frame.width == 256 else frame.width
        entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
    with open(path, "wb") as handle:
        handle.write(header + entries + b"".join(pngs))


def transform(size, art_height):
    """The master's coordinates as a transform into a `size` canvas."""
    scale = art_height / ART_H
    return f"translate({size / 2:g} {size / 2:g}) scale({scale:.6g}) translate({-((ART[0]+ART[2])/2):g} {-((ART[1]+ART[3])/2):g})"


def svg_from_master(size, art_height, ground):
    """An SVG of the master's own geometry, scaled onto a square canvas."""
    parts = []
    if ground:
        parts.append(f'  <rect width="{size}" height="{size}" rx="{round(size*112/512)}" fill="{ground}"/>')
    parts.append(f'  <g transform="{transform(size, art_height)}" fill="none" '
                 f'stroke="{"#%02X%02X%02X" % COLOR}" stroke-linecap="round" stroke-linejoin="round">')
    for pts, half, closed in SHAPES:
        coords = " ".join(f"{x:g},{y:g}" for x, y in pts)
        if closed:
            parts.append(f'    <polygon points="{coords}" stroke-width="{half*2:g}"/>')
        elif len(pts) == 2:
            (x1, y1), (x2, y2) = pts
            parts.append(f'    <line x1="{x1:g}" y1="{y1:g}" x2="{x2:g}" y2="{y2:g}" stroke-width="{half*2:g}"/>')
        else:
            parts.append(f'    <polyline points="{coords}" stroke-width="{half*2:g}"/>')
    parts.append("  </g>")
    return '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d">\n%s\n</svg>\n' % (
        size, size, "\n".join(parts))


def vector_drawable(size=108, art_height=108 * FOREGROUND_ART):
    """The Android vector foreground: the same strokes as path data."""
    scale = art_height / ART_H
    cx, cy = (ART[0] + ART[2]) / 2, (ART[1] + ART[3]) / 2

    def at(p):
        return ((p[0] - cx) * scale + size / 2, (p[1] - cy) * scale + size / 2)

    paths = []
    for pts, half, closed in SHAPES:
        pixel = [at(p) for p in pts]
        if closed:
            data = "M" + " L".join(f"{x:.3f},{y:.3f}" for x, y in pixel) + " Z"
        else:
            data = "M" + " L".join(f"{x:.3f},{y:.3f}" for x, y in pixel)
        paths.append(
            f'    <path\n        android:pathData="{data}"\n'
            f'        android:strokeColor="#{"%02X%02X%02X" % COLOR}"\n'
            f'        android:strokeWidth="{half*2*scale:.3f}"\n'
            f'        android:strokeLineCap="round"\n'
            f'        android:strokeLineJoin="round"\n'
            f'        android:fillColor="#00000000" />'
        )
    return (
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        f'    android:width="{size}dp"\n    android:height="{size}dp"\n'
        f'    android:viewportWidth="{size}"\n    android:viewportHeight="{size}">\n'
        + "\n".join(paths)
        + "\n</vector>\n"
    )


def favicon_href():
    """The web favicon as a data URI: a white tile and the mark, 24 units."""
    scale = (24 * FAVICON_ART) / ART_H
    cx, cy = (ART[0] + ART[2]) / 2, (ART[1] + ART[3]) / 2
    colour = "%23" + ("%02X%02X%02X" % COLOR)

    def at(p):
        return ((p[0] - cx) * scale + 12, (p[1] - cy) * scale + 12)

    body = []
    for pts, half, closed in SHAPES:
        pixel = [at(p) for p in pts]
        coords = " ".join(f"{x:.3f},{y:.3f}" for x, y in pixel)
        width = f"{half * 2 * scale:.3f}"
        if closed:
            body.append(f"%3Cpolygon points='{coords}' stroke-width='{width}'/%3E")
        elif len(pts) == 2:
            (x1, y1), (x2, y2) = pixel
            body.append(f"%3Cline x1='{x1:.3f}' y1='{y1:.3f}' x2='{x2:.3f}' y2='{y2:.3f}' stroke-width='{width}'/%3E")
        else:
            body.append(f"%3Cpolyline points='{coords}' stroke-width='{width}'/%3E")
    svg = (
        "%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'%3E"
        "%3Crect width='24' height='24' rx='5' fill='%23FFFFFF'/%3E"
        f"%3Cg fill='none' stroke='{colour}' stroke-linecap='round' stroke-linejoin='round'%3E"
        + "".join(body)
        + "%3C/g%3E%3C/svg%3E"
    )
    return "data:image/svg+xml," + svg


def write(path, text, mode="w"):
    with open(path, mode, encoding=None if "b" in mode else "utf-8") as handle:
        handle.write(text)
    print(path)


def main():
    cwd = os.getcwd()
    if os.path.abspath(cwd) != ROOT:
        sys.exit(f"run me from the repository root ({ROOT}), not {cwd}")

    base = os.path.join(ROOT, "mobile", "android", "app", "src", "main", "res")
    ios = os.path.join(ROOT, "mobile", "ios", "App", "App", "Assets.xcassets")

    write_ico(os.path.join(ROOT, "tools", "linkr-tui", "assets", "icon.ico"))

    appicon = os.path.join(ios, "AppIcon.appiconset", "AppIcon-512@2x.png")
    on_ground(1024, 1024 * APPICON_ART, (255, 255, 255, 255), "RGB").save(appicon)
    print(appicon)

    for suffix in ("", "-1", "-2"):
        splash = os.path.join(ios, "Splash.imageset", f"splash-2732x2732{suffix}.png")
        mode = Image.open(splash).mode
        on_ground(2732, IOS_SPLASH_ART, (255, 255, 255, 255), mode).save(splash)
        print(splash)

    for path, art in ANDROID_SPLASH_ART.items():
        splash = os.path.join(base, path)
        with Image.open(splash) as image:
            mode, size = image.mode, image.size
        canvas = Image.new("RGBA", size, (255, 255, 255, 255))
        mark = render_mark(art)
        canvas.alpha_composite(mark, ((size[0] - mark.width) // 2, (size[1] - mark.height) // 2))
        canvas.convert(mode).save(splash)
        print(splash)

    for density, side in (("mdpi", 48), ("hdpi", 72), ("xhdpi", 96), ("xxhdpi", 144), ("xxxhdpi", 192)):
        folder = os.path.join(base, f"mipmap-{density}")
        on_tile(side, side * LAUNCHER_ART).save(os.path.join(folder, "ic_launcher.png"))
        on_tile(side, side * ROUND_ART, round_tile=True).save(os.path.join(folder, "ic_launcher_round.png"))
        on_ground(side, side * FOREGROUND_ART, None).save(os.path.join(folder, "ic_launcher_foreground.png"))
        print(folder)

    write(os.path.join(base, "drawable-v24", "ic_launcher_foreground.xml"), vector_drawable())

    for path in ("harmonyos/AppScope/resources/base/media/app_icon.svg",
                 "harmonyos/entry/src/main/resources/base/media/app_icon.svg"):
        write(os.path.join(ROOT, path), svg_from_master(512, 512 * HARMONY_ART, "#FFFFFF"))

    index = os.path.join(ROOT, "web", "index.html")
    html = open(index, encoding="utf-8").read()
    href = favicon_href()
    new, count = re.subn(r'href="data:image/svg\+xml,[^"]*"', f'href="{href}"', html)
    if count != 1:
        sys.exit(f"expected exactly one inline favicon in web/index.html, found {count}")
    write(index, new)


if __name__ == "__main__":
    main()
