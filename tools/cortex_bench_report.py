#!/usr/bin/env python3
"""Summarise a `tools/cortex_bench.sh` output directory as one benchmark row.

Reads run-1.txt / run-2.txt (frontend launch stdout), route-N.csv and
cycles-N.csv, checks the two replays agree byte for byte, writes
`summary.json`, and prints a before/after table when `--baseline <dir>` names
an earlier output directory. Frame rate is display-start flips per vblank over
the whole tape; the cycle attribution is the emulator's own accounting.

A poll-bound tape ends after a fixed number of simulation ticks, so the bus
cycles to tape end are constant by construction. The continuous performance
number is therefore `idle_instructions`: retired instructions inside
`App::run_scheduled`, which is the vblank wait loop (exact PC-line counts
symbolised against the guest link map). `work_instructions` is everything
else. Flips quantise to whole vblanks and only move at a cadence boundary.
"""
import bisect
import argparse
import csv
import json
import pathlib
import re
import sys

CYCLE_COLUMNS = [
    "issue_cycles",
    "ram_load_stall_cycles",
    "stack_ram_load_stall_cycles",
    "ram_store_stall_cycles",
    "mmio_stall_cycles",
    "icache_refill_stall_cycles",
    "gte_busy_stall_cycles",
    "muldiv_interlock_stall_cycles",
]


def load_map(path: pathlib.Path):
    symbols = []
    for line in path.read_text().splitlines():
        m = re.match(r"^\s*([0-9a-f]{8})\s+[0-9a-f]{8}\s+([0-9a-f]+)\s+\d+\s{9,}(\S.*)$", line)
        if m and not m.group(3).startswith((".", "BYTE", "LONG", "SHORT", "QUAD", "FILL", "<internal")):
            symbols.append((int(m.group(1), 16), m.group(3).strip()))
    symbols.sort()
    return [a for a, _ in symbols], [n for _, n in symbols]


def idle_split(out: pathlib.Path, run: int) -> tuple[int, int]:
    """(idle, work) retired instructions from the exact PC-line log."""
    addrs, names = load_map(out / "link.map")
    idle = work = 0
    for row in csv.DictReader(open(out / f"pcline-{run}.csv")):
        pc = int(row["line_pc"], 16)
        count = int(row["instructions"])
        i = bisect.bisect_right(addrs, pc) - 1
        name = names[i] if i >= 0 else ""
        if "run_scheduled" in name:
            idle += count
        else:
            work += count
    return idle, work


def run_summary(out: pathlib.Path, run: int) -> dict:
    text = (out / f"run-{run}.txt").read_text()
    grab = lambda key: re.search(rf"^{key}=(\S+)", text, re.M)
    tick = re.search(r"^tick=(\d+)\s+cycles=(\d+)", text, re.M)
    if tick is None:
        sys.exit(f"run-{run}.txt has no tick/cycles line; the replay did not finish")
    summary = {
        "instructions": int(tick.group(1)),
        "bus_cycles": int(tick.group(2)),
        "route_ticks": int(grab("route-ticks").group(1)),
        "vram_hash": grab("vram_fnv1a_64").group(1),
        "display_hash": grab("display_fnv1a_64").group(1),
    }
    route = list(csv.DictReader(open(out / f"route-{run}.csv")))
    flips = sum(int(r["display_start_changed"]) for r in route)
    vblanks = max(len(route) - 1, 1)
    summary["flips"] = flips
    summary["fps"] = round(flips / (vblanks / 60.0), 3)
    if (out / f"pcline-{run}.csv").exists() and (out / "link.map").exists():
        idle, work = idle_split(out, run)
        summary["idle_instructions"] = idle
        summary["work_instructions"] = work
        summary["idle_percent"] = round(100.0 * idle / max(idle + work, 1), 2)
    cycles = list(csv.DictReader(open(out / f"cycles-{run}.csv")))
    total = sum(int(r["profiled_cpu_cycles"]) for r in cycles) or 1
    summary["cycle_shares"] = {
        c.replace("_cycles", ""): round(100.0 * sum(int(r[c]) for r in cycles) / total, 2)
        for c in CYCLE_COLUMNS
    }
    return summary


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--baseline", type=pathlib.Path)
    args = ap.parse_args()
    one, two = run_summary(args.out, 1), run_summary(args.out, 2)
    for key in ("vram_hash", "display_hash", "bus_cycles", "route_ticks"):
        if one[key] != two[key]:
            sys.exit(f"cortex-bench: FAIL: replays disagree on {key}: {one[key]} vs {two[key]}")
    gate = (args.out / "symbol-gate.txt").read_text().strip() if (args.out / "symbol-gate.txt").exists() else ""
    one["symbol_gate"] = "PASS" if gate.startswith("guest-symbol-gate: PASS") else "FAIL"
    one["exe_sha256"] = (args.out / "exe.sha256").read_text().split()[0] if (args.out / "exe.sha256").exists() else ""
    (args.out / "summary.json").write_text(json.dumps(one, indent=2))

    rows = [("after", one)]
    if args.baseline:
        rows.insert(0, ("before", json.loads((args.baseline / "summary.json").read_text())))
    cols = ["work_instructions", "idle_percent", "flips", "fps", "vram_hash", "display_hash", "symbol_gate"]
    for _, r in rows:
        r.setdefault("work_instructions", "n/a"); r.setdefault("idle_percent", "n/a")
    print("| run | " + " | ".join(cols) + " | ram_load% | icache% | muldiv% |")
    print("|---|" + "---|" * (len(cols) + 3))
    for name, s in rows:
        sh = s["cycle_shares"]
        print(
            f"| {name} | " + " | ".join(str(s[c]) for c in cols)
            + f" | {sh['ram_load_stall']} | {sh['icache_refill_stall']} | {sh['muldiv_interlock_stall']} |"
        )
    if args.baseline:
        b = rows[0][1]
        same = one["vram_hash"] == b["vram_hash"] and one["display_hash"] == b["display_hash"]
        if isinstance(b.get("work_instructions"), int) and isinstance(one.get("work_instructions"), int):
            delta = 100.0 * (one["work_instructions"] - b["work_instructions"]) / b["work_instructions"]
            print(f"\nwork instructions: {delta:+.3f}%  ({'hashes identical' if same else 'HASHES DIFFER: visual change, needs the 6.2 A/B'})")
    print(f"\ncortex-bench: PASS (two replays identical; summary in {args.out / 'summary.json'})")


if __name__ == "__main__":
    main()
