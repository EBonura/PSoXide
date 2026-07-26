#!/usr/bin/env python3
"""Generate a deterministic raw CD-DA track for the hardware-test disc.

The disc needs a real audio track to measure read-while-CD-DA contention, which
is the one CD failure no emulator reproduces. The content is synthesised here
rather than taken from any recording, matching how the PA2-PA5 probes use
synthetic sound: the disc must be redistributable and bit-reproducible.

A steady tone is deliberate. Contention shows up two ways at once: as timing in
the payload, and audibly as a dropout or pitch glitch in the same capture-card
recording that carries the QR pages.

Output is raw 16-bit stereo little-endian at 44.1 kHz, padded to a whole number
of 2352-byte CD sectors.

    python3 tools/gen-cdda-tone.py --seconds 10 --out track.pcm
"""

from __future__ import annotations

import argparse
import math
import pathlib
import struct

SAMPLE_RATE = 44100
BYTES_PER_SECTOR = 2352
BYTES_PER_FRAME = 4  # 16-bit stereo


def generate(seconds: float, left_hz: float, right_hz: float, amplitude: float) -> bytes:
    frames = int(SAMPLE_RATE * seconds)
    peak = int(32767 * amplitude)
    out = bytearray()
    for index in range(frames):
        phase = 2.0 * math.pi * index / SAMPLE_RATE
        left = int(peak * math.sin(phase * left_hz))
        right = int(peak * math.sin(phase * right_hz))
        out += struct.pack("<hh", left, right)
    # Pad to a whole sector: the burner and the drive both work in sectors, and
    # a partial one would either be dropped or read as garbage.
    remainder = len(out) % BYTES_PER_SECTOR
    if remainder:
        out += b"\x00" * (BYTES_PER_SECTOR - remainder)
    return bytes(out)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=float, default=10.0)
    # Distinct per channel so a captured recording proves stereo routing and
    # channel order, not just that something played.
    parser.add_argument("--left-hz", type=float, default=440.0)
    parser.add_argument("--right-hz", type=float, default=660.0)
    parser.add_argument("--amplitude", type=float, default=0.5)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    data = generate(args.seconds, args.left_hz, args.right_hz, args.amplitude)
    pathlib.Path(args.out).write_bytes(data)
    print(
        f"wrote {args.out}: {len(data)} bytes, "
        f"{len(data) // BYTES_PER_SECTOR} sectors, "
        f"{len(data) // BYTES_PER_FRAME / SAMPLE_RATE:.2f}s "
        f"L={args.left_hz:g}Hz R={args.right_hz:g}Hz"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
