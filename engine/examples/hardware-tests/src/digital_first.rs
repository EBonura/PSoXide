// SPDX-License-Identifier: GPL-2.0-or-later
//! PAD DIGITAL FIRST POLL: how soon `psx_pad::Controllers::digital()` reads a DualShock, in each of the
//! DualShock's modes, against `Controllers::new()` as the reference.
//!
//! A pad that is not read from its first answer loses input. PCSX-ReARMed's DualShock acknowledges the address
//! byte and answers `0xFF` to the command byte, as an empty socket does on that port, until about 33 video frames have gone by;
//! `Controllers` used to leave a port that answered like that alone for 59 polls, so a game built on
//! `digital()` ignored the pad for two seconds (the `0x41` ID came at the 121st poll). A console DualShock
//! should answer the first poll; this step is the console's word on it, in both modes, and on whether anything
//! in the first polls looks like the ReARMed case.
//!
//! Each case is [`ROUNDS`] rounds. A round puts the pad in the case's mode (digital: enter configuration, set
//! digital and unlocked, exit; analog: enter configuration, set analog and lock, exit; then a poll that must
//! read ID `0x41` or `0x73`), makes a fresh driver and polls port 1 once a video frame, [`POLLS`] times, with
//! VBlank interrupts on as in a game. It notes the first poll that read a connected pad.
//!
//! * `0x6FC`: `Controllers::digital()`, pad in digital mode.
//! * `0x6FD`: `Controllers::digital()`, pad in analog mode (digital() sends no handshake and reads what the
//!   pad answers).
//! * `0x6FE`: `Controllers::new()`, pad in digital mode: the first poll runs the handshake and the pad
//!   ends analog.
//!
//! Fields of each record (16 bits each, so counts are four bits):
//!
//! * `first`: the largest first-connected poll over the rounds (0 is the first poll), `0xFFFF` when a round
//!   never read a pad in [`POLLS`] polls.
//! * `bytes_seen`: the ID byte of the last poll of the last round (high byte: `0x41` digital, `0x73`
//!   analog, `0xFF` none) and its mode (low byte, `PadMode` as a number: 1 digital, 2 analog).
//! * `rounds`: rounds read from the first poll (bits 0-3), read from a later poll (4-7), never read (8-11), the
//!   pad would not enter the case's mode (12-15).
//!
//! Pass, with a DualShock in port 1: every case reads every round from the first poll (`rounds` `0x0002`,
//! `first` 0), with `bytes_seen` `0x4101` for `0x6FC` and `0x7302` for `0x6FD` and `0x6FE`. Fail: any round
//! read late or never. A late first poll is a pad that answers like an empty socket for a while, which
//! `Controllers` polls every video frame for its first 128 polls; past that it is read at the next probe
//! (one poll in 60) and `first` says how late.

use crate::config_irq::{make_digital, poll_id, send, token};
use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_io::controller_port::Port;
use psx_pad::{Controllers, PadMode};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// First record of the step: three records, `0x6FC` to `0x6FE`.
const RECORD: u16 = 0x6FC;
/// Rounds a case runs.
const ROUNDS: u32 = 2;
/// Polls a round makes, one a video frame: past the 128 early polls of an absent port.
const POLLS: u32 = 130;

/// Put the pad in analog mode and locked, and say whether a poll then answers ID `0x73`.
fn make_analog() -> bool {
    let mut port = token();
    send(&mut port, [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
    send(&mut port, [0x44, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00]);
    send(&mut port, [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    poll_id(&mut port) == 0x73
}

/// Which driver and which pad mode a case runs.
#[derive(Clone, Copy)]
enum Case {
    DigitalDriverDigitalPad,
    DigitalDriverAnalogPad,
    FullDriverDigitalPad,
}

/// One case: [`ROUNDS`] rounds, one record.
///
/// rec pad_first_poll: first_poll_worst, id_and_mode, rounds
fn case(records: &mut Records, next: &mut usize, index: u16, which: Case) {
    let id = RECORD + index;
    crate::bounds::record_start(id);
    let (mut at_once, mut later, mut never, mut refused) = (0u32, 0u32, 0u32, 0u32);
    let mut worst = 0u32;
    let mut last_id = 0xFFu32;
    let mut last_mode = 0u32;
    let mut round = 0;
    while round < ROUNDS {
        round += 1;
        let ready = match which {
            Case::DigitalDriverAnalogPad => make_analog() || make_analog(),
            _ => make_digital() || make_digital(),
        };
        if !ready {
            refused += 1;
            continue;
        }
        let mut digital = Controllers::digital();
        let mut full = Controllers::new();
        let mut port = token();
        let mut first = None;
        let mut poll = 0;
        while poll < POLLS {
            psx_rt::interrupts::wait_vblank();
            let state = *match which {
                Case::FullDriverDigitalPad => full.poll(&mut port, Port::One),
                _ => digital.poll(&mut port, Port::One),
            };
            if first.is_none() && matches!(state.mode, PadMode::Digital | PadMode::Analog) {
                first = Some(poll);
            }
            last_id = u32::from(state.id_low);
            last_mode = state.mode as u32;
            poll += 1;
        }
        match first {
            Some(0) => at_once += 1,
            Some(n) => {
                later += 1;
                worst = worst.max(n);
            }
            None => {
                never += 1;
                worst = u32::MAX;
            }
        }
    }
    let rounds = at_once | later << 4 | never << 8 | refused << 12;
    push_timing_record(records, next, record(id, worst, last_id << 8 | last_mode, rounds));
}

/// The three cases. CPU interrupts must be on, as in a game; VBlank is waited for once a poll.
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    case(records, next, 0, Case::DigitalDriverDigitalPad);
    case(records, next, 1, Case::DigitalDriverAnalogPad);
    case(records, next, 2, Case::FullDriverDigitalPad);
}
