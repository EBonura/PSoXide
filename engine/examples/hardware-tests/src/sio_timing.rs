// SPDX-License-Identifier: GPL-2.0-or-later
//! SIO0 measurements: what the controller port does on the wire, per port.
//!
//! Nothing here passes or fails. Each step records numbers for the emulator
//! to be tuned against, and none of it needs a particular pad or card: a port
//! with nothing in it is a result too (it answers `0xFF`, never pulses
//! `/ACK`), and every record is laid out the same way either way.
//!
//! Times are system-clock cycles on Timer 2, read at the edges only: the loop
//! that watches `STAT` for the `/ACK` level does nothing else, so an edge is
//! placed to within one `STAT` read. Memory-card frame times are in HBlanks
//! (Timer 1), because a frame read outlasts the 16-bit system-clock range.
//!
//! The memory card is only ever read, never written.
//!
//! The record layout, three fields each, is documented on the constants
//! below and mirrored in `tools/hwtest-report.py`.

use crate::console_tests::{record, spread};
use crate::list_busy_probes::GpuLoad;
use crate::{push_timing_record, IrqGuard, TimingRecord, TIMING_RECORD_COUNT};
use psx_hw::sio::sio0;
use psx_io::controller_port::{Port, Timing, Transport};
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_mc::{Block, Error as CardError, HardwareCard};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec sio_setup: first_ok_delay_cycles, last_failed_delay_cycles, ok_mask_per_delay (port 1 and 2, 0x600-0x601)
const SETUP_RECORD: u16 = 0x600;
/// rec sio_pad_ack: ack_rise_cycles, ack_width_cycles, byte_done_cycles (nine bytes a port, 0x610-0x618 and 0x620-0x628)
const PAD_ACK_RECORD: u16 = 0x610;
/// rec sio_card_ack: ack_rise_cycles, ack_width_cycles, byte_done_cycles (four bytes a port, 0x6C0-0x6C3 and 0x6C8-0x6CB)
const CARD_ACK_RECORD: u16 = 0x6C0;
/// rec sio_pad_answer: replies_0_and_1, replies_2_and_3, ack_mask_and_flags (port 1 and 2, 0x630-0x631)
const PAD_ANSWER_RECORD: u16 = 0x630;
/// rec sio_card_answer: replies_0_and_1, replies_2_and_3, ack_mask_and_flags (port 1 and 2, 0x634-0x635)
const CARD_ANSWER_RECORD: u16 = 0x634;
/// rec sio_mix_count: pad_ok_and_tried, card_ok_and_tried, error_counts (idle and loaded on each port, 0x640 0x644 0x648 0x64C)
const MIX_COUNT_RECORD: u16 = 0x640;
/// rec sio_mix_card: card_frame_hblanks_min, card_frame_hblanks_med, card_frame_hblanks_max (0x641 0x645 0x649 0x64D)
const MIX_CARD_RECORD: u16 = 0x641;
/// rec sio_mix_pad: pad_poll_cycles_min, pad_poll_cycles_med, pad_poll_cycles_max (0x642 0x646 0x64A 0x64E)
const MIX_PAD_RECORD: u16 = 0x642;

/// rec sio_mix_fault: round_and_kind, fault_and_exchange, exchanges_and_acks (the first failed card frame read of a mix pass: round in the high bits, kind 1 no card, 2 protocol, 3 checksum, 4 other; transport fault 0 none, 1 TX, 2 RX, 3 ACK, 4 ACK release in the high nibble with the exchange it happened at; port 1 idle, port 1 loaded, port 2 idle, port 2 loaded from 0x6D0 in steps of three; all zero when nothing failed)
const MIX_FAULT_RECORD: u16 = 0x6D0;
/// rec sio_mix_fault_prefix_a: response_bytes_0_1, response_bytes_2_3, response_bytes_4_5 (the card's first six replies of that frame read: select, command flags, 0x5A, 0x5D, then the echoes; 0x6D1 and every third after)
const MIX_FAULT_PREFIX_A_RECORD: u16 = 0x6D1;
/// rec sio_mix_fault_prefix_b: response_bytes_6_7, response_bytes_8_9, failures_of_any_kind (the ACK byte pair 0x5C 0x5D and the address echoes, then how many frames failed in the pass; a protocol error with these all right and no transport fault means the terminator byte was wrong; 0x6D2 and every third after)
const MIX_FAULT_PREFIX_B_RECORD: u16 = 0x6D2;

/// A time that did not happen.
const NONE: u16 = 0xFFFF;
/// `STAT` reads before giving up on an `/ACK`. At a few cycles a read this is
/// about 150 microseconds: past the byte itself (32 us at 250 kHz) and well
/// past the 100 us the BIOS allows a device to answer.
const WINDOW_READS: u32 = 1_200;
/// Setup delay for the passes that are not about it: past every delay the
/// sweep has seen an official pad need.
pub(crate) const SAFE_SETUP: u16 = 24_576;

/// Delays after asserting select, in system-clock cycles (33.8688 MHz).
const DELAYS: [u16; 16] = [
    0, 64, 128, 256, 512, 768, 1_024, 1_536, 2_048, 3_072, 4_096, 6_144, 8_192, 12_288, 16_384,
    24_576,
];

const PAD_POLL: [u8; 9] = [0x01, 0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
const CARD_READ: [u8; 4] = [0x81, 0x52, 0x00, 0x00];

/// One byte clocked out and what came of it.
#[derive(Copy, Clone)]
pub(crate) struct Byte {
    pub(crate) reply: u8,
    /// Cycles from the write to the byte being received.
    pub(crate) done: u16,
    /// Cycles from the write to `/ACK` asserting, and to it releasing.
    pub(crate) rise: u16,
    pub(crate) fall: u16,
}

impl Byte {
    pub(crate) const fn none() -> Self {
        Self {
            reply: 0xFF,
            done: NONE,
            rise: NONE,
            fall: NONE,
        }
    }

    pub(crate) const fn acked(&self) -> bool {
        self.rise != NONE
    }

    /// Cycles this byte took from the write: to `/ACK` releasing when it was
    /// answered, otherwise to its arrival (0 when neither was seen).
    pub(crate) const fn span(&self) -> u16 {
        if self.fall != NONE {
            self.fall
        } else if self.done != NONE {
            self.done
        } else {
            0
        }
    }

    pub(crate) const fn width(&self) -> u16 {
        if self.rise != NONE && self.fall != NONE && self.fall >= self.rise {
            self.fall - self.rise
        } else {
            NONE
        }
    }
}

/// What one transaction left behind.
pub(crate) struct Seen {
    pub(crate) bytes: [Byte; 9],
    /// `STAT` after the release.
    pub(crate) status: u32,
    /// System-clock cycles from select to release. The counter restarts with
    /// every byte, so this is the last byte's window, which for a pad whose
    /// last byte is never answered is the harness's own timeout.
    pub(crate) total: u16,
    /// System-clock cycles from the first byte written to the last byte
    /// received: each acknowledged byte up to its `/ACK` releasing, the
    /// unanswered last byte up to its arrival. The cost of the exchange
    /// itself, the select delay not included.
    pub(crate) span: u32,
}

fn token() -> ControllerPort {
    // SAFETY: a token is a logic guard, not a memory-safety one, and nothing
    // else drives SIO0 while a step of the run is in progress.
    unsafe { ControllerPort::steal() }
}

fn count() -> u16 {
    timers::counter(Timer::Timer2)
}

fn restart() {
    timers::set_mode(Timer::Timer2, 0);
    timers::set_counter(Timer::Timer2, 0);
}

/// Wait `cycles` system-clock cycles (under 65,536).
fn pause(cycles: u16) {
    restart();
    while count() < cycles {}
}

/// Clock one byte and stamp its edges.
fn exchange(port: &mut ControllerPort, tx: u8) -> Byte {
    let mut spins = 0u32;
    while port.status() & sio0::stat::TX_READY == 0 && spins < 4_096 {
        spins += 1;
    }
    let mut out = Byte::none();
    timers::set_counter(Timer::Timer2, 0);
    port.transmit(tx);
    let mut reads = 0u32;
    loop {
        let status = port.status();
        if out.done == NONE && status & sio0::stat::RX_NOT_EMPTY != 0 {
            out.done = count();
        }
        if status & sio0::stat::DSR_LEVEL != 0 {
            out.rise = count();
            break;
        }
        reads += 1;
        if reads >= WINDOW_READS {
            break;
        }
    }
    if out.rise != NONE {
        let mut reads = 0u32;
        loop {
            if port.status() & sio0::stat::DSR_LEVEL == 0 {
                out.fall = count();
                break;
            }
            reads += 1;
            if reads >= WINDOW_READS * 8 {
                break;
            }
        }
    }
    // The byte lands whether or not anyone answers.
    let mut reads = 0u32;
    while out.done == NONE && reads < WINDOW_READS * 8 {
        if port.status() & sio0::stat::RX_NOT_EMPTY != 0 {
            out.done = count();
        }
        reads += 1;
    }
    if port.status() & sio0::stat::RX_NOT_EMPTY != 0 {
        out.reply = port.receive();
    }
    out
}

/// One transaction: select the port, wait `delay` cycles, clock `tx` out,
/// release. With `mask_irq` the interrupt flag is clear for the whole thing.
pub(crate) fn transaction(port2: bool, delay: u16, tx: &[u8], mask_irq: bool) -> Seen {
    let mut port = token();
    let _irq = if mask_irq {
        Some(IrqGuard::mask())
    } else {
        None
    };
    let mut seen = Seen {
        bytes: [Byte::none(); 9],
        status: 0,
        total: 0,
        span: 0,
    };
    port.set_mode(sio0::MODE_8N1);
    port.set_baud(sio0::BAUD_250KHZ);
    port.set_control(sio0::ctrl::ACK);
    restart();
    port.set_control(sio0::selected_ctrl(port2, false));
    while count() < delay {}
    port.drain_receive();
    // An /ACK left asserted by the last transaction must not count as ours.
    let _ = port.wait_status_clear(sio0::stat::DSR_LEVEL, 4_096);
    for (slot, byte) in seen.bytes.iter_mut().zip(tx.iter()) {
        *slot = exchange(&mut port, *byte);
        seen.span += slot.span() as u32;
    }
    seen.total = count();
    port.deselect();
    seen.status = port.status();
    port.reset();
    // Let the device and the line settle before anyone selects again.
    pause(4_000);
    seen
}

/// What a transaction longer than nine bytes left behind.
pub(crate) struct LongSeen {
    /// Bytes clocked (fewer than asked when the first was never answered).
    pub(crate) sent: usize,
    /// `STAT` after the release.
    pub(crate) status: u32,
    /// System-clock cycles from the first byte written to the last received.
    pub(crate) span: u32,
}

/// A transaction of any length (a memory-card sector is 140 bytes), the edges
/// of every byte kept in `out`. With `stop_if_silent` it ends after the first
/// byte when nothing answered it (an empty slot): the port is released at
/// once. Interrupts are masked for the whole transaction.
pub(crate) fn transaction_long(
    port2: bool,
    delay: u16,
    tx: &[u8],
    out: &mut [Byte],
    stop_if_silent: bool,
) -> LongSeen {
    let mut port = token();
    let _irq = IrqGuard::mask();
    let mut seen = LongSeen {
        sent: 0,
        status: 0,
        span: 0,
    };
    port.set_mode(sio0::MODE_8N1);
    port.set_baud(sio0::BAUD_250KHZ);
    port.set_control(sio0::ctrl::ACK);
    restart();
    port.set_control(sio0::selected_ctrl(port2, false));
    while count() < delay {}
    port.drain_receive();
    let _ = port.wait_status_clear(sio0::stat::DSR_LEVEL, 4_096);
    for (index, (slot, byte)) in out.iter_mut().zip(tx.iter()).enumerate() {
        *slot = exchange(&mut port, *byte);
        seen.sent = index + 1;
        seen.span += slot.span() as u32;
        if index == 0 && stop_if_silent && !slot.acked() {
            break;
        }
    }
    port.deselect();
    seen.status = port.status();
    port.reset();
    pause(4_000);
    seen
}

fn push(records: &mut Records, next: &mut usize, row: TimingRecord) {
    push_timing_record(records, next, row);
}

// ------------------------------------------------------------ select delay

/// A poll counts as answered when the first two bytes pulsed `/ACK` and the
/// third reply is the `0x5A` every pad sends.
fn answered(bytes: &[Byte; 9]) -> bool {
    bytes[0].acked() && bytes[1].acked() && bytes[2].reply == 0x5A
}

/// How long after select the first byte may go before the pad stops
/// answering: the question the SCPH-1200 asked, in cycles instead of spins.
pub(crate) fn setup_sweep(records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        crate::bounds::record_start(SETUP_RECORD + port2 as u16);
        let mut mask = 0u32;
        let mut first_ok = NONE as u32;
        let mut last_fail = NONE as u32;
        for (index, delay) in DELAYS.iter().copied().enumerate() {
            let mut all = true;
            for _ in 0..4 {
                all &= answered(&transaction(port2, delay, &PAD_POLL[..3], true).bytes);
            }
            if all {
                mask |= 1 << index;
                if first_ok == NONE as u32 {
                    first_ok = delay as u32;
                }
            } else {
                last_fail = delay as u32;
            }
        }
        push(
            records,
            next,
            record(SETUP_RECORD + port2 as u16, first_ok, last_fail, mask),
        );
    }
}

// -------------------------------------------------------------- ack timing

const ROUNDS: usize = 8;

fn middle(values: &mut [u32; ROUNDS]) -> u32 {
    spread(values).1
}

/// `/ACK` timing of every byte of `tx` over several transactions, then what
/// the first one answered.
fn ack_timing(
    port2: bool,
    tx: &[u8],
    first_id: u16,
    answer_id: u16,
    records: &mut Records,
    next: &mut usize,
) {
    crate::bounds::record_start(first_id);
    let mut rise = [[0u32; ROUNDS]; 9];
    let mut width = [[0u32; ROUNDS]; 9];
    let mut done = [[0u32; ROUNDS]; 9];
    let mut first = [Byte::none(); 9];
    let mut after = 0u32;
    let mut ack_mask = 0u32;
    for round in 0..ROUNDS {
        let seen = transaction(port2, SAFE_SETUP, tx, true);
        if round == 0 {
            first = seen.bytes;
            after = seen.status;
        }
        for (i, byte) in seen.bytes.iter().enumerate().take(tx.len()) {
            rise[i][round] = byte.rise as u32;
            width[i][round] = byte.width() as u32;
            done[i][round] = byte.done as u32;
            if byte.acked() {
                ack_mask |= 1 << i;
            }
        }
    }
    for i in 0..tx.len() {
        let row = record(
            first_id + i as u16,
            middle(&mut rise[i]),
            middle(&mut width[i]),
            middle(&mut done[i]),
        );
        push(records, next, row);
    }
    // Which bytes ever pulsed /ACK (low nine bits), whether the third reply
    // was the 0x5A of a device that is there, the IRQ latch the transaction
    // left in STAT (STAT bit 9, here bit 10) and TX idle (STAT bit 2, here 11).
    let there = first[2].reply == 0x5A;
    let flags =
        ack_mask | ((there as u32) << 9) | (((after >> 9) & 1) << 10) | (((after >> 2) & 1) << 11);
    let row = record(
        answer_id,
        first[0].reply as u32 | ((first[1].reply as u32) << 8),
        first[2].reply as u32 | ((first[3].reply as u32) << 8),
        flags,
    );
    push(records, next, row);
}

/// The nine bytes of a pad poll on each port (the length of an analog pad's).
pub(crate) fn pad_timing(records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        let base = PAD_ACK_RECORD + 16 * port2 as u16;
        ack_timing(
            port2,
            &PAD_POLL,
            base,
            PAD_ANSWER_RECORD + port2 as u16,
            records,
            next,
        );
    }
}

/// The four bytes of a memory-card read command on each port. The command is
/// abandoned after its fourth byte, before any data moves.
pub(crate) fn card_timing(records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        let base = CARD_ACK_RECORD + 8 * port2 as u16;
        ack_timing(
            port2,
            &CARD_READ,
            base,
            CARD_ANSWER_RECORD + port2 as u16,
            records,
            next,
        );
    }
}

// -------------------------------------------------- latch and arming records

/// rec sio_latch_unarmed: irq_after_byte_mask, ack_mask, flags (a pad poll with the ACK interrupt armed never, port 1 and 2, 0x6E0-0x6E1). Bit n of the first two is byte n: STAT bit 9 seen after the byte's `/ACK` released, and `/ACK` seen. The flags are 1 for a device that answered with `0x5A`, 2 for a digital pad (five bytes), 4 for an analog pad. Does the latch set with CTRL bit 12 clear? A mask of zero says no, which the shared pad driver relies on.
const LATCH_UNARMED_RECORD: u16 = 0x6E0;
/// rec sio_arm_mid_packet: polls_clean_and_tried, first_deviation, irq_after_byte_mask (four polls that write CTRL, arm plus ACK, after every acknowledged byte, each compared with an unarmed poll of the same pad; port 1 and 2, 0x6E2-0x6E3). `polls_clean_and_tried` is the clean count in the low byte and the polls tried in the high byte; a poll is clean when its ID, its `0x5A` and its `/ACK` mask match the unarmed one; `first_deviation` is the poll in the high byte and the byte index in the low byte of the first mismatch, 0xFFFF when there is none. Does one CTRL write mid-packet disturb the rest of it? The shared pad driver does this only when a port reports the latch alone, but a console answer decides whether it may ever do it on silicon.
const ARM_MID_PACKET_RECORD: u16 = 0x6E2;

/// One pad poll with the select delay of the other passes. With `arm` the
/// interrupt is armed and the latch cleared by one `CTRL` write after every
/// byte that pulsed `/ACK`, exactly what `Transport::exchange` does on a port
/// that only latches. Returns the replies, the bytes that pulsed `/ACK`, and
/// the bytes after which STAT bit 9 was set.
fn latch_poll(port2: bool, arm: bool) -> ([u8; 9], u16, u16) {
    let mut port = token();
    let _irq = IrqGuard::mask();
    let mut replies = [0xFFu8; 9];
    let (mut acks, mut irqs) = (0u16, 0u16);
    port.set_mode(sio0::MODE_8N1);
    port.set_baud(sio0::BAUD_250KHZ);
    port.set_control(sio0::ctrl::ACK);
    restart();
    port.set_control(sio0::selected_ctrl(port2, false));
    while count() < SAFE_SETUP {}
    port.drain_receive();
    let _ = port.wait_status_clear(sio0::stat::DSR_LEVEL, 4_096);
    for (index, tx) in PAD_POLL.iter().enumerate() {
        let byte = exchange(&mut port, *tx);
        replies[index] = byte.reply;
        // Read the latch before the write that clears it.
        if port.status() & sio0::stat::IRQ != 0 {
            irqs |= 1 << index;
        }
        if byte.acked() {
            acks |= 1 << index;
            if arm {
                port.set_control(sio0::selected_ctrl(port2, true) | sio0::ctrl::ACK);
            }
        }
        if index == 0 && !byte.acked() {
            break;
        }
    }
    port.deselect();
    port.reset();
    pause(4_000);
    (replies, acks, irqs)
}

/// The two questions the shared pad driver's latch path rests on.
pub(crate) fn latch_and_arm(records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        crate::bounds::record_start(LATCH_UNARMED_RECORD + port2 as u16);
        let (reference, ref_acks, ref_irqs) = latch_poll(port2, false);
        let there = reference[2] == 0x5A;
        let kind = match reference[1] {
            0x41 => 2,
            0x73 | 0x53 => 4,
            _ => 0,
        };
        push(
            records,
            next,
            record(
                LATCH_UNARMED_RECORD + port2 as u16,
                ref_irqs as u32,
                ref_acks as u32,
                there as u32 | kind,
            ),
        );
        crate::bounds::record_start(ARM_MID_PACKET_RECORD + port2 as u16);
        let mut clean = 0u32;
        let mut deviation = 0xFFFFu32;
        let mut irq_union = 0u32;
        let tries = if there { 4 } else { 0 };
        for poll in 0..tries {
            let (replies, acks, irqs) = latch_poll(port2, true);
            irq_union |= irqs as u32;
            let mut bad = None;
            if replies[1] != reference[1] {
                bad = Some(1);
            } else if replies[2] != reference[2] {
                bad = Some(2);
            } else if acks != ref_acks {
                bad = Some((acks ^ ref_acks).trailing_zeros());
            }
            match bad {
                None => clean += 1,
                Some(byte) if deviation == 0xFFFF => deviation = (poll << 8) | byte,
                Some(_) => {}
            }
        }
        push(
            records,
            next,
            record(
                ARM_MID_PACKET_RECORD + port2 as u16,
                clean | (tries << 8),
                deviation,
                irq_union,
            ),
        );
    }
}


// ------------------------------------------------------- pad and card mix

const MIX_ROUNDS: usize = 24;

/// Pad polls and card frame reads taking turns on one port: a poll, a frame,
/// a poll, a frame. With `load` the GPU walks a list of large triangles
/// behind them and interrupts stay enabled, so the VBlank handler lands
/// inside the transfers as it does in a game. Without it, interrupts are
/// masked and the machine is otherwise idle.
fn mix(port2: bool, load: bool, records: &mut Records, next: &mut usize) {
    let slot = if port2 { Port::Two } else { Port::One };
    let base = 4 * (2 * port2 as u16 + load as u16);
    crate::bounds::record_start(MIX_COUNT_RECORD + base);
    let mut pad_ok = 0u32;
    let mut card_ok = 0u32;
    let (mut no_card, mut protocol, mut checksum, mut pad_lost) = (0u32, 0u32, 0u32, 0u32);
    let mut failures = 0u32;
    let mut first_failure: Option<(u32, u32, psx_mc::TransportTrace)> = None;
    let mut frame_times = [0u32; MIX_ROUNDS];
    let mut pad_times = [0u32; MIX_ROUNDS];
    let mut port = token();
    let mut card = HardwareCard::on_port(&mut port, slot);
    let mut buf = [0u8; 128];
    timers::set_mode(Timer::Timer1, 0x0100);
    let mut gpu_load = if load { Some(GpuLoad::start()) } else { None };
    let guard = if load { None } else { Some(IrqGuard::mask()) };
    for round in 0..MIX_ROUNDS {
        if let Some(busy) = gpu_load.as_mut() {
            busy.keep_busy();
        }
        let seen = transaction(port2, 2_048, &PAD_POLL[..3], false);
        pad_times[round] = seen.total as u32;
        if seen.bytes[2].reply == 0x5A {
            pad_ok += 1;
        } else {
            pad_lost += 1;
        }
        timers::set_counter(Timer::Timer1, 0);
        let result = card.read_frame((round as u16 * 7) % 64, &mut buf);
        frame_times[round] = timers::counter(Timer::Timer1) as u32;
        let kind = match result {
            Ok(()) => {
                card_ok += 1;
                0
            }
            Err(CardError::NoCard) => {
                no_card += 1;
                1
            }
            Err(CardError::BadChecksum) => {
                checksum += 1;
                3
            }
            Err(CardError::Protocol) => {
                protocol += 1;
                2
            }
            Err(_) => {
                protocol += 1;
                4
            }
        };
        if kind != 0 {
            failures += 1;
            if first_failure.is_none() {
                first_failure = Some((round as u32, kind, card.last_trace()));
            }
        }
    }
    drop(guard);
    if let Some(busy) = gpu_load.as_mut() {
        busy.stop();
    }
    let (fmin, fmed, fmax) = spread(&mut frame_times);
    let (pmin, pmed, pmax) = spread(&mut pad_times);
    push(
        records,
        next,
        record(
            MIX_COUNT_RECORD + base,
            (pad_ok << 8) | MIX_ROUNDS as u32,
            (card_ok << 8) | MIX_ROUNDS as u32,
            no_card.min(15)
                | (protocol.min(15) << 4)
                | (checksum.min(15) << 8)
                | (pad_lost.min(15) << 12),
        ),
    );
    push(
        records,
        next,
        record(MIX_CARD_RECORD + base, fmin, fmed, fmax),
    );
    push(
        records,
        next,
        record(MIX_PAD_RECORD + base, pmin, pmed, pmax),
    );
    push_fault(
        records,
        next,
        3 * (2 * port2 as u16 + load as u16),
        failures,
        first_failure,
    );
}

fn fault_code(fault: psx_mc::TransportFault) -> u32 {
    match fault {
        psx_mc::TransportFault::None => 0,
        psx_mc::TransportFault::TxTimeout => 1,
        psx_mc::TransportFault::RxTimeout => 2,
        psx_mc::TransportFault::AckTimeout => 3,
        psx_mc::TransportFault::AckReleaseTimeout => 4,
    }
}

/// The three evidence records of a mix pass: what the first failed frame read
/// looked like on the wire, as far as the SDK's trace kept it.
fn push_fault(
    records: &mut Records,
    next: &mut usize,
    slot: u16,
    failures: u32,
    first: Option<(u32, u32, psx_mc::TransportTrace)>,
) {
    let id = MIX_FAULT_RECORD + slot;
    let Some((round, kind, trace)) = first else {
        push(records, next, record(id, 0, 0, 0));
        push(
            records,
            next,
            record(MIX_FAULT_PREFIX_A_RECORD + slot, 0, 0, 0),
        );
        push(
            records,
            next,
            record(MIX_FAULT_PREFIX_B_RECORD + slot, 0, 0, failures),
        );
        return;
    };
    let p = trace.response_prefix;
    let pair = |a: usize| ((p[a] as u32) << 8) | p[a + 1] as u32;
    push(
        records,
        next,
        record(
            id,
            (round << 4) | kind,
            (fault_code(trace.fault) << 12) | (trace.fault_exchange as u32 & 0xFFF),
            ((trace.exchanges as u32 & 0xFF) << 8) | (trace.acknowledgements as u32 & 0xFF),
        ),
    );
    push(
        records,
        next,
        record(MIX_FAULT_PREFIX_A_RECORD + slot, pair(0), pair(2), pair(4)),
    );
    push(
        records,
        next,
        record(MIX_FAULT_PREFIX_B_RECORD + slot, pair(6), pair(8), failures),
    );
}

/// Port 1 and 2, idle and under load. A port with no card gets one probe
/// (a no-answer costs tens of milliseconds) and a row of "nothing there".
pub(crate) fn pad_and_card(records: &mut Records, next: &mut usize) {
    for port2 in [false, true] {
        let slot = if port2 { Port::Two } else { Port::One };
        let mut port = token();
        let mut probe = HardwareCard::on_port(&mut port, slot);
        let mut buf = [0u8; 128];
        let present = !matches!(probe.read_frame(0, &mut buf), Err(CardError::NoCard));
        if !present {
            for load in [false, true] {
                let base = 4 * (2 * port2 as u16 + load as u16);
                push(records, next, record(MIX_COUNT_RECORD + base, 0, 0, 0xFFFF));
                push(
                    records,
                    next,
                    record(
                        MIX_CARD_RECORD + base,
                        NONE as u32,
                        NONE as u32,
                        NONE as u32,
                    ),
                );
                push(
                    records,
                    next,
                    record(MIX_PAD_RECORD + base, NONE as u32, NONE as u32, NONE as u32),
                );
                let slot = 3 * (2 * port2 as u16 + load as u16);
                push(
                    records,
                    next,
                    record(MIX_FAULT_RECORD + slot, 0xFFFF, 0xFFFF, 0xFFFF),
                );
                push(
                    records,
                    next,
                    record(MIX_FAULT_PREFIX_A_RECORD + slot, 0xFFFF, 0xFFFF, 0xFFFF),
                );
                push(
                    records,
                    next,
                    record(MIX_FAULT_PREFIX_B_RECORD + slot, 0xFFFF, 0xFFFF, 0xFFFF),
                );
            }
            continue;
        }
        mix(port2, false, records, next);
        mix(port2, true, records, next);
    }
}

// ---------------------------------------------------------- select early

/// rec sio_select_early: polls, id_and_magic, poll_cycles_median (port 1, eight polls a case, 0x6F0-0x6F4). `polls` packs four counts of four bits: clean polls (bits 0-3), polls tried (4-7), polls whose earlier select was still in `CTRL` when the bytes began (8-11), card frames read in the gap (12-15). A poll is clean when every byte exchanged and the third reply was `0x5A`. `id_and_magic` is the last poll's ID in the high byte and its third reply in the low one; `poll_cycles_median` is the system-clock cycles from the start of the transaction to the last byte received, the select delay included only where the case waits it.
/// 0x6F0 the pad poll as the SDK did it before v2.13: select, 1,024 status reads, the bytes. 0x6F1 the select left open by the poll before and the bytes clocked one video frame later, as `psx_pad::Controllers` ships. 0x6F2 the same with half a second in between. 0x6F3 the bytes at once after the select, so the setup time is measured on root counter 0 alone. 0x6F4 a memory-card frame read on slot 1 between the open select and the bytes.
const SELECT_EARLY_RECORD: u16 = 0x6F0;
const EARLY_POLLS: usize = 8;
/// About one 60 Hz video frame of system-clock cycles, in pauses of 47,000.
const FRAME_PAUSES: u32 = 12;

/// What comes between the select and the bytes.
#[derive(Clone, Copy, PartialEq)]
enum Lead {
    /// The sequence before v2.13: select, then the fixed spin delay.
    Spins,
    /// The select opened by `Transport::open`, then `frames` video frames.
    Open { frames: u32 },
    /// The select opened, then a memory-card frame read on the same slot.
    OpenThenCard,
}

/// One pad poll on port 1 led in by `lead`: (clean, select kept, card frame
/// read, ID, third reply, cycles).
fn early_poll(lead: Lead) -> (bool, bool, bool, u8, u8, u16) {
    let mut port = token();
    let mut kept = false;
    let mut card_ok = false;
    let ready = match lead {
        Lead::Spins => {
            restart();
            port.select(Port::One);
            port.delay(Timing::PAD.setup_spins);
            port.drain_receive();
            port.wait_status_clear(sio0::stat::DSR_LEVEL, Timing::PAD.ack_spins)
        }
        Lead::Open { frames } => {
            let opened = port.open(Port::One);
            for _ in 0..frames * FRAME_PAUSES {
                pause(47_000);
            }
            kept = port.control() == sio0::selected_ctrl(false, false);
            restart();
            port.begin_pad(Port::One, Some(opened))
        }
        Lead::OpenThenCard => {
            let opened = port.open(Port::One);
            let mut buf = [0u8; 128];
            card_ok = HardwareCard::on_port(&mut port, Port::One)
                .read_frame(0, &mut buf)
                .is_ok();
            kept = port.control() == sio0::selected_ctrl(false, false);
            restart();
            port.begin_pad(Port::One, Some(opened))
        }
    };
    let mut ok = ready;
    let mut replies = [0xFFu8; 9];
    let mut length = 9;
    let mut i = 0;
    while ok && i < length {
        let tx = match i {
            0 => 0x01,
            1 => 0x42,
            _ => 0x00,
        };
        match port.exchange(Port::One, tx, i + 1 == length, Timing::PAD) {
            Ok(reply) => replies[i] = reply,
            Err(_) => ok = false,
        }
        if i == 1 && replies[1] == 0x41 {
            length = 5;
        }
        i += 1;
    }
    let cycles = count();
    port.finish(ok);
    (ok && replies[2] == 0x5A, kept, card_ok, replies[1], replies[2], cycles)
}

/// Five pass-or-fail cases for the pad's select left open (`psx_pad::Controllers`).
pub(crate) fn select_early(records: &mut Records, next: &mut usize) {
    // The pad driver times the setup on root counter 0, which psx-rt leaves
    // counting the system clock; earlier steps of this suite reprogram it.
    timers::set_mode(Timer::Timer0, 0);
    let leads = [
        Lead::Spins,
        Lead::Open { frames: 1 },
        Lead::Open { frames: 30 },
        Lead::Open { frames: 0 },
        Lead::OpenThenCard,
    ];
    for (case, lead) in leads.into_iter().enumerate() {
        let id = SELECT_EARLY_RECORD + case as u16;
        crate::bounds::record_start(id);
        let (mut clean, mut kept, mut cards) = (0u32, 0u32, 0u32);
        let mut answer = 0u32;
        let mut cycles = [0u32; EARLY_POLLS];
        let mut tried = 0usize;
        while tried < EARLY_POLLS {
            let (ok, held, card, id_low, magic, spent) = early_poll(lead);
            clean += ok as u32;
            kept += held as u32;
            cards += card as u32;
            answer = (u32::from(id_low) << 8) | u32::from(magic);
            cycles[tried] = u32::from(spent);
            tried += 1;
            // An empty port answers FF to everything: one poll says so.
            if id_low == 0xFF {
                break;
            }
        }
        let (_, median, _) = spread(&mut cycles[..tried]);
        let polls = clean | (tried as u32) << 4 | kept << 8 | cards << 12;
        push(records, next, record(id, polls, answer, median));
        pause(4_000);
    }
}
