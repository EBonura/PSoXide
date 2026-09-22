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
once a PS-EXE header naming that address is put in front of it. Detection is
tools/hazard_detect.py, the same code hazard_patch.py patches from.

A slot load whose consumer cannot be seen from the image counts as a hazard,
because nothing here can prove it safe:

    jr ra        the value lands on the caller's first instruction, and a
                 function reached through a pointer has no call site to check
                 (cs-psx's settings getters drew stale values through this
                 shape, 2026-09-15; hl-psx's `settings::value` has it too)
    jalr rs      the callee is unknown, so is its first instruction
    jr rs        a register jump whose jump table cannot be resolved
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
# disassemble and looks_like_code are looked up here, not in hazard_detect,
# so a caller that loads this file as a module can still replace them.
from hazard_detect import (HEADER, disassemble, find_hazards, load_address,  # noqa: E402
                           looks_like_code, straight_line_pairs)


def scan(path):
    with open(path, "rb") as f:
        data = f.read()
    base = load_address(data)
    listing = disassemble(path, base)
    image_end = base + len(data) - HEADER

    def word_at(addr):
        return int.from_bytes(data[addr - base + HEADER:][:4], "little")

    straight = len(straight_line_pairs(listing, looks_like_code))
    if straight:
        print(f"warning: {straight} straight-line load-use pairs (next instruction reads the loaded register)")
    hazards = []
    for addr, op, args, slot_op, slot_args, consumer, _ in find_hazards(listing, word_at, image_end, base, looks_like_code):
        site = f"{addr:08x}: {op} {args} | slot {slot_op} {slot_args}"
        if consumer is not None:
            top, targs = listing[consumer]
            hazards.append(f"{site} | {consumer:08x}: {top} {targs}")
        elif op == "jalr":
            hazards.append(f"{site} | callee unknown")
        elif args.strip() == "ra":
            hazards.append(f"{site} | the caller's first instruction")
        else:
            hazards.append(f"{site} | jump table not resolved, target unknown")
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
