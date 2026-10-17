#!/usr/bin/env python3
"""Draws the Spatial SoftReceiver mark and writes the app icons.

The mark is geometry, not a traced picture: three level bars (the sound the
receiver takes in) between two chamfered brackets (the space around the
listener). The tall bar is in the middle, the two short ones beside it, and
each bracket is a vertical stroke whose ends bend in at a slant and run
level toward the bars. At the 16 px tray size it still reads as a pair of
brackets around a tall bar.

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
MARK = (64, 209, 228, 255)  # #40d1e4

TILE_RADIUS = 144
STROKE = 82
# The left bracket's center line, top to bottom; the right one is its mirror.
# It runs level from END_X to the slant, slants out to the vertical stroke,
# and comes back the same way below.
BRACKET = ((285, 272), (180, 352), (180, 672), (285, 752))
END_X = 375  # where each level end stops, its outer corners rounded
END_RADIUS = 26
# (center x, width, height); every bar is centered on y = 512 with round ends
BARS = ((378, 80, 168), (512, 92, 320), (646, 80, 168))


def bracket(draw: ImageDraw.ImageDraw, scale: int) -> None:
    """The left bracket: the stroked center line plus two rounded ends."""
    (_, top_y), *_, (_, bottom_y) = BRACKET
    stub = END_X - END_RADIUS
    points = [(stub, top_y), *BRACKET, (stub, bottom_y)]
    draw.line([(x * scale, y * scale) for x, y in points], fill=MARK, width=STROKE * scale, joint="curve")
    half = STROKE / 2
    for y in (top_y, bottom_y):
        box = (END_X - 2 * END_RADIUS, y - half, END_X, y + half)
        draw.rounded_rectangle([v * scale for v in box], radius=END_RADIUS * scale, fill=MARK)


def master() -> Image.Image:
    """The mark at UNITS pixels."""
    scale = SUPERSAMPLE
    size = UNITS * scale
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((0, 0, size - 1, size - 1), radius=TILE_RADIUS * scale, fill=TILE)

    # Both brackets go on a layer of their own so the mirror copies only them.
    layer = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    bracket(ImageDraw.Draw(layer), scale)
    layer.alpha_composite(layer.transpose(Image.Transpose.FLIP_LEFT_RIGHT))
    image.alpha_composite(layer)

    for cx, width, height in BARS:
        box = (cx - width / 2, UNITS / 2 - height / 2, cx + width / 2, UNITS / 2 + height / 2)
        draw.rounded_rectangle([v * scale for v in box], radius=width / 2 * scale, fill=MARK)
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
