# The interrupt-driven pad engine

`psx-pad`'s synchronous driver (`poll_on`) waits. The engine in
`psx_pad::engine` and `psx_pad::console` does not: a VBlank starts a
transaction, and every byte after that is an interrupt. The CPU runs the game
in between. It is opt-in per game (`psx-pad` feature `irq-engine`); the
synchronous driver is unchanged and is the fallback.

## What it replaces

`poll_on` after 011cea59 ("pace controller packets on ACK and reject
incomplete replies") spends, per frame, in emulated CPU cycles
(`sdk/examples/pollbench`, headless, 200 polls, an analog pad on port 1 and
nothing on port 2):

| SDK | port 1 | empty port 2 | both |
|---|---|---|---|
| before 011cea59 | 7,572 | 7,376 | 14,947 |
| 011cea59 to main | 17,353 | 119,277 | 136,628 |

Against a 60 Hz frame of 564,480 cycles that is 3.1% for the one connected pad,
21% for the empty port 2 and 24% for both. The jump is the empty socket: a poll that is not answered used to
return at once; it now retries four times, each with the 1,024-read setup delay
and a 2,048-read wait for an `/ACK` that never comes. A connected pad pays for
the `/ACK` wait after every byte.

Every cycle of that is waiting, not work. A faster loop would still be a loop.

## The shape

```text
VBlank    select port 1, arm the timer for the setup time
timer     transmit the address byte, arm the timer for the /ACK limit
IRQ7      read the reply, clear the latch, transmit the next byte, re-arm
...       (the final byte has no /ACK: the timer ends it)
done      deselect, publish, start port 2 the same way
```

Three events drive a state machine ([`engine.rs`](../crates/psx-pad/src/engine.rs)):
`on_vblank`, `on_ack` (IRQ7, the device's `/ACK` pulse) and `on_deadline` (root
counter 0, one shot). Nothing in it waits except one bounded read loop in
`on_ack` for the few microseconds a pulse lasts.

* **Setup delay.** The SCPH-1200 gets no answer without a delay between `/CS`
  and the first byte. The engine selects the port and arms the timer for 7,000
  cycles (the synchronous driver's 1,024 reads are about 6,900); the first byte
  goes from the timer interrupt. The CPU spends those cycles on the game.
* **An empty socket** answers `0xFF` and never pulses `/ACK`. The address
  byte's `/ACK` limit expires, the reply is `0xFF`, and the port is `Absent`.
  That is one byte on the wire and three interrupts, about 1,000 CPU cycles
  for the whole round of two empty ports, and it repeats every frame, so a pad
  plugged in later is found by the next VBlank. There is nothing to remember
  and nothing to back off.
* **The last byte** is not acknowledged, so a timer reads it at the byte's
  length plus margin.
* **A byte that never comes** is the timer too. The transaction is dropped, the
  UART reset (the same reason as the synchronous driver: a late reply must not
  be taken for the next packet's), and the port reports `Faulted` with its last
  clean state kept, so a held button does not release and press again.
* **Lost interrupts.** A transaction still running at the second VBlank after
  it started is abandoned and counted (`Stats::stalls`).

## What the game sees

[`Snapshot`]: a sequence number and, per port, the last clean `PadState`, a
`Health` (`Unseen`, `Present`, `Absent`, `Faulted`), and counters.
`console::snapshot()` copies two buffers with a sequence check, so it takes no
lock and never masks interrupts. `Snapshot::pad(port)` is what
`PadReader::poll_on` returned. Polling cost on the game's thread: a copy.

The engine polls every VBlank by default; `Config::kick_every` divides that.
`Config::PORT1_ONLY` skips port 2. `Config::pacing` selects how a byte is
declared done (below).

## Pacing

`BytePacing::Ack` (default) advances on IRQ7 and uses the timer only as the
limit. It is the BIOS's pacing and adapts to a slow pad (the SCPH-110 slips a
packet if the next byte goes before its `/ACK`). To get a second IRQ7 the
latched `STAT` bit 9 has to be cleared, which is one write to `CTRL` between
bytes.

That write is what the silicon history warns about. The first ACK-wait driver
(2026-06-20) armed the interrupt and rewrote `CTRL` between bytes and the
third-party pad's bytes came back corrupted; the 2026-06-22 fix was a setup
delay, not ACK pacing. Today's synchronous driver deliberately writes nothing
between bytes. Whether the engine's single `CTRL` write per byte is safe on
each pad model is a question only a console answers, so the engine has a second
mode that avoids it:

`BytePacing::Timed` never arms the `/ACK` interrupt and writes nothing between
bytes; the timer alone spaces them by `timed_gap_cycles` (4,500). It is the
pre-ACK driver's wire pattern with the spin replaced by a timer. It needs the
gap to exceed the slowest pad's `/ACK` delay, which the console measurement
below fixes.

Both are covered by the host tests, including a pad that slips if clocked
early.

## Analog mode

`console::request_analog(port)` queues the three configuration packets
(enter, request-and-lock, exit) one per VBlank, the spacing Sony's libpad uses
and the synchronous `require_analog_on` blocks about four frames to produce. The
poll after them verifies the mode; a pad still answering `0xF3` is sent the exit
again up to three times. `console::analog_outcome(port)` reports
`Analog`, `DigitalOnly` or `Absent` once it is done. A program that wants the
answer before it starts (a launcher) keeps calling `require_analog_on` before
`install`, as it does today.

## Memory cards

A card transaction needs the port for its whole length, so the engine hands the
port over: `console::lease()` returns a `Lease` that derefs to the
`ControllerPort` token `psx_mc::HardwareCard::on_port` takes. While a lease is
out the engine touches nothing and the VBlanks that pass are counted
(`leased_skips`), so the snapshot stands still. A lease waits for a packet in
flight (under two milliseconds on the wire) and abandons it only after three
VBlanks. Keep a lease to one card transaction; `CardJob` already advances one per
step.

This is arbitration by exclusion, not interleaving: a sector transfer is about
as long as a frame, and a pad packet cannot be inserted into the middle of a
card packet on the same wires.

## Installing

```rust
// after the other exception wrappers (psx-cdstream::install) are in place
let mut port = peripherals.controller_port;
let _ = psx_pad::require_analog_on(&mut port, Port::One); // optional, blocking, as today
psx_pad::console::install(port, psx_pad::engine::Config::PORT1_ONLY)
    .expect("a wrapper in the vector that cannot be chained");
// per frame
let pad = psx_pad::console::pad(Port::One);
```

The wrapper sits in the exception vector like psx-cdstream's: it saves the
interrupted context, switches to its own 768-byte stack, runs the engine, restores
the context and jumps to the handler that was in the vector before it
(psx-rt's, or a cdstream wrapper that leads there). psx-cdstream's wrapper
chains the same way, so the two install in either order (before this change a
cdstream installed after the engine replaced it and the pad went dead:
`console::is_installed()` tells). It owns
root counter 0 and takes the `ControllerPort` token, so nothing can poll the
port synchronously while it runs, which `Lease` is for.

## What it costs

`examples/pad-engine-check` runs the three drivers on the same frames in the
headless emulator and counts the iterations of an empty game loop that fit
between VBlanks (the CPU the game keeps; emulated cycles, not wall time). The
self-check at its end prints `PASS`.

| Driver | Game loop kept | Cost per 60 Hz frame |
|---|---|---|
| nothing polls | 100% | 0 |
| synchronous, both ports (main) | 76.1% | 136,000 cycles |
| engine, VBlank entry only (no ports) | 99.9% | about 650 cycles |
| engine, port 1 | 98.5% | about 8,500 cycles |
| engine, both ports (port 2 empty) | 98.2% | about 10,000 cycles |
| engine, both ports, fixed pacing | 98.0% | about 10,500 cycles |
| engine, both ports, every other VBlank | 99.0% | about 5,500 cycles |

An empty port 2 adds under a quarter of a percent (inside the noise of where the
loop falls against the VBlank). The engine's cost is the events: about ten for a
connected analog pad, two for an empty port, and about 1,000 cycles each in the
emulator, of which 129 instructions are the wrapper and psx-rt's handler and the
rest is the engine and the loads that miss without a data cache. For a single
pad that is about what the old no-ACK synchronous driver spent (7,572 cycles),
not less, so a game that polls one pad at a sim rate
of 30 Hz sets `kick_every` to 2 and gains about 40% over today's synchronous
poll, not more. The large gain is the empty socket and the setup delay, which
are now free. NitroXide's attract demo: 38.18 fps on the 65a9b131 pin, 44.48
fps with the engine, 44.96 on the old pin before 011cea59 (flips per emulated
second after route tick 600).

Where the remaining per-event cost could go: a handler for the byte events in
assembly with four registers saved instead of twenty-two, and a direct return
instead of a hop through psx-rt's handler for an interrupt that is only ours.
Neither is done; both would be checked by the same self-check.

## What the host tests prove, and what they do not

[`engine_tests.rs`](../crates/psx-pad/src/engine_tests.rs) runs the engine
against a controller model with real timing: 1,088-cycle bytes, `/ACK` delay
and width per pad, interrupt entry cost, register access cost. It shows: the
same buttons from 0x41, 0x73 and 0xF3 frames; a pad ACKing from 1 to 13,000
cycles after the byte, with pulses from 20 to 2,000 cycles wide, read cleanly;
an SCPH-110 style pad that slips when clocked early never slips under `Ack`;
an empty socket costing under 1,200 CPU cycles a frame for both ports;
a lost interrupt recovered; hot-plug in both directions; the analog request
including a pad that ignores exits; the lease.

They are a model. What a console must measure is in the next section.

## Console measurements (hardware-tests v2.0)

The suite records numbers to the QR report; none of these is a pass/fail gate on a
separate disc.

1. **ACK latency and width per pad model**: the cycles from a byte's start to
   the `/ACK` assertion, and the assertion's width, for each byte of a 0x42 poll,
   by sampling `STAT` bit 7 against root counter 2 (system clock / 8) with
   interrupts off. Pads: the official SCPH-1200, a third-party pad, a
   DualShock (SCPH-110) in digital and in analog mode. Sets `ack_cycles`,
   `timed_gap_cycles` and whether `wait_ack_release`'s 64 reads are enough.
2. **Select to first byte**: the setup time the official pad needs, as a timer
   sweep (2,000 to 8,000 cycles in steps) on the engine's own path: select, timer
   interrupt, first byte, count clean 0x41/0x73 replies in 200. Confirms
   `setup_cycles`.
3. **The engine's byte pacing per pad**: 600 polls under `Ack` and under `Timed`,
   per pad, counting clean frames, faults and wrong IDs. This answers the
   `CTRL`-write question above.
4. **Empty port 2 and hot-plug**: the engine running with port 2 empty for 600
   frames (cycles in the handler per frame, from root counter 2); then the pad
   plugged and unplugged by hand with a prompt on screen, recording the frames
   until `Present` and until `Absent`.
5. **Pad and card together**: 600 frames of a polling engine with a card sector
   read and a write every 10 frames through `lease()`: pad faults, skipped
   frames (`leased_skips`), card checksum errors.
6. **Handler cost**: worst handler duration and stack use
   (`handler_stack_unused_bytes`) under load.

## Limits and open questions

* The engine takes root counter 0. Nothing else in the SDK uses it (the
  prepare-ahead select on `perf/pad-async` used it as a setup clock, which this
  supersedes: the timer interrupt is the setup wait, so there is nothing to
  prepare).
* A game with its own exception handler in front of psx-rt's must chain to the
  engine's wrapper for IRQ7 and counter 0 as well as VBlank, or install it
  before the engine (the engine chains to what it finds).
* Port 2 hot-plug of a DualShock starts in digital mode; the game calls
  `request_analog` on the `Absent`-to-`Present` edge, as the engine runner does.
* The synchronous driver keeps its cost. Its `PadReader::prepare_on` branch
  (`perf/pad-async`) is not needed by a game on the engine.
