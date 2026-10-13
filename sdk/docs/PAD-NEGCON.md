# neGcon input

`PadReader::poll_on`, `poll_on` and the IRQ engine publish `PadMode::Negcon`
with `PadState.negcon: Option<NegconState>` only after a complete response.
The state contains active-high digital `buttons`, raw `twist`, `i`, `ii` and
`left_shoulder`. No calibration or pressure-to-button mapping is imposed.

The device uses ID `0x23`, magic `0x5A` and nine exchanges including selection.
The four bytes after buttons are twist/I/II/L; nominal twist center is 128,
and each pressure spans 0 released to 255 pressed. Compatible wheels use the
same identity. See [PSX-SPX's controller specification](https://psx-spx.consoledev.net/ps1/sio/controllersandmemorycards/controllers-racing-controllers/).

```rust,ignore
let mut reader = psx_pad::PadReader::port1();
let pad = reader.poll_on(&mut peripherals.controller_port);
if let Some(input) = pad.negcon {
    steer(input.twist);
    throttle(input.i);
    brake(input.ii);
}
```

`is_analog()` and `has_sticks()` remain false for neGcon; `PadState.sticks`
stays centered. Existing digital/DualShock fields retain their meaning.
`RawPoll::negcon()` checks ID, magic and exact response length, but diagnostic
raw polling does not replace the production transport's timeout validation.
The IRQ engine and blocking reader retain the last clean sample on a failed
packet. A clean different device or disconnection removes neGcon input.

Adopting this revision requires adding `negcon: None` to explicit `PadState`
literals, or using `..PadState::NONE`, and handling the new `PadMode::Negcon`
variant in exhaustive matches. No game is repinned automatically. `hello-input`
shows twist and pressure values; protocol tests use deterministic controllers,
not physical neGcon hardware.

On the pinned host compiler, `PadState` is 16 bytes (previously 8),
`NegconState` is 6 and `RawPoll` remains 12. Account for the extra 8 bytes per
cached pad state when adopting in a game with a tight resident budget. The
MIPS example and all-feature MIPS lint also build; Rust layout is not a stable
external ABI.
