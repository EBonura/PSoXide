# Tank Boss source

`Enemy_02.fbx` is the project copy of the artist-delivered boss model. The
delivery calls this character "Tank". `Diffuse_Enemy01_256px.png` is the
matching 256 x 256 source texture; the FBX also embeds it.

Legacy FBX import settings:

- 1536-unit world height
- 128 x 128, 8bpp cooked atlas
- 12 Hz animation sampling
- default detail-bone collapse, retaining 21 joints

The FBX contains one static take named
`Armature_heavy|mixamo_import_raw`. It is useful for the authored rest
transforms, but it must not be retained as a multi-frame clip. Equivalent
endpoint quaternions have opposite signs, so interpolation can cross a zero
quaternion and make the model disappear. The registered `Tank Boss / Rest
Pose` is the first frame of that take only.

The FBX bind basis does not survive an FBX/GLB animation round trip cleanly.
Keep `Tank Boss Model` and its 21-joint rest clip as the delivered-model
backup, but do not retarget generated GLB clips onto it.

## Native animated model

The GLBs in `animations/heavy_walk_pack/` contain the same boss mesh, texture,
and 27-joint rig. They are cooked together as `Tank Boss Animated Model`, so
the model and clips share one native bind basis. The boss Character resource
uses its one-frame idle plus forward, backward, and left/right heavy walks;
the old FBX resources remain in the project as a reversible backup.

The heavy walk was generated locally with MoMask on CPU from:

> A large heavy person walks forward slowly and steadily with short deliberate
> steps, a planted upright torso, and only a small restrained arm swing.

- Generator seed: `260821`; selected candidate: repeat 1
- Raw motion: `animations/source/tank_boss_heavy_walk_momask_seed_260821.bvh`
- Selected source window: frames 75-158, 83-frame cycle at 30 Hz
- Measured stance speed: approximately 0.84 m/s
- Loop treatment: four-frame seam blend plus hips-only foot grounding
- Direction pass: retimed to 4.2 seconds using a constant pose-distance clock
  across the complete loop. This removes the generated gait's unequal
  left/right acceleration without adding foot-specific dwell. Pelvis motion is
  retained at 34%, torso motion at 42-46%, and upper-body counter-swing at
  50-68% to produce a slower, clunkier silhouette.
- Cooked output: 51 frames at 12 Hz
- Review render: `animations/previews/tank_boss_heavy_walk_ai.mp4`

The directional extension keeps the approved 4.2-second forward/backward
cadence, while the more reactive strafes use a 2.0-second cycle:

- `walk_bwd.glb` reverses the approved forward cycle. That produces a true
  backpedal while preserving its exact weight, grounding, and left/right
  timing.
- `walk_rgt.glb` uses frames 61-96 of MoMask seed `10107`, repeat 2, generated
  from "a person steps sideways to their right, staying square to the front."
  It uses the forward walk's pose-distance retiming and upper-body damping, but
  is intentionally 2.1x faster than the first 4.2-second strafe pass; matching
  the forward duration made the lateral motion read as extreme slow motion.
- `walk_lft.glb` is a rig-aware mirror of the right strafe, keeping both sides
  mechanically identical.
- Raw selected strafe:
  `animations/source/tank_boss_heavy_strafe_right_momask_seed_10107_repeat2.bvh`
- Four-way review grid (forward, backward, left, right):
  `animations/previews/tank_boss_heavy_directional_walks_ai.mp4`

There is deliberately no `run_fwd.glb`. An absent Run action is a gameplay
capability: the player motor refuses sprint state, and enemy pursuit uses the
walk clip and walk speed instead of treating the visual fallback as a run.

The looser 2.8-second first pass is retained as
`animations/source/tank_boss_heavy_walk_ai_first_pass.glb`, with its render at
`animations/previews/tank_boss_heavy_walk_ai_first_pass.mp4`, so the direction
change remains directly reversible.
