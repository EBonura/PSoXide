"""Author the Heavy Enemy's three-chimney missile-salvo animation.

This is deliberately mechanical instead of human motion-capture: the three
chimneys are skinned to the shared upper shell (`Spine1`), so the readable
motion is a planted brace followed by three staggered shell recoil beats.  The
three release frames are printed at the end for later projectile-event setup.

Run with Blender in background mode:

    Blender --background --python build_missile_salvo.py
"""

from __future__ import annotations

import math
from pathlib import Path

import bpy
from mathutils import Matrix, Quaternion, Vector


HERE = Path(__file__).resolve().parent
BASE_MODEL = HERE.parent / "heavy_walk_pack" / "idle.glb"
OUTPUT_GLB = HERE / "ranged_attack.glb"
OUTPUT_PREVIEW = HERE / "ranged_attack_preview.mp4"

FPS = 30
FRAME_END = 78
RELEASE_FRAMES = (31, 39, 47)


def sample_pose(rig: bpy.types.Object, frame: float):
    base = math.floor(frame)
    bpy.context.scene.frame_set(base, subframe=frame - base)
    return {
        bone.name: tuple(value.copy() for value in bone.matrix_basis.decompose())
        for bone in rig.pose.bones
    }


def copy_pose(pose):
    return {
        name: (location.copy(), rotation.copy(), scale.copy())
        for name, (location, rotation, scale) in pose.items()
    }


def rotate(pose, bone_name: str, x: float = 0.0, y: float = 0.0, z: float = 0.0):
    location, rotation, scale = pose[bone_name]
    delta = Quaternion((1.0, 0.0, 0.0), math.radians(x))
    delta @= Quaternion((0.0, 1.0, 0.0), math.radians(y))
    delta @= Quaternion((0.0, 0.0, 1.0), math.radians(z))
    pose[bone_name] = (location, rotation @ delta, scale)


def offset(pose, bone_name: str, x: float = 0.0, y: float = 0.0, z: float = 0.0):
    location, rotation, scale = pose[bone_name]
    pose[bone_name] = (location + Vector((x, y, z)), rotation, scale)


def make_pose(
    rest,
    *,
    spine_pitch: float = 0.0,
    shell_pitch: float = 0.0,
    shell_roll: float = 0.0,
    shell_drop: float = 0.0,
    shoulder_brace: float = 0.0,
):
    pose = copy_pose(rest)
    rotate(pose, "Spine", x=spine_pitch)
    rotate(pose, "Spine1", x=shell_pitch, z=shell_roll)
    # Local Y follows this rig's vertical bone axis.  A very small translation
    # gives the upper armour real mass without moving the root or either foot.
    offset(pose, "Spine1", y=shell_drop)
    rotate(pose, "LeftShoulder", z=shoulder_brace)
    rotate(pose, "RightShoulder", z=-shoulder_brace)
    return pose


def blend_pose(first, second, amount: float):
    amount = amount * amount * (3.0 - 2.0 * amount)
    return {
        name: (
            first[name][0].lerp(second[name][0], amount),
            first[name][1].slerp(second[name][1], amount),
            first[name][2].lerp(second[name][2], amount),
        )
        for name in first
    }


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


def pose_at_frame(controls, frame: int):
    for index in range(len(controls) - 1):
        first_frame, first_pose = controls[index]
        second_frame, second_pose = controls[index + 1]
        if frame <= second_frame:
            span = max(second_frame - first_frame, 1)
            return blend_pose(first_pose, second_pose, (frame - first_frame) / span)
    return controls[-1][1]


def export_glb(rig, action, path: Path):
    scene = bpy.context.scene
    rig.animation_data.action = action
    scene.frame_start = 1
    scene.frame_end = FRAME_END
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


def render_preview(rig, action, path: Path):
    scene = bpy.context.scene
    scene.frame_set(1)
    meshes = [obj for obj in bpy.data.objects if obj.type == "MESH"]
    points = [obj.matrix_world @ Vector(corner) for obj in meshes for corner in obj.bound_box]
    low = Vector(tuple(min(point[axis] for point in points) for axis in range(3)))
    high = Vector(tuple(max(point[axis] for point in points) for axis in range(3)))
    center = (low + high) * 0.5
    height = max(high.z - low.z, 0.01)

    bpy.ops.mesh.primitive_plane_add(size=height * 5.0, location=(center.x, center.y, 0.0))
    floor = bpy.context.object
    floor.name = "PreviewFloor"
    floor.color = (0.055, 0.065, 0.08, 1.0)

    camera_data = bpy.data.cameras.new("PreviewCamera")
    camera = bpy.data.objects.new("PreviewCamera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.type = "ORTHO"
    camera_data.ortho_scale = height * 1.55
    target = center + Vector((0.0, 0.0, height * 0.04))
    camera.location = target + Vector((height * 0.80, -height * 2.15, height * 0.48))
    camera.rotation_euler = (target - camera.location).to_track_quat("-Z", "Y").to_euler()

    scene.render.engine = "BLENDER_WORKBENCH"
    scene.display.shading.light = "STUDIO"
    scene.display.shading.color_type = "TEXTURE"
    scene.display.shading.show_shadows = True
    scene.display.shading.show_cavity = True
    scene.display.shading.cavity_type = "BOTH"
    scene.display.shading.background_type = "VIEWPORT"
    scene.display.shading.background_color = (0.018, 0.022, 0.03)
    scene.render.resolution_x = 640
    scene.render.resolution_y = 480
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "FFMPEG"
    scene.render.ffmpeg.format = "MPEG4"
    scene.render.ffmpeg.codec = "H264"
    scene.render.ffmpeg.constant_rate_factor = "MEDIUM"
    scene.render.ffmpeg.ffmpeg_preset = "GOOD"
    scene.render.filepath = str(path)
    scene.render.fps = FPS
    scene.frame_start = 1
    scene.frame_end = FRAME_END
    rig.animation_data.action = action
    bpy.ops.render.render(animation=True)


bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.render.fps = FPS
bpy.ops.import_scene.gltf(filepath=str(BASE_MODEL))
rig = next(obj for obj in bpy.data.objects if obj.type == "ARMATURE")
if rig.animation_data is None:
    rig.animation_data_create()

source_action = rig.animation_data.action
scene.frame_set(max(1, int(math.ceil(source_action.frame_range[0]))))
rest = sample_pose(rig, scene.frame_current)

for old_action in list(bpy.data.actions):
    bpy.data.actions.remove(old_action)
for bone in rig.pose.bones:
    bone.matrix_basis = Matrix.Identity(4)
    bone.rotation_mode = "QUATERNION"

idle = make_pose(rest)
brace = make_pose(
    rest,
    spine_pitch=-3.5,
    shell_pitch=-8.0,
    shell_drop=-0.025,
    shoulder_brace=3.0,
)
loaded = make_pose(
    rest,
    spine_pitch=-4.5,
    shell_pitch=-12.0,
    shell_drop=-0.04,
    shoulder_brace=4.0,
)
recoil_left = make_pose(
    rest,
    spine_pitch=2.0,
    shell_pitch=10.0,
    shell_roll=4.5,
    shell_drop=0.015,
    shoulder_brace=5.0,
)
recover_left = make_pose(
    rest,
    spine_pitch=-3.0,
    shell_pitch=-7.0,
    shell_roll=1.5,
    shell_drop=-0.02,
    shoulder_brace=4.0,
)
recoil_centre = make_pose(
    rest,
    spine_pitch=2.5,
    shell_pitch=12.0,
    shell_drop=0.02,
    shoulder_brace=5.5,
)
recover_centre = make_pose(
    rest,
    spine_pitch=-3.0,
    shell_pitch=-7.0,
    shell_drop=-0.02,
    shoulder_brace=4.0,
)
recoil_right = make_pose(
    rest,
    spine_pitch=2.0,
    shell_pitch=10.0,
    shell_roll=-4.5,
    shell_drop=0.015,
    shoulder_brace=5.0,
)
recover_right = make_pose(
    rest,
    spine_pitch=-2.5,
    shell_pitch=-6.0,
    shell_roll=-1.0,
    shell_drop=-0.018,
    shoulder_brace=3.5,
)
aftershock = make_pose(
    rest,
    spine_pitch=1.0,
    shell_pitch=3.0,
    shell_drop=0.005,
    shoulder_brace=2.0,
)

controls = [
    (1, idle),
    (8, idle),
    (22, brace),
    (29, loaded),
    (31, recoil_left),
    (35, recover_left),
    (39, recoil_centre),
    (43, recover_centre),
    (47, recoil_right),
    (52, recover_right),
    (60, aftershock),
    (72, idle),
    (78, idle),
]

action = bpy.data.actions.new("TankRangedMissileSalvo")
for frame in range(1, FRAME_END + 1):
    key_pose(rig, action, pose_at_frame(controls, frame), frame)
for curve in action.fcurves:
    for key in curve.keyframe_points:
        key.interpolation = "LINEAR"

rig.animation_data.action = action
export_glb(rig, action, OUTPUT_GLB)
render_preview(rig, action, OUTPUT_PREVIEW)
print(
    "TANK_MISSILE_SALVO_COMPLETE",
    f"frames={FRAME_END}",
    f"duration={(FRAME_END - 1) / FPS:.2f}s",
    f"release_frames={','.join(map(str, RELEASE_FRAMES))}",
    f"glb={OUTPUT_GLB}",
    f"preview={OUTPUT_PREVIEW}",
)
