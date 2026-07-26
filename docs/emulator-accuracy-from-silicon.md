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

## Triage of the 13 conformance divergences

Investigated 2026-07-26. Most are NOT quick wins, and it is worth being precise
about why rather than filing thirteen tickets.

**Already known, blocked on FIFO/latency modelling (cases 10, 17, 46, 49, 52).**
The suite itself marks these `info` and says so in the source: *"Racy on
silicon: both the GPUSTAT.24 and the I_STAT observations race the GPU command
FIFO and flip run-to-run"* and *"the GPUSTAT bits 29-30 readback lags the GP1(04)
write through the FIFO"*. They flip between runs on hardware, so the console
values recorded above are one sample of a race, not a target to match. Closing
them means modelling GPU FIFO latency, which is gated on CPU cycle accuracy.

**Instantaneous snapshots of toggling state (case 23).** `SPUSTAT readable`
reads the register once. The difference is bit 11, "writing to first/second half
of capture buffers", which toggles continuously as the SPU runs. A single read
landing on a different phase is not a defect.

**Environmental (case 44).** `direct port 1 pad poll stability` depends on the
physical controller and what it reports; an emulated pad answering differently
from an SCPH-1200 is expected.

**Raw timer counts (cases 14, 15, 38, 51).** Small differences are jitter. Case
15's 27-line gap in the scanline range is larger than that and may be real, but
it needs a second capture to separate from a one-off.

**Genuinely actionable: case 32, `mode read clears sticky flags`.** See below.

## Timers: registers are read without catching up first

**Status:** open, root-caused, not yet fixed. **Found:** 2026-07-26.

The test sets Timer 2 to target 24 with reset-at-target, lets it free-run, then
reads the mode register twice. Reading mode clears the sticky reached-target
flag, so the expected result is "set on the first read, clear on the second".
PSoXide gives exactly that (`0x3`). The console gives `0x1`: set on the first
read, and *still set* on the second.

The console is right, and for a mechanical reason. The timer is free-running at
the system clock with a target of 24, so it reaches target every 24 ticks, which
is fewer cycles than two consecutive MMIO reads take. The flag is genuinely
re-latched between the two reads. It cannot be otherwise on hardware.

PSoXide never sees this because `Timers::read32` returns the register value
without advancing the timer to the current cycle first, and the bus timer-read
paths (`bus.rs`, the 8/16/32-bit branches) call it directly. Between two
adjacent reads the emulated counter does not move at all.

The code already anticipates the behaviour it does not implement:

> Reading mode acknowledges both sticky reached flags. If the counter is still
> at its terminal condition, hardware may latch the target flag again on a later
> timer clock.

**Fix shape:** advance the timers to the current cycle before servicing a timer
register read, the usual catch-up-on-access pattern. `Timers::advance_to` already
exists and `bus.rs` already calls `advance_to_video` elsewhere, so the pieces are
there; the read paths need the current cycle and video parameters threaded in.

**Expect fallout.** Making timers advance on access will move timing-derived
numbers across the suite, so the emulator baseline will need re-pinning and the
change wants checking against the timing records rather than landing blind.

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
