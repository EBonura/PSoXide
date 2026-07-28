#!/usr/bin/env python3
"""Audit the hardware-test disc's measured instruction blocks in the LINKED EXE.

Every timing probe brackets its measured interval with harmless marker words
(`sll`/`addu` forms that write no live register). Those markers exist so the
final PS-X machine code can be audited rather than trusted: a timing number
only means what the docs claim if the instructions between the markers are
still the ones the source asked for.

This walks the linked EXE, extracts the word span between each probe's start
and end marker, and digests it. Pin the output with --baseline and any change
to a measured block -- an LLVM version bump reordering a wrapper, an edit that
accidentally lands inside the timed window -- shows up as a moved digest
instead of as a silently different cycle count.

    python3 tools/verify-hwtest-machine-code.py <exe> [--baseline f] [--fail-on-change]

Marker words are NOT unique in the binary (they are legal instruction
encodings LLVM also emits), so spans are matched as ordered start/end pairs
rather than by scanning for single words.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import struct
import sys

PSX_EXE_HEADER_BYTES = 0x800
# A measured block is tens of instructions; 512 words is slack, not a real bound.
MAX_SPAN_WORDS = 512
SOURCE = "engine/examples/hardware-tests/src/main.rs"

# One asm! block per probe. Capture the function name, then the first word
# commented as a start marker and the first commented as an end marker.
BLOCK_RE = re.compile(
    r"fn (?P<name>\w+)\([^)]*\)[^{]*\{(?P<body>.*?)\n\}", re.DOTALL
)
START_RE = re.compile(r'"\.word (0x[0-9A-Fa-f]+)",\s*//[^\n]*start marker')
END_RE = re.compile(r'"\.word (0x[0-9A-Fa-f]+)",\s*//[^\n]*end marker')


class ProbeError(Exception):
    pass


def probes_from_source(path: pathlib.Path) -> list[tuple[str, int, int]]:
    text = path.read_text(encoding="utf-8")
    found: list[tuple[str, int, int]] = []
    for block in BLOCK_RE.finditer(text):
        body = block.group("body")
        start = START_RE.search(body)
        end = END_RE.search(body)
        if not start:
            continue
        if not end:
            raise ProbeError(
                f"{block.group('name')}: start marker {start.group(1)} has no end marker"
            )
        found.append((block.group("name"), int(start.group(1), 16), int(end.group(1), 16)))
    return found


def exe_words(path: pathlib.Path) -> list[int]:
    body = path.read_bytes()[PSX_EXE_HEADER_BYTES:]
    usable = len(body) - (len(body) % 4)
    return list(struct.unpack_from(f"<{usable // 4}I", body, 0))


def find_span(words: list[int], start: int, end: int) -> list[tuple[int, int]]:
    """Every start->end pair with no intervening restart of the same block."""
    spans: list[tuple[int, int]] = []
    for i, value in enumerate(words):
        if value != start:
            continue
        for j in range(i + 1, min(i + MAX_SPAN_WORDS, len(words))):
            if words[j] == end:
                spans.append((i, j))
                break
            if words[j] == start:
                break
    return spans


def digest(values: list[int]) -> int:
    """FNV-1a over the measured words, matching the disc's own hash style."""
    acc = 0x811C_9DC5
    for value in values:
        acc = ((acc ^ (value & 0xFFFF_FFFF)) * 0x0100_0193) & 0xFFFF_FFFF
    return acc


def parse_baseline(path: pathlib.Path) -> dict[str, str]:
    rows: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#") or line.startswith("probe,"):
            continue
        name, rest = line.split(",", 1)
        rows[name] = rest
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", help="linked hardware-tests.exe")
    parser.add_argument("--source", default=SOURCE, help="probe source to read markers from")
    parser.add_argument("--baseline", help="previous output to compare against")
    parser.add_argument(
        "--fail-on-change",
        action="store_true",
        help="exit non-zero if any measured block moved (CI gate)",
    )
    args = parser.parse_args()

    try:
        probes = probes_from_source(pathlib.Path(args.source))
    except ProbeError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 2
    if not probes:
        print(f"FAIL: no marker-bracketed probes found in {args.source}", file=sys.stderr)
        return 2

    words = exe_words(pathlib.Path(args.exe))
    baseline = parse_baseline(pathlib.Path(args.baseline)) if args.baseline else None

    print(f"# exe={args.exe} words={len(words)} probes={len(probes)}")
    print("probe,start,end,words,digest" + (",baseline,changed" if baseline else ""))

    ambiguous = 0
    drift: list[str] = []
    for name, start, end in probes:
        spans = find_span(words, start, end)
        if len(spans) > 1 and baseline is not None:
            # Marker words are legal instruction encodings, so unrelated code
            # can coincidentally form a second start/end pair. When that
            # happens, the span whose LENGTH matches the pinned one is the real
            # block: a genuine edit to the measured block changes its digest,
            # which is still caught, so this disambiguates without excusing
            # drift.
            prior = baseline.get(name)
            if prior:
                want = prior.split(",")[2]
                sized = [s for s in spans if str(s[1] - s[0] - 1) == want]
                if len(sized) == 1:
                    spans = sized
        if len(spans) != 1:
            # An ambiguous or absent span means the audit cannot speak for this
            # block, which is a failure in its own right -- not something to
            # paper over with the first candidate.
            print(f"{name},{start:#010x},{end:#010x},AMBIGUOUS({len(spans)}),-")
            ambiguous += 1
            continue
        first, last = spans[0]
        measured = words[first + 1 : last]
        row = f"{name},{start:#010x},{end:#010x},{len(measured)},{digest(measured):#010x}"
        if baseline is not None:
            prior = baseline.get(name)
            current = row.split(",", 1)[1]
            changed = int(prior is not None and prior != current)
            row += f",{'-' if prior is None else 'pinned'},{changed}"
            if changed:
                drift.append(f"{name}: {prior} -> {current}")
        print(row)

    print(f"# ambiguous={ambiguous} drift={len(drift)}")
    for entry in drift:
        print(f"# drift: {entry}")
    if ambiguous:
        print(f"FAIL: {ambiguous} probe span(s) could not be located uniquely", file=sys.stderr)
        return 1
    if drift and args.fail_on_change:
        print(f"FAIL: {len(drift)} measured block(s) changed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
