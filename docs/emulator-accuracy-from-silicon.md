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

## Conformance: 13 observations diverge

**Status:** open. **Found:** 2026-07-26, partial console capture, HWTEST v1.3.

Only page 1 of 5 was recovered, which carries the header and the first 138 of
173 conformance observations. Thirteen of those 138 differ from PSoXide. The
timing records and precision values live on the unrecovered pages and remain
unknown.

| Case | Group | Test | PSoXide | Console |
|---:|---|---|---|---|
| 10 | IRQ | GPU IRQ visible through I_STAT | `0x0000000C` | `0x0000000A` |
| 17 | GPU | GP0 IRQ set + GP1 ack | `0x00000001` | `0x00000000` |
| 23 | SPU | SPUSTAT readable | `0x00000800` | `0x00000000` |
| 32 | TMR | mode read clears sticky flags | `0x00000003` | `0x00000001` |
| 44 | SIO | direct port 1 pad poll stability | `0x00737373` | `0x00414141` |
| 46 | GPU | DMA direction mode latch | `0x00000004` | `0x0000000B` |
| 49 | GPU | GPU IRQ1 flag settle latency | `0x00010001` | `0x00010000` |
| 52 | GPU | DMA-direction readback values | `0x000000A0` | `0x000000D6` |
| 121 | GTE | LZCR settle +1 | `0x0000001F` | `0x00000008` |

Cases 14, 15, 38 and 51 also differ but are raw timer counts, where a small
difference is expected rather than a defect; they are listed here only so a
later capture is not mistaken for a new finding:

| Case | Test | PSoXide | Console |
|---:|---|---|---|
| 14 | timer2 free-run increments | `0x00009251` | `0x00009253` |
| 15 | timer1 scanline range | `0x00000070` | `0x00000049` |
| 38 | timer1 HBlank clock advances | `0x00000228` | `0x00000227` |
| 51 | timer0 dot/system tick counts | `0x92A71CB6` | `0x925D1CC4` |

The three startup scan digests (CPU, GTE, SPU register-behaviour fingerprints)
match exactly, so those subsystems agree at the level those scans probe.

Case 15 is worth singling out: `0x70` against `0x49` is a 27-line difference in
the scanline range, far larger than counter jitter, and case 23's `0x800`
against `0x000` is an entire SPUSTAT bit that PSoXide reports and the console
does not.

## How these get found

The hardware-test disc is the instrument: `make hwtest-silicon SILICON=<pages>`
diffs a console capture against the emulator record by record. A gap that shows
up as a moved number there is far cheaper to act on than one inferred from a
game misbehaving.

No capture has been recovered in FULL yet, for reasons tracked in
`hardware-test-disc.md`, so the timing records and precision values have never
been compared. The conformance findings above come from a single recovered QR
page; the SPU ADSR gap came from the *shape* of a failed capture rather than its
contents.
