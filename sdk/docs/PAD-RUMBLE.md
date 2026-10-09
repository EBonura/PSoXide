# DualShock motors

A DualShock has two motors, a small one that is on or off and a large one with a
level. They are not under the poll until the pad has been told where to take
them from; the telling is a visit to configuration mode, and the pad forgets it
when it loses power.

## On the wire

```text
enter config   01 43 00 01 00 00 00 00 00
map motors     01 4D 00 00 01 FF FF FF FF     small -> first motor byte, large -> second
exit config    01 43 00 00 00 00 00 00 00
every poll     01 42 00 [small] [large] 00 00 00 00
```

`[small]` is `0x01` for on (bit 0), `0x00` for off; `[large]` is the level,
0 to 255 (below about 0x40 the motor does not turn). The mapping bytes are the
first two after `4D 00`: `00` puts the small motor on the first motor byte of a
poll, `01` puts the large motor on the next, `FF` leaves a slot unused. The
mapping packet is only answered `F3 5A` from configuration mode, which is how
the driver knows a pad took it; a pad that refuses configuration mode answers
with its own ID and is left alone.

A poll sends the motor bytes only to a pad that answers `0x73` (analog). A
digital pad (`0x41`) and a pad parked in configuration mode (`0xF3`) are sent
zeros, and a pad that was never mapped ignores the bytes it is sent.

## Synchronous driver

```rust
use psx_pad::{enable_rumble_on, poll_rumble_on, PadReader, Port, Rumble};

// Once, after a pad appears (blocks a few frames, like require_analog_on).
if enable_rumble_on(&mut port, Port::One) {
    // every frame
    let mut reader = PadReader::port1();
    reader.set_rumble(Rumble::new(true, 180));
    let pad = reader.poll_on(&mut port);
    // pause, menu, exit
    reader.stop_motors_on(&mut port);
}
```

`enable_rumble_on` returns `true` when the pad took the mapping from
configuration mode and reports analog mode afterwards; its analog or digital
mode is left as it was. `poll_rumble_on` is the free-function form of a poll
with a request. `PadReader` keeps the request and sends it on every poll, and
`stop_motors_on` asks for none and polls once so the pad hears it.

## Interrupt engine

```rust
use psx_pad::console;
console::enable_rumble(Port::One);            // maps now, and again after every replug
console::set_rumble(Port::One, Rumble::new(false, 120));
console::stop_motors();                       // returns once zeros have gone out
```

The engine maps the motors in one visit to configuration mode, one packet per
VBlank. If an analog request is pending at the same time the same visit does
both (enter, lock analog, map, exit). A pad that is unplugged loses its mapping
and the engine maps the next one on its own, as soon as it reports analog mode.
A digital pad, or one that refuses, is asked once per plug-in
(`Engine::rumble_refused`) and then only sent zeros. `stop_motors` blocks for up
to three VBlanks until a poll with the motors off has gone out on every port, and
`console::uninstall` does it before it hands the port back. A port a `Lease`
holds is not polled meanwhile, so a card transaction that outlasts the pause
leaves the motors as they were.

## Tests

`transport_tests.rs` and `engine_tests.rs` run the packet sequences against the
controller models: the enter, map, exit order and the mapping bytes; the motor
bytes each pad type is sent; a pad that refuses configuration mode, drops out
before the mapping packet, loses its reply, or ignores the exit; a replug; two
ports at once. `pad-engine-check` maps the emulator's pad through the engine.
The motors themselves are not observable off the console.
