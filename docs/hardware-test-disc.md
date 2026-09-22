# PS1 hardware-test disc

The `hardware-tests` disc is the silicon reference for PSoXide. It runs the
same executable in PSoXide and on a real PlayStation. Real-console data is
transported through QR payloads, so a TV/capture card is enough and no
character-by-character transcription is required.

## Versioning

Every payload carries a **suite version** alongside the transport schema
version. The schema says how bytes are laid out; the suite version says what a
record id *means*, which a schema version cannot express because an id can be
redefined without the layout changing.

`hwtest-report.py` refuses to diff captures across a MAJOR suite bump, since the
same id may name two different measurements. Baselines are named by version
rather than date. The bump rule and the full history of what each version
changed are in [hardware-test-versions.md](hardware-test-versions.md).

Current: **v1.23**, schema PX8. Not comparable with v0.18 captures, whose timing
was sampled without interrupt masking.

## Test tiers

Tests are split by one rule: **can this run in an arbitrary order without
leaving hardware state behind?**

**Tier 1, the standing battery.** The 200 conformance cases, the CPU/GTE/SPU
scans, 179 timing records (CPU, GTE, DMA, CD, CD-DA contention, GPU fill rate,
MDEC, SIO, and the warm-harness performance probes) and 192 raw precision values including console identity and 22
bit-exact raster hashes. These run only when `RUN ALL TESTS + CAPTURE` is
selected, need no further controller input, and mirror every PX8 page to the
debug TTY. This is what `make hwtest-diff` gates and what a checked-in baseline
describes.

**Tier 2, bespoke probes.** PA2-PA5 and the controller probe. These own SPU
state or need a specific boot state, so they cannot be batched: PA5 must
snapshot untouched BIOS reverb state before SDK init, and its variants are one
per reboot. They arm when selected from the main menu
(`HardwareTests::enter_mode`), never at boot.

A tier-2 probe must never become the boot mode. The disc once booted straight
into PA5, whose `spu::init()` ran before every automatic capture, so the tier-1
payload described a console PA5 had already touched. Boot now goes to the
capture pages. Only a probe reached from a fresh boot sees a true BIOS handoff, so
PA5's variants still require a reboot each.

## Testing controllers and analog drift

Choose **CONTROLLER TEST (P1 + P2)** from the root menu. This is a friendly,
interactive diagnostic; the older **CONTROLLER SIO TIMING** entry under
TARGETED PROBES remains the low-level serial-handshake measurement.

The controller test polls both front-panel ports every frame. Each side of the
screen reports the connected controller mode and keeps a complete button
history:

- **Yellow** means the button is held now.
- **Green** means the button has been observed at least once.
- **Grey** means it has not yet been tested.

An analog controller shows both stick positions and all four raw axis bytes.
Release both sticks and hold them still: after a short settling period the disc
samples 90 frames and reports the largest distance from the ideal centre byte
`128`. A maximum of 8 is green, 9-16 is a yellow warning, and anything above 16
is a red drift failure. Moving either stick automatically clears the old result
and starts a fresh measurement when the stick rests again. Digital controllers
remain fully testable and show `STICKS / DRIFT N/A` rather than inventing analog
data.

START is part of the button test, so it does not immediately leave this screen.
Hold **START+SELECT** together for roughly three quarters of a second on either
controller to return to the main menu.

## Capturing and clearing stale BIOS reverb state (`PA5`)

PA5 is reachable from the main menu (TRIANGLE) and waits five seconds on a variant
selector (default
`DEPTH0`). PA4 SPLIT established that the real-console noise begins only when
the t0a0 map bank is uploaded, after SDK init and the light bank, with all
voice mixer volumes at zero. PA5 tests whether stale global reverb state is
reading the newly uploaded map data as its work buffer.

The probe captures SPUCNT/SPUSTAT, wet-output volumes, reverb base, external
input volumes, EON, a per-stage hash/nonzero count of all 32 reverb registers,
raw VBlank clocks, and map-bank readback. Crucially, `main()` snapshots the
untouched BIOS state before any SDK initialization; all 32 raw boot reverb
configuration words are included in the CRC-protected QR so the hardware state
can be replayed exactly in PSoXide.

Use Left/Right and Cross, and reboot the console between variants:

| Variant | Reset immediately before t0a0 map upload |
|---|---|
| `CONTROL` | No reverb changes; expected hardware noise reproduction |
| `DEPTH0` | Set only reverb output volume L/R to zero |
| `DEPTH2` | Set reverb output volume L/R to zero, then wait two VBlanks |
| `BASE0` | Set only the reverb work-area base to zero |
| `FULL0` | Zero wet volume, EON, external inputs, reverb routing/master, base, and all 32 config words |

Run DEPTH0 first. Then reboot and run CONTROL and FULL0. BASE0 and DEPTH2 are
follow-up discriminators. Keep OBS running from the calibration beep through
the final QR and note whether noise starts during stage 06 (`T0A0 MAP BANK
ONLY`). Decode the final payload with:

```sh
python3 tools/hwtest-audio-report.py /tmp/pa5-qr.txt
```

Press TRIANGLE from the completed PA5 screen to return to the menu.

### SCPH hardware result (2026-07-20)

The real-console `DEPTH0` capture isolated the fault. The retail BIOS handed
the executable a live full reverb preset (`SPUCNT=C085`, `SPUSTAT=0805`,
`vLOUT=5EBC/5EBC`, work-area base `E128`, `EON=00FFFFFF`, configuration hash
`F0417A52`). SDK initialization cleared reverb routing/master state but left
the wet-output depth, work-area base, and configuration registers intact. A
later map-bank DMA then replaced the data under that still-readable reverb work
area.

Setting only `vLOUT` to zero reduced the captured map-upload interval from the
PA4 SPLIT reproduction's `-30.07 dBFS` to `-86.82 dBFS`, a `56.75 dB`
suppression. The map-bank readback still matched (`25C971C5`) and the full
reverb register block remained otherwise unchanged. This proves that the noise
was stale BIOS reverb output, not a voice, CD audio, DMA corruption, or a bad
map-bank upload.

The historical firmware-boot regression recreated this measured handoff state.
External firmware boot was removed on 2026-09-15; the current built-in launch
path does not reproduce that firmware handoff. PSoXide's SPU continues reverb
reads, APF processing, and wet output while the reverb master-write bit is clear, matching the hardware probe; only feedback writes
are gated. The SDK now zeros both wet-output depth registers during `spu::init`,
before game banks can reuse SPU RAM. In the historical regression, the old PA5
executable reproduced the fault under CONTROL and was digitally silent under DEPTH0 in PSoXide, while a
newly linked Half-Life build remained digitally silent through its first eight
seconds under the same hostile handoff profile.

## Isolating the stale-menu-voice handoff (`PA4`)

PA4 is the second probe and waits five seconds on a variant selector (default SAFE2).
Its schema-v2 QR records the raw VBlank counter immediately before and after
each blocking transition, while the final stage retains the SPU RAM hashes.
Use Left/Right and Cross to start sooner. Reboot the console between variants:

| Variant | Transition under test |
|---|---|
| `BASELINE` | PA3 order: naturally ended voice 16, then init + 3050 + 3198 |
| `SAFE0` | Zero voice-16 volume and key it off immediately before the exact transition |
| `SAFE1` | Same explicit stop, then wait one hardware VBlank |
| `SAFE2` | Same explicit stop, then wait two hardware VBlanks |
| `SPLIT` | No pre-stop; observe separately after init, 3050, 3198, and readback |

Run SAFE2 first. If it stays quiet, reboot and capture SAFE0, then SAFE1 only
if SAFE0 is noisy. BASELINE is the PA3 reproduction and SPLIT identifies the
specific operation that starts the fault. Keep OBS running from the calibration
beep through the final QR for each run.

The menu marker starts 15 frames into stage 2 and naturally ends before frame
45, matching PA3. The selected shutdown and transition execute at frame 45.
Every blocking operation records actual elapsed VBlanks and realigns the engine
clock. The QR stores all 24 voices' nonzero-volume mask plus voice 16's complete
register state, ENDX, SPUCNT/SPUSTAT, bank dimensions, and readback hashes.

Decode each final `PA4/.../C:...` QR with:

```sh
python3 tools/hwtest-audio-report.py /tmp/pa4-qr.txt
```

Press TRIANGLE from the completed PA4 screen to return to the menu.

## Reproducing the Hazard Course bank transition (`PA3`)

PA3 remains as the second probe, an automatic six-stage reproduction of the
exact audio-bank lifecycle used when Hazard Course is selected. It uploads the
287,808-byte full menu bank (chunk 3000), starts a synthetic menu-accept sound
on voice 16, then executes the same `spu::init()` and replacement sequence as
the game: the 114,320-byte light profile (chunk 3050) followed by t0a0's
367,344-byte map bank (chunk 3198). The sample rates, decoded lengths, ADPCM
block counts, loop ownership, SPU addresses, and one-DMA-per-sample call shape
match hl-psx; all sound content is synthetic.

Stage 4 is a deliberate positive control: voices 0, 15, and 17 remain active
while their map bank is overwritten with loud marker data. It must sound bad.
Stage 5 recreates the same premise, silences and keys off every known map-bank
owner, then performs the same overwrite. It must become quiet apart from one
short completion marker. This proves the capture can distinguish a live-bank
overwrite from a safe handoff.

Every blocking upload records elapsed hardware VBlanks, then asks the engine to
discard fixed-update debt. Consequently, stage snapshots retain their intended
wall-clock position even when a real transfer spans several display frames.
The QR also stores ENDX, current envelope/start address for voices 0/15/16/17,
and a diagnostic 64-byte stable-mode SPU readback hash.

Record the complete run without changing capture volume. Leave the final QR
visible for several seconds, decode its `PA3/.../C:...` text, then run:

```sh
python3 tools/hwtest-audio-report.py /tmp/pa3-qr.txt
```

Press TRIANGLE from the completed PA3 screen to return to the menu.

## Capturing hl-psx voice-bank audio (`PA2`)

PA2 remains as the second automatic voice probe. Record the complete run
without changing the capture volume. Stage 0 emits a half-second
calibration marker. PA2 then reconstructs sanitized banks with the exact
Hazard Course resident/per-map sample counts, rates, block lengths, and upload
order: 68 core samples plus 26 map samples, roughly 508 KiB of back-to-back SPU
DMA traffic. PA2 used chunk 3051 rather than the first-map chunk 3050, so PA3
is the authoritative reproduction of the original recording's startup path.
No Half-Life audio is stored in either fixture.

Production upload and playback run first; a comparison upload that waits for
SPUSTAT's delayed transfer-mode mirror and gives the final FIFO words time to
drain runs second. In both cases voice 15's target contains a short marker,
then silence, and a proper one-shot end block. The following allocation is a
loud overrun guard. Hearing a late tone during either `END + GUARD` stage proves
that the target's end block did not stop the voice.

Leave the final QR visible for several seconds. Decode its `PA2/.../C:...` text
from any clear OBS frame and run:

```sh
python3 tools/hwtest-audio-report.py /tmp/pa2-qr.txt
```

The report preserves SPUCNT/SPUSTAT, all voice-15 registers, ENDX, maximum
mode/drain poll counts, and expected/observed last-64-byte hashes for all six
stages. SPU DMA readback is intentionally diagnostic rather than a pass/fail
oracle; compare its hash with ENDX and the external OBS waveform. Press Down
once from the completed PA2 screen to reach the older PA1 CD-route probe.

## Capturing timing data

1. Build and burn `build/examples/mipsel-sony-psx/release/hardware-tests.bin`
   with its matching `.cue`.
2. Let the controller-probe page settle, then press **Start** to jump directly
   to `TIMING MAP`.
3. Scan QR page `01/03`, press Right for `02/03`, then once more for `03/03`.
   Save or copy the complete `PX6/.../C:...` text returned by each scan.
4. If a value looks unstable, press Cross once to run five fresh samples and
   scan all three pages from the new run. Do not mix pages from different
   runs.

## Operator flow

The disc boots side-effect free into its main menu. Nothing measures or changes
hardware state until the operator chooses an entry. Menus fit without scrolling:

   | Root | Contains |
   |---|---|
   | `RUN ALL TESTS + CAPTURE` | Runs the standing conformance battery and builds the PX8 conformance capture: verdicts, and one record per failing case |
   | `FULL CHARACTERISATION CAPTURE` | The same run, but the capture also carries timing envelopes, precision values and the register snapshot. Use when establishing a reference, not for a routine check |
   | `CONTROLLER TEST (P1 + P2)` | Live two-port button, stick and analog-drift diagnostic |
   | `MEMORY CARD (AT OWN RISK)` | Card diagnostic behind a consent screen: it has had limited testing on real hardware and reads and writes the operator's card, so corruption cannot be ruled out. CIRCLE accepts the risk before any card traffic happens; writes additionally require the L1+R1+CROSS chord |
   | `VIEW CAPTURE (QR PAGES)` | Back to the QR symbols the last capture produced |
   | `RESULTS BY SECTION` | All checks, then CPU/RAM/IRQ/DMA/TIMERS/GPU/GTE/SPU/CDROM/SIO |
   | `HARDWARE SCANS` | CPU sweep, GTE sweep, SPU register map |
   | `TARGETED PROBES` | SB1/SB2/SB4 SPU probes, controller SIO timing, CD-chain and PA1-PA5 audio probes, `PERF SWEEP (SAFE)` and `PERF A/B (MAY HANG)` |
   | `VIDEO LEVELS (TV/CAPTURE)` | Grey ramp and flat fields for display-chain checks |
   | `AUDIO READOUT` | Steps the tone off / through each rate, showing its state inline |
   | `RESUME FROM TEST` | Restarts a long battery after a selected test index |

Up/Down moves, Cross runs, and START backs out one level. During the standing
battery a progress bar names the in-flight case; after it completes, the capture
pages appear and Left/Right pages through them.

After a capture exists, the `AUDIO READOUT` menu row or SQUARE from another
screen steps through the available rates and off. The menu shows whether the
payload is ready and which rate is active.

### Recording a capture

Enable `AUDIO READOUT` after the capture finishes if audio transport is wanted.
One unreadable QR symbol costs the whole visual capture because the payload is
only valid complete; audio provides an independent recovery route.

**Record at least three repetitions of the tone**, about 45 seconds. OBS encodes
audio as AAC by default, and AAC is lossy: a link recorded through it does NOT
decode from any single repetition. It decodes reliably from three via the
per-bit majority vote, which is verified. Recording PCM/lossless instead removes
the need, but three repetitions is the cheaper habit.

**A recording usually spans several runs.** A reboot or `RUN ALL TESTS + CAPTURE`
produces different measurements under the same page numbers, so pages from
different runs cannot be combined; `tools/hwtest-video-qr.py` resolves this by
requiring the whole-binary CRC to check out.


Record video **and** audio from before power-on. Let the bar finish. Scan the
five pages. Then keep recording at least 15 seconds: one
repetition of the audio payload is ~13.6 s. If it will not decode, press SQUARE
again for a slower, more robust rate; the decoder detects which was used.

Expect the battery to take noticeably longer than it used to. It does around 40
real seeks plus the GPU, MDEC and SIO work.

## Timing records move when the guest binary changes

Adding unrelated code shifts I-cache alignment, which genuinely changes measured
cycle counts. A rebuild therefore drifts the timing baseline even when no
measurement logic changed at all.

`make hwtest-verify-code` is what separates the two cases: it digests the
instructions between each probe's markers, so `drift=0` there while
`hwtest-diff` reports drift means the measured blocks are byte-identical and only
their addresses moved. That is a re-baseline, not a regression.

The mechanism, measured on 2026-09-17: a commit of clippy fixes that touched no
probe moved 104 of the 151 timing minima in the emulator, by up to 40 cycles
(`multu_mflo_small` 126 -> 151, `nop_block` 211 -> 196, `divu_mflo` 317 ->
302). Each record takes five samples, and between samples the scan heartbeat
and a pad poll run. That code shares I-cache lines with the probe (the image is
far larger than the 4 KiB direct-mapped cache), so every sample starts with a
few of its own lines evicted and pays a refill for each. How many depends on
where the linker placed both. The minimum across five samples does not escape
it because all five are equally cold. This is also the likely reason the two
full silicon captures disagree on `multu_mflo_small` (126 on v1.7, 140 on
v1.17) while agreeing on the medium and large bands.

Records `0x01`-`0x0F` therefore measure instruction cost plus a
layout-dependent refill tax, and they are not comparable across suite builds.
Their warm twins (next section) are.

## Performance probes (records `0x72`-`0x8D`, and `0xDC`-`0xEC` on request)

`src/perf_probes.rs`. These exist to price CPU-side optimisation techniques on
silicon and to calibrate the emulator's cycle model against numbers that do not
depend on link layout.

**The warm harness.** Every probe here runs its timed block twice inside one
assembly block and reports the second pass, and the block starts on a
cache-line boundary. Every line it executes is resident by the time it is
timed, so the result is a property of the instructions alone. In the emulator
these records have zero jitter where their older twins show 70-90 cycles.
`hwtest-report.py --layout-immune-timing-only` counts timing drift only for
these ids, which is the useful gate for a refactor that moves code.

| Id | Record | Question |
|---|---|---|
| `72`-`78` | warm twins of `01` nops, `02` ALU, `03` cached RAM load, `09` scratchpad load, `0B` RAM store, `0C` scratchpad store, `04` taken branch | what those instructions cost with no refill tax |
| `79`-`84` | `multu; k nops; mflo` x16 at the three `rs` magnitude bands, for k = 0 and k = m-1, m, m+1 around each band's documented latency m (6, 9, 13) | how many independent instructions fit behind a multiply before the `mflo` interlock stall is gone. Flat up to a knee, then +16 per extra nop; the knee is the real latency |
| `85`-`89` | `divu; k nops; mflo` x8 for k = 0, 34, 36, 38, 40 | the same for the divider (documented 36) |
| `8A` | `multu` with a small `rs` against a large `rt` | whether the band is chosen by `rs` alone |
| `8B` | signed `mult` with `rs` = -5 | whether a small negative `rs` takes the fast band |
| `8C`, `8D` | 32 alternating calls to two one-line leaves, exactly 4 KiB apart versus on neighbouring lines | the cost of a direct-mapped I-cache conflict per call. The wrapper runs through KSEG1 so it cannot disturb the lines it measures |

Emulator values at v1.21: the knees land exactly on the modelled latencies
(`7A`/`7B` = 126, `7C` = 142; `86`/`87` = 302, `88` = 318), and the alias pair
costs 1362 against 1036, about five cycles per conflicting call.

**The register A/B group** answers whether two undocumented-in-practice
registers buy anything:

* `RAM_SIZE` (`0x1F801060`) bit 7, which psx-spx describes as "delay on
  simultaneous CODE+DATA fetch from RAM". `DC`/`DD` run 64 RAM loads through
  their KSEG1 alias, so every instruction fetch is a RAM access coinciding with
  a RAM data access, with the bit as found and flipped. `DE`/`DF` are the same
  loads from KSEG0, warm, as the control pair.
* Cache control (`0xFFFE0130`) bits 13-17, which psx-spx labels only
  "supposedly" (read priority, no wait state, bus grant, load scheduling, no
  streaming). Each is flipped alone around 64 cached RAM loads and around a
  cold sweep of all 256 cache lines. `EA` sets the refill size to two words, a
  bit the emulator does model, as a cross-check that the flip and the restore
  both take effect. Bit 12 (interrupt polarity) has no performance reading and
  is left alone; bus grant runs last.

Each pair goes through byte-identical code: one assembly block reads the
register, makes an untimed call in the normal state, writes `value ^ mask`,
times the call, and restores, all reached through KSEG1 with interrupts masked.
Mask 0 is the control. No other code ever runs in the flipped state.

A wrong guess about one of these bits can hang a console, so the group never
runs in the standing battery or the headless conformance capture. It runs from
`TARGETED PROBES > PERF A/B (MAY HANG)`, which takes only the performance
probes (no CD, GPU, MDEC or SIO batteries, so a power cycle costs about a
minute) and then shows a full capture. The record id is on screen while each
record runs, so a hang names its bit. The emulator models none of these bits
except the refill size: headless, every flipped record equals its control, and
`make hwtest-capture-perf` only proves the path runs and restores. The numbers
come from a console.

The memory-control block now ends with the `RAM_SIZE` and cache-control values
(eleven registers instead of nine), so a capture records what the BIOS left in
both.

### v1.23: extended ids, GPU batches done properly, and the open shapes

Record ids are sixteen bits in the guest. `00`-`FE` travel as before; ids of
`100` and up go in a new PX8 block, `TIMING_EXT` (flag bit 6, last in the
payload: a u16 count, then id/min/median/max as four u16). The in-flight id is
drawn as sixteen cells during a scan.

`100`-`114`, GPU batches (`src/gpu_probes.rs`). The v1.22 console run showed
the fill battery's method is broken: it writes to GP0 unpaced, overflowing the
16-word FIFO, ends on GPUSTAT bit 26, which pulses between primitives, and
samples mostly transparent texels. These batches are a linked list of one
packet per primitive handed to DMA, ending in a GP0(1Fh) interrupt request,
which the GPU takes in order with the drawing; Timer 2 runs from the DMA kick
to GPUSTAT bit 24. Textures and CLUTs have no zero entry. Flat, Gouraud,
dithered, textured, raw, translucent and Gouraud-textured triangles; flat, 4bpp
and 8bpp rects; an 8bpp rect with a CLUT change on each; a texture page change
on each; a wide UV span; clipped-away triangles; a letterboxed batch; VRAM
fill and copy; and 64 two-pixel triangles of three kinds for setup cost. The
emulator has no draw-time model and reads about 20 for all of them. The
standing `A0`-`AF` records are unchanged and should not be trusted; the v1.22
sweep's `BA`-`BF`, `3B`, `38`, `CB` and `F6` are retired.

`120`-`136`, the shapes v1.22 left open: the multiply interlock at k = 1 to 4
and the divide at k = 35; a load followed by 2, 3, 6 and 8 independent
instructions; a store followed by 1 and 2, bursts of 2, 4 and 8 stores, and a
GP0 store followed by 3; RTPS followed at once by a read of SXY2, MAC0, MAC1
or IR1 and then 20 nops, with a control that reads after RTPS has finished
(22 cycles a turn if the read is free, about 36 if it waits); and the alias
pair called from a cached wrapper that cannot share a line with either leaf.

What the v1.22 captures found, and what went into the emulator, is in
[emulator-accuracy-from-silicon.md](emulator-accuracy-from-silicon.md).

### The performance sweep (v1.22)

`TARGETED PROBES > PERF SWEEP (SAFE)` runs the records above plus the ones
below and nothing else, then shows a full capture (four pages). `PERF A/B (MAY
HANG)` is the same sweep followed by the register A/B group. Neither runs the
conformance battery, so conformance cases read as pending in these captures.
The sweep exists because every record id from `00` to `FE` is now taken and the
standing battery's full capture was already at five pages.

| Id | Record | What it prices |
|---|---|---|
| `1E` | the warm nop block through KSEG1 | an instruction executed uncached, which is also what thrashing degenerates to |
| `37`, `39`, `C8`-`CA`, `CC`, `CD`, `CF` | byte and KSEG1 stores; loads with no nop between them; byte loads; `lwl`/`lwr` and `swl`/`swr` unaligned pairs; sixteen sequential addresses | the data-access shapes the compiler emits. The emulator charges an unaligned word 18 cycles against 8 aligned, and LLVM emits thousands of them |
| `1F`, `CE` | a store then three independent instructions (against `76`); a load then four (against `74`) | the four-entry write queue and LSI's load scheduling. Sony's notes and nugget's kernels say the store becomes free and the load hides part of its wait; the emulator has neither and reads both as purely additive |
| `27`-`2F` | warm latency of RTPS, RTPT, NCLIP, MVMVA, AVSZ3, SQR, OP, GPF, NCDS, issued back to back | the GTE's latency table, which until now had six cold, layout-dependent silicon points. Every GTE record carries 48 trailing nops (subtract them) so the timed pass starts with the GTE idle |
| `3A` | RTPT followed by the six `mtc2` that load the next triple (against `28`) | psx-spx's GTE pipeline page says inputs are latched within about four cycles and `mtc2` never stalls, so the next vertices can be loaded behind the current command |
| `ED`-`F2` | RTPT then 21/23/25 nops, RTPS then 13/15/17 nops, before the next command | how much CPU work fits behind a GTE command for free. Knee at the latency |
| `F3`-`F5` | sixteen `mtc2`, `ctc2`, `mfc2` | the coprocessor register moves every vertex pays for |
| `F7`, `F8` | one three-component Q12 lerp with `mult` and with GPF | the same work on each side, including the register traffic |
| `8E`, `8F` | sixteen `multu` with no read between them, then one `mflo` | whether a multiply issued behind a running one waits for it. The emulator says it costs nothing |
| `F9`-`FD` | GPUSTAT read, GP0 write (a GPU nop), I_STAT read, SPU halfword read and write | what an I/O port costs in an inner loop. The emulator has an SPU read at 27 cycles |
| `33`, `34` | the DMA controller walking 256 and 1024 empty packets | what an unused ordering-table slot costs per frame |
| `35`, `36` | 128 nops with GPU DMA idle, and started right after kicking a 512-node list | whether the CPU runs while the list is walked. Equal means it does |
| `9F`, `FE` | the same pair with 64 RAM loads in place of the nops | psx-spx and Sony both say the CPU runs during DMA only until it needs the bus. The emulator lets it run freely |
| `BA`-`BF` (with `A0`, `A2`, `AE`, `AF` retaken) | raw-texture and translucent textured triangles, Gouraud-textured triangles, triangles clipped away entirely, VRAM fill and VRAM copy | GPU cases the fill battery left out, each next to its reference |
| `3B`, `38` | 64 two-pixel triangles, flat-textured and Gouraud-textured | per-triangle setup with nothing to fill. nocash: 100 against 250 cycles |
| `CB` | `AF`'s 8bpp rects with a different CLUT on every other one | the CLUT cache reload. nocash: 256 cycles each |
| `F6` | `A2` with the vertical display range collapsed to one line | whether the GPU renders faster when it is not fetching the picture. The screen blanks for the few milliseconds this takes |
| `3C`, `3D` (A/B run) | `RAM_SIZE` bit 7 around a cold sweep of 4 KiB of code that also loads data | the realistic case for that bit: line refills and data reads contending for RAM |
| `3E`, `3F` (A/B run) | the SPU bus read-delay nibble as found and shortened, around 64 SPU status reads | whether SPU register traffic can be made cheaper. The emulator models this one: 3489 to 2229 |

Where these claims come from, and what the engine does about each today, is in
[ps1-performance-research-2026-09-17.md](ps1-performance-research-2026-09-17.md).

`make hwtest-diff-perf` gates the whole A/B capture headless against
`px8-emulator-perf-v<version>.txt`, counting timing drift only for warm
records, so it should survive unrelated guest edits.

**Folding a console capture back in.** For each finding, change the emulator in
the PSoXide-emulator repository and bump `components.lock.json` here; nothing
under `emu/crates/emulator-core` is editable in this tree.

1. Gap sweep knees: `mult_cycles` and `DIV_CYCLES` in `cpu.rs`. A knee one nop
   later than modelled means the latency is one cycle longer.
2. `8A`/`8B`: the `rs`-only, sign-folded magnitude rule in `mult_cycles`.
3. Warm twins: the RAM, scratchpad and branch costs in
   `bus/memory_timing.rs`, which were fitted to the layout-dependent records.
4. `8C` against `8D`: the refill cost per line in `icache_fill_stalls`.
5. `DD` against `DC`: if the uncached pair differs and the cached pair does
   not, `RAM_SIZE` bit 7 is real. Model it in the fetch path. Do NOT clear it
   at boot: psx-spx says that hangs CD loading on PU-8 boards.
6. Any cache-control pair that differs: model the bit, then measure the games.
7. GTE latencies and gap knees: the table in `psx-gte-core/src/state.rs`.
8. `8E`/`8F`: `hilo_busy_until` is overwritten by a second multiply today. If
   silicon queues them, that is a stall the emulator does not charge.
9. `36` against `35`, and `33`/`34`: linked-list DMA cost per node and whether
   it holds the CPU off the bus.
10. `BA`-`BF` and the fill battery: the GPU draw-time model, which does not
    exist yet. nocash's measured rendering timings are the prior.
11. `1F` and `CE`: the write queue and load scheduling, neither of which the
    emulator has. With a third of every Cortex vblank in RAM stalls, these
    two move whole-frame numbers more than anything else here.
12. `E2`-`EC`: LSI documents NOPAD, LDSCH and RDPRI, and the BIOS value has
    all three in their fast setting, so expect each flip to slow its workload
    rather than speed it up.

## Performance-lever gates (cases `0xC8`-`0xD2`, records `137`-`13A`, v1.24)

Three levers measured well headless and each rests on something only silicon
can answer. The cases sit at the end of the conformance battery (indices
200-210), so RUN ALL TESTS and FULL CHARACTERISATION both run them, and RESUME
FROM TEST at 200 runs just these plus the timing scan. Code in
`src/lever_probes.rs`. INFO values and passing observations travel only in the
FULL CHARACTERISATION capture; a conformance capture carries a case's numbers
only when it fails.

**GTE vs IRQ** (gates psx-rt's handler under any IRQ-heavy present path).
psx-rt's exception handler returns to EPC. psx-spx documents that an interrupt
taken on a GTE command lets the command run and leaves EPC pointing at it, so
returning to EPC runs it twice; its fix is to step EPC over a GTE command. The
emulator never takes an interrupt in front of a GTE command
(`should_take_interrupt`), so headless both variants are clean by construction.
The probe loops `mtc2` x3 to SXY0-2 (sentinels outside RTPS's output range),
RTPS, 20 nops, and reads SXY0: one RTPS leaves the old SXY1, two leave the old
SXY2, none leaves SXY0. Timer 2 interrupts at 331, 457, 613 and 797 clocks
(8192 iterations each) on top of VBlank, under a probe-owned handler that
acknowledges, counts the interrupts whose EPC held a GTE command, and returns
to EPC or EPC + 4.

| Case | Status | Expected | Observed |
|---|---|---|---|
| `0xC8` return to EPC: exposure | INFO | interrupts taken | interrupts whose EPC held a GTE command |
| `0xC9` return to EPC: RTPS intact | PASS if none corrupted | interrupts on a GTE command | doubled (bits 0-15), lost or unrecognised (16-31) |
| `0xCA` skip GTE at EPC: exposure | INFO | interrupts taken | commands stepped over |
| `0xCB` skip GTE at EPC: RTPS intact | PASS if none corrupted | commands stepped over | as `0xC9` |

On silicon, `0xC9` failing with doubled close to its expected value says the
hazard is real at that rate; `0xCB` passing with a non-zero expected says the
fix works. `0xCB` failing with a non-zero high half says the command had not
run when the handler skipped it, i.e. the fix loses commands.

**Present queue** (gates quake-psx a29ca4d's `present-queue`). The prototype's
VBlank handler, copied instruction for instruction and chained to psx-rt's the
same way, runs 120 frames. Each frame's DMA chain sets its draw environment,
clears its buffer, draws 0 to 19 screen-sized Gouraud triangles (so some frames
outlast a VBlank and some edges find the GPU busy), a white bar that moves 16
pixels a frame, a marker, and ends in GP0(1Fh). Three instrumentation steps are
added to the handler: it counts edges, records GPUSTAT and Timer 1 when it
decides to flip, and acknowledges the GPU interrupt before the kick so each
chain's GP0(1Fh) raises GPUSTAT bit 24 afresh. A flip made while bit 24 is
clear exposes a frame the GPU has not finished: the tear this lever must not
cause.

| Case | Status | Expected | Observed |
|---|---|---|---|
| `0xCC` frames kicked and drawn | PASS if all 120 kicked, no timeout, both markers read back | 120 | kicks (bits 0-15), timeouts (16-23), bad markers (24-31) |
| `0xCD` bit 28 idle means drawn | PASS if no early flip | flips checked (119) | flips made before the previous chain's GP0(1Fh) |
| `0xCE` flip lines after VBlank | INFO | largest Timer 1 value seen (lines a frame) | latest flip line (bits 0-15), earliest (16-31) |
| `0xCF` busy edges skipped | INFO | VBlank edges seen | edges that found the GPU or channel 2 busy |

Timer 1 runs from HBlank with sync mode 1 (reset at VBlank). `0xCE` is a
measurement: where that reset sits relative to the VBlank IRQ differs between
console models, and the emulator reads timers without catching them up (see
"Timers" in [emulator-accuracy-from-silicon.md](emulator-accuracy-from-silicon.md)),
so its value there is not a beam position. On silicon a spread of a few lines
between earliest and latest says every flip landed at the same beam position.
The moving bar is for the camera: a torn flip breaks it horizontally.

**Scratchpad stack** (gates the SDK's `ScratchpadStack`, PSoXide 3e939cd54).
The editor pins an SDK from before it, so psx-rt's trampoline is vendored
(`__hwtest_call_on_stack`, without the panic bookkeeping). hello-spstack's
workload (three call levels, each with an array in its frame, reading a
256-byte table in the scratchpad) runs 32 rounds on the RAM stack, then 32 on
a stack in scratchpad bytes 256-1024, each round after a different spin so the
interrupts land at different points. Throughout both runs: Timer 2 interrupts
every 1531 clocks plus VBlank (the GTE probe's handler, in skip mode; it uses
only `$k0`/`$k1`), and between rounds a 2048-node linked list re-kicked on
channel 2, a 4 KiB SPU upload re-kicked on channel 4 (sound RAM 0x60000), the
SDK's SectorReader streaming the CDTEST region, and a pad poll. CD data moves
by PIO, as SectorReader does since a1e95d30: chopping CD DMA on channel 3 can
latch busy for good on the project console, which would take the timing scan's
CD records with it, and no DMA channel can reach the scratchpad anyway.

| Case | Status | Expected | Observed |
|---|---|---|---|
| `0xD0` checksum vs RAM stack | PASS if equal (and every round equal) | RAM-stack checksum | scratchpad-stack checksum |
| `0xD1` IRQs taken, all intact | PASS if no flag and at least 64 interrupts on the stack | 64 | interrupts taken with `$sp` in the scratchpad (bits 0-14), deepest stack use in bytes (15-26), flags (27-31: table changed, region bottom word changed, `$sp` not restored, caller's RAM frame changed, an interrupt saw the scratchpad during the RAM run) |
| `0xD2` background activity | INFO | 64 (rounds) | channel 2 kicks (bits 0-7), channel 4 kicks (8-15), CD sectors (16-23), pad polls (24-31), each saturating |

Records `137`-`13A` time one `level2` call (16 `level3` calls) with Timer 2,
inside the called function so both stacks run identical timed code: RAM stack,
scratchpad stack, then both again while channel 2 walks the 2048-node empty
list. Warm records `74`/`75` already price a bare RAM load against a
scratchpad one (514 against 126 for 64 on the v1.23 console sweep, about six
clocks a load). In the v1.24 build the timed call makes 515 stack loads and 899
stack stores (31 loads and 55 stores per `level3`, counted from the
disassembly, plus 19 of each in `level2`), so six clocks a load predicts about
3,100 clocks between `137` and `138`.

Emulator (frozen frontend, PSoXide-editor a03b8fa9): `137` 16,064, `138`
12,556, `139` 16,065, `13A` 12,517. The emulator does not model RAM loads
slowing during a linked-list DMA (record `FE`), so `139` equals `137` there;
silicon is expected to differ.

## CD-DA contention (records `0x9B`-`0x9E`)

The disc carries a synthetic CD-DA track (track 2, generated by
`tools/gen-cdda-tone.py`, 440 Hz left / 660 Hz right) specifically so
read-while-audio contention can be measured. This is the one CD failure no
emulator reproduces, and it cannot be probed without a real audio track, which
is why the track is part of the disc build rather than optional.

`0x9B` against `0x9C` is the whole measurement: the same read path, the same
sector count, the only difference being whether audio was live. The emulator
reports 6731 against 6730 HBlank ticks, i.e. **no contention at all**. Distinct
channel frequencies mean the same recording also proves stereo routing and lets
a dropout be heard, not just measured.

## GPU fill rate (records `0xA0`-`0xAF`)

The emulator models no GPU draw time whatsoever, so there is no data anywhere to
build a model from and the console is the only possible instrument. Pixel counts
are held identical across shading modes, so a difference isolates interpolation,
blending or dither cost rather than the per-pixel floor. `0xAA` against `0xAB`
holds total pixels constant while changing primitive count, separating setup
cost from fill cost. `0xAC` against `0xAD` holds pixels constant while changing
UV footprint, which is the texture-cache probe.

Current emulator readings show gouraud within 1% of flat and dithered identical
to undithered, and `0xAC`/`0xAD` identical, all consistent with none of these
costs being modelled. Every one is a claim console data will settle.

Everything draws into off-screen VRAM at y >= 400, so neither the photographed
capture nor the QR pages are disturbed. Sizes stay well under one Timer 2 wrap
(65,535 cycles): at roughly a pixel per cycle a 256x256 fill would sit on the
boundary and alias a slow result into a fast one.

## MDEC (records `0xB0`-`0xB3`)

Table uploads and reset settle. Two gotchas cost real debugging time and are
worth recording: **busy is status bit 29, not bit 31** (bit 31 is the data-out
FIFO flag and stays set while idle, so polling it never returns), and a command
holds busy until it has consumed *exactly* the number of payload words it
implies (16 for luma-only quant, 32 for luma+chroma, 32 for scale).

Decode-to-drained is measured for one and two macroblocks (`0xB4`, `0xB5`),
using minimal but valid blocks: a head halfword carrying quant scale and a
signed 10-bit DC, then `0xFE00`, whose run-length field of 63 overflows the
coefficient index and terminates the block.

Draining is part of the measurement, not overhead. It is also where the two
platforms disagree, so the wait accepts **either** termination: real hardware
clears busy once the decode finishes and its output is read, whereas PSoXide
only re-evaluates busy when the last parameter word arrives (output is already
queued by then), so busy stays set forever and a busy-only wait never returns.
Draining the expected pixel count terminates in emulation; the busy check
terminates, sooner, on silicon.

## SIO / pad (records `0xB6`-`0xB9`)

The same pad poll at four setup/inter-byte pacings. The spread is the signal: it
shows how much of a transfer is fixed cost and how much is pacing the pad
demands. The SCPH-1200 setup-delay hunt cost a whole session; this makes it a
standing measurement. `0xFFFF` means the pad did not answer, which is distinct
from a fast poll.

## Console identity and raster hashes (precision `128`-`159`)

A capture that cannot say which console and BIOS produced it is much harder to
trust later, and previously that was encoded only in a hand-written filename.
Two BIOS regions are sampled (build date at `0x100`, version string at
`0x7FF32`) as a deliberate hedge: these reads return zero on the emulator's
side-loaded HLE path, which maps no BIOS ROM, so they **cannot be validated
before a burn**. If one region reads zero on console the other still identifies
the machine.

Then 22 bit-exact raster hashes. A 32-bit hash covers a whole 96x96 VRAM region,
making this the cheapest coverage per payload byte in the schema, and it is what
originally caught the triangle rasterizer being Redux-shaped rather than
silicon. Timing says how long a primitive took; only a hash says it drew the
right pixels. `raster_hash_00` currently reads `0x04121005`, matching the
recorded silicon hash the emulator's own rasterizer test asserts.

## CD-ROM battery (records `0x90`-`0x9A`)

The drive is the one subsystem where the console is the only usable
instrument: seek time is mechanical and no emulator models head travel.

Every record here is timed on **Timer 1's HBlank clock** (~63.9 us per tick,
~4.19 s of range), not Timer 2's system clock, which wraps after ~1.9 ms and
could not express a seek at all. Seek and read acknowledge immediately and
finish much later, so these wait for the drive's SECOND response (the
completion IRQ) via `cdrom::try_command_until_complete`. Timing to the ack
would report a mechanical seek as microseconds.

| ID | Measurement |
|---:|---|
| `90`-`93` | SeekL over 1, 16, 128 and 512 sectors from a parked origin |
| `94`-`95` | 8 sequential sectors, single and double speed |
| `96`-`98` | GetStat, SetMode and GetLocP response |
| `99`-`9A` | Pause and Init, to completion |

Each seek re-parks the head first, so a record measures a known distance
rather than travel from wherever the previous record happened to finish.
Throughput discards the first sector, which carries the seek and spin-up
settle, and so reports sustained rate rather than first-byte latency.
`0xFFFF` means the command did not complete within its poll budget, which is
distinct from a zero-time result.

The emulator currently reports the same 845 HBlank ticks for a 128-sector and
a 512-sector seek, so its seek time is distance-independent. Single and double
speed differ by exactly 2.0x. Both are exactly the kind of claim a console
capture can confirm or demolish.

## Payload schema `PX8`

PX8 supersedes PX7: every block after the header is optional, behind a flags
byte, and pages are counted from the payload rather than fixed per schema.

PX7 always carried everything. Timing envelopes and precision values are 71% of
that payload and they are *characterisation*: there is no expected value to
check them against, silicon is the reference. A routine run does not need them,
and as the suite grows towards covering every chip, shipping them on every
capture is what limits how many cases can be added.

So a **conformance** capture carries the status bitmap and one record per
FAILING case -- 163 bytes and a single QR at 200 cases, and still one QR at a
thousand, because a passing case costs three bits and nothing else. A
**characterisation** capture carries the lot, in PX7's field order, so an
archived `px7-*` reference still describes the same thing a full PX8 does.

A failure record names its case by `TestSpec::id`, not by array position, so a
capture archived today still points at the same test after the array grows. The
ids are checked for uniqueness at compile time.

PX7 itself superseded PX6: four pages instead of three, a median column beside
each min/max, explicit per-record ids, and 128 record slots. Since v1.21 a PX8
timing block carries only the records that ran, and the header count says how
many that is; unfilled slots used to cost seven bytes each on the wire.

The median is not decoration. With five samples reduced to two numbers, a
min/max gap cannot distinguish one stray interrupt from a genuinely bimodal
distribution. Records now routinely read `min=126, median=126, max=286`, which
says plainly that the maximum is a single cold-I-cache outlier and the typical
cost is the minimum.

Samples are now taken with **interrupts masked** (`IrqGuard`, restored on
drop). Previously the only defence against a VBlank or CD IRQ landing inside a
measured window was that one of the five repeats happened to escape it, so the
min/max gap reported "did an interrupt hit" rather than silicon jitter.

Record ids are written into the payload rather than implied by position, so an
unused slot is skippable and adding a probe cannot silently shift the meaning
of every later record. `0xFF` marks an unfilled slot; `0x00` could not, being
the real empty-harness record.

Note that two work sizes per probe were considered and rejected: every probe
shares the identical harness prologue and epilogue with record `0x00`, so
`(t_probe - t_empty) / N` already cancels the harness exactly.

## Payload schema `PX6` (superseded)

The capture is a 1,733-byte versioned binary record encoded as exactly 2,312
Base64 characters. Its three pages carry 828, 828, and 656 characters. Each
screen encodes its complete `PX6/<page>/<chunk>/C:<crc>` text in the proven
Version-20-L QR geometry. A CRC-32 protects each page and a second CRC-32
protects the reconstructed binary record.

The record preserves all 173 conformance observations and their statuses, all
90 timing min/max pairs, CPU/GTE/SPU startup-scan summaries, the nine
memory-control registers, run IDs, section digests, and 128 raw precision
values. Fixed schema ordering avoids repeating labels and IDs on screen.
`tools/hwtest-report.py` accepts all three strings mirrored to the debug TTY
and reconstructs the complete report. It remains backward-compatible with
two-page PX5 captures.

The third page is intentionally a microscope rather than a duplicate. It
stores raw SPU DMA readback words for one-block and four-block BCR shapes plus
Stop/DMA-read mode poll counts and forced-stable comparison hashes, repeated
GPUSTAT reads after IRQ and DMA-direction writes, isolated Timer 2
mode/counter/I_STAT snapshots, raw SPU voice-register masks, consecutive OTC
CHCR reads, an exact NCLIP scene-A settle sweep from 47 through 64 NOPs, and an
immediate-versus-settled OP comparison. This extra data reveals corruption and
state-transition shapes that a checksum or one-shot pass/fail result cannot.

The timing minimum is normally the least-interrupted measurement; the min/max
gap records IRQ or hardware jitter instead of hiding it. BIOS revisions can
legitimately program different bus values, while the full GTE observations can
distinguish a true latency window from magnitude-dependent partial accumulation.

Record IDs:

| ID | Measurement | Work field |
|---:|---|---|
| `00` | Empty Timer 2 measurement harness | zero |
| `01` | Literal NOP block | instructions |
| `02` | Dependent `ADDIU` block | instructions |
| `03` | Cached `LW` plus explicit load-hazard slot | loads |
| `04` | Taken branch plus delay slot | branches |
| `05`–`07` | `MULTU`/`MFLO`, fixed small, medium, and large `rs` magnitudes | pairs |
| `08` | `DIVU` followed immediately by `MFLO` | pairs |
| `09`–`0A` | Scratchpad and uncached-RAM `LW` plus hazard slot | loads |
| `0B`–`0D` | Cached RAM, scratchpad, and uncached-RAM stores | stores |
| `0E`–`0F` | GPUSTAT and I_STAT reads plus hazard slot | reads |
| `10`–`1B` | Volatile spin at system, system/8, and dot clocks | iterations |
| `1C`–`1D` | Cold (flushed) and warm execution of one 4 KiB I-cache footprint | instructions |
| `20` | Timer 1 HBlank ticks during the long spin | `FFFF` sentinel |
| `21`–`26` | RTPS, RTPT, NCLIP, MVMVA, NCDT, and NCCT throughput | commands |
| `30`–`32` | 16-, 64-, and 256-word OTC DMA completion | words |
| `40` | End-to-end CD-ROM GetStat first-response latency | commands |
| `41` | GP0 IRQ1 command-to-GPUSTAT settle latency | commands |
| `42`–`44` | Cold minimal return target entered at cache-line word 0, 1, or 2 | calls |
| `45` | Warm minimal return target entered at cache-line word 0 | calls |
| `46` | Untaken branch plus delay slot | branches |
| `47`–`48` | Cached-RAM byte and halfword loads plus explicit load-hazard slot | loads |
| `49`–`4A` | Uncached-RAM byte and halfword loads plus explicit load-hazard slot | loads |
| `4B`–`4D` | BIOS-ROM word, halfword, and byte loads plus explicit load-hazard slot | loads |
| `4E` | SPU status halfword reads plus explicit load-hazard slot | reads |
| `4F` | SIO status word reads plus explicit load-hazard slot | reads |
| `50`–`51` | Cached-RAM byte and halfword stores | stores |
| `52`–`54` | CD-ROM byte, halfword, and word reads plus explicit load-hazard slot | reads |
| `55`–`57` | Expansion-1 byte, halfword, and word reads plus explicit load-hazard slot | reads |
| `58`–`5A` | Expansion-2 byte, halfword, and word reads plus explicit load-hazard slot | reads |
| `5B`–`5D` | Expansion-3 byte, halfword, and word reads plus explicit load-hazard slot | reads |
| `5E`–`5F` | SPU status byte and aligned SPU word reads plus explicit load-hazard slot | reads |
| `60`–`62` | Cache-control byte, halfword, and word reads plus explicit load-hazard slot | reads |
| `63` | Memory-control word reads plus explicit load-hazard slot | reads |
| `64` | Interlocked unaligned SPU `LWL`/`LWR` pair plus explicit hazard slot | pairs |
| `65` | 1 KiB RAM-to-SPU DMA completion (`512` halfwords) | halfwords |
| `66`–`68` | GPU block DMA of 16, 64, and 256 GP0 NOP words using a 16-word block size | words |
| `69`–`6A` | GPU block DMA of 256 GP0 NOP words using 64×4 and 256×1 BCR shapes | words |
| `6B` | GPU linked-list DMA with two headers and 128 GP0 NOP words per node | header + payload words |
| `6C`–`6D` | CPU-submitted monochrome lines: 16 short 16-pixel spans and 8 long 256-pixel spans | rasterized pixels |
| `6E`–`6F` | Matching Gouraud-line batches, including Q12 color interpolation | rasterized pixels |
| `70` | Main-RAM DRAM refresh period recovered from slow uncached reads | samples |
| `71` | Extra uncached-read wait imposed by a DRAM refresh slot | samples |

The exact instruction blocks use `.set noreorder` and literal repetitions, so
the workload does not depend on LLVM loop generation. Timer reset, workload,
and timer read are emitted in one non-inlined assembly block, with inputs live
before the reset. Harmless marker instructions outside the measured interval
let the final PS-X machine code be audited byte-for-byte. Five measurements are
kept per record; no average is used because an interrupt-inflated average is a
poor estimate of instruction cost.

The four short-cache records complement the 4 KiB aggregate. Their targets
are 16-byte aligned and enter at linked word offsets 0, 1, and 2, while the
timing wrappers occupy different direct-map indices. Comparing `42`–`44`
reveals entry-position/refill-shape cost; comparing `42` with `45` isolates a
cold refill from the same call/return path when warm. The final-EXE audit
checks these layouts rather than trusting source alignment alone.

The GPU DMA records send only GP0 NOP commands, so they do not alter VRAM or
the photographed capture. Records `68`–`6A` deliberately hold the 256-word
payload constant while changing BCR shape. That separates actual bus/FIFO
cost from completion models based only on block count; record `6B` also
exposes linked-list header and node-transition overhead.

The line records start Timer 2 immediately before the first GP0 packet and
stop only after GPUSTAT command-ready returns. CPU write stalls and the final
GPU drain are therefore both included. Short/long pairs separate command
setup from per-pixel cost; monochrome/Gouraud pairs expose interpolation cost.
They draw into off-screen VRAM and do not cover the photographed capture.

The final two timing records use single uncached-RAM reads as a refresh
detector. Interrupts are disabled during each scan. Timer 0 timestamps only
the contended reads, avoiding a counter read on every iteration that would
itself disturb the cadence. `70` reports the interval between refresh slots;
`71` reports the largest extra wait above the uncontended read floor. These
two records occupy the previously unused cells on timing page 15.

PX6 includes the observed values from conformance cases 116–137 in fixed order,
preserving the exact partial or stale values needed to infer silicon latency.

## SCPH-9902 calibration (PX6 `B2761BE1`)

The final PAL hardware capture established these constraints:

- NCLIP's scene-A MAC0 stayed at `0x00000874` for every read gap from 47
  through 64 NOPs. Other predecessor sequences produced `0x00002764`,
  `0xFFFFB964`, and `0x00007674` for the same scene. This is internal
  state/history-dependent accumulation, not a fixed command-result latency.
- OP produced MAC1/MAC2/MAC3 = `-768`, `1536`, `0` when issued immediately
  after its CTC2/MTC2 seed, and `-768`, `1536`, `-768` after 64 NOPs. The
  arithmetic is correct; the discrepancy belongs to input/control-write
  commitment.
- GP1 IRQ acknowledgement and DMA-direction changes become visible in
  GPUSTAT one read after the write.
- Timer reached-target/reached-wrap bits survive a mode write and clear on a
  mode-register read. Both Timer 2 interrupt probes raised I_STAT bit 6.
- SPUCNT's low-six-bit SPUSTAT mirror took 24-27 status polls to settle.
  SPUSTAT DMA-request bits did not assert before the DMA channel was armed, so
  they must not be used as a pre-start readiness condition. The captured SPU
  readback words are therefore diagnostic only, not RAM-content calibration.

These are behavioral constraints, not permission to reproduce them with
read-triggered special cases. GPU, GTE, and SPU timing should be scheduled in
bus/SPU/GTE time so unrelated instruction sequences observe the same effects.

## Reading the payload off a console

There are two independent readout paths carrying the *same* bytes.

**QR.** The proven path. Scan each page, keep the `PX8/<page>/<chunk>/C:<crc>`
text. Self-validating and visible, but paged and manual. A conformance capture
is one page; a characterisation capture is six. The page header carries the
total, so there is no fixed count to remember.

**Audio link.** The disc streams the whole 2,637-byte payload out of the SPU as
binary FSK and loops it in hardware, forever, with no CPU involvement. Point a
capture card at the console, record, and decode:

```sh
python3 tools/hwtest-audio-decode.py capture.wav --emit-pages pages.txt
python3 tools/hwtest-report.py pages.txt
```

One repetition takes about 13.6 seconds, so any recording longer than that
contains a complete copy, and a repetition spoiled by a glitch costs the next
11.6 seconds rather than another burn. The decoder re-emits the payload in the
QR wire format on purpose, so both paths feed the identical report pipeline and
cannot drift apart.

Four tone rates are available, cycled with SQUARE on the capture page. They all
replay the SAME uploaded stream at a lower SPU pitch, which stretches each block
over proportionally more output samples: a slower mode lowers both tones and the
baud rate together and multiplies energy per bit while costing no extra SPU RAM,
which matters because the fast stream already fills most of it. The decoder
sweeps all four and detects which was used, so a fallback needs no flag. A
calibration section of each pure tone precedes the data, so a failed decode can
be diagnosed offline instead of by burning again.

Modulation is FSK because it is amplitude-independent: capture chains apply AGC
and arbitrary volume, which would destroy on/off keying but cannot change which
tone is present. Each bit is exactly one ADPCM block, 28 samples at 44.1 kHz,
and the two tones (1575 Hz and 3150 Hz) fit a whole number of cycles into that
block so blocks concatenate without a phase discontinuity. Both land on exact
DFT bins of a 28-sample window, making a bit decision a two-bin comparison with
no filter design.

The emulator path is verified end to end by `make hwtest-audio`, which records
the SPU output, decodes it, and parses the result. The recovered bytes are
byte-identical to the QR payload, at both the fast rate and a slower fallback
rate.

`make hwtest-audio-chain` goes further and degrades that recording the way a
real capture chain does, requiring the decoder to still recover the identical
payload. All twelve cases pass: resampling to 48/32/96 kHz, a 20x gain range,
hard clipping, DC offset, band-limiting, 25% noise, and a stacked worst case.

**Recordings are resampled to 44.1 kHz before decoding, and this is not
optional.** The link's bit clock IS the console's 44.1 kHz sample rate, so at
48 kHz a bit spans 30.48 samples and the tones no longer land on exact DFT
bins. A 48 kHz recording fails completely without that step, and 48 kHz is what
OBS records by default. That proves the encoding and framing; it does
not prove analog robustness through a real capture chain, which only a console
recording can establish.

## Headless validation

None of these need pad input, a GUI, or QR scanning: the tier-1 battery runs at
boot and mirrors every page to the TTY.

```sh
make hwtest-capture   # build guest + run headless -> build/hwtest-capture.log
make hwtest-diff      # audit the linked EXE, then diff the capture vs baseline
make hwtest-audio     # record SPU output, decode it, parse the recovered payload
make hwtest-audio-chain  # decode again through simulated capture-card damage
make hwtest-baseline  # deliberately re-pin the baseline (review the diff first)
make hwtest-capture-full  # FULL characterisation capture (timing, memctl, precision)
make hwtest-diff-full     # ...diffed against px8-emulator-full-v<version>.txt
make hwtest-baseline-full # ...and re-pinned
make hwtest-capture-perf  # PERF A/B run: the sweep, then the register A/B group
make hwtest-diff-perf     # ...gated on the warm records
make hwtest-probe-capture ROW=<n>  # one TARGETED PROBES row, for before/after diffs
make hwtest-silicon SILICON=<payload.txt>   # compare against a console capture
```

`hwtest-capture` side-loads the EXE but mounts the CUE with `--disc`. The CD
battery needs a disc in the drive; against a driveless EXE every CD command
burns its full poll budget timing out, which alone exhausts the instruction cap
before the capture encodes. Booting the CUE directly instead produces no guest
TTY at all.

`hwtest-diff` is the routine gate. Its baseline is a conformance capture, so it
covers the 200 case verdicts and the expected/observed values of the failing
ones, and nothing else: since PX8 made the other blocks optional it has not
seen a timing minimum or a precision value. `hwtest-diff-full` covers those,
against a FULL baseline. Both the guest EXE and the headless capture are
byte-reproducible, so a difference is a real behaviour change rather than
toolchain noise; but see "Timing records move when the guest binary changes"
before reading a timing drift as a regression.

Two baselines are pinned, and they answer different questions:

| File | What it pins | Fails when |
|---|---|---|
| `docs/hardware-refs/px8-emulator-v*.txt` | conformance verdicts and failing values | emulator behaviour moves |
| `docs/hardware-refs/px8-emulator-full-v*.txt` | every captured value | emulator behaviour moves, or guest code shifts alignment |
| `docs/hardware-refs/hwtest-machine-code-*.txt` | the instructions between each probe's markers in the linked EXE | a measured block changes shape |

The second matters because a timing record only means what this document
claims if the instructions inside its measured window are still the ones the
source asked for. `tools/verify-hwtest-machine-code.py` extracts each span
from the linked EXE and digests it. Markers are `ori $zero, $zero, imm` words
(start `0x34000000 | id << 1`, end `start | 1`), which write no register and
which no compiler emits, so the verifier discovers probes by scanning the
image rather than the source: a macro-generated probe or one in another module
is audited like any other. Rows are keyed by probe id and named in the
baseline. A probe that appears, vanishes, reuses an id or loses its end marker
fails the gate. Until v1.20 the markers were `sll`-to-zero words that LLVM
also emits; eight were shared between probes and the six GTE command probes
were never audited at all. The re-keyed v1.20 baseline pins the same word
counts and digests for the original 19 probes, which is the evidence that the
marker change left every measured window untouched.

**The emulator baseline is not hardware truth.** It detects our own drift. The
comparison that matters is `hwtest-silicon` against a real console capture,
and no such payload is currently checked in: the SCPH-9902 capture described
below survives only as the prose in this file. Commit the raw payload text of
the next console run.

The headless run reported `126 pass, 21 fail, 26 info` when the conformance
section had 173 cases (it is `143 pass, 1 fail, 56 info` of 200 at v1.21). The guest's expected values were recalibrated against
SCPH-9902 across several commits (`Match SCPH-9902 hardware fidelity
checkpoint`, `Calibrate final PAL hardware fidelity probes`, `Calibrate OTC and
SPU behavior from SCPH-9902`) while the emulator's GTE was not changed, so
these 21 failures are measured emulator-vs-silicon gaps rather than a
regression.

They cluster almost entirely in GTE NCLIP state-dependent accumulation. Across
the whole big-value settle sweep the emulator returns `0x00000874` where the
console returned `0x00002764`, and the magnitude and controlled-scene variants
diverge similarly. Closing that gap is the highest-value GTE fidelity work
this disc currently points at.

Because expectations are compiled into the guest, revising one costs a rebuild
and a reburn, and old captures cannot be re-judged when understanding
improves. That is the argument for moving judgment host-side and letting the
disc report raw values only.

Completed pages are also mirrored verbatim to the debug TTY with the prefix
`hardware-tests: px6 `. This lets the headless release gate feed the exact same
payload to `hwtest-report.py` without host-side image recognition.

`mkisopsx` currently warns that it does not supply a licensed PS1 system area.
Use the same real-console burn/boot method that worked for the previous test
disc; the generated CUE/BIN pair itself is the artifact validated here.
