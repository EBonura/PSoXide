#!/usr/bin/env python3
"""Author the 64x64 4bpp actor floor-shadow decal (`.psxt`).

Run: python3 tools/gen_shadow_decal.py engine/examples/editor-playtest/assets/shadow_circle_64.psxt

The decal is drawn with BlendMode::Average, so the GPU computes
`(background + texel) / 2`. Two consequences drive the whole design:

*   A texel of pure black halves whatever the floor was, which is a shadow
    that works on every floor brightness and can never clip to a flat hole.
    That is why the darkening comes from a BLACK texel and not from a grey
    one, and why the mode is Average rather than Subtract.
*   The falloff cannot come from the colour alone. Raising the texel towards
    grey raises the result towards `(B + F) / 2`, which BRIGHTENS any floor
    darker than F. The rim ramp is therefore capped well below Cortex's
    darkest floors, and the last of the falloff is carried by ordered-dither
    transparency instead.

Every non-zero CLUT entry sets bit 15 (STP). On a semi-transparent primitive
the PS1 blends a texel only when its STP bit is set; an STP-clear texel is
painted OPAQUE. The shipped decal had STP clear on all 16 entries, so its
"shadow" was a patch of opaque near-black grey rather than a blend. Index 0
stays 0x0000, the one value the GPU treats as fully transparent.
"""

import struct
import sys

SIZE = 64
RADIUS = 31.5
# Where the solid core ends and the ramp begins, as a fraction of the radius.
CORE = 0.55
# Highest grey the rim ramp reaches, 0..255. Kept below the level's darkest
# floors so `(B + F) / 2` never comes out brighter than B.
RIM_GREY = 24
# Ordered 4x4 Bayer matrix, used for the outer transparency falloff.
BAYER = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
]


def rgb555(r: int, g: int, b: int, stp: bool) -> int:
    v = (r >> 3) | ((g >> 3) << 5) | ((b >> 3) << 10)
    return v | (0x8000 if stp else 0)


def clut() -> list[int]:
    """Index 0 transparent; 1..15 an STP-set black-to-dark-grey ramp."""
    entries = [0]
    for k in range(1, 16):
        grey = round((k - 1) / 14 * RIM_GREY)
        entries.append(rgb555(grey, grey, grey, stp=True))
    return entries


def index_at(x: int, y: int) -> int:
    dx = (x + 0.5) - SIZE / 2
    dy = (y + 0.5) - SIZE / 2
    d = (dx * dx + dy * dy) ** 0.5 / RADIUS
    if d >= 1.0:
        return 0
    if d <= CORE:
        return 1  # pure black: the maximum Average darkening, B/2
    # t walks 0..1 across the ramp band.
    t = (d - CORE) / (1.0 - CORE)
    # Colour carries the first 3/4 of the falloff...
    index = 1 + round(t * 14)
    # ...and ordered dither dissolves the outer edge into transparency, so the
    # silhouette has no hard ring at PS1 resolution.
    if t > 0.6:
        coverage = (1.0 - t) / 0.4  # 1 -> 0 across the outer band
        if coverage * 16.0 <= BAYER[y & 3][x & 3]:
            return 0
    return min(15, index)


def main() -> int:
    out = sys.argv[1]
    rows = []
    for y in range(SIZE):
        row = bytearray()
        for x in range(0, SIZE, 2):
            lo = index_at(x, y)
            hi = index_at(x + 1, y)
            row.append(lo | (hi << 4))
        rows.append(bytes(row))
    pixels = b"".join(rows)
    palette = b"".join(struct.pack("<H", e) for e in clut())

    payload = struct.pack("<BBHHHII", 4, 0, SIZE, SIZE, 16, len(pixels), len(palette))
    payload += pixels + palette
    # AssetHeader: magic, version, flags, payload_len. INDEX_ZERO_TRANSPARENT
    # (bit 0) states outright that entry 0 is the transparent slot.
    blob = b"PSXT" + struct.pack("<HHI", 1, 1, len(payload)) + payload
    with open(out, "wb") as fh:
        fh.write(blob)

    used = sorted({index_at(x, y) for y in range(SIZE) for x in range(SIZE)})
    print(f"wrote {out}: {len(blob)} bytes, indices used {used}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
