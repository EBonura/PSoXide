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
(cache probes) are ignored, and so is anything within 16 words of a byte
pattern that does not decode as an instruction: a PS-EXE carries its tables
and assets in the same load, and those decode as random branches.
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
# Instructions whose every register operand is a source: stores, coprocessor
# moves, register jumps, multiply/divide, and every conditional branch (a
# `beqz a2, T` consumer reads a2 as its FIRST operand, so the generic
# "destination first" rule below would miss it; this gap let a memcmp whose
# entry tested a2 read a stale count for its whole life, 2026-09-04).
READS_ALL = STORES | {"mtc0", "mtc2", "ctc2", "jr", "jalr", "mult", "multu", "div",
                      "divu", "mthi", "mtlo", "beq", "bne", "beqz", "bnez", "blez",
                      "bgtz", "bltz", "bgez", "bltzal", "bgezal", "beql", "bnel"}
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


def looks_like_code(listing, addr, words=16):
    """No undecodable word within `words` instructions on either side."""
    for offset in range(-words * 4, words * 4 + 4, 4):
        entry = listing.get(addr + offset)
        if entry is not None and entry[0] == ".word":
            return False
    return True


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


def jump_table(listing, jr_addr, word_at, image_end):
    """Resolve the table a `jr rs` dispatches through: (entry address, target)
    pairs. LLVM lowers a switch as `sll idx,idx,2 ; lui t,%hi(T) ; addu ;
    lw rs,%lo(T)(...) ; jr rs`; the base is the last `lui` before that load
    plus the load's offset. Entries run until a word stops being a code
    address (the next table's entries are code addresses too, so a few extra
    targets may be examined; a spurious match only costs one detour)."""
    op, args = listing[jr_addr]
    rs = args.strip()
    load = None
    for back in range(1, 12):
        entry = listing.get(jr_addr - back * 4)
        if entry is None:
            break
        m = re.match(r"(-?\d+)\(([a-z0-9]+)\)", entry[1].split(",")[-1].strip()) if entry[0] == "lw" else None
        if m and entry[1].split(",")[0].strip() == rs:
            load = (jr_addr - back * 4, int(m.group(1)))
            break
    if load is None:
        return None
    hi = None
    for back in range(1, 12):
        entry = listing.get(load[0] - back * 4)
        if entry is None:
            break
        if entry[0] == "lui":
            hi = int(entry[1].split(",")[1].strip(), 16)
            break
    if hi is None:
        return None
    base = ((hi << 16) + load[1]) & 0xFFFFFFFF
    entries = []
    for k in range(64):
        addr = base + k * 4
        if not LOAD_ADDR <= addr < image_end:
            break
        target = word_at(addr)
        if target & 3 or not LOAD_ADDR <= target < image_end or target not in listing:
            break
        entries.append((addr, target))
    return entries or None


def scan(path):
    listing = disassemble(path)
    data = open(path, "rb").read()
    image_end = LOAD_ADDR + len(data) - HEADER

    def word_at(addr):
        return int.from_bytes(data[addr - LOAD_ADDR + HEADER:][:4], "little")

    hazards = []
    for addr, (op, args) in listing.items():
        if op not in BRANCHES or addr + 4 not in listing:
            continue
        slot_op, slot_args = listing[addr + 4]
        rd = load_destination(slot_op, slot_args)
        if rd is None or not looks_like_code(listing, addr):
            continue
        targets = []
        if op == "jr" and args.strip() != "ra":
            # A switch dispatch: every table target is a possible consumer.
            entries = jump_table(listing, addr, word_at, image_end)
            if entries is None:
                print(f"warning {addr:08x}: jr {args} | slot {slot_op} {slot_args}"
                      " | table not resolved, targets unverified")
            else:
                targets.extend(target for _, target in entries)
        elif op == "jalr":
            # A register call: the callee is unknown, so an argument loaded
            # in the slot cannot be proven safe here.
            if rd in ("a0", "a1", "a2", "a3"):
                print(f"warning {addr:08x}: jalr {args} | slot {slot_op} {slot_args}"
                      " | callee unknown, argument unverified")
        elif op != "jr":
            m = re.search(r"0x([0-9a-f]+)$", args)
            if m:
                targets.append(int(m.group(1), 16))
        # A call returns to its fall-through much later and a plain jump never
        # falls through; only conditional branches expose both paths.
        if op not in ("j", "jal", "bal", "jr", "jalr", "bltzal", "bgezal"):
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
