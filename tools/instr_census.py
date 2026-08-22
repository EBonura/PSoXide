#!/usr/bin/env python3
"""Static instruction-mix census of a PSX-EXE built by this stack.

The guests link with `--oformat=binary` (no symbols), so this works on the
flat PSX-EXE: strip the 2 KiB header, disassemble with binutils' MIPS-I
disassembler, find the .text/.data boundary heuristically (the linker script
places .text first, then .data/.rodata), and count what the compiler emitted.

    python3 tools/instr_census.py <file.exe> [--name label] [--dis out.dis]

Counts are STATIC (linked text), not dynamic (retired). Use them to pick
targets and to sanity-check codegen; use the emulator's counters for cost.
Requires `mipsel-none-elf-objdump` (brew install mipsel-none-elf-binutils).
"""

import argparse
import collections
import re
import subprocess
import tempfile

OBJDUMP = "mipsel-none-elf-objdump"
LOAD_ADDR = 0x80010000
LOADS = ("lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr")
STORES = ("sw", "sh", "sb", "swl", "swr")
COND_BRANCHES = ("beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b")
JUMPS = ("j", "jal", "jr", "jalr")


def disassemble(path, dis_out=None):
    data = open(path, "rb").read()[0x800:]
    with tempfile.NamedTemporaryFile(suffix=".raw") as raw:
        raw.write(data)
        raw.flush()
        text = subprocess.run(
            [OBJDUMP, "-D", "-b", "binary", "-m", "mips:3000", "-EL",
             f"--adjust-vma={LOAD_ADDR:#x}", raw.name],
            capture_output=True, text=True, check=True,
        ).stdout
    if dis_out:
        open(dis_out, "w").write(text)
    rows = []
    for line in text.splitlines():
        m = re.match(r"^\s*([0-9a-f]{8}):\s+([0-9a-f]{8})\s+(\S+)\s*(.*)$", line)
        if m:
            rows.append((int(m.group(1), 16), int(m.group(2), 16), m.group(3), m.group(4)))
    return data, rows


def text_end(rows, window=256, bad_threshold=16):
    """First window dense in undecodable words marks the start of .data."""
    for i in range(0, len(rows) - window, 64):
        bad = sum(1 for r in rows[i:i + window] if r[2] in (".word", "jalx") or r[2].startswith("c3"))
        if bad >= bad_threshold:
            return i
    return len(rows)


def census(path, name, dis_out=None):
    data, rows = disassemble(path, dis_out)
    text = rows[:text_end(rows)]
    n = len(text)
    c = collections.Counter(r[2] for r in text)
    cop2 = collections.Counter()
    nop = nop_bd = nop_ld = nop_cop = nop_other = 0
    sext16 = spill = lui = 0
    frames = []
    mult_then_mf = 0
    load_then_nop = 0
    branch_then_nop = 0
    prev = None
    for i, (addr, word, mn, ops) in enumerate(text):
        nxt = text[i + 1] if i + 1 < n else None
        if word >> 26 == 0x12:
            if word & 0x0200_0000:
                cop2["cofun"] += 1
            else:
                cop2[{0: "mfc2", 2: "cfc2", 4: "mtc2", 6: "ctc2"}.get((word >> 21) & 0x1F, "other")] += 1
        if word == 0:
            nop += 1
            if prev is not None:
                pm, pw = prev[2], prev[1]
                if pm in COND_BRANCHES or pm in JUMPS:
                    nop_bd += 1
                elif pm in LOADS or (pw >> 26 == 0x10 and (pw >> 21) & 0x1F == 0):
                    nop_ld += 1
                elif pw >> 26 == 0x12 or (pw == 0 and i >= 2 and text[i - 2][1] >> 26 == 0x12):
                    nop_cop += 1
                else:
                    nop_other += 1
        if mn == "lui":
            lui += 1
        if mn == "sll" and ops.endswith(",0x10") and nxt and nxt[2] == "sra" and nxt[3].endswith(",0x10"):
            sext16 += 1
        if mn in ("sw", "lw") and "(sp)" in ops:
            spill += 1
        if mn == "addiu" and ops.startswith("sp,sp,-"):
            frames.append(int(ops.split("-")[1]))
        if mn in ("mult", "multu") and nxt and nxt[2] in ("mflo", "mfhi"):
            mult_then_mf += 1
        if mn in LOADS and nxt and nxt[1] == 0:
            load_then_nop += 1
        if (mn in COND_BRANCHES or mn in JUMPS) and nxt and nxt[1] == 0:
            branch_then_nop += 1
        prev = (addr, word, mn, ops)

    loads = sum(c[x] for x in LOADS)
    stores = sum(c[x] for x in STORES)
    branches = sum(c[x] for x in COND_BRANCHES) + sum(c[x] for x in JUMPS)
    muldiv = c["mult"] + c["multu"] + c["div"] + c["divu"]
    pct = lambda v: f"{100 * v / max(n, 1):.1f}%"
    print(f"## {name}: payload {len(data)} B, text~{n * 4} B ({n} instrs), data~{len(data) - n * 4} B")
    print(f"- nop {nop} ({pct(nop)}): branch-delay {nop_bd}, load-delay {nop_ld}, GTE/COP gaps {nop_cop}, other {nop_other}")
    print(f"- branches+jumps {branches}, delay slot is nop {branch_then_nop} ({100 * branch_then_nop / max(branches, 1):.1f}%)")
    print(f"- loads {loads} ({pct(loads)}), followed by nop {load_then_nop} ({100 * load_then_nop / max(loads, 1):.1f}%); stores {stores} ({pct(stores)})")
    print(f"- sp-relative lw/sw {spill} ({pct(spill)}); unaligned lwl/lwr/swl/swr {c['lwl'] + c['lwr'] + c['swl'] + c['swr']}")
    print(f"- mult/multu/div/divu {c['mult']}/{c['multu']}/{c['div']}/{c['divu']} (total {muldiv}); mult immediately followed by mflo/mfhi {mult_then_mf}")
    print(f"- COP2 {dict(cop2)}")
    print(f"- jal {c['jal']} jalr {c['jalr']} jr {c['jr']}; lui {lui} ({pct(lui)}); sll/sra 16 sign-extend pairs {sext16}; andi {c['andi']}; li {c['li']}; move {c['move']}")
    if frames:
        frames.sort()
        print(f"- stack frames {len(frames)}: median {frames[len(frames) // 2]} B, p90 {frames[int(len(frames) * 0.9)]} B, max {frames[-1]} B, >=128 B {sum(1 for f in frames if f >= 128)}")
    print("- top: " + ", ".join(f"{k} {v}" for k, v in c.most_common(14)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("exe")
    ap.add_argument("--name", default=None)
    ap.add_argument("--dis", default=None, help="also write the full disassembly here")
    args = ap.parse_args()
    census(args.exe, args.name or args.exe, args.dis)


if __name__ == "__main__":
    main()
