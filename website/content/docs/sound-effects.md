+++
title = "Play a sound on a button press"
description = "Upload ADPCM samples to the SPU, configure voices, and trigger sound effects once per press."
weight = 7
[extra]
kind = "How-to"
eyebrow = "How-to · SDK"
+++

{{<example_player name="hello-audio" />}}

## Build and try it

With the [SDK tools installed](@/docs/first-ps1-program.md#1-install-the-tools):

```sh
make disc EXAMPLE=hello-audio
```

Open `build/examples/mipsel-sony-psx/release/hello-audio.cue`. The example embeds its cooked `.psau` samples, so it needs no extra audio tracks. In the browser, start the player, enable sound with its speaker button, and focus the screen. **F** triggers jump, **G** coin, **H** punch and **X** swoosh. Arrow keys and Enter trigger other effects.

## Load samples once

Before the render loop, the example initializes the SPU and parses each embedded `Audio` asset. It copies the asset's ADPCM payload into sound RAM, starting at byte address `0x1010`:

```rust
let addr = SpuAddr::new(next_addr);
spu_driver.upload_adpcm(addr, audio.adpcm_bytes());
ch.voice.configure_sample(
    addr, audio.sample_rate_hz(), ch.volume, Adsr::sample(),
);
next_addr += audio.adpcm_bytes().len() as u32;
```

Each channel gets a voice, sample address, sample rate, volume and envelope. Keep allocations aligned to the SPU's sample-address requirements and within its 512 KB sound RAM. A sound's bytes must remain available there while its voice plays.

## Detect the press edge

Polling a held button every frame would restart a sound every frame. Instead, compare the current and previous button states:

```rust
let now = pad.is_held(ch.button);
let was = prev_pad.is_held(ch.button);
if now && !was {
    on_mask |= ch.voice.mask();
}
```

Once the channels have been checked, start all newly triggered voices together:

```rust
if on_mask != 0 {
    Voice::start(on_mask);
}
prev_pad = pad;
```

The [controller guide](@/docs/controller-input.md) explains this distinction between held input and a new press. In this example, releasing the button does not immediately cut a one-shot sample off. Pressing it again retriggers the same voice.

## Use the screen to debug

The example flashes the background and a `PLAYING` label for a short fixed number of frames when a channel is triggered. This is **trigger feedback**, not a measurement of whether the SPU voice is still audible. A working flash with no sound points you toward mute, output-device or SPU setup, rather than the input path.

## Try a change

Change a channel's `Volume::linear` ratio to reduce its level, then rebuild. Swap the button assignments for jump and coin. Hold a button: it should trigger once. Release and press again: it should retrigger. Press two mapped buttons together to exercise overlapping voices.

## Credits and browser limits

The nine effects are from **Kronbits Free SFX**, recorded as CC0-1.0 in the repository's [audio import report](https://github.com/EBonura/PSoXide/blob/main/assets/audio/freesfx/report.json). The report lists source files and hashes for the cooked assets.

Browser audio can require a click inside the player before it starts. Check its mute button and your device's output volume. This player does not download the full demo disc or play CD music. Check the final mix on original hardware before treating emulator playback as a hardware result.
