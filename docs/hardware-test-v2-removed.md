# What v2.0 took out of the hardware-test suite, and where each thing went

v1.28 was a menu of about forty screens that had to be run in a sensible order
by a person. v2.0 is one run in a fixed order with a reset per area (see
[hardware-test-disc.md](hardware-test-disc.md)). Everything below existed in
v1.28 and is no longer a separate screen. "Folded" means the measurement still
happens, as a step of the run, with its record ids unchanged unless the table
says otherwise.

| v1.28 entry | v2.0 |
|---|---|
| RUN ALL TESTS + CAPTURE | The run. There is no cheaper variant; the run is the characterisation. |
| FULL CHARACTERISATION CAPTURE | The run (the same thing). |
| CONTROLLER TEST (P1 + P2) | Kept, a separate screen (needs a person). |
| MEMORY CARD (AT OWN RISK) | Kept, a separate screen (touches the operator's card). |
| VIEW CAPTURE (QR PAGES) | Kept as VIEW LAST CAPTURE. |
| RESULTS BY SECTION (ALL CHECKS, CPU, RAM, IRQ, DMA, TIMERS, GPU, GTE, SPU, CDROM, SIO) | Removed as screens. Every case verdict is in the capture and in the TTY report; `make hwtest-run` and `tools/hwtest-report.py` list them by area. |
| HARDWARE SCANS: CPU SWEEP, GTE SWEEP, SPU REGISTER MAP | Folded: steps `CPU SWEEP`, `GTE SWEEP`, `SPU MAP`. |
| TARGETED PROBES: SPU DIAGNOSTIC (SB2) | Folded: step `SPU RAM AND VOICES`. A bug went with the move: in v1.28 SB2 never ran `begin_step` for its first segment, so its tables were never uploaded and the old tone records measured stale SPU RAM. Fixed in v2.0; the SB2 rows of silicon captures taken before v2.0 are not what their names say. |
| TARGETED PROBES: CAPTURE RINGS (SB4) | Folded: step `CAPTURE RINGS`. |
| TARGETED PROBES: UI SAMPLE END/LOOP (SB1) | Folded: step `UI SAMPLE END AND LOOP`. |
| TARGETED PROBES: CD READ MECHANISM (CL2) | Folded: step `CD READ MECHANISMS`. |
| TARGETED PROBES: CONTROLLER SIO TIMING | Folded into the SIO area, which grew (see below). |
| TARGETED PROBES: HL REVERB STATE (PA5) | The boot snapshot (`BOOT STATE`) and `SPU INIT STATE` take the reverb state before and after SDK init, once, at the start of the run. The one-variant-per-reboot flow is gone. |
| TARGETED PROBES: HL VOICE HANDOFF (PA4) | Folded: step `BANK HANDOFF` (the Baseline and Safe2 variants). |
| TARGETED PROBES: HL BANK TRANSITION (PA3), HL VOICE BANK (PA2) | Removed as screens. PA3 established the fault that PA4's variants isolate while keeping its timing, so the `BANK HANDOFF` step covers it; PA2 was a recording aid for the bank layouts, not a measurement (the layouts live on in the handoff probe). |
| TARGETED PROBES: CD/SPU AUDIO (PA1) | Folded: step `CD DATA VERSUS AUDIO ROUTE` (data reads against the SPU's CD audio route, read back from the capture buffer). |
| TARGETED PROBES: PERF SWEEP (SAFE) | Folded: steps `WARM PROBES`, `EXTENDED PROBES AND SHAPES`. |
| TARGETED PROBES: PERF A/B (MAY HANG) | Folded as the last step of the run, `REGISTER A/B (CAN HANG)`. Hold L2 when starting the run to skip it. |
| CONSOLE TESTS: KERNEL TIMING, DISPLAY WIDTHS, 480I INTERLACE | Folded: steps `KERNEL TIMING`, `DISPLAY WIDTHS`, `480I INTERLACE`. |
| CONSOLE TESTS: XA MUSIC LOOP | Folded: step `XA MUSIC LOOP`. |
| CONSOLE TESTS: CD STREAM COST, CD-DA HANDOFF, CD MOTOR (v1.28) | Folded: steps `STREAM COST`, `CD-DA HANDOFF`, `CD MOTOR`. |
| VIDEO LEVELS (TV/CAPTURE) | Removed. It was for setting up a capture rig, not a measurement. |
| AUDIO READOUT (the payload as an audio tone) | Removed together with its tone generator. That generator is what was still sounding when the v1.28 QR pages came up. |
| RESUME FROM TEST | Removed. The run is linear and every area starts from a reset; after a hang, power-cycle and start the run again (hold L2 to leave out the one step that can hang). |
| MDEC DIAGNOSTIC | Replaced by the short MDEC decode check (step `MDEC DECODE`, records `420`-`426`). |
| FMV STREAM TEST (75 s of STR playback) | Removed. The decode check is the only FMV measurement in the suite now. |

## Records

Comparing a v1.24 full silicon capture with a v2.0 run, these timing records are not
produced any more:

| Records | Why |
|---|---|
| `01`, `02`, `03`, `04`, `09`, `0B`, `0C` (nop block, dependent ALU, cached load hazard, taken branch, scratchpad load, RAM store, scratchpad store) | The cold-start CPU records. Their minima moved by tens of cycles when unrelated code shifted (see "Timing records move when the guest binary changes"). Their warm twins `72`-`78` measure the same instructions with every line resident and are the standing records. |
| `A0`-`AF` (the unpaced GPU fill battery) | Wrote primitives straight to GP0 and read GPUSTAT bit 26 as done; the v1.22 console captures showed that measures the FIFO, not the GPU. The paced list batches `100`-`114` replace them. |

No other record id was reassigned or dropped. Ids that belonged to removed screens
(`1F0`-`1F5` of the FMV stream test) are no longer produced; the report tool no longer
carries a work value for them but still decodes older captures.

## What the SIO area gained

v2.0 added the controller-port measurements the emulator could not settle and
the pad engine questions: see the SIO sections of
[hardware-test-disc.md](hardware-test-disc.md). They are measurement steps,
not gates; none of them fails the run.
