// SPDX-License-Identifier: GPL-2.0-or-later
//! SPU bulk upload: how fast RAM-to-SPU DMA runs, and whether a bank the size
//! of WipEout's (471,040 bytes) lands whole, in one transfer and through the
//! SDK's upload (4 KiB pieces, each waited out).
//!
//! Why: the SDK used to send an upload as one transfer behind a fixed
//! 500,000-spin wait. Record 0x65 says the console moves a halfword every 32.5
//! clocks, so that wait gave up after about 338 KB and the rest of the bank
//! never arrived (WipEout's countdown on the 11-19 console session); the
//! emulator, at 16 clocks a halfword then, landed it whole. This step measures
//! the rate with the poll count beside it (clocks per poll, so the budget can be
//! checked against silicon) and shows, per kilobyte, how much of each upload is
//! really in SPU RAM afterwards.
//!
//! The source is the program's own image from `0x8001_0000`: no 460 KB buffer
//! is needed, and its hash before and after (`0x9B5`) shows it did not change.
//! The destination is cleared before each upload so a lost tail cannot hide
//! behind the previous upload's copy. The one-transfer wait is bounded far
//! past any healthy console (`ONE_DMA_POLL_LIMIT`). Nothing here passes or
//! fails; the records are read against the console.
//!
//! * `0x9B0` clocks for 1,024 halfwords (one 2 KiB transfer, Timer 2), polls of
//!   the channel's busy bit in that time, the halfword count.
//! * `0x9B1` the whole bank in one transfer: HBlanks, polls (low, high).
//! * `0x9B2` its check: first kilobyte that differs (`0xFFFF` none), kilobytes
//!   that match, kilobytes sent.
//! * `0x9B3` the same bank through `psx_spu::upload_adpcm`: HBlanks, 1 if it
//!   returned a timeout, kilobytes it reports landed (all of them on success).
//! * `0x9B4` its check, as `0x9B2`.
//! * `0x9B5` the source: 1 if its hash changed over the step, the hash's low
//!   half, 0.

use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
use psx_io::dma;
use psx_io::timers::{self, Timer};
use psx_spu::SpuAddr;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const RATE_RECORD: u16 = 0x9B0;
const ONE_RECORD: u16 = 0x9B1;
const ONE_CHECK_RECORD: u16 = 0x9B2;
const PIECES_RECORD: u16 = 0x9B3;
const PIECES_CHECK_RECORD: u16 = 0x9B4;
const SOURCE_RECORD: u16 = 0x9B5;

/// Bytes of the bank: 460 KiB, WipEout's 470,544 rounded up to a kilobyte.
const BANK_BYTES: u32 = 460 * 1024;
const BANK_WORDS: u32 = BANK_BYTES / 4;
/// Where the source starts: the program image (static while the step runs).
const SOURCE: u32 = 0x8001_0000;
/// Where it goes in SPU RAM: above the capture buffers, below the parked
/// reverb work area.
const DEST: u32 = 0x1010;
/// The one-transfer wait: about 40 times the console's transfer time at the
/// SDK loop's 11 clocks a poll. Only a stuck channel reaches it.
const ONE_DMA_POLL_LIMIT: u32 = 30_000_000;
/// Timer 1 on the HBlank clock.
const HBLANK_CLOCK: u16 = 0x0100;

/// A kilobyte of zeros, the clearing source.
static ZEROS: [u32; 256] = [0; 256];

fn source() -> &'static [u32] {
    // SAFETY: the program image from 0x8001_0000 is mapped RAM the step only
    // reads; it is not written while the step runs (0x9B5 checks).
    unsafe { core::slice::from_raw_parts(SOURCE as *const u32, BANK_WORDS as usize) }
}

fn fnv_words(mut hash: u32, words: &[u32]) -> u32 {
    for &word in words {
        hash = (hash ^ (word & 0xFFFF)).wrapping_mul(0x0100_0193);
        hash = (hash ^ (word >> 16)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Zero the bank's range of SPU RAM, a kilobyte at a time.
fn clear_bank() {
    let bytes =
        // SAFETY: viewing the static zero words as bytes.
        unsafe { core::slice::from_raw_parts(ZEROS.as_ptr().cast::<u8>(), 1024) };
    for kb in 0..BANK_BYTES / 1024 {
        #[allow(deprecated)]
        let _ = psx_spu::upload_adpcm(SpuAddr::new(DEST + kb * 1024), bytes);
        crate::bounds::heartbeat();
    }
}

/// Compare the bank's range of SPU RAM with the source, a kilobyte at a time:
/// the first kilobyte that differs (0xFFFF for none) and how many match.
fn check_bank() -> (u32, u32) {
    let source = source();
    let mut back = [0u32; 256];
    let (mut first_bad, mut matching) = (0xFFFF, 0);
    for kb in 0..BANK_BYTES / 1024 {
        crate::spu_dma_read(DEST + kb * 1024, &mut back);
        let at = (kb * 256) as usize;
        if back[..] == source[at..at + 256] {
            matching += 1;
        } else if first_bad == 0xFFFF {
            first_bad = kb;
        }
    }
    (first_bad, matching)
}

/// Put the SPU in DMA-write mode at `dest` and wait for it to take the mode.
fn open_dma_write(dest: u32) -> u16 {
    // SAFETY: SPU register stores, the sequence psx-spu's upload makes.
    unsafe {
        let stopped = psx_io::read_u16(SPUCNT) & !0x0030;
        psx_io::write_u16(SPUCNT, stopped);
        wait_mode(stopped);
        psx_io::write_u16(TRANSFER_CTRL, 0x0004);
        psx_io::write_u16(TRANSFER_ADDR, (dest / 8) as u16);
        psx_io::write_u16(SPUCNT, stopped | 0x0020);
        wait_mode(stopped | 0x0020);
        stopped
    }
}

fn wait_mode(want: u16) {
    let mut guard = 0u32;
    // SAFETY: a status read.
    while unsafe { psx_io::read_u16(SPUSTAT) } & 0x003F != want & 0x003F && guard < 100_000 {
        guard += 1;
    }
}

/// Let the SPU write its FIFO out, then back to Stop.
fn close_dma_write(stopped: u16) {
    let mut guard = 0u32;
    // SAFETY: a status read, then the mode store.
    unsafe {
        while psx_io::read_u16(SPUSTAT) & 0x0400 != 0 && guard < 100_000 {
            guard += 1;
        }
        for _ in 0..256 {
            let _ = psx_io::read_u16(SPUSTAT);
        }
        psx_io::write_u16(SPUCNT, stopped);
    }
}

/// One block-sync transfer of `words` from `from` to SPU RAM at `dest`, waited
/// out against `limit` polls. Returns the polls it took (`limit` if it never
/// finished; the channel is then aborted).
fn one_transfer(from: u32, words: u32, dest: u32, limit: u32) -> u32 {
    let stopped = open_dma_write(dest);
    dma::enable_channel(dma::Channel::Spu);
    // SAFETY: the transfer reads `words` words from `from`, mapped RAM that
    // nothing writes while the wait below runs; the wait (or its abort) ends
    // the transfer before this returns.
    unsafe {
        dma::raw::set_address(dma::Channel::Spu, from);
        dma::raw::set_size(dma::Channel::Spu, dma::size_blocks(16, (words / 16) as u16));
        dma::raw::set_control(
            dma::Channel::Spu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );
    }
    let mut polls = 0u32;
    while dma::is_busy(dma::Channel::Spu) && polls < limit {
        polls += 1;
    }
    if polls == limit {
        dma::abort(dma::Channel::Spu);
    }
    close_dma_write(stopped);
    polls
}

pub(crate) fn run(records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(RATE_RECORD);
    let before = fnv_words(0x811C_9DC5, source());

    // The rate: 2 KiB, short enough for Timer 2 on the system clock.
    timers::set_mode(Timer::Timer2, 0);
    timers::set_counter(Timer::Timer2, 0);
    let polls = one_transfer(SOURCE, 512, DEST, 1_000_000);
    let clocks = timers::counter(Timer::Timer2) as u32;
    push_timing_record(records, next, record(RATE_RECORD, clocks, polls, 1024));

    // The whole bank in one transfer.
    crate::bounds::record_start(ONE_RECORD);
    clear_bank();
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let polls = one_transfer(SOURCE, BANK_WORDS, DEST, ONE_DMA_POLL_LIMIT);
    let hblanks = timers::counter(Timer::Timer1) as u32;
    push_timing_record(
        records,
        next,
        record(ONE_RECORD, hblanks, polls & 0xFFFF, polls >> 16),
    );
    let (first_bad, matching) = check_bank();
    push_timing_record(
        records,
        next,
        record(ONE_CHECK_RECORD, first_bad, matching, BANK_BYTES / 1024),
    );

    // The same bank through the SDK's upload.
    crate::bounds::record_start(PIECES_RECORD);
    clear_bank();
    // SAFETY: viewing the source words as bytes.
    let bytes =
        unsafe { core::slice::from_raw_parts(SOURCE as *const u8, BANK_BYTES as usize) };
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    #[allow(deprecated)]
    let result = psx_spu::upload_adpcm(SpuAddr::new(DEST), bytes);
    let hblanks = timers::counter(Timer::Timer1) as u32;
    let (timed_out, landed) = match result {
        Ok(()) => (0, BANK_BYTES),
        Err(timeout) => (1, timeout.landed_bytes),
    };
    push_timing_record(
        records,
        next,
        record(PIECES_RECORD, hblanks, timed_out, landed / 1024),
    );
    let (first_bad, matching) = check_bank();
    push_timing_record(
        records,
        next,
        record(PIECES_CHECK_RECORD, first_bad, matching, BANK_BYTES / 1024),
    );

    let after = fnv_words(0x811C_9DC5, source());
    push_timing_record(
        records,
        next,
        record(SOURCE_RECORD, u32::from(before != after), after & 0xFFFF, 0),
    );
}
