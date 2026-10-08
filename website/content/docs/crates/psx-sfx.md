+++
title = "psx-sfx"
description = "Sample banks and one-shot playback"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-sfx` for game sound effects when direct voice-register management would duplicate bookkeeping. It sits above `psx-spu` and reads cooked audio through `psx-asset`.

## How the crate is organized

The single source module defines `Bank` for sample placement, `Sample` for resident sample metadata, `OneShot` and `LoopingSample` for playback configuration, and `Player<N>` for a fixed set of voices.

## Integration notes

Initialize the SPU before uploading a bank. Reserve its sample-memory region and voices against other audio systems. Call `Player::tick` regularly using the same clock frequency supplied to `Player::new`; it silences one-shots after their expected duration. One-shot samples include a silent parking tail, while looped ambience uses its loop-aware playback path. Inspect the API's behavior for invalid cooked data and exhausted sample memory before accepting external assets.

The [complete audio example](@/docs/examples/hello-audio.md) covers the underlying SPU path and doesn't use `psx-sfx`. The reference below has the bank/player usage sequence and method contracts.

## API, dependencies and source structure

{{<sdk_crate name="psx-sfx" />}}
