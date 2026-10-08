+++
title = "Checking code against PlayStation hardware"
description = "The checks the SDK runs on every linked executable, the probe programs and test disc that compare PSoXide with a real console, and the bugs they found."
weight = 2
[extra]
kind = "Explainer"
eyebrow = "Explainer · SDK, emulator and editor"
+++

An emulator is a model of the console, and a model can be wrong in ways that let broken code run. PSoXide treats original hardware as the reference: when the emulator and a PlayStation disagree, the PlayStation is right. Three things enforce that. The SDK's build checks every linked executable for mistakes the CPU won't catch. Small probe programs in the SDK each test one piece of behaviour and print a verdict. The Hardware Tests disc runs a large battery on a real console and gets the results back to a computer, where they're compared with the emulator record by record.

The sections below cover each and the bugs it has found. Dates are when a capture was recorded or a change landed; statuses are as of the linked document or commit.

For the complete process from a testable question to a console capture, emulator
correction and regression test, read the [development methodology](@/docs/development-methodology.md).

## Checks after every link

The build for `hello-tri` and the other SDK examples doesn't stop at the linker. `make example` goes through the SDK's profile-guided build driver (see [below](#profile-guided-builds)), which runs three tools on the finished executable, using the linker map the link writes. `make test` runs the tools' own tests, and `make lint` runs a fourth check over the SDK's assembly.

### Load-delay hazards

The PlayStation's CPU, a MIPS R3000, has no load interlock: the instruction straight after a load still sees the register's old value. LLVM puts a `nop` after a load, but its delay-slot filler can then move that load into a branch delay slot, where the first instruction at the branch target reads the register one instruction too early.

The SDK's build turns on two extra delay-slot filler searches, because filled slots mean fewer `nop`s. [`sdk-examples.mk`](https://github.com/EBonura/PSoXide/blob/main/tools/sdk-examples.mk) records the measurement behind that: on the Half-Life port, 7% of executed instructions were delay-slot `nop`s, and the two switches gave 2.3% more rendered frames per second with 10.9 KB less code. Those searches can create hazards, so [`hazard-patch`](https://github.com/EBonura/PSoXide/tree/main/tools/psoxide-hazard) runs after the link. It reroutes each hazardous branch through a small trampoline in a `HAZARD_TRAMPOLINES` array that psx-rt declares, so the consumer runs later, without moving any other code. For register jumps (`jr ra`, `jalr`) the consumer isn't visible in the image, so the load is moved out of the slot instead. It then scans the patched image again and fails the build if anything is left.

[`hazard-scan`](https://github.com/EBonura/PSoXide/tree/main/tools/psoxide-hazard) proves an image is clean whatever built it. It shares its detector with the patcher. There used to be two copies, and they drifted until each missed a class of hazard the other caught. It counts any slot load whose consumer it can't see as a hazard. It has happened: in one port, settings getters returned stale values through a load in the delay slot of `jr ra` (15 September 2026). The scanner also warns, without failing, about a GTE command in a branch delay slot, for a reason covered under [hello-gteirq](#interrupts-and-gte-commands).

### Scratchpad stacks

The R3000 has no data cache. Its 1 KiB scratchpad is the only data memory that loads in one cycle, so psx-rt can run a call with its stack there ([`scratchpad.rs`](https://github.com/EBonura/PSoXide/blob/main/sdk/crates/psx-rt/src/scratchpad.rs)). Nothing at run time stops a deep call tree running off the bottom of that region, and an inlining change can grow a tree without any source change. A profile-guided Half-Life build grew one chain from 568 to 824 bytes.

[`stack-guard`](https://github.com/EBonura/PSoXide/tree/main/tools/psoxide-hazard) walks the linked image's call graph from every scratchpad stack entry, sums frame sizes down the deepest path and fails if the total exceeds the region minus psx-rt's 20 bytes of overhead. It also fails on what it can't bound: recursion, calls through a register, register jumps it can't prove are a switch, and `$sp` changed any other way.

### Coprocessor moves in assembly

`mfc0` and `mfc2` have a load delay too. [`check-mfc0`](https://github.com/EBonura/PSoXide/blob/main/tools/xtask/src/mfc0.rs) reads Rust source for inline assembly where the next instruction uses the destination register. Its documentation records that this shipped twice: once in psx-rt's `enable_cpu_interrupts`, and once in the demo disc's loader, where the stale value landed in the status register with the boot-vector bit set and every interrupt went to ROM. The emulator models this hazard, but only code that runs gets caught, so the check reads the source instead.

### 64-bit arithmetic

The PS1 has no 64-bit ALU, so `i64` and `u64` divides call compiler helpers that cost hundreds of cycles. [`guest_symbol_gate.sh`](https://github.com/EBonura/PSoXide/blob/main/tools/guest_symbol_gate.sh) fails when a link map contains those helpers. It isn't part of the SDK example build; the editor repository runs it (`make guest-symbol-gate`) and its Cortex Ignition benchmark does too.

## Profile-guided builds

[`psoxide-pgo`](https://github.com/EBonura/PSoXide/blob/main/tools/psoxide-pgo/README.md) builds a program with a profile collected in the emulator rather than from an instrumented build. It replays an input tape in the headless emulator while sampling the program counter, maps the samples through the program's debug information into an LLVM sample profile, and rebuilds with it. `choose` builds several variants (including `off`, no profile) and ranks them with a gate script the game supplies, by the frames that miss their deadline first and average work last.

The README's own results show why every game has to choose. The Half-Life port measured 9.3% more rendered frames per second on the route that trained the profile, and 5.9% more on a route that shared no map with it. On VoXide, every profiled variant executed 1.9 to 3.1% more instructions than the plain build, and `off` won. NitroXide is the case for ranking by deadlines: its `default` variant did 8.22% less work on average but showed the fewest frames at 60 fps, because the profiled builds did more work on the heavy frames.

The README also notes that the same Half-Life commit built from two checkout paths differed by about 0.4% in frame rate, because the path changes the code layout, so it treats smaller differences as unproven without a cycle breakdown. The SDK examples don't ship a profile, so `make example` builds them plainly, but it still goes through the driver and its checks.

## Probe programs in the SDK

Each probe is an ordinary SDK example that tests one behaviour and prints `PASS` or `FAIL` to the debug output and on screen, so the same disc can be read on a console. Build any of them with `make disc EXAMPLE=<name>`. The list is in the [SDK README](https://github.com/EBonura/PSoXide/blob/main/sdk/README.md).

### 64-bit signed division

[`hello-i64probe`](https://github.com/EBonura/PSoXide/blob/main/sdk/examples/hello-i64probe/src/main.rs) runs 64-bit multiply, divide and remainder through `black_box`, so the compiler can't fold them away. It showed that compiler-builtins' signed `__divdi3` and `__moddi3` return garbage on the `mipsel-sony-psx` target, while the unsigned helpers work. psx-rt now defines both with the sign handled by hand over the unsigned path, which overrides the weak compiler-builtins symbols for every program that links it ([`builtins.rs`](https://github.com/EBonura/PSoXide/blob/main/sdk/crates/psx-rt/src/builtins.rs), 6 July 2026).

### Interrupts and GTE commands

When an interrupt is taken on a GTE command, the command runs and EPC still points at it, so a handler that returns to EPC runs it twice. The Hardware Tests v1.24 console capture (23 September 2026) measured it: 38 of 38 interrupts that landed on an RTPS ran it twice. With psx-spx's fix, which resumes at EPC + 4 when the word at EPC is a GTE command, 61 were stepped over and none doubled or lost. psx-rt's handler now does the same ([`interrupts.rs`](https://github.com/EBonura/PSoXide/blob/main/sdk/crates/psx-rt/src/interrupts.rs)).

At the time, the emulator deferred every such interrupt past the command, so it couldn't show the bug. It now takes the interrupt with EPC on the command, as the console does (emulator commit [`c7e3ea8`](https://github.com/EBonura/PSoXide-emulator/commit/c7e3ea8), 23 September 2026). [`hello-gteirq`](https://github.com/EBonura/PSoXide/blob/main/sdk/examples/hello-gteirq/src/main.rs) runs the capture's RTPS pattern under psx-rt's handler and checks that nothing ran twice or went missing. The fix can't cover a GTE command in a branch delay slot, because EPC then points at the branch; that case isn't modelled either, which is why `hazard-scan` warns about it.

### When a frame has finished drawing

psx-rt flips the displayed buffer in its vertical-blank handler. It used to wait for GPUSTAT bit 28, but the v1.24 capture showed that bit rising when DMA had pushed the list's last packet, about one large primitive before the drawing ended (586,354 against 625,348 clocks on one test list). A flip on bit 28 could show a frame one primitive short. The handler now waits for bit 24, which the GPU sets when it reaches the frame's closing `GP0(1Fh)`; a present-queue probe on the same console flipped on that flag with 120 of 120 frames complete. That changed the API contract: a frame now calls `arm_draw_done` when it starts and ends with `end_with_draw_done` or `signal_draw_done`, or its flip stays queued. [`hello-present`](https://github.com/EBonura/PSoXide/blob/main/sdk/examples/hello-present/src/main.rs) checks that contract in the emulator (SDK commit [`18dfd47f2`](https://github.com/EBonura/PSoXide/commit/18dfd47f2), 23 September 2026).

### Scratchpad stacks under interrupts

[`hello-spstack`](https://github.com/EBonura/PSoXide/blob/main/sdk/examples/hello-spstack/src/main.rs) runs the same call-heavy workload on the RAM stack and on a scratchpad stack while vertical-blank interrupts arrive, and checks the results match and nothing around the stack was disturbed. The v1.24 console capture also timed one call on each stack: 13,618 cycles on the RAM stack and 10,806 on the scratchpad stack when idle. During a linked-list DMA the RAM-stack call slowed by 56% and the scratchpad one by 2.6% (recorded in the editor's [accuracy notes](https://github.com/EBonura/PSoXide-editor/blob/main/docs/emulator-accuracy-from-silicon.md)).

Two more probes are smaller in scope. [`hello-memprobe`](https://github.com/EBonura/PSoXide/blob/main/sdk/examples/hello-memprobe/src/main.rs) checks psx-rt's hand-scheduled `memcpy`, `memset` and `memcmp` against reference loops for every size and alignment. [`cdda-read-contention`](https://github.com/EBonura/PSoXide/blob/main/sdk/examples/cdda-read-contention/src/main.rs) issues a data read while CD audio is playing and records which interrupt the drive raises, so one binary can be compared across emulators and a console.

### Sound effects that don't end

[`psx-sfx`](https://github.com/EBonura/PSoXide/blob/main/sdk/crates/psx-sfx/src/lib.rs) exists because four programs on the demo disc had each written their own sample playback and got the same hardware details wrong. On the console, a one-shot sample's END flag doesn't mute the voice: it drops into the envelope's release phase, so a slow release keeps the voice playing past its own data, and a fast release only hides that. The voice's repeat address is also latched only from a block with the loop-start flag, which a one-shot doesn't have. So psx-sfx writes the repeat address itself, and silences each voice on a clock when its sample runs out rather than trusting the envelope (3 August 2026).

A console capture then showed the demo disc menu's browse sound playing the start of the next sample in the bank after it ended, even with the repeat register pointing at a shared silent block. psx-sfx now appends a self-looping silent block, with the loop-start flag set, after every sample (4 August 2026). Another capture on 3 August showed the hardware caps the pitch register at `0x3FFF`, so the cutoff clock now uses the clamped pitch; a sample asked to play faster would otherwise have been cut off early.

## The Hardware Tests disc

The [Hardware Tests disc](https://github.com/EBonura/PSoXide-editor/tree/main/engine/examples/hardware-tests) runs the same executable in PSoXide and on a real PlayStation. The current version in source is **v1.27** (4 October 2026). It lives in the editor repository and isn't on the public demo disc. Build it there with `make hardware-tests-disc`, then burn the BIN/CUE from `build/examples/mipsel-sony-psx/release/`.

{{<figure src="img/shots/hwtests.png" alt="The Hardware Tests v1.24 main menu, listing RUN ALL TESTS + CAPTURE, FULL CHARACTERISATION CAPTURE, CONTROLLER TEST and other entries" native={true} width={320} height={240} caption="The main menu in v1.24. Later versions add MDEC DIAGNOSTIC and FMV STREAM TEST rows." />}}

The disc boots to a menu and measures nothing until you pick an entry. **RUN ALL TESTS + CAPTURE** runs the standing battery of conformance cases; **FULL CHARACTERISATION CAPTURE** adds timing records, precision values and a register snapshot. There are also targeted probes for the SPU, controller timing and CD, hardware scans of the CPU, GTE and SPU registers, a controller and analog-drift test, video levels for checking a capture chain, and a memory card diagnostic behind a consent screen.

With no serial cable to get results off the console, the disc encodes its capture as a set of QR codes, each page carrying a CRC, and you record the TV output; [`hwtest-video-qr.py`](https://github.com/EBonura/PSoXide-editor/blob/main/tools/hwtest-video-qr.py) recovers the pages from the recording. As a second path, the disc can loop the same bytes out of the SPU as an FSK audio signal, which [`hwtest-audio-decode.py`](https://github.com/EBonura/PSoXide-editor/blob/main/tools/hwtest-audio-decode.py) turns back into the same page format. `make hwtest-silicon SILICON=<pages>` then diffs a console capture against the emulator's, record by record, and `make hwtest-diff` gates the emulator against a checked-in baseline.

Every capture carries a suite version, and the report tool refuses to compare captures across a major version, because a record ID can change meaning without the byte layout changing ([`hardware-test-versions.md`](https://github.com/EBonura/PSoXide-editor/blob/main/docs/hardware-test-versions.md)). Timing records also move when unrelated code moves. The layout of the executable changes which instruction-cache lines a probe shares: on 17 September 2026, a commit of lint fixes that touched no probe moved 104 of 151 timing minima in the emulator, by up to 40 cycles. `make hwtest-verify-code` digests the instructions between each probe's markers, so a moved number with unchanged code reads as a re-baseline, not a regression. The full operator guide is [`hardware-test-disc.md`](https://github.com/EBonura/PSoXide-editor/blob/main/docs/hardware-test-disc.md).

## What the console captures showed

The emulator repository's [`emulator-accuracy-from-silicon.md`](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/emulator-accuracy-from-silicon.md) lists places where a console and PSoXide provably disagreed, each with what was measured and how to reproduce it. It covers captures up to 7 August 2026. The editor repository keeps a [longer copy](https://github.com/EBonura/PSoXide-editor/blob/main/docs/emulator-accuracy-from-silicon.md) that continues with the September captures.

The first complete capture (26 July 2026, HWTEST v1.4) matched all 22 bit-exact raster hashes: every primitive family the disc draws came out pixel-identical. It also showed the CD model was wrong in both directions, with seeks far too fast and data reads about four times too slow. After a further capture on 31 July, the CD timing was recalibrated against the console's own answers; the commit reports every CD record within a few percent, apart from a known 12% residual on streaming reads, left because slowing the sector rate would starve XA audio (emulator commit [`648d8f0`](https://github.com/EBonura/PSoXide-emulator/commit/648d8f0)).

A recording of the v1.2 disc on 26 July showed that an all-zero SPU envelope decays to silence on the console in about three seconds, where PSoXide held the note indefinitely. The SDK's `Adsr::passthrough()` had documented exactly the opposite, and the test disc itself had used it and lost 80% of its payload on its first console capture. The SDK documentation is corrected ([`psx-spu`](https://github.com/EBonura/PSoXide/blob/main/sdk/crates/psx-spu/src/lib.rs)). The same document records a timer bug found by comparing captures (timer registers read without first advancing the timer to the current cycle), root-caused but listed as open.

A capture on 7 August 2026 found that a 64-byte DMA upload to sound RAM landed only its first 6 bytes. That matched a continuous 1575 Hz tone on the demo disc: psx-sfx's silent parking block had been corrupted, so a finished voice looped an audible tone instead of silence. psx-spu now waits until the transfer has really drained before resetting the transfer mode (SDK commit [`f9f520e1e`](https://github.com/EBonura/PSoXide/commit/f9f520e1e)). The emulator passes those upload tests, so on this point it's the permissive one, and only a console run can check the fix.

{% <callout kind="warn" title="Limits"> %}
The captures come from a small number of consoles, and some behaviour differs between models, so a single capture is one console's answer. Several probes only mean something on hardware: the emulator can pass them whether or not the code is right. Emulator checks don't replace testing on original hardware.
{% </callout> %}
