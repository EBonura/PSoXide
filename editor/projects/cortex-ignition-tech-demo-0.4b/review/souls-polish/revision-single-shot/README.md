# Ground-only dash, one charged shot, three melee windows

The regular disc and source project have been rebuilt. No diagnostic spawn or
AI override is included. Heavy animations were not changed.

The light melee hitboxes are named `Claw Combo / Swing 1`, `Swing 2`, `Swing 3`.
Their source-frame windows are 13-18, 28-33 and 40-45. Cooking trims the first two
frames, so native windows are 11-16, 26-31 and 38-43. Each can damage the player
once per attack animation. Simultaneously active capsules consume one contact;
invulnerability, occlusion and interrupted/stale tokens retain their guards.
The claw ribbon uses the same window and capsule endpoints, clears in every gap,
and clamps its history to the current swing. These are starter timings for
further editing of the existing blade capsule in the animation editor.

Ranged attacks now have one emitter at source frame 27; the first recoil was
removed from the motion. Standard damage returns to 20, and the stronger existing
variant stays at 28. English/Italian guidance describes one charged shot.

Validation: 204 runtime and 57 playtest tests pass. New tests cover independent
melee windows, duplicate-contact rejection, interrupted/new attacks, and trail
history boundaries. Regular native combat and dash captures have zero guest
faults. The normal replay releases one projectile per attack (frames 2141, 2375,
2941, 3175); the former early shots at 2105 and 2339 are absent. Native dash frames
show ground rings without movement streaks. The stationary-player replay exercises
melee motion; it does not prove every starter capsule hits from every angle.
Disc structural checks and the R3000 hazard scan pass. Heap headroom is 1,056 bytes.
Physical-console testing remains outstanding.

Earlier videos in `../videos/` show the superseded movement lines and two-shot
attack. Keep them as review history, not a preview of this revision.
