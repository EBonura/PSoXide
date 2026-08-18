#!/usr/bin/env python3
"""Per-frame motion profile of a COOKED .psxanim, in cooked frame numbers.

Blender-side measurements (tools/anim_validate.py) describe the source. The
game plays the cook: resampled to 15 Hz, quantised, and indexed by frame
number. Timing constants like "the strike is at frame 27" have to come from
this side or they are guesses about a different signal.

    python3 tools/psxanim_profile.py editor/projects/default/assets/animations/gen/light_attack.psxanim

Prints, per frame, how far each joint moved (model-local units) and the peak
joint, then names the fastest frame. `--joint N` profiles one joint (13 is
Aletha's sword hand).
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

ASSET_HEADER = 12
ANIM_HEADER = 8


def decode_q11(code: int) -> int:
    signed = ((code << 4) & 0xFFFF) - (0x10000 if (code << 4) & 0x8000 else 0)
    signed >>= 4
    return signed * 2 + (2 if (code & 0x0FFF) == 0x07FF else 0)


def load(path: Path):
    data = path.read_bytes()
    magic, version, _flags, _payload = struct.unpack_from("<4sHHI", data, 0)
    if magic != b"PSXA":
        raise SystemExit(f"not a psxanim: {path}")
    joints, frames, rate, shift = struct.unpack_from("<HHHH", data, ASSET_HEADER)
    record = {1: 30, 2: 24, 3: 20}[version]
    base = ASSET_HEADER + ANIM_HEADER
    poses = []
    for frame in range(frames):
        row = []
        for joint in range(joints):
            off = base + (frame * joints + joint) * record
            if version == 3:
                block = data[off : off + 14]
                packed = [
                    (block[p * 3] | (block[p * 3 + 1] << 8) | (block[p * 3 + 2] << 16))
                    for p in range(4)
                ]
                rot = []
                for value in packed:
                    rot.append(decode_q11(value & 0x0FFF))
                    rot.append(decode_q11((value >> 12) & 0x0FFF))
                rot.append(decode_q11((block[12] | (block[13] << 8)) & 0x0FFF))
                tx, ty, tz = struct.unpack_from("<3h", data, off + 14)
            elif version == 2:
                rot = list(struct.unpack_from("<9h", data, off))
                tx, ty, tz = struct.unpack_from("<3h", data, off + 18)
            else:
                rot = list(struct.unpack_from("<9h", data, off))
                tx, ty, tz = struct.unpack_from("<3i", data, off + 18)
            row.append((rot, (tx << shift, ty << shift, tz << shift)))
        poses.append(row)
    return version, joints, frames, rate, poses


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("clip", type=Path)
    parser.add_argument("--joint", type=int, help="Profile one joint (13 = sword hand)")
    parser.add_argument("--quiet", action="store_true", help="Only the summary line")
    args = parser.parse_args()

    version, joints, frames, rate, poses = load(args.clip.expanduser())
    print(
        f"{args.clip.name}: v{version} joints={joints} frames={frames} "
        f"rate={rate}Hz duration={frames / max(rate, 1):.2f}s"
    )

    speeds = []
    for index in range(frames - 1):
        best = (0.0, -1)
        total = 0.0
        for joint in range(joints):
            a = poses[index][joint][1]
            b = poses[index + 1][joint][1]
            step = sum((y - x) ** 2 for x, y in zip(a, b)) ** 0.5
            total += step
            if step > best[0]:
                best = (step, joint)
        tracked = None
        if args.joint is not None:
            a = poses[index][args.joint][1]
            b = poses[index + 1][args.joint][1]
            tracked = sum((y - x) ** 2 for x, y in zip(a, b)) ** 0.5
        speeds.append((index, total, best, tracked))

    if not args.quiet:
        for index, total, best, tracked in speeds:
            bar = "#" * int(total / max(max(s[1] for s in speeds), 1e-6) * 40)
            extra = f" joint{args.joint}={tracked:7.1f}" if tracked is not None else ""
            print(f"  frame {index:3}->{index + 1:<3} sum={total:8.1f} peak=j{best[1]:<3}{extra} {bar}")

    peak = max(speeds, key=lambda s: s[1])
    print(
        f"fastest frame {peak[0]}->{peak[0] + 1} (fraction {peak[0] / max(frames - 1, 1):.3f}) "
        f"driven by joint {peak[2][1]}"
    )
    if args.joint is not None:
        jpeak = max(speeds, key=lambda s: s[3])
        print(
            f"joint {args.joint} peak at frame {jpeak[0]}->{jpeak[0] + 1} "
            f"(fraction {jpeak[0] / max(frames - 1, 1):.3f})"
        )


main()
