// SPDX-License-Identifier: GPL-2.0-or-later
//! MDEC SETUP DIAGNOSTIC (ported from v1.26 `fmv_diag.rs` into v2.10).
//!
//! v1.25's FMV test showed a red screen on a PAL SCPH-9002: the SDK player's
//! DMA0 upload of the MDEC tables never finished, while the same disc played
//! on PSoXide and on a SuperStation One. That console read the MDEC status
//! straight after a reset as 0x6401_0000 (busy, data-in full: the state from
//! before the reset) where the FPGA read the reset state. The lead was that the
//! player's DMA-request enable, written right behind the reset, was lost to a
//! reset still in progress. This battery tests that and the alternatives, so
//! the silicon answer for every setup order is on record, not only the one the
//! SDK driver uses (which `mdec_check.rs` runs as the short decode check).
//!
//! Sequences, each from its own reset:
//!
//! - A: the v1.25 driver (SDK c756ec1d) verbatim: reset, enable, DPCR, then
//!   DMA0 the tables. No status reads inside it. If a table DMA times out it
//!   then writes the enable again and waits once more: a DMA that finishes
//!   after that proves the first enable was lost.
//! - B: reset, poll until not busy (clocks recorded), enable, then after the
//!   table command wait for the data-in request (status bit 28, clocks
//!   recorded) before DMA0.
//! - C: reset, a fixed 20,000-clock delay, enable, DMA0.
//! - D: reset, settle, both tables over CPU writes, then enable, settle and
//!   check bit 28; only the probe decode uses DMA.
//! - E: PSn00bSDK's order (DecDCTReset + DecDCTPutEnv): DPCR first, channels
//!   stopped, reset and enable back to back, then wait for not-busy before
//!   each command and after each DMA.
//! - F: the SDK driver on this build (psx_fmv::mdec::reset + load_tables).
//!
//! Every sequence is followed by the same probe decode: one DC-only colour
//! macroblock in over DMA0, 128 words of 15bpp out over DMA1. A run WORKS
//! when its tables went in and the probe came back whole. Each sequence runs
//! eight times: runs 1, 3, 5 and 7 start from an idle MDEC, runs 2, 4, 6 and 8
//! reset it in the middle of a table command. Run 1 of A starts from whatever
//! state the area reset left. Interrupts are masked for each run so a VBlank
//! cannot stretch a race. Then MDEC1 is traced over time after a reset from
//! idle, from busy, and from busy with the enable written straight after, and
//! a one-macroblock control decodes the probe block over CPU writes and reads
//! and over DMA and compares the output sums.
//!
//! What v1.26 had and this does not: the on-screen pages (v2 shows progress
//! only; the capture carries everything), the playback of the 15 s cut behind
//! each sequence (needs the 75 s MOVIE.STR that v2.0 dropped), and the control
//! on the movie's first frame (this control uses the probe macroblock).
//!
//! Records, ids moved from the v1.26 `0x200`-`0x2A2` so they cannot collide
//! with the v2 texture-state block at `0x200`-`0x223`; the layout inside is
//! unchanged apart from the dropped playback block: see [`records`].

use crate::console_tests::record;
use crate::TimingRecord;
use core::ptr::addr_of_mut;
use psx_fmv::mdec;
use psx_io::dma::{self, Channel};
use psx_io::{irq, timers};
use psx_rt::tty;

const MDEC0: u32 = 0x1F80_1820;
const MDEC1: u32 = 0x1F80_1824;
const RESET: u32 = psx_hw::mdec::CONTROL_RESET;
const ENABLE: u32 = psx_hw::mdec::CONTROL_ENABLE_DMA;
const BUSY: u32 = psx_hw::mdec::STATUS_BUSY;
const IN_REQUEST: u32 = psx_hw::mdec::STATUS_IN_REQUEST;
const IN_FULL: u32 = psx_hw::mdec::STATUS_IN_FULL;
const SET_QUANT: u32 = psx_hw::mdec::COMMAND_SET_QUANT;
const SET_SCALE: u32 = psx_hw::mdec::COMMAND_SET_SCALE;

/// DMA0 and DMA1 CHCR as every MDEC player writes them.
const CHCR_IN: u32 = 0x0100_0201;
const CHCR_OUT: u32 = 0x0100_0200;
/// Completion budget for one DMA: short, since a 32-word table or one
/// macroblock moves in microseconds, but still many times what it needs.
const DMA_SPINS: u32 = 100_000;
/// Status polls give up after this many system clocks (about 1.8 ms), well
/// short of Timer 2 wrapping.
const POLL_CLOCKS: u16 = 60_000;
/// Status reads a poll gets besides its clock bound (a read is under 20
/// clocks), so a stopped clock cannot make a wait endless.
const POLL_SPINS: u32 = 200_000;
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

/// Where a run stopped. 0 is none.
pub(crate) const STEP_SETTLE: u8 = 1;
pub(crate) const STEP_QUANT: u8 = 2;
pub(crate) const STEP_SCALE: u8 = 3;
pub(crate) const STEP_IDLE: u8 = 4;
pub(crate) const STEP_PROBE_IN: u8 = 5;
pub(crate) const STEP_PROBE_OUT: u8 = 6;

/// Status snapshot slots.
const SNAP_BEFORE: usize = 0;
const SNAP_RESET: usize = 1;
const SNAP_ENABLE: usize = 2;
const SNAP_COMMAND: usize = 3;
const SNAP_TABLES: usize = 4;
const SNAP_PROBE: usize = 5;
const SNAPS: usize = 6;

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

/// Everything the battery found.
#[derive(Copy, Clone)]
pub(crate) struct Diag {
    variants: [Variant; VARIANTS],
    /// How many times the battery has run since boot.
    batteries: u8,
    /// MDEC1 over time after a reset: from idle, from busy, and from busy
    /// with the enable written straight after.
    latency: [Trace; TRACES],
    /// The one-macroblock CPU-versus-DMA control decode.
    control: Control,
}

/// The control decode: the probe macroblock over CPU writes and reads, and
/// over DMA, with the output compared.
#[derive(Copy, Clone)]
struct Control {
    cpu_ok: bool,
    dma_ok: bool,
    cpu_words: u16,
    dma_words: u16,
    cpu_sum: u32,
    dma_sum: u32,
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

/// Reset (from idle, or from `busy`), optionally enable straight after, then
/// watch MDEC1 change.
fn trace(busy: bool, enable: bool) -> Trace {
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
    let limit = crate::bounds::scale(POLL_SPINS);
    let mut spins = 0u32;
    loop {
        let now = st();
        let clock = timers::counter(timers::Timer::Timer2);
        spins += 1;
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
        if clock >= TRACE_CLOCKS || spins > limit {
            break;
        }
    }
    irq::set_mask(mask);
    t
}

// --- Register access -------------------------------------------------------

#[inline(always)]
fn st() -> u32 {
    // SAFETY: MDEC status read.
    unsafe { psx_io::read_u32(MDEC1) }
}

#[inline(always)]
fn ctl(value: u32) {
    // SAFETY: MDEC control write.
    unsafe { psx_io::write_u32(MDEC1, value) }
}

#[inline(always)]
fn cmd(value: u32) {
    // SAFETY: MDEC command/parameter write.
    unsafe { psx_io::write_u32(MDEC0, value) }
}

fn snap(run: &mut Run, slot: usize) {
    run.snaps[slot] = st();
    run.taken |= 1 << slot;
}

/// Clocks until `status & mask == want`, counted from this call, or NEVER.
/// Timer 2 must be on the system clock (mode 0).
fn poll(mask: u32, want: u32) -> u16 {
    timers::set_counter(timers::Timer::Timer2, 0);
    let limit = crate::bounds::scale(POLL_SPINS);
    let mut spins = 0u32;
    loop {
        if st() & mask == want {
            return timers::counter(timers::Timer::Timer2);
        }
        spins += 1;
        if timers::counter(timers::Timer::Timer2) >= POLL_CLOCKS || spins > limit {
            return NEVER;
        }
    }
}

fn delay(clocks: u16) {
    timers::set_counter(timers::Timer::Timer2, 0);
    let limit = crate::bounds::scale(POLL_SPINS);
    let mut spins = 0u32;
    while timers::counter(timers::Timer::Timer2) < clocks && spins < limit {
        spins += 1;
    }
}

fn bcr(ch: Channel) -> u32 {
    // SAFETY: DMA register read.
    unsafe { psx_io::read_u32(ch.register_base() + 4) }
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
            dma::control(ch),
            bcr(ch),
            dma::address(ch),
            psx_io::read_u32(psx_hw::dma::DPCR),
            psx_io::read_u32(psx_hw::dma::DICR),
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
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_address(Channel::MdecIn, words as u32);
        dma::raw::set_size(Channel::MdecIn, dma::size_blocks(32, (count / 32) as u16));
        dma::raw::set_control(Channel::MdecIn, CHCR_IN);
    }
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
        if !dma_done(Channel::MdecIn) {
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
        let dpcr = psx_io::read_u32(psx_hw::dma::DPCR);
        psx_io::write_u32(psx_hw::dma::DPCR, (dpcr & !0xFF) | 0xBB);
    }
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_control(Channel::MdecIn, 0x0000_0201);
        dma::raw::set_control(Channel::MdecOut, 0x0000_0200);
    }
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
            run.extra =
                settled_bit | tables.enable_writes as u16 | ((tables.cpu_uploads as u16) << 8);
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

const SEQUENCES: [fn(&mut Run) -> bool; VARIANTS] = [
    variant_a, variant_b, variant_c, variant_d, variant_e, variant_f,
];

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
static mut PROBE_OUT: [u32; PROBE_OUT_WORDS] = [0; PROBE_OUT_WORDS];

fn probe(run: &mut Run) -> bool {
    // SAFETY: PROBE_OUT is only touched here, with DMA1 idle.
    let out = unsafe { &mut *addr_of_mut!(PROBE_OUT) };
    out.fill(PROBE_FILL);
    cmd(psx_hw::mdec::DECODE_15BPP | 32);
    run.probe_dreq = poll(IN_REQUEST, IN_REQUEST);
    dma_in(PROBE_IN.as_ptr(), 32);
    dma::abort(Channel::MdecOut);
    // SAFETY: single-threaded bookkeeping.
    unsafe { (*addr_of_mut!(KICK_MADR))[1] = out.as_mut_ptr() as u32 };
    // SAFETY: silicon probe: the transfer touches only memory this probe
    // owns, which stays live and untouched until the probe waits the
    // channel idle or aborts it.
    unsafe {
        dma::raw::set_address(Channel::MdecOut, out.as_mut_ptr() as u32);
        dma::raw::set_size(
            Channel::MdecOut,
            dma::size_blocks(32, (PROBE_OUT_WORDS / 32) as u16),
        );
        dma::raw::set_control(Channel::MdecOut, CHCR_OUT);
    }
    let out_done = dma_done(Channel::MdecOut);
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

/// CPU-path output of the control decode.
static mut CPU_OUT: [u32; PROBE_OUT_WORDS] = [0; PROBE_OUT_WORDS];

/// Output sum, the plain word sum the v1.26 control compared.
fn word_sum(words: &[u32]) -> u32 {
    let mut sum = 0u32;
    for &word in words {
        sum = sum.wrapping_add(word);
    }
    sum
}

/// The control: the SDK driver sets the MDEC up, the probe macroblock is
/// decoded with CPU writes and reads, then again over DMA, and the output
/// sums are recorded side by side. Equal sums mean both paths agree.
fn control() -> Control {
    let mut c = Control {
        cpu_ok: false,
        dma_ok: false,
        cpu_words: 0,
        dma_words: 0,
        cpu_sum: 0,
        dma_sum: 0,
    };
    let mask = irq::mask();
    irq::set_mask(0);
    abort_both();
    let _ = mdec::reset();
    if mdec::load_tables().is_some() {
        // SAFETY: CPU_OUT is only touched here and read after.
        let out = unsafe { &mut *addr_of_mut!(CPU_OUT) };
        out.fill(PROBE_FILL);
        cmd(psx_hw::mdec::DECODE_15BPP | 32);
        let mut wrote = true;
        for &word in PROBE_IN.iter() {
            if poll(IN_FULL, 0) == NEVER {
                wrote = false;
                break;
            }
            cmd(word);
        }
        let mut read = 0usize;
        if wrote {
            while read < PROBE_OUT_WORDS {
                if poll(psx_hw::mdec::STATUS_OUT_EMPTY, 0) == NEVER {
                    break;
                }
                // SAFETY: MDEC data read.
                out[read] = unsafe { psx_io::read_u32(MDEC0) };
                read += 1;
            }
        }
        c.cpu_ok = wrote && read == PROBE_OUT_WORDS;
        c.cpu_words = read as u16;
        c.cpu_sum = word_sum(out);
    }
    abort_both();
    let mut run = Run::new();
    let _ = mdec::reset();
    if mdec::load_tables().is_some() {
        c.dma_ok = probe(&mut run);
        // SAFETY: PROBE_OUT was written by the probe just now, DMA is idle.
        let out = unsafe { &*addr_of_mut!(PROBE_OUT) };
        c.dma_words = run.probe_words;
        c.dma_sum = word_sum(out);
    }
    abort_both();
    irq::set_mask(mask);
    c
}

/// Run every sequence RUNS times, the three traces and the control.
/// Takes Timer 2 and the MDEC; puts Timer 2's mode back.
pub(crate) fn run_battery() -> Diag {
    let timer_mode = timers::mode(timers::Timer::Timer2) & 0x03FF;
    timers::set_mode(timers::Timer::Timer2, 0);
    // SAFETY: DPCR read.
    let dpcr = unsafe { psx_io::read_u32(psx_hw::dma::DPCR) };
    let mut variants = [Variant {
        pass_mask: 0,
        run_fails: 0,
        detail: Run::new(),
        detail_run: 0,
    }; VARIANTS];
    let latency = [trace(false, false), trace(true, false), trace(true, true)];
    for (v, result) in variants.iter_mut().enumerate() {
        for r in 0..RUNS {
            crate::bounds::poll();
            let mask = irq::mask();
            irq::set_mask(0);
            // SAFETY: DPCR write, back to what the battery found.
            unsafe { psx_io::write_u32(psx_hw::dma::DPCR, dpcr) };
            abort_both();
            if v != 0 || r != 0 {
                prepare(r % 2 == 1);
            }
            let mut run = Run::new();
            snap(&mut run, SNAP_BEFORE);
            let mut ok = SEQUENCES[v](&mut run);
            if ok {
                ok = probe(&mut run);
            }
            abort_both();
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
    let control = control();
    timers::set_mode(timers::Timer::Timer2, timer_mode);
    // SAFETY: single-threaded counter.
    let batteries = unsafe {
        let b = &mut *addr_of_mut!(BATTERIES);
        *b = b.wrapping_add(1);
        *b
    };
    let diag = Diag {
        variants,
        batteries,
        latency,
        control,
    };
    print_tty(&diag);
    diag
}

// --- Capture records -------------------------------------------------------

/// Sequence v owns `0x230 + 0x10 * v` and up (v1.26: `0x200 + 0x10 * v`).
pub(crate) const FIRST_RECORD: u16 = 0x230;
/// Runs per sequence, battery count (v1.26: `0x260`).
pub(crate) const OVERVIEW_RECORD: u16 = 0x290;
/// Reset-latency trace t: `0x298 + 5 * t`, five records (v1.26: `0x290`).
pub(crate) const TRACE_RECORD: u16 = 0x298;
/// The control: `0x2A8`, three records (v1.26: `0x2A0`, a movie frame).
pub(crate) const CONTROL_RECORD: u16 = 0x2A8;
/// Halfwords a sequence carries: 29, or 41 with the timeout registers.
const STREAM_MAX: usize = 41;
/// Most records the battery writes.
pub(crate) const RECORD_MAX: usize = VARIANTS * STREAM_MAX.div_ceil(3) + 1 + TRACES * 5 + 3;

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
            let at = |i: usize| chunk.get(i).copied().unwrap_or(0) as u32;
            out[*slot] = record(id + k as u16, at(0), at(1), at(2));
            *slot += 1;
        }
    }
}

/// The diagnostic as timing-block records, unused slots marked
/// TIMING_RECORD_UNUSED. Each block is a halfword stream packed three to a
/// record (min, median, max), the last record zero-padded; u32 values go
/// low half first.
///
/// Sequence v, from `0x230 + 0x10 * v`: 0 pass mask (bit n = run n+1) | runs
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
/// `0x290`: chosen sequence (always 0xFFFF in v2.10: nothing plays behind
/// the battery), runs, battery count.
///
/// Trace t (idle, busy, busy + enable), `0x298 + 5 * t`: sample count, then
/// per sample clocks and status (u32).
///
/// Control, `0x2A8`: cpu ok | dma ok << 1 | sums equal << 2, CPU words, DMA
/// words, CPU sum (u32), DMA sum (u32).
pub(crate) fn records(diag: &Diag) -> [TimingRecord; RECORD_MAX] {
    let mut out = [TimingRecord {
        id: crate::TIMING_RECORD_UNUSED,
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
    out[slot] = record(OVERVIEW_RECORD, 0xFFFF, RUNS as u32, diag.batteries as u32);
    slot += 1;
    for (t, trace) in diag.latency.iter().enumerate() {
        let mut s = Stream::new();
        s.h(trace.count as u16);
        for i in 0..TRACE_SAMPLES {
            s.h(trace.clocks[i]);
            s.w(trace.status[i]);
        }
        s.emit(TRACE_RECORD + 5 * t as u16, &mut out, &mut slot);
    }
    let c = &diag.control;
    let mut s = Stream::new();
    s.h(c.cpu_ok as u16 | ((c.dma_ok as u16) << 1) | (((c.cpu_sum == c.dma_sum) as u16) << 2));
    s.h(c.cpu_words);
    s.h(c.dma_words);
    s.w(c.cpu_sum);
    s.w(c.dma_sum);
    s.emit(CONTROL_RECORD, &mut out, &mut slot);
    out
}

// --- TTY -------------------------------------------------------------------

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
            let snap = Line::new()
                .s(" s")
                .dec(slot as u32)
                .s("=")
                .hex(run.snaps[slot], 8);
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
    let c = &diag.control;
    let line = Line::new()
        .s("mdec-diag control cpu=")
        .dec(c.cpu_words as u32)
        .s(" dma=")
        .dec(c.dma_words as u32)
        .s(" sums=")
        .hex(c.cpu_sum, 8)
        .s("/")
        .hex(c.dma_sum, 8);
    tty::println(line.as_str());
}
