+++
title = "psx-settings"
description = "Versioned preferences and optional persistence"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-settings` for common game preferences, action bindings and high scores. Keep game-specific simulation state in your own save format.

## How the crate is organized

The root defines `Profile<ACTIONS, SCORES>`, validation, encoding/decoding and error types. The default `card` feature adds save/load helpers over `psx-mc`; disabling default features leaves the profile and codec available without card persistence.

## Integration notes

Initialize a profile from your action map, sanitize user-adjustable values, then encode into a caller-owned buffer. The record includes an explicit version and checksum. Handle missing, incompatible or corrupt records by restoring your chosen defaults. Card helpers still need a deliberate filename and save policy; they do not give the game ownership of every file on a card.

Study the [controller example](@/docs/examples/hello-input.md) for input handling and the [memory-card diagnostic](@/docs/examples/hello-memcard.md) for storage behavior. These are prerequisite examples, not dedicated `psx-settings` programs.

## API, dependencies and source structure

{{<sdk_crate name="psx-settings" />}}
