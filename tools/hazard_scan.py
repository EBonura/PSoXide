#!/usr/bin/env python3
"""Scan a PS-EXE for R3000 load-delay hazards created by branch delay slots.

The R3000 has no load interlock: the instruction after a load still sees the
register's old value. LLVM inserts the required nop after a load, but its
MipsDelaySlotFiller can then hoist that load into a branch delay slot, and the
first instruction of the branch target (or of the fall-through) reads the
register one instruction too early. The guest builds pass
`-Cllvm-args=-disable-mips-df-backward-search` to stop it; this scan proves an
image is clean, whatever built it.

    python3 tools/hazard_scan.py path/to/game.exe [more.exe ...]

Prints every hazard as `branch | delay-slot load | consumer` and exits 1 if
any image has one. Needs mipsel-none-elf-objdump on PATH. Loads into $zero
(cache probes) are ignored.
"""
import re
import subprocess
import sys

HEADER = 0x800
LOAD_ADDR = 0x80010000
LOADS = {"lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr", "lwc2", "mfc0", "mfc2", "cfc2"}
BRANCHES = {"beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "bltzal",
            "bgezal", "j", "jal", "jr", "jalr", "b", "bal"}
STORES = {"sw", "sh", "sb", "swl", "swr", "swc2"}
READS_ALL = STORES | {"mtc0", "mtc2", "ctc2", "jr", "jalr", "mult", "multu", "div",
                      "divu", "mthi", "mtlo"}
WRITES_ONLY = {"lui", "li", "mfhi", "mflo"}


def disassemble(path):
    out = subprocess.run(
        ["mipsel-none-elf-objdump", "-D", "-b", "binary", "-m", "mips:3000", "-EL",
         f"--adjust-vma={LOAD_ADDR - HEADER:#x}", path],
        capture_output=True, text=True, check=True).stdout
    listing = {}
    for line in out.splitlines():
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f]{8}\s+(\S+)\s*(.*)", line)
        if m:
            listing[int(m.group(1), 16)] = (m.group(2), m.group(3))
    return listing


def load_destination(op, args):
    if op not in LOADS:
        return None
    rd = args.split(",")[0].strip()
    return None if rd == "zero" else rd


def reads(op, args, reg):
    if op == "nop":
        return False
    parts = [p.strip() for p in args.split(",")] if args else []
    if op in LOADS:
        sources = parts[1:]
    elif op in READS_ALL:
        sources = parts
    elif op in WRITES_ONLY:
        sources = []
    else:
        sources = parts[1:] if len(parts) > 1 else parts
    for source in sources:
        m = re.search(r"\(([a-z0-9]+)\)", source)
        if (m and m.group(1) == reg) or source == reg:
            return True
    return False


def scan(path):
    listing = disassemble(path)
    hazards = []
    for addr, (op, args) in listing.items():
        if op not in BRANCHES or addr + 4 not in listing:
            continue
        slot_op, slot_args = listing[addr + 4]
        rd = load_destination(slot_op, slot_args)
        if rd is None:
            continue
        targets = []
        if op not in ("jr", "jalr"):
            m = re.search(r"0x([0-9a-f]+)$", args)
            if m:
                targets.append(int(m.group(1), 16))
        if op != "j":
            targets.append(addr + 8)
        for target in targets:
            if target in listing and reads(*listing[target], rd):
                top, targs = listing[target]
                hazards.append(f"{addr:08x}: {op} {args} | slot {slot_op} {slot_args}"
                               f" | {target:08x}: {top} {targs}")
    return hazards


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    total = 0
    for path in sys.argv[1:]:
        hazards = scan(path)
        for hazard in hazards:
            print(hazard)
        print(f"{len(hazards)} hazards in {path}")
        total += len(hazards)
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())
