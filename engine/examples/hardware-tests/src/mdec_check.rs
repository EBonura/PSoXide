// SPDX-License-Identifier: GPL-2.0-or-later
//! MDEC: a short decode check, not a movie.
//!
//! v1.25's FMV test never started on a PAL SCPH-9002: the SDK player's DMA0
//! upload of the MDEC tables did not finish, because the DMA-request enable
//! written right behind the reset was lost to a reset still in progress. v1.26
//! tried six setup sequences to find out why; the SDK's own driver
//! (`psx_fmv::mdec::reset` plus `load_tables`) is the one that holds, so it is
//! the only one left. This runs it twice (from an idle MDEC, and after a reset
//! in the middle of a table command), decodes one colour macroblock over
//! DMA0/DMA1 each time, and traces MDEC1 over the first 5,000 system clocks
//! after a reset from idle and from busy. A few hundred microseconds in all.
//!
//! Records `0x420`-`0x426`:
//! * `0x420` mdec_setup: runs that worked (bit 0 idle, bit 1 mid-command),
//!   driver notes of the idle run (bit 15 reset settled, bits 8-14 CPU
//!   uploads, bits 0-7 enable writes), failing steps (idle low nibble, busy
//!   high nibble).
//! * `0x421` mdec_probe: output words that arrived of 128 (idle run), whether
//!   they were all equal, the first word's low half.
//! * `0x422` mdec_probe_word: the first output word's high half, clocks from
//!   the decode command to data-in request, the busy run's words arrived.
//! * `0x423` mdec_status: MDEC1 after the reset, after the tables, after the
//!   probe (each the high half; the low half is the FIFO state).
//! * `0x424`-`0x425` mdec_trace_idle / mdec_trace_busy: samples taken, the
//!   status's high half at the last change, clocks of the last change.
//! * `0x426` mdec_timeout: CHCR of the channel at the first DMA timeout, its
//!   BCR high half and MADR low half; all zero when nothing timed out.

use crate::console_tests::record;
use crate::TimingRecord;
use core::ptr::addr_of_mut;
use psx_fmv::mdec;
use psx_io::dma::{self, Channel};
use psx_io::{irq, timers};

const MDEC0: u32 = 0x1F80_1820;
const MDEC1: u32 = 0x1F80_1824;
const RESET: u32 = psx_hw::mdec::CONTROL_RESET;
const IN_REQUEST: u32 = psx_hw::mdec::STATUS_IN_REQUEST;
const SET_QUANT: u32 = psx_hw::mdec::COMMAND_SET_QUANT;

/// rec mdec_setup: worked_mask, driver_notes, failing_steps
pub(crate) const MDEC_SETUP: u16 = 0x420;

/// DMA0 and DMA1 CHCR as every MDEC player writes them.
const CHCR_IN: u32 = 0x0100_0201;
const CHCR_OUT: u32 = 0x0100_0200;
const DMA_SPINS: u32 = 100_000;
/// Status polls give up after this many system clocks (about 1.8 ms).
const POLL_CLOCKS: u16 = 60_000;
const NEVER: u16 = 0xFFFF;
const PREP_DELAY: u16 = 2_000;
const PREP_WORDS: usize = 5;
const TRACE_SAMPLES: usize = 4;
const TRACE_CLOCKS: u16 = 5_000;

const STEP_QUANT: u8 = 2;
const STEP_PROBE_IN: u8 = 5;
const STEP_PROBE_OUT: u8 = 6;

/// 16x16 pixels at 15bpp.
const PROBE_OUT_WORDS: usize = 128;
/// Fill for the output buffer, so a word the DMA never wrote is visible.
const PROBE_FILL: u32 = 0x5A5A_A5A5;

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
static mut PROBE_OUT: [u32; PROBE_OUT_WORDS] = [0; PROBE_OUT_WORDS];
/// MADR of the last kick on each MDEC channel, for words-moved at a timeout.
static mut KICK_MADR: [u32; 2] = [0; 2];

#[derive(Copy, Clone)]
struct Pass {
    fail: u8,
    notes: u16,
    status_reset: u32,
    status_tables: u32,
    status_probe: u32,
    probe_dreq: u16,
    probe_words: u16,
    probe_flat: bool,
    probe_word: u32,
    timeout: [u32; 3],
    timed_out: bool,
}

impl Pass {
    const fn new() -> Self {
        Self {
            fail: 0,
            notes: 0,
            status_reset: 0,
            status_tables: 0,
            status_probe: 0,
            probe_dreq: NEVER,
            probe_words: 0,
            probe_flat: false,
            probe_word: 0,
            timeout: [0; 3],
            timed_out: false,
        }
    }
}

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

fn record_timeout(pass: &mut Pass, ch: Channel) {
    if pass.timed_out {
        return;
    }
    pass.timed_out = true;
    // SAFETY: DMA register reads; single thread.
    pass.timeout = unsafe {
        [
            dma::control(ch),
            psx_io::read_u32(ch.register_base() + 4),
            dma::address(ch),
        ]
    };
    let _ = unsafe { (*addr_of_mut!(KICK_MADR))[ch as usize & 1] };
}

fn abort_both() {
    dma::abort(Channel::MdecIn);
    dma::abort(Channel::MdecOut);
}

/// Put the MDEC in the state a pass starts from: idle, or `busy` in the
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

/// The SDK driver: reset, tables.
fn setup(pass: &mut Pass) -> bool {
    let settled = mdec::reset();
    pass.status_reset = st();
    let settled_bit = if settled { 0x8000 } else { 0 };
    match mdec::load_tables() {
        Some(tables) => {
            pass.notes =
                settled_bit | tables.enable_writes as u16 | ((tables.cpu_uploads as u16) << 8);
            pass.status_tables = st();
            true
        }
        None => {
            pass.notes = settled_bit;
            pass.fail = STEP_QUANT;
            record_timeout(pass, Channel::MdecIn);
            pass.status_tables = st();
            false
        }
    }
}

fn probe(pass: &mut Pass) -> bool {
    // SAFETY: PROBE_OUT is only touched here, with DMA1 idle.
    let out = unsafe { &mut *addr_of_mut!(PROBE_OUT) };
    out.fill(PROBE_FILL);
    cmd(psx_hw::mdec::DECODE_15BPP | 32);
    pass.probe_dreq = poll(IN_REQUEST, IN_REQUEST);
    dma::abort(Channel::MdecIn);
    // SAFETY: single-threaded bookkeeping.
    unsafe { (*addr_of_mut!(KICK_MADR))[0] = PROBE_IN.as_ptr() as u32 };
    // SAFETY: the transfers touch only memory this probe owns, which stays
    // live until the probe waits the channels idle or aborts them.
    unsafe {
        dma::raw::set_address(Channel::MdecIn, PROBE_IN.as_ptr() as u32);
        dma::raw::set_size(Channel::MdecIn, dma::size_blocks(32, 1));
        dma::raw::set_control(Channel::MdecIn, CHCR_IN);
    }
    dma::abort(Channel::MdecOut);
    // SAFETY: as above.
    unsafe {
        (*addr_of_mut!(KICK_MADR))[1] = out.as_mut_ptr() as u32;
        dma::raw::set_address(Channel::MdecOut, out.as_mut_ptr() as u32);
        dma::raw::set_size(
            Channel::MdecOut,
            dma::size_blocks(32, (PROBE_OUT_WORDS / 32) as u16),
        );
        dma::raw::set_control(Channel::MdecOut, CHCR_OUT);
    }
    let out_done = dma::wait_done(Channel::MdecOut, DMA_SPINS);
    let in_done = dma::wait_done(Channel::MdecIn, DMA_SPINS);
    pass.status_probe = st();
    pass.probe_words = out.iter().filter(|&&w| w != PROBE_FILL).count() as u16;
    pass.probe_word = out[0];
    pass.probe_flat = out.iter().all(|&w| w == out[0]);
    if !in_done {
        record_timeout(pass, Channel::MdecIn);
        pass.fail = STEP_PROBE_IN;
        return false;
    }
    if !out_done {
        record_timeout(pass, Channel::MdecOut);
        pass.fail = STEP_PROBE_OUT;
        return false;
    }
    true
}

/// One pass: prepare (idle or busy), set up with the SDK driver, decode.
fn one_pass(busy: bool) -> (Pass, bool) {
    let mut pass = Pass::new();
    abort_both();
    prepare(busy);
    let mut ok = setup(&mut pass);
    if ok {
        ok = probe(&mut pass);
    }
    abort_both();
    (pass, ok)
}

/// MDEC1 samples after one reset: the first read, then every change.
/// Returns (samples, status at the last change, clocks of the last change).
fn trace(busy: bool) -> (u32, u32, u32) {
    abort_both();
    prepare(busy);
    let mut count = 1u32;
    timers::set_counter(timers::Timer::Timer2, 0);
    ctl(RESET);
    let mut last = st();
    let mut last_status = last;
    let mut last_clock = timers::counter(timers::Timer::Timer2);
    loop {
        let now = st();
        let clock = timers::counter(timers::Timer::Timer2);
        if now != last {
            last = now;
            last_status = now;
            last_clock = clock;
            count += 1;
            if count as usize == TRACE_SAMPLES {
                break;
            }
        }
        if clock >= TRACE_CLOCKS {
            break;
        }
    }
    (count, last_status, last_clock as u32)
}

/// Run the check. Takes Timer 2 and the MDEC.
pub(crate) fn run() -> [TimingRecord; 7] {
    let mask = irq::mask();
    irq::set_mask(0);
    // SAFETY: DPCR read; the channels are enabled by the SDK driver.
    dma::enable_channel(Channel::MdecIn);
    dma::enable_channel(Channel::MdecOut);
    timers::set_mode(timers::Timer::Timer2, 0);
    let (idle, idle_ok) = one_pass(false);
    let (busy, busy_ok) = one_pass(true);
    let trace_idle = trace(false);
    let trace_busy = trace(true);
    abort_both();
    irq::set_mask(mask);
    let pick = if idle.timed_out { &idle } else { &busy };
    [
        record(
            MDEC_SETUP,
            idle_ok as u32 | ((busy_ok as u32) << 1),
            idle.notes as u32,
            (idle.fail & 0xF) as u32 | (((busy.fail & 0xF) as u32) << 4),
        ),
        record(
            MDEC_SETUP + 1,
            idle.probe_words as u32,
            idle.probe_flat as u32,
            idle.probe_word & 0xFFFF,
        ),
        record(
            MDEC_SETUP + 2,
            idle.probe_word >> 16,
            idle.probe_dreq as u32,
            busy.probe_words as u32,
        ),
        record(
            MDEC_SETUP + 3,
            idle.status_reset >> 16,
            idle.status_tables >> 16,
            idle.status_probe >> 16,
        ),
        record(
            MDEC_SETUP + 4,
            trace_idle.0,
            trace_idle.1 >> 16,
            trace_idle.2,
        ),
        record(
            MDEC_SETUP + 5,
            trace_busy.0,
            trace_busy.1 >> 16,
            trace_busy.2,
        ),
        record(
            MDEC_SETUP + 6,
            pick.timeout[0] & 0xFFFF,
            pick.timeout[1] >> 16,
            pick.timeout[2] & 0xFFFF,
        ),
    ]
}
