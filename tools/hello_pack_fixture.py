#!/usr/bin/env python3
"""Generate the hello-pack WORLD.PAK fixture rooms.

Emits 86 room_<id>.psxc chunks (ids 0..=85) so the pack's chunk table spans
two sectors: entry 84 straddles the sector boundary (28 + 84*24 = 2044,
crossing 2048), which makes the guest's stitched entry parse run against
real CD reads instead of only host tests.

Two chunks carry content the guest verifies; ids 0..=83 are 16-byte filler
that only exists to push the table across the boundary:

- room_84.psxc: 3000 bytes of an avalanche-hash pattern. Incompressible,
  so --world-pack-compress-rooms stores it RAW (2 sectors + partial tail).
- room_85.psxc: 5000 bytes of 64-byte runs. Compresses hard, so it is stored
  HLZC/LZ4-framed and exercises load_chunk_decompressed.

The two pattern functions are duplicated in
sdk/examples/hello-pack/src/main.rs (raw_byte / comp_byte); keep them in
sync byte for byte or the guest's DATA checks go red.
"""

import pathlib
import sys

RAW_ID = 84
RAW_LEN = 3000
COMP_ID = 85
COMP_LEN = 5000
CHUNKS = 86


def raw_byte(k: int) -> int:
    """Murmur3-finalizer avalanche of the index: statistically random
    bytes, so LZ4 cannot shrink the chunk and it stays stored raw.
    (A plain multiplicative hash is NOT enough: its high bits form a
    near-arithmetic sequence and 3000 bytes of it compress to ~1.3k.)"""
    h = (k + RAW_ID * 2654435761) & 0xFFFFFFFF
    h ^= h >> 16
    h = (h * 0x85EBCA6B) & 0xFFFFFFFF
    h ^= h >> 13
    h = (h * 0xC2B2AE35) & 0xFFFFFFFF
    h ^= h >> 16
    return h & 0xFF


def comp_byte(k: int) -> int:
    """64-byte runs: compresses to a small fraction of COMP_LEN."""
    return ((k >> 6) + COMP_ID) & 0xFF


def main() -> int:
    out = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "build/hello-pack-fixture")
    out.mkdir(parents=True, exist_ok=True)
    for i in range(CHUNKS):
        if i == RAW_ID:
            body = bytes(raw_byte(k) for k in range(RAW_LEN))
        elif i == COMP_ID:
            body = bytes(comp_byte(k) for k in range(COMP_LEN))
        else:
            body = bytes([i & 0xFF]) * 16
        (out / f"room_{i}.psxc").write_bytes(body)
    print(f"hello-pack fixture: {CHUNKS} chunks in {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
