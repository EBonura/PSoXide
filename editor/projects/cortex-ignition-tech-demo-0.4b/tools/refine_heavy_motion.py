"""Cut compact alert and foot-adjustment clips from the generated review takes."""
from pathlib import Path
import bpy

ROOT=Path(__file__).resolve().parents[1]
REVIEW=ROOT/'review/souls-polish/heavy-motion-candidates'
OUT=ROOT/'source_assets/animations/heavy_enemy'
OUT.mkdir(parents=True,exist_ok=True)
for name,source,first,last,intervals in [('alert','alert_01',72,150,42),('turn','turn_02',1,61,30)]:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene=bpy.context.scene;scene.render.fps=30
    bpy.ops.import_scene.gltf(filepath=str(REVIEW/'glb'/f'{source}.glb'))
    rig=next(o for o in bpy.data.objects if o.type=='ARMATURE')
    raw=rig.animation_data.action
    poses=[]
    for i in range(intervals+1):
        t=first+(last-first)*i/intervals;scene.frame_set(int(t),subframe=t-int(t))
        poses.append({b.name:b.matrix_basis.decompose() for b in rig.pose.bones})
    action=bpy.data.actions.new('Heavy '+name.title());rig.animation_data.action=action
    for i,pose in enumerate(poses):
        seam=max(0.0,(i/intervals-.78)/.22) if name=='turn' else 0
        seam=seam*seam*(3-2*seam)
        for bone in rig.pose.bones:
            loc,rot,scale=pose[bone.name];loc0,rot0,scale0=poses[0][bone.name]
            bone.rotation_mode='QUATERNION'
            bone.location=loc.lerp(loc0,seam);bone.rotation_quaternion=rot.slerp(rot0,seam);bone.scale=scale.lerp(scale0,seam)
            for field in ('location','rotation_quaternion','scale'):bone.keyframe_insert(field,frame=i+1,group=bone.name)
    for curve in action.fcurves:
        for key in curve.keyframe_points:key.interpolation='LINEAR'
    scene.frame_start=1;scene.frame_end=intervals+1
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.export_scene.gltf(filepath=str(OUT/f'{name}.glb'),export_format='GLB',use_selection=True,export_animations=True,export_animation_mode='ACTIVE_ACTIONS',export_frame_range=True,export_force_sampling=True,export_anim_single_armature=True,export_reset_pose_bones=True,export_optimize_animation_size=False)
    print('HEAVY_REFINED',name,intervals+1)
