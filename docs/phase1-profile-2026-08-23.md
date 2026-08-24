# Phase 1 low-level profile: Cortex, Quake, Half-Life

**Date:** 2026-08-23
**Input:** `docs/optimization-handoff-2026-08-23.md`
**Status:** revised after review. Symbolized attribution now available; four
conclusions in the first version were overstated and are corrected below.

> **Revision note.** The first version of this document drew conclusions from
> cycle-class aggregates alone. Review correctly identified four overstatements.
> All four are corrected here, three of them reversed outright by direct
> measurement that the first version did not have. Section 0 lists them.

This records Phase 0 recovery and the Phase 1 comparable profiles. It exists to
be read before the next code change, and it corrects two priorities in the
handoff's ranked list.

---

## 0. Corrections to the first version

| First version said | Correct position | Basis |
|---|---|---|
| "The GTE is idle everywhere… GTE offload is not a lever" | **Wrong.** The GTE runs 2,046 commands per rendered frame in Cortex, 2,711 in Quake. Zero interlock stalls means GTE latency is *fully hidden*, which implies headroom, not disuse. | `gte_busy_stall_cycles` only accrues on a COP2 command issued while the GTE is busy (`cpu.rs`: `opcode == 0x12 && instr & (1<<25)`). It measures scheduling quality, not utilisation. My own instruction-class table already contradicted the claim. |
| Cortex's 12.34% stack-relative load stall is "spill and reload traffic" | **Unproven.** It is every load through `$sp`: locals, stack arrays, struct fields, saved registers, incoming arguments. | `cpu.rs:1007` classifies it as literally `stack_relative: base == 29`. Nothing distinguishes a spill from a local. |
| HL's MMIO cost comes from the immediate-mode HUD/message/fade pass at `main.rs:1300` | **Wrong callsite.** 96.0% of HL's MMIO stall cycles are in `psx_gpu::framebuf::FrameBuffer::begin_swap`. No HUD, message or fade symbol appears anywhere near the top. | Direct per-PC MMIO stall attribution against a linker map for the exact profiled binary. |
| Target A (Cortex register-resident blend kernel) is the right next change | **Demoted.** Its actual function, `flush_blended_model_vertex_chunk`, is 4.7% of work instructions and ranks 7th. Model geometry submission (26.9%) and collision (20.7%) are far larger. | Symbolized `--pc-line-log` attribution. |

The first version's own "Known gaps" section admitted symbol resolution was
missing and that this blocked callsite attribution, then named a callsite anyway.
That was the sharpest error and it is what the rest of this revision fixes.

The cycle-class measurements themselves stand unchanged. What was wrong was the
interpretation layered on top of them.

---

## 1. Phase 0: what happened to the evidence

`/private/tmp` was pruned by the OS during this session, between two tool calls.
Lost: the built `frontend`, the three per-game discs and EXEs, and the tmp-side
evidence directories.

Rescued just before the prune:

- the three clean release commits, fetched into the live repos as
  `refs/release/2026-08-23-{psoxide,quake,hl}-clean`;
- the 12,265-sample Half-Life acceptance tape, copied to
  `psx-demo-disc/dist/hardware-candidate/tapes/hazard-current-menu.pxtape`
  (SHA-256 `f814caa8…`, unchanged).

Still intact: the combined `.bin`/`.cue` with the receipt's hashes, and the
10-file `chainload-evidence` copy inside `psx-demo-disc`.

**The release receipt no longer verifies.** It pins input paths under
`/private/tmp`, so it now fails closed on missing inputs. That is the receipt
behaving as designed, not corruption. Re-pinning it to durable paths is
integration work, not measurement work, and was not done here.

### Live repository state at handover (unchanged by this session)

| Repo | HEAD | Tracked diff SHA-256 |
|---|---|---|
| PSoXide | `a8c69b29` | `b9818ff94fa4da54acda1d51deb433cc825adc52ddb94e92d43a8b9c693eeff9` |
| quake-psx | `c56c416a` | `257514aef2f891540444c3babecc5283cebbe937057c0210e9a3b640e3d306f1` |
| hl-psx | `8d9e4c50` | `ca460bb27f22bc84e530cf6e958a0aec85a0e4d0ffc7ca402412c00d134b0d23` |
| psx-demo-disc | `6a619c98` | `a1124995f518d00cbc59edf9c491877e3211fa799164891296c9f7e9d81c7b21` |

All four match the handoff byte for byte. Nothing was cleaned, reset, staged, or
committed.

### Rebuilt baselines

Durable worktrees at `repos/.perf-2026-08-23/{psoxide,psoxide-pin,quake,hl}`,
checked out from the preserved refs. `psoxide-pin` is `1e8cb8de`, the SDK every
game links; `psoxide` is `7b95d937`, which adds only the Cortex camera fix.

| Program | Rebuilt artifact vs receipt | Functional validation |
|---|---|---|
| Quake | **byte-identical** (`dc2fb60a…`) | E1M1 chain bench **21.222 fps**, exactly the handoff figure |
| Half-Life | differs | full 12,265-sample tape consumed (12,317 polls) |
| Cortex | differs | passes the release textured-gameplay gate |

Quake reproduces byte-exactly because it stages its guest build at a canonical
path. Cortex and Half-Life embed absolute build paths in the guest binary, so
they are **source-equivalent rebuilds, not proven byte-identical** to the shipped
discs. Cortex's exe hash even changed between two of my own bakes that differed
only in stage root. Treat all three as self-consistent baselines; do not treat
the Cortex/HL rebuilds as the release artifacts.

### Recipe corrections found the hard way

- Quake's build binary is at the **workspace root** (`cargo run --release -- disc`),
  not `--manifest-path host/quake-build/Cargo.toml` as the handoff states.
- Both game repos need `.psoxide/` hydrated *before* cargo can resolve, via
  `psoxide-link --from <psoxide checkout> --into .psoxide`. Cargo cannot build
  the builder that would otherwise do it.
- Quake's off-main pin check calls GitHub and fails on a network timeout. A
  local release snapshot is by definition off-main, so
  `QUAKE_PSX_ALLOW_PSOXIDE_OFF_MAIN=1` is correct here.
- Half-Life's 461 MB `data/` is gitignored and exists only in the live repo.
- `make disc` for Half-Life **installs into `~/Downloads/ps1 games/`**, overwriting
  the library copy. Expected per project convention, but it is a side effect.

---

## 2. Routes used

A profile is only as good as its route. Two of the three needed work.

| Game | Route | Gameplay evidence |
|---|---|---|
| Cortex | `quake-e1m1-geometry` project disc, presses `600/1000/1400:cross` + `--hold-forward` | 1,869 qualifying textured frames, ticks 88–6076, median 489 textured tris |
| Quake | `e1m1-chain-bench` disc, `--digital-pad --guest-frames 3800` | project's own canonical route, PASS, 2,086 frames |
| Half-Life | shipping disc + the 12,265-sample Hazard tape | 12,317 polls, full tape consumed |

**The Cortex `quake_units_arena` disc is a bad perf route.** It boots and passes
the release gate (65 qualifying frames, 36-frame sustained run, 65 distinct
hashes), but `--hold-forward` walks the player into a wall around tick 1335 and
the view goes static for the remaining ~5,200 ticks. Screenshots confirm the game
is running fine the whole time; the triangle gate simply stops qualifying. The
`quake-e1m1-geometry` project sustains real traversal and is the route to use.

Two negative results worth not repeating:

- Cortex ignores d-pad `left`/`right` for turning. A run with ten alternating
  200-tick turn holds produced **byte-identical** VRAM and display hashes to the
  run without them. Camera yaw is not reachable through `--press`.
- `--counter-log` is empty on shipping Cortex discs. Camera/room counters are
  guest telemetry and need an `emulator-telemetry` build.

Half-Life's step cap matters: at `--steps 5910000000` (the handoff's figure) my
rebuild reached only 12,038 of 12,265 polls. `8000000000` completes the tape.
Different disc layout changes CD timing, so the cap is build-specific.

---

## 3. The comparable profile

Whole-route totals, all three from the same frontend build.

| | Cortex | Quake | Half-Life |
|---|---:|---:|---:|
| profiled CPU cycles | 3.471 G | 6.949 G | 21.423 G |
| instructions | 1,499.8 M | 3,180.9 M | 6,011.7 M |
| **CPI** | **2.31** | **2.18** | **3.56** |
| route ticks | 6,078 | 12,165 | 35,012 |
| display flips | 1,998 | 3,796 | 13,297 |
| I-cache refills | 68.5 M | 130.7 M | 573.3 M |

Cycle attribution, percent of profiled CPU cycles. The first nine rows partition
to 100%; `stack` is a subset of `ram load`.

| class | Cortex | Quake | Half-Life |
|---|---:|---:|---:|
| issue | 43.20% | 45.78% | 28.06% |
| **ram load stall** | **40.68%** | **35.71%** | **29.94%** |
| ram store stall | 3.01% | 2.98% | 3.21% |
| **mmio stall** | 0.94% | 4.19% | **23.90%** |
| icache refill stall | 9.73% | 9.17% | 13.08% |
| muldiv interlock | 2.44% | 2.17% | 1.77% |
| gte busy stall | 0.00% | 0.00% | 0.00% |
| uncached fetch | 0.00% | 0.00% | 0.00% |
| other | 0.00% | 0.00% | 0.03% |
| *(of which) stack ram load* | *12.34%* | *7.41%* | *9.78%* |

Three conclusions.

**RAM load stalls are the dominant non-issue cost in all three games**, at 30–41%.
Nothing else comes close in Cortex or Quake. In Cortex nearly a third of that
(12.34 of 40.68 points) is stack-relative.

Be careful what "stack-relative" means. The emulator classifies it as
`stack_relative: base == 29` (`cpu.rs:1007`): any load whose base register is
`$sp`. That covers locals, stack-resident arrays and structs, saved registers in
prologue and epilogue, and incoming arguments. **It is not evidence of compiler
spills**, and the first version was wrong to read it that way. Distinguishing
spill traffic from legitimate local access needs disassembly of the specific hot
functions, which nobody has done yet.

**The GTE is heavily used, and its latency is fully hidden.** `gte_busy_stall` is
0.00% in all three games, but that measures only whether a COP2 command was
issued while the GTE was still busy. Actual utilisation:

| | Cortex | Quake | Half-Life |
|---|---:|---:|---:|
| GTE commands | 4.09 M | 10.29 M | 8.94 M |
| **GTE commands per rendered frame** | **2,046** | **2,711** | **673** |
| GTE register transfers | 23.56 M | 55.04 M | 56.44 M |
| register transfers per command | 5.76 | 5.35 | 6.31 |
| COP2 as % of all instructions | 1.84% | 2.05% | 1.09% |

Zero interlock stalls with 5.76 register transfers per command means the scalar
MTC2/MFC2 shuffle around each command is long enough to cover the GTE's latency
completely. The correct reading is the opposite of the first version's: the GTE
has **latency headroom**, and the cost sits in the scalar transfer traffic
surrounding it. That agrees with the handoff's existing note that the per-vertex
cost is register shuffle rather than GTE compute, and with the standing project
position of keeping work on the GTE rather than moving it to the CPU.

**I-cache is real but second-order**, 9–13%, below RAM load stalls in every game.
The handoff's deprioritisation of cache-layout work is supported.

### Hot-code concentration

Exact retired-instruction counts per canonical 16-byte I-cache line, gameplay
windows only (`--pc-line-start-route-tick` 300 / 600 / 4000).

| game | distinct lines | top 10 | top 50 | top 200 | lines to reach 50% | to reach 90% |
|---|---:|---:|---:|---:|---:|---:|
| Cortex | 10,309 | **22.5%** | 36.3% | 53.2% | 166 | 1,650 |
| Quake | 13,791 | 16.8% | 35.1% | 59.7% | 123 | 1,443 |
| Half-Life | 16,654 | 9.1% | 17.3% | 29.8% | 569 | 2,866 |

Cortex retires 22.5% of all its instructions inside ten cache lines, 160 bytes of
code. Quake is similar. **Half-Life is flat**: it needs 569 lines to reach half
its instructions, more than three times Cortex's 166.

This decides where kernel-level rewrites can pay. Cortex and Quake have tight,
concentrated kernels worth hand-scheduling. Half-Life does not, which is direct
evidence against spending the next Half-Life effort on micro-optimising a single
walker.

---

## 4. Correction to the ranked list: Half-Life is MMIO-bound

Half-Life spends **23.90%** of its CPU cycles stalled on MMIO. Cortex and Quake
spend 0.94% and 4.19%. This does not appear anywhere in the handoff's Half-Life
list.

It is not loading or CD traffic. Windowed over 300-tick buckets:

| game | mmio% min | median | max |
|---|---:|---:|---:|
| Cortex | 0.7 | 0.7 | 4.8 |
| Quake | 0.2 | 0.2 | 42.3 |
| Half-Life | 6.6 | **22.2** | 59.3 |

Half-Life's six heaviest windows, which are gameplay:

| route tick | Mcyc | issue% | ramLd% | stack% | mmio% | icache% |
|---:|---:|---:|---:|---:|---:|---:|
| 31500 | 217.7 | 25.8 | 25.7 | 8.4 | **34.2** | 10.1 |
| 29100 | 199.7 | 23.7 | 28.2 | 9.3 | **30.9** | 12.8 |
| 23400 | 196.0 | 26.3 | 29.9 | 10.2 | 23.9 | 14.4 |
| 23100 | 192.9 | 26.2 | 29.5 | 10.6 | 24.2 | 14.1 |
| 32100 | 192.6 | 25.0 | 28.2 | 8.4 | **31.0** | 11.3 |
| 8700 | 191.8 | 25.5 | 30.7 | 9.8 | 25.5 | 13.5 |

It is a cost-per-access problem, not a volume problem:

| | Cortex | Quake | Half-Life |
|---|---:|---:|---:|
| MMIO accesses | 9.49 M | 100.23 M | 117.58 M |
| MMIO accesses per 1k instructions | 6.32 | 31.51 | 19.56 |
| **stall cycles per MMIO access** | **3.4** | **2.9** | **43.5** |

Half-Life issues *fewer* MMIO accesses per instruction than Quake, and each one
costs about fifteen times more.

**Mechanism.** The emulator charges a GP0 store the GPU's outstanding busy
cycles. This is explicit and tested in `emu/crates/emulator-core/src/bus.rs`,
`architectural_gp0_store_stalls_behind_gpu_execution_backlog`: `charge_busy(1234)`
followed by a GP0 NOP write advances the clock by exactly 1,234.

**Where it is actually paid.** Measured, not inferred. Per-PC MMIO stall
attribution against a linker map for the exact profiled binary:

| symbol | % of HL MMIO stall cycles |
|---|---:|
| **`<psx_gpu::framebuf::FrameBuffer>::begin_swap`** | **96.02%** |
| `<psx_pack::cd::SectorReader>::read_sector` | 2.30% |
| `psx_pad::poll_once_diag` | 0.99% |
| `hl_psx::play` | 0.59% |
| `psx_gpu::submit_linked_list` | 0.03% |
| `hl_psx::hltext::draw_text_scaled` | 0.02% |

The HUD text path is 0.02%. The first version's claimed callsite was wrong by
three orders of magnitude.

`begin_swap` is four register writes: draw area top-left, draw area
bottom-right, draw offset, then `gp1::display_start`
(`sdk/crates/psx-gpu/src/framebuf.rs:85`). It does no work and waits on no
vblank. The stall is entirely the GPU's outstanding backlog being charged to the
first CPU touch of GP0 after the frame's drawing was queued.

**So Half-Life is GPU-bound for roughly a quarter of its cycles.** The CPU is
waiting for the rasteriser to finish, at the flip.

This has a direct consequence the first version got backwards: **CPU
micro-optimisation cannot recover this band.** Making the CPU faster only makes
it arrive at `begin_swap` earlier and wait longer. The handoff's Target D
(tightening the ordinary-quad walker) is issue-side work and cannot touch 23.90%
of Half-Life's cycles.

**The contrast with Cortex is architectural, and also measured.** Cortex's MMIO
band is 0.94%, and 99.62% of *that* is `psx_pad::poll_once_diag`, i.e. controller
polling. Cortex shows essentially zero GPU back-pressure:

| | Cortex | Half-Life |
|---|---|---|
| MMIO stall, % of CPU cycles | 0.94% | 23.90% |
| dominant symbol | `psx_pad::poll_once_diag` (99.62%) | `FrameBuffer::begin_swap` (96.02%) |
| nature | controller poll, unavoidable | GPU draw backlog at the flip |

Cortex avoids the cost because its engine kicks the ordering table
asynchronously and flips on a true vblank IRQ edge, so the GPU draw overlaps CPU
work and the CPU never blocks on GP0. Half-Life's swap is synchronous and absorbs
whatever the GPU has left. That is the difference between 0.94% and 23.90%.

**Confidence.** The attribution is exact and the routes are deterministic (the
instrumented run reproduced the uninstrumented run's VRAM and display hashes
byte for byte). The *magnitude* still depends on the emulator's GPU timing model,
and this lands squarely in the fill-rate regime where the project's standing
caveat bites hardest. Confirm on silicon before funding a rework.

Note this also qualifies a standing project note. `CLAUDE.md` says the emulator
does not model GPU draw time; that is true of the DMA `ot_wait` band, but GP0
*stores* do stall behind the GPU backlog. The two statements are about different
paths, and this measurement is the second one.

Half-Life also does far more stack traffic than the others: 356 M stack loads and
**459 M stack stores** against 37 M / 64 M stores for Cortex / Quake, on only 2–4×
the instructions. 7.6% of all Half-Life instructions are stack stores.

---

## 5. Symbolized Cortex ranking

Two things had to be fixed before any ranking meant anything.

**~15% of Cortex's retired instructions are idle spin.** A single 16-byte line,
`0x80012920`, retires 206.9 M instructions (14.5% of everything) inside
`<psx_engine::app::App>::run_scheduled`. That is the inlined
`clock.wait_next_vblank()` busy-wait on the odd, sim-only ticks of the 60 Hz
sim / 30 Hz render cadence. It is not work, and any instruction-share ranking
that leaves it in the denominator is distorted.

On a work-only denominator (1,176.4 M instructions, spin removed):

| group | M instructions | % of work |
|---|---:|---:|
| **model geometry submit** (`submit_textured_model_geometry_impl`) | 316.1 | **26.9%** |
| **collision** (`CollisionHull::trace_into`, `point_contents_from`, `plane_contact`, providers) | 243.0 | **20.7%** |
| scene render (`GameApp::render`) | 227.0 | 19.3% |
| pxbsp world faces (`draw_pxbsp_faces`, `point_leaf_index`, `visible_bounds_mask`) | 86.3 | 7.3% |
| classic affine batch | 61.4 | 5.2% |
| animation pose | 56.7 | 4.8% |
| **CPU-blend flush, the handoff's Target A** | **55.2** | **4.7%** |
| memcpy/memset/memcmp | 49.9 | 4.2% |
| pad poll | 24.9 | 2.1% |

**Target A ranks seventh.** `flush_blended_model_vertex_chunk` is 4.7% of work
instructions. The handoff ranked it first on the strength of an older capture
that put it at roughly 95 k of 105 k project-stage cycles. Whatever that capture
measured, it is not what this route retires.

Collision at 20.7% is 4.4× larger, and the handoff lists collision only as
*Quake's* Target B. It is a top-two cost in Cortex as well.

### 5.1 How symbolization was unblocked

The first version reported this as a blocking gap. It is now solved, and the fix
is cheap enough that it should be standard.

Guest builds link with `--oformat=binary`, which emits a raw image with no symbol
table, hence no ELF and nothing for `nm` to read. The fix is `-Map`, which makes
lld write a side listing without touching the emitted image.

- **Half-Life already had this**: `HLPSX_LINK_MAP` in `game/build.rs`, passed as
  `cargo:rustc-link-arg` specifically so it does not enter the crate fingerprint.
  Its comment records a measured 133 KB of byte differences when the same flag
  goes through `RUSTFLAGS` instead.
- **Cortex/PSoXide had no equivalent.** I added `PSOXIDE_GUEST_LINK_MAP` to
  `tools/build_guest_staged.sh`, mirroring the Half-Life convention.
  **Proposed, in an isolated worktree, not merged.**

Both verified the only way that matters: with the map enabled the guest exe hash
is **byte-identical** to the profiled binary (`67aad5f6…` Cortex E1M1,
`41dee71f…` Half-Life). The map describes exactly the measured image.

One trap worth recording: Half-Life's `compile` and `disc` actions produce
*different* binaries (`b046d250…` vs `41dee71f…`). Only `disc` reproduces the
profiled image. The map is not the cause: with and without it, `compile` gives
the same `b046d250…`.

### 5.2 MMIO attribution, and why it needed new code

Instruction counts cannot answer an MMIO question: a single GP0 store can stall
for thousands of cycles while retiring one instruction. Nothing existing
attributed stall *cycles* to a PC.

Added `--mmio-stall-line-log` to the frontend: snapshot `mmio_stall_cycles`
around each step and charge the delta to the pre-step PC, into the same 16-byte
line histogram `--pc-line-log` already uses. It is roughly thirty lines, entirely
in `frontend/src/cli.rs`, and needs no `emulator-core` change. **Proposed, not
merged.**

Determinism check: the instrumented Half-Life run reproduced the uninstrumented
run's VRAM (`0x2074c2533a6600ab`) and display (`0xe03375c12bb739c9`) hashes
exactly, so the measurement does not perturb the guest.

---

## 6. Proposed next candidate

**Not Target A.** On measured attribution it is 4.7% of Cortex work instructions
and seventh in rank. A hand-scheduled register-resident kernel cannot pay for
itself against a ceiling that small.

**Cortex: profile inside `submit_textured_model_geometry_impl` (26.9%) and the
collision group (20.7%) before writing any kernel.** Those two are half of
Cortex's work. Collision is the more attractive of the pair, because the handoff
already scopes an equivalent Quake trace kernel (Target B), so one exact hull
trace design could serve both games. That is the kind of shared-substrate win the
handoff asks for, unlike a shared hot renderer.

**Half-Life: stop looking at the CPU.** 23.90% of its cycles are the GPU's draw
backlog collected at `begin_swap`. The two real levers are reducing GPU fill work
and overlapping draw with CPU work the way Cortex's async-kick/boundary-flip
present already does. Both are architecture changes, and both should be checked
on silicon first, because this is exactly the fill-rate regime where the
emulator's GPU model is least trustworthy.

Deprioritise further:

- **Target D** (Half-Life ordinary-quad walker). Issue-side work against a
  GPU-bound band. It also faces a flat profile: Half-Life needs 569 cache lines
  to reach half its instructions, against Cortex's 166.
- **I-cache placement** in all three games. Refill stalls sit below RAM load
  stalls everywhere.

### Before any of this is implemented

Per-symbol *cycle* attribution still does not exist. What exists now is
per-symbol **instruction** share plus per-symbol **MMIO stall** share. A function
with high CPI can cost far more cycles than its instruction share suggests, and
`ram_load_stall` is 30–41% of cycles in every game with no symbol attached to any
of it. Extending the per-PC attribution added in 5.2 to the RAM-load-stall class
is the same code path and would close the last gap. **Do that before ranking
anything by cycles.**

---

## 7. Known gaps in this profile

Stated plainly so the next agent does not assume coverage that is not there.

- **No per-symbol cycle attribution.** The single most important gap; see the end
  of section 6. Instruction share is a proxy, not cycle cost.
- **Quake is not symbolized.** Its map hook exists but is deliberately not an env
  var and is wired only into the `ship-boot` action, which builds a different
  disc than the `e1m1-chain-bench` image I profiled. It also routes through
  `RUSTFLAGS`, which Half-Life's `build.rs` documents as reshuffling code layout.
  A Quake map built that way may not describe the measured binary, so I did not
  produce one rather than produce a misleading one. Wiring the existing
  `request_guest_link_map` into the bench action is the clean fix.
- **No exact I-cache event logs were captured.** The flag exists and works; I
  skipped it because the analysis pointed away from cache layout. Capture it only
  if a candidate targets placement. Exact PC-line logs *were* captured for all
  three games, and every re-run reproduced its earlier VRAM and display hashes
  exactly, so the routes are deterministic.
- Cortex's route is `quake-e1m1-geometry`, not the shipped `quake_units_arena`
  disc. It is the better perf route but it is not the shipping content.
- Quake's 18.72 fps here is a whole-route figure including menu and loading; the
  gameplay-only acceptance number from its own bench is 21.222 fps. Do not mix
  the two denominators.
- Half-Life's 22.79 fps is likewise a whole-route figure over a tape that
  includes menus.
- The Half-Life GPU back-pressure magnitude depends on the emulator's GPU timing
  model. Nothing here is hardware-confirmed.

## 8. Proposed tooling changes (in a worktree, not merged)

Both are isolated in `repos/.perf-2026-08-23/`. Neither touches a live tree.

| change | file | why |
|---|---|---|
| `PSOXIDE_GUEST_LINK_MAP` env hook | `psoxide/tools/build_guest_staged.sh` | emits an lld `-Map` so a profiled guest can be symbolized; verified byte-identical output |
| `--mmio-stall-line-log` | `psoxide/emu/crates/frontend/src/cli.rs` | attributes MMIO stall cycles to the executing PC; ~30 lines, no `emulator-core` change |

The second is what disproved this document's own earlier HUD claim. Some form of
stall-cycle attribution should become standard before further low-level work.

## 9. Artifacts

Profiles are under `repos/.perf-2026-08-23/profiles/{cortex,quake,hl}/`:
`route.csv`, `gpu.csv`, `cpu-cycle.csv`, `instr-class.csv`, `pc.csv`,
`pc-callsite.csv`, `pc-window.csv`, `stack.csv`, `pc-lines.csv`, plus final
display/VRAM PPMs and route screenshots. Cortex and Half-Life additionally have
`mmio-lines.csv`.

Linker maps: `repos/.perf-2026-08-23/e1m1.map` (Cortex) and `hl.map`
(Half-Life), each byte-matched to its profiled binary. Symbolize with:

```
python3 psoxide/tools/pc_line_attribution.py <lines.csv> <map>
```
