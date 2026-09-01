# Cortex Ignition draw-ordering sweep (2026-09-01)

Status: findings complete, fix on `fix/draw-ordering-sweep`; numbers at the end.

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
| PXBSP world (classic affine) | per patch: tri `average3` = `classic_otz3_from_sum` (ZSF3 = 0x155), quad `sum4 >> 4`, on GTE `SZ` | `SZ` = 3 x view z (`load_pxbsp_view` bakes a 3.0 scale into the rotation), so OTZ = 3 z / 4 | slot = OTZ (2048 slots, whole table) | underdraw crack-seal triangles +8 slots | rejected if OTZ == 0 or OTZ >= 2048, i.e. beyond z = 2731 |
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

E6. The BSP view transform, read back from the guest (`load_pxbsp_view`
output at the tape end): rotation rows `(12255, 0, 903)`, `(-243, -11844,
3267)`, `(870, -3276, -11814)`, translation `(-4048, 1540, -1629)`. Every row
has magnitude 12288 = 0x3000 = 3.0 in Q12. The remap matrix in
`psx-bsp/src/render.rs::load_pxbsp_view` (and the classic `load_view`) is
`[[0,0,0x3000],[0,-0x3000,0],[0x3000,0,0]]`: a proper rotation scaled by
three, inherited from the lifted XBSP renderer. Projection divides the scale
out (`SX = H * 3x / 3z`), so the world lands on the same pixels as the
models, but the GTE `SZ` the classic affine path keys on is `3 z`, and with
`ZSF3 = 0x155` its OTZ is `3 z / 4`. Cross-check with the tags recorded in
E5: the wall patches under the enemy at true depth 258..304 (my projection
of faces 803/809 at those pixels) carry slots 193..228 = 0.75 x depth; the
far wall at 1,165 carries 872..884; the enemy itself, at 779, would carry
592 under the world's law but the entity path put it at (779 - 64) / 4 =
178. Every non-world draw is keyed at one third of the world's key.

## The defect

One rule was intended (`PXBSP_CLASSIC_DEPTH_RANGE`, "a flat triangle at
view depth z lands at z / 4, so map 0..8192 onto the world band"), and it
was derived without the view scale. The consequences:

- An actor at true depth z sorts in front of every wall farther than z / 3.
  The enemy in the tape-end frame (z = 779) beats the wall at z = 300 that
  should hide it. The player (z = 241) beats anything beyond 80 units,
  which is why she never clips into the floor or a pillar.
- Archive beacons (POI markers) and vitality circles use the same range,
  so they show through walls the same way (symptom 2).
- Effects were worse still (F1): keyed at about z / 2 through the room
  draw-distance range, so they vanished behind walls that are behind them.
- The actor clearance of 64 units was effectively 16 slots against a world
  whose slot is worth 1.33 units; against the corrected law it is 48 slots,
  which is what "half a sector" meant.
- The world itself fills the table by z = 2731 and rejects surfaces beyond
  that (OTZ >= 2048). The room's cooked draw distance is 4096; nothing in
  the 0.4 level is that far, but it is a real ceiling and now documented.

## The fix

One law, defined next to the thing that creates it:

- `psx_bsp::render::XBSP_VIEW_SCALE_Q12` names the 3.0 the view remaps bake
  in (both remaps now use it instead of a literal), and
  `pxbsp_classic_far_depth(ot_depth)` = `ot_depth * 4 / scale` = 2731 is the
  true depth at which the world reaches its last slot.
- `PXBSP_CLASSIC_DEPTH_RANGE` is `0..=pxbsp_classic_far_depth(OT_DEPTH)`; the
  host test `dynamic_world_range_matches_classic_affine_triangle_otz` now
  feeds the scaled `SZ` into `classic_otz3_from_sum` and asserts the slots
  match at 0, 4, 32, 127, 256, 512, 1024, 2048, 2730 and 2731, and that past
  the far depth the world rejects while runtime draws clamp to the back of
  the band.
- `runtime_config::world_depth_range(record, uses_pxbsp)` is the single
  chooser; `pxbsp_surface_options` and a new `Playtest::effect_depth_range`
  (particles, projectile charge/bolt/impact, wade splash) go through it, so
  effects stop using the room draw-distance range in BSP mode.
- No bias was changed. The actor clearance, shadow, water, decal, beacon
  line and underdraw biases are all expressed in view units or slots and
  stay as they were; only the unit conversion under them is now right.

Not changed, worth knowing: the BSP camera quantises yaw and pitch to 256
steps per turn (`rotate_xyz(angle >> 4)`), about 1.4 degrees, while models
use the exact camera basis, so world and model pixels can disagree by a few
pixels at the screen edge (my projections of faces 803/809 were off by
10..20 px from the drawn triangles for this reason). Separate issue.

## Numbers

Cortex whole-level bench (`make cortex-bench`, features
`cd-stream-bench lockstep-visuals`, private stage root, two identical
replays each):

| run | bus_cycles | work_instructions | idle_percent | flips | vram_hash | display_hash |
|---|---|---|---|---|---|---|
| before (origin/main be93eb6b) | 7603165754 | 3320634480 | 5.78 | 2625 | 0xd46c0e234f54b3f7 | 0x91c4e0b506217302 |
| after (this branch) | 7615169907 | 3320633097 | 5.83 | 2625 | 0x38709e1d4ded36c8 | 0x4c9bafe26e2f05ff |

Bus cycles +0.158 percent, work instructions unchanged (-1,383 over 3.3
billion). The extra bus cycles are GPU-side ordering (the same packets in a
different slot order) and the ordering table walk; no guest instruction
was added on the per-triangle path (a constant changed and one range
lookup moved into a helper). The 64-bit symbol gate fails on main before
and after (pre-existing, unrelated).

Tape-end frame pair (4x, 1280x960):

- before: `/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/bench-baseline/final-1.ppm`
- after: `/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/bench-fix/final-1.ppm`
- side by side crops: `/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/pair-enemy-crop.png` (enemy now
  hidden behind the wall except the part past its edge),
  `/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/pair-ledge-crop.png` (the black patch the player's blob
  shadow painted over the ledge wall is gone),
  `/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/pair-player-crop.png` (player unchanged).

10,298 of 1,228,800 pixels differ, all in those three regions.

Host gates: `cargo test -p psx-engine -p psx-bsp` (155 + 405 + 1 pass),
`cargo test --manifest-path engine/examples/editor-playtest/Cargo.toml`
(48 pass, including the corrected depth-law test).

## Combat checkpoint

`make combat-checkpoint` cannot run on `origin/main` at be93eb6b: its first
step invokes `cargo run -p psxed-project --bin gen_brush_combat_fixture`,
and that binary was removed when the BSP authoring was consolidated
(75d7c57e). The remaining steps (clean cook of the tracked fixture, MIPS
guest with `cd-stream-bench emulator-telemetry`, disc, two canonical
replays, the door replay) were run by hand in a private stage root, on this
branch and on unmodified `origin/main`, to separate what this change moves
from what was already stale.

## POI markers (symptom 2)

Archive beacons are drawn through `pxbsp_surface_options` (no clearance,
frame lines at -2), so they were keyed at z / 4 against a 3 z / 4 world
exactly like the enemy, and the same range change corrects them. The
whole-level tape never frames a beacon next to an occluding wall at a
screenshot interval I could pin down (the cyan and green glyphs seen on
walls in the route sheet are wall texture, not beacon packets), so there is
no before/after beacon pair; the mechanism and the fix are the same as for
the enemy, and the beacon draw path itself was not touched.

| run | melee hits | staggers | deaths | hits taken | post-kill x (biased) | vram_hash | display_hash | door display_hash |
|---|---|---|---|---|---|---|---|---|
| pinned in `tools/combat_checkpoint.sh` | 4 | 1 | 1 | 3 | 1003625 | 0xd6e3486e71e17d02 | 0x30c47969ed94bd59 | (not pinned) |
| origin/main be93eb6b, by hand | 4 | 1 | 0 | 7 | 1000227 | 0xe0d9c18fac19a030 | 0x246471bb0f276375 | 0x9dc5396c7707eac3 |
| this branch, by hand | 4 | 1 | 0 | 7 | 1000227 | 0x145340d20d84356b | 0xc555866cde643a27 | 0x9b73ff2231e204f7 |

Both runs are deterministic (two identical canonical replays each). The
gameplay counters are identical between main and this branch and already
disagree with the pins on main (no death, seven hits taken, a different
post-kill position), so the pinned gameplay expectations went stale before
this change, together with the generator. This change moves only the
hashes, which an ordering fix must. The pins were not updated here: the
hash pins alone cannot be re-pinned while the gate cannot run and its
gameplay pins are wrong, and deciding what the canonical tape should
assert now is Manny's call. Logs: `/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/combat-base/` and
`/private/tmp/claude-501/-Users-ebonura-Desktop-repos-PSoXide--claude-worktrees-psoxide-demo-disc-optimization-923892/3e5f59a4-d27b-4166-8dc7-46f2a8b2741e/scratchpad/ordering/combat-fix/`.

## Open items for Manny

1. Restore the combat checkpoint: reinstate or replace
   `gen_brush_combat_fixture`, then re-pin melee/stagger/death/hits-taken
   and the hashes from a run the design agrees with (the canonical tape no
   longer kills the enemy on main).
2. The BSP camera quantises yaw and pitch to 256 steps per turn while
   models use the exact basis; worth a look if world/model seams show.
3. The world renderer rejects surfaces beyond 2731 true units (OTZ >=
   2048) although the room's cooked draw distance is 4096; fine for 0.4,
   a ceiling for larger spaces.
4. The 64-bit symbol gate fails on main (i64 helpers linked), unrelated.
