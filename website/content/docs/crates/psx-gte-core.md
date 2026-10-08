+++
title = "psx-gte-core"
description = "Shared geometry math and host simulation"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-gte-core` when you need the shared register-shaped math types or a host-side software GTE. Normal PS1 game code usually reaches these types through `psx-gte`.

## How the crate is organized

`math` defines `Vec3I16`, `Vec3I32` and `Mat3I16`. `transform` contains shared transform helpers. `state` contains the software GTE register state and command execution and is compiled only on non-MIPS targets. The crate reexports its common types at the root.

## Integration notes

The PS1 API below excludes the simulator. Open the host API for `Gte`, its register operations and `execute`. A host preview using this core is useful during development; it does not replace a console check of the complete program, including CPU timing, interrupts and GPU submission.

The [hello-gte example](@/docs/examples/hello-gte.md) demonstrates the hardware-facing `psx-gte` layer that consumes these math types.

## API, dependencies and source structure

{{<sdk_crate name="psx-gte-core" />}}
