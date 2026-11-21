// SPDX-License-Identifier: GPL-2.0-or-later
//! CARD TEST SAVE (v2.25, records `0xA30`-`0xA3C`): the SDK's memory-card
//! write path on a console, and how long a card takes to commit a frame.
//!
//! No card write had ever run on a console: the write records of `CARD SECTOR
//! PROTOCOL` (`0x891`-`0x89E`, `0x8B1`-`0x8BE`) needed L1 and R1 held at the
//! start, and nobody held them. `psx_mc` waits `POST_WRITE_SETTLE_SPINS`
//! (400,000 spins) after every frame it writes, and `CardJob` 8 VBlanks;
//! neither figure was measured. This step writes ONE save of its own, named
//! [`NAME`] with the title [`TITLE`] (the BIOS card manager shows it), through
//! `psx_mc::Card` as a game does, and says so on screen. It touches nothing
//! else on the card: the filesystem writes only free directory entries and the
//! block it allocates, and the timing writes below go only to the last frame of
//! the test save's own block. The save is left on the card (one block); it is
//! replaced, not added again, on the next run, and can be deleted from the
//! BIOS card manager.
//!
//! The step does not write when the card does not answer, is not formatted,
//! fails `validate_filesystem` (an orphan or a broken chain: a card the SDK
//! would refuse), or has no free block and no test save; it says why.
//!
//! After the save is written, the last frame of its block is written back raw
//! (the bytes read from it) four times, each followed at once by reads until a
//! read comes back whole (status bytes, checksum, the same data): the commit
//! time a card needs before it answers again, against the SDK's waits. Then
//! `CARD SECTOR PROTOCOL` reads and writes that same frame (no chord needed).
//!
//! Records, `0xA30 + 8 * port` (port 1, then port 2 from `0xA38`):
//!
//! * `+0` flags (bit 0 the card answered, 1 formatted, 2 the filesystem is
//!   valid, 3 a test save was there, 4 it was deleted, 5 the new save was
//!   written, 6 it read back equal, 7 not written: no free block, bits 8-11 the
//!   first `psx_mc` error, 1 `NoCard` ... 13 `BadName`, as `Error`'s order plus
//!   one), free blocks before, the save's block (1-15, 0 none).
//! * `+1` `Card::write`: HBlanks (16 bits, about 4 s), VBlanks; HBlanks of
//!   `Card::delete` (0 when there was no old save).
//! * `+2` the card's FLAG byte (the reply to the command byte) on the first
//!   read of the step, before anything was written; FLAG on the first read
//!   after the save was written; the status byte of the first raw write
//!   (`0x47` good, `0x4E` bad checksum, `0xFF` bad frame).
//! * `+3` HBlanks from the end of a raw write to the first whole read, min,
//!   median, max over four writes (`0xFFFF` none within [`SETTLE_LINES`]).
//! * `+4` reads made before the first whole one after the first raw write;
//!   FLAG of the first read after it; writes with no whole read within the
//!   bound (bits 0-3) and writes not answered `0x47` (bits 4-7).
//!
//! Decoder labels: `card_save_pN`, `card_save_time_pN`, `card_save_flag_pN`,
//! `card_commit_pN`, `card_commit_reads_pN`.

use crate::console_tests::{record, spread};
use crate::sio_timing::{transaction_long, Byte, SAFE_SETUP};
use crate::ui::{self, Line};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::addr_of_mut;
use psx_font::FontAtlas;
use psx_io::controller_port::Port;
use psx_io::timers::{self, Timer};
use psx_mc::{Card, Error as CardError, HardwareCard};
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const RECORD: u16 = 0xA30;
/// The test save's BIOS file name and its title.
pub(crate) const NAME: &str = "BEPSOX-00000HWTEST";
pub(crate) const TITLE: &str = "PSOXIDE HW TEST SCRATCH";
/// The save's payload length.
const PAYLOAD: usize = 256;
/// Raw write-then-read rounds.
const ROUNDS: usize = 4;
/// The longest wait for a whole read after a raw write: 2,104 HBlanks, eight
/// NTSC VBlanks, the SDK's `SETTLE_VBLANKS`.
const SETTLE_LINES: u32 = 2_104;
const HBLANK_CLOCK: u16 = 0x0100;
const READ_LEN: usize = 140;
const WRITE_LEN: usize = 138;

/// The frame `CARD SECTOR PROTOCOL` reads and writes on each port: the last
/// frame of the test save's block, `None` when this step wrote no save.
pub(crate) static mut SAVE_FRAME: [Option<u16>; 2] = [None, None];

static mut BYTES: [Byte; READ_LEN] = [Byte::none(); READ_LEN];

fn error_code(error: CardError) -> u32 {
    match error {
        CardError::NoCard => 1,
        CardError::Protocol => 2,
        CardError::BadChecksum => 3,
        CardError::OutOfRange => 4,
        CardError::NotFormatted => 5,
        CardError::NotFound => 6,
        CardError::NoSpace => 7,
        CardError::Exists => 8,
        CardError::Corrupt => 9,
        CardError::BufferTooSmall => 10,
        CardError::BadContainer => 11,
        CardError::Compression => 12,
        CardError::BadName => 13,
    }
}

/// A raw sector read of `frame`: (answered, whole, flag byte), the reply
/// bytes left in [`BYTES`].
#[optimize(size)]
#[inline(never)]
fn raw_read(port2: bool, frame: u16) -> (bool, bool, u8) {
    let mut tx = [0u8; READ_LEN];
    tx[0] = 0x81;
    tx[1] = 0x52;
    tx[4] = (frame >> 8) as u8;
    tx[5] = frame as u8;
    // SAFETY: single thread; the buffer is this module's.
    let bytes = unsafe { &mut *addr_of_mut!(BYTES) };
    *bytes = [Byte::none(); READ_LEN];
    let seen = transaction_long(port2, SAFE_SETUP, &tx, bytes, true);
    if seen.sent < READ_LEN {
        return (false, false, 0xFF);
    }
    let mut check = bytes[8].reply ^ bytes[9].reply;
    for byte in &bytes[10..138] {
        check ^= byte.reply;
    }
    let whole = bytes[2].reply == 0x5A
        && bytes[3].reply == 0x5D
        && bytes[8].reply == tx[4]
        && bytes[9].reply == tx[5]
        && bytes[139].reply == 0x47
        && check == bytes[138].reply
        && bytes[1].reply & 0x04 == 0;
    (true, whole, bytes[1].reply)
}

/// A raw sector write of `data` to `frame` with no settle wait after it: the
/// status byte.
#[optimize(size)]
#[inline(never)]
fn raw_write(port2: bool, frame: u16, data: &[u8; 128]) -> u8 {
    let mut tx = [0u8; WRITE_LEN];
    tx[0] = 0x81;
    tx[1] = 0x57;
    tx[4] = (frame >> 8) as u8;
    tx[5] = frame as u8;
    tx[6..134].copy_from_slice(data);
    let mut check = tx[4] ^ tx[5];
    for byte in &tx[6..134] {
        check ^= byte;
    }
    tx[134] = check;
    // SAFETY: single thread; the buffer is this module's.
    let bytes = unsafe { &mut *addr_of_mut!(BYTES) };
    *bytes = [Byte::none(); READ_LEN];
    let seen = transaction_long(port2, SAFE_SETUP, &tx, bytes, true);
    if seen.sent < WRITE_LEN {
        return 0xFF;
    }
    bytes[137].reply
}

/// Read until a whole read with the same data: (HBlanks, reads made, FLAG of
/// the first read). HBlanks `0xFFFF` when none came within [`SETTLE_LINES`].
#[optimize(size)]
#[inline(never)]
fn settle(port2: bool, frame: u16, data: &[u8; 128]) -> (u32, u32, u32) {
    timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
    timers::set_counter(Timer::Timer1, 0);
    let mut reads = 0u32;
    let mut first_flag = 0xFFFF;
    loop {
        let (_, whole, flag) = raw_read(port2, frame);
        let lines = u32::from(timers::counter(Timer::Timer1));
        reads += 1;
        if reads == 1 {
            first_flag = u32::from(flag);
        }
        // SAFETY: single thread; the buffer is this module's.
        let bytes = unsafe { &*addr_of_mut!(BYTES) };
        if whole && bytes[10..138].iter().zip(data).all(|(a, b)| a.reply == *b) {
            return (lines, reads, first_flag);
        }
        if lines > SETTLE_LINES || reads > 400 {
            return (0xFFFF, reads, first_flag);
        }
    }
}

fn hblanks_since(start: u16) -> u32 {
    u32::from(timers::counter(Timer::Timer1).wrapping_sub(start))
}

/// One port. Returns the save's frame for `CARD SECTOR PROTOCOL`.
#[optimize(size)]
#[inline(never)]
fn port(font: &FontAtlas, port2: bool, records: &mut Records, next: &mut usize) -> Option<u16> {
    let base = RECORD + 8 * u16::from(port2);
    crate::bounds::record_start(base);
    let row = 2 * u32::from(port2);
    let mut flags = 0u32;
    let mut error = 0u32;
    let mut free = 0u32;
    let mut block = 0u32;
    let mut write_lines = 0u32;
    let mut write_vblanks = 0u32;
    let mut delete_lines = 0u32;
    let note = |error: &mut u32, e: CardError| {
        if *error == 0 {
            *error = error_code(e);
        }
    };
    // The FLAG byte before anything is written (frame 0, the card's header).
    let (answered, _, flag_before) = raw_read(port2, 0);
    let mut flag_after = 0xFFFFu32;
    let mut frame = None;
    let mut line = Line::new();
    line.s("CARD ").u(1 + u32::from(port2)).s(": ");
    if answered {
        flags |= 1;
        let mut token = crate::config_irq::token();
        let slot = if port2 { Port::Two } else { Port::One };
        let mut card = Card::new(HardwareCard::on_port(&mut token, slot));
        match card.is_formatted() {
            Ok(true) => flags |= 1 << 1,
            Ok(false) => {}
            Err(e) => note(&mut error, e),
        }
        if flags & 2 != 0 {
            match card.validate_filesystem() {
                Ok(()) => flags |= 1 << 2,
                Err(e) => note(&mut error, e),
            }
        }
        let mut existed = false;
        if flags & 4 != 0 {
            match card.find(NAME.as_bytes()) {
                Ok(found) => existed = found.is_some(),
                Err(e) => note(&mut error, e),
            }
            match card.free_blocks() {
                Ok(n) => free = n as u32,
                Err(e) => note(&mut error, e),
            }
            if existed {
                flags |= 1 << 3;
            }
        }
        if flags & 4 != 0 && error == 0 && !existed && free == 0 {
            flags |= 1 << 7;
            line.s("FULL, NOTHING WRITTEN");
        } else if flags & 4 != 0 && error == 0 {
            ui::live_line(font, row, "WRITING TEST SAVE \"PSOXIDE HW TEST SCRATCH\"", (255, 220, 80));
            ui::live_line(font, row + 1, "(ONE BLOCK; NO OTHER SAVE IS TOUCHED)", (255, 220, 80));
            timers::set_mode(Timer::Timer1, HBLANK_CLOCK);
            if existed {
                timers::set_counter(Timer::Timer1, 0);
                match card.delete(NAME) {
                    Ok(()) => flags |= 1 << 4,
                    Err(e) => note(&mut error, e),
                }
                delete_lines = hblanks_since(0);
            }
            let mut data = [0u8; PAYLOAD];
            for (i, byte) in data.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_mul(37) ^ 0x5A;
            }
            data[..TITLE.len()].copy_from_slice(TITLE.as_bytes());
            if error == 0 {
                let vblank = interrupts::vblank_count();
                timers::set_counter(Timer::Timer1, 0);
                let written = card.write(NAME, TITLE, &data);
                write_lines = hblanks_since(0);
                write_vblanks = interrupts::vblank_count().wrapping_sub(vblank);
                match written {
                    Ok(()) => flags |= 1 << 5,
                    Err(e) => note(&mut error, e),
                }
            }
            if flags & (1 << 5) != 0 {
                let mut back = [0u8; PAYLOAD];
                match card.read(NAME, &mut back) {
                    Ok(n) if n == PAYLOAD && back == data => flags |= 1 << 6,
                    Ok(_) => {}
                    Err(e) => note(&mut error, e),
                }
                match card.find(NAME.as_bytes()) {
                    Ok(Some(index)) => {
                        block = index as u32 + 1;
                        frame = Some((block * 64 + 63) as u16);
                    }
                    Ok(None) => {}
                    Err(e) => note(&mut error, e),
                }
            }
            line.s(if flags & (1 << 6) != 0 { "TEST SAVE WRITTEN AND READ BACK" } else { "WRITE FAILED" });
        } else {
            line.s("NOT FORMATTED OR INVALID, NOTHING WRITTEN");
        }
        drop(card);
    } else {
        line.s("NO CARD");
    }
    push_timing_record(records, next, record(base, flags | error << 8, free, block));
    push_timing_record(records, next, record(base + 1, write_lines, write_vblanks, delete_lines));

    // The raw write-then-read rounds on the save's own last frame.
    let mut status = 0xFFFFu32;
    let mut lines = [0xFFFFu32; ROUNDS];
    let (mut first_reads, mut first_flag, mut missed, mut refused) = (0u32, 0xFFFFu32, 0u32, 0u32);
    if let Some(frame) = frame {
        let (_, whole, flag) = raw_read(port2, frame);
        flag_after = u32::from(flag);
        let mut data = [0u8; 128];
        // SAFETY: single thread; the buffer is this module's.
        let bytes = unsafe { &*addr_of_mut!(BYTES) };
        for (slot, byte) in data.iter_mut().zip(&bytes[10..138]) {
            *slot = byte.reply;
        }
        if whole {
            for (round, slot) in lines.iter_mut().enumerate() {
                let reply = raw_write(port2, frame, &data);
                if round == 0 {
                    status = u32::from(reply);
                }
                if reply != 0x47 {
                    refused += 1;
                }
                let (taken, reads, flag) = settle(port2, frame, &data);
                *slot = taken;
                if taken == 0xFFFF {
                    missed += 1;
                }
                if round == 0 {
                    first_reads = reads;
                    first_flag = flag;
                }
            }
        }
    }
    let (min, median, max) = spread(&mut lines);
    push_timing_record(
        records,
        next,
        record(base + 2, u32::from(flag_before), flag_after, status),
    );
    push_timing_record(records, next, record(base + 3, min, median, max));
    push_timing_record(
        records,
        next,
        record(base + 4, first_reads, first_flag, missed.min(15) | refused.min(15) << 4),
    );
    if frame.is_some() {
        line.s(", COMMIT ").u(median * 1000 / 15_734).s(" MS");
    }
    let good = flags & (1 << 6) != 0 || !answered;
    ui::live_line(font, row, line.as_str(), if good { (120, 230, 120) } else { (255, 110, 90) });
    ui::live_line(font, row + 1, "", (220, 224, 230));
    frame
}

/// Both ports.
#[optimize(size)]
#[inline(never)]
pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        let frame = port(font, port2, records, next);
        // SAFETY: single thread; plain static read by CARD SECTOR PROTOCOL.
        unsafe { (*addr_of_mut!(SAVE_FRAME))[usize::from(port2)] = frame };
    }
}
