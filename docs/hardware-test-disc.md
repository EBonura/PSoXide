# PS1 hardware-test disc

The `hardware-tests` disc is the silicon reference for PSoXide. It runs the
same executable in PSoXide and on a real PlayStation. Real-console data is
transported through QR payloads, so a TV/capture card is enough and no
character-by-character transcription is required.

## Capturing and clearing stale BIOS reverb state (`PA5`)

PA5 boots first and waits five seconds on a variant selector (default
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

Press Down from the completed PA5 screen to revisit PA4.

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

PSoXide's normal real-BIOS fast boot now recreates this measured handoff state.
Its SPU continues reverb reads, APF processing, and wet output while the reverb
master-write bit is clear, matching the hardware probe; only feedback writes
are gated. The SDK now zeros both wet-output depth registers during `spu::init`,
before game banks can reuse SPU RAM. The old PA5 executable reproduces the
fault under CONTROL and is digitally silent under DEPTH0 in PSoXide, while a
newly linked Half-Life build remains digitally silent through its first eight
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

Press Down from the completed PA4 screen to revisit PA3.

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

Press Down once from the completed PA3 screen to reach PA2.

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

## Payload schema `PX6`

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

## Headless validation

Build the disc with:

```sh
make hardware-tests-disc
python3 tools/hwtest-report.py /tmp/hardware-tests.log
```

PSoXide can render a deterministic timing page without opening the GUI:

```sh
cargo run -p frontend --release -- launch \
  --path build/examples/mipsel-sony-psx/release/hardware-tests.exe \
  --steps 160000000 --pad-pulses '0x08@300+3,0x20@310+3,0x20@320+3' \
  --guest-debug-log --dump-display /tmp/hardware-tests.ppm --dump-hash \
  > /tmp/hardware-tests.log 2>&1
```

The current release run reports `129 pass, 18 fail, 26 info` for the 173-case
conformance section. The remaining failures are deliberately retained GTE
fidelity targets: the LZCR/NCLIP transition probes and the state-dependent
in-situ NCLIP sequence. Their full observed words are preserved in PX6 rather
than being hidden behind a green aggregate.

The active-high pad mask `0x08` is Start and `0x20` is Right. Multi-route-tick
pulses are used in automation so they cannot fall entirely between guest
frames. The release regression captures all three pages, reconstructs the binary,
and validates its per-page and full-record CRCs. All 173 conformance results,
90 timing records, three scan summaries, nine memory-control registers, 128
precision values, and 22 full-width GTE observations must be present.

Completed pages are also mirrored verbatim to the debug TTY with the prefix
`hardware-tests: px6 `. This lets the headless release gate feed the exact same
payload to `hwtest-report.py` without host-side image recognition.

`mkisopsx` currently warns that it does not supply a licensed PS1 system area.
Use the same real-console burn/boot method that worked for the previous test
disc; the generated CUE/BIN pair itself is the artifact validated here.
