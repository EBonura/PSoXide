# Cortex Ignition dodge: controls, i-frames, cage (2026-09-03)

Manny: the dodge should follow stick plus Circle and does not always; it is
not responsive; the wireframe is strange; are there i-frames during it?

## What the code did

Circle is one button for tap-evade and hold-sprint (`update_evade_run_button`).
The evade fired on **release**, and a press held past
`EVADE_RUN_HOLD_VBLANKS` = 8 (133 ms) became sprint intent with no dodge.
An ordinary tap is 80 to 150 ms, so about half of them produced no dodge.
The direction was read at release, after a quick tap had often let the
stick go, so the dodge went straight ahead. The motor only accepts an evade
while idle and nothing was buffered, so a tap during an attack, a hit
reaction or the previous dodge was dropped.

I-frames exist: `roll_invulnerable_frames` covers motor frames 0 to 9 of a
35-frame dodge (22 active, 13 recovery), and both enemy melee contact and
projectiles skip the player while it holds. The cage is timed on the clip:
solid to 3%, converting to 25%, full wire to 75%, restoring to 97%. On the
captured roll the wire showed on frames 4 to 24, so most of the cage was
vulnerable.

The cage drew the longest bind-pose edge of every fourth face, about 125
near-white lines for the 506-face player, which reads as loose sticks.

## Changes (fix/cortex-dash)

- hold threshold 8 to 16 vblanks; the stick is latched at press and
  refreshed while held; a release with a neutral stick dodges the latched
  way;
- a tap while locked is buffered for max(12, remaining lock) vblanks,
  capped at 48, and fires on the first free tick;
- `roll_invulnerable_frames` 10 to 25 on the player controller, ending as
  the texture restores;
- cage on every second face.

## Evidence (headless, same press script on both builds)

`before.png` and `after.png`: rows are a tap, d-pad left plus tap, a
12-vblank press (before: no dodge; after: dodge), R1 then a tap, and a tap
as the stick is released. `recovery-cancel.png`: a tap 20 ticks before the
swing ends fires the dodge at frame 3392 after, nothing before.
`cage-before-zoom.png`: the old cage.

Not done: the wire timing and the i-frame window are two independent
numbers (clip fraction vs motor frames); they match now by construction of
the values, not by code. If the roll clip is retimed, re-check them.

## Actor face reduction (same day)

Manny asked whether the actors' face counts could come down. Blender's
Decimate (collapse) on the source GLBs is the standard quadric algorithm,
but re-importing a GLB rebuilds the model's palette layout (see
`tools/derive_clawless_psx_model.py`), and the heavy enemy has no source
GLB in the repo at all, so `tools/psmd_decimate.py` collapses short edges on
the cooked PSMD instead: inside one rigid part only, never across a joint
or a texture seam, palette banks and header untouched, so clips and colours
keep working.

At ratio 0.85: heavy enemy 756 to 656 faces, light enemy 484 to 449,
Aletha 506 to 452 (`actor-decimation-ladder.png` shows 0.85 and 0.75 on the
GLBs; `actor-decimation-ingame.png` the cooked result in game at 2x).
Whole-level tape, lockstep, all three actors decimated: bus cycles -0.76%,
18.481 to 18.623 fps. The actor cost is joints and vertex projection more
than faces, so face reduction is a weak lever; not shipped, tool kept.

## Aletha's two-bone vertices (same day, shipped)

The expensive part of the actors is not their faces. Aletha had 153 of 265
vertices on the CPU two-bone blend path (the enemies have 35 and 38), and
121 of those carried a secondary weight of 25% or less. `tools/psmd_decimate.py
--blend-threshold 64` snaps those to their primary bone on the cooked model;
the 32 heavy seam vertices stay blended. Whole-level tape, lockstep: bus
cycles -1.55%, 18.481 to 18.772 fps, and the frames at four tape polls are
the same to the eye (`aletha-blend-threshold-ingame.png`). Shipped as the
cooked `aletha_delivered.psxmdl`; the source GLB and the cooker are
untouched, so a re-cook from the GLB would need the pass run again.
