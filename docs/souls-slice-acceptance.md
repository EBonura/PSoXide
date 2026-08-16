# Souls vertical slice: owner acceptance script

A short native-editor checklist for the tracked project at
`editor/archive/fixtures/souls-bsp-vertical-slice`. Everything below is a
hands-on look; the automated gate already covers the deterministic
loop (see the last section), so this session is about feel, not
correctness.

## Launch

```sh
make run-release
```

Then File > Open Project > `souls-bsp-vertical-slice` (or open the
directory `editor/archive/fixtures/souls-bsp-vertical-slice`).

## What to try

1. Top view: two combat spaces split by a thin divider, the lift-door
   brush in the doorway gap, the lava pool in the far room's south, and
   the sealed crypt box in its east. Drag a wall brush, resize it in the
   inspector, undo both.
2. Select a wall face and retexture it (swap Courtyard Brick and
   Courtyard Cobbles); confirm the face preview updates.
3. Select the Mantis Enemy entity and move it a little; check the
   inspector shows the Rust Mantis Enemy profile, its torso hurtbox,
   and the Sword1 Heavy equipment child.
4. Select the lift-door brush: the inspector's Model owner combo should
   read "Lift Door". Select the lava brush: BSP contents reads Lava.
5. Press Play. Walk forward through the trigger: the SYNC RELAY overlay
   confirms the checkpoint (CROSS dismisses it). Open the door with
   CROSS, fight the Mantis (R3 locks on, R1/R2/L2 attack), and check
   the sword rides the right hand on both characters.
6. Die deliberately in the lava pool. You should respawn at the sync
   relay, not the spawn, with the enemy and door reset.
7. Optional: rebuild-and-play after any brush edit to confirm the
   cook-while-running loop.

## What the automated gate already guarantees

`make editor-souls-bsp-check` re-authors this exact project through
production editor commands (drift fails the gate), clean-cooks it,
builds the MIPS guest and disc, and replays two tapes headlessly:

- canonical: checkpoint touch, door opening, a doorway-pinch kill
  (4 authored hits, 1 stagger, 4 hits taken), lava death, checkpoint
  respawn with the world reset, and a confirmation walk into the
  re-closed door, with every souls counter pinned exactly and the
  VRAM/display hashes byte-stable across runs;
- negative: no inputs, every progression counter pinned to zero, PVS
  suppressions still accruing from the sealed crypt sentinel.

So this checklist has no pass/fail numbers to collect; it exists for
the native-window judgment the gate cannot make (readability, feel,
camera, and audio).

For the same loop starting from File > New Project instead of this
tracked slice, see
[fresh-project-workflow-checklist.md](fresh-project-workflow-checklist.md).
