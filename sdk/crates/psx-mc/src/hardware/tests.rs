//! The card protocol against a model of a memory card on the shared SIO0
//! transport: the same `Transport` steps `psx-pad` polls a controller with.

extern crate std;

use super::*;
use psx_hw::sio::sio0;
use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

/// Ticks between a byte going out and its reply, and between the byte and the
/// card's `/ACK` pulse. Not calibrated to a real card; they only order the
/// events the driver waits for.
const REPLY_TICKS: u64 = 3;
const ACK_DELAY_TICKS: u64 = 8;
const ACK_WIDTH_TICKS: u64 = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Normal,
    /// No card: every byte reads 0xFF and nothing is acknowledged.
    Absent,
    /// A previous transaction left `/ACK` asserted for good.
    AckHeld,
    /// The checksum a read returns is wrong.
    BadReadChecksum,
}

/// A memory card, as the port sees it.
struct CardBus {
    image: Vec<u8>,
    behaviour: Behaviour,
    selected: bool,
    now: u64,
    index: usize,
    command: u8,
    msb: u8,
    lsb: u8,
    staged: [u8; FRAME_SIZE],
    staged_ok: bool,
    replies: VecDeque<(u8, u64)>,
    ack_from: u64,
    ack_until: u64,
    /// Every control-register write, in order.
    control: Vec<u16>,
    /// Control writes made while a byte exchange was under way.
    control_between_bytes: usize,
    sent: usize,
}

impl CardBus {
    fn new(behaviour: Behaviour) -> Self {
        let mut image = vec![0u8; FRAME_COUNT * FRAME_SIZE];
        for (i, byte) in image.iter_mut().enumerate() {
            *byte = (i * 7 + i / 128) as u8;
        }
        let mut bus = CardBus {
            image,
            behaviour,
            selected: false,
            now: 0,
            index: 0,
            command: 0,
            msb: 0,
            lsb: 0,
            staged: [0; FRAME_SIZE],
            staged_ok: false,
            replies: VecDeque::new(),
            ack_from: 0,
            ack_until: 0,
            control: Vec::new(),
            control_between_bytes: 0,
            sent: 0,
        };
        if behaviour == Behaviour::AckHeld {
            bus.ack_from = 1;
            bus.ack_until = u64::MAX;
        }
        bus
    }

    fn frame(&self, frame: u16) -> &[u8] {
        let start = frame as usize * FRAME_SIZE;
        &self.image[start..start + FRAME_SIZE]
    }

    /// The reply to the byte at packet position `index`.
    fn reply(&self, index: usize) -> u8 {
        if self.behaviour == Behaviour::Absent {
            return 0xFF;
        }
        let checksum = |data: &[u8]| data.iter().fold(self.msb ^ self.lsb, |a, b| a ^ b);
        match (self.command, index) {
            (_, 0) => 0xFF,
            (_, 1) => 0x08,
            (_, 2) => ID1,
            (_, 3) => 0x5D,
            (_, 4) => 0x00,
            (_, 5) => self.msb,
            (CMD_READ, 6) => ACK1,
            (CMD_READ, 7) => 0x5D,
            (CMD_READ, 8) => self.msb,
            (CMD_READ, 9) => self.lsb,
            (CMD_READ, 10..=137) => self.frame(u16::from_be_bytes([self.msb, self.lsb]))[index - 10],
            (CMD_READ, 138) => {
                let sum = checksum(self.frame(u16::from_be_bytes([self.msb, self.lsb])));
                if self.behaviour == Behaviour::BadReadChecksum {
                    !sum
                } else {
                    sum
                }
            }
            (CMD_READ, 139) => END_GOOD,
            (CMD_WRITE, 6..=134) => 0x00,
            (CMD_WRITE, 135) => ACK1,
            (CMD_WRITE, 136) => 0x5D,
            (CMD_WRITE, 137) => {
                if self.staged_ok {
                    END_GOOD
                } else {
                    0x4E
                }
            }
            _ => 0x00,
        }
    }

    /// Index of the last byte of the current command's packet.
    fn last_index(&self) -> usize {
        if self.command == CMD_READ {
            139
        } else {
            137
        }
    }

    fn end_transaction(&mut self) {
        if self.command == CMD_WRITE && self.staged_ok {
            let start = u16::from_be_bytes([self.msb, self.lsb]) as usize * FRAME_SIZE;
            self.image[start..start + FRAME_SIZE].copy_from_slice(&self.staged);
        }
        self.index = 0;
        self.command = 0;
        self.staged_ok = false;
        self.ack_from = 0;
        self.ack_until = 0;
    }
}

impl Transport for CardBus {
    fn status(&mut self) -> u32 {
        self.now += 1;
        let mut status = sio0::stat::TX_READY;
        if self.replies.front().is_some_and(|&(_, at)| self.now >= at) {
            status |= sio0::stat::RX_NOT_EMPTY;
        }
        if self.now >= self.ack_from && self.now < self.ack_until {
            status |= sio0::stat::DSR_LEVEL;
        }
        status
    }

    fn receive(&mut self) -> u8 {
        self.now += 1;
        match self.replies.front() {
            Some(&(byte, at)) if self.now >= at => {
                self.replies.pop_front();
                byte
            }
            _ => panic!("read of an empty RX FIFO"),
        }
    }

    fn transmit(&mut self, byte: u8) {
        self.now += 1;
        assert!(self.selected, "byte sent while deselected");
        let index = self.index;
        match index {
            1 => self.command = byte,
            4 => self.msb = byte,
            5 => self.lsb = byte,
            6..=133 if self.command == CMD_WRITE => self.staged[index - 6] = byte,
            134 if self.command == CMD_WRITE => {
                let sum = self.staged.iter().fold(self.msb ^ self.lsb, |a, b| a ^ b);
                self.staged_ok = byte == sum;
            }
            _ => {}
        }
        let reply = self.reply(index);
        self.replies.push_back((reply, self.now + REPLY_TICKS));
        if self.behaviour != Behaviour::Absent && index < self.last_index().max(1) {
            self.ack_from = self.now + ACK_DELAY_TICKS;
            self.ack_until = self.ack_from + ACK_WIDTH_TICKS;
        }
        self.index += 1;
        self.sent += 1;
    }

    fn set_mode(&mut self, _value: u16) {}
    fn set_baud(&mut self, _value: u16) {}

    fn set_control(&mut self, value: u16) {
        self.now += 1;
        if self.selected && value & sio0::ctrl::DTR != 0 && self.index != 0 {
            self.control_between_bytes += 1;
        }
        self.control.push(value);
        if value & sio0::ctrl::RESET != 0 {
            self.replies.clear();
        }
        let selected = value & sio0::ctrl::DTR != 0;
        if self.selected && !selected {
            self.end_transaction();
        }
        self.selected = selected;
    }
}

fn read(bus: &mut CardBus, frame: u16) -> (Result<()>, [u8; FRAME_SIZE], TransportTrace) {
    let mut out = [0u8; FRAME_SIZE];
    let mut trace = TransportTrace::new();
    let result = transact(bus, Slot::One, Timing::default(), &mut trace, |link| {
        link.read_frame(frame, &mut out)
    });
    (result, out, trace)
}

fn write(bus: &mut CardBus, frame: u16, data: &[u8; FRAME_SIZE]) -> (Result<()>, TransportTrace) {
    let mut trace = TransportTrace::new();
    let result = transact(bus, Slot::One, Timing::default(), &mut trace, |link| {
        link.write_frame(frame, data)
    });
    (result, trace)
}

#[test]
fn a_frame_reads_back_what_the_card_holds() {
    let mut bus = CardBus::new(Behaviour::Normal);
    let (result, out, trace) = read(&mut bus, 0x123);
    assert_eq!(result, Ok(()));
    assert_eq!(&out[..], bus.frame(0x123));
    // 140 byte exchanges, every one but the terminator acknowledged.
    assert_eq!((trace.exchanges, trace.acknowledgements), (140, 139));
    assert_eq!(trace.fault, TransportFault::None);
    assert_eq!(&trace.response_prefix[..3], &[0xFF, 0x08, ID1]);
}

#[test]
fn the_card_uses_the_pad_transport_not_a_private_one() {
    let mut bus = CardBus::new(Behaviour::Normal);
    let (result, _, _) = read(&mut bus, 5);
    assert_eq!(result, Ok(()));
    // The select clears the latch, asserts the port without arming the /ACK
    // interrupt, and the release drops it; nothing is written between bytes.
    assert_eq!(bus.control, [sio0::ctrl::ACK, 0x0003, 0]);
    assert_eq!(bus.control_between_bytes, 0);
    assert!(bus.control.iter().all(|c| c & sio0::ctrl::ACK_IRQ_EN == 0));
}

#[test]
#[cfg_attr(miri, ignore = "the post-write settle is hundreds of thousands of interpreted reads")]
fn a_written_frame_lands_on_the_card_and_reads_back() {
    let mut bus = CardBus::new(Behaviour::Normal);
    let data = core::array::from_fn(|i| (i as u8).wrapping_mul(3) ^ 0xA5);
    let (result, trace) = write(&mut bus, 0x2FF, &data);
    assert_eq!(result, Ok(()));
    assert_eq!((trace.exchanges, trace.acknowledgements), (138, 137));
    assert_eq!(bus.frame(0x2FF), &data[..]);
    let (result, out, _) = read(&mut bus, 0x2FF);
    assert_eq!(result, Ok(()));
    assert_eq!(out, data);
}

#[test]
fn a_wrong_checksum_is_reported() {
    let mut bus = CardBus::new(Behaviour::BadReadChecksum);
    let (result, _, trace) = read(&mut bus, 9);
    assert_eq!(result, Err(Error::BadChecksum));
    assert_eq!(trace.fault, TransportFault::None);
}

#[test]
fn an_empty_slot_reports_no_card_and_resets_the_port() {
    let mut bus = CardBus::new(Behaviour::Absent);
    let (result, _, trace) = read(&mut bus, 0);
    assert_eq!(result, Err(Error::NoCard));
    assert_eq!(trace.fault, TransportFault::AckTimeout);
    assert_eq!(trace.fault_exchange, 0);
    // A failed transaction ends with a release and a UART reset.
    assert_eq!(&bus.control[bus.control.len() - 2..], [0, sio0::ctrl::RESET]);
}

#[test]
fn an_ack_left_asserted_by_an_earlier_transaction_fails_the_select() {
    let mut bus = CardBus::new(Behaviour::AckHeld);
    let (result, _, trace) = read(&mut bus, 0);
    assert_eq!(result, Err(Error::NoCard));
    assert_eq!(trace.fault, TransportFault::AckReleaseTimeout);
    assert_eq!(bus.sent, 0, "no byte may go out while the line is held");
}

#[test]
fn the_card_waits_longer_for_ack_than_a_pad_and_otherwise_matches_it() {
    let card = Timing::default().link();
    let pad = psx_io::controller_port::Timing::PAD;
    assert_eq!(card, psx_io::controller_port::Timing::CARD);
    assert_eq!((card.setup_spins, card.byte_spins), (pad.setup_spins, pad.byte_spins));
    assert!(card.ack_spins > pad.ack_spins);
}

#[test]
fn the_card_hands_the_port_back() {
    // SAFETY: a test-local token on the host; no register is touched.
    let token = unsafe { ControllerPort::steal() };
    let card = HardwareCard::on_port(token, Slot::Two);
    let _token: ControllerPort = card.release();

    // SAFETY: as above.
    let mut port = unsafe { ControllerPort::steal() };
    let card = HardwareCard::on_port(&mut port, Slot::One);
    let _borrow: &mut ControllerPort = card.release();
}

#[test]
fn a_frame_past_the_end_is_refused_before_the_port_is_touched() {
    // SAFETY: a test-local token on the host; the call below never reaches a
    // register because the range check comes first.
    let mut card = HardwareCard::on_port(unsafe { ControllerPort::steal() }, Slot::One);
    let mut out = [0u8; FRAME_SIZE];
    assert_eq!(
        card.read_frame(FRAME_COUNT as u16, &mut out),
        Err(Error::OutOfRange)
    );
    assert_eq!(
        card.write_frame(u16::MAX, &out),
        Err(Error::OutOfRange)
    );
}
