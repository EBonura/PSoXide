#!/usr/bin/env python3
"""Fixtures for tools/stack_guard.py.

Each case assembles a tiny PS-EXE plus the ld.lld map that would describe it,
with a psx-rt scratchpad stack entry at the root of a small call tree, and
checks the depth the guard computes or the reason it refuses. The encoders
and the PS-EXE writer come from test_hazard_tools.py.

Needs a MIPS objdump: OBJDUMP, mipsel-none-elf-objdump or
mipsel-linux-gnu-objdump.
"""
import importlib.util
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fx = load("test_hazard_tools")  # also picks OBJDUMP
guard = load("stack_guard")

BASE = 0x80010000
NOP = fx.NOP


def entry(start, end, closure="t::f"):
    return f"<psx_rt::scratchpad::ScratchpadStack<{start}, {end}>>::stack_entry::<u32, {closure}>"


def prologue(frame):
    return [fx.addiu("sp", "sp", -frame)] if frame else []


def epilogue(frame):
    return [fx.jr("ra"), fx.addiu("sp", "sp", frame) if frame else NOP]


def or_(rd, rs, rt):
    return fx.r_type(0x25, rs, rt, rd)


def sll(rd, rt, sa):
    return fx.REG[rt] << 16 | fx.REG[rd] << 11 | sa << 6


def sw(rt, off, rs):
    return fx.i_type(0x2B, rs, rt, off)


def b(offset):
    return fx.beq("zero", "zero", offset)


def dispatch(index, base, table, rd="at"):
    """`sll ; addu ; lw ; nop ; jr ; nop`: a switch on `index` through the
    table at `table`, whose %hi is already in `base`."""
    return [sll(rd, index, 2), fx.addu(rd, rd, base), fx.lw(rd, fx.lo(table), rd), NOP, fx.jr(rd), NOP]


class Fixture:
    """Functions laid out every 0x40 bytes from BASE, then a HAZARD_TRAMPOLINES
    array at +0xC00; writes the exe and a matching map."""
    SLOT = 0x40

    def __init__(self):
        self.image = fx.Image(BASE)
        self.functions = []  # (address, size, name)
        self.sections = []  # (address, size, input section) past .text
        self.trampoline_words = 0

    def addr(self, index):
        return BASE + index * self.SLOT

    def function(self, index, name, words, slots=1):
        assert len(words) * 4 <= self.SLOT * slots, name
        self.image.put(index * self.SLOT, *words)
        self.functions.append((self.addr(index), len(words) * 4, name))
        return self.addr(index)

    def data(self, offset, *words, section=".rodata", owner=None):
        """Words at `offset` past .text (0x800..0xC00), such as a jump table,
        as one input section of the map: `.rodata`, where LLVM puts switch
        tables, unless `section` says otherwise. With `owner`, a function's
        address, the section is that function's own `.rodata.<function>`."""
        assert 0x800 < offset and offset + 4 * len(words) <= fx.Image.TRAMPOLINES
        self.image.put(offset, *words)
        self.sections.append((BASE + offset, 4 * len(words), owner or f"{section}.d{len(self.sections)}"))
        return BASE + offset

    def trampoline(self, *words):
        offset = fx.Image.TRAMPOLINES + 8 + self.trampoline_words * 4
        self.image.put(offset, *words)
        self.trampoline_words += len(words)
        return BASE + offset

    def write(self, directory, text_end_index=0x20, payload_skew=0, layout_skew=0):
        """`payload_skew` misstates the map's payload size; `layout_skew`
        moves every function and the trampoline array in the map but keeps
        the payload, like a stale map of a relink."""
        exe = os.path.join(directory, "fixture.exe")
        self.image.write(exe)
        payload = len(self.image.words) * 4
        text_end = BASE + text_end_index * self.SLOT
        lines = ["     VMA      LMA     Size Align Out     In      Symbol"]

        def row(address, size, depth, name, align=1):
            lines.append(f"{address:8x} {address:8x} {size:8x} {align:5d} " + " " * depth + name)

        row(BASE, 0, 8, "__text_start = .")
        keys = {}
        for i, (address, size, name) in enumerate(sorted(self.functions)):
            keys[address] = f"f{i}"
            row(address + layout_skew, size, 8, f"/fixture.o:(.text.f{i})", 4)
            row(address + layout_skew, size, 16, name)
        row(text_end, 0, 8, "__text_end = .")
        for address, size, section in sorted(self.sections, key=lambda s: s[0]):
            section = f".rodata.{keys[section]}" if isinstance(section, int) else section
            row(address, size, 8, f"/fixture.o:({section})", 4)
        tramp = BASE + fx.Image.TRAMPOLINES + layout_skew
        row(tramp, 8 + 64 * 4, 8, "/fixture.o:(.data.HAZARD_TRAMPOLINES)", 4)
        row(tramp, 8 + 64 * 4, 16, "HAZARD_TRAMPOLINES")
        row(BASE + payload + payload_skew, 0, 8, "__bss_start = .")
        map_path = os.path.join(directory, "fixture.map")
        Path(map_path).write_text("\n".join(lines) + "\n")
        return exe, map_path


@unittest.skipUnless(os.environ.get("OBJDUMP") and shutil.which(os.environ["OBJDUMP"]), "no MIPS objdump")
class StackGuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.fixture = Fixture()

    def tearDown(self):
        self.tmp.cleanup()

    def run_guard(self, *extra, **write):
        exe, map_path = self.fixture.write(self.tmp.name, **write)
        out = io.StringIO()
        pattern = extra[0] if extra else None
        budget = extra[1] if extra else None
        failures = guard.check(exe, map_path, pattern, budget, out=out)
        return failures, out.getvalue()

    def leaf(self, index, name, frame):
        return self.fixture.function(index, name, prologue(frame) + epilogue(frame))

    def caller(self, index, name, frame, *callees):
        body = prologue(frame)
        for callee in callees:
            body += [fx.jal(callee), NOP]
        return self.fixture.function(index, name, body + epilogue(frame))

    def test_sums_frames_down_the_deepest_path(self):
        b = self.leaf(3, "t::b", 24)
        c = self.leaf(4, "t::c", 8)
        a = self.caller(2, "t::a", 40, b)
        self.caller(1, entry(512, 1024), 16, a, c)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("80 of 492 bytes (region 512..1024", out)
        self.assertIn("t::a(40) > t::b(24)", out)

    def test_a_tree_deeper_than_the_region_fails(self):
        b = self.leaf(3, "t::b", 24)
        a = self.caller(2, "t::a", 40, b)
        self.caller(1, entry(960, 1024), 16, a)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("FAIL", out)
        self.assertIn("80 of 44 bytes", out)

    def test_recursion_is_refused(self):
        a_addr = self.fixture.addr(2)
        b = self.caller(3, "t::b", 8, a_addr)
        self.caller(2, "t::a", 8, b)
        self.caller(1, entry(0, 1024), 8, a_addr)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("recurses", out)

    def test_calls_through_a_register_are_refused(self):
        self.fixture.function(2, "t::dyn_call", prologue(8) + [fx.jalr("t9"), NOP] + epilogue(8))
        self.caller(1, entry(0, 1024), 8, self.fixture.addr(2))
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("calls through a register", out)

    def test_bios_style_register_jumps_are_refused(self):
        bios = self.fixture.function(2, "__bios_putchar", [fx.addiu("t0", "zero", 0xA0), fx.jr("t0"),
                                                          fx.addiu("t1", "zero", 0x3C)])
        self.caller(1, entry(0, 1024), 8, bios)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("not a jump table", out)

    def test_other_stack_pointer_writes_are_refused(self):
        self.fixture.function(2, "t::alloca", [fx.addu("sp", "sp", "t0")] + epilogue(0))
        self.caller(1, entry(0, 1024), 8, self.fixture.addr(2))
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("sets $sp", out)

    def test_hazard_trampolines_are_followed(self):
        b = self.leaf(3, "t::b", 200)
        tramp = self.fixture.trampoline(NOP, fx.j(b), NOP)
        # `jal TRAMP` as hazard_patch.py leaves a patched call.
        self.fixture.function(2, "t::a", prologue(16) + [fx.jal(tramp), NOP] + epilogue(16))
        self.caller(1, entry(0, 1024), 8, self.fixture.addr(2))
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("224 of 1004 bytes", out)

    def test_conditional_trampolines_count_both_exits(self):
        b = self.leaf(3, "t::b", 64)
        a_addr = self.fixture.addr(2)
        # bXX +3 ; nop ; j FALL ; nop ; j T ; nop, with FALL inside t::a.
        tramp = self.fixture.trampoline(fx.beq("a0", "zero", 3), NOP, fx.j(a_addr + 8), NOP, fx.j(b), NOP)
        self.fixture.function(2, "t::a", prologue(16) + [fx.j(tramp), NOP] + epilogue(16))
        self.caller(1, entry(0, 1024), 8, a_addr)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("88 of 1004 bytes", out)

    def test_switch_and_panic_handler_count_only_their_own_frames(self):
        deep = self.leaf(5, "t::deep_report", 900)
        # The real switch contains a jalr and moves $sp; neither may fail it.
        switch = self.fixture.function(3, "__psx_rt_call_on_stack", [
            fx.addiu("sp", "sp", -24), or_("s0", "sp", "zero"), fx.jalr("t9"), or_("sp", "a2", "zero"),
            or_("sp", "s0", "zero"), fx.jr("ra"), fx.addiu("sp", "sp", 24)])
        handler = self.caller(4, "__rustc::rust_begin_unwind", 32, deep)
        self.caller(1, entry(0, 1024), 8, switch, handler)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("40 of 1004 bytes", out)

    def test_a_map_from_another_link_is_refused(self):
        self.caller(1, entry(0, 1024), 8)
        failures, out = self.run_guard(payload_skew=0x800)
        self.assertEqual(failures, 1, out)
        self.assertIn("map does not match this image", out)

    def test_a_stale_map_with_the_same_payload_is_refused(self):
        # A relink moved every function and the trampoline array by 0x20
        # bytes but kept the 2 KiB aligned payload, so the old size check
        # passed; a stale Quake map cut 11 jump tables short this way.
        self.fixture.function(0, "_start", [fx.jal(self.fixture.addr(1)), NOP, fx.j(BASE), NOP])
        self.caller(1, entry(0, 1024), 8, self.leaf(2, "t::leaf", 8))
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        failures, out = self.run_guard(layout_skew=0x20)
        self.assertEqual(failures, 1, out)
        self.assertIn("map does not match this image", out)
        self.assertIn(f"entry point {BASE:08x}, map's _start {BASE + 0x20:08x}", out)
        self.assertIn(f"no HAZARD_TRAMPOLINES magic at the map's {BASE + fx.Image.TRAMPOLINES + 0x20:08x}", out)
        self.assertIn(f"2 calls to no function the map names, first jal {BASE + 0x40:08x} at {BASE:08x}", out)
        exe, map_path = self.fixture.write(self.tmp.name, layout_skew=0x20)
        for tool, args in (("hazard_patch.py", ["--check"]), ("hazard_patch.py", []), ("hazard_scan.py", [])):
            run = subprocess.run([sys.executable, str(TOOLS / tool), exe, *args, "--map", map_path],
                                 capture_output=True, text=True)
            self.assertEqual(run.returncode, 1, run.stdout)
            self.assertIn("map does not match this image", run.stdout)

    def test_a_functions_own_rodata_must_hold_its_jump_table(self):
        # LLVM writes a function's jump tables to `.rodata.<its section>`,
        # so every word there lands in that function, or in a trampoline
        # once patched. A map that puts the section over other words (here
        # one naming another function) is from another link.
        a, cases = self.switch(2, "t::a", 16, BASE + 0x900)
        other = self.leaf(4, "t::other", 8)
        self.caller(1, entry(0, 1024), 8, a, other)
        tramp = self.fixture.trampoline(NOP, fx.j(cases[1]), NOP)
        self.fixture.data(0x900, cases[0], tramp, owner=a)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.fixture.sections.clear()
        self.fixture.data(0x900, cases[0], other, owner=a)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn(f"1 jump table words outside their function, first {BASE + 0x904:08x} holds {other:08x}, "
                      f"not in the function at {a:08x}", out)

    def test_custom_roots_take_a_budget(self):
        b = self.leaf(3, "t::b", 100)
        self.caller(1, "game::projection_entry", 16, b)
        failures, out = self.run_guard(r"^game::projection_entry$", 100)
        self.assertEqual(failures, 1, out)
        self.assertIn("116 of 100 bytes", out)

    def switch(self, index, name, frame, table, cases=2):
        """A leaf that switches through `table` and returns from every case.
        Returns (address, case addresses)."""
        addr = self.fixture.addr(index)
        body = prologue(frame) + [fx.lui("t0", fx.hi(table))] + dispatch("a0", "t0", table)
        cases_at = []
        for _ in range(cases):
            cases_at.append(addr + 4 * len(body))
            body += epilogue(frame)
        self.fixture.function(index, name, body)
        return addr, cases_at

    def test_a_table_stops_at_the_next_functions_table(self):
        # The tables sit back to back, as in .rodata. Read on, t::a's table
        # names t::b's cases, and t::b's 600-byte frame lands in t::a's tree
        # (hk-psx: presentation::service's table ran into menu::run's). The
        # second word of t::b's table is a trampoline into t::b, as an
        # earlier patch leaves it: still not t::a's.
        table_a, table_b = BASE + 0x900, BASE + 0x908
        a, cases_a = self.switch(2, "t::a", 16, table_a)
        b_addr, cases_b = self.switch(4, "t::b", 600, table_b)
        tramp = self.fixture.trampoline(NOP, fx.j(cases_b[1]), NOP)
        self.fixture.data(0x900, *cases_a, tramp, cases_b[1])
        self.caller(1, entry(0, 1024), 8, a)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("24 of 1004 bytes", out)
        self.assertNotIn("t::b", out)
        image = guard.Image(*self.fixture.write(self.tmp.name))
        entries = guard.jump_table(image.listing, a + 24, image.word_at, image.image_end, image.base, image.map)
        self.assertEqual(entries, [(table_a, cases_a[0]), (table_a + 4, cases_a[1])])

    def far_base(self, base_reg, call=True, filler=12):
        """t::far loads its table's %hi at the top, calls a leaf, branches,
        and dispatches more than the old 11-instruction window later."""
        leaf = self.leaf(6, "t::leaf", 40)
        far = self.fixture.addr(2)
        table = BASE + 0x900
        body = prologue(24) + [fx.lui(base_reg, fx.hi(table)), sw("ra", 20, "sp")]
        body += [fx.jal(leaf), NOP] if call else [NOP, NOP]
        body += [fx.beq("a1", "zero", filler + 1), NOP] + [fx.addiu("v0", "v0", 1)] * filler
        body += dispatch("a0", base_reg, table)
        cases = []
        for _ in range(2):
            cases.append(far + 4 * len(body))
            body += [fx.lw("ra", 20, "sp"), fx.jr("ra"), fx.addiu("sp", "sp", 24)]
        self.fixture.function(2, "t::far", body, slots=3)
        self.fixture.data(0x900, *cases)
        self.caller(1, entry(0, 1024), 8, far)
        return self.run_guard()

    def test_a_table_base_loaded_far_away_is_followed_back(self):
        # State::apply keeps its table's %hi in s8 for the whole loop.
        failures, out = self.far_base("s0")
        self.assertEqual(failures, 0, out)
        self.assertIn("72 of 1004 bytes", out)
        self.assertIn("t::far(24) > t::leaf(40)", out)

    def test_a_caller_saved_base_across_a_call_stays_unresolved(self):
        # t0 does not survive the call, so no write of it reaches the use.
        failures, out = self.far_base("t0")
        self.assertEqual(failures, 1, out)
        self.assertIn("not a jump table it can prove", out)

    def test_a_base_from_the_caller_stays_unresolved(self):
        table = BASE + 0x900
        addr = self.fixture.addr(2)
        body = dispatch("a0", "a1", table)
        self.fixture.function(2, "t::from_arg", body + epilogue(0) + epilogue(0))
        self.fixture.data(0x900, addr + 24, addr + 32)
        self.caller(1, entry(0, 1024), 8, addr)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("not a jump table it can prove", out)

    def test_a_table_of_function_pointers_is_not_a_switch(self):
        # A tail call through a constant table of functions leaves t::tail.
        other = self.leaf(4, "t::other", 200)
        table = BASE + 0x900
        tail = self.fixture.function(2, "t::tail", [fx.lui("t0", fx.hi(table))] + dispatch("a0", "t0", table))
        self.fixture.data(0x900, other, other)
        self.caller(1, entry(0, 1024), 8, tail)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("not a jump table it can prove", out)

    def code_pointer_dispatch(self, *sections):
        """t::a dispatches through `lw` from BASE + 0x900 with `lw v0` in the
        jr's slot; its second case reads v0 at once. `sections` lays the
        words from 0x900 out as (section, case indexes), and the result is
        (exe, map, jr address, cases)."""
        self.fixture = Fixture()
        a = self.fixture.addr(2)
        table = BASE + 0x900
        body = [fx.lui("t0", fx.hi(table))] + dispatch("a0", "t0", table)
        body[-1] = fx.lw("v0", 0, "a1")
        cases = [a + 4 * len(body), a + 4 * len(body) + 8]
        self.fixture.function(2, "t::a", body + epilogue(0) + [fx.addu("v1", "v0", "zero")] + epilogue(0))
        self.caller(1, entry(0, 1024), 8, a)
        offset = 0x900
        for section, indexes in sections:
            self.fixture.data(offset, *(cases[i] for i in indexes), section=section)
            offset += 4 * len(indexes)
        exe, map_path = self.fixture.write(self.tmp.name)
        return exe, map_path, a + 4 * (len(body) - 2), cases

    def patch_with_map(self, exe, map_path, *args):
        return subprocess.run([sys.executable, str(TOOLS / "hazard_patch.py"), exe, "--map", map_path, *args],
                              capture_output=True, text=True).stdout

    def test_a_mutable_array_of_code_pointers_is_not_a_switch(self):
        # A `static mut` array in .data indexed by `lw` reads like a switch
        # table whose entries land inside t::a, but the image only holds its
        # initial words. In .rodata the second entry is proven and patched to
        # a trampoline; in .data the dispatch stays unresolved, so the slot
        # load moves out of the slot and the array is left as it was.
        exe, map_path, jr_at, cases = self.code_pointer_dispatch((".rodata", (0, 1)))
        self.assertIn(f"via table entry {BASE + 0x904:08x}", self.patch_with_map(exe, map_path, "--check"))
        exe, map_path, jr_at, cases = self.code_pointer_dispatch((".data", (0, 1)))
        image = guard.Image(exe, map_path)
        self.assertIsNone(guard.jump_table(image.listing, jr_at, image.word_at, image.image_end, image.base,
                                           image.map))
        out = self.patch_with_map(exe, map_path, "--check")
        self.assertIn(f"hazard {jr_at:08x}: jr at | slot lw v0,0(a1) | consumer the jump target (table not "
                      f"resolved)", out)
        self.assertNotIn("via table entry", out)
        out = self.patch_with_map(exe, map_path)
        self.assertIn(f"patched jr at at {jr_at:08x}", out)
        self.assertIn("0 remaining", out)
        self.assertEqual([guard.Image(exe, map_path).word_at(BASE + 0x900 + 4 * i) for i in range(2)], cases)
        failures, out = self.run_guard()
        self.assertEqual(failures, 1, out)
        self.assertIn("not a jump table it can prove", out)

    def test_a_table_ends_with_its_rodata_section(self):
        # The words right after the table name t::a's case too, but they are
        # a .data array: the table stops at its section's end and the array
        # word is neither an entry nor patched.
        exe, map_path, jr_at, cases = self.code_pointer_dispatch((".rodata", (0, 0)), (".data", (1,)))
        image = guard.Image(exe, map_path)
        entries = guard.jump_table(image.listing, jr_at, image.word_at, image.image_end, image.base, image.map)
        self.assertEqual(entries, [(BASE + 0x900, cases[0]), (BASE + 0x904, cases[0])])
        out = self.patch_with_map(exe, map_path, "--check")
        self.assertIn("0 hazards", out)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)

    def test_a_switch_inside_another_switchs_case(self):
        # The second dispatch's block is only entered through the first
        # table, so it resolves once that table is proven (apply_arena).
        nested = self.fixture.addr(2)
        table_1, table_2 = BASE + 0x900, BASE + 0x908
        body = prologue(16) + [fx.lui("t2", fx.hi(table_1))] + [fx.addiu("v0", "v0", 1)] * 12
        body += dispatch("a0", "t2", table_1)
        inner = nested + 4 * len(body)
        body += dispatch("a1", "t2", table_2)
        done = nested + 4 * len(body)
        body += epilogue(16)
        self.fixture.function(2, "t::nested", body, slots=2)
        self.fixture.data(0x900, inner, done, done, done)
        self.caller(1, entry(0, 1024), 8, nested)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("24 of 1004 bytes", out)

    def after_noreturn(self, base):
        """t::after_panic sets `base` to its table's %hi, but on one path
        moves an argument into it and calls t::panic, which never returns;
        the word after that call is a case block that loops back to the
        dispatch. That fall-through is not an edge: following it would find
        `move base, a0` and refuse the switch."""
        panic = self.fixture.function(6, "t::panic", prologue(8) + [b(-1), NOP])
        fn = self.fixture.addr(2)
        table = BASE + 0x900
        body = prologue(24) + [sw("ra", 20, "sp"), fx.lui(base, fx.hi(table))]
        branch = len(body)
        body += [fx.beq("a1", "zero", 0), NOP, or_(base, "a0", "zero"), fx.jal(panic), NOP]
        case_loop = fn + 4 * len(body)
        body += [fx.addiu("a1", "a1", -1), b(0), NOP]
        back = len(body) - 2
        case_exit = fn + 4 * len(body)
        body += [fx.lw("ra", 20, "sp"), fx.jr("ra"), fx.addiu("sp", "sp", 24)]
        top = len(body)
        body += dispatch("a1", base, table)
        body[branch] = fx.beq("a1", "zero", top - branch - 1)
        body[back] = b(top - back - 1)
        self.fixture.function(2, "t::after_panic", body, slots=2)
        self.fixture.data(0x900, case_loop, case_exit)
        self.caller(1, entry(0, 1024), 8, fn)
        failures, out = self.run_guard()
        self.assertEqual(failures, 0, out)
        self.assertIn("40 of 1004 bytes", out)

    def test_the_word_after_a_noreturn_call_is_not_its_return(self):
        # A callee-saved base survives calls that return, so only knowing
        # t::panic does not return drops the path.
        self.after_noreturn("s0")

    def test_no_path_carries_a_caller_saved_base_across_a_call(self):
        # The callee may change t3, so no value of it reaches back past the
        # call, returning or not (hk-psx State::apply keeps a base in ra).
        self.after_noreturn("t3")

    def test_the_patcher_bounds_tables_with_a_map(self):
        # t::a's switch loads v0 in its delay slot. t::b's first case reads
        # v0 at once, so a table that reads on names that case as a consumer
        # of t::a's load: a harmless extra trampoline. Without a map a table
        # stops where another dispatch's table starts, which bounds t::a's
        # while t::b's dispatch shows its own base; once t::b keeps that base
        # across a branch, only the map proves t::b and so bounds t::a. The
        # scanner agrees with the patcher every time.
        spurious = f"via table entry {BASE + 0x908:08x}"
        count = lambda out: re.search(r"(\d+) hazards in", out).group(1)
        for across_branch in (False, True):
            runs = self.two_switches(across_branch)
            self.assertEqual(spurious in runs["hazard_patch.py", False], across_branch, runs)
            self.assertNotIn(spurious, runs["hazard_patch.py", True])
            self.assertIn("0 hazards", runs["hazard_patch.py", True])
            for with_map in (False, True):
                self.assertEqual(count(runs["hazard_patch.py", with_map]), count(runs["hazard_scan.py", with_map]),
                                 runs)

    def two_switches(self, across_branch):
        """The patcher's --check and the scanner's output for t::a and t::b,
        with and without the map."""
        self.fixture = Fixture()
        table_a, table_b = BASE + 0x900, BASE + 0x908
        a = self.fixture.addr(2)
        body = [fx.lui("t0", fx.hi(table_a))] + dispatch("a0", "t0", table_a)
        body[-1] = fx.lw("v0", 0, "a1")
        cases_a = [a + 4 * len(body), a + 4 * len(body) + 8]
        self.fixture.function(2, "t::a", body + epilogue(0) + epilogue(0))
        b_addr = self.fixture.addr(4)
        body = [fx.lui("t0", fx.hi(table_b))]
        if across_branch:
            body += [fx.beq("a1", "zero", 1), NOP]  # to the dispatch, which is then a label
        body += dispatch("a0", "t0", table_b)
        case_b = b_addr + 4 * len(body)
        self.fixture.function(4, "t::b", body + [fx.addu("v1", "v0", "zero")] + epilogue(0))
        self.fixture.data(0x900, *cases_a, case_b, case_b)
        exe, map_path = self.fixture.write(self.tmp.name)
        runs = {}
        for tool in ("hazard_patch.py", "hazard_scan.py"):
            for extra in ([], ["--map", map_path]):
                args = [exe, "--check"] if tool == "hazard_patch.py" else [exe]
                runs[tool, bool(extra)] = subprocess.run([sys.executable, str(TOOLS / tool), *args, *extra],
                                                          capture_output=True, text=True).stdout
        return runs

    def test_without_a_map_only_the_switch_is_looked_for(self):
        self.caller(1, "t::main", 8)
        exe, _ = self.fixture.write(self.tmp.name)
        out = io.StringIO()
        self.assertEqual(guard.check(exe, None, out=out), 0, out.getvalue())
        self.fixture.function(3, "__psx_rt_call_on_stack", [fx.jalr("t9"), or_("sp", "a2", "zero")] + epilogue(0))
        exe, _ = self.fixture.write(self.tmp.name)
        out = io.StringIO()
        self.assertEqual(guard.check(exe, None, out=out), 1, out.getvalue())
        self.assertIn("pass its link map", out.getvalue())


if __name__ == "__main__":
    unittest.main()
