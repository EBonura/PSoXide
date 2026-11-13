# Pad driver

There is one controller driver in the SDK, the synchronous one in `psx-pad`
(`poll_on`, `PadReader`, `enable_analog_on`, `require_analog_on`, the rumble
calls). `psx-engine`'s `App` polls through it, and so does every game. An
interrupt-driven engine existed up to SDK `0ed8d7592` and was deleted: it cost
about 10 KB of RAM per game for under 1% of frame time, and the one game that
used it (NitroXide) lost 0.65% on the synchronous driver and gained 12.6 KB.
`git show 0ed8d7592:sdk/docs/PAD-IRQ-ENGINE.md` has its design.

Related pages: [PAD-ANALOG](PAD-ANALOG.md), [PAD-RUMBLE](PAD-RUMBLE.md),
[PAD-NEGCON](PAD-NEGCON.md).

## What a poll does

A poll selects the port, waits a setup delay, then exchanges the packet one
byte at a time, waiting for the device's `/ACK` after every byte but the last.
The setup delay exists because the official SCPH-1200 gets no answer without a
gap between `/CS` and the first byte (`DEFAULT_SETUP_SPINS`, 1,024 status
reads, about 6,900 cycles). The driver writes nothing to the control register
between bytes: the first ACK-wait driver (2026-06-20) rewrote it between bytes
and a third-party pad's bytes came back corrupted. A pad slower to acknowledge
than the wait, such as the SCPH-110 when clocked early, slips a packet; a
packet that fails part way reads `PadMode::Unknown`, and `PadReader` keeps the
last clean state instead.

An empty socket answers `0xFF` and never pulses `/ACK`. The poll retries, each
try paying the setup delay and the `/ACK` wait, so an empty port costs about as
much as a connected one.

## What it costs

`sdk/examples/pollbench` counts the cycles one poll takes, headless. On the
PSoXide emulator at SDK `0ed8d7592` (an analog pad on port 1, port 2 empty,
200 polls per phase):

| Phase | Cycles per poll |
|---|---|
| port 1 | 23,076 |
| empty port 2 | 20,203 |
| both ports | 43,277 |

A 60 Hz frame is 564,480 cycles, so polling both ports is about 7.7% of it.
A game that needs only one pad polls port 1 and nothing else (`Ctx` polls port 2
only when the scene calls `refresh_second_pad`). Hardware-tests steps
`SIO0 SELECT DELAY`, `SIO0 PAD ACK TIMING` and `SIO0 LATCH AND ARM` measure the
timing on a console.

## Analog mode and hot-plug

`require_analog_on` and `enable_analog_on` send the three configuration
packets one video frame apart, as Sony's libpad does, and block for about four
frames. A DualShock plugged in later starts in digital mode: `psx-engine`
negotiates again on the connection edge of port 1, and a game that reads port 2
calls `Ctx::enable_analog(Port::Two)` when that pad appears.

## Memory cards

A card transaction needs the whole port, and a poll needs it too. The
`ControllerPort` token from `Peripherals::take()` has one owner at a time and
both drivers borrow it for the length of one transaction, so a card and a poll
never overlap. A card transaction is about as long as a frame; a pad packet
cannot be inserted into the middle of it.
