# Emulator accuracy gaps confirmed against silicon

Findings where a real console and PSoXide provably disagree. Each entry records
what was measured, on what, and how to reproduce it, so a fix can be verified
rather than argued about.

A gap only belongs here once it has been observed on hardware. Suspicions from
reading code belong in the relevant subsystem doc instead.

## The first complete capture (2026-07-26)

`docs/hardware-refs/px7-silicon-2026-07-26.txt`, HWTEST v1.4, recovered from a
console recording via `tools/hwtest-video-qr.py` and CRC-valid. 158 values differ
from PSoXide. Compare with `make hwtest-silicon SILICON=<that file>`.

The harness validates itself on this capture: console single-speed reads measure
13.20 ms/sector against a 13.33 ms spec, and double-speed 6.49 against 6.67. The
CD numbers below are therefore trustworthy in absolute terms, not just relative.

### GPU rasterisation is pixel-exact

All 22 bit-exact raster hashes match silicon, every primitive family and edge
case. Whatever else differs, the rasterizer draws the right pixels.

### CD-ROM seek: far too fast, and distance-independent

| Seek | Console | PSoXide |
|---|---|---|
| +1 sector | 181 hblanks (12 ms) | 12 |
| +16 | 1239 (79 ms) | 13 |
| +128 | 5679 (361 ms) | 761 |
| +512 | 3022 (192 ms) | 763 |

The console cost tracks head travel; PSoXide returns essentially two values
regardless of distance. Any game whose streaming budget depends on seek cost is
being modelled optimistically by one to two orders of magnitude.

#### The seek data does not support a model yet

Attempted 2026-07-26, not implemented. Four distances is too few, and they are
non-monotonic: +128 sectors measures 361 ms while +512 measures 192 ms. Neither
a linear nor a square-root fit through the endpoints gets within 2x of the
middle points (both land ~0.2-0.5x at d=16 and d=128). No monotonic physical
model reproduces this, and baking a non-monotonic table into the emulator would
encode one disc's anomaly as hardware behaviour.

What it needs: more seek distances, several repeats each, ideally on more than
one disc, so an outlier is visible as an outlier. That is a guest-side change to
records `0x90`-`0x93`, so it wants doing before the next burn rather than after.

### CD-ROM read: four times too SLOW

Console 13.20 ms/sector single speed, 6.49 double, both within 1% of spec.
PSoXide takes 53.5 ms/sector, so a data read costs 4x what it should. This is the
opposite direction to the seek error, so the two do not cancel.

The constants are NOT the problem: `CD_READ_TIME` is 451,584 cycles, exactly
13.33 ms, and `sector_read_cycles()` halves it for double speed. Both are right.
The 4x is somewhere in the DataReady scheduling. `cdrom.rs` pushes a due
DataReady event out by `CD_READ_TIME / 2` whenever a CD IRQ is still pending
unacknowledged, which is a plausible contributor but accounts for 1.5x at most on
its own.

Confounder to rule out first: the probe acknowledges INT1 without ever reading
the sector data. Real software drains the sector; if the emulator's pacing
depends on that, the 4x is partly the probe's own doing and the fix belongs in
the guest. Settle this with emulator-side instrumentation of actual INT1
deltas before changing any scheduling.

### CD-DA contention: no measurable effect, on either

`0x9B` (read with audio playing) against `0x9C` (audio stopped) measures 1647
against 1657 hblanks on console: no contention at this granularity. A negative
result, and worth recording as such, since the premise of the probe was that
hardware would show a penalty here.

### GPU fill: the emulator rasterises on the CPU's timeline

Console fills are 3x to 80x faster than PSoXide for small primitives, and
SLOWER for large ones (`gpu_fill_rect_mono_16x32`: console 24021, PSoXide
13804). The two are not measuring the same thing. Hardware absorbs packets into
the FIFO and draws in the background, so the CPU-side interval reflects
submission until the FIFO backs up; PSoXide appears to rasterise during the GP0
write, putting draw cost on the CPU's clock.

`gpu_fill_quad_flat_4x64` matches almost exactly (9987 against 10114) because at
that size the console genuinely blocks, which is consistent with this reading.

This is the data that was missing to model GPU timing at all, and it says the
current model overstates CPU cost for small draws and understates it for large.

### SIO: one pad pacing does not work on hardware at all

| Variant | Console | PSoXide |
|---|---|---|
| 0 | 7516 | 375 |
| 1 | **no response** | 1391 |
| 2 | 9587 | 3439 |
| 3 | 12304 | 6521 |

Console polls take 2x to 20x longer, and variant 1 gets no valid reply at all
while PSoXide answers happily. That is the SCPH-1200 setup-delay family of
problem, now a standing measurement rather than a session of guesswork.

### MDEC

Table uploads cost the console about 1.6x what PSoXide charges (99 against 66
cycles for a 16-word luma table). Decode is roughly linear on console (4205 for
one macroblock, 7463 for two) and wildly non-linear in PSoXide (3119, then
39374), which points at the emulator's lazy decode-on-read rather than at
hardware.

## SIO: the pad's setup delay is not modelled at all

**Status:** open, fully characterised. **Found:** 2026-07-26, HWTEST v1.5 capture
(`docs/hardware-refs/px7-silicon-v1.5-2026-07-26.txt`).

A twelve-point sweep of the setup delay between selecting the controller and
clocking the first byte. The console has a hard threshold; PSoXide has none.

| Setup spins | Console | PSoXide |
|---:|---|---|
| 0 | **no reply** | 378 |
| 64 | **no reply** | 891 |
| 128 | **no reply** | 1403 |
| 192 | 8587 | 1915 |
| 256 | 8771 | 2432 |
| 512 | 10541 | 4475 |
| 1024 | 14077 | 8571 |
| 1536 | 17615 | 12667 |

Two separate defects. PSoXide **answers a poll that real hardware ignores** at
every delay below 192 spins, so guest code with too short a setup delay works in
the emulator and fails silently on a console. And once the pad does reply, the
console takes 2-5x longer per poll than PSoXide charges.

This is the SCPH-1200 controller problem, previously a remembered anecdote that
cost a debugging session, now bracketed to between 128 and 192 spins. The SDK's
`DEFAULT_SETUP_SPINS = 1024` is safely above it, and now demonstrably so rather
than by luck.

One caveat: an earlier capture saw setup 0 reply once. The boundary is not
perfectly repeatable, which is expected of a physical handshake, but everything
below 192 failed in the sweep run.

## CD seek is variance-dominated, not distance-dominated

**Status:** closed as "do not model as f(distance)". **Found:** 2026-07-26.

Ten distances, five repeats each, min/median/max retained:

| Distance | min | median | max |
|---:|---:|---:|---:|
| 1 | 11.3 ms | 11.5 | 12.6 |
| 4 | 51.2 | 51.4 | 51.6 |
| 8 | 104.9 | 105.1 | 117.9 |
| 16 | 104.5 | 104.7 | 105.3 |
| 32 | 289.3 | 289.4 | **552.8** |
| 64 | 159.7 | 173.6 | 318.1 |
| 128 | 137.3 | 137.6 | 137.7 |
| 256 | 90.7 | 91.1 | 91.6 |
| 512 | 151.8 | **310.2** | 310.5 |

Still non-monotonic with ten points, and now the reason is visible: the spread
within one distance (32 sectors ranges 289 to 553 ms) is larger than the spread
between distances. Seek cost here is dominated by rotational position and head
settling, not by how far the head travels. Backward seeks confirm it, differing
from forward in opposite directions at 64 and 256 sectors.

**Consequence:** the earlier plan to replace `SEEK_SECOND_RESPONSE_CYCLES` with a
distance model was wrong, and would have encoded noise. The defensible change is
to keep a constant and raise it: PSoXide's 53 ms sits below almost every console
sample, whose typical cost is 100-300 ms. That is a one-line change with a
measured target, and it wants checking against game boot paths since seeks
getting several times slower can expose timeouts.

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

## The first FULL characterisation capture (2026-08-07)

HWTEST v1.17, all five PX8 pages plus SB4 recovered from one console
recording. This is the capture the note at the bottom of this file was
waiting for, and most of what it found has already been fixed in v1.18.
What it leaves open:

### SPU RAM uploads do not land on silicon (the WRITE, not the read)

The v1.18 capture settles which half is broken, and it is the write.

Precision 002-017 are the raw words of a 64-byte DMA upload read straight
back. Undoing the documented unstable-read shape (one `0xFFFF` inserted
per DMA block, everything after it shifted by a halfword) reconstructs
SPU RAM as `0000 C0DE 0111 9C00 DD65 ...` against an uploaded `0000 C0DE
0111 C0DE 0222 ...`. **Only the first 6 bytes of 64 arrived.** Precision
039-042 read back a region written through the manual FIFO and match it
in **zero of 8 halfwords**.

Both readings are trustworthy: precision 037/038 hash the same region
through two different block shapes with the override armed and agree
exactly (`0x083B6E3D`), so the read path is self-consistent. A read that
were itself corrupting would not reproduce the same content twice at two
shapes.

This is the same fault as the demo disc's NitroXide tone. psx-sfx writes
a 16-byte parking block after every sample; a voice that finishes parks
on it and loops it forever. Emulator-side tests
(`a_correct_parking_block_is_silent_however_long_it_loops` and its
corrupted counterpart) show a correct block renders an exact 0 while one
whose header was replaced by `0xFFFF` self-loops audibly at 44100/28 Hz
= 1575 Hz -- the tone the console recording carries.

What is still open is WHY the write stops. `upload_adpcm` picks the
largest DMA block size dividing the payload, so 64 bytes go as a single
16-word block, which is the SPU's entire 32-halfword transfer FIFO with
no DRQ pause inside it. Conformance `0xBC`-`0xBF` ask the four questions
that separate the candidates: is the read stable, do the DMA and FIFO
writes agree with each other, does writing the transfer address after
arming the mode fix it, and does pacing the same payload as four
4-word blocks fix it.

### SPU RAM readback is lossy on silicon even with the DMA override armed

Conformance `0xA6` (DMA round trip) and `0xA7` (manual-FIFO round trip)
upload a known block to SPU RAM and read it back. Both still FAIL on
console (`B00FF59A` / `736B7C0F` against the analytic `D24F8305` /
`574B8A35`) after the v1.17 fix that arms the memory controller's SPU DMA
timing override (`1F801014h` bits 24-27) around the readback. The
emulator now PASSES both, so it is the permissive one.

The override was the right fix for the emulator, and PX7 precision values
036-038 show stable-mode reads being faithful on silicon, so the residual
corruption is NOT the unstable-read shape this suite already models. The
write side is the remaining suspect. The observed hashes move between
builds while staying stable within one, which points at content rather
than timing.

### NCLIP positive-winding anomaly (`0x8B`)

The one conformance case still failing in the emulator, and the console
passes it. The console computes the full cross product in both phases of
the controlled scene-C replica; the emulator's hazard model substitutes
an old Y in the settled reference phase, so its two phases disagree.
Two narrow fixes were tried and reverted: keying the spaced-cadence rule
on the SXY1->SXY2 gap alone, and restricting history establishment to
RTPT. Each repaired `0x8B` and broke the small-value settle cases
`0x74`-`0x78`, which the console passes. The model needs the
positive-winding anomaly itself, not another calibrated special case.
The v1.17 discriminators `0xAD`-`0xB0` were added for exactly this and
their console values are now in hand, including the read-interlock
result below.

### The CPU does not stall on COP2 reads

`0xB0` brackets `nclip` plus an immediate `mfc2` against a two-nop
baseline on Timer 2 and reads `lo=8, hi=9`: reading MAC0 the instruction
after issuing NCLIP costs no more than two NOPs. psx-spx and DuckStation
both describe a read interlock that stalls until the command completes;
this console has none, which is precisely why partial MAC0 values are
observable at all. Any future GTE latency work has to start here.

### The demo-disc glyph corruption is in the glyph rect path

`0xB3`-`0xB5` all fail on console. `0xB4` (blit of the render-to-VRAM
text cache) and `0xB5` (the same text drawn directly) return the SAME
hash on silicon, exactly as they do in the emulator, so the cache round
trip is faithful and the corruption happens when the 4bpp glyph
rectangles are rasterised. Per-glyph probes `0xB6`-`0xBA` landed in v1.18
to name the guilty glyph; alignment alone does not explain it, since 'r'
shares 'f''s atlas row and its `u&3==3` start and renders correctly on
screen.

## How these get found

The hardware-test disc is the instrument: `make hwtest-silicon SILICON=<pages>`
diffs a console capture against the emulator record by record. A gap that shows
up as a moved number there is far cheaper to act on than one inferred from a
game misbehaving.

A full characterisation capture was finally recovered on 2026-08-07 (all five
PX8 pages plus SB4, from one recording, decoded by `hwtest-video-qr.py` in a
single pass). The older findings above predate it and came from a single
recovered QR page; the SPU ADSR gap came from the *shape* of a failed capture
rather than its contents.
