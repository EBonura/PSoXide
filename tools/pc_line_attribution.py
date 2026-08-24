#!/usr/bin/env python3
"""Attribute exact 16-byte PC-line counts to linker-map symbols and cache sets."""

from __future__ import annotations

import argparse
import bisect
import csv
import pathlib
import re
from collections import defaultdict


MAP_ROW = re.compile(
    r"^\s*([0-9a-fA-F]+)\s+[0-9a-fA-F]+\s+([0-9a-fA-F]+)\s+\d+\s+(.+?)\s*$"
)


def load_symbols(path: pathlib.Path) -> list[tuple[int, int, str]]:
    symbols: list[tuple[int, int, str]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        match = MAP_ROW.match(line)
        if not match:
            continue
        address = int(match.group(1), 16)
        size = int(match.group(2), 16)
        name = match.group(3)
        if (
            size == 0
            or not 0x8000_0000 <= address <= 0xBFFF_FFFF
            or "/" in name
            or ":(" in name
            or name.startswith((".", "BYTE(", "LONG(", "QUAD(", "*fill*"))
            or " = " in name
        ):
            continue
        symbols.append((address, address + size, name))
    symbols.sort(key=lambda item: (item[0], item[1]))
    return symbols


def load_lines(path: pathlib.Path) -> list[tuple[int, int]]:
    lines: list[tuple[int, int]] = []
    with path.open(newline="", encoding="utf-8") as source:
        for row in csv.DictReader(source):
            lines.append((int(row["line_pc"], 16), int(row["instructions"])))
    return lines


def canonical_code_line(physical: int) -> int:
    """Map a physical code line to the conventional cached/BIOS alias."""
    if 0x1FC0_0000 <= physical < 0x1FC8_0000:
        return physical | 0xA000_0000
    return physical | 0x8000_0000


def load_eviction_pairs(
    path: pathlib.Path,
) -> list[tuple[int, int, int, int, int]]:
    """Return (victim, incoming, set, events, stalls) temporal replacements."""
    totals: dict[tuple[int, int, int], list[int]] = defaultdict(lambda: [0, 0])
    with path.open(newline="", encoding="utf-8") as source:
        for row in csv.DictReader(source):
            if row["miss_kind"] != "tag" or int(row["victim_valid_mask"], 16) == 0:
                continue
            victim = canonical_code_line(int(row["victim_line"], 16))
            incoming = canonical_code_line(int(row["incoming_line"], 16))
            cache_set = int(row["cache_set"], 16)
            aggregate = totals[(victim, incoming, cache_set)]
            aggregate[0] += 1
            aggregate[1] += int(row["stall_cycles"])
    return [
        (victim, incoming, cache_set, counts[0], counts[1])
        for (victim, incoming, cache_set), counts in totals.items()
    ]


def symbol_for(
    pc: int, symbols: list[tuple[int, int, str]], starts: list[int]
) -> str:
    index = bisect.bisect_right(starts, pc) - 1
    while index >= 0 and starts[index] == starts[bisect.bisect_right(starts, pc) - 1]:
        start, end, name = symbols[index]
        if start <= pc < end:
            return name
        index -= 1
    if index >= 0:
        start, end, name = symbols[index]
        if start <= pc < end:
            return name
    return "<unattributed>"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pc_lines", type=pathlib.Path)
    parser.add_argument("linker_map", type=pathlib.Path)
    parser.add_argument("--limit", type=int, default=30)
    parser.add_argument(
        "--icache-events",
        type=pathlib.Path,
        help="exact refill CSV emitted by frontend --icache-event-log",
    )
    args = parser.parse_args()

    lines = load_lines(args.pc_lines)
    symbols = load_symbols(args.linker_map)
    starts = [symbol[0] for symbol in symbols]
    total = sum(count for _, count in lines)

    by_symbol: dict[str, int] = defaultdict(int)
    by_set: dict[int, list[tuple[int, int, str]]] = defaultdict(list)
    attributed: list[tuple[int, int, str]] = []
    for pc, count in lines:
        name = symbol_for(pc, symbols, starts)
        by_symbol[name] += count
        by_set[(pc >> 4) & 0xFF].append((pc, count, name))
        attributed.append((pc, count, name))

    print("hot functions")
    print("instructions,percent,symbol")
    for name, count in sorted(by_symbol.items(), key=lambda item: -item[1])[: args.limit]:
        print(f"{count},{count * 100.0 / total:.4f},{name}")

    print("\nhot lines")
    print("instructions,percent,line_pc,cache_set,symbol")
    for pc, count, name in sorted(attributed, key=lambda item: -item[1])[: args.limit]:
        print(f"{count},{count * 100.0 / total:.4f},0x{pc:08x},0x{(pc >> 4) & 0xff:02x},{name}")

    pressures: list[tuple[int, int, int, list[tuple[int, int, str]]]] = []
    for cache_set, entries in by_set.items():
        entries.sort(key=lambda item: -item[1])
        set_total = sum(count for _, count, _ in entries)
        eviction_pressure = set_total - entries[0][1]
        pressures.append((eviction_pressure, set_total, cache_set, entries))
    pressures.sort(reverse=True)

    print("\nhot direct-map conflicts")
    print("pressure,total,cache_set,distinct_lines,top_lines")
    for pressure, set_total, cache_set, entries in pressures[: args.limit]:
        top = "; ".join(
            f"0x{pc:08x}:{count}:{name}" for pc, count, name in entries[:3]
        )
        print(f"{pressure},{set_total},0x{cache_set:02x},{len(entries)},{top}")

    if args.icache_events is not None:
        pairs = load_eviction_pairs(args.icache_events)
        print("\nexact temporal eviction pairs")
        print(
            "stall_cycles,events,cache_set,victim_line,victim_symbol,"
            "incoming_line,incoming_symbol"
        )
        for victim, incoming, cache_set, events, stalls in sorted(
            pairs, key=lambda item: (-item[4], -item[3], item[0], item[1])
        )[: args.limit]:
            victim_symbol = symbol_for(victim, symbols, starts)
            incoming_symbol = symbol_for(incoming, symbols, starts)
            print(
                f"{stalls},{events},0x{cache_set:02x},0x{victim:08x},"
                f"{victim_symbol},0x{incoming:08x},{incoming_symbol}"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
