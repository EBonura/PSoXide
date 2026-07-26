# Emulator accuracy gaps confirmed against silicon

Findings where a real console and PSoXide provably disagree. Each entry records
what was measured, on what, and how to reproduce it, so a fix can be verified
rather than argued about.

A gap only belongs here once it has been observed on hardware. Suspicions from
reading code belong in the relevant subsystem doc instead.

## SPU: an all-zero ADSR does not decay

**Status:** open. **Found:** 2026-07-26, console recording, HWTEST v1.2 disc.

Writing `ADSR = 0` (both halves) sets sustain level 0. On real hardware the
envelope therefore runs attack, then decays to silence shortly after key-on. In
PSoXide the voice holds its key-on level indefinitely.

Measured on the same guest binary, RMS per second of the hardware-test audio
readout, which keys one voice on and lets it run:

| | Envelope |
|---|---|
| Console | 863, 452, then silence. Gone in ~3 s |
| PSoXide | ~897 flat, unchanged after 44 s |

The console recording is `2026-07-26 12-29-39.mov`; the emulator side
reproduces with any build of the disc predating v1.3, since v1.3 stops using
that ADSR:

```sh
make hwtest-audio     # writes build/hwtest-audio.wav
```

then take RMS per second of the result.

**Why it went unnoticed:** the emulator's SPU parses ADSR into phases and
carries a `sustain_level` field, so the configuration is decoded; what does not
happen is the decay running down to a sustain target of zero. Anything relying
on a held voice therefore behaves in the emulator and fades on hardware.

**Blast radius:** any guest that keys a voice on with a zero ADSR expecting it
to hold. The SDK's `Adsr::passthrough()` documented itself as exactly that
("voice stays at key-on volume until key-off"), which is how the hardware-test
disc came to use it and lose 80% of its payload on the first console capture.
That doc comment is corrected as of the same change.

## How these get found

The hardware-test disc is the instrument: `make hwtest-silicon SILICON=<pages>`
diffs a console capture against the emulator record by record. A gap that shows
up as a moved number there is far cheaper to act on than one inferred from a
game misbehaving.

Nothing has been diffed yet: no console capture has been recovered in full, for
reasons tracked in `hardware-test-disc.md`. The ADSR gap above was found from
the *shape* of a failed capture rather than from its contents.
