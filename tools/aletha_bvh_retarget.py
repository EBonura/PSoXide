"""Retarget a MoMask / HumanML3D BVH (22-joint, Mixamo-named, T-pose rest)
onto the Aletha Delivered rig inside a running Blender session.

Used by aletha_walk_study.py (``--gait-bvh``); import-only, no CLI.

Method per joint (the lesson from the Kimodo bridge, tools/kimodo_retarget.py):
  * delta:      target_world = src_pose @ src_rest^-1 @ tgt_rest. Valid when both
                rests agree for that joint: hips, legs, feet (both rigs stand
                upright, legs straight, feet flat).
  * torso:      the source template's spine/neck/clavicle rest is a zig-zag
                (mid-spine 26 deg forward, neck 38 deg forward, clavicles 18 deg
                up), so per-bone deltas transplant that zig-zag onto Aletha as sag
                and lean. Instead the torso moves as one rigid frame: the rotation
                of the (spine base -> neck, shoulder line) frame from rest to pose is
                applied to Spine and Chest alike; the neck gets the aggregate
                (neck -> head) direction change; clavicles stay unmapped.
  * direction:  aim the target bone at the source bone's world direction, keep the
                target's own rest roll. Needed for the arms: the source rests in a
                T-pose, Aletha in an A-pose, so a raw delta would swing her arms
                down twice (arms crossed through the torso).
Hands, fingers and clavicles are left unmapped: the study layers the gait over
her idle, so they keep the idle pose. Root XY travel is cancelled (the motor
owns it) and its speed is reported so the tempo can be checked against the game.
"""

from __future__ import annotations

from pathlib import Path

import bpy
from mathutils import Matrix, Quaternion, Vector

# source (BVH) -> (target bone, method)
JOINT_MAP: dict[str, tuple[str, str]] = {
    "Hips": ("Hips", "delta"),
    "LeftUpLeg": ("LeftUpperLeg", "delta"),
    "LeftLeg": ("LeftLowerLeg", "delta"),
    "LeftFoot": ("LeftFoot", "delta"),
    "RightUpLeg": ("RightUpperLeg", "delta"),
    "RightLeg": ("RightLowerLeg", "delta"),
    "RightFoot": ("RightFoot", "delta"),
    "Spine": ("Spine", "torso"),
    "Spine2": ("Chest", "torso"),
    "Neck": ("Neck", "neck"),
    "LeftArm": ("LeftUpperArm", "direction"),
    "LeftForeArm": ("LeftLowerArm", "direction"),
    "RightArm": ("RightUpperArm", "direction"),
    "RightForeArm": ("RightLowerArm", "direction"),
}


MIXAMO_PREFIX = "mixamorig:"
# UE-mannequin vocabulary (Quaternius Universal Animation Library and friends)
# -> the Mixamo names JOINT_MAP speaks.
# Synty Polygon vocabulary (the sword-combat pack).
SYNTY_ALIASES = {
    "hips": "Hips",
    "spine_01": "Spine",
    "spine_03": "Spine2",
    "neck": "Neck",
    "head": "Head",
    "shoulder_l": "LeftArm",
    "elbow_l": "LeftForeArm",
    "shoulder_r": "RightArm",
    "elbow_r": "RightForeArm",
    "upperleg_l": "LeftUpLeg",
    "lowerleg_l": "LeftLeg",
    "ankle_l": "LeftFoot",
    "upperleg_r": "RightUpLeg",
    "lowerleg_r": "RightLeg",
    "ankle_r": "RightFoot",
}

UE_ALIASES = {
    "pelvis": "Hips",
    "spine_01": "Spine",
    "spine_03": "Spine2",
    "neck_01": "Neck",
    "head": "Head",
    "upperarm_l": "LeftArm",
    "lowerarm_l": "LeftForeArm",
    "thigh_l": "LeftUpLeg",
    "calf_l": "LeftLeg",
    "foot_l": "LeftFoot",
    "upperarm_r": "RightArm",
    "lowerarm_r": "RightForeArm",
    "thigh_r": "RightUpLeg",
    "calf_r": "RightLeg",
    "foot_r": "RightFoot",
}


def import_animation_source(path: Path, take: str | None = None) -> bpy.types.Object:
    """Import a gait source: MoMask BVH, Mixamo FBX, or a GLB animation library
    (UE-named rig). Bones are renamed into the Mixamo vocabulary so the same
    JOINT_MAP applies; imported meshes are deleted (they would render)."""
    ext = path.suffix.lower()
    if ext == ".bvh":
        return import_bvh(path)
    before = {obj.name for obj in bpy.data.objects}
    if ext == ".fbx":
        result = bpy.ops.import_scene.fbx(filepath=str(path), use_anim=True)
    elif ext in (".glb", ".gltf"):
        result = bpy.ops.import_scene.gltf(filepath=str(path))
    else:
        raise RuntimeError(f"Unsupported gait source {path}")
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import {path}")
    added = [o for o in bpy.data.objects if o.name not in before]
    armatures = [o for o in added for _ in [0] if o.type == "ARMATURE"]
    if not armatures:
        raise RuntimeError(f"No armature in {path}")
    src = armatures[0]
    for obj in added:
        if obj.type != "ARMATURE":
            bpy.data.objects.remove(obj, do_unlink=True)
    for bone in src.data.bones:
        name = bone.name
        if name.startswith(MIXAMO_PREFIX):
            bone.name = name[len(MIXAMO_PREFIX):]
        elif name.lower() in UE_ALIASES:
            bone.name = UE_ALIASES[name.lower()]
        elif name.lower() in SYNTY_ALIASES:
            bone.name = SYNTY_ALIASES[name.lower()]
    if src.animation_data is None:
        src.animation_data_create()
    if take:
        matches = [a for a in bpy.data.actions if a.name == take or a.name.startswith(take)]
        if not matches:
            raise RuntimeError(f"Take {take!r} not in {path.name}")
        src.animation_data.action = matches[0]
    elif src.animation_data.action is None:
        fresh = [a for a in bpy.data.actions if a.users == 0 or a.name.startswith("Armature")]
        if fresh:
            src.animation_data.action = fresh[0]
    src.name = "Gait_Source"
    return src


def import_bvh(path: Path) -> bpy.types.Object:
    before = {obj.name for obj in bpy.data.objects}
    result = bpy.ops.import_anim.bvh(
        filepath=str(path),
        use_fps_scale=True,  # 20 fps BVH keys land on the 30 fps scene timeline
        update_scene_fps=False,
        update_scene_duration=False,
    )
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import BVH: {path}")
    added = [o for o in bpy.data.objects if o.name not in before and o.type == "ARMATURE"]
    if len(added) != 1:
        raise RuntimeError(f"Expected one BVH armature, found {len(added)}")
    src = added[0]
    src.name = "BVH_Source"
    # Not hidden: hide_viewport drops the object from depsgraph evaluation in
    # background mode and pose reads go stale. Armatures never render anyway.
    return src


def _rest_world(arm: bpy.types.Object, bone: str) -> Matrix:
    return arm.matrix_world @ arm.data.bones[bone].matrix_local


def _pose_world(arm: bpy.types.Object, bone: str) -> Matrix:
    return arm.matrix_world @ arm.pose.bones[bone].matrix


def _direction_offset(src: bpy.types.Object, s: str, tgt: bpy.types.Object, t: str) -> Quaternion:
    src_rest = _rest_world(src, s).to_quaternion()
    tgt_rest = _rest_world(tgt, t).to_quaternion()
    sb, tb = src.data.bones[s], tgt.data.bones[t]
    src_dir = (src.matrix_world.to_3x3() @ (sb.tail_local - sb.head_local)).normalized()
    tgt_dir = (tgt.matrix_world.to_3x3() @ (tb.tail_local - tb.head_local)).normalized()
    align = tgt_dir.rotation_difference(src_dir)
    return src_rest.inverted() @ (align @ tgt_rest)


def _frame_rotation(up: Vector, lateral: Vector) -> Quaternion:
    """Orthonormal frame from an up axis and a lateral hint, as a rotation."""
    z = up.normalized()
    x = (lateral - lateral.project(z)).normalized()
    y = z.cross(x)
    return Matrix((x, y, z)).transposed().to_quaternion()


def _torso_frame(arm: bpy.types.Object, rest: bool) -> Quaternion:
    if rest:
        p = lambda b: _rest_world(arm, b).translation
    else:
        p = lambda b: _pose_world(arm, b).translation
    return _frame_rotation(p("Neck") - p("Spine"), p("LeftArm") - p("RightArm"))


def _neck_dir(arm: bpy.types.Object, rest: bool) -> Vector:
    p = (lambda b: _rest_world(arm, b).translation) if rest else (lambda b: _pose_world(arm, b).translation)
    return (p("Head") - p("Neck")).normalized()


def retarget(
    src: bpy.types.Object,
    tgt: bpy.types.Object,
    action_name: str,
    frame_start: int,
    frame_end: int,
    smooth: bool = True,
) -> tuple[bpy.types.Action, float]:
    """Bake src's current action onto tgt as a new action keyed on every integer
    frame in [frame_start, frame_end]. Returns (action, mean forward speed m/s
    of the source root before its travel was cancelled). `smooth` runs the
    temporal low-pass meant for 20 fps generated takes; off for authored loops."""
    scene = bpy.context.scene
    missing = [s for s in JOINT_MAP if s not in src.data.bones]
    if missing:
        raise RuntimeError(f"BVH lacks joints {missing}")
    for pb in tgt.pose.bones:
        pb.rotation_mode = "QUATERNION"

    offsets = {
        s: _direction_offset(src, s, tgt, t)
        for s, (t, method) in JOINT_MAP.items()
        if method == "direction"
    }
    src_rest_rot = {s: _rest_world(src, s).to_quaternion() for s in JOINT_MAP}
    tgt_rest_rot = {t: _rest_world(tgt, t).to_quaternion() for t, _ in JOINT_MAP.values()}
    src_torso_rest = _torso_frame(src, rest=True)
    src_neck_rest = _neck_dir(src, rest=True)
    tgt_hips_rest = _rest_world(tgt, "Hips").translation.copy()
    leg = lambda arm, a, b: (_rest_world(arm, a).translation - _rest_world(arm, b).translation).length
    scale = leg(tgt, "Hips", "LeftFoot") / max(leg(src, "Hips", "LeftFoot"), 1e-6)

    # Bones in hierarchy order so parents are posed before children.
    ordered = sorted(JOINT_MAP.items(), key=lambda kv: len(tgt.data.bones[kv[1][0]].parent_recursive))

    if tgt.animation_data is None:
        tgt.animation_data_create()
    action = bpy.data.actions.new(action_name)
    tgt.animation_data.action = action
    tgt_inv = tgt.matrix_world.inverted()

    scene.frame_set(frame_start)
    bpy.context.view_layer.update()
    src_hips0 = _pose_world(src, "Hips").translation.copy()
    src_hips_prev = src_hips0.copy()
    travel = 0.0

    for frame in range(frame_start, frame_end + 1):
        scene.frame_set(frame)
        for pb in tgt.pose.bones:
            pb.matrix_basis = Matrix.Identity(4)
        bpy.context.view_layer.update()
        src_hips = _pose_world(src, "Hips").translation.copy()
        travel += (src_hips.xy - src_hips_prev.xy).length
        src_hips_prev = src_hips

        torso_delta = _torso_frame(src, rest=False) @ src_torso_rest.inverted()
        neck_delta = src_neck_rest.rotation_difference(_neck_dir(src, rest=False))
        for s, (t, method) in ordered:
            src_pose = _pose_world(src, s).to_quaternion()
            if method == "direction":
                desired = src_pose @ offsets[s]
            elif method == "torso":
                desired = torso_delta @ tgt_rest_rot[t]
            elif method == "neck":
                desired = neck_delta @ tgt_rest_rot[t]
            else:
                desired = src_pose @ src_rest_rot[s].inverted() @ tgt_rest_rot[t]
            world = desired.to_matrix().to_4x4()
            pb = tgt.pose.bones[t]
            world.translation = _pose_world(tgt, t).translation
            if s == "Hips":
                # In place: keep only the vertical bob relative to the first frame.
                world.translation = tgt_hips_rest + Vector((0.0, 0.0, (src_hips.z - src_hips0.z) * scale))
            pb.matrix = tgt_inv @ world
            bpy.context.view_layer.update()
            pb.keyframe_insert("rotation_quaternion", frame=frame, group=t)
            if s == "Hips":
                pb.keyframe_insert("location", frame=frame, group=t)

    for fc in action.fcurves:
        for k in fc.keyframe_points:
            k.interpolation = "LINEAR"
    if smooth:
        smooth_action_rotations(tgt, action, frame_start, frame_end, passes=2, strength=0.35)
    seconds = (frame_end - frame_start) / scene.render.fps
    return action, (travel / seconds if seconds > 0 else 0.0)


def smooth_action_rotations(
    tgt: bpy.types.Object,
    action: bpy.types.Action,
    frame_start: int,
    frame_end: int,
    passes: int = 1,
    strength: float = 0.3,
) -> None:
    """Light temporal low-pass over the baked bone rotations (and the hips
    bob): each frame is slerped toward the midpoint of its neighbours. The
    20 fps generator resampled to 30 fps carries per-frame jitter that a fast
    gait amplifies into visible glitching; two passes at 0.35 remove the
    frame-to-frame noise while leaving the stride's real accelerations
    (verified by eye against the source: contact timing unchanged)."""
    frames = list(range(frame_start, frame_end + 1))
    bones = [b.name for b in tgt.pose.bones]
    # Read all keyed values once.
    rot: dict[str, list[Quaternion]] = {}
    loc: dict[str, list[Vector]] = {}
    for name in bones:
        rq = [None] * len(frames)
        lv = [None] * len(frames)
        for fc in action.fcurves:
            if not fc.data_path.startswith(f'pose.bones["{name}"].'):
                continue
            channel = fc.data_path.rsplit(".", 1)[1]
            for i, frame in enumerate(frames):
                v = fc.evaluate(frame)
                if channel == "rotation_quaternion":
                    if rq[i] is None:
                        rq[i] = [0.0, 0.0, 0.0, 0.0]
                    rq[i][fc.array_index] = v
                elif channel == "location":
                    if lv[i] is None:
                        lv[i] = [0.0, 0.0, 0.0]
                    lv[i][fc.array_index] = v
        if rq[0] is not None:
            rot[name] = [Quaternion(q) for q in rq]
        if lv[0] is not None:
            loc[name] = [Vector(v) for v in lv]
    for _ in range(passes):
        for name, qs in rot.items():
            prev = list(qs)
            for i in range(1, len(qs) - 1):
                mid = prev[i - 1].slerp(prev[i + 1], 0.5)
                qs[i] = prev[i].slerp(mid, strength)
        for name, vs in loc.items():
            prev = list(vs)
            for i in range(1, len(vs) - 1):
                mid = (prev[i - 1] + prev[i + 1]) * 0.5
                vs[i] = prev[i].lerp(mid, strength)
    # Write back.
    for fc in action.fcurves:
        if not fc.data_path.startswith('pose.bones["'):
            continue
        name = fc.data_path.split('"')[1]
        channel = fc.data_path.rsplit(".", 1)[1]
        for i, frame in enumerate(frames):
            if channel == "rotation_quaternion" and name in rot:
                value = rot[name][i][fc.array_index]
            elif channel == "location" and name in loc:
                value = loc[name][i][fc.array_index]
            else:
                continue
            for k in fc.keyframe_points:
                if abs(k.co.x - frame) < 0.5:
                    k.co.y = value
                    break
        fc.update()


def face_forward(
    src: bpy.types.Object, frame_start: int, frame_end: int, keep_deg: float = 0.0
) -> float:
    """Counter-rotate the source armature about world Z so the pelvis, on
    average over the window, faces the way it faces in the rest pose. Generated
    takes often walk turned 30-60 degrees off their rest facing; under a
    lock-on the body must face the target, so the take's own turn is removed
    here and only the stride's direction relative to the body survives.
    Returns the removed turn in degrees."""
    import math

    scene = bpy.context.scene

    def pelvis_forward(rest: bool):
        p = (lambda b: _rest_world(src, b).translation) if rest else (lambda b: _pose_world(src, b).translation)
        lateral = p("LeftUpLeg") - p("RightUpLeg")
        lateral.z = 0.0
        if lateral.length < 1e-6:
            return None
        forward = lateral.cross(Vector((0.0, 0.0, 1.0)))
        forward.normalize()
        return forward

    rest_forward = pelvis_forward(rest=True)
    if rest_forward is None:
        return 0.0
    sum_x = sum_y = 0.0
    for frame in range(frame_start, frame_end + 1):
        scene.frame_set(frame)
        bpy.context.view_layer.update()
        f = pelvis_forward(rest=False)
        if f is not None:
            sum_x += f.x
            sum_y += f.y
    if abs(sum_x) + abs(sum_y) < 1e-6:
        return 0.0
    mean_yaw = math.atan2(sum_y, sum_x)
    rest_yaw = math.atan2(rest_forward.y, rest_forward.x)
    turn = mean_yaw - rest_yaw
    if keep_deg != 0.0:
        turn -= math.radians(abs(keep_deg)) * (1.0 if turn >= 0 else -1.0)
    src.rotation_euler = (src.rotation_euler.x, src.rotation_euler.y, src.rotation_euler.z - turn)
    bpy.context.view_layer.update()
    return math.degrees(turn)


def gait_window(src: bpy.types.Object, frame_start: int, frame_end: int) -> tuple[int, int]:
    """Frames where the source root moves at >= 50% of its peak smoothed speed:
    trims the standing intro/outro that text-to-motion samples often carry."""
    scene = bpy.context.scene
    pos = []
    for frame in range(frame_start, frame_end + 1):
        scene.frame_set(frame)
        bpy.context.view_layer.update()
        pos.append(_pose_world(src, "Hips").translation.xy.copy())
    speed = [0.0] + [(pos[i] - pos[i - 1]).length for i in range(1, len(pos))]
    k = 8
    smooth = [sum(speed[max(0, i - k) : i + k + 1]) / len(speed[max(0, i - k) : i + k + 1]) for i in range(len(speed))]
    peak = max(smooth) if smooth else 0.0
    if peak <= 1e-6:
        return frame_start, frame_end
    moving = [i for i, v in enumerate(smooth) if v >= 0.5 * peak]
    return frame_start + moving[0], frame_start + moving[-1]
