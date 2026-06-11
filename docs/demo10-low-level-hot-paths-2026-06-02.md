# Demo10 Low-Level Hot-Path Baseline - 2026-06-02

> Naming note: the project measured here is the one now shipped as
> `cortex_ignition_v1`; newer docs and Makefile targets use that name.

This note records the first measurement pass for SDK/engine low-level optimization work.
The goal was to avoid guessing: capture guest-cycle telemetry first, then choose
functions whose per-frame call volume makes cycle shaving meaningful.

## Capture Setup

Build target:

```sh
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench"
cd tools/mkisopsx && cargo run --release -- \
  --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
  --volume PSOXIDE \
  --cdtest-sectors 32 \
  --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
  --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt
```

Menu capture:

```sh
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest \
  --guest-visual-frames 120 \
  --guest-frames 600 \
  --steps 360000000 \
  --dump-hw /tmp/psoxide-demo10-menu-profile.ppm \
  --dump-hash \
  --dump-guest-profile
```

Gameplay capture:

For the useful gameplay capture, `GAME_FLOW.entry` in the ignored generated
manifest was temporarily changed from `0` to `3` so the disc booted directly
into gameplay. This avoids starting menu CD-DA before streamed room loads. The
generated manifest and mastered disc were restored to menu entry afterward.

```sh
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest \
  --hold-forward \
  --hold-run \
  --guest-visual-frames 800 \
  --guest-frames 1600 \
  --steps 1500000000 \
  --dump-hw /tmp/psoxide-demo10-direct-gameplay-profile.ppm \
  --dump-hash \
  --dump-guest-profile
```

The direct-gameplay screenshot had visible room/image/player content and is the
baseline used below.

## Important Capture Caveat

Booting through the menu, pressing Play via input tape, and leaving menu CD-DA
running produced a sky/HUD-only gameplay profile. Room streaming reported
`CD_ROOM_CHUNK_STATUS=5`, which is `STATUS_CD_ERROR` in
`engine/examples/editor-playtest/src/cd_stream.rs`.

That means the "menu -> gameplay while CD-DA is active" path is not a valid room
render benchmark right now. It is also likely a real runtime issue to handle
separately: menu music and streamed room data cannot both own the CD drive.

## Menu Baseline

Menu-only rendering is cheap:

| Metric | Value |
| --- | ---: |
| Guest frames | 243 |
| Visual frames | 120 |
| Update cycles / sim tick | 749 |
| Render cycles / visual frame | 7,842 |
| GTE ops | 0 |

The menu is not where low-level optimization work should start.

## Gameplay Baseline

Direct gameplay:

| Metric | Value |
| --- | ---: |
| Guest frames | 1,600 |
| Sim ticks | 1,599 |
| Visual frames | 661 |
| Update cycles / sim tick | 269,353 |
| Render cycles / visual frame | 698,278 |
| Visual budget | ~1,128,254 cycles |
| Deadline misses | 136 |
| Latest world commands | 279 |
| Latest primitive packets | 293 |

Render stage breakdown:

| Stage | Cycles / visual | Share of render |
| --- | ---: | ---: |
| Player | 349,263 | 50.0% |
| Image props | 164,528 | 23.6% |
| Room | 58,513 | 8.4% |
| Sky | 48,723 | 7.0% |
| World flush/sort | 47,743 | 6.8% |
| OT submit | 4,893 | 0.7% |

Player/model sub-stages:

| Stage | Cycles / hit | Notes |
| --- | ---: | --- |
| Player draw | 342,745 | Main player render call |
| Model faces | 198,747 | Per-face cull/packet/command loop |
| Model project | 89,486 | GTE projection batches plus CPU-blend vertices |
| Model joints | 51,938 | Joint pose/GTE transform setup |
| Player bounds | 764 | Not worth optimizing first |

Room sub-stages:

| Stage | Cycles / hit | Notes |
| --- | ---: | --- |
| Room cell select | 28,056 | Visible-cell/depth selection |
| Room surface draw | 19,435 | Surface packet path |
| Room project | 8,556 | Cached-room projection |
| Room visible list | 713 | Cheap |

Update stage breakdown:

| Stage | Cycles / hit | Notes |
| --- | ---: | --- |
| Sim solve | 97,991 | Character motor movement/collision response |
| Camera | 66,064 | Camera target/third-person update |
| Sim collision | 52,634 | Collision room/blocker gather |
| Sim room track | 7,127 | Current room tracking |
| Sim residency | 5,281 | Runs on background ticks |
| Sim pump | 2,429 | Runs on background ticks |

GTE profile:

| Metric | Value |
| --- | ---: |
| Total GTE ops | 545,508 |
| GTE ops / guest frame | 341 |
| Estimated GTE cycles / visual frame | 9,234 |
| GTE budget share | 0.8% |

Opcode mix:

| Opcode | Count |
| --- | ---: |
| `MVMVA` | 318,339 |
| `RTPS` | 206,235 |
| `RTPT` | 19,740 |
| `NCLIP` | 1,194 |

The GTE has a lot of headroom. CPU-side model face processing, image props, and
movement/camera are better first targets than trying to reduce GTE op count.

## Candidate Optimization Slices

### 1. Textured Model Face Loop

Primary files:

- `engine/crates/psx-engine/src/render3d.rs`
- `engine/examples/editor-playtest/src/main.rs`

Measured cost:

- `TEXTURED_MODEL_FACES`: 198,747 cycles per player draw.
- Latest frame considered 496 model faces and submitted 235 player tris.
- All measured model submits were on the packed/unclamped eligible path.

Relevant code:

- `submit_textured_model_geometry_impl`, face loop around `render3d.rs:3037`.
- `submit_predecoded_model_face_packed_back_average_unclamped_fast`.
- `submit_projected_model_triangle_preclamped_packed_average_fast`.

Likely work:

- Add a finer face-loop microprofile: index fetch, backface, hardware extent,
  depth/slot, packet fill, command push.
- If one part dominates, introduce a cooked "fast face batch" for models where
  all faces are known front/in-bounds/packed, reducing repeated branch work.
- Consider a quad packet path where authored/cooked faces preserve quad pairs,
  but only if visual parity can be proven. The current model path is triangle
  based.

### 2. Image Prop Rendering

Primary file:

- `engine/examples/editor-playtest/src/main.rs`

Measured cost:

- `IMAGE_PROPS`: 164,528 cycles per visual frame.

Relevant code:

- `draw_image_props`, especially GTE/world-quad projection and two Gouraud
  triangle submissions around `main.rs:11160`.

Likely work:

- Add image-prop counters: considered, visible, projected, submitted.
- Split timing into cull, texture lookup/upload, projection, fog/lighting, and
  submit.
- Cache or precompute per-prop material and static UV/depth-bias values once
  residency is stable.

### 3. Character Motor Solve And Collision Gather

Primary files:

- `engine/crates/psx-engine/src/character_motor.rs`
- `engine/examples/editor-playtest/src/main.rs`

Measured cost:

- `SIM_SOLVE`: 97,991 cycles per sim tick.
- `SIM_COLLISION`: 52,634 cycles per sim tick.

Relevant code:

- `CharacterMotorState::update_vblanks_with_collision`.
- `CharacterMotorState::apply_vertical`.
- `supporting_floor_height`.
- `collect_collision_rooms`, `collect_collision_blockers`,
  `collect_box_prop_collision_blockers`.

Likely work:

- Add counters for collision rooms/blockers/AABBs collected.
- Add solve micro-stages: vertical, movement intent, wall sweep, actor/AABB
  collision, step-down.
- Cache blocker lists by room where possible; current collection scans model
  instances and box props each tick.

### 4. Sky Cyclorama

Primary file:

- `engine/examples/editor-playtest/src/main.rs`

Measured cost:

- `SKY`: 48,723 cycles per visual frame.

Relevant code:

- `draw_sky_panorama`.

Likely work:

- Cache per-sky yaw and pitch trig tables instead of recomputing them each
  visual frame.
- Cache static UV/material/page rows.
- Keep GTE projection per frame because camera rotation changes, but reduce the
  surrounding CPU setup.

### 5. World Flush/Sort

Primary file:

- `engine/crates/psx-engine/src/render3d.rs`

Measured cost:

- `WORLD_FLUSH`: 47,743 cycles per visual frame with default bucketed world
  ordering.

Relevant code:

- `WorldRenderPass::flush`.
- `WorldRenderPass::new_bucketed`.

Likely work:

- Add counters for command count at flush and selected ordering mode.
- Add microprofile around just the bucketed flush loop.
- Verify whether this stage includes fixed overhead or actual command insertion
  cost. It measured ~33k cycles even in the sky-only profile with no room/model
  commands, so do not optimize blindly.

## Recommended Next Task

Start with the model face loop, because it is the largest measured single
sub-stage and already has clear fast-path eligibility. The next slice should be:

1. Add model-face micro-counters under a profiling feature.
2. Capture the same direct-gameplay window.
3. Implement one narrow fast-path change.
4. Re-capture and compare cycles, display hash, and model counters.

Do not start with handwritten assembly. The current evidence points to
algorithmic and data-layout wins first: fewer per-face branches, fewer repeated
checks, and better cooked batches.

## First Optimization Result

After fixing the menu CD-DA handoff, the menu-to-gameplay tape became a valid
streamed-room benchmark. The first optimization specialized the common model
face route where all projected vertices are in front, inside hardware bounds,
back-face culling is active, average depth is used, and UVs are already packed.

Implementation:

- Split the fastest packed/unclamped model-face path into a dedicated batch loop.
- Keep the same back-face cull and hardware extent fallback checks.
- Use a pretrimmed projected-vertex slice plus checked indices before unchecked
  loads inside the hot batch.
- Accumulate hot-path stats locally and flush them back to
  `TexturedModelRenderStats` once per batch/fallback instead of writing every
  counter for every face.

Validation:

| Metric | Before | After |
| --- | ---: | ---: |
| Display hash | `0xc5627105d93127e7` | `0xc5627105d93127e7` |
| Render cycles / visual frame | 664,668 | 655,433 |
| Player cycles / hit | 354,363 | 342,832 |
| Player draw cycles / hit | 342,448 | 330,917 |
| Model faces cycles / hit | 198,669 | 184,500 |
| Model face latest count | 496 | 496 |
| Packed unclamped latest count | 496 | 496 |

The visible hash stayed identical. VRAM hash changed, which is acceptable here
because off-screen / non-displayed VRAM contents can differ while the displayed
frame is unchanged.

Observed gain:

- `TEXTURED_MODEL_FACES`: about 14.2k cycles saved per model-face hit
  (~7.1% of that stage).
- Full render: about 9.2k cycles saved per visual frame on this route
  (~1.4% of render).

## Second Optimization Result

The next useful slice was the `IMAGE_PROPS` bucket, which also includes box
props. A single-quad packet experiment for flat image props was visually safe
on the menu-to-gameplay route, but it was slower in the measured stage
(`IMAGE_PROPS` rose from 164,720 to 165,976 cycles / hit), so it was discarded.

The real win was caching static box-prop derived data at gameplay init:

- Precompute box-prop world vertices, face vertices, face centers, face normals,
  cull sphere, floor height, debris bounds, and AABB once.
- Render uses cached faces/bounds instead of rotating local vertices and
  recomputing face normals every visual frame.
- Break checks and collision AABB gathering use cached AABBs instead of
  rebuilding them every simulation tick.

Validation:

| Metric | Before | After |
| --- | ---: | ---: |
| Render cycles / visual frame | 653,261 | 606,929 |
| Update cycles / sim tick | 254,950 | 186,984 |
| `IMAGE_PROPS` cycles / hit | 164,720 | 114,761 |
| `SIM_COLLISION` cycles / hit | 52,962 | 16,722 |
| Deadline misses | 104 | 0 |
| Cadence | missed/late | steady |

The display hash changed under the fixed CPU-step capture because the optimized
run reached steady cadence and produced more visual frames before the same step
cap. The displayed screenshots were visually equivalent; the pixel diff between
the old and optimized step-cap captures changed only 47 pixels out of 76,800.

Next candidates remain the character motor solve and sky setup.
