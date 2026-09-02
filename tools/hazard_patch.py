#!/usr/bin/env python3
"""Fix R3000 load-delay hazards in a linked PS-EXE without moving any code.

LLVM's MIPS delay-slot filler can leave a load in a branch delay slot whose
destination the next executed instruction reads; the R3000 has no load
interlock, so that instruction sees the stale register. Rebuilding with the
filler disabled costs tens of kilobytes of nops, which some guests cannot
afford. This tool instead reroutes every hazardous branch through a small
trampoline, so the consumer runs at least three instructions after the load:

    j    T            ->  j    TRAMP        TRAMP: nop ; j T ; nop
    jal  F            ->  jal  TRAMP        TRAMP: nop ; j F ; nop
    bXX  rs[,rt], T   ->  j    TRAMP        TRAMP: bXX rs[,rt], +3 ; nop
                                                   j FALL ; nop
                                                   j T    ; nop

The delay-slot load stays where it is and still executes exactly once. The
conditional form re-evaluates the branch inside the trampoline, which is
sound because the slot instruction writes only its own destination and the
tool refuses any site where that destination is a branch source.

The trampolines live in a `.data` array the guest declares:

    #[no_mangle] #[used]
    pub static mut HAZARD_TRAMPOLINES: [u32; 2 + N] = { magic 0x48415a54, N, 0.. };

    python3 hazard_patch.py game.exe          # patch in place
    python3 hazard_patch.py game.exe --check  # report only, exit 1 on hazards

Exit status is non-zero when a hazard cannot be patched, the array is missing
or full, or the rescan after patching still finds one. Needs
mipsel-none-elf-objdump on PATH. Self-contained on purpose: game repos vendor
this file next to their build tool.
"""
import re
import struct
import subprocess
import sys

HEADER = 0x800
LOAD_ADDR = 0x80010000
MAGIC = 0x48415A54
LOADS = {"lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr", "lwc2", "mfc0", "mfc2", "cfc2"}
COND = {"beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b"}
LINKING = {"jal", "bal", "bltzal", "bgezal", "jalr"}
JUMPS = {"j", "jal"}
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


def looks_like_code(listing, addr, words=16):
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


def find_hazards(listing):
    """Every (branch address, op, args, slot op, slot args, consumer address)."""
    found = []
    for addr in sorted(listing):
        op, args = listing[addr]
        if op not in COND | JUMPS | LINKING | {"jr"} or addr + 4 not in listing:
            continue
        slot_op, slot_args = listing[addr + 4]
        rd = load_destination(slot_op, slot_args)
        if rd is None or not looks_like_code(listing, addr):
            continue
        targets = []
        if op not in ("jr", "jalr"):
            m = re.search(r"0x([0-9a-f]+)$", args)
            if m:
                targets.append(int(m.group(1), 16))
        # A call returns to the fall-through much later; only its target can
        # consume the slot load early. An unconditional jump never falls through.
        if op not in JUMPS and op not in LINKING:
            targets.append(addr + 8)
        for target in targets:
            if target in listing and reads(*listing[target], rd):
                found.append((addr, op, args, slot_op, slot_args, target))
    return found


def branch_sources(op, args):
    parts = [p.strip() for p in args.split(",")]
    return [p for p in parts if not p.startswith("0x")]


def encode_j(target, link=False):
    return ((3 if link else 2) << 26) | ((target >> 2) & 0x03FFFFFF)


def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    check_only = "--check" in sys.argv
    if len(argv) != 1:
        print(__doc__)
        return 2
    path = argv[0]
    data = bytearray(open(path, "rb").read())
    listing = disassemble(path)
    hazards = find_hazards(listing)
    for h in hazards:
        print("hazard %08x: %s %s | slot %s %s | consumer %08x" % (h[0], h[1], h[2], h[3], h[4], h[5]))
    if not hazards:
        print("0 hazards in %s" % path)
        return 0
    if check_only:
        print("%d hazards in %s" % (len(hazards), path))
        return 1

    def word_at(addr):
        off = addr - LOAD_ADDR + HEADER
        return struct.unpack_from("<I", data, off)[0]

    def put_word(addr, value):
        off = addr - LOAD_ADDR + HEADER
        struct.pack_into("<I", data, off, value)

    # The trampoline array: magic, capacity, then free words.
    area = None
    for off in range(HEADER, len(data) - 8, 4):
        if struct.unpack_from("<I", data, off)[0] == MAGIC:
            capacity = struct.unpack_from("<I", data, off + 4)[0]
            if 0 < capacity <= 4096:
                area = (LOAD_ADDR + off - HEADER + 8, capacity)
                break
    if area is None:
        print("no HAZARD_TRAMPOLINES array (magic %#x) in %s" % (MAGIC, path))
        return 1
    base, capacity = area
    cursor = 0
    # Any non-zero word means an earlier patch pass already used the area.
    while cursor < capacity and word_at(base + cursor * 4) != 0:
        cursor += 1

    nop = 0
    patched = 0
    for addr, op, args, slot_op, slot_args, consumer in hazards:
        rd = load_destination(slot_op, slot_args)
        if op in ("jr", "jalr", "bltzal", "bgezal", "bal"):
            print("cannot patch %08x: %s branches on a register or links inside the trampoline" % (addr, op))
            return 1
        if op in COND and rd in branch_sources(op, args):
            print("cannot patch %08x: the slot load writes a branch source (%s)" % (addr, rd))
            return 1
        target = int(re.search(r"0x([0-9a-f]+)$", args).group(1), 16)
        original = word_at(addr)
        tramp = base + cursor * 4
        if op in JUMPS:
            words = [nop, encode_j(target), nop]
            put_word(addr, encode_j(tramp, link=(op == "jal")))
        else:
            fall = addr + 8
            # Same opcode and registers, offset +3 words: skips nop, j FALL, nop.
            words = [(original & 0xFFFF0000) | 3, nop, encode_j(fall), nop, encode_j(target), nop]
            put_word(addr, encode_j(tramp))
        if cursor + len(words) > capacity:
            print("trampoline array full at %08x (%d words)" % (addr, capacity))
            return 1
        for i, w in enumerate(words):
            put_word(tramp + i * 4, w)
        cursor += len(words)
        patched += 1
        print("patched %08x -> trampoline %08x (%d words)" % (addr, tramp, len(words)))

    open(path, "wb").write(data)
    remaining = find_hazards(disassemble(path))
    for h in remaining:
        print("still hazardous %08x: %s %s" % (h[0], h[1], h[2]))
    print("%d patched, %d remaining, %d/%d trampoline words used in %s" % (patched, len(remaining), cursor, capacity, path))
    return 1 if remaining else 0


if __name__ == "__main__":
    sys.exit(main())
