#!/usr/bin/env python3
"""Draws the Spatial SoftReceiver mark and writes the app icons.

The mark is geometry, not a traced picture: a point (the sound the receiver
takes in) at the foot of two circles that grow from it (the space it spreads
the sound into). Both circles touch at the bottom, so the mark reads as one
thing widening upward, and at the 16 px tray size it is still a ring with a
smaller ring and a light dot at its foot.

It is drawn on a 1024-unit square, rendered at four times that and reduced,
so every size is antialiased. `apps/spatial-softreceiver/ui/public/mark.svg`
is the same mark written by hand with the same numbers; change both together.

Usage: python3 tools/ui-icons.py   (needs Pillow)
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "apps" / "spatial-softreceiver" / "icons"

UNITS = 1024
SUPERSAMPLE = 4

TILE = (17, 23, 30, 255)  # #11171e
RING = (59, 201, 219, 255)  # #3bc9db
POINT = (244, 247, 250, 255)  # #f4f7fa

TILE_RADIUS = 208
STROKE = 72
# (center x, center y, radius of the stroke's center line)
OUTER = (512, 512, 368)
INNER = (512, 680, 200)
DOT = (512, 760, 64)


def ring(draw: ImageDraw.ImageDraw, scale: int, circle: tuple[int, int, int]) -> None:
    """A circle stroked STROKE wide, centered on its radius like SVG does."""
    cx, cy, r = circle
    edge = r + STROKE / 2
    box = ((cx - edge) * scale, (cy - edge) * scale, (cx + edge) * scale, (cy + edge) * scale)
    draw.ellipse(box, outline=RING, width=STROKE * scale)


def master() -> Image.Image:
    """The mark at UNITS pixels."""
    scale = SUPERSAMPLE
    size = UNITS * scale
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((0, 0, size - 1, size - 1), radius=TILE_RADIUS * scale, fill=TILE)
    ring(draw, scale, OUTER)
    ring(draw, scale, INNER)
    cx, cy, r = DOT
    draw.ellipse(((cx - r) * scale, (cy - r) * scale, (cx + r) * scale, (cy + r) * scale), fill=POINT)
    return image.resize((UNITS, UNITS), Image.Resampling.LANCZOS)


def sized(image: Image.Image, pixels: int) -> Image.Image:
    return image.resize((pixels, pixels), Image.Resampling.LANCZOS)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    mark = master()
    for name, pixels in (
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("icon.png", 512),
    ):
        sized(mark, pixels).save(OUT / name, optimize=True)
        print(f"ui-icons: {name} {pixels}px")
    ico_sizes = (16, 32, 48, 256)
    images = [sized(mark, pixels) for pixels in ico_sizes]
    images[-1].save(
        OUT / "icon.ico",
        format="ICO",
        sizes=[(pixels, pixels) for pixels in ico_sizes],
        append_images=images[:-1],
    )
    print(f"ui-icons: icon.ico {', '.join(str(p) for p in ico_sizes)}px")


if __name__ == "__main__":
    main()
