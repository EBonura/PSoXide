// SPDX-License-Identifier: GPL-2.0-or-later
//! SPU BULK UPLOAD: does a bank the size of WipEout's (470,544 bytes) land in
//! SPU RAM, through the SDK's old upload and through the new one, and how fast
//! does RAM-to-SPU DMA really run.
//!
//! Why: on the 2026-11-19 console session WipEout's countdown and weapon-name
//! voices were silent while every emulator played them. The SDK sent an upload
//! as one DMA behind a fixed 500,000-spin wait and aborted the channel when the
//! wait ran out; record 0x65 puts the console at about 32.5 clocks a halfword,
//! so that wait would give up after about 338 KB. The SDK now sends 4 KiB
//! pieces, each waited out against a budget sized from its length. This step
//! measures the cut of the old path, checks the new one, and asks whether
//! clearing CHCR's start bit really stops a block-sync SPU DMA (the old path's
//! loss depends on it).
//!
//! The bank is the 470,544 bytes of the program image that end with
//! [`TONE`], a short square-wave sample that loops on itself: whatever lands
//! of the bank, its very last bytes are the tone. After each path the tone is
//! keyed from where it should be, for two seconds, with a label on screen: a
//! cut bank leaves zeros there and the tone is silent. SPU RAM is zeroed before
//! each upload, so a lost tail cannot hide behind the previous upload.
//!
//! Records (INFO only; byte counts over 16 bits are split, `lo` then `hi`):
//!
//! * `0x9B0` 512 halfwords (1 KiB) in one transfer timed on Timer 2: clocks,
//!   polls of the busy bit in that time, 512.
//! * `0x9B1` the OLD path (one transfer, block 4 words x 29,409, the old SDK's
//!   `wait_or_abort` with 500,000 spins): bytes landed `lo`, `hi` (the offset
//!   of the first byte of SPU RAM that differs from the bank: the cut), flags
//!   (bit 0 the first wait ran out and the channel was aborted, bit 1 the
//!   channel still read busy after the second wait).
//! * `0x9B2` the OLD path: HBlanks it took, kilobytes that match, kilobytes sent.
//! * `0x9B3` the NEW path (`psx_spu::upload_adpcm`): bytes landed `lo`, `hi`,
//!   flags (bit 0 it returned a timeout).
//! * `0x9B4` the NEW path: HBlanks, kilobytes that match, kilobytes sent.
//! * `0x9B5` the bank in one transfer waited out (up to 30 million polls):
//!   HBlanks, polls `lo`, `hi`. Clocks per halfword are HBlanks times the line
//!   length over 235,272.
//! * `0x9B6` CHCR abort: the bank in one transfer, CHCR written 0 after 100,000
//!   polls: 16-byte units the channel had read (MADR) at the abort, 16-byte
//!   units it had read 1,000,000 CHCR reads later, flags (bit 0 busy right after the
//!   abort, bit 1 busy after the wait).
//! * `0x9B7` CHCR abort: 16-byte units in SPU RAM afterwards (first differing
//!   offset / 16), BCR's block count at the abort, BCR's block count after the
//!   wait.
//! * `0x9B8` the bank's source: 1 if its hash changed over the step, the hash's
//!   low half, 1 if the bank's window fit the program image (0: nothing ran).

use crate::console_tests::record;
use crate::ui::{self, Line};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_font::FontAtlas;
use psx_hw::spu::{SPUCNT, SPUSTAT, TRANSFER_ADDR, TRANSFER_CTRL};
use psx_io::dma::{self, Channel};
use psx_io::timers::{self, Timer};
use psx_rt::interrupts;
use psx_spu::{Adsr, SpuAddr, Voice, Volume};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const RATE_RECORD: u16 = 0x9B0;
const OLD_RECORD: u16 = 0x9B1;
const OLD_CHECK_RECORD: u16 = 0x9B2;
const NEW_RECORD: u16 = 0x9B3;
const NEW_CHECK_RECORD: u16 = 0x9B4;
const LARGE_RECORD: u16 = 0x9B5;
const ABORT_RECORD: u16 = 0x9B6;
const ABORT_CHECK_RECORD: u16 = 0x9B7;
const SOURCE_RECORD: u16 = 0x9B8;

/// WipEout's bank.
const BANK_BYTES: u32 = 470_544;
const BANK_WORDS: u32 = BANK_BYTES / 4;
/// The old SDK's block shape for it: the largest block of at most 16 words
/// that divides 117,636 words.
const OLD_BLOCK_WORDS: u16 = 4;
/// The old SDK's wait.
const OLD_SPINS: u32 = 500_000;
/// Where the bank goes in SPU RAM, as in WipEout.
const DEST: u32 = 0x1010;
/// Kilobytes zeroed before each upload (covers the bank).
const CLEAR_KB: u32 = BANK_BYTES.div_ceil(1024);
/// The one-transfer waits that measure the hardware: far past any console.
const LONG_POLLS: u32 = 30_000_000;
/// Polls before the CHCR abort, and polls watched after it.
const ABORT_AFTER: u32 = 100_000;
const ABORT_WATCH: u32 = 1_000_000;
/// Timer 1 on the HBlank clock.
const HBLANK_CLOCK: u16 = 0x0100;
/// System clocks per NTSC line (33,868,800 / 15,734), for the on-screen rate.
const CLOCKS_PER_LINE: u32 = 2_153;
/// Frames each tone sounds for.
const TONE_FRAMES: u32 = 120;

/// The bank's last 64 bytes: four ADPCM blocks of a square wave (shift 0,
/// filter 0, nibbles +7 then -8), the first marked loop start, the last end
/// and repeat, so a voice keyed on it plays a steady tone.
#[repr(C, align(16))]
struct Tone([u8; 64]);

/// In the cold text, the last thing in `.text`: the 470,544 bytes before it are
/// code, which nothing writes while the step runs (`.rodata` sits with `.data`,
/// after the program's mutable statics, so a window ending there would not be
/// static).
#[link_section = ".psx_cold.spu_bulk_tone"]
static TONE: Tone = Tone([
    0x00, 0x04, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
    0x00, 0x00, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
    0x00, 0x00, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
    0x00, 0x03, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,
]);

/// A kilobyte of zeros, the clearing source.
static ZEROS: [u32; 256] = [0; 256];

/// The bank's window of the program image, ending with [`TONE`]. `None` when
/// the tone sits too low in the image for a whole bank before it.
#[optimize(size)]
#[inline(never)]
fn bank() -> Option<&'static [u32]> {
    let end = TONE.0.as_ptr() as u32 + TONE.0.len() as u32;
    let start = end.checked_sub(BANK_BYTES)?;
    if start < 0x8001_0000 || start % 4 != 0 {
        return None;
    }
    // SAFETY: the window lies inside the loaded program image (text and
    // read-only data, ending with the static tone) and is only read; 0x9B8
    // checks it did not change while the step ran.
    Some(unsafe { core::slice::from_raw_parts(start as *const u32, BANK_WORDS as usize) })
}

#[optimize(size)]
#[inline(never)]
fn fnv_words(words: &[u32]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &word in words {
        hash = (hash ^ (word & 0xFFFF)).wrapping_mul(0x0100_0193);
        hash = (hash ^ (word >> 16)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Zero the bank's range of SPU RAM, a kilobyte at a time.
#[optimize(size)]
#[inline(never)]
fn clear_bank() {
    // SAFETY: viewing the static zero words as bytes.
    let bytes = unsafe { core::slice::from_raw_parts(ZEROS.as_ptr().cast::<u8>(), 1024) };
    for kb in 0..CLEAR_KB {
        crate::probe_upload(SpuAddr::new(DEST + kb * 1024), bytes);
    }
    crate::bounds::heartbeat();
}

/// Compare SPU RAM with the bank: the offset of the first byte that differs
/// (the bank's length when none does) and the whole kilobytes that match.
#[optimize(size)]
#[inline(never)]
fn landed(bank: &[u32]) -> (u32, u32) {
    let mut back = [0u32; 256];
    let mut first = BANK_BYTES;
    let mut matching = 0;
    let mut at = 0u32;
    while at < BANK_WORDS {
        let words = (BANK_WORDS - at).min(256);
        let chunk = &mut back[..words as usize];
        crate::spu_dma_read(DEST + at * 4, chunk);
        let source = &bank[at as usize..(at + words) as usize];
        match chunk.iter().zip(source).position(|(a, b)| a != b) {
            None => {
                if words == 256 {
                    matching += 1;
                }
            }
            Some(i) => {
                if first == BANK_BYTES {
                    let byte = if chunk[i] & 0xFFFF == source[i] & 0xFFFF { 2 } else { 0 };
                    first = (at + i as u32) * 4 + byte;
                }
            }
        }
        at += words;
    }
    (first, matching)
}

/// Put the SPU in DMA-write mode at `dest` and wait for it to take the mode.
#[optimize(size)]
#[inline(never)]
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

#[optimize(size)]
#[inline(never)]
fn wait_mode(want: u16) {
    let mut guard = 0u32;
    // SAFETY: a status read.
    while unsafe { psx_io::read_u16(SPUSTAT) } & 0x003F != want & 0x003F && guard < 100_000 {
        guard += 1;
    }
}

/// Let the SPU write its FIFO out, then back to Stop.
#[optimize(size)]
#[inline(never)]
fn close_dma_write(stopped: u16) {
    let mut guard = 0u32;
    // SAFETY: status reads, then the mode store.
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

/// Start one block-sync transfer of `blocks` blocks of `block_words` words
/// from `from`.
///
/// # Safety
///
/// `from` holds `blocks * block_words` readable words that nothing writes
/// until the caller has waited the transfer out or aborted it.
#[optimize(size)]
#[inline(never)]
unsafe fn kick(from: u32, block_words: u16, blocks: u16) {
    dma::enable_channel(Channel::Spu);
    // SAFETY: as this function's contract.
    unsafe {
        dma::raw::set_address(Channel::Spu, from);
        dma::raw::set_size(Channel::Spu, dma::size_blocks(block_words, blocks));
        dma::raw::set_control(
            Channel::Spu,
            psx_hw::dma::CHCR_TO_DEVICE | psx_hw::dma::CHCR_SYNC_BLOCK | psx_hw::dma::CHCR_START,
        );
    }
}

#[optimize(size)]
#[inline(never)]
fn poll_until_idle(limit: u32) -> u32 {
    let mut polls = 0u32;
    while dma::is_busy(Channel::Spu) && polls < limit {
        polls += 1;
    }
    polls
}

#[optimize(size)]
#[inline(never)]
fn bcr_blocks() -> u32 {
    // SAFETY: a read of the SPU channel's size register.
    unsafe { psx_io::read_u32(Channel::Spu.register_base() + psx_hw::dma::BCR) >> 16 }
}

#[optimize(size)]
#[inline(never)]
fn start_hblanks() {
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
}

#[optimize(size)]
#[inline(never)]
fn hblanks() -> u32 {
    timers::counter(Timer::Timer1) as u32
}

/// Key the tone where the bank's end should be, for [`TONE_FRAMES`], under a
/// label, then silence it.
#[optimize(size)]
#[inline(never)]
fn play_tail(font: &FontAtlas, label: &str) {
    ui::live_line(font, 8, label, (255, 220, 80));
    // The area reset (`run::silence_spu`) leaves the main volume at 0: without
    // this the voice plays into silence (v2.24 on the console, 2026-11-19).
    psx_spu::set_main_volume(Volume::MAX, Volume::MAX);
    let voice = Voice::V0;
    voice.configure_sample(
        SpuAddr::new(DEST + BANK_BYTES - TONE.0.len() as u32),
        22_050,
        Volume(0x2000),
        Adsr::default_tone(),
    );
    Voice::start(voice.mask());
    let begin = interrupts::vblank_count();
    let mut guard = 0u32;
    while interrupts::vblank_count().wrapping_sub(begin) < TONE_FRAMES && guard < 200_000_000 {
        guard += 1;
    }
    Voice::release(voice.mask());
    voice.set_volume(Volume::SILENCE, Volume::SILENCE);
    psx_spu::set_main_volume(Volume::SILENCE, Volume::SILENCE);
    ui::live_line(font, 8, "", (255, 220, 80));
}

/// The result of a path, on the live band.
#[optimize(size)]
#[inline(never)]
fn show(font: &FontAtlas, row: u32, name: &str, landed: u32, hblanks: u32) {
    let mut line = Line::new();
    line.s(name)
        .s(" LANDED ")
        .u(landed)
        .s(" OF ")
        .u(BANK_BYTES)
        .s(" B, ")
        .u(hblanks * CLOCKS_PER_LINE / 1000)
        .s("K CLK");
    let colour = if landed == BANK_BYTES { (120, 230, 120) } else { (255, 110, 90) };
    ui::live_line(font, row, line.as_str(), colour);
}

#[optimize(size)]
#[inline(never)]
pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(SOURCE_RECORD);
    let Some(bank) = bank() else {
        push_timing_record(records, next, record(SOURCE_RECORD, 0, 0, 0));
        return;
    };
    let from = bank.as_ptr() as u32;
    let before = fnv_words(bank);

    // (3a) The rate: 512 halfwords, Timer 2 on the system clock.
    crate::bounds::record_start(RATE_RECORD);
    let stopped = open_dma_write(DEST);
    timers::set_mode(Timer::Timer2, 0);
    timers::set_counter(Timer::Timer2, 0);
    // SAFETY: 256 words of the bank, read only.
    unsafe { kick(from, 16, 16) };
    let polls = poll_until_idle(1_000_000);
    let clocks = timers::counter(Timer::Timer2) as u32;
    if polls == 1_000_000 {
        dma::abort(Channel::Spu);
    }
    close_dma_write(stopped);
    push_timing_record(records, next, record(RATE_RECORD, clocks, polls, 512));
    let mut line = Line::new();
    line.s("RATE ").u(clocks * 10 / 512).s(" /10 CLK PER HALFWORD, ");
    line.u(if polls == 0 { 0 } else { clocks * 10 / polls }).s(" /10 CLK PER POLL");
    ui::live_line(font, 0, line.as_str(), (220, 224, 230));

    // (1) The OLD path: one transfer, the old 500,000-spin wait and abort.
    crate::bounds::record_start(OLD_RECORD);
    clear_bank();
    let stopped = open_dma_write(DEST);
    start_hblanks();
    // SAFETY: the bank, read only; the wait or its abort ends the transfer.
    unsafe { kick(from, OLD_BLOCK_WORDS, (BANK_WORDS / u32::from(OLD_BLOCK_WORDS)) as u16) };
    // Exactly the old SDK's wait: 500,000 spins, then an abort and a second
    // 500,000-spin wait for the channel to read idle.
    let done = dma::wait_or_abort(Channel::Spu, OLD_SPINS);
    let still_busy = dma::is_busy(Channel::Spu);
    let lines = hblanks();
    close_dma_write(stopped);
    let (old_landed, old_kb) = landed(bank);
    push_timing_record(
        records,
        next,
        record(
            OLD_RECORD,
            old_landed & 0xFFFF,
            old_landed >> 16,
            u32::from(!done) | (u32::from(still_busy) << 1),
        ),
    );
    push_timing_record(records, next, record(OLD_CHECK_RECORD, lines, old_kb, CLEAR_KB));
    show(font, 1, "OLD SDK PATH", old_landed, lines);
    play_tail(font, "OLD SDK PATH: TONE AT THE END (SILENT = CUT)");

    // (2) The NEW path: the SDK's upload.
    crate::bounds::record_start(NEW_RECORD);
    clear_bank();
    // SAFETY: viewing the bank's words as bytes.
    let bytes = unsafe { core::slice::from_raw_parts(from as *const u8, BANK_BYTES as usize) };
    start_hblanks();
    #[allow(deprecated)]
    let result = psx_spu::upload_adpcm(SpuAddr::new(DEST), bytes);
    let lines = hblanks();
    let (new_landed, new_kb) = landed(bank);
    push_timing_record(
        records,
        next,
        record(
            NEW_RECORD,
            new_landed & 0xFFFF,
            new_landed >> 16,
            u32::from(result.is_err()),
        ),
    );
    push_timing_record(records, next, record(NEW_CHECK_RECORD, lines, new_kb, CLEAR_KB));
    show(font, 2, "NEW SDK PATH", new_landed, lines);
    play_tail(font, "NEW SDK PATH: TONE AT THE END (SHOULD SOUND)");

    // (3b) The rate over the whole bank, waited out.
    crate::bounds::record_start(LARGE_RECORD);
    let stopped = open_dma_write(DEST);
    start_hblanks();
    // SAFETY: the bank, read only; the wait or its abort ends the transfer.
    unsafe { kick(from, OLD_BLOCK_WORDS, (BANK_WORDS / u32::from(OLD_BLOCK_WORDS)) as u16) };
    let polls = poll_until_idle(LONG_POLLS);
    let lines = hblanks();
    if polls == LONG_POLLS {
        dma::abort(Channel::Spu);
    }
    close_dma_write(stopped);
    push_timing_record(
        records,
        next,
        record(LARGE_RECORD, lines, polls & 0xFFFF, polls >> 16),
    );
    let mut line = Line::new();
    line.s("WHOLE BANK ").u(lines * CLOCKS_PER_LINE * 10 / (BANK_BYTES / 2)).s(" /10 CLK PER HALFWORD");
    ui::live_line(font, 3, line.as_str(), (220, 224, 230));

    // (4) Does clearing CHCR's start bit stop a block-sync SPU DMA?
    crate::bounds::record_start(ABORT_RECORD);
    clear_bank();
    let stopped = open_dma_write(DEST);
    // SAFETY: the bank, read only; the abort and the watch below end it.
    unsafe { kick(from, OLD_BLOCK_WORDS, (BANK_WORDS / u32::from(OLD_BLOCK_WORDS)) as u16) };
    let _ = poll_until_idle(ABORT_AFTER);
    dma::abort(Channel::Spu);
    let at_abort = dma::address(Channel::Spu).wrapping_sub(from & 0x00FF_FFFF) / 16;
    let blocks_at_abort = bcr_blocks();
    let busy_at_abort = dma::is_busy(Channel::Spu);
    // A fixed time, whatever CHCR says: a channel that kept moving after the
    // abort shows it in MADR.
    let mut watch = 0u32;
    while watch < ABORT_WATCH {
        let _ = dma::is_busy(Channel::Spu);
        watch += 1;
    }
    let after_wait = dma::address(Channel::Spu).wrapping_sub(from & 0x00FF_FFFF) / 16;
    let blocks_after = bcr_blocks();
    let busy_after = dma::is_busy(Channel::Spu);
    close_dma_write(stopped);
    let (abort_landed, _) = landed(bank);
    push_timing_record(
        records,
        next,
        record(
            ABORT_RECORD,
            at_abort & 0xFFFF,
            after_wait & 0xFFFF,
            u32::from(busy_at_abort) | (u32::from(busy_after) << 1),
        ),
    );
    push_timing_record(
        records,
        next,
        record(ABORT_CHECK_RECORD, abort_landed / 16, blocks_at_abort, blocks_after),
    );
    let mut line = Line::new();
    line.s("ABORT: READ ")
        .u(at_abort * 16)
        .s(" B, LATER ")
        .u(after_wait * 16)
        .s(if after_wait == at_abort { " B: STOPS" } else { " B: RUNS ON" });
    ui::live_line(font, 4, line.as_str(), (220, 224, 230));

    let after = fnv_words(bank);
    push_timing_record(
        records,
        next,
        record(SOURCE_RECORD, u32::from(before != after), after & 0xFFFF, 1),
    );
}
