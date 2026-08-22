# Instruction-level optimisation survey: PSoXide, quake-psx, hl-psx (2026-08-22)

Consultant survey for a working agent. The question it answers: given that we
own the whole stack (SDK, engine, emulator with a silicon-calibrated cycle
model, three shipping guests) and can reason down to individual R3000A
instructions, where can hand-written assembly or instruction-level codegen
work still move the needle, and where has the evidence already closed the door.

Everything here is a claim to be verified, not an instruction to implement.
Each proposal carries: where, evidence, mechanism, expected effect (derived,
not measured unless stated), risk, an experiment recipe, and the gate it must
pass. Numbers without a source are derived from the static census in section 3
and are labelled as such. Nothing in this document has been measured on a
route unless the sentence says so.

Paths are relative to `~/Desktop/repos/` (`PSoXide/`, `quake-psx/`, `hl-psx/`).

---

## 0. Executive summary

1. The three guests are compiled by the same LLVM 22.1 MIPS-I backend and
   show the same static signature: about 11% of linked text is `nop`, roughly
   half of all branch/jump delay slots are unfilled, 28-30% of loads are
   followed by a `nop`, 60-70% of `mult` instructions read HI/LO on the very
   next instruction (full 6-13 cycle interlock exposed), and 13.5-14.6% of all
   instructions are `$sp`-relative loads/stores (spills, reloads, stack args).
   Those four categories are exactly what hand scheduling removes. Section 3.
2. The tree already contains the right kind of assembly: hl-psx's
   `global_asm!` world walkers, the SDK's `.word`-encoded COP2 schedules with
   console-confirmed hazard gaps, asm OT insertion loops, a scheduled Q12
   divide. The pattern to copy is established; the gap is coverage and
   measurement, not technique. Section 2.
3. The cheapest wide wins are not assembly at all: dynamic instruction-class
   counters in the emulator (so the static census becomes per-route truth),
   symbolised PC sampling for all three games, `panic=immediate-abort` for the
   two games that still use `abort`, a `$gp`-relative small-data experiment,
   a packed-record audit (the quake alias vertex loop assembles words from
   bytes, 4 bus loads where 1 would do), and an i64 audit of the collision
   trace. Section 5, tiers 0 and 1.
4. The highest-value hand-asm kernel is cortex's CPU-blended vertex path
   (64% of the player's vertices, ~590 cycles each, ~95k of the ~105k
   project-stage cycles per render vblank). The prior batching attempt lost
   because it round-tripped through RAM; the register-resident design using
   both spare GTE matrix slots (LLM/BK and LCM/FC) was proposed and never
   built. Section 5.2.1.
5. A second kernel, a flat model-face walker in the style of hl-psx's
   `__hlpsx_walk_ordinary_quads`, targets cortex's faces stage (~142-146k per
   render vblank, ~294 cycles/face). Section 5.2.2.
6. hl-psx's own evidence says its remaining gap is architectural (cooked
   draw-ready records, persistent packets, coarse ordering, phase-grouped
   loops; `hl-psx/docs/ps1-throughput-lessons.md`). Instruction-level work
   there is limited to tightening the existing asm walker (the guard-clamp
   chain alone is ~48 instructions per quad that can be ~12). Section 5.2.3.
7. A long list of things must not be retried: scratch-array batching, NCLIP
   for culling in transform-once paths, per-face DIV hoisting, RT/TR hoists
   across hazard-sensitive compose paths, pose caches, out-of-line factoring
   of hot emitters, cold sections, branch hints, templates inside the arena,
   `opt-level = "s"`. Section 6.

---

## 1. Ground truth

### 1.1 Toolchain and codegen settings (all three guests)

| Item | Value | Source |
| --- | --- | --- |
| Toolchain | `nightly-2026-03-25`, rustc 1.96.0-nightly, LLVM 22.1.0 | `PSoXide/rust-toolchain.toml`, `rustc -Vv` |
| Target | `mipsel-sony-psx`: `cpu = "mips1"`, `features = "+soft-float"`, static relocation, `rust-lld`, `panic-strategy = abort`, no atomics | `rustc --print target-spec-json` |
| Profile | `codegen-units = 1`, `lto = true`, `panic = "abort"` | `quake-psx/game/Cargo.toml:16`, `hl-psx/game/Cargo.toml:22`, `PSoXide/engine/examples/editor-playtest/Cargo.toml:13` |
| Cortex extra | `-Cpanic=immediate-abort` (RUSTFLAGS) | `PSoXide/tools/build_guest_staged.sh:39` |
| build-std | `core` (+`alloc` for cortex/quake), `compiler-builtins-mem` | `PSoXide/Makefile:309-310`, the games' `.cargo/config.toml` |
| Link | `-T sdk/psoxide.ld`, `--oformat=binary` (no ELF, no symbols) | `PSoXide/sdk/psoxide.ld`, `quake-psx/game/build.rs:141`, `hl-psx/game/build.rs:913` |
| `opt-level` | 3 (default). `"s"` is known to MISCOMPILE the guest; never use it | memory note `sanctum-walkthrough-findings-2026-08-15` |
| i64 | `__divdi3`/`__moddi3` from compiler-builtins are broken on this target; `psx-rt/src/builtins.rs` overrides them (correct, still slow) | `PSoXide/sdk/crates/psx-rt/src/builtins.rs` |

Note the consequence of `--oformat=binary`: none of the shipped guests have
symbols. `tools/pc_symbolize.py` documents the fix (relink the same sources as
an ELF by dropping `--oformat=binary`), and references a `make perf-symbols`
target that does not exist in the current `Makefile`. See 5.0.2.

### 1.2 What the emulator charges (the cost model every proposal is priced in)

All values are from `PSoXide/emu/crates/emulator-core/src/cpu.rs`,
`cpu/timing.rs`, `bus.rs`, `bus/memory_timing.rs`, `cpu/icache.rs` and
`sdk/crates/psx-gte-core/src/state.rs`. Memory note
`emulator-matches-console-perf` records that the user's console tracks this
model; the remaining blind spot is GPU draw time (fill-rate regime only).

| Event | Cost | Where |
| --- | --- | --- |
| Any instruction issue | 1 cycle (`BIAS = 1`) | `cpu/timing.rs:17` |
| Load from main RAM (any width) | +6 wait cycles (7 total) | `memory_timing.rs:170-176` ("8 clocks per load+delay-slot pair") |
| Store to main RAM | +1 wait cycle (2 total) | `bus.rs:1343-1360` |
| Load/store to scratchpad (`0x1f80_0000`, 1 KiB) | +0 | `memory_timing.rs:177-182` |
| DRAM refresh collision | +8 cached / +4 uncached, every 515 cycles if a RAM access lands in the 10-cycle window | `memory_timing.rs:25-60`, `bus.rs:1303-1330` |
| Load delay | one slot; the next instruction sees the OLD register value (no interlock) | `cpu.rs:154-163` |
| `MULT`/`MULTU` | 6 cycles if |rs| < 2^11, 9 if < 2^20, else 13; `MFLO`/`MFHI` stall until done | `cpu.rs:2640-2651`, `cpu.rs:1570-1603` |
| `DIV`/`DIVU` | 36 cycles, operand-independent | `cpu.rs:2621` |
| GTE command while previous in flight | stall to completion (`gte_sync`) | `cpu.rs:1225-1258` |
| GTE op latencies | RTPS 15, RTPT 23, NCLIP 8, MVMVA 8, AVSZ3 5, AVSZ4 6, SQR 5, GPF 5, GPL 5, OP 6, DPCS 8, INTPL 8, NCDS 19, NCCS 17, NCS 14, NCDT 44, NCCT 39, NCT 30, CC 11, CDP 13, DCPL 8, DPCT 17 | `psx-gte-core/src/state.rs:244-270` |
| `MFC2` of SXY/SZ/MAC1-3/IR after an op | live value, no stall | `cpu.rs:1365-1369` |
| `MFC2` of MAC0 (24) / LZCR (31) too early | STALE value (no stall) | `cpu.rs:1376-1379`; silicon: NCLIP->MAC0 needs 8 instructions (`psx-gte/src/scene.rs:1060-1071`) |
| `MFC2`/`CFC2` result | one load-delay slot like `LW` | `cpu.rs:1352-1362` |
| `MTC2` VXY0 then RTPS/MVMVA within 2 instructions | stale V0.x on silicon (HWB-010/011); SDK pads 2 NOPs | `scene.rs:459-463, 554-558, 663-668`; emulator `cpu.rs:233-252` |
| I-cache | 4 KiB direct-mapped, 256 lines x 4 words, per-word valid bits, refill charged per word | `cpu/icache.rs:1-60`, `bus.rs:1378-1390` |
| Uncached fetch (KSEG1 code) | +RAM wait per instruction | `bus.rs:1363-1375` |

Two consequences to keep in front of every proposal:

- Removing one RAM load saves ~7 cycles; removing one ALU instruction saves 1.
  This is why `docs/perf-30fps.md` records, twice, "per-primitive wins must
  remove MEMORY traffic, not ALU ops".
- Reading RTPT/MVMVA results immediately is free in the model and is the
  shipped, console-verified schedule (`scene.rs:499-518`, hl-psx
  `world_pipeline.rs:34-48`). The 23-cycle RTPT latency is only paid if the
  NEXT COP2 command issues inside the window. Software-pipelining the read is
  therefore NOT a lever by itself; spacing consecutive GTE commands with
  independent CPU work is. (`docs/perf-30fps.md:420-424` assumed the read
  eats the latency; the model does not agree. Verify on the hardware-test disc
  timing scan before relying on either reading; see 5.0.4.)

### 1.3 Measurement instruments that exist today

| Instrument | What it gives | Entry point |
| --- | --- | --- |
| Per-vblank stage profile | cycles per telemetry stage per vblank (CSV) | `frontend launch --profile-log` (`docs/playtest-profiling.md`) |
| Aggregate stage + GTE profile | stage totals, GTE op counts and estimated cycles | `--dump-guest-profile` |
| PC sampling | PC histogram every N retired instructions, optional `$ra` call-site and route-window variants | `--pc-sample-log`, `--pc-sample-callsite-log`, `--pc-sample-window-log`, `--pc-sample-instructions` (`frontend/src/cli.rs:347-369`) |
| I-cache counters | refill events, words, stall cycles per route tick | route log columns `icache_refill_*` (`cli.rs:1107`), `Cpu::instruction_cache_profile` |
| Stale-read tracing | which PC read MAC0/LZCR early | `PSOXIDE_TRACE_STALE=1` (`cpu.rs:1370-1396`) |
| Hazard repro | force the V0.x commit hazard | `PSOXIDE_GTE_V0X_STALE` (`cpu.rs:368`) |
| Silicon timing | 129 timing records incl. CPU/GTE/DMA, QR transport | `engine/examples/hardware-tests`, `docs/hardware-test-disc.md` |
| Machine-code audit | digests of measured instruction spans in the linked EXE | `tools/verify-hwtest-machine-code.py` |
| Static census | this survey's counts | `tools/instr_census.py` (added with this document) |
| Game gates | hl: `psoxide-profile`, `psoxide-map-smoke`, `psoxide-chart` (`hl-psx/Makefile:95-122`); quake: `visual-parity-regress`, fixed-tick E1M1 route; cortex: Arena/E1M1 route per `docs/shared-engine-standardisation-2026-08-22.md` | |

---

## 2. Hand-written assembly already in the tree

Do not duplicate any of these; extend them.

| Site | What it does | Why it exists |
| --- | --- | --- |
| `PSoXide/sdk/crates/psx-gte/src/regs.rs` | `mtc2!/mfc2!/ctc2!/cfc2!` macros, `.word`-encoded, pinned to `$8`, MFC2/CFC2 followed by a NOP | LLVM's `mipsel-sony-psx` target rejects COP2 mnemonics; load-delay on MFC2 |
| `psx-gte/src/ops.rs` | every cofun as a `.word` | same |
| `psx-gte/src/scene.rs:451-474, 499-518, 550-570, 595-630, 659-687, 803-820, 845-858, 877-896` | scheduled RTPT/RTPS/MVMVA/AVSZ3/AVSZ4 with the HWB-010/011 two-NOP input-commit gap and chained MFC2 reads sharing one delay NOP; `classic_otz3_from_sum` (multiply by 0x155 as shifts/adds) | console-confirmed hazards; keep reads in one block |
| `scene.rs:1083-1258` | hardware-safe NCLIP variants that fill the 8-instruction MAC0 gap with SZ loads | NCLIP MAC0 stale on silicon |
| `PSoXide/sdk/crates/psx-gpu/src/ot.rs:98-120, 217-250, 304-340, 390-430` | OT clear (8 stores per branch), packed-command insert forward/reverse, tagged-packet-stream insert; load delays filled with address shifts | the per-packet OT splice is the innermost loop of every renderer |
| `PSoXide/engine/crates/psx-engine/src/fixed.rs:156-182` | 12-step Q12 long-division fraction with the shift in the branch delay slot | replaces a 36-cycle DIV plus call |
| `PSoXide/engine/crates/psx-bsp/src/render.rs:365-410` | exact sign of a 64-bit plane distance accumulated in HI:LO with carries, small operand in `rs` | avoids LLVM's i64 scaffolding; operand-sensitive MULT |
| `PSoXide/sdk/crates/psx-asset/src/lib.rs:1480-1497`, `hmd8.rs:64-79`, `hl-psx/game/src/model.rs:47-76` | branchless Q11 decode (6 instructions; LLVM emitted a branch) | |
| `PSoXide/sdk/crates/psx-rt/src/{cache.rs,interrupts.rs,bios.rs}` | I-cache flush via KSEG1 alias + IsC, VBlank ISR on `$k0/$k1` without a frame, BIOS trampolines | runtime |
| `PSoXide/engine/crates/psx-engine/src/scratchpad.rs:23-27` | absolute symbol `__psoxide_scratchpad = 0x1f800000` so LLVM addresses it as an object | shared by all three games since 2026-08-22 |
| `quake-psx/crates/quake-core/src/liquid.rs:81-120` | 64x64 liquid warp resample, two texels per branch | |
| `hl-psx/game/src/world_pipeline.rs` | four `global_asm!` kernels: `__hlpsx_project_local_vertices` (8-byte vertex stream -> RTPT -> packed XY + depth, close-repair flag), `__hlpsx_project_soa_vertices`, `__hlpsx_admit_source_face_runs` (sphere-vs-frustum admission on MVMVA), `__hlpsx_walk_ordinary_quads` (the flat quad walker, 331 static instructions as of 2026-08-22; the hl doc quotes 1,228 B for an earlier revision: index fetch, near/guard reject, software winding, packet XY writes, max-depth OT key, fog colour, OT splice) | the only body in the codebase shaped like the retail loop (`ps1-throughput-lessons.md`) |
| `hl-psx/game/src/scratchpad.rs:25-45` | trampoline that runs a Rust entry on a stack carved from the scratchpad, with a guard canary | scratchpad-resident projection stack (A/B feature `main-ram-projection-stack`) |
| `hl-psx/game/src/render.rs:510-519`, `main.rs:20960-20968, 24946-24954` | `b 2f; .rept N nop` pads to pin later functions' I-cache addresses after a size change | direct-mapped cache layout control by hand |

Also note `quake-psx/crates/quake-core/src/world_batch.rs`: not asm, but a
hand-shaped in-place packet writer whose every word is one load and one store,
with all loads hoisted above the first store so the aliasing-unaware compiler
does not serialise them behind load delays (`world_batch.rs:172-178`). It is
host-proven byte-equivalent to the SDK writer. That is the Rust-level
ceiling; beyond it is asm.

---

## 3. Static instruction census (new data, 2026-08-22)

Method: `tools/instr_census.py` strips the PSX-EXE header, disassembles with
`mipsel-none-elf-objdump -m mips:3000`, and locates the `.text`/`.data`
boundary by the first 256-word window with >=16 undecodable words (the linker
script places `.text` first). Static counts over linked text; they say nothing
about how often each instruction retires. Binaries: the three `.exe` files
built on 2026-08-22 (`quake-psx/game/target/.../quake-psx.exe`,
`hl-psx/game/target/.../hl-psx.exe`,
`PSoXide/build/examples/mipsel-sony-psx/release/editor-playtest.exe`).

| Metric | quake-psx | hl-psx | cortex (editor-playtest) |
| --- | ---: | ---: | ---: |
| payload / text / data (bytes) | 491,520 / ~440,576 / ~50,944 | 636,928 / ~580,352 / ~56,576 | 1,654,784 / ~794,880 / ~859,904 |
| instructions in text | 110,144 | 145,088 | 198,720 |
| `nop` (share of text) | 11,801 (10.7%) | 15,414 (10.6%) | 22,335 (11.2%) |
| of which in branch/jump delay slots | 6,926 | 8,976 | 13,441 |
| of which after a load (load-delay fill) | 4,785 | 6,333 | 8,732 |
| of which deliberate COP2 gaps | 57 | 36 | 66 |
| branches+jumps, slot is `nop` | 14,706, 47.1% | 18,580, 48.3% | 25,962, 51.8% |
| loads, followed by `nop` | 15,794, 30.3% | 22,013, 28.7% | 31,608, 27.6% |
| stores | 13,976 (12.7%) | 17,468 (12.0%) | 26,240 (13.2%) |
| `$sp`-relative `lw`/`sw` | 15,779 (14.3%) | 19,622 (13.5%) | 29,023 (14.6%) |
| `lwl/lwr/swl/swr` | 994 | 1,334 | 2,330 |
| `mult/multu/div/divu` | 1,018/75/45/117 | 1,996/213/161/138 | 1,889/187/262/152 |
| `mult` immediately followed by `mflo/mfhi` | 665 (60.8%) | 1,401 (63.4%) | 1,341 (64.6%) |
| COP2 `mtc2/mfc2/ctc2/cofun` | 156/158/129/49 | 94/105/100/36 | 215/199/150/64 |
| `jal` / `jalr` / `jr` | 2,116 / 18 / 495 | 2,812 / 25 / 616 | 1,514 / 0 / 438 |
| `lui` (absolute address/constant materialisation) | 3,337 (3.0%) | 6,638 (4.6%) | 5,006 (2.5%) |
| `sll 16; sra 16` i16 sign-extend pairs | 228 | 133 | 311 |
| stack frames: count / median / p90 / max | 217 / 40 B / 248 B / 31,136 B | 334 / 56 B / 248 B / 16,496 B | 198 / 88 B / 656 B / 13,704 B |

What the numbers say, and what they do not:

- Unfilled delay slots. LLVM's MIPS delay-slot filler leaves about half of all
  branch and jump slots as `nop`, and the MIPS-I hazard handling leaves a
  `nop` after 28-30% of loads. A hand-scheduled loop fills nearly all of
  these (hl's walkers show the pattern: pointer increments and independent
  address arithmetic sit in the slots). Per iteration of a hot loop this is
  typically 10-25% of the issued instructions. This is the single largest
  mechanical gain hand asm offers on this CPU, and it is invisible to
  source-level Rust changes.
- Exposed multiply latency. 61-65% of `mult` sites read HI/LO on the next
  instruction; each such pair stalls 5-12 cycles. Hand asm interleaves
  independent work (the psx-bsp distance kernel and hl's winding test do
  this). `sdk/crates/psx-math/src/int32.rs:116` (`mul_q12_i32_wide`) and the
  i64 plane distances are the common sources.
- Spill traffic. 13.5-14.6% of all instructions touch the stack, and every
  one is a ~7-cycle bus load or a 2-cycle store. Part of this is ABI
  (argument spills, callee-saved saves) and unavoidable in Rust; inside a
  flat asm kernel with its own register plan it is zero. Frame sizes up to
  31 KiB (quake) and 13-16 KiB (hl, cortex) mark functions that build large
  stack arrays each call; those are worth a look with PC samples.
- Unaligned access. 1,000-2,300 `lwl/lwr/swl/swr` sites: each pair is two
  bus transactions for one word. Sources are `#[repr(C, packed)]` records
  and byte-slice `from_le_bytes` reads. See 5.1.5.
- Byte-assembled words. The quake alias-model RTPT loop at `0x800128a0`
  (dis of `quake-psx.exe`) consumes three 3-byte `ClassicAliasVertex`
  records per iteration (`addiu t6,t6,9`, `classic_affine.rs:437-440`) and
  assembles each GTE input word from `lbu` + `sll` + `or`: six byte loads
  plus shuffles for the three XY words, three more for the Z halves, where
  word-strided records would need one `lw` + one `lhu` per vertex.
  `ClassicAffineSourceVertex` (`classic_affine.rs:47-56`) is the 10-byte
  packed equivalent on the world side. Section 5.1.5.
- Address materialisation. 2.5-4.6% `lui`, mostly `lui + addiu/lw/sw`
  pairs to reach statics (static relocation model, no `$gp`-relative data).
  Section 5.1.3.

The census is static. Before acting on any line of this table the working
agent must get the dynamic counterpart (5.0.1) from a real route so that the
hot loops, not the whole binary, drive the priorities.

---

## 4. Architecture rules that decide what assembly can win here

Compiled from the cost model (1.2), the silicon findings in `psx-gte/scene.rs`,
`docs/perf-30fps.md` and `hl-psx/docs/ps1-throughput-lessons.md`.

1. The R3000A in the PS1 has no data cache. Every load is ~7 cycles. Keep a
   kernel's working set in the 32 GPRs plus the 1 KiB scratchpad; never stage
   through main RAM between passes (this is what killed the batched blend,
   `perf-30fps.md:468-487`).
2. The instruction cache is 4 KiB direct-mapped. What matters is the
   contiguity of the footprint touched per iteration, not total size; a loop
   that calls two helpers in other lines conflicts with itself. Measured
   three ways in hl-psx (`ps1-throughput-lessons.md`, "Two falsifiable
   attempts"). A hand kernel is therefore one straight-line body with no
   calls, placed in its own section so the linker keeps it contiguous.
3. Delay slots are free issue slots; fill every one. Load-delay slots may
   hold any instruction that does not read the loaded register (hl's walkers
   put address arithmetic there); branch-delay slots may hold the loop
   increment or the next iteration's first load.
4. MULT latency is operand-sensitive (6/9/13 by |rs|); put the small operand
   in `rs` and do at least 6 cycles of independent work before `mflo`.
   DIV is 36 cycles flat: a 32-bit divide costs more than five RAM loads;
   replace with reciprocal multiply when the divisor is loop-invariant and
   the domain is bounded, or with the 12-step scheduled division in
   `fixed.rs` when only a Q12 fraction is needed.
5. GTE results (SXY, SZ, MAC1-3, IR) read back immediately; MAC0 and LZCR do
   not (8-instruction gap after NCLIP on silicon). A new GTE command stalls
   until the previous one retires, so interleave at least the op's latency
   of CPU work between consecutive commands (RTPT 23, RTPS 15, MVMVA 8).
6. An MTC2 into V0/V1/V2 (or SZ/IR for AVSZ/SQR) must be at least four
   instructions (two NOPs or two useful instructions) before the op that
   consumes it (HWB-010/011). This is a hard correctness rule, console
   confirmed, modelled by the emulator only with `PSOXIDE_GTE_V0X_STALE`.
7. Register budget for a `global_asm!` kernel under o32: `$a0-$a3` args,
   `$v0/$v1` return, `$t0-$t9` free, `$s0-$s7` callee-saved (save/restore
   on entry), `$at` usable under `.set noat` (psx-bsp does), `$fp` (`$30`)
   free if saved, `$gp` (`$28`) unused by this static build and never set by
   `_start` (verify: grep `$28`/`gp` in `psx-rt` and the games; the PSX-EXE
   header GP field is 0, `psoxide.ld:56`). `$k0/$k1` are owned by the VBlank
   ISR (`interrupts.rs:19-28`); never use them. A kernel therefore plans
   up to 25 registers with no spills; the compiler, which must assume calls
   clobber `$t*`, spill around every `jal`, and leave `$at/$gp/$fp/$k*`
   alone, is where the 13.5-14.6% `$sp` traffic in section 3 comes from.
8. Writes to the GPU packet arena are 2 cycles; reads from it are 7. Build
   packets from registers, never read-modify-write them (the OT splice in
   `ot.rs` reads the OT head once and writes twice; that is the minimum).
9. Scratchpad is the only zero-wait memory. All three games already use it
   for per-batch vertex scratch (quake 1,020 B, cortex 896 B, hl
   phase-overlapped) and hl runs a projection stack there. Budget is fully
   spoken for per phase; a new kernel must either fit in registers or claim
   an audited phase window.
10. Anything that changes a GTE schedule in a path that has exposed a silicon
    hazard is not bit-neutral in the emulator either (the CTC2 commit-delay
    model made a RT/TR hoist fail the pixel gate by 3 pixels,
    `perf-30fps.md:689-699`). Such changes need the hardware-test disc, not
    just the emulator gate.

---

## 5. Proposals

Ordered by expected value per unit risk. "Expected" figures are derived
from counts in section 3 and stage costs in `docs/playtest-profiling.md` and
`docs/perf-30fps.md`; they are targets to measure against, not predictions.

### 5.0 Tier 0: instruments to build first (small, prerequisite)

#### 5.0.1 Dynamic instruction-class counters in the emulator

- Where: `emulator-core/src/cpu.rs` (`step`/dispatch), surfaced by
  `--dump-guest-profile` and as per-route-tick columns next to the I-cache
  counters in `cli.rs:1107`.
- Count, per retired instruction: `nop` in branch-delay slot, `nop` after a
  load/MFC2, `$sp`-relative load/store, `lwl/lwr/swl/swr`, `mult/div` and
  the cycles actually stalled in `MFLO/MFHI` (`cpu.rs:1570-1603` already
  computes the stall), cycles stalled in `gte_sync`, RAM load count vs
  scratchpad load count, `jal/jalr` count, byte/halfword loads (for the
  packed-record audit).
- Why first: the static census cannot rank hot loops. With these counters a
  route reports "X% of cycles are unfilled delay slots, Y% are multiply
  interlock, Z% are RAM loads in the render band", which is the ROI of a
  hand kernel before writing it. Also makes every later A/B falsifiable on
  the mechanism, not just on frame time.
- Risk: negligible; the counters must be feature-gated or cheap (a few
  adds per instruction). Keep them out of the web build if they cost
  measurably.
- Gate: counters must not change emulator timing (identical `--profile-log`
  and hashes with and without).

#### 5.0.2 Symbolised PC sampling for all three games

- `tools/pc_symbolize.py` exists and needs an ELF. Add a `perf-symbols`
  Makefile target (referenced in its docstring, absent in `Makefile`) that
  relinks `editor-playtest` without `--oformat=binary` (same staged source,
  same RUSTFLAGS otherwise) and runs `pc_symbolize.py`. Do the same in
  `quake-psx/game/build.rs:141` and `hl-psx/game/build.rs:913` behind an env
  var (hl already has a PC-sampling profile flow; reuse it).
- Verify the ELF's code is byte-identical to the shipped binary payload
  (compare `.text+.data` against the `.exe` after the header); otherwise the
  samples describe a different layout.
- Output wanted per game: top-40 functions by samples on the canonical route,
  and for each, its linked size and I-cache set range (5.0.3).

#### 5.0.3 I-cache set-conflict map

- From the ELF symbol table compute each function's 16-byte line set range
  (address bits 4..11). Cross with the PC-sample top list to report pairs of
  simultaneously-hot functions that alias. hl-psx currently controls this by
  hand-inserted `.rept N nop` pads (`render.rs:510`, `main.rs:20960`,
  `main.rs:24946`). A map turns that into a deliberate linker-script section
  order (`*(.text.__hlpsx_*)` first, then hot Rust symbols) instead of pads.
- Evidence this matters: hl's renderer-bound windows run at 42-48% I-cache
  stall cycles (`ps1-throughput-lessons.md`, window census).

#### 5.0.4 Settle the "MFC2 right after RTPT" question on silicon

- The emulator charges nothing for an immediate SXY/SZ read (1.2). The
  perf-30fps diagnosis assumed a 23-cycle exposure per triple. The
  hardware-test disc has GTE timing records 0x21-0x26
  (`timed_gte_rtps/rtpt/nclip/mvmva/ncdt/ncct_commands`,
  `hardware-tests/src/main.rs:3868-3893` and the `timed_gte_commands!`
  macro at `main.rs:9972`), but they time N back-to-back COMMANDS
  (throughput), not a command followed by a result read. Add two records:
  "RTPT; MFC2 SXY2; MFC2 SZ3" and "RTPT; 23 NOPs; MFC2 SXY2", and the MVMVA
  equivalents. Whichever is true decides whether pipelined RTPT (kick N,
  read N-1) is a lever or a no-op, and it constrains 5.2.1 and 5.2.2. Bump
  the suite version per `docs/hardware-test-versions.md`.

### 5.1 Tier 1: codegen-wide changes (no assembly, all three games)

#### 5.1.1 `-Cpanic=immediate-abort` for quake-psx and hl-psx

- Cortex already builds with it (`build_guest_staged.sh:39`); quake and hl
  use `panic = "abort"`, which still links the formatting machinery behind
  every bounds check and `unwrap`. Expected: smaller text (measure), fewer
  cold blocks interleaved with hot code, fewer `jal` targets.
- Gate: byte-exact visual/route gates; linked-size and I-cache stall deltas;
  confirm the games do not rely on panic messages reaching the TTY in any
  shipped diagnostic path (hl uses `tty::println` widely; panics are
  separate).

#### 5.1.2 Sample-based PGO from emulator PC samples

- Mechanism: `rustc -C profile-sample-use=<profdata>` accepts an LLVM sample
  profile (AutoFDO). The emulator's `--pc-sample-log` is a PC histogram; with
  an ELF built with line tables (`-C debuginfo=1`) each PC maps to
  `function:line.discriminator` via `addr2line`/`llvm-symbolizer`, and the
  text sample-profile format can be written directly and converted with
  `llvm-profdata merge --sample`. No guest instrumentation, no runtime.
- What it changes: inlining decisions, block layout, hot/cold splitting,
  spill placement. hl-psx measured that `core::hint::unlikely` layout hints
  were exactly neutral, so do not expect layout gains; the value, if any, is
  in inlining and register allocation priorities in the emitters.
- Risk: non-deterministic builds if the profile drifts; code layout changes
  alter I-cache behaviour, which can move deadline misses either way (the
  standardisation doc recorded a dependency-only change that moved sim
  ticks 288 -> 370). Gate on the dynamic counters, not on FPS alone.
- Order: after 5.0.1/5.0.2, since both are needed to build the profile.

#### 5.1.3 `$gp`-relative small data

- Static-relocation MIPS code reaches every global through `lui + addiu`
  or `lui + lw/sw` (3,337-6,638 `lui` sites). With a `$gp` base and
  `.sdata/.sbss` sections, globals within 64 KiB become a single
  `lw rt, off($gp)`. LLVM supports this for MIPS via `-mllvm -mgpopt` and
  `-mllvm -mips-ssection-threshold=N` (`-C llvm-args=...` in rustc); it
  requires `_gp` defined in `psoxide.ld` and `$gp` initialised in `_start`
  (`psx-rt/src/lib.rs`) and preserved by the ISR.
- Candidates are the small hot statics: OT heads, arena cursors, frame
  counters, telemetry stage accumulators, `PVS_*` cursors in hl.
- Risks: confirm LLVM 22 honours `-mgpopt` on the `mipsel-sony-psx` triple
  (it may be gated to non-PIC o32, which this is); confirm the `asm!`
  blocks that clobber `$28` (none found in the survey; re-grep) and that
  compiler-builtins' memcpy/memset do not touch `$gp`; the PSX-EXE header GP
  field (`psoxide.ld:56`) should carry `_gp` for BIOS-loaded EXEs.
- Expected: fewer instructions and fewer I-cache words in every loop that
  touches a global; measure `lui` retired before/after with 5.0.1.

#### 5.1.4 i64 audit in per-frame paths

- `psx-bsp/src/collision.rs:709-731` computes the trace fraction with
  `(numerator << 12) / denominator` on i64 and an i64 `saturating_mul` /
  divide for the interpolation. Each i64 divide is a call to `__udivdi3`
  (software loop; memory note `psx-rt-i64-divide-fix`: "still slow").
  `third_person_camera.rs:522` and `:2329-2330` divide i64 per frame.
- Replace with the same HI:LO accumulation pattern as
  `psx-bsp/render.rs:365-410` plus a 32-bit reciprocal or the `fixed.rs`
  12-step Q12 fraction (the quotient is a fraction in [0, 1] by construction
  at `collision.rs:727`, exactly the precondition `div_q12_fraction` needs).
- Measure: count retired `jal __udivdi3/__divdi3/__umoddi3` with 5.0.2 on
  the cortex sim band (sim solve 98k + collision 53k per render vblank,
  `docs/demo10-low-level-hot-paths-2026-06-02.md`).

#### 5.1.5 Packed-record audit (bytes vs words)

- Evidence: the quake alias loop at `0x800128a0` (6 `lbu`, 2 `sll`, 2 `or`
  per three GTE input words; stride 9). `ClassicAliasVertex` is `[u8; 3]`
  (`classic_affine.rs:437`); `ClassicAffineSourceVertex` is 10 bytes packed
  (`classic_affine.rs:47`); cortex and hl carry 2,330 and 1,334 `lwl/lwr/swl/swr`
  sites respectively, i.e. `read_unaligned` or packed fields in loops.
- Rule: a record the GTE consumes should be laid out as the words the GTE
  takes: `xy: u32 (i16,i16)`, `z: u16`, padding to 8 bytes, so the kernel
  does `lw; lhu` per vertex (exactly what `__hlpsx_project_local_vertices`
  consumes). `ClassicAffineWordSourceVertex` (12 bytes, word-strided,
  `classic_affine.rs:64-74`) already exists for this reason.
- Trade: alias frames double in RAM (4 -> 8 bytes per vertex). Quake's
  resident-arena margin is a gate (`VALIDATION.md`); measure before cooking.
- Measure: retired byte/halfword loads in the model band (5.0.1).

#### 5.1.6 `mfc2!`/`mtc2!` macro call-site audit

- Each `mfc2!` emits `MFC2 $8; NOP` and the compiler adds a `move` out of
  `$8`, so N consecutive reads written with the macro cost ~3N instructions
  instead of N+1 (the scheduled wrappers in `scene.rs` already batch reads
  into one `asm!` block). Grep the engine and games for `mfc2!`/`cfc2!` in
  per-vertex or per-face code and fold them into scheduled blocks. Small,
  mechanical, bit-exact.

#### 5.1.7 memcpy/memset provenance

- `compiler-builtins-mem` provides generic `memcpy`/`memset`. The packet
  writers avoid them (in-place word writes), but the CD sector path
  (PIO pops), pack decompression and any `[T; N]` copy larger than LLVM's
  inline threshold call them. PC-sample the symbols (5.0.2). If hot,
  provide an R3000 `memcpy` in `psx-rt`: word loop unrolled by 4 with the
  branch slot filled, `lwl/lwr` only for the misaligned head, `swl/swr`
  never on the hot path. Only if measured hot.

### 5.2 Tier 2: hand-written kernels

All kernels follow the hl-psx pattern: `global_asm!` in its own
`.text.<name>` section, narrow C ABI over a `#[repr(C)]` context struct with
`const` offset asserts, host fallback in Rust, `#[cfg(target_arch = "mips")]`.
Every kernel ships with a host test that proves byte-equivalence against the
Rust path it replaces (the way `quake-core/world_batch.rs` does), and every
new COP2 schedule goes through the hardware-test disc before a burn.

#### 5.2.1 Cortex: register-resident CPU-blend vertex kernel (highest value)

- Where: `psx-engine/src/render3d.rs:2846-2880`
  (`project_blended_textured_model_vertex`) and its callers in
  `render3d/world_pass_model.rs` (`submit_textured_model_geometry_impl`,
  line 411 onward).
- Evidence: 64% of the player's vertices (161/252) take the CPU-blend path at
  ~590 cycles each, ~95k of the ~104k project-stage cycles per render vblank
  (`perf-30fps.md:448-466`). Decomposition: projection segment (identity RT
  load + zero TR + RTPS wrapper + guards) 264, secondary segment (matrix
  load + MVMVA + 6-multiply lerp) 175, remainder ~33k stage-wide
  (`perf-30fps.md:489-518`). The batching attempt that staged `view_a` in a
  scratch array was bit-exact and 30k WORSE (`perf-30fps.md:468-487`); the
  chunked CPU-blend flush was +4.19% worse (memory note
  `blend-chunk-optimization-pending-verification`).
- Design (proposed in `perf-30fps.md:508-518` as lever 1, never built, now
  generalised): during a part's blended run keep `RT = identity, TR = 0`
  loaded once, put the primary joint in `LLM` + `BK` and the current
  secondary joint in `LCM` + `FC`. Per vertex:
  1. `MTC2 VXY0, VZ0` (2), two useful instructions (prefetch next vertex
     record into registers) to satisfy the commit gap (rule 6),
  2. `MVMVA mx=LLM, v=V0, cv=BK` (8 cycles) -> `MFC2 MAC1..3` (3 + delay),
  3. `MVMVA mx=LCM, v=V0, cv=FC` -> `MFC2 MAC1..3`, issued after >= 8
     instructions of independent work (the lerp of the previous vertex),
  4. lerp on the CPU in registers: `d = b - a` (3 `subu`), 3 `mult d, t`
     with the other vertex's independent work between each `mult` and its
     `mflo` (`t` is Q12 and small, so put `t` in `rs` for the 6-cycle form),
     `a + (p >> 12)` (3),
  5. `MTC2 VXY0/VZ0` of the blended view vertex (packed with 2 `sll/or`),
     gap, `RTPS`, `MFC2 SXY2, SZ3`, store the projected record.
  Matrix/translation loads: 5+3 CTC2 when the secondary joint changes (group
  blended vertices by `joint1` at cook time or sort once at load; the cook
  already controls vertex order), plus one restore of the part's primary RT
  after the run (the unblended RTPT run needs RT = primary).
- Why this is different from the rejected batch: no RAM staging; `view_a`
  and `view_b` never leave registers; one vertex record is read once; the
  secondary reloads only on joint change. Matrix swaps were measured cheaper
  than modelled (`perf-30fps.md:482-486`), so the gain is the removal of the
  scratch traffic and the wrapper glue, not of the CTC2s.
- Option to evaluate in the same kernel: lerp on the GTE with `GPF`/`GPL`
  (`IR0 = 4096 - t; IR1..3 = a; GPF` then `IR0 = t; IR1..3 = b; GPL`,
  MAC = a(1-t) + bt, 10 GTE cycles). That is 8 MTC2 + 2 ops + 3 MFC2 versus
  3 mult + 3 mflo + 9 ALU on the CPU; only wins if the multiply interlocks
  cannot be hidden. Measure both; keep the simpler one.
- Range constraint: the blended view vertex is re-entered through V0 as i16.
  The existing `project_gte_view_vertex` path already imposes this; the
  kernel must carry the same near-z / i16 guards (`render3d.rs`, the guards
  the decomposition charged into the 264 cycles) or reject into a slow path.
- Expected (derived): the Rust path's 590 cycles contain ~3 full matrix
  swaps, ~12 RAM touches of staged data, six exposed multiply interlocks and
  call glue. A flat kernel that keeps everything in registers and hides the
  interlocks should land in the 150-250 cycle range per blended vertex; at
  161 vertices that is roughly 55-70k cycles per render vblank, 7-9% of the
  ~843k render vblank. Measure; do not quote until measured.
- Gate: bit-exact player pixels on the gameplay tape (multi-frame dump of
  the DEFAULT build, memory note `perf-changes-need-multiframe-visual-gates`),
  `--profile-log` project/player stages, deadline misses, and the
  hardware-test disc for the new MVMVA mx=1/2 schedules (the shipped code
  only ever issues `mx=RT`; `psx-gte/src/ops.rs:267-282` exposes
  `0x4A08_0012` (RT,V0,TR) and `0x4A08_4012` (RT,V0,FC) only, and no
  LLM/LCM select exists anywhere in the three repos as of 2026-08-22. The
  new encodings derived from the header table in `ops.rs` are
  `MVMVA(LLM,V0,BK,sf=1) = 0x4A0A_2012` and
  `MVMVA(LCM,V0,FC,sf=1) = 0x4A0C_4012` (`mx` bits 18..17: 1 LLM, 2 LCM;
  `cv` bits 14..13: 1 BK, 2 FC). Verify both against the host GTE in
  `psx-gte-core` before trusting them, then on the disc).
- Stop conditions: if the dynamic counters show the blended path is no
  longer 64% of vertices on the current default character (Aletha Delivered
  replaced the earlier player; re-measure first), re-rank against 5.2.2.

#### 5.2.2 Cortex: flat model-face walker

- Where: `render3d/world_pass_model.rs:1525-2215` (the
  `submit_predecoded_model_face*` family, `submit_projected_model_triangle_*`).
- Evidence: faces (cull + packet build) 142-146k per render vblank at ~294
  cycles/face for ~496 faces (`perf-30fps.md:406-440`); the backface cross
  product uses two `mult` with exposed `mflo`, then packet field writes
  through the `psx-gpu` prim structs. NCLIP is closed for this path (memory
  note `gte-cull-depth-scalar-by-choice`: transform-once writes verts to an
  indexed scratch, the SXY FIFO is never live at cull time, and MAC0 is
  stale on silicon).
- Design: one `global_asm!` walker per fast-path variant that is actually
  hot (pick from PC samples; the family has five variants), modelled on
  `__hlpsx_walk_ordinary_quads`: load the face's three indices from the
  predecoded record (pre-scaled to the projected-record stride at cook
  time, as in `ps1-throughput-lessons.md` technique 6), load three projected
  `(xy word, depth)` pairs (6 loads), software winding with the two `mult`s
  separated by the depth-key max and the packet address computation,
  write the packet words from registers (tag, command|colour, xy, uv...
  where colour/uv come from the predecoded record so each is one `lw`),
  OT splice (`ot.rs` pattern: read head once, write twice), advance.
  Double-sided faces (`CullMode::None`, memory note
  `double-sided-player-flag-discovery`) take a separate walker or a flag
  that skips the winding branch; do not add a per-face mode test.
- Expected (derived): hl's walker is 331 static instructions of which 44
  are the guard chain and roughly a third is the fallback/subdivision
  bookkeeping that a cortex model-face walker does not need (no near band
  routing, no texture-window state, cook-time clamps). Count the common
  path with the 5.0.1 counters once hl's route is instrumented; until then
  treat "~60-90 issued instructions plus ~9 RAM loads per face, i.e. on the
  order of 130-170 cycles against 294 today" as the hypothesis to test, not
  a forecast. At ~496 faces that would be ~60-80k cycles per render vblank.
- Gate: same as 5.2.1; additionally the OT order within a bucket must be
  identical (world-order-bucketed mode; `perf-30fps.md` experiment 2).

#### 5.2.3 hl-psx: tighten the existing walker

- `__hlpsx_walk_ordinary_quads` guard clamp (`world_pipeline.rs`, the block
  "Guard clamp: both signed halfwords must remain in [-1022, 1022]") is
  44 instructions per quad (counted 2026-08-22): per halfword
  `sll/sra` or `sra`, `addiu`, `sltiu`, `beq`, `nop`, eight times, with
  eight unfilled branch slots. The walker itself is 331 static
  instructions. Two replacements, both to be proven on the host against the
  scalar form over all 2^16 halfword values:
  1. Exact window, one branch: keep `addiu + sltiu` per half (the window
     [-1022, 1022] has 2045 values, not a power of two, so a mask test
     cannot express it), but fold the eight verdicts with `and` and branch
     once: 8 x 4 + 7 = 39 instructions and one branch instead of eight; the
     saving is the eight `nop` slots and seven branches.
  2. Power-of-two window, fewer instructions: test `(h + 1024) & ~0x7ff == 0`,
     which accepts exactly [-1024, 1023], the GPU's signed 11-bit vertex
     range. Do NOT add the constant to the packed word directly: a negative
     low half carries into the high half (`-1022 + 1022` wraps to `0x10000`).
     Test the low half after `sll xy, 16` and the high half on the original
     word with a preloaded `0x0400_0000` constant and `0xf800_0000` mask:
     5 instructions per vertex, `or` the four results, one branch, ~24
     instructions per quad. This widens the accepted band by two units on
     each side versus today; the renderer owner must confirm nothing
     downstream assumed the two-unit margin (the packet writes the value
     unchanged into an 11-bit field, so 1023/-1024 are representable).
- Max-depth chain (three `sltu/beq/nop/move` blocks) -> branchless
  `subu/sra/and/addu` max: 12 instructions and 3 possible taken branches
  become 12 straight-line instructions. Small.
- Fog colour lookup and the `.Lhlpsx_walk_fallback` frequency: get the
  dynamic split from 5.0.1 before touching anything else; the doc's own
  conclusion is that the remaining hl gap is the four-step architecture
  chain (records, persistent packets, coarse ordering proven sound on t0a0,
  phase-grouped loops), not this loop.

#### 5.2.4 quake-psx: alias-model projection kernel

- The disassembly at `0x800128a0` is the model vertex loop: byte assembly
  of inputs, 6 MTC2, RTPT, 6 MFC2, 6 stores, with the loop counter and the
  stores filling nothing. After 5.1.5 changes the record to `xy:u32, z:u16`,
  port `__hlpsx_project_local_vertices` (it consumes exactly that record)
  into `psx_engine::projection` as the shared kernel for both games; the
  standardisation doc's queue item 1 ("model residency and animation")
  already plans this seam.
- Quake's world path is Rust at the `world_batch.rs` ceiling; before any
  asm there, PC-sample the E1M1 fixed route (5.0.2) and read the per-function
  census. The doc's own RAM margin (`VALIDATION.md`) and the 22.3 fps fixed
  route are the gates.

#### 5.2.5 Shared SDK: OT splice and clear

- `ot.rs` loops are already asm. The clear is CPU by default because OTC
  DMA wedged on console (CL2/CL3 notes); cost is ~2,048 x (2-cycle store +
  loop overhead/8) per frame, low. Leave unless 5.0.1 shows otherwise.

#### 5.2.6 GTE FLAG-based range verdicts

- `perf-30fps.md:413-418` proposed one `CFC2 FLAG` per RTPT triple instead
  of six scalar range checks; the rewrite dropped the "tautological
  hw-bounds checks" but it is unclear whether the FLAG read replaced the
  remaining verdicts. Verify in `world_pass_model.rs`; if scalar checks
  remain in the unblended triple loop, fold them into the kernel of 5.2.2
  as one CFC2 + mask.

### 5.3 Tier 3: LLVM backend knobs worth a 30-minute sweep

- Enumerate `rustc -C llvm-args=--help-hidden --target mipsel-sony-psx`
  for `mips-*` options; the relevant family is the delay-slot filler and
  `-mgpopt`/`-mips-ssection-threshold` (5.1.3). Use `-disable-mips-delay-filler`
  only as a negative control to confirm the census attribution.
- Verify LTO is actually cross-crate for the SDK+engine+game closure
  (`lto = true` is fat LTO; fine) and that `codegen-units = 1` holds for
  the staged builds (`build_guest_staged.sh` passes through profile flags).
- Do not try `opt-level = "s"`/`"z"` (miscompiles) or `-C target-cpu`
  other than `mips1`.

---

## 6. Do not retry (measured negatives and silicon constraints)

| Idea | Result | Source |
| --- | --- | --- |
| Stage blended vertices in a scratch array per part, batch by matrix | bit-exact, project +30k (413 -> 533 cyc/vertex) | `perf-30fps.md:468-487` |
| Chunked CPU-blend flush | +4.19% project-stage regression | memory `blend-chunk-optimization-pending-verification` |
| NCLIP for backface in transform-once paths | MAC0 stale on silicon; FIFO not live at cull time | `scene.rs:1060-1071`, memory `gte-cull-depth-scalar-by-choice` |
| `DepthBand::slot_depth` DIV hoist to a reciprocal map | +3.2k, the map's RAM loads cost more than the DIV | `perf-30fps.md:528-533` |
| Hoist per-model RT/TR loads out of the joint compose | flat, failed the pixel gate by 3 px (CTC2 commit-delay model) | `perf-30fps.md:689-699` |
| Endpoint-pose cache for joints | negative; endpoints change on ~75% of frames | `perf-30fps.md:701-709` |
| Factor per-quad helpers out of line | -0.84% FPS, +20.5M I-cache refills | `ps1-throughput-lessons.md` |
| Move rare branches to a cold section | -0.28% FPS | same |
| `core::hint::unlikely` layout hints | exactly neutral | same |
| Retained packet templates inside the submission arena | -1.53% FPS (capacity coupling) | same |
| Separately funded resident packet pool | -1.42% FPS (I-cache footprint of the machinery) | same |
| 64-entry source-addressed packet cache | -0.27% FPS | same |
| Project the whole PVS up front to batch RTPT | 4.95x too many vertices over the route | same (census) |
| Coarse OT ordering on its own | 0.22% of frame; only pays with phase-grouped loops | same |
| `opt-level = "s"` for the guest | miscompiles | memory `sanctum-walkthrough-findings-2026-08-15` |
| Moving GTE math to the CPU to dodge a hazard | measured silicon loss | memory `keep-work-on-gte-not-cpu` |
| i64 arithmetic in per-frame paths | software 64-bit routines; divide was even wrong before `builtins.rs` | memory `psx-rt-i64-divide-fix` |
| Issuing RTPS/MVMVA within 2 instructions of `MTC2 V0` | stale V0.x on silicon (vertex explosion) | `scene.rs:663-668`, HWB-010/011 |

---

## 7. Measurement and gating protocol for the working agent

1. Build the dynamic counters (5.0.1) and the symbol recipe (5.0.2) first.
   Run each game's canonical route once, archive: stage profile, PC top-40,
   I-cache stalls, the new class counters, linked size, RAM report.
2. Rank targets by `(cycles in unfilled slots + multiply interlock stalls +
   RAM loads) x retirements` per function, not by static counts.
3. For each kernel: write the host-equivalence test first (random inputs,
   byte-for-byte output, same counts), then the asm, then the emulator
   route A/B with the visual gate on multi-frame dumps of the DEFAULT build
   (not a telemetry build; memory `perf-changes-need-multiframe-visual-gates`),
   then the hardware-test disc for any new COP2 schedule, then a burn using
   the canonical burn protocol (memory `disc-burn-drutil-relative-cue-gotcha`).
4. A change is kept only if the mechanism moved (the counters it claimed to
   reduce went down) AND the game gate passed. FPS alone is not evidence;
   layout noise on hl is +/-0.122 fps and on quake the documented floor is
   the same order.
5. Record every result, including negatives, in the game's perf log
   (`docs/perf-30fps.md`, `hl-psx/docs/ps1-throughput-lessons.md`,
   `quake-psx/VALIDATION.md`). The negatives table above is what saved this
   survey from re-proposing four dead ideas.
6. Git: verify `HEAD` before committing, never amend in the active tree,
   no agent attribution anywhere (memory `no-git-amend-in-active-tree`,
   global CLAUDE.md).

---

## 8. Appendix

### 8.1 COP2 encodings used in the tree

```
MFC2 rt, rd : 0x4800_0000 | rt<<16 | rd<<11      (psx-gte/src/regs.rs)
CFC2 rt, rd : 0x4840_0000 | rt<<16 | rd<<11
MTC2 rt, rd : 0x4880_0000 | rt<<16 | rd<<11
CTC2 rt, rd : 0x48C0_0000 | rt<<16 | rd<<11
cofun       : 0x4A00_0000 | sf<<19 | mx<<17 | v<<15 | cv<<13 | lm<<10 | op
RTPS 0x4A08_0001  RTPT 0x4A08_0030  NCLIP 0x4A00_0006  AVSZ3 0x4A00_002D
AVSZ4 0x4A00_002E MVMVA(RT,V0,TR,sf1) 0x4A08_0012  SQR(sf1) 0x4A08_0028
GPF(sf1) 0x4A08_003D  GPL(sf1) 0x4A08_003E  OP(sf1) 0x4A08_000C
MVMVA matrix select mx: 0 RT, 1 LLM, 2 LCM; vector v: 0 V0, 1 V1, 2 V2, 3 IR;
translation cv: 0 TR, 1 BK, 2 FC, 3 none.   (psx-gte/src/ops.rs header)
```

Data registers: 0-5 V0..V2, 6 RGBC, 7 OTZ, 8 IR0, 9-11 IR1-3, 12-15 SXY
FIFO (15 = push), 16-19 SZ0-3, 24 MAC0, 25-27 MAC1-3, 31 LZCR.
Control: 0-4 RT, 5-7 TR, 8-12 LLM, 13-15 BK, 16-20 LCM, 21-23 FC, 24/25
OFX/OFY, 26 H, 27/28 DQA/DQB, 29/30 ZSF3/ZSF4, 31 FLAG.

### 8.2 Commands

```bash
# static census (any of the three .exe files)
python3 tools/instr_census.py build/examples/mipsel-sony-psx/release/editor-playtest.exe --name cortex --dis /tmp/cortex.dis
```

```bash
# look at the codegen around every RTPT in a disassembly
grep -n -B18 -A22 '4a080030' /tmp/cortex.dis | less
```

```bash
# PC samples on the cortex gameplay route (boot-into-gameplay disc, see docs/playtest-profiling.md)
cd emu && cargo run -p frontend --release -- launch --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue --embedded-playtest --hold-forward --steps 2000000000 --pc-sample-log /tmp/cortex-pc.csv --pc-sample-instructions 4096 --profile-log /tmp/cortex-vblank.csv --dump-hw /tmp/cortex.ppm
```

```bash
# symbolise (needs the ELF relink from 5.0.2)
python3 tools/pc_symbolize.py --elf /tmp/editor-playtest.elf --samples /tmp/cortex-pc.csv --top 40
```

### 8.3 Kernel skeleton (copy the hl-psx shape)

```rust
#[repr(C)]
pub struct FaceWalkContext { /* pointers + counts, word-aligned */ }
#[cfg(target_arch = "mips")]
const _: () = { assert!(core::mem::size_of::<FaceWalkContext>() == 28); };

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__psx_walk_model_faces,\"ax\",@progbits",
    ".globl __psx_walk_model_faces",
    ".type __psx_walk_model_faces,@function",
    ".set noreorder", ".set noat",
    ".ent __psx_walk_model_faces",
    "__psx_walk_model_faces:",
    // a0 = context. Load every field into registers once; never reload.
    // Every branch slot and load slot carries an independent instruction.
    // Two useful instructions between the last MTC2 and the op (HWB-011).
    // >= 8 instructions between consecutive GTE commands.
    // mult: small operand in rs, >= 6 instructions before mflo.
    "jr $ra", "nop",
    ".end __psx_walk_model_faces",
);

#[cfg(target_arch = "mips")]
unsafe extern "C" { fn __psx_walk_model_faces(ctx: *const FaceWalkContext) -> u32; }

#[inline(always)]
pub unsafe fn walk_model_faces(ctx: &FaceWalkContext) -> u32 {
    #[cfg(target_arch = "mips")] { unsafe { __psx_walk_model_faces(ctx) } }
    #[cfg(not(target_arch = "mips"))] { walk_model_faces_rust(ctx) } // the reference the host test compares against
}
```

### 8.4 Files read for this survey

SDK: `psx-gte/src/{regs,ops,scene}.rs`, `psx-gte-core/src/state.rs`,
`psx-gpu/src/{ot,prim}.rs`, `psx-rt/src/{lib,cache,interrupts,builtins}.rs`,
`psx-math/src/{int32,sincos}.rs`, `psx-asset/src/{lib,hmd8}.rs`, `psoxide.ld`.
Engine: `psx-engine/src/{render3d,classic_affine,projection,fixed,scratchpad}.rs`,
`render3d/world_pass_model.rs`, `psx-bsp/src/{render,collision}.rs`,
`character_motor.rs`, `third_person_camera.rs`.
Emulator: `emulator-core/src/{cpu,bus}.rs`, `cpu/{timing,icache}.rs`,
`bus/memory_timing.rs`, `frontend/src/cli.rs`.
Games: `hl-psx/game/src/{world_pipeline,scratchpad,render,model,main}.rs`,
`hl-psx/docs/{ps1-throughput-lessons,retained-world-pool-plan}.md`,
`quake-psx/crates/quake-core/src/{world_batch,liquid}.rs`,
`quake-psx/game/src/renderer.rs`, `quake-psx/RENDERING.md`, both `build.rs`.
Docs: `perf-30fps.md`, `demo10-low-level-hot-paths-2026-06-02.md`,
`shared-engine-standardisation-2026-08-22.md`, `playtest-profiling.md`,
`hardware-test-disc.md`, `cortex-30fps-experiments-2026-07-26.md` (headings).
