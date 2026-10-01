+++
title = "psx-mc"
description = "Memory-card transport, files and save containers"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-mc` for PS1 memory-card files and host-side save tests. Start on a RAM-backed card while developing your save format.

## How the crate is organized

`Block` abstracts 128-byte card frames. `HardwareCard` talks over SIO0 when `hw` is enabled; `RamCard` supplies a host-test backing store. `Card` adds the PS1 filesystem and save container. Internal `fs` and `ram` files implement those layers; `compress` optionally provides LZSS compression.

## Integration notes

Use `default-features = false` for logic tests that do not need hardware transport. Treat formatting as a destructive operation that requires an explicit user action; do not copy an automatic-format snippet into a game's startup path. Check errors, free space and existing filenames before writing. Controller and card access share SIO0 and need coordinated ownership.

The complete [memory-card diagnostic](@/docs/examples/hello-memcard.md) starts with a read/hash pass and enables a guarded test write only through its button combination. Read its preflight checks before adapting it.

## API, dependencies and source structure

{{<sdk_crate name="psx-mc" />}}
