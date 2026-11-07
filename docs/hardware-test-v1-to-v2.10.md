# v1.26/v1.27 to v2.10: where every measurement went

Editor main carried the v1.27 tree of the hardware tests (a menu of screens)
while the suite that is burned and run on the SuperStation card was v2.9 on a
branch (one linear run, BIOS-free). v2.10 puts them in one tree on editor
main. The rule for the merge: no measurement is lost. Each v1.x feature below
is either **ported** into v2.10, **covered** by a named v2.x case that measures
the same thing, **dropped by decision** (with who decided and why), or
**open** (a gap that was found and is not closed). Nothing is dropped
silently; this file is the record.

[hardware-test-v2-removed.md](hardware-test-v2-removed.md) is the older
v1.28 to v2.0 table (what the linear run folded). This file starts from the
v1.27 tree that was on main, so it also covers what main had and v2.0 never
saw: the grid-line E2 probe, the list-upload case, the device-owner changes.

Column "status": ported, covered, decision, open.

## Screens and menu rows of the v1.27 tree

| v1.27 feature | status | v2.10 |
|---|---|---|
| RUN ALL TESTS + CAPTURE, FULL CHARACTERISATION CAPTURE | covered | `RUN HARDWARE TEST` (the linear run is the characterisation) |
| CONTROLLER TEST (P1 + P2) | covered | kept as a screen, with the neGcon line (5025aece) |
| MEMORY CARD (AT OWN RISK) | covered | kept as a screen; takes its owner through `probe_tokens::memcard_diagnostic` |
| VIEW CAPTURE (QR PAGES) | covered | `VIEW LAST CAPTURE` |
| RESULTS BY SECTION (ALL, CPU, RAM, IRQ, DMA, TIMERS, GPU, GTE, SPU, CD-ROM, SIO) | covered | every case runs in its area and prints its verdict in the TTY report and the capture; no per-section screens |
| HARDWARE SCANS: CPU SWEEP, GTE SWEEP, SPU REGISTER MAP | covered | steps `CPU SWEEP`, `GTE SWEEP`, `SPU MAP` |
| TARGETED PROBES: SPU DIAGNOSTIC (SB2) | covered | step `SPU RAM AND VOICES` (v2.0 fixed the SB2 first-segment bug) |
| TARGETED PROBES: CAPTURE RINGS (SB4) | covered | step `CAPTURE RINGS` |
| TARGETED PROBES: UI SAMPLE END/LOOP (SB1) | covered | step `UI SAMPLE END AND LOOP` |
| TARGETED PROBES: CD READ MECHANISM (CL2) | covered | step `CD READ MECHANISMS` |
| TARGETED PROBES: CONTROLLER SIO TIMING | covered | the SIO area (`SIO TIMING`, `SIO0 SELECT DELAY`, ack timing steps) |
| TARGETED PROBES: HL VOICE HANDOFF (PA4) | covered | step `BANK HANDOFF` (Baseline and Safe2 variants) |
| TARGETED PROBES: HL BANK TRANSITION (PA3), HL VOICE BANK (PA2) | decision | v2.0: PA3 established the fault PA4's variants isolate and PA4 keeps the timing; PA2 was a recording aid for the bank layouts, not a measurement |
| TARGETED PROBES: HL REVERB STATE (PA5) | covered, partly open | `BOOT STATE` and `SPU INIT STATE` record the reverb state before and after SDK init. PA5's one-variant-per-reboot reset matrix (map-DMA reset variants) is not run. Open item 2 |
| TARGETED PROBES: CD/SPU AUDIO (PA1) | ported | step `CD DATA VERSUS AUDIO ROUTE`; v2.0 kept the peaks and a state word, v2.10 restores every per-stage field of the PA1 probe (records `0x520 + 8 * stage + k`). Differences that remain: stages run 40 frames and sample at frame 30 (PA1: 180 and 120), and the calibration tone voice in stage 0 (an OBS waveform reference) is not played |
| TARGETED PROBES: PERF SWEEP (SAFE), PERF A/B (MAY HANG) | covered | steps `WARM PROBES`, `EXTENDED PROBES AND SHAPES`; `REGISTER A/B (CAN HANG)` is the last step |
| CONSOLE TESTS: KERNEL TIMING (BIOS) | covered | step `KERNEL TIMING`, records `0x2C0`-`0x2C5`; v2.9 puts back whatever vector it finds, so on an OpenBIOS machine the BIOS part is skipped (flag bit 3) instead of hanging |
| CONSOLE TESTS: DISPLAY WIDTHS | covered | step `DISPLAY WIDTHS`, records `0x2D0`-`0x2D5`, now measured: GPUSTAT mode bits and dot-clock ticks per 64 lines. The on-screen edge-marker pattern a person checked on a TV is gone (v2.0 rule: objective signals, not looking) |
| CONSOLE TESTS: 480I INTERLACE | covered | step `480I INTERLACE`, records `0x2D6`-`0x2D7` (plus the new `480I DRAW RULE`) |
| CONSOLE TESTS: XA MUSIC LOOP | covered | step `XA MUSIC LOOP`, records `0x2E0`-`0x2E3` |
| v1.28 CD STREAM COST, CD-DA HANDOFF, CD MOTOR | covered | steps `STREAM COST`, `CD-DA HANDOFF`, `CD MOTOR` |
| VIDEO LEVELS (TV/CAPTURE) | decision | v2.0: it was for setting up a capture rig, not a measurement |
| AUDIO READOUT (the payload as an audio tone) | decision | v2.0, Manny 10-08: the tone was still sounding when the QR pages came up. The host decoder (`audio-decode`) stays for old recordings |
| RESUME FROM TEST | decision | v2.0: the run is linear and every area starts from a reset. A hang is a power cycle and a run with L2 held |
| MDEC DIAGNOSTIC | ported | step `MDEC SETUP DIAGNOSTIC` (see below) |
| FMV STREAM TEST (75 s of STR playback, records `0x1F0`-`0x1F5`) | decision, open | Manny 10-08: "FMV = short MDEC decode check only, no 75 s playback". `MDEC DECODE`, the new `MDEC SETUP DIAGNOSTIC`, `STREAM COST` (CD to CPU at double speed) and the `MDEC DMA VERSUS CPU` matrix cover the pieces; the end-to-end player run with its lost/bad/late sector counters is not in the suite and `MOVIE.STR` is not on the disc. Open item 1 |

## What main had that v2.0 never saw

| main feature | status | v2.10 |
|---|---|---|
| Case `0xEA` (index 234), `list_upload_probe.rs`: A0h image rows in 16-word list nodes read back exactly | ported | conformance case 234 in the GPU area; `TEST_COUNT` 235 |
| Grid-line E2 probe, six texture-window GPU batches (`0x140`-`0x145`, `gpu_probes.rs`) | ported | `GPU BATCHES`, records `0x146`-`0x14B` (the old ids are the v2 CPU/DMA matrix) |
| Token steals (4fcfc51ae): CD, DMA 6 and memory-card owners taken with `steal` in one module | ported | `probe_tokens.rs`; the v2.9 sources, which still called the removed free forwarders, were converted. `audio_probe.rs` itself is gone with its screen (its measurements are the PA1 row above) |
| Convention names (ea367f697): `write_data!`, `read_data!`, `project_triple` in host-only branches | ported | `main.rs` |
| neGcon line in the controller screens (5025aece) | covered | merged into `controller_test.rs` |
| Pin bumps, `psx-sfx` lock, `psoxide-emulator` package name in the Makefile | covered | editor main's lock: SDK `bfb464405`, emulator `bb788f5b`; `cargo run -p psoxide-emulator` in the Makefile |
| `psx-engine` reaching `psx-level` needs `alloc` | covered | build flags `-Zbuild-std=core,alloc`, `psx-rt` feature `alloc` |

## MDEC DIAGNOSTIC in detail

| v1.26 part | status | v2.10 |
|---|---|---|
| Six setup sequences A to F, eight runs each, probe decode after each | ported | same sequences, same run order; records `0x230 + 0x10 * v` and up (v1.26: `0x200 + 0x10 * v`, which the v2 texture-state block uses) |
| MDEC1 traces after reset (idle, busy, busy with enable) | ported | records `0x298 + 5 * t` |
| One-frame CPU-versus-DMA control on the movie's first frame | ported, changed | the control decodes the probe macroblock instead (records `0x2A8`); the movie is not on the disc |
| Playback of a 15 s cut behind each sequence | decision | needs `MOVIE.STR`; see FMV STREAM TEST above |
| On-screen summary and one page per sequence | decision | the capture carries it; the step shows progress only. Decoder rows: `mdec_diag`, `mdec_reset_trace`, `mdec_control` |
| Fault-injection feature (`diag-fault`) | decision | headless-only aid for the failure paths; not carried |

Captures taken with the v1.26 to v1.28 builds decode their MDEC diagnostic
(`0x200`-`0x2A2`) and FMV (`0x1F0`-`0x1F5`) records with the tool from editor
main before this landing (cb659f8e); v2.10's tool reads the new layout.

## Timing records v2.0 removed (still not produced)

| Records | status | why |
|---|---|---|
| `01`, `02`, `03`, `04`, `09`, `0B`, `0C` (cold-start CPU block) | decision, open | their minima move by tens of cycles when unrelated code shifts; the warm twins `72`-`78` measure the same instructions. Open item 3 |
| `A0`-`AF` (unpaced GPU fill battery) | decision, open | it read GPUSTAT bit 26 as done and so measured the FIFO, not the GPU (v1.22 console captures); the paced list batches `100`-`114` replace it. Open item 3 |

## Open items

1. The FMV stream test (75 s `MOVIE.STR` through the SDK player with sector accounting) is the one v1.x end-to-end check with no equivalent. Porting it back needs `MOVIE.STR` on the disc (FFmpeg and psxavenc to encode it, 9,826 video sectors) and the 75 s run, which Manny ruled out of the linear run on 10-08. If it should return, it is a last step behind the L2 skip.
2. PA5's reset-variant matrix (reverb state after each map-DMA reset variant, one variant per reboot) has no step.
3. The cold CPU block and the unpaced GPU fill battery were removed in v2.0 with documented reasons. If the silicon baselines from v1.24 and earlier must stay comparable for them, they need porting; otherwise they stay out.
