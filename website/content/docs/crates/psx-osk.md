+++
title = "psx-osk"
description = "Controller-driven text entry"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-osk` when a player needs to enter a name or other short text with a controller.

## How the crate is organized

The single module exposes `Keyboard`, navigation `Dir`, emitted `Action` values and a caller-selected `Palette`. The keyboard tracks the selected key, case and symbol page. Your program owns the text buffer and handles insertion, backspace and commit actions.

## Integration notes

Feed D-pad navigation through a repeat policy such as `PadTracker`, and activate a key on a button edge. Enforce your text capacity when handling `Action::Insert`; the keyboard does not own or resize the destination. Rendering assumes a 320-wide display and an 8×8 font cell, so account for the panel's height in your UI layout.

Read [controller input](@/docs/controller-input.md) first. The API reference includes the navigation/activation/drawing sequence; this crate has no dedicated standalone SDK example yet.

## API, dependencies and source structure

{{<sdk_crate name="psx-osk" />}}
