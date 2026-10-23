+++
title = "psx-display"
description = "Brightness and screen-position options for a game's settings"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-display` for the picture options a PlayStation game offers on its settings page: a BRIGHTNESS row that reads `DEFAULT` in the middle, `DARKER n` to the left and `BRIGHTER n` to the right, and SCREEN X and SCREEN Y rows that read `CENTRE`, `LEFT n`, `RIGHT n`, `UP n` and `DOWN n`. It exists so games share the step rules, the row text and the save byte instead of copying them.

## How the crate is organized

`Brightness` is a level from -5 to 5, with `DEFAULT` at 0. `ScreenOffset<RANGE>` is an x and y shift in pixels, with a default range of 16. `Label` is a fixed 12-byte string for a row, readable with `as_str`, `as_bytes` or `copy_into`. `BrightnessOverlay` is the plain GPU packet that darkens or brightens a finished frame. Nothing in the crate owns hardware or allocates.

## Integration notes

Step a value with `stepped(-1)` or `stepped_x(1)`; it clamps and never wraps. Show it with `label`, `label_x` or `label_y`. Save it with `to_byte` and `from_byte` for brightness, or `to_bytes` and `from_bytes` for the offset. The encoding is two's complement, so a save written before the option existed reads as the default, and any byte decodes to a clamped value.

The GPU has no gamma control, so brightness is one semi-transparent grey rectangle drawn over the finished frame, subtracting to darken and adding to brighten, by one colour unit per step. `overlay(resolution)` returns `None` at `DEFAULT`, so the default costs nothing per frame. Otherwise link the `BrightnessOverlay` into ordering-table slot 0 before the HUD and menus are inserted there, because inserts prepend and slot 0 is walked last. The draw area and offset must cover the whole frame when it runs, which a split screen or viewport has to restore first. `faded_overlay` keeps the setting while a screen fades.

`ScreenOffset::apply_to` edits the `DisplayConfig` the game already has. Pass the result to the GPU's display setter. It is a video-signal change, with nothing to do per frame. The [adoption guide](https://github.com/EBonura/PSoXide/tree/main/sdk/docs/DISPLAY-OPTIONS.md) lists the call for each need.

There is no standalone `psx-display` example in the SDK set.

## API, dependencies and source structure

{{<sdk_crate name="psx-display" />}}
