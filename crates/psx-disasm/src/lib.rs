// SPDX-License-Identifier: GPL-2.0-or-later
//! R3000 (MIPS I) instruction decoder that prints GNU objdump's syntax.
//!
//! The post-link guest checks (`psoxide-hazard`: the hazard patcher, the
//! hazard scanner and the stack guard) used to read
//! `mipsel-none-elf-objdump -D -b binary -m mips:3000 -EL` and match on its
//! text. This crate replaces that dependency: [`decode`] gives the mnemonic
//! and operands objdump 2.43 prints for a word, [`Insn`]'s `Display` prints
//! the operands exactly as objdump does (register names, decimal versus hex
//! immediates, absolute branch targets), and [`objdump_listing`] reproduces which
//! words objdump lists at all, since a run of zero words it collapses to
//! `...` is missing from its output.
//!
//! The text matters because the checks' reports print it and their tests
//! compare those reports, so a word must read exactly as objdump read it.
//! A word objdump does not decode for this machine (`.word 0x...`) is
//! `None` from [`decode`]. The decoder covers the whole MIPS I opcode space
//! as binutils sees it for `mips:3000`, including the coprocessor 0..3 forms
//! (`c2 0x...` is a GTE command), the R3010 FPU instructions and `jalx`.
//! It was checked against objdump on every word of the guest images the
//! tools run on and on a field-by-field sweep of the opcode space.
#![no_std]

use core::fmt;

/// o32 names of the general registers, as objdump prints them.
pub const GPR_NAMES: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "s8",
    "ra",
];

/// objdump's R3000 coprocessor 0 register names; `None` prints as `$n`.
const CP0_NAMES: [Option<&str>; 32] = {
    let mut names = [None; 32];
    names[0] = Some("c0_index");
    names[1] = Some("c0_random");
    names[2] = Some("c0_entrylo");
    names[4] = Some("c0_context");
    names[8] = Some("c0_badvaddr");
    names[10] = Some("c0_entryhi");
    names[12] = Some("c0_sr");
    names[13] = Some("c0_cause");
    names[14] = Some("c0_epc");
    names[15] = Some("c0_prid");
    names
};

/// One operand, printed as objdump prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A general register, by o32 name.
    Gpr(u8),
    /// A coprocessor 0 register: its R3000 name, or `$n`.
    Cp0(u8),
    /// A coprocessor register printed as `$n` (COP2/COP3, control registers).
    Cop(u8),
    /// An FPU control register: `c1_fir`, `c1_fcsr` or `$n`.
    Fcr(u8),
    /// A floating-point register, `$fn`.
    Fpr(u8),
    /// A signed immediate, printed in decimal.
    Dec(i32),
    /// An unsigned immediate or code, printed as `0x` hex.
    Hex(u32),
    /// `offset(base)`, the offset in decimal.
    Mem(i32, u8),
    /// An absolute branch or jump target, printed as `0x` hex.
    Target(u32),
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Operand::Gpr(r) => f.write_str(GPR_NAMES[usize::from(r & 31)]),
            Operand::Cp0(r) => match CP0_NAMES[usize::from(r & 31)] {
                Some(name) => f.write_str(name),
                None => write!(f, "${r}"),
            },
            Operand::Cop(r) => write!(f, "${r}"),
            Operand::Fcr(0) => f.write_str("c1_fir"),
            Operand::Fcr(31) => f.write_str("c1_fcsr"),
            Operand::Fcr(r) => write!(f, "${r}"),
            Operand::Fpr(r) => write!(f, "$f{r}"),
            Operand::Dec(v) => write!(f, "{v}"),
            Operand::Hex(v) | Operand::Target(v) => write!(f, "{v:#x}"),
            Operand::Mem(offset, base) => {
                write!(f, "{offset}({})", GPR_NAMES[usize::from(base & 31)])
            }
        }
    }
}

/// A decoded instruction: objdump's mnemonic and up to three operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    /// The mnemonic objdump prints, aliases included (`nop`, `move`, `li`,
    /// `b`, `beqz`, ...).
    pub mnemonic: &'static str,
    operands: [Option<Operand>; 3],
}

impl Insn {
    const fn new(mnemonic: &'static str, operands: [Option<Operand>; 3]) -> Self {
        Self { mnemonic, operands }
    }

    /// The operands, in printed order.
    pub fn operands(&self) -> impl Iterator<Item = Operand> + '_ {
        self.operands.iter().flatten().copied()
    }

    /// True for an instruction with a branch delay slot: every branch and
    /// jump, coprocessor branches included. objdump never collapses the
    /// zero word after one into `...`.
    pub fn has_delay_slot(&self) -> bool {
        matches!(
            self.mnemonic,
            "j" | "jal"
                | "jalx"
                | "jr"
                | "jalr"
                | "b"
                | "bal"
                | "beq"
                | "beqz"
                | "bne"
                | "bnez"
                | "blez"
                | "bgtz"
                | "bltz"
                | "bgez"
                | "bltzal"
                | "bgezal"
                | "bc0f"
                | "bc0t"
                | "bc1f"
                | "bc1t"
                | "bc2f"
                | "bc2t"
                | "bc3f"
                | "bc3t"
        )
    }
}

/// The operands, comma separated with no spaces, as objdump prints them
/// after the mnemonic (empty for `nop`, `rfe`, ...).
impl fmt::Display for Insn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, operand) in self.operands().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{operand}")?;
        }
        Ok(())
    }
}

fn op0(mnemonic: &'static str) -> Option<Insn> {
    Some(Insn::new(mnemonic, [None, None, None]))
}

fn op1(mnemonic: &'static str, a: Operand) -> Option<Insn> {
    Some(Insn::new(mnemonic, [Some(a), None, None]))
}

fn op2(mnemonic: &'static str, a: Operand, b: Operand) -> Option<Insn> {
    Some(Insn::new(mnemonic, [Some(a), Some(b), None]))
}

fn op3(mnemonic: &'static str, a: Operand, b: Operand, c: Operand) -> Option<Insn> {
    Some(Insn::new(mnemonic, [Some(a), Some(b), Some(c)]))
}

/// Decode `word` found at address `pc`, or `None` where objdump prints
/// `.word`. `pc` places branch and jump targets.
pub fn decode(word: u32, pc: u32) -> Option<Insn> {
    use Operand::{Cop, Cp0, Dec, Fpr, Gpr, Hex, Mem, Target};
    let op = word >> 26;
    let rs = (word >> 21 & 31) as u8;
    let rt = (word >> 16 & 31) as u8;
    let rd = (word >> 11 & 31) as u8;
    let sa = (word >> 6 & 31) as u8;
    let funct = word & 63;
    let imm = word & 0xFFFF;
    let simm = i32::from(imm as u16 as i16);
    let branch = Target(pc.wrapping_add(4).wrapping_add((simm << 2) as u32));
    let jump = (pc.wrapping_add(4) & 0xF000_0000) | (word & 0x03FF_FFFF) << 2;
    let (s, t) = (Gpr(rs), Gpr(rt));
    match op {
        0 => special(word, rs, rt, rd, sa, funct),
        1 => match rt {
            0 => op2("bltz", s, branch),
            1 if rs == 0 => op1("b", branch),
            1 => op2("bgez", s, branch),
            16 => op2("bltzal", s, branch),
            17 if rs == 0 => op1("bal", branch),
            17 => op2("bgezal", s, branch),
            _ => None,
        },
        2 => op1("j", Target(jump)),
        3 => op1("jal", Target(jump)),
        4 if rs == 0 && rt == 0 => op1("b", branch),
        4 if rt == 0 => op2("beqz", s, branch),
        4 => op3("beq", s, t, branch),
        5 if rt == 0 => op2("bnez", s, branch),
        5 => op3("bne", s, t, branch),
        6 if rt == 0 => op2("blez", s, branch),
        7 if rt == 0 => op2("bgtz", s, branch),
        8 => op3("addi", t, s, Dec(simm)),
        9 if rs == 0 => op2("li", t, Dec(simm)),
        9 => op3("addiu", t, s, Dec(simm)),
        10 => op3("slti", t, s, Dec(simm)),
        11 => op3("sltiu", t, s, Dec(simm)),
        12 => op3("andi", t, s, Hex(imm)),
        13 if rs == 0 => op2("li", t, Hex(imm)),
        13 => op3("ori", t, s, Hex(imm)),
        14 => op3("xori", t, s, Hex(imm)),
        15 if rs == 0 => op2("lui", t, Hex(imm)),
        16..=19 => coprocessor((op - 16) as u8, word, rs, rt, rd, sa, funct, branch),
        0x1D => op1("jalx", Target(jump | 1)),
        0x20..=0x2E => {
            let mnemonic = match op {
                0x20 => "lb",
                0x21 => "lh",
                0x22 => "lwl",
                0x23 => "lw",
                0x24 => "lbu",
                0x25 => "lhu",
                0x26 => "lwr",
                0x28 => "sb",
                0x29 => "sh",
                0x2A => "swl",
                0x2B => "sw",
                0x2E => "swr",
                _ => return None,
            };
            op2(mnemonic, t, Mem(simm, rs))
        }
        0x30..=0x33 | 0x38..=0x3B => {
            let z = op & 3;
            let mnemonic = match (op, z) {
                (0x30, _) => "lwc0",
                (0x31, _) => "lwc1",
                (0x32, _) => "lwc2",
                (0x33, _) => "lwc3",
                (0x38, _) => "swc0",
                (0x39, _) => "swc1",
                (0x3A, _) => "swc2",
                _ => "swc3",
            };
            let reg = match z {
                0 => Cp0(rt),
                1 => Fpr(rt),
                _ => Cop(rt),
            };
            op2(mnemonic, reg, Mem(simm, rs))
        }
        _ => None,
    }
}

fn special(word: u32, rs: u8, rt: u8, rd: u8, sa: u8, funct: u32) -> Option<Insn> {
    use Operand::{Gpr, Hex};
    let (s, t, d) = (Gpr(rs), Gpr(rt), Gpr(rd));
    match funct {
        0 if rs == 0 => match (word, rd, rt, sa) {
            (0, ..) => op0("nop"),
            (_, 0, 0, 1) => op0("ssnop"),
            (_, 0, 0, 3) => op0("ehb"),
            _ => op3("sll", d, t, Hex(u32::from(sa))),
        },
        2 if rs == 0 => op3("srl", d, t, Hex(u32::from(sa))),
        3 if rs == 0 => op3("sra", d, t, Hex(u32::from(sa))),
        4 if sa == 0 => op3("sllv", d, t, s),
        6 if sa == 0 => op3("srlv", d, t, s),
        7 if sa == 0 => op3("srav", d, t, s),
        8 if rt == 0 && rd == 0 && sa == 0 => op1("jr", s),
        9 if rt == 0 && sa == 0 && rd == 31 => op1("jalr", s),
        9 if rt == 0 && sa == 0 => op2("jalr", d, s),
        12 => match word >> 6 & 0xF_FFFF {
            0 => op0("syscall"),
            code => op1("syscall", Hex(code)),
        },
        13 => match (word >> 16 & 0x3FF, word >> 6 & 0x3FF) {
            (0, 0) => op0("break"),
            (high, 0) => op1("break", Hex(high)),
            (high, low) => op2("break", Hex(high), Hex(low)),
        },
        16 if rs == 0 && rt == 0 && sa == 0 => op1("mfhi", d),
        18 if rs == 0 && rt == 0 && sa == 0 => op1("mflo", d),
        17 if rt == 0 && rd == 0 && sa == 0 => op1("mthi", s),
        19 if rt == 0 && rd == 0 && sa == 0 => op1("mtlo", s),
        24 if rd == 0 && sa == 0 => op2("mult", s, t),
        25 if rd == 0 && sa == 0 => op2("multu", s, t),
        26 if rd == 0 && sa == 0 => op3("div", Gpr(0), s, t),
        27 if rd == 0 && sa == 0 => op3("divu", Gpr(0), s, t),
        32..=43 if sa == 0 => match funct {
            32 => op3("add", d, s, t),
            33 if rt == 0 => op2("move", d, s),
            33 => op3("addu", d, s, t),
            34 if rs == 0 => op2("neg", d, t),
            34 => op3("sub", d, s, t),
            35 if rs == 0 => op2("negu", d, t),
            35 => op3("subu", d, s, t),
            36 => op3("and", d, s, t),
            37 if rt == 0 => op2("move", d, s),
            37 => op3("or", d, s, t),
            38 => op3("xor", d, s, t),
            39 => op3("nor", d, s, t),
            42 => op3("slt", d, s, t),
            43 => op3("sltu", d, s, t),
            _ => None,
        },
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn coprocessor(
    z: u8,
    word: u32,
    rs: u8,
    rt: u8,
    rd: u8,
    sa: u8,
    funct: u32,
    branch: Operand,
) -> Option<Insn> {
    use Operand::{Cop, Cp0, Fcr, Fpr, Gpr, Hex};
    const MFC: [&str; 4] = ["mfc0", "mfc1", "mfc2", "mfc3"];
    const CFC: [&str; 4] = ["cfc0", "cfc1", "cfc2", "cfc3"];
    const MTC: [&str; 4] = ["mtc0", "mtc1", "mtc2", "mtc3"];
    const CTC: [&str; 4] = ["ctc0", "ctc1", "ctc2", "ctc3"];
    const BCF: [&str; 4] = ["bc0f", "bc1f", "bc2f", "bc3f"];
    const BCT: [&str; 4] = ["bc0t", "bc1t", "bc2t", "bc3t"];
    const COFUN: [&str; 4] = ["c0", "c1", "c2", "c3"];
    let i = usize::from(z);
    let data = match z {
        0 => Cp0(rd),
        1 => Fpr(rd),
        _ => Cop(rd),
    };
    let control = if z == 1 { Fcr(rd) } else { Cop(rd) };
    let moves = word & 0x7FF == 0;
    match rs {
        0 if moves => op2(MFC[i], Gpr(rt), data),
        2 if moves => op2(CFC[i], Gpr(rt), control),
        4 if moves => op2(MTC[i], Gpr(rt), data),
        6 if moves => op2(CTC[i], Gpr(rt), control),
        8 if rt == 0 => op1(BCF[i], branch),
        8 if rt == 1 => op1(BCT[i], branch),
        16..=31 => {
            let named = match (z, word) {
                (0, 0x4200_0001) => op0("tlbr"),
                (0, 0x4200_0002) => op0("tlbwi"),
                (0, 0x4200_0006) => op0("tlbwr"),
                (0, 0x4200_0008) => op0("tlbp"),
                (0, 0x4200_0010) => op0("rfe"),
                (1, _) => fpu(rs, rt, rd, sa, funct),
                _ => None,
            };
            named.or_else(|| op1(COFUN[i], Hex(word & 0x01FF_FFFF)))
        }
        _ => None,
    }
}

/// An R3010 arithmetic instruction (single, double or word format).
fn fpu(fmt: u8, ft: u8, fs: u8, fd: u8, funct: u32) -> Option<Insn> {
    use Operand::Fpr;
    let (t, s, d) = (Fpr(ft), Fpr(fs), Fpr(fd));
    match (fmt, funct) {
        (16 | 17, 0..=3) => {
            const NAMES: [[&str; 4]; 2] = [
                ["add.s", "sub.s", "mul.s", "div.s"],
                ["add.d", "sub.d", "mul.d", "div.d"],
            ];
            op3(NAMES[usize::from(fmt - 16)][funct as usize], d, s, t)
        }
        (16 | 17, 5..=7) if ft == 0 => {
            const NAMES: [[&str; 3]; 2] =
                [["abs.s", "mov.s", "neg.s"], ["abs.d", "mov.d", "neg.d"]];
            op2(NAMES[usize::from(fmt - 16)][funct as usize - 5], d, s)
        }
        (17, 32) if ft == 0 => op2("cvt.s.d", d, s),
        (20, 32) if ft == 0 => op2("cvt.s.w", d, s),
        (16, 33) if ft == 0 => op2("cvt.d.s", d, s),
        (20, 33) if ft == 0 => op2("cvt.d.w", d, s),
        (16, 36) if ft == 0 => op2("cvt.w.s", d, s),
        (17, 36) if ft == 0 => op2("cvt.w.d", d, s),
        (16 | 17, 48..=63) if fd == 0 => {
            const NAMES: [[&str; 16]; 2] = [
                [
                    "c.f.s", "c.un.s", "c.eq.s", "c.ueq.s", "c.olt.s", "c.ult.s", "c.ole.s",
                    "c.ule.s", "c.sf.s", "c.ngle.s", "c.seq.s", "c.ngl.s", "c.lt.s", "c.nge.s",
                    "c.le.s", "c.ngt.s",
                ],
                [
                    "c.f.d", "c.un.d", "c.eq.d", "c.ueq.d", "c.olt.d", "c.ult.d", "c.ole.d",
                    "c.ule.d", "c.sf.d", "c.ngle.d", "c.seq.d", "c.ngl.d", "c.lt.d", "c.nge.d",
                    "c.le.d", "c.ngt.d",
                ],
            ];
            op2(NAMES[usize::from(fmt - 16)][funct as usize - 48], s, t)
        }
        _ => None,
    }
}

/// Walk the words objdump lists for a flat little-endian image loaded at
/// `vma`, as `-D -b binary` prints them, calling `listed(address, word,
/// insn)` in address order with `None` for `.word`. Every whole word is
/// listed except that a run of eight or more zero bytes is collapsed to
/// `...` (its words are missing) unless the word before it has a delay
/// slot, in which case objdump first prints that slot as `nop`. A trailing
/// partial word is never listed.
pub fn objdump_listing(data: &[u8], vma: u32, mut listed: impl FnMut(u32, u32, Option<Insn>)) {
    let end = data.len();
    let mut off = 0usize;
    let mut after_delay = false;
    while off < end {
        if !after_delay {
            let run = data[off..].iter().take_while(|&&b| b == 0).count();
            let to_end = off + run == end;
            // objdump's skip_zeroes (8) and skip_zeroes_at_end (3).
            if run >= 8 || (to_end && run > 0 && run < 3) {
                off = if to_end { end } else { off + (run & !3) };
                continue;
            }
        }
        if off + 4 > end {
            return;
        }
        let word = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let pc = vma.wrapping_add(off as u32);
        let insn = decode(word, pc);
        after_delay = insn.is_some_and(|insn| insn.has_delay_slot());
        listed(pc, word, insn);
        off += 4;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::{String, ToString};
    use std::vec::Vec;

    fn text(word: u32, pc: u32) -> String {
        match decode(word, pc) {
            Some(insn) => {
                let args = insn.to_string();
                if args.is_empty() {
                    insn.mnemonic.to_string()
                } else {
                    std::format!("{} {}", insn.mnemonic, args)
                }
            }
            None => std::format!(".word {word:#x}"),
        }
    }

    /// Lines objdump 2.43 printed for these words at 0x8000f800 onward.
    #[test]
    fn matches_objdump_samples() {
        let cases: &[(u32, u32, &str)] = &[
            (0x3c1c8001, 0x8000f800, "lui gp,0x8001"),
            (0x27bdffe8, 0x8000f804, "addiu sp,sp,-24"),
            (0x10000003, 0x8000f810, "b 0x8000f820"),
            (0x8fa40004, 0x8000f824, "lw a0,4(sp)"),
            (0x00851021, 0x8000f828, "addu v0,a0,a1"),
            (0x00a01025, 0x8000f82c, "move v0,a1"),
            (0x24020005, 0x8000f830, "li v0,5"),
            (0x34021234, 0x8000f834, "li v0,0x1234"),
            (0x0000000d, 0x8000f838, "break"),
            (0x0007000d, 0x8000f83c, "break 0x7"),
            (0x0000004d, 0, "break 0x0,0x1"),
            (0x4a180001, 0x8000f840, "c2 0x180001"),
            (0x40026000, 0x8000f844, "mfc0 v0,c0_sr"),
            (0x48020800, 0x8000f848, "mfc2 v0,$1"),
            (0x48c20800, 0x8000f84c, "ctc2 v0,$1"),
            (0xc7a00004, 0x8000f850, "lwc1 $f0,4(sp)"),
            (0x46020000, 0x8000f854, "add.s $f0,$f0,$f2"),
            (0x0000000c, 0x8000f858, "syscall"),
            (0x42000010, 0x8000f85c, "rfe"),
            (0xffffffff, 0x8000f860, ".word 0xffffffff"),
            (0x03e00008, 0x8000f864, "jr ra"),
            (0x0c000010, 0x8000f870, "jal 0x80000040"),
            (0x74000000, 0, "jalx 0x1"),
            (0x0000f809, 0, "jalr zero"),
            (0x03c0f025, 0, "move s8,s8"),
            (0x000000c0, 0, "ehb"),
            (0x00000080, 0, "sll zero,zero,0x2"),
            (0x0000001a, 0, "div zero,zero,zero"),
            (0x44400000, 0, "cfc1 zero,c1_fir"),
            (0x445ff800, 0, "cfc1 ra,c1_fcsr"),
            (0x44022001, 0, ".word 0x44022001"),
            (0x1000ffff, 0x80010000, "b 0x80010000"),
            (0x04118000, 0x80000000, "bal 0x7ffe0004"),
        ];
        for &(word, pc, want) in cases {
            assert_eq!(text(word, pc), want, "{word:08x}");
        }
    }

    #[test]
    fn listing_collapses_zero_runs_like_objdump() {
        // objdump 2.43 on these words listed 0x0, 0x4, 0x10, 0x14, 0x18,
        // 0x24, 0x28, 0x34, 0x38, 0x3c, 0x40, 0x44, 0x48, 0x4c, 0x50, 0x54,
        // 0x58, 0x5c, 0x60, 0x64.
        let words: [u32; 26] = [
            0x45000003, 0, 0, 0, 0x24010001, 0x74000000, 0, 0, 0, 0x24010001, 0xffffffff, 0, 0,
            0x24010001, 0x0000f809, 0, 0, 0x24010001, 0x24010001, 0, 0x24010001, 0x10000001, 0, 0,
            0x24010001, 0,
        ];
        let data: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut listed = Vec::new();
        objdump_listing(&data, 0, |addr, _, _| listed.push(addr));
        assert_eq!(
            listed,
            [
                0x0, 0x4, 0x10, 0x14, 0x18, 0x24, 0x28, 0x34, 0x38, 0x3c, 0x40, 0x44, 0x48, 0x4c,
                0x50, 0x54, 0x58, 0x5c, 0x60, 0x64
            ]
        );
    }
}
