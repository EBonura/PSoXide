#!/usr/bin/env python3
"""Convert a 128x128 PNG into a PS1 4bpp texture module.

Promoted from pico8-psx (where it cooks the launcher's cart-label covers)
because voxide's boot splash was generated with a lost local variant; this
is now the one copy. Two modes:

Default (PICO-8): quantise each pixel to the nearest of the 16 PICO-8
palette colours and pack 4bpp (low nibble = leftmost pixel), matching the
in-game spritesheets so the consumer uploads it with the shared PICO8_CLUT.

--own-palette: derive the palette from the image itself (error if it has
more than 16 distinct colours after the resize), and additionally emit
`<NAME>_CLUT: [u16; 16]` in PS1 RGB555. Index 0 keeps colour 0x0000
(transparent black); any OTHER pure-black entry is emitted as 0x8000
(opaque black, STP bit set) so black pixels don't vanish -- the classic
PS1 palette gotcha.

Usage:
    python3 tools/png_to_cover.py [--own-palette] <in.png> <out.rs> <ARRAY_NAME>
"""

import os
import sys

PICO8_RGB = [
    (0, 0, 0), (29, 43, 83), (126, 37, 83), (0, 135, 81),
    (171, 82, 54), (95, 87, 79), (194, 195, 199), (255, 241, 232),
    (255, 0, 77), (255, 163, 0), (255, 236, 39), (0, 228, 54),
    (41, 173, 255), (131, 118, 156), (255, 119, 168), (255, 204, 170),
]


def nearest(palette, r, g, b):
    best, bi = 1 << 30, 0
    for i, (pr, pg, pb) in enumerate(palette):
        d = (r - pr) ** 2 + (g - pg) ** 2 + (b - pb) ** 2
        if d < best:
            best, bi = d, i
    return bi


def rgb555(r, g, b, index):
    word = (r >> 3) | ((g >> 3) << 5) | ((b >> 3) << 10)
    if word == 0 and index != 0:
        word = 0x8000  # opaque black; index 0 stays transparent black
    return word


def main():
    args = sys.argv[1:]
    own_palette = "--own-palette" in args
    if own_palette:
        args.remove("--own-palette")
    if len(args) != 3:
        sys.exit(__doc__)
    png, out, name = args
    from PIL import Image
    im = Image.open(png).convert("RGB").resize((128, 128))
    px = im.load()

    if own_palette:
        palette = []
        for y in range(128):
            for x in range(128):
                c = px[x, y]
                if c not in palette:
                    palette.append(c)
        if len(palette) > 16:
            sys.exit(f"{png}: {len(palette)} distinct colours; "
                     "--own-palette needs at most 16")
        # Keep black (if present) at index 0 so it stays the transparent slot.
        palette.sort(key=lambda c: (c != (0, 0, 0), c))
        palette += [(0, 0, 0)] * (16 - len(palette))
    else:
        palette = PICO8_RGB

    words = []
    for y in range(128):
        for x in range(0, 128, 4):
            p = [nearest(palette, *px[x + k, y]) for k in range(4)]
            words.append(p[0] | (p[1] << 4) | (p[2] << 8) | (p[3] << 12))

    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    with open(out, "w") as f:
        f.write(f"// Auto-generated from {os.path.basename(png)} by "
                "tools/png_to_cover.py. Do not edit by hand.\n\n")
        if own_palette:
            f.write("// 128x128 @ 4bpp (32 halfwords/row), palette below.\n")
        else:
            f.write("// 128x128 @ 4bpp (32 halfwords/row), PICO8_CLUT palette.\n")
        f.write(f"pub static {name}: [u16; {len(words)}] = [\n")
        for i in range(0, len(words), 32):
            f.write("    " + ", ".join(f"0x{w:04X}" for w in words[i:i + 32]) + ",\n")
        f.write("];\n")
        if own_palette:
            clut = [rgb555(r, g, b, i) for i, (r, g, b) in enumerate(palette)]
            f.write(f"\n/// RGB555 palette for `{name}` "
                    "(index 0 transparent black, other blacks opaque).\n")
            f.write(f"pub static {name}_CLUT: [u16; 16] = [\n    ")
            f.write(", ".join(f"0x{w:04X}" for w in clut))
            f.write(",\n];\n")
    print(f"Wrote {name} ({len(words)} halfwords) to {out}")


if __name__ == "__main__":
    main()
