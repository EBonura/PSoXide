# Cortex Ignition 0.4 — Vitality Finish-Line Handoff

**Date:** 2026-09-01  
**Purpose:** complete technical, product, visual, validation, and continuation handoff for the Cortex Ignition 0.4 finish-line work  
**Implementation worktree:** `/private/tmp/psoxide-vitality-finish`  
**Implementation branch:** `codex/vitality-circles-finish-line`  
**Last pushed commit:** `011a1a13ac271830b92a16c652c94f819f5bfcd2` — `Complete vitality systems and optimize Cortex 0.4`  
**Remote state at handoff:** `origin/codex/vitality-circles-finish-line` points to the same commit  
**Current implementation state:** substantial uncommitted work exists on top of the pushed commit

---

## 1. Executive summary

This branch turns the dual Horizon/Zenith vitality concept into a functional gameplay loop and moves Cortex Ignition 0.4 substantially closer to release:

- the player and enemies now have meaningful stance behavior;
- attacks are mapped relative to the active stance;
- enemies expose a readable stance and mutate their guard;
- vitality circles can be authored, cooked, claimed, entered, and used for refill/drain decisions;
- three boost modules are placed on persistent POI rewards;
- poise/stagger foundations, gameplay sound hooks, animation authoring improvements, and multiple renderer/RAM optimizations are present;
- the map uses the third-person affine profile and Release lighting;
- the latest uncommitted layer adds the user-approved world-circle look, revised HUD, smooth menu focus motion, directional dash wireframing, title palette changes, and a critical executable-size recovery.

The branch is promising but **not ready to merge to `main` yet**. The current blockers are operational and acceptance-oriented rather than a lack of broad implementation:

1. The newest work is uncommitted in an ephemeral `/private/tmp` worktree.
2. A clean checkout likely fails to compile `editor-playtest` because the tracked placeholder manifest is missing vitality-circle declarations/imports.
3. Several motion features have not been shown at correct real-time pacing.
4. The current executable sits on a fragile 2 KiB sector boundary.
5. The map still reports serious packet pressure.
6. Original-hardware performance has not established the requested 15 FPS merge gate.
7. Boss-arena music trigger zones remain unimplemented.

The immediate goal for the next agent is therefore: **preserve the work, restore clean-checkout reproducibility, prove the existing features correctly, then profile and optimize without regressing the accepted visuals.**

---

## 2. Source-of-truth and workspace safety

### 2.1 Use the correct checkout

The implementation is **not** in the primary checkout.

| Location | Branch/state | Instruction |
|---|---|---|
| `/private/tmp/psoxide-vitality-finish` | `codex/vitality-circles-finish-line` at `011a1a13`, with WIP changes | **Use this worktree for the vitality finish-line implementation.** |
| `/Users/ebonura/Desktop/repos/PSoXide` | unrelated `fix/bsp-on-plane-leaf-lookup`, with unrelated user changes | **Do not implement or reset the vitality work here.** This checkout only contains this durable handoff file. |

The main checkout has unrelated modified and untracked files. Do not clean, reset, checkout over, or mechanically copy the vitality work into it.

### 2.2 Current git state in the implementation worktree

The pushed branch is synchronized with its remote at `011a1a13`, but the latest work is not durable:

- 15 modified tracked files;
- 2 untracked texture assets;
- approximately 778 insertions and 191 deletions;
- nothing staged at the time of audit;
- `git diff --check` passes.

Modified files:

```text
editor/projects/cortex-ignition-tech-demo-0.4/project.ron
engine/crates/psx-engine/src/game_app.rs
engine/crates/psx-engine/src/ui.rs
engine/crates/psx-game-runtime/src/model_rendering.rs
engine/crates/psx-game-runtime/src/model_rendering/instances.rs
engine/crates/psx-game-runtime/src/vitality.rs
engine/crates/psx-game-runtime/src/vram.rs
engine/examples/editor-playtest/src/main.rs
engine/examples/editor-playtest/src/model_rendering.rs
engine/examples/editor-playtest/src/overlay.rs
engine/examples/editor-playtest/src/playtest_scene.rs
engine/examples/editor-playtest/src/playtest_update.rs
engine/examples/editor-playtest/src/runtime_config.rs
engine/examples/editor-playtest/src/vitality_circle_runtime.rs
engine/examples/editor-playtest/src/vram_runtime.rs
```

Untracked but required assets:

```text
editor/projects/cortex-ignition-tech-demo-0.4/source_assets/textures/vitality_circle_machine_glyph_v1.png
engine/examples/editor-playtest/assets/vitality_circle_machine_glyph_64.psxt
```

Asset facts:

| Asset | Format | SHA-256 |
|---|---|---|
| `vitality_circle_machine_glyph_v1.png` | 64×64 RGBA PNG | `7a847d09f0ef3266b1b935c745e4ca7c9e1ea39a53154606b8274f2aa9dd7514` |
| `vitality_circle_machine_glyph_64.psxt` | cooked 4bpp, 16-color PSXT | `dcda4f9e0ac29e35230f40bd990ce03e66db10e63645cb27b9486b09ce7037a7` |

### 2.3 First action for the next agent

Checkpoint the exact WIP before tuning anything else. The safest sequence is:

1. Inspect `git status --short` and `git diff --check` in `/private/tmp/psoxide-vitality-finish`.
2. Add only the 15 expected tracked files and the two expected texture assets.
3. Commit the current WIP as a preservation checkpoint.
4. Push the branch.
5. Fix the clean-checkout placeholder-manifest issue in a separate commit.

Do not fold speculative visual changes or performance experiments into the preservation commit.

---

## 3. Product intent and non-negotiable design rules

### 3.1 Vitality-circle mechanic

Preserve these rules exactly unless the user explicitly changes them:

1. A circle has one fixed authored axis: Horizon or Zenith.
2. It starts dormant.
3. Only a matching-axis attack claims it.
4. A claimed circle acts on the player's **active** vitality pool while the player is inside.
5. Matching active stance refills; mismatched active stance drains.
6. Voluntary stance swapping is locked while inside.
7. Leaving immediately stops refill/drain.
8. Leaving does not clear the claim.
9. Circle drain can break one pool and force a swap, but cannot directly empty both pools and kill the player.
10. Claims currently persist for the run only; no authored checkpoint/save persistence exists yet.

The stance lock is the key design rule. Without it, every circle becomes a free heal because the player can swap after entry.

### 3.2 Visual language

- Horizon / HRZ: orange-red.
- Zenith / ZTH: teal-cyan.
- CORTEX title: Zenith palette.
- IGNITION title: Horizon palette.
- Stance changes should read on the character mesh, not only in text or bars.
- The world circle should retain its dedicated machine-glyph texture, layered pulse, and animated cage. The user explicitly approved this appearance.
- The HUD charge circle is a separate widget from the world circle. The user has **not** accepted the current HUD widget yet.
- The quality bar is the main-menu button motion and activation echo: purposeful movement, readable anticipation, and a satisfying completion response.

### 3.3 Merge gate

The user authorized merging to `main` only after:

1. Cortex 0.4 runs from the demo disc;
2. the representative level runs at least around 15 FPS;
3. the relevant visuals and motion are verified;
4. no severe packet-overflow artifacts or size regressions remain.

A standalone disc boot or emulator-only flip count does not, by itself, satisfy the merge gate.

---

## 4. Complete status matrix

Status meanings:

| Status | Meaning |
|---|---|
| **Done / pushed** | Implemented in `011a1a13` and available remotely |
| **Done / WIP** | Implemented in the uncommitted layer; preserve and verify |
| **Partial** | Foundation exists, but behavior or acceptance is incomplete |
| **Verify** | Code exists; final visual, gameplay, performance, or hardware proof is outstanding |
| **Not started** | No implementation found |
| **Deferred** | Intentionally postponed or retained as future substrate |

### 4.1 Original vitality handoff tasks

| # | Requirement | Status | Current implementation | Remaining acceptance |
|---:|---|---|---|---|
| 1 | Rebuild player bars with slanted/forked language | **Done / WIP, verify** | Runtime now owns the player vitality cluster. Active bar is 144×16, inactive is 112×13, with stronger chamfers. Old authored player HUD nodes were removed. | Native 320×240 review through idle, swap, break, drain, and regen. |
| 2 | Bind active stance, swap progress, broken state | **Done / pushed** | Player dual vitality and stance bindings are live. | Demonstrate both stances, forced break, cooldown, and recovery. |
| 3 | Centered enemy stance HUD | **Done / pushed, verify** | Target health, secondary health, active stance, and swap progress bindings are authored. Hard lock is primary; soft lock is fallback. | Show an enemy changing stance at combat distance; confirm centering and no target churn. |
| 4 | Vitality-circle decal | **Done / WIP, user accepted** | Dedicated 64×64 machine glyph, additive outer/inner decals, counter-pulse, octagonal chase cage, cardinal ticks. | Preserve. Measure packet cost and ensure asset inclusion. |
| 5 | Circle claim/refill/drain/lock behavior | **Done / pushed** | Compact runtime state, matching strike claim, room/radius tests, refill/drain, swap lock, persistent claim. | Record complete mechanic loop in one gameplay capture. |
| 6 | Enemy stance | **Done / pushed, verify** | Guarded channel takes 0.5×, exposed channel 1.5×; guard rotates on recovery and emits a mutation tell. | Balance and verify both enemy types. |
| 7 | Three boost modules on POIs | **Done / pushed, verify fresh cook** | Rupture Filament, Zenith Condenser, Inertial Shell; each POI has one persistent reward. | Fresh cook, collect each once, confirm inventory/socket/persistence. |
| 8 | Place two circles | **Done / pushed** | Horizon by early light enemy; Zenith in heavy-enemy area; radius 768, refill/drain 12/s. | Confirm no unintended overlap and useful tactical placement. |
| 9 | Scale player/light/heavy cast and collision | **Done / pushed, verify** | Light visual scale 512 with offset normalized; heavy scale 416; collision targets approximate 120% and 170% of player. | Same-plane lineup screenshot and collision/hurtbox check. |
| 10 | Use third-person affine profile | **Done / pushed, perf open** | PXBSP paths use `ClassicAffineProfile::PXBSP_THIRD_PERSON`. | A/B floor warping and packet cost. |
| 11 | Release light bake | **Done / pushed** | `BSP_COOK_IS_RELEASE: true`; authored project uses Release BSP cook. | Final recook and light-leak/contrast review. |
| 12 | Correct enemy/POI ordering against geometry | **Done / pushed, verify** | Removed the body-instance behind/in-front-of-player double-pass and substantially simplified marker code. | High-resolution moving capture near walls and occluders. |
| 13 | Gameplay sounds | **Done / pushed, verify audio** | Footstep, light hit, heavy hit, player damage, enemy death; separate SPU voice range from UI. | Emulator and hardware balance/listening pass. |
| 14 | R1/R2 follow active stance | **Done / pushed** | R1/R2 map to active light/heavy; L1/L2 retain opposite light/heavy. | Test all four buttons in both stances. |
| 17 | Gate collision-cylinder debug renderer | **Done / pushed** | Behind `collision-debug-overlay`; excluded from shipping build unless requested. | Clean shipping symbol/size confirmation. |
| 18 | Delete BOX_PROPS | **Deferred intentionally** | Linker GC already removes the implementation when unused. | Optional source cleanup only; no release RAM priority. |

### 4.2 Requirements added during the session

| Requirement | Status | Notes / remaining work |
|---|---|---|
| Player stance mutation animation | **Done / pushed, verify motion** | Bottom-to-top destination-color sweep mutates submitted textured packets without resubmitting geometry. User liked the player effect. |
| Enemy stance mutation animation | **Done / pushed, verify motion** | Same projected sweep concept reads enemy guard state. Must be visible at combat distance and agree with target HUD. |
| Smooth main-menu cursor | **Done / WIP, not accepted yet** | Eight-rendered-frame smoothstep moves focus chrome/ring from the old control to the new control. Previous video did not demonstrate it. |
| Smooth pause-menu cursor | **Done / WIP** | User confirmed the motion concept, but font scale needed correction. |
| Reduce giant pause/inventory fonts | **Done / WIP, verify** | Several italic 1.0 labels changed to regular `Spleen5x8` at 0.75 scale. |
| Directional dash wireframe conversion/restoration | **Done / WIP, not accepted yet** | Solid → directional converting → full wire → directional restoring → solid. Previous video did not show the effect. |
| Poise system | **Partial** | Core poise/stagger existed in the parent commit; this branch integrates authored poise damage with stance hits. Player interruption semantics and tuning remain. |
| Boss-arena trigger starts CD-streamed music | **Not started** | Reuse intro track initially. Requires trigger-volume authoring/runtime integration and CD-stream coexistence rules. |
| Authorable circle pacing | **Done / pushed** | Axis, radius, refill, drain, and enabled state are editor-authored and cooked. |
| Confirm motion features with video | **Outstanding** | Old videos were incorrectly paced and are not acceptable evidence. |
| Audit old implementations and leftovers | **Partial** | Duplicate player HUD and old body-depth split removed; this handoff identifies remaining hazards. |
| Broad visual-polish pass | **Partial** | World circle is strong. HUD, dash, target HUD, combat effects, and global presentation still need acceptance. |
| CORTEX Zenith palette / IGNITION Horizon palette | **Done / WIP** | Project title colors updated. |
| Remove lines emerging from HRZ | **Done / WIP** | Old authored connector/fork cluster removed with the authored player HUD. |
| More pronounced health-bar slant | **Done / WIP, verify** | Runtime bars use larger chamfer cuts. |
| Remove old health bar behind new bar | **Done / WIP, visually observed at idle** | Latest inspected frame shows one player HUD cluster and no old HP core/duplicate authored bars. Motion states still need review. |
| Make active bar longer | **Done / WIP** | Active 144 px; inactive 112 px. |
| Remove HP text; use consumable/filling circle | **Partial / WIP** | HP text/core removed. Lean four-phase clockwise HUD charge ring and completion echo added. User has not accepted it. |
| Texture the HUD charge circle | **Deferred by executable budget** | Richer HUD ring crossed a 2 KiB executable sector and stalled boot. Revisit only after offsetting size savings. |
| Destructible-environment-style system | **Not started** | Next major gameplay system; needs its own budgeted design pass. |
| Complete/polish animation and combat | **Partial roadmap** | Clip authoring, in-place controls, stance tells, dash, sounds, and poise foundations exist. Final timing/content/balance remains. |
| General polish and missing systems | **Partial roadmap** | Break into scoped work after the performance gate. |

---

## 5. What is in pushed commit `011a1a13`

The commit contains 71 files, 2,475 insertions, and 825 deletions. It is the last durable remote checkpoint, not the complete current state.

### 5.1 Vitality-circle vertical slice

The commit adds the full authoring-to-runtime path:

- `NodeKind::VitalityCircle` with axis, radius, refill rate, drain rate, and enabled state;
- editor placement, bounds, colors, and inspector controls;
- cook validation with a hard maximum of 32 circles;
- `LevelVitalityCircleRecord` manifest output;
- compact `VitalityCircleState` using one `u32` claim bitset, one active index, and one fractional accumulator;
- matching-axis melee/environment claim integration;
- current-room and radius evaluation;
- match refill, mismatch drain, and stance locking;
- tests for claim persistence, refill/drain/lock behavior, and non-lethal two-pool drain.

Key files:

```text
editor/crates/psxed-project/src/scene_types.rs
editor/crates/psxed-ui/src/inspector_transform_node.rs
editor/crates/psxed-ui/src/workspace/painting.rs
editor/crates/psxed-project/src/playtest.rs
editor/crates/psxed-project/src/playtest/manifest.rs
engine/crates/psx-level/src/lib.rs
engine/crates/psx-game-runtime/src/vitality_circles.rs
engine/examples/editor-playtest/src/game_logic_runtime.rs
engine/examples/editor-playtest/src/playtest_update.rs
engine/examples/editor-playtest/src/vitality_circle_runtime.rs
```

Important limits:

- 32 circles maximum because claims use a `u32` bitset;
- update rates assume a 60-tick simulation;
- overlapping claimed circles are order-dependent because the state tracks one active circle and one accumulator;
- claims reset with the playtest and are not in save/checkpoint data.

### 5.2 Enemy stance/guard behavior

Enemies now have a live defensive stance:

- same-channel attacks are guarded at 0.5×;
- opposite-channel attacks land at 1.5×;
- guard rotates when the enemy enters `Recover`;
- a 12-tick mutation tell is exposed for rendering;
- damage numbers show scaled damage;
- poise damage remains independently authored rather than stance-scaled.

RAM-conscious packing reuses the existing attack-contact byte:

- bit 0: swing contact latch;
- bits 1–4: mutation elapsed;
- bit 7: Zenith guard.

This adds no new per-enemy array at the 64-actor cap.

### 5.3 Stance-relative controls

`update_attack_input` maps:

- R1: active stance light;
- R2: active stance heavy;
- L1: opposite stance light;
- L2: opposite stance heavy.

Heavy bindings are checked first when both buttons on one shoulder are pressed in the same tick. Missing clips disable only their associated input.

### 5.4 Stance-mutation mesh sweep

The player and enemy tells are implemented as a post-pass over already-submitted `TriTextured` packets:

- a rising projected frontier applies the destination stance color;
- packet opcode/tag is preserved;
- geometry is not resubmitted;
- no second packet buffer is allocated;
- the dash line-packet path is guarded to avoid applying the typed mutation to mixed packet types.

`PrimitivePacketArena::mutate_typed_slots<T>` has a strict unsafe contract: every slot in the selected interval must really contain `T`. Future renderer changes must preserve the immediate-post-submission, textured-only invariant.

### 5.5 Target vitality HUD

The commit adds target bindings for:

- Horizon health/max;
- Zenith health/max;
- active stance;
- stance swap progress.

Hard lock is the target authority, with soft lock as fallback. The authored target HUD is visible only while a valid target exists. Fast soft-lock churn may still make the HUD switch or appear/disappear abruptly; this needs a motion test.

### 5.6 Gameplay SFX

Five resident gameplay events are implemented:

- footstep;
- light hit;
- heavy hit;
- player damage;
- enemy death.

The scene raises event bits during update and `GameApp` drains them once after simulation. Gameplay uses SPU voices 16–19; UI uses 20–23. Same-type events in one simulation tick coalesce into one sound by design.

Project assets are deterministic 11.025 kHz generated samples with provenance in:

```text
editor/projects/cortex-ignition-tech-demo-0.4/assets/audio/gameplay/SOURCE.md
editor/projects/cortex-ignition-tech-demo-0.4/tools/generate_gameplay_sfx.py
```

### 5.7 Rendering, RAM, and content changes

The pushed commit also:

- replaces heap BSP material bindings with a fixed array and hard-fails over `MAX_ROOM_MATERIALS`;
- gates collision-cylinder debug drawing behind `collision-debug-overlay`;
- submits body model instances once instead of using the coarse two-pass player-relative split;
- aligns dynamic actor/world depth behavior with resident PXBSP OTZ handling;
- uses `PXBSP_THIRD_PERSON` for the PXBSP world/special/alias paths;
- switches Cortex 0.4 to Release BSP cooking;
- resizes/reseats light and heavy enemies and updates collision dimensions;
- authors the three boost modules and POI rewards;
- authors the two vitality circles;
- reduces legacy marker code substantially.

### 5.8 Tests added by the pushed commit

Focused tests cover:

- gameplay SFX sample-rate handling and event routing;
- circle authoring, manifest emission, claim persistence, refill/drain/lock, and non-lethal drain;
- PXBSP third-person profile selection;
- typed packet mutation bounds and tint-sweep packet/opcode behavior;
- enemy guarded/exposed scaling and guard mutation timing;
- target UI live values and target absence;
- footstep gait edge triggering;
- dynamic-world/PXBSP OT slot equivalence.

---

## 6. What is only in the uncommitted WIP

### 6.1 Polished world vitality circle

This is the most visually successful part of the WIP and should be preserved.

Rendering per visible/current-room circle currently consists of:

- one additive outer machine-glyph decal;
- one counter-pulsing inner machine-glyph decal;
- four chasing outer octagon arcs;
- four inner edges;
- four cardinal ticks.

The implementation uses a new generic allocation-free `draw_ground_decal` path, refactoring actor shadows to call the same primitive while preserving separate UV origin, floor lift, depth bias, and material policy.

The dedicated glyph occupies the free 64×64 block at U=0, V=64 inside the shared 128×128 gameplay-effects quadrant. Only a 16-entry CLUT consumes new allocator space.

Integration chain:

1. PNG source is manually cooked to PSXT.
2. `runtime_config::VITALITY_CIRCLE_GLYPH_BLOB` embeds the PSXT.
3. `VramRuntime::upload_vitality_circle_texture` uploads texels and a new CLUT.
4. `Playtest::init_gameplay` stores the returned material.
5. `draw_vitality_circles_world` submits the two decals and cage.
6. Gameplay VRAM release frees the new CLUT.

Known risks:

- no distance/PVS/object-visibility culling beyond current-room matching;
- up to two textured quads and twelve lines per circle;
- no checked-in cooker recipe or provenance command for the PNG→PSXT conversion;
- gameplay → title → gameplay re-entry may expose stale gameplay-effect material handles because release/reupload lifecycle needs explicit validation.

### 6.2 Runtime-owned player HUD

The WIP removes the old split ownership that caused a moving runtime bar to appear underneath an authored slanted bar. The runtime now owns:

- active bar: 144×16, chamfer cut 7;
- inactive bar: 112×13, chamfer cut 5;
- HRZ/ZTH labels;
- ladder ticks;
- the left mutation charge circle;
- completion echo.

The authored project HUD root now contains only the target HUD. Thirteen old player HUD nodes, including the HP core, forks, fills, labels, progress bar, and broken bar, are no longer children of the HUD root.

Current charge-circle behavior:

- empty at swap start;
- four coarse clockwise fill segments over swap progress;
- cardinal completion echo for thirteen ticks;
- no texture, to avoid the executable-size regression.

The latest inspected 320×240 frame confirms:

- gameplay is reached;
- the long active HRZ bar and shorter inactive ZTH bar are visible;
- the old HP core is gone;
- no duplicate authored player bar is visible at idle.

It does **not** prove:

- the consume/refill/echo motion;
- broken-state presentation;
- a Zenith-active layout swap;
- target-HUD coexistence;
- frame-time cost.

### 6.3 Smooth focus motion

`FlowCursor` now stores the previous focus node and elapsed transition frames. `focus_motion_offset` smoothsteps the focused chrome from the old navigation rect to the new one over eight rendered frames.

Scope and caveats:

- the focus ring and focus-only button chrome move;
- label/content remains at its destination;
- transitions triggered by `move_focus` animate;
- direct focus mutation paths may still jump;
- no new unit test exists;
- prior video did not demonstrate the main-menu effect and is not acceptance evidence.

### 6.4 Directional dash wireframe

The WIP expands the pre-existing full-wire middle phase into:

```text
Solid -> Converting -> Wire -> Restoring -> Solid
```

`DashWireVisual` carries phase progress and a screen-side origin. Dash-left begins on screen-left; dash-right begins on screen-right. Conversion and restoration use a projected-X frontier. The wire color is brightened to `(196, 255, 255)`.

Important implementation compromise:

- the middle `Wire` phase replaces the textured body;
- transition phases keep the textured body and overlay sparse wire lines;
- only every fourth face contributes a line;
- stance tint is suppressed during every non-solid dash phase.

This may still look too subtle because the transition is an overlay rather than progressive removal/restoration of textured faces. It must be judged in correctly paced motion.

### 6.5 Title palette and typography cleanup

The project WIP changes:

- CORTEX to Zenith teal: `(67,169,154)` → `(121,225,200)`;
- IGNITION to Horizon orange: `(214,75,48)` → `(248,126,72)`;
- target TGT/HRZ/ZTH labels from 1.0 italic to regular `Spleen5x8` at 0.75;
- multiple pause/inventory captions from oversized italic 1.0 to regular 0.75.

These changes need native-resolution screenshots of the main menu, pause menu, inventory, and target HUD.

### 6.6 Executable-size rescue

The WIP adds `#[inline(never)]` to selected dash and HUD helpers, especially:

- `dash_wire_visual`;
- `dash_wire_contains_x`;
- rectangle/chamfer/HUD drawing helpers.

These annotations are not cosmetic. They recovered one 2 KiB executable sector and restored gameplay boot. Do not remove them casually.

---

## 7. Critical clean-checkout bug

The tracked placeholder manifest is incomplete.

`engine/examples/editor-playtest/src/main.rs` imports `generated::VITALITY_CIRCLES`, but:

```text
engine/examples/editor-playtest/generated/level_manifest.rs
```

does not define `VITALITY_CIRCLES`. It also declares `GAMEPLAY_SFX_CUES` without importing its record type. Local builds pass because ignored:

```text
engine/examples/editor-playtest/generated/level_manifest.cooked.rs
```

exists locally, and `build.rs` prefers the cooked manifest whenever present.

### Required fix

Add the missing typed imports and an empty placeholder:

```rust
pub static VITALITY_CIRCLES: &[LevelVitalityCircleRecord] = &[];
```

Then prove compilation in a clean detached worktree with no ignored cooked manifest. This should be a separate commit after the WIP preservation checkpoint.

Do not claim clean-checkout reproducibility until this test has been performed.

---

## 8. RAM, executable size, packet pressure, and performance

### 8.1 Current measured state

| Metric | Current evidence |
|---|---:|
| Raw MIPS executable | **1,122,304 bytes** |
| Fastboot payload | **1,120,256 bytes** |
| Raw EXE path | `/private/tmp/psoxide-vitality-finish/build/examples/mipsel-sony-psx/release/editor-playtest.exe` |
| Headless gameplay boot | Successful after final no-inline reclaim |
| Latest boot evidence | `tick=1100000000 cycles=2681103117 pc=0x80093b28 route-ticks=4691 port1-polls=4355` |
| Map RAM estimate | 901,230 / 2,097,152 |
| Resident asset estimate | 651,680 / 985,088 |
| BSP | 212,768 bytes |
| PVS | 12,682 bytes |
| Light data | 28,644 bytes |
| Textures | 389,076 bytes |
| Packet warning | 2,837 / 1,536 |
| Emulator display-flip observation | approximately 19.47 flips/sec on the restored map |
| Original-hardware FPS | not established |

### 8.2 The 2 KiB sector cliff

A richer HUD-ring implementation produced:

- raw EXE: 1,124,352 bytes;
- fastboot payload: 1,122,304 bytes;
- black-transition stall near PC `0x8001024c`.

The lean HUD still crossed the boundary until selected dash helpers were marked `#[inline(never)]`. The working raw EXE then returned to 1,122,304 bytes and gameplay booted.

Consequences:

- record raw EXE and payload size after every MIPS build;
- treat a 2,048-byte increase as potentially catastrophic even if linking succeeds;
- keep the no-inline annotations unless an equivalent size saving is proven;
- offset every new visual/system feature with code/data savings;
- test actual gameplay boot, not only successful linking.

### 8.3 Packet pressure is a separate problem

Successful linking does not solve the conservative packet warning of 2,837 against 1,536.

Potential contributors include:

- `PXBSP_THIRD_PERSON` subdivision at greater distance;
- dense map geometry;
- world-circle decals and cage lines;
- runtime HUD primitives;
- actor/model rendering and shadows;
- effects during combat.

The next performance pass should collect representative heavy-sector counts, not only whole-level averages. The key question is which sector/view combination drives the worst submitted primitives and frame time.

### 8.4 Hardware caveat

The current emulator does not establish original-GPU draw/DMA cost. Emulator display flips are useful evidence, but the 15 FPS release gate still requires original-hardware or the closest available hardware-equivalent measurement.

---

## 9. Validation evidence and its limits

### 9.1 Session-level tests already reported

| Validation | Result |
|---|---|
| `cargo test --manifest-path engine/Cargo.toml -p psx-game-runtime -q` | 190 + 5 passed |
| `cargo test --manifest-path engine/Cargo.toml -p psx-engine -q` | 401 + 1 passed; 4 ignored |
| `cargo test --manifest-path engine/examples/editor-playtest/Cargo.toml -q` | 46 passed |
| Host check | passed |
| MIPS build | passed |
| Disc generation | passed |
| Headless gameplay boot after final size reclaim | passed |
| `git diff --check` | passed |

These are session records, not committed logs. They were run with an ignored cooked manifest present, so they do not prove clean-checkout compilation.

### 9.2 Current artifacts

```text
/private/tmp/psoxide-reclaimed-hud.ppm
/private/tmp/psoxide-reclaimed-hud.png
/private/tmp/psoxide-vitality-finish/build/examples/mipsel-sony-psx/release/editor-playtest.exe
/private/tmp/psoxide-vitality-finish/editor/projects/cortex-ignition-tech-demo-0.4/baked/cortex_ignition_tech_demo_0_4.bin
/private/tmp/psoxide-vitality-finish/editor/projects/cortex-ignition-tech-demo-0.4/baked/cortex_ignition_tech_demo_0_4.cue
```

The PPM is the original latest dump; the PNG is only a local conversion for inspection. Temporary artifacts should not be treated as durable project files.

### 9.3 Latest visual inspection

The latest 320×240 frame was inspected during this handoff. It proves:

- the level reaches gameplay;
- world geometry and the player render;
- the message overlay renders;
- the revised player vitality HUD renders;
- HRZ appears as the longer active bar;
- ZTH appears as the shorter inactive bar;
- the old HP core and duplicate player bar are not visible at idle.

It does not prove motion, hardware speed, world-circle appearance, target ordering, or the swap charge cycle.

---

## 10. Correct video-capture procedure

The prior videos looked near 5 FPS because every route screenshot was encoded, including repeated display frames. That is a capture bug, not reliable runtime evidence.

The route CSV includes:

```text
display_start_changed
```

Correct procedure:

1. Record route screenshots and route CSV together.
2. Keep only frames whose CSV row has `display_start_changed == 1`, or otherwise pace by actual display flips.
3. Encode those frames at the measured display cadence.
4. Do not duplicate repeated emulator route frames.
5. Keep emulator flip rate and original-hardware FPS labeled separately.

Required proof set:

| Feature | Required proof |
|---|---|
| Main-menu focus travel | correctly paced video changing between at least two options |
| Pause-menu focus travel | video plus native screenshot showing corrected typography |
| Player stance mutation | both directions in real-time video |
| Enemy stance mutation | both directions and both enemy classes if practical |
| Dash wireframe | dash-left and dash-right conversion/full-wire/restoration |
| Player HUD | active/inactive length exchange, no duplicate bar, no HP core |
| HUD charge circle | consume, clockwise refill, one completion echo |
| World vitality circle | dormant, claim hit, claimed pulse, match refill, mismatch drain |
| Poise | an attack interrupting/staggering an enemy |
| Enemy/POI depth | moving capture across wall occlusion boundaries |
| Cast scale | same-plane player/light/heavy screenshot |
| Release lighting | high-resolution representative-room screenshot |

---

## 11. Known risks and loose ends

### P0

1. **Ephemeral uncommitted work:** accepted world-circle visuals and mandatory assets can be lost if `/private/tmp` is cleaned.
2. **Clean-checkout compile failure:** tracked placeholder manifest lacks vitality-circle declarations/imports.
3. **Executable sector boundary:** tiny code changes can restore the black-transition stall.
4. **No valid motion proof:** cursor, dash, HUD charge, and enemy mutation remain unaccepted because the videos were mispaced.

### P1

1. **Gameplay VRAM re-entry:** gameplay resources are released on exit, but a front-end → gameplay re-entry needs explicit verification for shadow, particle, and new vitality materials.
2. **World-circle packet cost:** two decals plus up to twelve lines per current-room circle, without distance/PVS culling.
3. **Dash subtlety:** transition phases overlay sparse lines on the fully textured body instead of progressively withholding textured faces.
4. **Target HUD churn:** soft-lock fallback can switch abruptly.
5. **Overlapping circles:** first matching manifest circle wins; authoring should prevent overlap or runtime semantics must be defined.
6. **Session-only circle claims:** no save/checkpoint persistence.
7. **Target/player HUD acceptance:** label scale, slants, bar exchange, and broken presentation need native review.
8. **Poise completeness:** enemy foundation exists; player interruption behavior and final tuning are open.
9. **Third-person affine cost:** visually useful but potentially expensive in the dense map.
10. **Audio balance:** event wiring exists but hardware mix and voice pressure are untested.

### P2

1. **Boss music trigger:** no trigger volume or music transition exists yet.
2. **Global visual polish:** combat, animation transitions, hit reactions, and environmental effects remain uneven.
3. **Destructible-environment feature:** not designed or budgeted yet.
4. **BOX_PROPS source cleanup:** large code cleanup but essentially no linked RAM benefit while unused.

### Earlier editor issues outside this branch's verified scope

Do not assume these were solved by the vitality work:

- light editor bounding boxes were still visible through geometry in a later screenshot;
- POI editor icons should be occluded by room geometry;
- the light enemy editor preview was reported buried in the ground;
- selection should survive mode changes;
- editor viewport/display scaling was requested.

These may live on other branches or in unrelated changes. Audit before duplicating work.

---

## 12. Recommended continuation plan

### Phase A — preserve and restore reproducibility

1. Commit and push the exact WIP, including both texture assets.
2. Fix the tracked placeholder manifest.
3. Create a clean detached worktree from the new commit.
4. Run editor-playtest host compilation with no cooked manifest.
5. Re-run focused tests.

### Phase B — rebuild and guard the binary

1. Fresh-cook Cortex 0.4.
2. Build MIPS.
3. Record raw EXE byte size.
4. Build the project disc.
5. Boot through the black transition into gameplay.
6. Exercise title → gameplay → title → gameplay re-entry.
7. Fail the iteration if the binary crosses the known-good sector without a demonstrated replacement saving.

### Phase C — visual acceptance

1. Capture the player HUD at idle, swapping both directions, breaking, and recovering.
2. Capture the HUD charge circle consume/refill/echo.
3. Capture main and pause focus movement with corrected timing.
4. Capture dash-left and dash-right wire conversion/restoration.
5. Capture player and enemy stance mutation.
6. Capture both world circles through their full mechanic loop.
7. Capture target HUD and depth ordering near geometry.
8. Capture cast scale and Release lighting.

### Phase D — performance pass

1. Add or use per-sector/per-view draw-cost instrumentation.
2. Identify the worst representative camera positions.
3. Record BSP surfaces, subdivisions, dynamic triangles, decals, lines, shadows, and total packet demand.
4. Compare world-circle on/off and third-person affine profile alternatives without changing visual quality blindly.
5. Cull world-circle rendering by distance/PVS if measurement supports it.
6. Validate on original hardware or the best hardware-equivalent route.
7. Reach the 15 FPS target in representative heavy sectors.

### Phase E — demo disc and merge

1. Integrate Cortex 0.4 into the actual demo-disc build.
2. Boot and play the level from that disc.
3. Repeat size, packet, and FPS checks.
4. Run final tests from the merge candidate.
5. Merge to `main` only when the user-defined gate is satisfied.

### Phase F — next gameplay work

After the performance/merge milestone:

1. design the destructible-environment-style system with explicit RAM/packet budgets;
2. complete animation transitions, missing clips, hit reactions, and combat balancing;
3. implement boss-arena trigger zones and CD-streamed music switching;
4. perform the full presentation-polish pass.

---

## 13. Suggested validation commands

Run from `/private/tmp/psoxide-vitality-finish` unless noted otherwise.

```sh
git status --short --branch
git diff --check

cargo test --manifest-path engine/Cargo.toml -p psx-game-runtime -q
cargo test --manifest-path engine/Cargo.toml -p psx-engine -q
cargo test --manifest-path engine/examples/editor-playtest/Cargo.toml -q

make build-editor-playtest
wc -c build/examples/mipsel-sony-psx/release/editor-playtest.exe

cd emu
cargo run -p frontend --release -- build-project-disc \
  --project ../editor/projects/cortex-ignition-tech-demo-0.4/project.ron
```

For the clean-placeholder test, prefer a fresh detached worktree after committing rather than deleting or moving the current ignored cooked manifest:

```sh
git worktree add --detach /private/tmp/psoxide-vitality-clean-check HEAD
cd /private/tmp/psoxide-vitality-clean-check
cargo check --manifest-path engine/examples/editor-playtest/Cargo.toml
```

Use an explicit temporary path and remove it only after the check. Do not clean the implementation worktree.

---

## 14. Definition of done

The finish-line branch is ready to merge only when all of the following are true:

- [ ] Current WIP and both vitality-circle assets are committed and pushed.
- [ ] A clean checkout compiles editor-playtest without a pre-existing cooked manifest.
- [ ] Raw MIPS executable remains within the known-good working sector.
- [ ] Cortex 0.4 boots to gameplay after every final build.
- [ ] Gameplay → title → gameplay re-entry preserves effect textures/materials.
- [ ] Correctly paced videos prove main-menu and pause-menu focus travel.
- [ ] Correctly paced videos prove player and enemy stance mutation.
- [ ] Correctly paced videos prove directional dash conversion and restoration.
- [ ] Player HUD has no duplicate bar, HP core, stray connector lines, or giant labels.
- [ ] Active/inactive bars exchange length and emphasis correctly.
- [ ] HUD charge circle consumes, fills clockwise, and emits one completion echo.
- [ ] User-approved world-circle visuals are preserved.
- [ ] Both circles complete the dormant/claim/refill/drain/lock loop.
- [ ] Target HUD agrees with enemy stance and behaves correctly with no target.
- [ ] All three POI boost modules cook, collect, persist, and socket.
- [ ] Cast scale/collision and Release lighting are visually accepted.
- [ ] Gameplay sounds are balanced and reliable.
- [ ] Poise/stagger behavior is tuned and demonstrable.
- [ ] Representative heavy sectors avoid severe packet-drop artifacts.
- [ ] Original-hardware or best hardware-equivalent performance meets the 15 FPS gate.
- [ ] Cortex 0.4 runs from the actual demo disc.
- [ ] Final tests pass from the merge candidate.
- [ ] The branch is merged to `main` and the demo-disc integration is pushed.

---

## 15. Do-not-regress list

- Do not overwrite or reset the unrelated main checkout.
- Do not lose the untracked world-circle PNG or PSXT.
- Do not replace the user-approved world-circle look with the old reused shadow blob.
- Do not reintroduce the authored player HUD behind the runtime HUD.
- Do not reintroduce the HP text/core or stray HRZ connector lines.
- Do not make active and inactive bars the same length.
- Do not remove `#[inline(never)]` size guards without measuring the final MIPS binary and booting gameplay.
- Do not judge motion using route screenshots encoded at one frame per route tick.
- Do not claim hardware FPS from emulator display flips.
- Do not delete the `LevelLogicRecord` substrate; it is the natural basis for checkpoints and boss trigger volumes and is free while linker-stripped.
- Do not prioritize BOX_PROPS deletion as a RAM optimization.
- Do not merge before demo-disc boot and the user-defined performance gate.

---

## 16. Final handoff state

The project is much closer to the finish line than the unaccepted videos suggested. The core vitality loop, enemy stance, content rewards, control remap, animation tooling, sounds, and several critical renderer/RAM changes are already present. The latest uncommitted visuals also include the strongest circle presentation so far and a materially cleaner HUD.

The next agent should resist expanding scope immediately. The highest-value path is:

> preserve → fix clean checkout → verify correctly → measure heavy sectors → optimize → demo-disc boot → merge

Only after that gate should work move to boss music triggers, destructible environments, and the final combat/animation polish.
