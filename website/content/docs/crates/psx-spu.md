+++
title = "psx-spu"
description = "Sound voices, sample RAM and envelopes"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-spu` to configure the sound hardware directly. It supplies typed voice indices, pitch, volume, ADSR envelopes and aligned sample addresses.

## How the crate is organized

The root contains initialization, sample upload, voice configuration, key-on/key-off, loop, noise and interrupt helpers. `Voice`, `Pitch`, `Volume`, `CdVolume`, `Adsr` and `SpuAddr` keep the different register representations distinct.

## Integration notes

Initialize the SPU, upload prepared ADPCM, configure a voice and start it. Pitch uses a fixed-point sample-rate multiplier; addresses passed to `SpuAddr` are byte offsets with the required alignment. Keep sample memory and voices from overlapping other audio users. A one-shot's end flag is not a general voice-lifetime manager: use `psx-sfx` when you want timed cutoff and shared voice allocation. SPU IRQ helpers do not take ownership of CPU interrupt acknowledgement or your refill schedule.

See the [sound how-to](@/docs/sound-effects.md) and full [hello-audio source](@/docs/examples/hello-audio.md).

## API, dependencies and source structure

{{<sdk_crate name="psx-spu" />}}
