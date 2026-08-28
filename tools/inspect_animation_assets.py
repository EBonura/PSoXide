#!/usr/bin/env python3
"""Inspect animation carriers and compare their armatures with a target FBX.

Run through Blender, for example:

    blender --background --factory-startup --python tools/inspect_animation_assets.py -- \
        --target /path/to/player.fbx --source /path/to/light.glb --source /path/to/heavy.glb
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import bpy


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, type=Path)
    parser.add_argument("--source", action="append", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--fps",
        type=int,
        default=30,
        help="Scene rate used while importing glTF sampler times (default: 30)",
    )
    return parser.parse_args(argv)


def strip_namespace(name: str) -> str:
    return name.rsplit(":", 1)[-1]


def import_asset(path: Path, *, animations: bool) -> None:
    suffix = path.suffix.lower()
    if suffix == ".fbx":
        result = bpy.ops.import_scene.fbx(filepath=str(path), use_anim=animations)
    elif suffix in {".glb", ".gltf"}:
        result = bpy.ops.import_scene.gltf(filepath=str(path), import_shading="NORMALS")
    elif suffix == ".bvh":
        result = bpy.ops.import_anim.bvh(filepath=str(path), target="ARMATURE")
    else:
        raise RuntimeError(f"Unsupported asset type: {path}")
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import {path}")


def one_armature(label: str) -> bpy.types.Object:
    armatures = [obj for obj in bpy.data.objects if obj.type == "ARMATURE"]
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one {label} armature, found {len(armatures)}")
    return armatures[0]


def action_record(action: bpy.types.Action) -> dict[str, object]:
    slots = getattr(action, "slots", ())
    return {
        "name": action.name,
        "frame_start": float(action.frame_range[0]),
        "frame_end": float(action.frame_range[1]),
        "frame_count_inclusive": int(round(action.frame_range[1] - action.frame_range[0])) + 1,
        "fcurves": len(action.fcurves),
        "slots": len(slots),
    }


def inspect_source(target_path: Path, source_path: Path, fps: int) -> dict[str, object]:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    # Blender converts glTF animation times to scene frames during import.
    # Set the source rate first or a 60 Hz take is silently reported at the
    # factory scene's 24 Hz frame count.
    bpy.context.scene.render.fps = fps
    import_asset(target_path, animations=False)
    target = one_armature("target")
    target_bones = {
        strip_namespace(bone.name).lower(): bone for bone in target.data.bones
    }

    target_names = {obj.name for obj in bpy.data.objects}
    target_actions = {action.name for action in bpy.data.actions}
    import_asset(source_path, animations=True)
    source_armatures = [
        obj
        for obj in bpy.data.objects
        if obj.type == "ARMATURE" and obj.name not in target_names
    ]
    if len(source_armatures) != 1:
        raise RuntimeError(
            f"Expected one source armature in {source_path}, found {len(source_armatures)}"
        )
    source = source_armatures[0]
    source_actions = [
        action for action in bpy.data.actions if action.name not in target_actions
    ]

    source_lookup = {
        strip_namespace(bone.name).lower(): bone for bone in source.data.bones
    }
    common: list[str] = []
    translation_errors: list[float] = []
    rotation_errors: list[float] = []
    hierarchy_mismatches: list[str] = []
    source_height = (
        source_lookup["hips"].head_local - source_lookup["leftfoot"].head_local
    ).length
    target_height = (
        target_bones["hips"].head_local - target_bones["leftfoot"].head_local
    ).length
    for source_bone in source.data.bones:
        base = strip_namespace(source_bone.name).lower()
        target_bone = target_bones.get(base)
        if target_bone is None:
            continue
        common.append(base)
        source_parent = (
            strip_namespace(source_bone.parent.name).lower()
            if source_bone.parent is not None
            else None
        )
        target_parent = (
            strip_namespace(target_bone.parent.name).lower()
            if target_bone.parent is not None
            else None
        )
        if source_parent != target_parent:
            hierarchy_mismatches.append(base)
        # Parent-local rest transforms are invariant to the global axis and
        # uniform-scale conversions Blender's FBX exporter applies to an
        # animation-only armature.
        if source_bone.parent is not None and target_bone.parent is not None:
            source_local = source_bone.parent.matrix_local.inverted() @ source_bone.matrix_local
            target_local = target_bone.parent.matrix_local.inverted() @ target_bone.matrix_local
            translation_errors.append(
                (
                    source_local.translation / max(source_height, 1.0e-8)
                    - target_local.translation / max(target_height, 1.0e-8)
                ).length
            )
            angle = source_local.to_quaternion().rotation_difference(
                target_local.to_quaternion()
            ).angle
            rotation_errors.append(math.degrees(min(angle, math.tau - angle)))

    source_scene = bpy.context.scene
    return {
        "path": str(source_path),
        "objects": {
            object_type: sum(1 for obj in bpy.data.objects if obj.type == object_type)
            for object_type in ("ARMATURE", "MESH", "EMPTY")
        },
        "source_armature": source.name,
        "source_bones": len(source.data.bones),
        "target_bones": len(target.data.bones),
        "common_bones": len(common),
        "common_bone_names": common,
        "missing_target_bones": sorted(
            strip_namespace(bone.name)
            for bone in source.data.bones
            if strip_namespace(bone.name).lower() not in target_bones
        ),
        "structurally_compatible": len(common) == len(target.data.bones)
        and not hierarchy_mismatches,
        "hierarchy_mismatches": hierarchy_mismatches,
        "max_parent_local_translation_error_normalized": max(
            translation_errors, default=None
        ),
        "max_parent_local_rotation_error_degrees": max(rotation_errors, default=None),
        "scene_fps": source_scene.render.fps / source_scene.render.fps_base,
        "actions": [action_record(action) for action in source_actions],
    }


def main() -> None:
    args = parse_args()
    records = [inspect_source(args.target, source, args.fps) for source in args.source]
    payload = json.dumps(records, indent=2)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(payload + "\n")
    print("ANIMATION_ASSET_INSPECTION")
    print(payload)


if __name__ == "__main__":
    main()
