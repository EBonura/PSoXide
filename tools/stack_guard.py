#!/usr/bin/env python3
"""Prove every scratchpad stack call tree in a linked PS-EXE fits its region.

`psx_rt::scratchpad::ScratchpadStack::<START, END>::run(f)` runs `f` with $sp
in scratchpad bytes START..END. Nothing at run time can stop a deep call tree
from running off the bottom of the region into another scratchpad user's
bytes (or off the scratchpad), and an inlining change can grow a tree by
hundreds of bytes without any source change (a profile-guided hl-psx build
grew its projection chain from 568 to 824). This walks the linked image's
static call graph from every monomorphised entry,

    <psx_rt::scratchpad::ScratchpadStack<START, END>>::stack_entry::<R, F>

sums frame sizes (every `addiu sp,sp,-N`; a tail call counts as a call, which
only overestimates) down the deepest path, and fails
when the total exceeds the region minus psx-rt's 20-byte overhead (16 bytes
of o32 argument home area, one canary word). It also fails on what it cannot
bound: recursion, calls through a register (`jalr`, dyn and fn pointers),
register jumps it cannot prove are a switch (BIOS calls go through `jr` to
0xA0/0xB0/0xC0), and $sp adjusted any other way.

A switch's `jr` is proven from the map by hazard_patch.py's `jump_table`
(see its `Flow`): the table's address is followed back through the
function however far away it was loaded, and the table ends at its first
entry that leaves the function. A proven switch calls nothing, since every
entry lands inside its own function. Before, a table read on into the next
function's table, whose cases then counted as calls (hk-psx's input polling
pulled in the menu and memory card code), and a base loaded more than a few
instructions before the dispatch could not be resolved at all.

    python3 tools/stack_guard.py game.exe game.map
    python3 tools/stack_guard.py game.exe game.map --root REGEX --budget BYTES

The map is ld.lld's `-Map` output for the same link (the editor writes it
with PSOXIDE_GUEST_LINK_MAP; an SDK example's build.rs adds the flag). The
exe may be hazard-patched: calls rerouted through HAZARD_TRAMPOLINES are
followed to their targets. `--root`/`--budget` check a game's own stack
switch instead (an entry name regex and its byte budget), for code that
predates psx-rt's. Without a map the tool only checks that the image does not
contain psx-rt's stack switch, so a guest that starts using it cannot skip
the proof by not writing a map.

Two callees are counted without descending: psx-rt's switch trampoline
`__psx_rt_call_on_stack` (a nested `run` calls `f` in place at run time; the
trampoline path is never taken there, and if it were it would switch stacks)
and the panic handler (`rust_begin_unwind`), which moves $sp back to the RAM
stack before it reports. Each still counts its own frame.

Needs mipsel-none-elf-objdump on PATH, or another one named in OBJDUMP.
"""
import os
import re
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
# One disassembler and one jump-table resolver for every post-link tool.
from hazard_patch import (HEADER, READS_ALL, LinkMap, MapError, disassemble, jump_table,  # noqa: E402
                          load_address)

ENTRY = re.compile(r"^<psx_rt::scratchpad::ScratchpadStack<(\d+)(?:usize)?, (\d+)(?:usize)?>>::stack_entry::<")
STACK_OVERHEAD = 20
SWITCH = "__psx_rt_call_on_stack"
LEAVES = (SWITCH, "rust_begin_unwind", "__rustc::rust_begin_unwind")
COND = {"beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b", "bltzal", "bgezal", "bal"}


class GuardError(Exception):
    pass


class Image:
    def __init__(self, exe, map_path):
        with open(exe, "rb") as f:
            data = f.read()
        self.base = load_address(data)
        self.data = data
        self.image_end = self.base + len(data) - HEADER
        self.listing = disassemble(exe, self.base)
        try:
            self.map = LinkMap(map_path)
            self.map.check(data)
        except MapError as error:
            raise GuardError(str(error)) from None
        self.names = self.map.names
        self.text = self.map.text
        self.trampolines = self.map.trampolines

    def word_at(self, addr):
        return struct.unpack_from("<I", self.data, addr - self.base + HEADER)[0]

    def function(self, addr):
        """(start, end, name) of the function containing `addr`. A function
        is a named symbol, or runs from a call target to the next boundary."""
        return self.map.function(addr)

    def in_trampolines(self, addr):
        return self.trampolines is not None and self.trampolines[0] <= addr < self.trampolines[1]


def target(args):
    m = re.search(r"0x([0-9a-f]+)$", args)
    return int(m.group(1), 16) if m else None


def writes_sp(op, args):
    """True when the instruction's destination is $sp (stores, branches and
    coprocessor moves only read their first operand)."""
    return args.split(",")[0].strip() == "sp" and op not in READS_ALL


class Walker:
    def __init__(self, image):
        self.image = image
        self.memo = {}

    def transfers(self, start, end, addr, op, args, name):
        """Call targets outside [start, end) for one instruction."""
        if op in ("jal", "j") or op in COND:
            dest = target(args)
            if dest is None or start <= dest < end:
                return []
            if self.image.in_trampolines(dest):
                return self.trampoline(start, end, dest, name)
            return [dest]
        if op == "jalr":
            raise GuardError(f"{name} calls through a register at {addr:08x}; the callee cannot be bounded")
        if op == "jr" and args.strip() != "ra":
            entries = jump_table(self.image.listing, addr, self.image.word_at, self.image.image_end,
                                 self.image.base, self.image.map)
            if entries is None:
                raise GuardError(f"{name} jumps through a register at {addr:08x} ({op} {args}) and it is not "
                                 f"a jump table it can prove; a BIOS call or a tail call through a pointer "
                                 f"cannot be bounded")
            # A proven table ends at its first entry that leaves the function
            # (a switch never does), so a dispatch calls nothing.
            return []
        return []

    def trampoline(self, start, end, tramp, name):
        """Where a hazard_patch.py trampoline goes. `nop ; j T ; nop` stands
        for a jump, call or jump-table entry; `bXX +3 ; nop ; j FALL ; nop ;
        j T ; nop` for a conditional branch; `LOAD ; jr/jalr rs ; ...` for a
        register jump whose slot load it moved."""
        listing = self.image.listing
        first = listing.get(tramp, ("", ""))[0]
        if first in COND:
            jumps = [tramp + 8, tramp + 16]
        elif first == "nop":
            jumps = [tramp + 4]
        else:
            op, args = listing.get(tramp + 4, ("", ""))
            if op == "jr" and args.strip() == "ra":
                return []
            raise GuardError(f"{name} jumps or calls through a register via trampoline {tramp:08x} ({op} {args})")
        found = []
        for addr in jumps:
            op, args = listing.get(addr, ("", ""))
            dest = target(args) if op == "j" else None
            if dest is None:
                raise GuardError(f"{name} goes through {tramp:08x}, which is not a hazard_patch.py trampoline")
            if not start <= dest < end:
                found.append(dest)
        return found

    def depth(self, addr, path=()):
        """(bytes, chain) for the deepest path from the function at `addr`."""
        fn = self.image.function(addr)
        if fn is None:
            raise GuardError(f"call to {addr:08x}, outside .text")
        start, end, name = fn
        if start in self.memo:
            return self.memo[start]
        if start in path:
            raise GuardError(f"{name} recurses, so its depth has no bound")
        frame = 0
        for pc in range(start, end, 4):
            op, args = self.image.listing.get(pc, ("", ""))
            m = re.fullmatch(r"sp,sp,(-?\d+)", args.replace(" ", "")) if op == "addiu" else None
            if m:
                frame += max(0, -int(m.group(1)))
            elif writes_sp(op, args) and not (name == SWITCH or re.fullmatch(r"sp,(s8|fp)", args.replace(" ", ""))):
                raise GuardError(f"{name} sets $sp at {pc:08x} ({op} {args}); only addiu frames can be counted")
        deepest = (0, [])
        if not name.endswith(LEAVES):
            for pc in range(start, end, 4):
                op, args = self.image.listing.get(pc, ("", ""))
                for callee in self.transfers(start, end, pc, op, args, name):
                    below = self.depth(callee, path + (start,))
                    if below[0] > deepest[0]:
                        deepest = below
        result = (frame + deepest[0], [f"{short(name)}({frame})"] + deepest[1])
        self.memo[start] = result
        return result


def short(name):
    name = re.sub(r"::h[0-9a-f]{16}$", "", name)
    return name if len(name) <= 90 else name[:87] + "..."


def roots(image, pattern=None, budget=None):
    found = []
    for address, entries in sorted(image.names.items()):
        for _, name in entries:
            if pattern is not None:
                if re.search(pattern, name):
                    found.append((address, name, budget, None))
                continue
            m = ENTRY.match(name)
            if m:
                lo, hi = int(m.group(1)), int(m.group(2))
                found.append((address, name, hi - lo - STACK_OVERHEAD, (lo, hi)))
    return found


def has_switch(listing):
    """psx-rt's switch: `jalr t9` with `move sp,a2` in its delay slot."""
    return any(op == "jalr" and args.strip() == "t9" and listing.get(a + 4, ("", ""))[1].replace(" ", "") == "sp,a2"
               for a, (op, args) in listing.items())


def check(exe, map_path=None, pattern=None, budget=None, out=sys.stdout):
    """Print one line per entry; return the number of failures."""
    if map_path is None:
        with open(exe, "rb") as f:
            data = f.read()
        if has_switch(disassemble(exe, load_address(data))):
            print(f"stack guard: {exe} switches to a scratchpad stack; pass its link map (ld.lld -Map)", file=out)
            return 1
        print(f"stack guard: no scratchpad stack in {exe}", file=out)
        return 0
    try:
        image = Image(exe, map_path)
    except GuardError as error:
        print(f"stack guard: {error}", file=out)
        return 1
    entries = roots(image, pattern, budget)
    if not entries:
        if pattern is not None:
            print(f"stack guard: no symbol matches {pattern!r} in {map_path}", file=out)
            return 1
        if has_switch(image.listing):
            print(f"stack guard: {exe} contains psx-rt's stack switch but no ScratchpadStack entry is in "
                  f"{map_path}", file=out)
            return 1
        print(f"stack guard: no scratchpad stack entries in {exe}", file=out)
        return 0
    walker = Walker(image)
    failures = 0
    for address, name, limit, region in entries:
        where = f"region {region[0]}..{region[1]}, " if region else ""
        try:
            total, chain = walker.depth(address)
        except GuardError as error:
            print(f"FAIL {short(name)}: {error}", file=out)
            failures += 1
            continue
        verdict = "ok  " if total <= limit else "FAIL"
        print(f"{verdict} {short(name)}: {total} of {limit} bytes ({where}{address:08x}) via {' > '.join(chain)}",
              file=out)
        if total > limit:
            failures += 1
    return failures


def main(argv):
    args, pattern, budget = [], None, None
    it = iter(argv)
    for arg in it:
        if arg == "--root":
            pattern = next(it)
        elif arg == "--budget":
            budget = int(next(it), 0)
        else:
            args.append(arg)
    if not 1 <= len(args) <= 2 or (pattern is None) != (budget is None):
        print(__doc__)
        return 2
    failures = check(args[0], args[1] if len(args) == 2 else None, pattern, budget)
    if failures:
        print(f"stack guard: {failures} scratchpad stack entries fail in {args[0]}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
