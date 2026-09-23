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

A register jump's consumer is not in the image: `jr ra` returns into every
caller, `jalr` calls an unknown function, and a `jr` whose jump table cannot
be resolved goes somewhere unknown. Those move the load out of the slot
instead, so it runs two instructions before the jump lands:

    jr   rs ; LOAD    ->  j TRAMP ; nop    TRAMP: LOAD ; jr rs ; nop
    jalr rs ; LOAD    ->  j TRAMP ; nop    TRAMP: LOAD ; jalr rs ; nop
                                                  j NEXT ; nop

`j` touches no register and nothing runs between the original address setup
and the load, so the load reads the same operands. The callee of the `jalr`
returns into the trampoline, which jumps back to the original return address.
A load that writes the jump register itself is refused, and `jr ra` loading
ra is left alone (the caller never reads ra before restoring it). So is a
`jalr` slot load that reads the link register: the jalr writes it before its
slot runs, so the load reads the new return address, and hoisted ahead of the
jalr it would read the old one. A load reading the jump register is fine for
both shapes, since neither jump writes it. The other shapes keep the load in
its slot after the same (or, for `jal`, an identically linking) instruction.

The trampolines live in a `.data` array the guest declares:

    #[no_mangle] #[used]
    pub static mut HAZARD_TRAMPOLINES: [u32; 2 + N] = { magic 0x48415a54, N, 0.. };

    python3 hazard_patch.py game.exe          # patch in place
    python3 hazard_patch.py game.exe --check  # report only, exit 1 on hazards
    python3 hazard_patch.py game.exe --map game.map

With `--map` (ld.lld's `-Map` output for the same link) a switch's jump
table is proven and bounded to its own function, as the stack guard does
(see `jump_table`). Without it a table is found only from the dispatch's own
straight-line block (see `Block`), a dispatch whose table base comes from
farther away is unresolved (its slot load moves out of the slot), and a
table may read into a neighbouring one, which only adds harmless detours.
Scan an image patched with `--map` with `hazard_scan.py --map` too: without
the map the scanner may read into the next tables again and report the
detours it skipped.

Exit status is non-zero when a hazard cannot be patched, the array is missing
or full, or the rescan after patching still finds one. Needs
mipsel-none-elf-objdump on PATH, or another one named in OBJDUMP.

This file is also the one hazard detector: hazard_scan.py imports
`find_hazards` and its helpers from here, so a hazard class added once is seen
by both tools (two separate copies drifted and each missed a class, branch
operands 2026-09-04 and `jr ra` / `jalr` / unresolved `jr` 2026-09-22), and
tools/test_hazard_tools.py fails if their reports ever differ. It stays
self-contained, so a one-file copy still works; a game should still call the
SDK's copy from its hydrated `.psoxide/tools/` rather than vendor it.
"""
import bisect
import os
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


def jump_table(listing, jr_addr, word_at, image_end, base=LOAD_ADDR, link_map=None):
    """Resolve the table a `jr rs` dispatches through: (entry address, target)
    pairs, or None. LLVM lowers a switch as `sll idx,idx,2 ; lui t,%hi(T) ;
    addu ; lw rs,%lo(T)(...) ; jr rs`.

    With the image's `link_map` (a LinkMap) the answer is proven; see
    `Flow`. Without one, `Block` follows the jump register back to that
    load and its base back to the constant, inside the dispatch's own
    straight-line block, and gives up (None, so the site stays unresolved)
    when the chain leaves the block. Entries then run until the next table
    another dispatch resolves to, or a word that is not a code address.
    A neighbouring table no dispatch resolves still reads as part of this
    one; nothing without function bounds can tell whose table its words
    are, so it reads on rather than drop a real entry. For the
    patcher an extra entry costs one detour when its target happens to read
    the slot load; the stack guard always has a map."""
    if link_map is not None:
        fn = link_map.function(jr_addr)
        return flow_of(listing, word_at, base, image_end, link_map).jump_table(jr_addr, fn[:2]) if fn else None
    return block_of(listing, word_at, base, image_end).jump_table(jr_addr)


# The most entries `Block` reads from one table. A Rust match on a byte can
# have 256 cases; Quake's TargetGraph::apply_command has 74 (the old cap of
# 64 would have cut it). Reaching this means the words after the table
# are code addresses too, so it says so.
TABLE_CEILING = 4096


class Block:
    """Resolves a `jr` switch dispatch without a link map, from the
    dispatch's straight-line block alone. The old resolver took the nearest
    `lui` of any register and crossed labels, so it could name another
    table; a wrong table hides a hazard from the patcher and the scanner.

    The jump register must come from `lw rs, off(b)` and `b` from
    `addu b, x, y` with exactly one of x, y a constant (lui/li/addiu/ori/move
    chains, as `Flow` accepts), each found by walking back from its reader.
    The walk stops, and the site stays unresolved, where the block may be
    entered some other way: a branch target, an address a data word names
    (a case label, a function pointer), a function prologue
    (`addiu sp,sp,-N`), a word that does not decode, the fall-through of an
    unconditional jump or a return. It crosses a call only for a
    callee-saved register. Unresolved costs the patcher one trampoline that
    moves the load out of the slot, which is always safe; a guess costs a
    missed hazard when it is wrong. `hazard_patch.py --map` proves bases
    loaded farther away (hoisted out of a loop, kept across a branch).

    Tables sit back to back in `.rodata`, so a table runs until the next
    table any dispatch in the image resolves to, or its first word that is
    not a code address, at most TABLE_CEILING entries. On the eleven images
    with maps it was checked against (2026-09-23), every table it resolved
    that `Flow` also proved had the same address and at least `Flow`'s
    entries; the old fixed cap of 64 cut Quake's 74-entry table."""

    def __init__(self, listing, word_at, base, image_end):
        self.listing, self.word_at, self.base, self.image_end = listing, word_at, base, image_end
        self.labels = set()
        for op, args in listing.values():
            target = branch_target(op, args)
            if target is not None:
                self.labels.add(target)
        for addr in range(base, image_end - 3, 4):
            word = word_at(addr)
            if base <= word < image_end and not word & 3:
                self.labels.add(word)
        self.warned = set()
        self.starts = None

    def instruction(self, addr):
        """As Flow.instruction: objdump prints a run of zero words as `...`."""
        entry = self.listing.get(addr)
        if entry is None and self.base <= addr < self.image_end and self.word_at(addr) == 0:
            return ("nop", "")
        return entry

    def entered_here(self, addr):
        """True when control may reach `addr` other than from `addr - 4`."""
        return addr in self.labels or is_prologue(*(self.instruction(addr) or ("", "")))

    def writer(self, reg, at, limit=256):
        """The instruction whose write of `reg` reaches `at` inside its
        straight-line block, or None."""
        x = at
        for _ in range(limit):
            if self.entered_here(x):
                return None
            y = x - 4
            entry = self.instruction(y)
            if entry is None or entry[0] == ".word":
                return None
            before = (self.instruction(y - 4) or ("",))[0]
            if before in ("j", "b", "jr"):
                return None  # x follows an unconditional jump: only a label reaches it
            if before in LINKING and reg not in CALLEE_SAVED:
                return None  # x is a call's return point and the callee may change reg
            if writes(entry[0], entry[1], reg):
                return y
            x = y
        return None

    def value(self, reg, at, depth=0):
        if reg == "zero":
            return 0
        d = self.writer(reg, at) if depth < 8 else None
        if d is None:
            return None
        return constant(*self.instruction(d), lambda source: self.value(source, d, depth + 1))

    def table_address(self, jr_addr):
        rs = self.listing[jr_addr][1].strip()
        load = self.writer(rs, jr_addr)
        if load is None:
            return None
        op, args = self.instruction(load)
        m = re.fullmatch(r"[a-z0-9]+,(-?\d+)\(([a-z0-9]+)\)", args.replace(" ", ""))
        if op != "lw" or m is None:
            return None
        offset, pointer = int(m.group(1)), m.group(2)
        total = self.writer(pointer, load)
        if total is None:
            return None
        op, args = self.instruction(total)
        parts = [p.strip() for p in args.split(",")]
        if op != "addu" or len(parts) != 3:
            return None
        known = [v for v in (self.value(parts[1], total), self.value(parts[2], total)) if v is not None]
        if len(known) != 1:
            return None
        return (known[0] + offset) & 0xFFFFFFFF

    def table_starts(self):
        """Every table address a dispatch in code resolves to, sorted."""
        if self.starts is None:
            self.starts = sorted({t for t in (self.table_address(a) for a, (op, args) in self.listing.items()
                                              if op == "jr" and args.strip() != "ra"
                                              and looks_like_code(self.listing, a)) if t is not None})
        return self.starts

    def jump_table(self, jr_addr):
        table = self.table_address(jr_addr)
        if table is None:
            return None
        starts = self.table_starts()
        stop = min(next((s for s in starts if s > table), self.image_end), self.image_end)
        entries = []
        addr = table
        while self.target(addr, stop) is not None and len(entries) < TABLE_CEILING:
            entries.append((addr, self.target(addr, stop)))
            addr += 4
        if self.target(addr, stop) is not None and jr_addr not in self.warned:
            self.warned.add(jr_addr)
            print("warning: the jump table of the jr at %08x (%08x) still names code after %d entries; "
                  "read no further. Pass --map to bound it to its function" % (jr_addr, table, TABLE_CEILING))
        return entries or None

    def target(self, addr, stop):
        """The code address the table word at `addr` names, or None."""
        if not self.base <= addr < stop:
            return None
        target = self.word_at(addr)
        entry = self.listing.get(target)
        if target & 3 or not self.base <= target < self.image_end or entry is None or entry[0] == ".word":
            return None
        return target


def unmapped_warning(listing, word_at, image_end, base, is_code=looks_like_code):
    """One line for a scan without a link map of an image that has register
    jumps, or None: without the map no table is proven."""
    sites = [a for a, (op, args) in listing.items() if op == "jr" and args.strip() != "ra" and is_code(listing, a)]
    if not sites:
        return None
    block = block_of(listing, word_at, base, image_end)
    resolved = sum(1 for a in sites if block.table_address(a) is not None)
    return (f"warning: no --map: {resolved} of {len(sites)} register jumps resolve to a jump table from their own "
            f"block and none is proven; pass the link map (--map game.map)")


def is_prologue(op, args):
    """`addiu sp,sp,-N`: a function's first instruction, as far as an image
    without a map can tell."""
    return op == "addiu" and re.fullmatch(r"sp,sp,-\d+", args.replace(" ", "")) is not None


_BLOCK = [None, None]


def block_of(listing, word_at, base, image_end):
    """The Block of the last listing asked about (built once per image)."""
    if _BLOCK[0] is not listing:
        _BLOCK[:] = [listing, Block(listing, word_at, base, image_end)]
    return _BLOCK[1]


def constant(op, args, operand):
    """The value lui, li, addiu, ori or move computes, with `operand(reg)`
    giving its source register's value; None for anything else or unknown."""
    parts = [p.strip() for p in args.split(",")]
    try:
        if op == "lui":
            return (int(parts[1], 0) << 16) & 0xFFFFFFFF
        if op == "li":
            return int(parts[1], 0) & 0xFFFFFFFF
        if op not in ("addiu", "ori", "move"):
            return None
        v = operand(parts[1])
        if v is None:
            return None
        if op == "addiu":
            v += int(parts[2], 0)
        elif op == "ori":
            v |= int(parts[2], 0)
        return v & 0xFFFFFFFF
    except (IndexError, ValueError):
        return None


# Registers a callee preserves (o32): their value survives a call.
CALLEE_SAVED = {"s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "fp", "sp"}
# Registers whose value on entry a function may use: the arguments, the
# stack and the return address ($gp is never set up in a PS-EXE).
INCOMING = {"a0", "a1", "a2", "a3", "sp", "ra", "gp"}
# Instructions that write no general register (besides READS_ALL).
NO_DEST = {"nop", "break", "syscall", "rfe", "sync", "teq", "tne", "tge", "tgeu", "tlt", "tltu", "j", "b"}


def branch_target(op, args):
    """The immediate target of a branch or jump, or None."""
    if op not in COND and op not in ("j", "jal", "bal", "bltzal", "bgezal"):
        return None
    m = re.search(r"0x([0-9a-f]+)$", args)
    return int(m.group(1), 16) if m else None


def writes(op, args, reg):
    if op in ("jal", "bal", "bltzal", "bgezal"):
        return reg == "ra"
    if op == "jalr":
        parts = [p.strip() for p in args.split(",")]
        return reg == (parts[0] if len(parts) == 2 else "ra")
    if op in READS_ALL or op in NO_DEST or not args:
        return False
    return args.split(",")[0].strip() == reg


class Flow:
    """Proves where a `jr` switch dispatch reads its target, from the
    control flow of one function of a linked image and its link map.

    The table address is a constant LLVM loads once per switch (`lui`, maybe
    `addiu`, often hoisted out of a loop into a callee-saved register), and
    it holds on every path the compiler laid out to the dispatch. So the
    proof walks back from the `jr` along edges that are certainly in the
    compiled code, and every write of the base register it reaches must
    compute the same constant. The edges:

    * fall-through, and branches and jumps within the function or through
      its hazard trampolines;
    * past a call, only for a callee-saved register and only when the
      callee can return (it has a `jr` or jumps out); after a noreturn call
      (a panic) the next word is some other block, often a switch case;
    * from a switch whose table is already proven to each case it lists.

    A path that reaches something the image cannot show is unknown, and the
    dispatch stays unresolved: an argument, $sp or $ra at the function's
    first instruction, a branch in from other code, an undecodable word.
    A path is dropped when the o32 ABI says compiled code never reads a
    value along it (a register a call may change, a register that carries
    nothing into the function). Dropping a real edge can only lose a proof;
    following one that is not real could find a wrong write, which is why
    the edges above are restricted to ones the code has. The last kind can
    overshoot: until the next table of the function is proven, a table seems
    to run on into it and lends that table's cases to the wrong switch. The
    writes such a path finds must still agree with every other; a wrong
    proof needs every real path to dead-end and the lent ones to agree on
    another constant whose table also jumps only into the function.

    Nothing bounds a table in the image: an index that is an enum tag has
    no range check (hk-psx frame::simulate). A proven table runs to the
    next proven table's start or its first word that does not jump into the
    function past its first instruction (a switch never jumps to its
    function's entry, and a function pointer names nothing else); tables
    sit back to back in the function's `.rodata`, and reading on would take
    the next function's table for this one's (hk-psx, 2026-09-23:
    presentation::service's table ran into menu::run's). An entry may be a
    `nop ; j T ; nop` trampoline into the function: an earlier patch
    pointed it there. Every switch of a function is proven together, round
    by round (one that sits in another's case is only reached through that
    one's table); then each proof is checked again with the final tables,
    and one that no longer holds is dropped and the rounds resume.

    On the hk-psx, Quake and GoldSrc images it was checked against
    (2026-09-23), every table proven this way lies inside its function's own
    `.rodata.<function>` section, and together they cover each such section
    exactly."""

    def __init__(self, listing, word_at, base, image_end, link_map):
        self.listing = listing
        self.word_at, self.base, self.image_end = word_at, base, image_end
        self.map = link_map
        self.text = link_map.text
        self.tramps = link_map.trampolines
        self.sources = {}
        for addr, (op, args) in listing.items():
            target = branch_target(op, args)
            if target is not None:
                self.sources.setdefault(target, []).append(addr)
        self.tables = {}
        self.returning = {}

    def instruction(self, addr):
        """The listing entry at `addr`; objdump prints a run of zero words
        as `...`, so a word missing from the listing is a nop if it is zero."""
        entry = self.listing.get(addr)
        if entry is None and self.base <= addr < self.image_end and self.word_at(addr) == 0:
            return ("nop", "")
        return entry

    def in_tramps(self, addr):
        return self.tramps is not None and self.tramps[0] <= addr < self.tramps[1]

    def trampoline_target(self, addr):
        """T when `addr` is a `nop ; j T ; nop` hazard trampoline."""
        if not self.in_tramps(addr):
            return None
        op, args = self.listing.get(addr + 4, ("", ""))
        m = re.fullmatch(r"0x([0-9a-f]+)", args.strip()) if op == "j" else None
        if m and self.listing.get(addr, ("",))[0] == "nop" and self.listing.get(addr + 8, ("",))[0] == "nop":
            return int(m.group(1), 16)
        return None

    def returns(self, call):
        """False when the call at `call` certainly does not come back: its
        callee has no `jr` and never jumps out of itself."""
        op, args = self.instruction(call)
        target = branch_target(op, args)
        if target is None:
            return True
        target = self.trampoline_target(target) or target
        if target not in self.returning:
            fn = self.map.function(target)
            if fn is None:
                self.returning[target] = True
            else:
                start, end, _ = fn
                self.returning[target] = any(
                    op == "jr" or ((op == "j" or op in COND) and not start <= (branch_target(op, args) or start) < end)
                    for op, args in (self.instruction(a) or ("", "") for a in range(start, end, 4)))
        return self.returning[target]

    def preds(self, x, reg, fn):
        """Instructions that can run just before `x` with `reg` still
        holding the value that reaches `x`, or None when one is unknown."""
        start, end = fn
        if x == start:
            return None if reg in INCOMING else []
        found = [j + 4 for j, (_, targets) in self.tables.get(fn, {}).items() if x in targets]
        for source in self.sources.get(x, ()):
            if start <= source < end or self.in_tramps(source):
                found.append(source + 4)
            elif self.text[0] <= source < self.text[1]:
                return None
            # Otherwise a data word that decodes as a branch: data never runs.
        area = fn if start <= x < end else self.tramps
        if area is not None and area[0] <= x - 4 < area[1]:
            before = (self.instruction(x - 8) or ("",))[0] if area[0] <= x - 8 else ""
            if before in LINKING:
                if reg in CALLEE_SAVED and self.returns(x - 8):
                    found.append(x - 4)
            elif before not in ("j", "b", "jr"):
                found.append(x - 4)
        return found

    def reaching(self, reg, at, fn):
        """The instructions whose write of `reg` the walk carries to `at`,
        or None when a path leads somewhere unknown first."""
        todo = self.preds(at, reg, fn)
        if todo is None:
            return None
        defs, seen = set(), set()
        while todo:
            y = todo.pop()
            if y in seen:
                continue
            seen.add(y)
            entry = self.instruction(y)
            if entry is None or entry[0] == ".word":
                return None
            if writes(entry[0], entry[1], reg):
                defs.add(y)
                continue
            more = self.preds(y, reg, fn)
            if more is None:
                return None
            todo.extend(more)
        return defs

    def value(self, reg, at, fn, depth=0):
        """The constant `reg` holds when `at` runs, or None. Only lui, li,
        addiu, ori and move are followed, and every reaching write must agree."""
        if reg == "zero":
            return 0
        defs = self.reaching(reg, at, fn) if depth < 8 else None
        if not defs:
            return None
        values = set()
        for d in defs:
            v = constant(*self.instruction(d), lambda source: self.value(source, d, fn, depth + 1))
            if v is None:
                return None
            values.add(v)
        return values.pop() if len(values) == 1 else None

    def table_address(self, jr_addr, fn):
        """Where `jr rs` reads its target: `rs` must come from one
        `lw rs, off(b)` whose `b` is always `addu b, x, y` with exactly one of
        x, y a constant C (the other is the scaled index). The table is at C
        + off."""
        rs = self.listing[jr_addr][1].strip()
        loads = self.reaching(rs, jr_addr, fn)
        if not loads or len(loads) != 1:
            return None
        (load,) = loads
        op, args = self.instruction(load)
        m = re.fullmatch(r"[a-z0-9]+,(-?\d+)\(([a-z0-9]+)\)", args.replace(" ", ""))
        if op != "lw" or m is None:
            return None
        offset, pointer = int(m.group(1)), m.group(2)
        sums = self.reaching(pointer, load, fn)
        if not sums:
            return None
        tables = set()
        for d in sums:
            op, args = self.instruction(d)
            parts = [p.strip() for p in args.split(",")]
            if op != "addu" or len(parts) != 3:
                return None
            known = [v for v in (self.value(parts[1], d, fn), self.value(parts[2], d, fn)) if v is not None]
            if len(known) != 1:
                return None
            tables.add((known[0] + offset) & 0xFFFFFFFF)
        return tables.pop() if len(tables) == 1 else None

    def jump_table(self, jr_addr, fn):
        """The proven (entry, target) pairs of `jr_addr`'s table, or None."""
        if fn not in self.tables:
            self.solve(fn)
        got = self.tables[fn].get(jr_addr)
        return got[0] if got else None

    def solve(self, fn):
        """Prove every switch of the function together (see the class)."""
        start, end = fn
        sites = [a for a in range(start, end, 4) if self.is_switch(a)]
        if self.tramps is not None:
            # A patched `jr` moved into a trampoline the function jumps to.
            sites += [a for a in range(self.tramps[0], self.tramps[1], 4) if self.is_switch(a)
                      and any(start <= s < end for s in self.sources.get(a - 4, ()))]
        proven = {}
        for _ in range(4 * len(sites) + 4):
            starts = sorted(set(proven.values()))
            self.tables[fn] = {j: self.extent(t, starts, fn) for j, t in proven.items()}
            fresh = {}
            for j in sites:
                if j not in proven:
                    table = self.table_address(j, fn)
                    if table is not None and self.extent(table, starts, fn)[0]:
                        fresh[j] = table
            if fresh:
                proven.update(fresh)
                continue
            stale = [j for j, t in proven.items() if self.table_address(j, fn) != t]
            if not stale:
                return
            for j in stale:
                del proven[j]
        self.tables[fn] = {}

    def extent(self, table, starts, fn):
        """(entries, targets) of the table at `table`, up to the next of
        `starts` or its first word that does not jump into the function past
        its first instruction."""
        start, end = fn
        stop = min(next((s for s in starts if s > table), self.image_end), self.image_end)
        entries = []
        addr = table
        while self.base <= addr < stop:
            target = self.word_at(addr)
            into = self.trampoline_target(target) or target
            if target & 3 or not start < into < end:
                break
            entries.append((addr, target))
            addr += 4
        return entries, {target for _, target in entries}

    def is_switch(self, addr):
        op, args = self.listing.get(addr, ("", ""))
        return op == "jr" and args.strip() != "ra"


_FLOW = [None, None]


def flow_of(listing, word_at, base, image_end, link_map):
    """The Flow of the last listing asked about (built once per image)."""
    if _FLOW[0] is not listing:
        _FLOW[:] = [listing, Flow(listing, word_at, base, image_end, link_map)]
    return _FLOW[1]


class MapError(Exception):
    pass


MAP_LINE = re.compile(r"^([0-9a-f]+) +([0-9a-f]+) +([0-9a-f]+) +(\d+) (.*)$")


class LinkMap:
    """Function bounds and the trampoline array from ld.lld's `-Map` output
    for the link that made an image (psoxide.ld's layout). Symbols sit 16
    columns in, input sections 8."""

    def __init__(self, path):
        symbols, sections, text, trampolines = [], [], {}, None
        with open(path, errors="replace") as lines:
            text_lines = lines.read().splitlines()
        for line in text_lines:
            m = MAP_LINE.match(line)
            if not m:
                continue
            address, size = int(m.group(1), 16), int(m.group(3), 16)
            rest = m.group(5)
            depth = len(rest) - len(rest.lstrip(" "))
            name = rest.strip()
            if name in ("__text_start = .", "__text_end = .", "__bss_start = ."):
                text[name.split()[0]] = address
            elif depth == 16 and name == "HAZARD_TRAMPOLINES":
                trampolines = (address, address + size)
            elif depth == 8 and name.endswith(")") and ":(.text" in name:
                sections.append((address, size))
            elif depth == 16 and not name.startswith(".L") and " = " not in name:
                symbols.append((address, size, name))
        if "__text_start" not in text or "__text_end" not in text:
            raise MapError(f"{path}: no __text_start/__text_end, not an ld.lld map of psoxide.ld")
        lo, hi = text["__text_start"], text["__text_end"]
        self.path = path
        self.text = (lo, hi)
        self.bss = text.get("__bss_start")
        self.trampolines = trampolines
        self.names = {}
        for address, size, name in symbols:
            if lo <= address < hi:
                self.names.setdefault(address, []).append((size, name))
        sections = [s for s in sections if lo <= s[0] < hi]
        # Every symbol or section start ends the function before it.
        self.bounds = sorted(set(self.names) | {a for a, _ in sections} | {s + n for s, n in sections} | {hi})
        self.starts = sorted(self.names)

    def check(self, data):
        """Refuse a map from another link (a stale one, or another example's):
        the header's payload size is __bss_start - __text_start."""
        base = load_address(data)
        lo = self.text[0]
        if data[:8] == b"PS-X EXE" and self.bss is not None:
            payload = struct.unpack_from("<I", data, 0x1C)[0]
            if payload != self.bss - lo or base != lo:
                raise MapError(f"{self.path} does not describe this image (payload {payload:#x}, map says "
                               f"{self.bss - lo:#x} at {lo:#x}); relink so both come from one link")

    def function(self, addr):
        """(start, end, name) of the function containing `addr`. A function
        is a named symbol, or runs from `addr` to the next boundary."""
        starts = self.starts
        i = bisect.bisect_right(starts, addr) - 1
        if i >= 0:
            start = starts[i]
            size, name = max(self.names[start])
            end = start + size if size else self.bounds[bisect.bisect_right(self.bounds, start)]
            if addr < end:
                return start, end, name
        if self.text[0] <= addr < self.text[1]:
            end = self.bounds[bisect.bisect_right(self.bounds, addr)]
            return addr, end, f"<unnamed {addr:08x}>"
        return None


def find_hazards(listing, word_at=None, image_end=0, base=LOAD_ADDR, is_code=looks_like_code, link_map=None):
    """Every (branch address, op, args, slot op, slot args, consumer address,
    table entry address), in address order. The entry address is the
    jump-table word that names the consumer for a `jr` switch dispatch and
    None otherwise. The consumer is None for a register jump whose
    destination is not in the image: `jr ra`, `jalr`, and a `jr` whose table
    cannot be resolved. Nothing here can prove those safe, so each counts.

    `is_code` is the data guard. Both CLIs pass the `looks_like_code` they
    look up at call time, so a game that loads either file as a module and
    replaces it (to never skip proven .text) still reaches the detector.
    `link_map`, a LinkMap of the same link, makes `jump_table` prove each
    table and bound it to its own function."""
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
            entries = None
            if word_at and link_map is None:
                entries = jump_table(listing, addr, word_at, image_end, base)
            elif word_at:
                entries = jump_table(listing, addr, word_at, image_end, base, link_map)
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


def branch_sources(op, args):
    parts = [p.strip() for p in args.split(",")]
    return [p for p in parts if not p.startswith("0x")]


def encode_j(target, link=False):
    return ((3 if link else 2) << 26) | ((target >> 2) & 0x03FFFFFF)


def cli_args(argv):
    """(paths, --check given, --map path or None), or None when malformed."""
    paths, check_only, map_path = [], False, None
    it = iter(argv)
    for arg in it:
        if arg == "--check":
            check_only = True
        elif arg == "--map":
            map_path = next(it, None)
            if map_path is None:
                return None
        elif arg.startswith("--"):
            continue
        else:
            paths.append(arg)
    return paths, check_only, map_path


def open_map(map_path, data):
    """The LinkMap for `data`, or None; exits on a map from another link."""
    if map_path is None:
        return None
    try:
        link_map = LinkMap(map_path)
        link_map.check(bytes(data))
    except (MapError, OSError) as error:
        print(error)
        sys.exit(1)
    return link_map


def main():
    args = cli_args(sys.argv[1:])
    if args is None or len(args[0]) != 1:
        print(__doc__)
        return 2
    (path,), check_only, map_path = args
    data = bytearray(open(path, "rb").read())
    link_map = open_map(map_path, data)
    base = load_address(data)
    listing = disassemble(path, base)
    image_end = base + len(data) - HEADER

    def word_at(addr):
        off = addr - base + HEADER
        return struct.unpack_from("<I", data, off)[0]

    hazards = find_hazards(listing, word_at, image_end, base, looks_like_code, link_map)
    for h in hazards:
        via = " via table entry %08x" % h[6] if h[6] is not None else ""
        if h[5] is not None:
            where = "%08x" % h[5]
        elif h[1] == "jalr":
            where = "the callee"
        elif h[2].strip() == "ra":
            where = "the caller"
        else:
            where = "the jump target (table not resolved)"
        print("hazard %08x: %s %s | slot %s %s | consumer %s%s" % (h[0], h[1], h[2], h[3], h[4], where, via))
    if not hazards:
        print("0 hazards in %s" % path)
        return 0
    if check_only:
        print("%d hazards in %s" % (len(hazards), path))
        return 1

    def put_word(addr, value):
        off = addr - base + HEADER
        struct.pack_into("<I", data, off, value)

    # The trampoline array: magic, capacity, then free words.
    area = None
    for off in range(HEADER, len(data) - 8, 4):
        if struct.unpack_from("<I", data, off)[0] == MAGIC:
            capacity = struct.unpack_from("<I", data, off + 4)[0]
            if 0 < capacity <= 4096:
                area = (base + off - HEADER + 8, capacity)
                break
    if area is None:
        print("no HAZARD_TRAMPOLINES array (magic %#x) in %s" % (MAGIC, path))
        return 1
    area_start, capacity = area
    # An earlier pass may have used the area. Its trampolines contain nops,
    # so the first zero word is not free space: resume after the last
    # non-zero word plus the nop in its delay slot (every trampoline ends
    # with a jump and one nop).
    used = [i for i in range(capacity) if word_at(area_start + i * 4) != 0]
    cursor = used[-1] + 2 if used else 0

    nop = 0
    patched = 0
    # Diagnostics: HAZARD_PATCH_SKIP="80012298,8008c30c" leaves those sites
    # alone, HAZARD_PATCH_ONLY="..." patches nothing else. Both hex addresses.
    skip = {int(a, 16) for a in os.environ.get("HAZARD_PATCH_SKIP", "").split(",") if a}
    only = {int(a, 16) for a in os.environ.get("HAZARD_PATCH_ONLY", "").split(",") if a}
    # A conditional branch whose slot load is consumed on both paths is one
    # site, not two: its trampoline already covers the target and the
    # fall-through, and patching it twice would rewrite the first `j TRAMP`.
    seen = set()
    # Table entries pointing at the same consumer share one trampoline.
    table_trampolines = {}
    for addr, op, args, slot_op, slot_args, consumer, entry in hazards:
        if entry is not None:
            if addr in skip or (only and addr not in only):
                print("left alone %08x (diagnostic request)" % addr)
                continue
            tramp = table_trampolines.get(consumer)
            if tramp is None:
                tramp = area_start + cursor * 4
                words = [nop, encode_j(consumer), nop]
                if cursor + len(words) > capacity:
                    print("trampoline array full at %08x (%d words)" % (addr, capacity))
                    return 1
                for i, w in enumerate(words):
                    put_word(tramp + i * 4, w)
                cursor += len(words)
                table_trampolines[consumer] = tramp
            put_word(entry, tramp)
            patched += 1
            print("patched table entry %08x (jr at %08x) -> trampoline %08x -> %08x" % (entry, addr, tramp, consumer))
            continue
        if addr in seen:
            continue
        seen.add(addr)
        if addr in skip or (only and addr not in only):
            print("left alone %08x (diagnostic request)" % addr)
            continue
        rd = load_destination(slot_op, slot_args)
        if op in ("jr", "jalr"):
            # Move the load out of the slot: the trampoline runs it, then
            # makes the original jump with a nop in its slot.
            parts = [p.strip() for p in args.split(",")]
            jump_reg = parts[-1]
            link_reg = parts[0] if op == "jalr" and len(parts) == 2 else "ra"
            if rd == jump_reg or (op == "jalr" and rd == link_reg):
                print("cannot patch %08x: the slot load writes the jump or link register (%s)" % (addr, rd))
                return 1
            if op == "jalr" and reads(slot_op, slot_args, link_reg):
                print("cannot patch %08x: the slot load reads the link register (%s), which jalr "
                      "writes before its slot runs; hoisted ahead of the jalr it would read the old value"
                      % (addr, link_reg))
                return 1
            words = [word_at(addr + 4), word_at(addr), nop]
            if op == "jalr":
                # The callee returns into the trampoline, which goes back
                # to the original return address.
                words += [encode_j(addr + 8), nop]
            tramp = area_start + cursor * 4
            if cursor + len(words) > capacity:
                print("trampoline array full at %08x (%d words)" % (addr, capacity))
                return 1
            for i, w in enumerate(words):
                put_word(tramp + i * 4, w)
            cursor += len(words)
            put_word(addr, encode_j(tramp))
            put_word(addr + 4, nop)
            patched += 1
            print("patched %s %s at %08x -> trampoline %08x (load %s %s)" % (op, args, addr, tramp, slot_op, slot_args))
            continue
        if op in ("bltzal", "bgezal", "bal"):
            print("cannot patch %08x: %s links inside the trampoline" % (addr, op))
            return 1
        if op in COND and rd in branch_sources(op, args):
            print("cannot patch %08x: the slot load writes a branch source (%s)" % (addr, rd))
            return 1
        target = int(re.search(r"0x([0-9a-f]+)$", args).group(1), 16)
        original = word_at(addr)
        tramp = area_start + cursor * 4
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
    remaining = find_hazards(disassemble(path, base), word_at, image_end, base, looks_like_code, link_map)
    for h in remaining:
        print("still hazardous %08x: %s %s" % (h[0], h[1], h[2]))
    print("%d patched, %d remaining, %d/%d trampoline words used in %s" % (patched, len(remaining), cursor, capacity, path))
    if skip or only:
        return 0
    return 1 if remaining else 0


if __name__ == "__main__":
    sys.exit(main())
