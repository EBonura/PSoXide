#!/usr/bin/env python3
"""Cook the authored 32x32 grayscale HUD swap dial into a 4bpp PSXT.

The source PNG is deliberately kept beside the cooked asset so the tiny
runtime texture remains reproducible.  Its alpha channel selects transparent
texels; opaque luminance is quantised into fifteen neutral levels.  The GPU
then applies the active Horizon/Zenith colour through material tinting.

Run:
    python3 tools/cook_vitality_hud_dial.py \
        engine/examples/editor-playtest/assets/vitality_hud_dial_generated_32.png \
        engine/examples/editor-playtest/assets/vitality_hud_dial_32.psxt
"""

from pathlib import Path
import struct
import sys

from PIL import Image


SIZE = 32


def rgb555(grey: int) -> int:
    channel = min(31, max(0, grey >> 3))
    return channel | (channel << 5) | (channel << 10)


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {Path(sys.argv[0]).name} SOURCE.png OUTPUT.psxt", file=sys.stderr)
        return 2

    source = Image.open(sys.argv[1]).convert("RGBA")
    if source.size != (SIZE, SIZE):
        raise ValueError(f"HUD dial must be {SIZE}x{SIZE}, got {source.size}")

    indices: list[int] = []
    for red, green, blue, alpha in source.getdata():
        if alpha < 128:
            indices.append(0)
            continue
        luminance = (red * 54 + green * 183 + blue * 19) >> 8
        indices.append(1 + ((luminance * 14 + 127) // 255))

    pixels = bytearray()
    for offset in range(0, len(indices), 2):
        pixels.append(indices[offset] | (indices[offset + 1] << 4))

    # Index zero is transparent.  Index one starts above RGB555 zero so the
    # darkest authored ring details remain visible instead of disappearing.
    palette = [0]
    palette.extend(rgb555(8 + ((level - 1) * 247 + 7) // 14) for level in range(1, 16))
    palette_bytes = b"".join(struct.pack("<H", entry) for entry in palette)

    payload = struct.pack(
        "<BBHHHII", 4, 0, SIZE, SIZE, 16, len(pixels), len(palette_bytes)
    )
    payload += bytes(pixels) + palette_bytes
    blob = b"PSXT" + struct.pack("<HHI", 1, 1, len(payload)) + payload
    Path(sys.argv[2]).write_bytes(blob)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
