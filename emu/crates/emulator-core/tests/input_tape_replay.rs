//! End-to-end regression for the shared `PXITAPE1` controller-input tape.
//!
//! This needs no retail disc or BIOS, so it runs in
//! CI. It records a route to the on-disk tape format, reads it back, replays
//! each frame into a real `Bus` via the same `apply_to_bus` the editor and
//! the headless `launch --input-tape` path use, then polls the controller
//! through SIO0 MMIO exactly as a game's pad handler does, asserting the
//! polled wire bytes carry the recorded buttons.

use emulator_core::{
    button,
    input_tape::{read_tape, write_tape, PadSample},
    sio::Sio0,
    Bus,
};

/// JOY_CTRL select bit (`1 << 1`): asserts /DTR so port 1 is addressed.
const JOYN_OUTPUT: u16 = 0x0002;

/// Exchange one byte over SIO0 and return the controller's reply. Ticks
/// generously so the byte transfer and its ACK have completed before the
/// MMIO read pops the RX FIFO.
fn exchange(bus: &mut Bus, tx: u8) -> u8 {
    bus.write8(Sio0::BASE, tx);
    bus.tick(4096);
    bus.read8(Sio0::BASE)
}

/// Run a full digital-pad poll on port 1 and return the 16-bit button word
/// as the controller put it on the wire (active-low: a pressed bit reads 0).
fn poll_pad_wire(bus: &mut Bus) -> u16 {
    bus.write16(Sio0::BASE + 0x0A, JOYN_OUTPUT); // select port 1
    assert_eq!(exchange(bus, 0x01), 0xFF, "select-byte dummy reply");
    assert_eq!(exchange(bus, 0x42), 0x41, "digital pad id low");
    assert_eq!(exchange(bus, 0x00), 0x5A, "digital pad id high");
    let lo = exchange(bus, 0x00);
    let hi = exchange(bus, 0x00);
    bus.write16(Sio0::BASE + 0x0A, 0x0000); // deselect: resets the pad fsm
    u16::from_le_bytes([lo, hi])
}

#[test]
fn recorded_tape_replays_to_the_controller_a_game_polls() {
    // A short route that exercises the low button byte, the high button
    // byte, both together, a multi-bit press across both bytes, and release.
    let route = vec![
        PadSample::from_buttons(0),
        PadSample::from_buttons(button::START), // low byte
        PadSample::from_buttons(button::CROSS), // high byte
        PadSample::from_buttons(button::START | button::CROSS),
        PadSample::from_buttons(button::DOWN | button::TRIANGLE | button::SQUARE),
        PadSample::from_buttons(0), // release
    ];

    // Record to the binary PXITAPE1 file, then read it back.
    let path = std::env::temp_dir().join(format!("psoxide-replay-{}.pxtape", std::process::id()));
    write_tape(&path, &route).expect("write tape");
    let loaded = read_tape(&path).expect("read tape");
    let _ = std::fs::remove_file(&path);
    assert_eq!(loaded, route, "tape survived the binary round-trip");

    // Replay each recorded frame into a real bus and confirm a controller
    // poll returns exactly the recorded buttons.
    let mut bus = Bus::new_without_bios();
    bus.attach_digital_pad_port1();
    for (i, sample) in loaded.iter().enumerate() {
        sample.apply_to_bus(&mut bus);
        let wire = poll_pad_wire(&mut bus);
        assert_eq!(
            wire, !sample.buttons,
            "frame {i}: polled wire {wire:#06x} is not the active-low form of {:#06x}",
            sample.buttons
        );
    }
}
