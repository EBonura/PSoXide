#!/usr/bin/env python3
"""Retarget a Kimodo SOMA BVH clip onto a Mixamo-style FBX armature.

Run this script through Blender, for example:

    blender --background --factory-startup --python tools/kimodo_retarget.py -- \
        --target /path/to/character.fbx \
        --motion /path/to/kimodo_motion.bvh \
        --output /path/to/animation_only.fbx \
        --action-name kimodo_run_leap

The exported FBX contains the target armature and its baked action, but no mesh.
This makes it suitable for PSoXide's ``glb-model --animation`` input.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import bpy
from mathutils import Matrix, Vector


SOMA_TO_MIXAMO = {
    "Hips": "Hips",
    "Spine1": "Spine",
    "Spine2": "Spine1",
    "Chest": "Spine2",
    "Neck2": "Neck",
    "Head": "Head",
    "LeftShoulder": "LeftShoulder",
    "LeftArm": "LeftArm",
    "LeftForeArm": "LeftForeArm",
    "LeftHand": "LeftHand",
    "LeftHandThumb1": "LeftHandThumb1",
    "LeftHandThumb2": "LeftHandThumb2",
    "LeftHandIndex1": "LeftHandIndex1",
    "LeftHandIndex2": "LeftHandIndex2",
    "RightShoulder": "RightShoulder",
    "RightArm": "RightArm",
    "RightForeArm": "RightForeArm",
    "RightHand": "RightHand",
    "RightHandThumb1": "RightHandThumb1",
    "RightHandThumb2": "RightHandThumb2",
    "RightHandIndex1": "RightHandIndex1",
    "RightHandIndex2": "RightHandIndex2",
    "LeftLeg": "LeftUpLeg",
    "LeftShin": "LeftLeg",
    "LeftFoot": "LeftFoot",
    "LeftToeBase": "LeftToeBase",
    "RightLeg": "RightUpLeg",
    "RightShin": "RightLeg",
    "RightFoot": "RightFoot",
    "RightToeBase": "RightToeBase",
}


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, type=Path, help="Mixamo-style target FBX")
    parser.add_argument("--motion", required=True, type=Path, help="Kimodo standard-T-pose BVH")
    parser.add_argument("--output", required=True, type=Path, help="Animation-only FBX output")
    parser.add_argument("--action-name", default="kimodo_motion")
    parser.add_argument("--blend-output", type=Path, help="Optional diagnostic .blend file")
    parser.add_argument("--preview", type=Path, help="Optional 512px MP4 preview")
    return parser.parse_args(argv)


def armatures_added_since(before: set[str]) -> list[bpy.types.Object]:
    return [obj for obj in bpy.data.objects if obj.name not in before and obj.type == "ARMATURE"]


def strip_namespace(name: str) -> str:
    return name.rsplit(":", 1)[-1]


def target_bone_lookup(armature: bpy.types.Object) -> dict[str, str]:
    lookup: dict[str, str] = {}
    for bone in armature.data.bones:
        lookup.setdefault(strip_namespace(bone.name).lower(), bone.name)
    return lookup


def hierarchy_depth(bone: bpy.types.Bone) -> int:
    depth = 0
    parent = bone.parent
    while parent is not None:
        depth += 1
        parent = parent.parent
    return depth


def rest_distance(armature: bpy.types.Object, first: str, second: str) -> float:
    a = armature.data.bones[first].matrix_local.translation
    b = armature.data.bones[second].matrix_local.translation
    return (a - b).length


def absolute_pose_roll_offset(source_bone: bpy.types.Bone, target_bone: bpy.types.Bone):
    """Return target roll transported onto the source bone's rest direction.

    Applying a source rest-to-pose delta directly to a target with a different
    bind pose double-applies the rest-pose difference. In particular, SOMA is
    T-posed while the CI Player Mixamo rig has relaxed, downward-pointing arms.
    Instead, align the target rest direction to the source rest direction and
    retain only the target bone's roll around its local Y axis. The returned
    local offset can then be post-multiplied onto every absolute source pose.
    """

    source_rest = source_bone.matrix_local.to_quaternion()
    target_rest = target_bone.matrix_local.to_quaternion()
    source_direction = (source_bone.tail_local - source_bone.head_local).normalized()
    target_direction = (target_bone.tail_local - target_bone.head_local).normalized()
    direction_alignment = target_direction.rotation_difference(source_direction)
    aligned_target_rest = direction_alignment @ target_rest
    return source_rest.inverted() @ aligned_target_rest


def rest_delta_pose_rotation(
    source_pose_bone: bpy.types.PoseBone,
    source_rest_bone: bpy.types.Bone,
    target_rest_bone: bpy.types.Bone,
):
    """Transfer a pose delta without interpreting a branching bone's tail."""

    source_pose = source_pose_bone.matrix.to_quaternion()
    source_rest = source_rest_bone.matrix_local.to_quaternion()
    target_rest = target_rest_bone.matrix_local.to_quaternion()
    return source_pose @ source_rest.inverted() @ target_rest


def import_target(path: Path) -> bpy.types.Object:
    before = {obj.name for obj in bpy.data.objects}
    result = bpy.ops.import_scene.fbx(filepath=str(path), use_anim=False)
    if "FINISHED" not in result:
        raise RuntimeError(f"Blender could not import target FBX: {path}")
    armatures = armatures_added_since(before)
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one armature in {path}, found {len(armatures)}")
    armature = armatures[0]
    armature.name = "PSX_Target_Armature"
    return armature


def import_motion(path: Path) -> bpy.types.Object:
    before = {obj.name for obj in bpy.data.objects}
    result = bpy.ops.import_anim.bvh(
        filepath=str(path),
        target="ARMATURE",
        global_scale=0.01,
        frame_start=1,
        use_fps_scale=False,
        update_scene_fps=True,
        update_scene_duration=True,
        rotate_mode="QUATERNION",
        axis_forward="-Z",
        axis_up="Y",
    )
    if "FINISHED" not in result:
        raise RuntimeError(f"Blender could not import Kimodo BVH: {path}")
    armatures = armatures_added_since(before)
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one armature in {path}, found {len(armatures)}")
    armature = armatures[0]
    armature.name = "Kimodo_Source_Armature"
    return armature


def build_mapping(source: bpy.types.Object, target: bpy.types.Object) -> list[tuple[str, str]]:
    target_lookup = target_bone_lookup(target)
    mapping: list[tuple[str, str]] = []
    missing_source: list[str] = []
    missing_target: list[str] = []
    for source_name, target_base in SOMA_TO_MIXAMO.items():
        if source_name not in source.data.bones:
            missing_source.append(source_name)
            continue
        target_name = target_lookup.get(target_base.lower())
        if target_name is None:
            missing_target.append(target_base)
            continue
        mapping.append((source_name, target_name))

    if missing_source:
        print("KIMODO_RETARGET missing source bones:", ", ".join(missing_source))
    if missing_target:
        print("KIMODO_RETARGET missing target bones:", ", ".join(missing_target))
    if len(mapping) < 20:
        raise RuntimeError(f"Only {len(mapping)} bones mapped; target is not a compatible humanoid rig")

    mapping.sort(key=lambda pair: hierarchy_depth(target.data.bones[pair[1]]))
    return mapping


def retarget(source: bpy.types.Object, target: bpy.types.Object, action_name: str) -> bpy.types.Action:
    mapping = build_mapping(source, target)
    rotation_offsets = {
        (source_name, target_name): absolute_pose_roll_offset(
            source.data.bones[source_name], target.data.bones[target_name]
        )
        for source_name, target_name in mapping
    }
    source_action = source.animation_data.action if source.animation_data else None
    if source_action is None:
        raise RuntimeError("Imported BVH has no action")

    frame_start = int(source_action.frame_range[0])
    frame_end = int(source_action.frame_range[1])
    scene = bpy.context.scene
    scene.frame_start = frame_start
    scene.frame_end = frame_end

    if target.animation_data is None:
        target.animation_data_create()
    action = bpy.data.actions.new(action_name)
    target.animation_data.action = action

    target_lookup = target_bone_lookup(target)
    target_hips = target_lookup["hips"]
    target_left_foot = target_lookup["leftfoot"]
    source_scale = rest_distance(source, "Hips", "LeftFoot")
    target_scale = rest_distance(target, target_hips, target_left_foot)
    translation_scale = target_scale / source_scale if source_scale > 1.0e-8 else 1.0

    source_hips_rest = source.data.bones["Hips"].matrix_local.translation.copy()
    target_hips_rest = target.data.bones[target_hips].matrix_local.translation.copy()

    for pose_bone in target.pose.bones:
        pose_bone.rotation_mode = "QUATERNION"
        pose_bone.matrix_basis = Matrix.Identity(4)

    for frame in range(frame_start, frame_end + 1):
        scene.frame_set(frame)
        for pose_bone in target.pose.bones:
            pose_bone.matrix_basis = Matrix.Identity(4)
        bpy.context.view_layer.update()

        for source_name, target_name in mapping:
            source_bone = source.pose.bones[source_name]
            target_bone = target.pose.bones[target_name]
            if source_name == "Hips":
                # The BVH importer gives this branching joint a display tail
                # toward one of its leg children. That tail is not a semantic
                # torso direction, so absolute direction transfer would flip
                # the target pelvis and put the spine below the hips.
                desired_rotation = rest_delta_pose_rotation(
                    source_bone,
                    source.data.bones[source_name],
                    target.data.bones[target_name],
                )
            else:
                source_pose_rotation = source_bone.matrix.to_quaternion()
                desired_rotation = source_pose_rotation @ rotation_offsets[(source_name, target_name)]

            desired_matrix = desired_rotation.to_matrix().to_4x4()
            desired_matrix.translation = target_bone.matrix.translation
            if source_name == "Hips":
                source_motion = source_bone.matrix.translation - source_hips_rest
                desired_matrix.translation = target_hips_rest + source_motion * translation_scale
            target_bone.matrix = desired_matrix
            bpy.context.view_layer.update()

            target_bone.keyframe_insert("rotation_quaternion", frame=frame, group=target_name)
            if source_name == "Hips":
                target_bone.keyframe_insert("location", frame=frame, group=target_name)

    for fcurve in action.fcurves:
        for keyframe in fcurve.keyframe_points:
            keyframe.interpolation = "LINEAR"

    validate_hand_sides(source, target, frame_start, frame_end)

    print(
        "KIMODO_RETARGET",
        f"mapped={len(mapping)}",
        f"frames={frame_start}-{frame_end}",
        f"fps={scene.render.fps}",
        f"translation_scale={translation_scale:.6f}",
    )
    print("KIMODO_RETARGET mapping:", ", ".join(f"{a}->{b}" for a, b in mapping))
    return action


def validate_hand_sides(
    source: bpy.types.Object,
    target: bpy.types.Object,
    frame_start: int,
    frame_end: int,
) -> None:
    """Catch mirrored/crossed-arm retarget regressions at representative frames."""

    scene = bpy.context.scene
    target_lookup = target_bone_lookup(target)
    target_left = target_lookup["lefthand"]
    target_right = target_lookup["righthand"]
    target_hips = target_lookup["hips"]
    target_spine = target_lookup["spine"]
    sample_count = min(11, frame_end - frame_start + 1)
    frames = {
        round(frame_start + index * (frame_end - frame_start) / max(1, sample_count - 1))
        for index in range(sample_count)
    }
    hand_mismatches: list[int] = []
    torso_mismatches: list[tuple[int, float]] = []
    for frame in sorted(frames):
        scene.frame_set(frame)
        bpy.context.view_layer.update()
        source_separation = (
            source.pose.bones["LeftHand"].matrix.translation.x
            - source.pose.bones["RightHand"].matrix.translation.x
        )
        target_separation = (
            target.pose.bones[target_left].matrix.translation.x
            - target.pose.bones[target_right].matrix.translation.x
        )
        if abs(source_separation) > 1.0e-4 and source_separation * target_separation < 0.0:
            hand_mismatches.append(frame)

        source_torso = source.pose.bones["Spine1"].head - source.pose.bones["Hips"].head
        target_torso = target.pose.bones[target_spine].head - target.pose.bones[target_hips].head
        torso_dot = source_torso.normalized().dot(target_torso.normalized())
        if torso_dot < 0.95:
            torso_mismatches.append((frame, torso_dot))

    if hand_mismatches:
        raise RuntimeError(f"Hand sides are mirrored at frames: {hand_mismatches}")
    if torso_mismatches:
        raise RuntimeError(f"Torso direction is inverted or divergent: {torso_mismatches}")
    print(f"KIMODO_RETARGET hand-side-check=pass samples={len(frames)}")
    print(f"KIMODO_RETARGET torso-direction-check=pass samples={len(frames)}")


def configure_export_stack(target: bpy.types.Object, action: bpy.types.Action) -> None:
    animation_data = target.animation_data
    if animation_data is None:
        raise RuntimeError("Target armature has no animation data")
    animation_data.action = None
    for track in list(animation_data.nla_tracks):
        animation_data.nla_tracks.remove(track)
    track = animation_data.nla_tracks.new()
    track.name = action.name
    track.strips.new(action.name, int(action.frame_range[0]), action)


def export_animation(target: bpy.types.Object, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    bpy.ops.object.select_all(action="DESELECT")
    target.select_set(True)
    bpy.context.view_layer.objects.active = target
    result = bpy.ops.export_scene.fbx(
        filepath=str(output),
        use_selection=True,
        object_types={"ARMATURE"},
        use_armature_deform_only=False,
        add_leaf_bones=False,
        bake_anim=True,
        bake_anim_use_all_bones=True,
        bake_anim_use_nla_strips=True,
        bake_anim_use_all_actions=False,
        bake_anim_force_startend_keying=True,
        bake_anim_step=1.0,
        bake_anim_simplify_factor=0.0,
        axis_forward="-Z",
        axis_up="Y",
    )
    if "FINISHED" not in result:
        raise RuntimeError(f"Blender could not export animation FBX: {output}")


def render_preview(target: bpy.types.Object, source: bpy.types.Object, output: Path) -> None:
    scene = bpy.context.scene
    output.parent.mkdir(parents=True, exist_ok=True)
    source.hide_render = True
    source.hide_viewport = True

    camera_data = bpy.data.cameras.new("Kimodo_Preview_Camera")
    camera = bpy.data.objects.new("Kimodo_Preview_Camera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.lens = 52.0

    aim = bpy.data.objects.new("Kimodo_Preview_Aim", None)
    bpy.context.collection.objects.link(aim)
    track = camera.constraints.new(type="TRACK_TO")
    track.target = aim
    track.track_axis = "TRACK_NEGATIVE_Z"
    track.up_axis = "UP_Y"

    target_hips = target_bone_lookup(target)["hips"]
    object_scale = sum(abs(component) for component in target.matrix_world.to_scale()) / 3.0
    for frame in range(scene.frame_start, scene.frame_end + 1):
        scene.frame_set(frame)
        hips = target.matrix_world @ target.pose.bones[target_hips].matrix.translation
        aim.location = hips + Vector((0.0, 0.0, 35.0)) * object_scale
        camera.location = hips + Vector((280.0, 360.0, 145.0)) * object_scale
        aim.keyframe_insert("location", frame=frame)
        camera.keyframe_insert("location", frame=frame)

    for animated in (aim, camera):
        if animated.animation_data and animated.animation_data.action:
            for fcurve in animated.animation_data.action.fcurves:
                for keyframe in fcurve.keyframe_points:
                    keyframe.interpolation = "LINEAR"

    bpy.ops.mesh.primitive_plane_add(
        size=3000.0 * object_scale,
        location=(0.0, -450.0 * object_scale, 0.0),
    )
    ground = bpy.context.object
    ground.name = "Kimodo_Preview_Ground"
    ground.color = (0.055, 0.065, 0.085, 1.0)

    scene.render.engine = "BLENDER_WORKBENCH"
    scene.display.shading.light = "STUDIO"
    scene.display.shading.color_type = "MATERIAL"
    scene.display.shading.show_shadows = True
    scene.display.shading.show_cavity = True
    scene.display.shading.background_type = "WORLD"
    scene.display.shading.background_color = (0.025, 0.03, 0.045)
    scene.render.resolution_x = 512
    scene.render.resolution_y = 512
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "FFMPEG"
    scene.render.ffmpeg.format = "MPEG4"
    scene.render.ffmpeg.codec = "H264"
    scene.render.ffmpeg.constant_rate_factor = "MEDIUM"
    scene.render.filepath = str(output)
    scene.frame_set(scene.frame_start)
    bpy.ops.render.render(animation=True)
    print(f"KIMODO_RETARGET preview={output}")


def main() -> None:
    args = parse_args()
    for path in (args.target, args.motion):
        if not path.is_file():
            raise FileNotFoundError(path)

    bpy.ops.wm.read_factory_settings(use_empty=True)
    target = import_target(args.target)
    source = import_motion(args.motion)
    action = retarget(source, target, args.action_name)
    configure_export_stack(target, action)
    export_animation(target, args.output)
    if args.preview:
        render_preview(target, source, args.preview)
    if args.blend_output:
        args.blend_output.parent.mkdir(parents=True, exist_ok=True)
        bpy.ops.wm.save_as_mainfile(filepath=str(args.blend_output))
    print(f"KIMODO_RETARGET output={args.output}")


if __name__ == "__main__":
    main()
