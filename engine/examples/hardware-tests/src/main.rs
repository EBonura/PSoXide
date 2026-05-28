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

use core::{mem, ptr};

use psx_engine::{button, App, Config, Ctx, Scene};
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, Resolution, VideoMode};
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::ops as gte_ops;
use psx_gte::regs::pack_xy as pack_gte_xy;
use psx_gte::{cfc2, ctc2, mfc2, mtc2, scene as gte_scene};
use psx_io::{cdrom, dma, gpu as gpu_io, timers};
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const SUITE_VERSION: &str = "HWTEST v0.2";
const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const ROWS_PER_PAGE: usize = 7;
const TEST_COUNT: usize = 36;
const PAD_POLL_TEST_INDEX: usize = 19;
const MODE_COUNT: u8 = 14;

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
            Self::DmaChecks => 3,
            Self::TimerChecks => 4,
            Self::GpuChecks => 5,
            Self::GteChecks => 6,
            Self::SpuChecks => 7,
            Self::CdromChecks => 8,
            Self::SioChecks => 9,
            Self::CpuScan => 10,
            Self::GteScan => 11,
            Self::SpuScan => 12,
            Self::TimingScan => 13,
        }
    }

    const fn from_index(index: u8) -> Self {
        match index % MODE_COUNT {
            0 => Self::AllChecks,
            1 => Self::CpuChecks,
            2 => Self::MemoryChecks,
            3 => Self::DmaChecks,
            4 => Self::TimerChecks,
            5 => Self::GpuChecks,
            6 => Self::GteChecks,
            7 => Self::SpuChecks,
            8 => Self::CdromChecks,
            9 => Self::SioChecks,
            10 => Self::CpuScan,
            11 => Self::GteScan,
            12 => Self::SpuScan,
            _ => Self::TimingScan,
        }
    }

    const fn is_check_section(self) -> bool {
        matches!(
            self,
            Self::AllChecks
                | Self::CpuChecks
                | Self::MemoryChecks
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
        group: "DMA",
        name: "OTC reverse linked-list clear",
        run: test_dma_otc_clear,
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
        group: "SIO",
        name: "port 1 pad poll",
        run: test_pad_poll,
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
        group: "SIO",
        name: "direct port 1 pad poll stability",
        run: test_pad_direct_stability,
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
        self.font = Some(FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT));
        self.run_all();
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
    font.draw_text(8, 34, "PASS", Status::Pass.color());
    font.draw_text(
        48,
        34,
        dec3(suite.pass_count as u16).as_str(),
        Status::Pass.color(),
    );
    font.draw_text(80, 34, "FAIL", Status::Fail.color());
    font.draw_text(
        120,
        34,
        dec3(suite.fail_count as u16).as_str(),
        Status::Fail.color(),
    );
    font.draw_text(152, 34, "WARN", Status::Warn.color());
    font.draw_text(
        192,
        34,
        dec3(suite.warn_count as u16).as_str(),
        Status::Warn.color(),
    );
    font.draw_text(224, 34, "INFO", Status::Info.color());
    font.draw_text(
        264,
        34,
        dec3(suite.info_count as u16).as_str(),
        Status::Info.color(),
    );

    font.draw_text(8, 220, "PAGE", (140, 160, 190));
    font.draw_text(
        48,
        220,
        dec3((suite.page + 1) as u16).as_str(),
        (220, 220, 220),
    );
    font.draw_text(80, 220, "OF", (140, 160, 190));
    font.draw_text(
        104,
        220,
        dec3(page_count_for_mode(suite.mode) as u16).as_str(),
        (220, 220, 220),
    );
    font.draw_text(232, 220, "RUN", (140, 160, 190));
    font.draw_text(
        264,
        220,
        dec3(suite.rerun_count as u16).as_str(),
        (220, 220, 220),
    );
    font.draw_text(144, 220, "1ST FAIL", (140, 160, 190));
}

fn draw_mode_menu(font: &FontAtlas, suite: &HardwareTests) {
    font.draw_text(8, 8, "PS1 HARDWARE TESTS", (232, 236, 244));
    font.draw_text(224, 8, SUITE_VERSION, (112, 136, 170));
    font.draw_text(8, 20, "SECTION", (140, 160, 190));
    font.draw_text(72, 20, suite.mode.label(), (255, 232, 128));
    font.draw_text(184, 20, "UP/DN NEXT", (140, 160, 190));
    font.draw_text(272, 20, "X RUN", (140, 160, 190));
}

fn draw_rows(font: &FontAtlas, suite: &HardwareTests, mode: Mode) {
    font.draw_text(8, 44, mode.description(), (112, 136, 170));
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
    font.draw_text(8, 174, "NOTE", (140, 160, 190));
    font.draw_text(80, 174, report.note, color);
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

fn draw_problem_detail(font: &FontAtlas, suite: &HardwareTests, mode: Mode) {
    let y = 198;
    match suite.first_problem(mode) {
        Some(index) => {
            let result = suite.results[index];
            font.draw_text(8, y, "DETAIL", result.status.color());
            font.draw_text(64, y, TESTS[index].name, (230, 230, 230));
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
        }
        None => {
            font.draw_text(8, y, "ALL HARD FAILURES CLEAR", Status::Pass.color());
            font.draw_text(
                8,
                y + 10,
                "NEXT: RUN IN REDUX DUCKSTATION REAL PS1",
                (150, 170, 200),
            );
        }
    }
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
        }
    }
    if suite.fail_count == 0 && suite.warn_count == 0 {
        tty::println("hardware-tests: all hard failures clear");
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
    tty::print(" note=");
    tty::println(report.note);
}

fn tty_print_dec_u8(value: u8) {
    tty_print_dec_u16(value as u16);
}

fn tty_print_dec_u16(value: u16) {
    let hundreds = value / 100;
    let tens = (value / 10) % 10;
    let ones = value % 10;
    if hundreds != 0 {
        tty_print_digit(hundreds as u8);
        tty_print_digit(tens as u8);
    } else if tens != 0 {
        tty_print_digit(tens as u8);
    }
    tty_print_digit(ones as u8);
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

fn run_cpu_scan() -> ScanReport {
    let mut hash = 0x811C_9DC5;
    let mut items = 0u16;

    macro_rules! sample_r {
        ($instr:expr, $rs:expr, $rt:expr) => {{
            let instr = $instr;
            let result = cpu_r::<{ $instr }>($rs, $rt);
            hash = mix32(hash, instr);
            hash = mix32(hash, $rs);
            hash = mix32(hash, $rt);
            hash = mix32(hash, result);
            items = items.wrapping_add(1);
        }};
    }
    macro_rules! sample_i {
        ($instr:expr, $rs:expr) => {{
            let instr = $instr;
            let result = cpu_i::<{ $instr }>($rs);
            hash = mix32(hash, instr);
            hash = mix32(hash, $rs);
            hash = mix32(hash, result);
            items = items.wrapping_add(1);
        }};
    }
    macro_rules! sample_hilo {
        ($instr:expr, $rs:expr, $rt:expr) => {{
            let instr = $instr;
            let (lo, hi) = cpu_hilo::<{ $instr }>($rs, $rt);
            hash = mix32(hash, instr);
            hash = mix32(hash, $rs);
            hash = mix32(hash, $rt);
            hash = mix32(hash, lo);
            hash = mix32(hash, hi);
            items = items.wrapping_add(1);
        }};
    }

    sample_r!(mips_r(8, 9, 10, 0, 0x21), 0x7FFF_FFFE, 3);
    sample_r!(mips_r(8, 9, 10, 0, 0x23), 0x8000_0002, 7);
    sample_r!(mips_r(8, 9, 10, 0, 0x24), 0xF0F0_A55A, 0x0FF0_5AA5);
    sample_r!(mips_r(8, 9, 10, 0, 0x25), 0xF0F0_A55A, 0x0FF0_5AA5);
    sample_r!(mips_r(8, 9, 10, 0, 0x26), 0xF0F0_A55A, 0x0FF0_5AA5);
    sample_r!(mips_r(0, 9, 10, 7, 0x00), 0, 0x0000_0123);
    sample_r!(mips_r(0, 9, 10, 5, 0x02), 0, 0x8000_0000);
    sample_r!(mips_r(0, 9, 10, 5, 0x03), 0, 0x8000_0000);
    sample_r!(mips_r(8, 9, 10, 0, 0x04), 9, 0x0000_0101);
    sample_r!(mips_r(8, 9, 10, 0, 0x06), 11, 0xF000_0000);
    sample_r!(mips_r(8, 9, 10, 0, 0x07), 11, 0xF000_0000);
    sample_r!(mips_r(8, 9, 10, 0, 0x2A), 0xFFFF_FFFF, 1);
    sample_r!(mips_r(8, 9, 10, 0, 0x2B), 1, 0xFFFF_FFFF);

    sample_i!(mips_i(0x09, 8, 10, 0x8001), 0x0000_0002);
    sample_i!(mips_i(0x0A, 8, 10, 0x0001), 0xFFFF_FFFF);
    sample_i!(mips_i(0x0B, 8, 10, 0xFFFF), 1);
    sample_i!(mips_i(0x0C, 8, 10, 0x5AA5), 0xF0F0_A55A);
    sample_i!(mips_i(0x0D, 8, 10, 0x5AA5), 0xF0F0_0000);
    sample_i!(mips_i(0x0E, 8, 10, 0x5AA5), 0xF0F0_F0F0);
    sample_i!(mips_i(0x0F, 0, 10, 0xBEEF), 0);

    sample_hilo!(mips_r(8, 9, 0, 0, 0x18), 0xFFFF_FFFD, 7);
    sample_hilo!(mips_r(8, 9, 0, 0, 0x19), 0xFFFF_FFFD, 7);
    sample_hilo!(mips_r(8, 9, 0, 0, 0x1A), 0xFFFF_FFFD, 2);
    sample_hilo!(mips_r(8, 9, 0, 0, 0x1B), 0xFFFF_FFFD, 2);

    let (lo, hi) = cpu_mthi_mtlo(0x1357_2468, 0x89AB_CDEF);
    hash = mix32(hash, lo);
    hash = mix32(hash, hi);
    items = items.wrapping_add(1);

    hash = mix32(hash, cpu_branch_delay_battery());
    hash = mix32(hash, cpu_load_store_battery());
    items = items.wrapping_add(2);

    ScanReport::info(items, hash, 0, "safe mips-i forms")
}

fn run_gte_scan() -> ScanReport {
    let mut hash = 0x4754_4501;
    let mut items = 0u16;
    let mut flag_master_hits = 0u32;

    for opcode in 0..64u32 {
        for sf in 0..2u32 {
            for lm in 0..2u32 {
                for mx in 0..4u32 {
                    for vx in 0..4u32 {
                        for cv in 0..4u32 {
                            let instr = 0x4A00_0000
                                | (sf << 19)
                                | (mx << 17)
                                | (vx << 15)
                                | (cv << 13)
                                | (lm << 10)
                                | opcode;
                            seed_gte_state();
                            unsafe {
                                execute_raw_gte(instr);
                            }
                            let snapshot = gte_snapshot_hash();
                            if cfc2!(31) & 0x8000_0000 != 0 {
                                flag_master_hits = flag_master_hits.wrapping_add(1);
                            }
                            hash = mix32(hash, instr);
                            hash = mix32(hash, snapshot);
                            items = items.wrapping_add(1);
                        }
                    }
                }
            }
        }
    }

    ScanReport::info(items, hash, flag_master_hits, "cop2 command matrix")
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

#[repr(align(16))]
struct CodeStub([u32; 4]);

static mut GTE_SCAN_STUB: CodeStub = CodeStub([0; 4]);

unsafe fn execute_raw_gte(instr: u32) {
    #[cfg(target_arch = "mips")]
    unsafe {
        let cached = (&raw mut GTE_SCAN_STUB.0) as u32;
        let uncached = (0xA000_0000 | (cached & 0x001F_FFFF)) as *mut u32;
        ptr::write_volatile(uncached.add(0), instr);
        ptr::write_volatile(uncached.add(1), 0x03E0_0008); // jr ra
        ptr::write_volatile(uncached.add(2), 0x0000_0000); // delay slot
        ptr::write_volatile(uncached.add(3), 0x0000_0000);
        let func: extern "C" fn() = mem::transmute(uncached);
        func();
    }
    #[cfg(not(target_arch = "mips"))]
    {
        psx_gte::host::execute(instr);
    }
}

fn test_cpu_endian() -> TestResult {
    static mut WORD: u32 = 0;
    unsafe {
        let word = &raw mut WORD;
        ptr::write_volatile(word, 0x4433_2211);
        let bytes = word as *const u8;
        let observed = (ptr::read_volatile(bytes.add(0)) as u32)
            | ((ptr::read_volatile(bytes.add(1)) as u32) << 8)
            | ((ptr::read_volatile(bytes.add(2)) as u32) << 16)
            | ((ptr::read_volatile(bytes.add(3)) as u32) << 24);
        expect_eq(0x4433_2211, observed, "byte order")
    }
}

fn test_cpu_arithmetic() -> TestResult {
    let mut observed = 0u32;
    if 0x7FFF_FFFFu32.wrapping_add(1) == 0x8000_0000 {
        observed |= 1;
    }
    if (((0x8000_0000u32 as i32) >> 31) as u32) == 0xFFFF_FFFF {
        observed |= 2;
    }
    if 0x1234_5678u32.wrapping_mul(9) == 0xA3D7_0A38 {
        observed |= 4;
    }
    expect_eq(0x7, observed, "alu bits")
}

fn test_cpu_rtype_opcodes() -> TestResult {
    let mut observed = 0u32;
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x21) }>(0x1000_0000, 0x0000_0007) == 0x1000_0007 {
        observed |= 1 << 0; // ADDU
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x23) }>(0x1000_0000, 0x0000_0007) == 0x0FFF_FFF9 {
        observed |= 1 << 1; // SUBU
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x24) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0x00F0_0000 {
        observed |= 1 << 2; // AND
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x25) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0xFFF0_FFFF {
        observed |= 1 << 3; // OR
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x26) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0xFF00_FFFF {
        observed |= 1 << 4; // XOR
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x27) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0x000F_0000 {
        observed |= 1 << 5; // NOR
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x2A) }>(0xFFFF_FFFF, 0x0000_0001) == 1 {
        observed |= 1 << 6; // SLT
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x2B) }>(0x0000_0001, 0xFFFF_FFFF) == 1 {
        observed |= 1 << 7; // SLTU
    }
    if cpu_r::<{ mips_r(0, 9, 10, 3, 0x00) }>(0, 0x0000_0011) == 0x0000_0088 {
        observed |= 1 << 8; // SLL
    }
    if cpu_r::<{ mips_r(0, 9, 10, 4, 0x02) }>(0, 0x8000_0000) == 0x0800_0000 {
        observed |= 1 << 9; // SRL
    }
    if cpu_r::<{ mips_r(0, 9, 10, 4, 0x03) }>(0, 0x8000_0000) == 0xF800_0000 {
        observed |= 1 << 10; // SRA
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x04) }>(3, 0x0000_0011) == 0x0000_0088 {
        observed |= 1 << 11; // SLLV
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x06) }>(4, 0x8000_0000) == 0x0800_0000 {
        observed |= 1 << 12; // SRLV
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x07) }>(4, 0x8000_0000) == 0xF800_0000 {
        observed |= 1 << 13; // SRAV
    }
    expect_eq(0x3FFF, observed, "rtype")
}

fn test_cpu_immediate_opcodes() -> TestResult {
    let mut observed = 0u32;
    if cpu_i::<{ mips_i(0x09, 8, 10, 0x7FFF) }>(0x0000_0001) == 0x0000_8000 {
        observed |= 1 << 0; // ADDIU
    }
    if cpu_i::<{ mips_i(0x0C, 8, 10, 0x0FF0) }>(0xF0F0_A55A) == 0x0000_0550 {
        observed |= 1 << 1; // ANDI
    }
    if cpu_i::<{ mips_i(0x0D, 8, 10, 0x00FF) }>(0x1234_0000) == 0x1234_00FF {
        observed |= 1 << 2; // ORI
    }
    if cpu_i::<{ mips_i(0x0E, 8, 10, 0x00FF) }>(0x1234_00F0) == 0x1234_000F {
        observed |= 1 << 3; // XORI
    }
    if cpu_i::<{ mips_i(0x0A, 8, 10, 0x0001) }>(0xFFFF_FFFF) == 1 {
        observed |= 1 << 4; // SLTI
    }
    if cpu_i::<{ mips_i(0x0B, 8, 10, 0xFFFF) }>(0x0000_0001) == 1 {
        observed |= 1 << 5; // SLTIU
    }
    if cpu_i::<{ mips_i(0x0F, 0, 10, 0x1234) }>(0) == 0x1234_0000 {
        observed |= 1 << 6; // LUI
    }
    expect_eq(0x7F, observed, "itype")
}

fn test_cpu_hilo_opcodes() -> TestResult {
    let (mult_lo, mult_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x18) }>(0xFFFF_FFFE, 3);
    let (multu_lo, multu_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x19) }>(0xFFFF_FFFE, 3);
    let (div_lo, div_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x1A) }>(0xFFFF_FFFD, 2);
    let (divu_lo, divu_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x1B) }>(7, 3);
    let (mt_lo, mt_hi) = cpu_mthi_mtlo(0x1357_2468, 0x89AB_CDEF);

    let mut observed = 0u32;
    if (mult_lo, mult_hi) == (0xFFFF_FFFA, 0xFFFF_FFFF) {
        observed |= 1 << 0; // MULT
    }
    if (multu_lo, multu_hi) == (0xFFFF_FFFA, 0x0000_0002) {
        observed |= 1 << 1; // MULTU
    }
    if (div_lo, div_hi) == (0xFFFF_FFFF, 0xFFFF_FFFF) {
        observed |= 1 << 2; // DIV
    }
    if (divu_lo, divu_hi) == (2, 1) {
        observed |= 1 << 3; // DIVU
    }
    if (mt_lo, mt_hi) == (0x89AB_CDEF, 0x1357_2468) {
        observed |= 1 << 4; // MTHI/MTLO + MFHI/MFLO
    }
    expect_eq(0x1F, observed, "hilo")
}

fn test_cpu_branch_delay_opcodes() -> TestResult {
    let observed = cpu_branch_delay_battery();
    expect_eq(0x1FF, observed, "branch")
}

fn test_cpu_load_store_opcodes() -> TestResult {
    let observed = cpu_load_store_battery();
    expect_eq(0x1FF, observed, "load/store")
}

#[inline(never)]
fn cpu_r<const INSTR: u32>(rs_value: u32, rt_value: u32) -> u32 {
    #[cfg(target_arch = "mips")]
    {
        let out: u32;
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                instr = const INSTR,
                in("$8") rs_value,
                in("$9") rt_value,
                lateout("$10") out,
                options(nostack, nomem, preserves_flags),
            );
        }
        out
    }
    #[cfg(not(target_arch = "mips"))]
    {
        emulate_cpu_r(INSTR, rs_value, rt_value)
    }
}

#[inline(never)]
fn cpu_i<const INSTR: u32>(rs_value: u32) -> u32 {
    #[cfg(target_arch = "mips")]
    {
        let out: u32;
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                instr = const INSTR,
                in("$8") rs_value,
                lateout("$10") out,
                options(nostack, nomem, preserves_flags),
            );
        }
        out
    }
    #[cfg(not(target_arch = "mips"))]
    {
        emulate_cpu_i(INSTR, rs_value)
    }
}

#[inline(never)]
fn cpu_hilo<const INSTR: u32>(rs_value: u32, rt_value: u32) -> (u32, u32) {
    #[cfg(target_arch = "mips")]
    {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word {mflo}",
                ".word {mfhi}",
                ".word 0",
                instr = const INSTR,
                mflo = const mips_r(0, 0, 10, 0, 0x12),
                mfhi = const mips_r(0, 0, 11, 0, 0x10),
                in("$8") rs_value,
                in("$9") rt_value,
                lateout("$10") lo,
                lateout("$11") hi,
                options(nostack, nomem, preserves_flags),
            );
        }
        (lo, hi)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        emulate_cpu_hilo(INSTR, rs_value, rt_value)
    }
}

#[inline(never)]
fn cpu_mthi_mtlo(hi_value: u32, lo_value: u32) -> (u32, u32) {
    #[cfg(target_arch = "mips")]
    {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!(
                ".word {mthi}",
                ".word {mtlo}",
                ".word {mflo}",
                ".word {mfhi}",
                ".word 0",
                mthi = const mips_r(8, 0, 0, 0, 0x11),
                mtlo = const mips_r(9, 0, 0, 0, 0x13),
                mflo = const mips_r(0, 0, 10, 0, 0x12),
                mfhi = const mips_r(0, 0, 11, 0, 0x10),
                in("$8") hi_value,
                in("$9") lo_value,
                lateout("$10") lo,
                lateout("$11") hi,
                options(nostack, nomem, preserves_flags),
            );
        }
        (lo, hi)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        (lo_value, hi_value)
    }
}

#[inline(never)]
fn cpu_branch_delay_battery() -> u32 {
    #[cfg(target_arch = "mips")]
    {
        let out: u32;
        unsafe {
            core::arch::asm!(
                ".word {clear}",
                ".word {beq_taken}",
                ".word {delay_1}",
                ".word {skipped_100}",
                ".word {bne_not_taken}",
                ".word {delay_2}",
                ".word {fallthrough_4}",
                ".word {beq_always}",
                ".word {delay_8}",
                ".word {skipped_200}",
                ".word {set_neg}",
                ".word {blez_taken}",
                ".word {delay_16}",
                ".word {skipped_400}",
                ".word {bgtz_not_taken}",
                ".word {delay_32}",
                ".word {fallthrough_64}",
                ".word {set_neg}",
                ".word {bltz_taken}",
                ".word {delay_128}",
                ".word {skipped_800}",
                ".word {bgez_taken}",
                ".word {delay_256}",
                ".word {skipped_1000}",
                clear = const mips_i(0x09, 0, 10, 0),
                beq_taken = const mips_i(0x04, 8, 8, 2),
                delay_1 = const mips_i(0x0D, 10, 10, 1),
                skipped_100 = const mips_i(0x0D, 10, 10, 0x0100),
                bne_not_taken = const mips_i(0x05, 8, 8, 1),
                delay_2 = const mips_i(0x0D, 10, 10, 2),
                fallthrough_4 = const mips_i(0x0D, 10, 10, 4),
                beq_always = const mips_i(0x04, 0, 0, 2),
                delay_8 = const mips_i(0x0D, 10, 10, 8),
                skipped_200 = const mips_i(0x0D, 10, 10, 0x0200),
                set_neg = const mips_i(0x09, 0, 11, 0xFFFF),
                blez_taken = const mips_i(0x06, 11, 0, 2),
                delay_16 = const mips_i(0x0D, 10, 10, 16),
                skipped_400 = const mips_i(0x0D, 10, 10, 0x0400),
                bgtz_not_taken = const mips_i(0x07, 0, 0, 1),
                delay_32 = const mips_i(0x0D, 10, 10, 32),
                fallthrough_64 = const mips_i(0x0D, 10, 10, 64),
                bltz_taken = const mips_i(0x01, 11, 0, 2),
                delay_128 = const mips_i(0x0D, 10, 10, 128),
                skipped_800 = const mips_i(0x0D, 10, 10, 0x0800),
                bgez_taken = const mips_i(0x01, 0, 1, 2),
                delay_256 = const mips_i(0x0D, 10, 10, 256),
                skipped_1000 = const mips_i(0x0D, 10, 10, 0x1000),
                in("$8") 1u32,
                lateout("$10") out,
                lateout("$11") _,
                options(nostack, nomem, preserves_flags),
            );
        }
        out
    }
    #[cfg(not(target_arch = "mips"))]
    {
        0x1FF
    }
}

#[repr(align(4))]
struct AlignedBytes([u8; 16]);

#[inline(never)]
fn cpu_load_store_battery() -> u32 {
    static mut BUF: AlignedBytes = AlignedBytes([0; 16]);
    #[cfg(target_arch = "mips")]
    unsafe {
        let base = (&raw mut BUF.0) as *mut u8;
        for i in 0..16 {
            ptr::write_volatile(base.add(i), 0);
        }
        ptr::write_volatile(base.add(8) as *mut u32, 0xA5A5_1357);
        let lw: u32;
        let lh: u32;
        let lhu: u32;
        let lb: u32;
        let lbu: u32;
        let delayed: u32;
        let loaded_after_delay: u32;
        core::arch::asm!(
            ".word {sw}",
            ".word {lw}",
            ".word {sh}",
            ".word {lh}",
            ".word {lhu}",
            ".word {sb}",
            ".word {lb}",
            ".word {lbu}",
            ".word 0",
            sw = const mips_i(0x2B, 8, 9, 0),
            lw = const mips_i(0x23, 8, 10, 0),
            sh = const mips_i(0x29, 8, 11, 4),
            lh = const mips_i(0x21, 8, 12, 4),
            lhu = const mips_i(0x25, 8, 13, 4),
            sb = const mips_i(0x28, 8, 14, 6),
            lb = const mips_i(0x20, 8, 15, 6),
            lbu = const mips_i(0x24, 8, 24, 6),
            in("$8") base as u32,
            in("$9") 0x1234_5678u32,
            in("$11") 0xFFFF_80FEu32,
            in("$14") 0x0000_00F2u32,
            lateout("$10") lw,
            lateout("$12") lh,
            lateout("$13") lhu,
            lateout("$15") lb,
            lateout("$24") lbu,
            options(nostack, preserves_flags),
        );
        core::arch::asm!(
            ".word {set_old}",
            ".word {lw_delay}",
            ".word {capture_delay_slot}",
            ".word 0",
            set_old = const mips_i(0x09, 0, 10, 5),
            lw_delay = const mips_i(0x23, 8, 10, 8),
            capture_delay_slot = const mips_r(10, 0, 25, 0, 0x21),
            in("$8") base as u32,
            lateout("$10") loaded_after_delay,
            lateout("$25") delayed,
            options(nostack, preserves_flags),
        );
        let mut observed = cpu_load_store_observed(base, lw, lh, lhu, lb, lbu);
        if delayed == 5 && loaded_after_delay == 0xA5A5_1357 {
            observed |= 1 << 8;
        }
        observed
    }
    #[cfg(not(target_arch = "mips"))]
    unsafe {
        let base = (&raw mut BUF.0) as *mut u8;
        ptr::write_volatile(base as *mut u32, 0x1234_5678);
        ptr::write_volatile(base.add(4) as *mut u16, 0x80FE);
        ptr::write_volatile(base.add(6), 0xF2);
        cpu_load_store_observed(
            base,
            0x1234_5678,
            0xFFFF_80FE,
            0x0000_80FE,
            0xFFFF_FFF2,
            0xF2,
        ) | (1 << 8)
    }
}

unsafe fn cpu_load_store_observed(
    base: *mut u8,
    lw: u32,
    lh: u32,
    lhu: u32,
    lb: u32,
    lbu: u32,
) -> u32 {
    let mut observed = 0u32;
    if lw == 0x1234_5678 {
        observed |= 1 << 0;
    }
    if ptr::read_volatile(base.add(0)) == 0x78 && ptr::read_volatile(base.add(3)) == 0x12 {
        observed |= 1 << 1;
    }
    if lh == 0xFFFF_80FE {
        observed |= 1 << 2;
    }
    if lhu == 0x0000_80FE {
        observed |= 1 << 3;
    }
    if ptr::read_volatile(base.add(4)) == 0xFE && ptr::read_volatile(base.add(5)) == 0x80 {
        observed |= 1 << 4;
    }
    if lb == 0xFFFF_FFF2 {
        observed |= 1 << 5;
    }
    if lbu == 0x0000_00F2 {
        observed |= 1 << 6;
    }
    if ptr::read_volatile(base.add(6)) == 0xF2 {
        observed |= 1 << 7;
    }
    observed
}

#[cfg(not(target_arch = "mips"))]
fn emulate_cpu_r(instr: u32, rs_value: u32, rt_value: u32) -> u32 {
    let shamt = (instr >> 6) & 0x1F;
    match instr & 0x3F {
        0x00 => rt_value << shamt,
        0x02 => rt_value >> shamt,
        0x03 => ((rt_value as i32) >> shamt) as u32,
        0x04 => rt_value << (rs_value & 0x1F),
        0x06 => rt_value >> (rs_value & 0x1F),
        0x07 => ((rt_value as i32) >> (rs_value & 0x1F)) as u32,
        0x21 => rs_value.wrapping_add(rt_value),
        0x23 => rs_value.wrapping_sub(rt_value),
        0x24 => rs_value & rt_value,
        0x25 => rs_value | rt_value,
        0x26 => rs_value ^ rt_value,
        0x27 => !(rs_value | rt_value),
        0x2A => ((rs_value as i32) < (rt_value as i32)) as u32,
        0x2B => (rs_value < rt_value) as u32,
        _ => 0,
    }
}

#[cfg(not(target_arch = "mips"))]
fn emulate_cpu_i(instr: u32, rs_value: u32) -> u32 {
    let imm = instr as u16;
    match (instr >> 26) & 0x3F {
        0x09 => rs_value.wrapping_add((imm as i16 as i32) as u32),
        0x0A => ((rs_value as i32) < (imm as i16 as i32)) as u32,
        0x0B => (rs_value < (imm as i16 as i32 as u32)) as u32,
        0x0C => rs_value & imm as u32,
        0x0D => rs_value | imm as u32,
        0x0E => rs_value ^ imm as u32,
        0x0F => (imm as u32) << 16,
        _ => 0,
    }
}

#[cfg(not(target_arch = "mips"))]
fn emulate_cpu_hilo(instr: u32, rs_value: u32, rt_value: u32) -> (u32, u32) {
    match instr & 0x3F {
        0x18 => {
            let value = (rs_value as i32 as i64).wrapping_mul(rt_value as i32 as i64);
            (value as u32, (value >> 32) as u32)
        }
        0x19 => {
            let value = (rs_value as u64).wrapping_mul(rt_value as u64);
            (value as u32, (value >> 32) as u32)
        }
        0x1A => {
            let a = rs_value as i32;
            let b = rt_value as i32;
            ((a / b) as u32, (a % b) as u32)
        }
        0x1B => (rs_value / rt_value, rs_value % rt_value),
        _ => (0, 0),
    }
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
    expect_eq(0x3, observed, "irq")
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
    let sys = timer_delta(timers::Timer::Timer0, 0, 8192);
    let dot = timer_delta(timers::Timer::Timer0, TIMER_MODE_CLOCK_SOURCE_1, 8192);
    timers::set_mode(timers::Timer::Timer0, 0);
    let observed = ((sys > 0) as u32)
        | (((dot > 0) as u32) << 1)
        | (((sys as u32) >= (dot as u32).saturating_mul(2)) as u32) << 2
        | (((sys as u32) <= (dot as u32).saturating_mul(16)) as u32) << 3;
    expect_eq(0xF, observed, "dot/sys")
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
