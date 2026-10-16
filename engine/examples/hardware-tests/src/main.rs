//! `hardware-tests` -- the PS1 hardware test suite.
//!
//! A real PS1 application, not a host-side unit test. From v2.0 it is ONE
//! linear run (`run.rs`) that goes through every chip in a fixed order, resets
//! the machine to a defined state at the start of each area, checks at the end
//! of each area that it left clean state behind, and ends in a single capture
//! (`photo.rs`) shown as QR pages. The only separate screens are the controller
//! test and the memory-card diagnostic, which need a person.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#![cfg_attr(feature = "clut-preview", allow(dead_code, unused_imports))]
// The probes drive the pad, SPU and CD-ROM registers directly and on purpose,
// the way the BIOS and the first-party games of the era did, so the free
// functions that predate the SDK's ownership tokens stay in use here. The
// engine and the game crates do not get this allowance.
#![allow(deprecated)]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

extern crate psx_rt;

use core::ptr;

use psx_engine::{button, App, Config};
use psx_font::{fonts::SPLEEN_5X8, FontAtlas};
use psx_gpu::display::{Resolution, VideoMode};
use psx_gpu::prim::{self, FillRect, LineMono, QuadFlat, Sprite};
use psx_gpu::{self as gpu, Gpu};
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::ops as gte_ops;
use psx_gte::regs::pack_xy as pack_gte_xy;
use psx_gte::{read_control, read_data, scene as gte_scene, write_control, write_data};
use psx_io::{dma, gpu as gpu_io, irq, timers};
use psx_rt::tty;
use psx_spu::SpuAddr;
use psx_vram::{Clut, TextureDepth, TexturePage};

/// Binds `$gpu` to the GPU driver for a probe that draws or programs the
/// display from inside a test, over a token taken the way [`probe_gpu_dma`]
/// takes it. Declared before the modules so each of them can use it.
macro_rules! probe_gpu {
    ($gpu:ident) => {
        let mut probe_dma = $crate::probe_gpu_dma();
        let $gpu = psx_gpu::Gpu::from_dma_mut(&mut probe_dma);
    };
}

mod boot;
mod bounds;
mod card_proto;
mod cd_chain_probe;
mod cd_pause;
mod cd_route;
mod cdstream_cases;
mod clut_rewrite;
mod console_tests;
mod controller_test;
mod cpu_tests;
mod display_widths;
mod dma_edge;
mod dma_matrix;
mod field_rule;
mod gpu_probes;
mod handoff_probe;
mod kernel_timing;
mod lever_probes;
mod list_busy_probes;
mod mdec_check;
mod pad_engine;
mod payload;
mod perf_probes;
mod photo;
mod records;
mod regs;
mod report;
mod ring_probe;
mod rumble;
mod run;
mod sample_probe;
mod scene;
mod sio_timing;
mod spu_probe;
mod tick_loss;
mod timer1_rate;
mod ui;
mod v24;
mod workload;
mod xa_loop;
mod xa_measure;
use cpu_tests::*;
use payload::fnv32_words;
use regs::SPU_DELAY;
use report::{hex2, probe_column_ok, tty_print_dec_u16, tty_print_dec_u8};

// A complete 4 KiB direct-mapped PS1 I-cache footprint. The custom return
// register lets inline timing assembly call it without clobbering Rust's $ra.
core::arch::global_asm!(
    ".set noreorder",
    ".section .text.hwtest_icache",
    ".balign 4096",
    ".globl __hwtest_icache_block",
    "__hwtest_icache_block:",
    ".rept 1022",
    "nop",
    ".endr",
    "jr $10",
    "nop",
    // Three one-line return targets whose entry points occupy words 0, 1,
    // and 2 of separate 16-byte cache lines. The non-executed layout tags
    // (0x34008000 | n) pad those lines, and hwtest-verify-code pins each
    // tag's word position so the final linked layout is machine-verified.
    ".section .text.hwtest_icache_entries",
    ".balign 4096",
    ".globl __hwtest_icache_entry_w0",
    "__hwtest_icache_entry_w0:",
    "jr $10",
    "nop",
    ".word 0x34008001",
    ".word 0x34008002",
    ".balign 16",
    ".word 0x34008003",
    ".globl __hwtest_icache_entry_w1",
    "__hwtest_icache_entry_w1:",
    "jr $10",
    "nop",
    ".word 0x34008004",
    ".balign 16",
    ".word 0x34008005",
    ".word 0x34008006",
    ".globl __hwtest_icache_entry_w2",
    "__hwtest_icache_entry_w2:",
    "jr $10",
    "nop",
    // Exactly 4 KiB after entry_w0: same cache index, different tag, so the
    // two evict each other on every alternating call (perf_probes.rs).
    ".balign 4096",
    ".globl __hwtest_icache_alias_b",
    "__hwtest_icache_alias_b:",
    "jr $10",
    "nop",
    // Workload for the register A/B probes: 64 loads from the word in $25.
    // Position independent, so its KSEG1 alias runs the same instructions
    // with every fetch going to RAM.
    ".balign 16",
    ".globl __hwtest_perf_loads",
    "__hwtest_perf_loads:",
    ".rept 64",
    "lw $3, 0($25)",
    "nop",
    ".endr",
    "jr $10",
    "nop",
    // A cached caller for the alias pair, at page offset 0x100 so that its
    // lines (16 to 51) never share a cache index with the leaves (0 to 2).
    // Same shape as perf_probes.rs's warm harness: $a0/$a1 are the two leaves,
    // the second pass is the one timed, the count comes back in $v0.
    ".balign 4096",
    ".space 0x100",
    ".globl __hwtest_perf_cached_pairs",
    "__hwtest_perf_cached_pairs:",
    ".word 0x340000E4", // probe 114 start marker
    "lui $11, 0x1F80",
    "ori $11, $11, 0x1120",
    "addiu $13, $zero, 2",
    "2:",
    "sw $zero, 4($11)",
    "sw $zero, 0($11)",
    ".rept 32",
    "jalr $10, $4",
    "nop",
    "jalr $10, $5",
    "nop",
    ".endr",
    "lw $2, 0($11)",
    "addiu $13, $13, -1",
    "bnez $13, 2b",
    "nop",
    ".word 0x340000E5", // probe 114 end marker
    "jr $ra",
    "nop",
    // A full I-cache footprint of code that also reads data: the realistic
    // case for a refill and a RAM data access wanting the bus together.
    ".balign 4096",
    ".globl __hwtest_icache_load_block",
    "__hwtest_icache_load_block:",
    ".rept 511",
    "lw $3, 0($25)",
    "nop",
    ".endr",
    "jr $10",
    "nop",
    ".set reorder",
);

unsafe extern "C" {
    fn __hwtest_icache_block();
    fn __hwtest_icache_entry_w0();
    fn __hwtest_icache_entry_w1();
    fn __hwtest_icache_entry_w2();
    fn __hwtest_icache_alias_b();
    fn __hwtest_perf_loads();
    fn __hwtest_icache_load_block();
    fn __hwtest_perf_cached_pairs(a: u32, b: u32) -> u32;
}

// Suite version, written into every payload so a capture is self-identifying.
//
// The transport schema (PX8) and the suite version are different things: the
// schema says how bytes are laid out, the suite version says what a record id
// MEANS. Comparing a capture from one suite version against a baseline from
// another is the trap this exists to prevent, because record 0xA0 can be
// redefined while the byte layout stays identical.
//
// MAJOR: bump when an existing record's meaning changes -- a probe redefined,
//        a clock swapped, sample semantics altered, records removed or
//        regrouped. Captures across a MAJOR boundary are NOT comparable.
// MINOR: bump when records are only added, or a bug is fixed that leaves every
//        existing record measuring the same thing. Captures remain comparable
//        for the records they share.
//
// History, one entry per version: docs/hardware-test-versions.md.
const SUITE_VERSION_MAJOR: u8 = 2;
const SUITE_VERSION_MINOR: u8 = 7;
/// Display form. Keep in step with the two constants above.
const SUITE_VERSION: &str = "HWTEST v2.7";
const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const TEST_COUNT: usize = 234;
const PAD_POLL_TEST_INDEX: usize = 26;

/// Number of timing variants the SIO records sweep.
const PROBE_VARIANT_COUNT: usize = 5;
/// `(label, setup_spins, interbyte_spins)` the SIO records poll the pad with
/// (0xB6-0xB9): `setup_spins` is a delay after asserting the select line,
/// `interbyte_spins` a fixed gap after each byte. Both are bounded STAT reads.
const PROBE_VARIANTS: [(&str, u32, u32); PROBE_VARIANT_COUNT] = [
    ("SETUP 0", 0, 0),
    ("SETUP 128", 128, 0),
    ("SETUP 384", 384, 0),
    ("SETUP 768", 768, 0),
    ("SETUP 2K", 2048, 0),
];
/// Setup-delay values swept by records 0xD0.., in SIO spin units.
const SIO_SETUP_SWEEP: [u32; 12] = [0, 64, 128, 192, 256, 320, 448, 512, 640, 896, 1024, 1536];

const TIMER_MODE_SYNC_ENABLE: u16 = 1 << 0;
const TIMER_MODE_SYNC_MODE_1: u16 = 1 << 1;
const TIMER_MODE_RESET_AT_TARGET: u16 = 1 << 3;
const TIMER_MODE_IRQ_ON_TARGET: u16 = 1 << 4;
const TIMER_MODE_IRQ_ON_WRAP: u16 = 1 << 5;
const TIMER_MODE_CLOCK_SOURCE_1: u16 = 1 << 8;
const TIMER_MODE_CLOCK_SOURCE_2: u16 = 2 << 8;
const TIMER_MODE_IRQ_INACTIVE: u16 = 1 << 10;
const TIMER_MODE_REACHED_TARGET: u16 = 1 << 11;
const TIMER_MODE_REACHED_WRAP: u16 = 1 << 12;

static SPIN_SINK: u32 = 0;
static mut TIMING_WORD: u32 = 0;

const fn mips_r(rs: u32, rt: u32, rd: u32, shamt: u32, funct: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | (shamt << 6) | funct
}

const fn mips_i(op: u32, rs: u32, rt: u32, imm: u16) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | (imm as u32)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Warn,
    Info,
    Pending,
}

impl Status {
    const fn code(self) -> u32 {
        match self {
            Self::Pass => 1,
            Self::Fail => 2,
            Self::Warn => 3,
            Self::Info => 4,
            Self::Pending => 0,
        }
    }

    /// Whether a capture spends bytes describing this case as a failure.
    ///
    /// `Warn` counts: a warning is a case whose observed value the suite could
    /// not confidently judge, which is exactly the kind of thing worth carrying
    /// off the console. `Pending` does not -- an unrun case has nothing to say,
    /// and the status bitmap already records that it did not run.
    const fn is_failure(self) -> bool {
        matches!(self, Self::Fail | Self::Warn)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Pending => "....",
        }
    }

    const fn color(self) -> (u8, u8, u8) {
        match self {
            Self::Pass => (96, 240, 128),
            Self::Fail => (255, 88, 88),
            Self::Warn => (255, 216, 96),
            Self::Info => (120, 176, 255),
            Self::Pending => (128, 128, 128),
        }
    }
}

#[derive(Copy, Clone)]
struct TestResult {
    status: Status,
    expected: u32,
    observed: u32,
    note: &'static str,
}

impl TestResult {
    const fn pending() -> Self {
        Self {
            status: Status::Pending,
            expected: 0,
            observed: 0,
            note: "",
        }
    }

    const fn pass(expected: u32, observed: u32, note: &'static str) -> Self {
        Self {
            status: Status::Pass,
            expected,
            observed,
            note,
        }
    }

    const fn fail(expected: u32, observed: u32, note: &'static str) -> Self {
        Self {
            status: Status::Fail,
            expected,
            observed,
            note,
        }
    }

    const fn warn(expected: u32, observed: u32, note: &'static str) -> Self {
        Self {
            status: Status::Warn,
            expected,
            observed,
            note,
        }
    }

    const fn info(expected: u32, observed: u32, note: &'static str) -> Self {
        Self {
            status: Status::Info,
            expected,
            observed,
            note,
        }
    }
}

#[derive(Copy, Clone)]
struct TestSpec {
    /// Identity that outlives this array's ordering.
    ///
    /// A failure record names the case by this number, so a capture taken on
    /// one suite version stays readable against a later one. Assign the next
    /// free id and never reuse a retired one; `TEST_IDS_ARE_UNIQUE` refuses to
    /// compile a duplicate.
    id: u16,
    group: &'static str,
    name: &'static str,
    run: fn() -> TestResult,
}

// Compile-time uniqueness check for `TestSpec::id`.
const _: () = {
    let mut i = 0;
    while i < TEST_COUNT {
        let mut j = i + 1;
        while j < TEST_COUNT {
            assert!(TESTS[i].id != TESTS[j].id, "duplicate TestSpec id");
            j += 1;
        }
        i += 1;
    }
};

#[derive(Copy, Clone)]
struct ScanReport {
    status: Status,
    items: u16,
    hash: u32,
    aux: u32,
    runs: u8,
}

/// Slots in the timing report. `run::tests` asserts the largest run fits.
const TIMING_RECORD_COUNT: usize = 1300;
const MEMORY_CONTROL_REGISTER_COUNT: usize = MEMORY_CONTROL_REGISTERS.len();
/// Captured with the timing block, in this order (the host names them by
/// position). The first nine are the bus configuration the BIOS left.
const MEMORY_CONTROL_REGISTERS: [u32; 11] = [
    0x1F80_1000,
    0x1F80_1004,
    0x1F80_1008,
    0x1F80_100C,
    0x1F80_1010,
    0x1F80_1014,
    0x1F80_1018,
    0x1F80_101C,
    0x1F80_1020,
    regs::RAM_SIZE,
    regs::CACHE_CONTROL,
];
const PRECISION_VALUE_COUNT: usize = 192;

/// Samples per timing record. The minimum rejects interrupt interference, the
/// maximum exposes it, and the median says whether the spread is one stray
/// event or a genuinely bimodal distribution (cache or DRAM-refresh
/// interaction) that a min/max pair cannot distinguish.
const TIMING_SAMPLES: usize = 5;

#[derive(Copy, Clone)]
struct TimingRecord {
    /// `0x00`-`0xFE` travel in the PX8 timing block with a one-byte id, as
    /// they always have. Ids from `0x100` up go in the extended block.
    id: u16,
    work: u16,
    min: u16,
    med: u16,
    max: u16,
}

/// Marks a timing slot that was never filled. Must not be a real record id:
/// `0x00` is the empty-harness measurement, so zero cannot mean "unused".
const TIMING_RECORD_UNUSED: u16 = 0xFFFF;

impl TimingRecord {
    const fn pending() -> Self {
        Self {
            id: TIMING_RECORD_UNUSED,
            work: 0,
            min: 0,
            med: 0,
            max: 0,
        }
    }
}

#[derive(Copy, Clone)]
struct TimingReport {
    summary: ScanReport,
    records: [TimingRecord; TIMING_RECORD_COUNT],
    memory_control: [u32; MEMORY_CONTROL_REGISTER_COUNT],
    precision: [u32; PRECISION_VALUE_COUNT],
}

impl ScanReport {
    const fn pending() -> Self {
        Self {
            status: Status::Pending,
            items: 0,
            hash: 0,
            aux: 0,
            runs: 0,
        }
    }

    const fn info(items: u16, hash: u32, aux: u32) -> Self {
        Self {
            status: Status::Info,
            items,
            hash,
            aux,
            runs: 1,
        }
    }
}

const TESTS: [TestSpec; TEST_COUNT] = [
    TestSpec {
        id: 0x0000,
        group: "CPU",
        name: "little-endian word layout",
        run: test_cpu_endian,
    },
    TestSpec {
        id: 0x0001,
        group: "CPU",
        name: "wrapping add/shift/multiply",
        run: test_cpu_arithmetic,
    },
    TestSpec {
        id: 0x0002,
        group: "CPU",
        name: "MIPS-I R-type opcode battery",
        run: test_cpu_rtype_opcodes,
    },
    TestSpec {
        id: 0x0003,
        group: "CPU",
        name: "MIPS-I immediate opcode battery",
        run: test_cpu_immediate_opcodes,
    },
    TestSpec {
        id: 0x0004,
        group: "CPU",
        name: "MIPS-I HI/LO multiply divide",
        run: test_cpu_hilo_opcodes,
    },
    TestSpec {
        id: 0x0005,
        group: "CPU",
        name: "MIPS-I branch delay battery",
        run: test_cpu_branch_delay_opcodes,
    },
    TestSpec {
        id: 0x0006,
        group: "CPU",
        name: "MIPS-I load/store battery",
        run: test_cpu_load_store_opcodes,
    },
    TestSpec {
        id: 0x0007,
        group: "RAM",
        name: "volatile byte/half/word stores",
        run: test_volatile_memory,
    },
    TestSpec {
        id: 0x0008,
        group: "RAM",
        name: "KSEG1 uncached RAM alias",
        run: test_kseg1_alias,
    },
    TestSpec {
        id: 0x0009,
        group: "IRQ",
        name: "I_MASK register roundtrip",
        run: test_irq_mask_roundtrip,
    },
    TestSpec {
        id: 0x000a,
        group: "IRQ",
        name: "GPU IRQ visible through I_STAT",
        run: test_irq_gpu_ack_path,
    },
    TestSpec {
        id: 0x000b,
        group: "DMA",
        name: "OTC reverse linked-list clear",
        run: test_dma_otc_clear,
    },
    TestSpec {
        id: 0x000c,
        group: "DMA",
        name: "channel register roundtrip",
        run: test_dma_channel_register_roundtrip,
    },
    TestSpec {
        id: 0x000d,
        group: "DMA",
        name: "DPCR priority enable latch",
        run: test_dma_dpcr_roundtrip,
    },
    TestSpec {
        id: 0x000e,
        group: "TMR",
        name: "timer2 free-run increments",
        run: test_timer2_increments,
    },
    TestSpec {
        id: 0x000f,
        group: "TMR",
        name: "timer1 scanline range",
        run: test_timer1_scanline,
    },
    TestSpec {
        id: 0x0010,
        group: "GPU",
        name: "GPUSTAT mode/readiness",
        run: test_gpu_status,
    },
    TestSpec {
        id: 0x0011,
        group: "GPU",
        name: "GP0 IRQ set + GP1 ack",
        run: test_gpu_irq_ack,
    },
    TestSpec {
        id: 0x0012,
        group: "GPU",
        name: "primitive packet encoding",
        run: test_gpu_primitive_packet_encoding,
    },
    TestSpec {
        id: 0x0013,
        group: "GTE",
        name: "data/control register roundtrip",
        run: test_gte_register_roundtrip,
    },
    TestSpec {
        id: 0x0014,
        group: "GTE",
        name: "RTPS projects centre vertex",
        run: test_gte_projection_center,
    },
    TestSpec {
        id: 0x0015,
        group: "GTE",
        name: "all exposed GTE opcode battery",
        run: test_gte_all_ops_digest,
    },
    TestSpec {
        id: 0x0016,
        group: "GTE",
        name: "NCLIP MAC0 winding sign",
        run: test_gte_nclip_mac0,
    },
    TestSpec {
        id: 0x0017,
        group: "SPU",
        name: "SPUSTAT readable",
        run: test_spu_status_readable,
    },
    TestSpec {
        id: 0x0018,
        group: "SPU",
        name: "voice register matrix",
        run: test_spu_voice_registers,
    },
    TestSpec {
        id: 0x0019,
        group: "SPU",
        name: "main volume register roundtrip",
        run: test_spu_main_volume_roundtrip,
    },
    TestSpec {
        id: 0x001a,
        group: "SIO",
        name: "port 1 pad poll",
        run: test_pad_poll,
    },
    TestSpec {
        id: 0x001b,
        group: "SIO",
        name: "mode control baud latches",
        run: test_sio_register_latches,
    },
    TestSpec {
        id: 0x001c,
        group: "GPU",
        name: "draw area command latch",
        run: test_gpu_draw_area_command,
    },
    TestSpec {
        id: 0x001d,
        group: "DMA",
        name: "GPU DMA direction survives OTC",
        run: test_gpu_dma_direction_after_otc,
    },
    TestSpec {
        id: 0x001e,
        group: "TMR",
        name: "timer2 target sticky bit",
        run: test_timer2_target_sticky,
    },
    TestSpec {
        id: 0x001f,
        group: "TMR",
        name: "mode write resets counter",
        run: test_timer_mode_write_resets_counter,
    },
    TestSpec {
        id: 0x0020,
        group: "TMR",
        name: "mode read clears sticky flags",
        run: test_timer_mode_read_clears_sticky,
    },
    TestSpec {
        id: 0x0021,
        group: "TMR",
        name: "timer2 sync stop vs free-run",
        run: test_timer2_sync_stop_vs_free_run,
    },
    TestSpec {
        id: 0x0022,
        group: "TMR",
        name: "timer2 system clock divided by 8",
        run: test_timer2_clock_divider,
    },
    TestSpec {
        id: 0x0023,
        group: "TMR",
        name: "timer2 0xffff wrap sticky bit",
        run: test_timer2_wrap_sticky,
    },
    TestSpec {
        id: 0x0024,
        group: "TMR",
        name: "timer2 target IRQ latch",
        run: test_timer2_target_irq_latch,
    },
    TestSpec {
        id: 0x0025,
        group: "TMR",
        name: "timer2 wrap IRQ latch",
        run: test_timer2_wrap_irq_latch,
    },
    TestSpec {
        id: 0x0026,
        group: "TMR",
        name: "timer1 HBlank clock advances",
        run: test_timer1_hblank_clock_advances,
    },
    TestSpec {
        id: 0x0027,
        group: "TMR",
        name: "timer0 dot clock slower than system",
        run: test_timer0_dot_clock_ratio,
    },
    TestSpec {
        id: 0x0028,
        group: "DMA",
        name: "OTC DMA completes within bounded poll",
        run: test_dma_otc_bounded_completion,
    },
    TestSpec {
        id: 0x0029,
        group: "RAM",
        name: "scratchpad byte/half/word roundtrip",
        run: test_scratchpad_roundtrip,
    },
    TestSpec {
        id: 0x002a,
        group: "CD",
        name: "CD-ROM GetStat command response",
        run: test_cdrom_getstat_response,
    },
    TestSpec {
        id: 0x002b,
        group: "CD",
        name: "CD-ROM register index latch",
        run: test_cdrom_index_latch,
    },
    TestSpec {
        id: 0x002c,
        group: "SIO",
        name: "direct port 1 pad poll stability",
        run: test_pad_direct_stability,
    },
    TestSpec {
        id: 0x002d,
        group: "CPU",
        name: "MIPS-I unaligned load/store pairs",
        run: test_cpu_unaligned_load_store_pairs,
    },
    TestSpec {
        id: 0x002e,
        group: "GPU",
        name: "DMA direction mode latch",
        run: test_gpu_dma_direction_mode_latch,
    },
    TestSpec {
        id: 0x002f,
        group: "GPU",
        name: "GP1 info environment readback",
        run: test_gpu_gp1_info_environment_readback,
    },
    TestSpec {
        id: 0x0030,
        group: "TMR",
        name: "target register roundtrip",
        run: test_timer_target_register_roundtrip,
    },
    TestSpec {
        id: 0x0031,
        group: "GPU",
        name: "GPU IRQ1 flag settle latency",
        run: test_gpu_irq_latency_probe,
    },
    TestSpec {
        id: 0x0032,
        group: "GTE",
        name: "NCLIP MAC0 raw value probe",
        run: test_gte_nclip_mac0_value,
    },
    TestSpec {
        id: 0x0033,
        group: "TMR",
        name: "timer0 dot/system tick counts",
        run: test_timer0_dot_clock_counts,
    },
    TestSpec {
        id: 0x0034,
        group: "GPU",
        name: "DMA-direction readback values",
        run: test_gpu_dma_direction_readback,
    },
    TestSpec {
        id: 0x0035,
        group: "GTE",
        name: "RTPS off-centre projection value",
        run: test_gte_rtps_offcenter_value,
    },
    TestSpec {
        id: 0x0036,
        group: "GTE",
        name: "scene RTPS A SXY2 (div-ovf+sat)",
        run: test_gte_scene_rtps_a_sxy,
    },
    TestSpec {
        id: 0x0037,
        group: "GTE",
        name: "scene RTPS A FLAG (0x80066000)",
        run: test_gte_scene_rtps_a_flag,
    },
    TestSpec {
        id: 0x0038,
        group: "GTE",
        name: "scene RTPS B SXY2 (clamp hi/lo)",
        run: test_gte_scene_rtps_b_sxy,
    },
    TestSpec {
        id: 0x0039,
        group: "GTE",
        name: "scene RTPS C SXY2 (SZ3 survives)",
        run: test_gte_scene_rtps_c_sxy,
    },
    TestSpec {
        id: 0x003a,
        group: "GTE",
        name: "scene RTPS C FLAG (0x80002000)",
        run: test_gte_scene_rtps_c_flag,
    },
    TestSpec {
        id: 0x003b,
        group: "GTE",
        name: "scene RTPS D SXY2 (neg-X vertex)",
        run: test_gte_scene_rtps_d_sxy,
    },
    TestSpec {
        id: 0x003c,
        group: "GTE",
        name: "scene RTPS D FLAG (0x80006000)",
        run: test_gte_scene_rtps_d_flag,
    },
    TestSpec {
        id: 0x003d,
        group: "GTE",
        name: "NCLIP SXY0 input readback",
        run: test_gte_nclip_in_sxy0,
    },
    TestSpec {
        id: 0x003e,
        group: "GTE",
        name: "NCLIP SXY1 input readback",
        run: test_gte_nclip_in_sxy1,
    },
    TestSpec {
        id: 0x003f,
        group: "GTE",
        name: "NCLIP SXY2 input readback",
        run: test_gte_nclip_in_sxy2,
    },
    TestSpec {
        id: 0x0040,
        group: "GTE",
        name: "NCLIP MAC0 after 8 nops",
        run: test_gte_nclip_mac0_nop8,
    },
    TestSpec {
        id: 0x0041,
        group: "GTE",
        name: "NCLIP MAC0 after 16 nops",
        run: test_gte_nclip_mac0_nop16,
    },
    TestSpec {
        id: 0x0042,
        group: "GTE",
        name: "scene MVMVA A (RT*V0+TR)",
        run: test_gte_scene_mvmva_a,
    },
    TestSpec {
        id: 0x0043,
        group: "GTE",
        name: "scene MVMVA B (RT*V0+TR)",
        run: test_gte_scene_mvmva_b,
    },
    TestSpec {
        id: 0x0044,
        group: "GTE",
        name: "scene MVMVA C (RT*V0+TR)",
        run: test_gte_scene_mvmva_c,
    },
    TestSpec {
        id: 0x0045,
        group: "GTE",
        name: "scene MVMVA D (RT*V0+TR)",
        run: test_gte_scene_mvmva_d,
    },
    TestSpec {
        id: 0x0046,
        group: "GTE",
        name: "scene RTPT A SXY (div-ovf)",
        run: test_gte_scene_rtpt_a_sxy,
    },
    TestSpec {
        id: 0x0047,
        group: "GTE",
        name: "scene RTPT B SXY (div-ovf)",
        run: test_gte_scene_rtpt_b_sxy,
    },
    TestSpec {
        id: 0x0048,
        group: "GTE",
        name: "scene RTPT C SXY (div-ovf)",
        run: test_gte_scene_rtpt_c_sxy,
    },
    TestSpec {
        id: 0x0049,
        group: "GTE",
        name: "scene RTPT D SXY (all-clamp)",
        run: test_gte_scene_rtpt_d_sxy,
    },
    TestSpec {
        id: 0x004a,
        group: "GTE",
        name: "scene RTPT E SXY (in-frustum)",
        run: test_gte_scene_rtpt_e_sxy,
    },
    TestSpec {
        id: 0x004b,
        group: "GTE",
        name: "scene RTPT F SXY (in-frustum)",
        run: test_gte_scene_rtpt_f_sxy,
    },
    TestSpec {
        id: 0x004c,
        group: "GTE",
        name: "scene RTPT A FLAG (0x80006000)",
        run: test_gte_scene_rtpt_a_flag,
    },
    TestSpec {
        id: 0x004d,
        group: "GTE",
        name: "scene RTPT B FLAG (0x80066000)",
        run: test_gte_scene_rtpt_b_flag,
    },
    TestSpec {
        id: 0x004e,
        group: "GTE",
        name: "scene RTPT A SZ3 depth",
        run: test_gte_scene_rtpt_a_sz3,
    },
    TestSpec {
        id: 0x004f,
        group: "GTE",
        name: "scene RTPT E SZ3 depth",
        run: test_gte_scene_rtpt_e_sz3,
    },
    TestSpec {
        id: 0x0050,
        group: "GTE",
        name: "scene NCLIP A MAC0 (real)",
        run: test_gte_scene_nclip_a,
    },
    TestSpec {
        id: 0x0051,
        group: "GTE",
        name: "scene NCLIP B MAC0 (real)",
        run: test_gte_scene_nclip_b,
    },
    TestSpec {
        id: 0x0052,
        group: "GTE",
        name: "scene NCLIP C MAC0 (real)",
        run: test_gte_scene_nclip_c,
    },
    TestSpec {
        id: 0x0053,
        group: "GTE",
        name: "LZCR 0x00ffffff = 8",
        run: test_gte_lzcr_zeros,
    },
    TestSpec {
        id: 0x0054,
        group: "GTE",
        name: "LZCR 0xffff0000 = 16",
        run: test_gte_lzcr_half,
    },
    TestSpec {
        id: 0x0055,
        group: "GTE",
        name: "LZCR 0x00000001 = 31",
        run: test_gte_lzcr_one,
    },
    TestSpec {
        id: 0x0056,
        group: "GTE",
        name: "LZCR 0x7fffffff = 1",
        run: test_gte_lzcr_posmax,
    },
    TestSpec {
        id: 0x0057,
        group: "GTE",
        name: "LZCR 0x80000000 = 1",
        run: test_gte_lzcr_negmin,
    },
    TestSpec {
        id: 0x0058,
        group: "GTE",
        name: "MVMVA FC-bug MAC1",
        run: test_gte_mvmva_fc_mac1,
    },
    TestSpec {
        id: 0x0059,
        group: "GTE",
        name: "MVMVA FC-bug MAC2",
        run: test_gte_mvmva_fc_mac2,
    },
    TestSpec {
        id: 0x005a,
        group: "GTE",
        name: "MVMVA FC-bug MAC3",
        run: test_gte_mvmva_fc_mac3,
    },
    TestSpec {
        id: 0x005b,
        group: "GTE",
        name: "SQR squares IR1..3",
        run: test_gte_sqr,
    },
    TestSpec {
        id: 0x005c,
        group: "GTE",
        name: "OP cross product MAC1",
        run: test_gte_op_mac1,
    },
    TestSpec {
        id: 0x005d,
        group: "GTE",
        name: "OP cross product MAC2",
        run: test_gte_op_mac2,
    },
    TestSpec {
        id: 0x005e,
        group: "GTE",
        name: "OP cross product MAC3",
        run: test_gte_op_mac3,
    },
    TestSpec {
        id: 0x005f,
        group: "GTE",
        name: "OP full-seed MAC1",
        run: test_gte_op_full_seed_mac1,
    },
    TestSpec {
        id: 0x0060,
        group: "GTE",
        name: "OP full-seed MAC2",
        run: test_gte_op_full_seed_mac2,
    },
    TestSpec {
        id: 0x0061,
        group: "GTE",
        name: "OP full-seed MAC3",
        run: test_gte_op_full_seed_mac3,
    },
    TestSpec {
        id: 0x0062,
        group: "GTE",
        name: "AVSZ3 averages SZ -> OTZ",
        run: test_gte_avsz3,
    },
    TestSpec {
        id: 0x0063,
        group: "GTE",
        name: "RTPS SXY2 read-latency",
        run: test_gte_lat_sxy2,
    },
    TestSpec {
        id: 0x0064,
        group: "GTE",
        name: "RTPS SZ3 read-latency",
        run: test_gte_lat_sz3,
    },
    TestSpec {
        id: 0x0065,
        group: "GTE",
        name: "RTPS IR1 read-latency",
        run: test_gte_lat_ir1,
    },
    TestSpec {
        id: 0x0066,
        group: "GTE",
        name: "RTPS IR0 read-latency",
        run: test_gte_lat_ir0,
    },
    TestSpec {
        id: 0x0067,
        group: "GTE",
        name: "RT load settle +0",
        run: test_rt_settle_gap0,
    },
    TestSpec {
        id: 0x0068,
        group: "GTE",
        name: "RT load settle +2",
        run: test_rt_settle_gap2,
    },
    TestSpec {
        id: 0x0069,
        group: "GTE",
        name: "RT load settle +4",
        run: test_rt_settle_gap4,
    },
    TestSpec {
        id: 0x006a,
        group: "GTE",
        name: "RT load settle +8",
        run: test_rt_settle_gap8,
    },
    TestSpec {
        id: 0x006b,
        group: "GTE",
        name: "RT load settle +16",
        run: test_rt_settle_gap16,
    },
    TestSpec {
        id: 0x006c,
        group: "GTE",
        name: "RT load settle +32",
        run: test_rt_settle_gap32,
    },
    TestSpec {
        id: 0x006d,
        group: "GTE",
        name: "RT drop-during-RTPS +0",
        run: test_rt_drop_gap0,
    },
    TestSpec {
        id: 0x006e,
        group: "GTE",
        name: "RT drop-during-RTPS +4",
        run: test_rt_drop_gap4,
    },
    TestSpec {
        id: 0x006f,
        group: "GTE",
        name: "RT drop-during-RTPS +8",
        run: test_rt_drop_gap8,
    },
    TestSpec {
        id: 0x0070,
        group: "GTE",
        name: "RT drop-during-RTPS +16",
        run: test_rt_drop_gap16,
    },
    TestSpec {
        id: 0x0071,
        group: "GTE",
        name: "compose chain hot (engine shape)",
        run: test_compose_chain_hot,
    },
    TestSpec {
        id: 0x0072,
        group: "GTE",
        name: "compose chain V0-settled",
        run: test_compose_chain_v0_settled,
    },
    TestSpec {
        id: 0x0073,
        group: "GTE",
        name: "compose chain load-settled",
        run: test_compose_chain_load_settled,
    },
    TestSpec {
        id: 0x0074,
        group: "GTE",
        name: "NCLIP MAC0 settle +1",
        run: test_mac0_settle_gap1,
    },
    TestSpec {
        id: 0x0075,
        group: "GTE",
        name: "NCLIP MAC0 settle +2",
        run: test_mac0_settle_gap2,
    },
    TestSpec {
        id: 0x0076,
        group: "GTE",
        name: "NCLIP MAC0 settle +3",
        run: test_mac0_settle_gap3,
    },
    TestSpec {
        id: 0x0077,
        group: "GTE",
        name: "NCLIP MAC0 settle +4",
        run: test_mac0_settle_gap4,
    },
    TestSpec {
        id: 0x0078,
        group: "GTE",
        name: "NCLIP MAC0 settle +6",
        run: test_mac0_settle_gap6,
    },
    TestSpec {
        id: 0x0079,
        group: "GTE",
        name: "LZCR settle +1",
        run: test_lzcr_settle_gap1,
    },
    TestSpec {
        id: 0x007a,
        group: "GTE",
        name: "LZCR settle +2",
        run: test_lzcr_settle_gap2,
    },
    TestSpec {
        id: 0x007b,
        group: "GTE",
        name: "LZCR settle +3",
        run: test_lzcr_settle_gap3,
    },
    TestSpec {
        id: 0x007c,
        group: "GTE",
        name: "LZCR settle +4",
        run: test_lzcr_settle_gap4,
    },
    TestSpec {
        id: 0x007d,
        group: "GTE",
        name: "LZCR settle +6",
        run: test_lzcr_settle_gap6,
    },
    TestSpec {
        id: 0x007e,
        group: "GTE",
        name: "NCLIP big-value settle +1",
        run: test_mac0_big_settle_gap1,
    },
    TestSpec {
        id: 0x007f,
        group: "GTE",
        name: "NCLIP big-value settle +2",
        run: test_mac0_big_settle_gap2,
    },
    TestSpec {
        id: 0x0080,
        group: "GTE",
        name: "NCLIP big-value settle +4",
        run: test_mac0_big_settle_gap4,
    },
    TestSpec {
        id: 0x0081,
        group: "GTE",
        name: "NCLIP big-value settle +8",
        run: test_mac0_big_settle_gap8,
    },
    TestSpec {
        id: 0x0082,
        group: "GTE",
        name: "NCLIP big-value settle +12",
        run: test_mac0_big_gap12,
    },
    TestSpec {
        id: 0x0083,
        group: "GTE",
        name: "NCLIP big-value settle +16",
        run: test_mac0_big_gap16,
    },
    TestSpec {
        id: 0x0084,
        group: "GTE",
        name: "NCLIP big-value settle +24",
        run: test_mac0_big_gap24,
    },
    TestSpec {
        id: 0x0085,
        group: "GTE",
        name: "NCLIP big-value settle +32",
        run: test_mac0_big_gap32,
    },
    TestSpec {
        id: 0x0086,
        group: "GTE",
        name: "NCLIP big-value settle +48",
        run: test_mac0_big_gap48,
    },
    TestSpec {
        id: 0x0087,
        group: "GTE",
        name: "NCLIP magnitude quarter +4",
        run: test_mac0_mag_quarter,
    },
    TestSpec {
        id: 0x0088,
        group: "GTE",
        name: "NCLIP magnitude half +4",
        run: test_mac0_mag_half,
    },
    TestSpec {
        id: 0x0089,
        group: "GTE",
        name: "NCLIP magnitude double +4",
        run: test_mac0_mag_double,
    },
    TestSpec {
        id: 0x008a,
        group: "GTE",
        name: "NCLIP controlled scene-B +2",
        run: test_mac0_ctrl_b,
    },
    TestSpec {
        id: 0x008b,
        group: "GTE",
        name: "NCLIP controlled scene-C +2",
        run: test_mac0_ctrl_c,
    },
    TestSpec {
        id: 0x008c,
        group: "GTE",
        name: "SXY0 dump after A writes",
        run: test_sxy_dump_a_12,
    },
    TestSpec {
        id: 0x008d,
        group: "GTE",
        name: "SXY1 dump after A writes",
        run: test_sxy_dump_a_13,
    },
    TestSpec {
        id: 0x008e,
        group: "GTE",
        name: "SXY2 dump after A writes",
        run: test_sxy_dump_a_14,
    },
    TestSpec {
        id: 0x008f,
        group: "GTE",
        name: "SXY0 dump after C writes",
        run: test_sxy_dump_c_12,
    },
    TestSpec {
        id: 0x0090,
        group: "GTE",
        name: "SXY1 dump after C writes",
        run: test_sxy_dump_c_13,
    },
    TestSpec {
        id: 0x0091,
        group: "GTE",
        name: "SXY2 dump after C writes",
        run: test_sxy_dump_c_14,
    },
    TestSpec {
        id: 0x0092,
        group: "GTE",
        name: "SXY0 after probe NCLIP (A)",
        run: test_sxy_post_a,
    },
    TestSpec {
        id: 0x0093,
        group: "GTE",
        name: "SXY0 after probe NCLIP (C)",
        run: test_sxy_post_c,
    },
    TestSpec {
        id: 0x0094,
        group: "GPU",
        name: "VRAM fill + read-back",
        run: test_gpu_vram_roundtrip,
    },
    TestSpec {
        id: 0x0095,
        group: "GPU",
        name: "draw flat triangle",
        run: test_gpu_draw_flat_tri,
    },
    TestSpec {
        id: 0x0096,
        group: "GPU",
        name: "draw gouraud triangle",
        run: test_gpu_draw_gouraud_tri,
    },
    TestSpec {
        id: 0x0097,
        group: "GPU",
        name: "draw flat quad",
        run: test_gpu_draw_flat_quad,
    },
    TestSpec {
        id: 0x0098,
        group: "GPU",
        name: "draw gouraud quad",
        run: test_gpu_draw_gouraud_quad,
    },
    TestSpec {
        id: 0x0099,
        group: "GPU",
        name: "tri vertex past right edge",
        run: test_gpu_tri_past_right_edge,
    },
    TestSpec {
        id: 0x009a,
        group: "GPU",
        name: "tri negative coordinate",
        run: test_gpu_tri_negative_coord,
    },
    TestSpec {
        id: 0x009b,
        group: "GPU",
        name: "tri coord exceeds 11-bit (wrap)",
        run: test_gpu_tri_coord_wrap,
    },
    TestSpec {
        id: 0x009c,
        group: "GPU",
        name: "textured gouraud tri (player prim)",
        run: test_gpu_textured_gouraud_tri,
    },
    TestSpec {
        id: 0x009d,
        group: "GPU",
        name: "OT + DMA linked-list draw",
        run: test_gpu_ot_dma_draw,
    },
    TestSpec {
        id: 0x009e,
        group: "GPU",
        name: "tri X-span > 1023 (poly-too-large)",
        run: test_gpu_tri_large_span,
    },
    TestSpec {
        id: 0x009f,
        group: "GPU",
        name: "tri vertex past bottom edge",
        run: test_gpu_tri_y_past_edge,
    },
    TestSpec {
        id: 0x00a0,
        group: "GPU",
        name: "textured gouraud large span",
        run: test_gpu_texgouraud_large_span,
    },
    TestSpec {
        id: 0x00a1,
        group: "GPU",
        name: "textured gouraud via OT + DMA (player path)",
        run: test_gpu_texgouraud_ot_dma,
    },
    TestSpec {
        id: 0x00a2,
        group: "GPU",
        name: "8bpp CLUT textured tri (model format)",
        run: test_gpu_8bpp_clut_tri,
    },
    TestSpec {
        id: 0x00a3,
        group: "GPU",
        name: "deep OT + DMA (8 prims)",
        run: test_gpu_big_ot,
    },
    TestSpec {
        id: 0x00a4,
        group: "SPU",
        name: "voice0 writable-bit mask",
        run: test_spu_voice_writable_mask,
    },
    TestSpec {
        id: 0x00a5,
        group: "SPU",
        name: "voice0 pitch/ADSR readback",
        run: test_spu_voice_reg_readback,
    },
    TestSpec {
        id: 0x00a6,
        group: "SPU",
        name: "SPU RAM DMA upload round-trip",
        run: test_spu_ram_dma_roundtrip,
    },
    TestSpec {
        id: 0x00a7,
        group: "SPU",
        name: "SPU RAM manual-FIFO upload round-trip",
        run: test_spu_ram_manual_fifo_roundtrip,
    },
    TestSpec {
        id: 0x00a8,
        group: "GPU",
        name: "ordered-dither checkerboard (flat mid-tone)",
        run: test_gpu_dither_checkerboard,
    },
    TestSpec {
        id: 0x00a9,
        group: "GPU",
        name: "mask bit on CPU->VRAM copy",
        run: test_gpu_cpu_vram_upload_mask,
    },
    TestSpec {
        id: 0x00aa,
        group: "SIO",
        name: "port 1 handshake strict (no-wait)",
        run: test_pad_handshake_strict,
    },
    TestSpec {
        id: 0x00ab,
        group: "SIO",
        name: "port 1 diag setup+inter timing",
        run: test_pad_diag_timing,
    },
    TestSpec {
        id: 0x00ac,
        group: "SIO",
        name: "DualShock analog enable handshake",
        run: test_pad_analog_handshake,
    },
    // v1.17 NCLIP-mechanism discriminators (all characterisation; silicon
    // has never answered any of them, so there is nothing to pass or fail
    // yet). Together they separate the three candidate models for the
    // settle/winding anomalies: write-port commit hazard (SXYP variant),
    // magnitude-dependent MAC pipeline passes (ladder extension), and the
    // documented-but-unverified CPU read interlock (Timer 2 bracket).
    TestSpec {
        id: 0x00ad,
        group: "GTE",
        name: "NCLIP controlled scene-C +2 via SXYP",
        run: test_mac0_ctrl_c_sxyp,
    },
    TestSpec {
        id: 0x00ae,
        group: "GTE",
        name: "NCLIP magnitude eighth +4",
        run: test_mac0_mag_eighth,
    },
    TestSpec {
        id: 0x00af,
        group: "GTE",
        name: "NCLIP magnitude sixteenth +4",
        run: test_mac0_mag_sixteenth,
    },
    TestSpec {
        id: 0x00b0,
        group: "GTE",
        name: "NCLIP read-interlock T2 bracket",
        run: test_gte_nclip_read_interlock,
    },
    // CLUT-cache semantics, console-proven by proxy on 2026-08-07: the
    // demo-disc launcher rewrote its shot palette in place under a constant
    // clut word and real hardware kept showing the first palette for 8bpp
    // colors 10h-FFh even with 4bpp text (different clut words) interleaved
    // every frame. These two cases turn that observation into first-class
    // records: 0xb1 pins the no-reload-on-data-rewrite half, 0xb2 pins the
    // per-line half (an interleaved 4bpp draw with a different clut word
    // must NOT refresh the 240-entry 8bpp line).
    TestSpec {
        id: 0x00b1,
        group: "GPU",
        name: "CLUT stale after in-place rewrite",
        run: test_gpu_clut_inplace_rewrite_stale,
    },
    TestSpec {
        id: 0x00b2,
        group: "GPU",
        name: "CLUT 8bpp line survives 4bpp interleave",
        run: test_gpu_clut_stale_across_4bpp_interleave,
    },
    // Demo-disc text-pipeline replica: the console renders SPLEEN 'f' as a
    // bare crossbar through this exact pipeline while emulators draw it
    // whole. Expected values pinned from the emulator; a console FAIL
    // names the corrupt stage and its observed hash is the finding.
    TestSpec {
        id: 0x00b3,
        group: "GPU",
        name: "text cache glyph pass lands",
        run: test_gpu_text_cache_glyphs_land,
    },
    TestSpec {
        id: 0x00b4,
        group: "GPU",
        name: "text cache 15bpp blit output",
        run: test_gpu_text_cache_blit,
    },
    TestSpec {
        id: 0x00b5,
        group: "GPU",
        name: "direct SPLEEN glyph draw",
        run: test_gpu_text_direct_draw,
    },
    TestSpec {
        id: 0x00b6,
        group: "GPU",
        name: "lone glyph f",
        run: test_gpu_glyph_f,
    },
    TestSpec {
        id: 0x00b7,
        group: "GPU",
        name: "lone glyph r",
        run: test_gpu_glyph_r,
    },
    TestSpec {
        id: 0x00b8,
        group: "GPU",
        name: "lone glyph t",
        run: test_gpu_glyph_t,
    },
    TestSpec {
        id: 0x00b9,
        group: "GPU",
        name: "lone glyph o",
        run: test_gpu_glyph_o,
    },
    TestSpec {
        id: 0x00ba,
        group: "GPU",
        name: "glyph f after r (cache aliasing)",
        run: test_gpu_glyph_f_after_r,
    },
    TestSpec {
        id: 0x00bb,
        group: "GPU",
        name: "SPLEEN atlas VRAM readback",
        run: test_gpu_glyph_atlas_readback,
    },
    TestSpec {
        id: 0x00bc,
        group: "SPU",
        name: "SPU RAM read is repeatable",
        run: test_spu_read_is_repeatable,
    },
    TestSpec {
        id: 0x00bd,
        group: "SPU",
        name: "SPU DMA and FIFO writes agree",
        run: test_spu_dma_and_fifo_agree,
    },
    TestSpec {
        id: 0x00be,
        group: "SPU",
        name: "SPU upload, addr after mode",
        run: test_spu_upload_addr_after_mode,
    },
    TestSpec {
        id: 0x00bf,
        group: "SPU",
        name: "SPU upload, four small blocks",
        run: test_spu_upload_small_blocks,
    },
    TestSpec {
        id: 0x00c0,
        group: "GTE",
        name: "RTPT input commit +0",
        run: test_rtpt_input_gap0,
    },
    TestSpec {
        id: 0x00c1,
        group: "GTE",
        name: "RTPT input commit +1",
        run: test_rtpt_input_gap1,
    },
    TestSpec {
        id: 0x00c2,
        group: "GTE",
        name: "RTPT input commit +2",
        run: test_rtpt_input_gap2,
    },
    TestSpec {
        id: 0x00c3,
        group: "GTE",
        name: "RTPT input commit +4",
        run: test_rtpt_input_gap4,
    },
    TestSpec {
        id: 0x00c4,
        group: "GTE",
        name: "RTPT result read +0",
        run: test_rtpt_read_gap0,
    },
    TestSpec {
        id: 0x00c5,
        group: "GTE",
        name: "RTPT result read +8",
        run: test_rtpt_read_gap8,
    },
    TestSpec {
        id: 0x00c6,
        group: "GTE",
        name: "RTPT result read +16",
        run: test_rtpt_read_gap16,
    },
    TestSpec {
        id: 0x00c7,
        group: "GTE",
        name: "RTPT result read +24",
        run: test_rtpt_read_gap24,
    },
    // v1.24: console gates for the pending performance levers
    // (lever_probes.rs). RESUME FROM TEST 200 runs them and everything after.
    TestSpec {
        id: 0x00c8,
        group: "IRQ",
        name: "GTE vs IRQ, return to EPC: exposure",
        run: lever_probes::test_gte_irq_plain_exposure,
    },
    TestSpec {
        id: 0x00c9,
        group: "IRQ",
        name: "GTE vs IRQ, return to EPC: RTPS intact",
        run: lever_probes::test_gte_irq_plain_intact,
    },
    TestSpec {
        id: 0x00ca,
        group: "IRQ",
        name: "GTE vs IRQ, skip GTE at EPC: exposure",
        run: lever_probes::test_gte_irq_skip_exposure,
    },
    TestSpec {
        id: 0x00cb,
        group: "IRQ",
        name: "GTE vs IRQ, skip GTE at EPC: RTPS intact",
        run: lever_probes::test_gte_irq_skip_intact,
    },
    TestSpec {
        id: 0x00cc,
        group: "GPU",
        name: "present queue: frames kicked and drawn",
        run: lever_probes::test_present_queue_frames,
    },
    TestSpec {
        id: 0x00cd,
        group: "GPU",
        name: "present queue: bit 28 idle means drawn",
        run: lever_probes::test_present_queue_bit28,
    },
    TestSpec {
        id: 0x00ce,
        group: "GPU",
        name: "present queue: flip lines after VBlank",
        run: lever_probes::test_present_queue_flip_line,
    },
    TestSpec {
        id: 0x00cf,
        group: "GPU",
        name: "present queue: busy edges skipped",
        run: lever_probes::test_present_queue_skipped,
    },
    TestSpec {
        id: 0x00d0,
        group: "RAM",
        name: "scratchpad stack: checksum vs RAM stack",
        run: lever_probes::test_spstack_checksum,
    },
    TestSpec {
        id: 0x00d1,
        group: "RAM",
        name: "scratchpad stack: IRQs taken, all intact",
        run: lever_probes::test_spstack_integrity,
    },
    TestSpec {
        id: 0x00d2,
        group: "RAM",
        name: "scratchpad stack: background activity",
        run: lever_probes::test_spstack_activity,
    },
    // v1.24: how long channel 2 stays busy on a list whose nodes draw, and
    // what the CPU gets done meanwhile (list_busy_probes.rs). Indices 211-233.
    TestSpec {
        id: 0x00d3,
        group: "DMA",
        name: "empty list: CHCR clear",
        run: list_busy_probes::test_empty_chcr,
    },
    TestSpec {
        id: 0x00d4,
        group: "DMA",
        name: "empty list: GP0(1Fh) IRQ",
        run: list_busy_probes::test_empty_irq,
    },
    TestSpec {
        id: 0x00d5,
        group: "DMA",
        name: "empty list: GPUSTAT.28 settled",
        run: list_busy_probes::test_empty_bit28,
    },
    TestSpec {
        id: 0x00d6,
        group: "DMA",
        name: "empty list: GPUSTAT.26 settled",
        run: list_busy_probes::test_empty_bit26,
    },
    TestSpec {
        id: 0x00d7,
        group: "DMA",
        name: "cheap list: CHCR clear",
        run: list_busy_probes::test_cheap_chcr,
    },
    TestSpec {
        id: 0x00d8,
        group: "DMA",
        name: "cheap list: GP0(1Fh) IRQ",
        run: list_busy_probes::test_cheap_irq,
    },
    TestSpec {
        id: 0x00d9,
        group: "DMA",
        name: "cheap list: GPUSTAT.28 settled",
        run: list_busy_probes::test_cheap_bit28,
    },
    TestSpec {
        id: 0x00da,
        group: "DMA",
        name: "cheap list: GPUSTAT.26 settled",
        run: list_busy_probes::test_cheap_bit26,
    },
    TestSpec {
        id: 0x00db,
        group: "DMA",
        name: "expensive list: CHCR clear",
        run: list_busy_probes::test_expensive_chcr,
    },
    TestSpec {
        id: 0x00dc,
        group: "DMA",
        name: "expensive list: GP0(1Fh) IRQ",
        run: list_busy_probes::test_expensive_irq,
    },
    TestSpec {
        id: 0x00dd,
        group: "DMA",
        name: "expensive list: GPUSTAT.28 settled",
        run: list_busy_probes::test_expensive_bit28,
    },
    TestSpec {
        id: 0x00de,
        group: "DMA",
        name: "expensive list: GPUSTAT.26 settled",
        run: list_busy_probes::test_expensive_bit26,
    },
    TestSpec {
        id: 0x00df,
        group: "DMA",
        name: "packed list: CHCR clear",
        run: list_busy_probes::test_packed_chcr,
    },
    TestSpec {
        id: 0x00e0,
        group: "DMA",
        name: "packed list: GP0(1Fh) IRQ",
        run: list_busy_probes::test_packed_irq,
    },
    TestSpec {
        id: 0x00e1,
        group: "DMA",
        name: "packed list: GPUSTAT.28 settled",
        run: list_busy_probes::test_packed_bit28,
    },
    TestSpec {
        id: 0x00e2,
        group: "DMA",
        name: "packed list: GPUSTAT.26 settled",
        run: list_busy_probes::test_packed_bit26,
    },
    TestSpec {
        id: 0x00e3,
        group: "DMA",
        name: "packed list draws the same pixels",
        run: list_busy_probes::test_packed_pixels,
    },
    TestSpec {
        id: 0x00e4,
        group: "DMA",
        name: "CPU during walk: ALU iterations",
        run: list_busy_probes::test_alu_counts,
    },
    TestSpec {
        id: 0x00e5,
        group: "DMA",
        name: "CPU during walk: ALU walk clocks",
        run: list_busy_probes::test_alu_cycles,
    },
    TestSpec {
        id: 0x00e6,
        group: "DMA",
        name: "CPU during walk: RAM load iterations",
        run: list_busy_probes::test_ram_counts,
    },
    TestSpec {
        id: 0x00e7,
        group: "DMA",
        name: "CPU during walk: RAM load walk clocks",
        run: list_busy_probes::test_ram_cycles,
    },
    TestSpec {
        id: 0x00e8,
        group: "DMA",
        name: "CPU during walk: scratchpad iterations",
        run: list_busy_probes::test_spad_counts,
    },
    TestSpec {
        id: 0x00e9,
        group: "DMA",
        name: "CPU during walk: scratchpad walk clocks",
        run: list_busy_probes::test_spad_cycles,
    },
];

fn mix32(mut hash: u32, value: u32) -> u32 {
    hash ^= value;
    hash = hash.wrapping_mul(0x0100_0193);
    hash.rotate_left(5)
}

fn mix_str(mut hash: u32, value: &str) -> u32 {
    for byte in value.bytes() {
        hash = mix32(hash, byte as u32);
    }
    hash
}

fn run_gte_scan() -> ScanReport {
    let mut hash = 0x4754_4501;
    let mut items = 0u16;
    let mut flag_master_hits = 0u32;

    macro_rules! sample {
        ($instr:expr, $call:expr) => {{
            seed_gte_state();
            unsafe { $call };
            let snapshot = gte_snapshot_hash();
            if read_control!(31) & 0x8000_0000 != 0 {
                flag_master_hits = flag_master_hits.wrapping_add(1);
            }
            hash = mix32(hash, $instr);
            hash = mix32(hash, snapshot);
            items = items.wrapping_add(1);
        }};
    }

    sample!(0x4A08_0001, gte_ops::project_single());
    sample!(0x4A08_0030, gte_ops::project_triple());
    sample!(0x4A00_0006, gte_ops::screen_winding());
    sample!(0x4A08_000C, gte_ops::outer_product());
    sample!(0x4A00_002D, gte_ops::average_z3());
    sample!(0x4A00_002E, gte_ops::average_z4());
    sample!(0x4A08_0028, gte_ops::square());
    sample!(0x4A08_0013, gte_ops::light_color_depth_single());
    sample!(0x4A08_001B, gte_ops::light_color_single());
    sample!(0x4A08_001E, gte_ops::light_single());
    sample!(0x4A08_0016, gte_ops::light_color_depth_triple());
    sample!(0x4A08_0020, gte_ops::light_triple());
    sample!(0x4A08_003F, gte_ops::light_color_triple());
    sample!(0x4A08_0010, gte_ops::depth_cue_single());
    sample!(0x4A08_002A, gte_ops::depth_cue_triple());
    sample!(0x4A08_0011, gte_ops::interpolate_far_color());
    sample!(0x4A08_0029, gte_ops::depth_cue_light());
    sample!(0x4A08_001C, gte_ops::color_color());
    sample!(0x4A08_0014, gte_ops::color_depth_cue());
    sample!(0x4A08_003D, gte_ops::scale_vector());
    sample!(0x4A08_003E, gte_ops::scale_vector_accumulate());
    sample!(0x4A08_0012, gte_ops::rotate_translate_v0());

    ScanReport::info(items, hash, flag_master_hits)
}

fn run_spu_scan() -> ScanReport {
    const VOICE_STRIDE: u32 = 0x10;
    const OFFSETS: [u32; 7] = [0, 2, 4, 6, 8, 10, 14];

    let mut hash = 0x5350_5501;
    let mut items = 0u16;
    let mut changed = 0u32;

    unsafe {
        for voice in 0..24u32 {
            let base = psx_hw::spu::BASE + voice * VOICE_STRIDE;
            for offset in OFFSETS {
                let addr = base + offset;
                let old = psx_io::read_u16(addr);
                let pattern = 0x1000u16
                    ^ ((voice as u16).wrapping_mul(0x0111))
                    ^ ((offset as u16).wrapping_mul(0x0029));
                psx_io::write_u16(addr, pattern);
                let readback = psx_io::read_u16(addr);
                psx_io::write_u16(addr, old);
                if readback != old {
                    changed = changed.wrapping_add(1);
                }
                hash = mix32(hash, addr);
                hash = mix32(hash, pattern as u32);
                hash = mix32(hash, readback as u32);
                items = items.wrapping_add(1);
            }
        }
    }

    ScanReport::info(items, hash, changed)
}

fn push_timing_record(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
    record: TimingRecord,
) {
    tty::print("hardware-tests: rec ");
    tty::print(hex2((record.id >> 8) as u8).as_str());
    tty::print(hex2(record.id as u8).as_str());
    tty::print(" min=");
    tty_print_dec_u16(record.min);
    tty::print(" max=");
    tty_print_dec_u16(record.max);
    tty::println("");
    // Saturate rather than index past the end: with panic=abort an overflow
    // would hang the console mid-scan. tools/psoxide-hwtest/tests keeps the
    // slot count ahead of the record count.
    if let Some(slot) = records.get_mut(*next) {
        *slot = record;
        *next += 1;
    } else {
        // SAFETY: single thread; a plain static.
        unsafe { DROPPED_RECORDS += 1 };
    }
}

/// Records that did not fit, and record fields clamped to 16 bits: both are
/// counted into the integrity record `0x41E` instead of passing silently.
static mut DROPPED_RECORDS: u32 = 0;
static mut CLAMPED_FIELDS: u32 = 0;
static mut FIRST_CLAMPED_ID: u16 = 0;
static mut LAST_CLAMPED_ID: u16 = 0;

// Repeat the probe with interrupts masked, then keep min/median/max.
//
// Masking matters: without it the only defence against a VBlank or CD IRQ
// landing inside a measured window is that one of the repeats happens to
// escape, so the min/max gap reports "did an interrupt hit" rather than real
// hardware jitter. With IE clear the spread is the silicon's own.

// One shared body behind a dyn call. A generic body is monomorphised once per
// call site, which is ~150 copies of the sampling loop in an EXE that has a
// few sectors of headroom before it reaches the CDTEST region; and inlined,
// those copies push run_timing_scan past the +-128 KiB reach of a MIPS PC16
// branch. The indirect call sits outside every probe's measured window.
#[inline(always)]
fn sample_timing<F>(id: u16, work: u16, mut probe: F) -> TimingRecord
where
    F: FnMut() -> u16,
{
    sample_timing_dyn(id, work, &mut probe)
}

#[inline(never)]
fn sample_timing_dyn(id: u16, work: u16, probe: &mut dyn FnMut() -> u16) -> TimingRecord {
    bounds::record_start(id);
    let mut samples = [0u16; TIMING_SAMPLES];
    let guard = IrqGuard::mask();
    let mut run = 0;
    while run < TIMING_SAMPLES {
        samples[run] = probe();
        run += 1;
        ui::heartbeat(run & 1 == 0);
    }
    drop(guard);

    // Insertion sort: five elements, no allocator, no core::slice::sort.
    let mut i = 1;
    while i < TIMING_SAMPLES {
        let value = samples[i];
        let mut j = i;
        while j > 0 && samples[j - 1] > value {
            samples[j] = samples[j - 1];
            j -= 1;
        }
        samples[j] = value;
        i += 1;
    }

    TimingRecord {
        id,
        work,
        min: samples[0],
        med: samples[TIMING_SAMPLES / 2],
        max: samples[TIMING_SAMPLES - 1],
    }
}

/// Clears COP0 Status.IE for the lifetime of the guard and restores the exact
/// prior word on drop. Kept as a guard so an early return cannot leave the
/// console running with interrupts masked.
struct IrqGuard {
    status: u32,
}

impl IrqGuard {
    fn mask() -> Self {
        let status: u32;
        unsafe {
            core::arch::asm!("mfc0 $8, $12", "nop", lateout("$8") status);
            core::arch::asm!(
                "mtc0 $8, $12",
                "nop",
                "nop",
                "nop",
                in("$8") status & !1,
                options(nostack, nomem),
            );
        }
        Self { status }
    }
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        unsafe {
            core::arch::asm!(
                "mtc0 $8, $12",
                "nop",
                "nop",
                "nop",
                in("$8") self.status,
                options(nostack, nomem),
            );
        }
    }
}

/// Recover the main-RAM refresh cadence and the extra wait imposed by a
/// refresh slot. Both metrics come from the same five scans so the final two
/// Capture records remain correlated without consuming another photo page.
fn sample_dram_refresh() -> (TimingRecord, TimingRecord) {
    let mut periods = [0u16; TIMING_SAMPLES];
    let mut stalls = [0u16; TIMING_SAMPLES];
    let mut run = 0;
    while run < TIMING_SAMPLES {
        let (period, stall) = measure_dram_refresh();
        periods[run] = period;
        stalls[run] = stall;
        run += 1;
    }
    (
        // A scan can legitimately miss two adjacent refresh events and return
        // zero. `summarize_nonzero` drops those so one miss cannot masquerade
        // as a zero-cycle refresh period.
        summarize_nonzero(0x70, 4096, periods),
        summarize_nonzero(0x71, 4096, stalls),
    )
}

/// Min/median/max over the non-zero samples only, for probes where a zero
/// means "this scan did not observe the event" rather than a measurement.
fn summarize_nonzero(id: u16, work: u16, samples: [u16; TIMING_SAMPLES]) -> TimingRecord {
    let mut kept = [0u16; TIMING_SAMPLES];
    let mut count = 0usize;
    let mut i = 0;
    while i < TIMING_SAMPLES {
        if samples[i] != 0 {
            kept[count] = samples[i];
            count += 1;
        }
        i += 1;
    }
    if count == 0 {
        return TimingRecord {
            id,
            work,
            min: 0,
            med: 0,
            max: 0,
        };
    }
    let mut i = 1;
    while i < count {
        let value = kept[i];
        let mut j = i;
        while j > 0 && kept[j - 1] > value {
            kept[j] = kept[j - 1];
            j -= 1;
        }
        kept[j] = value;
        i += 1;
    }
    TimingRecord {
        id,
        work,
        min: kept[0],
        med: kept[count / 2],
        max: kept[count - 1],
    }
}

// ---------------------------------------------------------------------------
// CD-ROM battery
//
// The drive is the one subsystem where the console is the only usable
// instrument: seek time is mechanical, and no emulator models head travel.
// Timer 2 at the system clock wraps after ~1.9 ms, far short of a seek, so
// every record here is timed on Timer 1's HBlank clock: ~63.9 us per tick and
// ~4.19 s of range, which covers a full-stroke seek with room to spare.
//
// Seek and read acknowledge immediately and finish much later, so these wait
// for the SECOND response (the completion IRQ). Timing to the ack would
// measure command dispatch and report a mechanical seek as microseconds.
// ---------------------------------------------------------------------------

/// First LBA of the deterministic 600-sector CDTEST.BIN region.
const CD_TEST_LBA: u32 = 424;
/// Poll budget for the FIFO/dispatch handshakes, which are electrical and
/// fast. Mechanical waits are bounded in real time by `CD_DEADLINE_HBLANKS`
/// instead: a poll count is only a proxy for time and drifts with CPU and bus
/// speed, which is exactly wrong for measuring a drive.
const CD_SPINS: u32 = 200_000;
/// Loop passes the deadline-bound CD waits get besides their HBlank deadline,
/// so that a stopped timer cannot make them endless. A pass is a CD register
/// read and a counter read, some twenty clocks; the deadline is half a
/// second of HBlanks, so this is a good deal more than it can use.
const CD_WAIT_SPINS: u32 = 30_000_000;
/// Real-time deadline for one mechanical CD operation, in Timer 1 HBlank ticks
/// (~63.9 us each). 31,250 ticks is about two seconds, comfortably past a
/// full-stroke seek on a slow CD-R while still bounding a dead drive.
const CD_DEADLINE_HBLANKS: u16 = 31_250;
/// Reported when a command did not complete within `CD_SPINS`. Distinguishes
/// "the drive never answered" from "the operation took zero time", which a
/// plain 0 could not.
const CD_FAILED: u16 = 0xFFFF;

/// Time one CD operation on the HBlank clock, in Timer 1 ticks.
fn cd_timed<F>(op: F) -> u16
where
    F: FnOnce() -> bool,
{
    cd_clock_reset();
    let ok = op();
    let elapsed = timers::counter(timers::Timer::Timer1);
    if ok {
        // A wrapped counter would report a fast seek instead of a slow one.
        // The clock covers 4.19 s, so a wrap means the drive is not behaving
        // and the value must not be read as a measurement.
        elapsed
    } else {
        CD_FAILED
    }
}

/// Arm Timer 1 on the HBlank clock and zero it. Every CD deadline and
/// measurement below reads this one counter.
fn cd_clock_reset() {
    timers::set_mode(timers::Timer::Timer1, TIMER_MODE_CLOCK_SOURCE_1);
    timers::set_counter(timers::Timer::Timer1, 0);
}

/// Run a command to completion under a real-time deadline.
///
/// Waits for the ack and then the completion IRQ, giving up once Timer 1
/// passes `CD_DEADLINE_HBLANKS`. Assumes the caller has already reset the
/// clock, so the deadline covers the whole operation being timed.
fn cd_command_until_complete_timed(command: u8, params: &[u8]) -> bool {
    let Some(saved) = psx_io::cd::dispatch_command(command, params, CD_SPINS) else {
        return false;
    };
    let mut seen_ack = false;
    let mut spins = 0u32;
    let ok = loop {
        spins += 1;
        if timers::counter(timers::Timer::Timer1) >= CD_DEADLINE_HBLANKS
            || spins > bounds::scale(CD_WAIT_SPINS)
        {
            break false;
        }
        match psx_io::cd::irq_flag_value() {
            0 => {}
            5 => {
                psx_io::cd::discard_response();
                psx_io::cd::acknowledge_irq(5);
                break false;
            }
            3 => {
                psx_io::cd::discard_response();
                psx_io::cd::acknowledge_irq(3);
                seen_ack = true;
            }
            2 if seen_ack => {
                psx_io::cd::discard_response();
                psx_io::cd::acknowledge_irq(2);
                break true;
            }
            other => {
                psx_io::cd::discard_response();
                psx_io::cd::acknowledge_irq(other);
            }
        }
    };
    psx_io::cd::restore_irq_output(saved);
    ok
}

/// Read `sectors` data sectors, optionally while CD-DA track 2 is playing.
///
/// This is the contention measurement. Reading data while audio streams forces
/// the single laser to leave the audio track and come back, and no emulator
/// reproduces the resulting hardware failure, so the console is the only
/// instrument that can characterise it. `0x9B` (playing) against `0x9C`
/// (stopped) isolates the cost: the same read path, the same sector count, the
/// only difference being whether audio was live.
fn cd_read_with_audio(playing: bool, sectors: u32) -> u16 {
    cd_clock_reset();
    // PAUSE, not STOP: both guarantee "no audio" for the quiet arm, but
    // STOP drops the motor, and every sample then forces a spin-up on a
    // mech record 0x9B just seek-thrashed. On the 2026-07-31 console run
    // that ground record 0x9C into minutes of apparent freeze (the
    // operator had to START-skip); with the motor kept spinning the
    // ReadN starts like any other.
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);

    if playing {
        if psx_io::cd::try_set_mode(psx_hw::cd::MODE_CDDA, CD_SPINS).is_none() {
            return CD_FAILED;
        }
        // Track 2 is the synthetic tone the disc build appends.
        if psx_io::cd::try_play_track(2, CD_SPINS).is_none() {
            return CD_FAILED;
        }
        let _ = psx_io::cd::try_unmute(CD_SPINS);
        // Let playback actually establish before the read fights it; measuring
        // during spin-up would confuse start-up cost with contention.
        cd_clock_reset();
        while timers::counter(timers::Timer::Timer1) < 1_000 {}
    }

    // Data mode for the read itself.
    if psx_io::cd::try_set_mode(0, CD_SPINS).is_none()
        || psx_io::cd::try_set_target_lba(CD_TEST_LBA, CD_SPINS).is_none()
        || psx_io::cd::try_start_reading(CD_SPINS).is_none()
    {
        cd_clock_reset();
        let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
        return CD_FAILED;
    }
    cd_clock_reset();
    let primed = cd_sector_timed();
    let elapsed = cd_timed(|| {
        let mut seen = 0;
        while seen < sectors {
            if !cd_sector_timed() {
                return false;
            }
            seen += 1;
        }
        true
    });
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
    let _ = psx_io::cd::try_mute(CD_SPINS);
    if primed {
        elapsed
    } else {
        CD_FAILED
    }
}

/// Time CD-DA playback from the Play command to the first position report.
fn cd_play_start() -> u16 {
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_STOP, &[]);
    if psx_io::cd::try_set_mode(psx_hw::cd::MODE_CDDA, CD_SPINS).is_none() {
        return CD_FAILED;
    }
    let elapsed = cd_timed(|| {
        if psx_io::cd::try_play_track(2, CD_SPINS).is_none() {
            return false;
        }
        // Position advancing is the first proof audio is actually streaming,
        // rather than the command merely having been accepted.
        let mut polls = 0;
        while polls < 64 {
            if let Some(response) = psx_io::cd::try_play_position(CD_SPINS) {
                if !response.is_empty() {
                    return true;
                }
            }
            polls += 1;
        }
        false
    });
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
    let _ = psx_io::cd::try_mute(CD_SPINS);
    elapsed
}

/// Time GetLocP while CD-DA is genuinely streaming.
fn cd_getlocp_during_playback() -> u16 {
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_STOP, &[]);
    if psx_io::cd::try_set_mode(psx_hw::cd::MODE_CDDA, CD_SPINS).is_none()
        || psx_io::cd::try_play_track(2, CD_SPINS).is_none()
    {
        return CD_FAILED;
    }
    cd_clock_reset();
    while timers::counter(timers::Timer::Timer1) < 1_000 {}
    let elapsed = cd_timed(|| {
        psx_io::cd::try_play_position(CD_SPINS).is_some_and(|response| !response.is_empty())
    });
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
    let _ = psx_io::cd::try_mute(CD_SPINS);
    elapsed
}

/// Wait for one streamed data sector under the same real-time deadline.
fn cd_sector_timed() -> bool {
    let mut spins = 0u32;
    loop {
        spins += 1;
        if timers::counter(timers::Timer::Timer1) >= CD_DEADLINE_HBLANKS
            || spins > bounds::scale(CD_WAIT_SPINS)
        {
            return false;
        }
        match psx_io::cd::irq_flag_value() {
            1 => {
                psx_io::cd::acknowledge_irq(1);
                return true;
            }
            0 => {}
            5 => {
                psx_io::cd::discard_response();
                psx_io::cd::acknowledge_irq(5);
                return false;
            }
            other => {
                psx_io::cd::discard_response();
                psx_io::cd::acknowledge_irq(other);
            }
        }
    }
}

/// Park the head at a known LBA so the next seek covers a known distance.
/// Without this every seek record would measure travel from wherever the
/// previous record happened to leave the head.
fn cd_park(lba: u32) -> bool {
    cd_clock_reset();
    psx_io::cd::try_set_target_lba(lba, CD_SPINS).is_some()
        && cd_command_until_complete_timed(psx_hw::cd::CMD_SEEKL, &[])
}

/// Seek `distance` sectors BACKWARD onto the parked origin, timing only the
/// seek. Parks beyond the target so the head approaches from the far side.
fn cd_seek_backward(distance: u32) -> u16 {
    let origin = CD_TEST_LBA + distance;
    if !cd_park(origin) {
        return CD_FAILED;
    }
    if psx_io::cd::try_set_target_lba(CD_TEST_LBA, CD_SPINS).is_none() {
        return CD_FAILED;
    }
    cd_timed(|| cd_command_until_complete_timed(psx_hw::cd::CMD_SEEKL, &[]))
}

/// Seek `distance` sectors away from the parked origin and time only the seek.
fn cd_seek_distance(distance: u32) -> u16 {
    if !cd_park(CD_TEST_LBA) {
        return CD_FAILED;
    }
    let target = CD_TEST_LBA + distance;
    if psx_io::cd::try_set_target_lba(target, CD_SPINS).is_none() {
        return CD_FAILED;
    }
    cd_timed(|| cd_command_until_complete_timed(psx_hw::cd::CMD_SEEKL, &[]))
}

/// Time `sectors` sequential sector arrivals once a read is already streaming,
/// which isolates sustained throughput from the initial seek and spin-up.
fn cd_read_throughput(double_speed: bool, sectors: u32) -> u16 {
    let mode = if double_speed {
        psx_hw::cd::MODE_DOUBLE_SPEED
    } else {
        0
    };
    if psx_io::cd::try_set_mode(mode, CD_SPINS).is_none()
        || !cd_park(CD_TEST_LBA)
        || psx_io::cd::try_set_target_lba(CD_TEST_LBA, CD_SPINS).is_none()
        || psx_io::cd::try_start_reading(CD_SPINS).is_none()
    {
        cd_clock_reset();
        let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
        return CD_FAILED;
    }
    // Discard the first sector: it carries the seek and spin-up settle.
    cd_clock_reset();
    let primed = cd_sector_timed();
    let elapsed = cd_timed(|| {
        let mut seen = 0;
        while seen < sectors {
            if !cd_sector_timed() {
                return false;
            }
            seen += 1;
        }
        true
    });
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
    if primed {
        elapsed
    } else {
        CD_FAILED
    }
}

// ---------------------------------------------------------------------------
// GPU fill-rate battery
//
// The emulator models NO GPU draw time at all: its linked-list DMA completes in
// ~0 guest cycles, so `ot_wait` is always ~0 and the per-vblank chart is blind
// to fill cost. There is therefore no data anywhere to build a model from, and
// the console is the only instrument that can supply it.
//
// Everything draws into off-screen VRAM (y >= 384) so the photographed capture
// and the QR pages are never touched. Sizes are held well under one Timer 2
// wrap (65,535 system cycles, ~1.9 ms): at roughly a pixel per cycle a 256x256
// fill would sit right on the boundary and alias a slow result into a fast one.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// MDEC battery
//
// Zero coverage before now. The decoder is a real bottleneck for any FMV path
// and its timing is entirely unmodelled here.
// ---------------------------------------------------------------------------

const MDEC_CMD: u32 = 0x1F80_1820;
const MDEC_CTRL: u32 = 0x1F80_1824;
/// Status bit 29 = command busy. NOT bit 31, which is the data-out FIFO flag
/// and stays set while the decoder is idle: polling it never returns.
const MDEC_BUSY: u32 = 0x2000_0000;
/// Status bit 31 = reset request, when written to the control register.
const MDEC_RESET: u32 = 0x8000_0000;
/// Status bit 31 (on read) = data-out FIFO empty.
const MDEC_OUT_FIFO_EMPTY: u32 = 0x8000_0000;

/// Time an MDEC command's status settle.
///
/// The payload word count is part of the command's contract, not a free
/// parameter: MDEC holds the busy bit set until it has consumed exactly the
/// number of words the command implies. Set-quant-table (luma only) wants 16
/// words, luma+chroma 32, and set-scale-table 32. Feeding fewer leaves the
/// decoder waiting forever and every record reads as a timeout.
fn timed_mdec(command: u32, payload_words: u16) -> u16 {
    unsafe {
        // Reset: bit 31 aborts and clears the FIFOs, so each record starts
        // from the same state rather than inheriting the previous one's.
        psx_io::write_u32(MDEC_CTRL, MDEC_RESET);
    }
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    unsafe {
        psx_io::write_u32(MDEC_CMD, command);
        let mut word = 0u16;
        while word < payload_words {
            psx_io::write_u32(MDEC_CMD, 0x0000_0000);
            word += 1;
        }
    }
    let mut guard = 0u32;
    while unsafe { psx_io::read_u32(MDEC_CTRL) } & MDEC_BUSY != 0 && guard < 200_000 {
        guard += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    if guard < 200_000 {
        elapsed
    } else {
        0xFFFF
    }
}

/// One minimal MDEC block, packed as the two halfwords the decoder consumes.
///
/// Low halfword is the block head: bits 15..10 the quantisation scale, bits
/// 9..0 a signed 10-bit DC coefficient. High halfword is `0xFE00`, whose
/// run-length field of 63 overflows the coefficient index and so terminates
/// the block. Two halfwords is therefore a complete, valid block.
const MDEC_BLOCK_DC_EOB: u32 = 0xFE00_2040;
/// Blocks per colour macroblock: Cr, Cb, then four luma.
const MDEC_BLOCKS_PER_MACROBLOCK: u16 = 6;
/// Output words per 24-bit macroblock: 16x16 pixels at 3 bytes each.
const MDEC_WORDS_PER_MACROBLOCK: u32 = 16 * 16 * 3 / 4;

/// Decode `macroblocks` colour macroblocks and time decode-to-drained.
///
/// Draining is part of the measurement, not overhead: MDEC holds BUSY set until
/// its output has been read, and decode-to-drained is the interval that governs
/// FMV throughput. It is also the only way the command completes at all.
fn timed_mdec_decode(macroblocks: u16) -> u16 {
    mdec_load_tables();
    let words = macroblocks * MDEC_BLOCKS_PER_MACROBLOCK;
    // Command 1, 24-bit output depth (bits 28..27 = 2), parameter words in the
    // low half.
    let command = 0x2000_0000 | (2 << 27) | u32::from(words);
    let expected_out = u32::from(macroblocks) * MDEC_WORDS_PER_MACROBLOCK;

    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    unsafe {
        psx_io::write_u32(MDEC_CMD, command);
        let mut word = 0u16;
        while word < words {
            psx_io::write_u32(MDEC_CMD, MDEC_BLOCK_DC_EOB);
            word += 1;
        }
    }

    // Stop on EITHER termination, because the two disagree.
    //
    // Real hardware clears BUSY once the decode finishes and its output has
    // been read. PSoXide only re-evaluates BUSY when the last parameter word
    // arrives, and output is already queued by then, so BUSY stays set forever
    // and a busy-only wait never returns. Draining the expected pixel count is
    // what terminates in emulation; the BUSY check is what terminates, sooner,
    // on silicon. Accepting both makes the same record valid on each.
    let mut guard = 0u32;
    let mut drained = 0u32;
    while guard < 400_000 && drained < expected_out {
        let status = unsafe { psx_io::read_u32(MDEC_CTRL) };
        if status & MDEC_OUT_FIFO_EMPTY == 0 {
            let _ = unsafe { psx_io::read_u32(MDEC_CMD) };
            drained += 1;
            continue;
        }
        if status & MDEC_BUSY == 0 {
            break;
        }
        guard += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    // A full macroblock of pixels is the floor for calling this a decode rather
    // than a timeout wearing the costume of a fast one.
    if drained >= MDEC_WORDS_PER_MACROBLOCK {
        elapsed
    } else {
        0xFFFF
    }
}

/// Upload a flat quant table and a scale table so a decode has defined inputs
/// regardless of which record ran before it.
fn mdec_load_tables() {
    unsafe {
        psx_io::write_u32(MDEC_CTRL, MDEC_RESET);
        psx_io::write_u32(MDEC_CMD, 0x4000_0001);
        let mut word = 0;
        while word < 32 {
            psx_io::write_u32(MDEC_CMD, 0x1010_1010);
            word += 1;
        }
        psx_io::write_u32(MDEC_CMD, 0x6000_0000);
        word = 0;
        while word < 32 {
            psx_io::write_u32(MDEC_CMD, 0x0000_1000);
            word += 1;
        }
    }
}

/// Time a reset returning the decoder to idle.
fn timed_mdec_reset_settle() -> u16 {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    unsafe {
        psx_io::write_u32(MDEC_CTRL, MDEC_RESET);
    }
    let mut guard = 0u32;
    while unsafe { psx_io::read_u32(MDEC_CTRL) } & MDEC_BUSY != 0 && guard < 200_000 {
        guard += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    if guard < 200_000 {
        elapsed
    } else {
        0xFFFF
    }
}

/// MDEC status word after a reset, as a raw observation.
fn mdec_status() -> u32 {
    unsafe {
        psx_io::write_u32(MDEC_CTRL, MDEC_RESET);
        psx_io::read_u32(MDEC_CTRL)
    }
}

// ---------------------------------------------------------------------------
// SIO / controller-port battery
//
// The SCPH-1200 pad needed a setup delay after select, and finding that cost a
// whole session of guesswork. These records make the port's handshake timing a
// standing measurement instead.
// ---------------------------------------------------------------------------

/// Time a full pad poll at a given setup/inter-byte spin configuration. The
/// spread across configurations is the useful signal: it shows how much of the
/// transfer is fixed cost and how much is the pacing the pad demands.
fn timed_pad_poll(setup: u32, interbyte: u32) -> u16 {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let poll = psx_pad::poll_port1_diagnostics(setup, interbyte);
    let elapsed = timers::counter(timers::Timer::Timer2);
    if probe_column_ok(poll) {
        elapsed
    } else {
        // A pad that did not answer is not a timing measurement. Keep the
        // sentinel distinct so an absent controller cannot look like a fast one.
        0xFFFF
    }
}

fn gte_snapshot_hash() -> u32 {
    let mut hash = 0x9E37_79B9;
    hash = mix32(hash, read_data!(7));
    hash = mix32(hash, read_data!(8));
    hash = mix32(hash, read_data!(9));
    hash = mix32(hash, read_data!(10));
    hash = mix32(hash, read_data!(11));
    hash = mix32(hash, read_data!(12));
    hash = mix32(hash, read_data!(13));
    hash = mix32(hash, read_data!(14));
    hash = mix32(hash, read_data!(16));
    hash = mix32(hash, read_data!(17));
    hash = mix32(hash, read_data!(18));
    hash = mix32(hash, read_data!(19));
    hash = mix32(hash, read_data!(20));
    hash = mix32(hash, read_data!(21));
    hash = mix32(hash, read_data!(22));
    hash = mix32(hash, read_data!(24));
    hash = mix32(hash, read_data!(25));
    hash = mix32(hash, read_data!(26));
    hash = mix32(hash, read_data!(27));
    mix32(hash, read_control!(31))
}

fn test_volatile_memory() -> TestResult {
    static mut BUF: [u8; 12] = [0; 12];
    unsafe {
        let base = (&raw mut BUF) as *mut u8;
        ptr::write_volatile(base.add(0), 0xA5);
        ptr::write_volatile(base.add(2) as *mut u16, 0xBEEF);
        ptr::write_volatile(base.add(4) as *mut u32, 0x1234_5678);
        let observed = (ptr::read_volatile(base.add(0)) as u32)
            | ((ptr::read_volatile(base.add(2) as *const u16) as u32) << 8)
            | (ptr::read_volatile(base.add(4) as *const u32) & 0xFF00_0000);
        expect_eq(0x12BE_EFA5, observed, "volatile")
    }
}

fn test_kseg1_alias() -> TestResult {
    static mut WORD: u32 = 0;
    unsafe {
        let cached = &raw mut WORD as u32;
        let physical = cached & 0x001F_FFFF;
        let uncached = (0xA000_0000 | physical) as *mut u32;
        ptr::write_volatile(uncached, 0xCAFE_F00D);
        let observed = ptr::read_volatile(uncached);
        expect_eq(0xCAFE_F00D, observed, "kseg1")
    }
}

fn test_irq_mask_roundtrip() -> TestResult {
    let old_mask = irq::mask();
    let pattern = 0x0555;
    irq::set_mask(pattern);
    let readback = irq::mask() & 0x07FF;
    irq::set_mask(old_mask);
    expect_eq(pattern, readback, "i_mask")
}

fn test_irq_gpu_ack_path() -> TestResult {
    let old_mask = irq::mask();
    irq::set_mask(old_mask & !(1 << psx_hw::irq::source::GPU));
    irq::acknowledge(1 << psx_hw::irq::source::GPU);
    gpu_io::write_display_control(0x0200_0000);

    gpu_io::write_command(0x1F00_0000);
    let raised_gpu = gpu_io::status().bits() & (1 << 24) != 0;
    let raised_irq = irq::pending() & (1 << psx_hw::irq::source::GPU) != 0;

    gpu_io::write_display_control(0x0200_0000);
    irq::acknowledge(1 << psx_hw::irq::source::GPU);
    let cleared_gpu = gpu_io::status().bits() & (1 << 24) == 0;
    let cleared_irq = irq::pending() & (1 << psx_hw::irq::source::GPU) == 0;
    irq::set_mask(old_mask);

    let observed = (raised_gpu as u32)
        | ((raised_irq as u32) << 1)
        | ((cleared_gpu as u32) << 2)
        | ((cleared_irq as u32) << 3);
    // Racy on silicon: both the GPUSTAT.24 and the I_STAT observations
    // race the GPU command FIFO and flip run-to-run. Report rather than
    // fail until FIFO latency is modelled (gated on CPU cycle accuracy).
    TestResult::info(0x0F, observed, "racy fifo")
}

/// One bounded OTC kick: force-stop the channel, arm it with `chcr`,
/// wait with a spin cap, then verify and scrub the chain. Returns
/// (completed, chain_correct). Never hangs, even on a wedging channel.
fn otc_kick_bounded(ptr: *mut u32, words: u16, chcr: u32, delay: bool) -> (bool, bool) {
    // Clear a possibly-wedged START from the previous variant; on
    // silicon clearing bit 24 requests an abort.
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_control(dma::Channel::OrderingTableClear, 0);
    }
    for _ in 0..1_000u32 {
        unsafe { core::ptr::read_volatile(psx_hw::dma::DPCR as *const u32) };
    }
    dma::enable_channel(dma::Channel::OrderingTableClear);
    if delay {
        for _ in 0..10_000u32 {
            unsafe { core::ptr::read_volatile(psx_hw::dma::DPCR as *const u32) };
        }
    }
    let last = unsafe { ptr.add(words as usize - 1) };
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_address(dma::Channel::OrderingTableClear, last as u32);
        dma::raw::set_size(dma::Channel::OrderingTableClear, dma::size_words(words));
        dma::raw::set_control(dma::Channel::OrderingTableClear, chcr);
    }
    let mut spins = 0u32;
    while dma::is_busy(dma::Channel::OrderingTableClear) && spins < 200_000 {
        spins += 1;
    }
    let done = !dma::is_busy(dma::Channel::OrderingTableClear);
    let mut ok = unsafe { ptr::read_volatile(ptr) } == 0x00FF_FFFF;
    for i in 1..words as usize {
        let expected = unsafe { ptr.add(i - 1) } as u32 & 0x00FF_FFFF;
        ok &= unsafe { ptr::read_volatile(ptr.add(i)) } == expected;
    }
    for i in 0..words as usize {
        unsafe { core::ptr::write_volatile(ptr.add(i), 0xDEAD_BEEF) };
    }
    (done, ok)
}

fn test_dma_otc_clear() -> TestResult {
    static mut OT: [u32; 8] = [0; 8];
    // The SDK helper's unbounded wait hung this test on real silicon
    // (the same channel-6 wedge that froze the engine's boot), so the
    // kick is open-coded with a spin cap per CHCR variant and the
    // channel is force-stopped between attempts. `observed` packs
    // (completed, chain-correct) pairs per variant, low bits first:
    //   V0 canonical trigger+start (0x11000002)
    //   V1 start only, no trigger  (0x01000002)
    //   V2 canonical after a settle delay past the DPCR enable
    // On a healthy machine every pair reads 11; a wedge reports the
    // exact surviving variant instead of hanging the battery.
    let ptr = (&raw mut OT) as *mut u32;
    let variants: [(u32, bool); 3] = [
        (0x1100_0002, false),
        (0x0100_0002, false),
        (0x1100_0002, true),
    ];
    let mut observed = 0u32;
    for (index, (chcr, delay)) in variants.iter().enumerate() {
        let (done, ok) = otc_kick_bounded(ptr, 8, *chcr, *delay);
        observed |= (done as u32) << (index * 2);
        observed |= (ok as u32) << (index * 2 + 1);
    }
    // V1 (start only, no trigger) does not complete on this console --
    // stable across the v1.7 and v1.16 burns -- and the emulator agrees.
    // SCPH-9902 measured 0x3F, so keep that on record as per-model
    // variance; the reference silicon for this suite reads 0x33.
    expect_eq(0x33, observed, "otc variants (done,ok) pairs")
}

fn test_dma_channel_register_roundtrip() -> TestResult {
    let ch = dma::Channel::Expansion;
    let base = ch.register_base();
    unsafe {
        let old_madr = psx_io::read_u32(base);
        let old_bcr = psx_io::read_u32(base + 4);
        let old_chcr = psx_io::read_u32(base + 8);

        psx_io::write_u32(base, 0x0012_3400);
        psx_io::write_u32(base + 4, 0x0002_0034);
        psx_io::write_u32(
            base + 8,
            psx_hw::dma::CHCR_TO_DEVICE
                | psx_hw::dma::CHCR_SYNC_BLOCK
                | psx_hw::dma::CHCR_CHOPPING_ENABLE,
        );

        let mut observed = 0u32;
        if psx_io::read_u32(base) == 0x0012_3400 {
            observed |= 1 << 0;
        }
        if psx_io::read_u32(base + 4) == 0x0002_0034 {
            observed |= 1 << 1;
        }
        if psx_io::read_u32(base + 8)
            == (psx_hw::dma::CHCR_TO_DEVICE
                | psx_hw::dma::CHCR_SYNC_BLOCK
                | psx_hw::dma::CHCR_CHOPPING_ENABLE)
        {
            observed |= 1 << 2;
        }

        psx_io::write_u32(base, old_madr);
        psx_io::write_u32(base + 4, old_bcr);
        psx_io::write_u32(base + 8, old_chcr);

        expect_eq(0x07, observed, "dma regs")
    }
}

fn test_dma_dpcr_roundtrip() -> TestResult {
    unsafe {
        let old = psx_io::read_u32(psx_hw::dma::DPCR);
        psx_io::write_u32(psx_hw::dma::DPCR, 0x0765_4321);
        let readback = psx_io::read_u32(psx_hw::dma::DPCR) & 0x0FFF_FFFF;
        psx_io::write_u32(psx_hw::dma::DPCR, old);
        expect_eq(0x0765_4321, readback, "dpcr")
    }
}

fn test_timer2_increments() -> TestResult {
    timers::set_mode(timers::Timer::Timer2, 0x0000);
    timers::set_counter(timers::Timer::Timer2, 0);
    let start = timers::counter(timers::Timer::Timer2);
    spin(4096);
    let end = timers::counter(timers::Timer::Timer2);
    if end != start {
        TestResult::pass(1, end.wrapping_sub(start) as u32, "delta")
    } else {
        TestResult::fail(1, 0, "no tick")
    }
}

fn test_timer1_scanline() -> TestResult {
    // Configure once, then let the counter run: Timer 1 in HBlank mode must
    // advance on its own. (The old form read gpu::scanline_counter(), which
    // reconfigures Timer 1 before every read and so always returned ~0; the
    // `<= 340` range check passed vacuously.)
    // Mode: bit 0 sync enable, bits 1-2 reset at VBlank, bit 8 HBlank clock.
    timers::set_mode(timers::Timer::Timer1, 0x0103);
    let start = timers::counter(timers::Timer::Timer1);
    spin(65_536);
    let end = timers::counter(timers::Timer::Timer1);
    if end != start {
        TestResult::pass(1, end.wrapping_sub(start) as u32, "delta")
    } else {
        TestResult::fail(1, 0, "no tick")
    }
}

fn test_gpu_status() -> TestResult {
    let stat = gpu_io::status();
    let raw = stat.bits();
    let mut observed = 0u32;
    if stat.horizontal_resolution() == 320 {
        observed |= 1;
    }
    if stat.vertical_resolution() == 240 {
        observed |= 2;
    }
    if ((raw >> 29) & 0b11) == 2 {
        observed |= 4;
    }
    if raw & (1 << 26) != 0 {
        observed |= 8;
    }
    if raw & (1 << 28) != 0 {
        observed |= 16;
    }
    expect_eq(0x1F, observed, "gpustat")
}

fn test_gpu_irq_ack() -> TestResult {
    gpu_io::write_command(0x1F00_0000);
    let raised = gpu_io::status().bits() & (1 << 24) != 0;
    gpu_io::write_display_control(0x0200_0000);
    let cleared = gpu_io::status().bits() & (1 << 24) == 0;
    let observed = (raised as u32) | ((cleared as u32) << 1);
    // Racy on silicon: GPUSTAT.24 set/clear races the GPU command FIFO
    // (flipped FAIL->PASS between burns). Report until FIFO latency is
    // modelled (gated on CPU cycle accuracy).
    TestResult::info(0x3, observed, "racy fifo")
}

/// Exploratory probe for the GPU IRQ1 failures: measure how many
/// GPUSTAT reads it takes for bit 24 to reflect a GP0(0x1F) set and a
/// GP1(0x02) clear. GP0 commands settle asynchronously through the
/// GPU's command FIFO on real hardware, while GP1 is the immediate
/// control port -- so a zero-delay readback (as in `test_gpu_irq_ack`
/// and `test_irq_gpu_ack_path`) can race the FIFO. A synchronous
/// emulator settles both in zero extra reads, so this reports
/// 0x00000000 on PSoXide. A non-zero high halfword (set latency) on
/// real silicon confirms the race; a saturated 0xFFFF means the flag
/// never settled within the poll budget. INFO only: compare the packed
/// `(set_polls << 16) | clr_polls` across real PS1 / DuckStation / Redux.
fn test_gpu_irq_latency_probe() -> TestResult {
    const MAX_POLLS: u32 = 0xFFFF;

    // Begin from a known-clear flag (GP1 is the immediate control port).
    gpu_io::write_display_control(0x0200_0000);

    // Latency for GP0(0x1F) to raise GPUSTAT.24.
    gpu_io::write_command(0x1F00_0000);
    let mut set_polls = 0u32;
    while set_polls < MAX_POLLS && gpu_io::status().bits() & (1 << 24) == 0 {
        set_polls = set_polls.wrapping_add(1);
    }

    // Latency for GP1(0x02) to clear it again, now that the FIFO has
    // drained the 0x1F.
    gpu_io::write_display_control(0x0200_0000);
    let mut clr_polls = 0u32;
    while clr_polls < MAX_POLLS && gpu_io::status().bits() & (1 << 24) != 0 {
        clr_polls = clr_polls.wrapping_add(1);
    }

    let observed = (set_polls.min(0xFFFF) << 16) | clr_polls.min(0xFFFF);
    TestResult::info(0, observed, "set<<16|clr")
}

/// Companion measurement for the DMA-direction latch failure. Writes
/// GP1(0x04 | dir) for dir 0..3 and reports the raw GPUSTAT bits 29-30
/// each one reads back, packed 2 bits per direction
/// (`r0 | r1<<2 | r2<<4 | r3<<6`). PSoXide echoes every direction, so it
/// reads 0xE4 (11 10 01 00); whatever silicon returns shows which
/// directions don't latch and what they read instead. INFO only.
fn test_gpu_dma_direction_readback() -> TestResult {
    let mut observed = 0u32;
    for dir in 0..4u32 {
        gpu_io::write_display_control(0x0400_0000 | dir);
        let read = (gpu_io::status().bits() >> 29) & 0b11;
        observed |= read << (dir * 2);
    }
    gpu_io::write_display_control(0x0400_0000 | 2);
    TestResult::info(0xE4, observed, "dir 3..0")
}

fn test_gpu_primitive_packet_encoding() -> TestResult {
    let tri = prim::TriFlat::new([(1, 2), (3, 4), (5, 6)], 7, 8, 9);
    let line = prim::LineMono::new(-1, -2, 3, 4, 5, 6, 7);
    let rect = prim::RectFlat::new(8, 9, 10, 11, 12, 13, 14);
    let mut observed = 0u32;

    if prim::TriFlat::WORDS == 4 && core::mem::size_of::<prim::TriFlat>() == 20 {
        observed |= 1 << 0;
    }
    if tri.tag == 0 && tri.color_cmd == 0x2009_0807 && tri.v2 == 0x0006_0005 {
        observed |= 1 << 1;
    }
    if prim::LineMono::WORDS == 3
        && line.color_cmd == 0x4007_0605
        && line.v0 == 0xFFFE_FFFF
        && line.v1 == 0x0004_0003
    {
        observed |= 1 << 2;
    }
    if prim::RectFlat::WORDS == 3
        && rect.color_cmd == 0x600E_0D0C
        && rect.xy == 0x0009_0008
        && rect.wh == 0x000B_000A
    {
        observed |= 1 << 3;
    }

    expect_eq(0x0F, observed, "packets")
}

fn test_gte_register_roundtrip() -> TestResult {
    write_data!(0, 0x2222_1111);
    write_control!(31, 0);
    let data = read_data!(0);
    let flag = read_control!(31);
    let observed = ((data == 0x2222_1111) as u32) | (((flag & 0x7FFF_F000) == 0) as u32) << 1;
    expect_eq(0x3, observed, "gte regs")
}

fn test_gte_projection_center() -> TestResult {
    gte_scene::set_screen_offset(160 << 16, 120 << 16);
    gte_scene::set_projection_plane(256);
    gte_scene::load_rotation(&Mat3I16::IDENTITY);
    gte_scene::load_translation(Vec3I32::new(0, 0, 0x1000));
    let p = gte_scene::project_vertex(Vec3I16::new(0, 0, 0));
    let observed = ((p.sx as u16 as u32) << 16) | p.sy as u16 as u32;
    expect_eq((160u32 << 16) | 120, observed, "rtps")
}

fn test_gte_all_ops_digest() -> TestResult {
    let mut observed = 0u32;

    seed_gte_state();
    unsafe { gte_ops::project_single() };
    if gte_flag_master_clear() && read_data!(14) != 0 {
        observed |= 1 << 0;
    }

    seed_gte_state();
    unsafe { gte_ops::project_triple() };
    if gte_flag_master_clear() && read_data!(12) != read_data!(14) {
        observed |= 1 << 1;
    }

    seed_gte_state();
    unsafe { gte_ops::screen_winding() };
    if gte_flag_master_clear() {
        observed |= 1 << 2;
    }

    seed_gte_state();
    unsafe { gte_ops::outer_product() };
    if gte_flag_master_clear() {
        observed |= 1 << 3;
    }

    seed_gte_state();
    unsafe { gte_ops::average_z3() };
    if gte_flag_master_clear() && read_data!(7) != 0 {
        observed |= 1 << 4;
    }

    seed_gte_state();
    unsafe { gte_ops::average_z4() };
    if gte_flag_master_clear() && read_data!(7) != 0 {
        observed |= 1 << 5;
    }

    seed_gte_state();
    unsafe { gte_ops::square() };
    if gte_flag_master_clear() && read_data!(25) != 0 {
        observed |= 1 << 6;
    }

    seed_gte_state();
    unsafe { gte_ops::light_color_depth_single() };
    if gte_flag_master_clear() {
        observed |= 1 << 7;
    }

    seed_gte_state();
    unsafe { gte_ops::light_color_single() };
    if gte_flag_master_clear() {
        observed |= 1 << 8;
    }

    seed_gte_state();
    unsafe { gte_ops::light_single() };
    if gte_flag_master_clear() {
        observed |= 1 << 9;
    }

    seed_gte_state();
    unsafe { gte_ops::light_color_depth_triple() };
    if gte_flag_master_clear() {
        observed |= 1 << 10;
    }

    seed_gte_state();
    unsafe { gte_ops::light_triple() };
    if gte_flag_master_clear() {
        observed |= 1 << 11;
    }

    seed_gte_state();
    unsafe { gte_ops::light_color_triple() };
    if gte_flag_master_clear() {
        observed |= 1 << 12;
    }

    seed_gte_state();
    unsafe { gte_ops::depth_cue_single() };
    if gte_flag_master_clear() {
        observed |= 1 << 13;
    }

    seed_gte_state();
    unsafe { gte_ops::depth_cue_triple() };
    if gte_flag_master_clear() {
        observed |= 1 << 14;
    }

    seed_gte_state();
    unsafe { gte_ops::interpolate_far_color() };
    if gte_flag_master_clear() {
        observed |= 1 << 15;
    }

    seed_gte_state();
    unsafe { gte_ops::depth_cue_light() };
    if gte_flag_master_clear() {
        observed |= 1 << 16;
    }

    seed_gte_state();
    unsafe { gte_ops::color_color() };
    if gte_flag_master_clear() {
        observed |= 1 << 17;
    }

    seed_gte_state();
    unsafe { gte_ops::color_depth_cue() };
    if gte_flag_master_clear() {
        observed |= 1 << 18;
    }

    seed_gte_state();
    unsafe { gte_ops::scale_vector() };
    if gte_flag_master_clear() {
        observed |= 1 << 19;
    }

    seed_gte_state();
    unsafe { gte_ops::scale_vector_accumulate() };
    if gte_flag_master_clear() {
        observed |= 1 << 20;
    }

    seed_gte_state();
    unsafe { gte_ops::rotate_translate_v0() };
    if gte_flag_master_clear() && read_data!(27) != 0 {
        observed |= 1 << 21;
    }

    expect_eq(0x003F_FFFF, observed, "gte ops")
}

/// Burn ~N cycles (literal NOP slide) for functional GTE settling and the
/// dedicated hazard sweeps below.
macro_rules! gte_nops {
    (0) => {};
    ($n:literal) => {
        #[cfg(target_arch = "mips")]
        unsafe {
            core::arch::asm!(
                concat!(".rept ", $n, "\nnop\n.endr"),
                options(nostack, nomem, preserves_flags)
            );
        }
    };
}

/// Seed the positive-winding NCLIP triangle: SXY0=(0,0), SXY1=(10,0),
/// SXY2=(0,10). The cross product is +100, so a faithful GTE leaves
/// MAC0 = 0x64. Shared by every NCLIP MAC0 probe below.
fn seed_nclip_pos_triangle() {
    write_control!(31, 0);
    write_data!(12, pack_gte_xy(0, 0));
    write_data!(13, pack_gte_xy(10, 0));
    write_data!(14, pack_gte_xy(0, 10));
}

/// Probe GTE result-read latency for NCLIP. Runs the positive-winding
/// triangle, burns a fixed `nop` delay, then reports MAC0. On hardware:
/// if MAC0 climbs from 0 toward 100 as the delay grows, the divergence
/// is a read-too-soon hazard (the emulator completes the op instantly);
/// if it stays 0, the GTE genuinely computes a different value for this
/// winding (the negative winding already reads -100 correctly with no
/// delay, which a uniform read-latency could not produce). INFO only;
/// no delay is emitted on host, where the software GTE is instantaneous.
macro_rules! nclip_mac0_delay_test {
    ($name:ident, $delay:literal, $label:literal) => {
        fn $name() -> TestResult {
            seed_nclip_pos_triangle();
            unsafe { gte_ops::screen_winding() };
            #[cfg(target_arch = "mips")]
            unsafe {
                core::arch::asm!($delay, options(nostack, nomem, preserves_flags));
            }
            TestResult::info(100, read_data!(24), $label)
        }
    };
}
nclip_mac0_delay_test!(
    test_gte_nclip_mac0_nop8,
    ".rept 8\nnop\n.endr",
    "nclip mac0 +8nop"
);
nclip_mac0_delay_test!(
    test_gte_nclip_mac0_nop16,
    ".rept 16\nnop\n.endr",
    "nclip mac0 +16nop"
);

fn test_gte_nclip_mac0() -> TestResult {
    seed_nclip_pos_triangle();
    unsafe { gte_ops::screen_winding() };
    // This is the functional arithmetic check, not the result-latency
    // probe. Let MAC0 settle so real silicon's immediately-next-read hazard
    // does not turn correct NCLIP arithmetic into a headline failure.
    gte_nops!(64);
    let positive = read_data!(24) as i32;
    let positive_flag_clear = gte_flag_master_clear();

    write_control!(31, 0);
    write_data!(12, pack_gte_xy(0, 0));
    write_data!(13, pack_gte_xy(0, 10));
    write_data!(14, pack_gte_xy(10, 0));
    unsafe { gte_ops::screen_winding() };
    gte_nops!(64);
    let negative = read_data!(24) as i32;
    let negative_flag_clear = gte_flag_master_clear();

    let observed = ((positive == 100) as u32)
        | (((negative == -100) as u32) << 1)
        | ((positive_flag_clear as u32) << 2)
        | ((negative_flag_clear as u32) << 3);
    // Bit 0 stays clear on silicon: the positive-winding triangle reads
    // MAC0 = 0 (see the companion nclip-mac0-value INFO case), stable
    // across the v1.5, v1.7 and v1.16 burns, and the emulator reproduces
    // it. 0x0F was the pre-measurement aspiration, not a measured value.
    expect_eq(0x0E, observed, "nclip")
}

/// Companion measurement for the NCLIP winding failure. Runs the
/// positive-winding triangle the pass/fail test expects to yield
/// MAC0 = +100 and reports the raw MAC0 instead of a verdict.
/// `psx-gte-core` computes the symmetric cross product, so PSoXide
/// reads 0x00000064 (= 100); whatever real silicon returns here is the
/// exact value we must replicate in the GTE core. INFO only -- expected
/// shown as 100 for reference.
fn test_gte_nclip_mac0_value() -> TestResult {
    seed_nclip_pos_triangle();
    unsafe { gte_ops::screen_winding() };
    let mac0 = read_data!(24);
    TestResult::info(100, mac0, "nclip mac0")
}

// Inputs-survive check for the NCLIP MAC0 divergence. The positive
// triangle reads MAC0 = 0 on silicon while the negative one reads -100
// correctly (same code path, no delay), which a uniform read-latency
// could not produce -- so the first thing to rule out is whether the
// SXY0/1/2 the writes are supposed to leave in the GTE actually land.
// These read each input register straight back after seeding (before
// NCLIP). PSoXide returns exactly what was written (regs 12/13/14 are
// direct, not the SXY FIFO); a mismatch on hardware means the mtc2
// writes -- not NCLIP itself -- are the bug. INFO only.
fn test_gte_nclip_in_sxy0() -> TestResult {
    seed_nclip_pos_triangle();
    TestResult::info(0x0000_0000, read_data!(12), "nclip in sxy0")
}
fn test_gte_nclip_in_sxy1() -> TestResult {
    seed_nclip_pos_triangle();
    TestResult::info(0x0000_000A, read_data!(13), "nclip in sxy1")
}
fn test_gte_nclip_in_sxy2() -> TestResult {
    seed_nclip_pos_triangle();
    TestResult::info(0x000A_0000, read_data!(14), "nclip in sxy2")
}

/// Companion measurement for the GTE arithmetic surface. Projects a
/// fixed off-centre vertex with RTPS and reports the screen XY packed
/// as `(sx << 16) | sy`. With identity rotation, translation z=0x1000,
/// projection plane h=256 and screen offset (160,120), vertex
/// (256,128,0) projects to (176,128), so PSoXide reads 0x00B00080.
/// `test_gte_projection_center` only checks the on-axis centre; this
/// captures an exact off-axis value to replicate if the GTE digest
/// diverges. INFO only.
fn test_gte_rtps_offcenter_value() -> TestResult {
    gte_scene::set_screen_offset(160 << 16, 120 << 16);
    gte_scene::set_projection_plane(256);
    gte_scene::load_rotation(&Mat3I16::IDENTITY);
    gte_scene::load_translation(Vec3I32::new(0, 0, 0x1000));
    let p = gte_scene::project_vertex(Vec3I16::new(256, 128, 0));
    let observed = ((p.sx as u16 as u32) << 16) | p.sy as u16 as u32;
    TestResult::info(0x00B0_0080, observed, "rtps sx|sy")
}

// ---------------------------------------------------------------------------
// Real-scene RTPS conformance: register sets captured from a live
// cortex_ignition_v1 gameplay frame (probe_gameplay_gte_trace). Unlike the
// gentle synthetic cases above, these vertices project past the screen-coord
// clamp and into perspective-divide overflow (FLAG bit 17) -- the regime that
// drives the on-hardware vertex explosion and that the synthetic battery never
// exercises (it never sets FLAG). Expected values are PSoXide's outputs, so
// these pass in-emulator and FAIL on silicon exactly where the GTE diverges.
//
// The rotation matrix + projection (H, screen offset) are shared across the
// captured frame; only the input vertex V0 differs. Translation and depth-cue
// are zero, as in the scene.
fn seed_scene_rtps() {
    write_control!(31, 0); // clear FLAG
    write_control!(0, 0x0000_0f19); // R11,R12
    write_control!(1, 0x016e_fab4); // R13,R21
    write_control!(2, 0x0411_f098); // R22,R23
    write_control!(3, 0xfbb1_fae7); // R31,R32
    write_control!(4, 0xffff_f177); // R33
    write_control!(5, 0); // TRX
    write_control!(6, 0); // TRY
    write_control!(7, 0); // TRZ
    write_control!(24, 0x00a0_0000); // OFX
    write_control!(25, 0x0078_0000); // OFY
    write_control!(26, 0x0000_0140); // H (projection plane distance)
    write_control!(27, 0); // DQA
    write_control!(28, 0); // DQB
}

fn scene_rtps(vxy0: u32, vz0: u32) {
    seed_scene_rtps();
    write_data!(0, vxy0); // VXY0
    write_data!(1, vz0); // VZ0
    unsafe { gte_ops::project_single() };
}

// Sample A: FLAG=0x80066000 (divide overflow + SX/SY + SZ3 saturation); SXY2 clamps to (-1024,-1024).
fn test_gte_scene_rtps_a_sxy() -> TestResult {
    scene_rtps(0x0c3e_0000, 0x0000_0a4d);
    expect_eq(0xfc00_fc00, read_data!(14), "scene rtps A SXY2")
}
fn test_gte_scene_rtps_a_flag() -> TestResult {
    scene_rtps(0x0c3e_0000, 0x0000_0a4d);
    expect_eq(0x8006_6000, read_control!(31), "scene rtps A FLAG")
}
// Sample B: same FLAG; SX clamps high (+1023), SY clamps low (-1024).
fn test_gte_scene_rtps_b_sxy() -> TestResult {
    scene_rtps(0x0c3e_0529, 0x0000_08eb);
    expect_eq(0xfc00_03ff, read_data!(14), "scene rtps B SXY2")
}
// Sample C: FLAG=0x80002000 (SY-only saturation); SZ3 survives (no divide overflow).
fn test_gte_scene_rtps_c_sxy() -> TestResult {
    scene_rtps(0x0c3e_0526, 0xffff_f714);
    expect_eq(0xfc00_03b4, read_data!(14), "scene rtps C SXY2")
}
fn test_gte_scene_rtps_c_flag() -> TestResult {
    scene_rtps(0x0c3e_0526, 0xffff_f714);
    expect_eq(0x8000_2000, read_control!(31), "scene rtps C FLAG")
}
// Sample D: FLAG=0x80006000 (SX+SY saturation); negative-X vertex.
fn test_gte_scene_rtps_d_sxy() -> TestResult {
    scene_rtps(0x099c_f335, 0x0000_0000);
    expect_eq(0xfc00_fc00, read_data!(14), "scene rtps D SXY2")
}
fn test_gte_scene_rtps_d_flag() -> TestResult {
    scene_rtps(0x099c_f335, 0x0000_0000);
    expect_eq(0x8000_6000, read_control!(31), "scene rtps D FLAG")
}

// ---------------------------------------------------------------------------
// Comprehensive GTE conformance, part 2: the ops the scene runs BESIDES RTPS
// (inputs captured by probe_gameplay_gte_ops) plus a corner-case battery.
// Expecteds are PSoXide's own outputs (verified by the gte_expected_values
// host tool and the psx-gte-core mirror), so every case is green in-emulator
// and any FAIL on silicon is a real divergence. MVMVA is the prime vertex-
// explosion suspect; RTPT's divide-overflow SXY clamp is the other; NCLIP
// MAC0 is the missing-wall backface test, here with the scene's REAL coords.

/// Fold three GTE result words into one digest sensitive to each, so a single
/// dashboard line can cover a 3-component output (a transformed vector or
/// three projected screen XYs).
fn gte_tri_digest(a: u32, b: u32, c: u32) -> u32 {
    a ^ b.rotate_left(11) ^ c.rotate_left(22)
}

/// Rotation (RT) + real camera translation (TR) + projection (OFX/OFY/H) +
/// zeroed depth-cue (DQA/DQB) shared across the captured gameplay frame.
/// Unlike `seed_scene_rtps`, TR is nonzero here (world geometry).
fn seed_scene_xform() {
    write_control!(31, 0); // FLAG
    write_control!(0, 0x0000_0f19);
    write_control!(1, 0x016e_fab4);
    write_control!(2, 0x0411_f098);
    write_control!(3, 0xfbb1_fae7);
    write_control!(4, 0xffff_f177);
    write_control!(5, 0xffff_eabc); // TRX
    write_control!(6, 0xffff_fdb9); // TRY
    write_control!(7, 0x0000_35be); // TRZ
    write_control!(24, 0x00a0_0000); // OFX
    write_control!(25, 0x0078_0000); // OFY
    write_control!(26, 0x0000_0140); // H
    write_control!(27, 0); // DQA
    write_control!(28, 0); // DQB
}

// Scene MVMVA: RT*V0 + TR, sf=1 (skinning/world transform). FLAG never fires
// -- exact integer math -- so a divergence is a plain matrix-multiply miss.
fn scene_mvmva_digest(vxy0: u32, vz0: u32) -> u32 {
    seed_scene_xform();
    write_data!(0, vxy0);
    write_data!(1, vz0);
    unsafe { gte_ops::rotate_translate_v0() };
    gte_tri_digest(read_data!(25), read_data!(26), read_data!(27))
}
fn test_gte_scene_mvmva_a() -> TestResult {
    expect_eq(
        0xca74_6d65,
        scene_mvmva_digest(0x2040_0340, 0x0000_09c0),
        "scene mvmva A",
    )
}
fn test_gte_scene_mvmva_b() -> TestResult {
    expect_eq(
        0xd65a_09bf,
        scene_mvmva_digest(0x2040_0340, 0x0000_16c0),
        "scene mvmva B",
    )
}
fn test_gte_scene_mvmva_c() -> TestResult {
    expect_eq(
        0x511b_ee0c,
        scene_mvmva_digest(0x0b00_0340, 0x0000_23c0),
        "scene mvmva C",
    )
}
fn test_gte_scene_mvmva_d() -> TestResult {
    expect_eq(
        0x46ef_d743,
        scene_mvmva_digest(0x2040_09c0, 0x0000_09c0),
        "scene mvmva D",
    )
}

// Scene RTPT: projects 3 verts; divide-overflow cases clamp SXY to the
// screen-coord limits -- the exact regime behind missing/exploded triangles.
const RTPT_A: [u32; 6] = [
    0x0480_2d80,
    0x0000_2700,
    0x0480_2080,
    0x0000_2d80,
    0x0480_1a00,
    0x0000_2d80,
];
const RTPT_B: [u32; 6] = [
    0x0480_2700,
    0x0000_2d80,
    0x0480_2d80,
    0x0000_2d80,
    0x0480_2080,
    0x0000_3400,
];
const RTPT_C: [u32; 6] = [
    0x0480_1a00,
    0x0000_3400,
    0x0480_2700,
    0x0000_3400,
    0x0480_2d80,
    0x0000_3400,
];
const RTPT_D: [u32; 6] = [
    0x0680_3400,
    0x0000_2d80,
    0x0480_3400,
    0x0000_3400,
    0x0680_3a80,
    0x0000_2700,
];
const RTPT_E: [u32; 6] = [
    0x10c0_0000,
    0x0000_0d00,
    0x10c0_0680,
    0x0000_0d00,
    0x1dc0_0680,
    0x0000_0d00,
];
const RTPT_F: [u32; 6] = [
    0x1dc0_0000,
    0x0000_0d00,
    0x10c0_0680,
    0x0000_1380,
    0x10c0_0000,
    0x0000_1380,
];

// Exact RTPT input/result hazard characterisation. `RTPT_E` and `RTPT_F`
// produce visibly different projected triples, so F is a deterministic stale
// output/input poison for E. Each target sequence lives in one asm block: the
// compiler cannot hide an unsafe schedule by inserting a register move between
// the final VZ2 write and RTPT or between RTPT and its first MFC2.
const RTPT_HAZARD_TARGET: [u32; 6] = RTPT_E;
const RTPT_HAZARD_POISON: [u32; 6] = RTPT_F;

fn rtpt_result_digest(sxy0: u32, sxy1: u32, sxy2: u32, sz1: u32, sz2: u32, sz3: u32) -> u32 {
    gte_tri_digest(sxy0, sxy1, sxy2) ^ gte_tri_digest(sz1, sz2, sz3).rotate_left(7)
}

/// Leave a distinct, fully settled RTPT result and input triple in the GTE.
/// Both the later input-commit and result-read probes use this as their stale
/// value, so returning old state cannot accidentally compare equal.
fn prime_rtpt_hazard_state() {
    seed_scene_xform();
    gte_nops!(64);
    let v0_xy = RTPT_HAZARD_POISON[0];
    let v0_z = RTPT_HAZARD_POISON[1];
    let v1_xy = RTPT_HAZARD_POISON[2];
    let v1_z = RTPT_HAZARD_POISON[3];
    let v2_xy = RTPT_HAZARD_POISON[4];
    let v2_z = RTPT_HAZARD_POISON[5];
    #[cfg(target_arch = "mips")]
    unsafe {
        core::arch::asm!(
            ".word 0x48880000", // MTC2 $8,VXY0
            ".word 0x48890800", // MTC2 $9,VZ0
            ".word 0x488a1000", // MTC2 $10,VXY1
            ".word 0x488b1800", // MTC2 $11,VZ1
            ".word 0x488c2000", // MTC2 $12,VXY2
            ".word 0x488d2800", // MTC2 $13,VZ2
            ".word 0",
            ".word 0",
            ".word 0x4a080030", // RTPT
            in("$8") v0_xy,
            in("$9") v0_z,
            in("$10") v1_xy,
            in("$11") v1_z,
            in("$12") v2_xy,
            in("$13") v2_z,
            options(nostack, nomem, preserves_flags),
        );
    }
    #[cfg(not(target_arch = "mips"))]
    {
        mtc2!(0, v0_xy);
        mtc2!(1, v0_z);
        mtc2!(2, v1_xy);
        mtc2!(3, v1_z);
        mtc2!(4, v2_xy);
        mtc2!(5, v2_z);
        unsafe { gte_ops::rtpt() };
    }
    gte_nops!(64);
}

macro_rules! rtpt_input_gap_probe {
    ($name:ident, $gap:literal) => {
        fn $name() -> u32 {
            prime_rtpt_hazard_state();
            let v0_xy = RTPT_HAZARD_TARGET[0];
            let v0_z = RTPT_HAZARD_TARGET[1];
            let v1_xy = RTPT_HAZARD_TARGET[2];
            let v1_z = RTPT_HAZARD_TARGET[3];
            let v2_xy = RTPT_HAZARD_TARGET[4];
            let v2_z = RTPT_HAZARD_TARGET[5];
            #[cfg(target_arch = "mips")]
            unsafe {
                core::arch::asm!(
                    ".word 0x48880000",
                    ".word 0x48890800",
                    ".word 0x488a1000",
                    ".word 0x488b1800",
                    ".word 0x488c2000",
                    ".word 0x488d2800",
                    concat!(".rept ", $gap, "\nnop\n.endr"),
                    ".word 0x4a080030",
                    in("$8") v0_xy,
                    in("$9") v0_z,
                    in("$10") v1_xy,
                    in("$11") v1_z,
                    in("$12") v2_xy,
                    in("$13") v2_z,
                    options(nostack, nomem, preserves_flags),
                );
            }
            #[cfg(not(target_arch = "mips"))]
            {
                mtc2!(0, v0_xy);
                mtc2!(1, v0_z);
                mtc2!(2, v1_xy);
                mtc2!(3, v1_z);
                mtc2!(4, v2_xy);
                mtc2!(5, v2_z);
                unsafe { gte_ops::rtpt() };
            }
            gte_nops!(64);
            rtpt_result_digest(
                read_data!(12),
                read_data!(13),
                read_data!(14),
                read_data!(17),
                read_data!(18),
                read_data!(19),
            )
        }
    };
}

rtpt_input_gap_probe!(rtpt_input_digest_gap0, 0);
rtpt_input_gap_probe!(rtpt_input_digest_gap1, 1);
rtpt_input_gap_probe!(rtpt_input_digest_gap2, 2);
rtpt_input_gap_probe!(rtpt_input_digest_gap4, 4);
rtpt_input_gap_probe!(rtpt_input_digest_gap64, 64);

macro_rules! rtpt_read_gap_probe {
    ($name:ident, $gap:literal) => {
        fn $name() -> u32 {
            prime_rtpt_hazard_state();
            let mut v0_xy = RTPT_HAZARD_TARGET[0];
            let mut v0_z = RTPT_HAZARD_TARGET[1];
            let mut v1_xy = RTPT_HAZARD_TARGET[2];
            let mut v1_z = RTPT_HAZARD_TARGET[3];
            let mut v2_xy = RTPT_HAZARD_TARGET[4];
            let mut v2_z = RTPT_HAZARD_TARGET[5];
            #[cfg(target_arch = "mips")]
            unsafe {
                core::arch::asm!(
                    ".word 0x48880000",
                    ".word 0x48890800",
                    ".word 0x488a1000",
                    ".word 0x488b1800",
                    ".word 0x488c2000",
                    ".word 0x488d2800",
                    ".word 0",
                    ".word 0",
                    ".word 0x4a080030",
                    concat!(".rept ", $gap, "\nnop\n.endr"),
                    ".word 0x48086000", // MFC2 $8,SXY0
                    ".word 0x48096800", // MFC2 $9,SXY1
                    ".word 0x480a7000", // MFC2 $10,SXY2
                    ".word 0x480b8800", // MFC2 $11,SZ1
                    ".word 0x480c9000", // MFC2 $12,SZ2
                    ".word 0x480d9800", // MFC2 $13,SZ3
                    ".word 0",
                    inlateout("$8") v0_xy,
                    inlateout("$9") v0_z,
                    inlateout("$10") v1_xy,
                    inlateout("$11") v1_z,
                    inlateout("$12") v2_xy,
                    inlateout("$13") v2_z,
                    options(nostack, nomem, preserves_flags),
                );
            }
            #[cfg(not(target_arch = "mips"))]
            {
                mtc2!(0, v0_xy);
                mtc2!(1, v0_z);
                mtc2!(2, v1_xy);
                mtc2!(3, v1_z);
                mtc2!(4, v2_xy);
                mtc2!(5, v2_z);
                unsafe { gte_ops::rtpt() };
                v0_xy = mfc2!(12);
                v0_z = mfc2!(13);
                v1_xy = mfc2!(14);
                v1_z = mfc2!(17);
                v2_xy = mfc2!(18);
                v2_z = mfc2!(19);
            }
            rtpt_result_digest(v0_xy, v0_z, v1_xy, v1_z, v2_xy, v2_z)
        }
    };
}

rtpt_read_gap_probe!(rtpt_read_digest_gap0, 0);
rtpt_read_gap_probe!(rtpt_read_digest_gap8, 8);
rtpt_read_gap_probe!(rtpt_read_digest_gap16, 16);
rtpt_read_gap_probe!(rtpt_read_digest_gap24, 24);
rtpt_read_gap_probe!(rtpt_read_digest_gap64, 64);

macro_rules! rtpt_characterisation_test {
    ($name:ident, $reference:ident, $probe:ident, $note:literal) => {
        fn $name() -> TestResult {
            TestResult::info($reference(), $probe(), $note)
        }
    };
}

rtpt_characterisation_test!(
    test_rtpt_input_gap0,
    rtpt_input_digest_gap64,
    rtpt_input_digest_gap0,
    "rtpt input commit gap"
);
rtpt_characterisation_test!(
    test_rtpt_input_gap1,
    rtpt_input_digest_gap64,
    rtpt_input_digest_gap1,
    "rtpt input commit gap"
);
rtpt_characterisation_test!(
    test_rtpt_input_gap2,
    rtpt_input_digest_gap64,
    rtpt_input_digest_gap2,
    "rtpt input commit gap"
);
rtpt_characterisation_test!(
    test_rtpt_input_gap4,
    rtpt_input_digest_gap64,
    rtpt_input_digest_gap4,
    "rtpt input commit gap"
);
rtpt_characterisation_test!(
    test_rtpt_read_gap0,
    rtpt_read_digest_gap64,
    rtpt_read_digest_gap0,
    "rtpt result read gap"
);
rtpt_characterisation_test!(
    test_rtpt_read_gap8,
    rtpt_read_digest_gap64,
    rtpt_read_digest_gap8,
    "rtpt result read gap"
);
rtpt_characterisation_test!(
    test_rtpt_read_gap16,
    rtpt_read_digest_gap64,
    rtpt_read_digest_gap16,
    "rtpt result read gap"
);
rtpt_characterisation_test!(
    test_rtpt_read_gap24,
    rtpt_read_digest_gap64,
    rtpt_read_digest_gap24,
    "rtpt result read gap"
);

fn scene_rtpt(v: [u32; 6]) {
    seed_scene_xform();
    write_data!(0, v[0]);
    write_data!(1, v[1]);
    write_data!(2, v[2]);
    write_data!(3, v[3]);
    write_data!(4, v[4]);
    write_data!(5, v[5]);
    unsafe { gte_ops::project_triple() };
}
fn rtpt_sxy_digest(v: [u32; 6]) -> u32 {
    scene_rtpt(v);
    gte_tri_digest(read_data!(12), read_data!(13), read_data!(14))
}
fn test_gte_scene_rtpt_a_sxy() -> TestResult {
    expect_eq(0xfc1f_e61f, rtpt_sxy_digest(RTPT_A), "rtpt A sxy")
}
fn test_gte_scene_rtpt_b_sxy() -> TestResult {
    expect_eq(0xfbe0_066f, rtpt_sxy_digest(RTPT_B), "rtpt B sxy")
}
fn test_gte_scene_rtpt_c_sxy() -> TestResult {
    expect_eq(0x03d5_13df, rtpt_sxy_digest(RTPT_C), "rtpt C sxy")
}
fn test_gte_scene_rtpt_d_sxy() -> TestResult {
    expect_eq(0x0420_0420, rtpt_sxy_digest(RTPT_D), "rtpt D sxy")
}
fn test_gte_scene_rtpt_e_sxy() -> TestResult {
    expect_eq(0xaf36_5a05, rtpt_sxy_digest(RTPT_E), "rtpt E sxy")
}
fn test_gte_scene_rtpt_f_sxy() -> TestResult {
    expect_eq(0x7931_abae, rtpt_sxy_digest(RTPT_F), "rtpt F sxy")
}
fn test_gte_scene_rtpt_a_flag() -> TestResult {
    scene_rtpt(RTPT_A);
    expect_eq(0x8000_6000, read_control!(31), "rtpt A FLAG")
}
fn test_gte_scene_rtpt_b_flag() -> TestResult {
    scene_rtpt(RTPT_B);
    expect_eq(0x8006_6000, read_control!(31), "rtpt B FLAG")
}
fn test_gte_scene_rtpt_a_sz3() -> TestResult {
    scene_rtpt(RTPT_A);
    expect_eq(0x0000_02e9, read_data!(19), "rtpt A SZ3")
}
fn test_gte_scene_rtpt_e_sz3() -> TestResult {
    scene_rtpt(RTPT_E);
    expect_eq(0x0000_1fd9, read_data!(19), "rtpt E SZ3")
}

// Scene NCLIP: the REAL backface cross products the scene runs (large
// projected screen coords), unlike the synthetic (0,0)/(10,0)/(0,10). MAC0
// sign decides face culling -> the missing-wall divergence with real inputs.
fn scene_nclip_mac0(s0: u32, s1: u32, s2: u32) -> u32 {
    write_control!(31, 0);
    write_data!(12, s0);
    write_data!(13, s1);
    write_data!(14, s2);
    unsafe { gte_ops::screen_winding() };
    read_data!(24)
}
// Scene NCLIP results are history-dependent (the SCPH-9902 sweep produced
// 0x2764, 0xFFFFB964 and 0x7674 for the SAME scene depending on what
// preceded it), so a compiled expectation is a snapshot of one binary's GTE
// history and rots on every rebuild: v1.16 measured 0xFFFF752C on the same
// console the 0x2764 calibration came from. Characterisation, not
// conformance; hwtest-silicon diffs the raw values host-side.
fn test_gte_scene_nclip_a() -> TestResult {
    TestResult::info(
        0x0000_2764,
        scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
        "scene nclip A",
    )
}
fn test_gte_scene_nclip_b() -> TestResult {
    TestResult::info(
        0x0000_30ba,
        scene_nclip_mac0(0x0073_00d5, 0xffde_00dc, 0xffd8_0130),
        "scene nclip B",
    )
}
fn test_gte_scene_nclip_c() -> TestResult {
    TestResult::info(
        0x0000_3e7e,
        scene_nclip_mac0(0x0079_011f, 0xffd8_0130, 0xffd2_0194),
        "scene nclip C",
    )
}

// LZCS/LZCR: leading-bit count (bits equal to bit 31). These functional
// checks deliberately read a settled result; the dedicated +1..+6 cases
// below measure the silicon stale-read window independently.
fn lzcr(value: u32) -> u32 {
    write_data!(30, value);
    gte_nops!(64);
    read_data!(31)
}
fn test_gte_lzcr_zeros() -> TestResult {
    expect_eq(8, lzcr(0x00ff_ffff), "lzcr 00ffffff")
}
fn test_gte_lzcr_half() -> TestResult {
    expect_eq(16, lzcr(0xffff_0000), "lzcr ffff0000")
}
fn test_gte_lzcr_one() -> TestResult {
    expect_eq(31, lzcr(0x0000_0001), "lzcr 00000001")
}
fn test_gte_lzcr_posmax() -> TestResult {
    expect_eq(1, lzcr(0x7fff_ffff), "lzcr 7fffffff")
}
fn test_gte_lzcr_negmin() -> TestResult {
    expect_eq(1, lzcr(0x8000_0000), "lzcr 80000000")
}

// Corner ops: the famous bugged MVMVA far-color mode, plus SQR / OP / AVSZ3
// to widen op coverage. Expecteds from psx-gte-core (gte_expected_values).
// MVMVA bugged FC mode (cv=2): PSX-SPX documents that the far-color
// translation is dropped AND the first matrix column is dropped, so the
// result reduces to MAC_i = (Mx_i2*Vy + Mx_i3*Vz) >> sf. Per-component so
// the disc pins each MAC exactly; hardware (2026-06-09) returned the same
// fix, so these PASS on silicon = confirmation the GTE core now matches.
fn run_mvmva_fc() {
    seed_scene_xform();
    write_control!(21, 0x0000_1000); // FCX
    write_control!(22, 0x0000_2000); // FCY
    write_control!(23, 0x0000_3000); // FCZ
    write_data!(0, 0x2040_0340);
    write_data!(1, 0x0000_09c0);
    unsafe { gte_ops::rotate_v0_far_color() };
}
fn test_gte_mvmva_fc_mac1() -> TestResult {
    run_mvmva_fc();
    expect_eq(0xffff_fcc5, read_data!(25), "mvmva FC MAC1")
}
fn test_gte_mvmva_fc_mac2() -> TestResult {
    run_mvmva_fc();
    expect_eq(0xffff_e36c, read_data!(26), "mvmva FC MAC2")
}
fn test_gte_mvmva_fc_mac3() -> TestResult {
    run_mvmva_fc();
    expect_eq(0xffff_ee75, read_data!(27), "mvmva FC MAC3")
}
fn test_gte_sqr() -> TestResult {
    write_control!(31, 0);
    write_data!(9, 0x0000_1234);
    write_data!(10, 0x0000_f8ee);
    write_data!(11, 0x0000_0567);
    unsafe { gte_ops::square() };
    expect_eq(
        0x7498_ecb5,
        gte_tri_digest(read_data!(25), read_data!(26), read_data!(27)),
        "sqr",
    )
}
// OP cross product. The formula here matches PSX-SPX exactly and MVMVA/SQR
// read MAC1-3 immediately and pass, yet OP diverged on hardware
// (digest 0xBFF0043 vs 0xBFF002F) -- an unresolved silicon quirk. Split
// per-component so the next burn reveals WHICH MAC differs and by how
// much; expecteds are the current GTE-core values, so the FAIL on silicon
// captures the real number to fix `op_op` against. NOT latency (MAC1-3
// reads are latency-free, proven by MVMVA/SQR).
fn run_op() {
    write_control!(31, 0);
    write_control!(0, 0x0000_1000); // R11 (D1)
    write_control!(2, 0x0000_2000); // R22 (D2)
    write_control!(4, 0x0000_3000); // R33 (D3)
    write_data!(9, 0x0000_0400); // IR1
    write_data!(10, 0x0000_0500); // IR2
    write_data!(11, 0x0000_0600); // IR3
    unsafe { gte_ops::outer_product() };
}
fn test_gte_op_mac1() -> TestResult {
    run_op();
    // Immediate-read OP results sit inside the input/control-commitment
    // window (SCPH-9902: MAC3 reads 0 immediately, -768 settled), so the
    // observed value depends on issue timing, not arithmetic.
    // Characterisation, not conformance.
    TestResult::info(0xffff_fd00, read_data!(25), "op MAC1")
}
fn test_gte_op_mac2() -> TestResult {
    run_op();
    expect_eq(0x0000_0600, read_data!(26), "op MAC2")
}
fn test_gte_op_mac3() -> TestResult {
    run_op();
    expect_eq(0xffff_fd00, read_data!(27), "op MAC3")
}

// OP full-seed variant: identical diagonal + IR inputs to `run_op`, but with
// EVERY rotation control reg (0..=4, including the off-diagonal pairs 1 and 3)
// and the MAC/IR data regs written explicitly first -- the gte-fuzz pattern
// that the real console matches 1100/1100 (so OP compute is console-correct
// under full seeding). The original `run_op` leaves regs 1/3 holding leftover
// state from earlier tests and FAILED on silicon (MAC1 OBS 0xFFFFFBCE vs
// -768). Differential on one burn: full-seed PASS + original FAIL = hardware
// OP reads stale off-diagonal state (write-semantics family, same hunt as the
// SY0-drop battery); BOTH fail identically = input-independent OP quirk.
#[inline(always)]
fn seed_op_full() {
    write_control!(31, 0);
    write_control!(0, 0x0000_1000); // R11=D1, R12=0
    write_control!(1, 0x0000_0000); // R13=0, R21=0
    write_control!(2, 0x0000_2000); // R22=D2, R23=0
    write_control!(3, 0x0000_0000); // R31=0, R32=0
    write_control!(4, 0x0000_3000); // R33=D3
    write_data!(9, 0x0000_0400); // IR1
    write_data!(10, 0x0000_0500); // IR2
    write_data!(11, 0x0000_0600); // IR3
    write_data!(25, 0); // MAC1
    write_data!(26, 0); // MAC2
    write_data!(27, 0); // MAC3
}

fn run_op_full_seed() {
    seed_op_full();
    unsafe { gte_ops::outer_product() };
}

/// The prior console run produced the documented MAC1/MAC2 values but zero in
/// MAC3 even with every input explicitly seeded. A long control/input settle
/// before the identical OP distinguishes a CTC2/MTC2 commit hazard from an OP
/// arithmetic quirk without changing any operands.
fn run_op_full_seed_settled() {
    seed_op_full();
    gte_nops!(64);
    unsafe { gte_ops::outer_product() };
}
fn test_gte_op_full_seed_mac1() -> TestResult {
    run_op_full_seed();
    expect_eq(0xffff_fd00, read_data!(25), "op fs MAC1")
}
fn test_gte_op_full_seed_mac2() -> TestResult {
    run_op_full_seed();
    expect_eq(0x0000_0600, read_data!(26), "op fs MAC2")
}
fn test_gte_op_full_seed_mac3() -> TestResult {
    run_op_full_seed();
    // Same commitment-window dependence as op MAC1: both current platforms
    // read the immediate 0 here where the settled value is -768.
    TestResult::info(0xffff_fd00, read_data!(27), "op fs MAC3")
}
fn test_gte_avsz3() -> TestResult {
    write_control!(31, 0);
    write_control!(29, 0x0000_0155); // ZSF3
    write_data!(17, 0x0000_1000); // SZ1
    write_data!(18, 0x0000_2000); // SZ2
    write_data!(19, 0x0000_3000); // SZ3
    unsafe { gte_ops::average_z3() };
    expect_eq(0x0000_07fe, read_data!(7), "avsz3 OTZ")
}

// ---------------------------------------------------------------------------
// RTPS result-read latency sweep (chasing the player vertex EXPLOSION). The
// scene RTPS/RTPT cases all read SXY2 after a function-return gap, so an
// IMMEDIATE read of a freshly-projected register was never tested. If SXY /
// SZ / IR lag like MAC0 does, the player's `_faces` stage (which reads the
// projected SXY right after RTPS) would get stale coords -> scattered verts.
//
// Self-comparing so no baked expected is needed: project vertex A (let it
// settle), project vertex B, then read register R both IMMEDIATELY and after
// a settle delay. A and B give distinct SXY2/SZ3/IR1/IR0 (verified via the
// gte_expected_values host tool), so if R has read latency the immediate read
// returns A's value while the settled read returns B's -> the case FAILS on
// silicon and the dashboard shows EXP=settled(B) GOT=stale(A). Reads are live
// in-emulator (only MAC0/LZCR are modelled), so these PASS in emulation.
const LAT_A_XY: u32 = 0x0080_0100; // (256, 128)
const LAT_A_Z: u32 = 0x0000_0064; // 100
const LAT_B_XY: u32 = 0xffc0_ff38; // (-200, -64)
const LAT_B_Z: u32 = 0xffff_ff38; // -200

fn seed_proj_latency() {
    write_control!(31, 0);
    write_control!(0, 0x0000_1000); // identity R11,R12
    write_control!(1, 0x0000_0000); // R13,R21
    write_control!(2, 0x0000_1000); // R22,R23
    write_control!(3, 0x0000_0000); // R31,R32
    write_control!(4, 0x0000_1000); // R33
    write_control!(5, 0);
    write_control!(6, 0);
    write_control!(7, 0x0000_1000); // TRZ
    write_control!(24, 0x00a0_0000); // OFX 160
    write_control!(25, 0x0078_0000); // OFY 120
    write_control!(26, 0x0000_0100); // H 256
    write_control!(27, 0x0000_0100); // DQA
    write_control!(28, 0);
}

fn rtps_lat(vxy0: u32, vz0: u32) {
    write_data!(0, vxy0);
    write_data!(1, vz0);
    unsafe { gte_ops::project_single() };
}

/// Burn ~16 cycles so an in-flight GTE result settles before the read.
#[inline(always)]
fn gte_delay16() {
    #[cfg(target_arch = "mips")]
    unsafe {
        core::arch::asm!(
            ".rept 16\nnop\n.endr",
            options(nostack, nomem, preserves_flags)
        );
    }
}

// --- CTC2 matrix-load hazard sweeps ----------------------------------
// The cortex skinning path reloads the GTE rotation per blended vertex and
// the player explodes on real hardware; an 18-NOP settle gap did NOT fix it
// (HWB-007 follow-up). These sweeps MEASURE the hazard instead of guessing:
// every case compares a gapped matrix-load + MVMVA against a long-settled
// reference of the SAME load. Green in-emulator by construction (the CTC2
// hazard models are env-gated off); on silicon, every gap inside the true
// hazard window FAILs, and the OBS digest shows what the GTE actually
// computed. Two shapes:
//   "RT settle +N":  quiet load -> N nops -> MVMVA   (pure CTC2-use settle)
//   "RT drop +N":    RTPS issue -> N nops -> load -> LONG settle -> MVMVA
//                    (a failure here = the writes were LOST during the
//                    in-flight op, not merely late)
fn load_matrix_b() {
    write_control!(0, pack_gte_xy(0x2000, 0));
    write_control!(1, pack_gte_xy(0, 0));
    write_control!(2, pack_gte_xy(0x2000, 0));
    write_control!(3, pack_gte_xy(0, 0));
    write_control!(4, 0x2000);
}

fn rt_sweep_vertex() {
    write_data!(0, pack_gte_xy(0x123, -0x222));
    write_data!(1, 0x0333);
}

/// Long-settled ground truth: matrix B fully landed, then MVMVA.
fn rt_sweep_reference() -> u32 {
    seed_scene_xform();
    load_matrix_b();
    gte_nops!(64);
    rt_sweep_vertex();
    unsafe { gte_ops::rotate_translate_v0() };
    gte_delay16();
    gte_tri_digest(read_data!(25), read_data!(26), read_data!(27))
}

macro_rules! rt_settle_case {
    ($name:ident, $gap:tt) => {
        fn $name() -> TestResult {
            let expected = rt_sweep_reference();
            seed_scene_xform(); // matrix A in the GTE
            rt_sweep_vertex();
            unsafe { gte_ops::rotate_translate_v0() }; // pipe warmed with A
            gte_delay16();
            let _ = read_data!(25);
            load_matrix_b();
            gte_nops!($gap);
            unsafe { gte_ops::rotate_translate_v0() };
            gte_delay16();
            let got = gte_tri_digest(read_data!(25), read_data!(26), read_data!(27));
            expect_eq(expected, got, "rt settle gap")
        }
    };
}
rt_settle_case!(test_rt_settle_gap0, 0);
rt_settle_case!(test_rt_settle_gap2, 2);
rt_settle_case!(test_rt_settle_gap4, 4);
rt_settle_case!(test_rt_settle_gap8, 8);
rt_settle_case!(test_rt_settle_gap16, 16);
rt_settle_case!(test_rt_settle_gap32, 32);

macro_rules! rt_drop_case {
    ($name:ident, $gap:tt) => {
        fn $name() -> TestResult {
            let expected = rt_sweep_reference();
            seed_scene_xform();
            rt_sweep_vertex();
            unsafe { gte_ops::project_single() }; // 15-cycle op now in flight
            gte_nops!($gap);
            load_matrix_b(); // writes land while RTPS may be executing
            gte_nops!(64); // long settle: a failure means LOST, not late
            rt_sweep_vertex();
            unsafe { gte_ops::rotate_translate_v0() };
            gte_delay16();
            let got = gte_tri_digest(read_data!(25), read_data!(26), read_data!(27));
            expect_eq(expected, got, "rt drop-during-exec")
        }
    };
}
rt_drop_case!(test_rt_drop_gap0, 0);
rt_drop_case!(test_rt_drop_gap4, 4);
rt_drop_case!(test_rt_drop_gap8, 8);
rt_drop_case!(test_rt_drop_gap16, 16);

// --- Joint-compose chain replication -----------------------------------
// The engine builds each joint matrix per-frame ON the GTE
// (`gte_compose_joint_rotation`): load view rotation + zero TR, transform
// the model matrix's three COLUMNS as vertices back-to-back (MTC2 V0 pair,
// MVMVA, immediate MAC1-3 reads, zero gap between columns), then CTC2-load
// the composed result as the rotation for the vertex loop. The simple RT
// sweeps above passed on silicon, so THIS chained shape is the explosion's
// last untested suspect. `mode` isolates where a settle would matter:
//   0 = fully hot (the engine's exact shape)
//   1 = settle everywhere (ground truth)
//   2 = settle only before each MTC2 V0 pair
//   3 = settle only before the final composed-matrix CTC2 load
// All compare against mode 1; in-emulator every mode is identical. On
// silicon, the modes that FAIL share the unsettled step that matters.
fn compose_chain(mode: u8) -> u32 {
    let s_all = mode == 1;
    let s_writes = s_all || mode == 2;
    let s_load = s_all || mode == 3;
    seed_scene_xform();
    write_control!(5, 0); // TR = 0, matching gte_compose_joint_rotation
    write_control!(6, 0);
    write_control!(7, 0);
    if s_all {
        gte_nops!(64);
    }
    // Model matrix B fed column-wise, the engine's layout.
    let cols: [(u32, u32); 3] = [
        (pack_gte_xy(0x0FE0, 0x0200), 0x0100),
        (pack_gte_xy(0x0180, 0x0F80), 0x0240),
        (pack_gte_xy(0x00C0, 0x02C0), 0x0F40),
    ];
    let mut c = [[0i16; 3]; 3];
    let mut j = 0usize;
    while j < 3 {
        if s_writes {
            gte_nops!(64);
        }
        write_data!(0, cols[j].0);
        write_data!(1, cols[j].1);
        unsafe { gte_ops::rotate_translate_v0() };
        if s_all {
            gte_nops!(64);
        }
        c[0][j] = read_data!(25) as i32 as i16;
        c[1][j] = read_data!(26) as i32 as i16;
        c[2][j] = read_data!(27) as i32 as i16;
        j += 1;
    }
    if s_load {
        gte_nops!(64);
    }
    write_control!(0, pack_gte_xy(c[0][0], c[0][1]));
    write_control!(1, pack_gte_xy(c[0][2], c[1][0]));
    write_control!(2, pack_gte_xy(c[1][1], c[1][2]));
    write_control!(3, pack_gte_xy(c[2][0], c[2][1]));
    write_control!(4, c[2][2] as i32 as u32);
    if s_all {
        gte_nops!(64);
    }
    rt_sweep_vertex();
    unsafe { gte_ops::rotate_translate_v0() };
    gte_delay16();
    gte_tri_digest(read_data!(25), read_data!(26), read_data!(27))
}

fn test_compose_chain_hot() -> TestResult {
    expect_eq(compose_chain(1), compose_chain(0), "compose chain hot")
}
fn test_compose_chain_v0_settled() -> TestResult {
    expect_eq(
        compose_chain(1),
        compose_chain(2),
        "compose chain v0-settled",
    )
}
fn test_compose_chain_load_settled() -> TestResult {
    expect_eq(
        compose_chain(1),
        compose_chain(3),
        "compose chain load-settled",
    )
}

// --- Result-read settle sweeps (MAC0 / LZCR) ----------------------------
// Silicon showed only the ENDPOINTS so far: back-to-back reads are stale
// (they return the PREVIOUS result), +8 nops are settled. The emulator
// model needs the exact threshold to be faithful without breaking
// commercial games (libgte reads at distances the endpoints never
// measured -- the Crash menu regression). These sweep +1..+6: each case
// primes the stale slot with a DIFFERENT prior result, re-runs the op,
// reads after N nops, and compares against a settled reference. The
// smallest passing N is the silicon threshold, per register.
macro_rules! mac0_settle_case {
    ($name:ident, $gap:tt) => {
        fn $name() -> TestResult {
            // Settled reference: positive winding, MAC0 = +100.
            seed_nclip_pos_triangle();
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            let expected = read_data!(24);
            // Poison the stale slot: reversed winding -> MAC0 = -100.
            write_data!(13, pack_gte_xy(0, 10));
            write_data!(14, pack_gte_xy(10, 0));
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            // Probe: restore positive winding, read after N nops.
            seed_nclip_pos_triangle();
            unsafe { gte_ops::screen_winding() };
            gte_nops!($gap);
            let got = read_data!(24);
            expect_eq(expected, got, "nclip mac0 settle gap")
        }
    };
}
mac0_settle_case!(test_mac0_settle_gap1, 1);
mac0_settle_case!(test_mac0_settle_gap2, 2);
mac0_settle_case!(test_mac0_settle_gap3, 3);
mac0_settle_case!(test_mac0_settle_gap4, 4);
mac0_settle_case!(test_mac0_settle_gap6, 6);

macro_rules! lzcr_settle_case {
    ($name:ident, $gap:tt, $want:expr) => {
        fn $name() -> TestResult {
            // Prime the stale slot: LZCS 0x00ffffff -> LZCR 8, settled.
            write_data!(30, 0x00ff_ffff);
            gte_nops!(64);
            let _ = read_data!(31);
            // Probe: LZCS 0x00000001 -> LZCR 31, read after N nops.
            write_data!(30, 0x0000_0001);
            gte_nops!($gap);
            let got = read_data!(31);
            expect_eq($want, got, "lzcr settle gap")
        }
    };
}
// One intervening nop is NOT enough on the reference console (2026-08-07
// capture): the read still returns the PRIOR count, 8. Two or more settle.
// The expectation records that measured window rather than the ideal.
lzcr_settle_case!(test_lzcr_settle_gap1, 1, 8);
lzcr_settle_case!(test_lzcr_settle_gap2, 2, 31);
lzcr_settle_case!(test_lzcr_settle_gap3, 3, 31);
lzcr_settle_case!(test_lzcr_settle_gap4, 4, 31);
lzcr_settle_case!(test_lzcr_settle_gap6, 6, 31);

// Magnitude-dependent settle probe: the guest disassembly shows the scene
// NCLIP A/B/C reads sit at the SAME +2-instruction distance as the small-
// triangle settle sweep above -- yet on silicon the sweep PASSES and the
// scene cases FAIL. Distance is not the discriminator; the remaining
// variable is OPERAND MAGNITUDE (the scene cases compute with large real
// coordinates). If big products take longer to settle in the read path,
// these large-value sweeps fail at small N and pass at large N, giving
// the worst-case threshold the emulator model must use.
macro_rules! mac0_big_settle_case {
    ($name:ident, $gap:tt) => {
        fn $name() -> TestResult {
            // Settled reference with the scene-A coordinates.
            write_control!(31, 0);
            write_data!(12, 0x006e_0095);
            write_data!(13, 0xffe2_0094);
            write_data!(14, 0xffde_00dc);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            let expected = read_data!(24);
            // Poison MAC0 with a different settled result (swap winding).
            write_data!(13, 0xffde_00dc);
            write_data!(14, 0xffe2_0094);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            // Probe at +N with the original large coordinates.
            write_data!(13, 0xffe2_0094);
            write_data!(14, 0xffde_00dc);
            unsafe { gte_ops::screen_winding() };
            gte_nops!($gap);
            let got = read_data!(24);
            // These probes MEASURE the settle window; on silicon the window
            // is real (partial sums at small gaps is the documented
            // behavior), so "settled == probed" can never be a pass
            // criterion. Characterisation, not conformance.
            TestResult::info(expected, got, "nclip big-value settle gap")
        }
    };
}
mac0_big_settle_case!(test_mac0_big_settle_gap1, 1);
mac0_big_settle_case!(test_mac0_big_settle_gap2, 2);
mac0_big_settle_case!(test_mac0_big_settle_gap4, 4);
mac0_big_settle_case!(test_mac0_big_settle_gap8, 8);

// Follow-up battery for the partial-accumulation discovery: the big-value
// probe reads MAC0 = the four products NOT involving SY0 (0x874, exact term
// match) at +1..+8 nops, settling only by +64. Three questions, one burn:
// WHERE does it complete (gap bisect +12..+48)? WHAT makes "large" large
// (magnitude ladder at +4)? And do the in-situ scene B/C values reproduce
// under a CONTROLLED prestate (replicas with in-test poison at +2), or were
// they contaminated by whatever the harness left in the GTE?
// `$finish` picks the verdict path: `expect_eq` where "reference == probe"
// is a genuine invariant (scene-C: silicon computes the full cross in both
// phases), `TestResult::info` for the settle-window probes, whose whole
// point is that the probe reads a partial sum on silicon.
macro_rules! mac0_ctrl_case {
    ($name:ident, $finish:path, $gap:tt, $s0:literal, $s1:literal, $s2:literal) => {
        fn $name() -> TestResult {
            // Settled reference.
            write_control!(31, 0);
            write_data!(12, $s0);
            write_data!(13, $s1);
            write_data!(14, $s2);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            let expected = read_data!(24);
            // Poison: swap the two far vertices (negated cross), settled.
            write_data!(13, $s2);
            write_data!(14, $s1);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            // Probe at +N.
            write_data!(13, $s1);
            write_data!(14, $s2);
            unsafe { gte_ops::screen_winding() };
            gte_nops!($gap);
            let got = read_data!(24);
            $finish(expected, got, "nclip controlled settle")
        }
    };
}
// Gap bisect with the scene-A coordinates (0x874 regime).
mac0_ctrl_case!(
    test_mac0_big_gap12,
    TestResult::info,
    12,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap16,
    TestResult::info,
    16,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap24,
    TestResult::info,
    24,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap32,
    TestResult::info,
    32,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap48,
    TestResult::info,
    48,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
// Magnitude ladder at +4: quarter / half / double scale of scene-A.
mac0_ctrl_case!(
    test_mac0_mag_quarter,
    TestResult::info,
    4,
    0x001c_0025,
    0xfff8_0025,
    0xfff7_0037
);
mac0_ctrl_case!(
    test_mac0_mag_half,
    TestResult::info,
    4,
    0x0037_004a,
    0xfff1_004a,
    0xffef_006e
);
mac0_ctrl_case!(
    test_mac0_mag_double,
    TestResult::info,
    4,
    0x00dc_012a,
    0xffc4_0128,
    0xffbc_01b8
);
// Controlled-prestate replicas of scene B and C at the in-situ +2 distance.
// Scene-B still reads a partial on silicon (v1.16 console: 0xAFE), so it is
// characterisation; scene-C computes the full cross in both phases on
// silicon and stays a conformance invariant.
mac0_ctrl_case!(
    test_mac0_ctrl_b,
    TestResult::info,
    2,
    0x0073_00d5,
    0xffde_00dc,
    0xffd8_0130
);
mac0_ctrl_case!(
    test_mac0_ctrl_c,
    expect_eq,
    2,
    0x0079_011f,
    0xffd8_0130,
    0xffd2_0194
);
// Magnitude ladder extension downward: eighth and sixteenth scale of
// scene-A (round-half-away halving of the quarter constants). The ladder's
// point is finding WHERE the partial-sum regime begins; quarter still
// showed partials at +4 while the tiny winding triangle settles instantly,
// so the threshold sits below quarter scale.
mac0_ctrl_case!(
    test_mac0_mag_eighth,
    TestResult::info,
    4,
    0x000e_0013,
    0xfffc_0013,
    0xfffc_001c
);
mac0_ctrl_case!(
    test_mac0_mag_sixteenth,
    TestResult::info,
    4,
    0x0007_0009,
    0xfffe_0009,
    0xfffe_000e
);

/// Scene-C replica with every SXY write going through SXYP (data reg 15,
/// the FIFO-push mirror: a push moves SXY1->SXY0, SXY2->SXY1, then lands
/// in SXY2), the other documented write path. 0x8b's direct SXY2 writes
/// pass on silicon (full cross in both phases); if the commit hazard
/// tracks the write port, this variant reads differently.
fn test_mac0_ctrl_c_sxyp() -> TestResult {
    // Settled reference, all three vertices pushed in order.
    write_control!(31, 0);
    write_data!(15, 0x0079_011f);
    write_data!(15, 0xffd8_0130);
    write_data!(15, 0xffd2_0194);
    unsafe { gte_ops::screen_winding() };
    gte_nops!(64);
    let expected = read_data!(24);
    // Poison: far vertices swapped (negated cross), settled.
    write_data!(15, 0x0079_011f);
    write_data!(15, 0xffd2_0194);
    write_data!(15, 0xffd8_0130);
    unsafe { gte_ops::screen_winding() };
    gte_nops!(64);
    // Probe at +2 via the same push path.
    write_data!(15, 0x0079_011f);
    write_data!(15, 0xffd8_0130);
    write_data!(15, 0xffd2_0194);
    unsafe { gte_ops::screen_winding() };
    gte_nops!(2);
    let got = read_data!(24);
    TestResult::info(expected, got, "nclip ctrl scene-C via SXYP")
}

/// Does an MFC2 issued while NCLIP executes stall the CPU until the
/// command completes? psx-spx and DuckStation say yes (read interlock);
/// the measured settle-window partials on this console suggest otherwise.
/// Timer 2 at sysclock brackets nclip+immediate-mfc2; the same bracket
/// around two nops calibrates the overhead. With a real interlock the low
/// half cannot sit near the high half plus one; without one it can.
fn test_gte_nclip_read_interlock() -> TestResult {
    seed_nclip_pos_triangle();
    timers::set_mode(timers::Timer::Timer2, 0x0000);
    timers::set_counter(timers::Timer::Timer2, 0);
    let t0 = timers::counter(timers::Timer::Timer2);
    unsafe { gte_ops::screen_winding() };
    let _ = read_data!(24);
    let t1 = timers::counter(timers::Timer::Timer2);
    let with_gte = t1.wrapping_sub(t0);
    let t2 = timers::counter(timers::Timer::Timer2);
    unsafe { core::arch::asm!("nop", "nop") };
    let t3 = timers::counter(timers::Timer::Timer2);
    let baseline = t3.wrapping_sub(t2);
    TestResult::info(
        0,
        ((baseline as u32) << 16) | with_gte as u32,
        "hi=2-nop bracket lo=nclip+mfc2",
    )
}

/// Shared setup for the CLUT staleness pair: 16x16 8bpp indices plus a
/// deterministic 256-entry palette at `clut`, returning the primitive that
/// samples them into the scratch region.
fn clut_stale_setup(clut: Clut) -> prim::TriTextured {
    let mut idx = [0u8; 16 * 16];
    for (i, b) in idx.iter_mut().enumerate() {
        // Indices 16..255 only: colors 00-0F live in the SHARED 16-entry
        // line, which legitimately refreshes through interleaved 4bpp
        // draws. Staying above 0x10 makes both cases read the 240-entry
        // 8bpp-only line exclusively, which is the line under test.
        *b = 16 + ((i * 7) % 240) as u8;
    }
    // Rows 288.. of the (832,256) page: clear of 0xa2's block at v=0..15.
    psx_vram::upload_bytes(psx_vram::VramRect::new(832, 288, 8, 16), &idx);
    let mut pal = [psx_vram::Color555::raw(0); 256];
    for (i, c) in pal.iter_mut().enumerate() {
        let n = i as u8;
        *c = psx_vram::Color555::rgb5(n & 0x1f, (n >> 1) & 0x1f, (n >> 2) & 0x1f);
    }
    psx_vram::upload_clut(clut, &pal);
    let tpage = TexturePage::new(832, 256, TextureDepth::Bit8).uv_word(0);
    prim::TriTextured::new(
        [(8, 8), (88, 16), (40, 88)],
        [(0, 32), (15, 32), (8, 47)],
        clut.uv_word(),
        tpage,
        (0x80, 0x80, 0x80),
    )
}

/// Rewrite the palette under `clut` IN PLACE with a visibly different
/// mapping (channels rotated). A fresh CLUT fetch after this produces a
/// different raster hash; a stale cache reproduces the first draw exactly.
fn clut_stale_rewrite(clut: Clut) {
    let mut pal = [psx_vram::Color555::raw(0); 256];
    for (i, c) in pal.iter_mut().enumerate() {
        let n = i as u8;
        *c = psx_vram::Color555::rgb5((n >> 2) & 0x1f, n & 0x1f, (n >> 1) & 0x1f);
    }
    psx_vram::upload_clut(clut, &pal);
}

/// GPU: the CLUT cache must NOT reload when the palette data is rewritten
/// in place under an unchanged clut word (the reload key is the clut WORD,
/// not the VRAM content; the VRAM copy flushes the texture cache only).
fn test_gpu_clut_inplace_rewrite_stale() -> TestResult {
    let clut = Clut::new(0, 501);
    let tri = clut_stale_setup(clut);
    let first = gpu_draw_and_hash(&tri, prim::TriTextured::WORDS);
    clut_stale_rewrite(clut);
    let second = gpu_draw_and_hash(&tri, prim::TriTextured::WORDS);
    expect_eq(first, second, "stale clut after in-place rewrite")
}

/// GPU: the 240-entry 8bpp CLUT line tracks its own last-load address; a
/// 4bpp primitive with a DIFFERENT clut word between the rewrite and the
/// redraw must not refresh it. (A single shared reload register would.)
fn test_gpu_clut_stale_across_4bpp_interleave() -> TestResult {
    let clut = Clut::new(0, 501);
    let tri = clut_stale_setup(clut);
    let first = gpu_draw_and_hash(&tri, prim::TriTextured::WORDS);
    clut_stale_rewrite(clut);
    // Interleave: a 4bpp textured draw with a different clut word, into the
    // scratch region (cleared again by the final draw-and-hash).
    let mut idx4 = [0u8; 8];
    for (i, b) in idx4.iter_mut().enumerate() {
        *b = (i as u8) | ((i as u8) << 4);
    }
    psx_vram::upload_bytes(psx_vram::VramRect::new(832, 306, 2, 2), &idx4);
    let mut pal16 = [psx_vram::Color555::raw(0); 16];
    for (i, c) in pal16.iter_mut().enumerate() {
        *c = psx_vram::Color555::rgb5(i as u8 * 2, 0x1f - i as u8, 8);
    }
    let clut4 = Clut::new(0, 502);
    psx_vram::upload_clut(clut4, &pal16);
    let tpage4 = TexturePage::new(832, 256, TextureDepth::Bit4).uv_word(0);
    let interleave = prim::TriTextured::new(
        [(4, 4), (12, 4), (8, 12)],
        [(0, 50), (7, 50), (4, 51)],
        clut4.uv_word(),
        tpage4,
        (0x80, 0x80, 0x80),
    );
    let _ = gpu_draw_and_hash(&interleave, prim::TriTextured::WORDS);
    let second = gpu_draw_and_hash(&tri, prim::TriTextured::WORDS);
    expect_eq(first, second, "8bpp clut line survives 4bpp interleave")
}

// ---- v0.17 demo-disc text-pipeline replica -------------------------------
//
// The launcher's description text (SPLEEN 5x8, drawn into a 15bpp cache at
// (512,0) and blitted) renders lowercase 'f' as a bare crossbar on real
// hardware while every emulator draws it whole. These three cases rebuild
// that pipeline stage by stage at the launcher's EXACT VRAM geometry:
// whichever case's hash diverges on console names the corrupt stage
// (glyph rects into the cache, the 15bpp blit of it, or the same glyph
// rects drawn directly). Expected values are pinned from the emulator, so
// on silicon a FAIL is the finding and the observed hash is the data.

/// The launcher's small-font geometry, exactly: SPLEEN 5x8 as 4bpp at
/// (448,0) with its two-entry CLUT at (416,256).
fn spleen_replica() -> FontAtlas {
    FontAtlas::upload(
        &SPLEEN_5X8,
        TexturePage::new(448, 0, TextureDepth::Bit4),
        Clut::new(416, 256),
    )
}

/// Probe text, f-dense with in-word/leading/trailing forms plus controls
/// ('t' shares the crossbar shape, '-' is what the console shows instead).
const TEXT_LINE_1: &str = "for office fluffy staff";
const TEXT_LINE_2: &str = "the quick fox left -- tt";

/// Replicate TextCache::begin + the glyph pass: GP0(02) fill of the
/// 115x92 cache rect (NOT 16-aligned in width -- silicon rounds fill
/// widths up to 16, one of the candidate divergences), draw area and
/// offset pointed into it, two lines of SPLEEN rects.
fn text_cache_glyph_pass(small: &FontAtlas) {
    probe_gpu!(gpu);
    gpu.draw(&FillRect::new((512, 0), (115, 92), (0, 0, 0)));
    gpu.set_draw_area((512, 0), (512 + 115 - 1, 92 - 1));
    gpu.set_draw_offset((512, 0));
    small.draw_text(2, 2, TEXT_LINE_1, (255, 255, 255));
    small.draw_text(2, 12, TEXT_LINE_2, (255, 255, 255));
    gpu.wait_idle();
}

/// GPU: the glyph pass must LAND in the cache region correctly.
fn test_gpu_text_cache_glyphs_land() -> TestResult {
    let small = spleen_replica();
    text_cache_glyph_pass(&small);
    gpu_draw_env_scratch();
    // 115x92 is odd-area for the word reader; hash the even 114x92 span
    // (glyphs start at x=2, nothing lives in the last column).
    expect_eq(
        0xC14F_EAD9,
        gpu_hash_rect(512, 0, 114, 92),
        "text cache glyph pass VRAM",
    )
}

/// GPU: the 15bpp blit of the cache region must reproduce it on screen.
fn test_gpu_text_cache_blit() -> TestResult {
    probe_gpu!(gpu);
    let small = spleen_replica();
    text_cache_glyph_pass(&small);
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    let tpage = TexturePage::new(512, 0, TextureDepth::Bit15);
    gpu.set_draw_mode(psx_gpu::material::TextureMaterial::opaque(
        0,
        tpage.uv_word(0),
        (128, 128, 128),
    ));
    gpu.draw(&Sprite::with_material(
        0,
        0,
        96,
        24,
        (0, 0),
        psx_gpu::material::TextureMaterial::opaque(0, tpage.uv_word(0), (128, 128, 128)),
    ));
    gpu_io::wait_command_ready();
    // Same value as the direct draw's hash: the blit round trip is
    // pixel-exact in the emulator, which is the property under test.
    expect_eq(0x3B20_8994, gpu_hash_scratch(), "text cache blit output")
}

/// Draw ONE glyph, alone, into a cleared scratch and hash it. The v1.17
/// console run proved the corruption lives in the glyph rect path (0xB4 and
/// 0xB5 agree on silicon, so the cache round trip is faithful), but an
/// aggregate hash over a whole line cannot say WHICH glyph is wrong. These
/// isolate one character each, so the next capture reads as a per-glyph
/// verdict: 'f' is the reported bad one, 'r' and 't' share its atlas row and
/// its awkward u&3==3 alignment and look fine on screen, and 'o' straddles a
/// 16-texel boundary the way 'f' does. Expected values are pinned from the
/// emulator; on console a FAIL names a corrupt glyph and the observed hash
/// says how it differs.
fn draw_one_glyph_hash(ch: char) -> u32 {
    probe_gpu!(gpu);
    let small = spleen_replica();
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    let mut buf = [0u8; 4];
    let s: &str = ch.encode_utf8(&mut buf);
    small.draw_text(2, 2, s, (255, 255, 255));
    gpu.wait_idle();
    gpu_hash_scratch()
}

// ---- Why 0xA6/0xA7 still fail on silicon ------------------------------
//
// The v1.18 console capture narrowed this a long way. Precision 036-038 show
// that with the DMA timing override armed the readback is SELF-CONSISTENT on
// silicon: the single-block and four-block hashes come back identical
// (0x083B6E3D). The boot-mode words show the documented unstable shape, an
// 0xFFFF inserted at every DMA block start (words 00/04/08/12 of the
// four-block read). So the read path is understood and behaving; what is not
// established is whether the WRITE landed. 0xA7's readback is deterministic
// across two different builds while 0xA6's moves, which is what stale RAM
// under a write that never arrived would look like.
//
// These three separate the two halves, so one burn settles it.

/// The 64-byte pattern 0xA6 uploads, so the probes below can reuse it.
fn spu_probe_pattern() -> [u32; 16] {
    let mut src = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        src[i] = 0xC0DE_0000u32.wrapping_add((i as u32) * 0x111);
        i += 1;
    }
    src
}

/// SPU: reading the same SPU RAM twice must give the same answer. Nothing
/// writes between the two reads, so a mismatch means the READ path is
/// unstable and no upload test above it can be trusted.
fn test_spu_read_is_repeatable() -> TestResult {
    let mut a = [0u32; 16];
    let mut b = [0u32; 16];
    spu_dma_read(0x3000, &mut a);
    spu_dma_read(0x3000, &mut b);
    expect_eq(fnv32_words(&a), fnv32_words(&b), "spu read repeatable")
}

/// SPU: the same bytes uploaded by DMA and by the manual FIFO must leave SPU
/// RAM in the same state. Both land through the same read path, so this
/// compares the two WRITE paths against each other without depending on the
/// read being faithful. A pass with 0xA6/0xA7 failing means both writes agree
/// and the readback is what diverges from expectation; a fail names the
/// write path that is wrong.
fn test_spu_dma_and_fifo_agree() -> TestResult {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL, TRANSFER_DATA};
    let src = spu_probe_pattern();
    let bytes = unsafe { core::slice::from_raw_parts(src.as_ptr() as *const u8, 64) };
    psx_spu::upload_adpcm(SpuAddr::new(0x3800), bytes);

    // The same 64 bytes again, by hand through the FIFO, at 0x3C00.
    let halfwords = unsafe { core::slice::from_raw_parts(src.as_ptr() as *const u16, 32) };
    unsafe {
        psx_io::write_u16(TRANSFER_ADDR, (0x3C00u32 / 8) as u16);
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        let spucnt = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, spucnt | 0x0010);
        let mut settle = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != (spucnt | 0x0010) & 0x003F && settle < 0xFFFF {
            settle += 1;
        }
        for &hw in halfwords.iter() {
            psx_io::write_u16(TRANSFER_DATA, hw);
        }
        let mut drain = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x0400 != 0 && drain < 0xFFFF {
            drain += 1;
        }
        psx_io::write_u16(SPUCNT, spucnt);
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
    }

    let mut via_dma = [0u32; 16];
    let mut via_fifo = [0u32; 16];
    spu_dma_read(0x3800, &mut via_dma);
    spu_dma_read(0x3C00, &mut via_fifo);
    expect_eq(
        fnv32_words(&via_dma),
        fnv32_words(&via_fifo),
        "spu dma vs fifo write",
    )
}

/// SPU: the same DMA upload, but with the transfer address written AFTER the
/// transfer mode is armed rather than before.
///
/// This is the candidate fix. psx-spx documents the write to 1F801DA6 as
/// latching the SPU's internal current address; if arming DMA-write mode
/// re-latches it from somewhere else, the SDK's order (address, then mode)
/// would leave the transfer pointed at the wrong place, which is exactly a
/// write that never arrives. If this PASSES on console while 0xA6 fails, the
/// ordering is the bug and psx-spu's `upload_adpcm` gets the same swap.
fn test_spu_upload_addr_after_mode() -> TestResult {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
    let src = spu_probe_pattern();
    let dest: u32 = 0x4400;
    unsafe {
        let spucnt = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, spucnt);
        let mut settle = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != spucnt & 0x003F && settle < 0xFFFF {
            settle += 1;
        }
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        // Mode first...
        psx_io::write_u16(SPUCNT, spucnt | 0x0020);
        let mut armed = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != (spucnt | 0x0020) & 0x003F && armed < 0xFFFF {
            armed += 1;
        }
        // ...then the address.
        psx_io::write_u16(TRANSFER_ADDR, (dest / 8) as u16);

        dma::enable_channel(dma::Channel::Spu);
        dma::raw::set_address(dma::Channel::Spu, src.as_ptr() as u32);
        dma::raw::set_size(dma::Channel::Spu, dma::size_blocks(4, 4));
        dma::raw::set_control(
            dma::Channel::Spu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );
        if !dma::wait_done(dma::Channel::Spu, 200_000) {
            dma::abort(dma::Channel::Spu);
        }
        let mut idle = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x0400 != 0 && idle < 0xFFFF {
            idle += 1;
        }
        psx_io::write_u16(SPUCNT, spucnt);
    }
    let mut back = [0u32; 16];
    spu_dma_read(dest, &mut back);
    expect_eq(
        fnv32_words(&src),
        fnv32_words(&back),
        "spu upload addr-after-mode",
    )
}

/// SPU: the same 64-byte DMA upload paced as four 4-word blocks instead of
/// one 16-word block.
///
/// The v1.18 capture says the write is what fails, not the read: precision
/// 002-017 reconstruct to only the first 6 bytes of a 64-byte DMA upload
/// having landed, and 039-042 show a manual-FIFO upload landing not at all,
/// both read back through a path whose two block shapes agree with each
/// other. `upload_adpcm` picks the LARGEST block size dividing the payload,
/// so 64 bytes go as a single 16-word block -- exactly the SPU's whole
/// 32-halfword transfer FIFO, with no DRQ pause anywhere in it for the SPU to
/// drain. Smaller blocks give the device a say between each one. If this
/// passes on console while 0xA6 fails, block sizing is the bug and
/// `upload_adpcm` should cap it rather than maximise it.
fn test_spu_upload_small_blocks() -> TestResult {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
    let src = spu_probe_pattern();
    let dest: u32 = 0x4800;
    unsafe {
        let spucnt = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, spucnt);
        let mut settle = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != spucnt & 0x003F && settle < 0xFFFF {
            settle += 1;
        }
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        psx_io::write_u16(TRANSFER_ADDR, (dest / 8) as u16);
        psx_io::write_u16(SPUCNT, spucnt | 0x0020);
        let mut armed = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != (spucnt | 0x0020) & 0x003F && armed < 0xFFFF {
            armed += 1;
        }
        dma::enable_channel(dma::Channel::Spu);
        dma::raw::set_address(dma::Channel::Spu, src.as_ptr() as u32);
        // Four blocks of four words, rather than one block of sixteen.
        dma::raw::set_size(dma::Channel::Spu, dma::size_blocks(4, 4));
        dma::raw::set_control(
            dma::Channel::Spu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );
        if !dma::wait_done(dma::Channel::Spu, 200_000) {
            dma::abort(dma::Channel::Spu);
        }
        let mut idle = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x0400 != 0 && idle < 0xFFFF {
            idle += 1;
        }
        psx_io::write_u16(SPUCNT, spucnt);
    }
    let mut back = [0u32; 16];
    spu_dma_read(dest, &mut back);
    expect_eq(
        fnv32_words(&src),
        fnv32_words(&back),
        "spu upload 4x4 blocks",
    )
}

/// Read the uploaded SPLEEN atlas straight back out of VRAM and hash it.
/// This splits the glyph corruption cleanly in two: if the atlas readback
/// differs on console, the CPU->VRAM upload is dropping or duplicating
/// data and the rasteriser is innocent; if it matches while the drawn
/// glyphs still differ, the fault is in texel fetch. 64 halfwords by 24
/// rows, exactly the rect `FontAtlas::upload` writes for SPLEEN's padded
/// 8-texel cells (32 glyphs per row, three rows).
fn test_gpu_glyph_atlas_readback() -> TestResult {
    let _ = spleen_replica();
    expect_eq(
        0x7D60_40C4,
        gpu_hash_rect(448, 0, 64, 24),
        "spleen atlas VRAM",
    )
}

fn test_gpu_glyph_f() -> TestResult {
    expect_eq(0x492F_734F, draw_one_glyph_hash('f'), "lone glyph f")
}
fn test_gpu_glyph_r() -> TestResult {
    expect_eq(0x71C6_3DDB, draw_one_glyph_hash('r'), "lone glyph r")
}
fn test_gpu_glyph_t() -> TestResult {
    expect_eq(0x7AAF_034F, draw_one_glyph_hash('t'), "lone glyph t")
}
fn test_gpu_glyph_o() -> TestResult {
    expect_eq(0x6F95_5773, draw_one_glyph_hash('o'), "lone glyph o")
}

/// The same lone 'f', but drawn immediately after another glyph so the
/// texture cache is already populated from a different part of the atlas.
/// If 'f' alone is clean and 'f'-after-'r' is not, the fault is cache
/// aliasing between atlas columns rather than the glyph itself.
fn test_gpu_glyph_f_after_r() -> TestResult {
    probe_gpu!(gpu);
    let small = spleen_replica();
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    small.draw_text(2, 2, "r", (255, 255, 255));
    small.draw_text(2, 12, "f", (255, 255, 255));
    gpu.wait_idle();
    expect_eq(0x98B4_BD65, gpu_hash_scratch(), "glyph f after r")
}

/// GPU: the same two lines drawn STRAIGHT into the scratch, no cache
/// indirection. If this diverges on console too, the fault is the glyph
/// rect path itself, not the render-to-VRAM round trip.
fn test_gpu_text_direct_draw() -> TestResult {
    probe_gpu!(gpu);
    let small = spleen_replica();
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    small.draw_text(2, 2, TEXT_LINE_1, (255, 255, 255));
    small.draw_text(2, 12, TEXT_LINE_2, (255, 255, 255));
    gpu.wait_idle();
    expect_eq(0x3B20_8994, gpu_hash_scratch(), "direct glyph draw")
}

// --- SXY state dumps -----------------------------------------------------
// Every NCLIP result in the poison->probe shape matches "cross with the SY0
// terms dropped" (five exact values), EXCEPT controlled scene-C which
// computes correctly -- and no simple write-side-effect model explains both
// (offline brute-force over shift/clear/burst rules: zero matches). So stop
// inferring through the cross product: run the exact poison+probe write
// sequence, SETTLE, then read SXY0/1/2 BACK. The OBS values show directly
// what silicon left in each register. A-coords (a failing set) vs C-coords
// (the passing set) side by side is the differential that pins the rule.
macro_rules! sxy_dump_case {
    ($name:ident, $reg:tt, $expect:literal, $s0:literal, $s1:literal, $s2:literal) => {
        fn $name() -> TestResult {
            // Same shape as mac0_ctrl_case up to the probe writes.
            write_control!(31, 0);
            write_data!(12, $s0);
            write_data!(13, $s1);
            write_data!(14, $s2);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            let _ = read_data!(24);
            write_data!(13, $s2);
            write_data!(14, $s1);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            write_data!(13, $s1);
            write_data!(14, $s2);
            // No NCLIP here: settle, then dump the register state the probe
            // NCLIP would have consumed.
            gte_nops!(64);
            let got = read_data!($reg);
            expect_eq($expect, got, "sxy state dump")
        }
    };
}
// A coordinates (the y0-drop regime on silicon).
sxy_dump_case!(
    test_sxy_dump_a_12,
    12,
    0x006e_0095,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
sxy_dump_case!(
    test_sxy_dump_a_13,
    13,
    0xffe2_0094,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
sxy_dump_case!(
    test_sxy_dump_a_14,
    14,
    0xffde_00dc,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
// C coordinates (the passing set).
sxy_dump_case!(
    test_sxy_dump_c_12,
    12,
    0x0079_011f,
    0x0079_011f,
    0xffd8_0130,
    0xffd2_0194
);
sxy_dump_case!(
    test_sxy_dump_c_13,
    13,
    0xffd8_0130,
    0x0079_011f,
    0xffd8_0130,
    0xffd2_0194
);
sxy_dump_case!(
    test_sxy_dump_c_14,
    14,
    0xffd2_0194,
    0x0079_011f,
    0xffd8_0130,
    0xffd2_0194
);
// And the probe-NCLIP variant: same sequence WITH the probe nclip, then a
// long settle, then dump SXY0 -- does the op itself disturb the registers?
macro_rules! sxy_dump_post_nclip {
    ($name:ident, $expect:literal, $s0:literal, $s1:literal, $s2:literal) => {
        fn $name() -> TestResult {
            write_control!(31, 0);
            write_data!(12, $s0);
            write_data!(13, $s1);
            write_data!(14, $s2);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            let _ = read_data!(24);
            write_data!(13, $s2);
            write_data!(14, $s1);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            write_data!(13, $s1);
            write_data!(14, $s2);
            unsafe { gte_ops::screen_winding() };
            gte_nops!(64);
            let got = read_data!(12);
            expect_eq($expect, got, "sxy0 after probe nclip")
        }
    };
}
sxy_dump_post_nclip!(
    test_sxy_post_a,
    0x006e_0095,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
sxy_dump_post_nclip!(
    test_sxy_post_c,
    0x0079_011f,
    0x0079_011f,
    0xffd8_0130,
    0xffd2_0194
);

macro_rules! gte_result_latency_test {
    ($name:ident, $reg:literal, $label:literal) => {
        fn $name() -> TestResult {
            // Immediate read of B's result, primed with A settled.
            seed_proj_latency();
            rtps_lat(LAT_A_XY, LAT_A_Z);
            gte_delay16();
            rtps_lat(LAT_B_XY, LAT_B_Z);
            let immediate = read_data!($reg);
            // Settled reference: identical sequence but wait before reading.
            seed_proj_latency();
            rtps_lat(LAT_A_XY, LAT_A_Z);
            gte_delay16();
            rtps_lat(LAT_B_XY, LAT_B_Z);
            gte_delay16();
            let settled = read_data!($reg);
            expect_eq(settled, immediate, $label)
        }
    };
}
gte_result_latency_test!(test_gte_lat_sxy2, 14, "rtps SXY2 read-latency");
gte_result_latency_test!(test_gte_lat_sz3, 19, "rtps SZ3 read-latency");
gte_result_latency_test!(test_gte_lat_ir1, 9, "rtps IR1 read-latency");
gte_result_latency_test!(test_gte_lat_ir0, 8, "rtps IR0 read-latency");

// ---------------------------------------------------------------------------
// GPU render + VRAM read-back conformance. The GTE projects the player's
// vertices to in-frame coords (proven on the GTE pages) and the packet build
// is deterministic CPU -- so the on-hardware vertex stretching must be the GPU
// or the OT/DMA drawing a good coordinate wrong. These cases draw into an
// OFF-SCREEN scratch VRAM rect (clear of the framebuffer pages + font), read
// the pixels back via GP0 0xC0 + GPUREAD, and FNV-1a hash them. Expecteds are
// PSoXide's own pixel hashes (baked from a headless run), so green = GPU
// matches emulator and a RED case on silicon is the GPU/DMA quirk.
const GPU_SX: u16 = 512; // scratch VRAM x (clear of 320x240 fb pages + font tpage)
const GPU_SY: u16 = 256;
const GPU_SW: u16 = 96; // 16-aligned for the GP0 0x02 fill
const GPU_SH: u16 = 96;

/// GP0 0x02 fill rect (direct VRAM; ignores draw area/offset/mask).
fn gpu_fill(x: u16, y: u16, w: u16, h: u16, rgb24: u32) {
    gpu_io::wait_command_ready();
    gpu_io::write_command(0x0200_0000 | (rgb24 & 0x00FF_FFFF));
    gpu_io::write_command(((y as u32) << 16) | x as u32);
    gpu_io::write_command(((h as u32) << 16) | w as u32);
}

/// Point the drawing area + offset at the scratch rect, so primitive coords
/// are scratch-relative (0..GPU_SW / 0..GPU_SH).
fn gpu_draw_env_scratch() {
    let (x, y) = (GPU_SX as u32, GPU_SY as u32);
    gpu_io::write_command(0xE300_0000 | (x & 0x3FF) | ((y & 0x1FF) << 10));
    let (rx, ry) = (x + GPU_SW as u32 - 1, y + GPU_SH as u32 - 1);
    gpu_io::write_command(0xE400_0000 | (rx & 0x3FF) | ((ry & 0x1FF) << 10));
    gpu_io::write_command(0xE500_0000 | (x & 0x7FF) | ((y & 0x7FF) << 11));
}

/// Send a primitive's data words (skipping the leading OT tag) to GP0.
fn gpu_send_prim<T>(prim_ref: &T, words: u8) {
    let base = (prim_ref as *const T).cast::<u32>();
    gpu_io::wait_command_ready();
    for i in 0..words as usize {
        // +1 skips the `tag` word that only the OT/DMA path consumes.
        let word = unsafe { core::ptr::read(base.add(1 + i)) };
        gpu_io::write_command(word);
    }
}

/// Read the scratch rect back (GP0 0xC0 + GPUREAD) and FNV-1a hash the pixels.
fn gpu_hash_scratch() -> u32 {
    gpu_hash_rect(GPU_SX, GPU_SY, GPU_SW, GPU_SH)
}

/// FNV-hash an arbitrary VRAM rect over the C0 readback path. `w * h`
/// must be even (two 16bpp pixels per GPUREAD word).
fn gpu_hash_rect(x: u16, y: u16, w: u16, h: u16) -> u32 {
    gpu_io::wait_command_ready();
    gpu_io::write_command(0xC000_0000);
    gpu_io::write_command(((y as u32) << 16) | x as u32);
    gpu_io::write_command(((h as u32) << 16) | w as u32);
    let words = (w as u32 * h as u32) / 2;
    let mut hash = 0x811C_9DC5u32;
    for _ in 0..words {
        let mut guard = 0u32;
        // GPUSTAT bit 27 = ready to send VRAM->CPU data.
        while gpu_io::status().bits() & (1 << 27) == 0 && guard < 100_000 {
            guard += 1;
        }
        let w = gpu_io::read_data();
        hash = (hash ^ (w & 0xFFFF)).wrapping_mul(0x0100_0193);
        hash = (hash ^ (w >> 16)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Clear the scratch rect, draw a primitive into it, return the VRAM hash.
fn gpu_draw_and_hash<T>(prim_ref: &T, words: u8) -> u32 {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    gpu_send_prim(prim_ref, words);
    gpu_io::wait_command_ready();
    gpu_hash_scratch()
}

fn test_gpu_vram_roundtrip() -> TestResult {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0034_7c9a);
    expect_eq(0x1f84_f1c5, gpu_hash_scratch(), "gpu vram fill+read")
}
fn test_gpu_draw_flat_tri() -> TestResult {
    let tri = prim::TriFlat::new([(8, 8), (88, 16), (40, 88)], 0xc0, 0x40, 0x80);
    expect_eq(
        0x0412_1005,
        gpu_draw_and_hash(&tri, prim::TriFlat::WORDS),
        "gpu flat tri",
    )
}
fn test_gpu_draw_gouraud_tri() -> TestResult {
    let tri = prim::TriGouraud::new(
        [(8, 8), (88, 16), (40, 88)],
        [(0xf0, 0x00, 0x00), (0x00, 0xf0, 0x00), (0x00, 0x00, 0xf0)],
    );
    expect_eq(
        0x285a_c609,
        gpu_draw_and_hash(&tri, prim::TriGouraud::WORDS),
        "gpu gouraud tri",
    )
}
fn test_gpu_draw_flat_quad() -> TestResult {
    let q = prim::QuadFlat::new([(8, 8), (88, 8), (8, 88), (88, 88)], 0x30, 0xc0, 0x60);
    expect_eq(
        0x79e5_3dc5,
        gpu_draw_and_hash(&q, prim::QuadFlat::WORDS),
        "gpu flat quad",
    )
}
fn test_gpu_draw_gouraud_quad() -> TestResult {
    let q = prim::QuadGouraud::new(
        [(8, 8), (88, 8), (8, 88), (88, 88)],
        [(0xf0, 0, 0), (0, 0xf0, 0), (0, 0, 0xf0), (0xf0, 0xf0, 0)],
    );
    expect_eq(
        0x22b3_d6c3,
        gpu_draw_and_hash(&q, prim::QuadGouraud::WORDS),
        "gpu gouraud quad",
    )
}
// Edge-coordinate / large-span triangles -- the direct stretch suspects: how
// the GPU rasterizes a triangle whose vertex lands far outside the draw area,
// goes negative, or exceeds the 11-bit coordinate range (where it wraps).
fn test_gpu_tri_past_right_edge() -> TestResult {
    let tri = prim::TriFlat::new([(8, 8), (88, 8), (300, 88)], 0xff, 0x80, 0x20);
    expect_eq(
        0x69fc_0e38,
        gpu_draw_and_hash(&tri, prim::TriFlat::WORDS),
        "gpu tri past edge",
    )
}
fn test_gpu_tri_negative_coord() -> TestResult {
    let tri = prim::TriFlat::new([(8, 8), (-200, 40), (88, 88)], 0x20, 0xff, 0x80);
    expect_eq(
        0xa3a1_6bf5,
        gpu_draw_and_hash(&tri, prim::TriFlat::WORDS),
        "gpu tri neg coord",
    )
}
fn test_gpu_tri_coord_wrap() -> TestResult {
    // x=1500 exceeds the 11-bit signed range (max 1023) -> wraps on silicon.
    let tri = prim::TriFlat::new([(8, 48), (1500, 8), (48, 88)], 0x80, 0x20, 0xff);
    expect_eq(
        0x3df1_7315,
        gpu_draw_and_hash(&tri, prim::TriFlat::WORDS),
        "gpu tri coord wrap",
    )
}
// The player's EXACT primitive: a textured Gouraud triangle sampling a real
// VRAM texture (cortex's player is TriTexturedGouraud via OT/DMA). Upload a
// 16x16 15bpp texture into a tpage-aligned slot, then draw + read back.
fn test_gpu_textured_gouraud_tri() -> TestResult {
    let mut tex = [0u16; 16 * 16];
    for (i, texel) in tex.iter_mut().enumerate() {
        let x = (i % 16) as u16;
        let y = (i / 16) as u16;
        *texel = 0x8000 | (x << 10) | (y << 5) | ((x ^ y) & 0x1f);
    }
    psx_vram::upload_16bpp(psx_vram::VramRect::new(768, 256, 16, 16), &tex);
    let tpage = TexturePage::new(768, 256, TextureDepth::Bit15).uv_word(0);
    let tri = prim::TriTexturedGouraud::new(
        [(8, 8), (88, 16), (40, 88)],
        [(0, 0), (15, 0), (8, 15)],
        [(0x80, 0x80, 0x80), (0xc0, 0x80, 0x40), (0x40, 0xc0, 0x80)],
        0, // clut unused for 15bpp
        tpage,
    );
    expect_eq(
        0x0200_a836,
        gpu_draw_and_hash(&tri, prim::TriTexturedGouraud::WORDS),
        "gpu tex gouraud tri",
    )
}
/// The GPU DMA token for a probe that submits a frame from inside a test.
fn probe_gpu_dma() -> psx_io::periph::GpuDma {
    // SAFETY: the app runner holds the real token but starts no walk while a
    // test runs, and every submit waits for the previous walk before it
    // kicks, so this token never overlaps another one's transfer.
    unsafe { psx_io::periph::GpuDma::steal() }
}

// The player's submit PATH: build an ordering table, DMA it to the GPU
// (linked-list mode), then read back -- exercises the OT + DMA stage.
fn test_gpu_ot_dma_draw() -> TestResult {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    let mut t0 = prim::TriFlat::new([(4, 4), (90, 8), (4, 90)], 0xff, 0x20, 0x20);
    let mut t1 = prim::TriFlat::new([(90, 90), (90, 8), (8, 90)], 0x20, 0xff, 0x20);
    let mut t2 = prim::TriFlat::new([(40, 24), (72, 64), (20, 72)], 0x20, 0x20, 0xff);
    let mut ot = gpu::ot::OrderingTable::<4>::new();
    let mut frame = ot.frame();
    frame.add(2, &mut t0);
    frame.add(2, &mut t1);
    frame.add(0, &mut t2);
    frame.submit(&mut probe_gpu_dma());
    gpu_io::wait_command_ready();
    expect_eq(0xaffb_7c55, gpu_hash_scratch(), "gpu ot dma draw")
}

// The player renders TEXTURED-GOURAUD prims; upload a 16x16 15bpp texture into
// VRAM and return its tpage word, reused across the textured tests below.
fn gpu_upload_tex15() -> u16 {
    let mut tex = [0u16; 16 * 16];
    for (i, t) in tex.iter_mut().enumerate() {
        let x = (i % 16) as u16;
        let y = (i / 16) as u16;
        *t = 0x8000 | (x << 10) | (y << 5) | ((x ^ y) & 0x1f);
    }
    psx_vram::upload_16bpp(psx_vram::VramRect::new(768, 256, 16, 16), &tex);
    TexturePage::new(768, 256, TextureDepth::Bit15).uv_word(0)
}

// Polygon-too-large rule: real hardware DROPS any primitive whose X-span
// exceeds 1023 (or Y-span 511). These verts stay inside the 11-bit packet
// range (no coord wrap) yet span 1040 px, so silicon draws NOTHING while an
// emulator that skips the rule rasterises the clipped remainder. A prime
// suspect for the on-hardware "vertex flung across the screen" symptom.
fn test_gpu_tri_large_span() -> TestResult {
    let tri = prim::TriFlat::new([(-520, 40), (520, 8), (0, 88)], 0xc0, 0x40, 0xf0);
    expect_eq(
        0x02b7_edc5,
        gpu_draw_and_hash(&tri, prim::TriFlat::WORDS),
        "gpu tri large span",
    )
}

// Y-axis edge: mirror of the X edge/wrap cases -- a vertex far below the draw
// area. Confirms vertical clipping matches silicon.
fn test_gpu_tri_y_past_edge() -> TestResult {
    let tri = prim::TriFlat::new([(8, 8), (88, 8), (40, 300)], 0x30, 0xe0, 0x60);
    expect_eq(
        0x62d5_6b63,
        gpu_draw_and_hash(&tri, prim::TriFlat::WORDS),
        "gpu tri y past edge",
    )
}

// The player's EXACT primitive WITH the stretch geometry: a textured-gouraud
// triangle whose third vertex is flung far right. If the GPU mishandles a
// large textured span, this reproduces the explosion's primitive in isolation.
fn test_gpu_texgouraud_large_span() -> TestResult {
    let tpage = gpu_upload_tex15();
    let tri = prim::TriTexturedGouraud::new(
        [(4, 4), (92, 8), (400, 90)],
        [(0, 0), (15, 0), (8, 15)],
        [(0x80, 0x80, 0x80), (0xc0, 0x80, 0x40), (0x40, 0xc0, 0x80)],
        0,
        tpage,
    );
    expect_eq(
        0xc79f_5560,
        gpu_draw_and_hash(&tri, prim::TriTexturedGouraud::WORDS),
        "gpu texgouraud large span",
    )
}

// The player's EXACT primitive through the player's EXACT submit path:
// textured-gouraud via an ordering table + DMA linked-list, not a direct GP0
// push. The closest single test to how the model reaches the GPU each frame.
fn test_gpu_texgouraud_ot_dma() -> TestResult {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    let tpage = gpu_upload_tex15();
    gpu_draw_env_scratch();
    let mut tri = prim::TriTexturedGouraud::new(
        [(6, 6), (90, 14), (40, 90)],
        [(0, 0), (15, 0), (8, 15)],
        [(0x80, 0x80, 0x80), (0xc0, 0x80, 0x40), (0x40, 0xc0, 0x80)],
        0,
        tpage,
    );
    let mut ot = gpu::ot::OrderingTable::<4>::new();
    let mut frame = ot.frame();
    frame.add(0, &mut tri);
    frame.submit(&mut probe_gpu_dma());
    gpu_io::wait_command_ready();
    expect_eq(0x6392_570b, gpu_hash_scratch(), "gpu texgouraud ot dma")
}

// The cooked model textures are CLUT-indexed (the obsidian-wraith fixture is
// 8bpp / 256-colour), a DIFFERENT GPU read path than 15bpp direct colour: each
// texel is an index into a 256-entry palette. Upload an 8bpp texture + CLUT and
// draw a textured triangle through the palette.
fn test_gpu_8bpp_clut_tri() -> TestResult {
    // 16x16 8bpp indices (2 texels/halfword -> 8 halfwords wide).
    let mut idx = [0u8; 16 * 16];
    for (i, b) in idx.iter_mut().enumerate() {
        *b = ((i * 7) & 0xff) as u8;
    }
    psx_vram::upload_bytes(psx_vram::VramRect::new(832, 256, 8, 16), &idx);
    let mut pal = [psx_vram::Color555::raw(0); 256];
    for (i, c) in pal.iter_mut().enumerate() {
        let n = i as u8;
        *c = psx_vram::Color555::rgb5(n & 0x1f, (n >> 1) & 0x1f, (n >> 2) & 0x1f);
    }
    // CLUT is 256 entries wide (one row); place it low-left so x + 256 fits
    // VRAM and it clears the scratch/textures/framebuffers.
    let clut = Clut::new(0, 500);
    psx_vram::upload_clut(clut, &pal);
    let tpage = TexturePage::new(832, 256, TextureDepth::Bit8).uv_word(0);
    let tri = prim::TriTextured::new(
        [(8, 8), (88, 16), (40, 88)],
        [(0, 0), (15, 0), (8, 15)],
        clut.uv_word(),
        tpage,
        (0x80, 0x80, 0x80),
    );
    expect_eq(
        0x04ad_1fa1,
        gpu_draw_and_hash(&tri, prim::TriTextured::WORDS),
        "gpu 8bpp clut tri",
    )
}

// DMA linked-list stress: a deeper ordering table (8 primitives across several
// Z buckets) exercises the walker + chain termination harder than the 3-prim
// case. A malformed link or early DMA stop shows as a wrong readback.
fn test_gpu_big_ot() -> TestResult {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    let mut tris = [
        prim::TriFlat::new([(2, 2), (30, 6), (4, 40)], 0xff, 0x20, 0x20),
        prim::TriFlat::new([(34, 2), (62, 6), (36, 40)], 0x20, 0xff, 0x20),
        prim::TriFlat::new([(66, 2), (94, 6), (68, 40)], 0x20, 0x20, 0xff),
        prim::TriFlat::new([(2, 44), (30, 48), (4, 92)], 0xff, 0xff, 0x20),
        prim::TriFlat::new([(34, 44), (62, 48), (36, 92)], 0x20, 0xff, 0xff),
        prim::TriFlat::new([(66, 44), (94, 48), (68, 92)], 0xff, 0x20, 0xff),
        prim::TriFlat::new([(20, 20), (76, 30), (40, 80)], 0xa0, 0xa0, 0xa0),
        prim::TriFlat::new([(48, 8), (60, 60), (10, 70)], 0x60, 0xc0, 0x40),
    ];
    let mut ot = gpu::ot::OrderingTable::<8>::new();
    let mut frame = ot.frame();
    for (i, t) in tris.iter_mut().enumerate() {
        frame.add(i % 7, t);
    }
    frame.submit(&mut probe_gpu_dma());
    gpu_io::wait_command_ready();
    expect_eq(0x91a7_f548, gpu_hash_scratch(), "gpu big ot")
}

fn seed_gte_state() {
    write_control!(31, 0);

    write_data!(0, pack_gte_xy(-0x80, 0x40));
    write_data!(1, 0x0400);
    write_data!(2, pack_gte_xy(0x80, -0x40));
    write_data!(3, 0x0500);
    write_data!(4, pack_gte_xy(0x20, 0x90));
    write_data!(5, 0x0600);
    write_data!(6, 0x0040_4040);
    write_data!(8, 0x0800);
    write_data!(9, 0x0100);
    write_data!(10, 0x0200);
    write_data!(11, 0x0300);
    write_data!(12, pack_gte_xy(-16, 20));
    write_data!(13, pack_gte_xy(24, 36));
    write_data!(14, pack_gte_xy(48, 72));
    write_data!(16, 0x0400);
    write_data!(17, 0x0500);
    write_data!(18, 0x0600);
    write_data!(19, 0x0700);
    write_data!(20, 0x0010_1010);
    write_data!(21, 0x0020_2020);
    write_data!(22, 0x0030_3030);

    gte_scene::set_screen_offset(160 << 16, 120 << 16);
    gte_scene::set_projection_plane(256);
    gte_scene::load_rotation(&Mat3I16::IDENTITY);
    gte_scene::load_translation(Vec3I32::new(0, 0, 0x1000));

    write_control!(8, pack_gte_xy(0x1000, 0));
    write_control!(9, pack_gte_xy(0, 0));
    write_control!(10, pack_gte_xy(0x1000, 0));
    write_control!(11, pack_gte_xy(0, 0));
    write_control!(12, 0x1000);
    write_control!(13, 0);
    write_control!(14, 0);
    write_control!(15, 0);
    write_control!(16, pack_gte_xy(0x1000, 0));
    write_control!(17, pack_gte_xy(0, 0));
    write_control!(18, pack_gte_xy(0x1000, 0));
    write_control!(19, pack_gte_xy(0, 0));
    write_control!(20, 0x1000);
    write_control!(21, 0x20);
    write_control!(22, 0x20);
    write_control!(23, 0x20);
    write_control!(27, 0);
    write_control!(28, 0);
    write_control!(29, 0x0555);
    write_control!(30, 0x0400);
}

fn gte_flag_master_clear() -> bool {
    read_control!(31) & 0x8000_0000 == 0
}

fn test_spu_status_readable() -> TestResult {
    let observed = unsafe { psx_io::read_u16(psx_hw::spu::SPUSTAT) } as u32;
    if observed != 0xFFFF {
        TestResult::info(0, observed, "spustat")
    } else {
        TestResult::warn(0, observed, "open bus?")
    }
}

fn test_spu_voice_registers() -> TestResult {
    const VOICE_STRIDE: u32 = 0x10;
    const VOICE: u32 = 23;
    let base = psx_hw::spu::BASE + VOICE * VOICE_STRIDE;
    let mut observed = 0u32;

    unsafe {
        psx_io::write_u16(base, 0x1234);
        psx_io::write_u16(base + 2, 0x2345);
        psx_io::write_u16(base + 4, 0x1000);
        psx_io::write_u16(base + 6, 0x0040);
        psx_io::write_u16(base + 8, 0x8F1F);
        psx_io::write_u16(base + 10, 0x1F80);

        if psx_io::read_u16(base) == 0x1234 {
            observed |= 1 << 0;
        }
        if psx_io::read_u16(base + 2) == 0x2345 {
            observed |= 1 << 1;
        }
        if psx_io::read_u16(base + 4) == 0x1000 {
            observed |= 1 << 2;
        }
        if psx_io::read_u16(base + 6) == 0x0040 {
            observed |= 1 << 3;
        }
        if psx_io::read_u16(base + 8) == 0x8F1F {
            observed |= 1 << 4;
        }
        if psx_io::read_u16(base + 10) == 0x1F80 {
            observed |= 1 << 5;
        }
    }

    expect_eq(0x3F, observed, "voice")
}

fn test_spu_main_volume_roundtrip() -> TestResult {
    const MAIN_VOL_LEFT: u32 = psx_hw::spu::BASE + 0x180;
    const MAIN_VOL_RIGHT: u32 = psx_hw::spu::BASE + 0x182;

    unsafe {
        let old_left = psx_io::read_u16(MAIN_VOL_LEFT);
        let old_right = psx_io::read_u16(MAIN_VOL_RIGHT);

        psx_io::write_u16(MAIN_VOL_LEFT, 0x1234);
        psx_io::write_u16(MAIN_VOL_RIGHT, 0x2345);

        let mut observed = 0u32;
        if psx_io::read_u16(MAIN_VOL_LEFT) == 0x1234 {
            observed |= 1 << 0;
        }
        if psx_io::read_u16(MAIN_VOL_RIGHT) == 0x2345 {
            observed |= 1 << 1;
        }

        psx_io::write_u16(MAIN_VOL_LEFT, old_left);
        psx_io::write_u16(MAIN_VOL_RIGHT, old_right);

        expect_eq(0x03, observed, "main vol")
    }
}

/// SPU drill-in for the diverging SPU MAP scan. Voice 0: write 0xFFFF to
/// each of the eight per-voice halfword registers (offsets 0x0..=0xE) and
/// set one bit per offset that reads back 0xFFFF. PSoXide stores the full
/// 16 bits; hardware masks reserved bits in some registers, so the clear
/// bits localize which offsets diverge. Old values restored. INFO only.
fn test_spu_voice_writable_mask() -> TestResult {
    let voice0 = psx_hw::spu::BASE;
    let mut observed = 0u32;
    unsafe {
        for i in 0..8u32 {
            let addr = voice0 + i * 2;
            let old = psx_io::read_u16(addr);
            psx_io::write_u16(addr, 0xFFFF);
            if psx_io::read_u16(addr) == 0xFFFF {
                observed |= 1 << i;
            }
            psx_io::write_u16(addr, old);
        }
    }
    TestResult::info(0xFF, observed, "spu wr mask")
}

/// SPU drill-in companion: write 0xFFFF to voice 0 pitch (0x4) and ADSR1
/// (0x8) and report the raw readbacks packed `(pitch << 16) | adsr1`, so
/// the reserved-bit masks of two key registers are visible next to the
/// writable-bit mask. Old values restored. INFO only.
fn test_spu_voice_reg_readback() -> TestResult {
    let voice0 = psx_hw::spu::BASE;
    unsafe {
        let old_pitch = psx_io::read_u16(voice0 + 0x4);
        let old_adsr1 = psx_io::read_u16(voice0 + 0x8);
        psx_io::write_u16(voice0 + 0x4, 0xFFFF);
        psx_io::write_u16(voice0 + 0x8, 0xFFFF);
        let pitch = psx_io::read_u16(voice0 + 0x4) as u32;
        let adsr1 = psx_io::read_u16(voice0 + 0x8) as u32;
        psx_io::write_u16(voice0 + 0x4, old_pitch);
        psx_io::write_u16(voice0 + 0x8, old_adsr1);
        TestResult::info(0xFFFF_FFFF, (pitch << 16) | adsr1, "pitch|adsr1")
    }
}

// ============================================================
//  2026-06 hardware-accuracy pass -- on-device validation of the
//  SPU-RAM upload fix (the bug that droned/garbled audio on a real
//  console) and the GPU dither + mask-bit faithfulness fixes. These
//  use VRAM / SPU-RAM read-back so the same PASS/FAIL shows up on the
//  emulator and on silicon.
// ============================================================

/// FNV-1a over a slice of 32-bit words (low halfword first), matching
/// [`gpu_hash_scratch`]'s mixing so expected hashes are comparable.
/// FNV-1a over a slice of halfwords (same mixing constants).
fn fnv16_halfwords(hws: &[u16]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &h in hws {
        hash = (hash ^ h as u32).wrapping_mul(0x0100_0193);
    }
    hash
}

/// DMA `out.len()` words out of SPU RAM at byte address `addr` back into
/// main RAM (channel 4, from-device, block-sync). Arms SPUCNT DMA-Read
/// mode (bits 5..4 = 11) around the transfer, the read-side mirror of the
/// SDK's DMA upload.
pub(crate) fn spu_dma_read(addr: u32, out: &mut [u32]) {
    let words = out.len() as u32;
    let block_size: u32 = if words.is_multiple_of(16) {
        16
    } else if words.is_multiple_of(8) {
        8
    } else if words.is_multiple_of(4) {
        4
    } else if words.is_multiple_of(2) {
        2
    } else {
        1
    };
    // SPU->RAM DMA is only trustworthy with the memory controller's DMA
    // timing override armed (1F801014h bits 24-27 non-zero). The BIOS boot
    // value 0x200931E1 leaves it zero; in that mode silicon corrupts FIFO
    // block boundaries and the emulator reproduces the measured corruption
    // since the SCPH-9902 checkpoint. PX7 precision values 036-038 prove the
    // override reads RAM back faithfully on silicon. Restored afterwards so
    // the boot-mode shape stays observable to the precision scan, which
    // calls spu_dma_read_shape directly.
    unsafe {
        let boot = psx_io::read_u32(SPU_DELAY);
        psx_io::write_u32(SPU_DELAY, boot | 0x0200_0000);
        let _ = spu_dma_read_shape(addr, out, block_size);
        psx_io::write_u32(SPU_DELAY, boot);
    }
}

/// SPU->RAM DMA with an explicit BCR shape. Hardware's unstable read mode
/// corrupts FIFO boundaries, so the precision capture must compare one large
/// block with several small blocks while holding the payload constant.
fn spu_dma_read_shape(addr: u32, out: &mut [u32], block_size: u32) -> u32 {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
    let words = out.len() as u32;
    debug_assert!(block_size != 0 && words.is_multiple_of(block_size));
    let block_count = words / block_size;
    unsafe {
        let spucnt = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, spucnt);
        // SPUCNT is applied asynchronously. Starting DMA before SPUSTAT
        // reflects Stop returns FIFO/transition garbage even when the memory
        // control delay is configured for stable reads.
        let mut stop_guard = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != spucnt & 0x003F && stop_guard < 0xFFFF {
            stop_guard += 1;
        }
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        psx_io::write_u16(TRANSFER_ADDR, (addr / 8) as u16);
        psx_io::write_u16(SPUCNT, spucnt | 0x0030); // transfer mode = DMA Read
        let mut mode_guard = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != (spucnt | 0x0030) & 0x003F
            && mode_guard < 0xFFFF
        {
            mode_guard += 1;
        }
        // Do not wait for SPUSTAT's DMA-request bits before arming DMA. The
        // SCPH-9902 capture showed that the low-six mode mirror settles after
        // 24-27 polls, while bits 9/7 remain clear until the DMA side is armed.
        dma::enable_channel(dma::Channel::Spu);
        dma::raw::set_address(dma::Channel::Spu, out.as_ptr() as u32);
        dma::raw::set_size(
            dma::Channel::Spu,
            dma::size_blocks(block_size as u16, block_count as u16),
        );
        // from-device (no CHCR_TO_DEVICE), block-sync, start.
        dma::raw::set_control(
            dma::Channel::Spu,
            psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );
        // Bounded wait: never spin forever on silicon -- if SPU->RAM DMA
        // stalls the test fails gracefully (zeroed read-back) instead of
        // hanging the whole suite at a black screen.
        let mut guard = 0u32;
        while dma::is_busy(dma::Channel::Spu) && guard < 1_000_000 {
            guard += 1;
        }
        psx_io::write_u16(SPUCNT, spucnt); // back to Stop
                                           // Preserve both bounded counters independently. A DMA-read mode that
                                           // intentionally remains gated until channel arm reports `FFFF` in the
                                           // low half while the preceding Stop transition still retains its
                                           // useful sample-boundary count in the high half.
        (stop_guard << 16) | mode_guard
    }
}

/// AUDIO: the bug the user heard on hardware was ADPCM never reaching SPU
/// RAM (drone / garble). Upload a known 64-byte block through the fixed
/// `upload_adpcm` (DMA path) to SPU RAM above the capture region, DMA it
/// back, and hash-compare. PASS proves the upload path lands on silicon.
fn test_spu_ram_dma_roundtrip() -> TestResult {
    let mut src = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        src[i] = 0xC0DE_0000u32.wrapping_add((i as u32) * 0x111);
        i += 1;
    }
    let dest: u32 = 0x3000; // clear of the 0x000-0xFFF capture buffers
    let bytes = unsafe { core::slice::from_raw_parts(src.as_ptr() as *const u8, 64) };
    psx_spu::upload_adpcm(SpuAddr::new(dest), bytes);
    let mut back = [0u32; 16];
    spu_dma_read(dest, &mut back);
    expect_eq(fnv32_words(&src), fnv32_words(&back), "spu dma upload")
}

/// AUDIO: the same SPU-RAM landing check via the manual-write FIFO -- the
/// path the SDK's PIO fallback uses and the one the original bug skipped
/// (it never armed Manual-Write mode, so the FIFO writes were dropped).
/// Arm mode 01, push 8 halfwords, DMA them back, hash-compare.
fn test_spu_ram_manual_fifo_roundtrip() -> TestResult {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL, TRANSFER_DATA};
    let mut src = [0u16; 8];
    let mut i = 0;
    while i < 8 {
        src[i] = 0xBEEFu16.wrapping_add((i as u16) * 0x101);
        i += 1;
    }
    let dest: u32 = 0x3400;
    unsafe {
        psx_io::write_u16(TRANSFER_ADDR, (dest / 8) as u16);
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        let spucnt = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, spucnt | 0x0010); // transfer mode = Manual Write
                                                    // The SPUCNT low-6-bit SPUSTAT mirror takes 24-27 polls to settle on
                                                    // silicon; FIFO halfwords pushed before Manual-Write mode is active
                                                    // are dropped -- the very bug this test exists to catch.
        let mut settle = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x003F != (spucnt | 0x0010) & 0x003F && settle < 0xFFFF {
            settle += 1;
        }
        for &hw in src.iter() {
            psx_io::write_u16(TRANSFER_DATA, hw);
        }
        // Let the FIFO drain (SPUSTAT bit 10) before leaving the mode.
        let mut drain = 0u32;
        while psx_io::read_u16(SPUSTAT) & 0x0400 != 0 && drain < 0xFFFF {
            drain += 1;
        }
        psx_io::write_u16(SPUCNT, spucnt); // back to Stop
                                           // Leave the transfer type NORMAL (0004h): parking it at 0 poisons all
                                           // later sample-RAM access on silicon (SB2 finding, 2026-08-02).
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
    }
    spin(4000); // let the FIFO drain to SPU RAM on hardware
    let mut back = [0u32; 4];
    spu_dma_read(dest, &mut back);
    let mut got = [0u16; 8];
    let mut j = 0;
    while j < 4 {
        got[j * 2] = (back[j] & 0xFFFF) as u16;
        got[j * 2 + 1] = (back[j] >> 16) as u16;
        j += 1;
    }
    expect_eq(
        fnv16_halfwords(&src),
        fnv16_halfwords(&got),
        "spu fifo upload",
    )
}

/// Values 128..191: console identity, then bit-exact raster hashes.
///
/// Identity first, because a capture that cannot say which console and BIOS
/// produced it is much harder to trust later; these were previously encoded
/// only in a hand-written filename.
///
/// Then hashes. A 32-bit hash covers a whole 96x96 VRAM region, which makes it
/// the cheapest coverage per payload byte in the whole schema: this is what
/// caught the triangle rasterizer being Redux-shaped rather than silicon.
/// Timing tells you how long a primitive took; only a hash tells you it drew
/// the right pixels.
fn precision_identity_and_raster(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    // BIOS identity, read raw so the host identifies the machine without the
    // guest parsing strings. TWO regions are sampled deliberately: 0x100 holds
    // the build date and maker string, 0x7FF32 the "System ROM Version" text.
    // Sampling both is a hedge, because these reads return zero on the
    // emulator's side-loaded HLE path (which maps no BIOS ROM) and so cannot
    // be validated before a burn. If one region reads zero on console, the
    // other still identifies the machine.
    for base in [0xBFC0_0100u32, 0xBFC7_FF30] {
        let mut offset = 0u32;
        while offset < 4 {
            let word = unsafe { psx_io::read_u32(base + offset * 4) };
            push_precision(values, next, word);
            offset += 1;
        }
    }
    // GPUSTAT at rest, plus the MDEC status word after a reset. Both identify
    // silicon revision behaviour that timing alone cannot separate.
    push_precision(values, next, gpu_io::status().bits());
    push_precision(values, next, mdec_status());

    // 22 raster hashes. Each draws into the off-screen 96x96 scratch through
    // the same path the GPU conformance cases use, so a divergence localises
    // to one primitive rather than to "the rasterizer".
    for hash in raster_hashes() {
        push_precision(values, next, hash);
    }

    // Pad to the fixed schema length. Explicit rather than implicit: the
    // assert in run_precision_scan is what catches a miscount.
    while *next < PRECISION_VALUE_COUNT {
        push_precision(values, next, 0);
    }
}

/// Bit-exact hashes for one instance of each primitive family.
fn raster_hashes() -> [u32; 22] {
    use psx_gpu::prim::{QuadFlat, QuadGouraud, TriFlat, TriGouraud};

    let mut out = [0u32; 22];
    let mut index = 0usize;

    // Flat triangles at several coverage shapes: thin, wide, and off-edge,
    // where edge-rule differences show up most sharply.
    for corners in [
        [(8, 8), (88, 16), (40, 88)],
        [(8, 8), (88, 8), (8, 88)],
        [(4, 4), (92, 6), (48, 10)],
        [(48, 4), (50, 92), (46, 92)],
    ] {
        let tri = TriFlat::new(corners, 0xC0, 0x40, 0x80);
        out[index] = gpu_draw_and_hash(&tri, TriFlat::WORDS);
        index += 1;
    }
    // Gouraud triangles: interpolation and dither interact here.
    for corners in [[(8, 8), (88, 16), (40, 88)], [(2, 2), (94, 4), (48, 94)]] {
        let tri = TriGouraud::new(corners, [(0xFF, 0, 0), (0, 0xFF, 0), (0, 0, 0xFF)]);
        out[index] = gpu_draw_and_hash(&tri, TriGouraud::WORDS);
        index += 1;
    }
    // Flat quads, including the degenerate and reordered cases that decide
    // which diagonal the hardware splits along.
    for corners in [
        [(8, 8), (88, 8), (8, 88), (88, 88)],
        [(8, 8), (88, 16), (16, 88), (88, 88)],
        [(4, 4), (92, 4), (4, 92), (92, 92)],
    ] {
        let quad = QuadFlat::new(corners, 0x20, 0xC0, 0x60);
        out[index] = gpu_draw_and_hash(&quad, QuadFlat::WORDS);
        index += 1;
    }
    {
        let corners = [(8, 8), (88, 8), (8, 88), (88, 88)];
        let quad = QuadGouraud::new(
            corners,
            [(0xFF, 0, 0), (0, 0xFF, 0), (0, 0, 0xFF), (0xFF, 0xFF, 0)],
        );
        out[index] = gpu_draw_and_hash(&quad, QuadGouraud::WORDS);
        index += 1;
    }
    // Remaining slots stay zero: reserved so adding a primitive family later
    // does not shift the meaning of the hashes already recorded above.
    while index < out.len() {
        out[index] = 0;
        index += 1;
    }
    out
}

fn push_precision(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize, value: u32) {
    values[*next] = value;
    *next += 1;
}

/// Values 0..42: compare SPU RAM DMA-read corruption under the boot-time
/// memory-control setting against the documented stable-read setting. PSX-SPX
/// notes that 1F801014h bits 24..27 select whether the first FIFO halfword of
/// each block is dirty. Capturing every returned word reveals the exact shape.
fn precision_spu(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL, TRANSFER_DATA};

    let original_delay = unsafe { psx_io::read_u32(SPU_DELAY) };
    let packed_status =
        || unsafe { ((psx_io::read_u16(SPUCNT) as u32) << 16) | psx_io::read_u16(SPUSTAT) as u32 };
    push_precision(values, next, original_delay);
    push_precision(values, next, packed_status());

    let mut src = [0u32; 16];
    for (index, word) in src.iter_mut().enumerate() {
        *word = 0xC0DE_0000u32.wrapping_add(index as u32 * 0x111);
    }
    let dest = 0x3800;
    let bytes = unsafe { core::slice::from_raw_parts(src.as_ptr() as *const u8, 64) };
    psx_spu::upload_adpcm(SpuAddr::new(dest), bytes);

    let mut boot_single = [0u32; 16];
    let boot_single_waits = spu_dma_read_shape(dest, &mut boot_single, 16);
    for word in boot_single {
        push_precision(values, next, word);
    }
    push_precision(values, next, boot_single_waits);

    let mut boot_four = [0u32; 16];
    let boot_four_waits = spu_dma_read_shape(dest, &mut boot_four, 4);
    for word in boot_four {
        push_precision(values, next, word);
    }
    push_precision(values, next, boot_four_waits);

    // Preserve all BIOS-programmed wait fields and only make the documented
    // nonzero nibble explicit for the stable comparison. Hashes are enough
    // here: the two boot-mode arrays above retain the exact corruption shape.
    let stable_delay = original_delay | 0x0200_0000;
    unsafe { psx_io::write_u32(SPU_DELAY, stable_delay) };
    spin(64);
    push_precision(values, next, unsafe { psx_io::read_u32(SPU_DELAY) });
    let mut stable_single = [0u32; 16];
    let _ = spu_dma_read_shape(dest, &mut stable_single, 16);
    push_precision(values, next, fnv32_words(&stable_single));
    let mut stable_four = [0u32; 16];
    let _ = spu_dma_read_shape(dest, &mut stable_four, 4);
    push_precision(values, next, fnv32_words(&stable_four));

    let fifo_dest = 0x3C00;
    unsafe {
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        psx_io::write_u16(TRANSFER_ADDR, (fifo_dest / 8) as u16);
        let stopped = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, stopped | 0x0010);
        for index in 0..8u16 {
            psx_io::write_u16(TRANSFER_DATA, 0xBEEFu16.wrapping_add(index * 0x101));
        }
        psx_io::write_u16(SPUCNT, stopped);
    }
    spin(4000);
    let mut fifo_read = [0u32; 4];
    spu_dma_read(fifo_dest, &mut fifo_read);
    for word in fifo_read {
        push_precision(values, next, word);
    }
    unsafe { psx_io::write_u32(SPU_DELAY, original_delay) };
}

/// Values 43..60: raw GPUSTAT transitions. Three reads after each GP1 DMA
/// direction write expose whether the D0-vs-E4 result is a delayed latch;
/// the IRQ reads retain the command-FIFO set/ack transition shape.
fn precision_gpu(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    gpu_io::write_display_control(0x0200_0000);
    push_precision(values, next, gpu_io::status().bits());
    gpu_io::write_command(0x1F00_0000);
    for _ in 0..3 {
        push_precision(values, next, gpu_io::status().bits());
    }
    gpu_io::write_display_control(0x0200_0000);
    for _ in 0..2 {
        push_precision(values, next, gpu_io::status().bits());
    }
    for dir in 0..4u32 {
        gpu_io::write_display_control(0x0400_0000 | dir);
        for _ in 0..3 {
            push_precision(values, next, gpu_io::status().bits());
        }
    }
    gpu_io::write_display_control(0x0400_0002);
}

/// Values 61..72: exact Timer 2 state before and after target/FFFF events.
/// The two consecutive mode reads expose the read-to-clear flags. I_STAT is
/// cleared before each half and masked to its eleven implemented source bits,
/// avoiding a stale GPU IRQ and the open-bus upper half seen in the prior run.
fn precision_timer(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    const IRQ_SOURCE_MASK: u32 = 0x07FF;
    irq::acknowledge(IRQ_SOURCE_MASK);
    timers::set_target(timers::Timer::Timer2, 32);
    timers::set_mode(
        timers::Timer::Timer2,
        TIMER_MODE_RESET_AT_TARGET | TIMER_MODE_IRQ_ON_TARGET,
    );
    timers::set_counter(timers::Timer::Timer2, 0);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::counter(timers::Timer::Timer2) as u32);
    spin(8192);
    push_precision(values, next, timers::counter(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, irq::pending() & IRQ_SOURCE_MASK);

    irq::acknowledge(IRQ_SOURCE_MASK);
    timers::set_mode(timers::Timer::Timer2, TIMER_MODE_IRQ_ON_WRAP);
    timers::set_counter(timers::Timer::Timer2, 0xFFF0);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::counter(timers::Timer::Timer2) as u32);
    spin(8192);
    push_precision(values, next, timers::counter(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, irq::pending() & IRQ_SOURCE_MASK);
    timers::set_mode(timers::Timer::Timer2, 0);
}

/// Reproduce the controlled scene-A NCLIP sequence and return MAC0 after an
/// exact result-read gap. The prior run proved that SXY0/1/2 read back intact
/// while MAC0 remains at the same partial accumulation through 48 NOPs; the
/// settled reference uses 64. Sweeping every gap from 47 through 64 identifies
/// the precise silicon completion edge without spending another QR page.
macro_rules! nclip_scene_a_settle_probe {
    ($gap:literal) => {{
        const S0: u32 = 0x006E_0095;
        const S1: u32 = 0xFFE2_0094;
        const S2: u32 = 0xFFDE_00DC;

        write_control!(31, 0);
        write_data!(12, S0);
        write_data!(13, S1);
        write_data!(14, S2);
        unsafe { gte_ops::screen_winding() };
        gte_nops!(64);
        let _ = read_data!(24);

        // Poison MAC0 with the reverse winding, then restore scene A using
        // the exact controlled-prestate sequence from cases 126..134.
        write_data!(13, S2);
        write_data!(14, S1);
        unsafe { gte_ops::screen_winding() };
        gte_nops!(64);
        write_data!(13, S1);
        write_data!(14, S2);
        unsafe { gte_ops::screen_winding() };
        gte_nops!($gap);
        read_data!(24)
    }};
}

/// Values 73..127: unresolved GTE state, SPU register masks, and OTC DMA
/// completion. Already-matching MVMVA/compose paths are intentionally omitted
/// so the fixed QR budget targets differences that can still improve PSoXide.
fn precision_remaining(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    // RTPS consumed fresh V0 at every tested gap, including zero. Reuse those
    // resolved 18 words to locate the scene-A NCLIP completion edge exactly.
    for mac0 in [
        nclip_scene_a_settle_probe!(47),
        nclip_scene_a_settle_probe!(48),
        nclip_scene_a_settle_probe!(49),
        nclip_scene_a_settle_probe!(50),
        nclip_scene_a_settle_probe!(51),
        nclip_scene_a_settle_probe!(52),
        nclip_scene_a_settle_probe!(53),
        nclip_scene_a_settle_probe!(54),
        nclip_scene_a_settle_probe!(55),
        nclip_scene_a_settle_probe!(56),
        nclip_scene_a_settle_probe!(57),
        nclip_scene_a_settle_probe!(58),
        nclip_scene_a_settle_probe!(59),
        nclip_scene_a_settle_probe!(60),
        nclip_scene_a_settle_probe!(61),
        nclip_scene_a_settle_probe!(62),
        nclip_scene_a_settle_probe!(63),
        nclip_scene_a_settle_probe!(64),
    ] {
        push_precision(values, next, mac0);
    }
    run_op_full_seed();
    push_precision(values, next, read_data!(25));
    push_precision(values, next, read_data!(26));
    push_precision(values, next, read_data!(27));
    run_op_full_seed_settled();
    push_precision(values, next, read_data!(25));
    push_precision(values, next, read_data!(26));
    push_precision(values, next, read_data!(27));

    // Voice 0 raw masks: case 164 says which offsets differ, but not what the
    // masked values are. Preserve all eight readbacks after writing FFFF.
    let voice0 = psx_hw::spu::BASE;
    unsafe {
        for index in 0..8u32 {
            let addr = voice0 + index * 2;
            let old = psx_io::read_u16(addr);
            psx_io::write_u16(addr, 0xFFFF);
            push_precision(values, next, psx_io::read_u16(addr) as u32);
            psx_io::write_u16(addr, old);
        }
    }

    // OTC case 40 differs by a single busy poll. Consecutive CHCR reads show
    // exactly when START/TRIGGER clear; final registers and chain endpoints
    // distinguish status latency from transfer completion or pointer updates.
    static mut PRECISION_OT: [u32; 16] = [0; 16];
    unsafe {
        let ptr = (&raw mut PRECISION_OT) as *mut u32;
        for index in 0..16 {
            ptr::write_volatile(ptr.add(index), 0);
        }
        dma::enable_channel(dma::Channel::OrderingTableClear);
        dma::raw::set_address(dma::Channel::OrderingTableClear, ptr.add(15) as u32);
        dma::raw::set_size(dma::Channel::OrderingTableClear, dma::size_words(16));
        push_precision(values, next, dma::control(dma::Channel::OrderingTableClear));
        dma::raw::set_control(
            dma::Channel::OrderingTableClear,
            psx_hw::dma::CHCR_STEP_BACKWARD
                | psx_hw::dma::CHCR_SYNC_MANUAL
                | psx_hw::dma::CHCR_START
                | psx_hw::dma::CHCR_TRIGGER,
        );
        for _ in 0..6 {
            push_precision(values, next, dma::control(dma::Channel::OrderingTableClear));
        }
        let mut guard = 0u32;
        while dma::is_busy(dma::Channel::OrderingTableClear) && guard < 0xFFFF {
            guard += 1;
        }
        push_precision(values, next, dma::address(dma::Channel::OrderingTableClear));
        push_precision(
            values,
            next,
            psx_io::read_u32(dma::Channel::OrderingTableClear.register_base() + 4),
        );
        push_precision(values, next, guard);
        push_precision(values, next, ptr::read_volatile(ptr));
        push_precision(values, next, ptr::read_volatile(ptr.add(15)));
    }

    push_precision(
        values,
        next,
        scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
    );
    push_precision(
        values,
        next,
        scene_nclip_mac0(0x0073_00d5, 0xffde_00dc, 0xffd8_0130),
    );
    push_precision(
        values,
        next,
        scene_nclip_mac0(0x0079_011f, 0xffd8_0130, 0xffd2_0194),
    );

    // Cases 80..82 only diverge on silicon after the exact case-79 RTPT
    // predecessor; controlled NCLIP reproductions already match. Repeat that
    // boundary, then retain a sequential A/B/C run to reveal carried state.
    for _ in 0..4 {
        scene_rtpt(RTPT_E);
        let _ = read_data!(19);
        push_precision(
            values,
            next,
            scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
        );
    }
    scene_rtpt(RTPT_E);
    let _ = read_data!(19);
    push_precision(
        values,
        next,
        scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
    );
    push_precision(
        values,
        next,
        scene_nclip_mac0(0x0073_00d5, 0xffde_00dc, 0xffd8_0130),
    );
    push_precision(
        values,
        next,
        scene_nclip_mac0(0x0079_011f, 0xffd8_0130, 0xffd2_0194),
    );
    push_precision(
        values,
        next,
        scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
    );
}

/// Read one 32-bit VRAM word (two 15bpp pixels) at `(vx, vy)` via GP0 0xC0
/// + GPUREAD: low halfword is `(vx, vy)`, high is `(vx + 1, vy)`.
fn gpu_read_word_at(vx: u16, vy: u16) -> u32 {
    gpu_io::wait_command_ready();
    gpu_io::write_command(0xC000_0000);
    gpu_io::write_command(((vy as u32) << 16) | vx as u32);
    gpu_io::write_command((1u32 << 16) | 2); // 2 wide, 1 tall
    let mut guard = 0u32;
    while gpu_io::status().bits() & (1 << 27) == 0 && guard < 100_000 {
        guard += 1;
    }
    gpu_io::read_data()
}

/// CPU->VRAM block transfer (GP0 0xA0). Honours the current GP0 0xE6 mask
/// state on silicon (and now in the emulator).
fn gpu_cpu_to_vram(vx: u16, vy: u16, w: u16, h: u16, data: &[u32]) {
    gpu_io::wait_command_ready();
    gpu_io::write_command(0xA000_0000);
    gpu_io::write_command(((vy as u32) << 16) | vx as u32);
    gpu_io::write_command(((h as u32) << 16) | w as u32);
    for &d in data {
        gpu_io::write_command(d);
    }
    gpu_io::wait_command_ready();
}

/// VISUAL: ordered dither. With dithering on, a flat mid-grey Gouraud fill
/// must resolve to the signed 4x4 checkerboard, NOT a uniform value. Colour
/// 120 sits on an 8-boundary, so the negative matrix offsets round it down
/// to channel 14 and the non-negative ones to 15. Scratch (4,4)/(5,4) land
/// on matrix phases (0,0) and (1,0) -> pixels 0x39CE and 0x3DEF. The scratch
/// origin (512,256) is 4-aligned so the VRAM dither phase equals the scratch
/// phase.
fn test_gpu_dither_checkerboard() -> TestResult {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    gpu_io::write_command(0xE100_0000 | (1 << 9)); // draw mode: dither ON
    let mid = (120u8, 120u8, 120u8);
    let tri0 = prim::TriGouraud::new(
        [(0, 0), (GPU_SW as i16 - 1, 0), (0, GPU_SH as i16 - 1)],
        [mid, mid, mid],
    );
    let tri1 = prim::TriGouraud::new(
        [
            (GPU_SW as i16 - 1, 0),
            (0, GPU_SH as i16 - 1),
            (GPU_SW as i16 - 1, GPU_SH as i16 - 1),
        ],
        [mid, mid, mid],
    );
    gpu_send_prim(&tri0, prim::TriGouraud::WORDS);
    gpu_send_prim(&tri1, prim::TriGouraud::WORDS);
    gpu_io::wait_command_ready();
    gpu_io::write_command(0xE100_0000); // dither OFF (restore default)
    let observed = gpu_read_word_at(GPU_SX + 4, GPU_SY + 4);
    expect_eq(0x3DEF_39CE, observed, "gpu dither")
}

/// VISUAL: the mask bit on CPU->VRAM copies. Upload colour A with set-mask
/// (forces bit15=1), then upload colour B over the same pixels with
/// check-mask (must skip already-masked pixels). The first colour has to
/// survive: silicon honours the mask on the copy command, and now so does
/// the emulator.
fn test_gpu_cpu_vram_upload_mask() -> TestResult {
    let (px, py) = (GPU_SX, GPU_SY);
    let a: u32 = 0x168A; // colour A (bgr15)
    let b: u32 = 0x7FFF; // colour B -- would overwrite if the mask were ignored
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_io::write_command(0xE600_0000 | 1); // set-mask on, check-mask off
    gpu_cpu_to_vram(px, py, 2, 1, &[a | (a << 16)]);
    gpu_io::write_command(0xE600_0000 | 2); // check-mask on
    gpu_cpu_to_vram(px, py, 2, 1, &[b | (b << 16)]);
    gpu_io::write_command(0xE600_0000); // restore
    let observed = gpu_read_word_at(px, py);
    expect_eq(0x968A_968A, observed, "gpu copy mask")
}

fn test_pad_poll() -> TestResult {
    pad_poll_result(psx_engine::PadState::NONE)
}

fn pad_poll_result(pad: psx_engine::PadState) -> TestResult {
    if pad.is_connected() {
        TestResult::info(1, pad.id_low as u32, "connected")
    } else {
        TestResult::info(0, 0, "optional")
    }
}

/// Strict port-1 handshake under the shipping no-wait timing: a connected
/// controller must answer with the 0x5A magic and a classified mode. A desync
/// (wrong magic / Unknown mode) is a hard FAIL, not the old benign "optional".
/// An empty port stays optional -- absence is not a failure.
fn test_pad_handshake_strict() -> TestResult {
    let raw = psx_pad::poll_port1_diagnostics(psx_pad::DEFAULT_SETUP_SPINS, 0);
    let observed = ((raw.id_high as u32) << 8) | raw.id_low as u32;
    if !raw.mode.is_connected() {
        return TestResult::info(0, observed, "optional");
    }
    if raw.id_high == 0x5A && !matches!(raw.mode, psx_pad::PadMode::Unknown) {
        TestResult::pass(0x5A41, observed, "clean")
    } else {
        TestResult::fail(0x5A41, observed, "desync")
    }
}

/// The fixed setup+inter-byte diagnostic timing must read a connected pad as
/// cleanly as the base no-wait timing (it is the same no-wait exchange plus
/// fixed delays, no `/ACK`/CTRL machinery). PASS clean, WARN if a connected pad
/// desyncs under the delays, optional when nothing is plugged in.
fn test_pad_diag_timing() -> TestResult {
    let raw = psx_pad::poll_port1_diagnostics(2048, 2048);
    let observed = ((raw.id_high as u32) << 8) | raw.id_low as u32;
    if !raw.mode.is_connected() {
        return TestResult::info(0, observed, "optional");
    }
    if raw.id_high == 0x5A && !matches!(raw.mode, psx_pad::PadMode::Unknown) {
        TestResult::pass(0x5A41, observed, "clean")
    } else {
        TestResult::warn(0x5A41, observed, "diag desync")
    }
}

/// DualShock analog handshake: request analog mode via the config transaction
/// games use, then confirm the pad reports ID 0x73 with stick bytes. A
/// digital-only pad is INFO, not a failure; an empty port is optional.
fn test_pad_analog_handshake() -> TestResult {
    if !psx_pad::poll_port1().is_connected() {
        return TestResult::info(0, 0, "optional");
    }
    let became_analog = psx_pad::enable_analog_port1();
    let raw = psx_pad::poll_port1_diagnostics(psx_pad::DEFAULT_SETUP_SPINS, 0);
    let observed =
        ((raw.id_low as u32) << 16) | ((raw.sticks.left_x as u32) << 8) | raw.sticks.left_y as u32;
    if became_analog && raw.id_low == 0x73 {
        TestResult::pass(0x73, observed, "analog")
    } else if raw.mode.is_connected() {
        TestResult::info(raw.id_low as u32, observed, "digital-only")
    } else {
        TestResult::warn(0x73, observed, "lost pad")
    }
}

fn test_sio_register_latches() -> TestResult {
    unsafe {
        let old_mode = psx_io::read_u16(psx_hw::sio::sio0::MODE);
        let old_ctrl = psx_io::read_u16(psx_hw::sio::sio0::CTRL);
        let old_baud = psx_io::read_u16(psx_hw::sio::sio0::BAUD);

        psx_io::write_u16(psx_hw::sio::sio0::MODE, 0x000D);
        psx_io::write_u16(psx_hw::sio::sio0::BAUD, 0x0088);
        psx_io::write_u16(psx_hw::sio::sio0::CTRL, 0x0003);

        let mut observed = 0u32;
        if psx_io::read_u16(psx_hw::sio::sio0::MODE) == 0x000D {
            observed |= 1 << 0;
        }
        if psx_io::read_u16(psx_hw::sio::sio0::BAUD) == 0x0088 {
            observed |= 1 << 1;
        }
        if psx_io::read_u16(psx_hw::sio::sio0::CTRL) & 0x0003 == 0x0003 {
            observed |= 1 << 2;
        }

        psx_io::write_u16(psx_hw::sio::sio0::MODE, old_mode);
        psx_io::write_u16(psx_hw::sio::sio0::BAUD, old_baud);
        psx_io::write_u16(psx_hw::sio::sio0::CTRL, old_ctrl);

        expect_eq(0x07, observed, "sio regs")
    }
}

fn test_gpu_draw_area_command() -> TestResult {
    probe_gpu!(gpu);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    let observed = gpu_io::status().bits() & ((1 << 26) | (1 << 28));
    let expected = (1 << 26) | (1 << 28);
    expect_eq(expected, observed, "draw area")
}

fn test_gpu_dma_direction_after_otc() -> TestResult {
    static mut OT: [u32; 4] = [0; 4];
    // The helper is bounded now, so a wedged OTC channel reports instead
    // of hanging the battery; bit 2 carries whether it completed.
    // SAFETY: single-threaded test battery, no other live borrow of OT.
    let cleared = dma::clear_ordering_table(unsafe { &mut OT });
    gpu_io::write_display_control(0x0400_0000 | 2);
    let observed = ((gpu_io::status().bits() >> 29) & 0b11) | ((cleared as u32) << 2);
    expect_eq(0b110, observed, "dma dir | otc done")
}

fn test_gpu_dma_direction_mode_latch() -> TestResult {
    let mut observed = 0u32;
    for direction in 0..4u32 {
        gpu_io::write_display_control(0x0400_0000 | direction);
        if ((gpu_io::status().bits() >> 29) & 0b11) == direction {
            observed |= 1 << direction;
        }
    }
    gpu_io::write_display_control(0x0400_0000 | 2);
    // Racy on silicon: the GPUSTAT bits 29-30 readback lags the GP1(04)
    // write through the FIFO (the readback probe disagreed with this test
    // within one run). Report until FIFO latency is modelled.
    TestResult::info(0x0F, observed, "racy fifo")
}

fn test_gpu_gp1_info_environment_readback() -> TestResult {
    probe_gpu!(gpu);
    let texture_window = 0xE200_0000 | 0x0003 | (0x0005 << 5) | (0x0007 << 10) | (0x0009 << 15);
    let draw_area_top_left = 0xE300_0000 | 8 | (16 << 10);
    let draw_area_bottom_right = 0xE400_0000 | 300 | (220 << 10);
    let draw_offset = 0xE500_0000 | ((-12i32 as u32) & 0x7FF) | (34 << 11);

    gpu_io::write_command(texture_window);
    gpu_io::write_command(draw_area_top_left);
    gpu_io::write_command(draw_area_bottom_right);
    gpu_io::write_command(draw_offset);

    gpu_io::write_display_control(0x1000_0002);
    let texture_window_read = gpu_io::read_data();
    gpu_io::write_display_control(0x1000_0003);
    let top_left_read = gpu_io::read_data();
    gpu_io::write_display_control(0x1000_0004);
    let bottom_right_read = gpu_io::read_data();
    gpu_io::write_display_control(0x1000_0005);
    let offset_read = gpu_io::read_data();

    gpu_io::write_command(0xE200_0000);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));

    let mut observed = 0u32;
    if texture_window_read == (texture_window & 0x000F_FFFF) {
        observed |= 1 << 0;
    }
    if top_left_read == (draw_area_top_left & 0x000F_FFFF) {
        observed |= 1 << 1;
    }
    if bottom_right_read == (draw_area_bottom_right & 0x000F_FFFF) {
        observed |= 1 << 2;
    }
    if offset_read == (draw_offset & 0x003F_FFFF) {
        observed |= 1 << 3;
    }
    expect_eq(0x0F, observed, "gp1 info")
}

fn test_timer2_target_sticky() -> TestResult {
    timers::set_target(timers::Timer::Timer2, 32);
    timers::set_counter(timers::Timer::Timer2, 0);
    timers::set_mode(timers::Timer::Timer2, TIMER_MODE_RESET_AT_TARGET);
    spin(8192);
    let mode = timers::mode(timers::Timer::Timer2);
    let target_hit = mode & TIMER_MODE_REACHED_TARGET != 0;
    let counter = timers::counter(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0x0000);
    // The target itself is visible for one source tick before reset. Values
    // 0..=32 therefore prove the counter stayed in the target-reset cycle;
    // only a value above target indicates that reset-at-target failed.
    let observed = (target_hit as u32) | ((((counter as u32) <= 32) as u32) << 1);
    expect_eq(0x3, observed, "target")
}

fn test_timer_mode_write_resets_counter() -> TestResult {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0x6000);
    timers::set_mode(timers::Timer::Timer2, TIMER_MODE_CLOCK_SOURCE_2);
    let counter = timers::counter(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0);
    if counter <= 4 {
        TestResult::pass(4, counter as u32, "reset")
    } else {
        TestResult::fail(4, counter as u32, "reset")
    }
}

fn test_timer_mode_read_clears_sticky() -> TestResult {
    timers::set_target(timers::Timer::Timer2, 24);
    timers::set_mode(timers::Timer::Timer2, TIMER_MODE_RESET_AT_TARGET);
    timers::set_counter(timers::Timer::Timer2, 0);
    spin(8192);
    let before = timers::mode(timers::Timer::Timer2);
    let after = timers::mode(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0);
    let observed = ((before & TIMER_MODE_REACHED_TARGET != 0) as u32)
        | (((after & TIMER_MODE_REACHED_TARGET == 0) as u32) << 1);
    TestResult::info(0x3, observed, "mode read")
}

fn test_timer2_sync_stop_vs_free_run() -> TestResult {
    let stopped = timer_delta(
        timers::Timer::Timer2,
        TIMER_MODE_SYNC_ENABLE | TIMER_MODE_SYNC_MODE_1,
        4096,
    );
    let running = timer_delta(timers::Timer::Timer2, TIMER_MODE_SYNC_ENABLE, 4096);
    timers::set_mode(timers::Timer::Timer2, 0);
    let observed = ((stopped == 0) as u32) | (((running > 0) as u32) << 1);
    TestResult::info(0x3, observed, "sync")
}

fn test_timer2_clock_divider() -> TestResult {
    let fast = timer_delta(timers::Timer::Timer2, 0, 8192);
    let slow = timer_delta(timers::Timer::Timer2, TIMER_MODE_CLOCK_SOURCE_2, 8192);
    timers::set_mode(timers::Timer::Timer2, 0);
    let observed = ((fast > 0) as u32)
        | (((slow > 0) as u32) << 1)
        | (((fast as u32) >= (slow as u32).saturating_mul(4)) as u32) << 2
        | (((fast as u32) <= (slow as u32).saturating_mul(16)) as u32) << 3;
    TestResult::info(0xF, observed, "sys/8")
}

fn test_timer2_wrap_sticky() -> TestResult {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0xFFF0);
    spin(8192);
    let mode = timers::mode(timers::Timer::Timer2);
    let counter = timers::counter(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0);
    let observed =
        ((mode & TIMER_MODE_REACHED_WRAP != 0) as u32) | (((counter as u32) < 0xFFF0) as u32) << 1;
    TestResult::info(0x3, observed, "wrap")
}

fn test_timer2_target_irq_latch() -> TestResult {
    timers::set_target(timers::Timer::Timer2, 32);
    timers::set_mode(
        timers::Timer::Timer2,
        TIMER_MODE_RESET_AT_TARGET | TIMER_MODE_IRQ_ON_TARGET,
    );
    timers::set_counter(timers::Timer::Timer2, 0);
    spin(8192);
    let mode = timers::mode(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0);
    let observed = ((mode & TIMER_MODE_REACHED_TARGET != 0) as u32)
        | (((mode & TIMER_MODE_IRQ_INACTIVE) == 0) as u32) << 1;
    TestResult::info(0x3, observed, "irq tgt")
}

fn test_timer2_wrap_irq_latch() -> TestResult {
    timers::set_mode(timers::Timer::Timer2, TIMER_MODE_IRQ_ON_WRAP);
    timers::set_counter(timers::Timer::Timer2, 0xFFF0);
    spin(8192);
    let mode = timers::mode(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0);
    let observed = ((mode & TIMER_MODE_REACHED_WRAP != 0) as u32)
        | (((mode & TIMER_MODE_IRQ_INACTIVE) == 0) as u32) << 1;
    TestResult::info(0x3, observed, "irq wrap")
}

fn test_timer1_hblank_clock_advances() -> TestResult {
    let delta = timer_delta(timers::Timer::Timer1, TIMER_MODE_CLOCK_SOURCE_1, 0x20000);
    timers::set_mode(timers::Timer::Timer1, 0x0103);
    TestResult::info(1024, delta as u32, "hblank")
}

fn test_timer0_dot_clock_ratio() -> TestResult {
    // 4096, not 8192: at the system clock a longer spin overflows the
    // 16-bit counter on real hardware (the delta wraps), which made the
    // ratio comparison meaningless. 4096 keeps the system-source count
    // safely under 0xFFFF on silicon.
    let sys = timer_delta(timers::Timer::Timer0, 0, 4096);
    let dot = timer_delta(timers::Timer::Timer0, TIMER_MODE_CLOCK_SOURCE_1, 4096);
    timers::set_mode(timers::Timer::Timer0, 0);
    let observed = ((sys > 0) as u32)
        | (((dot > 0) as u32) << 1)
        | (((sys as u32) >= (dot as u32).saturating_mul(2)) as u32) << 2
        | (((sys as u32) <= (dot as u32).saturating_mul(16)) as u32) << 3;
    expect_eq(0xF, observed, "dot/sys")
}

/// Companion measurement for the timer0 dot-clock ratio failure. Counts
/// system-clock and dot-clock ticks over an identical spin and reports
/// them packed as `(sys << 16) | dot`, so we can calibrate the
/// emulator's dot-clock divisor against silicon instead of guessing.
/// At 320-wide NTSC the divisor is 8, so PSoXide reads a sys:dot ratio
/// near 8:1. INFO only.
fn test_timer0_dot_clock_counts() -> TestResult {
    // 4096, not 8192: at the system clock a longer spin overflows the
    // 16-bit counter on real hardware (the delta wraps), which made the
    // ratio comparison meaningless. 4096 keeps the system-source count
    // safely under 0xFFFF on silicon.
    let sys = timer_delta(timers::Timer::Timer0, 0, 4096);
    let dot = timer_delta(timers::Timer::Timer0, TIMER_MODE_CLOCK_SOURCE_1, 4096);
    timers::set_mode(timers::Timer::Timer0, 0);
    let observed = ((sys as u32) << 16) | dot as u32;
    TestResult::info(0, observed, "sys<<16|dot")
}

fn test_dma_otc_bounded_completion() -> TestResult {
    let wait = timed_otc_dma_wait();
    if wait < 0xFFFF {
        TestResult::pass(0xFFFF, wait as u32, "otc wait")
    } else {
        TestResult::fail(0xFFFF, wait as u32, "otc wait")
    }
}

fn test_scratchpad_roundtrip() -> TestResult {
    const SCRATCH0: u32 = 0x1F80_03F0;
    const SCRATCH1: u32 = 0x1F80_03F4;

    unsafe {
        let old0 = psx_io::read_u32(SCRATCH0);
        let old1 = psx_io::read_u32(SCRATCH1);

        psx_io::write_u32(SCRATCH0, 0xA55A_C33C);
        psx_io::write_u32(SCRATCH1, 0x1122_3344);

        let mut observed = 0u32;
        if psx_io::read_u32(SCRATCH0) == 0xA55A_C33C {
            observed |= 1 << 0;
        }
        if psx_io::read_u16(SCRATCH0) == 0xC33C {
            observed |= 1 << 1;
        }
        if psx_io::read_u8(SCRATCH0) == 0x3C {
            observed |= 1 << 2;
        }
        if psx_io::read_u32(SCRATCH1) == 0x1122_3344 {
            observed |= 1 << 3;
        }

        psx_io::write_u32(SCRATCH0, old0);
        psx_io::write_u32(SCRATCH1, old1);

        expect_eq(0x0F, observed, "scratch")
    }
}

fn test_cdrom_getstat_response() -> TestResult {
    match psx_io::cd::try_status(200_000) {
        Some(response) if !response.is_empty() => {
            TestResult::info(1, response.bytes()[0] as u32, "getstat")
        }
        Some(response) => TestResult::info(2, response.len() as u32, "empty"),
        None => TestResult::info(0, 0, "timeout"),
    }
}

fn test_cdrom_index_latch() -> TestResult {
    unsafe {
        let mut observed = 0u32;
        for index in 0..4u8 {
            psx_io::write_u8(psx_hw::cd::BASE, index);
            let status_index = psx_io::read_u8(psx_hw::cd::BASE) & 0x03;
            if status_index == index {
                observed |= 1 << index;
            }
        }
        psx_io::write_u8(psx_hw::cd::BASE, 0);
        expect_eq(0x0F, observed, "cd index")
    }
}

fn test_pad_direct_stability() -> TestResult {
    let first = psx_pad::poll_port1();
    spin(512);
    let second = psx_pad::poll_port1();
    spin(512);
    let third = psx_pad::poll_port1();

    let observed =
        ((first.id_low as u32) << 16) | ((second.id_low as u32) << 8) | third.id_low as u32;

    if !first.is_connected() && !second.is_connected() && !third.is_connected() {
        TestResult::info(0, observed, "optional")
    } else if first.mode == second.mode
        && second.mode == third.mode
        && first.id_low == second.id_low
        && second.id_low == third.id_low
    {
        TestResult::info(1, observed, "stable")
    } else {
        TestResult::warn(1, observed, "unstable")
    }
}

fn test_timer_target_register_roundtrip() -> TestResult {
    const PATTERNS: [u16; 3] = [0x0123, 0x4567, 0x89AB];
    const TIMERS: [timers::Timer; 3] = [
        timers::Timer::Timer0,
        timers::Timer::Timer1,
        timers::Timer::Timer2,
    ];

    let old0 = timer_target(timers::Timer::Timer0);
    let old1 = timer_target(timers::Timer::Timer1);
    let old2 = timer_target(timers::Timer::Timer2);
    let mut observed = 0u32;

    for index in 0..3 {
        timers::set_target(TIMERS[index], PATTERNS[index]);
        if timer_target(TIMERS[index]) == PATTERNS[index] {
            observed |= 1 << index;
        }
    }

    timers::set_target(timers::Timer::Timer0, old0);
    timers::set_target(timers::Timer::Timer1, old1);
    timers::set_target(timers::Timer::Timer2, old2);

    expect_eq(0x07, observed, "target reg")
}

fn timer_target(timer: timers::Timer) -> u16 {
    unsafe { psx_io::read_u32(0x1F80_1108 + 0x10 * (timer as u32)) as u16 }
}

fn timer_delta(timer: timers::Timer, mode: u16, spin_count: u32) -> u16 {
    timers::set_mode(timer, mode);
    timers::set_counter(timer, 0);
    let start = timers::counter(timer);
    spin(spin_count);
    let end = timers::counter(timer);
    end.wrapping_sub(start)
}

/// Scan single uncached-RAM reads until their occasional extra wait exposes
/// the DRAM refresh slot. Timer 0 timestamps only the slow samples: reading a
/// root counter latches that counter for two bus clocks, so sampling every
/// iteration would perturb the cadence we are trying to recover.
fn measure_dram_refresh() -> (u16, u16) {
    const SCAN_SAMPLES: u32 = 4096;
    const COUNTER_READ_HOLD: u16 = 2;

    let status: u32;
    unsafe {
        core::arch::asm!("mfc0 $8, $12", "nop", lateout("$8") status);
        core::arch::asm!(
            "mtc0 $8, $12",
            "nop",
            "nop",
            "nop",
            in("$8") status & !1,
            options(nostack, nomem),
        );
    }

    let cached = (&raw const TIMING_WORD) as u32;
    let uncached = 0xA000_0000 | (cached & 0x001F_FFFF);

    // Warm the detector itself, then establish the uncontended floor. A
    // refresh can only raise a sample, so the minimum is the stable baseline.
    let mut warm = 0;
    while warm < 64 {
        let _ = timed_uncached_ram_read_once(uncached);
        warm += 1;
    }
    let mut baseline = u16::MAX;
    let mut sample = 0;
    while sample < 256 {
        baseline = baseline.min(timed_uncached_ram_read_once(uncached));
        sample += 1;
    }

    timers::set_mode(timers::Timer::Timer0, 0);
    timers::set_counter(timers::Timer::Timer0, 0);

    let mut max_stall = 0u16;
    let mut best_period = 0u16;
    let mut previous_slow = 0u16;
    let mut have_previous = false;
    sample = 0;
    while sample < SCAN_SAMPLES {
        let elapsed = timed_uncached_ram_read_once(uncached);
        if elapsed > baseline {
            max_stall = max_stall.max(elapsed - baseline);
            let timestamp = timers::counter(timers::Timer::Timer0);
            if have_previous {
                // The preceding timestamp read held Timer 0 for two clocks.
                let period = timestamp
                    .wrapping_sub(previous_slow)
                    .wrapping_add(COUNTER_READ_HOLD);
                // Reject skipped refreshes and unrelated outliers while still
                // leaving ample room for an unknown retail DRAM cadence.
                if (384..=768).contains(&period) && period > best_period {
                    // An unrelated slow sample can split one real refresh
                    // interval into shorter pieces. Keep the largest
                    // single-period candidate; skipped refresh multiples are
                    // already excluded by the upper bound.
                    best_period = period;
                }
            }
            previous_slow = timestamp;
            have_previous = true;
        }
        sample += 1;
    }

    unsafe {
        core::arch::asm!(
            "mtc0 $8, $12",
            "nop",
            "nop",
            "nop",
            in("$8") status,
            options(nostack, nomem),
        );
    }
    (best_period, max_stall)
}

// Keep every microbenchmark in one non-inlined assembly block. Besides avoiding
// five optimizer-dependent copies, this guarantees that register setup happens
// before the Timer 2 counter is cleared. Each block is bracketed by a pair of
// `ori $zero, $zero, imm` words: start = 0x34000000 | (id << 1), end = start | 1,
// with `id` unique across the crate. They write no register, LLVM never emits
// them, and tools/psoxide-hwtest (verify-machine-code) finds every pair in the linked
// PS-X EXE to audit the words between them (make hwtest-verify-code; spans are
// pinned by id in docs/hardware-refs/). Ids in use: 1-25 here, 32+ in
// perf_probes.rs.
#[inline(never)]
fn timed_empty() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000002", // probe 01 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)", // timing starts after this counter reset
            "lw $12, 0($11)",
            "nop", // resolve the R3000A load delay before exposing the result
            ".word 0x34000003", // probe 01 end marker
            ".set reorder",
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_uncached_ram_read_once(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000032", // probe 25 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "lw $9, 0($8)",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000033", // probe 25 end marker
            ".set reorder",
            in("$8") address,
            lateout("$9") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_load_hazards_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000008", // probe 04 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            "lw $9, 0($8)",
            "nop",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000009", // probe 04 end marker
            ".set reorder",
            in("$8") address,
            lateout("$9") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_byte_load_hazards_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000028", // probe 20 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0x81090000", // lb $9,0($8)
            "nop",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000029", // probe 20 end marker
            ".set reorder",
            in("$8") address,
            lateout("$9") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_half_load_hazards_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x3400002A", // probe 21 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0x85090000", // lh $9,0($8)
            "nop",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x3400002B", // probe 21 end marker
            ".set reorder",
            in("$8") address,
            lateout("$9") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_unaligned_word_loads_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000030", // probe 24 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0x89090003", // lwl $9,3($8)
            ".word 0x99090000", // lwr $9,0($8), interlocked merge
            "nop",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000031", // probe 24 end marker
            ".set reorder",
            in("$8") address,
            lateout("$9") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_stores_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x3400000A", // probe 05 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            "sw $zero, 0($8)",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x3400000B", // probe 05 end marker
            ".set reorder",
            in("$8") address,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_byte_stores_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x3400002C", // probe 22 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0xA1000000", // sb $zero,0($8)
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x3400002D", // probe 22 end marker
            ".set reorder",
            in("$8") address,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_half_stores_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x3400002E", // probe 23 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0xA5000000", // sh $zero,0($8)
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x3400002F", // probe 23 end marker
            ".set reorder",
            in("$8") address,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_untaken_branches() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000026", // probe 19 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0x14000001", // bne $zero,$zero,+1 (never taken)
            "nop",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000027", // probe 19 end marker
            ".set reorder",
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_multu_mflo(lhs: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x3400000E", // probe 07 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 16",
            ".word 0x01090019", // multu $8,$9
            ".word 0x00005012", // mflo $10; keep rs magnitude fixed
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x3400000F", // probe 07 end marker
            ".set reorder",
            in("$8") lhs,
            in("$9") 0x0001_0041u32,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_divu_mflo() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000010", // probe 08 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 8",
            ".word 0x0109001B", // divu $8,$9
            ".word 0x00005012", // mflo $10; keep numerator fixed
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000011", // probe 08 end marker
            ".set reorder",
            in("$8") 0x7ABC_DEF1u32,
            in("$9") 0x0000_0101u32,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

fn flush_icache_without_irq() {
    // flush_i_cache disables interrupts internally for the isolated
    // sequence, so no SR dance is needed around it.
    psx_rt::cache::flush_instruction_cache();
}

/// Execute a timing wrapper through its KSEG1 alias. Cache probes must not run
/// their own setup from KSEG0: a 4 KiB target occupies every direct-map index,
/// so even a few cached wrapper instructions would evict part of the warmed
/// target before the timer starts.
#[inline(never)]
fn call_uncached_timing(wrapper: fn() -> u16) -> u16 {
    let address = (wrapper as usize & 0x1FFF_FFFF) | 0xA000_0000;
    let uncached: fn() -> u16 = unsafe { core::mem::transmute(address) };
    uncached()
}

#[inline(never)]
fn call_uncached_entry_timing(
    wrapper: fn(unsafe extern "C" fn()) -> u16,
    target: unsafe extern "C" fn(),
) -> u16 {
    let address = (wrapper as usize & 0x1FFF_FFFF) | 0xA000_0000;
    let uncached: fn(unsafe extern "C" fn()) -> u16 = unsafe { core::mem::transmute(address) };
    uncached(target)
}

#[inline(never)]
fn timed_icache_cold() -> u16 {
    flush_icache_without_irq();
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x3400001E", // probe 15 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x3400001F", // probe 15 end marker
            ".set reorder",
            in("$8") __hwtest_icache_block as *const () as usize,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_icache_warm() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000020", // probe 16 start marker
            "jalr $10, $8", // untimed first pass fills the whole I-cache
            "nop",
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000021", // probe 16 end marker
            ".set reorder",
            in("$8") __hwtest_icache_block as *const () as usize,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

/// Time a single cache-line entry after a BIOS tag flush. Keeping the target
/// pointer dynamic gives all three word-position cases one identical wrapper;
/// the final EXE verifier checks both this wrapper and each linked target.
#[inline(never)]
fn timed_icache_entry_cold(target: unsafe extern "C" fn()) -> u16 {
    flush_icache_without_irq();
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000022", // probe 17 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000023", // probe 17 end marker
            ".set reorder",
            in("$8") target as usize,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

/// Time the same minimal target after an untimed pass has made both of its
/// executed words valid. This separates call/return overhead from refill cost.
#[inline(never)]
fn timed_icache_entry_warm(target: unsafe extern "C" fn()) -> u16 {
    flush_icache_without_irq();
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x34000024", // probe 18 start marker
            "jalr $10, $8",
            "nop",
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x34000025", // probe 18 end marker
            ".set reorder",
            in("$8") target as usize,
            lateout("$10") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

macro_rules! timed_gte_commands {
    ($name:ident, $id:literal, $count:literal, $instruction:literal) => {
        #[inline(never)]
        fn $name() -> u16 {
            seed_gte_state();
            let elapsed: u32;
            unsafe {
                core::arch::asm!(
                    concat!(
                        ".set noreorder\n",
                        ".word 0x34000000 | (", stringify!($id), " << 1)\n",
                        "lui $11, 0x1F80\n",
                        "ori $11, $11, 0x1120\n",
                        "sw $zero, 4($11)\n",
                        "sw $zero, 0($11)\n",
                        ".rept ", stringify!($count), "\n",
                        ".word ", stringify!($instruction), "\n",
                        ".endr\n",
                        "lw $12, 0($11)\n",
                        "nop\n",
                        ".word 0x34000001 | (", stringify!($id), " << 1)\n",
                        ".set reorder"
                    ),
                    lateout("$11") _,
                    lateout("$12") elapsed,
                    options(nostack)
                );
            }
            elapsed as u16
        }
    };
}

timed_gte_commands!(timed_gte_rtps_commands, 9, 16, 0x4A080001);
timed_gte_commands!(timed_gte_rtpt_commands, 10, 8, 0x4A080030);
timed_gte_commands!(timed_gte_nclip_commands, 11, 16, 0x4A000006);
timed_gte_commands!(timed_gte_mvmva_commands, 12, 16, 0x4A080012);
timed_gte_commands!(timed_gte_ncdt_commands, 13, 4, 0x4A080016);
timed_gte_commands!(timed_gte_ncct_commands, 14, 4, 0x4A08003F);

fn timed_otc_dma_cycles(words: u16) -> u16 {
    static mut OT: [u32; 256] = [0; 256];
    unsafe {
        let ptr = (&raw mut OT) as *mut u32;
        for i in 0..words as usize {
            ptr::write_volatile(ptr.add(i), 0);
        }
        dma::enable_channel(dma::Channel::OrderingTableClear);
        dma::raw::set_address(
            dma::Channel::OrderingTableClear,
            ptr.add(words as usize - 1) as u32,
        );
        dma::raw::set_size(dma::Channel::OrderingTableClear, dma::size_words(words));
        timers::set_mode(timers::Timer::Timer2, 0);
        timers::set_counter(timers::Timer::Timer2, 0);
        dma::raw::set_control(
            dma::Channel::OrderingTableClear,
            psx_hw::dma::CHCR_STEP_BACKWARD
                | psx_hw::dma::CHCR_SYNC_MANUAL
                | psx_hw::dma::CHCR_START
                | psx_hw::dma::CHCR_TRIGGER,
        );
        let mut polls = 0u16;
        while dma::is_busy(dma::Channel::OrderingTableClear) && polls != 0xFFFF {
            polls = polls.wrapping_add(1);
        }
        let elapsed = timers::counter(timers::Timer::Timer2);
        if polls != 0xFFFF && ptr::read_volatile(ptr) == 0x00FF_FFFF {
            elapsed
        } else {
            0xFFFF
        }
    }
}

/// Time a 1 KiB main-RAM to SPU-RAM DMA transfer. The payload is 256 DMA
/// words / 512 SPU halfwords, large enough that the 16-cycle-per-halfword SPU
/// transfer slope dominates fixed register and polling overhead while staying
/// well below Timer 2's 16-bit wrap.
fn timed_spu_dma_write_512_halfwords() -> u16 {
    use psx_hw::spu::{SPUCNT, TRANSFER_ADDR, TRANSFER_CTRL};
    static mut SOURCE: [u32; 256] = [0; 256];

    unsafe {
        let source = &raw mut SOURCE as *mut u32;
        let mut index = 0usize;
        while index < 256 {
            ptr::write_volatile(source.add(index), 0x5A00_0000 | index as u32);
            index += 1;
        }

        let old_spucnt = psx_io::read_u16(SPUCNT);
        let stopped = old_spucnt & !0x0030;
        psx_io::write_u16(SPUCNT, stopped);
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        psx_io::write_u16(TRANSFER_ADDR, 0x0800); // SPU RAM byte address 0x4000
        psx_io::write_u16(SPUCNT, stopped | 0x0020); // DMA Write

        dma::enable_channel(dma::Channel::Spu);
        dma::raw::set_address(dma::Channel::Spu, source as u32);
        dma::raw::set_size(dma::Channel::Spu, dma::size_blocks(16, 16));

        timers::set_mode(timers::Timer::Timer2, 0);
        timers::set_counter(timers::Timer::Timer2, 0);
        dma::raw::set_control(
            dma::Channel::Spu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );

        let mut polls = 0u32;
        while dma::is_busy(dma::Channel::Spu) && polls < 1_000_000 {
            polls += 1;
        }
        let elapsed = timers::counter(timers::Timer::Timer2);
        psx_io::write_u16(SPUCNT, old_spucnt);
        if polls == 1_000_000 {
            0xFFFF
        } else {
            elapsed
        }
    }
}

/// Time a RAM-to-GP0 block DMA consisting entirely of GP0 NOP commands.
///
/// Varying `block_size` and `block_count` independently lets the silicon
/// capture distinguish a true per-word transfer cost from completion models
/// that depend only on BCR's block-count field. Because every payload word is
/// a NOP, the probe neither changes VRAM nor disturbs the photographed page.
fn timed_gpu_dma_block(block_size: u16, block_count: u16) -> u16 {
    static SOURCE: [u32; 256] = [0; 256];
    let words = block_size as u32 * block_count as u32;
    if words == 0 || words > SOURCE.len() as u32 {
        return 0xFFFF;
    }

    let old_direction = (gpu_io::status().bits() >> 29) & 3;
    gpu_io::write_display_control(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_address(dma::Channel::Gpu, SOURCE.as_ptr() as u32);
        dma::raw::set_size(dma::Channel::Gpu, dma::size_blocks(block_size, block_count));
    }

    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_control(
            dma::Channel::Gpu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );
    }

    let mut polls = 0u32;
    while dma::is_busy(dma::Channel::Gpu) && polls < 1_000_000 {
        polls += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    gpu_io::write_display_control(0x0400_0000 | old_direction);
    if polls == 1_000_000 {
        0xFFFF
    } else {
        elapsed
    }
}

/// Time a 2-node linked-list GPU DMA with 128 GP0 NOPs in each node.
/// The work count is 258 DMA words: two headers plus 256 payload words.
fn timed_gpu_dma_linked_2x128() -> u16 {
    static mut LIST: [u32; 258] = [0; 258];

    unsafe {
        let list = &raw mut LIST as *mut u32;
        let second = list.add(129);
        ptr::write_volatile(list, (128u32 << 24) | (second as u32 & 0x00FF_FFFF));
        ptr::write_volatile(second, (128u32 << 24) | 0x00FF_FFFF);
        let mut index = 1usize;
        while index < 129 {
            ptr::write_volatile(list.add(index), 0); // GP0 NOP
            ptr::write_volatile(second.add(index), 0); // GP0 NOP
            index += 1;
        }

        let old_direction = (gpu_io::status().bits() >> 29) & 3;
        gpu_io::write_display_control(0x0400_0002); // DMA CPU -> GP0
        dma::enable_channel(dma::Channel::Gpu);
        dma::raw::set_address(dma::Channel::Gpu, list as u32);
        dma::raw::set_size(dma::Channel::Gpu, dma::size_words(0));

        timers::set_mode(timers::Timer::Timer2, 0);
        timers::set_counter(timers::Timer::Timer2, 0);
        dma::raw::set_control(
            dma::Channel::Gpu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_LINKED | psx_hw::dma::CHCR_START,
        );

        let mut polls = 0u32;
        while dma::is_busy(dma::Channel::Gpu) && polls < 1_000_000 {
            polls += 1;
        }
        let elapsed = timers::counter(timers::Timer::Timer2);
        gpu_io::write_display_control(0x0400_0000 | old_direction);
        if polls == 1_000_000 {
            0xFFFF
        } else {
            elapsed
        }
    }
}

/// Time CPU-submitted line rendering from the first command word through the
/// final GPU command-ready transition. Short and long batches separate packet
/// setup from per-pixel execution; monochrome and Gouraud batches expose the
/// color-interpolation cost. The lines land in off-screen VRAM so the photo UI
/// remains readable.
fn timed_gpu_line_batch(shaded: bool, length: u16, count: u16) -> u16 {
    gpu_io::wait_command_ready();
    gpu_io::write_command(0xE300_0000); // draw area top-left = (0, 0)
    gpu_io::write_command(0xE400_0000 | 1023 | (511 << 10));
    gpu_io::write_command(0xE500_0000); // draw offset = (0, 0)
    gpu_io::write_command(0xE100_0000); // dither off
    gpu_io::wait_command_ready();

    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let mut index = 0u16;
    while index < count {
        let y = 384u32 + u32::from(index & 63);
        let x0 = 640u32;
        let x1 = x0 + u32::from(length);
        if shaded {
            gpu_io::write_command(0x5000_00FF); // red endpoint
            gpu_io::write_command((y << 16) | x0);
            gpu_io::write_command(0x00FF_0000); // blue endpoint
            gpu_io::write_command((y << 16) | x1);
        } else {
            gpu_io::write_command(0x4000_FFFF);
            gpu_io::write_command((y << 16) | x0);
            gpu_io::write_command((y << 16) | x1);
        }
        index += 1;
    }

    let mut guard = 0u32;
    while gpu_io::status().bits() & (1 << 26) == 0 && guard < 1_000_000 {
        guard += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    if guard == 1_000_000 {
        0xFFFF
    } else {
        elapsed
    }
}

fn timed_cdrom_getstat() -> u16 {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let response = psx_io::cd::try_status(200_000);
    let elapsed = timers::counter(timers::Timer::Timer2);
    if response.is_some_and(|value| !value.is_empty()) {
        elapsed
    } else {
        0xFFFF
    }
}

fn timed_gpu_irq_settle() -> u16 {
    gpu_io::write_display_control(0x0200_0000);
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    gpu_io::write_command(0x1F00_0000);
    let mut guard = 0u16;
    while gpu_io::status().bits() & (1 << 24) == 0 && guard != 0xFFFF {
        guard = guard.wrapping_add(1);
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    gpu_io::write_display_control(0x0200_0000);
    if guard == 0xFFFF {
        0xFFFF
    } else {
        elapsed
    }
}

fn timed_otc_dma_wait() -> u16 {
    static mut OT: [u32; 16] = [0; 16];
    unsafe {
        let ptr = (&raw mut OT) as *mut u32;
        for i in 0..16 {
            ptr::write_volatile(ptr.add(i), 0);
        }
        dma::enable_channel(dma::Channel::OrderingTableClear);
        dma::raw::set_address(dma::Channel::OrderingTableClear, ptr.add(15) as u32);
        dma::raw::set_size(dma::Channel::OrderingTableClear, dma::size_words(16));
        dma::raw::set_control(
            dma::Channel::OrderingTableClear,
            psx_hw::dma::CHCR_STEP_BACKWARD
                | psx_hw::dma::CHCR_SYNC_MANUAL
                | psx_hw::dma::CHCR_START
                | psx_hw::dma::CHCR_TRIGGER,
        );
        let mut polls = 0u16;
        while dma::is_busy(dma::Channel::OrderingTableClear) && polls != 0xFFFF {
            polls = polls.wrapping_add(1);
        }
        let mut ok = ptr::read_volatile(ptr) == 0x00FF_FFFF;
        for i in 1..16 {
            ok &= ptr::read_volatile(ptr.add(i)) == (ptr.add(i - 1) as u32 & 0x00FF_FFFF);
        }
        if ok {
            polls
        } else {
            0xFFFF
        }
    }
}

fn expect_eq(expected: u32, observed: u32, note: &'static str) -> TestResult {
    if expected == observed {
        TestResult::pass(expected, observed, note)
    } else {
        TestResult::fail(expected, observed, note)
    }
}

fn spin(count: u32) {
    for _ in 0..count {
        unsafe {
            ptr::read_volatile(&raw const SPIN_SINK);
        }
    }
}

struct Hex8 {
    bytes: [u8; 10],
}

impl Hex8 {
    /// The 8 hex nibbles without the `0x` prefix.
    fn digits(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes[2..]) }
    }
}

fn hex8(value: u32) -> Hex8 {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = [0u8; 10];
    out[0] = b'0';
    out[1] = b'x';
    out[2] = HEX[((value >> 28) & 0xF) as usize];
    out[3] = HEX[((value >> 24) & 0xF) as usize];
    out[4] = HEX[((value >> 20) & 0xF) as usize];
    out[5] = HEX[((value >> 16) & 0xF) as usize];
    out[6] = HEX[((value >> 12) & 0xF) as usize];
    out[7] = HEX[((value >> 8) & 0xF) as usize];
    out[8] = HEX[((value >> 4) & 0xF) as usize];
    out[9] = HEX[(value & 0xF) as usize];
    Hex8 { bytes: out }
}
