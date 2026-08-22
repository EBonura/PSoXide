"""Retarget the 16 MoMask audition motions and render review clips in Blender.

Run with Blender in background mode.  The script expects the MoMask generation
folder produced from prompts.txt and writes reproducible BVH, GLB, and MP4
candidate assets beside itself.
"""

from __future__ import annotations

import math
import os
import shutil
import sys
from pathlib import Path

import bpy
from mathutils import Matrix, Vector


REPO = Path(os.environ.get("PSOXIDE_REPO", "/Users/ebonura/Desktop/repos/PSoXide"))
HERE = Path(__file__).resolve().parent
OUTPUT_DIR = Path(os.environ.get("CANDIDATE_OUTPUT_DIR", str(HERE)))
BASE_MODEL = (
    REPO
    / "editor/projects/default/source_assets/characters/tank_boss/animations/heavy_walk_pack/idle.glb"
)
MOMASK_GENERATION = Path(
    os.environ.get(
        "MOMASK_GENERATION",
        "/tmp/psoxide-tank-heavy-walk.Rs2kLP/momask-codes/generation/"
        "tank_boss_core_showcase_v1/animations",
    )
)
DEATH_MOMASK_GENERATION = Path(
    os.environ.get("DEATH_MOMASK_GENERATION", str(MOMASK_GENERATION))
)
ATTACK_MOMASK_GENERATION = Path(
    os.environ.get("ATTACK_MOMASK_GENERATION", str(MOMASK_GENERATION))
)
SOURCE_DIR = OUTPUT_DIR / "source_bvh"
GLB_DIR = OUTPUT_DIR / "glb"
RENDER_DIR = OUTPUT_DIR / "renders"
FPS = 30
GENERATION_SEED = os.environ.get("GENERATION_SEED", "260822")
DEATH_SAMPLE_MAP = [
    int(item.strip())
    for item in os.environ.get("DEATH_SAMPLE_MAP", "0,1,2,3").split(",")
]
ATTACK_SAMPLE_MAP = [
    int(item.strip())
    for item in os.environ.get("ATTACK_SAMPLE_MAP", "0,1,2,3").split(",")
]

sys.path.insert(0, str(REPO / "tools"))
import aletha_bvh_retarget as bridge  # noqa: E402


TANK_MAP = {
    "Hips": ("Hips", "delta"),
    "LeftUpLeg": ("LeftUpLeg", "delta"),
    "LeftLeg": ("LeftLeg", "delta"),
    "LeftFoot": ("LeftFoot", "delta"),
    "RightUpLeg": ("RightUpLeg", "delta"),
    "RightLeg": ("RightLeg", "delta"),
    "RightFoot": ("RightFoot", "delta"),
    "Spine": ("Spine", "torso"),
    "Spine2": ("Spine1", "torso"),
    "LeftArm": ("LeftArm", "direction"),
    "LeftForeArm": ("LeftForeArm", "direction"),
    "RightArm": ("RightArm", "direction"),
    "RightForeArm": ("RightForeArm", "direction"),
}

VARIANTS = [
    ("idle", 1, 0),
    ("idle", 2, 1),
    ("idle", 3, 2),
    ("idle", 4, 3),
    ("attack", 1, 4),
    ("attack", 2, 5),
    ("attack", 3, 6),
    ("attack", 4, 7),
    ("hit", 1, 8),
    ("hit", 2, 9),
    ("hit", 3, 10),
    ("hit", 4, 11),
    ("death", 1, 12),
    ("death", 2, 13),
    ("death", 3, 14),
    ("death", 4, 15),
]


def source_for(kind: str, sample: int) -> Path:
    if kind == "death" and DEATH_MOMASK_GENERATION != MOMASK_GENERATION:
        generation = DEATH_MOMASK_GENERATION
        generation_sample = DEATH_SAMPLE_MAP[sample - 12]
    elif kind == "attack" and ATTACK_MOMASK_GENERATION != MOMASK_GENERATION:
        generation = ATTACK_MOMASK_GENERATION
        generation_sample = ATTACK_SAMPLE_MAP[sample - 4]
    else:
        generation = MOMASK_GENERATION
        generation_sample = sample
    matches = sorted((generation / str(generation_sample)).glob("*_ik.bvh"))
    if len(matches) != 1:
        raise RuntimeError(f"Expected one IK BVH for sample {sample}, found {matches}")
    return matches[0]


def sample_pose(rig: bpy.types.Object, action: bpy.types.Action, frame: float):
    base = math.floor(frame)
    rig.animation_data.action = action
    bpy.context.scene.frame_set(base, subframe=frame - base)
    return {
        bone.name: tuple(value.copy() for value in bone.matrix_basis.decompose())
        for bone in rig.pose.bones
    }


def blend_pose(first, second, amount: float):
    result = {}
    for name in first:
        location_a, rotation_a, scale_a = first[name]
        location_b, rotation_b, scale_b = second[name]
        result[name] = (
            location_a.lerp(location_b, amount),
            rotation_a.slerp(rotation_b, amount),
            scale_a.lerp(scale_b, amount),
        )
    return result


def key_pose(rig: bpy.types.Object, action: bpy.types.Action, pose, frame: int):
    rig.animation_data.action = action
    bpy.context.scene.frame_set(frame)
    for bone in rig.pose.bones:
        location, rotation, scale = pose[bone.name]
        bone.location = location
        bone.rotation_mode = "QUATERNION"
        bone.rotation_quaternion = rotation
        bone.scale = scale
        bone.keyframe_insert("location", frame=frame, group=bone.name)
        bone.keyframe_insert("rotation_quaternion", frame=frame, group=bone.name)
        bone.keyframe_insert("scale", frame=frame, group=bone.name)


def linear(action: bpy.types.Action):
    for curve in action.fcurves:
        for key in curve.keyframe_points:
            key.interpolation = "LINEAR"


def normalized_action(
    rig: bpy.types.Object,
    raw: bpy.types.Action,
    kind: str,
    name: str,
) -> tuple[bpy.types.Action, int]:
    source_first = int(math.ceil(raw.frame_range[0]))
    source_last = int(math.floor(raw.frame_range[1]))
    source_span = max(source_last - source_first, 1)
    first_pose = sample_pose(rig, raw, source_first)

    if kind == "idle":
        intervals = round(4.0 * FPS)
    else:
        intervals = source_span

    action = bpy.data.actions.new(name)
    for index in range(intervals + 1):
        phase = index / intervals
        source_frame = source_first + phase * source_span
        pose = sample_pose(rig, raw, source_frame)
        if kind == "idle" and phase >= 0.78:
            blend = (phase - 0.78) / 0.22
            blend = blend * blend * (3.0 - 2.0 * blend)
            pose = blend_pose(pose, first_pose, blend)
        if kind == "idle" and index == intervals:
            pose = first_pose
        key_pose(rig, action, pose, index + 1)

    if kind == "death":
        final_pose = sample_pose(rig, action, intervals + 1)
        hold = round(0.65 * FPS)
        for extra in range(1, hold + 1):
            key_pose(rig, action, final_pose, intervals + 1 + extra)
        intervals += hold

    linear(action)
    return action, intervals + 1


def keep_feet_above_starting_floor(
    rig: bpy.types.Object, action: bpy.types.Action, frame_count: int
):
    rig.animation_data.action = action
    bpy.context.scene.frame_set(1)
    floor = min(
        (rig.matrix_world @ rig.pose.bones["LeftFoot"].head).z,
        (rig.matrix_world @ rig.pose.bones["RightFoot"].head).z,
    )
    hips = rig.pose.bones["Hips"]
    for frame in range(1, frame_count + 1):
        bpy.context.scene.frame_set(frame)
        current = min(
            (rig.matrix_world @ rig.pose.bones["LeftFoot"].head).z,
            (rig.matrix_world @ rig.pose.bones["RightFoot"].head).z,
        )
        correction = max(0.0, floor - current)
        if correction > 0.0:
            # This rig's local Y is its vertical translation axis.
            hips.location.y += correction
            hips.keyframe_insert("location", frame=frame, group=hips.name)
    linear(action)


def export_glb(rig, action, path: Path, frame_count: int):
    scene = bpy.context.scene
    rig.animation_data.action = action
    scene.frame_start = 1
    scene.frame_end = frame_count
    bpy.ops.object.select_all(action="DESELECT")
    rig.select_set(True)
    for child in rig.children_recursive:
        child.select_set(True)
    bpy.context.view_layer.objects.active = rig
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
        raise RuntimeError(f"GLB export failed for {path}")


def add_floor(scene, center: Vector, height: float):
    bpy.ops.mesh.primitive_plane_add(size=height * 5.0, location=(center.x, center.y, 0.0))
    floor = bpy.context.object
    floor.name = "PreviewFloor"
    floor.color = (0.055, 0.065, 0.08, 1.0)


def render_preview(rig, action, path: Path, frame_count: int):
    scene = bpy.context.scene
    meshes = [obj for obj in bpy.data.objects if obj.type == "MESH"]
    scene.frame_start = 1
    scene.frame_end = frame_count
    scene.frame_set(1)
    points = [obj.matrix_world @ Vector(corner) for obj in meshes for corner in obj.bound_box]
    low = Vector(tuple(min(point[axis] for point in points) for axis in range(3)))
    high = Vector(tuple(max(point[axis] for point in points) for axis in range(3)))
    center = (low + high) * 0.5
    height = max(high.z - low.z, 0.01)

    add_floor(scene, center, height)
    camera_data = bpy.data.cameras.new("PreviewCamera")
    camera = bpy.data.objects.new("PreviewCamera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.type = "ORTHO"
    camera_data.ortho_scale = height * 1.55
    target = center + Vector((0.0, 0.0, height * 0.02))
    camera.location = target + Vector((height * 0.82, -height * 2.1, height * 0.42))
    camera.rotation_euler = (target - camera.location).to_track_quat("-Z", "Y").to_euler()

    scene.render.engine = "BLENDER_WORKBENCH"
    scene.display.shading.light = "STUDIO"
    scene.display.shading.color_type = "TEXTURE"
    scene.display.shading.show_shadows = True
    scene.display.shading.show_cavity = True
    scene.display.shading.cavity_type = "BOTH"
    scene.display.shading.background_type = "VIEWPORT"
    scene.display.shading.background_color = (0.018, 0.022, 0.03)
    scene.render.resolution_x = 512
    scene.render.resolution_y = 512
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "FFMPEG"
    scene.render.ffmpeg.format = "MPEG4"
    scene.render.ffmpeg.codec = "H264"
    scene.render.ffmpeg.constant_rate_factor = "MEDIUM"
    scene.render.ffmpeg.ffmpeg_preset = "GOOD"
    scene.render.filepath = str(path)
    rig.animation_data.action = action
    bpy.ops.render.render(animation=True)


def build(kind: str, number: int, sample: int):
    name = f"{kind}_{number:02d}"
    source_path = source_for(kind, sample)
    SOURCE_DIR.mkdir(parents=True, exist_ok=True)
    GLB_DIR.mkdir(parents=True, exist_ok=True)
    RENDER_DIR.mkdir(parents=True, exist_ok=True)
    copied_source = SOURCE_DIR / f"{name}_momask_seed_{GENERATION_SEED}.bvh"
    shutil.copy2(source_path, copied_source)

    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.render.fps = FPS
    bpy.ops.import_scene.gltf(filepath=str(BASE_MODEL))
    rig = next(obj for obj in bpy.data.objects if obj.type == "ARMATURE")
    if rig.animation_data is None:
        rig.animation_data_create()
    rig.animation_data.action = None
    for bone in rig.pose.bones:
        bone.matrix_basis = Matrix.Identity(4)
        bone.rotation_mode = "QUATERNION"
    for old_action in list(bpy.data.actions):
        bpy.data.actions.remove(old_action)

    source = bridge.import_animation_source(copied_source)
    source_action = source.animation_data.action
    source_first = int(math.ceil(source_action.frame_range[0]))
    source_last = int(math.floor(source_action.frame_range[1]))
    removed_yaw = bridge.face_forward(source, source_first, source_last)
    bridge.JOINT_MAP = TANK_MAP
    raw, _speed = bridge.retarget(
        source,
        rig,
        f"{name}_raw",
        source_first,
        source_last,
        smooth=True,
    )
    action, frame_count = normalized_action(rig, raw, kind, name)
    # Preserve generated root descent for falls; standing motions need the
    # regular foot-floor clamp.
    if kind != "death":
        keep_feet_above_starting_floor(rig, action, frame_count)

    bpy.data.objects.remove(source, do_unlink=True)
    for old_action in list(bpy.data.actions):
        if old_action is not action:
            bpy.data.actions.remove(old_action)
    rig.animation_data.action = action
    export_glb(rig, action, GLB_DIR / f"{name}.glb", frame_count)
    render_preview(rig, action, RENDER_DIR / f"{name}.mp4", frame_count)
    print(
        "TANK_CORE_CANDIDATE",
        name,
        f"frames={frame_count}",
        f"duration={(frame_count - 1) / FPS:.2f}s",
        f"yaw_removed={removed_yaw:.1f}",
    )


enabled_kinds = {
    item.strip()
    for item in os.environ.get("CANDIDATE_KINDS", "idle,attack,hit,death").split(",")
    if item.strip()
}
enabled_numbers = {
    int(item.strip())
    for item in os.environ.get("CANDIDATE_NUMBERS", "1,2,3,4").split(",")
    if item.strip()
}
for variant in VARIANTS:
    if variant[0] not in enabled_kinds or variant[1] not in enabled_numbers:
        continue
    build(*variant)

print("TANK_CORE_SHOWCASE_CANDIDATES_COMPLETE", OUTPUT_DIR)
