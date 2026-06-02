# Architecture Cleanup Roadmap

Staged plan for tightening PSoXide's structure, ordered most-critical first.
Each stage is independent enough to land on its own branch and be verified with
`make check` / `make test` before the next.

The guiding rule: **fix the number of states the program can be in before fixing
the number of lines in a file.** Most of the debt here is encoded as data layout,
not as text length. Splitting a 40K-line file into ten 4K-line files reduces
scrolling and merge conflicts but buys zero encapsulation if every function still
holds `&mut self` over the same 93 fields. We attack the state shape first; the
file splits then fall out mechanically.

## Context (what this is *not*)

This is a mature codebase, not a beginner project. It already has:

- A real correctness suite: `parity-oracle`, BIOS/disc **canaries**, commercial
  **visual guards**, `runtime-numeric-guard`, `lint-policy-guard`, ~1,560 tests.
- Workspace lints that matter: `missing_docs`, `unsafe_op_in_unsafe_fn = deny`,
  `clippy -D warnings`, `unused_must_use = deny`.
- A lean, render-free `emulator-core` and a genuinely shared `psx-gte-core`
  consumed by **both** the SDK (`mipsel-sony-psx`) and the host emulator.

The debt is concentrated in the **editor / UI layer** and in the **build
topology**. The emulator domain logic is in good shape; it is deliberately last.

Measured snapshot (Rust LOC, excluding `target/` and `.claude/worktrees/`):

| Area     | LOC    |
|----------|--------|
| emu      | 89,372 |
| editor   | 87,985 |
| engine   | 54,554 |
| sdk      | 15,799 |
| crates   |  3,157 |
| tools    |    689 |
| **total**| ~251K  |

The single worst offender, in hard numbers:

`editor/crates/psxed-ui/src/lib.rs` — **40,324 lines**, one `EditorWorkspace`
struct with **93 fields**, **32 `impl` blocks**, **1,191 functions** (170 of them
`draw_*`/`ui_*`), and **166 inline tests** reaching private internals. The graph
attributes **481 edges** to `EditorWorkspace`: that is not 481 collaborators, it is
481 reach-ins to one mutable bag.

---

## Stage 0 — Collapse illegal state-spaces in `EditorWorkspace` — CRITICAL

The highest value-to-risk fix in the repo, and the prerequisite for everything in
Stages 1 and 2. Two clusters of fields encode states the program can never legally
be in, so the compiler can't help and invariants are maintained by hand.

**0a. The interaction fields are one state machine spelled as seven booleans-in-disguise.**

```rust
viewport_box_select:  Option<ViewportBoxSelect>,
primitive_drag:       Option<PrimitiveDrag>,
primitive_grid_drag:  Option<PrimitiveGridDrag>,
primitive_gizmo_drag: Option<PrimitiveGizmoDrag>,
node_gizmo_drag:      Option<NodeGizmoDrag>,
node_drag:            Option<NodeDrag>,
ui_canvas_drag:       Option<UiCanvasDrag>,
```

At most one is ever `Some`. Seven `Option`s give 2⁷ representable states for ~8
legal ones. "Clear the other six when one starts" is a hand-maintained invariant
scattered across the drag handlers, and it is exactly what produces "two drags
active at once" bugs. Collapse to one field:

```rust
enum Interaction {
    Idle,
    BoxSelect(ViewportBoxSelect),
    PrimitiveDrag(PrimitiveDrag),
    PrimitiveGridDrag(PrimitiveGridDrag),
    Gizmo(GizmoDrag),       // primitive + node gizmo merge here
    NodeDrag(NodeDrag),
    UiCanvasDrag(UiCanvasDrag),
}
interaction: Interaction,
```

The transitions (`Idle → PrimitiveDrag → Idle`) become a pure function unit-testable
with no egui in scope.

**0b. The ~19 modal / dialog fields are one `enum Modal`.** `new_project_dialog_open`,
`new_project_name`, `new_project_error`, `delete_project_dialog_open`,
`delete_project_error`, the rename buffers, etc. You can't have two modals open, but
the type says you can. Same disease, same cure:

```rust
enum Modal {
    None,
    NewProject { name: String, error: Option<String> },
    DeleteProject { error: Option<String> },
    RenameNode { id: NodeId, buffer: String },
    // ...
}
```

**Outcome.** ~25 fields disappear from `EditorWorkspace`, a class of bugs becomes
unrepresentable, and the interaction/modal logic becomes pure and testable.

**Verify.** `cargo check -p psxed-ui`, `make check`, `make test`. No behavior change;
the diff is type-driven and bisectable. Land 0a and 0b as separate commits.

---

## Stage 1 — Decompose `EditorWorkspace` into owned sub-states — CRITICAL

With the illegal states gone, decompose the remaining ~68 fields into cohesive
sub-structs that own their slice of state:

```
EditorWorkspace
├── session:     ProjectSession   // project, dir, saved name, dirty/name-editing
├── selection:   Selection        // nodes / resources / sectors / primitives + anchors + modes
├── interaction: Interaction      // Stage 0a
├── modal:       Modal            // Stage 0b
└── viewport:    ViewportState    // camera, hover, box-select preview
```

This is the step that actually moves coupling. `draw_inspector` takes
`&mut self.selection`, not `&mut self`. Each sub-struct gets its own small `impl`
and its own tests, and the file split (Stage 2) becomes a mechanical follow-on
rather than cosmetic line-shuffling.

The 166 inline tests get re-pointed at the sub-state they actually exercise
(selection math, viewport projection, interaction transitions) and stop asserting
on monolith internals, so they test **behavior** instead of ossifying layout.

**Verify.** `cargo check -p psxed-ui`; `make check` + `make test`. Do it one
sub-struct per commit so each is reviewable.

---

## Stage 2 — Split the file along the new seams — HIGH

Only now is a file split worth doing, because Stage 1 created real boundaries to
split along. Move each sub-state and its draw functions into a module:
`session`, `selection`, `interaction`, `modal`, `viewport`, plus feature modules
(`dialogs`, `inspector`, `scene_tree`, `world_grid`, `play_mode`). Any draw fn over
~150 lines becomes a widget struct that owns its transient state and exposes
`ui(&mut self, …)`. Target: **no file > 1,500 lines.**

Draw functions should only translate state→widgets and events→intents. Pure logic
moved out in Stage 1 stays out.

**Verify.** `cargo check -p psxed-ui` then `make check`; `make test` for moved tests.
One module per commit.

---

## Stage 3 — Workspace topology (the Makefile is a symptom) — HIGH

**Problem.** 5 separate `[workspace]` roots (`/`, `emu`, `editor`, `engine`, `sdk`)
plus per-example workspaces — **27 `Cargo.lock` files**. A **609-line** Makefile
papers over it: `make check`/`test`/`lint`/`fmt` each `cd` into four workspaces and
run `cargo` four times. No single `cargo` (or rust-analyzer) session sees the whole
graph; shared deps recompile per `target/`; `egui`/`wgpu`/`winit`/`glam` skew is
invisible until it bites.

**The legitimate constraint.** Host crates (`emu`, `editor`: `std` + `wgpu`/`egui`)
and bare-metal crates (`sdk`, `engine`: `no_std`, `mipsel-sony-psx`,
`-Z build-std`, `panic=abort`) genuinely cannot share one `cargo build --workspace`.

**Fix — split on the axis that matters (target), not by folder.** Two workspaces:

- `host`: `emu`, `editor`, host tools.
- `device`: `sdk`, `engine`, on-hardware examples.
- Shared `no_std` cores (`psx-gte-core`, `psx-math`, …) live once, consumed by both
  via path deps.

Collapse the root, per-example, and redundant lockfiles into those two. Outcome:
two lockfiles, two `cargo` entry points, and the Makefile shrinks to thin wrappers.

Sequenced after the editor decomposition because Stages 0–2 are where the daily
bleeding is; this is high-effort plumbing whose payoff is unified tooling.

**Verify.** `make check`/`make test` green from both workspaces; a single
`cargo check --workspace` works within each root.

---

## Stage 4 — Layering / dependency direction — HIGH

**Problem.** The egui UI crate `psxed-ui` depends directly on `psx-engine`,
`psx-level`, `psx-gte`, `psx-asset`; `psxed-project` pulls
`psx-engine` + `psx-level` + `psx-asset` + `psx-iso`. UI, project data model, asset
pipeline, and runtime engine are entangled, so the data model can't be built or
tested without the renderer.

**Fix.** Enforce a layered graph:
`psxed-format` / `psxed-project` (pure data — **no egui, no engine**) ←
`psxed-ui` (egui) ← `psxed` (binary). Reach engine/SDK through a thin adapter trait,
not across three workspaces. Make it a `lint-policy-guard` rule that denies `egui`
imports in core crates. The Stage 1 decomposition is a prerequisite: the project
data model has to stop being co-owned by the UI struct first.

**Verify.** `cargo tree -p psxed-project` shows no `egui`/`psx-engine`; `make check`.

---

## Stage 5 — Examples that are secretly applications — MEDIUM

**Problem.** `engine/examples/editor-playtest/src/main.rs` is **13,378 lines** with
its own `Cargo.lock` and a checked-in **3,381-line generated**
`level_manifest.cooked.rs`. Similar in `psxed-project/src/playtest.rs` (9,761) and
`psxed-project/src/lib.rs` (12,799).

**Fix.** If it's a real tool, promote it to a proper crate with modules. If it's a
sample, shrink it to sample size. Route generated artifacts through `build.rs` /
`OUT_DIR`, never hand-checked source.

**Verify.** `make build-editor-playtest` / `make cook-playtest` produce identical
output; regenerated manifest is byte-stable.

---

## Stage 6 — Domain god-files — LOW (cohesive, not kitchen sinks)

**Problem.** `engine/.../render3d.rs` (6,686), `world_render.rs` (5,580),
`emulator-core/gpu.rs` (4,755), `psx-gpu-compute/rasterizer.rs` (4,064),
`cdrom.rs` (3,397), `spu.rs` (3,142), `bus.rs` (2,720).

**Why last.** Each is **one domain** — size hides internal state machines, not a
grab-bag. They are the hot, correctness-critical paths, so churn is riskier and the
parity suite is what protects them.

**Fix.** Extract sub-modules along natural seams: GPU → command parse / VRAM ops /
drawing; CD-ROM → command FSM / sector read / CDDA; SPU → voice / mixer / reverb.
Pure refactor, behavior-preserving, guarded by `parity` + `canaries`.

**Verify.** `make parity`, `make canaries`, `make commercial-visual-guards` unchanged.

---

## Cross-cutting: testing

The emulator side is well covered (parity-oracle, canaries, visual guards). The thin
spot is editor/UI logic, where ~462 tests are buried inline in the monolith and
reach private internals. The fix is not a new integration tier bolted on; it is a
side effect of Stages 0–2: once selection math, drag-delta math, and interaction
transitions are pure functions on small types, they unit-test without egui and the
pyramid corrects itself. Add a `tests/` suite at each new crate seam created by
Stages 3–4 so the refactor is netted as it lands.

---

## Already right — do not touch

- Lean, render-free `emulator-core`.
- `psx-gte-core` shared between SDK and emulator (real domain reuse).
- Workspace lints (`missing_docs`, clippy `-D warnings`, `unsafe_op_in_unsafe_fn`).
- The `parity-oracle` + canary + visual-guard correctness strategy.

---

## Execution order

0. **Stage 0** — collapse illegal states (interaction enum, modal enum). Day-scale,
   type-proven, immediately shrinks the struct and kills a bug class.
1. **Stage 1** — decompose `EditorWorkspace` into sub-states. The seam that matters.
2. **Stage 2** — split the file along those seams (now mechanical).
3. **Stage 3** — workspace topology, once tooling pain justifies the plumbing.
4. **Stage 4 → 5 → 6.**

Each stage is its own branch, gated on `make check` + `make test` (plus `make parity`
for Stage 6). No stage merges without the guards green locally.
