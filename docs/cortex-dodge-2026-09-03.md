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
