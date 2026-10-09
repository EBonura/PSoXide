// SPDX-License-Identifier: GPL-2.0-or-later
//! DualShock motors, on the wire, in the controller section of the run.
//!
//! The objective part writes the documented packet sequence out byte by byte so
//! the console's raw answers are the evidence; the operator part drives the
//! motors through the SDK's own API (`psx_pad::enable_rumble_on` and
//! `poll_rumble_on`), so the console tests what ships. The packets:
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
use psx_io::controller_port::Port;
use psx_io::periph::ControllerPort;
use psx_pad::{button, Rumble};
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec rumble_config: pad_id_plain, pad_id_in_config_mode, config_flags (port 1 then 2, three records each from 0x760)
const RUMBLE_CONFIG_RECORD: u16 = 0x760;
/// rec rumble_mapping: old_mapping_bytes_0_1, old_mapping_bytes_2_3, old_mapping_bytes_4_5 (0x761 and 0x764)
const RUMBLE_MAPPING_RECORD: u16 = 0x761;
/// rec rumble_after: id_and_5a_after_motor_poll, buttons_in_that_poll, id_after_stop_all (0x762 and 0x765)
const RUMBLE_AFTER_RECORD: u16 = 0x762;
/// rec rumble_operator: answers_two_bits_each, stimuli_asked, port_tested_and_api_enabled_in_bit_8 (0x766; answers 0 none, 1 yes, 2 no, in order: small, large 0 64 128 192 255, large pulses, small pulses)
const RUMBLE_OPERATOR_RECORD: u16 = 0x766;
/// rec rumble_poll_cost: poll_cycles_idle, poll_cycles_motors_on, polls_each (port 1 0x767, port 2 0x768; cycles from the first byte written to the last byte received, v2.3; v2.2 recorded the last byte's unanswered timeout window)
const RUMBLE_COST_RECORD: u16 = 0x767;

/// rec rumble_enter_a: last_attempt_replies_1_2, replies_3_4, replies_5_6 (the nine-byte reply to Enter Config, bytes 1 to 8 as pairs; port 1 0x769, port 2 0x76B; v2.1 saw 0xFF with no 0x5A)
const RUMBLE_ENTER_A_RECORD: u16 = 0x769;
/// rec rumble_enter_b: last_attempt_replies_7_8, attempts_made, first_attempt_replies_1_2 (port 1 0x76A, port 2 0x76C; up to four attempts a frame apart, stopping at the first 0x5A)
const RUMBLE_ENTER_B_RECORD: u16 = 0x76A;
/// rec pad_identity: reply_id_and_5a, reply_buttons, reply_sticks_01 (a plain poll, bytes 1 to 6 of the reply as pairs; port 1 0x76D, port 2 0x76E; the id says digital 0x41, analog 0x73, config 0xF3)
const PAD_IDENTITY_RECORD: u16 = 0x76D;
/// rec pad_model: query_replies_3_4, query_replies_5_6, query_replies_7_8 (the 0x45 model query after the attempts to enter config mode; port 1 0x76F, port 2 0x770; bytes 3 to 8 are the model, mode and LED bytes when the pad is in config mode, 0xFF otherwise)
const PAD_MODEL_RECORD: u16 = 0x76F;
/// rec rumble_answer_buttons: raw_buttons_stimulus_0, raw_buttons_stimulus_1, raw_buttons_stimulus_2 (0x771 stimuli 0 to 2, 0x772 stimuli 3 to 5, 0x773 stimuli 6 and 7 then a mask of the questions whose buttons were never let go; a word is the pad's active-high button bits when the answer was taken, 0xFFFF for no answer)
const RUMBLE_ANSWER_BUTTONS_RECORD: u16 = 0x771;
/// rec rumble_answer_frames: answer_frame_stimulus_0, answer_frame_stimulus_1, answer_frame_stimulus_2 (frames from the question appearing to the answer; 0x774 stimuli 0 to 2, 0x775 stimuli 3 to 5, 0x776 stimuli 6 and 7 then a mask of the questions that saw a button down during the first second, when input is ignored; 0xFFFF for no answer)
const RUMBLE_ANSWER_FRAMES_RECORD: u16 = 0x774;
/// Frames a question stays up with the pad ignored.
const ASK_SHOW_FRAMES: u32 = 60;
/// Consecutive frames with no button down before a question takes a press.
const ASK_RELEASED_FRAMES: u32 = 6;
/// Frames after the first second in which the release and then the press must come.
const ASK_ANSWER_FRAMES: u32 = 240;
/// The second pad's records are the first's plus this: 0x760 + 0x1E0 = 0x940.
const PASS_TWO_OFFSET: u16 = 0x1E0;
static mut ID_OFFSET: u16 = 0;

/// A record id of the pass in progress.
fn rid(base: u16) -> u16 {
    // SAFETY: single thread; a plain static.
    base + unsafe { ID_OFFSET }
}

fn set_pass(second: bool) {
    // SAFETY: single thread; a plain static.
    unsafe { ID_OFFSET = if second { PASS_TWO_OFFSET } else { 0 } };
}

/// rec pad_choice: model_code, raw_buttons, answer_frame (which pad the operator said was in port 1: code 1 SCPH-1200 by CROSS, 2 SCPH-110 by CIRCLE, 3 another pad by SQUARE, 4 no pad or a digital one by TRIANGLE, 0 no answer; the raw button word and the frame the answer arrived on; 0x960 the first pad, 0x961 the pad after the swap)
const PAD_CHOICE_RECORD: u16 = 0x960;
/// rec pad_passes: flags, config_ports, answer_flags (0x962; flags bit 0 the second pass ran, bit 1 the first pass found a pad that took the config packets, bit 2 the second; config_ports the port the operator part ran on, 1 or 2, first pass in the low byte and second in the high byte, 0 for none; answer_flags bit 0 the first model question's release wait ran out, bit 1 its first second saw a button down, bits 2 and 3 the same for the second question)
const PAD_PASSES_RECORD: u16 = 0x962;
/// Frames the swap and model question may take for the pad after the swap.
const SWAP_ANSWER_FRAMES: u32 = 1_500;
/// Frames the first model question may take.
const MODEL_ANSWER_FRAMES: u32 = 600;
/// Attempts at Enter Config, a frame apart.
const ENTER_ATTEMPTS: u32 = 4;

const NONE: u32 = 0xFFFF;
/// Questions the operator part asks.
const STIMULI: usize = 8;
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

fn socket(port2: bool) -> Port {
    if port2 {
        Port::Two
    } else {
        Port::One
    }
}

fn api_poll(port2: bool, rumble: Rumble) -> psx_pad::PadState {
    // SAFETY: a token is a logic guard; nothing else drives SIO0 here.
    let mut port = unsafe { ControllerPort::steal() };
    psx_pad::poll_rumble_on(&mut port, socket(port2), rumble)
}

/// Poll once a frame through the SDK with the given motors for `frames`
/// frames.
fn hold(port2: bool, small: u8, large: u8, frames: u32) {
    for _ in 0..frames {
        wait_frames(1);
        let _ = api_poll(port2, Rumble::new(small != 0, large));
    }
}

fn stop_all(port2: bool) {
    for _ in 0..4 {
        wait_frames(1);
        let _ = api_poll(port2, Rumble::OFF);
    }
}

/// The three config-mode packets and the first motor poll, on one port.
/// Returns the identifier it saw, and whether a config-capable pad is there.
fn pair(seen: &crate::sio_timing::Seen, a: usize) -> u32 {
    ((seen.bytes[a].reply as u32) << 8) | seen.bytes[a + 1].reply as u32
}

/// A plain poll, its first six reply bytes recorded and put on the screen
/// (`PAD n ID 73 5A BTN ...`), so a film of the run says which pad it was.
fn identity(font: &FontAtlas, port2: bool, records: &mut Records, next: &mut usize) -> u8 {
    let id_record = rid(PAD_IDENTITY_RECORD + port2 as u16);
    crate::bounds::record_start(id_record);
    wait_frames(1);
    let seen = send(port2, &poll(0, 0));
    push_timing_record(
        records,
        next,
        record(id_record, pair(&seen, 1), pair(&seen, 3), pair(&seen, 5)),
    );
    let mut line = ui::Line::new();
    line.s("PORT ")
        .u(port2 as u32 + 1)
        .s(" REC ")
        .hex(id_record as u32, 3)
        .s(" ID ")
        .hex(seen.bytes[1].reply as u32, 2)
        .s(" ")
        .hex(seen.bytes[2].reply as u32, 2)
        .s(" ")
        .hex(pair(&seen, 3), 4)
        .s(" ")
        .hex(pair(&seen, 5), 4);
    ui::detail(font, "PAD", line.as_str());
    wait_frames(90);
    seen.bytes[1].reply
}

/// The three config-mode packets and the first motor poll, on one port, each
/// packet a frame after the last. Enter Config is tried up to four times; its
/// reply bytes and the model query's are recorded and shown. Returns whether a
/// config-capable pad is there.
fn objective(font: &FontAtlas, port2: bool, records: &mut Records, next: &mut usize) -> bool {
    let slot = 3 * port2 as u16;
    let base = rid(RUMBLE_CONFIG_RECORD + slot);
    let id_plain = identity(font, port2, records, next);
    crate::bounds::record_start(base);
    wait_frames(1);
    let plain = send(port2, &poll(0, 0));
    let present = plain.bytes[2].reply == 0x5A;
    let enter_record = rid(RUMBLE_ENTER_A_RECORD + 2 * port2 as u16);
    crate::bounds::record_start(enter_record);
    let mut attempts = 0u32;
    let mut first_replies = 0u32;
    let mut enter = send(port2, &ENTER_CONFIG);
    for attempt in 0..ENTER_ATTEMPTS {
        if attempt != 0 {
            wait_frames(1);
            enter = send(port2, &ENTER_CONFIG);
        }
        attempts += 1;
        if attempt == 0 {
            first_replies = pair(&enter, 1);
        }
        if enter.bytes[2].reply == 0x5A {
            break;
        }
    }
    push_timing_record(
        records,
        next,
        record(
            enter_record,
            pair(&enter, 1),
            pair(&enter, 3),
            pair(&enter, 5),
        ),
    );
    push_timing_record(
        records,
        next,
        record(
            rid(RUMBLE_ENTER_B_RECORD + 2 * port2 as u16),
            pair(&enter, 7),
            attempts,
            first_replies,
        ),
    );
    wait_frames(1);
    let query = send(port2, &QUERY);
    let model_record = rid(PAD_MODEL_RECORD + port2 as u16);
    push_timing_record(
        records,
        next,
        record(
            model_record,
            pair(&query, 3),
            pair(&query, 5),
            pair(&query, 7),
        ),
    );
    let mut line = ui::Line::new();
    line.s("P")
        .u(port2 as u32 + 1)
        .s(" ENTER ")
        .u(attempts)
        .s("X ")
        .hex(pair(&enter, 1), 4)
        .s(" Q ")
        .hex(pair(&query, 1), 4)
        .s(" ")
        .hex(pair(&query, 3), 4)
        .s(" ")
        .hex(pair(&query, 5), 4);
    ui::detail(font, "CFG", line.as_str());
    wait_frames(90);
    let in_config = query.bytes[1].reply == 0xF3;
    wait_frames(1);
    let map = send(port2, &MAP_MOTORS);
    wait_frames(1);
    let exit = send(port2, &EXIT_CONFIG);
    // The motors on, then the stop, as a game would.
    wait_frames(1);
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
    push_timing_record(
        records,
        next,
        record(rid(RUMBLE_MAPPING_RECORD + slot), m(3, 4), m(5, 6), m(7, 8)),
    );
    push_timing_record(
        records,
        next,
        record(
            rid(RUMBLE_AFTER_RECORD + slot),
            ((motors.bytes[1].reply as u32) << 8) | motors.bytes[2].reply as u32,
            ((motors.bytes[3].reply as u32) << 8) | motors.bytes[4].reply as u32,
            after_stop.bytes[1].reply as u32,
        ),
    );
    present && in_config
}

/// Poll cost with the motors idle and with both running, in system-clock
/// cycles from the first byte written to the last byte received (the select
/// delay and the wait for an answer that never comes after the last byte are
/// not in it), median of eight polls each. v2.2 used the transaction's total,
/// which is the last byte's unanswered window.
fn poll_cost(port2: bool, records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(rid(RUMBLE_COST_RECORD + port2 as u16));
    let mut idle = [0u32; 8];
    let mut busy = [0u32; 8];
    for slot in idle.iter_mut() {
        wait_frames(1);
        *slot = send(port2, &poll(0, 0)).span;
    }
    for slot in busy.iter_mut() {
        wait_frames(1);
        *slot = send(port2, &poll(0x01, 0xFF)).span;
    }
    stop_all(port2);
    let (_, idle_med, _) = spread(&mut idle);
    let (_, busy_med, _) = spread(&mut busy);
    push_timing_record(
        records,
        next,
        record(
            rid(RUMBLE_COST_RECORD + port2 as u16),
            idle_med,
            busy_med,
            8,
        ),
    );
}

/// What one question got.
#[derive(Copy, Clone)]
struct Answer {
    /// 1 yes, 2 no, 0 none.
    value: u32,
    /// The raw active-high button word when the answer was taken.
    buttons: u32,
    /// Frames from the question appearing to the answer.
    frame: u32,
    /// A button was down during the first second, when input is ignored.
    early: bool,
    /// No frame in the answer window had every button up.
    stuck: bool,
}

impl Answer {
    const NONE: Answer = Answer {
        value: 0,
        buttons: NONE,
        frame: NONE,
        early: false,
        stuck: false,
    };
}

/// Ask `question`. The pad is polled once a frame with the motors at zero. For
/// the first second the question only shows (a press during it is noted and
/// ignored), then every button must be up for a few frames in a row, and only
/// a press after that counts: CROSS yes, CIRCLE no. Four seconds for the
/// release and the press together; every wait is a frame count.
fn ask(font: &FontAtlas, port2: bool, question: &str) -> Answer {
    ask_with(
        font,
        "MOTOR",
        question,
        Some(port2),
        ASK_ANSWER_FRAMES,
        |bits| {
            if bits & button::CROSS != 0 {
                1
            } else if bits & button::CIRCLE != 0 {
                2
            } else {
                0
            }
        },
    )
}

/// The same question with the answer window and the meaning of the buttons
/// given. `port2` of `None` reads both ports' pads together.
fn ask_with(
    font: &FontAtlas,
    group: &str,
    question: &str,
    port2: Option<bool>,
    answer_frames: u32,
    classify: fn(u16) -> u32,
) -> Answer {
    ui::detail(font, group, question);
    let read = || -> u16 {
        match port2 {
            Some(second) => api_poll(second, Rumble::OFF).buttons.bits(),
            None => {
                api_poll(false, Rumble::OFF).buttons.bits()
                    | api_poll(true, Rumble::OFF).buttons.bits()
            }
        }
    };
    let mut answer = Answer::NONE;
    let mut frame = 0u32;
    for _ in 0..ASK_SHOW_FRAMES {
        wait_frames(1);
        frame += 1;
        if read() != 0 {
            answer.early = true;
        }
    }
    let mut released = 0u32;
    for _ in 0..answer_frames {
        wait_frames(1);
        frame += 1;
        let bits = read();
        if released < ASK_RELEASED_FRAMES {
            released = if bits == 0 { released + 1 } else { 0 };
            continue;
        }
        let value = classify(bits);
        if value != 0 {
            answer.value = value;
            answer.buttons = bits as u32;
            answer.frame = frame;
            return answer;
        }
    }
    answer.stuck = released < ASK_RELEASED_FRAMES;
    answer
}

/// Which pad is in the port: CROSS SCPH-1200, CIRCLE SCPH-110, SQUARE another
/// model, TRIANGLE none or a digital pad.
fn choose(font: &FontAtlas, group: &str, text: &str, answer_frames: u32) -> Answer {
    ask_with(font, group, text, None, answer_frames, |bits| {
        if bits & button::CROSS != 0 {
            1
        } else if bits & button::CIRCLE != 0 {
            2
        } else if bits & button::SQUARE != 0 {
            3
        } else if bits & button::TRIANGLE != 0 {
            4
        } else {
            0
        }
    })
}

fn prompt(font: &FontAtlas, port2: bool, what: &str, small: u8, large: u8) -> Answer {
    ui::detail(font, "MOTOR", what);
    hold(port2, small, large, 60);
    stop_all(port2);
    let mut line = ui::Line::new();
    line.s(what).s(" FELT? X YES O NO");
    ask(font, port2, line.as_str())
}

/// Pulses of 1, 2, 4, 8 and 15 frames, each followed by as many frames of
/// stillness.
fn pulses(port2: bool, small: u8, large: u8) {
    for length in [1u32, 2, 4, 8, 15] {
        hold(port2, small, large, length);
        stop_all(port2);
        wait_frames(length.min(8));
    }
}

fn operator(font: &FontAtlas, port2: bool, records: &mut Records, next: &mut usize) {
    // The SDK's own enable (config entry, mapping, exit, a frame apart each).
    // SAFETY: a token is a logic guard; nothing else drives SIO0 here.
    let mut port = unsafe { ControllerPort::steal() };
    let enabled = psx_pad::enable_rumble_on(&mut port, socket(port2));
    let mut answers = 0u32;
    let mut taken = [Answer::NONE; STIMULI];
    let mut asked = 0usize;
    let mut put = |answer: Answer| {
        answers |= answer.value << (2 * asked);
        taken[asked] = answer;
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
        record(
            rid(RUMBLE_OPERATOR_RECORD),
            answers,
            asked as u32,
            (port2 as u32 + 1) | ((enabled as u32) << 8),
        ),
    );
    push_answer_records(records, next, &taken);
}

/// The raw button words and arrival frames of the operator's answers, in the
/// six records `0x771` to `0x776`.
fn push_answer_records(records: &mut Records, next: &mut usize, taken: &[Answer; STIMULI]) {
    let mut stuck = 0u32;
    let mut early = 0u32;
    for (index, answer) in taken.iter().enumerate() {
        stuck |= (answer.stuck as u32) << index;
        early |= (answer.early as u32) << index;
    }
    let word = |index: usize, frames: bool| -> u32 {
        match taken.get(index) {
            Some(answer) if frames => answer.frame,
            Some(answer) => answer.buttons,
            None => NONE,
        }
    };
    for group in 0..3usize {
        let base = group * 3;
        // Stimuli 6 and 7 only fill two slots; the third carries the mask.
        let (buttons_tail, frames_tail) = if group == 2 {
            (stuck, early)
        } else {
            (word(base + 2, false), word(base + 2, true))
        };
        push_timing_record(
            records,
            next,
            record(
                rid(RUMBLE_ANSWER_BUTTONS_RECORD + group as u16),
                word(base, false),
                word(base + 1, false),
                buttons_tail,
            ),
        );
        push_timing_record(
            records,
            next,
            record(
                rid(RUMBLE_ANSWER_FRAMES_RECORD + group as u16),
                word(base, true),
                word(base + 1, true),
                frames_tail,
            ),
        );
    }
}

/// One pass over the pad in port 1 (and port 2): identity, config, the cost
/// of a poll, and when a config-capable pad is there the operator's questions.
/// Returns the port the operator part ran on.
fn pass(font: &FontAtlas, records: &mut Records, next: &mut usize) -> Option<bool> {
    let mut tested = None;
    for port2 in [false, true] {
        if objective(font, port2, records, next) && tested.is_none() {
            tested = Some(port2);
        }
        poll_cost(port2, records, next);
    }
    match tested {
        Some(port2) => {
            operator(font, port2, records, next);
        }
        None => {
            push_timing_record(
                records,
                next,
                record(rid(RUMBLE_OPERATOR_RECORD), NONE, 0, NONE),
            );
            push_answer_records(records, next, &[Answer::NONE; STIMULI]);
        }
    }
    tested
}

fn choice_record(id: u16, answer: &Answer) -> TimingRecord {
    record(id, answer.value, answer.buttons, answer.frame)
}

/// The whole step: the pad in the port is named by the operator and run
/// through the battery; then the operator may swap it for another, name that
/// one, and the battery runs again into the second set of records (`0x940`
/// and up). No answer to the second question means no second pass.
pub(crate) fn run(font: &FontAtlas, records: &mut Records, next: &mut usize) {
    crate::bounds::record_start(PAD_CHOICE_RECORD);
    set_pass(false);
    let first = choose(
        font,
        "WHICH PAD",
        "X 1200 O 110 SQ OTHER TRI NONE",
        MODEL_ANSWER_FRAMES,
    );
    push_timing_record(records, next, choice_record(PAD_CHOICE_RECORD, &first));
    let tested_first = pass(font, records, next);

    crate::bounds::record_start(PAD_CHOICE_RECORD + 1);
    let second = choose(
        font,
        "SWAP PAD",
        "THEN X 1200 O 110 SQ OTHER TRI NONE",
        SWAP_ANSWER_FRAMES,
    );
    push_timing_record(records, next, choice_record(PAD_CHOICE_RECORD + 1, &second));
    let mut tested_second = None;
    let second_ran = second.value != 0;
    if second_ran {
        set_pass(true);
        tested_second = pass(font, records, next);
        set_pass(false);
    }
    let port_code = |tested: Option<bool>| match tested {
        Some(false) => 1u32,
        Some(true) => 2,
        None => 0,
    };
    push_timing_record(
        records,
        next,
        record(
            PAD_PASSES_RECORD,
            second_ran as u32
                | ((tested_first.is_some() as u32) << 1)
                | ((tested_second.is_some() as u32) << 2),
            port_code(tested_first) | (port_code(tested_second) << 8),
            first.stuck as u32
                | ((first.early as u32) << 1)
                | ((second.stuck as u32) << 2)
                | ((second.early as u32) << 3),
        ),
    );
}
