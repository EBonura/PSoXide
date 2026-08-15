#!/usr/bin/env python3
"""Build a contact-aligned windup/cruise/winddown locomotion study.

Run through Blender, for example:

    blender --background --factory-startup --python tools/locomotion_study.py -- \
        --target /path/to/character.fbx \
        --motion /path/to/walk.fbx \
        --output /path/to/walk-study.mp4 \
        --metadata /path/to/walk-study.json

The target and motion FBXs must use the same Mixamo rest skeleton. The generated
clip is deliberately in place: gameplay owns horizontal translation, while the
authored motion supplies stride phase, weight transfer, and vertical body motion.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import bpy
from mathutils import Matrix, Quaternion, Vector


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, type=Path)
    parser.add_argument("--motion", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--metadata", type=Path)
    parser.add_argument("--blend-output", type=Path)
    parser.add_argument("--fbx-output", type=Path)
    parser.add_argument("--action-name", default="locomotion_study")
    parser.add_argument(
        "--idle-output",
        type=Path,
        help="Optional MP4 preview of the newly generated idle loop",
    )
    parser.add_argument(
        "--idle-fbx-output",
        type=Path,
        help="Optional animation-only FBX containing the generated idle loop",
    )
    parser.add_argument("--idle-action-name", default="generated_idle")
    parser.add_argument(
        "--idle-frames",
        type=int,
        default=180,
        help="Unique 30 Hz frames in the seamless generated idle loop",
    )
    parser.add_argument(
        "--source-action",
        help="Case-insensitive substring selecting one take when the FBX contains several",
    )
    parser.add_argument("--windup-frames", type=int, default=12)
    parser.add_argument("--cruise-cycles", type=int, default=3)
    parser.add_argument("--winddown-frames", type=int, default=12)
    parser.add_argument(
        "--asset-fps",
        type=int,
        default=15,
        help="Cooked animation rate used to report surviving split frames",
    )
    return parser.parse_args(argv)


def strip_namespace(name: str) -> str:
    return name.rsplit(":", 1)[-1]


def armatures_added_since(before: set[str]) -> list[bpy.types.Object]:
    return [obj for obj in bpy.data.objects if obj.name not in before and obj.type == "ARMATURE"]


def actions_added_since(before: set[str]) -> list[bpy.types.Action]:
    return [action for action in bpy.data.actions if action.name not in before]


def import_target(path: Path) -> bpy.types.Object:
    before = {obj.name for obj in bpy.data.objects}
    result = bpy.ops.import_scene.fbx(filepath=str(path), use_anim=False)
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import target FBX: {path}")
    armatures = armatures_added_since(before)
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one target armature, found {len(armatures)}")
    armatures[0].name = "Locomotion_Target_Armature"
    return armatures[0]


def import_motion(
    path: Path, source_action_selector: str | None
) -> tuple[bpy.types.Object, bpy.types.Action]:
    objects_before = {obj.name for obj in bpy.data.objects}
    actions_before = {action.name for action in bpy.data.actions}
    result = bpy.ops.import_scene.fbx(filepath=str(path))
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import motion FBX: {path}")
    armatures = armatures_added_since(objects_before)
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one motion armature, found {len(armatures)}")
    actions = actions_added_since(actions_before)
    if not actions:
        raise RuntimeError(f"Motion FBX contains no actions: {path}")
    if source_action_selector:
        lowered = source_action_selector.lower()
        matches = [candidate for candidate in actions if lowered in candidate.name.lower()]
        if len(matches) != 1:
            raise RuntimeError(
                f"Source-action selector {source_action_selector!r} matched {len(matches)} takes"
            )
        action = matches[0]
    else:
        # Animation-only Mixamo downloads contain one take. When a carrier FBX
        # also contains legacy takes, callers should pass --source-action.
        action = max(actions, key=lambda candidate: candidate.frame_range.length)
    armature = armatures[0]
    armature.name = "Locomotion_Source_Armature"
    if armature.animation_data is None:
        armature.animation_data_create()
    for track in armature.animation_data.nla_tracks:
        track.mute = True
    armature.animation_data.action = action
    return armature, action


def compatible_bones(
    source: bpy.types.Object, target: bpy.types.Object
) -> list[tuple[str, str]]:
    target_lookup = {strip_namespace(bone.name).lower(): bone.name for bone in target.data.bones}
    mapping: list[tuple[str, str]] = []
    rest_failures: list[str] = []
    for source_bone in source.data.bones:
        target_name = target_lookup.get(strip_namespace(source_bone.name).lower())
        if target_name is None:
            continue
        target_bone = target.data.bones[target_name]
        translation_error = (
            source_bone.matrix_local.translation - target_bone.matrix_local.translation
        ).length
        rotation_error = source_bone.matrix_local.to_quaternion().rotation_difference(
            target_bone.matrix_local.to_quaternion()
        ).angle
        if translation_error > 1.0e-3 or rotation_error > math.radians(0.1):
            rest_failures.append(strip_namespace(source_bone.name))
            continue
        mapping.append((source_bone.name, target_name))
    if rest_failures:
        raise RuntimeError(
            "Motion and target do not share the same Mixamo rest skeleton: "
            + ", ".join(rest_failures)
        )
    if len(mapping) < 20:
        raise RuntimeError(f"Only {len(mapping)} compatible bones were found")
    return mapping


def set_source_frame(scene: bpy.types.Scene, frame: float) -> None:
    whole = math.floor(frame)
    scene.frame_set(whole, subframe=frame - whole)
    bpy.context.view_layer.update()


def foot_samples(
    source: bpy.types.Object, action: bpy.types.Action, frame_start: int, unique_frames: int
) -> tuple[dict[str, list[Vector]], str, int]:
    lookup = {strip_namespace(bone.name).lower(): bone.name for bone in source.data.bones}
    feet = {
        "left": lookup.get("lefttoebase", lookup["leftfoot"]),
        "right": lookup.get("righttoebase", lookup["rightfoot"]),
    }
    scene = bpy.context.scene
    samples: dict[str, list[Vector]] = {side: [] for side in feet}
    for offset in range(unique_frames):
        set_source_frame(scene, frame_start + offset)
        for side, bone_name in feet.items():
            samples[side].append(source.matrix_world @ source.pose.bones[bone_name].head)

    height_values = [position.z for positions in samples.values() for position in positions]
    height_span = max(max(height_values) - min(height_values), 1.0e-6)
    speed_values: list[float] = []
    speeds: dict[str, list[float]] = {side: [] for side in feet}
    for side, positions in samples.items():
        for index in range(unique_frames):
            previous = positions[(index - 1) % unique_frames]
            following = positions[(index + 1) % unique_frames]
            speed = (following - previous).length * 0.5
            speeds[side].append(speed)
            speed_values.append(speed)
    speed_span = max(max(speed_values) - min(speed_values), 1.0e-6)

    best_side = "left"
    best_offset = 0
    best_score = float("inf")
    minimum_height = min(height_values)
    minimum_speed = min(speed_values)
    for side, positions in samples.items():
        for offset, position in enumerate(positions):
            height_score = (position.z - minimum_height) / height_span
            speed_score = (speeds[side][offset] - minimum_speed) / speed_span
            score = height_score + speed_score * 0.45
            if score < best_score:
                best_score = score
                best_side = side
                best_offset = offset
    return samples, best_side, frame_start + best_offset


def smoothstep(value: float) -> float:
    value = max(0.0, min(1.0, value))
    return value * value * (3.0 - 2.0 * value)


def wrapped_source_frame(frame_start: int, unique_frames: int, phase: float) -> float:
    return frame_start + (phase % unique_frames)


def bone_lookup(armature: bpy.types.Object) -> dict[str, str]:
    return {
        strip_namespace(bone.name).lower(): bone.name for bone in armature.data.bones
    }


def reset_pose(armature: bpy.types.Object) -> None:
    for pose_bone in armature.pose.bones:
        pose_bone.rotation_mode = "QUATERNION"
        pose_bone.matrix_basis = Matrix.Identity(4)
    bpy.context.view_layer.update()


def set_bone_direction(
    armature: bpy.types.Object,
    bone_name: str,
    desired_direction: Vector,
) -> None:
    """Aim a bone in armature space while preserving its authored rest roll."""

    rest_bone = armature.data.bones[bone_name]
    pose_bone = armature.pose.bones[bone_name]
    rest_direction = (rest_bone.tail_local - rest_bone.head_local).normalized()
    desired_direction = desired_direction.normalized()
    direction_alignment = rest_direction.rotation_difference(desired_direction)
    desired_rotation = direction_alignment @ rest_bone.matrix_local.to_quaternion()
    desired_matrix = desired_rotation.to_matrix().to_4x4()
    desired_matrix.translation = pose_bone.matrix.translation
    pose_bone.matrix = desired_matrix
    bpy.context.view_layer.update()


def set_bone_rest_rotation(
    armature: bpy.types.Object,
    bone_name: str,
    armature_space_delta: Quaternion,
) -> None:
    """Rotate a rest bone in armature space, including twist around its axis."""

    rest_bone = armature.data.bones[bone_name]
    pose_bone = armature.pose.bones[bone_name]
    desired_rotation = armature_space_delta @ rest_bone.matrix_local.to_quaternion()
    desired_matrix = desired_rotation.to_matrix().to_4x4()
    desired_matrix.translation = pose_bone.matrix.translation
    pose_bone.matrix = desired_matrix
    bpy.context.view_layer.update()


def periodic_bump(phase: float, center: float, half_width: float) -> float:
    """A seamless, zero-velocity event envelope on the unit phase circle."""

    distance = abs(((phase - center + 0.5) % 1.0) - 0.5)
    if distance >= half_width:
        return 0.0
    return 0.5 + 0.5 * math.cos(math.pi * distance / half_width)


def periodic_hold(
    phase: float,
    center: float,
    half_width: float,
    feather: float,
) -> float:
    """A seamless event with eased turns and a readable held pose."""

    distance = abs(((phase - center + 0.5) % 1.0) - 0.5)
    if distance >= half_width:
        return 0.0
    hold_edge = max(0.0, half_width - feather)
    if distance <= hold_edge:
        return 1.0
    return smoothstep((half_width - distance) / max(feather, 1.0e-6))


def apply_generated_idle_pose(target: bpy.types.Object, phase: float) -> None:
    """Author a human, combat-ready idle directly on the target skeleton.

    This intentionally does not sample or retarget an external idle clip.  The
    six-second loop has readable behavioural beats: two breaths, a full weight
    transfer, a shoulder/arm reset, and a deliberate three-part environmental
    scan.  Every driver is periodic, so the action returns to its first pose
    and velocity seamlessly.
    """

    reset_pose(target)
    lookup = bone_lookup(target)
    tau = math.tau
    weight = math.sin(tau * phase)
    weight_follow = math.sin(tau * phase - 0.42)
    breath = math.sin(tau * phase * 2.0)
    breath_secondary = math.sin(tau * phase * 2.0 - 0.55)
    settle = periodic_bump(phase, 0.48, 0.12)
    shoulder_reset = periodic_bump(phase, 0.43, 0.13)
    arm_adjust = periodic_bump(phase, 0.82, 0.11)
    # Held scans are much more readable than momentary sinusoidal glances on a
    # faceless low-poly head: turn left, cross centre, inspect right for longer,
    # then lift the chin briefly before the loop settles.
    glance_left = periodic_hold(phase, 0.19, 0.14, 0.05)
    glance_right = periodic_hold(phase, 0.62, 0.18, 0.055)
    glance_up = periodic_hold(phase, 0.88, 0.07, 0.032)
    head_yaw = glance_left * 27.0 - glance_right * 34.0 + weight_follow * 1.2
    head_pitch = glance_left * 1.5 + glance_right * 4.0 - glance_up * 10.0
    head_roll = glance_left * 2.0 - glance_right * 3.0

    hips = target.pose.bones[lookup["hips"]]
    # This is oscillating pose motion, not root displacement: the pelvis always
    # returns to its origin and the character motor still owns world movement.
    hips_shift = weight * 4.0
    hips.location = Vector(
        (
            hips_shift,
            weight_follow * 0.72 + settle * 0.38,
            -1.35 + breath * 0.68 - abs(weight) * 0.30 - settle * 0.85,
        )
    )
    bpy.context.view_layer.update()

    world_x = Vector((1.0, 0.0, 0.0))
    world_y = Vector((0.0, 1.0, 0.0))
    world_z = Vector((0.0, 0.0, 1.0))
    torso_pitch = math.radians(
        3.8 + breath * 1.25 + settle * 2.0 - shoulder_reset * 0.75
    )
    torso_roll = math.radians(-weight * 4.0 + breath_secondary * 0.28)
    torso_twist = math.radians(
        weight_follow * 2.5
        + shoulder_reset * 4.0
        - arm_adjust * 2.4
        + glance_left * 5.0
        - glance_right * 8.0
    )
    torso_rotation = (
        Quaternion(world_z, torso_twist)
        @ Quaternion(world_y, torso_roll)
        @ Quaternion(world_x, torso_pitch)
    )

    for base_name, influence in (("spine", 0.35), ("spine1", 0.68), ("spine2", 1.0)):
        name = lookup[base_name]
        partial = Quaternion((1.0, 0.0, 0.0, 0.0)).slerp(torso_rotation, influence)
        set_bone_rest_rotation(target, name, partial)

    neck_name = lookup["neck"]
    head_name = lookup["head"]
    neck_follow = (
        Quaternion(world_z, math.radians(head_yaw * 0.55))
        @ Quaternion(world_y, math.radians(weight * 0.8 + head_roll * 0.45))
        @ Quaternion(
            world_x,
            math.radians(-breath * 0.65 - settle * 0.9 + head_pitch * 0.42),
        )
    )
    head_follow = (
        Quaternion(world_z, math.radians(head_yaw))
        @ Quaternion(world_y, math.radians(weight * 1.1 + head_roll))
        @ Quaternion(
            world_x,
            math.radians(-breath * 1.1 - settle * 1.35 + head_pitch),
        )
    )
    set_bone_rest_rotation(target, neck_name, neck_follow)
    set_bone_rest_rotation(target, head_name, head_follow)

    # The arms lag behind the torso, then visibly resettle at different moments
    # so the body does not move as one symmetrical block.
    arm_swing = weight_follow * 0.10
    shoulder_open = shoulder_reset * 0.11
    left_arm = Vector(
        (
            0.19 + shoulder_open,
            -0.08 - arm_swing - shoulder_reset * 0.12,
            -0.979 + shoulder_reset * 0.045,
        )
    )
    right_arm = Vector(
        (
            -0.19 - shoulder_open * 0.72 - arm_adjust * 0.12,
            -0.08 + arm_swing - arm_adjust * 0.15,
            -0.979 + arm_adjust * 0.065,
        )
    )
    left_forearm = Vector(
        (
            0.052 + shoulder_reset * 0.035,
            -0.17 - breath_secondary * 0.035 - shoulder_reset * 0.30,
            -0.983 + shoulder_reset * 0.075,
        )
    )
    right_forearm = Vector(
        (
            -0.052 + arm_adjust * 0.34,
            -0.17 + breath_secondary * 0.03 - arm_adjust * 0.26,
            -0.983 + arm_adjust * 0.095,
        )
    )
    left_hand = Vector(
        (0.028, -0.12 - shoulder_reset * 0.18 - breath * 0.02, -0.992)
    )
    right_hand = Vector(
        (-0.028 + arm_adjust * 0.18, -0.12 - arm_adjust * 0.18 + breath * 0.018, -0.992)
    )
    for base_name, direction in (
        ("leftarm", torso_rotation @ left_arm),
        ("rightarm", torso_rotation @ right_arm),
        ("leftforearm", torso_rotation @ left_forearm),
        ("rightforearm", torso_rotation @ right_forearm),
        ("lefthand", torso_rotation @ left_hand),
        ("righthand", torso_rotation @ right_hand),
    ):
        set_bone_direction(target, lookup[base_name], direction)

    # Counter-lean the legs under the oscillating pelvis so the feet remain
    # visually planted while the knees alternately take and release weight.
    counter_lean = -hips_shift / 94.0
    left_knee = math.radians(2.5 - weight * 1.0 + settle * 1.35)
    right_knee = math.radians(2.5 + weight * 1.0 + settle * 0.9)
    left_thigh = Quaternion(world_x, left_knee) @ Vector(
        (0.004 + counter_lean * 0.72, 0.003, -1.0)
    )
    right_thigh = Quaternion(world_x, right_knee) @ Vector(
        (-0.004 + counter_lean * 0.72, 0.003, -1.0)
    )
    left_shin = Quaternion(world_x, -left_knee * 1.85) @ Vector(
        (-0.013 + counter_lean * 0.28, 0.031, -0.999)
    )
    right_shin = Quaternion(world_x, -right_knee * 1.85) @ Vector(
        (0.013 + counter_lean * 0.28, 0.031, -0.999)
    )
    for base_name, direction in (
        ("leftupleg", left_thigh),
        ("rightupleg", right_thigh),
        ("leftleg", left_shin),
        ("rightleg", right_shin),
    ):
        set_bone_direction(target, lookup[base_name], direction)


def capture_basis_pose(
    target: bpy.types.Object,
) -> dict[str, tuple[Vector, Quaternion, Vector]]:
    captured: dict[str, tuple[Vector, Quaternion, Vector]] = {}
    for pose_bone in target.pose.bones:
        location, rotation, scale = pose_bone.matrix_basis.decompose()
        captured[pose_bone.name] = (location.copy(), rotation.copy(), scale.copy())
    return captured


def build_generated_idle_action(
    target: bpy.types.Object,
    action_name: str,
    unique_frames: int,
) -> tuple[bpy.types.Action, dict[str, tuple[Vector, Quaternion, Vector]]]:
    if target.animation_data is None:
        target.animation_data_create()
    action = bpy.data.actions.new(action_name)
    # The locomotion action becomes active again before the diagnostic .blend
    # is saved.  Keep the standalone generated idle available in that source
    # file instead of letting Blender discard it as an unused datablock.
    action.use_fake_user = True
    target.animation_data.action = action
    transition_pose: dict[str, tuple[Vector, Quaternion, Vector]] | None = None
    for frame_index in range(unique_frames):
        phase = frame_index / unique_frames
        apply_generated_idle_pose(target, phase)
        if frame_index == 0:
            transition_pose = capture_basis_pose(target)
        output_frame = frame_index + 1
        for pose_bone in target.pose.bones:
            pose_bone.keyframe_insert("location", frame=output_frame, group=pose_bone.name)
            pose_bone.keyframe_insert(
                "rotation_quaternion", frame=output_frame, group=pose_bone.name
            )
            pose_bone.keyframe_insert("scale", frame=output_frame, group=pose_bone.name)
    for fcurve in action.fcurves:
        for keyframe in fcurve.keyframe_points:
            keyframe.interpolation = "LINEAR"
    if transition_pose is None:
        raise RuntimeError("Generated idle produced no frames")
    return action, transition_pose


def build_study_action(
    source: bpy.types.Object,
    target: bpy.types.Object,
    source_action: bpy.types.Action,
    mapping: list[tuple[str, str]],
    contact_frame: int,
    action_name: str,
    windup_frames: int,
    cruise_cycles: int,
    winddown_frames: int,
    idle_pose: dict[str, tuple[Vector, Quaternion, Vector]],
) -> tuple[bpy.types.Action, dict[str, list[int]], int]:
    scene = bpy.context.scene
    source_start = int(source_action.frame_range[0])
    source_end = int(source_action.frame_range[1])
    unique_frames = source_end - source_start
    if unique_frames < 4:
        raise RuntimeError("Locomotion source is too short to contain a cycle")

    cruise_frames = unique_frames * cruise_cycles
    total_frames = windup_frames + cruise_frames + winddown_frames
    half_cycle = unique_frames * 0.5
    contact_phase = float(contact_frame - source_start)
    splits = {
        "windup": [1, windup_frames],
        "cruise": [windup_frames + 1, windup_frames + cruise_frames],
        "winddown": [windup_frames + cruise_frames + 1, total_frames],
    }

    if target.animation_data is None:
        target.animation_data_create()
    output_action = bpy.data.actions.new(action_name)
    target.animation_data.action = output_action
    identity_rotation = Quaternion((1.0, 0.0, 0.0, 0.0))
    identity_scale = Vector((1.0, 1.0, 1.0))
    target_hips = next(
        target_name for _, target_name in mapping if strip_namespace(target_name).lower() == "hips"
    )

    for output_index in range(total_frames):
        if output_index < windup_frames:
            progress = output_index / max(1, windup_frames - 1)
            envelope = smoothstep(progress)
            phase = contact_phase + half_cycle * envelope
        elif output_index < windup_frames + cruise_frames:
            cruise_index = output_index - windup_frames
            envelope = 1.0
            phase = contact_phase + half_cycle + cruise_index + 1
        else:
            winddown_index = output_index - windup_frames - cruise_frames
            progress = (winddown_index + 1) / max(1, winddown_frames)
            envelope = 1.0 - smoothstep(progress)
            phase = contact_phase + half_cycle + cruise_frames + half_cycle * smoothstep(progress)

        set_source_frame(
            scene, wrapped_source_frame(source_start, unique_frames, phase)
        )
        for pose_bone in target.pose.bones:
            pose_bone.matrix_basis = Matrix.Identity(4)
            pose_bone.rotation_mode = "QUATERNION"

        output_frame = output_index + 1
        for source_name, target_name in mapping:
            source_basis = source.pose.bones[source_name].matrix_basis.copy()
            location, rotation, scale = source_basis.decompose()
            if target_name == target_hips:
                # Runtime collision and movement own horizontal translation.
                location.x = 0.0
                location.y = 0.0
            target_bone = target.pose.bones[target_name]
            idle_location, idle_rotation, idle_scale = idle_pose.get(
                target_name,
                (Vector((0.0, 0.0, 0.0)), identity_rotation, identity_scale),
            )
            target_bone.location = idle_location.lerp(location, envelope)
            target_bone.rotation_quaternion = idle_rotation.slerp(rotation, envelope)
            target_bone.scale = idle_scale.lerp(scale, envelope)
            target_bone.keyframe_insert("location", frame=output_frame, group=target_name)
            target_bone.keyframe_insert(
                "rotation_quaternion", frame=output_frame, group=target_name
            )
            target_bone.keyframe_insert("scale", frame=output_frame, group=target_name)

    for fcurve in output_action.fcurves:
        for keyframe in fcurve.keyframe_points:
            keyframe.interpolation = "LINEAR"
    scene.frame_start = 1
    scene.frame_end = total_frames
    scene.frame_set(1)
    return output_action, splits, unique_frames


def configure_render(target: bpy.types.Object, source: bpy.types.Object, output: Path) -> None:
    scene = bpy.context.scene
    output.parent.mkdir(parents=True, exist_ok=True)
    source.hide_render = True
    source.hide_viewport = True
    for child in source.children_recursive:
        child.hide_render = True
        child.hide_viewport = True

    camera_data = bpy.data.cameras.new("Locomotion_Study_Camera")
    camera = bpy.data.objects.new("Locomotion_Study_Camera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.lens = 58.0

    lookup = {strip_namespace(bone.name).lower(): bone.name for bone in target.data.bones}
    hips_name = lookup["hips"]
    object_scale = sum(abs(component) for component in target.matrix_world.to_scale()) / 3.0
    scene.frame_set(1)
    hips = target.matrix_world @ target.pose.bones[hips_name].matrix.translation
    aim = bpy.data.objects.new("Locomotion_Study_Aim", None)
    bpy.context.collection.objects.link(aim)
    aim.location = hips + Vector((0.0, 0.0, 35.0)) * object_scale
    # Keep the full stride in frame while making small idle gestures legible in
    # a 512px review render.
    camera.location = hips + Vector((220.0, 330.0, 125.0)) * object_scale
    track = camera.constraints.new(type="TRACK_TO")
    track.target = aim
    track.track_axis = "TRACK_NEGATIVE_Z"
    track.up_axis = "UP_Y"

    bpy.ops.mesh.primitive_plane_add(size=3000.0 * object_scale, location=(0.0, 0.0, 0.0))
    ground = bpy.context.object
    ground.name = "Locomotion_Study_Ground"
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
    scene.render.fps = 30


def configure_export_stack(target: bpy.types.Object, action: bpy.types.Action) -> None:
    animation_data = target.animation_data
    if animation_data is None:
        raise RuntimeError("Target has no animation data")
    animation_data.action = None
    for track in list(animation_data.nla_tracks):
        animation_data.nla_tracks.remove(track)
    track = animation_data.nla_tracks.new()
    track.name = action.name
    track.strips.new(action.name, int(action.frame_range[0]), action)


def activate_action(target: bpy.types.Object, action: bpy.types.Action) -> None:
    animation_data = target.animation_data
    if animation_data is None:
        raise RuntimeError("Target has no animation data")
    animation_data.action = None
    for track in list(animation_data.nla_tracks):
        animation_data.nla_tracks.remove(track)
    animation_data.action = action


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
        raise RuntimeError(f"Could not export animation FBX: {output}")


def sampled_split_ranges(
    splits: dict[str, list[int]],
    output_end: int,
    preview_fps: int,
    asset_fps: int,
) -> dict[str, list[int]]:
    """Map authored ranges onto frames that survive fixed-rate cooking.

    PSoXide numbers cooked frames from zero. Locomotion split boundaries should
    be authored on this grid so the planted-contact pose is retained verbatim.
    """

    if asset_fps <= 0 or preview_fps % asset_fps != 0:
        raise ValueError("Preview FPS must be an integer multiple of asset FPS")
    step = preview_fps // asset_fps
    sampled_source_frames = list(range(1, output_end + 1, step))
    if sampled_source_frames[-1] != output_end:
        sampled_source_frames.append(output_end)
    sampled: dict[str, list[int]] = {}
    for phase, (start, end) in splits.items():
        indices = [
            index
            for index, source_frame in enumerate(sampled_source_frames)
            if start <= source_frame <= end
        ]
        if not indices:
            raise RuntimeError(f"{phase} contains no frame at {asset_fps} Hz")
        sampled[phase] = [indices[0], indices[-1]]
    return sampled


def sampled_source_frames(output_end: int, preview_fps: int, asset_fps: int) -> list[int]:
    step = preview_fps // asset_fps
    frames = list(range(1, output_end + 1, step))
    if frames[-1] != output_end:
        frames.append(output_end)
    return frames


def pose_rotations(target: bpy.types.Object, frame: int) -> dict[str, Quaternion]:
    bpy.context.scene.frame_set(frame)
    bpy.context.view_layer.update()
    return {
        pose_bone.name: pose_bone.matrix.to_quaternion().copy()
        for pose_bone in target.pose.bones
    }


def pose_rotation_rms_degrees(
    first: dict[str, Quaternion], second: dict[str, Quaternion]
) -> float:
    # Quaternion signs are interchangeable (q and -q encode the same
    # orientation), so use the absolute dot product to avoid false 360° jumps.
    angles = []
    for name in first.keys() & second.keys():
        dot = max(0.0, min(1.0, abs(first[name].dot(second[name]))))
        angles.append(2.0 * math.acos(dot))
    if not angles:
        return 0.0
    return math.degrees(math.sqrt(sum(angle * angle for angle in angles) / len(angles)))


def continuity_metrics(
    target: bpy.types.Object,
    asset_splits: dict[str, list[int]],
    output_end: int,
    preview_fps: int,
    asset_fps: int,
) -> dict[str, float]:
    sampled_frames = sampled_source_frames(output_end, preview_fps, asset_fps)
    poses = {frame: pose_rotations(target, frame) for frame in sampled_frames}

    def delta(first: int, second: int) -> float:
        return pose_rotation_rms_degrees(poses[first], poses[second])

    cruise_first_index, cruise_last_index = asset_splits["cruise"]
    cruise_indices = range(cruise_first_index, cruise_last_index + 1)
    cruise_deltas = [
        delta(sampled_frames[index - 1], sampled_frames[index])
        for index in cruise_indices
        if index > cruise_first_index
    ]
    typical = sorted(cruise_deltas)[len(cruise_deltas) // 2]
    windup_last = sampled_frames[asset_splits["windup"][1]]
    cruise_first = sampled_frames[cruise_first_index]
    cruise_last = sampled_frames[cruise_last_index]
    winddown_first = sampled_frames[asset_splits["winddown"][0]]
    epsilon = 1.0e-6
    start_join = delta(windup_last, cruise_first)
    loop_join = delta(cruise_last, cruise_first)
    stop_join = delta(cruise_last, winddown_first)
    return {
        "typical_cruise_step_rms_degrees": round(typical, 6),
        "windup_to_cruise_rms_degrees": round(start_join, 6),
        "cruise_loop_rms_degrees": round(loop_join, 6),
        "cruise_to_winddown_rms_degrees": round(stop_join, 6),
        "windup_join_vs_typical": round(start_join / max(typical, epsilon), 6),
        "loop_join_vs_typical": round(loop_join / max(typical, epsilon), 6),
        "winddown_join_vs_typical": round(stop_join / max(typical, epsilon), 6),
    }


def main() -> None:
    args = parse_args()
    for path in (args.target, args.motion):
        if not path.is_file():
            raise FileNotFoundError(path)
    if args.windup_frames < 2 or args.winddown_frames < 1 or args.cruise_cycles < 1:
        raise ValueError("Windup/winddown frames and cruise cycles must be positive")
    if args.idle_frames < 30:
        raise ValueError("Generated idle must contain at least 30 unique frames")
    if args.asset_fps <= 0:
        raise ValueError("Asset FPS must be positive")

    bpy.ops.wm.read_factory_settings(use_empty=True)
    target = import_target(args.target)
    source, source_action = import_motion(args.motion, args.source_action)
    mapping = compatible_bones(source, target)
    idle_action, idle_transition_pose = build_generated_idle_action(
        target,
        args.idle_action_name,
        args.idle_frames,
    )
    source_start = int(source_action.frame_range[0])
    source_end = int(source_action.frame_range[1])
    unique_frames = source_end - source_start
    _, contact_side, contact_frame = foot_samples(
        source, source_action, source_start, unique_frames
    )
    action, splits, unique_frames = build_study_action(
        source,
        target,
        source_action,
        mapping,
        contact_frame,
        args.action_name,
        args.windup_frames,
        args.cruise_cycles,
        args.winddown_frames,
        idle_transition_pose,
    )

    configure_render(target, source, args.output)
    activate_action(target, action)
    bpy.context.scene.frame_start = int(action.frame_range[0])
    bpy.context.scene.frame_end = int(action.frame_range[1])
    bpy.context.scene.render.filepath = str(args.output)
    bpy.context.scene.frame_set(1)
    bpy.ops.render.render(animation=True)
    if args.fbx_output:
        configure_export_stack(target, action)
        export_animation(target, args.fbx_output)

    if args.idle_output:
        args.idle_output.parent.mkdir(parents=True, exist_ok=True)
        activate_action(target, idle_action)
        bpy.context.scene.frame_start = int(idle_action.frame_range[0])
        bpy.context.scene.frame_end = int(idle_action.frame_range[1])
        bpy.context.scene.render.filepath = str(args.idle_output)
        bpy.context.scene.frame_set(1)
        bpy.ops.render.render(animation=True)
    if args.idle_fbx_output:
        configure_export_stack(target, idle_action)
        export_animation(target, args.idle_fbx_output)

    activate_action(target, action)
    bpy.context.scene.frame_start = int(action.frame_range[0])
    bpy.context.scene.frame_end = int(action.frame_range[1])
    bpy.context.scene.frame_set(1)

    preview_fps = bpy.context.scene.render.fps
    asset_splits = sampled_split_ranges(
        splits,
        bpy.context.scene.frame_end,
        preview_fps,
        args.asset_fps,
    )
    continuity = continuity_metrics(
        target,
        asset_splits,
        bpy.context.scene.frame_end,
        preview_fps,
        args.asset_fps,
    )
    metadata = {
        "target": str(args.target),
        "motion": str(args.motion),
        "source_action": source_action.name,
        "source_frame_range": [source_start, source_end],
        "source_unique_cycle_frames": unique_frames,
        "canonical_contact": {"foot": contact_side, "frame": contact_frame},
        "preview_fps": preview_fps,
        "asset_fps": args.asset_fps,
        "output_frame_range": [1, bpy.context.scene.frame_end],
        "splits": splits,
        "asset_splits": asset_splits,
        "continuity": continuity,
        "horizontal_root_motion": "removed from gait; idle pelvis sway is periodic with zero net displacement",
        "generated_idle": {
            "action": idle_action.name,
            "frame_range": [
                int(idle_action.frame_range[0]),
                int(idle_action.frame_range[1]),
            ],
            "unique_frames": args.idle_frames,
            "external_motion_source": None,
            "style": "dynamic alert idle with breathing, full weight transfer, independent fidgets, and held environmental scans",
            "beats": [
                "shift weight across both legs",
                "reset shoulders and left arm",
                "settle through the knees",
                "adjust right arm",
                "turn and hold 27 degrees left",
                "cross centre and hold 34 degrees right with torso follow",
                "return forward with a short chin-up check",
            ],
        },
    }
    if args.metadata:
        args.metadata.parent.mkdir(parents=True, exist_ok=True)
        args.metadata.write_text(json.dumps(metadata, indent=2) + "\n")
    if args.blend_output:
        args.blend_output.parent.mkdir(parents=True, exist_ok=True)
        bpy.ops.wm.save_as_mainfile(filepath=str(args.blend_output))
    print("LOCOMOTION_STUDY", json.dumps(metadata, sort_keys=True))
    print(f"LOCOMOTION_STUDY output={args.output}")


if __name__ == "__main__":
    main()
