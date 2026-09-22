"""The one R3000 load-delay hazard detector behind hazard_scan.py and
hazard_patch.py.

Both tools used to carry their own copy of this logic, and the copies drifted:
each missed a hazard class that the other's history had already met (branch
operands, 2026-09-04; `jr ra` / `jalr` / unresolved `jr`, 2026-09-22). Every
hazard class lives here now, in `find_hazards`, so the scanner reports exactly
the sites the patcher rewrites. Add a class here once and both tools see it;
tools/test_hazard_tools.py fails if their reports ever differ.

Import it from the tools' own directory (both CLIs put that directory on
sys.path first), so the SDK's hydrated `.psoxide/tools/` works as is. Needs
mipsel-none-elf-objdump on PATH, or another one named in OBJDUMP.
"""
import os
import re
import struct
import subprocess

HEADER = 0x800
LOAD_ADDR = 0x80010000
LOADS = {"lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr", "lwc2", "mfc0", "mfc2", "cfc2"}
COND = {"beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b"}
LINKING = {"jal", "bal", "bltzal", "bgezal", "jalr"}
JUMPS = {"j", "jal"}
# Every instruction with a delay slot.
BRANCHES = COND | LINKING | JUMPS | {"jr"}
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
    """No undecodable word within `words` instructions on either side. A
    PS-EXE carries its tables and assets in the same load, and those decode
    as random branches."""
    for offset in range(-words * 4, words * 4 + 4, 4):
        entry = listing.get(addr + offset)
        if entry is not None and entry[0] == ".word":
            return False
    return True


def load_destination(op, args):
    """The register a load writes; None for anything else and for loads into
    $zero (cache probes)."""
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


def find_hazards(listing, word_at=None, image_end=0, base=LOAD_ADDR, is_code=looks_like_code):
    """Every (branch address, op, args, slot op, slot args, consumer address,
    table entry address), in address order. The entry address is the
    jump-table word that names the consumer for a `jr` switch dispatch and
    None otherwise. The consumer is None for a register jump whose
    destination is not in the image: `jr ra`, `jalr`, and a `jr` whose table
    cannot be resolved. Nothing here can prove those safe, so each counts.

    `is_code` is the data guard. The CLIs pass their own module's
    `looks_like_code`, so a game that loads hazard_scan.py as a module and
    replaces it (to never skip proven .text) still reaches the detector."""
    found = []
    for addr in sorted(listing):
        op, args = listing[addr]
        if addr + 4 not in listing:
            continue
        if op not in BRANCHES:
            continue
        slot_op, slot_args = listing[addr + 4]
        rd = load_destination(slot_op, slot_args)
        if rd is None or not is_code(listing, addr):
            continue
        if op == "jr" and args.strip() == "ra":
            # A function returning a value loaded in its own delay slot: the
            # caller's first instruction reads it one instruction early, and
            # through a function pointer there is no call site to check, so
            # every one counts (cs-psx 31517d4, hl-psx settings::value). A
            # slot load into ra itself only changes a register the caller
            # never reads before restoring it, so it is left alone.
            if rd != "ra":
                found.append((addr, op, args, slot_op, slot_args, None, None))
            continue
        if op == "jalr":
            # The callee, and so its first instruction, is unknown.
            found.append((addr, op, args, slot_op, slot_args, None, None))
            continue
        if op == "jr":
            # A switch dispatch: the table words are data, so an entry whose
            # target consumes the slot load can be pointed at a trampoline.
            # An unresolved table leaves the target unknown, like a return.
            entries = jump_table(listing, addr, word_at, image_end, base) if word_at else None
            if entries is None:
                found.append((addr, op, args, slot_op, slot_args, None, None))
                continue
            for entry, target in entries:
                if reads(*listing[target], rd):
                    found.append((addr, op, args, slot_op, slot_args, target, entry))
            continue
        targets = []
        m = re.search(r"0x([0-9a-f]+)$", args)
        if m:
            targets.append(int(m.group(1), 16))
        # A call returns to the fall-through much later; only its target can
        # consume the slot load early. An unconditional jump never falls through.
        if op not in JUMPS and op not in LINKING:
            targets.append(addr + 8)
        for target in targets:
            if target in listing and reads(*listing[target], rd):
                found.append((addr, op, args, slot_op, slot_args, target, None))
    return found


def straight_line_pairs(listing, is_code=looks_like_code):
    """Addresses of loads the very next instruction reads, outside any delay
    slot. LLVM's MIPS-I scheduler keeps these apart; a register allocator or
    scheduler switch that breaks that (-regalloc=pbqp did, 2026-09-04) shows
    up here first."""
    pairs = []
    for addr, (op, args) in listing.items():
        rd = load_destination(op, args)
        if rd is None or addr + 4 not in listing or not is_code(listing, addr):
            continue
        prev = listing.get(addr - 4)
        if prev is not None and prev[0] in BRANCHES:
            continue  # a delay-slot load is find_hazards' case
        if reads(*listing[addr + 4], rd):
            pairs.append(addr)
    return pairs
