# Weapon attachment campaign: handoff

2026-08-10. Written for a worker agent continuing this work with no prior
conversation context. Read this fully before touching anything, then read
`docs/weapon-attachment-plan.md` (same directory) for per-phase detail.

## Mission context

PSoXide is a PS1 game engine + editor + emulator, four separate Cargo
workspaces (`editor/`, `emu/`, `engine/`, `sdk/`). All guest/engine math is
i32/u32 fixed-point, no float, no 64-bit. The campaign added weapon-to-bone
attachment authoring to the editor's Animation view and made equipped
weapons render on both the player and enemies. The artist (Albert) delivered
two swords (`Sword1_Light`, `Sword1_Heavy`) that now live in the
`cortex_anim` project.

All five planned phases plus a dead-code cleanup are DONE and committed.
What remains is authoring polish, a port to the game project, and the
follow-up list at the bottom.

## Repo state

- Work branch: `weapon-attachment` in worktree
  `~/Desktop/repos/PSoXide-weapon-attach` (this tree). Based on main
  `6f96d281`. NOT pushed, NOT merged to main.
- Commits, oldest first:
  - `22706efa` engine: unscaled socket basis for equipment orientation (P1 bug fix)
  - `bfbc101e` editor: weapon prop import harness and attachment plan (P1)
  - `9782d3ce` editor: attachment socket authoring mode in the animation view (P2)
  - `9c452202` editor: preview characters with their in-game material override (P3 + follow-up)
  - `77cf2000` editor: capture and display source bone names for skeletons (P4)
  - `64040715` merge of branch `dead-weapon-hitboxes` (the G6 cleanup, originally commit `8cd28838` in worktree `~/Desktop/repos/PSoXide-hitbox-cleanup`, now fully contained here; that worktree/branch can be deleted after the main merge)
  - `2b89e3b3` engine+editor: enemy equipment rides its instance's live pose (P5)
- Other live worktrees to stay away from: `PSoXide-quake-bsp` (the BSP world
  overhaul; it does not touch any file this campaign touched, and its docs
  promise the skeletal pipeline stays untouched) and the main checkout
  `~/Desktop/repos/PSoXide` (user's own WIP on another branch; read-only for
  us EXCEPT the untracked `editor/projects/` data, see below).
- Merge-to-main is the user's call (repo convention: branch off main,
  FF-merge, push; owner bypass works on the protected ref).

## What shipped, with anchors

**Engine fix (latent bug, P1).** Equipped weapon models had NEVER rendered:
the socket composer used `compute_joint_world_transform`'s scaled pose
matrix as the weapon orientation, so the weapon inherited the host's
model-to-world scale (~0.01) on top of its own and collapsed to sub-pixel
(every triangle zero-area, binned as backface-culled). Fix:
`psx_engine::compute_joint_world_basis` (unscaled orthonormal basis,
`engine/crates/psx-engine/src/render3d.rs`) used for socket/grip/weapon
orientation; scaled matrix kept for OFFSETS (socket translations and combat
capsules are authored in model-local units).

**P1 import harness.** `editor/crates/psxed-project/examples/import_weapon_props.rs`
imports rigid weapon GLBs end to end into any project: static cook (1 joint
+ generated `bind_pose` clip, which it writes and registers because the
model importer discards package clips and the cook demands >= 1 clip per
model), Weapon resources, provisional `right_hand_grip` sockets on
character-referenced models, Equipment nodes on scene character entities.
Rerun-safe, backs up project.ron first.

**P2 socket authoring.** Third mutually-exclusive Animation-view mode
"Sockets" (`editor/crates/psxed-ui/src/model_animation_viewer.rs`:
`draw_attachment_socket_editor`, `attach_selected_socket_to_joint`,
`manipulate_selected_socket`). Click a highlighted joint to re-anchor the
selected socket (offset resets, rotation survives); left-drag Move/Rotate on
a bone-local axis; panel embeds the numeric `attachment_socket_list_editor`.
Preview draws RGB axis triads composed EXACTLY like the runtime
(`compute_joint_world_basis` x socket Euler), white origin cross on the
selection (`model_import_preview.rs`: `PreviewSocket`, `draw_socket_marker`).

**P3 weapon preview + material override.** "Preview weapon" picker in the
socket panel; the preview rasterises the weapon model + atlas into the
character's z-buffer at the runtime equipment composition verbatim
(`PreviewEquippedWeapon`, `draw_equipped_weapon_overlay`). Characters now
preview with their IN-GAME material override (Character material first,
else scene ModelRenderer's), resolved through the cook's own
`psxed_project::resolve_material_texture_psxt` (all modes incl. Generated),
tiled sampling, UV scroll advanced on the preview clock via the runtime's
`LevelMaterialUvMotion::offset_at_tick` (`PreviewMaterialLayer`). Aletha
previews as her crystal hologram instead of orange placeholder.

**P4 joint names.** `RigidModelPackage.joint_names` (post-collapse,
cooked-joint order, from glTF/FBX node names; static path = `["root"]`),
backfilled onto `SkeletonResource.joint_names` at import (empty-only, so a
signature-shared skeleton is named once). Pickers show "13 · RightHand"
(namespace prefixes like `mixamorig:` stripped for display via
`inspector_character_ui::joint_label`). `examples/backfill_joint_names.rs`
re-cooks Model sources for names only. KEY DISCOVERY: on the shared 22-bone
rig, joint 9 = `mixamorig:LeftHand`, joint 13 = `mixamorig:RightHand`. The
P1 provisional sockets were on 9 (wrong hand); project sockets now on 13.

**G6 cleanup (merged branch).** Deleted the dead render-path weapon-hitbox
evaluator (counter-only, XZ-only math). KEPT `WeaponHitboxRecord` + cook:
`combat::player_melee_spec` unions its active frame windows into the live
melee hit window; only the SHAPE geometry is authoring-only (documented in
psx-level + combat.rs). Telemetry counter ids 18/19 retired, not reused.
Also removed the per-room filter on PLAYER equipment visuals (melee is
deliberately room-agnostic; the sword used to vanish outside its spawn room
while still dealing damage).

**P5 enemy equipment.** `EquipmentRecord.model_instance` (additive cooked
field, sentinel `EquipmentRecord::NO_INSTANCE` for the player) binds each
non-player equipment record to its host entity's model instance (cook fills
it in `psxed-project/src/playtest.rs` using the same last-pushed-instance
convention as game entities). `instance_pose_context`
(`engine/crates/psx-game-runtime/src/model_rendering/instances.rs`) is the
extracted single source of truth for an instance's live pose;
`draw_instance_equipment` (`model_rendering/equipment.rs`) is a room-gated
per-room pass composing bound weapons from that context (no crossfade,
matching the instance body), sharing `submit_equipped_weapon` with the
player pass. Example wiring: per-room call in
`engine/examples/editor-playtest/src/playtest_scene.rs` after the instance
depth passes, staged under the EQUIPMENT telemetry band (stats currently
discarded).

## Project data state (IMPORTANT: lives in the MAIN checkout, untracked)

`~/Desktop/repos/PSoXide/editor/projects/cortex_anim/` (260MB, gitignored;
only `editor/projects/default/` is tracked). Changes made:

- Swords imported: Models + shared 1-joint skeleton + shared
  `prop_bind_pose` clip + Weapon resources "Sword1 Light" / "Sword1 Heavy";
  sources in `source_assets/props/`; heavy shares light's atlas (byte-equal),
  which does NOT byte-match the Rust Mantis atlas (separate VRAM page).
- Sockets `right_hand_grip` on Models "Aletha-uthana" and "Rust Mantis":
  joint 13, translation (0, -6000, 0), rotation_q12 (0, 1024, 0). These
  offsets were blind-tuned for the OLD joint-9 (LeftHand) basis and need
  interactive re-tuning in the P2 editor (followup 1).
- Equipment nodes: player entity carries Sword1 Light, "Mantis Enemy"
  entity carries Sword1 Heavy.
- Skeletons named (backfill run): 22-bone shared rig from idle.glb, Aletha
  Delivered (26), sword root. The Rust Mantis / CI Player FBX sources are
  MISSING on disk (`~/Desktop/Bonnie Studios/...` moved), but they share
  the named 22-bone skeleton, so nothing is lost.
- Sandbox scene fixes: "Mantis Enemy" node pitch was `rotation_degrees
  (-90, 4, 0)`, cooking the enemy LYING INSIDE THE FLOOR (invisible in
  every dump ever taken); now (0, 4, 0). Its spawn moved from the far
  corner to in front of the player spawn (out of aggro range on the
  camera-visible side) so it's actually watchable while authoring.
- Backups: `logs/project.ron.pre-weapons.bak` (pre-campaign) and
  `logs/project.ron.pre-jointnames.bak`.

## Gotchas a worker agent must know

1. **Recook does NOT reach the emulator without a guest rebuild.** The
   cooked manifest is `include!`-ed into the guest exe. The loop is ALWAYS:
   cook-playtest, `make build-editor-playtest`, mkisopsx, THEN run. Skipping
   the make ships stale data silently (cost several iterations this
   session).
2. The real cooked manifest is
   `engine/examples/editor-playtest/generated/level_manifest.cooked.rs`;
   `level_manifest.rs` next to it is the committed empty stub. Grep the
   cooked one.
3. Enemy AI flanking (`circle_chance`) deliberately circles to the player's
   camera blind side; an aggroed enemy will always end up behind the
   camera in headless dumps. Place test enemies OUT of aggro on the
   visible side (camera looks toward -Z from behind the spawn player).
4. The clipless-weapon rule: a weapon model renders only if it resolves a
   clip; static props cook a 1-frame identity `bind_pose` and the harness
   registers it. Never delete that clip resource.
5. Sockets/capsule offsets are MODEL-LOCAL units (scaled joint matrix
   convention); weapon orientation is the UNSCALED basis. Do not "fix" one
   by changing the other.
6. Preview vs Play scale nuance: the animation view previews at
   `ModelResource.scale_q8` while Play applies the Character's
   `visual_scale_q8` (360/256 for Aletha), so socket offsets reach ~1.4x
   further in-game (pre-existing convention shared with the capsule editor).
7. `grep -c` exits nonzero on zero matches; do not chain `&&` after it in
   verification one-liners. Run sips/python converters with absolute paths
   (cwd drifts between chained cds).
8. Multi-workspace: build/tests run per workspace dir (`editor/`, `engine/`,
   `emu/`). The frontend (emu workspace) embeds psxed-ui; rebuild it after
   editor changes to confirm.
9. The user launches the editor GUI themselves; never auto-launch it.
   Verify with headless dumps and the test-embedded DUMP hooks.
10. BSP coordination: additive-only changes to psx-level records; no new
    `ViewTool` variants or `EditorWorkspace` fields; sync with the BSP
    effort before its P3 entity migration rewrites `LevelGameEntityRecord`.
11. No em dashes in committed text. No AI attribution anywhere.

## Verification recipes

Editor tests: `cd editor && cargo test -p psxed-ui -p psxed-project -p psxed-gltf`
(306 + 390 + 25 green at handoff). Engine: `cd engine && cargo test --release
-p psx-game-runtime` (68 green). Frontend build: `cd emu && cargo build
--release -p frontend`.

Headless gameplay gate (from this worktree; project path stays in the main
checkout):

    cd editor && cargo run --release -p psxed-project --bin cook-playtest -- \
      /Users/ebonura/Desktop/repos/PSoXide/editor/projects/cortex_anim/project.ron
    cd .. && make build-editor-playtest
    cd tools/mkisopsx && cargo run --release -- \
      --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
      --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
      --volume PSOXIDE --cdtest-sectors 32 \
      --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
      --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
      --ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
      --ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
      --cdda-track-list ../../engine/examples/editor-playtest/generated/cdda_tracks.txt
    cd ../../emu && cargo run -p frontend --release -- launch \
      --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
      --embedded-playtest \
      --pad-pulses "0x4000@120+20,0x4000@300+20,0x4000@600+20" \
      --steps 1450000000 --dump-hw /tmp/gate.ppm

(The pad pulses press CROSS through the lore splash into gameplay; the
dump at that step count shows the player at spawn with the light sword and
the patrolling Mantis with the heavy sword ahead of her.)

Preview inspection dumps (no emulator needed; psxed-ui tests): env-driven
hooks on `socket_markers_draw_over_the_wraith_preview` and
`equipped_weapon_overlay_rides_the_socket` in
`editor/crates/psxed-ui/src/model_import_preview.rs`: `DUMP_MODEL`,
`DUMP_CLIP`, `DUMP_WEAPON_MODEL`, `DUMP_WEAPON_ATLAS`, `DUMP_OVERRIDE_ATLAS`
(character material psxt; produce one with
`examples/dump_material_psxt.rs`), `DUMP_SOCKET_JOINT`, `DUMP_SOCKET_T`,
`DUMP_SOCKET_R`, `DUMP_YAW`, `DUMP_SOCKET_PREVIEW` / `DUMP_WEAPON_PREVIEW`
(output ppm paths).

## Follow-ups, prioritized

1. **Grip re-tune on joint 13 (authoring, user-driven).** The current
   socket offsets were tuned for the wrong hand. Open the editor from this
   worktree, Animation tab, select Aletha-uthana, Sockets mode, pick a
   weapon in "Preview weapon", scrub light/heavy attack clips, drag the
   socket until the grip sits in the hand. Same for Rust Mantis. This is
   interactive by design; an agent can only pre-position numerically.
2. **Port to the game project.** Decision on record: cortex_anim first,
   then the game project (cortex_ignition_v1, directory
   `editor/projects/cortex_v1`). Run `import_weapon_props` against
   cortex_v1's project.ron, copy the TUNED socket values onto its character
   models (sockets do not propagate between projects), hang Equipment nodes
   on its entities, run `backfill_joint_names` there too, and check its
   Mantis scene nodes for the same -90 pitch artifact found in cortex_anim.
3. **Author swing hitboxes (existing tools, no engineering).** Combat
   capsules on character joints via the animation view's Combat Volumes
   mode with timeline active windows; the live damage path
   (`resolve_player_combat_capsules`) already consumes them. The Mantis has
   `combat_capsule_count: 0` today, so it also needs hurtboxes for real
   fights.
4. **Spawn-tick saturated weapon origin (small bug).** One spawn-adjacent
   tick produced weapon origin X = i32::MIN (self-corrects next frame; at
   worst a one-frame flicker). Suspect the spawn-transition crossfade
   (`ModelPoseBlend`) feeding `attachment_socket_pose`. Reproduce by
   probing `EquipmentDrawStats` around the menu-to-gameplay transition;
   fix likely a clamp or a blend-window guard in
   `equipment.rs::attachment_socket_pose`.
5. **Preview scale parity (small).** Pull the wielding Character's
   `visual_scale_q8` into the animation-view preview context (affects the
   model raster, socket triads, weapon overlay, and `fit_capsule_to_joint`)
   so offsets read identically to Play. Shared with the capsule editor,
   so do it once at the `LoadedModelContext` level.
6. **Model inspector socket names (small).** `inspector_assets.rs` passes
   `None` for joint names into `attachment_socket_list_editor` (borrow
   ordering: the ModelResource is mutably borrowed before the skeleton can
   be read). Resolve the skeleton's name vec BEFORE the mutable borrow and
   pass it through, so the Model inspector matches the animation view.
7. **Weapon double_sided + instance-equipment telemetry (tiny).** The
   equipment submit hardcodes `CullMode::Back`, ignoring the weapon
   model's double_sided flag; honor it. `draw_instance_equipment` stats
   are discarded at the call site; fold them into the equipment counters
   if profiling ever needs them.
8. **Atlas unification (optional VRAM win).** The sword psxt does not
   byte-match the Rust Mantis atlas despite the artist authoring against
   the same 256px image (different cook pipelines: PNG-in-GLB vs
   FBX-embedded). Re-exporting the Mantis with the same texture, or
   pointing the sword models' `texture_path` at the mantis psxt AFTER a
   visual A/B, saves a VRAM page.
9. **Perf check on the new pass (due diligence).** The per-room instance
   equipment pass costs one socket compose + a 61-94 tri submit per armed
   enemy per room. Run the per-vblank chart recipe from
   `docs/playtest-profiling.md` on cortex_v1 after the port (followup 2)
   and eyeball the equipment band; on-console verification rides the next
   scheduled burn, no dedicated burn needed.
10. **Merge to main.** User's call. After merging `weapon-attachment`,
    delete the `dead-weapon-hitboxes` branch + `PSoXide-hitbox-cleanup`
    worktree (its content is fully contained via the merge commit).
    Expect zero conflicts with the BSP branch today.
