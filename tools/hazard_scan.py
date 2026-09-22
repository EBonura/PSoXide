#!/usr/bin/env python3
"""Scan a PS-EXE for R3000 load-delay hazards created by branch delay slots.

The R3000 has no load interlock: the instruction after a load still sees the
register's old value. LLVM inserts the required nop after a load, but its
MipsDelaySlotFiller can then hoist that load into a branch delay slot, and the
first instruction of the branch target (or of the fall-through) reads the
register one instruction too early. A guest either passes
`-Cllvm-args=-disable-mips-df-backward-search`, which stops the hoist and
leaves a nop in most delay slots, or keeps every filler search on and runs
`tools/hazard_patch.py` after the link (`tools/sdk-examples.mk` does the
latter). This scan proves an image is clean, whatever built it.

    python3 tools/hazard_scan.py path/to/game.exe [more.exe ...]

Prints every hazard as `branch | delay-slot load | consumer` and exits 1 if
any image has one. Needs mipsel-none-elf-objdump on PATH, or another one named
in OBJDUMP. Loads into $zero (cache probes) are ignored, and so is anything
within 16 words of a byte pattern that does not decode as an instruction: a
PS-EXE carries its tables and assets in the same load, and those decode as
random branches. Addresses come from the header's load address, so a raw blob
linked elsewhere (the demo disc's chain loader at 0x801F0000) can be scanned
once a PS-EXE header naming that address is put in front of it.

A slot load whose consumer cannot be seen from the image counts as a hazard,
because nothing here can prove it safe:

    jr ra        the value lands on the caller's first instruction, and a
                 function reached through a pointer has no call site to check
                 (cs-psx's settings getters drew stale values through this
                 shape, 2026-09-15; hl-psx's `settings::value` has it too)
    jalr rs      the callee is unknown, so is its first instruction
    jr rs        a register jump whose jump table cannot be resolved
"""
import re
import os
import struct
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


def load_address(data):
    """The header's t_addr; images without a PS-EXE header load at LOAD_ADDR."""
    if data[:8] == b"PS-X EXE":
        return struct.unpack_from("<I", data, 0x18)[0]
    return LOAD_ADDR


def disassemble(path, base=LOAD_ADDR):
    out = subprocess.run(
        [os.environ.get("OBJDUMP", "mipsel-none-elf-objdump"), "-D", "-b", "binary", "-m", "mips:3000", "-EL",
         f"--adjust-vma={base - HEADER:#x}", path],
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


def jump_table(listing, jr_addr, word_at, image_end, base=LOAD_ADDR):
    """Resolve the table a `jr rs` dispatches through: (entry address, target)
    pairs. LLVM lowers a switch as `sll idx,idx,2 ; lui t,%hi(T) ; addu ;
    lw rs,%lo(T)(...) ; jr rs`; the table is at the last `lui` before that load
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
    table = ((hi << 16) + load[1]) & 0xFFFFFFFF
    entries = []
    for k in range(64):
        addr = table + k * 4
        if not base <= addr < image_end:
            break
        target = word_at(addr)
        if target & 3 or not base <= target < image_end or target not in listing:
            break
        entries.append((addr, target))
    return entries or None


def scan(path):
    with open(path, "rb") as f:
        data = f.read()
    base = load_address(data)
    listing = disassemble(path, base)
    image_end = base + len(data) - HEADER

    def word_at(addr):
        return int.from_bytes(data[addr - base + HEADER:][:4], "little")

    hazards = []
    # Straight-line load-use pairs: a load whose destination the very next
    # instruction reads, outside any delay slot. LLVM's MIPS-I scheduler
    # keeps these apart; a register allocator or scheduler switch that
    # breaks that (-regalloc=pbqp did, 2026-09-04) shows up here first.
    straight = 0
    for addr, (op, args) in listing.items():
        rd = load_destination(op, args)
        if rd is None or addr + 4 not in listing or not looks_like_code(listing, addr):
            continue
        prev = listing.get(addr - 4)
        if prev is not None and prev[0] in BRANCHES:
            continue  # a delay-slot load is the branch case below
        if reads(*listing[addr + 4], rd):
            straight += 1
    if straight:
        print(f"warning: {straight} straight-line load-use pairs (next instruction reads the loaded register)")
    for addr, (op, args) in listing.items():
        if op not in BRANCHES or addr + 4 not in listing:
            continue
        slot_op, slot_args = listing[addr + 4]
        rd = load_destination(slot_op, slot_args)
        if rd is None or not looks_like_code(listing, addr):
            continue
        site = f"{addr:08x}: {op} {args} | slot {slot_op} {slot_args}"
        targets = []
        if op == "jr" and args.strip() == "ra":
            # A function returning through its own delay-slot load: the
            # caller's first instruction is the consumer. A slot load into
            # ra itself only changes a register the caller never reads
            # before restoring it, so it is left alone.
            if rd != "ra":
                hazards.append(f"{site} | the caller's first instruction")
            continue
        if op == "jr":
            # A switch dispatch: every table target is a possible consumer.
            entries = jump_table(listing, addr, word_at, image_end, base)
            if entries is None:
                hazards.append(f"{site} | jump table not resolved, target unknown")
                continue
            targets.extend(target for _, target in entries)
        elif op == "jalr":
            # A register call: the callee, and so its first instruction, is
            # unknown.
            hazards.append(f"{site} | callee unknown")
            continue
        else:
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
                hazards.append(f"{site} | {target:08x}: {top} {targs}")
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
