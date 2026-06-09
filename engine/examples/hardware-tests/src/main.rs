//! `hardware-tests` -- visual PS1 hardware conformance suite.
//!
//! This is intentionally a real PS1 application, not a host-side unit
//! test. It paints a deterministic dashboard to the framebuffer and
//! exercises small, focused hardware behaviours through the same path
//! we will later run in PSoXide, PCSX-Redux, DuckStation, and on a
//! physical console.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

extern crate psx_rt;

use core::ptr;

use psx_engine::{button, App, Config, Ctx, Scene};
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, prim, Resolution, VideoMode};
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::ops as gte_ops;
use psx_gte::regs::pack_xy as pack_gte_xy;
use psx_gte::{cfc2, ctc2, mfc2, mtc2, scene as gte_scene};
use psx_io::{cdrom, dma, gpu as gpu_io, irq, sio, timers};
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

mod cpu_tests;
use cpu_tests::*;

const SUITE_VERSION: &str = "HWTEST v0.2";
const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const ROWS_PER_PAGE: usize = 6;
const TEST_COUNT: usize = 98;
const PAD_POLL_TEST_INDEX: usize = 26;
const MODE_COUNT: u8 = 15;
const CHECK_MODES: [Mode; 11] = [
    Mode::AllChecks,
    Mode::CpuChecks,
    Mode::MemoryChecks,
    Mode::IrqChecks,
    Mode::DmaChecks,
    Mode::TimerChecks,
    Mode::GpuChecks,
    Mode::GteChecks,
    Mode::SpuChecks,
    Mode::CdromChecks,
    Mode::SioChecks,
];

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

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    AllChecks,
    CpuChecks,
    MemoryChecks,
    IrqChecks,
    DmaChecks,
    TimerChecks,
    GpuChecks,
    GteChecks,
    SpuChecks,
    CdromChecks,
    SioChecks,
    CpuScan,
    GteScan,
    SpuScan,
    TimingScan,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Self::AllChecks => "ALL CHECKS",
            Self::CpuChecks => "CPU CHECKS",
            Self::MemoryChecks => "RAM CHECKS",
            Self::IrqChecks => "IRQ CHECKS",
            Self::DmaChecks => "DMA CHECKS",
            Self::TimerChecks => "TIMER CHECKS",
            Self::GpuChecks => "GPU CHECKS",
            Self::GteChecks => "GTE CHECKS",
            Self::SpuChecks => "SPU CHECKS",
            Self::CdromChecks => "CD-ROM CHECKS",
            Self::SioChecks => "SIO CHECKS",
            Self::CpuScan => "CPU SWEEP",
            Self::GteScan => "GTE MATRIX",
            Self::SpuScan => "SPU MAP",
            Self::TimingScan => "TIMING MAP",
        }
    }

    const fn hint(self) -> &'static str {
        match self {
            Self::AllChecks
            | Self::CpuChecks
            | Self::MemoryChecks
            | Self::IrqChecks
            | Self::DmaChecks
            | Self::TimerChecks
            | Self::GpuChecks
            | Self::GteChecks
            | Self::SpuChecks
            | Self::CdromChecks
            | Self::SioChecks => "L/R PAGE  X RERUN SECTION",
            Self::CpuScan => "X FINGERPRINT SAFE MIPS-I FORMS",
            Self::GteScan => "X FINGERPRINT COP2 COMMAND MATRIX",
            Self::SpuScan => "X MAP SPU VOICE REG READBACK",
            Self::TimingScan => "X SAMPLE TIMER DMA GTE COSTS",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::AllChecks => "ALL STABLE PASS/FAIL CHECKS",
            Self::CpuChecks => "CPU INSTRUCTIONS AND MEMORY ACCESS",
            Self::MemoryChecks => "RAM KSEG AND SCRATCHPAD CHECKS",
            Self::IrqChecks => "INTERRUPT MASK STATUS ACK CHECKS",
            Self::DmaChecks => "DMA CHANNEL AND OTC BEHAVIOUR",
            Self::TimerChecks => "ROOT COUNTER TIMING AND IRQS",
            Self::GpuChecks => "GPU STATUS COMMAND AND IRQ CHECKS",
            Self::GteChecks => "GTE REGISTERS PROJECTION OPCODES",
            Self::SpuChecks => "SPU STATUS AND VOICE REGISTERS",
            Self::CdromChecks => "CD-ROM COMMAND RESPONSE CHECKS",
            Self::SioChecks => "CONTROLLER SIO PORT CHECKS",
            Self::CpuScan => "DETERMINISTIC CPU OPCODE FINGERPRINT",
            Self::GteScan => "EXPLORATORY RAW GTE COMMAND MATRIX",
            Self::SpuScan => "SPU REGISTER BEHAVIOUR FINGERPRINT",
            Self::TimingScan => "RELATIVE HARDWARE TIMING PROBE",
        }
    }

    const fn aux_label(self) -> &'static str {
        match self {
            Self::AllChecks
            | Self::CpuChecks
            | Self::MemoryChecks
            | Self::IrqChecks
            | Self::DmaChecks
            | Self::TimerChecks
            | Self::GpuChecks
            | Self::GteChecks
            | Self::SpuChecks
            | Self::CdromChecks
            | Self::SioChecks => "DETAIL",
            Self::CpuScan => "EXTRA",
            Self::GteScan => "FLAG HITS",
            Self::SpuScan => "CHANGED",
            Self::TimingScan => "TIMER SUM",
        }
    }

    const fn index(self) -> u8 {
        match self {
            Self::AllChecks => 0,
            Self::CpuChecks => 1,
            Self::MemoryChecks => 2,
            Self::IrqChecks => 3,
            Self::DmaChecks => 4,
            Self::TimerChecks => 5,
            Self::GpuChecks => 6,
            Self::GteChecks => 7,
            Self::SpuChecks => 8,
            Self::CdromChecks => 9,
            Self::SioChecks => 10,
            Self::CpuScan => 11,
            Self::GteScan => 12,
            Self::SpuScan => 13,
            Self::TimingScan => 14,
        }
    }

    const fn from_index(index: u8) -> Self {
        match index % MODE_COUNT {
            0 => Self::AllChecks,
            1 => Self::CpuChecks,
            2 => Self::MemoryChecks,
            3 => Self::IrqChecks,
            4 => Self::DmaChecks,
            5 => Self::TimerChecks,
            6 => Self::GpuChecks,
            7 => Self::GteChecks,
            8 => Self::SpuChecks,
            9 => Self::CdromChecks,
            10 => Self::SioChecks,
            11 => Self::CpuScan,
            12 => Self::GteScan,
            13 => Self::SpuScan,
            _ => Self::TimingScan,
        }
    }

    const fn is_check_section(self) -> bool {
        matches!(
            self,
            Self::AllChecks
                | Self::CpuChecks
                | Self::MemoryChecks
                | Self::IrqChecks
                | Self::DmaChecks
                | Self::TimerChecks
                | Self::GpuChecks
                | Self::GteChecks
                | Self::SpuChecks
                | Self::CdromChecks
                | Self::SioChecks
        )
    }

    fn includes_test(self, spec: TestSpec) -> bool {
        match self {
            Self::AllChecks => true,
            Self::CpuChecks => spec.group == "CPU",
            Self::MemoryChecks => spec.group == "RAM",
            Self::IrqChecks => spec.group == "IRQ",
            Self::DmaChecks => spec.group == "DMA",
            Self::TimerChecks => spec.group == "TMR",
            Self::GpuChecks => spec.group == "GPU",
            Self::GteChecks => spec.group == "GTE",
            Self::SpuChecks => spec.group == "SPU",
            Self::CdromChecks => spec.group == "CD",
            Self::SioChecks => spec.group == "SIO",
            _ => false,
        }
    }

    const fn next(self) -> Self {
        Self::from_index(self.index() + 1)
    }

    const fn previous(self) -> Self {
        Self::from_index(self.index() + MODE_COUNT - 1)
    }
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
    group: &'static str,
    name: &'static str,
    run: fn() -> TestResult,
}

#[derive(Copy, Clone)]
struct ScanReport {
    status: Status,
    items: u16,
    hash: u32,
    aux: u32,
    note: &'static str,
    runs: u8,
}

impl ScanReport {
    const fn pending(note: &'static str) -> Self {
        Self {
            status: Status::Pending,
            items: 0,
            hash: 0,
            aux: 0,
            note,
            runs: 0,
        }
    }

    const fn info(items: u16, hash: u32, aux: u32, note: &'static str) -> Self {
        Self {
            status: Status::Info,
            items,
            hash,
            aux,
            note,
            runs: 1,
        }
    }

    fn with_run(mut self, previous: Self) -> Self {
        self.runs = previous.runs.wrapping_add(1);
        self
    }
}

#[derive(Copy, Clone)]
struct SectionReport {
    cases: u16,
    pass: u16,
    fail: u16,
    warn: u16,
    info: u16,
    pending: u16,
    hash: u32,
}

const TESTS: [TestSpec; TEST_COUNT] = [
    TestSpec {
        group: "CPU",
        name: "little-endian word layout",
        run: test_cpu_endian,
    },
    TestSpec {
        group: "CPU",
        name: "wrapping add/shift/multiply",
        run: test_cpu_arithmetic,
    },
    TestSpec {
        group: "CPU",
        name: "MIPS-I R-type opcode battery",
        run: test_cpu_rtype_opcodes,
    },
    TestSpec {
        group: "CPU",
        name: "MIPS-I immediate opcode battery",
        run: test_cpu_immediate_opcodes,
    },
    TestSpec {
        group: "CPU",
        name: "MIPS-I HI/LO multiply divide",
        run: test_cpu_hilo_opcodes,
    },
    TestSpec {
        group: "CPU",
        name: "MIPS-I branch delay battery",
        run: test_cpu_branch_delay_opcodes,
    },
    TestSpec {
        group: "CPU",
        name: "MIPS-I load/store battery",
        run: test_cpu_load_store_opcodes,
    },
    TestSpec {
        group: "RAM",
        name: "volatile byte/half/word stores",
        run: test_volatile_memory,
    },
    TestSpec {
        group: "RAM",
        name: "KSEG1 uncached RAM alias",
        run: test_kseg1_alias,
    },
    TestSpec {
        group: "IRQ",
        name: "I_MASK register roundtrip",
        run: test_irq_mask_roundtrip,
    },
    TestSpec {
        group: "IRQ",
        name: "GPU IRQ visible through I_STAT",
        run: test_irq_gpu_ack_path,
    },
    TestSpec {
        group: "DMA",
        name: "OTC reverse linked-list clear",
        run: test_dma_otc_clear,
    },
    TestSpec {
        group: "DMA",
        name: "channel register roundtrip",
        run: test_dma_channel_register_roundtrip,
    },
    TestSpec {
        group: "DMA",
        name: "DPCR priority enable latch",
        run: test_dma_dpcr_roundtrip,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 free-run increments",
        run: test_timer2_increments,
    },
    TestSpec {
        group: "TMR",
        name: "timer1 scanline range",
        run: test_timer1_scanline,
    },
    TestSpec {
        group: "GPU",
        name: "GPUSTAT mode/readiness",
        run: test_gpu_status,
    },
    TestSpec {
        group: "GPU",
        name: "GP0 IRQ set + GP1 ack",
        run: test_gpu_irq_ack,
    },
    TestSpec {
        group: "GPU",
        name: "primitive packet encoding",
        run: test_gpu_primitive_packet_encoding,
    },
    TestSpec {
        group: "GTE",
        name: "data/control register roundtrip",
        run: test_gte_register_roundtrip,
    },
    TestSpec {
        group: "GTE",
        name: "RTPS projects centre vertex",
        run: test_gte_projection_center,
    },
    TestSpec {
        group: "GTE",
        name: "all exposed GTE opcode battery",
        run: test_gte_all_ops_digest,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 winding sign",
        run: test_gte_nclip_mac0,
    },
    TestSpec {
        group: "SPU",
        name: "SPUSTAT readable",
        run: test_spu_status_readable,
    },
    TestSpec {
        group: "SPU",
        name: "voice register matrix",
        run: test_spu_voice_registers,
    },
    TestSpec {
        group: "SPU",
        name: "main volume register roundtrip",
        run: test_spu_main_volume_roundtrip,
    },
    TestSpec {
        group: "SIO",
        name: "port 1 pad poll",
        run: test_pad_poll,
    },
    TestSpec {
        group: "SIO",
        name: "mode control baud latches",
        run: test_sio_register_latches,
    },
    TestSpec {
        group: "GPU",
        name: "draw area command latch",
        run: test_gpu_draw_area_command,
    },
    TestSpec {
        group: "DMA",
        name: "GPU DMA direction survives OTC",
        run: test_gpu_dma_direction_after_otc,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 target sticky bit",
        run: test_timer2_target_sticky,
    },
    TestSpec {
        group: "TMR",
        name: "mode write resets counter",
        run: test_timer_mode_write_resets_counter,
    },
    TestSpec {
        group: "TMR",
        name: "mode read clears sticky flags",
        run: test_timer_mode_read_clears_sticky,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 sync stop vs free-run",
        run: test_timer2_sync_stop_vs_free_run,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 system clock divided by 8",
        run: test_timer2_clock_divider,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 0xffff wrap sticky bit",
        run: test_timer2_wrap_sticky,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 target IRQ latch",
        run: test_timer2_target_irq_latch,
    },
    TestSpec {
        group: "TMR",
        name: "timer2 wrap IRQ latch",
        run: test_timer2_wrap_irq_latch,
    },
    TestSpec {
        group: "TMR",
        name: "timer1 HBlank clock advances",
        run: test_timer1_hblank_clock_advances,
    },
    TestSpec {
        group: "TMR",
        name: "timer0 dot clock slower than system",
        run: test_timer0_dot_clock_ratio,
    },
    TestSpec {
        group: "DMA",
        name: "OTC DMA completes within bounded poll",
        run: test_dma_otc_bounded_completion,
    },
    TestSpec {
        group: "RAM",
        name: "scratchpad byte/half/word roundtrip",
        run: test_scratchpad_roundtrip,
    },
    TestSpec {
        group: "CD",
        name: "CD-ROM GetStat command response",
        run: test_cdrom_getstat_response,
    },
    TestSpec {
        group: "CD",
        name: "CD-ROM register index latch",
        run: test_cdrom_index_latch,
    },
    TestSpec {
        group: "SIO",
        name: "direct port 1 pad poll stability",
        run: test_pad_direct_stability,
    },
    TestSpec {
        group: "CPU",
        name: "MIPS-I unaligned load/store pairs",
        run: test_cpu_unaligned_load_store_pairs,
    },
    TestSpec {
        group: "GPU",
        name: "DMA direction mode latch",
        run: test_gpu_dma_direction_mode_latch,
    },
    TestSpec {
        group: "GPU",
        name: "GP1 info environment readback",
        run: test_gpu_gp1_info_environment_readback,
    },
    TestSpec {
        group: "TMR",
        name: "target register roundtrip",
        run: test_timer_target_register_roundtrip,
    },
    TestSpec {
        group: "GPU",
        name: "GPU IRQ1 flag settle latency",
        run: test_gpu_irq_latency_probe,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 raw value probe",
        run: test_gte_nclip_mac0_value,
    },
    TestSpec {
        group: "TMR",
        name: "timer0 dot/system tick counts",
        run: test_timer0_dot_clock_counts,
    },
    TestSpec {
        group: "GPU",
        name: "DMA-direction readback values",
        run: test_gpu_dma_direction_readback,
    },
    TestSpec {
        group: "GTE",
        name: "RTPS off-centre projection value",
        run: test_gte_rtps_offcenter_value,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS A SXY2 (div-ovf+sat)",
        run: test_gte_scene_rtps_a_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS A FLAG (0x80066000)",
        run: test_gte_scene_rtps_a_flag,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS B SXY2 (clamp hi/lo)",
        run: test_gte_scene_rtps_b_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS C SXY2 (SZ3 survives)",
        run: test_gte_scene_rtps_c_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS C FLAG (0x80002000)",
        run: test_gte_scene_rtps_c_flag,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS D SXY2 (neg-X vertex)",
        run: test_gte_scene_rtps_d_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPS D FLAG (0x80006000)",
        run: test_gte_scene_rtps_d_flag,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP SXY0 input readback",
        run: test_gte_nclip_in_sxy0,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP SXY1 input readback",
        run: test_gte_nclip_in_sxy1,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP SXY2 input readback",
        run: test_gte_nclip_in_sxy2,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 after 8 nops",
        run: test_gte_nclip_mac0_nop8,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 after 16 nops",
        run: test_gte_nclip_mac0_nop16,
    },
    TestSpec {
        group: "GTE",
        name: "scene MVMVA A (RT*V0+TR)",
        run: test_gte_scene_mvmva_a,
    },
    TestSpec {
        group: "GTE",
        name: "scene MVMVA B (RT*V0+TR)",
        run: test_gte_scene_mvmva_b,
    },
    TestSpec {
        group: "GTE",
        name: "scene MVMVA C (RT*V0+TR)",
        run: test_gte_scene_mvmva_c,
    },
    TestSpec {
        group: "GTE",
        name: "scene MVMVA D (RT*V0+TR)",
        run: test_gte_scene_mvmva_d,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT A SXY (div-ovf)",
        run: test_gte_scene_rtpt_a_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT B SXY (div-ovf)",
        run: test_gte_scene_rtpt_b_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT C SXY (div-ovf)",
        run: test_gte_scene_rtpt_c_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT D SXY (all-clamp)",
        run: test_gte_scene_rtpt_d_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT E SXY (in-frustum)",
        run: test_gte_scene_rtpt_e_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT F SXY (in-frustum)",
        run: test_gte_scene_rtpt_f_sxy,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT A FLAG (0x80006000)",
        run: test_gte_scene_rtpt_a_flag,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT B FLAG (0x80066000)",
        run: test_gte_scene_rtpt_b_flag,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT A SZ3 depth",
        run: test_gte_scene_rtpt_a_sz3,
    },
    TestSpec {
        group: "GTE",
        name: "scene RTPT E SZ3 depth",
        run: test_gte_scene_rtpt_e_sz3,
    },
    TestSpec {
        group: "GTE",
        name: "scene NCLIP A MAC0 (real)",
        run: test_gte_scene_nclip_a,
    },
    TestSpec {
        group: "GTE",
        name: "scene NCLIP B MAC0 (real)",
        run: test_gte_scene_nclip_b,
    },
    TestSpec {
        group: "GTE",
        name: "scene NCLIP C MAC0 (real)",
        run: test_gte_scene_nclip_c,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR 0x00ffffff = 8",
        run: test_gte_lzcr_zeros,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR 0xffff0000 = 16",
        run: test_gte_lzcr_half,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR 0x00000001 = 31",
        run: test_gte_lzcr_one,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR 0x7fffffff = 1",
        run: test_gte_lzcr_posmax,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR 0x80000000 = 1",
        run: test_gte_lzcr_negmin,
    },
    TestSpec {
        group: "GTE",
        name: "MVMVA FC-bug MAC1",
        run: test_gte_mvmva_fc_mac1,
    },
    TestSpec {
        group: "GTE",
        name: "MVMVA FC-bug MAC2",
        run: test_gte_mvmva_fc_mac2,
    },
    TestSpec {
        group: "GTE",
        name: "MVMVA FC-bug MAC3",
        run: test_gte_mvmva_fc_mac3,
    },
    TestSpec {
        group: "GTE",
        name: "SQR squares IR1..3",
        run: test_gte_sqr,
    },
    TestSpec {
        group: "GTE",
        name: "OP cross product MAC1",
        run: test_gte_op_mac1,
    },
    TestSpec {
        group: "GTE",
        name: "OP cross product MAC2",
        run: test_gte_op_mac2,
    },
    TestSpec {
        group: "GTE",
        name: "OP cross product MAC3",
        run: test_gte_op_mac3,
    },
    TestSpec {
        group: "GTE",
        name: "AVSZ3 averages SZ -> OTZ",
        run: test_gte_avsz3,
    },
    TestSpec {
        group: "SPU",
        name: "voice0 writable-bit mask",
        run: test_spu_voice_writable_mask,
    },
    TestSpec {
        group: "SPU",
        name: "voice0 pitch/ADSR readback",
        run: test_spu_voice_reg_readback,
    },
];

struct HardwareTests {
    font: Option<FontAtlas>,
    mode: Mode,
    results: [TestResult; TEST_COUNT],
    cpu_scan: ScanReport,
    gte_scan: ScanReport,
    spu_scan: ScanReport,
    timing_scan: ScanReport,
    pass_count: u8,
    fail_count: u8,
    warn_count: u8,
    info_count: u8,
    page: usize,
    rerun_count: u8,
}

#[cfg(target_arch = "mips")]
fn enable_cop2_for_diagnostics() {
    let mut sr: u32;
    unsafe {
        core::arch::asm!("mfc0 $8, $12", lateout("$8") sr);
        sr |= 0x4000_0000;
        core::arch::asm!(
            "mtc0 $8, $12",
            "nop",
            "nop",
            "nop",
            in("$8") sr,
            options(nostack, nomem, preserves_flags),
        );
    }
}

#[cfg(not(target_arch = "mips"))]
fn enable_cop2_for_diagnostics() {}

impl HardwareTests {
    const fn new() -> Self {
        Self {
            font: None,
            mode: Mode::AllChecks,
            results: [TestResult::pending(); TEST_COUNT],
            cpu_scan: ScanReport::pending("press x to sweep"),
            gte_scan: ScanReport::pending("press x to sweep"),
            spu_scan: ScanReport::pending("press x to map"),
            timing_scan: ScanReport::pending("press x to sample"),
            pass_count: 0,
            fail_count: 0,
            warn_count: 0,
            info_count: 0,
            page: 0,
            rerun_count: 0,
        }
    }

    fn run_all(&mut self) {
        for (index, spec) in TESTS.iter().enumerate() {
            self.results[index] = (spec.run)();
        }
        self.recount();
        self.rerun_count = self.rerun_count.wrapping_add(1);
        print_conformance_report(self);
        print_all_section_reports(&self.results);
        print_case_reports(Mode::AllChecks, &self.results);
    }

    fn run_startup_scans(&mut self) {
        self.cpu_scan = run_cpu_scan();
        print_scan_report(Mode::CpuScan, self.cpu_scan);
        self.gte_scan = run_gte_scan();
        print_scan_report(Mode::GteScan, self.gte_scan);
        self.spu_scan = run_spu_scan();
        print_scan_report(Mode::SpuScan, self.spu_scan);
        self.timing_scan = run_timing_scan();
        print_scan_report(Mode::TimingScan, self.timing_scan);
    }

    fn run_section(&mut self, mode: Mode) {
        for (index, spec) in TESTS.iter().enumerate() {
            if mode.includes_test(*spec) {
                self.results[index] = (spec.run)();
            }
        }
        self.recount();
        self.rerun_count = self.rerun_count.wrapping_add(1);
        print_conformance_report(self);
        print_section_report(mode, section_report(mode, &self.results));
        print_case_reports(mode, &self.results);
    }

    fn run_active(&mut self) {
        match self.mode {
            mode if mode.is_check_section() => self.run_section(mode),
            Mode::CpuScan => {
                self.cpu_scan = run_cpu_scan().with_run(self.cpu_scan);
                print_scan_report(self.mode, self.cpu_scan);
            }
            Mode::GteScan => {
                self.gte_scan = run_gte_scan().with_run(self.gte_scan);
                print_scan_report(self.mode, self.gte_scan);
            }
            Mode::SpuScan => {
                self.spu_scan = run_spu_scan().with_run(self.spu_scan);
                print_scan_report(self.mode, self.spu_scan);
            }
            Mode::TimingScan => {
                self.timing_scan = run_timing_scan().with_run(self.timing_scan);
                print_scan_report(self.mode, self.timing_scan);
            }
            _ => self.run_section(self.mode),
        }
    }

    fn recount(&mut self) {
        self.pass_count = 0;
        self.fail_count = 0;
        self.warn_count = 0;
        self.info_count = 0;

        for result in self.results {
            match result.status {
                Status::Pass => self.pass_count = self.pass_count.saturating_add(1),
                Status::Fail => self.fail_count = self.fail_count.saturating_add(1),
                Status::Warn => self.warn_count = self.warn_count.saturating_add(1),
                Status::Info => self.info_count = self.info_count.saturating_add(1),
                Status::Pending => {}
            }
        }
    }

    fn first_problem(&self, mode: Mode) -> Option<usize> {
        for (index, result) in self.results.iter().enumerate() {
            if mode.includes_test(TESTS[index])
                && matches!(result.status, Status::Fail | Status::Warn)
            {
                return Some(index);
            }
        }
        None
    }
}

impl Scene for HardwareTests {
    fn init(&mut self, _ctx: &mut Ctx) {
        enable_cop2_for_diagnostics();
        self.font = Some(FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT));
        self.run_all();
        self.run_startup_scans();
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.results[PAD_POLL_TEST_INDEX] = pad_poll_result(ctx.pad);
        self.recount();

        if ctx.just_pressed(button::UP) {
            self.mode = self.mode.previous();
            self.page = 0;
        }
        if ctx.just_pressed(button::DOWN) {
            self.mode = self.mode.next();
            self.page = 0;
        }

        if self.mode.is_check_section() && ctx.just_pressed(button::LEFT) {
            self.page = if self.page == 0 {
                page_count_for_mode(self.mode) - 1
            } else {
                self.page - 1
            };
        }
        if self.mode.is_check_section() && ctx.just_pressed(button::RIGHT) {
            self.page = (self.page + 1) % page_count_for_mode(self.mode);
        }
        if ctx.just_pressed(button::CROSS) {
            self.run_active();
        }
    }

    fn render(&mut self, ctx: &mut Ctx) {
        draw_test_pattern(ctx.sim_tick.as_u32());

        let Some(font) = self.font.as_ref() else {
            return;
        };

        draw_mode_menu(font, self);

        if self.mode.is_check_section() {
            draw_summary(font, self);
            draw_rows(font, self, self.mode);
            draw_problem_detail(font, self, self.mode);
        } else {
            match self.mode {
                Mode::CpuScan => draw_scan_report(font, self.mode, self.cpu_scan),
                Mode::GteScan => draw_scan_report(font, self.mode, self.gte_scan),
                Mode::SpuScan => draw_scan_report(font, self.mode, self.spu_scan),
                Mode::TimingScan => draw_scan_report(font, self.mode, self.timing_scan),
                _ => {}
            }
        }
    }
}

#[no_mangle]
fn main() -> ! {
    let mut suite = HardwareTests::new();
    let config = Config {
        screen_w: SCREEN_W as u16,
        screen_h: SCREEN_H as u16,
        video_mode: VideoMode::Ntsc,
        resolution: Resolution::R320X240,
        clear_color: (6, 8, 18),
        ..Config::default()
    };
    App::run(config, &mut suite);
}

fn draw_summary(font: &FontAtlas, suite: &HardwareTests) {
    font.draw_text(8, 28, "PASS", Status::Pass.color());
    font.draw_text(
        48,
        28,
        dec3(suite.pass_count as u16).as_str(),
        Status::Pass.color(),
    );
    font.draw_text(80, 28, "FAIL", Status::Fail.color());
    font.draw_text(
        120,
        28,
        dec3(suite.fail_count as u16).as_str(),
        Status::Fail.color(),
    );
    font.draw_text(152, 28, "WARN", Status::Warn.color());
    font.draw_text(
        192,
        28,
        dec3(suite.warn_count as u16).as_str(),
        Status::Warn.color(),
    );
    font.draw_text(224, 28, "INFO", Status::Info.color());
    font.draw_text(
        264,
        28,
        dec3(suite.info_count as u16).as_str(),
        Status::Info.color(),
    );

    font.draw_text(8, 230, "PAGE", (140, 160, 190));
    font.draw_text(
        48,
        230,
        dec3((suite.page + 1) as u16).as_str(),
        (220, 220, 220),
    );
    font.draw_text(80, 230, "OF", (140, 160, 190));
    font.draw_text(
        104,
        230,
        dec3(page_count_for_mode(suite.mode) as u16).as_str(),
        (220, 220, 220),
    );
    font.draw_text(232, 230, "RUN", (140, 160, 190));
    font.draw_text(
        264,
        230,
        dec3(suite.rerun_count as u16).as_str(),
        (220, 220, 220),
    );
    font.draw_text(144, 230, "1ST FAIL", (140, 160, 190));
}

fn draw_mode_menu(font: &FontAtlas, suite: &HardwareTests) {
    font.draw_text(8, 8, "PS1 HARDWARE TESTS", (232, 236, 244));
    font.draw_text(224, 8, SUITE_VERSION, (112, 136, 170));
    font.draw_text(8, 18, "SECTION", (140, 160, 190));
    font.draw_text(72, 18, suite.mode.label(), (255, 232, 128));
    font.draw_text(184, 18, "UP/DN NEXT", (140, 160, 190));
    font.draw_text(272, 18, "X RUN", (140, 160, 190));
}

fn draw_rows(font: &FontAtlas, suite: &HardwareTests, mode: Mode) {
    font.draw_text(8, 38, mode.description(), (112, 136, 170));
    let first = suite.page * ROWS_PER_PAGE;
    let mut visible_index = 0usize;
    let mut row = 0usize;

    for index in 0..TEST_COUNT {
        let spec = TESTS[index];
        if !mode.includes_test(spec) {
            continue;
        }
        if visible_index < first {
            visible_index += 1;
            continue;
        }
        if row >= ROWS_PER_PAGE {
            break;
        }
        let y = 52 + row as i16 * 20;
        let result = suite.results[index];
        let color = result.status.color();

        font.draw_text(8, y, result.status.label(), color);
        font.draw_text(48, y, spec.group, (140, 170, 210));
        if matches!(result.status, Status::Fail | Status::Warn | Status::Info) {
            font.draw_text(216, y, "OBS", (140, 160, 190));
            font.draw_text(248, y, hex8(result.observed).as_str(), color);
        }
        font.draw_text(16, y + 10, clipped_text(spec.name, 37), (220, 224, 230));
        visible_index += 1;
        row += 1;
    }
}

fn draw_scan_report(font: &FontAtlas, mode: Mode, report: ScanReport) {
    let color = report.status.color();
    font.draw_text(8, 40, "ADVANCED DIAGNOSTIC", (255, 232, 128));
    font.draw_text(8, 52, mode.description(), (232, 236, 244));
    font.draw_text(8, 66, mode.hint(), (150, 170, 200));

    font.draw_text(8, 92, "STATUS", (140, 160, 190));
    font.draw_text(80, 92, report.status.label(), color);
    font.draw_text(8, 106, "CASES", (140, 160, 190));
    font.draw_text(80, 106, hex8(report.items as u32).as_str(), (220, 224, 230));
    font.draw_text(8, 120, "DIGEST", (140, 160, 190));
    font.draw_text(80, 120, hex8(report.hash).as_str(), (220, 224, 230));
    font.draw_text(8, 134, mode.aux_label(), (140, 160, 190));
    font.draw_text(80, 134, hex8(report.aux).as_str(), (220, 224, 230));
    font.draw_text(8, 148, "RUN", (140, 160, 190));
    font.draw_text(80, 148, dec3(report.runs as u16).as_str(), (220, 224, 230));
    font.draw_text(8, 162, "FIELDS", (140, 160, 190));
    font.draw_text(80, 162, scan_field_hint(mode), (180, 190, 210));
    font.draw_text(8, 176, "NOTE", (140, 160, 190));
    font.draw_text(80, 176, report.note, color);
    font.draw_text(8, 198, "COMPARE DIGESTS ACROSS EMUS/PS1", (112, 136, 170));
    font.draw_text(
        8,
        208,
        "DIFFERENCE = INVESTIGATE THIS MODE",
        (112, 136, 170),
    );
    font.draw_text(
        8,
        218,
        "EXPLORATORY: NOT PASS/FAIL BY ITSELF",
        (112, 136, 170),
    );
}

const fn scan_field_hint(mode: Mode) -> &'static str {
    match mode {
        Mode::CpuScan => "ITEMS=OP SAMPLES HASH=OUTPUT DIGEST",
        Mode::GteScan => "ITEMS=COP2 OPS AUX=FLAG HITS",
        Mode::SpuScan => "ITEMS=SPU REGS AUX=CHANGED READBACKS",
        Mode::TimingScan => "ITEMS=PROBES AUX=TIMER/DMA PACK",
        _ => "ITEMS=CASES HASH=DIGEST AUX=EXTRA",
    }
}

fn draw_problem_detail(font: &FontAtlas, suite: &HardwareTests, mode: Mode) {
    let y = 190;
    match suite.first_problem(mode) {
        Some(index) => {
            let result = suite.results[index];
            font.draw_text(8, y, "DETAIL", result.status.color());
            font.draw_text(64, y, clipped_text(TESTS[index].name, 32), (230, 230, 230));
            font.draw_text(8, y + 10, "EXP", (150, 170, 200));
            font.draw_text(40, y + 10, hex8(result.expected).as_str(), (220, 220, 220));
            font.draw_text(128, y + 10, "GOT", (150, 170, 200));
            font.draw_text(
                160,
                y + 10,
                hex8(result.observed).as_str(),
                result.status.color(),
            );
            font.draw_text(248, y + 10, result.note, (180, 190, 210));
            draw_case_diagnostics(font, y + 20, index, result);
        }
        None => {
            font.draw_text(8, 200, "ALL HARD FAILURES CLEAR", Status::Pass.color());
            font.draw_text(
                8,
                210,
                "NEXT: RUN IN REDUX DUCKSTATION REAL PS1",
                (150, 170, 200),
            );
        }
    }
}

fn draw_case_diagnostics(font: &FontAtlas, y: i16, index: usize, result: TestResult) {
    let details = diagnostic_lines_for_case(index);
    if details.is_empty() {
        return;
    }

    let mut drawn = 0usize;
    drawn = draw_case_diagnostic_pass(font, y, result, details, drawn, false);
    draw_case_diagnostic_pass(font, y, result, details, drawn, true);
}

fn draw_case_diagnostic_pass(
    font: &FontAtlas,
    y: i16,
    result: TestResult,
    details: &'static [&'static str],
    mut drawn: usize,
    matching: bool,
) -> usize {
    for (bit, label) in details.iter().enumerate() {
        let expected = (result.expected >> bit) & 1;
        let observed = (result.observed >> bit) & 1;
        if (expected == observed) != matching {
            continue;
        }
        if drawn >= 2 {
            return drawn;
        }

        let line_y = y + drawn as i16 * 10;
        let color = if expected == observed {
            Status::Pass.color()
        } else {
            Status::Fail.color()
        };
        font.draw_text(
            8,
            line_y,
            if expected == observed { "OK" } else { "BAD" },
            color,
        );
        font.draw_text(40, line_y, clipped_text(label, 25), (220, 224, 230));
        font.draw_text(248, line_y, "E", (140, 160, 190));
        font.draw_text(264, line_y, if expected != 0 { "1" } else { "0" }, color);
        font.draw_text(280, line_y, "G", (140, 160, 190));
        font.draw_text(296, line_y, if observed != 0 { "1" } else { "0" }, color);
        drawn += 1;
    }
    drawn
}

fn test_count_for_mode(mode: Mode) -> usize {
    let mut count = 0usize;
    for spec in TESTS {
        if mode.includes_test(spec) {
            count += 1;
        }
    }
    count
}

fn page_count_for_mode(mode: Mode) -> usize {
    let count = test_count_for_mode(mode);
    if count == 0 {
        1
    } else {
        (count + ROWS_PER_PAGE - 1) / ROWS_PER_PAGE
    }
}

fn section_report(mode: Mode, results: &[TestResult; TEST_COUNT]) -> SectionReport {
    let mut report = SectionReport {
        cases: 0,
        pass: 0,
        fail: 0,
        warn: 0,
        info: 0,
        pending: 0,
        hash: 0x4857_5445,
    };

    report.hash = mix32(report.hash, mode.index() as u32);
    for (index, spec) in TESTS.iter().enumerate() {
        if !mode.includes_test(*spec) {
            continue;
        }
        let result = results[index];
        report.cases = report.cases.wrapping_add(1);
        report.hash = mix32(report.hash, index as u32);
        report.hash = mix_str(report.hash, spec.group);
        report.hash = mix_str(report.hash, spec.name);
        report.hash = mix32(report.hash, result.status.code());
        report.hash = mix32(report.hash, result.expected);
        report.hash = mix32(report.hash, result.observed);

        match result.status {
            Status::Pass => report.pass = report.pass.wrapping_add(1),
            Status::Fail => report.fail = report.fail.wrapping_add(1),
            Status::Warn => report.warn = report.warn.wrapping_add(1),
            Status::Info => report.info = report.info.wrapping_add(1),
            Status::Pending => report.pending = report.pending.wrapping_add(1),
        }
    }

    report.hash = mix32(report.hash, report.cases as u32);
    report.hash = mix32(report.hash, report.pass as u32);
    report.hash = mix32(report.hash, report.fail as u32);
    report.hash = mix32(report.hash, report.warn as u32);
    report.hash = mix32(report.hash, report.info as u32);
    report.hash = mix32(report.hash, report.pending as u32);
    report
}

fn test_case_hash(index: usize, spec: TestSpec, result: TestResult) -> u32 {
    let mut hash = 0x4341_5345;
    hash = mix32(hash, index as u32);
    hash = mix_str(hash, spec.group);
    hash = mix_str(hash, spec.name);
    hash = mix32(hash, result.status.code());
    hash = mix32(hash, result.expected);
    hash = mix32(hash, result.observed);
    hash
}

fn print_case_reports(mode: Mode, results: &[TestResult; TEST_COUNT]) {
    for (index, spec) in TESTS.iter().enumerate() {
        if !mode.includes_test(*spec) {
            continue;
        }
        let result = results[index];
        tty::print("hardware-tests: case ");
        tty_print_dec_u16(index as u16);
        tty::print(" ");
        tty::print(spec.group);
        tty::print(" status=");
        tty::print(result.status.label());
        tty::print(" exp=0x");
        tty::print_hex_u32(result.expected);
        tty::print(" got=0x");
        tty::print_hex_u32(result.observed);
        tty::print(" hash=0x");
        tty::print_hex_u32(test_case_hash(index, *spec, result));
        tty::print(" name=");
        tty::println(spec.name);
    }
}

fn print_all_section_reports(results: &[TestResult; TEST_COUNT]) {
    for mode in CHECK_MODES {
        print_section_report(mode, section_report(mode, results));
    }
}

fn print_section_report(mode: Mode, report: SectionReport) {
    tty::print("hardware-tests: section ");
    tty::print(mode.label());
    tty::print(" cases=");
    tty_print_dec_u16(report.cases);
    tty::print(" pass=");
    tty_print_dec_u16(report.pass);
    tty::print(" fail=");
    tty_print_dec_u16(report.fail);
    tty::print(" warn=");
    tty_print_dec_u16(report.warn);
    tty::print(" info=");
    tty_print_dec_u16(report.info);
    tty::print(" pending=");
    tty_print_dec_u16(report.pending);
    tty::print(" hash=0x");
    tty::print_hex_u32(report.hash);
    tty::print("\n");
}

fn clipped_text(text: &'static str, max_chars: usize) -> &'static str {
    let mut count = 0usize;
    for (index, _) in text.char_indices() {
        if count == max_chars {
            return &text[..index];
        }
        count += 1;
    }
    text
}

fn diagnostic_lines_for_case(index: usize) -> &'static [&'static str] {
    match index {
        1 => &["wrapping add", "arithmetic shift", "wrapping multiply"],
        2 => &[
            "ADDU", "SUBU", "AND", "OR", "XOR", "NOR", "SLT", "SLTU", "SLL", "SRL", "SRA", "SLLV",
            "SRLV", "SRAV",
        ],
        3 => &["ADDIU", "ANDI", "ORI", "XORI", "SLTI", "SLTIU", "LUI"],
        4 => &["MULT", "MULTU", "DIV", "DIVU", "MTHI MTLO"],
        5 => &[
            "BEQ delay",
            "BNE delay",
            "BNE fallthrough",
            "BEQ always delay",
            "BLEZ delay",
            "BGTZ delay",
            "BGTZ fallthrough",
            "BLTZ delay",
            "BGEZ delay",
        ],
        6 => &[
            "LW result",
            "SW byte order",
            "LH sign extend",
            "LHU zero extend",
            "SH byte order",
            "LB sign extend",
            "LBU zero extend",
            "SB byte store",
            "load delay slot",
        ],
        10 => &["GPUSTAT24 set", "I_STAT set", "GPUSTAT24 clr", "I_STAT clr"],
        11 => &[
            "OT terminator",
            "OT link 1",
            "OT link 2",
            "OT link 3",
            "OT link 4",
            "OT link 5",
            "OT link 6",
            "OT link 7",
        ],
        12 => &["MADR", "BCR", "CHCR"],
        16 => &[
            "horizontal res",
            "vertical res",
            "DMA direction",
            "ready cmd",
            "ready DMA",
        ],
        17 => &["GP0 IRQ raised", "GP1 IRQ ack clears"],
        18 => &[
            "TriFlat layout",
            "TriFlat words",
            "LineMono words",
            "RectFlat words",
        ],
        19 => &["data register", "control register"],
        21 => &[
            "RTPS", "RTPT", "NCLIP", "OP", "AVSZ3", "AVSZ4", "SQR", "NCDS", "NCCS", "NCS", "NCDT",
            "NCT", "NCCT", "DPCS", "DPCT", "INTPL", "DCPL", "CC", "CDP", "GPF", "GPL", "MVMVA",
        ],
        22 => &[
            "positive MAC0",
            "negative MAC0",
            "positive FLAG",
            "negative FLAG",
        ],
        24 => &[
            "voice volume L",
            "voice volume R",
            "ADSR low",
            "ADSR high",
            "repeat addr",
            "pitch",
        ],
        25 => &["main volume L", "main volume R"],
        27 => &["SIO mode", "SIO baud", "SIO ctrl"],
        30 => &["target sticky", "counter reset"],
        32 => &["target before read", "target cleared by read"],
        33 => &["sync stop holds", "sync free-runs"],
        34 => &["system ticks", "div8 ticks", "ratio min", "ratio max"],
        35 => &["wrap sticky", "counter wrapped"],
        36 => &["target sticky", "IRQ active low"],
        37 => &["wrap sticky", "IRQ active low"],
        39 => &["system ticks", "dot ticks", "ratio min", "ratio max"],
        41 => &[
            "scratch word",
            "scratch half",
            "scratch byte",
            "scratch second word",
        ],
        43 => &["index 0", "index 1", "index 2", "index 3"],
        45 => &[
            "LWL/LWR result",
            "load pre byte",
            "store pre byte",
            "SWL/SWR byte 0",
            "SWL/SWR byte 1",
            "SWL/SWR byte 2",
            "SWL/SWR byte 3",
            "store post byte",
        ],
        46 => &["DMA off", "DMA fifo", "DMA CPU->GP0", "DMA GPUREAD->CPU"],
        47 => &[
            "texture window",
            "draw area TL",
            "draw area BR",
            "draw offset",
        ],
        48 => &["timer0 target", "timer1 target", "timer2 target"],
        _ => &[],
    }
}

fn print_conformance_report(suite: &HardwareTests) {
    tty::print("hardware-tests: ");
    tty::println(SUITE_VERSION);
    tty::print("hardware-tests: conformance pass=");
    tty_print_dec_u8(suite.pass_count);
    tty::print(" fail=");
    tty_print_dec_u8(suite.fail_count);
    tty::print(" warn=");
    tty_print_dec_u8(suite.warn_count);
    tty::print(" info=");
    tty_print_dec_u8(suite.info_count);
    tty::print("\n");

    for (index, result) in suite.results.iter().enumerate() {
        if matches!(result.status, Status::Fail | Status::Warn) {
            let spec = TESTS[index];
            tty::print("hardware-tests: ");
            tty::print(result.status.label());
            tty::print(" ");
            tty::print(spec.group);
            tty::print(" ");
            tty::print(spec.name);
            tty::print(" exp=0x");
            tty::print_hex_u32(result.expected);
            tty::print(" got=0x");
            tty::print_hex_u32(result.observed);
            tty::print(" note=");
            tty::println(result.note);
            print_case_diagnostics(index, *result);
        }
    }
    if suite.fail_count == 0 && suite.warn_count == 0 {
        tty::println("hardware-tests: all hard failures clear");
    }
}

fn print_case_diagnostics(index: usize, result: TestResult) {
    let details = diagnostic_lines_for_case(index);
    if details.is_empty() {
        return;
    }

    let mut mismatches = 0u16;
    for (bit, label) in details.iter().enumerate() {
        let expected = (result.expected >> bit) & 1;
        let observed = (result.observed >> bit) & 1;
        if expected == observed {
            continue;
        }
        mismatches = mismatches.wrapping_add(1);
        tty::print("hardware-tests: detail case=");
        tty_print_dec_u16(index as u16);
        tty::print(" bit=");
        tty_print_dec_u16(bit as u16);
        tty::print(" exp=");
        tty_print_dec_u16(expected as u16);
        tty::print(" got=");
        tty_print_dec_u16(observed as u16);
        tty::print(" ");
        tty::println(label);
    }
    if mismatches == 0 {
        tty::print("hardware-tests: detail case=");
        tty_print_dec_u16(index as u16);
        tty::println(" no sub-bit mismatch; inspect raw expected/observed");
    }
}

fn print_scan_report(mode: Mode, report: ScanReport) {
    tty::print("hardware-tests: ");
    tty::print(mode.label());
    tty::print(" items=");
    tty_print_dec_u16(report.items);
    tty::print(" hash=0x");
    tty::print_hex_u32(report.hash);
    tty::print(" aux=0x");
    tty::print_hex_u32(report.aux);
    tty::print(" fields=");
    tty::print(scan_field_hint(mode));
    tty::print(" note=");
    tty::println(report.note);
}

fn tty_print_dec_u8(value: u8) {
    tty_print_dec_u16(value as u16);
}

fn tty_print_dec_u16(value: u16) {
    let mut divisor = 10000u16;
    let mut started = false;
    while divisor > 0 {
        let digit = value / divisor % 10;
        if digit != 0 || started || divisor == 1 {
            tty_print_digit(digit as u8);
            started = true;
        }
        divisor /= 10;
    }
}

fn tty_print_digit(value: u8) {
    let byte = b'0' + value.min(9);
    let text = [byte];
    let text = unsafe { core::str::from_utf8_unchecked(&text) };
    tty::print(text);
}

fn draw_test_pattern(_tick: u32) {
    gpu::draw_quad_flat([(0, 0), (320, 0), (0, 47), (320, 47)], 12, 18, 36);
    gpu::draw_quad_flat([(0, 188), (320, 188), (0, 240), (320, 240)], 8, 12, 28);
    gpu::draw_line_mono(0, 48, 319, 48, 60, 80, 110);
    gpu::draw_line_mono(0, 187, 319, 187, 60, 80, 110);
    gpu::draw_line_mono(272, 50, 312, 90, 255, 80, 80);
    gpu::draw_line_mono(312, 50, 272, 90, 80, 180, 255);
}

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
            if cfc2!(31) & 0x8000_0000 != 0 {
                flag_master_hits = flag_master_hits.wrapping_add(1);
            }
            hash = mix32(hash, $instr);
            hash = mix32(hash, snapshot);
            items = items.wrapping_add(1);
        }};
    }

    sample!(0x4A08_0001, gte_ops::rtps());
    sample!(0x4A08_0030, gte_ops::rtpt());
    sample!(0x4A00_0006, gte_ops::nclip());
    sample!(0x4A08_000C, gte_ops::op_sf1());
    sample!(0x4A00_002D, gte_ops::avsz3());
    sample!(0x4A00_002E, gte_ops::avsz4());
    sample!(0x4A08_0028, gte_ops::sqr());
    sample!(0x4A08_0013, gte_ops::ncds());
    sample!(0x4A08_001B, gte_ops::nccs());
    sample!(0x4A08_001E, gte_ops::ncs());
    sample!(0x4A08_0016, gte_ops::ncdt());
    sample!(0x4A08_0020, gte_ops::nct());
    sample!(0x4A08_003F, gte_ops::ncct());
    sample!(0x4A08_0010, gte_ops::dpcs());
    sample!(0x4A08_002A, gte_ops::dpct());
    sample!(0x4A08_0011, gte_ops::intpl());
    sample!(0x4A08_0029, gte_ops::dcpl());
    sample!(0x4A08_001C, gte_ops::cc());
    sample!(0x4A08_0014, gte_ops::cdp());
    sample!(0x4A08_003D, gte_ops::gpf());
    sample!(0x4A08_003E, gte_ops::gpl());
    sample!(0x4A08_0012, gte_ops::mvmva_rt_v0_tr_sf1());

    ScanReport::info(items, hash, flag_master_hits, "documented cop2 ops")
}

fn run_spu_scan() -> ScanReport {
    const VOICE_STRIDE: u32 = 0x10;
    const OFFSETS: [u32; 7] = [0, 2, 4, 6, 8, 10, 14];

    let mut hash = 0x5350_5501;
    let mut items = 0u16;
    let mut changed = 0u32;

    unsafe {
        for voice in 0..24u32 {
            let base = psx_io::spu::SPU_BASE + voice * VOICE_STRIDE;
            for offset in OFFSETS {
                let addr = base + offset;
                let old = psx_io::read16(addr);
                let pattern = 0x1000u16
                    ^ ((voice as u16).wrapping_mul(0x0111))
                    ^ ((offset as u16).wrapping_mul(0x0029));
                psx_io::write16(addr, pattern);
                let readback = psx_io::read16(addr);
                psx_io::write16(addr, old);
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

    ScanReport::info(items, hash, changed, "spu voice regs")
}

fn run_timing_scan() -> ScanReport {
    const SPINS: [u32; 4] = [256, 1024, 4096, 16384];

    let mut hash = 0x5449_4D31;
    let mut items = 0u16;
    let mut aux = 0u32;

    for spin_count in SPINS {
        let sys = timer_delta(timers::Timer::Timer2, 0, spin_count);
        let div8 = timer_delta(timers::Timer::Timer2, TIMER_MODE_CLOCK_SOURCE_2, spin_count);
        let dot = timer_delta(timers::Timer::Timer0, TIMER_MODE_CLOCK_SOURCE_1, spin_count);
        hash = mix32(hash, spin_count);
        hash = mix32(hash, sys as u32);
        hash = mix32(hash, div8 as u32);
        hash = mix32(hash, dot as u32);
        aux = aux.wrapping_add(sys as u32);
        items = items.wrapping_add(3);
    }

    let hblank = timer_delta(timers::Timer::Timer1, TIMER_MODE_CLOCK_SOURCE_1, 0x20000);
    hash = mix32(hash, hblank as u32);
    aux = aux.wrapping_add((hblank as u32) << 16);
    items = items.wrapping_add(1);

    let cpu_mix = timed_cpu_mix(256);
    hash = mix32(hash, cpu_mix);
    items = items.wrapping_add(1);

    let gte_rtps = timed_gte_rtps(64);
    hash = mix32(hash, gte_rtps as u32);
    items = items.wrapping_add(1);

    let otc_wait = timed_otc_dma_wait();
    hash = mix32(hash, otc_wait as u32);
    aux ^= (otc_wait as u32) << 24;
    items = items.wrapping_add(1);

    ScanReport::info(items, hash, aux, "timer dma gte costs")
}

fn gte_snapshot_hash() -> u32 {
    let mut hash = 0x9E37_79B9;
    hash = mix32(hash, mfc2!(7));
    hash = mix32(hash, mfc2!(8));
    hash = mix32(hash, mfc2!(9));
    hash = mix32(hash, mfc2!(10));
    hash = mix32(hash, mfc2!(11));
    hash = mix32(hash, mfc2!(12));
    hash = mix32(hash, mfc2!(13));
    hash = mix32(hash, mfc2!(14));
    hash = mix32(hash, mfc2!(16));
    hash = mix32(hash, mfc2!(17));
    hash = mix32(hash, mfc2!(18));
    hash = mix32(hash, mfc2!(19));
    hash = mix32(hash, mfc2!(20));
    hash = mix32(hash, mfc2!(21));
    hash = mix32(hash, mfc2!(22));
    hash = mix32(hash, mfc2!(24));
    hash = mix32(hash, mfc2!(25));
    hash = mix32(hash, mfc2!(26));
    hash = mix32(hash, mfc2!(27));
    mix32(hash, cfc2!(31))
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
    irq::set_mask(old_mask & !(1 << irq::source::GPU));
    irq::ack(1 << irq::source::GPU);
    gpu_io::write_gp1(0x0200_0000);

    gpu_io::write_gp0(0x1F00_0000);
    let raised_gpu = gpu_io::gpustat().bits() & (1 << 24) != 0;
    let raised_irq = irq::stat() & (1 << irq::source::GPU) != 0;

    gpu_io::write_gp1(0x0200_0000);
    irq::ack(1 << irq::source::GPU);
    let cleared_gpu = gpu_io::gpustat().bits() & (1 << 24) == 0;
    let cleared_irq = irq::stat() & (1 << irq::source::GPU) == 0;
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

fn test_dma_otc_clear() -> TestResult {
    static mut OT: [u32; 8] = [0; 8];
    unsafe {
        let ptr = (&raw mut OT) as *mut u32;
        dma::clear_ordering_table(ptr, 8);
        let mut observed = 0u32;
        if ptr::read_volatile(ptr) == 0x00FF_FFFF {
            observed |= 1;
        }
        for i in 1..8 {
            let expected = ptr.add(i - 1) as u32 & 0x00FF_FFFF;
            if ptr::read_volatile(ptr.add(i)) == expected {
                observed |= 1 << i;
            }
        }
        expect_eq(0xFF, observed, "otc")
    }
}

fn test_dma_channel_register_roundtrip() -> TestResult {
    let ch = dma::Channel::Pio;
    let base = ch.base();
    unsafe {
        let old_madr = psx_io::read32(base);
        let old_bcr = psx_io::read32(base + 4);
        let old_chcr = psx_io::read32(base + 8);

        psx_io::write32(base, 0x0012_3400);
        psx_io::write32(base + 4, 0x0002_0034);
        psx_io::write32(
            base + 8,
            dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_CHOPPING_ENABLE,
        );

        let mut observed = 0u32;
        if psx_io::read32(base) == 0x0012_3400 {
            observed |= 1 << 0;
        }
        if psx_io::read32(base + 4) == 0x0002_0034 {
            observed |= 1 << 1;
        }
        if psx_io::read32(base + 8)
            == (dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_CHOPPING_ENABLE)
        {
            observed |= 1 << 2;
        }

        psx_io::write32(base, old_madr);
        psx_io::write32(base + 4, old_bcr);
        psx_io::write32(base + 8, old_chcr);

        expect_eq(0x07, observed, "dma regs")
    }
}

fn test_dma_dpcr_roundtrip() -> TestResult {
    unsafe {
        let old = psx_io::read32(dma::DPCR);
        psx_io::write32(dma::DPCR, 0x0765_4321);
        let readback = psx_io::read32(dma::DPCR) & 0x0FFF_FFFF;
        psx_io::write32(dma::DPCR, old);
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
    let scanline = gpu::scanline_counter();
    if scanline <= 340 {
        TestResult::pass(340, scanline as u32, "scanline")
    } else {
        TestResult::fail(340, scanline as u32, "range")
    }
}

fn test_gpu_status() -> TestResult {
    let stat = gpu_io::gpustat();
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
    gpu_io::write_gp0(0x1F00_0000);
    let raised = gpu_io::gpustat().bits() & (1 << 24) != 0;
    gpu_io::write_gp1(0x0200_0000);
    let cleared = gpu_io::gpustat().bits() & (1 << 24) == 0;
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
    gpu_io::write_gp1(0x0200_0000);

    // Latency for GP0(0x1F) to raise GPUSTAT.24.
    gpu_io::write_gp0(0x1F00_0000);
    let mut set_polls = 0u32;
    while set_polls < MAX_POLLS && gpu_io::gpustat().bits() & (1 << 24) == 0 {
        set_polls = set_polls.wrapping_add(1);
    }

    // Latency for GP1(0x02) to clear it again, now that the FIFO has
    // drained the 0x1F.
    gpu_io::write_gp1(0x0200_0000);
    let mut clr_polls = 0u32;
    while clr_polls < MAX_POLLS && gpu_io::gpustat().bits() & (1 << 24) != 0 {
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
        gpu_io::write_gp1(0x0400_0000 | dir);
        let read = (gpu_io::gpustat().bits() >> 29) & 0b11;
        observed |= read << (dir * 2);
    }
    gpu_io::write_gp1(0x0400_0000 | 2);
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
    mtc2!(0, 0x2222_1111);
    ctc2!(31, 0);
    let data = mfc2!(0);
    let flag = cfc2!(31);
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
    unsafe { gte_ops::rtps() };
    if gte_flag_master_clear() && mfc2!(14) != 0 {
        observed |= 1 << 0;
    }

    seed_gte_state();
    unsafe { gte_ops::rtpt() };
    if gte_flag_master_clear() && mfc2!(12) != mfc2!(14) {
        observed |= 1 << 1;
    }

    seed_gte_state();
    unsafe { gte_ops::nclip() };
    if gte_flag_master_clear() {
        observed |= 1 << 2;
    }

    seed_gte_state();
    unsafe { gte_ops::op_sf1() };
    if gte_flag_master_clear() {
        observed |= 1 << 3;
    }

    seed_gte_state();
    unsafe { gte_ops::avsz3() };
    if gte_flag_master_clear() && mfc2!(7) != 0 {
        observed |= 1 << 4;
    }

    seed_gte_state();
    unsafe { gte_ops::avsz4() };
    if gte_flag_master_clear() && mfc2!(7) != 0 {
        observed |= 1 << 5;
    }

    seed_gte_state();
    unsafe { gte_ops::sqr() };
    if gte_flag_master_clear() && mfc2!(25) != 0 {
        observed |= 1 << 6;
    }

    seed_gte_state();
    unsafe { gte_ops::ncds() };
    if gte_flag_master_clear() {
        observed |= 1 << 7;
    }

    seed_gte_state();
    unsafe { gte_ops::nccs() };
    if gte_flag_master_clear() {
        observed |= 1 << 8;
    }

    seed_gte_state();
    unsafe { gte_ops::ncs() };
    if gte_flag_master_clear() {
        observed |= 1 << 9;
    }

    seed_gte_state();
    unsafe { gte_ops::ncdt() };
    if gte_flag_master_clear() {
        observed |= 1 << 10;
    }

    seed_gte_state();
    unsafe { gte_ops::nct() };
    if gte_flag_master_clear() {
        observed |= 1 << 11;
    }

    seed_gte_state();
    unsafe { gte_ops::ncct() };
    if gte_flag_master_clear() {
        observed |= 1 << 12;
    }

    seed_gte_state();
    unsafe { gte_ops::dpcs() };
    if gte_flag_master_clear() {
        observed |= 1 << 13;
    }

    seed_gte_state();
    unsafe { gte_ops::dpct() };
    if gte_flag_master_clear() {
        observed |= 1 << 14;
    }

    seed_gte_state();
    unsafe { gte_ops::intpl() };
    if gte_flag_master_clear() {
        observed |= 1 << 15;
    }

    seed_gte_state();
    unsafe { gte_ops::dcpl() };
    if gte_flag_master_clear() {
        observed |= 1 << 16;
    }

    seed_gte_state();
    unsafe { gte_ops::cc() };
    if gte_flag_master_clear() {
        observed |= 1 << 17;
    }

    seed_gte_state();
    unsafe { gte_ops::cdp() };
    if gte_flag_master_clear() {
        observed |= 1 << 18;
    }

    seed_gte_state();
    unsafe { gte_ops::gpf() };
    if gte_flag_master_clear() {
        observed |= 1 << 19;
    }

    seed_gte_state();
    unsafe { gte_ops::gpl() };
    if gte_flag_master_clear() {
        observed |= 1 << 20;
    }

    seed_gte_state();
    unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
    if gte_flag_master_clear() && mfc2!(27) != 0 {
        observed |= 1 << 21;
    }

    expect_eq(0x003F_FFFF, observed, "gte ops")
}

/// Seed the positive-winding NCLIP triangle: SXY0=(0,0), SXY1=(10,0),
/// SXY2=(0,10). The cross product is +100, so a faithful GTE leaves
/// MAC0 = 0x64. Shared by every NCLIP MAC0 probe below.
fn seed_nclip_pos_triangle() {
    ctc2!(31, 0);
    mtc2!(12, pack_gte_xy(0, 0));
    mtc2!(13, pack_gte_xy(10, 0));
    mtc2!(14, pack_gte_xy(0, 10));
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
            unsafe { gte_ops::nclip() };
            #[cfg(target_arch = "mips")]
            unsafe {
                core::arch::asm!($delay, options(nostack, nomem, preserves_flags));
            }
            TestResult::info(100, mfc2!(24), $label)
        }
    };
}
nclip_mac0_delay_test!(test_gte_nclip_mac0_nop8, ".rept 8\nnop\n.endr", "nclip mac0 +8nop");
nclip_mac0_delay_test!(test_gte_nclip_mac0_nop16, ".rept 16\nnop\n.endr", "nclip mac0 +16nop");

fn test_gte_nclip_mac0() -> TestResult {
    seed_nclip_pos_triangle();
    unsafe { gte_ops::nclip() };
    let positive = mfc2!(24) as i32;
    let positive_flag_clear = gte_flag_master_clear();

    ctc2!(31, 0);
    mtc2!(12, pack_gte_xy(0, 0));
    mtc2!(13, pack_gte_xy(0, 10));
    mtc2!(14, pack_gte_xy(10, 0));
    unsafe { gte_ops::nclip() };
    let negative = mfc2!(24) as i32;
    let negative_flag_clear = gte_flag_master_clear();

    let observed = ((positive == 100) as u32)
        | (((negative == -100) as u32) << 1)
        | ((positive_flag_clear as u32) << 2)
        | ((negative_flag_clear as u32) << 3);
    expect_eq(0x0F, observed, "nclip")
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
    unsafe { gte_ops::nclip() };
    let mac0 = mfc2!(24);
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
    TestResult::info(0x0000_0000, mfc2!(12), "nclip in sxy0")
}
fn test_gte_nclip_in_sxy1() -> TestResult {
    seed_nclip_pos_triangle();
    TestResult::info(0x0000_000A, mfc2!(13), "nclip in sxy1")
}
fn test_gte_nclip_in_sxy2() -> TestResult {
    seed_nclip_pos_triangle();
    TestResult::info(0x000A_0000, mfc2!(14), "nclip in sxy2")
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
    ctc2!(31, 0); // clear FLAG
    ctc2!(0, 0x0000_0f19); // R11,R12
    ctc2!(1, 0x016e_fab4); // R13,R21
    ctc2!(2, 0x0411_f098); // R22,R23
    ctc2!(3, 0xfbb1_fae7); // R31,R32
    ctc2!(4, 0xffff_f177); // R33
    ctc2!(5, 0); // TRX
    ctc2!(6, 0); // TRY
    ctc2!(7, 0); // TRZ
    ctc2!(24, 0x00a0_0000); // OFX
    ctc2!(25, 0x0078_0000); // OFY
    ctc2!(26, 0x0000_0140); // H (projection plane distance)
    ctc2!(27, 0); // DQA
    ctc2!(28, 0); // DQB
}

fn scene_rtps(vxy0: u32, vz0: u32) {
    seed_scene_rtps();
    mtc2!(0, vxy0); // VXY0
    mtc2!(1, vz0); // VZ0
    unsafe { gte_ops::rtps() };
}

// Sample A: FLAG=0x80066000 (divide overflow + SX/SY + SZ3 saturation); SXY2 clamps to (-1024,-1024).
fn test_gte_scene_rtps_a_sxy() -> TestResult {
    scene_rtps(0x0c3e_0000, 0x0000_0a4d);
    expect_eq(0xfc00_fc00, mfc2!(14), "scene rtps A SXY2")
}
fn test_gte_scene_rtps_a_flag() -> TestResult {
    scene_rtps(0x0c3e_0000, 0x0000_0a4d);
    expect_eq(0x8006_6000, cfc2!(31), "scene rtps A FLAG")
}
// Sample B: same FLAG; SX clamps high (+1023), SY clamps low (-1024).
fn test_gte_scene_rtps_b_sxy() -> TestResult {
    scene_rtps(0x0c3e_0529, 0x0000_08eb);
    expect_eq(0xfc00_03ff, mfc2!(14), "scene rtps B SXY2")
}
// Sample C: FLAG=0x80002000 (SY-only saturation); SZ3 survives (no divide overflow).
fn test_gte_scene_rtps_c_sxy() -> TestResult {
    scene_rtps(0x0c3e_0526, 0xffff_f714);
    expect_eq(0xfc00_03b4, mfc2!(14), "scene rtps C SXY2")
}
fn test_gte_scene_rtps_c_flag() -> TestResult {
    scene_rtps(0x0c3e_0526, 0xffff_f714);
    expect_eq(0x8000_2000, cfc2!(31), "scene rtps C FLAG")
}
// Sample D: FLAG=0x80006000 (SX+SY saturation); negative-X vertex.
fn test_gte_scene_rtps_d_sxy() -> TestResult {
    scene_rtps(0x099c_f335, 0x0000_0000);
    expect_eq(0xfc00_fc00, mfc2!(14), "scene rtps D SXY2")
}
fn test_gte_scene_rtps_d_flag() -> TestResult {
    scene_rtps(0x099c_f335, 0x0000_0000);
    expect_eq(0x8000_6000, cfc2!(31), "scene rtps D FLAG")
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
    ctc2!(31, 0); // FLAG
    ctc2!(0, 0x0000_0f19);
    ctc2!(1, 0x016e_fab4);
    ctc2!(2, 0x0411_f098);
    ctc2!(3, 0xfbb1_fae7);
    ctc2!(4, 0xffff_f177);
    ctc2!(5, 0xffff_eabc); // TRX
    ctc2!(6, 0xffff_fdb9); // TRY
    ctc2!(7, 0x0000_35be); // TRZ
    ctc2!(24, 0x00a0_0000); // OFX
    ctc2!(25, 0x0078_0000); // OFY
    ctc2!(26, 0x0000_0140); // H
    ctc2!(27, 0); // DQA
    ctc2!(28, 0); // DQB
}

// Scene MVMVA: RT*V0 + TR, sf=1 (skinning/world transform). FLAG never fires
// -- exact integer math -- so a divergence is a plain matrix-multiply miss.
fn scene_mvmva_digest(vxy0: u32, vz0: u32) -> u32 {
    seed_scene_xform();
    mtc2!(0, vxy0);
    mtc2!(1, vz0);
    unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
    gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27))
}
fn test_gte_scene_mvmva_a() -> TestResult {
    expect_eq(0xca74_6d65, scene_mvmva_digest(0x2040_0340, 0x0000_09c0), "scene mvmva A")
}
fn test_gte_scene_mvmva_b() -> TestResult {
    expect_eq(0xd65a_09bf, scene_mvmva_digest(0x2040_0340, 0x0000_16c0), "scene mvmva B")
}
fn test_gte_scene_mvmva_c() -> TestResult {
    expect_eq(0x511b_ee0c, scene_mvmva_digest(0x0b00_0340, 0x0000_23c0), "scene mvmva C")
}
fn test_gte_scene_mvmva_d() -> TestResult {
    expect_eq(0x46ef_d743, scene_mvmva_digest(0x2040_09c0, 0x0000_09c0), "scene mvmva D")
}

// Scene RTPT: projects 3 verts; divide-overflow cases clamp SXY to the
// screen-coord limits -- the exact regime behind missing/exploded triangles.
const RTPT_A: [u32; 6] = [0x0480_2d80, 0x0000_2700, 0x0480_2080, 0x0000_2d80, 0x0480_1a00, 0x0000_2d80];
const RTPT_B: [u32; 6] = [0x0480_2700, 0x0000_2d80, 0x0480_2d80, 0x0000_2d80, 0x0480_2080, 0x0000_3400];
const RTPT_C: [u32; 6] = [0x0480_1a00, 0x0000_3400, 0x0480_2700, 0x0000_3400, 0x0480_2d80, 0x0000_3400];
const RTPT_D: [u32; 6] = [0x0680_3400, 0x0000_2d80, 0x0480_3400, 0x0000_3400, 0x0680_3a80, 0x0000_2700];
const RTPT_E: [u32; 6] = [0x10c0_0000, 0x0000_0d00, 0x10c0_0680, 0x0000_0d00, 0x1dc0_0680, 0x0000_0d00];
const RTPT_F: [u32; 6] = [0x1dc0_0000, 0x0000_0d00, 0x10c0_0680, 0x0000_1380, 0x10c0_0000, 0x0000_1380];
fn scene_rtpt(v: [u32; 6]) {
    seed_scene_xform();
    mtc2!(0, v[0]);
    mtc2!(1, v[1]);
    mtc2!(2, v[2]);
    mtc2!(3, v[3]);
    mtc2!(4, v[4]);
    mtc2!(5, v[5]);
    unsafe { gte_ops::rtpt() };
}
fn rtpt_sxy_digest(v: [u32; 6]) -> u32 {
    scene_rtpt(v);
    gte_tri_digest(mfc2!(12), mfc2!(13), mfc2!(14))
}
fn test_gte_scene_rtpt_a_sxy() -> TestResult { expect_eq(0xfc1f_e61f, rtpt_sxy_digest(RTPT_A), "rtpt A sxy") }
fn test_gte_scene_rtpt_b_sxy() -> TestResult { expect_eq(0xfbe0_066f, rtpt_sxy_digest(RTPT_B), "rtpt B sxy") }
fn test_gte_scene_rtpt_c_sxy() -> TestResult { expect_eq(0x03d5_13df, rtpt_sxy_digest(RTPT_C), "rtpt C sxy") }
fn test_gte_scene_rtpt_d_sxy() -> TestResult { expect_eq(0x0420_0420, rtpt_sxy_digest(RTPT_D), "rtpt D sxy") }
fn test_gte_scene_rtpt_e_sxy() -> TestResult { expect_eq(0xaf36_5a05, rtpt_sxy_digest(RTPT_E), "rtpt E sxy") }
fn test_gte_scene_rtpt_f_sxy() -> TestResult { expect_eq(0x7931_abae, rtpt_sxy_digest(RTPT_F), "rtpt F sxy") }
fn test_gte_scene_rtpt_a_flag() -> TestResult { scene_rtpt(RTPT_A); expect_eq(0x8000_6000, cfc2!(31), "rtpt A FLAG") }
fn test_gte_scene_rtpt_b_flag() -> TestResult { scene_rtpt(RTPT_B); expect_eq(0x8006_6000, cfc2!(31), "rtpt B FLAG") }
fn test_gte_scene_rtpt_a_sz3() -> TestResult { scene_rtpt(RTPT_A); expect_eq(0x0000_02e9, mfc2!(19), "rtpt A SZ3") }
fn test_gte_scene_rtpt_e_sz3() -> TestResult { scene_rtpt(RTPT_E); expect_eq(0x0000_1fd9, mfc2!(19), "rtpt E SZ3") }

// Scene NCLIP: the REAL backface cross products the scene runs (large
// projected screen coords), unlike the synthetic (0,0)/(10,0)/(0,10). MAC0
// sign decides face culling -> the missing-wall divergence with real inputs.
fn scene_nclip_mac0(s0: u32, s1: u32, s2: u32) -> u32 {
    ctc2!(31, 0);
    mtc2!(12, s0);
    mtc2!(13, s1);
    mtc2!(14, s2);
    unsafe { gte_ops::nclip() };
    mfc2!(24)
}
fn test_gte_scene_nclip_a() -> TestResult {
    expect_eq(0x0000_2764, scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc), "scene nclip A")
}
fn test_gte_scene_nclip_b() -> TestResult {
    expect_eq(0x0000_30ba, scene_nclip_mac0(0x0073_00d5, 0xffde_00dc, 0xffd8_0130), "scene nclip B")
}
fn test_gte_scene_nclip_c() -> TestResult {
    expect_eq(0x0000_3e7e, scene_nclip_mac0(0x0079_011f, 0xffd8_0130, 0xffd2_0194), "scene nclip C")
}

// LZCS/LZCR: leading-bit count (bits equal to bit 31). A classic emulator-
// vs-silicon divergence; writing LZCS (data reg 30) updates LZCR (reg 31).
fn lzcr(value: u32) -> u32 {
    mtc2!(30, value);
    mfc2!(31)
}
fn test_gte_lzcr_zeros() -> TestResult { expect_eq(8, lzcr(0x00ff_ffff), "lzcr 00ffffff") }
fn test_gte_lzcr_half() -> TestResult { expect_eq(16, lzcr(0xffff_0000), "lzcr ffff0000") }
fn test_gte_lzcr_one() -> TestResult { expect_eq(31, lzcr(0x0000_0001), "lzcr 00000001") }
fn test_gte_lzcr_posmax() -> TestResult { expect_eq(1, lzcr(0x7fff_ffff), "lzcr 7fffffff") }
fn test_gte_lzcr_negmin() -> TestResult { expect_eq(1, lzcr(0x8000_0000), "lzcr 80000000") }

// Corner ops: the famous bugged MVMVA far-color mode, plus SQR / OP / AVSZ3
// to widen op coverage. Expecteds from psx-gte-core (gte_expected_values).
// MVMVA bugged FC mode (cv=2): PSX-SPX documents that the far-color
// translation is dropped AND the first matrix column is dropped, so the
// result reduces to MAC_i = (Mx_i2*Vy + Mx_i3*Vz) >> sf. Per-component so
// the disc pins each MAC exactly; hardware (2026-06-09) returned the same
// fix, so these PASS on silicon = confirmation the GTE core now matches.
fn run_mvmva_fc() {
    seed_scene_xform();
    ctc2!(21, 0x0000_1000); // FCX
    ctc2!(22, 0x0000_2000); // FCY
    ctc2!(23, 0x0000_3000); // FCZ
    mtc2!(0, 0x2040_0340);
    mtc2!(1, 0x0000_09c0);
    unsafe { gte_ops::mvmva_rt_v0_fc_sf1() };
}
fn test_gte_mvmva_fc_mac1() -> TestResult { run_mvmva_fc(); expect_eq(0xffff_fcc5, mfc2!(25), "mvmva FC MAC1") }
fn test_gte_mvmva_fc_mac2() -> TestResult { run_mvmva_fc(); expect_eq(0xffff_e36c, mfc2!(26), "mvmva FC MAC2") }
fn test_gte_mvmva_fc_mac3() -> TestResult { run_mvmva_fc(); expect_eq(0xffff_ee75, mfc2!(27), "mvmva FC MAC3") }
fn test_gte_sqr() -> TestResult {
    ctc2!(31, 0);
    mtc2!(9, 0x0000_1234);
    mtc2!(10, 0x0000_f8ee);
    mtc2!(11, 0x0000_0567);
    unsafe { gte_ops::sqr() };
    expect_eq(0x7498_ecb5, gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27)), "sqr")
}
// OP cross product. The formula here matches PSX-SPX exactly and MVMVA/SQR
// read MAC1-3 immediately and pass, yet OP diverged on hardware
// (digest 0xBFF0043 vs 0xBFF002F) -- an unresolved silicon quirk. Split
// per-component so the next burn reveals WHICH MAC differs and by how
// much; expecteds are the current GTE-core values, so the FAIL on silicon
// captures the real number to fix `op_op` against. NOT latency (MAC1-3
// reads are latency-free, proven by MVMVA/SQR).
fn run_op() {
    ctc2!(31, 0);
    ctc2!(0, 0x0000_1000); // R11 (D1)
    ctc2!(2, 0x0000_2000); // R22 (D2)
    ctc2!(4, 0x0000_3000); // R33 (D3)
    mtc2!(9, 0x0000_0400); // IR1
    mtc2!(10, 0x0000_0500); // IR2
    mtc2!(11, 0x0000_0600); // IR3
    unsafe { gte_ops::op_sf1() };
}
fn test_gte_op_mac1() -> TestResult { run_op(); expect_eq(0xffff_fd00, mfc2!(25), "op MAC1") }
fn test_gte_op_mac2() -> TestResult { run_op(); expect_eq(0x0000_0600, mfc2!(26), "op MAC2") }
fn test_gte_op_mac3() -> TestResult { run_op(); expect_eq(0xffff_fd00, mfc2!(27), "op MAC3") }
fn test_gte_avsz3() -> TestResult {
    ctc2!(31, 0);
    ctc2!(29, 0x0000_0155); // ZSF3
    mtc2!(17, 0x0000_1000); // SZ1
    mtc2!(18, 0x0000_2000); // SZ2
    mtc2!(19, 0x0000_3000); // SZ3
    unsafe { gte_ops::avsz3() };
    expect_eq(0x0000_07fe, mfc2!(7), "avsz3 OTZ")
}

fn seed_gte_state() {
    ctc2!(31, 0);

    mtc2!(0, pack_gte_xy(-0x80, 0x40));
    mtc2!(1, 0x0400);
    mtc2!(2, pack_gte_xy(0x80, -0x40));
    mtc2!(3, 0x0500);
    mtc2!(4, pack_gte_xy(0x20, 0x90));
    mtc2!(5, 0x0600);
    mtc2!(6, 0x0040_4040);
    mtc2!(8, 0x0800);
    mtc2!(9, 0x0100);
    mtc2!(10, 0x0200);
    mtc2!(11, 0x0300);
    mtc2!(12, pack_gte_xy(-16, 20));
    mtc2!(13, pack_gte_xy(24, 36));
    mtc2!(14, pack_gte_xy(48, 72));
    mtc2!(16, 0x0400);
    mtc2!(17, 0x0500);
    mtc2!(18, 0x0600);
    mtc2!(19, 0x0700);
    mtc2!(20, 0x0010_1010);
    mtc2!(21, 0x0020_2020);
    mtc2!(22, 0x0030_3030);

    gte_scene::set_screen_offset(160 << 16, 120 << 16);
    gte_scene::set_projection_plane(256);
    gte_scene::load_rotation(&Mat3I16::IDENTITY);
    gte_scene::load_translation(Vec3I32::new(0, 0, 0x1000));

    ctc2!(8, pack_gte_xy(0x1000, 0));
    ctc2!(9, pack_gte_xy(0, 0));
    ctc2!(10, pack_gte_xy(0x1000, 0));
    ctc2!(11, pack_gte_xy(0, 0));
    ctc2!(12, 0x1000);
    ctc2!(13, 0);
    ctc2!(14, 0);
    ctc2!(15, 0);
    ctc2!(16, pack_gte_xy(0x1000, 0));
    ctc2!(17, pack_gte_xy(0, 0));
    ctc2!(18, pack_gte_xy(0x1000, 0));
    ctc2!(19, pack_gte_xy(0, 0));
    ctc2!(20, 0x1000);
    ctc2!(21, 0x20);
    ctc2!(22, 0x20);
    ctc2!(23, 0x20);
    ctc2!(27, 0);
    ctc2!(28, 0);
    ctc2!(29, 0x0555);
    ctc2!(30, 0x0400);
}

fn gte_flag_master_clear() -> bool {
    cfc2!(31) & 0x8000_0000 == 0
}

fn test_spu_status_readable() -> TestResult {
    let observed = unsafe { psx_io::read16(psx_io::spu::SPUSTAT) } as u32;
    if observed != 0xFFFF {
        TestResult::info(0, observed, "spustat")
    } else {
        TestResult::warn(0, observed, "open bus?")
    }
}

fn test_spu_voice_registers() -> TestResult {
    const VOICE_STRIDE: u32 = 0x10;
    const VOICE: u32 = 23;
    let base = psx_io::spu::SPU_BASE + VOICE * VOICE_STRIDE;
    let mut observed = 0u32;

    unsafe {
        psx_io::write16(base, 0x1234);
        psx_io::write16(base + 2, 0x2345);
        psx_io::write16(base + 4, 0x1000);
        psx_io::write16(base + 6, 0x0040);
        psx_io::write16(base + 8, 0x8F1F);
        psx_io::write16(base + 10, 0x1F80);

        if psx_io::read16(base) == 0x1234 {
            observed |= 1 << 0;
        }
        if psx_io::read16(base + 2) == 0x2345 {
            observed |= 1 << 1;
        }
        if psx_io::read16(base + 4) == 0x1000 {
            observed |= 1 << 2;
        }
        if psx_io::read16(base + 6) == 0x0040 {
            observed |= 1 << 3;
        }
        if psx_io::read16(base + 8) == 0x8F1F {
            observed |= 1 << 4;
        }
        if psx_io::read16(base + 10) == 0x1F80 {
            observed |= 1 << 5;
        }
    }

    expect_eq(0x3F, observed, "voice")
}

fn test_spu_main_volume_roundtrip() -> TestResult {
    const MAIN_VOL_LEFT: u32 = psx_io::spu::SPU_BASE + 0x180;
    const MAIN_VOL_RIGHT: u32 = psx_io::spu::SPU_BASE + 0x182;

    unsafe {
        let old_left = psx_io::read16(MAIN_VOL_LEFT);
        let old_right = psx_io::read16(MAIN_VOL_RIGHT);

        psx_io::write16(MAIN_VOL_LEFT, 0x1234);
        psx_io::write16(MAIN_VOL_RIGHT, 0x2345);

        let mut observed = 0u32;
        if psx_io::read16(MAIN_VOL_LEFT) == 0x1234 {
            observed |= 1 << 0;
        }
        if psx_io::read16(MAIN_VOL_RIGHT) == 0x2345 {
            observed |= 1 << 1;
        }

        psx_io::write16(MAIN_VOL_LEFT, old_left);
        psx_io::write16(MAIN_VOL_RIGHT, old_right);

        expect_eq(0x03, observed, "main vol")
    }
}

/// SPU drill-in for the diverging SPU MAP scan. Voice 0: write 0xFFFF to
/// each of the eight per-voice halfword registers (offsets 0x0..=0xE) and
/// set one bit per offset that reads back 0xFFFF. PSoXide stores the full
/// 16 bits; hardware masks reserved bits in some registers, so the clear
/// bits localize which offsets diverge. Old values restored. INFO only.
fn test_spu_voice_writable_mask() -> TestResult {
    let voice0 = psx_io::spu::SPU_BASE;
    let mut observed = 0u32;
    unsafe {
        for i in 0..8u32 {
            let addr = voice0 + i * 2;
            let old = psx_io::read16(addr);
            psx_io::write16(addr, 0xFFFF);
            if psx_io::read16(addr) == 0xFFFF {
                observed |= 1 << i;
            }
            psx_io::write16(addr, old);
        }
    }
    TestResult::info(0xFF, observed, "spu wr mask")
}

/// SPU drill-in companion: write 0xFFFF to voice 0 pitch (0x4) and ADSR1
/// (0x8) and report the raw readbacks packed `(pitch << 16) | adsr1`, so
/// the reserved-bit masks of two key registers are visible next to the
/// writable-bit mask. Old values restored. INFO only.
fn test_spu_voice_reg_readback() -> TestResult {
    let voice0 = psx_io::spu::SPU_BASE;
    unsafe {
        let old_pitch = psx_io::read16(voice0 + 0x4);
        let old_adsr1 = psx_io::read16(voice0 + 0x8);
        psx_io::write16(voice0 + 0x4, 0xFFFF);
        psx_io::write16(voice0 + 0x8, 0xFFFF);
        let pitch = psx_io::read16(voice0 + 0x4) as u32;
        let adsr1 = psx_io::read16(voice0 + 0x8) as u32;
        psx_io::write16(voice0 + 0x4, old_pitch);
        psx_io::write16(voice0 + 0x8, old_adsr1);
        TestResult::info(0xFFFF_FFFF, (pitch << 16) | adsr1, "pitch|adsr1")
    }
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

fn test_sio_register_latches() -> TestResult {
    unsafe {
        let old_mode = psx_io::read16(sio::MODE);
        let old_ctrl = psx_io::read16(sio::CTRL);
        let old_baud = psx_io::read16(sio::BAUD);

        psx_io::write16(sio::MODE, 0x000D);
        psx_io::write16(sio::BAUD, 0x0088);
        psx_io::write16(sio::CTRL, 0x0003);

        let mut observed = 0u32;
        if psx_io::read16(sio::MODE) == 0x000D {
            observed |= 1 << 0;
        }
        if psx_io::read16(sio::BAUD) == 0x0088 {
            observed |= 1 << 1;
        }
        if psx_io::read16(sio::CTRL) & 0x0003 == 0x0003 {
            observed |= 1 << 2;
        }

        psx_io::write16(sio::MODE, old_mode);
        psx_io::write16(sio::BAUD, old_baud);
        psx_io::write16(sio::CTRL, old_ctrl);

        expect_eq(0x07, observed, "sio regs")
    }
}

fn test_gpu_draw_area_command() -> TestResult {
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let observed = gpu_io::gpustat().bits() & ((1 << 26) | (1 << 28));
    let expected = (1 << 26) | (1 << 28);
    expect_eq(expected, observed, "draw area")
}

fn test_gpu_dma_direction_after_otc() -> TestResult {
    static mut OT: [u32; 4] = [0; 4];
    dma::clear_ordering_table((&raw mut OT) as *mut u32, 4);
    gpu_io::write_gp1(0x0400_0000 | 2);
    let observed = (gpu_io::gpustat().bits() >> 29) & 0b11;
    expect_eq(2, observed, "dma dir")
}

fn test_gpu_dma_direction_mode_latch() -> TestResult {
    let mut observed = 0u32;
    for direction in 0..4u32 {
        gpu_io::write_gp1(0x0400_0000 | direction);
        if ((gpu_io::gpustat().bits() >> 29) & 0b11) == direction {
            observed |= 1 << direction;
        }
    }
    gpu_io::write_gp1(0x0400_0000 | 2);
    // Racy on silicon: the GPUSTAT bits 29-30 readback lags the GP1(04)
    // write through the FIFO (the readback probe disagreed with this test
    // within one run). Report until FIFO latency is modelled.
    TestResult::info(0x0F, observed, "racy fifo")
}

fn test_gpu_gp1_info_environment_readback() -> TestResult {
    let texture_window = 0xE200_0000 | 0x0003 | (0x0005 << 5) | (0x0007 << 10) | (0x0009 << 15);
    let draw_area_top_left = 0xE300_0000 | 8 | (16 << 10);
    let draw_area_bottom_right = 0xE400_0000 | 300 | (220 << 10);
    let draw_offset = 0xE500_0000 | ((-12i32 as u32) & 0x7FF) | (34 << 11);

    gpu_io::write_gp0(texture_window);
    gpu_io::write_gp0(draw_area_top_left);
    gpu_io::write_gp0(draw_area_bottom_right);
    gpu_io::write_gp0(draw_offset);

    gpu_io::write_gp1(0x1000_0002);
    let texture_window_read = gpu_io::gpuread();
    gpu_io::write_gp1(0x1000_0003);
    let top_left_read = gpu_io::gpuread();
    gpu_io::write_gp1(0x1000_0004);
    let bottom_right_read = gpu_io::gpuread();
    gpu_io::write_gp1(0x1000_0005);
    let offset_read = gpu_io::gpuread();

    gpu_io::write_gp0(0xE200_0000);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);

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
    let observed = (target_hit as u32) | ((((counter as u32) < 32) as u32) << 1);
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
    if (1..1024).contains(&delta) {
        TestResult::info(1024, delta as u32, "hblank")
    } else {
        TestResult::info(1024, delta as u32, "hblank")
    }
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
        let old0 = psx_io::read32(SCRATCH0);
        let old1 = psx_io::read32(SCRATCH1);

        psx_io::write32(SCRATCH0, 0xA55A_C33C);
        psx_io::write32(SCRATCH1, 0x1122_3344);

        let mut observed = 0u32;
        if psx_io::read32(SCRATCH0) == 0xA55A_C33C {
            observed |= 1 << 0;
        }
        if psx_io::read16(SCRATCH0) == 0xC33C {
            observed |= 1 << 1;
        }
        if psx_io::read8(SCRATCH0) == 0x3C {
            observed |= 1 << 2;
        }
        if psx_io::read32(SCRATCH1) == 0x1122_3344 {
            observed |= 1 << 3;
        }

        psx_io::write32(SCRATCH0, old0);
        psx_io::write32(SCRATCH1, old1);

        expect_eq(0x0F, observed, "scratch")
    }
}

fn test_cdrom_getstat_response() -> TestResult {
    match cdrom::try_get_stat(200_000) {
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
            psx_io::write8(cdrom::BASE, index);
            let status_index = psx_io::read8(cdrom::BASE) & 0x03;
            if status_index == index {
                observed |= 1 << index;
            }
        }
        psx_io::write8(cdrom::BASE, 0);
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
    unsafe { psx_io::read32(0x1F80_1108 + 0x10 * (timer as u32)) as u16 }
}

fn timer_delta(timer: timers::Timer, mode: u16, spin_count: u32) -> u16 {
    timers::set_mode(timer, mode);
    timers::set_counter(timer, 0);
    let start = timers::counter(timer);
    spin(spin_count);
    let end = timers::counter(timer);
    end.wrapping_sub(start)
}

fn timed_cpu_mix(iterations: u16) -> u32 {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let mut acc = 0x1234_5678u32;
    for i in 0..iterations {
        acc = acc.rotate_left(3) ^ (i as u32).wrapping_mul(0x45D9_F3B);
        acc = acc.wrapping_add(0x9E37_79B9);
    }
    let delta = timers::counter(timers::Timer::Timer2) as u32;
    timers::set_mode(timers::Timer::Timer2, 0);
    mix32(delta, acc)
}

fn timed_gte_rtps(iterations: u16) -> u16 {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    for _ in 0..iterations {
        seed_gte_state();
        unsafe { gte_ops::rtps() };
    }
    let delta = timers::counter(timers::Timer::Timer2);
    timers::set_mode(timers::Timer::Timer2, 0);
    delta
}

fn timed_otc_dma_wait() -> u16 {
    static mut OT: [u32; 16] = [0; 16];
    unsafe {
        let ptr = (&raw mut OT) as *mut u32;
        for i in 0..16 {
            ptr::write_volatile(ptr.add(i), 0);
        }
        dma::enable_channel(dma::Channel::Otc);
        dma::set_madr(dma::Channel::Otc, ptr.add(15) as u32);
        dma::set_bcr_manual(dma::Channel::Otc, 16);
        dma::set_chcr(
            dma::Channel::Otc,
            dma::CHCR_STEP_BACKWARD | dma::CHCR_SYNC_MANUAL | dma::CHCR_START | dma::CHCR_TRIGGER,
        );
        let mut polls = 0u16;
        while dma::is_busy(dma::Channel::Otc) && polls != 0xFFFF {
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
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes) }
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

struct Dec3 {
    bytes: [u8; 3],
}

impl Dec3 {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes) }
    }
}

fn dec3(value: u16) -> Dec3 {
    let value = value.min(999);
    let hundreds = value / 100;
    let tens = (value / 10) % 10;
    let ones = value % 10;
    Dec3 {
        bytes: [b'0' + hundreds as u8, b'0' + tens as u8, b'0' + ones as u8],
    }
}
