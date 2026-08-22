"""Build four Rust Mantis backward-walk auditions from local MoMask BVHs.

Run with Blender in background mode. Raw generated takes are copied beside the
outputs so the candidate pack remains reproducible after the local MoMask
checkout is removed.
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
BASE_MODEL = REPO / "editor/projects/default/source_assets/characters/rust_mantis.glb"
MOMASK = Path(
    os.environ.get(
        "MOMASK_ROOT",
        "/Users/ebonura/Desktop/Bonnie Studios/MoMask/momask-codes",
    )
)
SOURCE_DIR = HERE / "source_bvh"
GLB_DIR = HERE / "glb"
RENDER_DIR = HERE / "renders"
FPS = 30
PREVIEW_FRAMES = 6 * FPS

sys.path.insert(0, str(REPO / "tools"))
import aletha_bvh_retarget as bridge  # noqa: E402
import aletha_walk_study as study  # noqa: E402


MANTIS_MAP = {
    "Hips": ("Hips", "delta"),
    "LeftUpLeg": ("LeftUpLeg", "delta"),
    "LeftLeg": ("LeftLeg", "delta"),
    "LeftFoot": ("LeftFoot", "delta"),
    "RightUpLeg": ("RightUpLeg", "delta"),
    "RightLeg": ("RightLeg", "delta"),
    "RightFoot": ("RightFoot", "delta"),
    "Spine": ("Spine", "torso"),
    "Spine2": ("Spine2", "torso"),
    "Neck": ("Neck", "neck"),
    "LeftArm": ("LeftArm", "direction"),
    "LeftForeArm": ("LeftForeArm", "direction"),
    "RightArm": ("RightArm", "direction"),
    "RightForeArm": ("RightForeArm", "direction"),
}

# number, generation family, repeat, declared MoMask length, seed
CANDIDATES = [
    (1, "bk_a", 0, 140, 811),
    (2, "bk_c", 2, 140, 833),
    (3, "lat_back", 2, 120, 501),
    (4, "bk_a", 1, 140, 811),
]


def origin_bvh(family: str, repeat: int, length: int) -> Path:
    return (
        MOMASK
        / "generation"
        / family
        / "animations"
        / "0"
        / f"sample0_repeat{repeat}_len{length}_ik.bvh"
    )


def local_bvh(number: int, family: str, repeat: int, seed: int) -> Path:
    SOURCE_DIR.mkdir(parents=True, exist_ok=True)
    path = SOURCE_DIR / f"backward_{number:02d}_{family}_seed_{seed}_repeat_{repeat}.bvh"
    if not path.exists():
        source = origin_bvh(family, repeat, next(c[3] for c in CANDIDATES if c[0] == number))
        if not source.exists():
            raise RuntimeError(f"Missing MoMask source: {source}")
        shutil.copy2(source, path)
    return path


def import_mantis() -> bpy.types.Object:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.context.scene.render.fps = FPS
    result = bpy.ops.import_scene.gltf(filepath=str(BASE_MODEL))
    if "FINISHED" not in result:
        raise RuntimeError(f"Could not import {BASE_MODEL}")
    rig = next(obj for obj in bpy.data.objects if obj.type == "ARMATURE")
    rig.name = "RustMantisRig"

    bpy.ops.object.select_all(action="DESELECT")
    for obj in bpy.data.objects:
        if obj.type in {"ARMATURE", "MESH"}:
            obj.select_set(True)
    bpy.context.view_layer.objects.active = rig
    bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)

    for bone in rig.data.bones:
        if bone.name.startswith(bridge.MIXAMO_PREFIX):
            bone.name = bone.name[len(bridge.MIXAMO_PREFIX) :]
    for action in list(bpy.data.actions):
        bpy.data.actions.remove(action)
    if rig.animation_data is None:
        rig.animation_data_create()
    for bone in rig.pose.bones:
        bone.matrix_basis = Matrix.Identity(4)
        bone.rotation_mode = "QUATERNION"
    return rig


def key_pose(rig: bpy.types.Object, action: bpy.types.Action, pose, frame: int) -> None:
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


def linear(action: bpy.types.Action) -> None:
    for curve in action.fcurves:
        for key in curve.keyframe_points:
            key.interpolation = "LINEAR"


def cyclic_poses(
    rig: bpy.types.Object,
    action: bpy.types.Action,
    first: int,
    cycle: int,
    contact: int,
):
    poses = []
    for offset in range(cycle):
        phase = (contact + offset) % cycle
        poses.append(study.sample_pose(rig, action, first + phase))
    return poses


def action_from_poses(
    rig: bpy.types.Object,
    name: str,
    poses,
    frames: int | None = None,
) -> bpy.types.Action:
    action = bpy.data.actions.new(name)
    cycle = len(poses)
    count = frames if frames is not None else cycle + 1
    for index in range(count):
        key_pose(rig, action, poses[index % cycle], index + 1)
    linear(action)
    return action


def export_glb(rig: bpy.types.Object, action: bpy.types.Action, path: Path, frames: int) -> None:
    scene = bpy.context.scene
    rig.animation_data.action = action
    scene.frame_start, scene.frame_end = 1, frames
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
        raise RuntimeError(f"GLB export failed: {path}")


def render_preview(rig: bpy.types.Object, action: bpy.types.Action, path: Path) -> None:
    scene = bpy.context.scene
    rig.animation_data.action = action
    scene.frame_start, scene.frame_end = 1, PREVIEW_FRAMES
    scene.frame_set(1)
    bpy.context.view_layer.update()
    meshes = [obj for obj in rig.children_recursive if obj.type == "MESH"]
    points = [obj.matrix_world @ Vector(corner) for obj in meshes for corner in obj.bound_box]
    low = Vector(tuple(min(point[axis] for point in points) for axis in range(3)))
    high = Vector(tuple(max(point[axis] for point in points) for axis in range(3)))
    center = (low + high) * 0.5
    height = max(high.z - low.z, 0.01)

    bpy.ops.mesh.primitive_plane_add(size=height * 5.0, location=(center.x, center.y, low.z))
    floor = bpy.context.object
    floor.name = "PreviewFloor"
    floor.color = (0.045, 0.055, 0.068, 1.0)

    camera_data = bpy.data.cameras.new("PreviewCamera")
    camera = bpy.data.objects.new("PreviewCamera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.type = "ORTHO"
    camera_data.ortho_scale = height * 1.52
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
    bpy.ops.render.render(animation=True)


def build(number: int, family: str, repeat: int, length: int, seed: int) -> None:
    name = f"backward_{number:02d}"
    source_path = local_bvh(number, family, repeat, seed)
    GLB_DIR.mkdir(parents=True, exist_ok=True)
    RENDER_DIR.mkdir(parents=True, exist_ok=True)

    rig = import_mantis()
    source = bridge.import_animation_source(source_path)
    source_action = source.animation_data.action
    source_first = int(math.ceil(source_action.frame_range[0]))
    source_last = int(math.floor(source_action.frame_range[1]))
    moving_first, moving_last = bridge.gait_window(source, source_first, source_last)
    removed_yaw = bridge.face_forward(source, moving_first, moving_last)
    bridge.JOINT_MAP = MANTIS_MAP
    raw, speed = bridge.retarget(
        source,
        rig,
        f"{name}_raw",
        moving_first,
        moving_last,
        smooth=True,
    )
    # Character movement owns translation. The established Mantis locomotion
    # bridge also removes the generated hips bob for a stable in-place clip.
    for curve in list(raw.fcurves):
        if curve.data_path.endswith("location"):
            raw.fcurves.remove(curve)

    gait_frames = moving_last - moving_first + 1
    feet = {
        "left": study.world_positions(rig, raw, "LeftFoot", moving_first, gait_frames),
        "right": study.world_positions(rig, raw, "RightFoot", moving_first, gait_frames),
    }
    hips = study.world_positions(rig, raw, "Hips", moving_first, gait_frames)
    cycle = study.detect_cycle({**feet, "hips": hips}, gait_frames)
    cycle = max(20, min(cycle, gait_frames - 1))
    loop_at = gait_frames - cycle
    loop_feet = {side: values[loop_at : loop_at + cycle] for side, values in feet.items()}
    contact = study.contact_frame(loop_feet, cycle)
    loop_first = moving_first + loop_at
    poses = cyclic_poses(rig, raw, loop_first, cycle, contact)

    loop = action_from_poses(rig, name, poses)
    preview = action_from_poses(rig, f"{name}_preview", poses, frames=PREVIEW_FRAMES)
    bpy.data.objects.remove(source, do_unlink=True)

    export_glb(rig, loop, GLB_DIR / f"{name}.glb", cycle + 1)
    render_preview(rig, preview, RENDER_DIR / f"{name}.mp4")
    print(
        "MANTIS_BACKWARD_CANDIDATE",
        name,
        f"source={family}/repeat{repeat}",
        f"window={moving_first}..{moving_last}",
        f"cycle={cycle}",
        f"duration={cycle / FPS:.2f}s",
        f"source_speed={speed:.2f}m/s",
        f"yaw_removed={removed_yaw:+.1f}deg",
    )


enabled = {
    int(item.strip())
    for item in os.environ.get("CANDIDATE_NUMBERS", "1,2,3,4").split(",")
    if item.strip()
}
for candidate in CANDIDATES:
    if candidate[0] in enabled:
        build(*candidate)

print("MANTIS_BACKWARD_CANDIDATES_COMPLETE", HERE)
