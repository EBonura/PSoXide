# Cortex Ignition draw-ordering sweep (2026-09-01)

Status: IN PROGRESS. Written incrementally so partial findings survive.

Branch `fix/draw-ordering-sweep` from `origin/main` at `be93eb6b`, in worktree
`.claude/worktrees/psoxide-draw-ordering` (the assigned `psoxide-perf-idle-probes`
worktree was taken over by another session mid-task).

## Scope

One ordering table (`OtFrame<OT_DEPTH>`, `OT_DEPTH = 2048` with the `ot-2048`
feature, `engine/crates/psx-engine/src/render.rs`) receives:

- the PXBSP brush world (`engine/crates/psx-bsp/src/render.rs` ->
  `engine/crates/psx-engine/src/classic_affine.rs`, profile
  `ClassicAffineProfile::PXBSP_THIRD_PERSON`, `ot_depth 2048`);
- the sky (`engine/crates/psx-game-runtime/src/sky.rs`);
- every runtime model (player, enemies, model instances, box/cylinder/image
  props, archive beacons, water, shadows) through `WorldRenderPass`
  (`engine/crates/psx-engine/src/render3d/world_pass_model.rs` and friends);
- particles, projectile bolts/charges/impacts and the water wade splash
  through `psx_engine::DepthRange::slot` directly
  (`engine/crates/psx-game-runtime/src/particles.rs`).

Damage numbers and the HUD are drawn after the OT is submitted (overlay pass in
`playtest_scene.rs::submit_render`), so they are not part of the depth sort and
are out of scope (Manny is working on the UI).

## Ordering rule per path, as it exists today

Slot 0 is nearest (drawn last), slot 2047 farthest (drawn first).

| Path | Depth key | Units | Slot mapping | Bias | Clamp |
|---|---|---|---|---|---|
| PXBSP world (classic affine) | per patch: tri `average3` = `classic_otz3_from_sum` (ZSF3 = 0x155), quad `sum4 >> 4` | view z / 4 | slot = OTZ (2048 slots, whole table) | underdraw crack-seal triangles +8 slots | rejected if OTZ == 0 or OTZ >= 2048 |
| Sky panorama | fixed | none | slot 2047, inserted after PXBSP so same-slot prepend executes it first | none | none |
| Room-owned model instances / props (`pxbsp_surface_options`) | per tri `(sz0+sz1+sz2)/3` | view z | `WORLD_BAND (0..2046)` over `PXBSP_CLASSIC_DEPTH_RANGE (0, 8192)`: slot = z*2046/8192 | 0 | clamp to band |
| Actors: player, enemies, model instances drawn with `pxbsp_actor_surface_options` | same | view z | same | `-sector_size/2` (actor clearance) | clamp to band |
| Archive beacons (POI markers, `marker_runtime.rs`) | per tri avg | view z | options passed by caller (TBD) | body 0, frame lines -2 | clamp |
| Actor blob shadow | per tri avg | view z | actor options | actor clearance + `SHADOW_DEPTH_BIAS` (TBD) | clamp |
| Water | per tri avg | view z | actor options | actor bias - 64 | clamp |
| Particles, projectile bolt/charge/impact, wade splash (`playtest_runtime.rs`) | sprite centre `sz` (bolt: min(head, tail)) | view z | `DepthRange::slot` over `room_depth_range(record) = (NEAR_Z, room_draw_distance)` over the WHOLE table, even when `self.bsp.is_some()` | 0 | clamp |

`runtime_config.rs` documents the intended rule: "Models, beacons and other
runtime geometry must use [`PXBSP_CLASSIC_DEPTH_RANGE`] when the resident BSP
owns the static world or a nearer BSP wall can otherwise sort behind them."
The mapping is verified exact against the classic triangle OTZ for equal-depth
vertices by `pxbsp_depth_order_tests`.

## Findings so far

Units check. Both paths key on GTE `SZ` (view-space z in cooked world units):
the PXBSP path translates the Q12 camera origin back with `>> 12`
(`bsp_runtime.rs::pxbsp_camera`, `psx-bsp/src/render.rs:324`), the model path
reads `ProjectedLit.sz`. The cooked Cortex 0.4 room record is
`sector_size: 128, draw_distance: 4096`, camera `distance: 231, height: 134`
(the cook scales project units by 1/8), so numbers below are in those units.

F1. Effects use the wrong depth range in BSP mode. `draw_particle_emitters`,
`draw_combat_projectiles` (charge, bolt, impact) and
`draw_player_water_wade_splash` in `playtest_runtime.rs` map through
`room_depth_range(record)` = `(4, 4096)` over the whole 2048-slot table, so
slot = (z - 4) * 2047 / 4092, about z/2, while every wall is keyed at z/4. An
effect therefore sorts at twice its real depth: a bolt at z = 300 lands in
slot 148, where the world puts z = 592, so any wall between 300 and 592 units
away paints over it. Projectiles and particles vanish behind walls that are
behind them. The doc comment on `PXBSP_CLASSIC_DEPTH_RANGE` already states the
rule these paths break.

F2. Actor clearance bias. Actors (player, enemies, equipment, blob shadows,
water, vitality circles, box/cylinder/arch/image props, debris, destructible
fragments: everything drawn with `actor_options`) are pulled `sector_size/2`
= 64 view units = 16 OT slots toward the camera. In the grid world that was
"half a tile" so a character never lost to the tile under her feet. In PXBSP
the near field is subdivided by the classic lattice (twice below OTZ 136,
z < 544; once below OTZ 272, z < 1088) so the patch under an actor's feet is
a quarter of a root triangle, and 64 units is in the right order for faces up
to 512 units. The cost is that an actor within 64 units behind a wall patch's
average depth paints over that wall.

Not a defect: the green "5" glyphs on the wall in the tape-end frame are wall
texture content (the pixels belong to world triangles `34` in the draw list,
no separate primitive covers them), not POI markers. No archive beacon is
drawn in that frame (no gouraud `30..33` packets), so the POI symptom needs a
different tape position.


## Experiments

E1. Actor clearance 0 (temporary edit of `pxbsp_actor_surface_options`),
same disc features, tape-end frame: the enemy at the upper left still paints
over the left wall. Frame diff against the baseline is 8,773 of 1.2M pixels
(4x), concentrated at the right-hand ledge next to the player and a few
hundred pixels of the enemy. The clearance is not what puts that enemy in
front of the wall.

E2. Walking the ordering table out of a `--dump-ram` image at the tape end is
not usable: the run stops mid-build and the OT chains are inconsistent
(400k packets walked, 132 decodable draws).

E3. A guest built with `emulator-telemetry` for the debug-log channel changes
tape pacing (361 pad polls instead of 5,260; the game never leaves the front
end), so positions are read back through a plain RAM ring buffer instead
(`DIAG_ORDERING_BUF`, temporary, dumped with `--dump-ram` and located through
the link map).

E4. Tape-end frame positions (RAM ring, cooked units): camera (1394, 350,
-559), yaw sin -370/4096, pitch sin -1395/4096 (about 20 degrees down);
player (1413, 224, -769) at view z 241. The enemy at the upper left is model
instance 0 at (1161, 224, -1363), view (x -306, y 147, z 779): with
FOCAL 320 it projects to screen (35, 60), a ground enemy standing 538 units
beyond the player and to the left. Its OT slot is about (779 - 64) / 4 = 178.

E5. Draw-list forensics for that frame (`--dump-draws`, last frame after the
final full-screen fill): the packets under the enemy are the sky (`2C`,
slot 2047), far wall triangles (`34`, tags 872..878, z about 3,500), then the
tall left wall faces `[(32,-6),(45,122),(39,61)]`, `[(3,164),(32,-6),(19,67)]`
(world `34`), then the enemy's `24` triangles (tpage 0x1C) interleaved with
two more wall faces `[(3,164),(-10,76),(19,67),(10,-14)]` and
`[(-26,-40),(8,203),(-7,94)]`. So the wall faces on the enemy's right sort
farther than the enemy and the ones on its left sort nearer: the enemy and
that wall are at about the same depth, and the outcome is decided by the
per-face average keys, not by a scale or bias mismatch. The cooked level has
no box, arch, cylinder or image props and no destructibles, so every `34`
or `3C` packet is PXBSP world.
