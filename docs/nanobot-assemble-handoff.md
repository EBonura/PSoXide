# Nanobot assemble effect: handoff (branch fx-nanobot-assemble)

Parked 2026-08-02. Slice 1 (spawn-in) is CLOSE: everything works except
the per-face vertex transform, which puts every submitted face
somewhere invisible. One numeric bisection stands between this and the
first real screenshot sheet.

## What this is

Aletha is made of nanobots (game lore). Spawn/death/dodge disassemble
her into the model's own triangles. Slice 1 = spawn-in: triangles rain
down, tumbling, additive-ghost ramping to opaque as each face seats
onto the LIVE idle pose (FF7-style materialize, combo of screen-door
stagger + additive ramp -- Manny approved the combo, wants plenty of
screenshots to judge).

## Where everything lives

- `psx-game-runtime/src/model_rendering.rs::draw_player_assemble`:
  the whole effect. Per-face deterministic hash -> stagger (ft),
  fall offset, tumble, tint ramp (additive 128..255 while ft<3072,
  over-bright seat flash <3584, then opaque). Returns submitted count.
- `editor-playtest`: `PLAYER_ASSEMBLE_TICKS = 900` (15s observation
  speed; make it an authored setting later), `assemble_active` +
  `assemble_start_tick` stamped on FIRST world render
  (playtest_scene, before the draw_player call site which switches to
  the effect), progress helper in playtest_runtime.
- `debug_log_assemble_frame(progress, faces)` logs "asm p=N faces=M"
  every effect frame under --guest-debug-log.

## Ground truth established (do not re-derive)

1. `submit_textured_world_triangle` from this call site WORKS: a
   hardcoded triangle at `origin` rendered perfectly (position,
   texture, materials). The path/OT/material plumbing is fine.
2. The effect RUNS and submits: instrumented run shows the window is
   guest frames ~683..1582 (starts at PLAY press, i.e. WORLD RENDER
   BEGINS DURING/BEFORE THE LOADING OVERLAY -- earlier "empty room"
   sheets at f1085+ were in-window with 300+ faces submitted).
   Face count grows 8 -> 506 with the stagger, progress 0 -> 4096.
3. So: faces submitted, path proven, NOTHING visible => the computed
   world-space verts are wrong (off-screen/degenerate/microscopic).
4. Units ground truth: `compute_joint_world_transform` bakes
   local_to_world into rotation (scaled_pose_matrix) AND scales pose
   translation; combat capsules (`transform_combat_capsule`) feed RAW
   model-local i16 points through jwt.rotation rows (>>12) + jwt
   translation and land on the visible pose. Current code mirrors
   that (raw verts + rotate_offset_q12) and still shows nothing.

## Next probe (start here)

Log ONE face's numbers: verts[0] world coords + jwt[joint].translation
+ origin, compare against a capsule joint from
`player_joint_world_transform` at the same tick. Bisect:
- translation right but rotation wrong -> face collapses (degenerate,
  likely rotate_offset_q12 vs capsule row convention mismatch:
  ROW-vs-COLUMN major! capsule uses rotation ROWS as [row]·local;
  check whether rotate_offset_q12 multiplies the same orientation).
- translation wrong -> off-screen (check pose_translation space).
Strong suspect: Mat3I16 row/column convention differs between
rotate_offset_q12 (equipment socket math) and transform_joint_local
_point (capsules); one of them transposes.
Also verify `joint_of_vertex` (part first_vertex/vertex_count ranges
map vertex index -> part joint).

## Repro loop (all headless)

```sh
# branch has cortex_v1 cooked; guest rebuild + disc:
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry"
cd tools/mkisopsx && cargo run --release -- --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin --volume PSOXIDE --cdtest-sectors 32 \
  --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
  --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
  --ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
  --ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
  --cdda-track-list ../../engine/examples/editor-playtest/generated/cdda_tracks.txt
# instrumented run (asm p=/faces= lines) + exact-frame dumps:
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest --press '400:cross:8,520:cross:8,700:cross:8' \
  --guest-frames 1100 --steps 6000000000 --guest-debug-log   # add --dump-hw x.ppm
```
Dump INSIDE f700..1560 (true window). Beware zsh noclobber (`>` onto
existing files fails silently mid-chain) and grep -c exit codes
breaking && chains; cd to absolute dirs (cwd persists between calls).

## After it renders

Tune for readability (shards were invisible additive-dim before:
already over-brightened; consider 2-3x airborne shard scale for
distance readability), Manny judges sheets, THEN: death = time
reversal, dodge = short window during dash i-frames, duration becomes
an authored character/project setting.

## Deferred (other campaign, do not mix)

cortex_v3 visibility campaign: docs/cortex-v3-visuals-30fps.md
(corridor cell-emission bug next); Aletha moveset remainder in memory
notes; demo-disc burn waits for the CD spindle.
