#!/usr/bin/env python3
"""Derive the palette-compatible clawless Rust Mantis PSMD.

The canonical Mantis was authored against a 256x256 four-bank 4bpp atlas.
Re-importing its GLB asks the generic cooker to build a new 128x128 palette
layout, so merely pointing that model at the canonical atlas changes colours.

This derivation keeps the canonical PSMD byte layout, UVs, face palette-bank
selectors, quantisation scale, and animation precision intact. It disables
only the render part rigidly owned by the requested hand joint. The original
vertices/faces remain in the payload as an intentional precision/layout
anchor, but no runtime part submits them.

Usage::

    python3 tools/derive_clawless_psx_model.py \
      assets/models/rust_mantis/rust_mantis.psxmdl \
      assets/models/rust_mantis_clawless/rust_mantis_clawless.psxmdl \
      --joint 9
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path


ASSET_HEADER_SIZE = 12
MODEL_HEADER_SIZE = 16
JOINT_RECORD_SIZE = 4
MATERIAL_RECORD_SIZE = 8
PART_RECORD_SIZE = 16


def derive(source: Path, output: Path, joint: int) -> None:
    data = bytearray(source.read_bytes())
    if data[:4] != b"PSMD":
        raise ValueError(f"{source}: not a PSMD model")
    if len(data) < ASSET_HEADER_SIZE + MODEL_HEADER_SIZE:
        raise ValueError(f"{source}: truncated model header")

    joint_count, part_count, _vertex_count, _face_count, material_count = (
        struct.unpack_from("<5H", data, ASSET_HEADER_SIZE)
    )
    if not 0 <= joint < joint_count:
        raise ValueError(f"joint {joint} is outside the model's {joint_count} joints")
    part_start = (
        ASSET_HEADER_SIZE
        + MODEL_HEADER_SIZE
        + joint_count * JOINT_RECORD_SIZE
        + material_count * MATERIAL_RECORD_SIZE
    )
    part_end = part_start + part_count * PART_RECORD_SIZE
    if part_end > len(data):
        raise ValueError(f"{source}: truncated part table")

    hidden_parts = 0
    hidden_vertices = 0
    hidden_faces = 0
    for index in range(part_count):
        offset = part_start + index * PART_RECORD_SIZE
        part_joint, _first_vertex, vertex_count, _first_face, face_count = (
            struct.unpack_from("<5H", data, offset)
        )
        if part_joint != joint:
            continue
        hidden_parts += 1
        hidden_vertices += vertex_count
        hidden_faces += face_count
        # Keep the table ranges and all following payload offsets stable. A
        # zero count is the format's ordinary representation of an empty part.
        struct.pack_into("<H", data, offset + 4, 0)
        struct.pack_into("<H", data, offset + 8, 0)

    if hidden_parts == 0:
        raise ValueError(f"{source}: no render part is owned by joint {joint}")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(data)
    print(
        f"joint {joint}: hid {hidden_parts} part(s), {hidden_vertices} vertices, "
        f"{hidden_faces} faces; preserved {len(data)}-byte PSMD layout -> {output}"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--joint", type=int, required=True)
    args = parser.parse_args()
    derive(args.source.resolve(), args.output.resolve(), args.joint)


if __name__ == "__main__":
    main()
