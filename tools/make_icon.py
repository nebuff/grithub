#!/usr/bin/env python3
"""Draw the grithub app icon and write it as a multi-size Windows .ico.

The mark is a three-column kanban board: columns of stacked cards at three gray
levels on paper white, so it still reads as a board at 16 px. It is drawn as
pixels and box-filtered down, so the build has no font or image-library
dependency.
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

SIZE = 256
MARGIN = 26
COLUMNS = 3
GAP = 18
COLUMN_W = (SIZE - 2 * MARGIN - (COLUMNS - 1) * GAP) // COLUMNS
TOP = 34
CARD_GAP = 12
CARD_RADIUS = 5
RADIUS = 8

PAPER = (255, 255, 255)
# Dark to light: the board's three columns read as one shape at small sizes.
INK = [
    (32, 32, 32),
    (96, 96, 96),
    (158, 158, 158),
]
# How many cards each column holds; uneven counts are what make it a board.
CARDS = [3, 2, 4]


def rounded_rect(image: list[list[tuple[int, int, int]]], x0: int, y0: int, w: int, h: int,
                 radius: int, color: tuple[int, int, int]) -> None:
    for y in range(h):
        for x in range(w):
            if radius <= x < w - radius or radius <= y < h - radius:
                inside = True
            else:
                cx = radius if x < radius else w - 1 - radius
                cy = radius if y < radius else h - 1 - radius
                inside = (x - cx) ** 2 + (y - cy) ** 2 <= radius**2
            if inside:
                image[y0 + y][x0 + x] = color


def render() -> list[list[tuple[int, int, int]]]:
    image = [[PAPER] * SIZE for _ in range(SIZE)]
    for col in range(COLUMNS):
        x0 = MARGIN + col * (COLUMN_W + GAP)
        count = CARDS[col]
        card_h = (SIZE - TOP - MARGIN - (count - 1) * CARD_GAP) // count
        for card in range(count):
            y0 = TOP + card * (card_h + CARD_GAP)
            rounded_rect(image, x0, y0, COLUMN_W, card_h, CARD_RADIUS, INK[col])
    return image


def downscale(image: list[list[tuple[int, int, int]]], size: int) -> list[list[tuple[int, int, int]]]:
    scale = SIZE // size
    out: list[list[tuple[int, int, int]]] = []
    for y in range(size):
        row = []
        for x in range(size):
            r = g = b = 0
            for sy in range(scale):
                for sx in range(scale):
                    pr, pg, pb = image[y * scale + sy][x * scale + sx]
                    r += pr
                    g += pg
                    b += pb
            count = scale * scale
            row.append((r // count, g // count, b // count))
        out.append(row)
    return out


def ico_entry(image: list[list[tuple[int, int, int]]], size: int) -> bytes:
    pixels = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            r, g, b = image[y][x]
            pixels.extend((b, g, r, 255))
    mask_stride = ((size + 31) // 32) * 4
    mask = bytes(mask_stride * size)
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0)
    return header + pixels + mask


def build() -> bytes:
    master = render()
    entries = []
    payload_offset = 6 + 16 * 5
    for size in (16, 32, 48, 64, 128):
        data = ico_entry(downscale(master, size), size)
        entries.append((size, len(data), payload_offset))
        payload_offset += len(data)
    out = bytearray(struct.pack("<HHH", 0, 1, len(entries)))
    for size, length, offset in entries:
        out.extend(struct.pack("<BBBBHHII", size, size, 0, 0, 1, 32, length, offset))
    for size in (16, 32, 48, 64, 128):
        out.extend(ico_entry(downscale(master, size), size))
    return bytes(out)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="icon.ico")
    args = parser.parse_args()
    data = build()
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(data)
    print(f"{out}: {len(data)} bytes, sizes 16 32 48 64 128")


if __name__ == "__main__":
    main()
