#!/usr/bin/env python3
"""Build a spawn-cued attack study from an exact-rest humanoid animation.

The source motion is kept intact, but wrapped with an authored empty-hand
anticipation, a readable weapon-spawn hold, and a return to the generated idle.
The exported FBX contains only the target armature; the preview weapon is never
included in the animation asset.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import bpy
from mathutils import Matrix, Quaternion, Vector

sys.path.insert(0, str(Path(__file__).resolve().parent))
import locomotion_study as locomotion  # noqa: E402


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, type=Path)
    parser.add_argument("--motion", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path, help="MP4 preview")
    parser.add_argument("--fbx-output", type=Path)
    parser.add_argument("--blend-output", type=Path)
    parser.add_argument("--metadata", type=Path)
    parser.add_argument("--action-name", default="spawn_attack")
    parser.add_argument("--attack-label", default="Attack")
    parser.add_argument("--anticipation-frames", type=int, default=12)
    parser.add_argument("--spawn-hold-frames", type=int, default=6)
    parser.add_argument(
        "--release-frames",
        type=int,
        default=8,
        help="Frames easing from the held conjuration pose into the source performance",
    )
    parser.add_argument("--recovery-frames", type=int, default=14)
    parser.add_argument(
        "--hit-count",
        type=int,
        default=1,
        help="Number of proposed active windows to infer from weapon-hand speed",
    )
    parser.add_argument("--source-start", type=int)
    parser.add_argument("--source-end", type=int)
    parser.add_argument("--asset-fps", type=int, default=15)
    parser.add_argument(
        "--raw-source",
        action="store_true",
        help="Preview the unwrapped source motion for timing review",
    )
    return parser.parse_args(argv)


def import_source(path: Path) -> tuple[bpy.types.Object, bpy.types.Action]:
    object_names = {obj.name for obj in bpy.data.objects}
    action_names = {action.name for action in bpy.data.actions}
    if path.suffix.lower() in {".glb", ".gltf"}:
        result = bpy.ops.import_scene.gltf(filepath=str(path), import_shading="NORMALS")
    elif path.suffix.lower() == ".fbx":
        result = bpy.ops.import_scene.fbx(filepath=str(path))
    else:
        raise RuntimeError(f"Unsupported motion asset: {path}")
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import motion asset: {path}")
    armatures = [
        obj
        for obj in bpy.data.objects
        if obj.type == "ARMATURE" and obj.name not in object_names
    ]
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one source armature, found {len(armatures)}")
    actions = [action for action in bpy.data.actions if action.name not in action_names]
    if not actions:
        raise RuntimeError(f"No action in motion asset: {path}")
    action = max(actions, key=lambda candidate: candidate.frame_range.length)
    source = armatures[0]
    source.name = "Attack_Source_Armature"
    if source.animation_data is None:
        source.animation_data_create()
    for track in source.animation_data.nla_tracks:
        track.mute = True
    source.animation_data.action = action
    return source, action


def interpolate_pose(
    first: dict[str, tuple[Vector, Quaternion, Vector]],
    second: dict[str, tuple[Vector, Quaternion, Vector]],
    amount: float,
) -> dict[str, tuple[Vector, Quaternion, Vector]]:
    identity_rotation = Quaternion((1.0, 0.0, 0.0, 0.0))
    identity_scale = Vector((1.0, 1.0, 1.0))
    result: dict[str, tuple[Vector, Quaternion, Vector]] = {}
    for name in first.keys() | second.keys():
        a_location, a_rotation, a_scale = first.get(
            name, (Vector((0.0, 0.0, 0.0)), identity_rotation, identity_scale)
        )
        b_location, b_rotation, b_scale = second.get(
            name, (Vector((0.0, 0.0, 0.0)), identity_rotation, identity_scale)
        )
        result[name] = (
            a_location.lerp(b_location, amount),
            a_rotation.slerp(b_rotation, amount),
            a_scale.lerp(b_scale, amount),
        )
    return result


def source_pose(
    scene: bpy.types.Scene,
    source: bpy.types.Object,
    mapping: list[tuple[str, str]],
    frame: float,
) -> dict[str, tuple[Vector, Quaternion, Vector]]:
    locomotion.set_source_frame(scene, frame)
    pose: dict[str, tuple[Vector, Quaternion, Vector]] = {}
    for source_name, target_name in mapping:
        location, rotation, scale = source.pose.bones[source_name].matrix_basis.decompose()
        if locomotion.strip_namespace(target_name).lower() == "hips":
            # Gameplay owns world-space displacement. The attack keeps only
            # its vertical body compression and weight transfer.
            location.x = 0.0
            location.y = 0.0
        pose[target_name] = (location.copy(), rotation.copy(), scale.copy())
    return pose


def authored_summon_pose(
    target: bpy.types.Object,
    idle_pose: dict[str, tuple[Vector, Quaternion, Vector]],
    source_ready: dict[str, tuple[Vector, Quaternion, Vector]],
) -> dict[str, tuple[Vector, Quaternion, Vector]]:
    """Make the first attack pose read as a low, side-hand conjuration beat."""

    pose = interpolate_pose(idle_pose, source_ready, 0.74)
    lookup = locomotion.bone_lookup(target)
    world_x = Vector((1.0, 0.0, 0.0))
    world_y = Vector((0.0, 1.0, 0.0))
    world_z = Vector((0.0, 0.0, 1.0))

    # Apply the blended pose so the arm-space direction operations below are
    # evaluated on the actual target hierarchy.
    apply_pose(target, pose)
    hips = target.pose.bones[lookup["hips"]]
    # Load the opposite leg so the summoning hand can hang clear of the right
    # hip without the whole pose collapsing toward that side.
    hips.location += Vector((2.4, -0.4, -1.6))
    bpy.context.view_layer.update()

    torso = (
        Quaternion(world_z, math.radians(-5.5))
        @ Quaternion(world_x, math.radians(4.5))
        @ Quaternion(world_y, math.radians(-2.0))
    )
    for bone_name, influence in (("spine", 0.35), ("spine1", 0.65), ("spine2", 1.0)):
        locomotion.set_bone_rest_rotation(
            target,
            lookup[bone_name],
            Quaternion(world_z, math.radians(-5.5) * influence)
            @ Quaternion(world_x, math.radians(4.5) * influence),
        )

    # The right arm hangs low and laterally outside the hip. The hand is not in
    # front of the chest: it owns an isolated patch of silhouette where the
    # weapon can appear before being lifted into the source performance.
    locomotion.set_bone_direction(
        target, lookup["rightarm"], torso @ Vector((-0.47, -0.03, -0.882))
    )
    locomotion.set_bone_direction(
        target, lookup["rightforearm"], torso @ Vector((-0.24, -0.12, -0.963))
    )
    locomotion.set_bone_direction(
        target, lookup["righthand"], torso @ Vector((-0.10, 0.03, -0.995))
    )

    # The free hand stays compact near the torso, keeping the asymmetry and the
    # summoning side obvious at the low resolution we are targeting.
    locomotion.set_bone_direction(
        target, lookup["leftarm"], torso @ Vector((0.34, -0.10, -0.935))
    )
    locomotion.set_bone_direction(
        target, lookup["leftforearm"], torso @ Vector((-0.18, -0.28, -0.943))
    )

    # A small eye-line cue sells intention: the character checks the empty
    # hand at their side, then looks back into the attack during release.
    locomotion.set_bone_rest_rotation(
        target,
        lookup["neck"],
        Quaternion(world_z, math.radians(-7.0))
        @ Quaternion(world_x, math.radians(3.0)),
    )
    locomotion.set_bone_rest_rotation(
        target,
        lookup["head"],
        Quaternion(world_z, math.radians(-14.0))
        @ Quaternion(world_x, math.radians(6.0)),
    )
    return locomotion.capture_basis_pose(target)


def apply_pose(
    target: bpy.types.Object,
    pose: dict[str, tuple[Vector, Quaternion, Vector]],
) -> None:
    locomotion.reset_pose(target)
    for name, (location, rotation, scale) in pose.items():
        bone = target.pose.bones.get(name)
        if bone is None:
            continue
        bone.rotation_mode = "QUATERNION"
        bone.location = location
        bone.rotation_quaternion = rotation
        bone.scale = scale
    bpy.context.view_layer.update()


def key_pose(target: bpy.types.Object, frame: int) -> None:
    for bone in target.pose.bones:
        bone.keyframe_insert("location", frame=frame, group=bone.name)
        bone.keyframe_insert("rotation_quaternion", frame=frame, group=bone.name)
        bone.keyframe_insert("scale", frame=frame, group=bone.name)


def build_action(
    source: bpy.types.Object,
    target: bpy.types.Object,
    source_action: bpy.types.Action,
    mapping: list[tuple[str, str]],
    args: argparse.Namespace,
) -> tuple[
    bpy.types.Action,
    dict[str, list[int]],
    dict[str, int],
    tuple[int, int],
    list[dict[str, int | float]],
]:
    scene = bpy.context.scene
    source_start = (
        args.source_start
        if args.source_start is not None
        else int(math.floor(source_action.frame_range[0]))
    )
    source_end = (
        args.source_end
        if args.source_end is not None
        else int(math.ceil(source_action.frame_range[1]))
    )
    if source_start >= source_end:
        raise ValueError("Source frame range must contain at least two frames")

    locomotion.apply_generated_idle_pose(target, 0.02)
    idle_pose = locomotion.capture_basis_pose(target)
    ready_pose = source_pose(scene, source, mapping, source_start)

    if args.raw_source:
        anticipation_frames = 0
        spawn_hold_frames = 0
        release_frames = 0
        recovery_frames = 0
        summon_pose = ready_pose
    else:
        anticipation_frames = args.anticipation_frames
        spawn_hold_frames = args.spawn_hold_frames
        release_frames = min(args.release_frames, source_end - source_start + 1)
        recovery_frames = args.recovery_frames
        summon_pose = authored_summon_pose(target, idle_pose, ready_pose)

    motion_peaks = infer_hit_peaks(
        scene,
        source,
        target,
        mapping,
        source_start,
        source_end,
        args.hit_count,
    )

    source_frames = source_end - source_start + 1
    total_frames = anticipation_frames + spawn_hold_frames + source_frames + recovery_frames
    if target.animation_data is None:
        target.animation_data_create()
    output_action = bpy.data.actions.new(args.action_name)
    target.animation_data.action = output_action

    for output_index in range(total_frames):
        output_frame = output_index + 1
        if output_index < anticipation_frames:
            progress = (output_index + 1) / max(1, anticipation_frames)
            pose = interpolate_pose(idle_pose, summon_pose, locomotion.smoothstep(progress))
        elif output_index < anticipation_frames + spawn_hold_frames:
            pose = summon_pose
        elif output_index < anticipation_frames + spawn_hold_frames + source_frames:
            source_index = output_index - anticipation_frames - spawn_hold_frames
            sampled_pose = source_pose(
                scene, source, mapping, source_start + source_index
            )
            if release_frames and source_index < release_frames:
                progress = (source_index + 1) / release_frames
                pose = interpolate_pose(
                    summon_pose, sampled_pose, locomotion.smoothstep(progress)
                )
            else:
                pose = sampled_pose
        else:
            recovery_index = (
                output_index - anticipation_frames - spawn_hold_frames - source_frames
            )
            progress = (recovery_index + 1) / max(1, recovery_frames)
            pose = interpolate_pose(
                source_pose(scene, source, mapping, source_end),
                idle_pose,
                locomotion.smoothstep(progress),
            )
        apply_pose(target, pose)
        key_pose(target, output_frame)

    for fcurve in output_action.fcurves:
        for keyframe in fcurve.keyframe_points:
            keyframe.interpolation = "LINEAR"

    source_output_start = anticipation_frames + spawn_hold_frames + 1
    splits: dict[str, list[int]] = {}
    if anticipation_frames:
        splits["anticipation"] = [1, anticipation_frames]
    if spawn_hold_frames:
        splits["spawn_hold"] = [anticipation_frames + 1, anticipation_frames + spawn_hold_frames]
    if release_frames:
        splits["release_to_source"] = [
            source_output_start,
            source_output_start + release_frames - 1,
        ]
    splits["source_attack"] = [
        source_output_start + release_frames,
        source_output_start + source_frames - 1,
    ]
    if recovery_frames:
        splits["return_to_idle"] = [total_frames - recovery_frames + 1, total_frames]
    events = {
        "spawn_weapon": max(1, anticipation_frames + 1),
        "attack_motion_start": source_output_start + release_frames,
        "attack_motion_end": source_output_start + source_frames - 1,
        "return_to_idle_start": total_frames - recovery_frames + 1 if recovery_frames else total_frames,
    }
    scene.frame_start = 1
    scene.frame_end = total_frames
    return output_action, splits, events, (source_start, source_end), motion_peaks


def infer_hit_peaks(
    scene: bpy.types.Scene,
    source: bpy.types.Object,
    target: bpy.types.Object,
    mapping: list[tuple[str, str]],
    source_start: int,
    source_end: int,
    hit_count: int,
) -> list[dict[str, int | float]]:
    """Propose hit instants from local maxima of the right-hand weapon tip."""

    if hit_count <= 0:
        return []
    hand_name = locomotion.bone_lookup(target)["righthand"]
    positions: list[Vector] = []
    for frame in range(source_start, source_end + 1):
        apply_pose(target, source_pose(scene, source, mapping, frame))
        hand = target.pose.bones[hand_name]
        positions.append(hand.matrix @ Vector((0.0, 78.0, 0.0)))

    speeds = [0.0] * len(positions)
    for index in range(1, len(positions) - 1):
        speeds[index] = (positions[index + 1] - positions[index - 1]).length * 0.5
    candidates = [
        index
        for index in range(1, len(speeds) - 1)
        if speeds[index] >= speeds[index - 1] and speeds[index] > speeds[index + 1]
    ]
    candidates.sort(key=lambda index: speeds[index], reverse=True)
    chosen: list[int] = []
    minimum_separation = 10
    for index in candidates:
        if all(abs(index - other) >= minimum_separation for other in chosen):
            chosen.append(index)
        if len(chosen) >= hit_count:
            break
    chosen.sort()
    return [
        {
            "source_peak_frame": source_start + index,
            "source_active_start": max(source_start, source_start + index - 2),
            "source_active_end": min(source_end, source_start + index + 2),
            "weapon_tip_speed": round(speeds[index], 5),
        }
        for index in chosen
    ]


def add_preview_weapon(target: bpy.types.Object, spawn_frame: int) -> None:
    """Add a bright, low-poly spectral blade for preview only."""

    lookup = locomotion.bone_lookup(target)
    hand_name = lookup["righthand"]

    root = bpy.data.objects.new("PREVIEW_ONLY_SpectralWeapon", None)
    bpy.context.collection.objects.link(root)
    root.parent = target
    root.parent_type = "BONE"
    root.parent_bone = hand_name
    root.location = Vector((0.0, -2.0, 0.0))
    root.rotation_euler = (0.0, 0.0, math.radians(3.0))

    bpy.ops.mesh.primitive_cube_add(size=1.0)
    blade = bpy.context.object
    blade.name = "PREVIEW_ONLY_SpectralBlade"
    blade.parent = root
    blade.location = Vector((0.0, 42.0, 0.0))
    blade.scale = Vector((2.5, 42.0, 1.15))
    blade.color = (0.18, 0.72, 1.0, 1.0)
    blade_material = bpy.data.materials.new("PREVIEW_ONLY_SpectralBlue")
    blade_material.diffuse_color = (0.08, 0.62, 1.0, 1.0)
    blade.data.materials.append(blade_material)

    bpy.ops.mesh.primitive_cube_add(size=1.0)
    guard = bpy.context.object
    guard.name = "PREVIEW_ONLY_SpectralGuard"
    guard.parent = root
    guard.location = Vector((0.0, 2.0, 0.0))
    guard.scale = Vector((15.0, 2.3, 2.0))
    guard.color = (0.65, 0.90, 1.0, 1.0)
    guard_material = bpy.data.materials.new("PREVIEW_ONLY_SpectralWhite")
    guard_material.diffuse_color = (0.58, 0.88, 1.0, 1.0)
    guard.data.materials.append(guard_material)

    for obj in (blade, guard):
        obj.hide_render = True
        obj.hide_viewport = True
        obj.keyframe_insert("hide_render", frame=max(1, spawn_frame - 1))
        obj.keyframe_insert("hide_viewport", frame=max(1, spawn_frame - 1))
        obj.hide_render = False
        obj.hide_viewport = False
        obj.keyframe_insert("hide_render", frame=spawn_frame)
        obj.keyframe_insert("hide_viewport", frame=spawn_frame)
        if obj.animation_data and obj.animation_data.action:
            for fcurve in obj.animation_data.action.fcurves:
                for keyframe in fcurve.keyframe_points:
                    keyframe.interpolation = "CONSTANT"

def sampled_frame_at_or_after(frame_30hz: int, output_end: int, asset_fps: int) -> int:
    if asset_fps <= 0 or 30 % asset_fps != 0:
        raise ValueError("Asset FPS must divide the 30 Hz preview rate")
    source_frames = locomotion.sampled_source_frames(output_end, 30, asset_fps)
    return next(
        (
            index
            for index, source_frame in enumerate(source_frames)
            if source_frame >= frame_30hz
        ),
        len(source_frames) - 1,
    )


def sampled_frame_at_or_before(frame_30hz: int, output_end: int, asset_fps: int) -> int:
    if asset_fps <= 0 or 30 % asset_fps != 0:
        raise ValueError("Asset FPS must divide the 30 Hz preview rate")
    source_frames = locomotion.sampled_source_frames(output_end, 30, asset_fps)
    indices = [
        index for index, source_frame in enumerate(source_frames) if source_frame <= frame_30hz
    ]
    return indices[-1] if indices else 0


def sampled_range(
    start_30hz: int, end_30hz: int, output_end: int, asset_fps: int
) -> list[int]:
    source_frames = locomotion.sampled_source_frames(output_end, 30, asset_fps)
    indices = [
        index
        for index, source_frame in enumerate(source_frames)
        if start_30hz <= source_frame <= end_30hz
    ]
    if indices:
        return [indices[0], indices[-1]]
    nearest = sampled_frame_at_or_after(start_30hz, output_end, asset_fps)
    return [nearest, nearest]


def main() -> None:
    args = parse_args()
    if not args.target.is_file() or not args.motion.is_file():
        raise FileNotFoundError("Target or motion source does not exist")
    if min(
        args.anticipation_frames,
        args.spawn_hold_frames,
        args.release_frames,
        args.recovery_frames,
        args.hit_count,
    ) < 0:
        raise ValueError("Attack phase durations cannot be negative")

    bpy.ops.wm.read_factory_settings(use_empty=True)
    target = locomotion.import_target(args.target)
    source, source_action = import_source(args.motion)
    mapping = locomotion.compatible_bones(source, target)
    action, splits, events, used_source_range, motion_peaks = build_action(
        source, target, source_action, mapping, args
    )

    locomotion.configure_render(target, source, args.output)
    if not args.raw_source:
        add_preview_weapon(target, events["spawn_weapon"])
    locomotion.activate_action(target, action)
    bpy.context.scene.frame_start = int(action.frame_range[0])
    bpy.context.scene.frame_end = int(action.frame_range[1])
    bpy.context.scene.render.filepath = str(args.output)
    bpy.context.scene.frame_set(1)
    bpy.ops.render.render(animation=True)

    if args.fbx_output:
        locomotion.configure_export_stack(target, action)
        locomotion.export_animation(target, args.fbx_output)
    if args.blend_output:
        args.blend_output.parent.mkdir(parents=True, exist_ok=True)
        bpy.ops.wm.save_as_mainfile(filepath=str(args.blend_output))

    output_end = int(action.frame_range[1])
    source_output_start = splits.get("release_to_source", splits["source_attack"])[0]
    proposed_active_windows = []
    for peak in motion_peaks:
        output_window = [
            source_output_start
            + int(peak["source_active_start"])
            - used_source_range[0],
            source_output_start
            + int(peak["source_active_end"])
            - used_source_range[0],
        ]
        proposed_active_windows.append(
            {
                **peak,
                "output_30hz": output_window,
                "asset_15hz_zero_based": sampled_range(
                    output_window[0], output_window[1], output_end, args.asset_fps
                ),
            }
        )
    metadata = {
        "target": str(args.target),
        "motion": str(args.motion),
        "source_action": source_action.name,
        "source_frame_range": [
            int(math.floor(source_action.frame_range[0])),
            int(math.ceil(source_action.frame_range[1])),
        ],
        "used_source_frame_range": list(used_source_range),
        "preview_fps": 30,
        "asset_fps": args.asset_fps,
        "output_frame_range": [1, output_end],
        "splits_30hz": splits,
        "events_30hz": events,
        "events_asset_frames_zero_based": {
            "spawn_weapon": sampled_frame_at_or_after(
                events["spawn_weapon"], output_end, args.asset_fps
            ),
            "attack_motion_start": sampled_frame_at_or_after(
                events["attack_motion_start"], output_end, args.asset_fps
            ),
            "attack_motion_end": sampled_frame_at_or_before(
                events["attack_motion_end"], output_end, args.asset_fps
            ),
            "return_to_idle_start": sampled_frame_at_or_after(
                events["return_to_idle_start"], output_end, args.asset_fps
            ),
        },
        "splits_asset_frames_zero_based": {
            name: sampled_range(start, end, output_end, args.asset_fps)
            for name, (start, end) in splits.items()
        },
        "proposed_active_windows": proposed_active_windows,
        "preview_weapon": "visible in MP4 only; not exported in the animation FBX",
        "horizontal_root_motion": "removed; gameplay owns displacement",
    }
    if args.metadata:
        args.metadata.parent.mkdir(parents=True, exist_ok=True)
        args.metadata.write_text(json.dumps(metadata, indent=2) + "\n")
    print("ATTACK_STUDY", json.dumps(metadata, sort_keys=True))
    print(f"ATTACK_STUDY output={args.output}")


if __name__ == "__main__":
    main()
