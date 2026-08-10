# Weapon attachment and hitbox authoring plan

2026-08-10. Branch `weapon-attachment`, worktree `~/Desktop/repos/PSoXide-weapon-attach`, base `main` @ `6f96d281`. Assets: `Props.zip` from Albert (Sword1_Light, Sword1_Heavy).

## The short version

The backend for weapon-to-bone attachment already exists end to end on main: socket records on models, weapon resources with grip transforms, the Equipment scene component, cook emission, and a runtime composer that places a weapon model on an animated joint every frame. Rig-attached combat capsules (hit/hurt volumes with per-clip active frame windows) also exist, including a capsule editor inside the animation view.

What is missing is the authoring surface: the animation view cannot create or place sockets visually (today they are numeric DragValues buried in the Model inspector), it cannot show a weapon riding the animation, and joints have no names anywhere, so every picker says "Joint 14". demo_03's three authored sockets all say `joint: 0` with a guessed offset, which is exactly what authoring without visuals produces.

So this is a UI/preview gap-fill plus two small import/runtime gaps, not a new system.

## What exists (verified at 6f96d281)

| Piece | Where |
| --- | --- |
| `AttachmentSocket { name, joint, translation, rotation_q12 }` on `ModelResource.attachments` | `editor/crates/psxed-project/src/resource_types.rs:948`, `:1274` |
| `WeaponResource` (grip, hitboxes, arc, damage), `NodeKind::Equipment` | `resource_types.rs:1149`, `scene_types.rs:276` |
| Numeric socket editor, weapon editor, attachment lab | `psxed-ui/src/inspector_character_ui.rs:2418`, `:2515`, `:2627` |
| Combat-capsule editor in the animation view (click-to-attach joint, Move/Rotate/Resize, timeline ranges) | `psxed-ui/src/model_animation_viewer.rs:1847`, `:2187`, `:2218` |
| Cook: sockets validated and emitted; weapon model auto-promoted as a second model; equipment records | `psxed-project/src/playtest/cook_entities.rs:1139`, `:1981` (model promotion at `:2050`) |
| Runtime composer: socket pose sampled with the same crossfade as the body, grip inverse composition | `engine/crates/psx-game-runtime/src/model_rendering/equipment.rs:206`, `:233`, `:123` |
| Runtime hitboxes: `CombatCapsuleRecord` (HITBOX/HURTBOX flags, frame windows), capsule narrow phase, damage | `engine/crates/psx-level/src/lib.rs:2867`, `psx-game-runtime/src/combat.rs`, `editor-playtest/src/game_logic_runtime.rs:279` |
| Preview infra: CPU rasterizer computes per-joint world transforms with the same `psx_engine` function the runtime calls | `psxed-ui/src/model_import_preview.rs:171`, `:249`; `psx-engine/src/render3d.rs:2251` |

Cooked joint poses are absolute model-space (no runtime parent walk), so any joint is O(1) sampleable; `compute_joint_world_transform` is documented as the socket/grip/hit-volume composition point.

## Gaps

- **G1** No socket authoring in the animation view.
- **G2** No equipped-weapon preview on the animated model.
- **G3** Joints are nameless. Source bone names exist transiently at import (`psxed-gltf`) and are dropped; `SkeletonResource`'s doc comment already anticipates storing them (`resource_types.rs:71`).
- **G4** Clipless weapons render nothing: the equipment draw requires a clip (`equipment.rs:137`). The swords have no animation, so import must produce a 1-frame rest `.psxanim` (or the importer gets a small fallback).
- **G5** Enemies cannot show equipment: `draw_player_equipment` is gated on `equipment_flags::PLAYER` (`equipment.rs:94`). Only needed if a sword goes to the enemy.
- **G6** Flagged, out of scope here: `evaluate_weapon_hitboxes` (`equipment.rs:250`) is a dead parallel hit system (returns counters only, XZ-only math, Box shape degenerates to a circle); the live damage path is `CombatCapsuleRecord`. Separately, the weapon visual is room-filtered while its damage is room-agnostic (`equipment.rs:93` vs `combat.rs:288`). Both pre-date this work; track as their own cleanups.

## Phases

**P1. Import the swords** (small) -- DONE 2026-08-10
Shipped as `psxed-project/examples/import_weapon_props.rs` (rerun-safe, backs up project.ron to `logs/project.ron.pre-weapons.bak`): copies the GLBs into `source_assets/props/`, cooks both through the static path (1 joint, auto-generated `bind_pose` clip closes G4; the harness writes and registers the clip since `import_model_with_animation_sources` discards package clips), byte-dedupes the shared atlas between the two swords, creates the two WeaponResources, adds a provisional `right_hand_grip` socket to every character-referenced model, and hangs Equipment nodes on the scene's player and enemy entities. Gate met: sword renders in gameplay attached to the hand socket (headless dump verified).

P1 findings:
- **Engine bug found and fixed**: the socket composer reused `compute_joint_world_transform`'s scaled pose matrix as the weapon orientation, so an attached model inherited the host's local-to-world scale on top of its own and collapsed to sub-pixel size; every face then culled as zero-area and the weapon was invisible. New `compute_joint_world_basis` (unscaled, orthonormal) now supplies socket orientation; origins keep the scaled-offset convention (matches combat capsules). This path had never rendered anything: every pre-existing Weapon resource has `model: None`.
- Hand-tuned baseline socket on the 22-bone rig: joint 9, translation `(0, -6000, 0)`, rotation_q12 `(0, 1024, 0)`. Blade visible at the hand; blind Euler tuning through rebuild cycles is exactly the pain P2 removes.
- Sword atlases cook byte-identical to each other (shared), but NOT to the Rust Mantis atlas (different source pipeline), so one extra VRAM page for now.
- Open (cosmetic, investigate during P2/P5): one spawn-adjacent tick produced a saturated weapon origin X (`i32::MIN`), suspect the spawn-transition crossfade feeding the socket sampler; self-corrects next frame.
- Observed while testing: the equipment draw hardcodes `CullMode::Back` and ignores the model's double_sided flag; harmless for the swords, worth aligning later.
- Host-side socket math is exercised by `psxed-project/examples/debug_socket_math.rs`, a useful reference for the P3 preview overlay (same `psx_engine` calls).

**P2. Socket mode in the animation view** (the core ask, medium) -- DONE 2026-08-10
Shipped as planned: a third mutually-exclusive mode ("Sockets", MAP_PIN icon, enabled when a Model is selected) beside Combat Volumes and Pose Keys. The side panel combines a socket picker, Move/Rotate viewport tools with the shared local-axis selector (Resize falls back to Move), and the pre-existing numeric `attachment_socket_list_editor` for add/remove/fine-tuning. Clicking a highlighted joint re-anchors the selected socket there (offset resets, rotation survives); left-drag moves along or rotates around the selected bone-local axis in the same gesture language as the capsule editor. The preview draws each socket as an RGB axis triad whose orientation composes exactly like the runtime weapon path (`compute_joint_world_basis` x socket Euler, scaled-matrix offsets), so the triad IS the frame a weapon inherits; the selected socket gets full-brightness axes plus a white origin cross. Covered by three unit tests (attach re-anchor semantics, move, rotate-with-wrap) and a marker render test that doubles as a visual-inspection dump (`DUMP_SOCKET_PREVIEW` env, same convention as the existing preview dump).

**P3. Equipped-weapon preview** (medium) -- DONE 2026-08-10
The socket panel gained a "Preview weapon" picker (any Weapon resource; the overlay follows the SELECTED socket, the same pairing the cook resolves by name). The preview rasterises the weapon model with its own atlas into the character's z-buffer, so occlusion matches Play, placed with the runtime composition verbatim: unscaled joint basis x socket Euler, then grip inverse, then the scaled grip offset backed out of the socket origin. The weapon renders at its bind pose (static props cook a 1-frame identity clip, the frame the runtime samples). The weapon model/atlas decode gets its own cache slot so switching never thrashes the character decode. Verified by a render test (overlay draws, overlay tracks socket offsets) that doubles as a visual dump (`DUMP_WEAPON_MODEL`/`DUMP_WEAPON_ATLAS`/`DUMP_SOCKET_*`/`DUMP_WEAPON_PREVIEW` envs); the Aletha + Sword1 Light dump shows the textured sword riding the tuned socket and z-clipping behind her body.
Known nuance (pre-existing viewer convention, shared with the capsule editor): the animation view previews at the MODEL's scale_q8, while Play applies the wielding Character's visual scale (Aletha: 360/256), so socket offsets reach ~1.4x further in-game than the preview shows for her. If it bites during authoring, the fix is pulling the selected Character's visual scale into the preview context.

P3 follow-up (same day, commit 9adf33c7): the preview now renders characters with their in-game MATERIAL OVERRIDE (Character's material first, else the scene ModelRenderer's), resolved through the cook's own resolver (all modes including Generated) and sampled tiled with the material's UV scroll advanced on the preview clock via the runtime's `offset_at_tick`. Aletha previews as her crystal hologram instead of her bare import atlas, and dark weapons read against her. Weapons keep their own atlas. `dump_material_psxt` example resolves any material to a `.psxt`; dump hooks gained `DUMP_OVERRIDE_ATLAS` and `DUMP_YAW`.

**P4. Joint names** (small, big UX payoff)
Plumb `node_names` (`psxed-gltf/src/lib.rs:836`, `fbx.rs:70`) into `SkeletonResource` as optional names; show "14 RightHand" in the socket panel, capsule editor, and pose-key picker. Names populate on the next import; existing skeletons keep indices; the parent-table signature stays untouched. Note the default `collapse_bone_patterns` strips finger bones, so grips hang off the hand/wrist joint with an offset; document that in the panel.

**P5. Enemy equipment rendering** (small to medium)
Close G5: extend the equipment draw beyond the `PLAYER` flag to model instances, mirroring `draw_player_equipment` on top of `model_instance_joint_world_transform` (`model_rendering.rs:1397`); the cook already emits per-entity equipment records. Gate: Rust Mantis holds a sword in Play. Swing hitboxes need no new code path: author them on character joints with the existing capsule editor and timeline windows (decision below), so the only hitbox work is authoring, not engineering.

## Parallel work, coordination rules

- **quake-bsp-world** (worktree `PSoXide-quake-bsp`): zero file overlap with the surfaces above today; its docs promise the skeletal pipeline stays untouched ("the migration's core promise"). Rules to keep it frictionless: no new `ViewTool` variant, no new `EditorWorkspace` field (all state lives on `ModelAnimationViewerState`), no layout changes to `psx-level` records (none are needed; strictly additive if P5b happens), and sync with that effort before its P3 entity migration rewrites `LevelGameEntityRecord` coordinates.
- **Live WIP in the main checkout** (`codex/windowed-classic-affine`, uncommitted): `import_locomotion.rs` gains a `--new-model` flag and a native model cook path, plus rustfmt noise. This plan does not touch that file; trivial rebase whichever lands first.
- **30fps campaign**: the weapon adds one small rigid draw (61 to 94 tris) to the player band. Check the per-vblank chart after P3; no structural risk expected.

## Asset notes

Sword1_Light: 208 verts, 94 tris. Sword1_Heavy: 115 verts, 61 tris. Rigid, no skin, no clips, origin at grip, blade along +Y, roughly 1.2 to 1.4 Blender units long. Material `M_Enemy01` over `Diffuse_Enemy01_256px`, the enemy atlas, so likely zero extra VRAM when that page is resident (cook dedupes textures by path; confirm at P1). GLB and FBX both provided; use GLB.

## Decisions (Manny, 2026-08-10)

1. Wielder: both player and enemy from the start, so P5 (enemy equipment draw) is in the main line, not optional.
2. Project: cortex_anim first, port to the game project after. Caveat: sockets live per-project on ModelResource, they do not auto-propagate; the port step needs its own checklist item.
3. Hitboxes: authored on character joints via the existing capsule editor. The dead weapon-hitbox system (G6) stays a separate cleanup, not part of this branch.
