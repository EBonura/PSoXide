// SPDX-License-Identifier: GPL-2.0-or-later
//! DualShock motors, on the wire, in the controller section of the run.
//!
//! The packet sequence is the documented one, written out here byte by byte so
//! the console's answers are the evidence (the SDK's motor API will replace
//! the packets, not the questions):
//!
//! * enter config mode: `01 43 00 01 00 00 00 00 00`;
//! * map the motors: `01 4D 00 00 01 FF FF FF FF` (poll byte 0 drives the
//!   small motor, byte 1 the large one; `FF` is unmapped); the reply carries
//!   the old mapping;
//! * leave config mode: `01 43 00 00 5A 5A 5A 5A 5A`;
//! * then every poll is `01 42 00 small large 00 00 00 00`, where the small
//!   motor is on for a value of `01` and the large one runs at the byte's
//!   level.
//!
//! A digital pad, or a pad that is absent, must refuse the configuration
//! without harm: its identifier stays `41` (or `FF`) and never reads `F3`.
//! An analog pad in digital mode is expected to accept it.
//!
//! The objective part records every reply identifier and the old mapping, the
//! cost of a poll with the motors idle and with both running, and that the
//! motors stop. The operator part runs the small motor, the large motor at 0,
//! 64, 128, 192 and 255, and two series of pulses (1, 2, 4, 8 and 15 frames
//! apart by as many), holding each for a second and asking whether it
//! vibrated: CROSS for yes, CIRCLE for no, three seconds to answer. No answer
//! is recorded as no answer. Every motor is stopped before the step ends,
//! with several polls of zeros.

use crate::console_tests::{record, spread};
use crate::sio_timing::{transaction, SAFE_SETUP};
use crate::ui;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_font::FontAtlas;
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec rumble_config: pad_id_plain, pad_id_in_config_mode, config_flags (port 1 then 2, three records each from 0x760)
const RUMBLE_CONFIG_RECORD: u16 = 0x760;
/// rec rumble_mapping: old_mapping_bytes_0_1, old_mapping_bytes_2_3, old_mapping_bytes_4_5 (0x761 and 0x764)
const RUMBLE_MAPPING_RECORD: u16 = 0x761;
/// rec rumble_after: id_and_5a_after_motor_poll, buttons_in_that_poll, id_after_stop_all (0x762 and 0x765)
const RUMBLE_AFTER_RECORD: u16 = 0x762;
/// rec rumble_operator: answers_two_bits_each, stimuli_asked, port_tested (0x766; answers 0 none, 1 yes, 2 no, in order: small, large 0 64 128 192 255, large pulses, small pulses)
const RUMBLE_OPERATOR_RECORD: u16 = 0x766;
/// rec rumble_poll_cost: poll_cycles_idle, poll_cycles_motors_on, polls_each (port 1 0x767, port 2 0x768)
const RUMBLE_COST_RECORD: u16 = 0x767;

const NONE: u32 = 0xFFFF;
const ENTER_CONFIG: [u8; 9] = [0x01, 0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
const MAP_MOTORS: [u8; 9] = [0x01, 0x4D, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF];
const EXIT_CONFIG: [u8; 9] = [0x01, 0x43, 0x00, 0x00, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A];
const QUERY: [u8; 9] = [0x01, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

fn poll(small: u8, large: u8) -> [u8; 9] {
    [0x01, 0x42, 0x00, small, large, 0, 0, 0, 0]
}

fn send(port2: bool, packet: &[u8; 9]) -> crate::sio_timing::Seen {
    transaction(port2, SAFE_SETUP, packet, true)
}

fn wait_frames(count: u32) {
    for _ in 0..count {
        let start = interrupts::vblank_count();
        let mut spins = 0u32;
        while interrupts::vblank_count() == start && spins < 20_000_000 {
            spins += 1;
        }
    }
}

/// Poll once a frame with the given motor bytes for `frames` frames; returns
/// the last poll's buttons (active low) and whether the pad answered.
fn hold(port2: bool, small: u8, large: u8, frames: u32) -> (u16, bool) {
    let mut buttons = 0xFFFFu16;
    let mut answered = false;
    for _ in 0..frames {
        wait_frames(1);
        let seen = send(port2, &poll(small, large));
        if seen.bytes[2].reply == 0x5A {
            answered = true;
            buttons = ((seen.bytes[3].reply as u16) << 8) | seen.bytes[4].reply as u16;
        }
    }
    (buttons, answered)
}

fn stop_all(port2: bool) {
    for _ in 0..4 {
        wait_frames(1);
        let _ = send(port2, &poll(0, 0));
    }
}

/// The three config-mode packets and the first motor poll, on one port.
/// Returns the identifier it saw, and whether a config-capable pad is there.
fn objective(port2: bool, records: &mut Records, next: &mut usize) -> bool {
    let base = RUMBLE_CONFIG_RECORD + 3 * port2 as u16;
    let plain = send(port2, &poll(0, 0));
    let id_plain = plain.bytes[1].reply;
    let present = plain.bytes[2].reply == 0x5A;
    let enter = send(port2, &ENTER_CONFIG);
    let query = send(port2, &QUERY);
    let in_config = query.bytes[1].reply == 0xF3;
    let map = send(port2, &MAP_MOTORS);
    let exit = send(port2, &EXIT_CONFIG);
    // The motors on, then the stop, as a game would.
    let motors = send(port2, &poll(0x01, 0xFF));
    let after_stop = {
        stop_all(port2);
        send(port2, &poll(0, 0))
    };
    let flags = (enter.bytes[2].reply == 0x5A) as u32
        | ((in_config as u32) << 1)
        | (((map.bytes[2].reply == 0x5A) as u32) << 2)
        | (((map.bytes[1].reply == 0xF3) as u32) << 3)
        | (((exit.bytes[1].reply == 0xF3) as u32) << 4)
        | (((motors.bytes[1].reply == id_plain) as u32) << 5)
        | (((motors.bytes[2].reply == 0x5A) as u32) << 6)
        | (((!present) as u32) << 7)
        | (((query.bytes[2].reply == 0x5A) as u32) << 8);
    push_timing_record(
        records,
        next,
        record(
            base,
            id_plain as u32,
            query.bytes[1].reply as u32 | ((enter.bytes[1].reply as u32) << 8),
            flags,
        ),
    );
    let m = |a: usize, b: usize| ((map.bytes[a].reply as u32) << 8) | map.bytes[b].reply as u32;
    push_timing_record(records, next, record(base + 1, m(3, 4), m(5, 6), m(7, 8)));
    push_timing_record(
        records,
        next,
        record(
            base + 2,
            ((motors.bytes[1].reply as u32) << 8) | motors.bytes[2].reply as u32,
            ((motors.bytes[3].reply as u32) << 8) | motors.bytes[4].reply as u32,
            after_stop.bytes[1].reply as u32,
        ),
    );
    present && in_config
}

/// Poll cost with the motors idle and with both running, in system-clock
/// cycles from select to release, median of eight polls each.
fn poll_cost(port2: bool, records: &mut Records, next: &mut usize) {
    let mut idle = [0u32; 8];
    let mut busy = [0u32; 8];
    for slot in idle.iter_mut() {
        wait_frames(1);
        *slot = send(port2, &poll(0, 0)).total as u32;
    }
    for slot in busy.iter_mut() {
        wait_frames(1);
        *slot = send(port2, &poll(0x01, 0xFF)).total as u32;
    }
    stop_all(port2);
    let (_, idle_med, _) = spread(&mut idle);
    let (_, busy_med, _) = spread(&mut busy);
    push_timing_record(
        records,
        next,
        record(RUMBLE_COST_RECORD + port2 as u16, idle_med, busy_med, 8),
    );
}

/// Ask `question` and wait three seconds for CROSS (yes) or CIRCLE (no) on
/// the pad, polled once a frame with the motors at zero. 1 yes, 2 no, 0 none.
fn ask(font: &FontAtlas, port2: bool, question: &str) -> u32 {
    ui::detail(font, "MOTOR", question);
    // Let a button still held from the last answer go first.
    let mut released = 0;
    for _ in 0..90 {
        wait_frames(1);
        let seen = send(port2, &poll(0, 0));
        let buttons = seen.bytes[4].reply;
        let pressed = seen.bytes[2].reply == 0x5A && (buttons & 0x60) != 0x60;
        if pressed {
            released = 0;
        } else {
            released += 1;
        }
        if released >= 6 {
            break;
        }
    }
    for _ in 0..180 {
        wait_frames(1);
        let seen = send(port2, &poll(0, 0));
        if seen.bytes[2].reply != 0x5A {
            continue;
        }
        let buttons = seen.bytes[4].reply;
        if buttons & 0x40 == 0 {
            return 1;
        }
        if buttons & 0x20 == 0 {
            return 2;
        }
    }
    0
}

fn prompt(font: &FontAtlas, port2: bool, what: &str, small: u8, large: u8) -> u32 {
    ui::detail(font, "MOTOR", what);
    let _ = hold(port2, small, large, 60);
    stop_all(port2);
    let mut line = ui::Line::new();
    line.s(what).s(" FELT? X YES O NO");
    ask(font, port2, line.as_str())
}

/// Pulses of 1, 2, 4, 8 and 15 frames, each followed by as many frames of
/// stillness.
fn pulses(port2: bool, small: u8, large: u8) {
    for length in [1u32, 2, 4, 8, 15] {
        let _ = hold(port2, small, large, length);
        stop_all(port2);
        wait_frames(length.min(8));
    }
}

fn operator(font: &FontAtlas, port2: bool, records: &mut Records, next: &mut usize) {
    let mut answers = 0u32;
    let mut asked = 0u32;
    let mut put = |value: u32| {
        answers |= value << (2 * asked);
        asked += 1;
    };
    put(prompt(font, port2, "SMALL ON", 0x01, 0x00));
    for level in [0u8, 64, 128, 192, 255] {
        let mut line = ui::Line::new();
        line.s("LARGE ").u(level as u32);
        put(prompt(font, port2, line.as_str(), 0x00, level));
    }
    ui::detail(font, "MOTOR", "LARGE PULSES 1 2 4 8 15 FRAMES");
    pulses(port2, 0x00, 0xFF);
    put(ask(font, port2, "LARGE PULSES FELT? X YES O NO"));
    ui::detail(font, "MOTOR", "SMALL PULSES 1 2 4 8 15 FRAMES");
    pulses(port2, 0x01, 0x00);
    put(ask(font, port2, "SMALL PULSES FELT? X YES O NO"));
    stop_all(port2);
    push_timing_record(
        records,
        next,
        record(RUMBLE_OPERATOR_RECORD, answers, asked, port2 as u32 + 1),
    );
}

/// The whole step. The operator part runs on the first port that answers the
/// config packets; with none, its record says so (`0xFFFF`).
pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    let mut tested = None;
    for port2 in [false, true] {
        if objective(port2, records, next) && tested.is_none() {
            tested = Some(port2);
        }
        poll_cost(port2, records, next);
    }
    match tested {
        Some(port2) => {
            // Map the motors again: leaving config mode is not remembered
            // across a power cycle but is across polls, so this is only
            // belt and braces after the objective pass.
            let _ = send(port2, &ENTER_CONFIG);
            let _ = send(port2, &MAP_MOTORS);
            let _ = send(port2, &EXIT_CONFIG);
            operator(font, port2, records, next);
        }
        None => push_timing_record(records, next, record(RUMBLE_OPERATOR_RECORD, NONE, 0, NONE)),
    }
}
