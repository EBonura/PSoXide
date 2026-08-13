# `tools/` (standalone utilities)

Small host-side command-line utilities used by the build and disc-mastering
flow. Each is its own workspace.

| Tool | Purpose |
|------|---------|
| [`mkisopsx`](mkisopsx) | Wrap a PSX-EXE into a bootable PS1 disc image (BIN/CUE). Used by the example and project-disc build targets. |
| [`psx-exe-pack`](psx-exe-pack) | `psx-exe-info`: validate a PSX-EXE and print its header. The linker script already emits a PSX-EXE header; this verifies it. |
| [`kimodo_retarget.py`](kimodo_retarget.py) | Run through Blender to retarget a Kimodo SOMA standard-T-pose BVH clip onto a Mixamo-style FBX armature and export an animation-only FBX for PSoXide. |
| [`locomotion_study.py`](locomotion_study.py) | Generate a restrained idle directly on a Mixamo-style character, then build a compatible gait into idle/windup/cruise/winddown previews, metadata, and optional animation-only FBXs. |
| [`attack_study.py`](attack_study.py) | Wrap an exact-rest humanoid attack with idle anticipation, a weapon-spawn cue, attack/follow-through, and idle recovery; render a preview-only spectral weapon and export animation-only FBX. |
| [`inspect_animation_assets.py`](inspect_animation_assets.py) | Inspect FBX/GLB animation carriers in Blender and report actions, bone/hierarchy overlap, and parent-local bind diagnostics against a target FBX. |

## Kimodo animation retargeting

Export a SOMA clip from Kimodo with `--bvh --bvh_standard_tpose`, then run:

```sh
/Applications/Blender.app/Contents/MacOS/Blender \
  --background --factory-startup \
  --python tools/kimodo_retarget.py -- \
  --target /path/to/mixamo-character.fbx \
  --motion /path/to/kimodo-motion.bvh \
  --output /path/to/animation-only.fbx \
  --action-name run_leap
```

Optional `--blend-output` and `--preview` arguments write a diagnostic Blender
scene and a camera-following MP4. The FBX can then be passed repeatedly to
`psxed glb-model --animation`; compatible characters can share the same export.

## Spawn-cued attack studies

Use an exact-rest GLB/FBX attack performance to build an animation-only player
clip with a deliberate conjuration beat:

```bash
/Applications/Blender.app/Contents/MacOS/Blender \
  --background --factory-startup \
  --python tools/attack_study.py -- \
  --target /path/to/player.fbx \
  --motion /path/to/light_attack.glb \
  --output /path/to/light_attack_preview.mp4 \
  --fbx-output /path/to/light_attack.fbx \
  --metadata /path/to/light_attack.json \
  --action-name light_attack_spawn_cued \
  --hit-count 1
```

The bright blade exists only in the preview. The FBX contains the armature and
baked action, so weapon spawning and socket attachment remain gameplay events.
The JSON records the spawn cue, phase splits, and proposed hit windows on both
the 30 Hz authoring timeline and the zero-based 15 Hz cooked timeline.

## See also

- [Root README](../README.md#quick-start). Build and run flow.
- [`crates/psx-iso`](../crates/psx-iso). The BIN/CUE / ISO9660 parsing these build on.
