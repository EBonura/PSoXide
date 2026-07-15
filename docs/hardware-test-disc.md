# PS1 hardware-test disc

The `hardware-tests` disc is the silicon reference for PSoXide. It runs the
same executable in PSoXide and on a real PlayStation. Real-console data is
transported through three QR pages, so a TV and phone camera are enough and no
character-by-character transcription is required.

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
forced-stable comparison hashes, repeated GPUSTAT reads after IRQ and
DMA-direction writes, exact Timer 2 mode/counter/I_STAT snapshots, raw SPU
voice-register masks, consecutive OTC CHCR reads, an RTPS V0 input-commit gap
sweep, and repeated full-width GTE results. This extra data reveals corruption
and state-transition shapes that a checksum or one-shot pass/fail result cannot.

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
