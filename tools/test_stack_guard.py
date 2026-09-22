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
import shutil
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


class Fixture:
    """Functions laid out every 0x40 bytes from BASE, then a HAZARD_TRAMPOLINES
    array at +0xC00; writes the exe and a matching map."""
    SLOT = 0x40

    def __init__(self):
        self.image = fx.Image(BASE)
        self.functions = []  # (address, size, name)
        self.trampoline_words = 0

    def addr(self, index):
        return BASE + index * self.SLOT

    def function(self, index, name, words):
        assert len(words) * 4 <= self.SLOT, name
        self.image.put(index * self.SLOT, *words)
        self.functions.append((self.addr(index), len(words) * 4, name))
        return self.addr(index)

    def trampoline(self, *words):
        offset = fx.Image.TRAMPOLINES + 8 + self.trampoline_words * 4
        self.image.put(offset, *words)
        self.trampoline_words += len(words)
        return BASE + offset

    def write(self, directory, text_end_index=0x20, payload_skew=0):
        exe = os.path.join(directory, "fixture.exe")
        self.image.write(exe)
        payload = len(self.image.words) * 4
        text_end = BASE + text_end_index * self.SLOT
        lines = ["     VMA      LMA     Size Align Out     In      Symbol"]

        def row(address, size, depth, name, align=1):
            lines.append(f"{address:8x} {address:8x} {size:8x} {align:5d} " + " " * depth + name)

        row(BASE, 0, 8, "__text_start = .")
        for i, (address, size, name) in enumerate(sorted(self.functions)):
            row(address, size, 8, f"/fixture.o:(.text.f{i})", 4)
            row(address, size, 16, name)
        row(text_end, 0, 8, "__text_end = .")
        tramp = BASE + fx.Image.TRAMPOLINES
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
        self.assertIn("does not describe", out)

    def test_custom_roots_take_a_budget(self):
        b = self.leaf(3, "t::b", 100)
        self.caller(1, "game::projection_entry", 16, b)
        failures, out = self.run_guard(r"^game::projection_entry$", 100)
        self.assertEqual(failures, 1, out)
        self.assertIn("116 of 100 bytes", out)

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
