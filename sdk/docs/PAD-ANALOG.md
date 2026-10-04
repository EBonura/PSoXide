# Requiring analog mode

A program that needs the sticks, or a disc that wants every program to hold
the pad in one known mode, should ask for analog mode once at boot, lock it,
and read the pad through a reader that never hands over a garbled packet. This
page is what a game changes to do that and what the driver does underneath.

## Why the old poll was not enough

On a console with an SCPH-110 (PS one DualShock) pressing the Analog button
produced phantom SELECT presses. The pad is slow to acknowledge the byte
before its first button byte. A host that clocks that byte without waiting
for `/ACK` reads the ID's `0x5A` again, the packet slips one byte, and `0x5A`
decoded as button bytes reads as SELECT, R3, LEFT and RIGHT together.

`poll_port1` and `poll_port2` now pace every non-final byte on the pad's
`/ACK`, take the packet length from the ID the pad actually reported (a 0x41
digital frame is five bytes, 0x73 and 0xF3 are nine), and reject a packet that
fails part way instead of decoding what arrived. A rejected poll resets the
deselected UART so a late byte cannot leak into the next poll, and is retried
a few times before it is reported as `PadMode::Unknown` with every button
released.

## At boot

```rust
use psx_pad::{require_analog_port1, AnalogRequirement};

match require_analog_port1() {
    AnalogRequirement::Analog => {}      // locked: the Analog button is dead
    AnalogRequirement::DigitalOnly => {} // an original pad, or one that refused
    AnalogRequirement::Absent => {}      // nothing answered yet
}
```

`require_analog_port1` enters configuration mode, requests analog, locks it,
exits, and spaces the three commands about one video frame apart, the way
Sony's libpad does. A pad still answering ID `0xF3` afterwards (the SCPH-110
failure: the exit was ignored, so it reports buttons but never analog and
ignores the lock) is sent the exit again, up to three more times. The call
blocks for a few frames, so make it at boot, not from the frame loop.

A program started by a launcher inherits whatever mode the previous program
left, so call it at boot even when the launcher already did. If the answer is
not `Analog`, call it again every so often (a pad plugged in later starts in
digital mode); a disc that requires analog shows a notice meanwhile.
`enable_analog_port1` is the shorter-spaced request for callers that retry from
a frame loop; it does not report what the pad settled on.

## In the frame loop

Read the pad through a `PadReader` instead of calling `poll_port1` directly.

```rust
use psx_pad::PadReader;

let mut pad = PadReader::port1();
loop {
    let state = pad.poll();
    // state.buttons, state.sticks, state.mode
}
```

A failed poll reports `Unknown` with every button released. Taken at face
value, a held button reads as released for that frame and as a fresh press on
the next, so a held Select or Cross fires twice. The reader returns the last
clean state instead, and accepts a clean report of an empty port as it is
(unplugging a pad does release its buttons). `PadReader::accept` applies the
same rule to a poll the caller made another way, and `PadReader::last` reads
the held state without polling.

The reader polls once per call; it adds no timing of its own.

## What does not get the fix

`poll_port1_diagnostics(setup, interbyte)` and `poll_port1_raw(Pacing::NoAckWait)`
are the legacy no-wait transport, kept so a hardware test can reproduce the
slip next to the fixed poll. A game that samples its pad through either keeps
the bug whatever SDK it is pinned to. Use `poll_port1` or `PadReader`.

A game built on `psx-engine` gets the ACK-paced transport from its SDK pin
because the engine polls through `poll_port1`; the engine's own boot request
is `enable_analog_port1`, and it keeps the latest poll without the last-clean
rule.

## Tests

`cargo test -p psx-pad` runs the real driver against a controller model
(`src/mock_sio.rs`), no hardware access: 0x41, 0x73 and 0xF3 frames, no byte
clocked before the previous ACK, whole-packet rejection at every byte index,
the late-ACK slip (with a no-wait control that proves the model slips), the
last-clean reader, and `require_analog_port1` against a pad that ignores the
exit command. `make miri` runs the same tests under Miri.

The model's delays are abstract ticks. Nothing in it is calibrated to a
physical SCPH-110, and the console route that originally showed the symptom
still has to be repeated on the console.
