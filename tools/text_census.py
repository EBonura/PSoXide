#!/usr/bin/env python3
"""Static instruction census of a PSX-EXE .text section.

Decodes the MIPS-I instruction stream of a linked PSoXide guest executable and
reports the instruction mix, the modelled cycle split (loads vs stores vs
everything else), delay-slot waste, stack-relative memory traffic, and
cache-line alignment of branch targets.

The cycle weights come from `emu/crates/emulator-core/src/bus/memory_timing.rs`
(silicon-calibrated on SCPH-9902): a KSEG0 main-RAM load is 1 issue + 6 stall
cycles, a store is 1 + 1, everything else is 1 plus its own stall class.

Usage:
    python3 tools/text_census.py build/examples/mipsel-sony-psx/release/editor-playtest.exe

`.text` extent is auto-detected from `jr $ra` density, then validated by
invalid-encoding rate. Both are printed so a bad guess is visible.
"""

import collections
import struct
import sys

LOAD_OPS = {0x23: "lw", 0x21: "lh", 0x25: "lhu", 0x20: "lb", 0x24: "lbu", 0x22: "lwl", 0x26: "lwr"}
STORE_OPS = {0x2B: "sw", 0x29: "sh", 0x28: "sb", 0x2A: "swl", 0x2E: "swr"}
OPS = {
    0x09: "addiu", 0x0C: "andi", 0x0D: "ori", 0x0F: "lui", 0x04: "beq", 0x05: "bne",
    0x06: "blez", 0x07: "bgtz", 0x01: "bcondz", 0x02: "j", 0x03: "jal", 0x0A: "slti",
    0x0B: "sltiu", 0x0E: "xori", 0x08: "addi", 0x10: "cop0", 0x12: "cop2",
    0x32: "lwc2", 0x3A: "swc2",
}
SPECIAL = {
    0x00: "sll", 0x02: "srl", 0x03: "sra", 0x04: "sllv", 0x06: "srlv", 0x07: "srav",
    0x08: "jr", 0x09: "jalr", 0x10: "mfhi", 0x12: "mflo", 0x11: "mthi", 0x13: "mtlo",
    0x18: "mult", 0x19: "multu", 0x1A: "div", 0x1B: "divu", 0x20: "add", 0x21: "addu",
    0x22: "sub", 0x23: "subu", 0x24: "and", 0x25: "or", 0x26: "xor", 0x27: "nor",
    0x2A: "slt", 0x2B: "sltu", 0x0C: "syscall", 0x0D: "break",
}
BRANCH_OPS = {0x04, 0x05, 0x06, 0x07, 0x01, 0x02, 0x03}
COND_BRANCH_OPS = {0x04, 0x05, 0x06, 0x07, 0x01}

RAM_LOAD_CYCLES = 7
RAM_STORE_CYCLES = 2


def decode(word):
    """Return a mnemonic, or None when the encoding is not a MIPS-I instruction."""
    if word == 0:
        return "nop"
    op = word >> 26
    if op == 0:
        return SPECIAL.get(word & 0x3F)
    return LOAD_OPS.get(op) or STORE_OPS.get(op) or OPS.get(op)


def find_text_end(words, base):
    """Last 4 KiB window that still contains a `jr $ra`, plus one window of slack."""
    window = 1024
    last = 0
    for start in range(0, len(words), window):
        if any(w == 0x03E00008 for w in words[start:start + window]):
            last = start + window
    return base + last * 4


def main(path):
    data = open(path, "rb").read()
    if data[:8] != b"PS-X EXE":
        sys.exit(f"{path}: not a PSX-EXE")
    base, size = struct.unpack_from("<I", data, 0x18)[0], struct.unpack_from("<I", data, 0x1C)[0]
    payload = [struct.unpack_from("<I", data, 0x800 + i * 4)[0] for i in range(size // 4)]
    end = find_text_end(payload, base)
    n = (end - base) // 4
    words = payload[:n]

    mix = collections.Counter()
    invalid = 0
    sp_loads = sp_stores = 0
    for word in words:
        name = decode(word)
        if name is None:
            invalid += 1
            mix["<invalid>"] += 1
            continue
        mix[name] += 1
        op = word >> 26
        if (word >> 21) & 0x1F == 29:
            if op in LOAD_OPS:
                sp_loads += 1
            elif op in STORE_OPS:
                sp_stores += 1

    loads = sum(mix[m] for m in LOAD_OPS.values())
    stores = sum(mix[m] for m in STORE_OPS.values())
    others = n - loads - stores
    cycles = loads * RAM_LOAD_CYCLES + stores * RAM_STORE_CYCLES + others

    print(f"image      {path}")
    print(f".text      {base:#010x}..{end:#010x}  ({(end - base) / 1024:.0f} KiB, {n} instructions)")
    print(f"payload    {size / 1024:.0f} KiB total (text + data)")
    print(f"invalid    {invalid} ({100 * invalid / n:.2f}%)  <- above ~1% means the .text guess is wrong")
    print()
    print("instruction mix")
    for name, count in mix.most_common(24):
        print(f"  {name:10s} {count:7d}  {100 * count / n:5.2f}%")
    print()
    print(f"modelled cycles {cycles} over {n} instructions = {cycles / n:.2f} CPI (excludes I-cache refill)")
    print(f"  loads   {loads:6d} ({100 * loads / n:5.2f}% of instructions) = {100 * loads * RAM_LOAD_CYCLES / cycles:5.1f}% of cycles")
    print(f"  stores  {stores:6d} ({100 * stores / n:5.2f}%) = {100 * stores * RAM_STORE_CYCLES / cycles:5.1f}% of cycles")
    print(f"  other   {others:6d} ({100 * others / n:5.2f}%) = {100 * others / cycles:5.1f}% of cycles")
    print()
    sp_cycles = sp_loads * RAM_LOAD_CYCLES + sp_stores * RAM_STORE_CYCLES
    print(f"stack traffic ($sp-relative)")
    print(f"  loads   {sp_loads:6d} = {100 * sp_loads / loads:.1f}% of all loads")
    print(f"  stores  {sp_stores:6d} = {100 * sp_stores / stores:.1f}% of all stores")
    print(f"  cycles  {sp_cycles} = {100 * sp_cycles / cycles:.1f}% of modelled cycles")
    print(f"  the same traffic against the scratchpad costs {sp_loads + sp_stores} cycles")
    print()

    nops = mix["nop"]
    load_names = set(LOAD_OPS)
    after_load = after_branch = after_mf = 0
    for i in range(1, n):
        if words[i] != 0:
            continue
        prev, op = words[i - 1], words[i - 1] >> 26
        if op in load_names:
            after_load += 1
        elif op in BRANCH_OPS or (op == 0 and (prev & 0x3F) in (0x08, 0x09)):
            after_branch += 1
        elif op == 0 and (prev & 0x3F) in (0x10, 0x12):
            after_mf += 1
    branches = sum(mix[m] for m in ("beq", "bne", "blez", "bgtz", "bcondz", "j", "jal", "jr", "jalr"))
    print(f"delay slots: {nops} nops = {100 * nops / n:.2f}% of instructions, {100 * nops / cycles:.1f}% of cycles")
    print(f"  in a branch/jump delay slot {after_branch} = {100 * after_branch / branches:.1f}% of {branches} branches unfilled")
    print(f"  padding a load-delay slot   {after_load}")
    print(f"  padding an mfhi/mflo hazard {after_mf}")
    print()

    line_pos = collections.Counter()
    for i, word in enumerate(words):
        if word >> 26 in COND_BRANCH_OPS:
            offset = struct.unpack("<h", struct.pack("<H", word & 0xFFFF))[0]
            target = base + (i + 1) * 4 + offset * 4
            line_pos[(target // 4) % 4] += 1
    total = sum(line_pos.values())
    print("branch-target position inside the 4-word I-cache line (word0 = line aligned)")
    for k in range(4):
        print(f"  word{k}: {line_pos[k]:6d}  {100 * line_pos[k] / total:5.1f}%")
    print("  a uniform 25/25/25/25 split means nothing is cache-line aligned;")
    print("  a tag miss fills only from the entry word to the end of the line.")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else
         "build/examples/mipsel-sony-psx/release/editor-playtest.exe")
