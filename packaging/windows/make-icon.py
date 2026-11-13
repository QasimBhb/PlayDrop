#!/usr/bin/env python3
"""Writes packaging/windows/playdrop.ico, the icon of the Windows programs and installer.

The shape is the one in apps/desktop/assets/playdrop.svg and apps/desktop/src/icon.rs: a
blue rounded square with a white drop. It is drawn here with the standard library only, so
the icon can be made again on any machine: python3 packaging/windows/make-icon.py

Sizes 16 to 64 are stored as 32-bit bitmaps (what every resource compiler accepts) and
256 as a PNG, as Windows expects for that size.
"""
import struct
import sys
import zlib
from pathlib import Path

SIZES = (16, 20, 24, 32, 40, 48, 64, 256)
BLUE = (47, 111, 237)  # #2f6fed
SAMPLES = 4  # per pixel edge


def in_background(x, y):
    r, s = 14.0, 64.0
    cx, cy = min(max(x, r), s - r), min(max(y, r), s - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_drop(x, y):
    cx, cy, r, tip = 32.0, 38.0, 14.0, 12.0
    dx, dy = x - cx, y - cy
    if dx * dx + dy * dy <= r * r:
        return True
    if tip <= y < cy:
        return abs(dx) <= r * (y - tip) / (cy - tip)
    return False


def render(size):
    """Rows of (r, g, b, a), top to bottom."""
    scale = 64.0 / size
    total = SAMPLES * SAMPLES
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            bg = fg = 0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    x = (px + (sx + 0.5) / SAMPLES) * scale
                    y = (py + (sy + 0.5) / SAMPLES) * scale
                    if in_background(x, y):
                        bg += 1
                        fg += in_drop(x, y)
            white = fg / bg if bg else 0.0
            r, g, b = (round(c + (255 - c) * white) for c in BLUE)
            row.append((r, g, b, round(255 * bg / total)))
        rows.append(row)
    return rows


def as_bitmap(rows):
    """An icon bitmap: header, BGRA rows bottom to top, then an empty 1-bit mask."""
    size = len(rows)
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b"".join(
        bytes(v for (r, g, b, a) in row for v in (b, g, r, a)) for row in reversed(rows)
    )
    mask_row = b"\x00" * (((size + 31) // 32) * 4)
    return header + pixels + mask_row * size


def as_png(rows):
    size = len(rows)

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\x00" + bytes(v for px in row for v in px) for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    out = Path(__file__).with_name("playdrop.ico")
    images = [(s, as_png(render(s)) if s >= 256 else as_bitmap(render(s))) for s in SIZES]
    offset = 6 + 16 * len(images)
    directory = struct.pack("<HHH", 0, 1, len(images))
    for size, data in images:
        edge = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", edge, edge, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    out.write_bytes(directory + b"".join(data for _, data in images))
    print(f"wrote {out} ({out.stat().st_size} bytes, sizes {', '.join(map(str, SIZES))})")


if __name__ == "__main__":
    sys.exit(main())
