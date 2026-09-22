#!/usr/bin/env python3
"""Fixtures for tools/hazard_scan.py and tools/hazard_patch.py.

Each case assembles a tiny PS-EXE around one load-delay shape, runs it on a
small R3000 interpreter that delivers a load one instruction late, and checks
three things: the scanner and `hazard_patch.py --check` both report it, the
unpatched program reads the stale register, and after patching the rescan is
clean and the program reads the loaded value. The last check matters most:
the scanner and patcher share one detector (in hazard_patch.py) and so its
blind spots, and "0 hazards" alone would not have caught either past gap
(branch operands, 2026-09-04; `jr ra` returns, 2026-09-22). Every scan also
checks that the scanner and `hazard_patch.py --check` name the same sites, so
a filter added to one CLI cannot make them drift apart again.

Needs a MIPS objdump: OBJDUMP, mipsel-none-elf-objdump or
mipsel-linux-gnu-objdump.
"""
import importlib.util
import io
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

TOOLS = Path(__file__).parent
if "OBJDUMP" not in os.environ:
    for candidate in ("mipsel-none-elf-objdump", "mipsel-linux-gnu-objdump"):
        if shutil.which(candidate):
            os.environ["OBJDUMP"] = candidate
            break


def load(name):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


scanner = load("hazard_scan")

REG = {name: i for i, name in enumerate(
    "zero at v0 v1 a0 a1 a2 a3 t0 t1 t2 t3 t4 t5 t6 t7 "
    "s0 s1 s2 s3 s4 s5 s6 s7 t8 t9 k0 k1 gp sp fp ra".split())}
MAGIC = 0x48415A54
STALE = 0x5A1E0000  # every register starts as STALE + its number
VALUE = 0x2A        # what the slot load fetches


# The handful of encodings the fixtures need.
def r_type(funct, rs="zero", rt="zero", rd="zero"):
    return REG[rs] << 21 | REG[rt] << 16 | REG[rd] << 11 | funct


def i_type(op, rs, rt, imm):
    return op << 26 | REG[rs] << 21 | REG[rt] << 16 | (imm & 0xFFFF)


NOP = 0
BREAK = 0x0D
def addu(rd, rs, rt): return r_type(0x21, rs, rt, rd)
def jr(rs): return r_type(0x08, rs)
def jalr(rs): return r_type(0x09, rs, "zero", "ra")
def addiu(rt, rs, imm): return i_type(0x09, rs, rt, imm)
def lui(rt, imm): return i_type(0x0F, "zero", rt, imm)
def ori(rt, rs, imm): return i_type(0x0D, rs, rt, imm)
def lw(rt, off, rs): return i_type(0x23, rs, rt, off)
def lbu(rt, off, rs): return i_type(0x24, rs, rt, off)
def beq(rs, rt, offset): return i_type(0x04, rs, rt, offset)
def bne(rs, rt, offset): return i_type(0x05, rs, rt, offset)
def j(target): return 2 << 26 | (target >> 2) & 0x03FFFFFF
def jal(target): return 3 << 26 | (target >> 2) & 0x03FFFFFF


def hi(addr): return (addr + 0x8000) >> 16 & 0xFFFF
def lo(addr): return addr & 0xFFFF


class Image:
    """A PS-EXE with code at `base`, functions every 0x100 bytes, a data
    page at +0x800 and the trampoline array at +0xC00, far enough apart that
    the tools' 16-word data guard never sees data next to code."""
    CODE, DATA, TRAMPOLINES = 0x000, 0x800, 0xC00

    def __init__(self, base=0x80010000):
        self.base = base
        self.words = [NOP] * 0x400
        self.put(self.DATA, VALUE)
        self.put(self.TRAMPOLINES, MAGIC)
        self.put(self.TRAMPOLINES + 4, 64)

    def addr(self, offset):
        return self.base + offset

    def put(self, offset, *words):
        for i, word in enumerate(words):
            self.words[offset // 4 + i] = word

    def write(self, path):
        header = bytearray(0x800)
        header[:8] = b"PS-X EXE"
        struct.pack_into("<IIII", header, 0x10, self.base, 0, self.base, len(self.words) * 4)
        Path(path).write_bytes(bytes(header) + struct.pack(f"<{len(self.words)}I", *self.words))


def run(path, limit=500):
    """Execute from the header's entry until `break`; loads land one
    instruction late, as on the R3000. Returns the register file."""
    data = Path(path).read_bytes()
    base = struct.unpack_from("<I", data, 0x18)[0]
    mem = bytearray(data[0x800:])
    regs = [STALE + i for i in range(32)]
    regs[0] = 0
    pc, npc, pending = base, base + 4, None
    for _ in range(limit):
        word = struct.unpack_from("<I", mem, pc - base)[0]
        op, rs, rt = word >> 26, word >> 21 & 31, word >> 16 & 31
        rd, funct, imm = word >> 11 & 31, word & 63, word & 0xFFFF
        simm = imm - 0x10000 if imm & 0x8000 else imm
        landing, pending = pending, None
        nxt, nnxt = npc, npc + 4
        write = None
        if op == 0 and funct == BREAK:
            return regs
        if op == 0 and funct == 0x21:
            write = (rd, regs[rs] + regs[rt] & 0xFFFFFFFF)
        elif op == 0 and funct == 0x08:
            nnxt = regs[rs]
        elif op == 0 and funct == 0x09:
            write, nnxt = (rd, pc + 8), regs[rs]
        elif op == 0 and word == NOP:
            pass
        elif op in (2, 3):
            nnxt = (pc & 0xF0000000) | (word & 0x03FFFFFF) << 2
            if op == 3:
                write = (31, pc + 8)
        elif op in (4, 5):
            if (regs[rs] == regs[rt]) == (op == 4):
                nnxt = npc + (simm << 2)
        elif op == 0x09:
            write = (rt, regs[rs] + simm & 0xFFFFFFFF)
        elif op == 0x0D:
            write = (rt, regs[rs] | imm)
        elif op == 0x0F:
            write = (rt, imm << 16)
        elif op in (0x23, 0x24):
            addr = regs[rs] + simm & 0xFFFFFFFF
            value = struct.unpack_from("<I", mem, addr - base)[0] if op == 0x23 else mem[addr - base]
            pending = (rt, value)
        else:
            raise AssertionError(f"fixture interpreter: unsupported word {word:08x} at {pc:08x}")
        if write and write[0]:
            regs[write[0]] = write[1]
        if landing and landing[0]:
            regs[landing[0]] = landing[1]
        pc, npc = nxt, nnxt
    raise AssertionError("fixture program did not reach break")


def scanner_site(line):
    """(branch, consumer) from `hazard_scan.py`: `B: op | slot .. | C: op`,
    with no consumer address for a register jump that leaves the image."""
    m = re.match(r"([0-9a-f]{8}): .* \| slot .* \| (?:([0-9a-f]{8}): )?", line)
    return m.group(1), m.group(2)


def patcher_site(line):
    """(branch, consumer) from `hazard_patch.py --check`: `hazard B: op |
    slot .. | consumer C`."""
    m = re.match(r"hazard ([0-9a-f]{8}): .* \| slot .* \| consumer (?:([0-9a-f]{8})\b)?", line)
    return m.group(1), m.group(2)


@unittest.skipUnless(os.environ.get("OBJDUMP") and shutil.which(os.environ["OBJDUMP"]),
                     "no MIPS objdump")
class HazardToolTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.tmp.name, "fixture.exe")

    def tearDown(self):
        self.tmp.cleanup()

    def scan(self):
        """The scanner's report, after checking that `hazard_patch.py
        --check` names the same (branch, consumer) sites on this image."""
        with redirect_stdout(io.StringIO()):
            hazards = scanner.scan(self.path)
        scanned = sorted(scanner_site(line) for line in hazards)
        check = self.patch("--check")
        self.assertEqual(check.returncode, 1 if hazards else 0, check.stdout)
        checked = sorted(patcher_site(line) for line in check.stdout.splitlines()
                         if line.startswith("hazard "))
        self.assertEqual(scanned, checked, check.stdout)
        return hazards

    def patch(self, *args):
        return subprocess.run([sys.executable, str(TOOLS / "hazard_patch.py"), self.path, *args],
                              capture_output=True, text=True)

    def assert_fixed(self, image, reg, hazards=1):
        """The shape is reported by both tools, reads the stale register
        unpatched, and reads VALUE once patched with nothing left over."""
        image.write(self.path)
        self.assertEqual(len(self.scan()), hazards, self.scan())
        self.assertEqual(self.patch("--check").returncode, 1)
        self.assertNotEqual(run(self.path)[REG[reg]], VALUE, "fixture does not expose the hazard")
        result = self.patch()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.scan(), [])
        self.assertEqual(run(self.path)[REG[reg]], VALUE, result.stdout)

    def caller(self, image, callee_offset, reg="v0"):
        """main: jal callee ; nop ; addu s0, reg, zero ; break"""
        image.put(0, jal(image.addr(callee_offset)), NOP, addu("s0", reg, "zero"), BREAK)

    def test_return_value_loaded_in_jr_ra_slot(self):
        # The hl-psx/cs-psx settings getter: the caller's first instruction
        # reads v0 before the load lands.
        image = Image()
        self.caller(image, 0x100)
        data = image.addr(Image.DATA)
        image.put(0x100, lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at"))
        self.assert_fixed(image, "s0")

    def test_jr_ra_at_a_non_default_load_address(self):
        # The demo disc's chain loader is linked at 0x801F0000.
        image = Image(base=0x801F0000)
        self.caller(image, 0x100)
        data = image.addr(Image.DATA)
        image.put(0x100, lui("at", hi(data)), jr("ra"), lw("v0", lo(data), "at"))
        self.assert_fixed(image, "s0")

    def test_callee_reads_argument_as_branch_operand(self):
        # 2026-09-04: a callee whose first instruction is `beq a2, t1, ...`
        # reads a2 as a branch's first operand. It returns 2 when a2 holds
        # VALUE and 1 otherwise.
        image = Image()
        data = image.addr(Image.DATA)
        image.put(0, lui("t0", hi(data)), addiu("t1", "zero", VALUE), jal(image.addr(0x100)),
                  lw("a2", lo(data), "t0"), addu("s0", "v0", "zero"), BREAK)
        image.put(0x100, beq("a2", "t1", 3), NOP, jr("ra"), addiu("v0", "zero", 1),
                  jr("ra"), addiu("v0", "zero", 2))
        image.write(self.path)
        self.assertEqual(len(self.scan()), 1)
        self.assertEqual(run(self.path)[REG["s0"]], 1)
        self.assertEqual(self.patch().returncode, 0)
        self.assertEqual(self.scan(), [])
        self.assertEqual(run(self.path)[REG["s0"]], 2)

    def test_conditional_fall_through_consumer(self):
        # bne not taken: the fall-through reads the slot load at once.
        image = Image()
        data = image.addr(Image.DATA)
        image.put(0, lui("t0", hi(data)), bne("zero", "zero", 8), lw("a1", lo(data), "t0"),
                  addu("s0", "a1", "zero"), BREAK)
        self.assert_fixed(image, "s0")

    def test_jump_table_target_consumer(self):
        # A switch: `lw at, %lo(T)(v1) ; jr at ; lw a1, X` with the table
        # entry's target reading a1 first.
        image = Image()
        data, table = image.addr(Image.DATA), image.addr(Image.DATA + 0x40)
        image.put(Image.DATA + 0x40, image.addr(0x100))
        image.put(0, lui("v1", hi(table)), lw("at", lo(table), "v1"), lui("t0", hi(data)),
                  jr("at"), lw("a1", lo(data), "t0"))
        image.put(0x100, addu("s0", "a1", "zero"), BREAK)
        self.assert_fixed(image, "s0")

    def test_register_jump_with_unresolved_target(self):
        # `jr t9` built from lui/ori, no table load to resolve: the target is
        # unknown, so the slot load must be moved out of the slot.
        image = Image()
        data, target = image.addr(Image.DATA), image.addr(0x100)
        image.put(0, lui("t9", target >> 16), ori("t9", "t9", target & 0xFFFF), lui("t0", hi(data)),
                  jr("t9"), lw("a0", lo(data), "t0"))
        image.put(0x100, addu("s0", "a0", "zero"), BREAK)
        self.assert_fixed(image, "s0")

    def test_register_call_reads_argument_first(self):
        # jalr: the callee (a leaf reading a0 at once) is unknown to the
        # tools. The patched call must still return to the original site.
        image = Image()
        data, callee = image.addr(Image.DATA), image.addr(0x100)
        image.put(0, lui("t9", callee >> 16), ori("t9", "t9", callee & 0xFFFF), lui("t0", hi(data)),
                  jalr("t9"), lw("a0", lo(data), "t0"), addiu("s1", "zero", 7), BREAK)
        image.put(0x100, addu("s0", "a0", "zero"), jr("ra"), NOP)
        self.assert_fixed(image, "s0")
        self.assertEqual(run(self.path)[REG["s1"]], 7)

    def test_clean_shapes_are_not_reported(self):
        image = Image()
        data = image.addr(Image.DATA)
        self.caller(image, 0x100)
        # Epilogue arithmetic in the slot, a slot load into ra, and a load
        # whose value has an instruction to land in before the return.
        image.put(0x100, jr("ra"), addiu("sp", "sp", 8))
        image.put(0x200, lui("at", hi(data)), jr("ra"), lw("ra", lo(data), "at"))
        image.put(0x300, lui("at", hi(data)), lw("v0", lo(data), "at"), NOP, jr("ra"), NOP)
        image.write(self.path)
        self.assertEqual(self.scan(), [])
        self.assertEqual(self.patch("--check").returncode, 0)

    def test_scanner_and_patcher_name_the_same_sites(self):
        # Several shapes in one image, including a conditional whose load is
        # read on both paths (two sites at one branch). scan() fails if the
        # two CLIs disagree on any of them.
        image = Image()
        data = image.addr(Image.DATA)
        image.put(0x100, lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at"))
        image.put(0x200, lui("t0", hi(data)), beq("zero", "zero", 2), lw("a1", lo(data), "t0"),
                  addu("s0", "a1", "zero"), addu("s1", "a1", "zero"), jr("ra"), NOP)
        image.put(0x300, lui("t0", hi(data)), jalr("t9"), lw("a0", lo(data), "t0"), jr("ra"), NOP)
        image.write(self.path)
        sites = sorted(scanner_site(line) for line in self.scan())
        self.assertEqual(sites, sorted([
            ("%08x" % image.addr(0x104), None),
            ("%08x" % image.addr(0x204), "%08x" % image.addr(0x20C)),
            ("%08x" % image.addr(0x204), "%08x" % image.addr(0x210)),
            ("%08x" % image.addr(0x304), None),
        ]))

    def test_module_callers_can_replace_the_data_guard(self):
        # Games load hazard_scan.py as a module and replace looks_like_code
        # so proven .text is never skipped as data (alttp-psx, hk-psx). The
        # replacement has to reach the shared detector.
        image = Image()
        data = image.addr(Image.DATA)
        self.caller(image, 0x100)
        image.put(0x100, lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at"))
        image.put(0x120, 0xFFFFFFFF)  # decodes as `.word`, inside the 16-word guard
        image.write(self.path)
        guard = scanner.looks_like_code
        try:
            self.assertEqual(self.scan(), [])
            scanner.looks_like_code = lambda *args, **kwargs: True
            with redirect_stdout(io.StringIO()):
                self.assertEqual(len(scanner.scan(self.path)), 1)
        finally:
            scanner.looks_like_code = guard

    def test_second_pass_keeps_earlier_trampolines(self):
        # Every trampoline contains nops, so a second pass must not take the
        # first zero word after the magic for free space.
        image = Image()
        data = image.addr(Image.DATA)
        self.caller(image, 0x100)
        image.put(0x100, lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at"))
        image.put(0x200, lui("at", hi(data)), jr("ra"), lbu("v1", lo(data), "at"))
        image.write(self.path)
        env = dict(os.environ, HAZARD_PATCH_ONLY="%08x" % image.addr(0x104))
        first = subprocess.run([sys.executable, str(TOOLS / "hazard_patch.py"), self.path],
                               capture_output=True, text=True, env=env)
        self.assertEqual(first.returncode, 0, first.stdout)
        before = Path(self.path).read_bytes()
        self.assertEqual(self.patch().returncode, 0)
        after = Path(self.path).read_bytes()
        start = 0x800 + Image.TRAMPOLINES + 8
        self.assertEqual(after[start:start + 12], before[start:start + 12])
        self.assertEqual(self.scan(), [])
        self.assertEqual(run(self.path)[REG["s0"]], VALUE)


if __name__ == "__main__":
    unittest.main()
