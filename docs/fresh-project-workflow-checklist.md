# Fresh project: owner GUI checklist

A short hands-on pass proving a project you create today supports the whole
souls authoring loop. Its sibling, [souls-slice-acceptance.md](souls-slice-acceptance.md),
is the same idea for the *tracked* slice at `editor/projects/souls-bsp-vertical-slice`;
this one starts from File > New Project instead, so it also covers the steps
you only take once per map.

Nothing here has pass/fail numbers to collect. The automated gates already
pin every number, and each section below says exactly which ones. What is
left is the judgment a headless test cannot make.

```sh
make run-release
```

## 0. Arm the project

New Project copies the roofless starter courtyard: five brushes, two
materials, no characters. Resources panel > the add/import menu >
**Starter Characters** syncs the player, the Rust Mantis, both swords and
their clips into the project. Do this first; without it the Place >
Character lane has nothing to place.

*Proven headlessly:* the template copy is byte-identical to the tracked
`editor/projects/brush-open-courtyard` (`make editor-blank-playtest-check`
diffs `project.ron` and both `.psxt` files), and the sync lands every model,
clip and profile as a byte-copy of the verified defaults
(`starter_character_sync_arms_a_new_project_with_verified_combat_content`).

*Yours to confirm:* the menu item is where you expect it, and the status
line reports what it synced.

## 1. Brushes

Drag a box in the Top view, click it, move it, resize it, then click a face
and retexture it (swap Courtyard Cobbles and Courtyard Brick with **Apply to
face**, or sweep the Material Paint tool across several faces).

*Proven headlessly:* creation, selection, Move/Resize/Edge/Vertex drags in
Top, Front and Side, and the same Move and Resize from the Select tool, all
through real egui pointer events on the real viewport response
(`bsp_brush_click_selection_runs_through_real_egui_response_dispatch` and
`select_tool_selected_brush_uses_visible_move_and_resize_via_real_egui`, both
run by `make editor-blank-playtest-check`, plus
`visible_brush_modes_drive_plain_drag_move_and_resize_in_every_2d_view`).
Retexturing paints
exactly the face under the cursor, costs one undo per gesture, survives save
and reopen, and changes the cooked brush world
(`apply_to_face_button_paints_only_the_selected_face_and_undoes_once`,
`material_paint_click_paints_one_bsp_brush_face_and_samples_it_back`,
`face_material_swap_survives_reopen_and_reaches_the_cooked_brush_world`).

*Yours to confirm:* that the brush the cursor is over is the brush that
lights up at your display scale, that the handles are big enough to grab,
and that the 3D view repaints the face after a retexture.

## 2. The five entities

Place a player and an enemy through Place > Character (pick the profile in
the Resources panel first), then:

- **Door**: Place > Logic, rename it, switch its Inspector **Kind** combo to
  *Door*, and bind the door brush with the brush Inspector's **Model owner**
  combo.
- **Trigger**: Place > Logic again, set its extent, and type the checkpoint
  entity's name into **Target**. A volume with no target is rejected at cook
  time with `Trigger Volume '<name>' has no target`, and the editor selects
  and frames the offending node, so an empty Target costs one Play attempt.
- **Checkpoint**: an Entity, then Add Component > Interactable, then its
  Inspector **Kind** combo to *Checkpoint*.

*Proven headlessly:* all five placed on a freshly created project through the
production command paths, cooked, and verified down to the body hulls, the
door mover, both equipment records and the trigger-to-checkpoint chain
(`souls_slice_project_is_authored_through_production_commands`, which
`make editor-souls-bsp-check` re-runs and diffs against the tracked slice).
Both Inspector kind combos are driven through real egui
(`fresh_project_inspector_switches_logic_to_door_and_interactable_to_checkpoint`).
A placed trigger's box contains a player standing on the surface it was
placed on, and fires once
(`placed_trigger_volume_contains_a_player_standing_on_the_placement_surface`).

*Yours to confirm:* that the placement gizmos land where you aimed, and that
the Inspector reads back the profile, hurtbox and equipment child you
expect.

## 3. Save, reopen, cook, play

Save, close and reopen the project, then press **Play**. Edit a brush and
press **Rebuild & Play**.

*Proven headlessly:* author, save, cook, reopen, edit, recook, with the
second cook of unchanged data byte-identical
(`bsp_new_project_can_author_save_cook_edit_and_recook_without_grid_rooms`,
`bsp_blank_slate_commands_preserve_rooted_prop_door_and_portal_contract`);
the Play and Rebuild & Play buttons emit their requests through real egui
(`play_and_rebuild_buttons_emit_requests_through_real_egui_input`); the
tracked slice still opens in the editor without rewriting itself and cooks
clean (`tracked_souls_slice_opens_clean_in_the_editor_and_cooks_without_errors`).
`make editor-blank-playtest-check` carries an authored project through the
real cook, the MIPS link, the disc pack and two byte-identical emulator
replays.

*Yours to confirm:* how long Play takes on your machine, and that the
embedded viewport shows the map rather than a loading screen.

## 4. In the playtest

Walk into the trigger (the SYNC RELAY overlay confirms; CROSS dismisses it),
open the door with CROSS, fight, die in the lava, and check you respawn at
the relay with the enemy and door reset.

*Proven headlessly:* `make editor-souls-bsp-check` replays the authored tape
twice and pins the whole loop: 6 attack starts, 4 melee hits, 1 stagger,
1 enemy death, 4 hits taken, 2 weapon attachments, 1 checkpoint activation,
1 door activation, 6 liquid damage events, 1 player death, 2910 PVS
suppressions, and a pinned end-of-tape position for the confirmation walk
that follows the respawn. A neutral-pad replay pins every combat and
progression counter to zero while PVS suppression keeps accruing, which is
how the gate catches a build that quietly no-ops the souls runtime, and a
second guest binary with a different code layout reproduces the identical
simulation while presenting a different number of frames.
`make combat-checkpoint` and `make editor-bsp-liquid-check` pin the combat
and hazard halves separately.

*Yours to confirm:* everything about how it reads and feels. Camera framing
during the fight, whether the lock-on target is legible, whether the sword
looks attached to the hand in motion rather than merely being at the right
coordinates, audio, and whether the death and respawn are understandable
without knowing what should happen.

## What this checklist cannot tell you

Every claim above comes from headless tests and headless emulator replays.
No step was performed in the native editor window, and none of it has been
run on original hardware. If the editor fails to open, renders wrongly at
your display scale, or is too slow to author in, the gates above will still
be green.
