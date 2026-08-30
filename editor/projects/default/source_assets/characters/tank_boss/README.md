# Tank Boss source

Generated animation candidates and review renders are local working inputs
kept outside version control under `local-assets/`. The repository retains the
model sources and the cooked runtime `.psxanim` clips.

`Enemy_02.fbx` is retained only as provenance for the artist delivery, which
calls this character "Tank". It is not registered as a second engine model and
is not copied into newly created projects. `Diffuse_Enemy01_256px.png` is the
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
The obsolete 21-joint `Tank Boss Model`, rest clip, and placeholder animation
set were therefore removed. All runtime animation targets the native model
below.

## Native animated model

The GLBs in `animations/heavy_walk_pack/` and the selected core-animation
candidates contain the same boss mesh, texture, and 27-joint rig. Selected
Idle 2 is the authoritative model source; every additional take is cooked
against its fixed model bounds, either in the original native bundle or in a
later fixed-bound combat pass. This is important because `.psxanim` does not
carry a quantisation frame of its own:
letting a high-travel death take enlarge the model bounds would shrink the
standing boss, while cooking later clips against a different frame would pull
its rigid armour sections apart. The boss Character resource uses Idle 2 plus
forward, backward, and left/right heavy walks, selected Attack 3 as its light
two-handed shove, selected Attack 1 as its heavy overhead smash, the authored
MoMask missile-salvo candidate 3 as its ranged attack, selected Hit 2, and selected
Death 2. The combat source takes are retained under `animations/combat/`; all
are cooked against Idle 2's fixed model bounds so their rigid armour sections
use the same quantisation frame as the model.

The ranged take was generated locally through the established MoMask audition
workflow, retargeted onto this exact textured 27-joint rig, and selected from a
numbered four-candidate reel. Its provenance is generation
`psoxide_tank_missile_salvo_v1`, seed `260850`, candidate `attack_03`. The
projectile events for the three chimneys remain independently authorable on the
animation timeline.

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
- Review render: local-only (not tracked)

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
- Four-way review grid (forward, backward, left, right): local-only (not
  tracked)

There is deliberately no `run_fwd.glb`. An absent Run action is a gameplay
capability: the player motor refuses sprint state, and enemy pursuit uses the
walk clip and walk speed instead of treating the visual fallback as a run.

The looser 2.8-second first pass is retained as
`animations/source/tank_boss_heavy_walk_ai_first_pass.glb`; its review render
is local-only, so the direction change remains directly reversible.
