"""Blend the accepted charge candidate with the existing forward weapon pose.
Run using Blender --background --factory-startup --python this_file.py.
Outputs a review candidate only; does not alter project bindings.
"""
from pathlib import Path
import bpy
from mathutils import Matrix, Quaternion

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'review/souls-polish/animation-candidates'
bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.render.fps = 30

def load(path):
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=str(path))
    objects = set(bpy.data.objects) - before
    rig = next(o for o in objects if o.type == 'ARMATURE')
    return rig, objects

def sample(rig, frame):
    scene.frame_set(int(frame), subframe=frame-int(frame))
    bpy.context.view_layer.update()
    return {b.name.split(':')[-1]: b.matrix_basis.copy() for b in rig.pose.bones}

rig, objects = load(OUT / 'glb/attack_02.glb')
raw = rig.animation_data.action
source_poses = [sample(rig, i+1) for i in range(96)]
aim_rig, aim_objects = load(ROOT / 'source_assets/animations/light_enemy/zenith_light.glb')
# Sample the existing forward cannon pose at 2.25 seconds (Blender frame 68.5).
sample(aim_rig, 68.5)
aim = {
    b.name.split(':')[-1]:
        (aim_rig.matrix_world @ b.matrix).to_quaternion() @
        (aim_rig.matrix_world @ b.bone.matrix_local).to_quaternion().inverted()
    for b in aim_rig.pose.bones
}
print('AIM_SOURCE_FRAMES', tuple(aim_rig.animation_data.action.frame_range))
for b in rig.data.bones:
    other = next((v for v in aim_rig.data.bones if v.name.split(':')[-1] == b.name.split(':')[-1]), None)
    if other:
        delta = b.matrix_local.inverted() @ other.matrix_local
        print('REST_MATCH', b.name, tuple(round(v,4) for v in delta.to_euler()))
for obj in aim_objects:
    bpy.data.objects.remove(obj, do_unlink=True)
# Keep generated torso/legs; settle both arms into the proven forward pose.
action = bpy.data.actions.new('Mantis Charged Shot')
rig.animation_data.action = action
arm_names = {'LeftShoulder','LeftArm','LeftForeArm','LeftHand','RightShoulder','RightArm','RightForeArm','RightHand'}
for f, pose in enumerate(source_poses, 1):
    amount = min(max((f-8)/22, 0), 1) * min(max((96-f)/18, 0), 1)
    amount = amount * amount * (3 - 2 * amount)
    posed_rotations = {}
    for bone in rig.pose.bones:
        name = bone.name.split(':')[-1]
        loc, rot, scale = pose[name].decompose()
        rest = bone.bone.matrix_local.to_quaternion()
        if bone.parent:
            parent_rest = bone.parent.bone.matrix_local.to_quaternion()
            before_basis = posed_rotations[bone.parent.name] @ parent_rest.inverted() @ rest
        else:
            before_basis = rest
        if name in arm_names and name in aim:
            world = rig.matrix_world.to_quaternion()
            kick = max((max(0.0, 1.0-abs(f-(shot+3))/3.0) for shot in (55,)), default=0.0)
            recoil = Quaternion((1,0,0), -0.10*kick) if name in {'RightArm','RightForeArm','RightHand'} else Quaternion()
            desired = world.inverted() @ recoil @ aim[name] @ world @ rest
            target = before_basis.inverted() @ desired
            rot = rot.slerp(target, amount)
        posed_rotations[bone.name] = before_basis @ rot
        bone.rotation_mode = 'QUATERNION'
        bone.location, bone.rotation_quaternion, bone.scale = loc, rot, scale
        bone.keyframe_insert('location', frame=f, group=bone.name)
        bone.keyframe_insert('rotation_quaternion', frame=f, group=bone.name)
        bone.keyframe_insert('scale', frame=f, group=bone.name)
for curve in action.fcurves:
    for key in curve.keyframe_points:key.interpolation='LINEAR'
scene.frame_start=1
scene.frame_end=96
bpy.ops.object.select_all(action='DESELECT')
for obj in objects:obj.select_set(True)
bpy.context.view_layer.objects.active=rig
bpy.ops.export_scene.gltf(filepath=str(OUT / 'glb/charge_volley_refined.glb'), export_format='GLB', use_selection=True, export_animations=True, export_animation_mode='ACTIVE_ACTIONS', export_frame_range=True, export_force_sampling=True, export_anim_single_armature=True, export_reset_pose_bones=True, export_optimize_animation_size=False)
# Save the editable Blender review scene; a turntable helper renders it next.
bpy.ops.wm.save_as_mainfile(filepath=str(OUT / 'charge_volley_refined.blend'))
print('REFINED_CHARGE_EXPORTED',96)
