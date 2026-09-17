// SPDX-License-Identifier: GPL-2.0-or-later
//! Performance probes: how much CPU time a technique really buys on silicon.
//!
//! Every probe here uses the warm harness. The older CPU records take five
//! samples with the scan heartbeat and a pad poll between them, and that code
//! evicts some of the probe's I-cache lines, so each sample pays a few
//! line refills whose number depends on where the linker put things. Their
//! minima move by tens of cycles when unrelated code shifts. A warm probe runs
//! its timed block twice inside one assembly block and reports the second
//! pass, so every line it executes is resident and the result is a property
//! of the instructions alone. The block starts on a cache-line boundary for
//! the same reason.
//!
//! Two groups:
//!
//! * `SAFE` always runs with the timing scan: warm twins of the core CPU
//!   records, the MULT/DIV gap sweep (how many independent instructions fit
//!   between `multu`/`divu` and `mflo` before the interlock stall is gone),
//!   MULT operand dependence, and the I-cache 4 KiB alias pair.
//! * `RISKY` flips one bit of RAM_SIZE or the cache-control port around a
//!   fixed workload and restores it. A wrong guess about an undocumented bit
//!   can hang the console, so these run only from TARGETED PROBES > PERF A/B,
//!   never from the default scan or the headless conformance capture. The
//!   record id is on screen while each one runs: a hang names its culprit.
//!
//! Marker ids 32 and up belong to this file (see the scheme in main.rs).

use crate::regs::{CACHE_CONTROL, RAM_SIZE};
use crate::{__hwtest_icache_alias_b, __hwtest_icache_entry_w1, __hwtest_perf_loads};
use crate::{__hwtest_icache_block, __hwtest_icache_entry_w0, TIMING_RECORD_COUNT};
use crate::{flush_icache_without_irq, push_timing_record, sample_timing, TimingRecord};

const SCRATCHPAD: u32 = 0x1F80_0000;

static mut PERF_WORD: u32 = 0;

/// A probe argument that is only known at run time.
#[derive(Copy, Clone)]
enum Arg {
    Imm(u32),
    /// A word of cached main RAM.
    RamWord,
    /// Leaf at line 0 of a 4 KiB page.
    EntryW0,
    /// Leaf on another line of the same page: never conflicts with `EntryW0`.
    EntryW1,
    /// Leaf exactly 4 KiB after `EntryW0`: same cache index, different tag.
    AliasB,
    /// 64 RAM loads, executed from KSEG0 (cached).
    Loads,
    /// The same 64 loads through their KSEG1 alias: every instruction fetch
    /// is a RAM access that coincides with a RAM data access.
    LoadsUncached,
    /// 1024 words covering every I-cache line.
    IcacheBlock,
}

impl Arg {
    fn resolve(self) -> u32 {
        match self {
            Self::Imm(value) => value,
            Self::RamWord => (&raw const PERF_WORD) as u32,
            Self::EntryW0 => __hwtest_icache_entry_w0 as *const () as u32,
            Self::EntryW1 => __hwtest_icache_entry_w1 as *const () as u32,
            Self::AliasB => __hwtest_icache_alias_b as *const () as u32,
            Self::Loads => __hwtest_perf_loads as *const () as u32,
            Self::LoadsUncached => {
                (__hwtest_perf_loads as *const () as u32 & 0x1FFF_FFFF) | 0xA000_0000
            }
            Self::IcacheBlock => __hwtest_icache_block as *const () as u32,
        }
    }
}

type WarmFn = fn(u32, u32) -> u16;

#[derive(Copy, Clone)]
struct Probe {
    id: u8,
    work: u16,
    run: WarmFn,
    a: Arg,
    b: Arg,
    /// Run the wrapper through KSEG1 so it cannot disturb the lines it measures.
    uncached: bool,
}

const fn probe(id: u8, work: u16, run: WarmFn, a: Arg, b: Arg) -> Probe {
    Probe {
        id,
        work,
        run,
        a,
        b,
        uncached: false,
    }
}

impl Probe {
    const fn uncached(mut self) -> Self {
        self.uncached = true;
        self
    }
}

// MULTU operands: `rs` picks the 6/9/13-cycle band, `rt` stays where the old
// records 0x05-0x07 had it so the k=0 rows are their warm twins.
const RS_SMALL: Arg = Arg::Imm(0x0000_07FF);
const RS_MEDIUM: Arg = Arg::Imm(0x000F_FFFF);
const RS_LARGE: Arg = Arg::Imm(0x1357_2468);
const RT: Arg = Arg::Imm(0x0001_0041);
const NUMERATOR: Arg = Arg::Imm(0x7ABC_DEF1);
const DIVISOR: Arg = Arg::Imm(0x0000_0101);
const NONE: Arg = Arg::Imm(0);

const SAFE: [Probe; 28] = [
    // Warm twins of 0x01, 0x02, 0x03, 0x09, 0x0B, 0x0C and 0x04.
    probe(0x72, 128, warm_nops, NONE, NONE),
    probe(0x73, 128, warm_dependent_alu, NONE, NONE),
    probe(0x74, 64, warm_loads, Arg::RamWord, NONE),
    probe(0x75, 64, warm_loads, Arg::Imm(SCRATCHPAD), NONE),
    probe(0x76, 64, warm_stores, Arg::RamWord, NONE),
    probe(0x77, 64, warm_stores, Arg::Imm(SCRATCHPAD), NONE),
    probe(0x78, 64, warm_taken_branches, NONE, NONE),
    // Gap sweep. Around each band's documented latency m the cost should be
    // flat up to k = m and then rise by one cycle per nop per repeat.
    probe(0x79, 16, multu_gap_0, RS_SMALL, RT),
    probe(0x7A, 16, multu_gap_5, RS_SMALL, RT),
    probe(0x7B, 16, multu_gap_6, RS_SMALL, RT),
    probe(0x7C, 16, multu_gap_7, RS_SMALL, RT),
    probe(0x7D, 16, multu_gap_0, RS_MEDIUM, RT),
    probe(0x7E, 16, multu_gap_8, RS_MEDIUM, RT),
    probe(0x7F, 16, multu_gap_9, RS_MEDIUM, RT),
    probe(0x80, 16, multu_gap_10, RS_MEDIUM, RT),
    probe(0x81, 16, multu_gap_0, RS_LARGE, RT),
    probe(0x82, 16, multu_gap_12, RS_LARGE, RT),
    probe(0x83, 16, multu_gap_13, RS_LARGE, RT),
    probe(0x84, 16, multu_gap_14, RS_LARGE, RT),
    probe(0x85, 8, divu_gap_0, NUMERATOR, DIVISOR),
    probe(0x86, 8, divu_gap_34, NUMERATOR, DIVISOR),
    probe(0x87, 8, divu_gap_36, NUMERATOR, DIVISOR),
    probe(0x88, 8, divu_gap_38, NUMERATOR, DIVISOR),
    probe(0x89, 8, divu_gap_40, NUMERATOR, DIVISOR),
    // Is the band chosen by rs alone? Small rs against a large rt, and a
    // signed multiply whose rs is a small negative number.
    probe(0x8A, 16, multu_gap_0, RS_SMALL, RS_LARGE),
    probe(0x8B, 16, mult_gap_0, Arg::Imm(-5i32 as u32), RT),
    // 32 alternating calls to two one-line leaves: 4 KiB apart they evict
    // each other on every call, on neighbouring lines they never do.
    probe(0x8C, 32, warm_call_pairs, Arg::EntryW0, Arg::AliasB).uncached(),
    probe(0x8D, 32, warm_call_pairs, Arg::EntryW0, Arg::EntryW1).uncached(),
];

#[derive(Copy, Clone)]
struct AbProbe {
    id: u8,
    work: u16,
    target: Arg,
    /// Called once, untimed, in the register's normal state.
    warm: Arg,
    register: u32,
    /// XORed into the register for the timed call; 0 is the control.
    mask: u32,
    /// Flush the I-cache first, so the timed call is a cold sweep.
    cold: bool,
}

const fn ab_probe(id: u8, work: u16, target: Arg, register: u32, mask: u32) -> AbProbe {
    AbProbe {
        id,
        work,
        target,
        warm: target,
        register,
        mask,
        cold: false,
    }
}

impl AbProbe {
    /// Cold sweep: warm a leaf that is not the target, after a flush.
    const fn cold(mut self) -> Self {
        self.warm = Arg::EntryW0;
        self.cold = true;
        self
    }
}

const CODE_DATA_DELAY: u32 = 1 << 7;
const IBLKSZ_LOW: u32 = 1 << 8;
const RDPRI: u32 = 1 << 13;
const NOPAD: u32 = 1 << 14;
const BGNT: u32 = 1 << 15;
const LDSCH: u32 = 1 << 16;
const NOSTR: u32 = 1 << 17;

// Each pair is control then flipped, through byte-identical code. INTP (bit
// 12) is left alone: it has no performance reading. BGNT goes last because
// bus grant is the bit most likely to stop the machine.
const RISKY: [AbProbe; 17] = [
    ab_probe(0xDC, 64, Arg::LoadsUncached, RAM_SIZE, 0),
    ab_probe(0xDD, 64, Arg::LoadsUncached, RAM_SIZE, CODE_DATA_DELAY),
    ab_probe(0xDE, 64, Arg::Loads, RAM_SIZE, 0),
    ab_probe(0xDF, 64, Arg::Loads, RAM_SIZE, CODE_DATA_DELAY),
    ab_probe(0xE0, 64, Arg::Loads, CACHE_CONTROL, 0),
    ab_probe(0xE1, 1024, Arg::IcacheBlock, CACHE_CONTROL, 0).cold(),
    ab_probe(0xE2, 64, Arg::Loads, CACHE_CONTROL, RDPRI),
    ab_probe(0xE3, 1024, Arg::IcacheBlock, CACHE_CONTROL, RDPRI).cold(),
    ab_probe(0xE4, 64, Arg::Loads, CACHE_CONTROL, NOPAD),
    ab_probe(0xE5, 1024, Arg::IcacheBlock, CACHE_CONTROL, NOPAD).cold(),
    ab_probe(0xE6, 64, Arg::Loads, CACHE_CONTROL, LDSCH),
    ab_probe(0xE7, 1024, Arg::IcacheBlock, CACHE_CONTROL, LDSCH).cold(),
    ab_probe(0xE8, 64, Arg::Loads, CACHE_CONTROL, NOSTR),
    ab_probe(0xE9, 1024, Arg::IcacheBlock, CACHE_CONTROL, NOSTR).cold(),
    // The emulator models this one (2-word refill on a word-0 miss), so it
    // doubles as a cross-check that the flip and the restore both happen.
    ab_probe(0xEA, 1024, Arg::IcacheBlock, CACHE_CONTROL, IBLKSZ_LOW).cold(),
    ab_probe(0xEB, 64, Arg::Loads, CACHE_CONTROL, BGNT),
    ab_probe(0xEC, 1024, Arg::IcacheBlock, CACHE_CONTROL, BGNT).cold(),
];

pub(crate) fn push_safe(records: &mut [TimingRecord; TIMING_RECORD_COUNT], next: &mut usize) {
    for entry in SAFE {
        let (a, b) = (entry.a.resolve(), entry.b.resolve());
        let record = sample_timing(entry.id, entry.work, || {
            if entry.uncached {
                call_uncached(entry.run, a, b)
            } else {
                (entry.run)(a, b)
            }
        });
        push_timing_record(records, next, record);
    }
}

pub(crate) fn push_risky(records: &mut [TimingRecord; TIMING_RECORD_COUNT], next: &mut usize) {
    let data = Arg::RamWord.resolve();
    for entry in RISKY {
        let (target, warm) = (entry.target.resolve(), entry.warm.resolve());
        let record = sample_timing(entry.id, entry.work, || {
            if entry.cold {
                flush_icache_without_irq();
            }
            call_uncached_ab(target, warm, entry.register, entry.mask, data)
        });
        push_timing_record(records, next, record);
    }
}

#[inline(never)]
fn call_uncached(wrapper: WarmFn, a: u32, b: u32) -> u16 {
    let address = (wrapper as usize & 0x1FFF_FFFF) | 0xA000_0000;
    let uncached: WarmFn = unsafe { core::mem::transmute(address) };
    uncached(a, b)
}

#[inline(never)]
fn call_uncached_ab(target: u32, warm: u32, register: u32, mask: u32, data: u32) -> u16 {
    type AbFn = fn(u32, u32, u32, u32, u32) -> u16;
    let address = (timed_register_flip as *const () as usize & 0x1FFF_FFFF) | 0xA000_0000;
    let uncached: AbFn = unsafe { core::mem::transmute(address) };
    uncached(target, warm, register, mask, data)
}

/// One warm probe: `$payload` is timed on its second pass. `$8`/`$9` carry
/// the two arguments and `$10` is scratch; the harness owns `$11`-`$13`, and
/// its loop label is `2` so a payload may use `1`.
macro_rules! warm_probe {
    ($name:ident, $id:literal, $payload:expr) => {
        #[inline(never)]
        fn $name(a: u32, b: u32) -> u16 {
            let elapsed: u32;
            unsafe {
                core::arch::asm!(
                    concat!(
                        ".set noreorder\n",
                        ".balign 16\n",
                        ".word 0x34000000 | (", stringify!($id), " << 1)\n",
                        "lui $11, 0x1F80\n",
                        "ori $11, $11, 0x1120\n",
                        "addiu $13, $zero, 2\n",
                        "2:\n",
                        "sw $zero, 4($11)\n",
                        "sw $zero, 0($11)\n",
                        $payload,
                        "lw $12, 0($11)\n",
                        "addiu $13, $13, -1\n",
                        "bnez $13, 2b\n",
                        "nop\n",
                        ".word 0x34000001 | (", stringify!($id), " << 1)\n",
                        ".set reorder"
                    ),
                    inout("$8") a => _,
                    inout("$9") b => _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") elapsed,
                    lateout("$13") _,
                    options(nostack)
                );
            }
            elapsed as u16
        }
    };
}

/// `$count` x (`$op` $8,$9; `$gap` nops; mflo $10).
macro_rules! muldiv_gap_probe {
    ($name:ident, $id:literal, $count:literal, $op:literal, $gap:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept ",
                stringify!($count),
                "\n",
                ".word ",
                stringify!($op),
                "\n",
                ".rept ",
                stringify!($gap),
                "\n",
                "nop\n",
                ".endr\n",
                ".word 0x00005012\n", // mflo $10
                ".endr\n"
            )
        );
    };
}

warm_probe!(warm_nops, 32, ".rept 128\nnop\n.endr\n");
warm_probe!(
    warm_dependent_alu,
    33,
    ".rept 128\naddiu $8, $8, 1\n.endr\n"
);
warm_probe!(warm_loads, 34, ".rept 64\nlw $9, 0($8)\nnop\n.endr\n");
warm_probe!(warm_stores, 35, ".rept 64\nsw $zero, 0($8)\n.endr\n");
warm_probe!(
    warm_taken_branches,
    36,
    ".rept 64\nbeq $zero, $zero, 1f\nnop\n1:\n.endr\n"
);
warm_probe!(
    warm_call_pairs,
    37,
    ".rept 32\njalr $10, $8\nnop\njalr $10, $9\nnop\n.endr\n"
);

// 0x01090019 = multu $8,$9; 0x01090018 = mult $8,$9; 0x0109001B = divu $8,$9.
muldiv_gap_probe!(multu_gap_0, 40, 16, 0x01090019, 0);
muldiv_gap_probe!(multu_gap_5, 41, 16, 0x01090019, 5);
muldiv_gap_probe!(multu_gap_6, 42, 16, 0x01090019, 6);
muldiv_gap_probe!(multu_gap_7, 43, 16, 0x01090019, 7);
muldiv_gap_probe!(multu_gap_8, 44, 16, 0x01090019, 8);
muldiv_gap_probe!(multu_gap_9, 45, 16, 0x01090019, 9);
muldiv_gap_probe!(multu_gap_10, 46, 16, 0x01090019, 10);
muldiv_gap_probe!(multu_gap_12, 47, 16, 0x01090019, 12);
muldiv_gap_probe!(multu_gap_13, 48, 16, 0x01090019, 13);
muldiv_gap_probe!(multu_gap_14, 49, 16, 0x01090019, 14);
muldiv_gap_probe!(mult_gap_0, 50, 16, 0x01090018, 0);
muldiv_gap_probe!(divu_gap_0, 51, 8, 0x0109001B, 0);
muldiv_gap_probe!(divu_gap_34, 52, 8, 0x0109001B, 34);
muldiv_gap_probe!(divu_gap_36, 53, 8, 0x0109001B, 36);
muldiv_gap_probe!(divu_gap_38, 54, 8, 0x0109001B, 38);
muldiv_gap_probe!(divu_gap_40, 55, 8, 0x0109001B, 40);

/// Time one call to `target` with `mask` XORed into `register`, then put the
/// register back. The flip, the workload and the restore are one assembly
/// block reached through KSEG1 with interrupts masked, so no other code ever
/// runs in the flipped state and the restore does not depend on the cache.
/// With `mask` = 0 this is the control, through the same instructions.
#[inline(never)]
fn timed_register_flip(target: u32, warm: u32, register: u32, mask: u32, data: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".balign 16",
            ".word 0x34000070", // probe 56 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "lw $15, 0($9)",
            "nop",
            "xor $13, $15, $14",
            "jalr $10, $24", // untimed, register in its normal state
            "nop",
            "sw $13, 0($9)",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            "sw $15, 0($9)",
            ".word 0x34000071", // probe 56 end marker
            ".set reorder",
            in("$8") target,
            in("$24") warm,
            in("$9") register,
            in("$14") mask,
            in("$25") data,
            lateout("$3") _,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            lateout("$13") _,
            lateout("$15") _,
            options(nostack)
        );
    }
    elapsed as u16
}
