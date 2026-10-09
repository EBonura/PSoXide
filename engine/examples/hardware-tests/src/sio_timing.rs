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
use psx_io::controller_port::{Port, Transport};
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
    done: u16,
    /// Cycles from the write to `/ACK` asserting, and to it releasing.
    rise: u16,
    fall: u16,
}

impl Byte {
    const fn none() -> Self {
        Self {
            reply: 0xFF,
            done: NONE,
            rise: NONE,
            fall: NONE,
        }
    }

    const fn acked(&self) -> bool {
        self.rise != NONE
    }

    const fn width(&self) -> u16 {
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
    /// System-clock cycles from select to release.
    pub(crate) total: u16,
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
    }
    seen.total = count();
    port.deselect();
    seen.status = port.status();
    port.reset();
    // Let the device and the line settle before anyone selects again.
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
    let mut pad_ok = 0u32;
    let mut card_ok = 0u32;
    let (mut no_card, mut protocol, mut checksum, mut pad_lost) = (0u32, 0u32, 0u32, 0u32);
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
        match result {
            Ok(()) => card_ok += 1,
            Err(CardError::NoCard) => no_card += 1,
            Err(CardError::BadChecksum) => checksum += 1,
            Err(_) => protocol += 1,
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
            }
            continue;
        }
        mix(port2, false, records, next);
        mix(port2, true, records, next);
    }
}
