// SPDX-License-Identifier: GPL-2.0-or-later
//! Memory-card protocol timing, byte by byte, v2.4 (records `0x880`-`0x8BF`).
//!
//! The emulator's SIO0 model has to give a card's `/ACK` at the right delay
//! and width for every byte of a sector read and write, and to leave the last
//! byte unanswered; a change to the SIO register width broke Soul Reaver's
//! card detection, and nothing on the console side said what the real
//! sequence looks like. This is that sequence.
//!
//! A sector read is 140 bytes: `81 52 00 00 MSB LSB` then four more zeros,
//! 128 more zeros for the data, the checksum and the terminator. The replies
//! are `FF`, the flag byte, `5A 5D`, a dummy, the MSB, `5C 5D`, the MSB and LSB
//! again, 128 data bytes, the checksum and `47`. A write is 138 bytes: `81 57 00 00 MSB LSB`, 128 data
//! bytes, the checksum and three zeros (`5C 5D`, then the status). For each
//! slot with a card the read is done four times (of frame 56, see
//! `WRITE_FRAME`), every byte's `/ACK` rise, width and arrival kept; the writes happen
//! only with L1 and R1 held at the start and only when every gate on the
//! reads passes (see `slot`), and write back the same 128 bytes the read
//! returned, to the same frame. A slot with no card answers `FF` and
//! never pulses `/ACK`; the empty-slot record says so.
//!
//! Times are system-clock cycles from the byte's write; a time that did not
//! happen is 0xFFFF.
//!

// The record constants document the layout; the code builds ids from their bases.
#![allow(dead_code)]

use crate::console_tests::{record, spread};
use crate::sio_timing::{transaction_long, Byte, SAFE_SETUP};
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use core::ptr::addr_of_mut;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec card_proto_head: ack_rise_cycles, ack_width_cycles, byte_done_cycles (the first ten bytes of a sector read, median of four reads; port 1 from 0x880 to 0x889, port 2 from 0x8A0)
pub(crate) const CARD_PROTO_RECORD: u16 = 0x880;
/// rec card_proto_data: min_cycles, median_cycles, max_cycles (128 data bytes of the read times four: the `/ACK` rise in 0x88A and the `/ACK` width in 0x88B; port 2 from 0x8AA)
pub(crate) const CARD_PROTO_DATA_RECORD: u16 = 0x88A;
/// rec card_proto_tail: ack_rise_cycles, ack_width_cycles, byte_done_cycles (the read's checksum byte in 0x88C and terminator byte in 0x88D; 0xFFFF rise on the terminator means no `/ACK`; port 2 from 0x8AC)
pub(crate) const CARD_PROTO_TAIL_RECORD: u16 = 0x88C;
/// rec card_proto_total: span_low_half, span_high_half, flags (a sector read: clocks from the first byte written to the last received; flags bit 0 a good read (flag byte, 5A, 5D, terminator 47), bit 1 the checksum matched, bit 2 the slot is empty, bit 3 the terminator was acknowledged, bits 4 to 11 the card's flag byte, bits 12 to 14 reads that were good out of four; port 1 0x88E, port 2 0x8AE)
pub(crate) const CARD_PROTO_TOTAL_RECORD: u16 = 0x88E;
/// rec card_proto_replies: replies_0_and_1, replies_2_and_3, replies_4_and_5 (the first six replies of a sector read, the first read; port 1 0x88F, port 2 0x8AF)
pub(crate) const CARD_PROTO_REPLIES_RECORD: u16 = 0x88F;
/// rec card_proto_replies_b: replies_6_and_7, replies_8_and_9, replies_138_and_139 (the next four replies and the checksum and terminator; port 1 0x890, port 2 0x8B0)
pub(crate) const CARD_PROTO_REPLIES_B_RECORD: u16 = 0x890;
/// rec card_proto_write_head: ack_rise_cycles, ack_width_cycles, byte_done_cycles (the first six bytes of a sector write, median of two writes; port 1 from 0x891 to 0x896, port 2 from 0x8B1)
pub(crate) const CARD_PROTO_WRITE_HEAD_RECORD: u16 = 0x891;
/// rec card_proto_write_data: min_cycles, median_cycles, max_cycles (128 data bytes of the write times two: the `/ACK` rise in 0x897 and the width in 0x898; port 2 from 0x8B7)
pub(crate) const CARD_PROTO_WRITE_DATA_RECORD: u16 = 0x897;
/// rec card_proto_write_tail: ack_rise_cycles, ack_width_cycles, byte_done_cycles (the write's checksum byte and the three bytes after it: 0x899 to 0x89C; port 2 from 0x8B9)
pub(crate) const CARD_PROTO_WRITE_TAIL_RECORD: u16 = 0x899;
/// rec card_proto_write_total: span_low_half, span_high_half, flags (a sector write: span as for the read; flags bit 0 the status byte was 47 (good), bit 1 not written because L1 and R1 were not held, bit 2 not written because the slot is empty or a read was not good, bit 3 because a read's checksum did not match, bit 4 because a read had the card's error flag, bit 5 because the four reads returned different data (a write happens only when none of bits 1 to 5 is set), bits 8 to 15 the status byte; port 1 0x89D, port 2 0x8BD)
pub(crate) const CARD_PROTO_WRITE_TOTAL_RECORD: u16 = 0x89D;
/// rec card_proto_write_replies: replies_134_and_135, replies_136_and_137, status_after_low_half (the last four replies of a write and SIO0 STAT low half after it; port 1 0x89E, port 2 0x8BE)
pub(crate) const CARD_PROTO_WRITE_REPLIES_RECORD: u16 = 0x89E;
const NONE: u32 = 0xFFFF;

const READ_LEN: usize = 140;
const WRITE_LEN: usize = 138;
const READ_ROUNDS: usize = 4;
const WRITE_ROUNDS: usize = 2;
const HEAD_READ: usize = 10;
const HEAD_WRITE: usize = 6;
/// The sector read four times and written back: frame 56, one of the unused
/// frames of block 0. Block 0 is the card's own (psx-spx, memory card data
/// format): frame 0 the header ("MC"), frames 1 to 15 the directory, 16 to 35
/// the broken-sector list, 36 to 55 the broken-sector replacement data, 56 to
/// 62 unused, 63 the BIOS's write-test frame. Frames 36 to 55 can hold live
/// replacement data on a card with bad sectors, so they are not used. A torn
/// write to an unused frame cannot make the BIOS see an unformatted card, which
/// one to the header or the directory could. Frames 0 to 55 and 63 are never
/// written.
const WRITE_FRAME: u16 = 56;

static mut BYTES: [Byte; READ_LEN] = [Byte::none(); READ_LEN];
static mut READS: [[Byte; READ_LEN]; READ_ROUNDS] = [[Byte::none(); READ_LEN]; READ_ROUNDS];
static mut WRITES: [[Byte; WRITE_LEN]; WRITE_ROUNDS] = [[Byte::none(); WRITE_LEN]; WRITE_ROUNDS];

fn read_command() -> [u8; READ_LEN] {
    let mut tx = [0u8; READ_LEN];
    tx[0] = 0x81;
    tx[1] = 0x52;
    tx[4] = (WRITE_FRAME >> 8) as u8;
    tx[5] = WRITE_FRAME as u8;
    tx
}

fn pair(bytes: &[Byte], a: usize) -> u32 {
    ((bytes[a].reply as u32) << 8) | bytes[a + 1].reply as u32
}

fn rise(byte: &Byte) -> u32 {
    byte.rise as u32
}

fn width(byte: &Byte) -> u32 {
    byte.width() as u32
}

/// Median over rounds of one per-byte measure of byte `index`.
fn byte_median(rounds: &[[Byte; READ_LEN]], index: usize, pick: fn(&Byte) -> u32) -> u32 {
    let mut values = [0u32; READ_ROUNDS];
    for (slot, round) in values.iter_mut().zip(rounds) {
        *slot = pick(&round[index]);
    }
    spread(&mut values).1
}

fn write_byte_median(rounds: &[[Byte; WRITE_LEN]], index: usize, pick: fn(&Byte) -> u32) -> u32 {
    let mut values = [0u32; WRITE_ROUNDS];
    for (slot, round) in values.iter_mut().zip(rounds) {
        *slot = pick(&round[index]);
    }
    spread(&mut values).1
}

fn done(byte: &Byte) -> u32 {
    byte.done as u32
}

/// Min, median and max of a measure over bytes `from..to` of every round.
fn range_spread(
    rounds: &[[Byte; READ_LEN]],
    from: usize,
    to: usize,
    pick: fn(&Byte) -> u32,
) -> (u32, u32, u32) {
    let mut values = [0u32; READ_ROUNDS * 128];
    let mut n = 0;
    for round in rounds {
        for byte in &round[from..to] {
            values[n] = pick(byte);
            n += 1;
        }
    }
    spread(&mut values[..n])
}

fn write_range_spread(
    rounds: &[[Byte; WRITE_LEN]],
    from: usize,
    to: usize,
    pick: fn(&Byte) -> u32,
) -> (u32, u32, u32) {
    let mut values = [0u32; WRITE_ROUNDS * 128];
    let mut n = 0;
    for round in rounds {
        for byte in &round[from..to] {
            values[n] = pick(byte);
            n += 1;
        }
    }
    spread(&mut values[..n])
}

fn good_read(bytes: &[Byte]) -> bool {
    bytes[2].reply == 0x5A && bytes[3].reply == 0x5D && bytes[139].reply == 0x47
}

fn checksum_ok(bytes: &[Byte]) -> bool {
    let mut check = bytes[8].reply ^ bytes[9].reply;
    for byte in &bytes[10..138] {
        check ^= byte.reply;
    }
    check == bytes[138].reply
}

/// One slot: the read, then the write when asked.
fn slot(port2: bool, write: bool, records: &mut Records, next: &mut usize) {
    let base = CARD_PROTO_RECORD + 0x20 * port2 as u16;
    crate::bounds::record_start(base);
    // SAFETY: single thread; these statics are this function's.
    let reads = unsafe { &mut *addr_of_mut!(READS) };
    let scratch = unsafe { &mut *addr_of_mut!(BYTES) };
    let writes = unsafe { &mut *addr_of_mut!(WRITES) };
    let tx = read_command();
    let mut good_reads = 0u32;
    let mut span = 0u32;
    let mut first_good: Option<usize> = None;
    let mut empty = false;
    let mut status = 0u32;
    for (round, slot) in reads.iter_mut().enumerate() {
        *scratch = [Byte::none(); READ_LEN];
        let seen = transaction_long(port2, SAFE_SETUP, &tx, scratch, true);
        *slot = *scratch;
        status = seen.status;
        if seen.sent < READ_LEN {
            empty = true;
            break;
        }
        if round == 0 {
            span = seen.span;
        }
        if good_read(scratch) {
            good_reads += 1;
            if first_good.is_none() {
                first_good = Some(round);
            }
        }
    }
    let rounds = if empty { 1 } else { READ_ROUNDS };
    let first = reads[0];
    // Header bytes, median of the reads that ran.
    for (index, byte) in first.iter().enumerate().take(HEAD_READ) {
        let id = base + index as u16;
        let row = if empty {
            record(id, rise(byte), width(byte), done(byte))
        } else {
            record(
                id,
                byte_median(reads, index, rise),
                byte_median(reads, index, width),
                byte_median(reads, index, done),
            )
        };
        push_timing_record(records, next, row);
    }
    let (r0, r1, r2, w0, w1, w2) = if empty {
        (NONE, NONE, NONE, NONE, NONE, NONE)
    } else {
        let r = range_spread(&reads[..rounds], HEAD_READ, 138, rise);
        let w = range_spread(&reads[..rounds], HEAD_READ, 138, width);
        (r.0, r.1, r.2, w.0, w.1, w.2)
    };
    push_timing_record(records, next, record(base + 0x0A, r0, r1, r2));
    push_timing_record(records, next, record(base + 0x0B, w0, w1, w2));
    for (offset, index) in [(0x0Cu16, 138usize), (0x0D, 139)] {
        let row = if empty {
            record(base + offset, NONE, NONE, NONE)
        } else {
            record(
                base + offset,
                byte_median(reads, index, rise),
                byte_median(reads, index, width),
                byte_median(reads, index, done),
            )
        };
        push_timing_record(records, next, row);
    }
    let ok = !empty && first_good.is_some();
    let sum_ok = ok && checksum_ok(&reads[first_good.unwrap_or(0)]);
    let terminator_acked = !empty && reads[0][139].acked();
    let flag_byte = if empty { 0xFF } else { first[1].reply as u32 };
    let flags = ok as u32
        | ((sum_ok as u32) << 1)
        | ((empty as u32) << 2)
        | ((terminator_acked as u32) << 3)
        | (flag_byte << 4)
        | (good_reads.min(7) << 12);
    push_timing_record(
        records,
        next,
        record(base + 0x0E, span & 0xFFFF, span >> 16, flags),
    );
    push_timing_record(
        records,
        next,
        record(
            base + 0x0F,
            pair(&first, 0),
            pair(&first, 2),
            pair(&first, 4),
        ),
    );
    let b_row = if empty {
        record(base + 0x10, NONE, NONE, NONE)
    } else {
        record(
            base + 0x10,
            pair(&first, 6),
            pair(&first, 8),
            pair(&first, 138),
        )
    };
    push_timing_record(records, next, b_row);
    let _ = status;

    // The write happens only when it was asked for AND every gate passes:
    // the slot holds a card, all four reads were good (flag byte, 5A, 5D and
    // the terminator 47), every read's checksum matches the one computed from
    // its bytes, none reported the card's error flag (bit 2 of the flag byte),
    // and the four reads returned identical data. Anything else writes
    // nothing, and the reason is recorded. What is written is the bytes just
    // read, to the frame just read.
    let all_good = !empty && good_reads as usize == READ_ROUNDS;
    let all_sums = !empty
        && reads.iter().all(|round| checksum_ok(round))
        && !cfg!(feature = "gate-test-bad-checksum");
    let error_flag = !empty && reads.iter().any(|round| round[1].reply & 0x04 != 0);
    let identical = !empty
        && reads[1..].iter().all(|round| {
            round[10..138]
                .iter()
                .zip(&reads[0][10..138])
                .all(|(a, b)| a.reply == b.reply)
        });
    let gates = all_good && all_sums && !error_flag && identical;
    if !write || !gates {
        let reasons = ((!write) as u32) << 1
            | ((!all_good) as u32) << 2
            | ((!all_sums) as u32) << 3
            | (error_flag as u32) << 4
            | ((!identical) as u32) << 5;
        for offset in 0x11u16..0x1D {
            push_timing_record(records, next, record(base + offset, NONE, NONE, NONE));
        }
        push_timing_record(records, next, record(base + 0x1D, 0, 0, reasons));
        push_timing_record(records, next, record(base + 0x1E, NONE, NONE, NONE));
        return;
    }
    let source = &reads[0];
    let mut wtx = [0u8; WRITE_LEN];
    wtx[0] = 0x81;
    wtx[1] = 0x57;
    wtx[4] = (WRITE_FRAME >> 8) as u8;
    wtx[5] = WRITE_FRAME as u8;
    for i in 0..128 {
        wtx[6 + i] = source[10 + i].reply;
    }
    // Verification builds only: change one byte, so a run on a private card
    // shows that the write lands on WRITE_FRAME and nowhere else.
    if cfg!(feature = "gate-test-marker") {
        wtx[6] ^= 0xA5;
    }
    let mut check = wtx[4] ^ wtx[5];
    for byte in &wtx[6..134] {
        check ^= byte;
    }
    wtx[134] = check;
    let mut span_w = 0u32;
    let mut status_byte = 0u32;
    let mut last_status = 0u32;
    for (round, slot) in writes.iter_mut().enumerate() {
        let mut buffer = [Byte::none(); READ_LEN];
        let seen = transaction_long(port2, SAFE_SETUP, &wtx, &mut buffer, true);
        let mut copy = [Byte::none(); WRITE_LEN];
        copy.copy_from_slice(&buffer[..WRITE_LEN]);
        *slot = copy;
        if round == 0 {
            span_w = seen.span;
            status_byte = buffer[137].reply as u32;
        }
        last_status = seen.status;
        // The card needs time to commit before it is spoken to again.
        for _ in 0..4 {
            crate::v24::frame();
        }
    }
    for index in 0..HEAD_WRITE {
        push_timing_record(
            records,
            next,
            record(
                base + 0x11 + index as u16,
                write_byte_median(writes, index, rise),
                write_byte_median(writes, index, width),
                write_byte_median(writes, index, done),
            ),
        );
    }
    let r = write_range_spread(writes, HEAD_WRITE, 134, rise);
    let w = write_range_spread(writes, HEAD_WRITE, 134, width);
    push_timing_record(records, next, record(base + 0x17, r.0, r.1, r.2));
    push_timing_record(records, next, record(base + 0x18, w.0, w.1, w.2));
    for (offset, index) in [(0x19u16, 134usize), (0x1A, 135), (0x1B, 136), (0x1C, 137)] {
        push_timing_record(
            records,
            next,
            record(
                base + offset,
                write_byte_median(writes, index, rise),
                write_byte_median(writes, index, width),
                write_byte_median(writes, index, done),
            ),
        );
    }
    let first_write = &writes[0];
    let flags = ((status_byte == 0x47) as u32) | (status_byte << 8);
    push_timing_record(
        records,
        next,
        record(base + 0x1D, span_w & 0xFFFF, span_w >> 16, flags),
    );
    push_timing_record(
        records,
        next,
        record(
            base + 0x1E,
            pair(first_write, 134),
            pair(first_write, 136),
            last_status & 0xFFFF,
        ),
    );
}

pub(crate) fn run(write: bool, records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        slot(port2, write, records, next);
    }
}
