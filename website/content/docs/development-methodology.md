+++
title = "Development methodology"
description = "How console measurements become emulator improvements, SDK fixes and repeatable regression tests."
weight = 1
[extra]
kind = "Methodology"
eyebrow = "Development · Measure, compare, repeat"
+++

PSoXide asks what happens if we keep learning how to develop for the original
PlayStation. That learning needs a reference: **the real console**. The emulator
makes experiments quick to repeat; hardware measurements tell us where its model
needs to change. Games expose practical problems, small test programs isolate
them, and the results feed back into the emulator and SDK.

For day-to-day commands, start with the [development workflow](@/docs/development-workflow.md).
For the SDK's build checks and individual probe programs, see [hardware checks](@/docs/hardware-checks.md).

## The hardware feedback loop

1. **Ask a specific question.** Reduce a game problem to a small probe with an observable result.
2. **Freeze the experiment.** Record the test build and run the same executable in the emulator and on a console.
3. **Capture the console.** Record the test and its result pages, including video and audio where behaviour matters.
4. **Decode and compare.** Recover complete, checked payloads and compare matching records.
5. **Correct the model or program.** Explain the discrepancy, make a focused change and add a regression check.
6. **Retest and retain the evidence.** Rerun the battery and affected games; return to hardware where the claim needs it.

### Ask a measurable question

“This scene is slow” is a starting point. “When does GPU DMA finish relative to
the last primitive being drawn?” is a testable question. A useful probe varies
one condition and records register values, cycle counts, memory readback, pixel
hashes or audio. The Hardware Tests suite covers CPU and geometry operations,
graphics, sound, disc access, DMA, timers and controllers. A passing emulator run
establishes a reproducible starting point; it does not establish what the console does.

### Freeze the experiment

Keep the source revision, toolchain, build options and executable or disc hash
with the result. Record the suite version, console model, video mode, boot path
and probe order. Loading through a launcher can leave a different starting state
from a fresh boot. Some probes alter hardware state and require a reboot before
the next comparison.

The payload schema describes the encoding; the suite version describes what each
record means. Matching record numbers alone is insufficient. Even an unrelated
rebuild can move code in the instruction cache and change a timing result. The
linked-code verifier checks the measured instruction spans, while warm timing
probes reduce cache-layout effects. See the
[suite version rules](https://github.com/EBonura/PSoXide-editor/blob/4f7adcf98ae3e0022b3257cb9d98a4a3122e15ea/docs/hardware-test-versions.md)
and [measurement guide](https://github.com/EBonura/PSoXide-editor/blob/4f7adcf98ae3e0022b3257cb9d98a4a3122e15ea/docs/hardware-test-disc.md#timing-records-move-when-the-guest-binary-changes).

### Capture the console

The suite can run from a test disc or as the Demo Disc's Hardware Tests entry.
For a new reference, choose **FULL CHARACTERISATION CAPTURE**. A routine
conformance capture omits measurements that are informational rather than
failures; it cannot answer every timing question.

Record from before power-on, let the battery finish, and hold every result QR
page steady long enough to recover it. Keep video and audio of the tests: a
moving marker can reveal tearing, and an audible tone can resolve an ambiguous
sound-RAM readback. The page count depends on the suite version. Targeted probes
may need separate fresh-boot runs.

Burned discs need a console with a compatible homebrew boot method, such as a
modchipped console. PSoXide does not endorse modchipping or provide installation
instructions. Modifications can permanently damage or brick hardware; see the
[hardware warning](@/legal.md#running-burned-discs-on-original-hardware).

### Decode and compare

The console displays machine-readable QR payloads so results can be recovered
from a recording. The transport checks individual pages and the assembled
payload with CRCs. Pages from different runs must not be mixed, even when their
page numbers match. An incomplete or invalid payload is missing evidence.

The emulator mirrors payloads to its debug output, so both routes feed the same
reporting tools. These commands run from a configured **PSoXide-editor** checkout
with the matching test build; replace the recording and output paths:

```sh
# Recover and inspect the console's full capture.
python3 tools/hwtest-video-qr.py /path/to/console.mov /path/to/console.txt
python3 tools/hwtest-report.py /path/to/console.txt

# Capture the same suite in the emulator, then compare full results.
make hwtest-capture-full
python3 tools/hwtest-report.py \
  --baseline /path/to/console.txt build/hwtest-capture-full.log
```

Use the [operator guide](https://github.com/EBonura/PSoXide-editor/blob/4f7adcf98ae3e0022b3257cb9d98a4a3122e15ea/docs/hardware-test-disc.md#headless-validation)
for setup and capture options. Exact values and pixel hashes can be compared
directly where the probe defines them as deterministic. Timing needs its units,
sampling conditions and variation preserved. A CRC validates transport, not the
experiment's design or interpretation.

### Correct, retest and retain

A mismatch can come from the emulator, the SDK or game, the probe itself, the
capture, or a different console configuration. Isolate those possibilities
before fitting a timing constant. Hardware results have corrected both emulator
behaviour and assumptions made by SDK code.

After a fix, rerun the focused probe, the wider battery and affected game routes.
Retain the console capture separately from the emulator baseline: the former
records a hardware observation; the latter detects changes in our implementation.
Review baseline changes rather than accepting new output merely to make a test
pass. If an SDK or game fix changes console behaviour, a console retest closes
that claim.

## A measured example: when drawing really finishes

On 23 September 2026, Hardware Tests v1.24 measured an expensive graphics list on
a PAL console running in NTSC video mode. The test distinguished completion of
the DMA transfer from completion of drawing. All values below are elapsed clocks
from the list's submission, for that test and those emulator revisions.

<div class="table-wrap" role="region" aria-label="Recorded GPU timing comparison" tabindex="0">

| Observation | Console | Emulator before | Emulator after |
|---|---:|---:|---:|
| DMA channel stops being busy | 586,354 | 283 | 585,656 |
| GPU reaches the closing draw-complete interrupt command | 625,348 | 6 | 624,731 |

</div>

The earlier model completed the transfer using a word-count formula and signalled
the interrupt almost immediately. The console kept DMA busy as the GPU consumed
the list. The corrected model paced the transfer through the GPU's command FIFO
and accounted for drawing time. These are the recorded results for emulator
`0d3f4c9` before and `561bd2c` after, not a blanket accuracy score.
[Read the capture analysis and remaining discrepancies](https://github.com/EBonura/PSoXide-editor/blob/4f7adcf98ae3e0022b3257cb9d98a4a3122e15ea/docs/emulator-accuracy-from-silicon.md#gpu-dma-channel-2-stays-busy-while-the-list-draws).

That distinction affected development decisions. An apparent roughly 21% gain
from one Half-Life rendering experiment depended on the old DMA model and did
not survive the more realistic model. The same console capture showed why the
SDK must wait for a draw-complete signal before presenting a frame: DMA readiness
could return while the last primitive was still drawing. The
[presentation probe](@/docs/hardware-checks.md#when-a-frame-has-finished-drawing)
explains the resulting SDK contract.

The [archived six-page console payload](https://github.com/EBonura/PSoXide-editor/blob/4f7adcf98ae3e0022b3257cb9d98a4a3122e15ea/docs/hardware-refs/px8-silicon-2026-09-23-v1.24-full.txt)
is retained alongside the analysis. Other captures led to corrections in CPU
timing, interrupt handling around geometry commands, and sound transfers. The
[hardware checks guide](@/docs/hardware-checks.md) follows those examples in detail.

## Measuring an improvement in a game

Freeze the emulator revision while comparing two game builds. Replay the same
inputs over the same gameplay window, and inspect frame-time distributions and
missed deadlines as well as average frame rate. Keep load screens and gameplay
separate. Check images, game state and audio so a reduction in work does not hide
missing output or changed behaviour.

Choose comparison checkpoints that remain meaningful when execution gets faster.
A final image can differ because a camera or animation advanced further. An
improvement in one microbenchmark or recorded route is evidence for that workload,
not every game or the whole game. When the emulator's timing model changes,
rerun both builds before comparing their numbers.

Host performance is a separate measurement: how fast a PC or browser runs the
emulator does not say how fast the game runs on a PS1. The
[recorded comparisons](@/emulator/compare.md) state their test conditions.

## What a result establishes

| Evidence | What it supports |
|---|---|
| Passing emulator regression | Behaviour remains consistent with a recorded emulator baseline. |
| Console capture | The measured behaviour on the recorded hardware, build and test conditions. |
| Model fitted to captures | Better agreement for the measured cases, with residual errors still reported. |
| Game route passes after a fix | That route works under the stated conditions; broader coverage needs more tests. |

Console revisions and peripherals can behave differently. Unmeasured behaviour
stays unmeasured, and an explanation remains a hypothesis until a test can
distinguish it from alternatives. Keeping those limits visible is how the
emulator becomes a more useful development tool as the project learns more
about the hardware.
