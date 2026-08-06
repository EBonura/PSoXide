#!/usr/bin/env python3
"""Decode and diff SB4 capture-ring payloads.

Input is any text containing an `SB4/<base64>/C:<crc>` line: the emulator's
TTY log, or the decoded text of a photographed QR. With --baseline it
compares every segment against a previous capture and exits 1 naming each
field that moved, which is what lets `make hwtest-sb4` gate emulator drift
the way `make hwtest-diff` gates the conformance battery.

The run byte is excluded from comparison: it counts reruns within a boot,
not behaviour.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import pathlib
import struct
import sys
from dataclasses import dataclass

MAGIC = b"SB4B"
SEGMENT_LABELS = ("SQUARE", "IMPULSE", "ENVRAMP", "NOISE", "VOICE3")
# Bit 15 of the stat field marks a half-flag wait that timed out; the rest of
# that row describes nothing.
STAT_TIMEOUT = 0x8000


@dataclass(frozen=True)
class Segment:
    label: str
    stat: int
    envx: int
    first: int
    hash: int
    raw: tuple[int, ...]

    @property
    def timed_out(self) -> bool:
        return bool(self.stat & STAT_TIMEOUT)


@dataclass(frozen=True)
class Capture:
    schema: int
    run: int
    segments: tuple[Segment, ...]
    crc: int


def extract_payload(text: str) -> str:
    at = text.find("SB4/")
    if at < 0:
        raise ValueError("no SB4/ payload found")
    line = text[at:].splitlines()[0]
    return line


def parse(text: str) -> Capture:
    payload = extract_payload(text)
    body, claimed = payload.removeprefix("SB4/").rsplit("/C:", 1)
    raw = base64.b64decode(body)
    if raw[:4] != MAGIC:
        raise ValueError(f"bad magic {raw[:4]!r}")
    schema, segment_count, raw_count, run = raw[4], raw[5], raw[6], raw[7]
    if schema != 1:
        raise ValueError(f"unknown SB4 schema {schema}")
    crc = struct.unpack_from("<I", raw, len(raw) - 4)[0]
    actual = binascii.crc32(raw[:-4]) & 0xFFFF_FFFF
    if crc != actual:
        raise ValueError(f"binary CRC mismatch: payload {crc:08X}, calculated {actual:08X}")
    if int(claimed, 16) != crc:
        raise ValueError("text CRC disagrees with binary CRC")

    segment_bytes = 2 + 2 + 2 + 4 + raw_count * 2
    segments = []
    at = 8
    for index in range(segment_count):
        stat, envx, first = struct.unpack_from("<HHH", raw, at)
        digest = struct.unpack_from("<I", raw, at + 6)[0]
        samples = struct.unpack_from(f"<{raw_count}h", raw, at + 10)
        label = (
            SEGMENT_LABELS[index] if index < len(SEGMENT_LABELS) else f"SEG{index:02X}"
        )
        segments.append(Segment(label, stat, envx, first, digest, samples))
        at += segment_bytes
    return Capture(schema, run, tuple(segments), crc)


def report(capture: Capture) -> None:
    print(f"# schema=SB4v{capture.schema} run={capture.run:02X} crc={capture.crc:08X}")
    print("segment,stat,envx,first,hash")
    for seg in capture.segments:
        note = " TIMEOUT" if seg.timed_out else ""
        print(f"{seg.label},{seg.stat:04X},{seg.envx:04X},{seg.first},{seg.hash:08X}{note}")
    for seg in capture.segments:
        print(f"# {seg.label} raw: {' '.join(str(s) for s in seg.raw)}")


def diff(current: Capture, baseline: Capture) -> int:
    drift = 0
    pairs = zip(current.segments, baseline.segments)
    if len(current.segments) != len(baseline.segments):
        print(
            f"DRIFT segment count: {len(current.segments)} vs baseline "
            f"{len(baseline.segments)}"
        )
        drift += 1
    for seg, base in pairs:
        for field in ("stat", "envx", "first", "hash", "raw"):
            now, was = getattr(seg, field), getattr(base, field)
            if now != was:
                if field in ("hash",):
                    print(f"DRIFT {seg.label}.{field}: {now:08X} was {was:08X}")
                elif field == "raw":
                    moved = [i for i, (a, b) in enumerate(zip(now, was)) if a != b]
                    print(f"DRIFT {seg.label}.raw: {len(moved)} sample(s), first at {moved[0]}")
                else:
                    print(f"DRIFT {seg.label}.{field}: {now:04X} was {was:04X}")
                drift += 1
    print(f"# drift={drift}")
    return 1 if drift else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", help="text containing an SB4/ payload line")
    parser.add_argument("--baseline", help="previous capture to compare against")
    parser.add_argument(
        "--fail-on-change",
        action="store_true",
        help="exit 1 if any segment field differs from the baseline",
    )
    args = parser.parse_args()

    current = parse(pathlib.Path(args.capture).read_text(encoding="utf-8"))
    report(current)
    if not args.baseline:
        return 0
    baseline = parse(pathlib.Path(args.baseline).read_text(encoding="utf-8"))
    result = diff(current, baseline)
    return result if args.fail_on_change else 0


if __name__ == "__main__":
    sys.exit(main())
