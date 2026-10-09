//! The real driver run against a deterministic SIO0 controller model
//! ([`crate::mock_sio`]).
//!
//! Covers the wire behaviour a console depends on: 0x41 and 0x73 and 0xF3
//! frames decode the same buttons, no byte is clocked before the previous
//! byte's ACK, a packet that fails part way is rejected whole, a slipped
//! 0x5A byte never reaches the game as buttons, and a pad parked in
//! configuration mode is brought back.

extern crate std;

use crate::mock_sio::{self as mock, Fault, MockBus, Model};
use crate::{button, poll_on, AnalogRequirement, PadMode, PadReader, PadState, Port};
use std::vec;

fn start(model: Model) {
    mock::reset(model);
}

fn poll_with(model: Model) -> PadState {
    start(model);
    poll_on(&mut MockBus, Port::One)
}

fn fault_model(id: u8, fault: Fault, byte: usize, once: bool) -> Model {
    Model {
        id,
        fault,
        fault_byte: byte,
        fault_once: once,
        ..Model::default()
    }
}

#[test]
fn digital_analog_and_config_frames_decode_the_same_buttons() {
    for (id, sends) in [(0x41, 5), (0x73, 9), (0xF3, 9)] {
        let held = button::CROSS | button::START | button::LEFT;
        let pad = poll_with(Model {
            id,
            buttons: held,
            ..Model::default()
        });
        assert_eq!(pad.buttons.bits(), held, "id {id:02x}");
        mock::with(|m| {
            assert_eq!(
                (m.sends, m.attempts, m.early, m.tx_errors),
                (sends, 1, 0, 0)
            );
            assert!(!m.is_selected());
        });
    }
}

#[test]
fn digital_frames_report_centered_sticks_and_analog_frames_the_wire_sticks() {
    let digital = poll_with(Model {
        id: 0x41,
        ..Model::default()
    });
    assert_eq!(digital.mode, PadMode::Digital);
    assert!(!digital.mode.has_sticks());
    let analog = poll_with(Model::default());
    assert_eq!(analog.mode, PadMode::Analog);
    assert!(analog.is_analog());
}

#[test]
fn an_acknowledgement_before_or_with_the_reply_is_honoured() {
    for (rx_delay, ack_delay, ack_width) in [(12, 3, 3), (3, 3, 5)] {
        let pad = poll_with(Model {
            rx_delay,
            ack_delay,
            ack_width,
            ..Model::default()
        });
        assert_eq!(pad.mode, PadMode::Analog);
        assert_eq!(pad.buttons.bits(), 0);
        mock::with(|m| assert_eq!((m.attempts, m.sends, m.early, m.tx_errors), (1, 9, 0, 0)));
    }
}

#[test]
fn a_late_reply_after_an_abandoned_poll_is_reset_away() {
    let pad = poll_with(fault_model(0x73, Fault::LateRx, 3, true));
    assert_eq!(pad.mode, PadMode::Analog);
    assert_eq!(pad.buttons.bits(), 0);
    mock::with(|m| {
        assert_eq!((m.attempts, m.resets, m.tx_errors), (2, 1, 0));
        assert_eq!(m.pending_replies(), 0);
    });
}

#[test]
#[cfg_attr(
    miri,
    ignore = "every failed byte waits out a 32768-read spin budget; the missing-ACK test covers the same accesses"
)]
fn a_packet_that_fails_part_way_is_rejected_whole() {
    for fault in [Fault::Tx, Fault::Rx, Fault::Ack, Fault::AckHeld] {
        for byte in [2, 3, 4, 5, 6, 7] {
            let pad = poll_with(fault_model(0x73, fault, byte, false));
            assert_eq!(pad.mode, PadMode::Unknown, "{fault:?} at byte {byte}");
            assert_eq!(pad.buttons.bits(), 0);
            mock::with(|m| assert!(!m.is_selected()));
        }
        let pad = poll_with(fault_model(0x73, fault, 3, true));
        assert_eq!(pad.mode, PadMode::Analog, "{fault:?} recovers on retry");
        mock::with(|m| assert_eq!((m.attempts, m.early), (2, 0)));
    }
    for fault in [Fault::Tx, Fault::Rx] {
        for byte in [0, 1, 8] {
            let pad = poll_with(fault_model(0x73, fault, byte, false));
            assert_eq!(pad.mode, PadMode::Unknown, "{fault:?} at byte {byte}");
        }
    }
}

#[test]
fn a_stuck_acknowledge_on_select_recovers_on_the_next_attempt() {
    let pad = poll_with(fault_model(0x73, Fault::Acquire, 0, true));
    assert_eq!(pad.mode, PadMode::Analog);
    mock::with(|m| assert_eq!((m.attempts, m.sends), (2, 9)));
}

#[test]
fn an_empty_socket_is_told_at_once_and_cheaply() {
    let absent = poll_with(Model {
        id: 0xFF,
        ..Model::default()
    });
    assert_eq!(absent.mode, PadMode::Disconnected);
    // One attempt: the setup delay and the address byte's short wait, not
    // four attempts of the long one (about 12,000 reads).
    mock::with(|m| {
        assert_eq!((m.attempts, m.sends), (1, 1));
        assert!(m.now < 2_000, "{} register accesses", m.now);
    });
    // A reader that never saw a pad is told the same, once.
    start(Model {
        id: 0xFF,
        ..Model::default()
    });
    let mut reader = PadReader::port1();
    assert_eq!(reader.poll_on(&mut MockBus).mode, PadMode::Disconnected);
    mock::with(|m| assert_eq!(m.attempts, 1));
    // And it is found when the pad is plugged in.
    mock::with(|m| m.id = 0x73);
    let back = reader.poll_on(&mut MockBus);
    assert_eq!((back.mode, back.buttons.bits()), (PadMode::Analog, 0));
}

#[test]
fn a_pad_that_was_there_has_its_absence_confirmed_four_times() {
    // Four empty replies in a row: gone.
    start(Model {
        sequence: vec![
            (0x73, Fault::None, 0),
            (0xFF, Fault::None, 0),
            (0xFF, Fault::None, 0),
            (0xFF, Fault::None, 0),
            (0xFF, Fault::None, 0),
        ],
        ..Model::default()
    });
    let mut reader = PadReader::port1();
    assert_eq!(reader.poll_on(&mut MockBus).mode, PadMode::Analog);
    assert_eq!(reader.poll_on(&mut MockBus).mode, PadMode::Disconnected);
    mock::with(|m| assert_eq!(m.attempts, 5));
    // One of the four is a garbled packet: not proof of absence, so the reader
    // keeps what it last read cleanly.
    start(Model {
        sequence: vec![
            (0x73, Fault::None, 0),
            (0xFF, Fault::None, 0),
            (0x73, Fault::Rx, 3),
            (0xFF, Fault::None, 0),
            (0xFF, Fault::None, 0),
        ],
        ..Model::default()
    });
    let mut reader = PadReader::port1();
    let held = reader.poll_on(&mut MockBus);
    assert_eq!(reader.poll_on(&mut MockBus), held);
    mock::with(|m| assert_eq!(m.attempts, 5));
}

#[test]
fn a_slow_address_acknowledgement_inside_the_bios_limit_still_reads() {
    // The address byte's wait is shortened to the BIOS's own limit; a pad that
    // answers inside it is unaffected.
    let pad = poll_with(Model {
        slow_ack: Some((0, 400)),
        ..Model::default()
    });
    assert_eq!(pad.mode, PadMode::Analog);
}

#[test]
fn each_frame_length_follows_its_own_id_across_mode_switches() {
    start(Model::default());
    for id in [0x41, 0x73, 0x41, 0x73] {
        mock::with(|m| m.id = id);
        let pad = poll_on(&mut MockBus, Port::One);
        assert_eq!(
            (pad.mode, pad.buttons.bits()),
            (crate::mode_from_id_low(id), 0)
        );
    }
}

/// The SCPH-110 symptom: the first button byte is loaded after the ACK, a
/// host that clocks it earlier reads the ID's 0x5A again, and 0x5A decoded as
/// button bytes reads as SELECT + R3 + LEFT + RIGHT.
fn late_ack_pad() -> Model {
    Model {
        slow_ack: Some((2, 400)),
        ..Model::default()
    }
}

#[test]
fn a_late_acknowledge_never_slips_a_byte_into_the_buttons() {
    start(late_ack_pad());
    // Many polls natively; a few under Miri, where each is interpreted.
    let polls = if cfg!(miri) { 4 } else { 100 };
    for poll in 0..polls {
        let pad = poll_on(&mut MockBus, Port::One);
        assert_eq!(pad.mode, PadMode::Analog, "poll {poll}");
        assert_eq!(pad.buttons.bits(), 0, "phantom buttons on poll {poll}");
    }
    mock::with(|m| assert_eq!((m.early, m.tx_errors), (0, 0)));
}

#[test]
fn the_late_acknowledge_model_does_slip_a_driver_that_does_not_wait() {
    // Control: the legacy no-wait pacing against the same pad reads 0x5A as
    // the first button byte. Without this the test above could pass on a
    // model that never slips.
    start(late_ack_pad());
    let raw = crate::poll_raw_on(&mut MockBus, Port::One, crate::Pacing::NoAckWait);
    assert_eq!(raw.buttons_low, 0x5A);
    let phantom = button::SELECT | button::R3 | button::LEFT | button::RIGHT;
    assert_eq!(raw.to_state().buttons.bits() & phantom, phantom);
}

#[test]
fn a_reader_holds_the_last_clean_state_through_a_failed_poll() {
    let held = button::SELECT;
    start(Model {
        buttons: held,
        ..Model::default()
    });
    let mut reader = PadReader::port1();
    assert_eq!(reader.poll_on(&mut MockBus).buttons.bits(), held);
    mock::with(|m| {
        m.fault = Fault::Ack;
        m.fault_byte = 3;
    });
    let during = reader.poll_on(&mut MockBus);
    assert_eq!(during.buttons.bits(), held, "held Select must not release");
    mock::with(|m| m.fault = Fault::None);
    assert_eq!(reader.poll_on(&mut MockBus).buttons.bits(), held);
    mock::with(|m| m.id = 0xFF);
    assert_eq!(
        reader.poll_on(&mut MockBus).buttons.bits(),
        0,
        "an unplugged pad releases"
    );
}

/// A DualShock that starts in digital mode, as after a reset.
fn digital_dualshock() -> Model {
    Model {
        id: 0x41,
        ..Model::default()
    }
}

/// `require_analog_on` with a short gap between the configuration
/// commands. The public call spaces them about a video frame apart, which
/// costs hundreds of thousands of modelled register reads per command; the
/// sequence and the retry rule are the same.
fn require_quickly() -> AnalogRequirement {
    AnalogRequirement::from_mode(crate::request_analog(&mut MockBus, Port::One, 16).mode)
}

#[test]
fn requiring_analog_switches_the_pad_and_locks_it() {
    start(digital_dualshock());
    assert_eq!(require_quickly(), AnalogRequirement::Analog);
    mock::with(|m| {
        assert_eq!(m.id, 0x73);
        assert!(m.locked, "the analog button must be locked out");
        assert!(!m.in_config());
        assert!(!m.is_selected());
    });
    assert_eq!(poll_on(&mut MockBus, Port::One).mode, PadMode::Analog);
}

#[test]
fn a_pad_parked_in_config_mode_is_sent_the_exit_again() {
    // The SCPH-110 failure: the exit command is ignored and the pad keeps
    // answering 0xF3 with buttons but never analog.
    start(Model {
        ignore_exits: 2,
        ..digital_dualshock()
    });
    assert_eq!(require_quickly(), AnalogRequirement::Analog);
    mock::with(|m| {
        assert_eq!((m.id, m.ignore_exits), (0x73, 0));
        assert!(m.locked);
    });
}

#[test]
fn a_pad_that_never_leaves_config_mode_is_reported_digital_only() {
    start(Model {
        ignore_exits: 100,
        ..digital_dualshock()
    });
    assert_eq!(require_quickly(), AnalogRequirement::DigitalOnly);
    // Three retries after the first exit, then it gives up.
    mock::with(|m| {
        assert!(m.in_config());
        assert_eq!(m.ignore_exits, 100 - 4);
    });
}

#[test]
fn a_digital_only_pad_and_an_empty_port_are_told_apart() {
    start(Model {
        dualshock: false,
        ..digital_dualshock()
    });
    assert_eq!(require_quickly(), AnalogRequirement::DigitalOnly);
    start(Model {
        id: 0xFF,
        ..Model::default()
    });
    assert_eq!(require_quickly(), AnalogRequirement::Absent);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "the frame-length gaps are millions of interpreted reads; the tests above cover the same accesses"
)]
fn the_public_requests_use_the_frame_spaced_sequence_on_either_port() {
    start(digital_dualshock());
    assert_eq!(
        crate::require_analog_on(&mut MockBus, Port::One),
        AnalogRequirement::Analog
    );
    assert_eq!(
        crate::require_analog_on(&mut MockBus, Port::Two),
        AnalogRequirement::Analog
    );
    mock::with(|m| assert!(m.locked && m.id == 0x73));
}

#[test]
fn a_missing_or_stuck_acknowledge_rejects_the_packet_and_recovers() {
    // The short-budget failures, cheap enough for Miri.
    for fault in [Fault::Ack, Fault::AckHeld] {
        let pad = poll_with(fault_model(0x73, fault, 3, false));
        assert_eq!(pad.mode, PadMode::Unknown, "{fault:?}");
        assert_eq!(pad.buttons.bits(), 0);
        mock::with(|m| assert!(!m.is_selected()));
        let pad = poll_with(fault_model(0x73, fault, 3, true));
        assert_eq!(pad.mode, PadMode::Analog, "{fault:?} recovers on retry");
    }
}

/// The public entry points take the real token, not only the model.
#[allow(dead_code)]
fn the_token_is_accepted(port: &mut psx_io::periph::ControllerPort) {
    let _ = poll_on(port, Port::One);
    let _ = PadReader::port2().poll_on(port);
    let _ = crate::require_analog_on(port, Port::Two);
}

// ------------------------------------------------------------------ rumble

use crate::{enable_rumble_on, poll_rumble_on, Rumble};

fn analog_dualshock() -> Model {
    Model {
        id: 0x73,
        analog_requested: true,
        ..Model::default()
    }
}

#[test]
fn enabling_rumble_enters_config_maps_the_motors_and_leaves() {
    start(analog_dualshock());
    assert!(enable_rumble_on(&mut MockBus, Port::One));
    mock::with(|m| {
        // Enter, map, exit, then the poll that reads what the pad settled on.
        assert_eq!(m.cmd_log, [0x43, 0x4D, 0x43, 0x42]);
        // Small motor on the first motor byte, large on the second, the rest
        // unused.
        assert_eq!(m.map_params, [[0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF]]);
        assert!(m.rumble_mapped);
        assert_eq!(m.id, 0x73, "analog mode is left as it was");
        assert!(!m.is_selected());
    });
}

#[test]
fn a_poll_sends_the_motor_bytes_to_a_mapped_dualshock() {
    start(analog_dualshock());
    assert!(enable_rumble_on(&mut MockBus, Port::One));
    mock::with(|m| m.motor_tx.clear());
    let pad = poll_rumble_on(&mut MockBus, Port::One, Rumble::new(true, 200));
    assert_eq!(pad.mode, PadMode::Analog);
    mock::with(|m| {
        assert_eq!(m.motor_tx, [(0x73, 0x01, 200)]);
        assert_eq!(m.motors, (true, 200));
    });
    poll_rumble_on(&mut MockBus, Port::One, Rumble::new(false, 0x40));
    mock::with(|m| assert_eq!(m.motors, (false, 0x40)));
    // A plain poll asks for none.
    poll_on(&mut MockBus, Port::One);
    mock::with(|m| assert_eq!(m.motors, (false, 0)));
}

#[test]
fn only_a_dualshock_in_analog_mode_is_sent_motor_bytes() {
    // A digital pad, and a pad parked in configuration mode, hear zeros.
    for id in [0x41, 0xF3] {
        start(Model {
            id,
            dualshock: false,
            ..Model::default()
        });
        poll_rumble_on(&mut MockBus, Port::One, Rumble::new(true, 255));
        mock::with(|m| assert_eq!(m.motor_tx, [(id, 0, 0)], "id {id:02x}"));
    }
}

#[test]
fn a_reader_asks_for_its_rumble_on_every_poll_and_stops_the_motors_on_request() {
    start(analog_dualshock());
    let mut reader = PadReader::port1();
    assert!(reader.enable_rumble_on(&mut MockBus));
    reader.set_rumble(Rumble::new(true, 90));
    reader.poll_on(&mut MockBus);
    reader.poll_on(&mut MockBus);
    mock::with(|m| {
        assert_eq!(m.motors, (true, 90));
        let rumbles: vec::Vec<_> = m.motor_tx.iter().rev().take(2).collect();
        assert!(rumbles.iter().all(|&&(_, s, l)| (s, l) == (1, 90)));
    });
    let pad = reader.stop_motors_on(&mut MockBus);
    assert!(pad.is_analog());
    assert!(reader.rumble().is_off());
    mock::with(|m| assert_eq!(m.motors, (false, 0)));
}

#[test]
fn a_pad_that_refuses_configuration_mode_is_not_mapped() {
    // It reports analog and ignores every configuration command: no mapping
    // happens, and the driver says so.
    start(Model {
        id: 0x73,
        dualshock: false,
        ..Model::default()
    });
    assert!(!enable_rumble_on(&mut MockBus, Port::One));
    mock::with(|m| assert!(!m.rumble_mapped));
    // Polls still read it, motor bytes and all.
    let pad = poll_rumble_on(&mut MockBus, Port::One, Rumble::new(true, 255));
    assert_eq!(pad.mode, PadMode::Analog);
    mock::with(|m| assert_eq!(m.motors, (false, 0), "an unmapped pad does not spin"));
}

#[test]
fn a_pad_that_drops_out_part_way_through_the_mapping_is_not_mapped() {
    // Attempt 2 is the mapping packet: the pad is gone for it.
    start(Model {
        sequence: vec![
            (0x73, Fault::None, 0),
            (0xFF, Fault::None, 0),
            (0x73, Fault::None, 0),
            (0x73, Fault::None, 0),
        ],
        ..Model::default()
    });
    assert!(!enable_rumble_on(&mut MockBus, Port::One));
    mock::with(|m| assert!(!m.rumble_mapped));
    // The same, with the reply to the mapping packet lost.
    start(Model {
        sequence: vec![
            (0x73, Fault::None, 0),
            (0x73, Fault::Rx, 1),
            (0x73, Fault::None, 0),
            (0x73, Fault::None, 0),
        ],
        ..Model::default()
    });
    assert!(!enable_rumble_on(&mut MockBus, Port::One));
}

#[test]
fn a_pad_that_ignores_the_exit_is_sent_it_again_and_still_ends_mapped() {
    start(Model {
        ignore_exits: 1,
        ..analog_dualshock()
    });
    assert!(enable_rumble_on(&mut MockBus, Port::One));
    mock::with(|m| {
        assert!(m.rumble_mapped);
        assert_eq!(m.id, 0x73);
    });
}

#[test]
fn rumble_is_off_by_default_and_the_plain_poll_is_unchanged() {
    assert!(Rumble::default().is_off());
    assert_eq!(Rumble::OFF, Rumble::new(false, 0));
    assert!(!Rumble::new(true, 0).is_off());
    assert!(!Rumble::new(false, 1).is_off());
    start(analog_dualshock());
    poll_on(&mut MockBus, Port::One);
    mock::with(|m| assert_eq!(m.motor_tx, [(0x73, 0, 0)]));
}
