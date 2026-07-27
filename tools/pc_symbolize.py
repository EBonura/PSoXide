#!/usr/bin/env python3
"""Aggregate `frontend launch --pc-sample-log` output into guest functions.

The editor-playtest guest links with `--oformat=binary`, so it has no symbols.
Relink the same sources into an ELF (drop `--oformat=binary`, keep every other
RUSTFLAG) and pass that ELF here together with the sample CSV:

    make perf-symbols                       # builds the ELF + prints the table
    python3 tools/pc_symbolize.py --elf <elf> --samples <pc-csv> [--top 40]

Addresses that fall inside no symbol are reported as `<unmapped>`.
"""

import argparse
import bisect
import csv
import subprocess
import sys

NM = "mipsel-none-elf-nm"


def load_symbols(elf):
    out = subprocess.run(
        [NM, "-n", "--defined-only", elf], capture_output=True, text=True, check=True
    ).stdout
    syms = []
    for line in out.splitlines():
        parts = line.split(maxsplit=2)
        if len(parts) != 3:
            continue
        addr, kind, name = parts
        # 'A' is an absolute linker constant (RAM_BASE etc), not code.
        if kind.upper() == "A":
            continue
        syms.append((int(addr, 16), name))
    syms.sort()
    return [a for a, _ in syms], [n for _, n in syms]


def demangle(names):
    if not names:
        return {}
    try:
        out = subprocess.run(
            ["rustfilt"], input="\n".join(names), capture_output=True, text=True, check=True
        ).stdout.splitlines()
        return dict(zip(names, out))
    except (FileNotFoundError, subprocess.CalledProcessError):
        return {}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--elf", required=True)
    ap.add_argument("--samples", required=True)
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument(
        "--min-window-start",
        type=int,
        default=None,
        help=(
            "for --pc-sample-window-log CSVs, ignore buckets whose "
            "window_start_tick is below this route tick"
        ),
    )
    args = ap.parse_args()

    addrs, names = load_symbols(args.elf)
    if not addrs:
        sys.exit(f"{NM} found no symbols in {args.elf}")

    totals, hottest = {}, {}
    grand = 0
    with open(args.samples) as fh:
        for row in csv.DictReader(fh):
            if args.min_window_start is not None:
                if "window_start_tick" not in row:
                    ap.error("--min-window-start requires a windowed PC sample CSV")
                if int(row["window_start_tick"]) < args.min_window_start:
                    continue
            pc = int(row["pc"], 16)
            n = int(row["samples"])
            grand += n
            i = bisect.bisect_right(addrs, pc) - 1
            sym = names[i] if i >= 0 else "<unmapped>"
            totals[sym] = totals.get(sym, 0) + n
            if n > hottest.get(sym, (0, 0))[0]:
                hottest[sym] = (n, pc)

    ranked = sorted(totals.items(), key=lambda kv: -kv[1])[: args.top]
    pretty = demangle([s for s, _ in ranked])

    print(f"{grand} samples over {len(totals)} symbols")
    print(f"{'samples':>8} {'pct':>7}  {'hot pc':>10}  symbol")
    for sym, n in ranked:
        _, pc = hottest[sym]
        print(f"{n:>8} {100 * n / grand:>6.2f}%  0x{pc:08x}  {pretty.get(sym, sym)}")


if __name__ == "__main__":
    main()
