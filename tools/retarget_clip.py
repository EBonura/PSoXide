#!/usr/bin/env python3
"""Retarget ONE take onto Aletha, frame for frame, and export it. Nothing else.

`aletha_walk_study.py` exists to build gaits: it layers a take over an idle,
retimes it, detects a cycle and splits phases. Every one of those steps is
wrong for a one-shot performance like an attack, where the recorded timing is
the point. This does the single thing that is actually needed: run the
retarget bridge over the take's own frame range and write a GLB.

    blender --background --factory-startup --python tools/retarget_clip.py -- \
        --source ~/Downloads/Aletha.glb --clip raw.glb --out out.glb [--take NAME]

Check the result with tools/anim_validate.py --against the same clip.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import bpy

sys.path.insert(0, str(Path(__file__).resolve().parent))
from aletha_bvh_retarget import import_animation_source, retarget  # noqa: E402
from aletha_walk_study import import_source  # noqa: E402


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path, help="Rigged target GLB")
    parser.add_argument("--clip", required=True, type=Path, help="Take to retarget")
    parser.add_argument("--take", help="Action name inside the clip")
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument(
        "--fps",
        type=int,
        default=30,
        help="Scene fps. MUST match the source's rate: Blender's default scene "
        "is 24, so exporting a 30 fps take from a 24 fps scene stretches it by "
        "1.25x, which the cook then bakes in (a 1.9 s attack became 2.4 s).",
    )
    parser.add_argument(
        "--smooth",
        action="store_true",
        help="Temporal low-pass (for 20 Hz generated takes; off for authored ones)",
    )
    return parser.parse_args(argv)


def main() -> None:
    args = parse_args()
    # Set the rate BEFORE importing: the glTF importer converts sampler times
    # to frames with the scene's fps.
    bpy.context.scene.render.fps = args.fps or 30
    rig = import_source(args.source.expanduser())
    for bone in rig.pose.bones:
        bone.rotation_mode = "QUATERNION"

    src = import_animation_source(args.clip.expanduser(), args.take)
    action = src.animation_data.action
    first, last = int(action.frame_range[0]), int(action.frame_range[1])
    print(f"[retarget] {args.clip.name}: frames {first}..{last} ({last - first + 1})")

    baked, speed = retarget(src, rig, args.clip.stem, first, last, smooth=args.smooth)
    print(f"[retarget] source root speed {speed:.3f} m/s")

    # Export the target rig alone, with exactly this action, over exactly the
    # take's frame range: no idle layer, no retime, no phase split.
    rig.animation_data.action = baked
    scene = bpy.context.scene
    scene.frame_start, scene.frame_end = first, last
    bpy.ops.object.select_all(action="DESELECT")
    rig.select_set(True)
    for child in rig.children_recursive:
        child.select_set(True)
    bpy.context.view_layer.objects.active = rig
    out = args.out.expanduser()
    out.parent.mkdir(parents=True, exist_ok=True)
    result = bpy.ops.export_scene.gltf(
        filepath=str(out),
        export_format="GLB",
        use_selection=True,
        export_animations=True,
        export_animation_mode="ACTIVE_ACTIONS",
        export_frame_range=True,
        export_force_sampling=True,
        export_anim_single_armature=True,
        export_reset_pose_bones=True,
        export_optimize_animation_size=False,
    )
    if "FINISHED" not in result:
        raise SystemExit(f"export failed: {out}")
    print(f"[retarget] wrote {out}")


main()
