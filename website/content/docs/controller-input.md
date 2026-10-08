+++
title = "Read controller input"
description = "Poll a controller, distinguish a held button from a new press, and use input to change the screen."
weight = 3
[extra]
kind = "How-to"
eyebrow = "How-to · SDK"
+++

{{<example_player name="hello-input" />}}

## Build and try it

Follow the [tool setup](@/docs/first-ps1-program.md#1-install-the-tools), then run this from the SDK checkout:

```sh
make disc EXAMPLE=hello-input
```

Open `build/examples/mipsel-sony-psx/release/hello-input.cue` in the desktop emulator. Keep its BIN beside it. In the browser example, click the screen to give it keyboard focus. Arrow keys change the background; **F / G / H / X** are Cross / Circle / Square / Triangle and draw a coloured triangle. Right resets the background.

## Poll once each frame

`PadReader::poll_on(&mut port)` returns the controller's connection mode, buttons and stick values. `port` is the controller-port token from `Peripherals`, shared with the memory card driver. A `PadReader` is bound to one socket (`PadReader::port1()` or `port2()`), and when a poll comes back garbled it returns the last clean state instead, so a held button does not read as released for one frame and then as a fresh press. Read it once at the beginning of your update and share that snapshot with the rest of your game:

```rust
let mut port = peripherals.controller_port;
let mut reader = PadReader::port1();

// Inside the loop:
let state = reader.poll_on(&mut port);
let pad = state.buttons;
if pad.is_held(button::UP) {
    r = r.saturating_add(4);
}
```

This is the pattern in `hello-input`. `saturating_add` stops at 255 instead of wrapping a bright channel back to black. The screen prints the held button names, so you can compare the input you expect with the packet the program read.

## Trigger an action once

Movement usually continues while a button is held. Jumping, selecting a menu item or starting a sound usually happens once when the button goes down. Keep the previous frame's state to distinguish them:

```rust
// Before the loop:
let mut previous = ButtonState::NONE;

// Inside the loop, after polling:
let cross_pressed = pad.is_held(button::CROSS)
    && !previous.is_held(button::CROSS);
if cross_pressed {
    r = 255;
}
previous = pad;
```

This is a small change you can make in `hello-input`; import `ButtonState` if it is not already imported. Rebuild and hold Cross: the action happens on the first frame. Release and press again to trigger it again. The [audio guide](@/docs/sound-effects.md) uses this same technique.

## Check the connection and analogue mode

A physical controller can disconnect or change mode. Use `state.is_connected()` before relying on input, and `state.is_analog()` before using `state.sticks`. The example displays the mode and raw stick bytes. It also asks for analog mode at boot with `require_analog_on` and asks again if a pad is plugged in later, because a program started from a launcher inherits whatever mode the last program left. Digital directional buttons and analogue axes are different inputs; do not assume a pad always returns stick data.

## Try a change

Change the colour step from `4` to `1`, rebuild, and compare how long it takes to reach full brightness. Then change the Cross action to reset all three channels. Changes to the local source affect your rebuilt program; the player above remains the published example.

## If nothing responds

Click the game screen, release any held keys and try again. The page shortcuts do not control an unfocused player. On a phone, use a connected keyboard or controller; this player has no touch D-pad. Test physical controllers on the desktop emulator and original hardware as well: browser input is not a controller-transport test.
