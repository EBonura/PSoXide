"""Author a clip on the Rust Mantis rig from hand-written key poses.

    blender -b --python tools/mantis_authored_clip.py -- \
        <normalised mantis glb> <clip name> <out.glb> [--fps 12]

Why this exists. Generated motion is a human doing a human thing, and no amount
of retiming makes it read as a machine: the pose language is wrong before the
timing is. Stepping a generated idle gave a stop-motion human, not a robot. A
machine idle is a handful of deliberate silhouettes held and snapped between,
which is a few dozen numbers, so it is written here rather than sampled from a
model.

The pose table is joint rotations in the bone's OWN space, degrees, applied to
the rest pose. Bones absent from a pose keep their rest orientation, so a pose
is a diff and stays readable. The timeline holds each pose for a number of
frames and changes it in ONE frame, which is what the runtime's linear blend
turns into a snap rather than a drift.

Everything here is deliberately literal. There is no ease, no noise and no
procedural layer: an uncanny idle wants exact repetition, and the moment this
grows a random jitter it stops being reproducible and starts being a thing
nobody can re-derive.
"""

from __future__ import annotations

import math
import sys

import bpy
from mathutils import Euler

MIXAMO_PREFIX = "mixamorig:"

# Key poses. Bone -> (x, y, z) degrees in the bone's own space.
#
# The rig is a stock Mixamo humanoid: spine and legs run along Z, arms along X,
# and a pose bone's local Y is along its own bone, so Y is twist and X/Z are the
# two bends. The claw is on the LEFT arm.
POSES: dict[str, dict[str, tuple[float, float, float]]] = {
    # Read the axes off the rig with `--probe` before changing these. The rest
    # pose holds both arms straight out to the SIDES, so a pose that does not
    # bring them down reads as a scarecrow no matter what else it does. On this
    # rig, local Z swings a limb down and forward, Y twists along the bone
    # (so it is yaw for the neck), and X is the forward/back bend.
    #
    # Weight settled, both arms down and folded, head level. This is the
    # silhouette the idle returns to, so everything else reads as a deviation.
    "guard": {
        "Spine2": (10, 0, 0),
        "Neck": (6, 0, 0),
        "Head": (4, 0, 0),
        "LeftArm": (0, 0, 62),
        "LeftForeArm": (0, 0, 68),
        "RightArm": (0, 0, 56),
        "RightForeArm": (0, 0, 52),
        "LeftUpLeg": (5, 0, 0),
        "RightUpLeg": (-4, 0, 0),
        "LeftLeg": (-9, 0, 0),
        "RightLeg": (-6, 0, 0),
    },
    # Sensor sweep to its left. The head turns further than the neck follows,
    # and the body does not turn at all: a machine moves its sensor, not itself.
    "scan_left": {
        "Spine2": (10, -6, 0),
        "Neck": (6, -30, 0),
        "Head": (4, -42, 0),
        "LeftArm": (0, 0, 62),
        "LeftForeArm": (0, 0, 68),
        "RightArm": (0, 0, 56),
        "RightForeArm": (0, 0, 52),
        "LeftUpLeg": (5, 0, 0),
        "RightUpLeg": (-4, 0, 0),
        "LeftLeg": (-9, 0, 0),
        "RightLeg": (-6, 0, 0),
    },
    # And to its right, deliberately further and not a mirror: an asymmetric
    # sweep reads as a routine, a symmetric one reads as a person looking round.
    "scan_right": {
        "Spine2": (10, 5, 0),
        "Neck": (6, 34, 0),
        "Head": (2, 48, 0),
        "LeftArm": (0, 0, 58),
        "LeftForeArm": (0, 0, 74),
        "RightArm": (0, 0, 56),
        "RightForeArm": (0, 0, 52),
        "LeftUpLeg": (5, 0, 0),
        "RightUpLeg": (-4, 0, 0),
        "LeftLeg": (-9, 0, 0),
        "RightLeg": (-6, 0, 0),
    },
    # The uncanny beat: the head cocks hard sideways, which no animal idle does,
    # while the claw jerks up. Held briefly so it registers as a fault.
    "cock": {
        "Spine2": (10, 0, 5),
        "Neck": (6, -8, 26),
        "Head": (8, -12, 34),
        "LeftArm": (0, 0, 40),
        "LeftForeArm": (0, 0, 88),
        "RightArm": (0, 0, 56),
        "RightForeArm": (0, 0, 52),
        "LeftUpLeg": (5, 0, 0),
        "RightUpLeg": (-4, 0, 0),
        "LeftLeg": (-9, 0, 0),
        "RightLeg": (-6, 0, 0),
    },
}

# (pose, frames held). The last entry runs into the first, so the loop closes on
# `guard`. Uneven holds on purpose: an even beat reads as a metronome, and the
# thing that unsettles is a machine that waits slightly too long.
TIMELINE: list[tuple[str, int]] = [
    ("guard", 10),
    ("scan_left", 7),
    ("guard", 4),
    ("scan_right", 8),
    ("cock", 3),
    ("guard", 9),
]


def load_rig(path: str) -> bpy.types.Object:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=path)
    armature = next(o for o in bpy.data.objects if o.type == "ARMATURE")
    for bone in armature.data.bones:
        if bone.name.startswith(MIXAMO_PREFIX):
            bone.name = bone.name[len(MIXAMO_PREFIX) :]
    for action in list(bpy.data.actions):
        bpy.data.actions.remove(action)
    return armature


def apply_pose(rig: bpy.types.Object, pose: dict[str, tuple[float, float, float]]) -> None:
    """Set every bone: named ones to their authored angles, the rest to rest.

    Resetting the unnamed ones matters. Without it a pose inherits whatever the
    previous one left behind and the table stops describing what you see.
    """
    for bone in rig.pose.bones:
        bone.rotation_mode = "XYZ"
        degrees = pose.get(bone.name, (0.0, 0.0, 0.0))
        bone.rotation_euler = Euler([math.radians(d) for d in degrees], "XYZ")


def build_action(rig: bpy.types.Object, name: str) -> bpy.types.Action:
    rig.animation_data_create()
    action = bpy.data.actions.new(name)
    rig.animation_data.action = action
    frame = 1
    for pose_name, held in TIMELINE:
        apply_pose(rig, POSES[pose_name])
        for _ in range(held):
            for bone in rig.pose.bones:
                bone.keyframe_insert("rotation_euler", frame=frame, group=bone.name)
            frame += 1
    # Close the loop: the last stored frame repeats the first, which is how the
    # cook writes a looping clip and how the runtime knows where to blend back.
    apply_pose(rig, POSES[TIMELINE[0][0]])
    for bone in rig.pose.bones:
        bone.keyframe_insert("rotation_euler", frame=frame, group=bone.name)
    # Linear throughout. The holds are identical keys, so the only interpolation
    # left to do is the single frame where a pose changes, which is the snap.
    for curve in action.fcurves:
        for key in curve.keyframe_points:
            key.interpolation = "LINEAR"
    return action, frame


def probe_tables(bones: list[str], degrees: float) -> None:
    """Replace the tables with one pose per (bone, axis).

    Authoring blind is guesswork about which local axis bends which way, and
    guessing wrong produces poses that all look the same. This cooks a single
    clip whose frames ARE the answer: rest, then each bone rotated about X, Y
    and Z in turn, so one render tells you the mapping.
    """
    POSES.clear()
    TIMELINE.clear()
    POSES["rest"] = {}
    TIMELINE.append(("rest", 3))
    for bone in bones:
        for axis, index in (("x", 0), ("y", 1), ("z", 2)):
            angles = [0.0, 0.0, 0.0]
            angles[index] = degrees
            name = f"{bone}_{axis}"
            POSES[name] = {bone: tuple(angles)}
            TIMELINE.append((name, 3))


def main() -> None:
    argv = sys.argv[sys.argv.index("--") + 1 :]
    model, clip_name, out = argv[0], argv[1], argv[2]
    if "--probe" in argv:
        probe_tables(argv[argv.index("--probe") + 1].split(","), 55.0)

    rig = load_rig(model)
    action, last = build_action(rig, clip_name)
    scene = bpy.context.scene
    scene.frame_start, scene.frame_end = 1, last
    # Author at the rate the clip cooks at. Blender defaults to 24, and the
    # import bakes at 12, so every hold written here would be halved and the
    # one-frame snaps would land between samples and smear.
    scene.render.fps = int(argv[argv.index("--fps") + 1]) if "--fps" in argv else 12

    bpy.ops.object.select_all(action="DESELECT")
    rig.select_set(True)
    for child in rig.children_recursive:
        child.select_set(True)
    bpy.context.view_layer.objects.active = rig
    result = bpy.ops.export_scene.gltf(
        filepath=out,
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
        raise RuntimeError(f"GLB export failed: {out}")
    beats = " -> ".join(f"{p}({h})" for p, h in TIMELINE)
    print(f"AUTHORED {clip_name}: {last} frames, {beats} -> {out}")


main()
