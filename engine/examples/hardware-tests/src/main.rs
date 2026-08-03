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

use hello_memcard_recovery::Diagnostic as MemoryCardDiagnostic;
use psx_engine::{button, App, Config, Ctx, Scene};
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, prim, Resolution, VideoMode};
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::ops as gte_ops;
use psx_gte::regs::pack_xy as pack_gte_xy;
use psx_gte::{cfc2, ctc2, mfc2, mtc2, scene as gte_scene};
use psx_io::{cdrom, dma, gpu as gpu_io, irq, sio, timers};
use psx_rt::tty;
use psx_spu::SpuAddr;
use psx_vram::{Clut, TexDepth, Tpage};

mod audio_link;
mod cd_chain_probe;
mod controller_test;
mod audio_probe;
mod cpu_tests;
mod handoff_probe;
mod photo;
mod reverb_probe;
mod sample_probe;
mod spu_probe;
mod transition_probe;
mod voice_probe;
use audio_probe::AudioProbe;
use cpu_tests::*;
use cd_chain_probe::CdChainProbe;
use controller_test::ControllerTest;
use handoff_probe::HandoffProbe;
use photo::PhotoCapture;
use reverb_probe::{ReverbProbe, ReverbSnapshot};
use transition_probe::TransitionProbe;
use voice_probe::VoiceProbe;
use sample_probe::SampleProbe;
use spu_probe::SpuProbe;

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
    // and 2 of separate 16-byte cache lines. The non-executed SLL-to-zero
    // markers make their final linked layouts machine-verifiable.
    ".section .text.hwtest_icache_entries",
    ".balign 4096",
    ".globl __hwtest_icache_entry_w0",
    "__hwtest_icache_entry_w0:",
    "jr $10",
    "nop",
    ".word 0x00000500",
    ".word 0x00000540",
    ".balign 16",
    ".word 0x00000580",
    ".globl __hwtest_icache_entry_w1",
    "__hwtest_icache_entry_w1:",
    "jr $10",
    "nop",
    ".word 0x000005C0",
    ".balign 16",
    ".word 0x00000600",
    ".word 0x00000640",
    ".globl __hwtest_icache_entry_w2",
    "__hwtest_icache_entry_w2:",
    "jr $10",
    "nop",
    ".set reorder",
);

unsafe extern "C" {
    fn __hwtest_icache_block();
    fn __hwtest_icache_entry_w0();
    fn __hwtest_icache_entry_w1();
    fn __hwtest_icache_entry_w2();
}

// Suite version, written into every payload so a capture is self-identifying.
//
// The transport schema (PX7) and the suite version are different things: the
// schema says how bytes are laid out, the suite version says what a record id
// MEANS. Comparing a capture from one suite version against a baseline from
// another is the trap this exists to prevent, because record 0xA0 can be
// redefined while the byte layout stays identical.
//
// MAJOR: bump when an existing record's meaning changes -- a probe redefined,
//        a clock swapped, sample semantics altered. Captures across a MAJOR
//        boundary are NOT comparable.
// MINOR: bump when records are only added, or a bug is fixed that leaves every
//        existing record measuring the same thing. Captures remain comparable
//        for the records they share.
//
// v1.10: SB2, aimed at the wider SPU problem: Celeste's looped
//       wavetables and VoXide's sample bank are both wrong on console while
//       CD-DA -- the one path that never stores in SPU RAM -- is fine. Pass
//       1 uploads a known pattern and reads it back over four DMA/PIO
//       combinations, so a bad upload, a bad readback and a lying emulator
//       are told apart; SB1's console capture already hashed SPU RAM back
//       differently from the emulator, which is the lead. Pass 2 plays tones
//       whose frequency is arithmetic (a synthesised square table at known
//       pitches, 1.5s on 0.5s off) so an OBS recording measures what the
//       speaker got while the QR carries what the registers said.
// v1.9: SB1, the UI sample end/loop probe: the demo-disc launcher blip
//       repeats aggressively on console, and the suspect is the blip sample's
//       own ADPCM terminator under an ADSR that sustains forever. SB1 audits
//       both launcher blips' block flags, replays the exact launcher voice
//       path (plus retrigger, key_off and percussive variants), and traces
//       envelope/ENDX into one QR. Frozen record schema unchanged.
// v1.8: Adds a polished, interactive two-port controller test. It tracks every
//       button independently, enables and visualises both DualShock sticks,
//       and samples their resting offset for drift. The frozen PX7 record
//       schema is unchanged; the minor bump identifies the newly linked binary.
// v1.7: The CD-DA contention pair (0x9B/0x9C) normalises with PAUSE instead
//       of STOP. The 2026-07-31 console run proved STOP+respin grinds the
//       mech for minutes right after the contention read; with the motor
//       kept up, 0x9C measures the read it always claimed to measure
//       instead of a spin-up. Also: the scan draws the in-flight record id
//       as bit-cells, and START skips mid-record.
// v1.6: Menu-first boot and the shared memory-card hardware diagnostic. The
//       record schema is unchanged, but linking the diagnostic moves timing
//       code, so machine-code and emulator timing baselines are pinned to this
//       binary rather than compared byte-for-byte with v1.5.
// v1.5: Sweeps aimed at two findings the first full capture could not settle.
//       Ten seek distances plus two backward seeks, because four forward
//       distances came back non-monotonic and no fit reached the middle
//       points. Twelve SIO setup delays, because the console answered at 0,
//       fell silent at 128 and answered again at 384.
// v1.4: The capture is frozen when taken. Paging previously REBUILT the whole
//       payload, and some observations are live (the pad poll is refreshed
//       every frame), so each page carried a different payload while only the
//       last page's CRC described its own bytes. No multi-page console capture
//       could ever reconstruct. This is why three captures failed.
// v1.3: Audio readout holds its level. v1.2 keyed the voice with an all-zero
//       ADSR, which is sustain level 0, so on hardware the envelope decayed and
//       a console capture carried ~3 seconds of a 13.6 second payload. The
//       emulator does not model that decay, so only silicon could show it.
// v1.2: Audio readout on by default. Off-by-default cost a console session:
//       the operator has no reason to know a silent disc is withholding the
//       payload, and the QR route then lost a symbol, which costs the whole
//       capture. Volume stays at the reduced level.
// v1.1: Operator flow. Boot runs the battery behind a visible progress bar,
//       then lands on the capture pages; a main menu (TRIANGLE) reruns the
//       startup tests or opens results, scans and probes; the audio readout is
//       silent until asked for. No record changed meaning, but the guest binary
//       did, and timing records shift with code alignment, so the baseline was
//       re-pinned.
// v1.0: PX7. CD/CD-DA/GPU/MDEC/SIO batteries, interrupt-masked sampling,
//       median column, explicit record ids, raster hashes, audio readout.
//       Supersedes the v0.18 suite, whose records used a different sampling
//       method and cannot be compared against these.
const SUITE_VERSION_MAJOR: u8 = 1;
const SUITE_VERSION_MINOR: u8 = 11;
/// Display form. Keep in step with the two constants above.
const SUITE_VERSION: &str = "HWTEST v1.11";
const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const ROWS_PER_PAGE: usize = 6;
const TEST_COUNT: usize = 173;
const PAD_POLL_TEST_INDEX: usize = 26;

/// Number of timing variants the controller probe sweeps.
const PROBE_VARIANT_COUNT: usize = 5;
/// `(label, setup_spins, interbyte_spins)` the controller probe tries each frame
/// to find what timing a strict original pad (SCPH-1200) needs. `setup_spins` is
/// a delay after asserting the select line; `interbyte_spins` is a fixed gap
/// after each byte. Both are bounded STAT reads, no `/ACK`/CTRL machinery.
/// Setup-delay values swept by records 0xD0.., in SIO spin units.
const SIO_SETUP_SWEEP: [u32; 12] = [
    0, 64, 128, 192, 256, 320, 448, 512, 640, 896, 1024, 1536,
];

const PROBE_VARIANTS: [(&str, u32, u32); PROBE_VARIANT_COUNT] = [
    ("SETUP 0", 0, 0),
    ("SETUP 128", 128, 0),
    ("SETUP 384", 384, 0),
    ("SETUP 768", 768, 0),
    ("SETUP 2K", 2048, 0),
];
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

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    /// Main menu. This is always the boot mode.
    Menu,
    MemoryCard,
    ReverbProbe,
    HandoffProbe,
    CdChainProbe,
    TransitionProbe,
    VoiceProbe,
    AudioProbe,
    /// UI sample end/loop probe: the demo-disc launcher blip bug (SB1).
    SampleProbe,
    /// SPU RAM integrity then a controlled tone ladder (SB2).
    SpuProbe,
    /// Friendly operator-facing pad and analog-drift diagnostic. Kept
    /// separate from `ControllerProbe`, which measures SIO handshake timing.
    ControllerTest,
    ControllerProbe,
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
    /// Flat video-level chart for a TV or capture card. Not a measurement:
    /// the console is the instrument, the operator's display is what is
    /// under test.
    VideoLevels,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Self::Menu => "MAIN MENU",
            Self::MemoryCard => "MEMORY CARD",
            Self::ReverbProbe => "HL REVERB STATE",
            Self::HandoffProbe => "HL VOICE HANDOFF",
            Self::CdChainProbe => "CD CHAIN-LOAD",
            Self::TransitionProbe => "HL BANK TRANSITION",
            Self::VoiceProbe => "HL VOICE BANK",
            Self::AudioProbe => "CD/SPU AUDIO",
            Self::SampleProbe => "UI SAMPLE PROBE",
            Self::SpuProbe => "SPU DIAGNOSTIC",
            Self::ControllerTest => "CONTROLLER TEST",
            Self::ControllerProbe => "CONTROLLER PROBE",
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
            Self::VideoLevels => "VIDEO LEVELS",
        }
    }

    const fn hint(self) -> &'static str {
        match self {
            Self::Menu => "UP/DOWN SELECT  CROSS RUN",
            Self::MemoryCard => "L/R PAGE  L1+R1+X GUARDED ACTION",
            Self::ReverbProbe
            | Self::HandoffProbe
            | Self::CdChainProbe
            | Self::TransitionProbe
            | Self::VoiceProbe
            | Self::AudioProbe
            | Self::SampleProbe
            | Self::SpuProbe => "AUTOMATIC  X RERUN WHEN COMPLETE",
            Self::ControllerTest => "LIVE P1+P2  HOLD START+SELECT FOR MENU",
            Self::ControllerProbe => "DOWN=TESTS  NO PAD NEEDED TO READ THIS",
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
            Self::TimingScan => "L/R CAPTURE PAGE  X RESAMPLE TIMING",
            Self::VideoLevels => "X NEXT FIELD  START MENU",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Menu => "SELECT A TEST - NOTHING RUNS UNTIL CHOSEN",
            Self::MemoryCard => "NON-DESTRUCTIVE FULL-CARD READ/WRITE TEST",
            Self::ReverbProbe => "BIOS REVERB STATE + MAP DMA RESET VARIANTS QR",
            Self::HandoffProbe => "SELECTABLE MENU-VOICE SHUTDOWN + BANK SPLIT QR",
            Self::CdChainProbe => "CD READ MECHANISM MATRIX QR (CL2)",
            Self::TransitionProbe => "EXACT FULL/LIGHT/MAP LIVE-BANK HANDOFF QR",
            Self::VoiceProbe => "HL BANK DMA + VOICE 15 END GUARD QR",
            Self::AudioProbe => "READN AUDIO PATH + CAPTURE BUFFER QR",
            Self::SampleProbe => "UI BLIP END/LOOP FLAGS + ENVELOPE TRACE QR (SB1)",
            Self::SpuProbe => "SPU RAM INTEGRITY + MEASURABLE TONES QR (SB2)",
            Self::ControllerTest => "BUTTON HISTORY + ANALOG CENTRE/DRIFT TEST",
            Self::ControllerProbe => "RAW PAD HANDSHAKE: NO-WAIT VS ACK-WAIT",
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
            Self::VideoLevels => "GREY RAMP AND FLAT FIELDS FOR TV/CAPTURE",
        }
    }

    const fn aux_label(self) -> &'static str {
        match self {
            Self::Menu => "MENU",
            Self::MemoryCard => "CARD",
            Self::ReverbProbe
            | Self::HandoffProbe
            | Self::CdChainProbe
            | Self::TransitionProbe
            | Self::VoiceProbe
            | Self::AudioProbe
            | Self::SampleProbe
            | Self::SpuProbe => "CAPTURE",
            Self::ControllerTest => "LIVE",
            Self::ControllerProbe => "ACK",
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
            Self::TimingScan => "JITTER",
            Self::VideoLevels => "FIELD",
        }
    }

    /// Stable id used in the section-report hash. Values must not change:
    /// they are mixed into digests that checked-in baselines pin.
    const fn index(self) -> u8 {
        match self {
            // Menu is not a measurement section and never reaches a report,
            // so it takes a value outside the original range.
            Self::Menu => u8::MAX,
            // Interactive diagnostic only; it is not part of the frozen
            // conformance report schema.
            Self::MemoryCard => u8::MAX - 2,
            // Interactive diagnostic like MemoryCard: never part of the
            // frozen conformance report schema.
            Self::CdChainProbe => u8::MAX - 3,
            // Friendly input diagnostic only; not part of the frozen report.
            Self::ControllerTest => u8::MAX - 4,
            // Targeted diagnostic for the demo-disc blip bug; not part of
            // the frozen conformance report schema.
            Self::SampleProbe => u8::MAX - 5,
            Self::SpuProbe => u8::MAX - 6,
            // Same reasoning as Menu: this draws a chart for the operator's
            // display, it never produces a result row or a report.
            Self::VideoLevels => u8::MAX - 1,
            Self::ReverbProbe => 0,
            Self::HandoffProbe => 1,
            Self::TransitionProbe => 2,
            Self::VoiceProbe => 3,
            Self::AudioProbe => 4,
            Self::ControllerProbe => 5,
            Self::AllChecks => 6,
            Self::CpuChecks => 7,
            Self::MemoryChecks => 8,
            Self::IrqChecks => 9,
            Self::DmaChecks => 10,
            Self::TimerChecks => 11,
            Self::GpuChecks => 12,
            Self::GteChecks => 13,
            Self::SpuChecks => 14,
            Self::CdromChecks => 15,
            Self::SioChecks => 16,
            Self::CpuScan => 17,
            Self::GteScan => 18,
            Self::SpuScan => 19,
            Self::TimingScan => 20,
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

const TIMING_RECORD_COUNT: usize = 176;
const MEMORY_CONTROL_REGISTER_COUNT: usize = 9;
const PRECISION_VALUE_COUNT: usize = 192;

/// Samples per timing record. The minimum rejects interrupt interference, the
/// maximum exposes it, and the median says whether the spread is one stray
/// event or a genuinely bimodal distribution (cache or DRAM-refresh
/// interaction) that a min/max pair cannot distinguish.
const TIMING_SAMPLES: usize = 5;

/// Which menu page is showing.
///
/// Two levels rather than one flat list: 21 reachable modes do not fit on a
/// 240-line display, and a scrolling list means paging past eleven result
/// sections to reach the probes. Every page here fits on screen at once.
#[derive(Copy, Clone, PartialEq, Eq)]
enum MenuPage {
    Root,
    Results,
    Scans,
    Probes,
}

/// What a menu row does when chosen.
#[derive(Copy, Clone)]
enum MenuAction {
    /// Run conformance, scans, timing battery, then build capture output.
    RunFullSuite,
    Open(Mode),
    Submenu(MenuPage),
    Back,
    /// Step the audio readout: off, each rate, off again.
    CycleAudio,
    /// Run the conformance battery starting at `resume_index`.
    RunFromIndex,
}

const ROOT_MENU: [(&str, MenuAction); 10] = [
    // Row 0 is pinned: `make hwtest-capture` selects it by firing CROSS at a
    // fixed tick with the cursor still at its boot position. Move this row and
    // the capture opens whatever took its place, which produces an empty log
    // rather than a failure anyone would notice.
    ("RUN ALL TESTS + CAPTURE", MenuAction::RunFullSuite),
    (
        "CONTROLLER TEST (P1 + P2)",
        MenuAction::Open(Mode::ControllerTest),
    ),
    ("MEMORY CARD (SAFE)", MenuAction::Open(Mode::MemoryCard)),
    ("VIEW CAPTURE (QR PAGES)", MenuAction::Open(Mode::TimingScan)),
    ("RESULTS BY SECTION", MenuAction::Submenu(MenuPage::Results)),
    ("HARDWARE SCANS", MenuAction::Submenu(MenuPage::Scans)),
    ("TARGETED PROBES", MenuAction::Submenu(MenuPage::Probes)),
    // Root, not a submenu: this one is aimed at the capture rig rather than
    // at the console, so an operator setting up a recording finds it first.
    ("VIDEO LEVELS (TV/CAPTURE)", MenuAction::Open(Mode::VideoLevels)),
    // Listed, not just bound to SQUARE: an operator cannot discover a hidden
    // button, and this is the control they need while recording.
    ("AUDIO READOUT", MenuAction::CycleAudio),
    // A test that hangs the console used to cost a fresh burn to get past.
    // LEFT/RIGHT pick a start index, CROSS runs the battery from there, so
    // the operator power-cycles, resumes past the offender, and keeps
    // enumerating the rest in one session.
    ("RESUME FROM TEST", MenuAction::RunFromIndex),
];

const RESULTS_MENU: [(&str, MenuAction); 12] = [
    ("ALL CHECKS", MenuAction::Open(Mode::AllChecks)),
    ("CPU", MenuAction::Open(Mode::CpuChecks)),
    ("RAM", MenuAction::Open(Mode::MemoryChecks)),
    ("IRQ", MenuAction::Open(Mode::IrqChecks)),
    ("DMA", MenuAction::Open(Mode::DmaChecks)),
    ("TIMERS", MenuAction::Open(Mode::TimerChecks)),
    ("GPU", MenuAction::Open(Mode::GpuChecks)),
    ("GTE", MenuAction::Open(Mode::GteChecks)),
    ("SPU", MenuAction::Open(Mode::SpuChecks)),
    ("CDROM", MenuAction::Open(Mode::CdromChecks)),
    ("SIO", MenuAction::Open(Mode::SioChecks)),
    ("BACK", MenuAction::Back),
];

const SCANS_MENU: [(&str, MenuAction); 4] = [
    ("CPU SWEEP", MenuAction::Open(Mode::CpuScan)),
    ("GTE SWEEP", MenuAction::Open(Mode::GteScan)),
    ("SPU REGISTER MAP", MenuAction::Open(Mode::SpuScan)),
    ("BACK", MenuAction::Back),
];

const PROBES_MENU: [(&str, MenuAction); 10] = [
    ("SPU DIAGNOSTIC (SB2)", MenuAction::Open(Mode::SpuProbe)),
    ("UI SAMPLE END/LOOP (SB1)", MenuAction::Open(Mode::SampleProbe)),
    ("CD READ MECHANISM (CL2)", MenuAction::Open(Mode::CdChainProbe)),
    ("CONTROLLER SIO TIMING", MenuAction::Open(Mode::ControllerProbe)),
    ("HL REVERB STATE (PA5)", MenuAction::Open(Mode::ReverbProbe)),
    ("HL VOICE HANDOFF (PA4)", MenuAction::Open(Mode::HandoffProbe)),
    ("HL BANK TRANSITION (PA3)", MenuAction::Open(Mode::TransitionProbe)),
    ("HL VOICE BANK (PA2)", MenuAction::Open(Mode::VoiceProbe)),
    ("CD/SPU AUDIO (PA1)", MenuAction::Open(Mode::AudioProbe)),
    ("BACK", MenuAction::Back),
];

const fn menu_entries(page: MenuPage) -> &'static [(&'static str, MenuAction)] {
    match page {
        MenuPage::Root => &ROOT_MENU,
        MenuPage::Results => &RESULTS_MENU,
        MenuPage::Scans => &SCANS_MENU,
        MenuPage::Probes => &PROBES_MENU,
    }
}

const fn menu_title(page: MenuPage) -> &'static str {
    match page {
        MenuPage::Root => "MAIN MENU",
        MenuPage::Results => "RESULTS BY SECTION",
        MenuPage::Scans => "HARDWARE SCANS",
        MenuPage::Probes => "TARGETED PROBES",
    }
}


#[derive(Copy, Clone)]
struct TimingRecord {
    id: u8,
    work: u16,
    min: u16,
    med: u16,
    max: u16,
}

/// Marks a timing slot that was never filled. Must not be a real record id:
/// `0x00` is the empty-harness measurement, so zero cannot mean "unused".
const TIMING_RECORD_UNUSED: u8 = 0xFF;

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

impl TimingReport {
    const fn pending() -> Self {
        Self {
            summary: ScanReport::pending("press x to sample"),
            records: [TimingRecord::pending(); TIMING_RECORD_COUNT],
            memory_control: [0; MEMORY_CONTROL_REGISTER_COUNT],
            precision: [0; PRECISION_VALUE_COUNT],
        }
    }

    fn with_run(mut self, previous: Self) -> Self {
        self.summary.runs = previous.summary.runs.wrapping_add(1);
        self
    }
}

const fn timing_page_count() -> usize {
    photo::CAPTURE_PAGE_COUNT
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
        name: "OP full-seed MAC1",
        run: test_gte_op_full_seed_mac1,
    },
    TestSpec {
        group: "GTE",
        name: "OP full-seed MAC2",
        run: test_gte_op_full_seed_mac2,
    },
    TestSpec {
        group: "GTE",
        name: "OP full-seed MAC3",
        run: test_gte_op_full_seed_mac3,
    },
    TestSpec {
        group: "GTE",
        name: "AVSZ3 averages SZ -> OTZ",
        run: test_gte_avsz3,
    },
    TestSpec {
        group: "GTE",
        name: "RTPS SXY2 read-latency",
        run: test_gte_lat_sxy2,
    },
    TestSpec {
        group: "GTE",
        name: "RTPS SZ3 read-latency",
        run: test_gte_lat_sz3,
    },
    TestSpec {
        group: "GTE",
        name: "RTPS IR1 read-latency",
        run: test_gte_lat_ir1,
    },
    TestSpec {
        group: "GTE",
        name: "RTPS IR0 read-latency",
        run: test_gte_lat_ir0,
    },
    TestSpec {
        group: "GTE",
        name: "RT load settle +0",
        run: test_rt_settle_gap0,
    },
    TestSpec {
        group: "GTE",
        name: "RT load settle +2",
        run: test_rt_settle_gap2,
    },
    TestSpec {
        group: "GTE",
        name: "RT load settle +4",
        run: test_rt_settle_gap4,
    },
    TestSpec {
        group: "GTE",
        name: "RT load settle +8",
        run: test_rt_settle_gap8,
    },
    TestSpec {
        group: "GTE",
        name: "RT load settle +16",
        run: test_rt_settle_gap16,
    },
    TestSpec {
        group: "GTE",
        name: "RT load settle +32",
        run: test_rt_settle_gap32,
    },
    TestSpec {
        group: "GTE",
        name: "RT drop-during-RTPS +0",
        run: test_rt_drop_gap0,
    },
    TestSpec {
        group: "GTE",
        name: "RT drop-during-RTPS +4",
        run: test_rt_drop_gap4,
    },
    TestSpec {
        group: "GTE",
        name: "RT drop-during-RTPS +8",
        run: test_rt_drop_gap8,
    },
    TestSpec {
        group: "GTE",
        name: "RT drop-during-RTPS +16",
        run: test_rt_drop_gap16,
    },
    TestSpec {
        group: "GTE",
        name: "compose chain hot (engine shape)",
        run: test_compose_chain_hot,
    },
    TestSpec {
        group: "GTE",
        name: "compose chain V0-settled",
        run: test_compose_chain_v0_settled,
    },
    TestSpec {
        group: "GTE",
        name: "compose chain load-settled",
        run: test_compose_chain_load_settled,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 settle +1",
        run: test_mac0_settle_gap1,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 settle +2",
        run: test_mac0_settle_gap2,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 settle +3",
        run: test_mac0_settle_gap3,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 settle +4",
        run: test_mac0_settle_gap4,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP MAC0 settle +6",
        run: test_mac0_settle_gap6,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR settle +1",
        run: test_lzcr_settle_gap1,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR settle +2",
        run: test_lzcr_settle_gap2,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR settle +3",
        run: test_lzcr_settle_gap3,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR settle +4",
        run: test_lzcr_settle_gap4,
    },
    TestSpec {
        group: "GTE",
        name: "LZCR settle +6",
        run: test_lzcr_settle_gap6,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +1",
        run: test_mac0_big_settle_gap1,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +2",
        run: test_mac0_big_settle_gap2,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +4",
        run: test_mac0_big_settle_gap4,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +8",
        run: test_mac0_big_settle_gap8,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +12",
        run: test_mac0_big_gap12,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +16",
        run: test_mac0_big_gap16,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +24",
        run: test_mac0_big_gap24,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +32",
        run: test_mac0_big_gap32,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP big-value settle +48",
        run: test_mac0_big_gap48,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP magnitude quarter +4",
        run: test_mac0_mag_quarter,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP magnitude half +4",
        run: test_mac0_mag_half,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP magnitude double +4",
        run: test_mac0_mag_double,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP controlled scene-B +2",
        run: test_mac0_ctrl_b,
    },
    TestSpec {
        group: "GTE",
        name: "NCLIP controlled scene-C +2",
        run: test_mac0_ctrl_c,
    },
    TestSpec {
        group: "GTE",
        name: "SXY0 dump after A writes",
        run: test_sxy_dump_a_12,
    },
    TestSpec {
        group: "GTE",
        name: "SXY1 dump after A writes",
        run: test_sxy_dump_a_13,
    },
    TestSpec {
        group: "GTE",
        name: "SXY2 dump after A writes",
        run: test_sxy_dump_a_14,
    },
    TestSpec {
        group: "GTE",
        name: "SXY0 dump after C writes",
        run: test_sxy_dump_c_12,
    },
    TestSpec {
        group: "GTE",
        name: "SXY1 dump after C writes",
        run: test_sxy_dump_c_13,
    },
    TestSpec {
        group: "GTE",
        name: "SXY2 dump after C writes",
        run: test_sxy_dump_c_14,
    },
    TestSpec {
        group: "GTE",
        name: "SXY0 after probe NCLIP (A)",
        run: test_sxy_post_a,
    },
    TestSpec {
        group: "GTE",
        name: "SXY0 after probe NCLIP (C)",
        run: test_sxy_post_c,
    },
    TestSpec {
        group: "GPU",
        name: "VRAM fill + read-back",
        run: test_gpu_vram_roundtrip,
    },
    TestSpec {
        group: "GPU",
        name: "draw flat triangle",
        run: test_gpu_draw_flat_tri,
    },
    TestSpec {
        group: "GPU",
        name: "draw gouraud triangle",
        run: test_gpu_draw_gouraud_tri,
    },
    TestSpec {
        group: "GPU",
        name: "draw flat quad",
        run: test_gpu_draw_flat_quad,
    },
    TestSpec {
        group: "GPU",
        name: "draw gouraud quad",
        run: test_gpu_draw_gouraud_quad,
    },
    TestSpec {
        group: "GPU",
        name: "tri vertex past right edge",
        run: test_gpu_tri_past_right_edge,
    },
    TestSpec {
        group: "GPU",
        name: "tri negative coordinate",
        run: test_gpu_tri_negative_coord,
    },
    TestSpec {
        group: "GPU",
        name: "tri coord exceeds 11-bit (wrap)",
        run: test_gpu_tri_coord_wrap,
    },
    TestSpec {
        group: "GPU",
        name: "textured gouraud tri (player prim)",
        run: test_gpu_textured_gouraud_tri,
    },
    TestSpec {
        group: "GPU",
        name: "OT + DMA linked-list draw",
        run: test_gpu_ot_dma_draw,
    },
    TestSpec {
        group: "GPU",
        name: "tri X-span > 1023 (poly-too-large)",
        run: test_gpu_tri_large_span,
    },
    TestSpec {
        group: "GPU",
        name: "tri vertex past bottom edge",
        run: test_gpu_tri_y_past_edge,
    },
    TestSpec {
        group: "GPU",
        name: "textured gouraud large span",
        run: test_gpu_texgouraud_large_span,
    },
    TestSpec {
        group: "GPU",
        name: "textured gouraud via OT + DMA (player path)",
        run: test_gpu_texgouraud_ot_dma,
    },
    TestSpec {
        group: "GPU",
        name: "8bpp CLUT textured tri (model format)",
        run: test_gpu_8bpp_clut_tri,
    },
    TestSpec {
        group: "GPU",
        name: "deep OT + DMA (8 prims)",
        run: test_gpu_big_ot,
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
    TestSpec {
        group: "SPU",
        name: "SPU RAM DMA upload round-trip",
        run: test_spu_ram_dma_roundtrip,
    },
    TestSpec {
        group: "SPU",
        name: "SPU RAM manual-FIFO upload round-trip",
        run: test_spu_ram_manual_fifo_roundtrip,
    },
    TestSpec {
        group: "GPU",
        name: "ordered-dither checkerboard (flat mid-tone)",
        run: test_gpu_dither_checkerboard,
    },
    TestSpec {
        group: "GPU",
        name: "mask bit on CPU->VRAM copy",
        run: test_gpu_cpu_vram_upload_mask,
    },
    TestSpec {
        group: "SIO",
        name: "port 1 handshake strict (no-wait)",
        run: test_pad_handshake_strict,
    },
    TestSpec {
        group: "SIO",
        name: "port 1 diag setup+inter timing",
        run: test_pad_diag_timing,
    },
    TestSpec {
        group: "SIO",
        name: "DualShock analog enable handshake",
        run: test_pad_analog_handshake,
    },
];

struct HardwareTests {
    font: Option<FontAtlas>,
    mode: Mode,
    results: [TestResult; TEST_COUNT],
    cpu_scan: ScanReport,
    gte_scan: ScanReport,
    spu_scan: ScanReport,
    timing_scan: TimingReport,
    timing_capture: PhotoCapture,
    reverb_probe: ReverbProbe,
    handoff_probe: HandoffProbe,
    cd_chain_probe: CdChainProbe,
    transition_probe: TransitionProbe,
    voice_probe: VoiceProbe,
    sample_probe: SampleProbe,
    spu_probe: SpuProbe,
    audio_probe: AudioProbe,
    controller_test: ControllerTest,
    memory_card: MemoryCardDiagnostic,
    pass_count: u8,
    fail_count: u8,
    warn_count: u8,
    info_count: u8,
    page: usize,
    rerun_count: u8,
    /// Latest port-1 poll for each [`PROBE_VARIANTS`] timing, refreshed every
    /// frame while in the controller probe. Used to find which setup/inter-byte
    /// timing wakes a strict original pad.
    probe_variants: [psx_pad::RawPoll; PROBE_VARIANT_COUNT],
    /// Readout tone rate: 0 is off, otherwise `RATE_DIVISORS[audio_rate - 1]`.
    /// Starts at the fastest rate so a recording always carries the payload.
    audio_rate: usize,
    /// The capture payload must exist before the audio readout can start.
    audio_prepared: bool,
    /// Selected row on the current menu page.
    menu_cursor: usize,
    /// First conformance test the next RESUME FROM TEST run executes.
    resume_index: u16,
    /// Which menu page is showing.
    menu_page: MenuPage,
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
    fn new(boot_reverb: ReverbSnapshot) -> Self {
        Self {
            font: None,
            // Boot is deliberately side-effect free. The operator chooses
            // whether to run the long capture battery, a focused probe, or the
            // memory-card diagnostic.
            mode: Mode::Menu,
            results: [TestResult::pending(); TEST_COUNT],
            cpu_scan: ScanReport::pending("press x to sweep"),
            gte_scan: ScanReport::pending("press x to sweep"),
            spu_scan: ScanReport::pending("press x to map"),
            timing_scan: TimingReport::pending(),
            timing_capture: PhotoCapture::new(),
            reverb_probe: ReverbProbe::new(boot_reverb),
            handoff_probe: HandoffProbe::new(),
            cd_chain_probe: CdChainProbe::new(),
            transition_probe: TransitionProbe::new(),
            voice_probe: VoiceProbe::new(),
            sample_probe: SampleProbe::new(),
            spu_probe: SpuProbe::new(),
            audio_probe: AudioProbe::new(),
            controller_test: ControllerTest::new(),
            memory_card: MemoryCardDiagnostic::new(),
            pass_count: 0,
            fail_count: 0,
            warn_count: 0,
            info_count: 0,
            page: 0,
            rerun_count: 0,
            probe_variants: [psx_pad::RawPoll::NONE; PROBE_VARIANT_COUNT],
            audio_rate: 0,
            audio_prepared: false,
            menu_cursor: 0,
            resume_index: 0,
            menu_page: MenuPage::Root,
        }
    }

    fn run_all(&mut self) {
        self.run_all_from(0);
    }

    fn run_all_from(&mut self, start: usize) {
        for (index, spec) in TESTS.iter().enumerate().skip(start) {
            // Name the test BEFORE it runs, on screen and on the TTY: each
            // body is fully blocking, so when one hangs on silicon (the
            // first full-suite run after the DMA-free boot froze at ~10%
            // with an anonymous bar) the frozen frame identifies it.
            self.draw_running_label(index, spec.group, spec.name);
            self.results[index] = (spec.run)();
            // Green while conformance runs, blue while the timing battery does;
            // the operator can tell which phase is slow.
            draw_init_progress(index + 1, TEST_COUNT, (96, 240, 128));
        }
        self.recount();
        self.rerun_count = self.rerun_count.wrapping_add(1);
        print_conformance_report(self);
        print_all_section_reports(&self.results);
        print_case_reports(Mode::AllChecks, &self.results);
    }

    fn run_startup_scans(&mut self) {
        unsafe { core::ptr::write_volatile(&raw mut SCAN_ABORT, false) };
        self.draw_running_label(TEST_COUNT, "scan", "cpu sweep");
        self.cpu_scan = run_cpu_scan();
        print_scan_report(Mode::CpuScan, self.cpu_scan);
        self.draw_running_label(TEST_COUNT, "scan", "gte matrix");
        self.gte_scan = run_gte_scan();
        print_scan_report(Mode::GteScan, self.gte_scan);
        self.draw_running_label(TEST_COUNT, "scan", "spu map");
        self.spu_scan = run_spu_scan();
        print_scan_report(Mode::SpuScan, self.spu_scan);
        self.draw_running_label(TEST_COUNT, "scan", "timing map  START SKIPS");
        self.timing_scan = run_timing_scan();
        self.encode_capture(0);
        print_scan_report(Mode::TimingScan, self.timing_scan.summary);
    }

    /// Paint "RUN <group>: <name>" just above the progress bar in BOTH
    /// framebuffers (the battery blocks the frame loop, so only absolute
    /// VRAM writes stay visible), and mirror it to the TTY. The strip is
    /// cleared with the same absolute-coordinate fill the bar uses.
    fn draw_running_label(&mut self, index: usize, group: &str, name: &str) {
        tty::print("hardware-tests: run ");
        tty::print(dec3(index as u16).as_str());
        tty::print(" ");
        tty::print(group);
        tty::print(": ");
        tty::println(name);
        let Some(font) = self.font.as_ref() else {
            return;
        };
        gpu_io::wait_cmd_ready();
        for buffer_y in [184u32, 424] {
            gpu_io::write_gp0(0x0200_0000); // fill, black
            gpu_io::write_gp0((buffer_y << 16) | 16);
            gpu_io::write_gp0((12u32 << 16) | 288);
        }
        gpu::set_draw_area(0, 0, 1023, 511);
        gpu::set_draw_offset(0, 0);
        let mut label = [0u8; 44];
        let mut n = 0usize;
        let index_text = dec3(index as u16);
        for &part in &[
            b"RUN " as &[u8],
            index_text.as_str().as_bytes(),
            b" ",
            group.as_bytes(),
            b": ",
            name.as_bytes(),
        ] {
            let take = part.len().min(label.len() - n);
            label[n..n + take].copy_from_slice(&part[..take]);
            n += take;
        }
        // SAFETY: assembled from ASCII string literals and spec names.
        let text = unsafe { core::str::from_utf8_unchecked(&label[..n]) };
        font.draw_text(24, 186, text, (255, 216, 96));
        font.draw_text(24, 426, text, (255, 216, 96));
        gpu::draw_sync();
    }

    fn prepare_audio_readout(&mut self) {
        audio_link::stop();
        self.audio_rate = 0;
        let bits = audio_link::prepare(self.timing_capture.binary());
        self.audio_prepared = true;
        tty::print("hardware-tests: audio-link transmitting bits=");
        tty_print_dec_u16(bits as u16);
        tty::println(" (SQUARE changes rate / off)");
    }

    fn run_full_suite(&mut self) {
        if self.audio_prepared {
            audio_link::stop();
            self.audio_rate = 0;
        }
        self.run_all();
        self.run_startup_scans();
        self.prepare_audio_readout();
    }

    fn encode_capture(&mut self, page: usize) {
        self.timing_capture.encode(
            &self.timing_scan,
            &self.results,
            self.rerun_count,
            [self.cpu_scan, self.gte_scan, self.spu_scan],
            page,
        );
        // Mirror a complete, internally consistent page set on every encode.
        // Headless validation must not depend on controller pulse timing, and
        // keeping the latest occurrence of each page must never mix captures.
        for other in 0..photo::CAPTURE_PAGE_COUNT {
            if other != page {
                self.timing_capture.print_page(other);
            }
        }
    }

    /// Tier-2 probes own hardware state (SPU banks, voice keys, reverb
    /// config), so they arm on entry rather than at boot. Keeping them out of
    /// `init` is what lets the tier-1 capture describe a console they have not
    /// touched. A probe entered this way still starts from its own clean
    /// sequence, but note that only a probe reached from a fresh boot sees an
    /// untouched BIOS handoff; PA5's variants remain one-per-reboot.
    fn enter_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.page = 0;
        match mode {
            Mode::MemoryCard => self.memory_card = MemoryCardDiagnostic::new(),
            Mode::ReverbProbe => self.reverb_probe.start(),
            Mode::HandoffProbe => self.handoff_probe.start(),
            Mode::CdChainProbe => self.cd_chain_probe.start(),
            Mode::TransitionProbe => self.transition_probe.start(),
            Mode::VoiceProbe => self.voice_probe.start(),
            Mode::AudioProbe => self.audio_probe.start(),
            Mode::SampleProbe => self.sample_probe.start(),
            Mode::SpuProbe => self.spu_probe.start(),
            Mode::ControllerTest => self.controller_test.start(),
            _ => {}
        }
    }

    fn open_menu_page(&mut self, page: MenuPage) {
        self.mode = Mode::Menu;
        self.menu_page = page;
        self.menu_cursor = 0;
        self.page = 0;
    }

    /// Step the audio readout: each rate, then off, then back round. It starts
    /// on, so a recording carries the payload without anyone remembering to
    /// enable it; the cycle is for dropping to a slower, more robust rate when
    /// a capture chain cannot decode the fastest one.
    fn cycle_audio_readout(&mut self) {
        if !self.audio_prepared {
            tty::println("hardware-tests: audio-link unavailable until full capture runs");
            return;
        }
        self.audio_rate = (self.audio_rate + 1) % (audio_link::RATE_DIVISORS.len() + 1);
        if self.audio_rate == 0 {
            audio_link::stop();
            tty::println("hardware-tests: audio-link off");
        } else {
            audio_link::set_rate(self.audio_rate - 1);
            tty::print("hardware-tests: audio-link rate index ");
            tty_print_dec_u8((self.audio_rate - 1) as u8);
            tty::println("");
        }
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
                self.encode_capture(self.page);
                print_scan_report(self.mode, self.timing_scan.summary);
            }
            Mode::ReverbProbe => self.reverb_probe.restart(),
            Mode::HandoffProbe => self.handoff_probe.restart(),
            Mode::CdChainProbe => self.cd_chain_probe.restart(),
            Mode::TransitionProbe => self.transition_probe.restart(),
            Mode::VoiceProbe => self.voice_probe.restart(),
            Mode::AudioProbe => self.audio_probe.restart(),
            Mode::SampleProbe => self.sample_probe.restart(),
            Mode::SpuProbe => self.spu_probe.restart(),
            Mode::VideoLevels => self.page = (self.page + 1) % VIDEO_FIELDS.len(),
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
        tty::println("hardware-tests: main menu ready");
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.results[PAD_POLL_TEST_INDEX] = pad_poll_result(ctx.pad);
        self.recount();

        if matches!(self.mode, Mode::ControllerTest) {
            if self.controller_test.update(ctx.pad) {
                self.open_menu_page(MenuPage::Root);
            }
            // This screen must receive START and SELECT as ordinary testable
            // buttons. Its deliberate hold gesture owns navigation, so do not
            // let the global START shortcut consume either button first.
            return;
        } else if matches!(self.mode, Mode::MemoryCard) {
            // The engine's controller poll has fully completed before this
            // card transaction starts. Pad and card share SIO0, but are never
            // accessed concurrently.
            self.memory_card.scan_step();
            if ctx.just_pressed(button::START) || ctx.just_pressed(button::TRIANGLE) {
                self.open_menu_page(MenuPage::Root);
                return;
            }
            if ctx.just_pressed(button::LEFT) {
                self.memory_card.page_left();
            }
            if ctx.just_pressed(button::RIGHT) {
                self.memory_card.page_right();
            }
            self.memory_card.guarded_write(
                ctx.is_held(button::L1),
                ctx.is_held(button::R1),
                ctx.just_pressed(button::CROSS),
            );
            return;
        } else if matches!(self.mode, Mode::ReverbProbe) {
            let (realign, consume_input) = self.reverb_probe.update(ctx);
            if realign {
                ctx.request_timing_realign();
            }
            if consume_input {
                return;
            }
        } else if matches!(self.mode, Mode::HandoffProbe) {
            let (realign, consume_input) = self.handoff_probe.update(ctx);
            if realign {
                ctx.request_timing_realign();
            }
            if consume_input {
                return;
            }
        } else if matches!(self.mode, Mode::CdChainProbe) {
            let (realign, consume_input) = self.cd_chain_probe.update(ctx);
            if realign {
                ctx.request_timing_realign();
            }
            if consume_input {
                return;
            }
        } else if matches!(self.mode, Mode::TransitionProbe) {
            if self.transition_probe.update(ctx.sim_tick.as_u32()) {
                ctx.request_timing_realign();
            }
        } else if matches!(self.mode, Mode::VoiceProbe) {
            self.voice_probe.update(ctx.sim_tick.as_u32());
        } else if matches!(self.mode, Mode::AudioProbe) {
            self.audio_probe.update(ctx.sim_tick.as_u32());
        } else if matches!(self.mode, Mode::SampleProbe) {
            self.sample_probe.update(ctx.sim_tick.as_u32());
        } else if matches!(self.mode, Mode::SpuProbe) {
            self.spu_probe.update(ctx.sim_tick.as_u32());
        }

        if matches!(self.mode, Mode::ControllerProbe) {
            // Sweep every timing variant once per frame. Each poll selects and
            // deselects independently, so the variants stay isolated; the strict
            // original pad either wakes up for some setup/inter-byte timing or it
            // does not, and we see exactly which.
            let mut i = 0;
            while i < PROBE_VARIANT_COUNT {
                let (_, setup, interbyte) = PROBE_VARIANTS[i];
                self.probe_variants[i] = psx_pad::poll_port1_diag(setup, interbyte);
                i += 1;
            }
        }

        if matches!(self.mode, Mode::Menu) {
            let entries = menu_entries(self.menu_page);
            if ctx.just_pressed(button::UP) {
                self.menu_cursor = (self.menu_cursor + entries.len() - 1) % entries.len();
            }
            if ctx.just_pressed(button::DOWN) {
                self.menu_cursor = (self.menu_cursor + 1) % entries.len();
            }
            if matches!(entries[self.menu_cursor].1, MenuAction::RunFromIndex) {
                // L1/R1 step by ten on their own. They used to be modifiers
                // held with LEFT/RIGHT, which is not what the row says, so
                // pressing them did nothing and the label was a lie.
                let last = TEST_COUNT as u16 - 1;
                if ctx.just_pressed(button::LEFT) {
                    self.resume_index = self.resume_index.saturating_sub(1);
                }
                if ctx.just_pressed(button::RIGHT) {
                    self.resume_index = (self.resume_index + 1).min(last);
                }
                if ctx.just_pressed(button::L1) {
                    self.resume_index = self.resume_index.saturating_sub(10);
                }
                if ctx.just_pressed(button::R1) {
                    self.resume_index = (self.resume_index + 10).min(last);
                }
            }
            // START and TRIANGLE both back out one level, and leave the menu
            // entirely from the root, so one button walks the whole way out.
            if ctx.just_pressed(button::START) || ctx.just_pressed(button::TRIANGLE) {
                if matches!(self.menu_page, MenuPage::Root) {
                    self.enter_mode(Mode::TimingScan);
                } else {
                    self.open_menu_page(MenuPage::Root);
                }
                return;
            }
            if ctx.just_pressed(button::CROSS) {
                match entries[self.menu_cursor].1 {
                    MenuAction::RunFullSuite => {
                        self.run_full_suite();
                        self.enter_mode(Mode::TimingScan);
                    }
                    MenuAction::Open(mode) => self.enter_mode(mode),
                    MenuAction::Submenu(page) => self.open_menu_page(page),
                    MenuAction::Back => self.open_menu_page(MenuPage::Root),
                    MenuAction::CycleAudio => self.cycle_audio_readout(),
                    MenuAction::RunFromIndex => {
                        self.run_all_from(self.resume_index as usize);
                        self.run_startup_scans();
                        self.prepare_audio_readout();
                        self.enter_mode(Mode::TimingScan);
                    }
                }
            }
            return;
        }

        // START opens the main menu from any screen. It is the one navigation
        // button an operator needs to know, so it is printed on every screen.
        if ctx.just_pressed(button::START) || ctx.just_pressed(button::TRIANGLE) {
            self.open_menu_page(MenuPage::Root);
            return;
        }

        if (self.mode.is_check_section() || matches!(self.mode, Mode::TimingScan))
            && ctx.just_pressed(button::LEFT)
        {
            let pages = if matches!(self.mode, Mode::TimingScan) {
                timing_page_count()
            } else {
                page_count_for_mode(self.mode)
            };
            self.page = if self.page == 0 {
                pages - 1
            } else {
                self.page - 1
            };
            if matches!(self.mode, Mode::TimingScan) {
                self.timing_capture.render_page(self.page);
            }
        }
        if (self.mode.is_check_section() || matches!(self.mode, Mode::TimingScan))
            && ctx.just_pressed(button::RIGHT)
        {
            let pages = if matches!(self.mode, Mode::TimingScan) {
                timing_page_count()
            } else {
                page_count_for_mode(self.mode)
            };
            self.page = (self.page + 1) % pages;
            if matches!(self.mode, Mode::TimingScan) {
                self.timing_capture.render_page(self.page);
            }
        }
        if ctx.just_pressed(button::CROSS) {
            self.run_active();
        }
        // SQUARE is the shortcut for the same thing the menu lists, so the
        // rate can be changed without leaving the capture page mid-recording.
        if ctx.just_pressed(button::SQUARE) {
            self.cycle_audio_readout();
        }
    }

    fn render(&mut self, ctx: &mut Ctx) {
        // The video-levels page owns the whole framebuffer: the surround bars
        // are lit pixels, and a level chart read next to them is worthless.
        if !matches!(
            self.mode,
            Mode::VideoLevels | Mode::MemoryCard | Mode::ControllerTest
        ) {
            draw_test_pattern(ctx.sim_tick.as_u32());
        }
        // GPU line liveness cross only on the GPU section -- keeps the controller
        // probe and other focused screens clean.
        if matches!(self.mode, Mode::GpuChecks) {
            draw_gpu_line_probe();
        }

        let Some(font) = self.font.as_ref() else {
            return;
        };

        if matches!(self.mode, Mode::Menu) {
            draw_menu(font, self);
            return;
        }

        if !matches!(
            self.mode,
            Mode::TimingScan
                | Mode::ReverbProbe
                | Mode::HandoffProbe
                | Mode::TransitionProbe
                | Mode::VoiceProbe
                | Mode::AudioProbe
                | Mode::ControllerTest
                | Mode::VideoLevels
                | Mode::MemoryCard
        ) {
            draw_mode_menu(font, self);
        }

        if self.mode.is_check_section() {
            draw_summary(font, self);
            draw_rows(font, self, self.mode);
            draw_problem_detail(font, self, self.mode);
        } else {
            match self.mode {
                Mode::ReverbProbe => self.reverb_probe.draw(font),
                Mode::HandoffProbe => self.handoff_probe.draw(font),
                Mode::CdChainProbe => self.cd_chain_probe.draw(font),
                Mode::TransitionProbe => self.transition_probe.draw(font),
                Mode::VoiceProbe => self.voice_probe.draw(font),
                Mode::AudioProbe => self.audio_probe.draw(font),
                Mode::SampleProbe => self.sample_probe.draw(font),
                Mode::SpuProbe => self.spu_probe.draw(font),
                Mode::ControllerTest => self.controller_test.draw(font),
                Mode::ControllerProbe => draw_controller_probe(font, self),
                Mode::CpuScan => draw_scan_report(font, self.mode, self.cpu_scan),
                Mode::GteScan => draw_scan_report(font, self.mode, self.gte_scan),
                Mode::SpuScan => draw_scan_report(font, self.mode, self.spu_scan),
                Mode::TimingScan => photo::draw_capture_page(font, &self.timing_capture, self.page),
                Mode::VideoLevels => draw_video_levels(font, self.page),
                Mode::MemoryCard => self.memory_card.draw(font),
                _ => {}
            }
        }
    }
}

#[no_mangle]
fn main() -> ! {
    // Capture the BIOS-owned reverb state before any SDK or engine path can
    // initialise the SPU. PA5 transports the raw snapshot in its QR payload.
    let boot_reverb = ReverbSnapshot::capture();
    let mut suite = HardwareTests::new(boot_reverb);
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
    font.draw_text(320 - 8 - SUITE_VERSION.len() as i16 * 8, 8, SUITE_VERSION, (112, 136, 170));
    font.draw_text(8, 18, "SECTION", (140, 160, 190));
    font.draw_text(72, 18, suite.mode.label(), (255, 232, 128));
    font.draw_text(216, 18, "START MENU", (140, 160, 190));
}

/// The main menu. Every page fits on screen, so there is no scrolling.
fn draw_menu(font: &FontAtlas, suite: &HardwareTests) {
    font.draw_text(8, 6, "PS1 HARDWARE TESTS", (232, 236, 244));
    font.draw_text(320 - 8 - SUITE_VERSION.len() as i16 * 8, 6, SUITE_VERSION, (112, 136, 170));
    font.draw_text(8, 20, menu_title(suite.menu_page), (255, 232, 128));

    let entries = menu_entries(suite.menu_page);
    let mut y = 40i16;
    let mut row = 0usize;
    while row < entries.len() {
        let selected = row == suite.menu_cursor;
        if selected {
            font.draw_text(10, y, ">", (255, 232, 128));
        }
        let colour = if selected {
            (255, 232, 128)
        } else {
            (176, 190, 210)
        };
        font.draw_text(22, y, entries[row].0, colour);
        // The audio row shows its own state inline, so the operator never has
        // to guess whether the tone is currently playing.
        if matches!(entries[row].1, MenuAction::RunFromIndex) {
            font.draw_text(180, y, dec3(suite.resume_index).as_str(), (255, 232, 128));
            font.draw_text(212, y, "<> 1  L1R1 10", (140, 160, 190));
        }
        if matches!(entries[row].1, MenuAction::CycleAudio) {
            if !suite.audio_prepared {
                font.draw_text(150, y, "RUN CAPTURE FIRST", (255, 216, 96));
            } else if suite.audio_rate == 0 {
                font.draw_text(150, y, "OFF", (176, 190, 210));
            } else {
                font.draw_text(150, y, "ON  RATE", (96, 240, 128));
                font.draw_text(214, y, hex2((suite.audio_rate - 1) as u8).as_str(), (96, 240, 128));
            }
        }
        y += 12;
        row += 1;
    }

    font.draw_text(8, 214, "UP/DOWN SELECT   CROSS RUN", (140, 160, 190));
    font.draw_text(8, 226, "START BACK / CLOSE MENU", (140, 160, 190));
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
            // Bare 8 nibbles (no `0x`) so the full value fits on a 320px
            // screen -- the `0x`-prefixed form clipped the low nibble.
            font.draw_text(248, y, hex8(result.observed).digits(), color);
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

/// The boot screen: poll port 1 two ways and show the raw handshake so a
/// controller can be diagnosed with no working controller needed to navigate.
/// Column A uses the legacy no-wait timing (which desyncs slow original pads);
/// column B uses the `/ACK`-paced timing the SDK now ships. On an SCPH-1200 the
/// two columns disagree (A garbage, B clean); on a fast clone or in the emulator
/// they agree.
fn draw_controller_probe(font: &FontAtlas, suite: &HardwareTests) {
    font.draw_text(8, 30, "PORT 1 SETUP-DELAY SWEEP", (232, 236, 244));
    font.draw_text(
        8,
        42,
        "MIN SELECT SETUP THE PAD NEEDS (FIX=2K)",
        (150, 170, 200),
    );

    let hdr = (140, 160, 190);
    font.draw_text(8, 58, "VARIANT", hdr);
    font.draw_text(112, 58, "ID", hdr);
    font.draw_text(160, 58, "MOD", hdr);
    font.draw_text(200, 58, "BTN", hdr);
    font.draw_text(248, 58, "RES", hdr);

    let base_ok = probe_column_ok(suite.probe_variants[0]);
    let mut woke: Option<&'static str> = None;
    let mut any_present = false;

    let mut i = 0usize;
    while i < PROBE_VARIANT_COUNT {
        let (label, _, _) = PROBE_VARIANTS[i];
        let raw = suite.probe_variants[i];
        let ok = probe_column_ok(raw);
        let connected = raw.mode.is_connected();
        any_present |= connected;
        let y = 72 + i as i16 * 16;
        let color = if ok {
            Status::Pass.color()
        } else if connected {
            Status::Fail.color()
        } else {
            (130, 140, 160)
        };

        font.draw_text(8, y, label, (220, 224, 230));
        font.draw_text(
            112,
            y,
            hex2(raw.id_high).as_str(),
            if raw.id_high == 0x5A {
                (224, 228, 234)
            } else {
                color
            },
        );
        font.draw_text(128, y, hex2(raw.id_low).as_str(), color);
        font.draw_text(160, y, mode_short(raw.mode), color);
        font.draw_text(200, y, hex2(raw.buttons_low).as_str(), (200, 206, 214));
        font.draw_text(216, y, hex2(raw.buttons_high).as_str(), (200, 206, 214));
        let res = if !connected {
            "NOPAD"
        } else if ok {
            "CLEAN"
        } else {
            "DSYNC"
        };
        font.draw_text(248, y, res, color);

        // First clean variant that the base (old no-wait) timing did NOT get is
        // the timing the strict pad needs.
        if ok && !base_ok && woke.is_none() {
            woke = Some(label);
        }
        i += 1;
    }

    font.draw_text(8, 168, "VERDICT", (140, 160, 190));
    if let Some(label) = woke {
        font.draw_text(8, 180, "OFFICIAL PAD NEEDS:", Status::Pass.color());
        font.draw_text(160, 180, label, Status::Pass.color());
    } else if base_ok {
        font.draw_text(
            8,
            180,
            "PAD WORKS AT BASE TIMING (CLONE)",
            Status::Pass.color(),
        );
    } else if any_present {
        font.draw_text(8, 180, "ANSWERS BUT NO VARIANT CLEAN", Status::Warn.color());
    } else {
        font.draw_text(8, 180, "NO PAD - ALL VARIANTS DEAD", Status::Warn.color());
    }
    font.draw_text(8, 200, "BASE = OLD NO-WAIT. RERUNS LIVE.", (112, 136, 170));
    font.draw_text(
        8,
        212,
        "DOWN = TESTS   START = TIMING CAPTURE",
        (150, 170, 200),
    );
}

/// A poll is clean when something answered with a valid 0x5A magic and a
/// classified mode (not the `Unknown` desync bucket).
fn probe_column_ok(raw: psx_pad::RawPoll) -> bool {
    raw.mode.is_connected() && !matches!(raw.mode, psx_pad::PadMode::Unknown) && raw.id_high == 0x5A
}

const fn mode_short(mode: psx_pad::PadMode) -> &'static str {
    match mode {
        psx_pad::PadMode::Digital => "DIG",
        psx_pad::PadMode::Analog => "ANA",
        psx_pad::PadMode::Config => "CFG",
        psx_pad::PadMode::Unknown => "UNK",
        psx_pad::PadMode::Disconnected => "---",
    }
}

struct Hex2 {
    bytes: [u8; 2],
}

impl Hex2 {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes) }
    }
}

fn hex2(value: u8) -> Hex2 {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    Hex2 {
        bytes: [HEX[(value >> 4) as usize], HEX[(value & 0xF) as usize]],
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
}

/// GPU monochrome-line liveness check: a red and a blue diagonal crossing in a
/// small box. Only shown on the GPU section now (it is cosmetic -- the GPU draw
/// tests assert the real line behaviour), so the focused screens stay clean.
fn draw_gpu_line_probe() {
    gpu::draw_line_mono(272, 50, 312, 90, 255, 80, 80);
    gpu::draw_line_mono(312, 50, 272, 90, 80, 180, 255);
}

/// Pages of the video-levels screen. Page 0 is the chart; the rest are flat
/// fields at a single code, which is what a capture card's histogram or a TV
/// service menu actually wants to be pointed at.
const VIDEO_FIELDS: [(&str, Option<u8>); 4] = [
    ("CHART", None),
    ("FLAT BLACK  CODE 00", Some(0)),
    ("FLAT MID    CODE 16", Some(16)),
    ("FLAT WHITE  CODE 31", Some(31)),
];

/// 5-bit framebuffer code to the 8-bit value a GP0 colour word needs to land
/// on it exactly. The framebuffer is 15bpp and these primitives are flat (no
/// dither), so 32 codes are all the DAC can emit; labelling anything in 8-bit
/// units here would invent precision the console does not have.
const fn code_rgb(code: u8) -> u8 {
    code << 3
}

/// Separator colour for the chart. Dim enough not to shift how the near-black
/// patches read, blue so it is never taken for a grey being judged.
const GRID_BLUE: u8 = 96;

/// Grey ramp and flat fields, for telling display gamma apart from clipped
/// levels. Nothing here is measured on the console: the console is the known
/// source and the operator's TV or capture chain is what is under test.
///
/// Read it as: bottom codes indistinguishable from each other = black crush
/// somewhere in the chain (levels, a setup pedestal, a limited/full range
/// mismatch). All codes distinct but the ramp sitting dark = display gamma,
/// which is what a CRT is supposed to do and a monitor is not.
fn draw_video_levels(font: &FontAtlas, page: usize) {
    let (title, flat) = VIDEO_FIELDS[page % VIDEO_FIELDS.len()];
    gpu::draw_rect_flat(0, 0, 320, 240, 0, 0, 0);

    if let Some(code) = flat {
        let v = code_rgb(code);
        // Field stops short of the last text line so the label can be cropped
        // out of a measurement without cropping the field itself.
        gpu::draw_rect_flat(0, 0, 320, 222, v, v, v);
        // The caption sits on the black surround, not on the field, so it
        // stays legible at every field value and crops off cleanly.
        let ink = (120, 120, 120);
        font.draw_text(8, 228, title, ink);
        font.draw_text(176, 228, "X NEXT  START MENU", ink);
        return;
    }

    let label = (140, 160, 190);
    let head = (232, 236, 244);
    font.draw_text(8, 12, "VIDEO LEVELS", head);
    font.draw_text(232, 12, SUITE_VERSION, label);

    // Full 32-code ramp, one 8px bar per code, so nothing is interpolated away.
    // The blue frame is not decoration: code 0 is the page background, so
    // without it a display that crushes the bottom of the ramp looks exactly
    // like one where the ramp did not draw. Blue rather than grey so it can
    // never be mistaken for one of the patches being judged.
    gpu::draw_rect_flat(30, 26, 260, 44, 0, 0, GRID_BLUE);
    let mut code = 0u8;
    while code < 32 {
        let v = code_rgb(code);
        gpu::draw_rect_flat(32 + (code as i16) * 8, 28, 8, 40, v, v, v);
        code += 1;
    }
    font.draw_text(32, 70, "000", label);
    font.draw_text(96, 70, "008", label);
    font.draw_text(160, 70, "016", label);
    font.draw_text(224, 70, "024", label);
    font.draw_text(280, 70, "031", label);

    draw_level_row(font, 86, "NEAR BLACK", 0, label);
    draw_level_row(font, 146, "NEAR WHITE", 24, label);

    font.draw_text(8, 206, "PATCHES ALL DISTINCT? IF NOT, LEVELS", label);
    font.draw_text(8, 218, "X NEXT FIELD   START MENU", label);
}

/// Eight patches from `first` upward, labelled with their 5-bit code.
fn draw_level_row(font: &FontAtlas, y: i16, title: &'static str, first: u8, label: (u8, u8, u8)) {
    font.draw_text(8, y, title, label);
    // Same reason as the ramp frame: the 2px gutters were already there, this
    // only lights them, so every patch keeps a visible edge even when the
    // display cannot separate its value from its neighbour's.
    gpu::draw_rect_flat(30, y + 8, 258, 34, 0, 0, GRID_BLUE);
    let mut step = 0u8;
    while step < 8 {
        let code = first + step;
        let v = code_rgb(code);
        let x = 32 + (step as i16) * 32;
        gpu::draw_rect_flat(x, y + 10, 30, 30, v, v, v);
        font.draw_text(x + 3, y + 44, dec3(code as u16).as_str(), label);
        step += 1;
    }
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

fn run_timing_scan() -> TimingReport {
    // Stay below the 16-bit root-counter wrap even when silicon RAM/MMIO is
    // slower than the emulator. Geometric points let the host fit a slope and
    // intercept instead of treating harness overhead as per-iteration cost.
    const SPINS: [u32; 4] = [64, 256, 1024, 4096];
    let mut records = [TimingRecord::pending(); TIMING_RECORD_COUNT];
    let mut next = 0usize;

    push_timing_record(&mut records, &mut next, sample_timing(0x00, 0, timed_empty));
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x01, 128, timed_nops),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x02, 128, timed_dependent_alu),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x03, 64, timed_load_hazards),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x04, 64, timed_taken_branches),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x05, 16, || timed_multu_mflo(0x0000_07FF)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x06, 16, || timed_multu_mflo(0x000F_FFFF)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x07, 16, || timed_multu_mflo(0x1357_2468)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x08, 8, timed_divu_mflo),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x09, 64, || timed_load_hazards_at(0x1F80_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x0A, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_load_hazards_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x0B, 64, || {
            timed_stores_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x0C, 64, || timed_stores_at(0x1F80_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x0D, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_stores_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x0E, 64, || timed_load_hazards_at(0x1F80_1814)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x0F, 64, || timed_load_hazards_at(irq::I_STAT)),
    );

    for (set, spin_count) in SPINS.into_iter().enumerate() {
        let base = 0x10 + set as u8 * 3;
        push_timing_record(
            &mut records,
            &mut next,
            sample_timing(base, spin_count as u16, || {
                timer_delta(timers::Timer::Timer2, 0, spin_count)
            }),
        );
        push_timing_record(
            &mut records,
            &mut next,
            sample_timing(base + 1, spin_count as u16, || {
                timer_delta(timers::Timer::Timer2, TIMER_MODE_CLOCK_SOURCE_2, spin_count)
            }),
        );
        push_timing_record(
            &mut records,
            &mut next,
            sample_timing(base + 2, spin_count as u16, || {
                timer_delta(timers::Timer::Timer0, TIMER_MODE_CLOCK_SOURCE_1, spin_count)
            }),
        );
    }

    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x1C, 1024, || call_uncached_timing(timed_icache_cold)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x1D, 1024, || call_uncached_timing(timed_icache_warm)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x20, 0xFFFF, || {
            timer_delta(timers::Timer::Timer1, TIMER_MODE_CLOCK_SOURCE_1, 0x20000)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x21, 16, timed_gte_rtps_commands),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x22, 8, timed_gte_rtpt_commands),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x23, 16, timed_gte_nclip_commands),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x24, 16, timed_gte_mvmva_commands),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x25, 4, timed_gte_ncdt_commands),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x26, 4, timed_gte_ncct_commands),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x30, 16, || timed_otc_dma_cycles(16)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x31, 64, || timed_otc_dma_cycles(64)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x32, 256, || timed_otc_dma_cycles(256)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x40, 1, timed_cdrom_getstat),
    );
    // CD battery. Timed on Timer 1 HBlank ticks, not Timer 2 cycles, and each
    // record repeats through sample_timing so a retry or a bad block shows up
    // as a min/max spread instead of silently becoming the answer.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x90, 1, || cd_seek_distance(1)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x91, 16, || cd_seek_distance(16)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x92, 128, || cd_seek_distance(128)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x93, 512, || cd_seek_distance(512)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x94, 8, || cd_read_throughput(false, 8)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x95, 8, || cd_read_throughput(true, 8)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x96, 1, || {
            cd_timed(|| cdrom::try_get_stat(CD_SPINS).is_some())
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x97, 1, || {
            cd_timed(|| cdrom::try_set_mode(0, CD_SPINS).is_some())
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x98, 1, || {
            cd_timed(|| cdrom::try_get_loc_p(CD_SPINS).is_some())
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x99, 1, || {
            cd_timed(|| cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]))
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x9A, 1, || {
            cd_timed(|| cd_command_until_complete_timed(cdrom::CMD_INIT, &[]))
        }),
    );
    // CD-DA contention. 0x9B against 0x9C is the whole point: identical read
    // path, identical sector count, the only difference being live audio.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x9B, 8, || cd_read_with_audio(true, 8)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x9C, 8, || cd_read_with_audio(false, 8)),
    );
    push_timing_record(&mut records, &mut next, sample_timing(0x9D, 1, cd_play_start));
    // Seek sweep. Four distances proved too few to model: the console measured
    // +128 slower than +512, and no monotonic fit came within 2x of the middle
    // points. Ten distances with the same five repeats each make an outlier
    // visible AS an outlier rather than as the shape of the curve.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC0, 2, || cd_seek_distance(2)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC1, 4, || cd_seek_distance(4)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC2, 8, || cd_seek_distance(8)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC3, 32, || cd_seek_distance(32)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC4, 64, || cd_seek_distance(64)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC5, 256, || cd_seek_distance(256)),
    );
    // Backward seeks at the same distances. A drive settles differently
    // approaching from outside, and every existing record seeks forward only,
    // so a direction asymmetry would currently be invisible.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC6, 64, || cd_seek_backward(64)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xC7, 256, || cd_seek_backward(256)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        // Must establish playback itself: 0x9D pauses and mutes when it
        // finishes, so measuring "during CD-DA" without restarting audio would
        // silently measure the idle case instead.
        sample_timing(0x9E, 1, cd_getlocp_during_playback),
    );
    // GPU fill rate. Pixel counts are held identical across shading modes so
    // the DIFFERENCE isolates interpolation, blending and dither cost from the
    // per-pixel floor; 0xAA/0xAB hold total pixels constant while changing
    // primitive count, which separates setup cost from fill cost.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA0, 16, || {
            timed_fill_batch(16, 0x2000_80FF, 32, FillKind::Flat, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA1, 16, || {
            timed_fill_batch(16, 0x3000_00FF, 32, FillKind::Gouraud, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA2, 16, || {
            timed_fill_batch(16, 0x2400_80FF, 32, FillKind::Textured { tpage: 0, span: 32 }, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA3, 16, || {
            timed_fill_batch(16, 0x2400_80FF, 32, FillKind::Textured { tpage: 0x40, span: 32 }, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA4, 16, || {
            timed_fill_batch(16, 0x2400_80FF, 32, FillKind::Textured { tpage: 0x80, span: 32 }, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA5, 16, || {
            timed_fill_batch(16, 0x2800_80FF, 32, FillKind::Flat, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA6, 16, || {
            timed_fill_batch(16, 0x3800_00FF, 32, FillKind::Gouraud, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA7, 16, || {
            timed_fill_batch(16, 0x2C00_80FF, 32, FillKind::Textured { tpage: 0, span: 32 }, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA8, 16, || {
            timed_fill_batch(16, 0x2A00_80FF, 32, FillKind::Translucent, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xA9, 16, || {
            timed_fill_batch(16, 0x3000_00FF, 32, FillKind::Gouraud, true)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xAA, 4, || {
            timed_fill_batch(4, 0x2800_80FF, 64, FillKind::Flat, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xAB, 64, || {
            timed_fill_batch(64, 0x2800_80FF, 8, FillKind::Flat, false)
        }),
    );
    // Texture cache: identical pixel count, different UV footprint. A 2 KiB
    // cache should make the wide walk markedly slower than the tight resample.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xAC, 16, || {
            timed_fill_batch(16, 0x2C00_80FF, 32, FillKind::Textured { tpage: 0, span: 255 }, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xAD, 16, || {
            timed_fill_batch(16, 0x2C00_80FF, 32, FillKind::Textured { tpage: 0, span: 8 }, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xAE, 16, || {
            timed_fill_batch(16, 0x6000_80FF, 32, FillKind::Rect, false)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xAF, 16, || {
            // GP0 0x64: the 8bpp-CLUT textured rect path that renders black in
            // both backends and has never been measured on silicon.
            timed_fill_batch(16, 0x6400_80FF, 32, FillKind::TexturedRect { clut: 0 }, false)
        }),
    );
    // MDEC. No coverage at all before now. Command number lives in bits 31..29:
    // 2 = set quant table, 3 = set scale table, 1 = decode.
    push_timing_record(
        &mut records,
        &mut next,
        // Set quant table, luma only: 64 bytes = 16 words.
        sample_timing(0xB0, 16, || timed_mdec(0x4000_0000, 16)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        // Set quant table, luma + chroma: 128 bytes = 32 words.
        sample_timing(0xB1, 32, || timed_mdec(0x4000_0001, 32)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        // Set scale (IDCT) table: 64 halfwords = 32 words.
        sample_timing(0xB2, 32, || timed_mdec(0x6000_0000, 32)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB3, 1, timed_mdec_reset_settle),
    );
    // Decode-to-drained for one and four colour macroblocks. Two points, so
    // the host can separate per-macroblock cost from command setup.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB4, 1, || timed_mdec_decode(1)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB5, 2, || timed_mdec_decode(2)),
    );
    // SIO: the same poll at four pacing configurations.
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB6, 1, || timed_pad_poll(PROBE_VARIANTS[0].1, PROBE_VARIANTS[0].2)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB7, 1, || timed_pad_poll(PROBE_VARIANTS[1].1, PROBE_VARIANTS[1].2)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB8, 1, || timed_pad_poll(PROBE_VARIANTS[2].1, PROBE_VARIANTS[2].2)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0xB9, 1, || timed_pad_poll(PROBE_VARIANTS[3].1, PROBE_VARIANTS[3].2)),
    );
    // Setup-delay sweep. The console answered at setup 0, gave NO reply at 128,
    // and answered again at 384: non-monotonic, so the threshold cannot be read
    // off four points. Twelve evenly spaced delays bracket where a real pad
    // starts replying, which is the SCPH-1200 problem stated as a measurement.
    let mut sweep = 0usize;
    while sweep < SIO_SETUP_SWEEP.len() {
        let setup = SIO_SETUP_SWEEP[sweep];
        push_timing_record(
            &mut records,
            &mut next,
            sample_timing(0xD0 + sweep as u8, (setup / 8) as u16, move || {
                timed_pad_poll(setup, 0)
            }),
        );
        sweep += 1;
    }
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x41, 1, timed_gpu_irq_settle),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x42, 1, || {
            call_uncached_entry_timing(timed_icache_entry_cold, __hwtest_icache_entry_w0)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x43, 1, || {
            call_uncached_entry_timing(timed_icache_entry_cold, __hwtest_icache_entry_w1)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x44, 1, || {
            call_uncached_entry_timing(timed_icache_entry_cold, __hwtest_icache_entry_w2)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x45, 1, || {
            call_uncached_entry_timing(timed_icache_entry_warm, __hwtest_icache_entry_w0)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x46, 64, timed_untaken_branches),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x47, 64, || {
            timed_byte_load_hazards_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x48, 64, || {
            timed_half_load_hazards_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x49, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_byte_load_hazards_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x4A, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_half_load_hazards_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x4B, 64, || timed_load_hazards_at(0xBFC0_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x4C, 64, || timed_half_load_hazards_at(0xBFC0_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x4D, 64, || timed_byte_load_hazards_at(0xBFC0_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x4E, 64, || timed_half_load_hazards_at(0x1F80_1DAE)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x4F, 64, || timed_load_hazards_at(0x1F80_1044)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x50, 64, || {
            timed_byte_stores_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x51, 64, || {
            timed_half_stores_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x52, 64, || timed_byte_load_hazards_at(0x1F80_1800)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x53, 64, || timed_half_load_hazards_at(0x1F80_1800)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x54, 64, || timed_load_hazards_at(0x1F80_1800)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x55, 64, || timed_byte_load_hazards_at(0x1F00_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x56, 64, || timed_half_load_hazards_at(0x1F00_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x57, 64, || timed_load_hazards_at(0x1F00_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x58, 64, || timed_byte_load_hazards_at(0x1F80_2000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x59, 64, || timed_half_load_hazards_at(0x1F80_2000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x5A, 64, || timed_load_hazards_at(0x1F80_2000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x5B, 64, || timed_byte_load_hazards_at(0x1FA0_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x5C, 64, || timed_half_load_hazards_at(0x1FA0_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x5D, 64, || timed_load_hazards_at(0x1FA0_0000)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x5E, 64, || timed_byte_load_hazards_at(0x1F80_1DAE)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x5F, 64, || timed_load_hazards_at(0x1F80_1DAC)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x60, 64, || timed_byte_load_hazards_at(0xFFFE_0130)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x61, 64, || timed_half_load_hazards_at(0xFFFE_0130)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x62, 64, || timed_load_hazards_at(0xFFFE_0130)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x63, 64, || timed_load_hazards_at(0x1F80_1010)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x64, 64, || timed_unaligned_word_loads_at(0x1F80_1DAA)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x65, 512, timed_spu_dma_write_512_halfwords),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x66, 16, || timed_gpu_dma_block(16, 1)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x67, 64, || timed_gpu_dma_block(16, 4)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x68, 256, || timed_gpu_dma_block(16, 16)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x69, 256, || timed_gpu_dma_block(64, 4)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x6A, 256, || timed_gpu_dma_block(256, 1)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x6B, 258, timed_gpu_dma_linked_2x128),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x6C, 272, || timed_gpu_line_batch(false, 16, 16)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x6D, 2056, || timed_gpu_line_batch(false, 256, 8)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x6E, 272, || timed_gpu_line_batch(true, 16, 16)),
    );
    push_timing_record(
        &mut records,
        &mut next,
        sample_timing(0x6F, 2056, || timed_gpu_line_batch(true, 256, 8)),
    );
    let (refresh_period, refresh_stall) = sample_dram_refresh();
    push_timing_record(&mut records, &mut next, refresh_period);
    push_timing_record(&mut records, &mut next, refresh_stall);
    debug_assert_eq!(next, TIMING_RECORD_COUNT);

    let mut memory_control = [0u32; MEMORY_CONTROL_REGISTER_COUNT];
    let mut register = 0usize;
    while register < memory_control.len() {
        memory_control[register] = unsafe { psx_io::read32(0x1F80_1000 + register as u32 * 4) };
        register += 1;
    }

    let mut hash = 0x5449_4D33;
    let mut jitter = 0u32;
    for record in records {
        hash = mix32(hash, record.id as u32);
        hash = mix32(hash, record.work as u32);
        hash = mix32(hash, record.min as u32);
        hash = mix32(hash, record.max as u32);
        jitter = jitter.wrapping_add(record.max.wrapping_sub(record.min) as u32);
    }
    for (index, value) in memory_control.iter().copied().enumerate() {
        hash = mix32(hash, 0x4D43_0000 | index as u32);
        hash = mix32(hash, value);
    }
    let precision = run_precision_scan();
    for (index, value) in precision.iter().copied().enumerate() {
        hash = mix32(hash, 0x5052_0000 | index as u32);
        hash = mix32(hash, value);
    }
    TimingReport {
        summary: ScanReport::info(
            TIMING_RECORD_COUNT as u16,
            hash,
            jitter,
            "qr min max cycles",
        ),
        records,
        memory_control,
        precision,
    }
}

fn push_timing_record(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
    record: TimingRecord,
) {
    tty::print("hardware-tests: rec ");
    tty::print(hex2(record.id).as_str());
    tty::print(" min=");
    tty_print_dec_u16(record.min);
    tty::print(" max=");
    tty_print_dec_u16(record.max);
    tty::println("");
    records[*next] = record;
    *next += 1;
    draw_init_progress(*next, TIMING_RECORD_COUNT, (80, 200, 255));
    // Between records the console is briefly ours again: let START or
    // TRIANGLE skip the rest of the scan. Remaining records stay pending
    // and the capture still encodes, so a stalled drive costs the timing
    // envelopes instead of the whole session.
    if !scan_aborted() {
        let pad = psx_pad::poll_port1().buttons;
        if pad.is_held(button::START) || pad.is_held(button::TRIANGLE) {
            unsafe { core::ptr::write_volatile(&raw mut SCAN_ABORT, true) };
            tty::println("hardware-tests: timing scan aborted by operator");
        }
    }
}

/// Blink a marker beside the progress bar once per timing sample.
///
/// Ten of this scan's records are mechanical CD work -- four seek
/// distances, two throughput probes and four command round trips -- at
/// five samples each and a two-second deadline apiece. The bar only
/// advances once per record, so the screen can sit motionless for ten
/// seconds at a time, which is exactly what every hang we chased
/// tonight looked like. This makes slow and stuck different pictures.
fn scan_heartbeat() {
    static mut PHASE: u32 = 0;
    let phase = unsafe {
        let next = core::ptr::read_volatile(&raw const PHASE).wrapping_add(1);
        core::ptr::write_volatile(&raw mut PHASE, next);
        next
    };
    let rgb = if phase & 1 == 0 {
        0x0020_2020
    } else {
        0x00FF_E040
    };
    gpu_io::wait_cmd_ready();
    // Absolute VRAM coordinates, like the bar: the battery blocks the
    // frame loop, so ordinary draws would land in the hidden buffer.
    // GP0(02h) snaps X and width to 16, hence the aligned geometry.
    for buffer_y in [200u32, 440] {
        gpu_io::write_gp0(0x0200_0000 | rgb);
        gpu_io::write_gp0((buffer_y << 16) | 304);
        gpu_io::write_gp0((8u32 << 16) | 16);
    }
}

/// Paint the id of the record being measured as eight bit-cells under the
/// progress bar, most significant bit first: bright yellow = 1, dark = 0.
///
/// The bar moves 1.5 px per record, so a photo of a frozen screen only
/// narrows a hang down to "roughly record 18". This row names the exact
/// record instead. Same immediate GP0 fill-rects as the bar, both buffer
/// halves, cell width 16 because GP0(02h) snaps X and width to 16.
fn draw_record_id(id: u8) {
    for buffer_y in [212u32, 452] {
        for bit in 0..8u32 {
            let rgb = if id & (0x80 >> bit) != 0 {
                0x0040_E0FF // (255, 224, 64) bright yellow
            } else {
                0x0020_2020
            };
            gpu_io::wait_cmd_ready();
            gpu_io::write_gp0(0x0200_0000 | rgb);
            gpu_io::write_gp0((buffer_y << 16) | (32 + bit * 16));
            gpu_io::write_gp0((8u32 << 16) | 16);
        }
    }
    gpu_io::wait_cmd_ready();
}

/// Draw a progress bar straight into the visible framebuffer.
///
/// The whole battery runs inside `Scene::init`, before the engine has drawn a
/// single frame, so `tty::print` progress is invisible on a real console: the
/// operator watches a black screen for tens of seconds and reasonably concludes
/// the disc is dead. That is exactly what happened on the first burn.
///
/// Immediate GP0 fill-rects need no framebuffer swap, no font upload and no
/// access to `Ctx`, so they work at any point during init. Both buffer halves
/// are painted because which one is currently being scanned out depends on how
/// many swaps have happened, and a bar only visible half the time is no better
/// than no bar.
fn draw_init_progress(done: usize, total: usize, colour: (u8, u8, u8)) {
    const BAR_X: u32 = 24;
    const BAR_W: u32 = 272;
    const BAR_H: u32 = 8;
    let filled = if total == 0 {
        0
    } else {
        (BAR_W * done as u32 / total as u32).min(BAR_W)
    };
    let rgb = (colour.0 as u32) | ((colour.1 as u32) << 8) | ((colour.2 as u32) << 16);

    gpu_io::wait_cmd_ready();
    for buffer_y in [200u32, 440] {
        // Track, then the filled portion. GP0 0x02 takes absolute VRAM
        // coordinates and ignores draw area and offset, which is what makes it
        // usable before any draw environment has been set up.
        gpu_io::write_gp0(0x0200_0000 | 0x0020_2020);
        gpu_io::write_gp0((buffer_y << 16) | BAR_X);
        gpu_io::write_gp0((BAR_H << 16) | BAR_W);
        if filled != 0 {
            gpu_io::write_gp0(0x0200_0000 | rgb);
            gpu_io::write_gp0((buffer_y << 16) | BAR_X);
            gpu_io::write_gp0((BAR_H << 16) | filled);
        }
    }
    gpu_io::wait_cmd_ready();
}

/// Repeat the probe with interrupts masked, then keep min/median/max.
///
/// Masking matters: without it the only defence against a VBlank or CD IRQ
/// landing inside a measured window is that one of the repeats happens to
/// escape, so the min/max gap reports "did an interrupt hit" rather than real
/// hardware jitter. With IE clear the spread is the silicon's own.
/// Set by [`push_timing_record`] when the operator asks to move on.
/// The CD records in this scan are mechanical: four seek distances and
/// the read-throughput probes, five samples each, every one bounded by a
/// two-second deadline. On a drive that never answers, that is minutes
/// of an apparently frozen console, which is indistinguishable from the
/// hangs we have been chasing all evening.
static mut SCAN_ABORT: bool = false;

fn scan_aborted() -> bool {
    unsafe { core::ptr::read_volatile(&raw const SCAN_ABORT) }
}

// inline(never): with every instantiation inlined, run_timing_scan grows
// past the +-128 KiB reach of a MIPS PC16 branch and the build dies with
// "out of range PC16 fixup".
#[inline(never)]
fn sample_timing<F>(id: u8, work: u16, mut probe: F) -> TimingRecord
where
    F: FnMut() -> u16,
{
    // Checked here rather than at the push, because the measurement runs
    // as the push's argument: this is the only place that can skip it.
    if scan_aborted() {
        return TimingRecord::pending();
    }
    draw_record_id(id);
    let mut samples = [0u16; TIMING_SAMPLES];
    let guard = IrqGuard::mask();
    let mut run = 0;
    while run < TIMING_SAMPLES {
        samples[run] = probe();
        run += 1;
        scan_heartbeat();
        // The pad driver polls SIO0 directly and never takes the CPU
        // interrupt, so it works under the IrqGuard. Checking between
        // samples (not just between records) means START still skips a
        // record whose probe is slow or stuck mid-measurement; the guard's
        // Drop restores SR on the early return.
        let pad = psx_pad::poll_port1().buttons;
        if pad.is_held(button::START) || pad.is_held(button::TRIANGLE) {
            unsafe { core::ptr::write_volatile(&raw mut SCAN_ABORT, true) };
            tty::println("hardware-tests: timing scan aborted by operator");
            return TimingRecord::pending();
        }
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
fn summarize_nonzero(id: u8, work: u16, samples: [u16; TIMING_SAMPLES]) -> TimingRecord {
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
    let Some(saved) = cdrom::dispatch_command(command, params, CD_SPINS) else {
        return false;
    };
    let mut seen_ack = false;
    let ok = loop {
        if timers::counter(timers::Timer::Timer1) >= CD_DEADLINE_HBLANKS {
            break false;
        }
        match cdrom::irq_flag_value() {
            0 => {}
            5 => {
                cdrom::discard_response();
                cdrom::acknowledge_irq(5);
                break false;
            }
            3 => {
                cdrom::discard_response();
                cdrom::acknowledge_irq(3);
                seen_ack = true;
            }
            2 if seen_ack => {
                cdrom::discard_response();
                cdrom::acknowledge_irq(2);
                break true;
            }
            other => {
                cdrom::discard_response();
                cdrom::acknowledge_irq(other);
            }
        }
    };
    cdrom::restore_irq_output(saved);
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
    let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);

    if playing {
        if cdrom::try_set_mode(cdrom::MODE_CDDA, CD_SPINS).is_none() {
            return CD_FAILED;
        }
        // Track 2 is the synthetic tone the disc build appends.
        if cdrom::try_play_track(2, CD_SPINS).is_none() {
            return CD_FAILED;
        }
        let _ = cdrom::try_demute(CD_SPINS);
        // Let playback actually establish before the read fights it; measuring
        // during spin-up would confuse start-up cost with contention.
        cd_clock_reset();
        while timers::counter(timers::Timer::Timer1) < 1_000 {}
    }

    // Data mode for the read itself.
    if cdrom::try_set_mode(0, CD_SPINS).is_none()
        || cdrom::try_set_loc_lba(CD_TEST_LBA, CD_SPINS).is_none()
        || cdrom::try_read_n(CD_SPINS).is_none()
    {
        cd_clock_reset();
        let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);
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
    let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);
    let _ = cdrom::try_mute(CD_SPINS);
    if primed {
        elapsed
    } else {
        CD_FAILED
    }
}

/// Time CD-DA playback from the Play command to the first position report.
fn cd_play_start() -> u16 {
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(cdrom::CMD_STOP, &[]);
    if cdrom::try_set_mode(cdrom::MODE_CDDA, CD_SPINS).is_none() {
        return CD_FAILED;
    }
    let elapsed = cd_timed(|| {
        if cdrom::try_play_track(2, CD_SPINS).is_none() {
            return false;
        }
        // Position advancing is the first proof audio is actually streaming,
        // rather than the command merely having been accepted.
        let mut polls = 0;
        while polls < 64 {
            if let Some(response) = cdrom::try_get_loc_p(CD_SPINS) {
                if !response.is_empty() {
                    return true;
                }
            }
            polls += 1;
        }
        false
    });
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);
    let _ = cdrom::try_mute(CD_SPINS);
    elapsed
}

/// Time GetLocP while CD-DA is genuinely streaming.
fn cd_getlocp_during_playback() -> u16 {
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(cdrom::CMD_STOP, &[]);
    if cdrom::try_set_mode(cdrom::MODE_CDDA, CD_SPINS).is_none()
        || cdrom::try_play_track(2, CD_SPINS).is_none()
    {
        return CD_FAILED;
    }
    cd_clock_reset();
    while timers::counter(timers::Timer::Timer1) < 1_000 {}
    let elapsed = cd_timed(|| {
        cdrom::try_get_loc_p(CD_SPINS).is_some_and(|response| !response.is_empty())
    });
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);
    let _ = cdrom::try_mute(CD_SPINS);
    elapsed
}

/// Wait for one streamed data sector under the same real-time deadline.
fn cd_sector_timed() -> bool {
    loop {
        if timers::counter(timers::Timer::Timer1) >= CD_DEADLINE_HBLANKS {
            return false;
        }
        match cdrom::irq_flag_value() {
            1 => {
                cdrom::acknowledge_irq(1);
                return true;
            }
            0 => {}
            5 => {
                cdrom::discard_response();
                cdrom::acknowledge_irq(5);
                return false;
            }
            other => {
                cdrom::discard_response();
                cdrom::acknowledge_irq(other);
            }
        }
    }
}

/// Park the head at a known LBA so the next seek covers a known distance.
/// Without this every seek record would measure travel from wherever the
/// previous record happened to leave the head.
fn cd_park(lba: u32) -> bool {
    cd_clock_reset();
    cdrom::try_set_loc_lba(lba, CD_SPINS).is_some()
        && cd_command_until_complete_timed(cdrom::CMD_SEEKL, &[])
}

/// Seek `distance` sectors BACKWARD onto the parked origin, timing only the
/// seek. Parks beyond the target so the head approaches from the far side.
fn cd_seek_backward(distance: u32) -> u16 {
    let origin = CD_TEST_LBA + distance;
    if !cd_park(origin) {
        return CD_FAILED;
    }
    if cdrom::try_set_loc_lba(CD_TEST_LBA, CD_SPINS).is_none() {
        return CD_FAILED;
    }
    cd_timed(|| cd_command_until_complete_timed(cdrom::CMD_SEEKL, &[]))
}

/// Seek `distance` sectors away from the parked origin and time only the seek.
fn cd_seek_distance(distance: u32) -> u16 {
    if !cd_park(CD_TEST_LBA) {
        return CD_FAILED;
    }
    let target = CD_TEST_LBA + distance;
    if cdrom::try_set_loc_lba(target, CD_SPINS).is_none() {
        return CD_FAILED;
    }
    cd_timed(|| cd_command_until_complete_timed(cdrom::CMD_SEEKL, &[]))
}

/// Time `sectors` sequential sector arrivals once a read is already streaming,
/// which isolates sustained throughput from the initial seek and spin-up.
fn cd_read_throughput(double_speed: bool, sectors: u32) -> u16 {
    let mode = if double_speed {
        cdrom::MODE_DOUBLE_SPEED
    } else {
        0
    };
    if cdrom::try_set_mode(mode, CD_SPINS).is_none()
        || !cd_park(CD_TEST_LBA)
        || cdrom::try_set_loc_lba(CD_TEST_LBA, CD_SPINS).is_none()
        || cdrom::try_read_n(CD_SPINS).is_none()
    {
        cd_clock_reset();
        let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);
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
    let _ = cd_command_until_complete_timed(cdrom::CMD_PAUSE, &[]);
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

/// Off-screen scratch row base. Below the 240-line display and clear of the
/// 96x96 hash scratch at (512, 256).
const FILL_Y: u32 = 400;
const FILL_X: u32 = 640;

/// Set up a clean draw environment for a fill measurement.
fn fill_env(dither: bool) {
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xE300_0000);
    gpu_io::write_gp0(0xE400_0000 | 1023 | (511 << 10));
    gpu_io::write_gp0(0xE500_0000);
    gpu_io::write_gp0(if dither { 0xE100_0200 } else { 0xE100_0000 });
    // Texture window covering the whole page, so texture records are not
    // silently clamped to a sub-rect.
    gpu_io::write_gp0(0xE200_0000);
    gpu_io::wait_cmd_ready();
}

/// Wait for the GPU to report command-ready, so the measured interval includes
/// the actual raster work rather than only the CPU's FIFO writes.
fn fill_drain() -> bool {
    let mut guard = 0u32;
    while gpu_io::gpustat().bits() & (1 << 26) == 0 && guard < 1_000_000 {
        guard += 1;
    }
    guard < 1_000_000
}

/// One monochrome/Gouraud/textured triangle or quad batch.
///
/// `command` is the GP0 opcode; `words` are the packet words after it, built by
/// the caller so each variant's exact packet shape is explicit.
fn timed_fill_batch(count: u16, command: u32, size: u32, kind: FillKind, dither: bool) -> u16 {
    fill_env(dither);
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let mut index = 0u16;
    while index < count {
        let y = FILL_Y + u32::from(index & 7);
        let x = FILL_X;
        gpu_io::write_gp0(command);
        match kind {
            FillKind::Flat | FillKind::Translucent => {
                gpu_io::write_gp0((y << 16) | x);
                gpu_io::write_gp0(((y) << 16) | (x + size));
                gpu_io::write_gp0(((y + size) << 16) | x);
                if command & 0x0800_0000 != 0 {
                    gpu_io::write_gp0(((y + size) << 16) | (x + size));
                }
            }
            FillKind::Gouraud => {
                gpu_io::write_gp0((y << 16) | x);
                gpu_io::write_gp0(0x0000_FF00);
                gpu_io::write_gp0(((y) << 16) | (x + size));
                gpu_io::write_gp0(0x00FF_0000);
                gpu_io::write_gp0(((y + size) << 16) | x);
                if command & 0x0800_0000 != 0 {
                    gpu_io::write_gp0(0x00FF_00FF);
                    gpu_io::write_gp0(((y + size) << 16) | (x + size));
                }
            }
            FillKind::Textured { tpage, span } => {
                // UV span is decoupled from screen size so a record can walk a
                // wide texture (cache-hostile) or resample a small one
                // (cache-friendly) at identical pixel cost.
                gpu_io::write_gp0((y << 16) | x);
                gpu_io::write_gp0((u32::from(tpage) << 16) | 0x0000);
                gpu_io::write_gp0(((y) << 16) | (x + size));
                gpu_io::write_gp0(u32::from(span) & 0xFF);
                gpu_io::write_gp0(((y + size) << 16) | x);
                gpu_io::write_gp0(u32::from(span) << 8);
                if command & 0x0800_0000 != 0 {
                    gpu_io::write_gp0(((y + size) << 16) | (x + size));
                    gpu_io::write_gp0((u32::from(span) << 8) | u32::from(span));
                }
            }
            FillKind::Rect => {
                gpu_io::write_gp0((y << 16) | x);
                gpu_io::write_gp0((size << 16) | size);
            }
            FillKind::TexturedRect { clut } => {
                // GP0 0x64 takes an extra UV + CLUT word between position and
                // extent. Omitting it shifts the extent into the UV slot and
                // the GPU draws something unrelated to what was asked for.
                gpu_io::write_gp0((y << 16) | x);
                gpu_io::write_gp0((u32::from(clut) << 16) | 0x0000);
                gpu_io::write_gp0((size << 16) | size);
            }
        }
        index += 1;
    }
    let ok = fill_drain();
    let elapsed = timers::counter(timers::Timer::Timer2);
    if ok {
        elapsed
    } else {
        0xFFFF
    }
}

#[derive(Copy, Clone)]
enum FillKind {
    Flat,
    Gouraud,
    Translucent,
    Textured { tpage: u16, span: u8 },
    Rect,
    TexturedRect { clut: u16 },
}

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
        psx_io::write32(MDEC_CTRL, MDEC_RESET);
    }
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    unsafe {
        psx_io::write32(MDEC_CMD, command);
        let mut word = 0u16;
        while word < payload_words {
            psx_io::write32(MDEC_CMD, 0x0000_0000);
            word += 1;
        }
    }
    let mut guard = 0u32;
    while unsafe { psx_io::read32(MDEC_CTRL) } & MDEC_BUSY != 0 && guard < 200_000 {
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
        psx_io::write32(MDEC_CMD, command);
        let mut word = 0u16;
        while word < words {
            psx_io::write32(MDEC_CMD, MDEC_BLOCK_DC_EOB);
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
        let status = unsafe { psx_io::read32(MDEC_CTRL) };
        if status & MDEC_OUT_FIFO_EMPTY == 0 {
            let _ = unsafe { psx_io::read32(MDEC_CMD) };
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
        psx_io::write32(MDEC_CTRL, MDEC_RESET);
        psx_io::write32(MDEC_CMD, 0x4000_0001);
        let mut word = 0;
        while word < 32 {
            psx_io::write32(MDEC_CMD, 0x1010_1010);
            word += 1;
        }
        psx_io::write32(MDEC_CMD, 0x6000_0000);
        word = 0;
        while word < 32 {
            psx_io::write32(MDEC_CMD, 0x0000_1000);
            word += 1;
        }
    }
}

/// Time a reset returning the decoder to idle.
fn timed_mdec_reset_settle() -> u16 {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    unsafe {
        psx_io::write32(MDEC_CTRL, MDEC_RESET);
    }
    let mut guard = 0u32;
    while unsafe { psx_io::read32(MDEC_CTRL) } & MDEC_BUSY != 0 && guard < 200_000 {
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
        psx_io::write32(MDEC_CTRL, MDEC_RESET);
        psx_io::read32(MDEC_CTRL)
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
    let poll = psx_pad::poll_port1_diag(setup, interbyte);
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

/// One bounded OTC kick: force-stop the channel, arm it with `chcr`,
/// wait with a spin cap, then verify and scrub the chain. Returns
/// (completed, chain_correct). Never hangs, even on a wedging channel.
fn otc_kick_bounded(ptr: *mut u32, words: u16, chcr: u32, delay: bool) -> (bool, bool) {
    // Clear a possibly-wedged START from the previous variant; on
    // silicon clearing bit 24 requests an abort.
    dma::set_chcr(dma::Channel::Otc, 0);
    for _ in 0..1_000u32 {
        unsafe { core::ptr::read_volatile(psx_io::dma::DPCR as *const u32) };
    }
    dma::enable_channel(dma::Channel::Otc);
    if delay {
        for _ in 0..10_000u32 {
            unsafe { core::ptr::read_volatile(psx_io::dma::DPCR as *const u32) };
        }
    }
    let last = unsafe { ptr.add(words as usize - 1) };
    dma::set_madr(dma::Channel::Otc, last as u32);
    dma::set_bcr_manual(dma::Channel::Otc, words);
    dma::set_chcr(dma::Channel::Otc, chcr);
    let mut spins = 0u32;
    while dma::is_busy(dma::Channel::Otc) && spins < 200_000 {
        spins += 1;
    }
    let done = !dma::is_busy(dma::Channel::Otc);
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
    expect_eq(0x3F, observed, "otc variants (done,ok) pairs")
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
    // Configure once, then let the counter run: Timer 1 in HBlank mode must
    // advance on its own. (The old form read gpu::scanline_counter(), which
    // reconfigures Timer 1 before every read and so always returned ~0; the
    // `<= 340` range check passed vacuously.)
    gpu::configure_vsync_timer();
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
    unsafe { gte_ops::nclip() };
    // This is the functional arithmetic check, not the result-latency
    // probe. Let MAC0 settle so real silicon's immediately-next-read hazard
    // does not turn correct NCLIP arithmetic into a headline failure.
    gte_nops!(64);
    let positive = mfc2!(24) as i32;
    let positive_flag_clear = gte_flag_master_clear();

    ctc2!(31, 0);
    mtc2!(12, pack_gte_xy(0, 0));
    mtc2!(13, pack_gte_xy(0, 10));
    mtc2!(14, pack_gte_xy(10, 0));
    unsafe { gte_ops::nclip() };
    gte_nops!(64);
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
    expect_eq(0x8000_6000, cfc2!(31), "rtpt A FLAG")
}
fn test_gte_scene_rtpt_b_flag() -> TestResult {
    scene_rtpt(RTPT_B);
    expect_eq(0x8006_6000, cfc2!(31), "rtpt B FLAG")
}
fn test_gte_scene_rtpt_a_sz3() -> TestResult {
    scene_rtpt(RTPT_A);
    expect_eq(0x0000_02e9, mfc2!(19), "rtpt A SZ3")
}
fn test_gte_scene_rtpt_e_sz3() -> TestResult {
    scene_rtpt(RTPT_E);
    expect_eq(0x0000_1fd9, mfc2!(19), "rtpt E SZ3")
}

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
    expect_eq(
        0x0000_2764,
        scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
        "scene nclip A",
    )
}
fn test_gte_scene_nclip_b() -> TestResult {
    expect_eq(
        0x0000_30ba,
        scene_nclip_mac0(0x0073_00d5, 0xffde_00dc, 0xffd8_0130),
        "scene nclip B",
    )
}
fn test_gte_scene_nclip_c() -> TestResult {
    expect_eq(
        0x0000_3e7e,
        scene_nclip_mac0(0x0079_011f, 0xffd8_0130, 0xffd2_0194),
        "scene nclip C",
    )
}

// LZCS/LZCR: leading-bit count (bits equal to bit 31). These functional
// checks deliberately read a settled result; the dedicated +1..+6 cases
// below measure the silicon stale-read window independently.
fn lzcr(value: u32) -> u32 {
    mtc2!(30, value);
    gte_nops!(64);
    mfc2!(31)
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
    ctc2!(21, 0x0000_1000); // FCX
    ctc2!(22, 0x0000_2000); // FCY
    ctc2!(23, 0x0000_3000); // FCZ
    mtc2!(0, 0x2040_0340);
    mtc2!(1, 0x0000_09c0);
    unsafe { gte_ops::mvmva_rt_v0_fc_sf1() };
}
fn test_gte_mvmva_fc_mac1() -> TestResult {
    run_mvmva_fc();
    expect_eq(0xffff_fcc5, mfc2!(25), "mvmva FC MAC1")
}
fn test_gte_mvmva_fc_mac2() -> TestResult {
    run_mvmva_fc();
    expect_eq(0xffff_e36c, mfc2!(26), "mvmva FC MAC2")
}
fn test_gte_mvmva_fc_mac3() -> TestResult {
    run_mvmva_fc();
    expect_eq(0xffff_ee75, mfc2!(27), "mvmva FC MAC3")
}
fn test_gte_sqr() -> TestResult {
    ctc2!(31, 0);
    mtc2!(9, 0x0000_1234);
    mtc2!(10, 0x0000_f8ee);
    mtc2!(11, 0x0000_0567);
    unsafe { gte_ops::sqr() };
    expect_eq(
        0x7498_ecb5,
        gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27)),
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
    ctc2!(31, 0);
    ctc2!(0, 0x0000_1000); // R11 (D1)
    ctc2!(2, 0x0000_2000); // R22 (D2)
    ctc2!(4, 0x0000_3000); // R33 (D3)
    mtc2!(9, 0x0000_0400); // IR1
    mtc2!(10, 0x0000_0500); // IR2
    mtc2!(11, 0x0000_0600); // IR3
    unsafe { gte_ops::op_sf1() };
}
fn test_gte_op_mac1() -> TestResult {
    run_op();
    expect_eq(0xffff_fd00, mfc2!(25), "op MAC1")
}
fn test_gte_op_mac2() -> TestResult {
    run_op();
    expect_eq(0x0000_0600, mfc2!(26), "op MAC2")
}
fn test_gte_op_mac3() -> TestResult {
    run_op();
    expect_eq(0xffff_fd00, mfc2!(27), "op MAC3")
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
    ctc2!(31, 0);
    ctc2!(0, 0x0000_1000); // R11=D1, R12=0
    ctc2!(1, 0x0000_0000); // R13=0, R21=0
    ctc2!(2, 0x0000_2000); // R22=D2, R23=0
    ctc2!(3, 0x0000_0000); // R31=0, R32=0
    ctc2!(4, 0x0000_3000); // R33=D3
    mtc2!(9, 0x0000_0400); // IR1
    mtc2!(10, 0x0000_0500); // IR2
    mtc2!(11, 0x0000_0600); // IR3
    mtc2!(25, 0); // MAC1
    mtc2!(26, 0); // MAC2
    mtc2!(27, 0); // MAC3
}

fn run_op_full_seed() {
    seed_op_full();
    unsafe { gte_ops::op_sf1() };
}

/// The prior console run produced the documented MAC1/MAC2 values but zero in
/// MAC3 even with every input explicitly seeded. A long control/input settle
/// before the identical OP distinguishes a CTC2/MTC2 commit hazard from an OP
/// arithmetic quirk without changing any operands.
fn run_op_full_seed_settled() {
    seed_op_full();
    gte_nops!(64);
    unsafe { gte_ops::op_sf1() };
}
fn test_gte_op_full_seed_mac1() -> TestResult {
    run_op_full_seed();
    expect_eq(0xffff_fd00, mfc2!(25), "op fs MAC1")
}
fn test_gte_op_full_seed_mac2() -> TestResult {
    run_op_full_seed();
    expect_eq(0x0000_0600, mfc2!(26), "op fs MAC2")
}
fn test_gte_op_full_seed_mac3() -> TestResult {
    run_op_full_seed();
    expect_eq(0xffff_fd00, mfc2!(27), "op fs MAC3")
}
fn test_gte_avsz3() -> TestResult {
    ctc2!(31, 0);
    ctc2!(29, 0x0000_0155); // ZSF3
    mtc2!(17, 0x0000_1000); // SZ1
    mtc2!(18, 0x0000_2000); // SZ2
    mtc2!(19, 0x0000_3000); // SZ3
    unsafe { gte_ops::avsz3() };
    expect_eq(0x0000_07fe, mfc2!(7), "avsz3 OTZ")
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
    ctc2!(31, 0);
    ctc2!(0, 0x0000_1000); // identity R11,R12
    ctc2!(1, 0x0000_0000); // R13,R21
    ctc2!(2, 0x0000_1000); // R22,R23
    ctc2!(3, 0x0000_0000); // R31,R32
    ctc2!(4, 0x0000_1000); // R33
    ctc2!(5, 0);
    ctc2!(6, 0);
    ctc2!(7, 0x0000_1000); // TRZ
    ctc2!(24, 0x00a0_0000); // OFX 160
    ctc2!(25, 0x0078_0000); // OFY 120
    ctc2!(26, 0x0000_0100); // H 256
    ctc2!(27, 0x0000_0100); // DQA
    ctc2!(28, 0);
}

fn rtps_lat(vxy0: u32, vz0: u32) {
    mtc2!(0, vxy0);
    mtc2!(1, vz0);
    unsafe { gte_ops::rtps() };
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
    ctc2!(0, pack_gte_xy(0x2000, 0));
    ctc2!(1, pack_gte_xy(0, 0));
    ctc2!(2, pack_gte_xy(0x2000, 0));
    ctc2!(3, pack_gte_xy(0, 0));
    ctc2!(4, 0x2000);
}

fn rt_sweep_vertex() {
    mtc2!(0, pack_gte_xy(0x123, -0x222));
    mtc2!(1, 0x0333);
}

/// Long-settled ground truth: matrix B fully landed, then MVMVA.
fn rt_sweep_reference() -> u32 {
    seed_scene_xform();
    load_matrix_b();
    gte_nops!(64);
    rt_sweep_vertex();
    unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
    gte_delay16();
    gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27))
}

macro_rules! rt_settle_case {
    ($name:ident, $gap:tt) => {
        fn $name() -> TestResult {
            let expected = rt_sweep_reference();
            seed_scene_xform(); // matrix A in the GTE
            rt_sweep_vertex();
            unsafe { gte_ops::mvmva_rt_v0_tr_sf1() }; // pipe warmed with A
            gte_delay16();
            let _ = mfc2!(25);
            load_matrix_b();
            gte_nops!($gap);
            unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
            gte_delay16();
            let got = gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27));
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
            unsafe { gte_ops::rtps() }; // 15-cycle op now in flight
            gte_nops!($gap);
            load_matrix_b(); // writes land while RTPS may be executing
            gte_nops!(64); // long settle: a failure means LOST, not late
            rt_sweep_vertex();
            unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
            gte_delay16();
            let got = gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27));
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
    ctc2!(5, 0); // TR = 0, matching gte_compose_joint_rotation
    ctc2!(6, 0);
    ctc2!(7, 0);
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
        mtc2!(0, cols[j].0);
        mtc2!(1, cols[j].1);
        unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
        if s_all {
            gte_nops!(64);
        }
        c[0][j] = mfc2!(25) as i32 as i16;
        c[1][j] = mfc2!(26) as i32 as i16;
        c[2][j] = mfc2!(27) as i32 as i16;
        j += 1;
    }
    if s_load {
        gte_nops!(64);
    }
    ctc2!(0, pack_gte_xy(c[0][0], c[0][1]));
    ctc2!(1, pack_gte_xy(c[0][2], c[1][0]));
    ctc2!(2, pack_gte_xy(c[1][1], c[1][2]));
    ctc2!(3, pack_gte_xy(c[2][0], c[2][1]));
    ctc2!(4, c[2][2] as i32 as u32);
    if s_all {
        gte_nops!(64);
    }
    rt_sweep_vertex();
    unsafe { gte_ops::mvmva_rt_v0_tr_sf1() };
    gte_delay16();
    gte_tri_digest(mfc2!(25), mfc2!(26), mfc2!(27))
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
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            let expected = mfc2!(24);
            // Poison the stale slot: reversed winding -> MAC0 = -100.
            mtc2!(13, pack_gte_xy(0, 10));
            mtc2!(14, pack_gte_xy(10, 0));
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            // Probe: restore positive winding, read after N nops.
            seed_nclip_pos_triangle();
            unsafe { gte_ops::nclip() };
            gte_nops!($gap);
            let got = mfc2!(24);
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
    ($name:ident, $gap:tt) => {
        fn $name() -> TestResult {
            // Prime the stale slot: LZCS 0x00ffffff -> LZCR 8, settled.
            mtc2!(30, 0x00ff_ffff);
            gte_nops!(64);
            let _ = mfc2!(31);
            // Probe: LZCS 0x00000001 -> LZCR 31, read after N nops.
            mtc2!(30, 0x0000_0001);
            gte_nops!($gap);
            let got = mfc2!(31);
            expect_eq(31, got, "lzcr settle gap")
        }
    };
}
lzcr_settle_case!(test_lzcr_settle_gap1, 1);
lzcr_settle_case!(test_lzcr_settle_gap2, 2);
lzcr_settle_case!(test_lzcr_settle_gap3, 3);
lzcr_settle_case!(test_lzcr_settle_gap4, 4);
lzcr_settle_case!(test_lzcr_settle_gap6, 6);

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
            ctc2!(31, 0);
            mtc2!(12, 0x006e_0095);
            mtc2!(13, 0xffe2_0094);
            mtc2!(14, 0xffde_00dc);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            let expected = mfc2!(24);
            // Poison MAC0 with a different settled result (swap winding).
            mtc2!(13, 0xffde_00dc);
            mtc2!(14, 0xffe2_0094);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            // Probe at +N with the original large coordinates.
            mtc2!(13, 0xffe2_0094);
            mtc2!(14, 0xffde_00dc);
            unsafe { gte_ops::nclip() };
            gte_nops!($gap);
            let got = mfc2!(24);
            expect_eq(expected, got, "nclip big-value settle gap")
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
macro_rules! mac0_ctrl_case {
    ($name:ident, $gap:tt, $s0:literal, $s1:literal, $s2:literal) => {
        fn $name() -> TestResult {
            // Settled reference.
            ctc2!(31, 0);
            mtc2!(12, $s0);
            mtc2!(13, $s1);
            mtc2!(14, $s2);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            let expected = mfc2!(24);
            // Poison: swap the two far vertices (negated cross), settled.
            mtc2!(13, $s2);
            mtc2!(14, $s1);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            // Probe at +N.
            mtc2!(13, $s1);
            mtc2!(14, $s2);
            unsafe { gte_ops::nclip() };
            gte_nops!($gap);
            let got = mfc2!(24);
            expect_eq(expected, got, "nclip controlled settle")
        }
    };
}
// Gap bisect with the scene-A coordinates (0x874 regime).
mac0_ctrl_case!(
    test_mac0_big_gap12,
    12,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap16,
    16,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap24,
    24,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap32,
    32,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
mac0_ctrl_case!(
    test_mac0_big_gap48,
    48,
    0x006e_0095,
    0xffe2_0094,
    0xffde_00dc
);
// Magnitude ladder at +4: quarter / half / double scale of scene-A.
mac0_ctrl_case!(
    test_mac0_mag_quarter,
    4,
    0x001c_0025,
    0xfff8_0025,
    0xfff7_0037
);
mac0_ctrl_case!(test_mac0_mag_half, 4, 0x0037_004a, 0xfff1_004a, 0xffef_006e);
mac0_ctrl_case!(
    test_mac0_mag_double,
    4,
    0x00dc_012a,
    0xffc4_0128,
    0xffbc_01b8
);
// Controlled-prestate replicas of scene B and C at the in-situ +2 distance.
mac0_ctrl_case!(test_mac0_ctrl_b, 2, 0x0073_00d5, 0xffde_00dc, 0xffd8_0130);
mac0_ctrl_case!(test_mac0_ctrl_c, 2, 0x0079_011f, 0xffd8_0130, 0xffd2_0194);

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
            ctc2!(31, 0);
            mtc2!(12, $s0);
            mtc2!(13, $s1);
            mtc2!(14, $s2);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            let _ = mfc2!(24);
            mtc2!(13, $s2);
            mtc2!(14, $s1);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            mtc2!(13, $s1);
            mtc2!(14, $s2);
            // No NCLIP here: settle, then dump the register state the probe
            // NCLIP would have consumed.
            gte_nops!(64);
            let got = mfc2!($reg);
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
            ctc2!(31, 0);
            mtc2!(12, $s0);
            mtc2!(13, $s1);
            mtc2!(14, $s2);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            let _ = mfc2!(24);
            mtc2!(13, $s2);
            mtc2!(14, $s1);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            mtc2!(13, $s1);
            mtc2!(14, $s2);
            unsafe { gte_ops::nclip() };
            gte_nops!(64);
            let got = mfc2!(12);
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
            let immediate = mfc2!($reg);
            // Settled reference: identical sequence but wait before reading.
            seed_proj_latency();
            rtps_lat(LAT_A_XY, LAT_A_Z);
            gte_delay16();
            rtps_lat(LAT_B_XY, LAT_B_Z);
            gte_delay16();
            let settled = mfc2!($reg);
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
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0x0200_0000 | (rgb24 & 0x00FF_FFFF));
    gpu_io::write_gp0(((y as u32) << 16) | x as u32);
    gpu_io::write_gp0(((h as u32) << 16) | w as u32);
}

/// Point the drawing area + offset at the scratch rect, so primitive coords
/// are scratch-relative (0..GPU_SW / 0..GPU_SH).
fn gpu_draw_env_scratch() {
    let (x, y) = (GPU_SX as u32, GPU_SY as u32);
    gpu_io::write_gp0(0xE300_0000 | (x & 0x3FF) | ((y & 0x1FF) << 10));
    let (rx, ry) = (x + GPU_SW as u32 - 1, y + GPU_SH as u32 - 1);
    gpu_io::write_gp0(0xE400_0000 | (rx & 0x3FF) | ((ry & 0x1FF) << 10));
    gpu_io::write_gp0(0xE500_0000 | (x & 0x7FF) | ((y & 0x7FF) << 11));
}

/// Send a primitive's data words (skipping the leading OT tag) to GP0.
fn gpu_send_prim<T>(prim_ref: &T, words: u8) {
    let base = (prim_ref as *const T).cast::<u32>();
    gpu_io::wait_cmd_ready();
    for i in 0..words as usize {
        // +1 skips the `tag` word that only the OT/DMA path consumes.
        let word = unsafe { core::ptr::read(base.add(1 + i)) };
        gpu_io::write_gp0(word);
    }
}

/// Read the scratch rect back (GP0 0xC0 + GPUREAD) and FNV-1a hash the pixels.
fn gpu_hash_scratch() -> u32 {
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xC000_0000);
    gpu_io::write_gp0(((GPU_SY as u32) << 16) | GPU_SX as u32);
    gpu_io::write_gp0(((GPU_SH as u32) << 16) | GPU_SW as u32);
    let words = (GPU_SW as u32 * GPU_SH as u32) / 2; // two 16bpp pixels per word
    let mut hash = 0x811C_9DC5u32;
    for _ in 0..words {
        let mut guard = 0u32;
        // GPUSTAT bit 27 = ready to send VRAM->CPU data.
        while gpu_io::gpustat().bits() & (1 << 27) == 0 && guard < 100_000 {
            guard += 1;
        }
        let w = gpu_io::gpuread();
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
    gpu_io::wait_cmd_ready();
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
    for i in 0..tex.len() {
        let x = (i % 16) as u16;
        let y = (i / 16) as u16;
        tex[i] = 0x8000 | (x << 10) | (y << 5) | ((x ^ y) & 0x1f);
    }
    psx_vram::upload_16bpp(psx_vram::VramRect::new(768, 256, 16, 16), &tex);
    let tpage = Tpage::new(768, 256, TexDepth::Bit15).uv_tpage_word(0);
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
// The player's submit PATH: build an ordering table, DMA it to the GPU
// (linked-list mode), then read back -- exercises the OT + DMA stage.
fn test_gpu_ot_dma_draw() -> TestResult {
    gpu_fill(GPU_SX, GPU_SY, GPU_SW, GPU_SH, 0x0000_0000);
    gpu_draw_env_scratch();
    let mut ot = gpu::ot::OrderingTable::<4>::new();
    ot.clear();
    let mut t0 = prim::TriFlat::new([(4, 4), (90, 8), (4, 90)], 0xff, 0x20, 0x20);
    let mut t1 = prim::TriFlat::new([(90, 90), (90, 8), (8, 90)], 0x20, 0xff, 0x20);
    let mut t2 = prim::TriFlat::new([(40, 24), (72, 64), (20, 72)], 0x20, 0x20, 0xff);
    ot.add(2, &mut t0, prim::TriFlat::WORDS);
    ot.add(2, &mut t1, prim::TriFlat::WORDS);
    ot.add(0, &mut t2, prim::TriFlat::WORDS);
    ot.submit();
    gpu_io::wait_cmd_ready();
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
    Tpage::new(768, 256, TexDepth::Bit15).uv_tpage_word(0)
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
    let mut ot = gpu::ot::OrderingTable::<4>::new();
    ot.clear();
    let mut tri = prim::TriTexturedGouraud::new(
        [(6, 6), (90, 14), (40, 90)],
        [(0, 0), (15, 0), (8, 15)],
        [(0x80, 0x80, 0x80), (0xc0, 0x80, 0x40), (0x40, 0xc0, 0x80)],
        0,
        tpage,
    );
    ot.add(0, &mut tri, prim::TriTexturedGouraud::WORDS);
    ot.submit();
    gpu_io::wait_cmd_ready();
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
    let tpage = Tpage::new(832, 256, TexDepth::Bit8).uv_tpage_word(0);
    let tri = prim::TriTextured::new(
        [(8, 8), (88, 16), (40, 88)],
        [(0, 0), (15, 0), (8, 15)],
        clut.uv_clut_word(),
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
    let mut ot = gpu::ot::OrderingTable::<8>::new();
    ot.clear();
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
    for (i, t) in tris.iter_mut().enumerate() {
        ot.add(i % 7, t, prim::TriFlat::WORDS);
    }
    ot.submit();
    gpu_io::wait_cmd_ready();
    expect_eq(0x91a7_f548, gpu_hash_scratch(), "gpu big ot")
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

// ============================================================
//  2026-06 hardware-accuracy pass -- on-device validation of the
//  SPU-RAM upload fix (the bug that droned/garbled audio on a real
//  console) and the GPU dither + mask-bit faithfulness fixes. These
//  use VRAM / SPU-RAM read-back so the same PASS/FAIL shows up on the
//  emulator and on silicon.
// ============================================================

/// FNV-1a over a slice of 32-bit words (low halfword first), matching
/// [`gpu_hash_scratch`]'s mixing so expected hashes are comparable.
fn fnv32_words(words: &[u32]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &w in words {
        hash = (hash ^ (w & 0xFFFF)).wrapping_mul(0x0100_0193);
        hash = (hash ^ (w >> 16)).wrapping_mul(0x0100_0193);
    }
    hash
}

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
    let block_size: u32 = if words % 16 == 0 {
        16
    } else if words % 8 == 0 {
        8
    } else if words % 4 == 0 {
        4
    } else if words % 2 == 0 {
        2
    } else {
        1
    };
    let _ = spu_dma_read_shape(addr, out, block_size);
}

/// SPU->RAM DMA with an explicit BCR shape. Hardware's unstable read mode
/// corrupts FIFO boundaries, so the precision capture must compare one large
/// block with several small blocks while holding the payload constant.
fn spu_dma_read_shape(addr: u32, out: &mut [u32], block_size: u32) -> u32 {
    use psx_io::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
    let words = out.len() as u32;
    debug_assert!(block_size != 0 && words % block_size == 0);
    let block_count = words / block_size;
    unsafe {
        let spucnt = psx_io::read16(SPUCNT) & !0x0030;
        psx_io::write16(SPUCNT, spucnt);
        // SPUCNT is applied asynchronously. Starting DMA before SPUSTAT
        // reflects Stop returns FIFO/transition garbage even when the memory
        // control delay is configured for stable reads.
        let mut stop_guard = 0u32;
        while psx_io::read16(SPUSTAT) & 0x003F != spucnt & 0x003F && stop_guard < 0xFFFF {
            stop_guard += 1;
        }
        psx_io::write16(TRANSFER_CTRL, 0x0004);
        psx_io::write16(TRANSFER_ADDR, (addr / 8) as u16);
        psx_io::write16(SPUCNT, spucnt | 0x0030); // transfer mode = DMA Read
        let mut mode_guard = 0u32;
        while psx_io::read16(SPUSTAT) & 0x003F != (spucnt | 0x0030) & 0x003F && mode_guard < 0xFFFF
        {
            mode_guard += 1;
        }
        // Do not wait for SPUSTAT's DMA-request bits before arming DMA. The
        // SCPH-9902 capture showed that the low-six mode mirror settles after
        // 24-27 polls, while bits 9/7 remain clear until the DMA side is armed.
        dma::enable_channel(dma::Channel::Spu);
        dma::set_madr(dma::Channel::Spu, out.as_ptr() as u32);
        dma::set_bcr_block(dma::Channel::Spu, block_size as u16, block_count as u16);
        // from-device (no CHCR_TO_DEVICE), block-sync, start.
        dma::set_chcr(dma::Channel::Spu, dma::CHCR_SYNC_BLOCK | dma::CHCR_START);
        // Bounded wait: never spin forever on silicon -- if SPU->RAM DMA
        // stalls the test fails gracefully (zeroed read-back) instead of
        // hanging the whole suite at a black screen.
        let mut guard = 0u32;
        while dma::is_busy(dma::Channel::Spu) && guard < 1_000_000 {
            guard += 1;
        }
        psx_io::write16(SPUCNT, spucnt); // back to Stop
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
    use psx_io::spu::{SPUCNT, TRANSFER_ADDR, TRANSFER_CTRL, TRANSFER_DATA};
    let mut src = [0u16; 8];
    let mut i = 0;
    while i < 8 {
        src[i] = 0xBEEFu16.wrapping_add((i as u16) * 0x101);
        i += 1;
    }
    let dest: u32 = 0x3400;
    unsafe {
        psx_io::write16(TRANSFER_CTRL, 0x0000);
        psx_io::write16(TRANSFER_ADDR, (dest / 8) as u16);
        psx_io::write16(TRANSFER_CTRL, 0x0004);
        let spucnt = psx_io::read16(SPUCNT) & !0x0030;
        psx_io::write16(SPUCNT, spucnt | 0x0010); // transfer mode = Manual Write
        for &hw in src.iter() {
            psx_io::write16(TRANSFER_DATA, hw);
        }
        psx_io::write16(SPUCNT, spucnt); // back to Stop
        psx_io::write16(TRANSFER_CTRL, 0x0000);
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

/// PX6's third QR is a fixed-order microscope for unresolved silicon
/// differences. It intentionally stores raw words rather than more hashes:
/// the host report can then distinguish transfer corruption, delayed status
/// latches, sticky-bit read semantics, and deterministic GTE state hazards.
fn run_precision_scan() -> [u32; PRECISION_VALUE_COUNT] {
    let mut values = [0u32; PRECISION_VALUE_COUNT];
    let mut next = 0usize;

    precision_spu(&mut values, &mut next);
    precision_gpu(&mut values, &mut next);
    precision_timer(&mut values, &mut next);
    precision_remaining(&mut values, &mut next);
    precision_identity_and_raster(&mut values, &mut next);

    debug_assert_eq!(next, PRECISION_VALUE_COUNT);
    values
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
            let word = unsafe { psx_io::read32(base + offset * 4) };
            push_precision(values, next, word);
            offset += 1;
        }
    }
    // GPUSTAT at rest, plus the MDEC status word after a reset. Both identify
    // silicon revision behaviour that timing alone cannot separate.
    push_precision(values, next, gpu_io::gpustat().bits());
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
    for corners in [
        [(8, 8), (88, 16), (40, 88)],
        [(2, 2), (94, 4), (48, 94)],
    ] {
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
    for corners in [[(8, 8), (88, 8), (8, 88), (88, 88)]] {
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
    use psx_io::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL, TRANSFER_DATA};

    const SPU_DELAY: u32 = 0x1F80_1014;
    let original_delay = unsafe { psx_io::read32(SPU_DELAY) };
    let packed_status =
        || unsafe { ((psx_io::read16(SPUCNT) as u32) << 16) | psx_io::read16(SPUSTAT) as u32 };
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
    unsafe { psx_io::write32(SPU_DELAY, stable_delay) };
    spin(64);
    push_precision(values, next, unsafe { psx_io::read32(SPU_DELAY) });
    let mut stable_single = [0u32; 16];
    let _ = spu_dma_read_shape(dest, &mut stable_single, 16);
    push_precision(values, next, fnv32_words(&stable_single));
    let mut stable_four = [0u32; 16];
    let _ = spu_dma_read_shape(dest, &mut stable_four, 4);
    push_precision(values, next, fnv32_words(&stable_four));

    let fifo_dest = 0x3C00;
    unsafe {
        psx_io::write16(TRANSFER_CTRL, 0x0004);
        psx_io::write16(TRANSFER_ADDR, (fifo_dest / 8) as u16);
        let stopped = psx_io::read16(SPUCNT) & !0x0030;
        psx_io::write16(SPUCNT, stopped | 0x0010);
        for index in 0..8u16 {
            psx_io::write16(TRANSFER_DATA, 0xBEEFu16.wrapping_add(index * 0x101));
        }
        psx_io::write16(SPUCNT, stopped);
    }
    spin(4000);
    let mut fifo_read = [0u32; 4];
    spu_dma_read(fifo_dest, &mut fifo_read);
    for word in fifo_read {
        push_precision(values, next, word);
    }
    unsafe { psx_io::write32(SPU_DELAY, original_delay) };
}

/// Values 43..60: raw GPUSTAT transitions. Three reads after each GP1 DMA
/// direction write expose whether the D0-vs-E4 result is a delayed latch;
/// the IRQ reads retain the command-FIFO set/ack transition shape.
fn precision_gpu(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    gpu_io::write_gp1(0x0200_0000);
    push_precision(values, next, gpu_io::gpustat().bits());
    gpu_io::write_gp0(0x1F00_0000);
    for _ in 0..3 {
        push_precision(values, next, gpu_io::gpustat().bits());
    }
    gpu_io::write_gp1(0x0200_0000);
    for _ in 0..2 {
        push_precision(values, next, gpu_io::gpustat().bits());
    }
    for dir in 0..4u32 {
        gpu_io::write_gp1(0x0400_0000 | dir);
        for _ in 0..3 {
            push_precision(values, next, gpu_io::gpustat().bits());
        }
    }
    gpu_io::write_gp1(0x0400_0002);
}

/// Values 61..72: exact Timer 2 state before and after target/FFFF events.
/// The two consecutive mode reads expose the read-to-clear flags. I_STAT is
/// cleared before each half and masked to its eleven implemented source bits,
/// avoiding a stale GPU IRQ and the open-bus upper half seen in the prior run.
fn precision_timer(values: &mut [u32; PRECISION_VALUE_COUNT], next: &mut usize) {
    const IRQ_SOURCE_MASK: u32 = 0x07FF;
    irq::ack(IRQ_SOURCE_MASK);
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
    push_precision(values, next, irq::stat() & IRQ_SOURCE_MASK);

    irq::ack(IRQ_SOURCE_MASK);
    timers::set_mode(timers::Timer::Timer2, TIMER_MODE_IRQ_ON_WRAP);
    timers::set_counter(timers::Timer::Timer2, 0xFFF0);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::counter(timers::Timer::Timer2) as u32);
    spin(8192);
    push_precision(values, next, timers::counter(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, timers::mode(timers::Timer::Timer2) as u32);
    push_precision(values, next, irq::stat() & IRQ_SOURCE_MASK);
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

        ctc2!(31, 0);
        mtc2!(12, S0);
        mtc2!(13, S1);
        mtc2!(14, S2);
        unsafe { gte_ops::nclip() };
        gte_nops!(64);
        let _ = mfc2!(24);

        // Poison MAC0 with the reverse winding, then restore scene A using
        // the exact controlled-prestate sequence from cases 126..134.
        mtc2!(13, S2);
        mtc2!(14, S1);
        unsafe { gte_ops::nclip() };
        gte_nops!(64);
        mtc2!(13, S1);
        mtc2!(14, S2);
        unsafe { gte_ops::nclip() };
        gte_nops!($gap);
        mfc2!(24)
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
    push_precision(values, next, mfc2!(25));
    push_precision(values, next, mfc2!(26));
    push_precision(values, next, mfc2!(27));
    run_op_full_seed_settled();
    push_precision(values, next, mfc2!(25));
    push_precision(values, next, mfc2!(26));
    push_precision(values, next, mfc2!(27));

    // Voice 0 raw masks: case 164 says which offsets differ, but not what the
    // masked values are. Preserve all eight readbacks after writing FFFF.
    let voice0 = psx_io::spu::SPU_BASE;
    unsafe {
        for index in 0..8u32 {
            let addr = voice0 + index * 2;
            let old = psx_io::read16(addr);
            psx_io::write16(addr, 0xFFFF);
            push_precision(values, next, psx_io::read16(addr) as u32);
            psx_io::write16(addr, old);
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
        dma::enable_channel(dma::Channel::Otc);
        dma::set_madr(dma::Channel::Otc, ptr.add(15) as u32);
        dma::set_bcr_manual(dma::Channel::Otc, 16);
        push_precision(values, next, dma::chcr(dma::Channel::Otc));
        dma::set_chcr(
            dma::Channel::Otc,
            dma::CHCR_STEP_BACKWARD | dma::CHCR_SYNC_MANUAL | dma::CHCR_START | dma::CHCR_TRIGGER,
        );
        for _ in 0..6 {
            push_precision(values, next, dma::chcr(dma::Channel::Otc));
        }
        let mut guard = 0u32;
        while dma::is_busy(dma::Channel::Otc) && guard < 0xFFFF {
            guard += 1;
        }
        push_precision(values, next, dma::madr(dma::Channel::Otc));
        push_precision(values, next, psx_io::read32(dma::Channel::Otc.base() + 4));
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
        let _ = mfc2!(19);
        push_precision(
            values,
            next,
            scene_nclip_mac0(0x006e_0095, 0xffe2_0094, 0xffde_00dc),
        );
    }
    scene_rtpt(RTPT_E);
    let _ = mfc2!(19);
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
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xC000_0000);
    gpu_io::write_gp0(((vy as u32) << 16) | vx as u32);
    gpu_io::write_gp0((1u32 << 16) | 2); // 2 wide, 1 tall
    let mut guard = 0u32;
    while gpu_io::gpustat().bits() & (1 << 27) == 0 && guard < 100_000 {
        guard += 1;
    }
    gpu_io::gpuread()
}

/// CPU->VRAM block transfer (GP0 0xA0). Honours the current GP0 0xE6 mask
/// state on silicon (and now in the emulator).
fn gpu_cpu_to_vram(vx: u16, vy: u16, w: u16, h: u16, data: &[u32]) {
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xA000_0000);
    gpu_io::write_gp0(((vy as u32) << 16) | vx as u32);
    gpu_io::write_gp0(((h as u32) << 16) | w as u32);
    for &d in data {
        gpu_io::write_gp0(d);
    }
    gpu_io::wait_cmd_ready();
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
    gpu_io::write_gp0(0xE100_0000 | (1 << 9)); // draw mode: dither ON
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
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xE100_0000); // dither OFF (restore default)
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
    gpu_io::write_gp0(0xE600_0000 | 1); // set-mask on, check-mask off
    gpu_cpu_to_vram(px, py, 2, 1, &[a | (a << 16)]);
    gpu_io::write_gp0(0xE600_0000 | 2); // check-mask on
    gpu_cpu_to_vram(px, py, 2, 1, &[b | (b << 16)]);
    gpu_io::write_gp0(0xE600_0000); // restore
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
    let raw = psx_pad::poll_port1_diag(psx_pad::DEFAULT_SETUP_SPINS, 0);
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
    let raw = psx_pad::poll_port1_diag(2048, 2048);
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
    let raw = psx_pad::poll_port1_diag(psx_pad::DEFAULT_SETUP_SPINS, 0);
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
    // The helper is bounded now, so a wedged OTC channel reports instead
    // of hanging the battery; bit 2 carries whether it completed.
    let cleared = dma::clear_ordering_table((&raw mut OT) as *mut u32, 4);
    gpu_io::write_gp1(0x0400_0000 | 2);
    let observed = ((gpu_io::gpustat().bits() >> 29) & 0b11) | ((cleared as u32) << 2);
    expect_eq(0b110, observed, "dma dir | otc done")
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
// before the Timer 2 counter is cleared. The harmless SLL-to-zero words bracket
// each block so tools/verify-hwtest-machine-code.py can audit the final PS-X EXE
// (make hwtest-verify-code; spans are pinned in docs/hardware-refs/).
#[inline(never)]
fn timed_empty() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x00000040", // probe 01 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)", // timing starts after this counter reset
            "lw $12, 0($11)",
            "nop", // resolve the R3000A load delay before exposing the result
            ".word 0x00000440", // probe 01 end marker
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
            ".word 0x00000640", // probe 25 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "lw $9, 0($8)",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x00000A40", // probe 25 end marker
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
fn timed_nops() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x00000080", // probe 02 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 128",
            "nop",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x00000480", // probe 02 end marker
            ".set reorder",
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

#[inline(never)]
fn timed_dependent_alu() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x000000C0", // probe 03 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 128",
            "addiu $8, $8, 1",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x000004C0", // probe 03 end marker
            ".set reorder",
            lateout("$8") _,
            lateout("$11") _,
            lateout("$12") elapsed,
            options(nostack)
        );
    }
    elapsed as u16
}

fn timed_load_hazards() -> u16 {
    timed_load_hazards_at((&raw const SPIN_SINK) as u32)
}

#[inline(never)]
fn timed_load_hazards_at(address: u32) -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x00000100", // probe 04 start marker
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
            ".word 0x00000500", // probe 04 end marker
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
            ".word 0x00000500", // probe 20 start marker
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
            ".word 0x00000900", // probe 20 end marker
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
            ".word 0x00000540", // probe 21 start marker
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
            ".word 0x00000940", // probe 21 end marker
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
            ".word 0x00000600", // probe 24 start marker
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
            ".word 0x00000A00", // probe 24 end marker
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
            ".word 0x00000140", // probe 05 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            "sw $zero, 0($8)",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x00000540", // probe 05 end marker
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
            ".word 0x00000580", // probe 22 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0xA1000000", // sb $zero,0($8)
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x00000980", // probe 22 end marker
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
            ".word 0x000005C0", // probe 23 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            ".word 0xA5000000", // sh $zero,0($8)
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x000009C0", // probe 23 end marker
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
fn timed_taken_branches() -> u16 {
    let elapsed: u32;
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            ".word 0x00000180", // probe 06 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            ".rept 64",
            "beq $zero, $zero, 1f",
            "nop",
            "1:",
            ".endr",
            "lw $12, 0($11)",
            "nop",
            ".word 0x00000580", // probe 06 end marker
            ".set reorder",
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
            ".word 0x000004C0", // probe 19 start marker
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
            ".word 0x000008C0", // probe 19 end marker
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
            ".word 0x000001C0", // probe 07 start marker
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
            ".word 0x000005C0", // probe 07 end marker
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
            ".word 0x00000200", // probe 08 start marker
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
            ".word 0x00000600", // probe 08 end marker
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
    psx_rt::cache::flush_i_cache();
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
            ".word 0x000003C0", // probe 15 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x000007C0", // probe 15 end marker
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
            ".word 0x00000400", // probe 16 start marker
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
            ".word 0x00000021", // addu $zero,$zero,$zero end marker
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
            ".word 0x00000440", // probe 17 start marker
            "lui $11, 0x1F80",
            "ori $11, $11, 0x1120",
            "sw $zero, 4($11)",
            "sw $zero, 0($11)",
            "jalr $10, $8",
            "nop",
            "lw $12, 0($11)",
            "nop",
            ".word 0x00000840", // probe 17 end marker
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
            ".word 0x00000480", // probe 18 start marker
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
            ".word 0x00000880", // probe 18 end marker
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
    ($name:ident, $count:literal, $instruction:literal, $start:literal, $end:literal) => {
        #[inline(never)]
        fn $name() -> u16 {
            seed_gte_state();
            let elapsed: u32;
            unsafe {
                core::arch::asm!(
                    concat!(
                        ".set noreorder\n",
                        ".word ", stringify!($start), "\n",
                        "lui $11, 0x1F80\n",
                        "ori $11, $11, 0x1120\n",
                        "sw $zero, 4($11)\n",
                        "sw $zero, 0($11)\n",
                        ".rept ", stringify!($count), "\n",
                        ".word ", stringify!($instruction), "\n",
                        ".endr\n",
                        "lw $12, 0($11)\n",
                        "nop\n",
                        ".word ", stringify!($end), "\n",
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

timed_gte_commands!(
    timed_gte_rtps_commands,
    16,
    0x4A080001,
    0x00000240,
    0x00000640
);
timed_gte_commands!(
    timed_gte_rtpt_commands,
    8,
    0x4A080030,
    0x00000280,
    0x00000680
);
timed_gte_commands!(
    timed_gte_nclip_commands,
    16,
    0x4A000006,
    0x000002C0,
    0x000006C0
);
timed_gte_commands!(
    timed_gte_mvmva_commands,
    16,
    0x4A080012,
    0x00000300,
    0x00000700
);
timed_gte_commands!(
    timed_gte_ncdt_commands,
    4,
    0x4A080016,
    0x00000340,
    0x00000740
);
timed_gte_commands!(
    timed_gte_ncct_commands,
    4,
    0x4A08003F,
    0x00000380,
    0x00000780
);

fn timed_otc_dma_cycles(words: u16) -> u16 {
    static mut OT: [u32; 256] = [0; 256];
    unsafe {
        let ptr = (&raw mut OT) as *mut u32;
        for i in 0..words as usize {
            ptr::write_volatile(ptr.add(i), 0);
        }
        dma::enable_channel(dma::Channel::Otc);
        dma::set_madr(dma::Channel::Otc, ptr.add(words as usize - 1) as u32);
        dma::set_bcr_manual(dma::Channel::Otc, words);
        timers::set_mode(timers::Timer::Timer2, 0);
        timers::set_counter(timers::Timer::Timer2, 0);
        dma::set_chcr(
            dma::Channel::Otc,
            dma::CHCR_STEP_BACKWARD | dma::CHCR_SYNC_MANUAL | dma::CHCR_START | dma::CHCR_TRIGGER,
        );
        let mut polls = 0u16;
        while dma::is_busy(dma::Channel::Otc) && polls != 0xFFFF {
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
    use psx_io::spu::{SPUCNT, TRANSFER_ADDR, TRANSFER_CTRL};
    static mut SOURCE: [u32; 256] = [0; 256];

    unsafe {
        let source = &raw mut SOURCE as *mut u32;
        let mut index = 0usize;
        while index < 256 {
            ptr::write_volatile(source.add(index), 0x5A00_0000 | index as u32);
            index += 1;
        }

        let old_spucnt = psx_io::read16(SPUCNT);
        let stopped = old_spucnt & !0x0030;
        psx_io::write16(SPUCNT, stopped);
        psx_io::write16(TRANSFER_CTRL, 0x0004);
        psx_io::write16(TRANSFER_ADDR, 0x0800); // SPU RAM byte address 0x4000
        psx_io::write16(SPUCNT, stopped | 0x0020); // DMA Write

        dma::enable_channel(dma::Channel::Spu);
        dma::set_madr(dma::Channel::Spu, source as u32);
        dma::set_bcr_block(dma::Channel::Spu, 16, 16);

        timers::set_mode(timers::Timer::Timer2, 0);
        timers::set_counter(timers::Timer::Timer2, 0);
        dma::set_chcr(
            dma::Channel::Spu,
            dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_START,
        );

        let mut polls = 0u32;
        while dma::is_busy(dma::Channel::Spu) && polls < 1_000_000 {
            polls += 1;
        }
        let elapsed = timers::counter(timers::Timer::Timer2);
        psx_io::write16(SPUCNT, old_spucnt);
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

    let old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
    gpu_io::write_gp1(0x0400_0002); // DMA CPU -> GP0
    dma::enable_channel(dma::Channel::Gpu);
    dma::set_madr(dma::Channel::Gpu, SOURCE.as_ptr() as u32);
    dma::set_bcr_block(dma::Channel::Gpu, block_size, block_count);

    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    dma::set_chcr(
        dma::Channel::Gpu,
        dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_START,
    );

    let mut polls = 0u32;
    while dma::is_busy(dma::Channel::Gpu) && polls < 1_000_000 {
        polls += 1;
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    gpu_io::write_gp1(0x0400_0000 | old_direction);
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

        let old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
        gpu_io::write_gp1(0x0400_0002); // DMA CPU -> GP0
        dma::enable_channel(dma::Channel::Gpu);
        dma::set_madr(dma::Channel::Gpu, list as u32);
        dma::set_bcr_manual(dma::Channel::Gpu, 0);

        timers::set_mode(timers::Timer::Timer2, 0);
        timers::set_counter(timers::Timer::Timer2, 0);
        dma::set_chcr(
            dma::Channel::Gpu,
            dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_LINKED | dma::CHCR_START,
        );

        let mut polls = 0u32;
        while dma::is_busy(dma::Channel::Gpu) && polls < 1_000_000 {
            polls += 1;
        }
        let elapsed = timers::counter(timers::Timer::Timer2);
        gpu_io::write_gp1(0x0400_0000 | old_direction);
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
    gpu_io::wait_cmd_ready();
    gpu_io::write_gp0(0xE300_0000); // draw area top-left = (0, 0)
    gpu_io::write_gp0(0xE400_0000 | 1023 | (511 << 10));
    gpu_io::write_gp0(0xE500_0000); // draw offset = (0, 0)
    gpu_io::write_gp0(0xE100_0000); // dither off
    gpu_io::wait_cmd_ready();

    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let mut index = 0u16;
    while index < count {
        let y = 384u32 + u32::from(index & 63);
        let x0 = 640u32;
        let x1 = x0 + u32::from(length);
        if shaded {
            gpu_io::write_gp0(0x5000_00FF); // red endpoint
            gpu_io::write_gp0((y << 16) | x0);
            gpu_io::write_gp0(0x00FF_0000); // blue endpoint
            gpu_io::write_gp0((y << 16) | x1);
        } else {
            gpu_io::write_gp0(0x4000_FFFF);
            gpu_io::write_gp0((y << 16) | x0);
            gpu_io::write_gp0((y << 16) | x1);
        }
        index += 1;
    }

    let mut guard = 0u32;
    while gpu_io::gpustat().bits() & (1 << 26) == 0 && guard < 1_000_000 {
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
    let response = cdrom::try_get_stat(200_000);
    let elapsed = timers::counter(timers::Timer::Timer2);
    if response.is_some_and(|value| !value.is_empty()) {
        elapsed
    } else {
        0xFFFF
    }
}

fn timed_gpu_irq_settle() -> u16 {
    gpu_io::write_gp1(0x0200_0000);
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    gpu_io::write_gp0(0x1F00_0000);
    let mut guard = 0u16;
    while gpu_io::gpustat().bits() & (1 << 24) == 0 && guard != 0xFFFF {
        guard = guard.wrapping_add(1);
    }
    let elapsed = timers::counter(timers::Timer::Timer2);
    gpu_io::write_gp1(0x0200_0000);
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

    /// The 8 hex nibbles without the `0x` prefix. The OBS column lives at
    /// x=248 on a 320px screen; `0x` + 8 nibbles = 80px overruns the right
    /// edge and the low nibble is clipped off-screen. The bare 8 nibbles
    /// (64px) fit, so the full hardware value is readable on a real TV.
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
