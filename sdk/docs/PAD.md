# Pad driver

There is one controller driver in the SDK, the synchronous one in `psx-pad`.
Games read their pads through `psx_pad::Controllers` (one `poll(&mut port, Port)`
call per port per loop; the first answer of a pad gets the analog handshake, an
empty port is probed once in 60 polls, a garbled packet returns the last clean
state, `set_rumble` maps the motors). `psx-engine`'s `App` and
`Ctx::refresh_second_pad` poll through it. The older entry points (`poll_on`,
`PadReader`, `enable_analog_on`, `require_analog_on`, the rumble calls) are what
it is built from and stay for the games that have not moved yet. An
interrupt-driven engine existed up to SDK `0ed8d7592` and was deleted: it cost
about 10 KB of RAM per game for under 1% of frame time, and the one game that
used it (NitroXide) lost 0.65% on the synchronous driver and gained 12.6 KB.
`git show 0ed8d7592:sdk/docs/PAD-IRQ-ENGINE.md` has its design.

Related pages: [PAD-ANALOG](PAD-ANALOG.md), [PAD-RUMBLE](PAD-RUMBLE.md),
[PAD-NEGCON](PAD-NEGCON.md).

## What a poll does

A poll selects the port, waits a setup time, then exchanges the packet one
byte at a time, waiting for the device's `/ACK` after every byte but the last.
The setup time exists because the official SCPH-1200 gets no answer without a
gap between `/CS` and the first byte. The driver writes nothing to the control
register between bytes: the first ACK-wait driver (2026-06-20) rewrote it
between bytes and a third-party pad's bytes came back corrupted. A pad slower to
acknowledge than the wait, such as the SCPH-110 when clocked early, slips a
packet; a packet that fails part way reads `PadMode::Unknown`, and `Controllers`
keeps the last clean state instead.

### The select is left open

The setup time is a minimum, and a game has a whole frame of other work to do in
it. So a `Controllers` poll ends by asserting `/CS` again (`Transport::open`) on
the port it expects to be asked for next: the port polled before this one if a
pad is in it (a two-player loop alternates), else this one. The next poll finds
that select still in `CTRL` and starts clocking bytes as soon as
`PAD_SETUP_CYCLES` (7,168 system-clock cycles, what the 1,024 status reads of
the earlier delay took on a console) have passed since it was asserted
(`Transport::begin_pad`). The game's frame runs in that time instead of a busy
wait.

The minimum is guaranteed by construction, not by the game's frame being long:

* The time is read from root counter 0 (`Transport::clock`), which `psx-rt`
  sets to count the system clock at entry and nothing in the SDK reprograms. A
  16-bit difference of two readings is never more than the time that passed (a
  wrap only makes it smaller), so a poll that follows the last one at once still
  waits the whole setup time, and a wrap can only make it wait longer than
  needed. The wait is also bounded by 2,048 readings, which take longer than the
  setup time on their own, so a stopped counter cannot hang a poll.
* The select is used only if `CTRL` still reads back as the select that was
  left. A memory-card transfer, a configuration packet, a chain-loaded
  program's entry or anything else that touched the port in between leaves
  something else there, and the poll selects again and waits the old 1,024
  status reads, the sequence every poll used before.
* Only the first try of a poll uses an open select; a retry selects again.
  No select is left open on an empty port, so a pad plugged in always sees a
  fresh `/CS` edge.

An interrupt between the two halves does nothing to the port: no handler in the
SDK touches SIO0. Hardware-tests step `SIO0 SELECT EARLY` (records
`0x6F0`-`0x6F4`) is the console's verdict on all of this.

An empty socket answers `0xFF` and never pulses `/ACK`; `Controllers` probes it
once in 60 polls, so it costs a game almost nothing.

## What it costs

On the PSoXide emulator, one poll of an analog pad on port 1 between 100,000
cycles of other work (port 2 empty):

| Driver | Cycles per poll | Both ports, port 2 empty |
|---|---|---|
| `PadReader` (SDK `efdd3083c`) | 23,054 | 43,218 |
| `Controllers` with the select left open | 16,129 | about 16,600 |

Of the 16,129, 15,471 are the nine bytes on the wire (each byte's
1,360-cycle shift, its `/ACK` delay and pulse) and about 650 are the driver. A
60 Hz frame is 564,480 cycles.

## Analog mode and hot-plug

`Controllers` sends the three configuration packets one video frame apart, as
Sony's libpad does, the first time a pad answers on a port: at boot, and when a
pad is plugged in later (about four frames). A game that wants analog mode again
later calls `Ctx::enable_analog`. A pad parked in configuration mode (the SCPH-110
failure) gets the exit again.

### A pad left digital

The handshake reads none of the answers and a packet that fails part way is not
sent again, so a packet abandoned part way can leave a DualShock digital for the
rest of the session. The history, from the repositories:

* VoXide `c28433f` (2026-07-06): on a console the stick read nothing while the
  buttons worked. The boot code enabled VBlank interrupts and then ran the
  `0x43`/`0x44` exchange, which then ran with VBlank interrupts on; the fix ran
  it first and a main-loop request every 15 frames while the pad read digital
  covered a pad that was not ready. The driver was the synchronous one of that
  day, with no `/ACK` wait and no interrupt engine (the engine arrived on
  2026-10-08).
* Quake's boot code and its `AnalogRetry` (every 15 polls, eight times, any
  state but analog, `Unknown` included: "restricting this retry to those two
  modes left Quake permanently digital on real hardware") come from before
  2026-08-25, the squashed first public commit, so before the `/ACK`-paced
  poll (`c9d5dd4f5`, 2026-09-27). It kept all of it through its repin to the
  paced driver (`8fd7835`), so no console has run Quake without it.
* The demo-disc launcher's request every 60 ticks (`6e60ee3`, 2026-10-03) is a
  notice policy for a disc that requires analog (an SCPH-110 Analog button
  toggling it), not a finding about interrupts.
* The SCPH-110 parked in configuration mode by back-to-back packets
  (`deddb1649`, 2026-08-16) is "not yet re-measured on silicon".

None of the failures belongs to the deleted interrupt-driven engine: they
predate it, and the mechanism applies to today's driver as it did to theirs. A
pad's `/ACK` is a pulse of about 90 cycles that the exchange finds by reading
`STAT`; an interrupt handler running over it hides it (`Transport::exchange_card`
says the same of a card), the wait times out and the packet is dropped.
What no console has shown is how often that happens to the ACK-paced driver;
hardware-tests v2.15, step `PAD CONFIG UNDER IRQ` (`0x6F5`-`0x6F7`), forces the
worst case and measures the plain handshake, for the one console session.

`Controllers` therefore does two things, neither of which a normal frame pays for:

* Each configuration packet goes out with CPU interrupts masked
  (`Transport::mask_interrupts`), from its first byte to its last, about
  15,000 cycles; a pending interrupt is taken after. The select and the setup
  wait before the bytes are outside: an interrupt there only lengthens the gap.
* A pad that answered a packet in full (only a DualShock acknowledges all
  nine bytes; a digital pad stops after the fourth) and still reports digital,
  an unknown ID or configuration mode after the handshake gets the handshake
  (or the exit) again, three times in all for the plug-in, each a few video
  frames after the last because the handshake spaces its packets. A digital
  pad pays nothing beyond the first handshake it has always paid. A DualShock
  that refuses analog pays the three, about eleven video frames, once.

A pad that answers an ID the driver does not know every poll keeps the last
clean state (nothing, if it never had one): `Controllers` does not trust it as
Quake's `UNKNOWN_MODE_TRUST_POLLS` did.

## Memory cards

A card transaction needs the whole port, and a poll needs it too. The
`ControllerPort` token from `Peripherals::take()` has one owner at a time and
both drivers borrow it for the length of one transaction, so a card and a poll
never overlap. A card transaction is about as long as a frame; a pad packet
cannot be inserted into the middle of it. A card transfer that runs while a
pad's select is open deselects it first (`select_with` writes `CTRL`), so the
card sees its own fresh select, and the next poll selects again.
