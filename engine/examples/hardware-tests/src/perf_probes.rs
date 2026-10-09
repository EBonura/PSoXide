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
//!   can hang the console, so these are the last step of the linear run
//!   (`REGISTER A/B (CAN HANG)`), skipped when L2 is held at the start. The
//!   record id is on screen while each one runs: a hang names its culprit.
//!
//! Marker ids 32 and up belong to this file (see the scheme in main.rs).

use crate::regs::{CACHE_CONTROL, RAM_SIZE, SPU_DELAY};
use crate::{__hwtest_icache_alias_b, __hwtest_icache_entry_w1, __hwtest_perf_loads};
use crate::{__hwtest_icache_block, __hwtest_icache_entry_w0, TIMING_RECORD_COUNT};
use crate::{__hwtest_icache_load_block, __hwtest_perf_cached_pairs, seed_gte_state};
use crate::{flush_icache_without_irq, push_timing_record, sample_timing, TimingRecord};
use psx_io::dma;
use psx_io::gpu as gpu_io;

const SCRATCHPAD: u32 = 0x1F80_0000;
const GPUSTAT: u32 = 0x1F80_1814;
const GP0: u32 = 0x1F80_1810;
const I_STAT: u32 = 0x1F80_1070;
/// TRANSFER_CTRL + SPUSTAT as one aligned word: two halfword bus accesses,
/// no side effects on read.
const SPU_STATUS_WORD: u32 = 0x1F80_1DAC;
/// A read/write SPU register with no function (psx-spx: "unknown").
const SPU_SPARE: u32 = 0x1F80_1DBC;

/// 16 words so the sequential-address probe has somewhere to walk, plus slack
/// for the unaligned store pair, which writes one byte past a word.
static mut PERF_WORDS: [u32; 18] = [0; 18];

/// A probe argument that is only known at run time.
#[derive(Copy, Clone)]
enum Arg {
    Imm(u32),
    /// A word of cached main RAM.
    RamWord,
    /// One byte into that word: the address an unaligned access uses.
    RamUnaligned,
    /// The same word through KSEG1.
    RamWordKseg1,
    /// 4 KiB of `lw; nop` pairs covering every I-cache line.
    IcacheLoadBlock,
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
            Self::RamWord => (&raw const PERF_WORDS) as u32,
            Self::RamUnaligned => (&raw const PERF_WORDS) as u32 + 1,
            Self::RamWordKseg1 => ((&raw const PERF_WORDS) as u32 & 0x1FFF_FFFF) | 0xA000_0000,
            Self::IcacheLoadBlock => __hwtest_icache_load_block as *const () as u32,
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
    id: u16,
    work: u16,
    run: WarmFn,
    a: Arg,
    b: Arg,
    /// Run the wrapper through KSEG1 so it cannot disturb the lines it measures.
    uncached: bool,
    /// Load a defined GTE state first, so a command's inputs are not whatever
    /// the last test left behind.
    seed_gte: bool,
    /// Flush the I-cache before each sample, so the block runs cold.
    cold: bool,
}

const fn probe(id: u16, work: u16, run: WarmFn, a: Arg, b: Arg) -> Probe {
    Probe {
        id,
        work,
        run,
        a,
        b,
        uncached: false,
        seed_gte: false,
        cold: false,
    }
}

impl Probe {
    const fn uncached(mut self) -> Self {
        self.uncached = true;
        self
    }

    const fn gte(mut self) -> Self {
        self.seed_gte = true;
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

// GTE command words: 0x4A000000 | sf << 19 | command. NCLIP takes no sf.
const T_SMALL: Arg = Arg::Imm(0x0000_0400);
const LERP_A: Arg = Arg::Imm(0x0000_1234);

/// The rest of the performance sweep, the `EXTENDED PROBES AND SHAPES` step.
const EXTENDED: [Probe; 39] = [
    probe(0x1E, 128, warm_nops, NONE, NONE).uncached(),
    // Each store is followed by three independent instructions, which a
    // four-deep write queue should absorb for free. This probe measures
    // that claim against 0x76's 64 back-to-back stores.
    probe(0x1F, 64, warm_stores_spaced, Arg::RamWord, NONE),
    // Warm GTE command latency: back-to-back commands, each stalling until
    // the one before it has finished.
    probe(0x27, 16, gte_rtps, NONE, NONE).gte(),
    probe(0x28, 8, gte_rtpt, NONE, NONE).gte(),
    probe(0x29, 16, gte_nclip, NONE, NONE).gte(),
    probe(0x2A, 16, gte_mvmva, NONE, NONE).gte(),
    probe(0x2B, 16, gte_avsz3, NONE, NONE).gte(),
    probe(0x2C, 16, gte_sqr, NONE, NONE).gte(),
    probe(0x2D, 16, gte_op, NONE, NONE).gte(),
    probe(0x2E, 16, gte_gpf, NONE, NONE).gte(),
    probe(0x2F, 8, gte_ncds, NONE, NONE).gte(),
    // A second multiply issued while the first is still running: does it
    // queue behind it, replace it, or cost nothing until the read?
    probe(0x8E, 16, multu_back_to_back, RS_SMALL, RT),
    probe(0x8F, 16, multu_back_to_back, RS_LARGE, RT),
    // Data access shapes the compiler emits all the time.
    probe(0xC8, 64, warm_loads_back_to_back, Arg::RamWord, NONE),
    probe(
        0xC9,
        64,
        warm_loads_back_to_back,
        Arg::Imm(SCRATCHPAD),
        NONE,
    ),
    probe(0xCA, 64, warm_byte_loads, Arg::RamWord, NONE),
    probe(0xCC, 64, warm_unaligned_loads, Arg::RamUnaligned, NONE),
    probe(0xCD, 64, warm_unaligned_stores, Arg::RamUnaligned, NONE),
    // Load scheduling: a load followed by independent instructions should
    // hide part of its wait behind them. Against 0x74, load then nop.
    probe(0xCE, 64, warm_loads_spaced, Arg::RamWord, NONE),
    probe(0xCF, 64, warm_sequential_loads, Arg::RamWord, NONE),
    // GTE gap sweep: independent instructions behind a command before the
    // next command stalls. Knees expected at the command's latency.
    probe(0xED, 8, rtpt_gap_21, NONE, NONE).gte(),
    probe(0xEE, 8, rtpt_gap_23, NONE, NONE).gte(),
    probe(0xEF, 8, rtpt_gap_25, NONE, NONE).gte(),
    probe(0xF0, 16, rtps_gap_13, NONE, NONE).gte(),
    probe(0xF1, 16, rtps_gap_15, NONE, NONE).gte(),
    probe(0xF2, 16, rtps_gap_17, NONE, NONE).gte(),
    // Coprocessor register moves: three or more per vertex in every loop.
    probe(0xF3, 16, gte_mtc2, NONE, NONE).gte(),
    probe(0xF4, 16, gte_ctc2, NONE, NONE).gte(),
    probe(0xF5, 16, gte_mfc2, NONE, NONE).gte(),
    // The same three-component Q12 lerp on the CPU and through GPF.
    probe(0xF7, 8, lerp3_cpu, LERP_A, T_SMALL),
    probe(0xF8, 8, lerp3_gte, LERP_A, T_SMALL).gte(),
    // I/O ports touched from inner loops.
    probe(0xF9, 64, warm_loads, Arg::Imm(GPUSTAT), NONE),
    probe(0xFA, 64, warm_stores, Arg::Imm(GP0), NONE),
    probe(0xFB, 64, warm_loads, Arg::Imm(I_STAT), NONE),
    probe(
        0xFC,
        64,
        warm_half_loads,
        Arg::Imm(SPU_STATUS_WORD + 2),
        NONE,
    ),
    probe(0xFD, 64, warm_half_stores, Arg::Imm(SPU_SPARE), NONE),
    // A byte store, and a store through KSEG1, which has to wait for the
    // write queue to drain.
    probe(0x37, 64, warm_byte_stores, Arg::RamWord, NONE),
    probe(0x39, 64, warm_stores, Arg::RamWordKseg1, NONE),
    // psx-spx's GTE pipeline page: mtc2 does not stall while a command runs
    // and RTPT has latched its inputs within about four cycles, so the next
    // triple can be loaded behind the current command. If so this equals
    // 0x28, RTPT with nothing behind it.
    probe(0x3A, 8, rtpt_then_next_inputs, NONE, NONE).gte(),
];

/// v1.23: the shapes the v1.22 console captures left open. Ids from `0x120`.
const SHAPES: [Probe; 22] = [
    // The multiply interlock between k = 0 and k = m - 1. v1.22 found the
    // pair one clock cheaper at k = 5 than at k = 0 or k = 6 (latency 6);
    // these say whether it is flat in between, as the emulator now assumes.
    probe(0x120, 16, multu_gap_1, RS_SMALL, RT),
    probe(0x121, 16, multu_gap_2, RS_SMALL, RT),
    probe(0x122, 16, multu_gap_3, RS_SMALL, RT),
    probe(0x123, 16, multu_gap_4, RS_SMALL, RT),
    probe(0x124, 8, divu_gap_35, NUMERATOR, DIVISOR),
    // The load shadow: v1.22 has a load then one instruction at 8 clocks and
    // a load then four at 8.9, so about two clocks hide. Where, exactly?
    probe(0x125, 64, load_then_2, Arg::RamWord, NONE),
    probe(0x126, 64, load_then_3, Arg::RamWord, NONE),
    probe(0x127, 64, load_then_6, Arg::RamWord, NONE),
    probe(0x128, 64, load_then_8, Arg::RamWord, NONE),
    // The write queue: a store then three instructions is free (v1.22), two
    // stores back to back are two clocks each. One and two instructions, and
    // bursts of 2, 4 and 8 stores followed by twice as many instructions.
    probe(0x129, 64, store_then_1, Arg::RamWord, NONE),
    probe(0x12A, 64, store_then_2, Arg::RamWord, NONE),
    probe(0x12B, 64, store_burst_2, Arg::RamWord, NONE),
    probe(0x12C, 64, store_burst_4, Arg::RamWord, NONE),
    probe(0x12D, 64, store_burst_8, Arg::RamWord, NONE),
    probe(0x12E, 64, store_then_3, Arg::Imm(GP0), NONE),
    // Does a coprocessor read wait for the command? RTPS, the read, then 20
    // nops so the next RTPS never stalls: 22 clocks a turn if the read is
    // free, about 36 if it waits. The SCPH-9902 said free for MAC0 after
    // NCLIP; the v1.22 lerp on the launch console says MAC1-3 wait for GPF.
    probe(0x130, 16, rtps_read_sxy2, NONE, NONE).gte(),
    probe(0x131, 16, rtps_read_mac0, NONE, NONE).gte(),
    probe(0x132, 16, rtps_read_mac1, NONE, NONE).gte(),
    probe(0x133, 16, rtps_read_ir1, NONE, NONE).gte(),
    probe(0x134, 16, rtps_read_otz_after_gap, NONE, NONE).gte(),
    // The alias pair again, from a CACHED wrapper placed so that it cannot
    // share a line with either leaf. v1.22 measured 8 clocks a conflicting
    // call with the jump landing in uncached code; the emulator assumes a
    // cached landing costs the same.
    probe(0x135, 32, cached_call_pairs, Arg::EntryW0, Arg::AliasB),
    probe(0x136, 32, cached_call_pairs, Arg::EntryW0, Arg::EntryW1),
];

/// v1.24: the scratchpad-stack lever (lever_probes.rs). One call of
/// hello-spstack's `level2` (16 `level3` calls) on the RAM stack and on a
/// scratchpad stack, idle and while channel 2 walks a 2048-node empty list.
/// Warm records 0x74/0x75 already price a bare RAM load against a scratchpad
/// one; these ask what that is worth to compiled code with spills and frame
/// arrays.
const LEVERS: [Probe; 4] = [
    probe(0x137, 16, crate::lever_probes::level2_ram_stack, NONE, NONE),
    probe(
        0x138,
        16,
        crate::lever_probes::level2_scratchpad_stack,
        NONE,
        NONE,
    ),
    probe(
        0x139,
        16,
        crate::lever_probes::level2_ram_stack_during_dma,
        NONE,
        NONE,
    ),
    probe(
        0x13A,
        16,
        crate::lever_probes::level2_scratchpad_stack_during_dma,
        NONE,
        NONE,
    ),
];

/// v2.0: every GTE command's latency as the CPU sees it, three ways. For each
/// of 22 commands: the command then an immediate `mfc2` of MAC0, the same
/// with MAC1, and the command back to back with itself. Whether a coprocessor
/// read waits for the command, and for which registers, is the unknown; the
/// back-to-back row is the command's issue-to-issue time for comparison.
/// Ids `0x150`-`0x191`, three a command in the order of `GTE_LATENCY_NAMES`.
const GTE_LATENCY: [Probe; 66] = [
    probe(0x150, 16, lat_rtps_mac0, NONE, NONE).gte(),
    probe(0x151, 16, lat_rtps_mac1, NONE, NONE).gte(),
    probe(0x152, 16, lat_rtps_b2b, NONE, NONE).gte(),
    probe(0x153, 16, lat_nclip_mac0, NONE, NONE).gte(),
    probe(0x154, 16, lat_nclip_mac1, NONE, NONE).gte(),
    probe(0x155, 16, lat_nclip_b2b, NONE, NONE).gte(),
    probe(0x156, 16, lat_op_mac0, NONE, NONE).gte(),
    probe(0x157, 16, lat_op_mac1, NONE, NONE).gte(),
    probe(0x158, 16, lat_op_b2b, NONE, NONE).gte(),
    probe(0x159, 16, lat_dpcs_mac0, NONE, NONE).gte(),
    probe(0x15A, 16, lat_dpcs_mac1, NONE, NONE).gte(),
    probe(0x15B, 16, lat_dpcs_b2b, NONE, NONE).gte(),
    probe(0x15C, 16, lat_intpl_mac0, NONE, NONE).gte(),
    probe(0x15D, 16, lat_intpl_mac1, NONE, NONE).gte(),
    probe(0x15E, 16, lat_intpl_b2b, NONE, NONE).gte(),
    probe(0x15F, 16, lat_mvmva_mac0, NONE, NONE).gte(),
    probe(0x160, 16, lat_mvmva_mac1, NONE, NONE).gte(),
    probe(0x161, 16, lat_mvmva_b2b, NONE, NONE).gte(),
    probe(0x162, 8, lat_ncds_mac0, NONE, NONE).gte(),
    probe(0x163, 8, lat_ncds_mac1, NONE, NONE).gte(),
    probe(0x164, 8, lat_ncds_b2b, NONE, NONE).gte(),
    probe(0x165, 8, lat_cdp_mac0, NONE, NONE).gte(),
    probe(0x166, 8, lat_cdp_mac1, NONE, NONE).gte(),
    probe(0x167, 8, lat_cdp_b2b, NONE, NONE).gte(),
    probe(0x168, 8, lat_ncdt_mac0, NONE, NONE).gte(),
    probe(0x169, 8, lat_ncdt_mac1, NONE, NONE).gte(),
    probe(0x16A, 8, lat_ncdt_b2b, NONE, NONE).gte(),
    probe(0x16B, 8, lat_nccs_mac0, NONE, NONE).gte(),
    probe(0x16C, 8, lat_nccs_mac1, NONE, NONE).gte(),
    probe(0x16D, 8, lat_nccs_b2b, NONE, NONE).gte(),
    probe(0x16E, 16, lat_cc_mac0, NONE, NONE).gte(),
    probe(0x16F, 16, lat_cc_mac1, NONE, NONE).gte(),
    probe(0x170, 16, lat_cc_b2b, NONE, NONE).gte(),
    probe(0x171, 8, lat_ncs_mac0, NONE, NONE).gte(),
    probe(0x172, 8, lat_ncs_mac1, NONE, NONE).gte(),
    probe(0x173, 8, lat_ncs_b2b, NONE, NONE).gte(),
    probe(0x174, 8, lat_nct_mac0, NONE, NONE).gte(),
    probe(0x175, 8, lat_nct_mac1, NONE, NONE).gte(),
    probe(0x176, 8, lat_nct_b2b, NONE, NONE).gte(),
    probe(0x177, 16, lat_sqr_mac0, NONE, NONE).gte(),
    probe(0x178, 16, lat_sqr_mac1, NONE, NONE).gte(),
    probe(0x179, 16, lat_sqr_b2b, NONE, NONE).gte(),
    probe(0x17A, 16, lat_dcpl_mac0, NONE, NONE).gte(),
    probe(0x17B, 16, lat_dcpl_mac1, NONE, NONE).gte(),
    probe(0x17C, 16, lat_dcpl_b2b, NONE, NONE).gte(),
    probe(0x17D, 8, lat_dpct_mac0, NONE, NONE).gte(),
    probe(0x17E, 8, lat_dpct_mac1, NONE, NONE).gte(),
    probe(0x17F, 8, lat_dpct_b2b, NONE, NONE).gte(),
    probe(0x180, 16, lat_avsz3_mac0, NONE, NONE).gte(),
    probe(0x181, 16, lat_avsz3_mac1, NONE, NONE).gte(),
    probe(0x182, 16, lat_avsz3_b2b, NONE, NONE).gte(),
    probe(0x183, 16, lat_avsz4_mac0, NONE, NONE).gte(),
    probe(0x184, 16, lat_avsz4_mac1, NONE, NONE).gte(),
    probe(0x185, 16, lat_avsz4_b2b, NONE, NONE).gte(),
    probe(0x186, 8, lat_rtpt_mac0, NONE, NONE).gte(),
    probe(0x187, 8, lat_rtpt_mac1, NONE, NONE).gte(),
    probe(0x188, 8, lat_rtpt_b2b, NONE, NONE).gte(),
    probe(0x189, 16, lat_gpf_mac0, NONE, NONE).gte(),
    probe(0x18A, 16, lat_gpf_mac1, NONE, NONE).gte(),
    probe(0x18B, 16, lat_gpf_b2b, NONE, NONE).gte(),
    probe(0x18C, 16, lat_gpl_mac0, NONE, NONE).gte(),
    probe(0x18D, 16, lat_gpl_mac1, NONE, NONE).gte(),
    probe(0x18E, 16, lat_gpl_b2b, NONE, NONE).gte(),
    probe(0x18F, 8, lat_ncct_mac0, NONE, NONE).gte(),
    probe(0x190, 8, lat_ncct_mac1, NONE, NONE).gte(),
    probe(0x191, 8, lat_ncct_b2b, NONE, NONE).gte(),
];

/// The emulator timing audit's list (`TIMING-AUDIT-STEPS.md`), M1 to M8 and
/// M10, as warm-harness records: Timer 2 on the system clock unless the
/// comment says otherwise, interrupts masked, five samples.
const AUDIT: [Probe; 46] = [
    // M1: RTPS then swc2 / cfc2 / lwc2 (16 turns, 20 nops), against 0x130.
    probe(
        0x192,
        16,
        m1_swc2_sxy2_scratchpad,
        Arg::Imm(SCRATCHPAD),
        NONE,
    )
    .gte(),
    probe(0x193, 16, m1_cfc2_flag, NONE, NONE).gte(),
    probe(
        0x194,
        16,
        m1_lwc2_zero_scratchpad,
        Arg::Imm(SCRATCHPAD),
        NONE,
    )
    .gte(),
    // M2: the swc2 again, to cached RAM.
    probe(0x195, 16, m2_swc2_sxy2_ram, Arg::RamWord, NONE).gte(),
    // M3: MMIO read cost, 64 x (lw, nop), timed by a counter that is not the
    // one read: Timer 0 counter, DMA2 CHCR and DPCR by Timer 2, Timer 2's
    // counter by Timer 0.
    probe(0x1A0, 64, warm_loads, Arg::Imm(0x1F80_1100), NONE),
    probe(0x1A1, 64, warm_loads, Arg::Imm(0x1F80_10A8), NONE),
    probe(0x1A2, 64, warm_loads, Arg::Imm(0x1F80_10F0), NONE),
    probe(
        0x1A3,
        64,
        m3_timer2_read_timed_by_timer0,
        Arg::Imm(0x1F80_1120),
        NONE,
    ),
    // M4: scratchpad lb, lbu, lh, lhu, sb, sh, 64 each with a nop.
    probe(
        0x1A8,
        64,
        warm_signed_byte_loads,
        Arg::Imm(SCRATCHPAD),
        NONE,
    ),
    probe(0x1A9, 64, warm_byte_loads, Arg::Imm(SCRATCHPAD), NONE),
    probe(
        0x1AA,
        64,
        warm_signed_half_loads,
        Arg::Imm(SCRATCHPAD),
        NONE,
    ),
    probe(0x1AB, 64, warm_half_loads, Arg::Imm(SCRATCHPAD), NONE),
    probe(0x1AC, 64, warm_byte_stores_nop, Arg::Imm(SCRATCHPAD), NONE),
    probe(0x1AD, 64, warm_half_stores_nop, Arg::Imm(SCRATCHPAD), NONE),
    // M5: IsC clear against the scratchpad (control), IsC set, IsC and TAG
    // set; 64 sw and 64 lw each, run through KSEG1.
    probe(0x1B0, 64, isolate_control_sw, Arg::Imm(SCRATCHPAD), NONE).uncached(),
    probe(0x1B1, 64, isolate_control_lw, Arg::Imm(SCRATCHPAD), NONE).uncached(),
    probe(0x1B2, 64, isolate_data_sw, Arg::RamWord, NONE).uncached(),
    probe(0x1B3, 64, isolate_data_lw, Arg::RamWord, NONE).uncached(),
    probe(0x1B4, 64, isolate_tag_sw, Arg::RamWord, NONE).uncached(),
    probe(0x1B5, 64, isolate_tag_lw, Arg::RamWord, NONE).uncached(),
    // M7: the 4 KiB evictor before every sample, then (a) 64 sw to RAM, (b) 64
    // sw to the scratchpad, (c) 32 x (lw GPUSTAT, nop), (d) 32 x (lw RAM, nop),
    // (e) 64 nops.
    probe(0x1B8, 64, m7_sw_ram, Arg::RamWord, Arg::IcacheBlock),
    probe(
        0x1B9,
        64,
        m7_sw_scratchpad,
        Arg::Imm(SCRATCHPAD),
        Arg::IcacheBlock,
    ),
    probe(
        0x1BA,
        32,
        m7_gpustat_pairs,
        Arg::Imm(GPUSTAT),
        Arg::IcacheBlock,
    ),
    probe(0x1BB, 32, m7_ram_pairs, Arg::RamWord, Arg::IcacheBlock),
    probe(0x1BC, 64, m7_nops, NONE, Arg::IcacheBlock),
    // M8: warm BIOS ROM, EXP1 and EXP3, through KSEG1: 64 x (lw, nop), 64
    // bare lw, then half and byte (lhu, lbu with a nop).
    probe(0x1C0, 64, warm_loads, Arg::Imm(0xBFC0_0000), NONE),
    probe(
        0x1C1,
        64,
        warm_loads_back_to_back,
        Arg::Imm(0xBFC0_0000),
        NONE,
    ),
    probe(0x1C2, 64, warm_half_loads, Arg::Imm(0xBFC0_0000), NONE),
    probe(0x1C3, 64, warm_byte_loads, Arg::Imm(0xBFC0_0000), NONE),
    probe(0x1C4, 64, warm_loads, Arg::Imm(0xBF00_0000), NONE),
    probe(
        0x1C5,
        64,
        warm_loads_back_to_back,
        Arg::Imm(0xBF00_0000),
        NONE,
    ),
    probe(0x1C6, 64, warm_half_loads, Arg::Imm(0xBF00_0000), NONE),
    probe(0x1C7, 64, warm_byte_loads, Arg::Imm(0xBF00_0000), NONE),
    probe(0x1C8, 64, warm_loads, Arg::Imm(0xBFA0_0000), NONE),
    probe(
        0x1C9,
        64,
        warm_loads_back_to_back,
        Arg::Imm(0xBFA0_0000),
        NONE,
    ),
    probe(0x1CA, 64, warm_half_loads, Arg::Imm(0xBFA0_0000), NONE),
    probe(0x1CB, 64, warm_byte_loads, Arg::Imm(0xBFA0_0000), NONE),
    // M10: divu, multu small, mflo after a gap of 0, 10, 30 and 36 nops
    // against divu and 36 nops; multu large, mtlo, mflo; multu, mthi, mfhi.
    probe(0x1D0, 8, m10_divu_multu_gap0, RS_SMALL, DIVISOR),
    probe(0x1D1, 8, m10_divu_multu_gap10, RS_SMALL, DIVISOR),
    probe(0x1D2, 8, m10_divu_multu_gap30, RS_SMALL, DIVISOR),
    probe(0x1D3, 8, m10_divu_multu_gap36, RS_SMALL, DIVISOR),
    probe(0x1D4, 8, m10_divu_36_nops, RS_SMALL, DIVISOR),
    probe(0x1D5, 16, m10_multu_mtlo_mflo, RS_LARGE, RT),
    probe(0x1D6, 16, m10_multu_mthi_mfhi, RS_SMALL, RT),
    probe(0x1D7, 16, m10_multu_mthi_mfhi, RS_LARGE, RT),
    probe(0x1D8, 16, m10_multu_mtlo_mflo, RS_SMALL, RT),
];

#[derive(Copy, Clone)]
struct AbProbe {
    id: u16,
    work: u16,
    target: Arg,
    /// Called once, untimed, in the register's normal state.
    warm: Arg,
    register: u32,
    /// XORed into the register for the timed call; 0 is the control.
    mask: u32,
    /// What the workload's loads read.
    data: Arg,
    /// Flush the I-cache first, so the timed call is a cold sweep.
    cold: bool,
}

const fn ab_probe(id: u16, work: u16, target: Arg, register: u32, mask: u32) -> AbProbe {
    AbProbe {
        id,
        work,
        target,
        warm: target,
        register,
        mask,
        data: Arg::RamWord,
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

    const fn reading(mut self, data: Arg) -> Self {
        self.data = data;
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
/// SPU_DELAY's read-delay nibble (bits 4-7) is 0xE in the BIOS value
/// 0x200931E1. XORing 0xA0 makes it 0x4 there. On a console whose value
/// differs the result differs too; the capture's memory-control block says
/// what the register held.
const SPU_FASTER_READ: u32 = 0xA0;

const RISKY: [AbProbe; 21] = [
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
    // The realistic case for RAM_SIZE bit 7: a cold sweep of code that also
    // loads data, so line refills and data reads contend for RAM.
    ab_probe(0x3C, 511, Arg::IcacheLoadBlock, RAM_SIZE, 0).cold(),
    ab_probe(0x3D, 511, Arg::IcacheLoadBlock, RAM_SIZE, CODE_DATA_DELAY).cold(),
    // 64 word reads of SPU status (two halfword bus accesses each) with the
    // SPU bus read delay as found and shortened. A read that is too fast
    // returns garbage, which nothing here consumes.
    ab_probe(0x3E, 64, Arg::Loads, SPU_DELAY, 0).reading(Arg::Imm(SPU_STATUS_WORD)),
    ab_probe(0x3F, 64, Arg::Loads, SPU_DELAY, SPU_FASTER_READ).reading(Arg::Imm(SPU_STATUS_WORD)),
    ab_probe(0xEB, 64, Arg::Loads, CACHE_CONTROL, BGNT),
    ab_probe(0xEC, 1024, Arg::IcacheBlock, CACHE_CONTROL, BGNT).cold(),
];

type Records = [TimingRecord; TIMING_RECORD_COUNT];

fn push_probes(table: &[Probe], records: &mut Records, next: &mut usize) {
    for entry in table {
        let (a, b) = (entry.a.resolve(), entry.b.resolve());
        let record = sample_timing(entry.id, entry.work, || {
            if entry.seed_gte {
                seed_gte_state();
            }
            if entry.cold {
                flush_icache_without_irq();
            }
            if entry.uncached {
                call_uncached(entry.run, a, b)
            } else {
                (entry.run)(a, b)
            }
        });
        push_timing_record(records, next, record);
    }
}

pub(crate) fn push_safe(records: &mut Records, next: &mut usize) {
    push_probes(&SAFE, records, next);
    push_probes(&LEVERS, records, next);
}

pub(crate) fn push_extended(records: &mut Records, next: &mut usize) {
    push_probes(&EXTENDED, records, next);
    push_probes(&SHAPES, records, next);
    push_dma(records, next);
}

/// The GTE command latency table (v2.0), with the GTE seeded before each
/// sample so the inputs are defined.
pub(crate) fn push_gte_latency(records: &mut Records, next: &mut usize) {
    push_probes(&GTE_LATENCY, records, next);
}

/// The emulator timing audit's list (M1 to M11 bar the polled-timer one).
pub(crate) fn push_audit(records: &mut Records, next: &mut usize) {
    push_probes(&AUDIT, records, next);
}

/// Where the CPU's RAM loads wait on each DMA channel: 64 loads with the
/// channel idle and the same loads right after kicking a transfer big enough
/// to outlast them. GPU list walking is `push_dma`'s; these are the other
/// three: SPU block DMA, OTC clear and GPU block DMA. A channel that never
/// goes idle is stopped and described (`0x780`-`0x787`) and the next one
/// runs.
pub(crate) fn push_dma_channels(records: &mut Records, next: &mut usize) {
    type Overlap = fn(u32, u32, u32, u32) -> u16;
    let data = Arg::RamWord.resolve();
    let run: Overlap = timed_loads_with_dma;

    // SPU: 256 words as 16 blocks of 16, written to SPU RAM at 16 cycles a
    // halfword, which keeps the channel busy for thousands of cycles.
    dma_end_reset(DmaGroup::Spu);
    for (id, kick) in [(0x140u16, 0u32), (0x141, SPU_KICK)] {
        let record = sample_timing(id, 64, || {
            with_spu_dma(|base, head| run(base, head, kick, data))
        });
        push_timing_record(records, next, record);
    }
    push_dma_end(records, next, DmaGroup::Spu);
    // OTC: 1024 words cleared backwards, one RAM write a word.
    dma_end_reset(DmaGroup::Otc);
    for (id, kick) in [(0x142u16, 0u32), (0x143, OTC_KICK)] {
        let record = sample_timing(id, 64, || {
            with_otc_dma(|base, head| run(base, head, kick, data))
        });
        push_timing_record(records, next, record);
    }
    push_dma_end(records, next, DmaGroup::Otc);
    // GPU block DMA of NOP words: the channel reads RAM and the GPU takes it.
    dma_end_reset(DmaGroup::GpuBlock);
    for (id, kick) in [(0x144u16, 0u32), (0x145, GPU_BLOCK_KICK)] {
        let record = sample_timing(id, 64, || {
            with_gpu_block_dma(|base, head| run(base, head, kick, data))
        });
        push_timing_record(records, next, record);
    }
    push_dma_end(records, next, DmaGroup::GpuBlock);
}

const SPU_KICK: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START;
const GPU_BLOCK_KICK: u32 = SPU_KICK;
const OTC_KICK: u32 = psx_hw::dma::CHCR_STEP_BACKWARD
    | psx_hw::dma::CHCR_SYNC_MANUAL
    | psx_hw::dma::CHCR_START
    | psx_hw::dma::CHCR_TRIGGER;

static mut DMA_SOURCE: [u32; 256] = [0; 256];
const OTC_WORDS: usize = 1024;
static mut DMA_OT: [u32; OTC_WORDS] = [0; OTC_WORDS];

/// The DMA rows whose end state is described: each group's records are
/// `0x780 + 2 * group` and the next one.
#[derive(Copy, Clone)]
enum DmaGroup {
    Spu = 0,
    Otc = 1,
    GpuBlock = 2,
    GpuList = 3,
}

/// rec dma_end: timeout_flags, chcr_high_half, device_status (SPU, OTC, GPU block and GPU list groups, 0x780 0x782 0x784 0x786; flags bit 0 busy before a kick, bit 1 busy after the loop, bit 2 the SPU mode never matched, bit 3 something stopped; ffff in the status = none)
const DMA_END_RECORD: u16 = 0x780;
/// rec dma_end_registers: madr_low_half, bcr_high_half, bcr_low_half (the same groups, 0x781 0x783 0x785 0x787; read after the timeout, before the channel was stopped)
const DMA_END_REGISTERS_RECORD: u16 = 0x781;
/// rec spu_mode_wait: iterations_first, iterations_longest, mode_never_matched (SPUSTAT bits 5-0 against the SPUCNT mode before the SPU DMA kick, 0x788)
const SPU_MODE_WAIT_RECORD: u16 = 0x788;

#[derive(Copy, Clone)]
struct DmaEnd {
    flags: u32,
    chcr: u32,
    status: u32,
    madr: u32,
    bcr: u32,
    seen: bool,
}

const DMA_END_BLANK: DmaEnd = DmaEnd {
    flags: 0,
    chcr: 0,
    status: 0xFFFF,
    madr: 0,
    bcr: 0,
    seen: false,
};
static mut DMA_END: [DmaEnd; 4] = [DMA_END_BLANK; 4];
/// SPUSTAT wait before the SPU DMA kick: iterations the first and the
/// longest took.
static mut SPU_MODE_ITERATIONS: [u32; 3] = [0; 3];
const SPU_MODE_WAIT_ITERATIONS: u32 = 100_000;

fn dma_end_reset(group: DmaGroup) {
    // SAFETY: single thread; plain statics.
    unsafe {
        DMA_END[group as usize] = DMA_END_BLANK;
        DMA_WAIT_FLAGS = 0;
        if matches!(group, DmaGroup::Spu) {
            SPU_MODE_ITERATIONS = [0; 3];
        }
    }
}

/// After a probe call: take the wait flags, and if a wait ran out describe
/// the channel (CHCR, MADR, BCR and `status`) and stop it by clearing CHCR.
/// Without a timeout the last call's state is kept.
fn dma_settle(group: DmaGroup, channel: dma::Channel, status: u32) {
    // SAFETY: single thread; plain statics, and register reads.
    unsafe {
        let flags = DMA_WAIT_FLAGS;
        DMA_WAIT_FLAGS = 0;
        let end = &mut DMA_END[group as usize];
        end.flags |= flags;
        if flags != 0 || !end.seen {
            let base = channel.register_base();
            end.chcr = dma::control(channel);
            end.madr = dma::address(channel);
            end.bcr = psx_io::read_u32(base + 4);
            end.status = status;
            end.seen = flags == 0 || end.seen;
        }
        if flags & 3 != 0 {
            end.flags |= 8;
            dma::abort(channel);
        }
    }
}

fn push_dma_end(records: &mut Records, next: &mut usize, group: DmaGroup) {
    // SAFETY: single thread; plain statics.
    let end = unsafe { DMA_END[group as usize] };
    let id = DMA_END_RECORD + 2 * group as u16;
    push_timing_record(
        records,
        next,
        crate::console_tests::record(id, end.flags, end.chcr >> 16, end.status),
    );
    push_timing_record(
        records,
        next,
        crate::console_tests::record(
            DMA_END_REGISTERS_RECORD + 2 * group as u16,
            end.madr & 0xFFFF,
            end.bcr >> 16,
            end.bcr & 0xFFFF,
        ),
    );
    if matches!(group, DmaGroup::Spu) {
        // SAFETY: single thread; plain static.
        let wait = unsafe { SPU_MODE_ITERATIONS };
        push_timing_record(
            records,
            next,
            crate::console_tests::record(SPU_MODE_WAIT_RECORD, wait[0], wait[1], wait[2]),
        );
    }
}

/// Set the SPU up for a DMA write of 256 words and run `body(base, source)`,
/// then put SPUCNT back. The channel's address and size are set; the body's
/// assembly writes MADR and CHCR. SPUSTAT bits 5-0 follow SPUCNT's mode a
/// moment after it is written (psx-spx), and a kick before they match is the
/// suspect for a block write that never ends, so the wait is counted and its
/// length recorded; if it runs out the kick goes ahead and says so.
fn with_spu_dma(body: impl FnOnce(u32, u32) -> u16) -> u16 {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
    // SAFETY: SPU register writes and a DMA from memory this probe owns; the
    // body's waits are counted and a stuck channel is stopped below.
    unsafe {
        let old = psx_io::read_u16(SPUCNT);
        let stopped = old & !0x0030;
        let mut waits = [0u32; 2];
        for (slot, mode) in [stopped, stopped | 0x0020].into_iter().enumerate() {
            psx_io::write_u16(SPUCNT, mode);
            if slot == 1 {
                psx_io::write_u16(TRANSFER_CTRL, 0x0004);
                psx_io::write_u16(TRANSFER_ADDR, 0x0800);
            }
            let limit = crate::bounds::scale(SPU_MODE_WAIT_ITERATIONS);
            let mut iterations = 0u32;
            while psx_io::read_u16(SPUSTAT) & 0x003F != mode & 0x003F && iterations < limit {
                iterations += 1;
            }
            waits[slot] = iterations;
            if iterations >= limit && limit > 1 {
                DMA_WAIT_FLAGS |= 4;
            }
        }
        // The first sample's wait, the longest of all of them, how many ran out.
        let record = &mut *(&raw mut SPU_MODE_ITERATIONS);
        if record[0] == 0 {
            record[0] = waits[1];
        }
        record[1] = record[1].max(waits[1]);
        record[2] += (waits[1] >= SPU_MODE_WAIT_ITERATIONS) as u32;
        dma::enable_channel(dma::Channel::Spu);
        dma::raw::set_size(dma::Channel::Spu, dma::size_blocks(16, 16));
        let source = (&raw mut DMA_SOURCE) as u32;
        let elapsed = body(dma::Channel::Spu.register_base(), source);
        let status = psx_io::read_u16(SPUSTAT) as u32;
        dma_settle(DmaGroup::Spu, dma::Channel::Spu, status);
        // Mode 0 on the SPU side too, so a transfer that never ended is not
        // left armed.
        psx_io::write_u16(SPUCNT, stopped);
        psx_io::write_u16(SPUCNT, old);
        elapsed
    }
}

fn with_otc_dma(body: impl FnOnce(u32, u32) -> u16) -> u16 {
    // SAFETY: a DMA into a table this probe owns; the body's waits are
    // counted and a stuck channel is stopped below.
    unsafe {
        let table = (&raw mut DMA_OT) as *mut u32;
        dma::enable_channel(dma::Channel::OrderingTableClear);
        dma::raw::set_size(
            dma::Channel::OrderingTableClear,
            dma::size_words(OTC_WORDS as u16),
        );
        let end = table.add(OTC_WORDS - 1) as u32;
        let elapsed = body(dma::Channel::OrderingTableClear.register_base(), end);
        dma_settle(DmaGroup::Otc, dma::Channel::OrderingTableClear, 0xFFFF);
        elapsed
    }
}

fn with_gpu_block_dma(body: impl FnOnce(u32, u32) -> u16) -> u16 {
    let old_direction = (gpu_io::status().bits() >> 29) & 3;
    gpu_io::write_display_control(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    // SAFETY: a DMA of GP0 NOPs from memory this probe owns; the body's waits
    // are counted and a stuck channel is stopped below.
    let elapsed = unsafe {
        dma::raw::set_size(dma::Channel::Gpu, dma::size_blocks(16, 16));
        body(
            dma::Channel::Gpu.register_base(),
            (&raw mut DMA_SOURCE) as u32,
        )
    };
    let status = gpu_io::status().bits();
    dma_settle(DmaGroup::GpuBlock, dma::Channel::Gpu, status >> 16);
    // A transfer that never ended leaves the direction at 0 (off); one that
    // did gets the old direction back.
    let timed_out = unsafe { DMA_END[DmaGroup::GpuBlock as usize].flags & 8 != 0 };
    gpu_io::write_display_control(0x0400_0000 | if timed_out { 0 } else { old_direction });
    elapsed
}

pub(crate) fn push_risky(records: &mut Records, next: &mut usize) {
    for entry in RISKY {
        let (target, warm) = (entry.target.resolve(), entry.warm.resolve());
        let data = entry.data.resolve();
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

warm_probe!(
    warm_loads_back_to_back,
    58,
    ".rept 64\nlw $9, 0($8)\n.endr\n"
);
warm_probe!(warm_byte_loads, 59, ".rept 64\nlbu $9, 0($8)\nnop\n.endr\n");
// 0x89090003 = lwl $9,3($8); 0x99090000 = lwr $9,0($8): one unaligned word.
warm_probe!(
    warm_unaligned_loads,
    61,
    ".rept 64\n.word 0x89090003\n.word 0x99090000\n.endr\n"
);
// 0xA9090003 = swl $9,3($8); 0xB9090000 = swr $9,0($8).
warm_probe!(
    warm_unaligned_stores,
    62,
    ".rept 64\n.word 0xA9090003\n.word 0xB9090000\n.endr\n"
);
// Sixteen consecutive words, four times: does the memory controller reward
// sequential addresses? (`warm_loads` reads one word 64 times.)
warm_probe!(
    warm_sequential_loads,
    63,
    concat!(".rept 4\n", "lw $9, 0($8)\nnop\nlw $9, 4($8)\nnop\nlw $9, 8($8)\nnop\nlw $9, 12($8)\nnop\nlw $9, 16($8)\nnop\nlw $9, 20($8)\nnop\nlw $9, 24($8)\nnop\nlw $9, 28($8)\nnop\nlw $9, 32($8)\nnop\nlw $9, 36($8)\nnop\nlw $9, 40($8)\nnop\nlw $9, 44($8)\nnop\nlw $9, 48($8)\nnop\nlw $9, 52($8)\nnop\nlw $9, 56($8)\nnop\nlw $9, 60($8)\nnop\n", ".endr\n")
);
warm_probe!(warm_byte_stores, 64, ".rept 64\nsb $zero, 0($8)\n.endr\n");
warm_probe!(warm_half_stores, 65, ".rept 64\nsh $zero, 0($8)\n.endr\n");
// Sixteen multiplies with no read between them, then one mflo.
warm_probe!(
    multu_back_to_back,
    66,
    ".rept 16\n.word 0x01090019\n.endr\n.word 0x00005012\n"
);

/// `$count` x (GTE command `$word`; `$gap` nops). With no gap every command
/// stalls until the previous one finishes, so the total is the latency.
///
/// The 48 trailing nops outlast the slowest command (NCDT, 44 cycles), so the
/// timed pass always starts with the GTE idle. Without them its first command
/// inherits whatever the warm-up pass left running, and the reading moves by
/// a DRAM refresh slot depending on where that slot fell.
macro_rules! gte_probe {
    ($name:ident, $id:literal, $count:literal, $word:literal, $gap:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept ",
                stringify!($count),
                "\n",
                ".word ",
                stringify!($word),
                "\n",
                ".rept ",
                stringify!($gap),
                "\n",
                "nop\n",
                ".endr\n",
                ".endr\n",
                ".rept 48\nnop\n.endr\n"
            )
        );
    };
}

gte_probe!(gte_rtps, 68, 16, 0x4A080001, 0);
gte_probe!(gte_rtpt, 69, 8, 0x4A080030, 0);
gte_probe!(gte_nclip, 70, 16, 0x4A000006, 0);
gte_probe!(gte_mvmva, 71, 16, 0x4A080012, 0);
gte_probe!(gte_avsz3, 72, 16, 0x4A08002D, 0);
gte_probe!(gte_sqr, 73, 16, 0x4A080028, 0);
gte_probe!(gte_op, 74, 16, 0x4A08000C, 0);
gte_probe!(gte_gpf, 75, 16, 0x4A08003D, 0);
gte_probe!(gte_ncds, 76, 8, 0x4A080013, 0);
gte_probe!(rtpt_gap_21, 79, 8, 0x4A080030, 21);
gte_probe!(rtpt_gap_23, 80, 8, 0x4A080030, 23);
gte_probe!(rtpt_gap_25, 81, 8, 0x4A080030, 25);
gte_probe!(rtps_gap_13, 82, 16, 0x4A080001, 13);
gte_probe!(rtps_gap_15, 83, 16, 0x4A080001, 15);
gte_probe!(rtps_gap_17, 84, 16, 0x4A080001, 17);
// 0x48884800 = mtc2 $8,IR1; 0x48C82800 = ctc2 $8,TRX;
// 0x480A4800 = mfc2 $10,IR1.
gte_probe!(gte_mtc2, 85, 16, 0x48884800, 0);
gte_probe!(gte_ctc2, 86, 16, 0x48C82800, 0);
gte_probe!(gte_mfc2, 87, 16, 0x480A4800, 0);

// r = a + ((a * t) >> 12) for three components, `$8` = a, `$9` = t with t in
// `rs` so the multiply takes its fast band. 0x01280018 = mult $9,$8.
warm_probe!(
    lerp3_cpu,
    89,
    concat!(
        ".rept 8\n",
        ".rept 3\n",
        ".word 0x01280018\n",
        ".word 0x00005012\n",
        "sra $10, $10, 12\n",
        "addu $10, $10, $8\n",
        ".endr\n",
        ".endr\n"
    )
);
// The same through GPF: IR0 = t, IR1-3 = a, command, read MAC1-3.
// 0x48894000 = mtc2 $9,IR0; 0x48884800/5000/5800 = mtc2 $8,IR1/2/3;
// 0x4A08003D = GPF; 0x480AC800/D000/D800 = mfc2 $10,MAC1/2/3.
warm_probe!(
    lerp3_gte,
    90,
    concat!(
        ".rept 8\n",
        ".word 0x48894000\n",
        ".word 0x48884800\n",
        ".word 0x48885000\n",
        ".word 0x48885800\n",
        ".word 0x4A08003D\n",
        ".word 0x480AC800\n",
        ".word 0x480AD000\n",
        ".word 0x480AD800\n",
        ".endr\n",
        ".rept 48\nnop\n.endr\n"
    )
);

warm_probe!(
    warm_stores_spaced,
    57,
    ".rept 64\nsw $zero, 0($8)\naddiu $10, $10, 1\naddiu $10, $10, 1\naddiu $10, $10, 1\n.endr\n"
);
warm_probe!(warm_half_loads, 60, ".rept 64\nlhu $9, 0($8)\nnop\n.endr\n");
warm_probe!(
    warm_loads_spaced,
    93,
    ".rept 64\nlw $9, 0($8)\naddiu $10, $10, 1\naddiu $10, $10, 1\naddiu $10, $10, 1\naddiu $10, $10, 1\n.endr\n"
);
// RTPT, then the six mtc2 that load V0-V2 for the next one.
// 0x48880000.. = mtc2 $8, VXY0 / VZ0 / VXY1 / VZ1 / VXY2 / VZ2.
warm_probe!(
    rtpt_then_next_inputs,
    67,
    concat!(
        ".rept 8\n",
        ".word 0x4A080030\n",
        ".word 0x48880000\n",
        ".word 0x48880800\n",
        ".word 0x48881000\n",
        ".word 0x48881800\n",
        ".word 0x48882000\n",
        ".word 0x48882800\n",
        ".endr\n",
        ".rept 48\nnop\n.endr\n"
    )
);

// ---------------------------------------------------------------------------
// v1.23 shape probes
// ---------------------------------------------------------------------------

muldiv_gap_probe!(multu_gap_1, 94, 16, 0x01090019, 1);
muldiv_gap_probe!(multu_gap_2, 95, 16, 0x01090019, 2);
muldiv_gap_probe!(multu_gap_3, 96, 16, 0x01090019, 3);
muldiv_gap_probe!(multu_gap_4, 97, 16, 0x01090019, 4);
muldiv_gap_probe!(divu_gap_35, 98, 8, 0x0109001B, 35);

/// 64 x (`$access`, then `$count` independent instructions).
macro_rules! access_then_probe {
    ($name:ident, $id:literal, $access:literal, $count:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept 64\n",
                $access,
                "\n",
                ".rept ",
                stringify!($count),
                "\n",
                "addiu $10, $10, 1\n",
                ".endr\n",
                ".endr\n"
            )
        );
    };
}

access_then_probe!(load_then_2, 99, "lw $9, 0($8)", 2);
access_then_probe!(load_then_3, 100, "lw $9, 0($8)", 3);
access_then_probe!(load_then_6, 101, "lw $9, 0($8)", 6);
access_then_probe!(load_then_8, 102, "lw $9, 0($8)", 8);
access_then_probe!(store_then_1, 103, "sw $zero, 0($8)", 1);
access_then_probe!(store_then_2, 104, "sw $zero, 0($8)", 2);
access_then_probe!(store_then_3, 105, "sw $zero, 0($8)", 3);

/// 64 stores in bursts of `$burst`, each burst followed by twice as many
/// independent instructions.
macro_rules! store_burst_probe {
    ($name:ident, $id:literal, $bursts:literal, $burst:literal, $gap:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept ",
                stringify!($bursts),
                "\n",
                ".rept ",
                stringify!($burst),
                "\n",
                "sw $zero, 0($8)\n",
                ".endr\n",
                ".rept ",
                stringify!($gap),
                "\n",
                "addiu $10, $10, 1\n",
                ".endr\n",
                ".endr\n"
            )
        );
    };
}

store_burst_probe!(store_burst_2, 106, 32, 2, 4);
store_burst_probe!(store_burst_4, 107, 16, 4, 8);
store_burst_probe!(store_burst_8, 108, 8, 8, 16);

/// 16 x (RTPS; `$read`; 20 nops), then the usual idle tail.
macro_rules! rtps_read_probe {
    ($name:ident, $id:literal, $before:literal, $read:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept 16\n",
                ".word 0x4A080001\n",
                ".rept ",
                stringify!($before),
                "\n",
                "nop\n",
                ".endr\n",
                ".word ",
                stringify!($read),
                "\n",
                ".rept 20\n",
                "nop\n",
                ".endr\n",
                ".endr\n",
                ".rept 48\nnop\n.endr\n"
            )
        );
    };
}

// mfc2 $10 from SXY2 (14), MAC0 (24), MAC1 (25), IR1 (9), OTZ (7).
rtps_read_probe!(rtps_read_sxy2, 109, 0, 0x480A7000);
rtps_read_probe!(rtps_read_mac0, 110, 0, 0x480AC000);
rtps_read_probe!(rtps_read_mac1, 111, 0, 0x480AC800);
rtps_read_probe!(rtps_read_ir1, 112, 0, 0x480A4800);
// The control: the same read once RTPS (15 clocks) has certainly finished.
rtps_read_probe!(rtps_read_otz_after_gap, 113, 16, 0x480A3800);

/// `$reps` x (GTE command `$word`; `$read`), then the usual idle tail. The
/// read is `mfc2 $10` of MAC0 (0x480AC000) or MAC1 (0x480AC800): if it waits
/// for the command, the block costs the command's latency a turn; if it does
/// not, it costs two instructions. The back-to-back twin uses `gte_probe!`.
macro_rules! gte_read_probe {
    ($name:ident, $id:literal, $reps:literal, $word:literal, $read:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept ",
                stringify!($reps),
                "\n",
                ".word ",
                stringify!($word),
                "\n",
                ".word ",
                stringify!($read),
                "\n",
                ".endr\n",
                ".rept 48\nnop\n.endr\n"
            )
        );
    };
}

gte_read_probe!(lat_rtps_mac0, 130, 16, 0x4A080001, 0x480AC000);
gte_read_probe!(lat_rtps_mac1, 131, 16, 0x4A080001, 0x480AC800);
gte_probe!(lat_rtps_b2b, 132, 16, 0x4A080001, 0);
gte_read_probe!(lat_nclip_mac0, 133, 16, 0x4A000006, 0x480AC000);
gte_read_probe!(lat_nclip_mac1, 134, 16, 0x4A000006, 0x480AC800);
gte_probe!(lat_nclip_b2b, 135, 16, 0x4A000006, 0);
gte_read_probe!(lat_op_mac0, 136, 16, 0x4A08000C, 0x480AC000);
gte_read_probe!(lat_op_mac1, 137, 16, 0x4A08000C, 0x480AC800);
gte_probe!(lat_op_b2b, 138, 16, 0x4A08000C, 0);
gte_read_probe!(lat_dpcs_mac0, 139, 16, 0x4A080010, 0x480AC000);
gte_read_probe!(lat_dpcs_mac1, 140, 16, 0x4A080010, 0x480AC800);
gte_probe!(lat_dpcs_b2b, 141, 16, 0x4A080010, 0);
gte_read_probe!(lat_intpl_mac0, 142, 16, 0x4A080011, 0x480AC000);
gte_read_probe!(lat_intpl_mac1, 143, 16, 0x4A080011, 0x480AC800);
gte_probe!(lat_intpl_b2b, 144, 16, 0x4A080011, 0);
gte_read_probe!(lat_mvmva_mac0, 145, 16, 0x4A080012, 0x480AC000);
gte_read_probe!(lat_mvmva_mac1, 146, 16, 0x4A080012, 0x480AC800);
gte_probe!(lat_mvmva_b2b, 147, 16, 0x4A080012, 0);
gte_read_probe!(lat_ncds_mac0, 148, 8, 0x4A080013, 0x480AC000);
gte_read_probe!(lat_ncds_mac1, 149, 8, 0x4A080013, 0x480AC800);
gte_probe!(lat_ncds_b2b, 150, 8, 0x4A080013, 0);
gte_read_probe!(lat_cdp_mac0, 151, 8, 0x4A080014, 0x480AC000);
gte_read_probe!(lat_cdp_mac1, 152, 8, 0x4A080014, 0x480AC800);
gte_probe!(lat_cdp_b2b, 153, 8, 0x4A080014, 0);
gte_read_probe!(lat_ncdt_mac0, 154, 8, 0x4A080016, 0x480AC000);
gte_read_probe!(lat_ncdt_mac1, 155, 8, 0x4A080016, 0x480AC800);
gte_probe!(lat_ncdt_b2b, 156, 8, 0x4A080016, 0);
gte_read_probe!(lat_nccs_mac0, 157, 8, 0x4A08001B, 0x480AC000);
gte_read_probe!(lat_nccs_mac1, 158, 8, 0x4A08001B, 0x480AC800);
gte_probe!(lat_nccs_b2b, 159, 8, 0x4A08001B, 0);
gte_read_probe!(lat_cc_mac0, 160, 16, 0x4A08001C, 0x480AC000);
gte_read_probe!(lat_cc_mac1, 161, 16, 0x4A08001C, 0x480AC800);
gte_probe!(lat_cc_b2b, 162, 16, 0x4A08001C, 0);
gte_read_probe!(lat_ncs_mac0, 163, 8, 0x4A08001E, 0x480AC000);
gte_read_probe!(lat_ncs_mac1, 164, 8, 0x4A08001E, 0x480AC800);
gte_probe!(lat_ncs_b2b, 165, 8, 0x4A08001E, 0);
gte_read_probe!(lat_nct_mac0, 166, 8, 0x4A080020, 0x480AC000);
gte_read_probe!(lat_nct_mac1, 167, 8, 0x4A080020, 0x480AC800);
gte_probe!(lat_nct_b2b, 168, 8, 0x4A080020, 0);
gte_read_probe!(lat_sqr_mac0, 169, 16, 0x4A080028, 0x480AC000);
gte_read_probe!(lat_sqr_mac1, 170, 16, 0x4A080028, 0x480AC800);
gte_probe!(lat_sqr_b2b, 171, 16, 0x4A080028, 0);
gte_read_probe!(lat_dcpl_mac0, 172, 16, 0x4A080029, 0x480AC000);
gte_read_probe!(lat_dcpl_mac1, 173, 16, 0x4A080029, 0x480AC800);
gte_probe!(lat_dcpl_b2b, 174, 16, 0x4A080029, 0);
gte_read_probe!(lat_dpct_mac0, 175, 8, 0x4A08002A, 0x480AC000);
gte_read_probe!(lat_dpct_mac1, 176, 8, 0x4A08002A, 0x480AC800);
gte_probe!(lat_dpct_b2b, 177, 8, 0x4A08002A, 0);
gte_read_probe!(lat_avsz3_mac0, 178, 16, 0x4A08002D, 0x480AC000);
gte_read_probe!(lat_avsz3_mac1, 179, 16, 0x4A08002D, 0x480AC800);
gte_probe!(lat_avsz3_b2b, 180, 16, 0x4A08002D, 0);
gte_read_probe!(lat_avsz4_mac0, 181, 16, 0x4A08002E, 0x480AC000);
gte_read_probe!(lat_avsz4_mac1, 182, 16, 0x4A08002E, 0x480AC800);
gte_probe!(lat_avsz4_b2b, 183, 16, 0x4A08002E, 0);
gte_read_probe!(lat_rtpt_mac0, 184, 8, 0x4A080030, 0x480AC000);
gte_read_probe!(lat_rtpt_mac1, 185, 8, 0x4A080030, 0x480AC800);
gte_probe!(lat_rtpt_b2b, 186, 8, 0x4A080030, 0);
gte_read_probe!(lat_gpf_mac0, 187, 16, 0x4A08003D, 0x480AC000);
gte_read_probe!(lat_gpf_mac1, 188, 16, 0x4A08003D, 0x480AC800);
gte_probe!(lat_gpf_b2b, 189, 16, 0x4A08003D, 0);
gte_read_probe!(lat_gpl_mac0, 190, 16, 0x4A08003E, 0x480AC000);
gte_read_probe!(lat_gpl_mac1, 191, 16, 0x4A08003E, 0x480AC800);
gte_probe!(lat_gpl_b2b, 192, 16, 0x4A08003E, 0);
gte_read_probe!(lat_ncct_mac0, 193, 8, 0x4A08003F, 0x480AC000);
gte_read_probe!(lat_ncct_mac1, 194, 8, 0x4A08003F, 0x480AC800);
gte_probe!(lat_ncct_b2b, 195, 8, 0x4A08003F, 0);

// M1 and M2 of the timing audit: copies of the 0x130 shape (RTPS, the read,
// then 20 nops, sixteen turns). An interlocked read costs 638 clocks a block as
// 0x130 does, a free one about 400. swc2 $14,0($8): 0xE90E0000; cfc2 $10,$31:
// 0x484AF800; lwc2 $0,0($8): 0xC9000000.
rtps_read_probe!(m1_swc2_sxy2_scratchpad, 196, 0, 0xE90E0000);
rtps_read_probe!(m1_cfc2_flag, 197, 0, 0x484AF800);
rtps_read_probe!(m1_lwc2_zero_scratchpad, 198, 0, 0xC9000000);
rtps_read_probe!(m2_swc2_sxy2_ram, 199, 0, 0xE90E0000);

/// `warm_probe!` with the timer chosen by the caller (`$timer` is the low half
/// of the counter's address: 0x1120 Timer 2, 0x1100 Timer 0, both on the
/// system clock in mode 0), `$14` and `$15` free to the payload, and an
/// `$epilogue` after the second pass, outside the timed window.
macro_rules! warm_probe_ext {
    ($name:ident, $id:literal, $timer:literal, $payload:expr, $epilogue:expr) => {
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
                        "ori $11, $11, ", stringify!($timer), "\n",
                        "addiu $13, $zero, 2\n",
                        "2:\n",
                        "sw $zero, 4($11)\n",
                        "sw $zero, 0($11)\n",
                        $payload,
                        "lw $12, 0($11)\n",
                        "addiu $13, $13, -1\n",
                        "bnez $13, 2b\n",
                        "nop\n",
                        $epilogue,
                        ".word 0x34000001 | (", stringify!($id), " << 1)\n",
                        ".set reorder"
                    ),
                    inout("$8") a => _,
                    inout("$9") b => _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") elapsed,
                    lateout("$13") _,
                    lateout("$14") _,
                    lateout("$15") _,
                    options(nostack)
                );
            }
            elapsed as u16
        }
    };
}

// M3: Timer 2's own counter read, timed by Timer 0 on the system clock (the
// other MMIO reads are `warm_loads` on Timer 2).
warm_probe_ext!(
    m3_timer2_read_timed_by_timer0,
    200,
    0x1100,
    ".rept 64\nlw $9, 0($8)\nnop\n.endr\n",
    ""
);

// M4: scratchpad sub-word accesses, 64 each with a nop after.
warm_probe!(
    warm_signed_byte_loads,
    201,
    ".rept 64\nlb $9, 0($8)\nnop\n.endr\n"
);
warm_probe!(
    warm_signed_half_loads,
    202,
    ".rept 64\nlh $9, 0($8)\nnop\n.endr\n"
);
warm_probe!(
    warm_byte_stores_nop,
    203,
    ".rept 64\nsb $zero, 0($8)\nnop\n.endr\n"
);
warm_probe!(
    warm_half_stores_nop,
    204,
    ".rept 64\nsh $zero, 0($8)\nnop\n.endr\n"
);

// M5: 64 sw and 64 lw run through KSEG1 with COP0 Status bit 16 (IsC) clear
// against the scratchpad (the control), set, and set with the cache-control
// TAG bit (bit 2) as well. The Status read-modify-write is in all of them, so
// only the bit differs. The epilogue is the BIOS's own flush (IsC, TAG, a zero
// word to all 256 lines) so the I-cache comes back with no valid line.
macro_rules! isolate_probe {
    ($name:ident, $id:literal, $enter:literal, $access:literal, $leave:literal) => {
        warm_probe_ext!(
            $name,
            $id,
            0x1120,
            concat!($enter, ".rept 64\n", $access, "\n.endr\n", $leave),
            "lui $15, 0xFFFE\nori $15, $15, 0x0130\nlw $14, 0($15)\nnop\nori $9, $14, 4\nsw $9, 0($15)\nmfc0 $10, $12\nnop\nlui $9, 1\nor $9, $9, $10\nmtc0 $9, $12\nnop\nnop\nmove $9, $zero\n.rept 256\nsw $zero, 0($9)\naddiu $9, $9, 16\n.endr\nmtc0 $10, $12\nnop\nnop\nsw $14, 0($15)\n"
        );
    };
}
isolate_probe!(
    isolate_control_sw,
    205,
    "mfc0 $10, $12\nnop\nmove $9, $10\nmtc0 $9, $12\nnop\nnop\n",
    "sw $zero, 0($8)",
    "mtc0 $10, $12\nnop\nnop\n"
);
isolate_probe!(
    isolate_control_lw,
    206,
    "mfc0 $10, $12\nnop\nmove $9, $10\nmtc0 $9, $12\nnop\nnop\n",
    "lw $9, 0($8)\nnop",
    "mtc0 $10, $12\nnop\nnop\n"
);
isolate_probe!(
    isolate_data_sw,
    207,
    "mfc0 $10, $12\nnop\nlui $9, 1\nor $9, $9, $10\nmtc0 $9, $12\nnop\nnop\n",
    "sw $zero, 0($8)",
    "mtc0 $10, $12\nnop\nnop\n"
);
isolate_probe!(
    isolate_data_lw,
    208,
    "mfc0 $10, $12\nnop\nlui $9, 1\nor $9, $9, $10\nmtc0 $9, $12\nnop\nnop\n",
    "lw $9, 0($8)\nnop",
    "mtc0 $10, $12\nnop\nnop\n"
);
isolate_probe!(isolate_tag_sw, 209, "lui $15, 0xFFFE\nori $15, $15, 0x0130\nlw $14, 0($15)\nnop\nori $9, $14, 4\nsw $9, 0($15)\nmfc0 $10, $12\nnop\nlui $9, 1\nor $9, $9, $10\nmtc0 $9, $12\nnop\nnop\n", "sw $zero, 0($8)", "mtc0 $10, $12\nnop\nnop\nsw $14, 0($15)\n");
isolate_probe!(isolate_tag_lw, 210, "lui $15, 0xFFFE\nori $15, $15, 0x0130\nlw $14, 0($15)\nnop\nori $9, $14, 4\nsw $9, 0($15)\nmfc0 $10, $12\nnop\nlui $9, 1\nor $9, $9, $10\nmtc0 $9, $12\nnop\nnop\n", "lw $9, 0($8)\nnop", "mtc0 $10, $12\nnop\nnop\nsw $14, 0($15)\n");

/// One pass, the I-cache evicted first by the 4 KiB block `0x1C` uses (`b` is
/// its address; it returns through `$10`), so the lines of the code that
/// follows are refills. M7.
macro_rules! cold_probe {
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
                        "jalr $10, $9\n",
                        "nop\n",
                        "lui $11, 0x1F80\n",
                        "ori $11, $11, 0x1120\n",
                        "sw $zero, 4($11)\n",
                        "sw $zero, 0($11)\n",
                        $payload,
                        "lw $12, 0($11)\n",
                        ".word 0x34000001 | (", stringify!($id), " << 1)\n",
                        ".set reorder"
                    ),
                    inout("$8") a => _,
                    inout("$9") b => _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") elapsed,
                    options(nostack)
                );
            }
            elapsed as u16
        }
    };
}
cold_probe!(m7_sw_ram, 211, ".rept 64\nsw $zero, 0($8)\n.endr\n");
cold_probe!(m7_sw_scratchpad, 212, ".rept 64\nsw $zero, 0($8)\n.endr\n");
cold_probe!(
    m7_gpustat_pairs,
    213,
    ".rept 32\nlw $9, 0($8)\nnop\n.endr\n"
);
cold_probe!(m7_ram_pairs, 214, ".rept 32\nlw $9, 0($8)\nnop\n.endr\n");
cold_probe!(m7_nops, 215, ".rept 64\nnop\n.endr\n");

// M10: the multiply/divide unit. (a) divu, multu small, a gap, mflo, against
// divu and 36 nops; (b) multu large, mtlo, mflo; (c) multu, mthi, mfhi.
// 0x0109001B divu $8,$9; 0x01090019 multu $8,$9; 0x00005012 mflo $10;
// 0x00005010 mfhi $10; 0x01000013 mtlo $8; 0x01000011 mthi $8.
macro_rules! divmul_gap_probe {
    ($name:ident, $id:literal, $gap:literal) => {
        warm_probe!(
            $name,
            $id,
            concat!(
                ".rept 8\n.word 0x0109001B\n.word 0x01090019\n.rept ",
                stringify!($gap),
                "\nnop\n.endr\n.word 0x00005012\n.endr\n.rept 48\nnop\n.endr\n"
            )
        );
    };
}
divmul_gap_probe!(m10_divu_multu_gap0, 216, 0);
divmul_gap_probe!(m10_divu_multu_gap10, 217, 10);
divmul_gap_probe!(m10_divu_multu_gap30, 218, 30);
divmul_gap_probe!(m10_divu_multu_gap36, 219, 36);
warm_probe!(
    m10_divu_36_nops,
    220,
    ".rept 8\n.word 0x0109001B\n.rept 36\nnop\n.endr\n.endr\n.rept 48\nnop\n.endr\n"
);
warm_probe!(
    m10_multu_mtlo_mflo,
    221,
    ".rept 16\n.word 0x01090019\n.word 0x01000013\n.word 0x00005012\n.endr\n.rept 48\nnop\n.endr\n"
);
warm_probe!(
    m10_multu_mthi_mfhi,
    222,
    ".rept 16\n.word 0x01090019\n.word 0x01000011\n.word 0x00005010\n.endr\n.rept 48\nnop\n.endr\n"
);

/// The cached twin of `warm_call_pairs`. It lives in the I-cache entry section
/// at page offset 0x100, so its lines (16 to 51) can never share a cache index
/// with the leaves it calls (lines 0, 1 and 2).
fn cached_call_pairs(a: u32, b: u32) -> u16 {
    unsafe { __hwtest_perf_cached_pairs(a, b) as u16 }
}

// ---------------------------------------------------------------------------
// DMA: what an ordering table costs to walk, and whether the CPU runs meanwhile
// ---------------------------------------------------------------------------

const EMPTY_LIST_NODES: usize = 1024;
static mut EMPTY_LIST: [u32; EMPTY_LIST_NODES] = [0; EMPTY_LIST_NODES];

/// Link the first `nodes` entries into a list of empty packets and return its
/// head. An empty packet is what every unused ordering-table slot is.
fn build_empty_list(nodes: usize) -> u32 {
    let list = (&raw mut EMPTY_LIST) as *mut u32;
    for index in 0..nodes {
        let next = if index + 1 == nodes {
            0x00FF_FFFF
        } else {
            unsafe { list.add(index + 1) as u32 & 0x00FF_FFFF }
        };
        unsafe { core::ptr::write_volatile(list.add(index), next) };
    }
    list as u32
}

/// Run `body` with the GPU DMA channel set up for a linked list, then put the
/// GPU's DMA direction back.
fn with_gpu_list_dma(body: impl FnOnce() -> u16) -> u16 {
    let old_direction = (gpu_io::status().bits() >> 29) & 3;
    gpu_io::write_display_control(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));
    }
    let elapsed = body();
    // A list that never ended leaves the direction off, not as it was.
    // SAFETY: single thread; plain static.
    let timed_out = unsafe { DMA_END[DmaGroup::GpuList as usize].flags & 8 != 0 };
    gpu_io::write_display_control(0x0400_0000 | if timed_out { 0 } else { old_direction });
    elapsed
}

const LIST_KICK: u32 =
    psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START;

/// Cycles for the DMA controller to walk `nodes` empty packets.
fn timed_empty_list(nodes: usize) -> u16 {
    let head = build_empty_list(nodes);
    with_gpu_list_dma(|| {
        // SAFETY: silicon probe: the transfer touches only memory this probe
        // owns, which stays live and untouched until the probe waits the
        // channel idle or aborts it.
        unsafe {
            dma::raw::set_address(dma::Channel::Gpu, head);
        }
        psx_io::timers::set_mode(psx_io::timers::Timer::Timer2, 0);
        psx_io::timers::set_counter(psx_io::timers::Timer::Timer2, 0);
        // SAFETY: silicon probe: the transfer touches only memory this probe
        // owns, which stays live and untouched until the probe waits the
        // channel idle or aborts it.
        unsafe {
            dma::raw::set_control(dma::Channel::Gpu, LIST_KICK);
        }
        let mut polls = 0u32;
        while dma::is_busy(dma::Channel::Gpu) && polls < 1_000_000 {
            polls += 1;
        }
        let elapsed = psx_io::timers::counter(psx_io::timers::Timer::Timer2);
        if polls == 1_000_000 {
            0xFFFF
        } else {
            elapsed
        }
    })
}

fn push_dma(records: &mut Records, next: &mut usize) {
    let base = dma::Channel::Gpu.register_base();
    push_timing_record(
        records,
        next,
        sample_timing(0x33, 256, || timed_empty_list(256)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x34, 1024, || timed_empty_list(1024)),
    );
    // 128 nops with the channel idle, then the same 128 nops started right
    // after kicking a 512-node list. Equal means the CPU runs alongside the
    // list walk; the difference is the time the walk took the bus away.
    // Then the same with 64 RAM loads in place of the nops: psx-spx says the
    // CPU runs during DMA only until it needs the bus.
    type Overlap = fn(u32, u32, u32, u32) -> u16;
    let ram = Arg::RamWord.resolve();
    let scratchpad = SCRATCHPAD;
    // (id, loop length, loop, CHCR, data address). The idle rows have CHCR 0.
    // M6's GPU walk rows: 64 sw back to back, 64 sw + 3 nops, 64 lw + 3 nops,
    // 64 scratchpad lw + nop, against the 0xFE control (64 lw + nop).
    let cases: [(u16, u16, Overlap, u32, u32); 12] = [
        (0x35, 128, timed_nops_with_dma, 0, ram),
        (0x36, 128, timed_nops_with_dma, LIST_KICK, ram),
        (0x9F, 64, timed_loads_with_dma, 0, ram),
        (0xFE, 64, timed_loads_with_dma, LIST_KICK, ram),
        (0x1E0, 64, timed_sw_with_dma, 0, ram),
        (0x1E1, 64, timed_sw_with_dma, LIST_KICK, ram),
        (0x1E2, 64, timed_sw_3nops_with_dma, 0, ram),
        (0x1E3, 64, timed_sw_3nops_with_dma, LIST_KICK, ram),
        (0x1E4, 64, timed_lw_3nops_with_dma, 0, ram),
        (0x1E5, 64, timed_lw_3nops_with_dma, LIST_KICK, ram),
        (0x1E6, 64, timed_loads_with_dma, 0, scratchpad),
        (0x1E7, 64, timed_loads_with_dma, LIST_KICK, scratchpad),
    ];
    dma_end_reset(DmaGroup::GpuList);
    for (id, work, run, chcr, data) in cases {
        // 512 rather than 1024: if the CPU does wait for the walk, the whole
        // walk lands in a 16-bit counter.
        let head = build_empty_list(512);
        let record = sample_timing(id, work, || {
            with_gpu_list_dma(|| {
                let elapsed = run(base, head, chcr, data);
                dma_settle(
                    DmaGroup::GpuList,
                    dma::Channel::Gpu,
                    gpu_io::status().bits() >> 16,
                );
                elapsed
            })
        });
        push_timing_record(records, next, record);
    }
    push_dma_end(records, next, DmaGroup::GpuList);
}

/// How the last overlap probe's waits ended: bit 0 the channel was still busy
/// when a pass was about to kick it, bit 1 it was still busy when the block
/// ended. OR-ed over a record's samples; cleared by the caller.
static mut DMA_WAIT_FLAGS: u32 = 0;

/// Loop iterations a wait on a DMA channel's busy bit gets before the block
/// kicks anyway (before a pass: an idle channel reads idle at once) or gives
/// up (after the loop: the longest legitimate transfer here, the SPU's 256
/// words at about 16 cycles a halfword, is under 10,000 cycles; an iteration
/// is about 20 cycles, so 20,000 of them is some 400,000 cycles, over ten
/// milliseconds, forty times longer).
const DMA_PRE_WAIT_ITERATIONS: u32 = 4_000;
const DMA_FINAL_WAIT_ITERATIONS: u32 = 20_000;

/// `$payload`, timed on the second pass, each pass starting by writing `chcr`
/// to the DMA channel at `base` with MADR = `head`. Each pass first waits for
/// the channel to go idle, and the block ends the same way, so the caller can
/// restore the GPU's DMA direction. `$24` holds a RAM address for payloads
/// that load. Both waits are counted loops: a channel that never goes idle
/// costs `DMA_PRE_WAIT_ITERATIONS` or `DMA_FINAL_WAIT_ITERATIONS` passes and
/// sets a bit in `DMA_WAIT_FLAGS`, and the block goes on, so the caller can
/// stop the channel and say what it found.
macro_rules! dma_overlap_probe {
    ($name:ident, $start:literal, $end:literal, $payload:literal) => {
        #[inline(never)]
        fn $name(base: u32, head: u32, chcr: u32, data: u32) -> u16 {
            let elapsed: u32;
            let flags: u32;
            let pre = crate::bounds::scale(DMA_PRE_WAIT_ITERATIONS);
            let post = crate::bounds::scale(DMA_FINAL_WAIT_ITERATIONS);
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    ".balign 16",
                    $start,
                    "move $15, $zero",
                    "lui $11, 0x1F80",
                    "ori $11, $11, 0x1120",
                    "addiu $13, $zero, 2",
                    "2:",
                    "move $3, $4",
                    "3:",
                    "lw $10, 8($8)",
                    "nop",
                    "srl $10, $10, 24",
                    "andi $10, $10, 1",
                    "beqz $10, 5f",
                    "nop",
                    "addiu $3, $3, -1",
                    "bnez $3, 3b",
                    "nop",
                    "ori $15, $15, 1",
                    "5:",
                    "sw $9, 0($8)",
                    "sw $zero, 4($11)",
                    "sw $zero, 0($11)",
                    "sw $14, 8($8)",
                    $payload,
                    "lw $12, 0($11)",
                    "addiu $13, $13, -1",
                    "bnez $13, 2b",
                    "nop",
                    "move $3, $5",
                    "4:",
                    "lw $10, 8($8)",
                    "nop",
                    "srl $10, $10, 24",
                    "andi $10, $10, 1",
                    "beqz $10, 6f",
                    "nop",
                    "addiu $3, $3, -1",
                    "bnez $3, 4b",
                    "nop",
                    "ori $15, $15, 2",
                    "6:",
                    $end,
                    ".set reorder",
                    in("$8") base,
                    in("$9") head,
                    in("$14") chcr,
                    in("$24") data,
                    in("$4") pre,
                    in("$5") post,
                    lateout("$3") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$12") elapsed,
                    lateout("$13") _,
                    lateout("$15") flags,
                    options(nostack)
                );
                DMA_WAIT_FLAGS |= flags;
            }
            elapsed as u16
        }
    };
}

// Registers and I-cache only: psx-spx says the CPU keeps running.
dma_overlap_probe!(
    timed_nops_with_dma,
    ".word 0x340000B6", // probe 91 start marker
    ".word 0x340000B7", // probe 91 end marker
    ".rept 128\nnop\n.endr"
);
// RAM loads: psx-spx says the first one stalls until the DMA lets go.
dma_overlap_probe!(
    timed_loads_with_dma,
    ".word 0x340000B8", // probe 92 start marker
    ".word 0x340000B9", // probe 92 end marker
    ".rept 64\nlw $10, 0($24)\nnop\n.endr"
);

// M6's other loop shapes.
dma_overlap_probe!(
    timed_sw_with_dma,
    ".word 0x340001BE", // probe 223 start marker
    ".word 0x340001BF", // probe 223 end marker
    ".rept 64\nsw $zero, 0($24)\n.endr"
);
dma_overlap_probe!(
    timed_sw_3nops_with_dma,
    ".word 0x340001C0", // probe 224 start marker
    ".word 0x340001C1", // probe 224 end marker
    ".rept 64\nsw $zero, 0($24)\nnop\nnop\nnop\n.endr"
);
dma_overlap_probe!(
    timed_lw_3nops_with_dma,
    ".word 0x340001C2", // probe 225 start marker
    ".word 0x340001C3", // probe 225 end marker
    ".rept 64\nlw $10, 0($24)\nnop\nnop\nnop\n.endr"
);

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
