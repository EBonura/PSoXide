// SPDX-License-Identifier: GPL-2.0-or-later
//! v1.26 MDEC SETUP DIAGNOSTIC: runs at the start of FMV STREAM TEST, before
//! the movie.
//!
//! v1.25's FMV test showed a red screen on a PAL SCPH-9002: the SDK player's
//! DMA0 upload of the MDEC tables never finished ("mdec tables"), while the
//! same disc played on PSoXide and on a SuperStation One. That console read
//! the MDEC status straight after a reset as 0x6401_0000 (busy, data-in full:
//! the state from before the reset) where the FPGA read the reset state. The
//! lead is that the player's DMA-request enable, written right behind the
//! reset, was lost to a reset still in progress. This battery tests that and
//! the alternatives in one burn, then plays the movie behind the first
//! sequence that worked.
//!
//! Sequences, each from its own reset:
//!
//! - A: the v1.25 driver (SDK c756ec1d) verbatim: reset, enable, DPCR, then
//!   DMA0 the tables. No status reads inside it, so its timing is the
//!   player's. If a table DMA times out it then writes the enable again and
//!   waits once more: a DMA that finishes after that proves the first enable
//!   was lost.
//! - B: reset, poll until not busy (clocks recorded), enable, then after the
//!   table command wait for the data-in request (status bit 28, clocks
//!   recorded) before DMA0.
//! - C: reset, a fixed 20,000-clock delay, enable, DMA0.
//! - D: reset, settle, both tables over CPU writes, then enable, settle and
//!   check bit 28; only the probe decode uses DMA.
//! - E: PSn00bSDK's order (DecDCTReset + DecDCTPutEnv, libpsn00b/psxpress):
//!   DPCR first, channels stopped, reset and enable back to back, then wait
//!   for not-busy before each command and after each DMA.
//! - F: the SDK driver on this build (psx_fmv::mdec::reset + load_tables),
//!   which settles before enabling, checks bit 28 and falls back to CPU
//!   writes.
//!
//! Every sequence is followed by the same probe decode: one DC-only colour
//! macroblock in over DMA0, 128 words of 15bpp out over DMA1. A run WORKS
//! when its tables went in and the probe came back whole. Each sequence runs
//! eight times: runs 1, 3, 5 and 7 start from an idle MDEC, runs 2, 4, 6 and
//! 8 reset it in the middle of a table command. Run 1 of A instead starts from whatever state
//! the console was in, which from a fresh boot is the condition that failed.
//! Interrupts are masked for each run so a VBlank cannot stretch a race.
//!
//! Results go on screen (a summary page and one page per sequence) and into
//! the capture as timing-block records 0x200-0x25B plus 0x260; see
//! [`records`] for the layout and tools/hwtest-report.py for the decoder.

use crate::{TimingRecord, TIMING_RECORD_COUNT, TIMING_RECORD_UNUSED};
use core::ptr::addr_of_mut;
use psx_engine::button;
use psx_fmv::mdec;
use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_io::dma::{self, Channel};
use psx_io::{irq, timers};
use psx_rt::{interrupts, tty};

const MDEC0: u32 = 0x1F80_1820;
const MDEC1: u32 = 0x1F80_1824;
const RESET: u32 = mdec::CONTROL_RESET;
const ENABLE: u32 = mdec::CONTROL_ENABLE_DMA;
const BUSY: u32 = mdec::STATUS_BUSY;
const IN_REQUEST: u32 = mdec::STATUS_IN_REQUEST;
const IN_FULL: u32 = mdec::STATUS_IN_FULL;
const SET_QUANT: u32 = mdec::COMMAND_SET_QUANT;
const SET_SCALE: u32 = mdec::COMMAND_SET_SCALE;

/// Headless test build only (`--features diag-fault`): fakes a table DMA
/// timeout in A, a probe DMA1 timeout in E and a setup failure behind C's
/// playback, so the failure paths PSoXide never takes can be looked at.
/// Never on a burned disc.
const FAULT: bool = cfg!(feature = "diag-fault");

/// DMA0 and DMA1 CHCR as every MDEC player writes them.
const CHCR_IN: u32 = 0x0100_0201;
const CHCR_OUT: u32 = 0x0100_0200;
/// Completion budget for one DMA: short, since a 32-word table or one
/// macroblock moves in microseconds, but still many times what it needs.
const DMA_SPINS: u32 = 100_000;
/// Status polls give up after this many system clocks (about 1.8 ms), well
/// short of Timer 2 wrapping.
const POLL_CLOCKS: u16 = 60_000;
/// Poll result for "never happened".
pub(crate) const NEVER: u16 = 0xFFFF;
/// C's fixed delay between reset and enable, in system clocks (~0.6 ms).
const FIXED_DELAY: u16 = 20_000;
/// Idle time after the reset that prepares a run, in system clocks.
const PREP_DELAY: u16 = 2_000;
/// Table words written before an odd run's reset, leaving the MDEC busy.
const PREP_WORDS: usize = 5;

pub(crate) const VARIANTS: usize = 6;
pub(crate) const RUNS: usize = 8;
const LETTERS: [&str; VARIANTS] = ["A", "B", "C", "D", "E", "F"];
const NAMES: [&str; VARIANTS] = [
    "CONTROL V1.25",
    "SETTLE+ENABLE",
    "FIXED DELAY",
    "CPU TABLES",
    "PSN00BSDK",
    "SDK DRIVER",
];
/// The index of F, the SDK driver: the movie prefers it when it held.
const SDK: usize = 5;

/// Where a run stopped. 0 is none.
pub(crate) const STEP_SETTLE: u8 = 1;
pub(crate) const STEP_QUANT: u8 = 2;
pub(crate) const STEP_SCALE: u8 = 3;
pub(crate) const STEP_IDLE: u8 = 4;
pub(crate) const STEP_PROBE_IN: u8 = 5;
pub(crate) const STEP_PROBE_OUT: u8 = 6;
const STEP_NAMES: [&str; 7] = [
    "-",
    "RESET",
    "QUANT",
    "SCALE",
    "IDLE",
    "PROBE IN",
    "PROBE OUT",
];

/// Status snapshot slots.
const SNAP_BEFORE: usize = 0;
const SNAP_RESET: usize = 1;
const SNAP_ENABLE: usize = 2;
const SNAP_COMMAND: usize = 3;
const SNAP_TABLES: usize = 4;
const SNAP_PROBE: usize = 5;
const SNAPS: usize = 6;
const SNAP_NAMES: [&str; SNAPS] = [
    "BEFORE RESET ",
    "AFTER RESET  ",
    "AFTER ENABLE ",
    "AFTER COMMAND",
    "AFTER TABLES ",
    "AFTER PROBE  ",
];

/// One run of one sequence.
#[derive(Copy, Clone)]
pub(crate) struct Run {
    /// First step that failed, 0 if none.
    fail: u8,
    /// MDEC1 status at each snapshot point; `taken` says which were read.
    snaps: [u32; SNAPS],
    taken: u8,
    /// Clocks from the reset write to busy dropping (B, D, E), or NEVER.
    settle: u16,
    /// Clocks from the first table command to status bit 28 (B, C), or from
    /// the enable to bit 28 (D, with the MDEC idle), or NEVER.
    dreq: u16,
    /// Clocks from the probe's decode command to bit 28, or NEVER.
    probe_dreq: u16,
    /// F only: enable writes, CPU uploads in bits 8+, bit 15 = reset settled.
    extra: u16,
    /// At the first DMA timeout: that channel's CHCR, BCR, MADR, then DPCR,
    /// DICR, and the MADR the kick started from (words moved = difference).
    timeout: [u32; 6],
    timed_out: bool,
    /// A only: 1 when writing the enable again finished the stuck DMA, 2
    /// when it did not; with the status right after.
    rescue: u8,
    rescue_status: u32,
    /// Probe output words that arrived (of 128), whether they were all
    /// equal, and the first.
    probe_words: u16,
    probe_flat: bool,
    probe_word: u32,
}

impl Run {
    const fn new() -> Self {
        Run {
            fail: 0,
            snaps: [0; SNAPS],
            taken: 0,
            settle: NEVER,
            dreq: NEVER,
            probe_dreq: NEVER,
            extra: 0,
            timeout: [0; 6],
            timed_out: false,
            rescue: 0,
            rescue_status: 0,
            probe_words: 0,
            probe_flat: false,
            probe_word: 0,
        }
    }
}

/// One sequence's result over its runs.
#[derive(Copy, Clone)]
pub(crate) struct Variant {
    /// Bit n set when run n+1 worked.
    pass_mask: u8,
    /// Each run's failing step, four bits per run, run 1 lowest.
    run_fails: u32,
    /// The first run that failed, or run 1 when none did.
    detail: Run,
    detail_run: u8,
}

#[derive(Copy, Clone)]
pub(crate) struct Diag {
    variants: [Variant; VARIANTS],
    /// The sequence the movie plays behind first, if any worked.
    pub(crate) chosen: Option<usize>,
    /// How many times the battery has run since boot.
    batteries: u8,
    /// MDEC1 over time after a reset: from idle, from busy, and from busy
    /// with the enable written straight after.
    latency: [Trace; TRACES],
    /// The one-frame CPU-versus-DMA control decode.
    control: hello_fmv::FrameControl,
    control_ran: bool,
    /// The short playback behind each sequence, once FMV STREAM TEST ran.
    pub(crate) plays: [Option<hello_fmv::Outcome>; VARIANTS],
}

/// MDEC1 samples after one reset: the first read, then every change, with
/// Timer 2 clocks since just before the reset write.
#[derive(Copy, Clone)]
struct Trace {
    count: u8,
    clocks: [u16; TRACE_SAMPLES],
    status: [u32; TRACE_SAMPLES],
}

const TRACES: usize = 3;
const TRACE_SAMPLES: usize = 4;
/// How long a trace watches, in system clocks.
const TRACE_CLOCKS: u16 = 5_000;
const TRACE_NAMES: [&str; TRACES] = ["FROM IDLE", "FROM BUSY", "BUSY+ENABLE"];

/// Reset (from idle, or from `busy`), optionally enable straight after, then
/// watch MDEC1 change.
fn trace(font: &FontAtlas, busy: bool, enable: bool) -> Trace {
    let mask = irq::mask();
    irq::set_mask(0);
    abort_both();
    prepare(busy);
    let mut t = Trace {
        count: 0,
        clocks: [0; TRACE_SAMPLES],
        status: [0; TRACE_SAMPLES],
    };
    timers::set_counter(timers::Timer::Timer2, 0);
    ctl(RESET);
    if enable {
        ctl(ENABLE);
    }
    let mut last = st();
    t.clocks[0] = timers::counter(timers::Timer::Timer2);
    t.status[0] = last;
    t.count = 1;
    loop {
        let now = st();
        let clock = timers::counter(timers::Timer::Timer2);
        if now != last {
            last = now;
            let i = t.count as usize;
            t.clocks[i] = clock;
            t.status[i] = now;
            t.count += 1;
            if t.count as usize == TRACE_SAMPLES {
                break;
            }
        }
        if clock >= TRACE_CLOCKS {
            break;
        }
    }
    irq::set_mask(mask);
    let _ = font;
    t
}

// --- Register access -------------------------------------------------------

#[inline(always)]
fn st() -> u32 {
    // SAFETY: MDEC status read.
    unsafe { psx_io::read32(MDEC1) }
}

#[inline(always)]
fn ctl(value: u32) {
    // SAFETY: MDEC control write.
    unsafe { psx_io::write32(MDEC1, value) }
}

#[inline(always)]
fn cmd(value: u32) {
    // SAFETY: MDEC command/parameter write.
    unsafe { psx_io::write32(MDEC0, value) }
}

fn snap(run: &mut Run, slot: usize) {
    run.snaps[slot] = st();
    run.taken |= 1 << slot;
}

/// Clocks until `status & mask == want`, counted from this call, or NEVER.
/// Timer 2 must be on the system clock (mode 0).
fn poll(mask: u32, want: u32) -> u16 {
    timers::set_counter(timers::Timer::Timer2, 0);
    loop {
        if st() & mask == want {
            return timers::counter(timers::Timer::Timer2);
        }
        if timers::counter(timers::Timer::Timer2) >= POLL_CLOCKS {
            return NEVER;
        }
    }
}

fn delay(clocks: u16) {
    timers::set_counter(timers::Timer::Timer2, 0);
    while timers::counter(timers::Timer::Timer2) < clocks {}
}

fn bcr(ch: Channel) -> u32 {
    // SAFETY: DMA register read.
    unsafe { psx_io::read32(ch.base() + 4) }
}

/// MADR of the last kick on each MDEC channel, for words-moved at a timeout.
static mut KICK_MADR: [u32; 2] = [0; 2];

fn record_timeout(run: &mut Run, ch: Channel) {
    if run.timed_out {
        return;
    }
    run.timed_out = true;
    // SAFETY: DMA register reads; KICK_MADR is single-threaded.
    run.timeout = unsafe {
        [
            dma::chcr(ch),
            bcr(ch),
            dma::madr(ch),
            psx_io::read32(dma::DPCR),
            psx_io::read32(dma::DICR),
            (*addr_of_mut!(KICK_MADR))[ch as usize & 1],
        ]
    };
}

/// Kick DMA0 with `count` words from `words`, aborting the channel first as
/// psx-fmv does.
fn dma_in(words: *const u32, count: usize) {
    dma::abort(Channel::MdecIn);
    dma_in_raw(words, count);
}

/// Kick DMA0 without the abort, as PSn00bSDK does.
fn dma_in_raw(words: *const u32, count: usize) {
    // SAFETY: single-threaded bookkeeping.
    unsafe { (*addr_of_mut!(KICK_MADR))[0] = words as u32 };
    dma::set_madr(Channel::MdecIn, words as u32);
    dma::set_bcr_block(Channel::MdecIn, 32, (count / 32) as u16);
    dma::set_chcr(Channel::MdecIn, CHCR_IN);
}

fn dma_done(ch: Channel) -> bool {
    dma::wait_done(ch, DMA_SPINS)
}

fn enable_channels() {
    dma::enable_channel(Channel::MdecIn);
    dma::enable_channel(Channel::MdecOut);
}

/// Command then its 32 words over the CPU, polling data-in full before each.
fn cpu_upload(command: u32, words: &[u32; 32]) -> bool {
    cmd(command);
    for &word in words {
        if poll(IN_FULL, 0) == NEVER {
            return false;
        }
        cmd(word);
    }
    true
}

// --- The sequences ---------------------------------------------------------

/// A: psx-fmv's reset() and load_tables() as of SDK c756ec1d.
fn variant_a(run: &mut Run) -> bool {
    ctl(RESET);
    ctl(ENABLE);
    enable_channels();
    let tables: [(u8, u32, &[u32; 32]); 2] = [
        (STEP_QUANT, SET_QUANT, &mdec::QUANT_WORDS),
        (STEP_SCALE, SET_SCALE, &mdec::SCALE_WORDS),
    ];
    for (step, command, words) in tables {
        cmd(command);
        dma_in(words.as_ptr(), 32);
        if (FAULT && step == STEP_QUANT) || !dma_done(Channel::MdecIn) {
            record_timeout(run, Channel::MdecIn);
            run.fail = step;
            snap(run, SNAP_TABLES);
            // Does an enable written now, long after the reset, free it?
            ctl(ENABLE);
            run.rescue = if dma_done(Channel::MdecIn) { 1 } else { 2 };
            run.rescue_status = st();
            dma::abort(Channel::MdecIn);
            return false;
        }
    }
    snap(run, SNAP_TABLES);
    true
}

/// Both tables over DMA0 after the first command's request check. Shared by
/// B and C, which differ only in how they get from reset to enable.
fn dma_tables_checked(run: &mut Run) -> bool {
    cmd(SET_QUANT);
    run.dreq = poll(IN_REQUEST, IN_REQUEST);
    snap(run, SNAP_COMMAND);
    dma_in(mdec::QUANT_WORDS.as_ptr(), 32);
    if !dma_done(Channel::MdecIn) {
        record_timeout(run, Channel::MdecIn);
        run.fail = STEP_QUANT;
        snap(run, SNAP_TABLES);
        return false;
    }
    cmd(SET_SCALE);
    dma_in(mdec::SCALE_WORDS.as_ptr(), 32);
    if !dma_done(Channel::MdecIn) {
        record_timeout(run, Channel::MdecIn);
        run.fail = STEP_SCALE;
        snap(run, SNAP_TABLES);
        return false;
    }
    snap(run, SNAP_TABLES);
    true
}

/// B: settle on not-busy, then enable.
fn variant_b(run: &mut Run) -> bool {
    enable_channels();
    ctl(RESET);
    run.settle = poll(BUSY, 0);
    snap(run, SNAP_RESET);
    if run.settle == NEVER {
        run.fail = STEP_SETTLE;
        return false;
    }
    ctl(ENABLE);
    snap(run, SNAP_ENABLE);
    dma_tables_checked(run)
}

/// C: a fixed delay, then enable.
fn variant_c(run: &mut Run) -> bool {
    enable_channels();
    ctl(RESET);
    delay(FIXED_DELAY);
    snap(run, SNAP_RESET);
    ctl(ENABLE);
    snap(run, SNAP_ENABLE);
    dma_tables_checked(run)
}

/// D: tables over the CPU, enable afterwards.
fn variant_d(run: &mut Run) -> bool {
    enable_channels();
    ctl(RESET);
    run.settle = poll(BUSY, 0);
    snap(run, SNAP_RESET);
    if run.settle == NEVER {
        run.fail = STEP_SETTLE;
        return false;
    }
    if !cpu_upload(SET_QUANT, &mdec::QUANT_WORDS) {
        run.fail = STEP_QUANT;
        snap(run, SNAP_TABLES);
        return false;
    }
    if !cpu_upload(SET_SCALE, &mdec::SCALE_WORDS) {
        run.fail = STEP_SCALE;
        snap(run, SNAP_TABLES);
        return false;
    }
    if poll(BUSY, 0) == NEVER {
        run.fail = STEP_IDLE;
        snap(run, SNAP_TABLES);
        return false;
    }
    snap(run, SNAP_TABLES);
    ctl(ENABLE);
    // Idle: an MDEC that only raises the request during a command (PSoXide)
    // times out here, which is not a failure.
    run.dreq = poll(IN_REQUEST, IN_REQUEST);
    snap(run, SNAP_ENABLE);
    true
}

/// E: PSn00bSDK's DecDCTReset(0) and DecDCTPutEnv(0, 0).
fn variant_e(run: &mut Run) -> bool {
    // SetDMAPriority(DMA_MDEC_IN/OUT, 3): priority 3 and the enable bit.
    // SAFETY: DPCR read-modify-write, then channel stops, as the library.
    unsafe {
        let dpcr = psx_io::read32(dma::DPCR);
        psx_io::write32(dma::DPCR, (dpcr & !0xFF) | 0xBB);
    }
    dma::set_chcr(Channel::MdecIn, 0x0000_0201);
    dma::set_chcr(Channel::MdecOut, 0x0000_0200);
    ctl(RESET);
    ctl(ENABLE);
    // DecDCTinSync before each command and after each DMA. The library
    // carries on after a timeout; so does this, and the next wait decides.
    run.settle = poll(BUSY, 0);
    let tables: [(u8, u32, &[u32; 32]); 2] = [
        (STEP_SCALE, SET_SCALE, &mdec::SCALE_WORDS),
        (STEP_QUANT, SET_QUANT, &mdec::QUANT_WORDS),
    ];
    for (step, command, words) in tables {
        cmd(command);
        dma_in_raw(words.as_ptr(), 32);
        if poll(BUSY, 0) == NEVER {
            record_timeout(run, Channel::MdecIn);
            run.fail = step;
            snap(run, SNAP_TABLES);
            dma::abort(Channel::MdecIn);
            return false;
        }
    }
    snap(run, SNAP_TABLES);
    true
}

/// F: the SDK driver on this build.
fn variant_f(run: &mut Run) -> bool {
    let settled = mdec::reset();
    snap(run, SNAP_ENABLE);
    let settled_bit = if settled { 0x8000 } else { 0 };
    match mdec::load_tables() {
        Some(tables) => {
            run.extra = settled_bit
                | tables.enable_writes as u16
                | ((tables.cpu_uploads as u16) << 8);
            snap(run, SNAP_TABLES);
            true
        }
        None => {
            run.extra = settled_bit;
            run.fail = STEP_QUANT;
            record_timeout(run, Channel::MdecIn);
            snap(run, SNAP_TABLES);
            false
        }
    }
}

const SEQUENCES: [fn(&mut Run) -> bool; VARIANTS] =
    [variant_a, variant_b, variant_c, variant_d, variant_e, variant_f];

// --- The probe decode ------------------------------------------------------

/// One colour macroblock: six DC-only blocks (scale 8, DC 64, then the FE00
/// end code), padded to the 32-word DMA block with FE00 halfwords, which the
/// MDEC skips at a block start.
static PROBE_IN: [u32; 32] = {
    let mut words = [0xFE00_FE00u32; 32];
    let mut i = 0;
    while i < 6 {
        words[i] = 0xFE00_2040;
        i += 1;
    }
    words
};
/// 16x16 pixels at 15bpp.
const PROBE_OUT_WORDS: usize = 128;
/// Fill for the output buffer, so a word the DMA never wrote is visible.
const PROBE_FILL: u32 = 0x5A5A_A5A5;
/// Fault-injection switch for the probe (see FAULT).
struct FaultFlag(core::cell::Cell<bool>);
// SAFETY: single-threaded guest.
unsafe impl Sync for FaultFlag {}
impl FaultFlag {
    fn load(&self) -> bool {
        self.0.get()
    }
    fn store(&self, value: bool) {
        self.0.set(value)
    }
}
static FAULT_PROBE: FaultFlag = FaultFlag(core::cell::Cell::new(false));
static mut PROBE_OUT: [u32; PROBE_OUT_WORDS] = [0; PROBE_OUT_WORDS];

fn probe(run: &mut Run) -> bool {
    // SAFETY: PROBE_OUT is only touched here, with DMA1 idle.
    let out = unsafe { &mut *addr_of_mut!(PROBE_OUT) };
    out.fill(PROBE_FILL);
    cmd(mdec::DECODE_15BPP | 32);
    run.probe_dreq = poll(IN_REQUEST, IN_REQUEST);
    dma_in(PROBE_IN.as_ptr(), 32);
    dma::abort(Channel::MdecOut);
    // SAFETY: single-threaded bookkeeping.
    unsafe { (*addr_of_mut!(KICK_MADR))[1] = out.as_mut_ptr() as u32 };
    dma::set_madr(Channel::MdecOut, out.as_mut_ptr() as u32);
    dma::set_bcr_block(Channel::MdecOut, 32, (PROBE_OUT_WORDS / 32) as u16);
    dma::set_chcr(Channel::MdecOut, CHCR_OUT);
    let out_done = dma_done(Channel::MdecOut) && !(FAULT && FAULT_PROBE.load());
    let in_done = dma_done(Channel::MdecIn);
    snap(run, SNAP_PROBE);
    run.probe_words = out.iter().filter(|&&w| w != PROBE_FILL).count() as u16;
    run.probe_word = out[0];
    run.probe_flat = out.iter().all(|&w| w == out[0]);
    if !in_done {
        record_timeout(run, Channel::MdecIn);
        run.fail = STEP_PROBE_IN;
        return false;
    }
    if !out_done {
        record_timeout(run, Channel::MdecOut);
        run.fail = STEP_PROBE_OUT;
        return false;
    }
    true
}

// --- The battery -----------------------------------------------------------

/// Put the MDEC in the state a run starts from: idle, or `busy` in the
/// middle of a quant-table command.
fn prepare(busy: bool) {
    ctl(RESET);
    delay(PREP_DELAY);
    if busy {
        cmd(SET_QUANT);
        for &word in &mdec::QUANT_WORDS[..PREP_WORDS] {
            cmd(word);
        }
    }
}

fn abort_both() {
    dma::abort(Channel::MdecIn);
    dma::abort(Channel::MdecOut);
}

static mut BATTERIES: u8 = 0;

/// Run every sequence RUNS times and pick the one the movie plays behind.
/// Takes Timer 2 and the MDEC; puts Timer 2's mode back.
pub(crate) fn run_battery(font: &FontAtlas) -> Diag {
    let timer_mode = timers::mode(timers::Timer::Timer2) & 0x03FF;
    timers::set_mode(timers::Timer::Timer2, 0);
    // SAFETY: DPCR read.
    let dpcr = unsafe { psx_io::read32(dma::DPCR) };
    let mut variants = [Variant {
        pass_mask: 0,
        run_fails: 0,
        detail: Run::new(),
        detail_run: 0,
    }; VARIANTS];
    begin_screen();
    font.draw_text(X0, 16, "MDEC SETUP DIAGNOSTIC", WHITE);
    font.draw_text(X0 + 8 * 23, 16, crate::SUITE_VERSION, WHITE);
    font.draw_text(X0, 40, "RESET LATENCY TRACES", YELLOW);
    end_screen();
    let latency = [
        trace(font, false, false),
        trace(font, true, false),
        trace(font, true, true),
    ];
    for (v, result) in variants.iter_mut().enumerate() {
        for r in 0..RUNS {
            let mask = irq::mask();
            irq::set_mask(0);
            // SAFETY: DPCR write, back to what the battery found.
            unsafe { psx_io::write32(dma::DPCR, dpcr) };
            abort_both();
            if v != 0 || r != 0 {
                prepare(r % 2 == 1);
            }
            let mut run = Run::new();
            FAULT_PROBE.store(v == 4);
            progress(font, v, r, "SETUP");
            snap(&mut run, SNAP_BEFORE);
            let mut ok = SEQUENCES[v](&mut run);
            if ok {
                progress(font, v, r, "PROBE");
                ok = probe(&mut run);
            }
            abort_both();
            progress(font, v, r, if ok { "WORKED" } else { "FAILED" });
            irq::set_mask(mask);
            if ok {
                result.pass_mask |= 1 << r;
            }
            result.run_fails |= ((run.fail & 0xF) as u32) << (4 * r);
            let keep = r == 0 || (!ok && result.detail.fail == 0);
            if keep {
                result.detail = run;
                result.detail_run = r as u8;
            }
        }
    }
    timers::set_mode(timers::Timer::Timer2, timer_mode);
    // SAFETY: single-threaded counter.
    let batteries = unsafe {
        let b = &mut *addr_of_mut!(BATTERIES);
        *b = b.wrapping_add(1);
        *b
    };
    let chosen = choose(&variants);
    let mut diag = Diag {
        chosen,
        variants,
        batteries,
        latency,
        control: hello_fmv::FrameControl::default(),
        control_ran: false,
        plays: [None; VARIANTS],
    };
    print_tty(&diag);
    progress_line(font, 0, "ONE-FRAME CONTROL DECODE (CD)", YELLOW);
    diag.control = hello_fmv::frame_control(setup_fn(&diag));
    diag.control_ran = true;
    diag
}

/// The movie's setup: the SDK driver when it held every run, else the first
/// sequence that did, else the first that worked at all.
fn choose(variants: &[Variant; VARIANTS]) -> Option<usize> {
    let order = [SDK, 0, 1, 2, 3, 4];
    let all = (1u16 << RUNS) as u8 as u16;
    let _ = all;
    order
        .iter()
        .copied()
        .find(|&v| variants[v].pass_mask == 0xFF)
        .or_else(|| order.iter().copied().find(|&v| variants[v].pass_mask != 0))
}

/// Run sequence `v` once outside the battery (no probe), for the player.
/// Puts Timer 2's mode back: the player profiles with it.
fn setup_once(v: usize) -> bool {
    let timer_mode = timers::mode(timers::Timer::Timer2) & 0x03FF;
    timers::set_mode(timers::Timer::Timer2, 0);
    let mut run = Run::new();
    let ok = SEQUENCES[v](&mut run);
    timers::set_mode(timers::Timer::Timer2, timer_mode);
    ok
}

fn setup_a() -> bool {
    setup_once(0)
}
fn setup_b() -> bool {
    setup_once(1)
}
fn setup_c() -> bool {
    !FAULT && setup_once(2)
}
fn setup_d() -> bool {
    setup_once(3)
}
fn setup_e() -> bool {
    setup_once(4)
}

/// The setup of the chosen sequence, or the SDK default.
pub(crate) fn setup_fn(diag: &Diag) -> fn() -> bool {
    setup_for(diag.chosen.unwrap_or(SDK))
}

// --- Playback --------------------------------------------------------------

/// Video sectors in each short playback: 15 s of a movie that carries 9,826
/// in 75 s.
pub(crate) const CUT_SECTORS: u32 = 1965;
const PLAY_LABELS: [&str; VARIANTS] = ["SEQ A", "SEQ B", "SEQ C", "SEQ D", "SEQ E", "SEQ F SDK"];
/// How long a summary stays up before the next run starts, in VBlanks.
const SUMMARY_VBLANKS: u32 = 240;

/// The order the playbacks run in: the chosen sequence first, then every
/// other sequence that worked at least once, A to F. With none working,
/// one run behind the SDK default, so the capture still says how it fails.
fn play_order(diag: &Diag) -> ([usize; VARIANTS], usize) {
    let mut order = [0; VARIANTS];
    let mut n = 0;
    let first = diag.chosen.unwrap_or(SDK);
    order[n] = first;
    n += 1;
    for v in 0..VARIANTS {
        if v != first && diag.variants[v].pass_mask != 0 {
            order[n] = v;
            n += 1;
        }
    }
    (order, n)
}

/// Play the short cut behind each sequence in [`play_order`]. Each summary
/// stays up for a few seconds with the next run named under it; the last
/// one is left on screen for the caller. Returns the first run's outcome.
pub(crate) fn play_all(diag: &mut Diag) -> hello_fmv::Outcome {
    let (order, n) = play_order(diag);
    let mut first = hello_fmv::Outcome::default();
    for (i, &v) in order[..n].iter().enumerate() {
        let setup = if Some(v) == diag.chosen || v != SDK {
            setup_for(v)
        } else {
            hello_fmv::mdec_setup
        };
        let outcome = hello_fmv::run_with(hello_fmv::Options {
            setup,
            max_sectors: CUT_SECTORS,
            label: PLAY_LABELS[v],
        });
        diag.plays[v] = Some(outcome);
        if i == 0 {
            first = outcome;
        }
        // The player left its summary displayed at VRAM row 0 and its own
        // fonts over the suite's atlas.
        let font = FontAtlas::upload(&psx_font::fonts::BASIC, crate::FONT_TPAGE, crate::FONT_CLUT);
        gpu::set_draw_area(0, 0, 319, 239);
        gpu::set_draw_offset(0, 0);
        let stop = Line::new()
            .s("STOP ")
            .s(STOP_NAMES[(outcome.stop as usize).min(STOP_NAMES.len() - 1)]);
        gpu::fill_rect(192, 0, 128, 10, 0, 0, 0);
        font.draw_text(200, 2, stop.as_str(), YELLOW);
        if i + 1 < n {
            let next = Line::new()
                .s("RUN ")
                .dec(i as u32 + 1)
                .s("/")
                .dec(n as u32)
                .s(" NEXT: ")
                .s(PLAY_LABELS[order[i + 1]])
                .s(" IN 4 S");
            gpu::fill_rect(0, 226, 320, 12, 0, 0, 0);
            font.draw_text(X0, 228, next.as_str(), YELLOW);
        }
        gpu::draw_sync();
        if i + 1 < n {
            let start = interrupts::vblank_count();
            while interrupts::vblank_count().wrapping_sub(start) < SUMMARY_VBLANKS {
                interrupts::wait_vblank();
            }
        }
    }
    first
}

const STOP_NAMES: [&str; 5] = ["END", "STALL", "WEDGED", "CD ERROR", "SETUP"];

/// The MDEC setup for sequence `v` outside the battery.
fn setup_for(v: usize) -> fn() -> bool {
    match v {
        0 => setup_a,
        1 => setup_b,
        2 => setup_c,
        3 => setup_d,
        4 => setup_e,
        _ => hello_fmv::mdec_setup,
    }
}

// --- Capture records -------------------------------------------------------

/// Sequence v owns 0x200 + 0x10 * v and up.
pub(crate) const FIRST_RECORD: u16 = 0x200;
/// Chosen sequence, runs per sequence, battery count.
pub(crate) const OVERVIEW_RECORD: u16 = 0x260;
/// Playback behind sequence v: 0x270 + 4 * v, four records.
pub(crate) const PLAY_RECORD: u16 = 0x270;
/// Reset-latency trace t: 0x290 + 5 * t, five records.
pub(crate) const TRACE_RECORD: u16 = 0x290;
/// The one-frame control: 0x2A0, three records.
pub(crate) const CONTROL_RECORD: u16 = 0x2A0;
/// Last id this module writes.
const LAST_RECORD: u16 = 0x2AF;
/// Halfwords a sequence carries: 29, or 41 with the timeout registers.
const STREAM_MAX: usize = 41;
pub(crate) const RECORD_MAX: usize =
    VARIANTS * STREAM_MAX.div_ceil(3) + 1 + VARIANTS * 4 + TRACES * 5 + 3;

struct Stream {
    halves: [u16; STREAM_MAX],
    n: usize,
}

impl Stream {
    fn new() -> Self {
        Stream {
            halves: [0; STREAM_MAX],
            n: 0,
        }
    }
    fn h(&mut self, value: u16) {
        self.halves[self.n] = value;
        self.n += 1;
    }
    fn w(&mut self, value: u32) {
        self.h(value as u16);
        self.h((value >> 16) as u16);
    }
    /// Three halves to a record from `id`, zero-padded.
    fn emit(&self, id: u16, out: &mut [TimingRecord; RECORD_MAX], slot: &mut usize) {
        for (k, chunk) in self.halves[..self.n].chunks(3).enumerate() {
            let at = |i: usize| chunk.get(i).copied().unwrap_or(0);
            out[*slot] = TimingRecord {
                id: id + k as u16,
                work: 0,
                min: at(0),
                med: at(1),
                max: at(2),
            };
            *slot += 1;
        }
    }
}

/// The diagnostic as timing-block records, unused slots marked
/// TIMING_RECORD_UNUSED. Each block is a halfword stream packed three to a
/// record (min, median, max), the last record zero-padded; u32 values go
/// low half first.
///
/// Sequence v, from 0x200 + 0x10 * v: 0 pass mask (bit n = run n+1) | runs
/// << 8; 1 fail step of the detail run | detail run index << 8; 2 settle
/// clocks; 3 request clocks; 4 probe request clocks; 5 extra (F: enable
/// writes | CPU uploads << 8 | reset settled << 15); 6 snapshots read (bit
/// per slot) | rescue << 8; 7 probe words | flat << 15; 8 timed out; 9-10
/// per-run fail steps (u32, four bits a run, run 1 lowest); 11-22 six
/// status snapshots (before reset, after reset, after enable, after
/// command, after tables, after probe); 23-24 probe first word; 25-26
/// status after A's rescue; then only when a DMA timed out, 27-38 CHCR,
/// BCR, MADR, DPCR, DICR and the kick's MADR.
///
/// 0x260: chosen sequence (0-5, 0xFFFF none), runs, battery count.
///
/// Playback behind sequence v, 0x270 + 4 * v (only sequences played): stop
/// | pass << 4 | setup error << 8, shown, late; good, target, lost; bad,
/// dropped, decode errors; cd errors, first error LBA (0xFFFF none), last
/// good LBA.
///
/// Trace t (idle, busy, busy + enable), 0x290 + 5 * t: sample count, then
/// per sample clocks and status (u32).
///
/// Control, 0x2A0: read | cpu ok << 1 | dma ok << 2 | sums equal << 3, RLE
/// words, expected output words, CPU words, DMA words, CPU sum (u32), DMA
/// sum (u32).
pub(crate) fn records(diag: &Diag) -> [TimingRecord; RECORD_MAX] {
    let mut out = [TimingRecord {
        id: TIMING_RECORD_UNUSED,
        work: 0,
        min: 0,
        med: 0,
        max: 0,
    }; RECORD_MAX];
    let mut slot = 0;
    for (v, variant) in diag.variants.iter().enumerate() {
        let run = &variant.detail;
        let mut s = Stream::new();
        s.h(variant.pass_mask as u16 | ((RUNS as u16) << 8));
        s.h(run.fail as u16 | ((variant.detail_run as u16) << 8));
        s.h(run.settle);
        s.h(run.dreq);
        s.h(run.probe_dreq);
        s.h(run.extra);
        s.h(run.taken as u16 | ((run.rescue as u16) << 8));
        s.h(run.probe_words | ((run.probe_flat as u16) << 15));
        s.h(run.timed_out as u16);
        s.w(variant.run_fails);
        for value in run.snaps {
            s.w(value);
        }
        s.w(run.probe_word);
        s.w(run.rescue_status);
        if run.timed_out {
            for value in run.timeout {
                s.w(value);
            }
        }
        s.emit(FIRST_RECORD + 0x10 * v as u16, &mut out, &mut slot);
    }
    out[slot] = TimingRecord {
        id: OVERVIEW_RECORD,
        work: 0,
        min: diag.chosen.map_or(0xFFFF, |v| v as u16),
        med: RUNS as u16,
        max: diag.batteries as u16,
    };
    slot += 1;
    for (v, play) in diag.plays.iter().enumerate() {
        let Some(o) = play else {
            continue;
        };
        let setup = o
            .setup_error
            .and_then(|what| crate::fmv_test::setup_error_code(what))
            .unwrap_or(0) as u16;
        let mut s = Stream::new();
        s.h(o.stop as u16 | ((o.pass as u16) << 4) | (setup << 8));
        s.h(o.shown.min(0xFFFF) as u16);
        s.h(o.late.min(0xFFFF) as u16);
        s.h(o.good.min(0xFFFF) as u16);
        s.h(o.target.min(0xFFFF) as u16);
        s.h(o.lost.min(0xFFFF) as u16);
        s.h(o.bad.min(0xFFFF) as u16);
        s.h(o.dropped.min(0xFFFF) as u16);
        s.h(o.decode_errors.min(0xFFFF) as u16);
        s.h(o.cd_errors.min(0xFFFF) as u16);
        s.h(o.first_err_lba.map_or(0xFFFF, |lba| lba.min(0xFFFE) as u16));
        s.h(o.lba.min(0xFFFF) as u16);
        s.emit(PLAY_RECORD + 4 * v as u16, &mut out, &mut slot);
    }
    for (t, trace) in diag.latency.iter().enumerate() {
        let mut s = Stream::new();
        s.h(trace.count as u16);
        for i in 0..TRACE_SAMPLES {
            s.h(trace.clocks[i]);
            s.w(trace.status[i]);
        }
        s.emit(TRACE_RECORD + 5 * t as u16, &mut out, &mut slot);
    }
    if diag.control_ran {
        let c = &diag.control;
        let mut s = Stream::new();
        s.h(c.read as u16
            | ((c.cpu_ok as u16) << 1)
            | ((c.dma_ok as u16) << 2)
            | (((c.cpu_sum == c.dma_sum) as u16) << 3));
        s.h(c.rle_words.min(0xFFFF) as u16);
        s.h(c.expected.min(0xFFFF) as u16);
        s.h(c.cpu_words.min(0xFFFF) as u16);
        s.h(c.dma_words.min(0xFFFF) as u16);
        s.w(c.cpu_sum);
        s.w(c.dma_sum);
        s.emit(CONTROL_RECORD, &mut out, &mut slot);
    }
    out
}

/// Put the records into `slots`, replacing an earlier battery's (all of
/// them: blocks this one does not carry are cleared). False if the slots
/// were full.
pub(crate) fn merge(
    slots: &mut [TimingRecord; TIMING_RECORD_COUNT],
    records: &[TimingRecord; RECORD_MAX],
) -> bool {
    for slot in slots.iter_mut() {
        if (FIRST_RECORD..=LAST_RECORD).contains(&slot.id) {
            *slot = TimingRecord {
                id: TIMING_RECORD_UNUSED,
                work: 0,
                min: 0,
                med: 0,
                max: 0,
            };
        }
    }
    let mut all = true;
    for record in records.iter().filter(|r| r.id != TIMING_RECORD_UNUSED) {
        match slots.iter().position(|s| s.id == TIMING_RECORD_UNUSED) {
            Some(index) => slots[index] = *record,
            None => all = false,
        }
    }
    all
}

// --- Screens ---------------------------------------------------------------

/// Fixed-size text line.
struct Line {
    buf: [u8; 64],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Line {
            buf: [b' '; 64],
            len: 0,
        }
    }
    fn s(mut self, text: &str) -> Self {
        for &b in text.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }
    fn pad(mut self, column: usize) -> Self {
        while self.len < column.min(self.buf.len()) {
            self.buf[self.len] = b' ';
            self.len += 1;
        }
        self
    }
    fn hex(self, value: u32, digits: u32) -> Self {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = self;
        let mut d = digits;
        while d > 0 {
            d -= 1;
            let nibble = ((value >> (4 * d)) & 0xF) as usize;
            if out.len < out.buf.len() {
                out.buf[out.len] = HEX[nibble];
                out.len += 1;
            }
        }
        out
    }
    fn dec(mut self, value: u32) -> Self {
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut x = value;
        loop {
            digits[count] = b'0' + (x % 10) as u8;
            count += 1;
            x /= 10;
            if x == 0 {
                break;
            }
        }
        while count > 0 {
            count -= 1;
            if self.len < self.buf.len() {
                self.buf[self.len] = digits[count];
                self.len += 1;
            }
        }
        self
    }
    /// Clocks, or "----" for NEVER.
    fn clocks(self, value: u16) -> Self {
        if value == NEVER {
            self.s("----")
        } else {
            self.dec(value as u32)
        }
    }
    fn mask(mut self, mask: u8) -> Self {
        for bit in 0..RUNS {
            if self.len < self.buf.len() {
                self.buf[self.len] = if mask & (1 << bit) != 0 { b'1' } else { b'0' };
                self.len += 1;
            }
        }
        self
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

const WHITE: (u8, u8, u8) = (220, 220, 220);
const GREEN: (u8, u8, u8) = (80, 230, 110);
const RED: (u8, u8, u8) = (250, 80, 80);
const YELLOW: (u8, u8, u8) = (240, 216, 90);
const X0: i16 = 16;
const PAGES: usize = 2 + VARIANTS;

fn begin_screen() {
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    gpu::fill_rect(0, 0, 320, 240, 0, 0, 0);
}

fn end_screen() {
    gpu::draw_sync();
    psx_io::gpu::write_gp1(0x0500_0000);
}

/// One progress line in the band under the title, replacing the last.
/// Drawn between runs and phases, never inside a sequence, so it cannot
/// change a sequence's timing. A hang leaves the last line on screen.
fn progress_line(font: &FontAtlas, row: i16, text: &str, tint: (u8, u8, u8)) {
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    gpu::fill_rect(0, 60 + 12 * row as u16, 320, 12, 0, 0, 0);
    font.draw_text(X0, 62 + 12 * row, text, tint);
    gpu::draw_sync();
}

/// Variant, run and phase, with MDEC1 and DMA0 CHCR as they are now.
fn progress(font: &FontAtlas, v: usize, r: usize, phase: &str) {
    let head = Line::new()
        .s(LETTERS[v])
        .s(" ")
        .s(NAMES[v])
        .s(" RUN ")
        .dec(r as u32 + 1)
        .s("/")
        .dec(RUNS as u32)
        .s(" ")
        .s(phase);
    progress_line(font, 0, head.as_str(), YELLOW);
    let regs = Line::new()
        .s("MDEC1 ")
        .hex(st(), 8)
        .s(" DMA0 CHCR ")
        .hex(dma::chcr(Channel::MdecIn), 8);
    progress_line(font, 1, regs.as_str(), WHITE);
}

fn verdict_tint(variant: &Variant) -> (u8, u8, u8) {
    match variant.pass_mask {
        0xFF => GREEN,
        0 => RED,
        _ => YELLOW,
    }
}

fn draw_summary(font: &FontAtlas, diag: &Diag) {
    begin_screen();
    font.draw_text(X0, 16, "MDEC SETUP DIAGNOSTIC", WHITE);
    font.draw_text(X0 + 8 * 23, 16, crate::SUITE_VERSION, WHITE);
    font.draw_text(X0, 32, "  SEQUENCE        RUNS FIRST FAIL", WHITE);
    for (v, variant) in diag.variants.iter().enumerate() {
        let passed = variant.pass_mask.count_ones();
        let fail = variant.detail.fail as usize;
        let line = Line::new()
            .s(LETTERS[v])
            .s(" ")
            .s(NAMES[v])
            .pad(18)
            .dec(passed)
            .s("/")
            .dec(RUNS as u32)
            .pad(23)
            .s(STEP_NAMES[fail.min(STEP_NAMES.len() - 1)]);
        font.draw_text(X0, 46 + 12 * v as i16, line.as_str(), verdict_tint(variant));
    }
    let a = &diag.variants[0].detail;
    let rescue = match a.rescue {
        1 => ("A: LATE ENABLE FREED THE DMA", YELLOW),
        2 => ("A: LATE ENABLE DID NOT HELP", YELLOW),
        _ => ("A: NO TIMEOUT, NO RESCUE", WHITE),
    };
    font.draw_text(X0, 126, rescue.0, rescue.1);
    let f = &diag.variants[SDK].detail;
    let line = Line::new()
        .s("F: ENABLE WRITES ")
        .dec((f.extra & 0xFF) as u32)
        .s(" CPU TABLES ")
        .dec(((f.extra >> 8) & 0x7F) as u32);
    font.draw_text(X0, 138, line.as_str(), WHITE);
    match diag.chosen {
        Some(v) => {
            let line = Line::new().s("MOVIE USES ").s(LETTERS[v]).s(" ").s(NAMES[v]);
            font.draw_text(X0, 160, line.as_str(), GREEN);
        }
        None => font.draw_text(X0, 160, "NO SEQUENCE WORKED: SDK DEFAULT", RED),
    }
    font.draw_text(X0, 196, "LEFT/RIGHT: DETAIL PAGES", WHITE);
    font.draw_text(X0, 208, "CROSS: GO ON", WHITE);
    draw_page_number(font, 0);
    end_screen();
}

fn draw_page_number(font: &FontAtlas, page: usize) {
    let line = Line::new()
        .s("PAGE ")
        .dec(page as u32 + 1)
        .s("/")
        .dec(PAGES as u32);
    font.draw_text(X0 + 8 * 26, 208, line.as_str(), WHITE);
}

fn draw_variant(font: &FontAtlas, diag: &Diag, v: usize) {
    begin_screen();
    let variant = &diag.variants[v];
    let run = &variant.detail;
    let title = Line::new().s(LETTERS[v]).s(" ").s(NAMES[v]);
    font.draw_text(X0, 16, title.as_str(), verdict_tint(variant));
    let mask = Line::new()
        .s("RUNS ")
        .mask(variant.pass_mask)
        .s("  ")
        .dec(variant.pass_mask.count_ones())
        .s("/")
        .dec(RUNS as u32)
        .s(" WORKED");
    font.draw_text(X0, 28, mask.as_str(), WHITE);
    font.draw_text(X0, 40, "RUN 1 LEFT. ODD RUNS IDLE, EVEN BUSY", WHITE);
    let fail = run.fail as usize;
    let detail = Line::new()
        .s("DETAIL RUN ")
        .dec(variant.detail_run as u32 + 1)
        .s(": ")
        .s(if run.fail == 0 { "WORKED" } else { "FAIL " })
        .s(if run.fail == 0 { "" } else { STEP_NAMES[fail.min(STEP_NAMES.len() - 1)] });
    font.draw_text(X0, 54, detail.as_str(), if run.fail == 0 { GREEN } else { RED });
    for slot in 0..SNAPS {
        let line = Line::new().s("MDEC1 ").s(SNAP_NAMES[slot]).s(" ");
        let line = if run.taken & (1 << slot) != 0 {
            line.hex(run.snaps[slot], 8)
        } else {
            line.s("--------")
        };
        font.draw_text(X0, 68 + 10 * slot as i16, line.as_str(), WHITE);
    }
    let clocks = Line::new()
        .s("SETTLE ")
        .clocks(run.settle)
        .s(" DREQ ")
        .clocks(run.dreq)
        .s(" PROBE ")
        .clocks(run.probe_dreq);
    font.draw_text(X0, 132, clocks.as_str(), WHITE);
    if run.timed_out {
        let l1 = Line::new()
            .s("TIMEOUT CHCR ")
            .hex(run.timeout[0], 8)
            .s(" BCR ")
            .hex(run.timeout[1], 8);
        let l2 = Line::new()
            .s("MADR ")
            .hex(run.timeout[2], 8)
            .s(" DPCR ")
            .hex(run.timeout[3], 8);
        let l3 = Line::new().s("DICR ").hex(run.timeout[4], 8);
        font.draw_text(X0, 144, l1.as_str(), RED);
        font.draw_text(X0, 154, l2.as_str(), RED);
        font.draw_text(X0, 164, l3.as_str(), RED);
    } else {
        font.draw_text(X0, 144, "NO DMA TIMEOUT", WHITE);
    }
    if v == 0 && run.rescue != 0 {
        let line = Line::new()
            .s(if run.rescue == 1 { "LATE ENABLE: FREED " } else { "LATE ENABLE: STUCK " })
            .hex(run.rescue_status, 8);
        font.draw_text(X0, 176, line.as_str(), YELLOW);
    }
    if v == SDK {
        let line = Line::new()
            .s("ENABLE WRITES ")
            .dec((run.extra & 0xFF) as u32)
            .s(" CPU ")
            .dec(((run.extra >> 8) & 0x7F) as u32)
            .s(" SETTLED ")
            .s(if run.extra & 0x8000 != 0 { "Y" } else { "N" });
        font.draw_text(X0, 176, line.as_str(), WHITE);
    }
    let probe = Line::new()
        .s("PROBE OUT ")
        .dec(run.probe_words as u32)
        .s("/128 FLAT ")
        .s(if run.probe_flat { "Y " } else { "N " })
        .hex(run.probe_word, 8);
    font.draw_text(X0, 188, probe.as_str(), WHITE);
    font.draw_text(X0, 208, "CROSS: GO ON", WHITE);
    draw_page_number(font, 1 + v);
    end_screen();
}

fn draw_page(font: &FontAtlas, diag: &Diag, page: usize) {
    if page == 0 {
        draw_summary(font, diag);
    } else if page <= VARIANTS {
        draw_variant(font, diag, page - 1);
    } else {
        draw_latency(font, diag);
    }
}

fn draw_latency(font: &FontAtlas, diag: &Diag) {
    begin_screen();
    font.draw_text(X0, 16, "RESET LATENCY: MDEC1 AFTER RESET", WHITE);
    font.draw_text(X0, 28, "(CLOCKS SINCE THE RESET WRITE)", WHITE);
    let mut y = 42;
    for (t, trace) in diag.latency.iter().enumerate() {
        font.draw_text(X0, y, TRACE_NAMES[t], YELLOW);
        y += 10;
        for i in 0..trace.count as usize {
            let line = Line::new()
                .s("  ")
                .dec(trace.clocks[i] as u32)
                .pad(9)
                .hex(trace.status[i], 8);
            font.draw_text(X0, y, line.as_str(), WHITE);
            y += 10;
        }
    }
    let c = &diag.control;
    let line = Line::new()
        .s("FRAME CPU ")
        .s(if c.cpu_ok { "OK " } else { "FAIL " })
        .hex(c.cpu_sum, 8)
        .s(" DMA ")
        .s(if c.dma_ok { "OK " } else { "FAIL " });
    let tint = if c.cpu_ok && c.dma_ok && c.cpu_sum == c.dma_sum { GREEN } else { RED };
    font.draw_text(X0, 172, line.as_str(), tint);
    let line = Line::new()
        .s("DMA SUM ")
        .hex(c.dma_sum, 8)
        .s(if c.cpu_sum == c.dma_sum { " SAME" } else { " DIFFERENT" });
    font.draw_text(X0, 184, line.as_str(), tint);
    let line = Line::new()
        .s("WORDS CPU ")
        .dec(c.cpu_words)
        .s(" DMA ")
        .dec(c.dma_words)
        .s(" OF ")
        .dec(c.expected);
    font.draw_text(X0, 196, line.as_str(), WHITE);
    font.draw_text(X0, 208, "CROSS: GO ON", WHITE);
    draw_page_number(font, PAGES - 1);
    end_screen();
}

/// Show the result pages until CROSS or START. LEFT/RIGHT page through.
pub(crate) fn show(font: &FontAtlas, diag: &Diag) {
    let mut page = 0;
    draw_page(font, diag, page);
    let held = |mask: u16| psx_pad::poll_port1().buttons.is_held(mask);
    // Wait out the press that started the test.
    while held(button::CROSS) || held(button::START) {
        interrupts::wait_vblank();
    }
    let mut last = 0u16;
    loop {
        interrupts::wait_vblank();
        let now = [button::LEFT, button::RIGHT, button::CROSS, button::START]
            .iter()
            .fold(0u16, |acc, &b| if held(b) { acc | b } else { acc });
        let pressed = now & !last;
        last = now;
        if pressed & (button::CROSS | button::START) != 0 {
            break;
        }
        if pressed & button::RIGHT != 0 {
            page = (page + 1) % PAGES;
            draw_page(font, diag, page);
        }
        if pressed & button::LEFT != 0 {
            page = (page + PAGES - 1) % PAGES;
            draw_page(font, diag, page);
        }
    }
    // Let go before the player starts.
    while held(button::CROSS) || held(button::START) {
        interrupts::wait_vblank();
    }
}

// --- TTY -------------------------------------------------------------------

fn print_tty(diag: &Diag) {
    for (v, variant) in diag.variants.iter().enumerate() {
        let run = &variant.detail;
        let head = Line::new()
            .s("mdec-diag ")
            .s(LETTERS[v])
            .s(" mask=")
            .mask(variant.pass_mask)
            .s(" run=")
            .dec(variant.detail_run as u32 + 1)
            .s(" fail=")
            .dec(run.fail as u32)
            .s(" settle=")
            .clocks(run.settle);
        tty::print(head.as_str());
        let tail = Line::new()
            .s(" dreq=")
            .clocks(run.dreq)
            .s(" probe=")
            .clocks(run.probe_dreq)
            .s(" words=")
            .dec(run.probe_words as u32)
            .s(" extra=")
            .hex(run.extra as u32, 4);
        tty::print(tail.as_str());
        for slot in 0..SNAPS {
            let snap = Line::new().s(" s").dec(slot as u32).s("=").hex(run.snaps[slot], 8);
            tty::print(snap.as_str());
        }
        tty::println("");
        if run.timed_out {
            let line = Line::new()
                .s("mdec-diag ")
                .s(LETTERS[v])
                .s(" timeout chcr=")
                .hex(run.timeout[0], 8);
            tty::print(line.as_str());
            let line = Line::new()
                .s(" bcr=")
                .hex(run.timeout[1], 8)
                .s(" madr=")
                .hex(run.timeout[2], 8);
            tty::print(line.as_str());
            let line = Line::new()
                .s(" dpcr=")
                .hex(run.timeout[3], 8)
                .s(" dicr=")
                .hex(run.timeout[4], 8)
                .s(" rescue=")
                .dec(run.rescue as u32);
            tty::println(line.as_str());
        }
    }
    let line = Line::new().s("mdec-diag chosen=").s(match diag.chosen {
        Some(v) => LETTERS[v],
        None => "none",
    });
    tty::println(line.as_str());
}
