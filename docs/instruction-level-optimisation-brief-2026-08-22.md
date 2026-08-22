# Instruction-level optimisation brief: PSoXide, quake-psx, hl-psx

**Date:** 2026-08-22
**Audience:** a working agent that will verify and then implement.
**Scope:** the shared PS1 stack (`PSoXide/sdk`, `PSoXide/engine`) and its two
biggest consumers, `quake-psx` and `hl-psx`.
**Status of every claim below:** labelled `MEASURED` (I ran it this session and
gave you the command), `MODELLED` (read out of the emulator's silicon-calibrated
timing source), `DOCUMENTED` (from an existing repo doc or code comment), or
`HYPOTHESIS` (my inference, needs your experiment). Do not treat a HYPOTHESIS as
a finding.

---

## 0. The short version

The question was whether hand-written assembly can push the PS1 further, given
that we own the whole stack down to the instruction.

The answer is yes, but almost none of the value is in arithmetic. The GTE is
0.31% of the shipped instruction stream and roughly 1.3% of frame time in
hl-psx's own measurement. Multiply and divide together are 1.24% of the stream.
Replacing scalar math with cleverer scalar math is playing for scraps.

Where the cycles actually go, from a static decode of today's shipped
`editor-playtest.exe`:

| class | share of instructions | share of modelled cycles |
|---|---:|---:|
| main-RAM loads (7 cycles each) | 15.76% | **53.1%** |
| main-RAM stores (2 cycles each) | 13.08% | 12.6% |
| everything else (1 cycle) | 71.16% | 34.3% |

And within those loads:

> **58.6% of every load in the shipped guest is `$sp`-relative.**
> Stack spill and reload traffic alone is 38.5% of modelled CPU cycles.
> The same traffic against the CPU scratchpad would cost 34,000 cycles instead
> of 160,575.

That is the headline. The PS1 has no data cache. A load from main RAM costs 7
cycles; a load from the 1 KiB scratchpad at `0x1F80_0000` costs 1. LLVM spills
heavily on a 32-register MIPS-I with a 400 KiB renderer, and every one of those
spills is paying full DRAM latency for a value that was in a register three
instructions ago.

So the ranked opportunity is:

1. **Get the hot call chain's stack into the scratchpad.** Biggest single number
   on the table, needs no algorithm change, needs assembly (Rust cannot express
   "run this subtree on a different stack"). Section 4.1.
2. **Cache-line alignment of hot code.** Nothing in the image is 16-byte
   aligned; 75% of branch targets land mid-line and a tag miss only fills from
   the entry word to the end of the line, so we throw away roughly 37% of every
   refill's prefetch. Section 4.2.
3. **Profile-guided code layout on a direct-mapped cache.** 784 KiB of `.text`
   through a 4 KiB direct-mapped I-cache. hl-psx proved that *local*
   rearrangement loses; nobody has tried *global conflict-aware* placement.
   Section 4.3.
4. **Fill the GTE hazard NOPs with real work.** The SDK already proved the
   pattern once and then didn't generalise it. Bounded but free. Section 4.4.
5. **Divide and multiply scheduling.** 421 `div` sites, each ~36 cycles with a
   back-to-back `mflo` that hides nothing, plus Rust's panic checks. Section 4.5.
6. **Delay-slot filling.** 51.7% of branches ship with an empty delay slot.
   Worth 5.8% of cycles at absolute best, so do it last and only where it is
   free. Section 4.6.
7. **Settle the GTE input-hazard disagreement between PSoXide and hl-psx.**
   One of the two repos is wrong and it matters either way. Section 6.1.

Section 5 is the list of things you must *not* re-run, with the evidence that
closed them. Read it before proposing anything.

---

## 1. How to reproduce every number in this brief

```bash
python3 tools/text_census.py build/examples/mipsel-sony-psx/release/editor-playtest.exe
```

That script is new, checked in with this brief, and prints the entire table set
above. It auto-detects the `.text` extent from `jr $ra` density and prints the
invalid-encoding rate so a bad guess is visible (today: 0.02%, i.e. the region
is genuinely all code).

The image I measured was built at 2026-08-22 13:04 from branch
`codex/arena-playable-slice`. `.text` runs `0x80010000..0x800d4000`, which is
**784 KiB of code**, 200,704 instructions. Total payload is 1,616 KiB.

The MIPS codegen samples in section 3 come from:

```bash
cd <scratch>/asmprobe && cargo rustc --release -- --emit asm
```

with a four-function `#![no_std]` lib, `.cargo/config.toml` carrying
`build-std = ["core"]` and `target = "mipsel-sony-psx"`, and the repo's
`rust-toolchain.toml` copied in. Rebuild it yourself before trusting section 3;
it is the cheapest way to see what LLVM actually does with a given source shape.

---

## 2. The machine model you are optimising against

All of this is `MODELLED`, read out of `emu/crates/emulator-core`, whose timing
was calibrated against a real SCPH-9902 (see the comments in
`bus/memory_timing.rs` citing the silicon timing disc). Memory
`emulator-matches-console-perf` records that the user's console tracks this
model, so treat it as the real machine, not as an approximation.

### 2.1 Memory

| access | cycles | source |
|---|---:|---|
| main-RAM load, any width, KSEG0 | 1 issue + **6 stall** | `memory_timing.rs:read_stalls` |
| main-RAM store, any width | 1 issue + **1 stall** | `bus.rs:cpu_write_stalls` |
| **scratchpad load or store** | **1, zero stall** | `read_stalls` returns 0 for `0x1F80_0000` |
| internal MMIO (GPU, DMA, timers) | 1 + 4 | `read_stalls` |
| I-cache line refill from RAM | 1 setup + 1 per word | `icache_fill_stalls` |
| DRAM refresh arbitration | +8 (cached) / +4 (uncached), every 515 clocks | `DRAM_REFRESH_*` |

Two consequences that drive everything else:

- **The load stall is charged at the load instruction, not at the use.** There
  is no non-blocking load and no data cache to miss into. Hoisting a load
  earlier buys nothing. The only ways to win are: issue fewer loads, or issue
  them against the scratchpad.
- **Stores are cheap.** The external write buffer hides most of the wait state.
  A store is 2 cycles against a load's 7. Any transformation that trades loads
  for stores is a 3.5:1 win before you count anything else.

### 2.2 Instruction cache

4 KiB, **direct-mapped**, 256 lines of 4 words. No associativity, so address
bits alone decide which line a byte lands in, and two hot regions 4 KiB apart
evict each other unconditionally.

The refill policy matters and is easy to miss
(`emu/crates/emulator-core/src/cpu/icache.rs:48`):

> A tag miss fills **from the requested word to the end of the line**, not the
> whole line. Only a tag *hit* with an invalid word refills all four.

So entering a line at word 0 fetches 4 words for 5 stall cycles (1.25 cycles per
instruction of prefetch). Entering at word 3 fetches 1 word for 2 stall cycles,
and words 0..2 of that line stay invalid until something jumps to them. Landing
mid-line is strictly worse, and section 4.2 shows we land mid-line 75% of the
time.

`IBLKSZ` (cache-control bits 8..9) is a live knob the emulator models: value 0
gives a 2-word fill for misses that begin at word 0. The BIOS default `0x1E988`
selects the 4-word fill. Cheap experiment, unlikely to pay, listed in 7.4 for
completeness.

### 2.3 CPU

- 1 issue cycle per instruction, plus the stall classes above
  (memory `emulator-cycle-model`; the older "flat 2 cycles" note is stale).
- `MULT` ≈ 7-13 cycles, `DIV` = 36, both asynchronous. `MFHI`/`MFLO` interlock
  on the result, so **independent work placed between the multiply and the read
  is free**. `cpu.rs:1570-1603`.
- MIPS-I hazard: `MFHI`/`MFLO` must not be followed within two instructions by
  a new `MULT`/`DIV`. LLVM honours this with two NOPs (measured, section 3.1).
- One architectural load-delay slot, not interlocked. LLVM fills it or pads it.
- GTE: issuing a command while one is in flight stalls (`gte_sync`). `MAC0` and
  `LZCR` have a modelled result-read latency; the other data registers are read
  live. See section 6.1, this is the part I could not settle from source alone.

### 2.4 The build

`nightly-2026-03-25`, target `mipsel-sony-psx`, `cpu = "mips1"`, `+soft-float`,
`-mno-check-zero-division`, `relocation-model = "static"`, `rust-lld`. Guest
profile is `opt-level = 3` (release default), `lto = true`, `codegen-units = 1`,
`panic = "abort"`.

`sdk/psoxide.ld` places `.text` as a single unordered `*(.text .text.*)` blob.
**There is no section ordering, no hot/cold split, and no symbol ordering file.**
That is the hook section 4.3 needs.

---

## 3. What LLVM actually emits (MEASURED)

Four representative source shapes, compiled at `opt-level=3` for the real
target. This is the ground truth for "would hand assembly beat the compiler
here", and the answer differs sharply per shape.

### 3.1 A three-term Q12 dot product

Source: `(m[0]*v.x + m[1]*v.y + m[2]*v.z) >> 12` over `i16` fields.

```mips
dot_fold:
	lh	$1, 4($5)
	lh	$2, 4($4)
	lh	$3, 0($4)
	lh	$4, 2($4)
	mult	$2, $1
	lh	$2, 0($5)
	mflo	$1
	nop
	nop
	mult	$3, $2
	lh	$3, 2($5)
	mflo	$2
	nop
	nop
	mult	$4, $3
	mflo	$3
	addu	$2, $3, $2
	addu	$1, $2, $1
	jr	$ra
	sra	$2, $1, 12
```

20 instructions, 4 of them NOPs for the `MFLO`-then-`MULT` hazard, 6 loads. In
the emulator's model that is 20 issue cycles + 36 load-stall cycles = **~56
cycles for one dot product**, and 3 multiplies whose latency is partly exposed.
A 3x3 matrix-vector is three of these: **~170 cycles**.

The GTE does the identical operation, with saturation and the translation add,
in a single `MVMVA` at roughly 8 cycles. So: **any residual scalar 3x3 transform
in a hot path is 15-20x more expensive than the GTE call it should be.** This is
not "hand assembly beats the compiler", it is "the coprocessor beats both".
Audit for these; `Mat3I16::mul` has 19 call sites today and
`scene::compose_rotation_scheduled` already exists as the GTE replacement.

### 3.2 An integer divide

Source: `(x << 8) / z`.

```mips
persp:
	addiu	$sp, $sp, -16
	beqz	$5, $BB3_4          ; divide-by-zero panic check
	nop
	addiu	$1, $zero, -1
	bne	$5, $1, $BB3_3      ; INT_MIN / -1 overflow check
	sll	$2, $4, 8
	lui	$1, 32768
	beq	$2, $1, $BB3_5
	nop
$BB3_3:
	div	$zero, $2, $5
	mflo	$2                  ; interlocks for the full 36 cycles
	jr	$ra
	addiu	$sp, $sp, 16
```

Eight instructions of panic scaffolding around the divide, two of them NOPs in
unfilled delay slots, and the `mflo` sits directly behind the `div` so the whole
36-cycle latency is exposed. `-mno-check-zero-division` is already in the target
spec; it suppresses LLVM's trap, not Rust's panic branch. There are **421 `div`
sites** in the shipped `.text`.

### 3.3 A bounds-checked slice loop

Source: a `while i < xy.len()` loop writing `out[i]`.

```mips
$BB1_4:
	beqz	$8, $BB1_8          ; second slice's bounds check, per iteration
	nop
	lw	$1, 0($4)
	addiu	$8, $8, -1
	addiu	$4, $4, 4
	sltu	$1, $1, $3
	sb	$1, 0($6)
	addiu	$6, $6, 1
	bne	$6, $5, $BB1_4
	addu	$2, $2, $1
```

LLVM eliminated one bounds check and kept the other, so every iteration pays a
compare, a branch, and a NOP for the delay slot it could not fill. Ten
instructions of which three are pure overhead. The fix here is source-level
(`zip`, `chunks_exact`, `split_at`, or raw pointers), not assembly.

### 3.4 A straight-line packet store

```mips
build:
	lw	$1, 0($5)
	sw	$6, 0($4)
	sw	$1, 4($4)
	lw	$1, 4($5)
	nop                          ; unfillable load-delay slot
	sw	$1, 8($4)
```

LLVM does a decent job and only pads when it genuinely has nothing to move. Note
that the NOP is *on top of* the 6-cycle RAM stall the `lw` already paid.

**Conclusion for section 3:** LLVM's MIPS-I output is competent. It is not
leaving 2x on the table through bad scheduling. What it cannot do is (a) know
that a load costs 7 cycles rather than 1, (b) place code to avoid direct-mapped
conflicts, (c) put a stack somewhere other than where `_start` set `$sp`, or
(d) use the GTE. Every real opportunity below is one of those four.

---

## 4. The opportunities, ranked

### 4.1 Move the hot render call chain's stack into the scratchpad

**Evidence (MEASURED).** 18,515 of 31,622 loads in the shipped `.text` are
`$sp`-relative (58.6%). 15,485 of 26,255 stores are (59.0%). At the modelled
weights that traffic is 160,575 cycles out of 416,691, i.e. **38.5% of modelled
CPU cycles are spill/reload against main RAM**. The identical traffic against
the scratchpad costs 34,000 cycles.

**Why it exists.** No data cache, a 32-register file, `opt-level=3` with `lto`,
and renderer functions large enough that hl-psx measured a single one at 4,672
bytes. Register pressure is enormous and every spill is a full DRAM round trip.

**Why Rust cannot do it.** There is no way to say "run this call subtree with
`$sp` pointing at `0x1F80_0000`". It needs an assembly trampoline. This is the
single clearest case in the whole codebase where hand-written assembly buys
something the compiler structurally cannot.

**Shape of the change.**

```
__psoxide_call_on_scratch_stack(fn_ptr, arg) :
    save $ra and the caller's $sp into two globals (or into the top of the
        scratchpad region itself)
    set  $sp = SCRATCH_STACK_TOP        (0x1F80_0000 + reserved size, 8-aligned)
    jalr fn_ptr with arg in $a0
    restore $sp and $ra
    jr   $ra
```

Wrap exactly one entry point: the visual-frame render call. Everything below it
inherits the fast stack.

**The three things that will bite you, in order.**

1. **1 KiB is small and the failure mode is silent corruption.** You must
   measure the maximum stack depth of the render subtree before enabling this.
   Recipe: fill the scratchpad with a known pattern at frame start, run the
   frame, scan downward from the top for the lowest modified word. Do this in
   the emulator over a full tape, not on one frame. If the subtree needs more
   than the reservation, this optimisation is dead in its current form and you
   fall back to hoisting only the largest hot arrays (see below).
2. **Interrupts.** The VBlank ISR and any DMA-completion handler will run on
   whatever `$sp` is live. If they run on the scratchpad stack they can blow the
   1 KiB. Either give the ISR its own stack (it already needs a defined one) or
   verify the ISR's own depth fits inside the headroom you leave.
3. **Anything that DMAs from a stack local dies instantly.** The scratchpad is
   not DMA-addressable. Grep the render subtree for pointers passed to
   `psx-io::dma` and for `submit_linked_list*`; the OT and packet arena are
   already static, but check for any stack-resident staging buffer.

**Guard rails.** Put a canary word at the bottom of the scratchpad stack and
check it once per frame in debug builds. `debug_assert` is not enough on a
release guest; make it a cheap always-on compare that sets a telemetry bit.

**Expected gain.** If dynamic stack traffic is anywhere near the static 58.6%,
this is a 25-30% CPU reduction in the render path. I want to be explicit that
static counts overweight function prologues and epilogues, which execute once
per call rather than once per iteration, so **the dynamic number could be
materially lower**. Measure it before you build it (section 7.1 tells you how).

**Fallback if the depth does not fit.** Hoist only the hot arrays. The engine
already does this in two places:
`engine/crates/psx-engine/src/render3d/world_pass_model.rs:583` puts the blended
vertex chunk there, and `engine/crates/psx-bsp/src/render.rs:795` puts the affine
batch workspace there. Both are correct patterns. Extending that to the
projected-vertex working set (128 packed 8-byte records, or 256 4-byte XY words)
is a smaller, safer version of the same idea. Note that the scratchpad only wins
for data that is read more than once, or for a store-then-load pair: a single
read from RAM into the scratchpad costs the same 7 cycles you were trying to
avoid.

**Cross-repo.** `hl-psx/game/src/scratchpad.rs` and PSoXide's
`engine/crates/psx-engine/src/scratchpad.rs` are the same module. quake-psx does
not use it at all. Land the trampoline in the SDK (`psx-rt`), not in a game.

---

### 4.2 Align hot code to the 16-byte I-cache line

**Evidence (MEASURED).** Conditional-branch targets in the shipped image are
distributed 25.2% / 25.2% / 24.6% / 25.0% across the four words of a cache line.
That is exactly uniform, which means **nothing is aligned**. The generated
assembly confirms it: LLVM emits only `.p2align 2` (4 bytes) for MIPS-I
functions, never `.p2align 4`.

**Why it costs.** Section 2.2: a tag miss fills only from the entry word to the
end of the line. Entering at word 0 prefetches 4 instructions; entering at word
3 prefetches 1 and leaves the rest of the line invalid. Expected prefetch on a
cold taken branch is 2.5 words instead of 4, so **we discard about 37% of the
refill's value**, and we pay a second miss when execution later reaches the
earlier words of that same line.

**The change.** Force 16-byte alignment on hot loop heads and hot function
entries. Three mechanisms, in increasing order of effort:

- `#[repr(align)]` does not apply to functions. Use a naked `global_asm!`
  `.p2align 4` immediately before the label for hand-written routines
  (`hl-psx/game/src/world_pipeline.rs` bodies, `psx-gpu/src/ot.rs`'s clear loop,
  the SDK's GTE schedules). Free and immediate.
- For Rust functions, put each hot one in its own section
  (`#[unsafe(link_section = ".text.hot.<name>")]`) and align the section in
  `psoxide.ld` with `. = ALIGN(16);` before each input-section match. This also
  gives you the handle you need for 4.3.
- For loop bodies inside Rust functions there is no clean lever short of
  rewriting the loop as assembly. Do not chase this one.

**Expected gain.** Bounded by the I-cache refill stall total, which hl-psx
measures at 21% of route CPU and 42-48% inside renderer-bound windows. If
alignment recovers even a fifth of the wasted prefetch, that is low single-digit
percent overall and more inside the bad windows. Low risk, mechanical, and it is
a prerequisite for making 4.3 measurable.

**How it could be wrong.** Alignment padding grows `.text`, and on a
direct-mapped cache growing the footprint can shift a hot region into a new
conflict. Measure `icache_refill_stall_cycles` before and after, not just FPS.

---

### 4.3 Profile-guided, conflict-aware code layout

**Evidence.** `.text` is 784 KiB, 196x the cache (MEASURED).
`sdk/psoxide.ld` emits `*(.text .text.*)` with no ordering, so today's layout is
whatever order rust-lld happened to see the input sections in (DOCUMENTED, read
the script). hl-psx's `PS1_THROUGHPUT_LESSONS.md` measured its two hot regions,
the quad emitter and the per-frame render tail, alternating every frame and
evicting each other, with 42-48% I-cache stalls in the affected windows.

**What has already been tried and failed** (hl-psx, DOCUMENTED, do not repeat):
factoring per-quad helpers out of line (-0.84% route FPS), moving rare branches
to a cold section (-0.28%), branch-probability layout hints (0.000%). Every one
of these *rearranged code within its own neighbourhood*. The conclusion drawn
was "layout cannot help".

**Why I think that conclusion is too strong (HYPOTHESIS).** On a direct-mapped
cache, whether two regions conflict is decided by their addresses **modulo
4096**, and none of those three experiments controlled that variable. They
changed sizes and call structure and let the linker place the result wherever it
landed. The failure mode they measured, "two hot regions evict each other every
frame", is exactly the failure mode that address assignment controls and nothing
else does.

**The experiment to run.**

1. Add PC-bucketed sampling to the emulator. It is our emulator; this is a
   ~40-line change in `cpu.rs` behind a feature flag: on each step, increment a
   counter indexed by `(pc - text_base) >> 4`, i.e. per cache line. Dump on
   exit. Combine with the existing `instruction_cache_profile`.
2. Run the canonical tape headless and take the top N lines by sample count.
   Those are your hot set. Note whether the hot set exceeds 4 KiB; if it does,
   conflict is unavoidable and the goal changes to minimising it rather than
   eliminating it.
3. Emit a symbol ordering file and pass `-C link-arg=--symbol-ordering-file=...`
   to rust-lld. Place the hot symbols contiguously so their combined footprint
   maps to as few colliding sets as possible, and push everything cold beyond
   them.
4. Gate on `icache_refill_stall_cycles` from the counter log, not on FPS alone.
   FPS is noisy; the refill counter is deterministic for a fixed tape.

**Expected gain.** Unknown, genuinely. It could be zero if the hot set is 20 KiB
and hopelessly self-conflicting. It could be large in exactly the windows
hl-psx identified as renderer-bound. The measurement in step 1 tells you which
before you spend any implementation time, so do step 1 regardless: a per-line PC
histogram is useful for every other item in this document too.

**A cheaper adjacent idea.** PSoXide's 784 KiB `.text` is 7x hl-psx's `play`
function's whole 112 KiB. Ask whether the guest is linking code it never runs
during gameplay: menu, boot, loading, editor-only paths, debug overlays. Moving
those to a `.text.cold` block placed after everything hot does not shrink the
image but does stop them from occupying cache sets between the code that
matters. That is a linker-script change plus `link_section` attributes, and it
is safe.

---

### 4.4 Fill the GTE hazard NOPs with real work

**Evidence (DOCUMENTED, read the source).** `sdk/crates/psx-gte/src/scene.rs`
pads every GTE schedule with `.word 0` NOPs to satisfy the console-confirmed
HWB-010/011 input-commit hazard and the NCLIP `MAC0` result latency. Counts per
call:

| helper | dead NOPs |
|---|---:|
| `project_vertex_mips` | 3 |
| `project_triangle_mips` | 3 |
| `rtpt_kick` | 2 |
| `transform_vertex_mips` | 3 |
| `aabb_dot_mvmva` | 3 |
| `average_cached_z3` / `_z4` | 3 |
| `screen_area_mac0_scheduled` | **10** |
| `screen_area_and_average_cached_z3_scheduled` | 8 |

**The pattern is already proven in this file.**
`screen_area_and_classic_otz3_scheduled` fills NCLIP's eight-instruction `MAC0`
gap with the entire `sum * 0x155` shift-add sequence and its own result read,
paying zero extra cycles for the OTZ. That is the model. It was written once and
never generalised.

**The change.** For each hot caller, identify independent work that can move
into the gap and add a fused helper, exactly as
`screen_area_and_classic_otz3_scheduled` did. Candidates:

- `screen_area_mac0_scheduled` at
  `engine/crates/psx-engine/src/world_render/indexed_cache.rs:2993` burns 10
  cycles per face doing nothing. The next face's index decode, the packet base
  pointer bump, the texture-page word, or the material lookup all fit.
- `project_vertex_mips` and `transform_vertex_mips` are `#[inline(always)]`, so
  LLVM *cannot* move surrounding work into the gap: an `asm!` block is opaque to
  the scheduler. The gap has to be filled explicitly inside the block.

**Expected gain.** Honest sizing: at ~100-300 emitted faces per frame,
`screen_area_mac0_scheduled` alone is 1,000-3,000 cycles out of a ~843k render
vblank, so **0.1-0.4%**. Per-vertex helpers scale with vertex count and are
worth more. This is a real but small lever. Do it because it is nearly free, not
because it will move the frame rate.

**Do not** shorten the gaps to gain the cycles. They are console-confirmed
(memory `cortex-hardware-rendering-issues`, the HWB-011 vertex explosion).
Section 6.1 covers the one place where the gap length is genuinely in question.

---

### 4.5 Divides and multiplies

**Evidence (MEASURED).** 421 `div`/`divu` and 2,076 `mult`/`multu` in the
shipped `.text`. Section 3.2 shows the emitted shape: panic scaffolding, then
`div` followed immediately by `mflo`, exposing the full 36-cycle latency.

**Three separate changes, in descending value.**

1. **Hide the latency.** `MFLO` interlocks; the divide runs asynchronously
   (`cpu.rs:1582-1603`). Any independent instruction between `div` and `mflo`
   is free. LLVM does not know the latency is 36 and does not try. In a hot
   loop, an `asm!` block that issues the divide, does the next iteration's
   address arithmetic, then reads `LO`, recovers most of 36 cycles per divide.
2. **Delete the panic scaffolding.** `unchecked_div` / `unchecked_rem` remove
   the zero and overflow branches (6 instructions plus 2 NOPs per site) where
   the caller can prove the divisor is non-zero. Audit the hot sites first;
   there is no point doing this to 421 places when a handful are hot.
3. **Use the GTE where the operation is really a perspective divide.** `RTPS`
   already computes `H/SZ` with saturation. `engine/crates/psx-engine/src/fixed.rs`
   has a hand-written 12-step binary long division for the Q12 fraction; check
   whether its callers could take the GTE's divide instead. Note memory
   `gte-rtps-divide-clamp-models-shrank`: the GTE divide clamps `H/SZ` at 2.0,
   which is a real behavioural difference, not a drop-in.

**Multiply note.** The `MFLO`-then-`MULT` two-NOP hazard cost 4 of 20
instructions in the section 3.1 sample. A hand-scheduled sequence that
interleaves the three products of a dot product (issue, do address arithmetic,
read, issue) removes those. This only matters if a scalar multiply chain is
still hot after you have moved everything you can to the GTE, which per section
3.1 it should not be.

---

### 4.6 Delay slots

**Evidence (MEASURED).** 24,080 NOPs, 12.00% of instructions, 5.8% of modelled
cycles. Of those, 13,455 sit in a branch or jump delay slot, meaning **51.7% of
all 26,001 branches and jumps ship with an empty slot**. 8,733 pad a load-delay
slot. 721 pad the `MFHI`/`MFLO` hazard.

**The honest sizing.** Filling *every* slot perfectly saves at most 5.8% of
cycles, and you cannot fill every slot. Half of the branch slots are
`beq`-to-panic-handler and function-return sequences where there is genuinely
nothing to move.

**Where it is actually worth doing.** In the hand-written assembly bodies, which
today use `.set noreorder` and put `nop` in essentially every slot by hand:

- `hl-psx/game/src/world_pipeline.rs`: the four walkers. `__hlpsx_admit_source_face_runs`
  has ~14 `nop`s in delay slots inside a per-face loop. That is a per-face cost
  in the hottest loop in that game.
- `sdk/crates/psx-gpu/src/ot.rs:clear_software`: one wasted slot per 8 words
  cleared, so 256 cycles per 2,048-slot OT clear. Trivially fixable by moving
  the pointer bump into the slot.
- `sdk/crates/psx-rt/src/cache.rs`: the invalidate loop. Runs rarely; ignore.

**Rule.** Do this only inside assembly you are already editing for another
reason. A standalone "fill the delay slots" pass on hand-written assembly is a
high-risk, low-reward change, and the compiler already does it for Rust code as
well as it is going to.

---

### 4.7 Narrow loads and record packing

**Evidence (MEASURED).** 8,079 of 31,622 loads (25.5%) are `lh`/`lhu`/`lb`/`lbu`.
A narrow load costs the same 7 cycles as a word load
(`read_stalls` returns 6 for all widths in RAM).

**The implication.** Reading two `i16` fields as two `lhu`s costs 14 cycles.
Reading them as one `lw` plus a shift and a mask costs 7 + 2 = 9. Reading four
`u8` fields as four `lbu`s costs 28; one `lw` plus extraction costs ~11.

hl-psx already documents the cooked-record version of this idea in
`PS1_THROUGHPUT_LESSONS.md` ("one 32-bit load of the whole face record",
"pre-scaled indices"). What the census adds is the cycle justification: on this
machine the win is not the instruction count, it is that each avoided load is 7
cycles.

**The change.** Audit the hot record loads in
`engine/crates/psx-engine/src/render3d/world_pass_model.rs`,
`engine/crates/psx-bsp/src/render.rs`, and hl-psx's face records for multi-field
reads from the same word, and fuse them. This is source-level, not assembly, and
it composes with 4.1: a fused load that then lives in a register does not spill.

**Also worth checking:** `lwl`/`lwr` appear 637 times each, which means 637
unaligned word accesses, each costing an extra lane-steering cycle on top
(`read_lwl_lanes`). Find out what is misaligned and fix the layout.

---

### 4.8 Lift quake-psx onto the same footing

**Evidence (MEASURED by grep).** quake-psx has exactly one `asm!` block in its
own crates (`crates/quake-core/src/liquid.rs`, the texture warp). It does not
use the scratchpad at all. Everything it gains, it gains from the SDK.

**Implication for sequencing.** Anything landed in `psx-rt`, `psx-gte` or
`psx-gpu` benefits all three games with no per-game work. Anything landed in a
game's own renderer benefits one. Given that hl-psx's own conclusion is that its
renderer needs a co-designed rewrite rather than local optimisation, and that
quake-psx has nothing to optimise locally, **put the effort in the SDK.** The
scratchpad-stack trampoline (4.1) is the clearest example: one `psx-rt` routine,
three games.

---

## 5. Closed lines of enquiry: do not re-run these

All of these were measured by the hl-psx work and are documented in
`hl-psx/PS1_THROUGHPUT_LESSONS.md`. Re-running them wastes tape time.

| attempt | result | why it failed |
|---|---|---|
| Batch RTPT harder / project shared vertices once | GTE is 1.3% of frame | nothing to win |
| Persistent packet templates in the submission arena | -1.53% route FPS | a resident slot consumes packet capacity even when culled |
| Separately funded resident packet pool | -1.42% | +10.6% I-cache stalls from the extra hot control flow |
| 64-entry associative source-patch cache | -0.27% | lookup cost exceeds six saved stores over 64 faces |
| Factor per-quad helpers out of line | -0.84% | fragments the loop's cache footprint across regions |
| Move rare branches to a cold section | -0.28% | same, and the "rare" branch was not rare |
| `core::hint::unlikely` layout hints | 0.000% | LLVM had already ordered the blocks |
| Deduplicate the tri path | no duplication exists | 8,004 B is genuine work |
| Coalesce adjacent equal-OTZ inserts | 14.84% of OT writes at best | too little bookkeeping to matter |
| Three-pass phase-grouped band fill | -0.169% | reordering fights the depth-band painter's order |
| Coarse ordering standalone | ~0.22% by calculation | its value is enabling phase-grouping, not the deleted math |
| Chunked CPU blend flush (PSoXide) | +4.19% regression | memory `blend-chunk-optimization-pending-verification` |
| Move GTE cull/depth work to NCLIP/AVSZ3 (PSoXide) | rejected | memory `gte-cull-depth-scalar-by-choice`: the FIFO is never live at cull time |

**The meta-lesson from that table**, which I think is correct and which you
should carry into anything you propose: on a 4 KiB direct-mapped cache,
rearranging or adding machinery around a hot loop reliably loses, because it
fragments the footprint one iteration touches. The only admissible directions
are (a) make the loop do genuinely less work, (b) make its data cheaper to
reach, or (c) change where the code sits in the address space. This document's
top three items are exactly one of each.

---

## 6. Open hardware questions

### 6.1 PSoXide and hl-psx disagree about the GTE input-commit hazard

This is the most important unresolved item and it cuts both ways.

**PSoXide's position (DOCUMENTED, console-confirmed).**
`sdk/crates/psx-gte/src/scene.rs` inserts two NOPs between the final `MTC2` and
the GTE command, everywhere. The comment on `transform_vertex_mips` states that
without them, real silicon commits the `VXY0` write between the MVMVA's
sequential MAC1 and MAC2 compute phases, so MAC1 reads the previous `V0.x`. This
is the HWB-010/011 vertex-explosion mechanism, captured live on the user's
console. The emulator models it (`cpu.rs:1234`, `gte_v0x_hazard`).

**hl-psx's position (MEASURED by reading its source).**
`game/src/world_pipeline.rs:__hlpsx_project_local_vertices` issues six `MTC2`s
and then `RTPT` **with no gap at all**:

```
.word 0x488c2000    ; MTC2 $12 -> VXY2
.word 0x488d2800    ; MTC2 $13 -> VZ2
.word 0x4a080030    ; RTPT, immediately
```

The same shape appears in `__hlpsx_project_soa_vertices`. That is exactly the
schedule PSoXide documents as unsafe, in a routine that ships and that the user
has burned to disc.

**Three possible resolutions, all actionable.**

1. The hazard is `MVMVA`-specific and does not apply to `RTPT`. The emulator's
   gate is `opcode == 0x01 || (opcode == 0x12 && ...)`, i.e. RTPS and MVMVA, and
   deliberately **not** RTPT (0x30). If that gate is right, PSoXide's two NOPs in
   `project_triangle_mips` and `rtpt_kick` are pure waste, ~2 cycles per vertex
   triple across every model and room face in three games.
2. The hazard does apply to RTPT and hl-psx has a rare, unnoticed vertex bug.
3. The hazard depends on the operand-write cadence rather than the opcode, in
   which case both repos are partially right and the SDK needs a documented
   rule rather than a blanket pad.

**How to settle it.** `engine/examples/hardware-tests` already has the harness
and the SDK already ships `transform_vertex_probed` as a purpose-built hazard
instrument. Add an RTPT variant of that probe: run the unpadded six-`MTC2`-then-
`RTPT` schedule and the padded one over a fixed vertex set, compare `SXY0..2`,
and burn it. Memory `render-guest-visuals-before-burn` and
`disc-burn-drutil-relative-cue-gotcha` apply.

**Why it matters either way.** If (1), we delete two NOPs from the hottest
per-vertex path in the stack. If (2), hl-psx has a latent correctness bug in its
shipped renderer. Neither outcome is small.

### 6.2 Does `MFC2` interlock while the GTE is busy?

`cpu.rs:1330` states that real hardware does **not** stall `MFC2`/`CFC2` reads,
and models only `MAC0` and `LZCR` as returning stale values. But
`rtpt_kick`'s doc comment in `psx-gte/src/scene.rs:485` says the opposite:

> "If the GTE is still busy the first MFC2 interlocks until the op retires,
> exactly like the blocking wrapper."

Both cannot be true. This matters because `rtpt_kick` is used in a
software-pipelined loop in
`engine/crates/psx-engine/src/render3d/world_pass_model.rs` where the overlapped
work between kick and read is short. If `MFC2` does not interlock and the
overlapped work is shorter than `RTPT`'s latency, that loop reads stale `SXY`
on silicon while looking correct in emulation, which is precisely the class of
bug that produced HWB-010.

Settle it with the same hardware-test harness: kick an `RTPT`, execute a
controlled number of NOPs, read `SXY0`, and sweep the NOP count. If the read is
correct at zero NOPs, it interlocks. Fix whichever of the two comments is wrong,
in the same change.

### 6.3 Is the 6-cycle RAM load stall right?

Everything in section 4 is weighted by this number. The emulator cites a
SCPH-9902 timing disc measuring "approximately seven total cycles for all RAM
widths" and "8 clocks per load plus delay-slot pair". That is higher than the
4-5 cycles usually quoted for PSX RAM in homebrew folklore. Memory
`emulator-matches-console-perf` says the console tracks the model, and
`cd-timing-calibrated-from-silicon` shows this team calibrates against silicon
rather than folklore, so I am inclined to believe it.

Confirm it anyway before building 4.1 on top of it. A tight loop of N dependent
`lw`s from RAM against N from the scratchpad, timed with the root counter,
settles it in one disc.

---

## 7. Tooling to add first

None of the items above should be implemented before the measurements exist to
gate them. All four of these are small changes to code we own.

### 7.1 Dynamic instruction-class cycle accounting

Add to `emu/crates/emulator-core/src/cpu.rs`, behind a feature or an env var, a
counter set that accumulates *cycles* (not instruction counts) by class: RAM
load stall, RAM store stall, MMIO stall, I-cache refill stall, GTE busy stall,
muldiv interlock stall, and plain issue. Dump alongside the existing
`--profile-log`.

This turns the entire static census in this document into a dynamic one, and it
is the gate for 4.1: **if dynamic `$sp`-relative load stalls are not a large
share of cycles, do not build the scratchpad stack.** Add `$sp`-relative as a
sub-bucket of the RAM-load class; the base register is right there in the
instruction word.

### 7.2 Per-cache-line PC histogram

Increment `hist[(pc - text_base) >> 4]` per step. Dump on exit. Gives you the
hot set for 4.3, tells you whether it exceeds 4 KiB, and identifies which
symbols are actually hot rather than which ones look hot. hl-psx did this by
hand against a linker map; make it a first-class emulator feature so all three
games get it.

### 7.3 Stack high-water measurement

Pattern-fill the stack region at boot, scan for the lowest modified word at
frame end, report the maximum over a tape. Needed to size the scratchpad stack
in 4.1 and useful on its own, since `psoxide.ld` reserves a flat 32 KiB today
with no evidence behind the number.

### 7.4 `IBLKSZ` sweep

One line in the guest's boot: write a different `IBLKSZ` to `0xFFFE_0130` and
re-run the tape with `icache_refill_stall_cycles` logged. The emulator already
models the 2-word mode. Cheap enough to just do; I expect no gain, since the
default 4-word fill should win on any code with sequential runs, but the cost of
finding out is about ten minutes.

---

## 8. Suggested execution order

Each phase gates the next. Do not skip ahead.

**Phase 0, measurement.** 7.1, 7.2 and 7.3. Nothing else. Re-run
`tools/text_census.py` after any build change so the static baseline stays live.
Exit criterion: you can state the dynamic cycle split by class and the render
subtree's stack high-water mark.

**Phase 1, the two silicon questions.** 6.1 and 6.2 in one hardware-test disc,
plus 6.3 in the same disc since it is a different loop in the same harness.
These are correctness questions as much as performance ones and they invalidate
or confirm assumptions the later phases rest on.

**Phase 2, the big lever.** 4.1, gated on Phase 0's dynamic number and Phase 1's
confirmation of the load cost. Land the trampoline in `psx-rt` with the canary
and the depth guard. Gate on `--profile-log` deltas plus the multi-frame visual
hash comparison that memory `perf-changes-need-multiframe-visual-gates` requires.

**Phase 3, the cache.** 4.2 first because it is mechanical and independent, then
4.3 using Phase 0's PC histogram. Gate on `icache_refill_stall_cycles`, not FPS.

**Phase 4, the cheap remainder.** 4.4, 4.5, 4.7 in whatever order the profile
says. 4.6 only inside assembly you are already touching.

**Throughout.** Every change lands in the SDK if it can (4.8). Every change is
gated on the canonical tape with exact display and VRAM hashes where the change
is meant to be output-neutral, and on eyes-on visual review where it is not.
Single-run visual A/Bs are not valid on hl-psx (memory
`hl-psx-wedge-regression`).

---

## 9. What I am least sure about

Stated plainly so you can attack the weak points first rather than discovering
them at implementation time.

1. **Static instruction mix is not dynamic instruction mix.** Everything in
   section 0's table is a static decode of `.text`. Prologue and epilogue spills
   are overweighted relative to loop bodies. The 58.6% stack-load figure is the
   number most exposed to this. 7.1 exists specifically to replace it. If the
   dynamic figure comes back at 20%, item 4.1 drops below item 4.3 in the
   ranking and the whole document reorders.
2. **The 4 KiB direct-mapped conflict argument in 4.3 is my inference**, not a
   measurement. hl-psx measured three layout experiments that all lost, and I am
   arguing they did not test the variable that matters. I could be wrong; the
   hot set may simply be too large for placement to help. 7.2 answers this
   before you write any linker-script code.
3. **The scratchpad stack could be blocked outright by depth or by interrupts.**
   1 KiB is not much. If the render subtree needs 3 KiB, the idea is dead in its
   general form and degrades to the array-hoisting fallback, which is worth
   much less.
4. **I did not run the game.** Every number here comes from decoding the shipped
   binary, reading the emulator's timing source, and compiling probe functions.
   No tape was replayed for this document. The existing documented baselines
   (843k-cycle render vblank, the stage split in `CLAUDE.md`) are consistent
   with what I found, which is mild corroboration, not verification.
5. **The GTE-is-idle claim is imported from hl-psx**, whose renderer is not
   PSoXide's. PSoXide's own baseline says "project" is 108k of a 322k player
   stage and attributes it to scalar register shuffling around the GTE rather
   than GTE compute, which agrees in direction. Confirm with 7.1's GTE-busy
   bucket rather than assuming.
