# Architecture Cleanup Roadmap

Staged plan for tightening PSoXide's structure, ordered most-critical first.
Each stage is independent enough to land on its own branch and be verified with
`make check` / `make test` before moving on.

## Context (what this is *not*)

This is a mature codebase, not a beginner project. It already has:

- A real correctness suite: `parity-oracle`, BIOS/disc **canaries**, commercial
  **visual guards**, `runtime-numeric-guard`, `lint-policy-guard`, ~1,560 tests.
- Workspace lints that matter: `missing_docs`, `unsafe_op_in_unsafe_fn = deny`,
  `clippy -D warnings`, `unused_must_use = deny`.
- A lean, render-free `emulator-core` (deps: `psx-hw`, `psx-iso`, `psx-trace`,
  `psx-gte-core`, `thiserror`).
- A genuinely shared domain core: `psx-gte-core` is consumed by **both** the SDK
  (on-hardware, `mipsel-sony-psx`) and the emulator (host).
- Existing architecture docs (`editor-architecture.md`, `world-grid-architecture.md`,
  `frontend.md`, …).

The debt is **structural** and concentrated in two places: the build topology, and
the editor / UI layer. The emulator domain logic is in good shape — leave it mostly
alone (Stage 5 is deliberately low priority).

Measured snapshot (Rust LOC, excluding `target/` and `.claude/worktrees/`):

| Area     | LOC    | Tests |
|----------|--------|-------|
| emu      | 89,372 | 704   |
| editor   | 87,985 | 462   |
| engine   | 54,554 | 197   |
| sdk      | 15,799 | 137   |
| crates   |  3,157 |  56   |
| tools    |    689 |   8   |
| **total**| ~251K  | 1,564 |

---

## Stage 1 — Workspace topology (the Makefile is a symptom) — CRITICAL

**Problem.** There are 5 separate `[workspace]` roots (`/`, `emu`, `editor`,
`engine`, `sdk`) plus per-example workspaces — ~27 `Cargo.lock` files in total. The
30 KB `Makefile` exists largely to paper over this: `make check`, `make test`,
`make lint`, `make fmt` each `cd` into four workspaces and run `cargo` four times.

**Why it hurts.**
- No single `cargo` invocation (or rust-analyzer session) sees the whole graph.
- Each workspace re-resolves and recompiles shared deps into its own `target/`.
- Version skew between `egui` / `wgpu` / `winit` / `glam` is invisible until it bites.
- The split buys *fragmentation cost* without *independence*: the crates are tightly
  coupled across roots via `../../../` path deps (`editor → engine → sdk → crates`).

**The legitimate constraint.** The split isn't pure accident. Host crates (`emu`,
`editor`: `std` + `wgpu`/`egui`) and bare-metal crates (`sdk`, `engine`: `no_std`,
`mipsel-sony-psx`, `-Z build-std`, `panic=abort`) genuinely cannot share one
`cargo build --workspace`.

**Fix — split along the axis that actually matters (target), not by folder.**
Two workspaces instead of five:

- `host` workspace: `emu`, `editor`, host-side tools.
- `device` workspace: `sdk`, `engine`, on-hardware examples.
- Shared `no_std` cores (`psx-gte-core`, `psx-math`, …) live in one place and are
  consumed by both via path deps.

Collapse the root, the per-example workspaces, and the redundant lockfiles into those
two. Outcome: two lockfiles, two `cargo` entry points, and the Makefile shrinks to
thin wrappers instead of being the only thing that understands the build.

**Verify.** `make check` and `make test` still green from both workspaces; confirm a
single `cargo check --workspace` works within each of the two roots.

---

## Stage 2 — Break up `editor/crates/psxed-ui/src/lib.rs` (40,472 lines) — CRITICAL

**Problem.** One file: ~1.49 MB, 113 structs/enums, 35 `impl` blocks, 166 inline
tests, and individual draw functions up to **838 lines** (`draw_node_kind_editor`),
538 (`draw_world_grid_settings`), 355, 353, 348, …

**Why it hurts.** It's both a kitchen sink *and* a pile of mega-functions:
- Compile bottleneck — any edit recompiles the crate's largest translation unit.
- Effectively unreviewable; a permanent merge-conflict magnet.
- No module boundary = no encapsulation; every type can touch every other.
- The 166 tests are welded to the monolith and reach private internals, so they
  ossify implementation rather than behavior.

**Fix.** Split by feature into modules (and/or sub-crates):
`dialogs` (texture/model import), `viewport` (3D, gizmos, box-select), `inspector`,
`scene_tree`, `world_grid`, `play_mode`. Rust allows `impl App { … }` blocks to be
spread across files in the same crate, so most of this is **mechanical, low
semantic-risk** code movement. Any draw fn > ~150 lines → extract a widget struct
that owns its state and exposes a `ui(&mut self, …)` method. Move each test next to
the unit it covers. Target: **no file > 1,500 lines.**

**Verify.** `cargo check -p psxed-ui` then `make check`; `make test` for the moved
tests. Do it in slices (one module per commit) so each is reviewable and bisectable.

---

## Stage 3 — Layering / dependency direction — HIGH

**Problem.** The egui UI crate `psxed-ui` depends directly on `psx-engine`,
`psx-level`, `psx-gte`, `psx-asset`; `psxed-project` pulls `psx-engine` + `psx-level`
+ `psx-asset` + `psx-iso`. UI, project model, asset pipeline, and the runtime engine
are entangled.

**Why it hurts.** You can't build or test the editor's *data model* without the
renderer; UI edits risk engine rebuilds; there's no clean "core vs UI" seam.

**Fix.** Enforce a layered graph:
`psxed-format` / `psxed-project` (pure data — **no egui, no engine**) ←
`psxed-ui` (egui) ← `psxed` (binary). Consume engine/SDK through a thin adapter
trait rather than reaching across three workspaces directly. Make the rule explicit
(e.g. a `lint-policy-guard` check that denies `egui` imports in core crates).

**Verify.** `cargo tree -p psxed-project` shows no `egui`/`psx-engine`; `make check`.

---

## Stage 4 — Examples that are secretly applications — HIGH

**Problem.** `engine/examples/editor-playtest/src/main.rs` is **13,378 lines**, has
its own `Cargo.lock`, and ships a checked-in **3,381-line generated**
`level_manifest.cooked.rs`. Similar smell in `psxed-project/src/playtest.rs` (9,761)
and `psxed-project/src/lib.rs` (12,799).

**Why it hurts.** A 13 K-line "example" is a second application with none of a
crate's structure; a generated file in source control rots and obscures real diffs.

**Fix.** If it's a real tool, promote it to a proper crate with modules. If it's a
sample, shrink it to sample size. Route generated artifacts through `build.rs` /
`OUT_DIR` or a dedicated `cooked/` artifact dir — not hand-checked source.

**Verify.** `make build-editor-playtest` / `make cook-playtest` still produce
identical output; regenerated manifest is byte-stable.

---

## Stage 5 — Domain god-files — MEDIUM (cohesive, not kitchen sinks)

**Problem.** `engine/.../render3d.rs` (6,686), `world_render.rs` (5,580),
`emulator-core/gpu.rs` (4,755), `psx-gpu-compute/rasterizer.rs` (4,064),
`cdrom.rs` (3,397), `spu.rs` (3,142), `bus.rs` (2,720).

**Why it's lower priority.** Each of these is **one domain** — size mostly hides
internal state machines, not a grab-bag of unrelated concerns. They're also the
hot, correctness-critical paths, so churn here is riskier and the parity suite is
what protects them.

**Fix.** Extract sub-modules along natural seams: GPU → command parse / VRAM ops /
drawing; CD-ROM → command FSM / sector read / CDDA; SPU → voice / mixer / reverb.
Pure refactor, behavior-preserving, guarded by `parity` + `canaries`.

**Verify.** `make parity`, `make canaries`, `make commercial-visual-guards` unchanged.

---

## Stage 6 — Even out the test pyramid — MEDIUM

**Problem.** The emulator side is well covered (parity-oracle, canaries, visual
guards). The thin spots: editor/UI logic (462 tests across ~88 K LOC, many buried
inline in the monolith) and only **11** `tests/` integration files repo-wide.

**Fix.** As Stages 2–4 split crates, add a `tests/` integration suite at **each new
seam** so the refactor is netted as it happens. Pull pure logic out of egui draw
code so it becomes unit-testable without a UI. Lean on `parity-oracle` as the golden
master for any emu-side change.

**Verify.** Coverage of the new core crates; integration tests green in CI.

---

## Already right — do not touch

- Lean, render-free `emulator-core`.
- `psx-gte-core` shared between SDK and emulator (real domain reuse).
- Workspace lints (`missing_docs`, clippy `-D warnings`, `unsafe_op_in_unsafe_fn`).
- The `parity-oracle` + canary + visual-guard correctness strategy.
- Existing architecture docs.

---

## Suggested execution order

1. **Stage 1** first — it unblocks a unified `cargo check`/`test`/clippy and makes
   every later refactor cheaper to verify.
2. **Stage 2** next — highest-visibility win, mostly mechanical.
3. Stages 3 → 4 → 6 → 5.

Each stage = its own branch, gated on `make check` + `make test` (and `make parity`
for Stage 5). No stage should be merged without the guards green locally.
