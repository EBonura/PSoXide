#!/usr/bin/env python3
"""Three-phase locomotion study built from Aletha Delivered's own takes.

The artist walk takes are ~6 s two-stride loops on the shipped rig, so no
retargeting happens here: one GLB provides the rig, the gait take, and the
idle take. The study assembles, on that rig:

    live idle -> windup (half stride) -> cruise (N cycles) -> winddown -> live idle

with the gait retimed by --tempo (default 3x, the pace the runtime currently
fakes with speed_q8) so the baked clip plays at 1x in the engine. Horizontal
root travel is measured and cancelled automatically; the takes are expected
to be near in-place already.

Run through Blender:
    blender --background --factory-startup --python tools/aletha_walk_study.py -- \
        --source ~/Downloads/Aletha.glb --take aletha_walk_fwd \
        --output "~/Desktop/Bonnie Studios/Aletha Studies/Locomotion/walk_fwd.mp4"
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
from aletha_bvh_retarget import face_forward, gait_window, import_animation_source, retarget  # noqa: E402

PREVIEW_FPS = 30


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path, help="Rigged GLB with takes")
    parser.add_argument("--take", help="Gait action name in the GLB (artist take)")
    parser.add_argument(
        "--gait-bvh", "--gait-file", dest="gait_bvh", type=Path,
        help="Retarget this source onto the rig as the gait: MoMask BVH, Mixamo FBX, "
        "or a GLB animation library (UE-named rig, pick with --gait-take)",
    )
    parser.add_argument("--gait-take", help="Action name inside a multi-take gait source")
    parser.add_argument(
        "--loop-take", action="store_true",
        help="The gait source is exactly one authored loop: cycle = whole take, no smoothing",
    )
    parser.add_argument(
        "--face-forward", action="store_true",
        help="Remove the take's mean body turn (pelvis facing) before retargeting",
    )
    parser.add_argument(
        "--face-forward-keep",
        type=float,
        default=0.0,
        help="Degrees of the take's own body turn to KEEP when facing it "
        "forward: a locked strafe reads better angled into its travel "
        "direction than square to the camera (sign follows the take)",
    )
    parser.add_argument(
        "--reverse", action="store_true",
        help="Play the gait window backwards (a forward walk reversed reads as walking backward)",
    )
    parser.add_argument(
        "--gait-window", type=int, nargs=2, metavar=("FIRST", "LAST"),
        help="Gait frames to cycle (30 fps timeline); default: artist take = whole, BVH = auto",
    )
    parser.add_argument("--idle-take", default="aletha_idle")
    parser.add_argument("--output", required=True, type=Path, help="Preview MP4 path")
    parser.add_argument("--fbx-output", type=Path, help="Baked study FBX (armature only)")
    parser.add_argument(
        "--phase-glb-dir", type=Path,
        help="Export windup / cruise / winddown as <stem>_windup.glb, <stem>.glb, <stem>_winddown.glb "
             "(rig + mesh, one action each) for import-locomotion",
    )
    parser.add_argument("--phase-stem", default="walk_fwd")
    parser.add_argument(
        "--mirror-cruise-stem",
        help="Also export the cruise loop mirrored (left/right swapped) under this stem",
    )
    parser.add_argument("--metadata", type=Path, help="Phase-range JSON")
    parser.add_argument("--tempo", type=float, default=1.35, help="Gait speed-up baked in")
    parser.add_argument(
        "--cycle-frames", type=int, help="Override the detected gait cycle length (30 fps frames)"
    )
    parser.add_argument(
        "--torso-keep",
        type=float,
        default=0.15,
        help="Fraction of the torso's own motion kept around its mean gait "
        "orientation (1 = untouched). Generated takes wander in pitch; the "
        "mean forward lean survives, the pumping goes.",
    )
    parser.add_argument(
        "--lean-offset-deg",
        type=float,
        default=0.0,
        help="Extra constant forward torso lean added on top of the take's mean",
    )
    parser.add_argument(
        "--arm-swing-deg",
        type=float,
        default=0.0,
        help="Replace the take's arm motion with a procedural pumping cycle "
        "locked to the legs (upper-arm swing amplitude; 0 = keep the take's arms)",
    )
    parser.add_argument("--elbow-deg", type=float, default=90.0, help="Procedural arms: elbow bend")
    parser.add_argument(
        "--arm-abduct-deg", type=float, default=10.0, help="Procedural arms: outward tilt"
    )
    parser.add_argument(
        "--view", choices=("threequarter", "side", "front"), default="threequarter",
        help="Review camera placement",
    )
    parser.add_argument("--cruise-cycles", type=int, default=2)
    parser.add_argument("--idle-seconds", type=float, default=1.2)
    parser.add_argument("--transition-seconds", type=float, default=0.5)
    return parser.parse_args(argv)


def import_source(path: Path) -> bpy.types.Object:
    # Factory startup ships a cube, light and camera; clear them.
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete()
    before = {obj.name for obj in bpy.data.objects}
    result = bpy.ops.import_scene.gltf(filepath=str(path))
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import GLB: {path}")
    armatures = [
        obj
        for obj in bpy.data.objects
        if obj.name not in before and obj.type == "ARMATURE"
    ]
    if len(armatures) != 1:
        raise RuntimeError(f"Expected one armature, found {len(armatures)}")
    rig = armatures[0]
    if rig.animation_data is None:
        rig.animation_data_create()
    for track in rig.animation_data.nla_tracks:
        track.mute = True
    return rig


def action_named(name: str) -> bpy.types.Action:
    for action in bpy.data.actions:
        if action.name == name or action.name.startswith(name):
            return action
    raise RuntimeError(f"Take {name!r} not found; have: {[a.name for a in bpy.data.actions]}")


def bone_by_suffix(rig: bpy.types.Object, suffix: str) -> str:
    lowered = suffix.lower()
    for bone in rig.data.bones:
        if bone.name.lower().endswith(lowered):
            return bone.name
    raise RuntimeError(f"No bone ending in {suffix!r}")


def set_frame(frame: float) -> None:
    whole = math.floor(frame)
    bpy.context.scene.frame_set(whole, subframe=frame - whole)
    bpy.context.view_layer.update()


def sample_pose(rig: bpy.types.Object, action: bpy.types.Action, frame: float):
    rig.animation_data.action = action
    set_frame(frame)
    return {
        bone.name: (
            bone.location.copy(),
            bone.rotation_quaternion.copy()
            if bone.rotation_mode == "QUATERNION"
            else bone.matrix_basis.to_quaternion(),
            bone.scale.copy(),
        )
        for bone in rig.pose.bones
    }


def world_positions(
    rig: bpy.types.Object, action: bpy.types.Action, bone: str, first: int, frames: int
) -> list[Vector]:
    rig.animation_data.action = action
    out = []
    for frame in range(frames):
        set_frame(first + frame)
        out.append((rig.matrix_world @ rig.pose.bones[bone].matrix).translation.copy())
    return out


def detect_cycle(positions: dict[str, list[Vector]], frames: int) -> int:
    """Autocorrelation over foot+hips world positions: the lag with minimal
    mean distance to the unshifted signal, searched in 25%..90% of the take
    (a single-cycle take's true period sits near its full length)."""
    best_lag, best_score = 0, float("inf")
    for lag in range(frames // 4, (frames * 9) // 10):
        score = 0.0
        count = 0
        for series in positions.values():
            for i in range(frames - lag):
                score += (series[i] - series[i + lag]).length
                count += 1
        score /= max(count, 1)
        if score < best_score:
            best_score, best_lag = score, lag
    return best_lag


def reverse_action_window(action: bpy.types.Action, first: int, last: int) -> None:
    """Mirror every keyframe inside [first, last] in time: frame f -> first + last - f."""
    for fc in action.fcurves:
        for key in fc.keyframe_points:
            f = key.co.x
            if first - 0.5 <= f <= last + 0.5:
                key.co.x = first + last - f
                key.handle_left.x = first + last - key.handle_left.x
                key.handle_right.x = first + last - key.handle_right.x
        fc.update()


def face_forward_action(
    rig: bpy.types.Object,
    action: bpy.types.Action,
    first: int,
    last: int,
    keep_deg: float = 0.0,
) -> float:
    """Remove a baked take's mean body turn so it can serve as a locked
    directional clip: an artist "walk left" take yaws the body ~84 degrees
    into the direction of travel, but under lock-on the body must face the
    target while the feet carry the direction.

    The correction rewrites the ROOT bone's keyframe values directly (rather
    than posing per frame, which the depsgraph re-evaluates from the action
    and partially discards): with `rest` the root's armature-space rest
    matrix and `R` the world-Z counter-rotation, every basis is left-
    multiplied by `rest^-1 * R * rest`. Returns the removed turn in degrees.
    """
    import math

    lh, rh = bone_by_suffix(rig, "leftupperleg"), bone_by_suffix(rig, "rightupperleg")
    root = bone_by_suffix(rig, "hips")

    def facing(rest: bool):
        if rest:
            p = lambda b: (rig.matrix_world @ rig.data.bones[b].matrix_local).translation
        else:
            p = lambda b: (rig.matrix_world @ rig.pose.bones[b].matrix).translation
        lateral = p(lh) - p(rh)
        lateral.z = 0.0
        if lateral.length < 1e-6:
            return None
        forward = lateral.normalized().cross(Vector((0.0, 0.0, 1.0)))
        return math.atan2(forward.y, forward.x)

    rig.animation_data.action = action
    rest_yaw = facing(rest=True)
    if rest_yaw is None:
        return 0.0
    sum_x = sum_y = 0.0
    for frame in range(first, last + 1):
        set_frame(frame)
        yaw = facing(rest=False)
        if yaw is not None:
            sum_x += math.cos(yaw)
            sum_y += math.sin(yaw)
    if abs(sum_x) + abs(sum_y) < 1e-6:
        return 0.0
    turn = math.atan2(sum_y, sum_x) - rest_yaw
    # Keep a slice of the take's own turn: rotating the root carries the legs
    # too, so removing all of it leaves a forward stride. A locked strafe
    # reads best angled into its travel direction.
    if keep_deg != 0.0:
        keep = math.radians(abs(keep_deg)) * (1.0 if turn >= 0 else -1.0)
        turn -= keep

    rest_local = rig.data.bones[root].matrix_local
    basis_rotation = (
        rest_local.inverted() @ Matrix.Rotation(-turn, 4, "Z") @ rest_local
    ).to_3x3()
    basis_quat = basis_rotation.to_quaternion()

    curves = {}
    for fc in action.fcurves:
        if fc.data_path == f'pose.bones["{root}"].rotation_quaternion':
            curves.setdefault("rot", {})[fc.array_index] = fc
        elif fc.data_path == f'pose.bones["{root}"].location':
            curves.setdefault("loc", {})[fc.array_index] = fc
    rot = curves.get("rot", {})
    if len(rot) == 4:
        frames = [key.co.x for key in rot[0].keyframe_points]
        for index, frame in enumerate(frames):
            q = Quaternion([rot[axis].keyframe_points[index].co.y for axis in range(4)])
            q = basis_quat @ q
            for axis in range(4):
                rot[axis].keyframe_points[index].co.y = q[axis]
    loc = curves.get("loc", {})
    if len(loc) == 3:
        for index in range(len(loc[0].keyframe_points)):
            v = Vector([loc[axis].keyframe_points[index].co.y for axis in range(3)])
            v = basis_rotation @ v
            for axis in range(3):
                loc[axis].keyframe_points[index].co.y = v[axis]
    for fc in action.fcurves:
        fc.update()
    return math.degrees(turn)


def stance_speed(
    rig: bpy.types.Object,
    action: bpy.types.Action,
    feet: dict[str, list[Vector]],
    hips: list[Vector],
    first: int,
) -> float:
    """Ground speed implied by the retargeted gait: how fast the planted foot
    slides backward under the hips (m/s at 30 fps). Independent of any root
    travel, so it also works for in-place loops; this is the number the motor
    must match to avoid foot skating."""
    lh = bone_by_suffix(rig, "leftupperleg")
    rh = bone_by_suffix(rig, "rightupperleg")
    speeds = []
    n = len(hips)
    for series in feet.values():
        zs = [p.z for p in series]
        z0, zspan = min(zs), max(max(zs) - min(zs), 1e-6)
        for i in range(1, n - 1):
            if (series[i].z - z0) / zspan > 0.15:
                continue
            set_frame(first + i)
            w = lambda name: (rig.matrix_world @ rig.pose.bones[name].matrix).translation
            lateral = w(lh) - w(rh)
            lateral.z = 0.0
            if lateral.length < 1e-6:
                continue
            fwd = lateral.normalized().cross(Vector((0.0, 0.0, 1.0)))
            rel = (series[i + 1] - hips[i + 1]) - (series[i - 1] - hips[i - 1])
            rel.z = 0.0
            speeds.append(rel.length * 0.5 * PREVIEW_FPS)
    speeds = sorted(speeds)
    return speeds[len(speeds) // 2] if speeds else 0.0


def contact_frame(feet: dict[str, list[Vector]], cycle: int) -> int:
    """Lowest + slowest foot sample inside the first cycle (a planted heel)."""
    zs = [p.z for series in feet.values() for p in series]
    z0, zspan = min(zs), max(max(zs) - min(zs), 1e-6)
    best, best_score = 0, float("inf")
    for series in feet.values():
        for i in range(1, min(cycle, len(series) - 1)):
            speed = (series[i + 1] - series[i - 1]).length * 0.5
            score = (series[i].z - z0) / zspan + speed * 0.45
            if score < best_score:
                best_score, best = score, i
    return best


def flatten_torso_lean(
    rig: bpy.types.Object,
    action: bpy.types.Action,
    first: int,
    frames: int,
    keep: float,
    offset_deg: float = 0.0,
) -> None:
    """Hold the torso's world-space lean at its mean (+ offset) over the gait window.

    The lean (spine-base->neck axis vs vertical, in the facing plane) mostly
    rides on the PELVIS rotation, which local-bone smoothing cannot see. Per
    frame the deviation from the mean lean is measured in world space and the
    Spine bone (root of the upper body; the legs are not its children) is
    counter-rotated about the world lateral axis through its own head, so the
    feet keep their placement while the torso holds its lean. `keep` leaves a
    fraction of the natural deviation (stride bounce)."""
    import math as _math

    spine = bone_by_suffix(rig, "spine")
    neck = bone_by_suffix(rig, "neck")
    lh = bone_by_suffix(rig, "leftupperleg")
    rh = bone_by_suffix(rig, "rightupperleg")
    rig.animation_data.action = action
    measures = []
    for frame in range(first, first + frames + 1):
        set_frame(frame)
        w = lambda name: (rig.matrix_world @ rig.pose.bones[name].matrix).translation
        axis = w(neck) - w(spine)
        lateral = w(lh) - w(rh)
        fwd = lateral.cross(Vector((0, 0, 1)))
        fwd.z = 0.0
        if fwd.length < 1e-6 or axis.length < 1e-6:
            measures.append((frame, 0.0, Vector((1, 0, 0))))
            continue
        fwd.normalize()
        horiz = Vector((axis.x, axis.y, 0.0))
        lean = _math.atan2(horiz.dot(fwd), axis.z)
        measures.append((frame, lean, lateral.normalized()))
    mean = sum(lean for _, lean, _ in measures) / max(len(measures), 1)
    offset = _math.radians(offset_deg)
    for frame, lean, lateral in measures:
        # new lean = mean + offset + keep * (lean - mean)
        delta = (lean - mean) * (1.0 - keep) - offset
        if abs(delta) < 1e-4:
            continue
        set_frame(frame)
        pb = rig.pose.bones[spine]
        head = pb.matrix.translation.copy()
        correction = (
            Matrix.Translation(head)
            @ Matrix.Rotation(-delta, 4, lateral)
            @ Matrix.Translation(-head)
        )
        pb.matrix = correction @ pb.matrix
        bpy.context.view_layer.update()
        pb.keyframe_insert("rotation_quaternion", frame=frame, group=spine)
        pb.keyframe_insert("location", frame=frame, group=spine)
    for fc in action.fcurves:
        fc.update()


def procedural_arms(
    rig: bpy.types.Object,
    action: bpy.types.Action,
    first: int,
    frames: int,
    swing_deg: float,
    elbow_deg: float,
    abduct_deg: float,
) -> None:
    """Overwrite the arm swing with a clean pumping cycle locked to the legs.

    Generated takes carry noisy, low-energy arms (elbows at the waist, hands
    trailing). A run's arms are simple: upper arm swings about the shoulder in
    opposition to the same-side leg, elbow held bent so the hand rides
    forward-up when the arm is ahead. Phase comes straight from the feet
    (right foot ahead => left arm forward), so it never desyncs from the
    stride. Hands stay unmapped (idle pose)."""
    import math as _math

    lf, rf = bone_by_suffix(rig, "leftfoot"), bone_by_suffix(rig, "rightfoot")
    lh, rh = bone_by_suffix(rig, "leftupperleg"), bone_by_suffix(rig, "rightupperleg")
    arms = {
        "left": (bone_by_suffix(rig, "leftupperarm"), bone_by_suffix(rig, "leftlowerarm")),
        "right": (bone_by_suffix(rig, "rightupperarm"), bone_by_suffix(rig, "rightlowerarm")),
    }
    rig.animation_data.action = action
    world3 = rig.matrix_world.to_3x3()
    rest_dir, rest_rot = {}, {}
    for ua, la in arms.values():
        for name in (ua, la):
            b = rig.data.bones[name]
            rest_dir[name] = (world3 @ (b.tail_local - b.head_local)).normalized()
            rest_rot[name] = (rig.matrix_world @ b.matrix_local).to_quaternion()

    lo = int(action.frame_range[0])
    hi = int(action.frame_range[1])
    up = Vector((0.0, 0.0, 1.0))
    samples = []
    for frame in range(lo, hi + 1):
        set_frame(frame)
        w = lambda n: (rig.matrix_world @ rig.pose.bones[n].matrix).translation
        left_dir = w(lh) - w(rh)
        left_dir.z = 0.0
        if left_dir.length < 1e-6:
            left_dir = Vector((-1.0, 0.0, 0.0))
        left_dir.normalize()
        fwd = left_dir.cross(up).normalized()
        samples.append((frame, left_dir, fwd, (w(rf) - w(lf)).dot(fwd)))
    in_window = [d for f, _, _, d in samples if first <= f <= first + frames]
    # Sinusoid amplitude from the RMS (a single foot pop must not shrink the
    # whole swing); clamped to +-1 below.
    dmax = (sum(d * d for d in in_window) / max(len(in_window), 1)) ** 0.5 * 2 ** 0.5 or 1.0

    SWING_CENTRE = _math.radians(-15.0)
    ELBOW_MODULATION = _math.radians(-15.0)
    swing = _math.radians(swing_deg)
    elbow = _math.radians(elbow_deg)
    abduct = _math.radians(abduct_deg)
    for frame, left_dir, fwd, d in samples:
        set_frame(frame)
        s = max(-1.0, min(1.0, d / dmax))
        right_dir = -left_dir
        for side, (ua, la) in arms.items():
            out_dir = left_dir if side == "left" else right_dir
            phase = s if side == "left" else -s
            # Swing centred a little behind vertical (elbows ride behind the
            # torso); the elbow closes slightly as the hand comes forward.
            angle = SWING_CENTRE + swing * phase
            bend = elbow + ELBOW_MODULATION * phase
            hang = -up * _math.cos(abduct) + out_dir * _math.sin(abduct)
            upper = Matrix.Rotation(angle, 3, right_dir) @ hang
            lower = Matrix.Rotation(bend, 3, right_dir) @ upper
            for name, direction in ((ua, upper), (la, lower)):
                pb = rig.pose.bones[name]
                head = (rig.matrix_world @ pb.matrix).translation
                q = rest_dir[name].rotation_difference(direction) @ rest_rot[name]
                world = Matrix.Translation(head) @ q.to_matrix().to_4x4()
                pb.matrix = rig.matrix_world.inverted() @ world
                bpy.context.view_layer.update()
                pb.keyframe_insert("rotation_quaternion", frame=frame, group=name)
    for fc in action.fcurves:
        fc.update()


def stabilize_torso(
    rig: bpy.types.Object,
    action: bpy.types.Action,
    first: int,
    frames: int,
    keep: float,
) -> None:
    """Pull the torso chain toward its mean orientation over the gait window,
    keeping `keep` of each frame's own deviation. Kills the slow forward/back
    pitching the generator wanders through while preserving the mean lean and
    a natural fraction of stride-coupled torso motion."""
    if keep >= 0.999:
        return
    names = [
        bone.name
        for bone in rig.pose.bones
        if bone.name.lower().endswith(("spine", "chest", "neck"))
    ]
    for name in names:
        curves = [
            fc
            for fc in action.fcurves
            if fc.data_path == f'pose.bones["{name}"].rotation_quaternion'
        ]
        if len(curves) != 4:
            continue
        curves.sort(key=lambda fc: fc.array_index)
        qs = []
        for frame in range(first, first + frames + 1):
            qs.append(Quaternion([fc.evaluate(frame) for fc in curves]))
        reference = qs[0]
        total = [0.0, 0.0, 0.0, 0.0]
        for q in qs:
            aligned = -q if reference.dot(q) < 0 else q
            for i in range(4):
                total[i] += aligned[i]
        mean = Quaternion(total).normalized()
        for offset, q in enumerate(qs):
            aligned = -q if mean.dot(q) < 0 else q
            stabilized = mean.slerp(aligned, keep)
            frame = first + offset
            for fc in curves:
                for key in fc.keyframe_points:
                    if abs(key.co.x - frame) < 0.5:
                        key.co.y = stabilized[fc.array_index]
                        break
        for fc in curves:
            fc.update()


def smoothstep(v: float) -> float:
    v = max(0.0, min(1.0, v))
    return v * v * (3.0 - 2.0 * v)


def mesh_floor_z(rig: bpy.types.Object) -> float:
    """Lowest world Z of the skinned mesh in the rig's current pose."""
    depsgraph = bpy.context.evaluated_depsgraph_get()
    lowest = float("inf")
    for child in rig.children_recursive:
        if child.type != "MESH":
            continue
        evaluated = child.evaluated_get(depsgraph)
        for vertex in evaluated.data.vertices:
            lowest = min(lowest, (evaluated.matrix_world @ vertex.co).z)
    return lowest


def level_poses_to_floor(
    rig: bpy.types.Object,
    assembled: bpy.types.Action,
    frames: list[int],
    floor_z: float,
) -> list[dict]:
    """Sample `frames` of `assembled` and shift the Hips of each pose so the
    mesh's lowest point sits exactly on `floor_z`. A rotation-only retarget
    onto a rig with other leg proportions loses the source's foot planting
    (the planted foot bobs a few cm through its stance, which the runtime,
    anchoring a clip by its first frame only, shows as floating mid-stride).
    Pinning the lowest point per frame keeps the planted foot on the ground
    and lets the swing foot rise."""
    hips = bone_by_suffix(rig, "hips")
    rest_inv = rig.data.bones[hips].matrix_local.to_3x3().inverted()
    poses = []
    for frame in frames:
        pose = sample_pose(rig, assembled, frame)
        delta = mesh_floor_z(rig) - floor_z
        loc, rot, scale = pose[hips]
        pose[hips] = (loc + rest_inv @ Vector((0.0, 0.0, -delta)), rot, scale)
        poses.append(pose)
    return poses


def export_phase_glbs(
    rig: bpy.types.Object,
    assembled: bpy.types.Action,
    splits: dict[str, list[int]],
    out_dir: Path,
    stem: str,
) -> None:
    """Slice the assembled clip into per-phase actions (rebased to frame 1)
    and export each as its own GLB: the locomotion importer cooks one take per
    file. The cruise is exactly the loop cycle. Every phase is levelled to
    the idle's floor (frame 1 of the assembled clip)."""
    out_dir.mkdir(parents=True, exist_ok=True)
    scene = bpy.context.scene
    sample_pose(rig, assembled, 1)
    floor_z = mesh_floor_z(rig)
    print(f"[study] floor from idle frame 1: z={floor_z:.4f}")
    phases = {
        f"{stem}_windup": splits["windup"],
        stem: splits["cruise_loop"],
        f"{stem}_winddown": splits["winddown"],
    }
    for name, (first, last) in phases.items():
        poses = level_poses_to_floor(rig, assembled, list(range(first, last + 1)), floor_z)
        action = bpy.data.actions.new(name)
        rig.animation_data.action = action
        for index, pose in enumerate(poses):
            for bone in rig.pose.bones:
                bone.location, bone.rotation_quaternion, bone.scale = pose[bone.name]
                bone.keyframe_insert("location", frame=index + 1, group=bone.name)
                bone.keyframe_insert("rotation_quaternion", frame=index + 1, group=bone.name)
                bone.keyframe_insert("scale", frame=index + 1, group=bone.name)
        for fcurve in action.fcurves:
            for key in fcurve.keyframe_points:
                key.interpolation = "LINEAR"
        scene.frame_start, scene.frame_end = 1, len(poses)
        bpy.ops.object.select_all(action="DESELECT")
        rig.select_set(True)
        for child in rig.children_recursive:
            child.select_set(True)
        bpy.context.view_layer.objects.active = rig
        path = out_dir / f"{name}.glb"
        result = bpy.ops.export_scene.gltf(
            filepath=str(path),
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
            raise RuntimeError(f"GLB export failed: {path}")
        print(f"[study] phase {name}: frames 1..{len(poses)} -> {path}")

    # The mirrored winddown (other foot) and, when requested, the mirrored
    # cruise: a left strafe mirrored is the right strafe.
    export_mirrored_phase(rig, assembled, splits["winddown"], floor_z, out_dir, f"{stem}_winddown_mirror")
    if MIRROR_CRUISE_STEM:
        export_mirrored_phase(rig, assembled, splits["cruise_loop"], floor_z, out_dir, MIRROR_CRUISE_STEM)


# Set from --mirror-cruise-stem: also export the cruise loop mirrored under this stem.
MIRROR_CRUISE_STEM = ""


def export_mirrored_phase(
    rig: bpy.types.Object,
    assembled: bpy.types.Action,
    phase: list[int],
    floor_z: float,
    out_dir: Path,
    name: str,
) -> None:
    """Export one phase mirrored in WORLD space across the sagittal plane
    (x = 0, the rig is centred and the gait is in place) with left/right bones
    swapped, so it does not depend on Blender's bone-roll mirror convention."""
    scene = bpy.context.scene
    first, last = phase
    poses = level_poses_to_floor(rig, assembled, list(range(first, last + 1)), floor_z)
    mirror = Matrix.Diagonal((-1.0, 1.0, 1.0, 1.0))
    partner = {}
    for bone in rig.data.bones:
        if bone.name.startswith("Left"):
            partner[bone.name] = "Right" + bone.name[4:]
        elif bone.name.startswith("Right"):
            partner[bone.name] = "Left" + bone.name[5:]
        else:
            partner[bone.name] = bone.name
    ordered = sorted(rig.pose.bones, key=lambda b: len(b.parent_recursive))
    action = bpy.data.actions.new(name)
    world_inv = rig.matrix_world.inverted()
    for index, pose in enumerate(poses):
        rig.animation_data.action = action
        for bone in ordered:
            bone.location, bone.rotation_quaternion, bone.scale = pose[bone.name]
        bpy.context.view_layer.update()
        world = {b.name: (rig.matrix_world @ b.matrix).copy() for b in rig.pose.bones}
        for bone in ordered:
            target = mirror @ world[partner[bone.name]] @ mirror
            bone.matrix = world_inv @ target
            bpy.context.view_layer.update()
        for bone in rig.pose.bones:
            bone.keyframe_insert("location", frame=index + 1, group=bone.name)
            bone.keyframe_insert("rotation_quaternion", frame=index + 1, group=bone.name)
            bone.keyframe_insert("scale", frame=index + 1, group=bone.name)
    for fcurve in action.fcurves:
        for key in fcurve.keyframe_points:
            key.interpolation = "LINEAR"
    scene.frame_start, scene.frame_end = 1, len(poses)
    rig.animation_data.action = action
    bpy.ops.object.select_all(action="DESELECT")
    rig.select_set(True)
    for child in rig.children_recursive:
        child.select_set(True)
    bpy.context.view_layer.objects.active = rig
    path = out_dir / f"{name}.glb"
    result = bpy.ops.export_scene.gltf(
        filepath=str(path),
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
        raise RuntimeError(f"GLB export failed: {path}")
    print(f"[study] phase {name}: frames 1..{len(poses)} -> {path}")


def main() -> None:
    args = parse_args()
    scene = bpy.context.scene
    scene.render.fps = PREVIEW_FPS

    rig = import_source(args.source.expanduser())
    for bone in rig.pose.bones:
        bone.rotation_mode = "QUATERNION"
    idle = action_named(args.idle_take)
    idle_frames = int(idle.frame_range[1] - idle.frame_range[0]) or 1
    take_label = args.take
    if args.gait_bvh:
        src = import_animation_source(args.gait_bvh.expanduser(), args.gait_take)
        src_action = src.animation_data.action
        src_first, src_last = int(src_action.frame_range[0]), int(src_action.frame_range[1])
        if args.gait_window:
            window = tuple(args.gait_window)
        elif args.loop_take:
            window = (src_first, src_last)
        else:
            window = gait_window(src, src_first, src_last)
        if args.face_forward:
            turn = face_forward(src, window[0], window[1], args.face_forward_keep)
            print(f"[study] face_forward removed {turn:+.1f} deg of body turn")
        gait, speed = retarget(
            src, rig, "generated_gait", window[0], window[1], smooth=not args.loop_take
        )
        gait_first, gait_frames = window[0], max(1, window[1] - window[0])
        take_label = args.gait_take or args.gait_bvh.name
        print(f"[study] gait window={window} of {src_first}..{src_last} source_speed={speed:.3f} m/s")
        if args.loop_take:
            args.cycle_frames = gait_frames
    elif args.take:
        gait = action_named(args.take)
        gait_first = int(gait.frame_range[0])
        gait_frames = int(gait.frame_range[1] - gait.frame_range[0]) or 1
        if args.face_forward:
            # Same normalisation as the import path, applied to the baked take
            # itself: counter-rotate every frame's root so the body's mean
            # facing matches the rest pose. A "walk left" take that turns the
            # character 84 degrees becomes a true strafe.
            turn = face_forward_action(
                rig, gait, gait_first, gait_first + gait_frames, args.face_forward_keep
            )
            print(f"[study] face_forward removed {turn:+.1f} deg of body turn")
        if args.gait_window:
            gait_first, gait_frames = args.gait_window[0], max(1, args.gait_window[1] - args.gait_window[0])
    else:
        raise SystemExit("need --take or --gait-bvh")

    if args.reverse:
        reverse_action_window(gait, gait_first, gait_first + gait_frames)

    hips = bone_by_suffix(rig, "hips")
    left = bone_by_suffix(rig, "leftfoot")
    right = bone_by_suffix(rig, "rightfoot")

    feet = {
        "left": world_positions(rig, gait, left, gait_first, gait_frames),
        "right": world_positions(rig, gait, right, gait_first, gait_frames),
    }
    hips_track = {"hips": world_positions(rig, gait, hips, gait_first, gait_frames)}
    print(f"[study] stance_speed={stance_speed(rig, gait, feet, hips_track['hips'], gait_first):.3f} m/s")
    stabilize_torso(rig, gait, gait_first, gait_frames, args.torso_keep)
    flatten_torso_lean(rig, gait, gait_first, gait_frames, args.torso_keep, args.lean_offset_deg)
    if args.arm_swing_deg > 0.0:
        procedural_arms(
            rig, gait, gait_first, gait_frames,
            args.arm_swing_deg, args.elbow_deg, args.arm_abduct_deg,
        )
    cycle = args.cycle_frames or detect_cycle({**feet, **hips_track}, gait_frames)
    drift = (hips_track["hips"][-1] - hips_track["hips"][0]).length
    # Loop exactly one cycle, the last full one in the take (past any run-in),
    # so the wrap lands on the same phase instead of hitching.
    loop_at = gait_frames - cycle
    feet = {side: series[loop_at : loop_at + cycle] for side, series in feet.items()}
    contact = contact_frame(feet, cycle)
    gait_first += loop_at
    print(
        f"[study] take={take_label} frames={gait_frames} cycle={cycle} loop_at={loop_at} "
        f"contact={contact} root_drift={drift:.4f}"
    )
    gait_frames = cycle

    idle_lead = int(args.idle_seconds * PREVIEW_FPS)
    transition = int(args.transition_seconds * PREVIEW_FPS)
    cruise = int(round(args.cruise_cycles * cycle / args.tempo))
    total = idle_lead + transition + cruise + transition + idle_lead
    splits = {
        "idle_in": [1, idle_lead],
        "windup": [idle_lead + 1, idle_lead + transition],
        "cruise": [idle_lead + transition + 1, idle_lead + transition + cruise],
        "cruise_loop": [
            idle_lead + transition + 1,
            idle_lead + transition + int(round(cycle / args.tempo)),
        ],
        "winddown": [idle_lead + transition + cruise + 1, idle_lead + transition + cruise + transition],
        "idle_out": [idle_lead + transition + cruise + transition + 1, total],
    }

    output_action = bpy.data.actions.new(f"{Path(take_label).stem}_study")
    half = cycle * 0.5

    def gait_phase_at(output_index: int) -> tuple[float, float]:
        """(source frame, gait envelope 0..1) for one output frame index."""
        w0 = idle_lead
        c0 = w0 + transition
        d0 = c0 + cruise
        end = d0 + transition
        if output_index < w0 or output_index >= end:
            return 0.0, 0.0
        if output_index < c0:
            progress = (output_index - w0 + 1) / (transition + 1)
            envelope = smoothstep(progress)
            return contact + half * envelope, envelope
        if output_index < d0:
            i = output_index - c0
            return contact + half + (i + 1) * args.tempo, 1.0
        progress = (output_index - d0 + 1) / (transition + 1)
        envelope = 1.0 - smoothstep(progress)
        return contact + half + cruise * args.tempo + half * smoothstep(progress), envelope

    # The idle take plays continuously as the base layer; the gait is layered
    # on top with the envelope, so both joins land on the live idle pose.
    # Bones the gait does not drive (a retarget leaves hands, fingers and
    # clavicles unmapped) stay on the idle layer instead of fading to bind.
    driven = {
        fc.data_path.split('"')[1]
        for fc in gait.fcurves
        if fc.data_path.startswith('pose.bones["')
    }
    SEAM = 3  # cruise frames blended into the loop-start pose at each wrap
    frames_keyed = 0
    for output_index in range(total):
        idle_frame = idle.frame_range[0] + (output_index % idle_frames)
        base = sample_pose(rig, idle, idle_frame)
        phase, envelope = gait_phase_at(output_index)
        layer = (
            sample_pose(rig, gait, gait_first + (phase % gait_frames))
            if envelope > 0.0
            else base
        )
        # Loop crossfade: the take continues PAST the loop window on both
        # sides (loop_at > 0 picks the last cycle), so the frames just before
        # gait_first are exactly what flows into the loop start. Blending the
        # cycle's tail toward those pre-window frames makes the wrap C1-
        # continuous instead of hoping the detected cycle is exact.
        in_cruise = splits["cruise"][0] <= output_index + 1 <= splits["cruise"][1]
        if envelope > 0.0 and in_cruise and cycle > 2 * SEAM and gait_first > SEAM:
            cycle_pos = phase % cycle
            to_end = cycle - cycle_pos
            if to_end <= SEAM:
                t = (SEAM - to_end + 1) / (SEAM + 1)
                pre = sample_pose(rig, gait, gait_first + cycle_pos - cycle)
                for bone_name, (loc0, rot0, sc0) in layer.items():
                    p_loc, p_rot, p_sc = pre[bone_name]
                    layer[bone_name] = (
                        loc0.lerp(p_loc, t),
                        rot0.slerp(p_rot, t),
                        sc0.lerp(p_sc, t),
                    )
        rig.animation_data.action = output_action
        out_frame = output_index + 1
        for bone in rig.pose.bones:
            idle_loc, idle_rot, idle_scale = base[bone.name]
            loc, rot, scale = layer[bone.name] if bone.name in driven else base[bone.name]
            loc = idle_loc.lerp(loc, envelope)
            rot = idle_rot.slerp(rot, envelope)
            scale = idle_scale.lerp(scale, envelope)
            if bone.name == hips:
                # The character motor owns horizontal travel.
                loc.x = idle_loc.x
                loc.y = idle_loc.y
            bone.location = loc
            bone.rotation_quaternion = rot
            bone.scale = scale
            bone.keyframe_insert("location", frame=out_frame, group=bone.name)
            bone.keyframe_insert("rotation_quaternion", frame=out_frame, group=bone.name)
            bone.keyframe_insert("scale", frame=out_frame, group=bone.name)
        frames_keyed += 1

    for fcurve in output_action.fcurves:
        for key in fcurve.keyframe_points:
            key.interpolation = "LINEAR"
    rig.animation_data.action = output_action
    scene.frame_start, scene.frame_end = 1, total
    scene.frame_set(1)

    # Jitter metric over the cruise: mean second difference of joint angles,
    # deg/frame^2 (higher = twitchier). Printed for candidate pre-screening.
    c0, c1 = splits["cruise"]
    prev_step = None
    total_jerk = 0.0
    jerk_samples = 0
    prev_pose = None
    for frame in range(c0, c1 + 1):
        pose = sample_pose(rig, output_action, frame)
        if prev_pose is not None:
            step = {
                name: prev_pose[name][1].rotation_difference(pose[name][1]).angle
                for name in pose
            }
            if prev_step is not None:
                for name, angle in step.items():
                    total_jerk += abs(angle - prev_step[name])
                    jerk_samples += 1
            prev_step = step
        prev_pose = pose
    if jerk_samples:
        import math as _math
        print(f"[study] cruise_jerk_deg={_math.degrees(total_jerk / jerk_samples):.3f}")

    # Torso lean trace over the cruise: the angle of the spine-base->neck
    # axis from vertical, signed forward, per frame. THE anti-bob metric:
    # its std is what "the torso holds its lean" means numerically.
    import math as _math
    spine_name = bone_by_suffix(rig, "spine")
    neck_name = bone_by_suffix(rig, "neck")
    lh = bone_by_suffix(rig, "leftupperleg")
    rh = bone_by_suffix(rig, "rightupperleg")
    leans = []
    for frame in range(c0, c1 + 1):
        sample_pose(rig, output_action, frame)
        bpy.context.view_layer.update()
        w = lambda name: (rig.matrix_world @ rig.pose.bones[name].matrix).translation
        axis = w(neck_name) - w(spine_name)
        lateral = w(lh) - w(rh)
        fwd = lateral.cross(Vector((0, 0, 1)))
        fwd.z = 0.0
        if fwd.length < 1e-6 or axis.length < 1e-6:
            continue
        fwd.normalize()
        horiz = Vector((axis.x, axis.y, 0.0))
        lean = _math.degrees(_math.atan2(horiz.dot(fwd), axis.z))
        leans.append(lean)
    if leans:
        mean = sum(leans) / len(leans)
        std = (sum((v - mean) ** 2 for v in leans) / len(leans)) ** 0.5
        print(
            f"[study] cruise_lean_deg mean={mean:.1f} min={min(leans):.1f} "
            f"max={max(leans):.1f} std={std:.2f}"
        )

    # Review render: studio workbench, camera framing the whole figure.
    out = args.output.expanduser()
    out.parent.mkdir(parents=True, exist_ok=True)
    camera_data = bpy.data.cameras.new("Study_Camera")
    camera = bpy.data.objects.new("Study_Camera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.lens = 58.0
    # Frame from the skinned mesh's world bounds at the first frame.
    scene.frame_set(1)
    bpy.context.view_layer.update()
    meshes = [child for child in rig.children_recursive if child.type == "MESH"]
    corners = [
        child.matrix_world @ Vector(corner)
        for child in meshes
        for corner in child.bound_box
    ]
    lo = Vector((min(c.x for c in corners), min(c.y for c in corners), min(c.z for c in corners)))
    hi = Vector((max(c.x for c in corners), max(c.y for c in corners), max(c.z for c in corners)))
    center = (lo + hi) * 0.5
    height = max(hi.z - lo.z, 1e-3)
    aim = bpy.data.objects.new("Study_Aim", None)
    bpy.context.collection.objects.link(aim)
    aim.location = center
    view_offset = {
        "threequarter": Vector((1.0, -1.45, 0.25)),
        "side": Vector((1.75, 0.0, 0.15)),
        "front": Vector((0.0, -1.75, 0.15)),
    }[args.view]
    camera.location = center + view_offset * height
    track = camera.constraints.new(type="TRACK_TO")
    track.target = aim
    track.track_axis = "TRACK_NEGATIVE_Z"
    track.up_axis = "UP_Y"
    bpy.ops.mesh.primitive_plane_add(size=40.0 * height, location=(center.x, center.y, lo.z))
    bpy.context.object.color = (0.055, 0.065, 0.085, 1.0)

    scene.render.engine = "BLENDER_WORKBENCH"
    scene.display.shading.light = "STUDIO"
    scene.display.shading.color_type = "MATERIAL"
    scene.display.shading.show_shadows = True
    scene.display.shading.show_cavity = True
    scene.display.shading.background_type = "WORLD"
    scene.display.shading.background_color = (0.025, 0.03, 0.045)
    scene.render.resolution_x = 640
    scene.render.resolution_y = 640
    scene.render.image_settings.file_format = "FFMPEG"
    scene.render.ffmpeg.format = "MPEG4"
    scene.render.ffmpeg.codec = "H264"
    scene.render.ffmpeg.constant_rate_factor = "MEDIUM"
    scene.render.filepath = str(out)
    bpy.ops.render.render(animation=True)

    if args.fbx_output:
        fbx = args.fbx_output.expanduser()
        fbx.parent.mkdir(parents=True, exist_ok=True)
        bpy.ops.object.select_all(action="DESELECT")
        rig.select_set(True)
        bpy.context.view_layer.objects.active = rig
        bpy.ops.export_scene.fbx(
            filepath=str(fbx),
            use_selection=True,
            object_types={"ARMATURE"},
            add_leaf_bones=False,
            bake_anim=True,
            bake_anim_use_nla_strips=False,
            bake_anim_use_all_actions=False,
            bake_anim_force_startend_keying=True,
            bake_anim_step=1.0,
            bake_anim_simplify_factor=0.0,
        )

    if args.phase_glb_dir:
        global MIRROR_CRUISE_STEM
        MIRROR_CRUISE_STEM = args.mirror_cruise_stem or ""
        export_phase_glbs(rig, output_action, splits, args.phase_glb_dir.expanduser(), args.phase_stem)

    if args.metadata:
        meta = args.metadata.expanduser()
        meta.parent.mkdir(parents=True, exist_ok=True)
        meta.write_text(
            json.dumps(
                {
                    "take": take_label,
                    "tempo": args.tempo,
                    "cycle_source_frames": cycle,
                    "contact_source_frame": contact,
                    "preview_fps": PREVIEW_FPS,
                    "splits": splits,
                    "frames": frames_keyed,
                },
                indent=2,
            )
        )
    print(f"[study] wrote {out} ({frames_keyed} frames)")


if __name__ == "__main__":
    main()
