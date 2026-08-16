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
from aletha_bvh_retarget import gait_window, import_bvh, retarget  # noqa: E402

PREVIEW_FPS = 30


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path, help="Rigged GLB with takes")
    parser.add_argument("--take", help="Gait action name in the GLB (artist take)")
    parser.add_argument(
        "--gait-bvh", type=Path, help="Retarget this MoMask/HumanML3D BVH onto the rig as the gait"
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
    parser.add_argument("--metadata", type=Path, help="Phase-range JSON")
    parser.add_argument("--tempo", type=float, default=1.35, help="Gait speed-up baked in")
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
    mean distance to the unshifted signal, searched in 25%..75% of the take."""
    best_lag, best_score = 0, float("inf")
    for lag in range(frames // 4, (frames * 3) // 4):
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


def smoothstep(v: float) -> float:
    v = max(0.0, min(1.0, v))
    return v * v * (3.0 - 2.0 * v)


def export_phase_glbs(
    rig: bpy.types.Object,
    assembled: bpy.types.Action,
    splits: dict[str, list[int]],
    out_dir: Path,
    stem: str,
) -> None:
    """Slice the assembled clip into per-phase actions (rebased to frame 1)
    and export each as its own GLB: the locomotion importer cooks one take per
    file. The cruise is exactly the loop cycle."""
    out_dir.mkdir(parents=True, exist_ok=True)
    scene = bpy.context.scene
    phases = {
        f"{stem}_windup": splits["windup"],
        stem: splits["cruise_loop"],
        f"{stem}_winddown": splits["winddown"],
    }
    for name, (first, last) in phases.items():
        poses = [sample_pose(rig, assembled, frame) for frame in range(first, last + 1)]
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

    # Mirrored winddown: the same stop from the other foot, for the half-stride
    # phase. Mirrored in WORLD space across the sagittal plane (x = 0, the rig
    # is centred and the gait is in place) and left/right bones swapped, so it
    # does not depend on Blender's bone-roll mirror convention.
    first, last = splits["winddown"]
    poses = [sample_pose(rig, assembled, frame) for frame in range(first, last + 1)]
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
    name = f"{stem}_winddown_mirror"
    action = bpy.data.actions.new(name)
    world_inv = rig.matrix_world.inverted()
    for index, pose in enumerate(poses):
        # Pose the rig with the original frame and read every bone's world matrix.
        rig.animation_data.action = assembled
        set_frame(first + index)
        world = {b.name: (rig.matrix_world @ b.matrix).copy() for b in rig.pose.bones}
        rig.animation_data.action = action
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
        src = import_bvh(args.gait_bvh.expanduser())
        src_action = src.animation_data.action
        src_first, src_last = int(src_action.frame_range[0]), int(src_action.frame_range[1])
        window = tuple(args.gait_window) if args.gait_window else gait_window(src, src_first, src_last)
        gait, speed = retarget(src, rig, "generated_gait", window[0], window[1])
        gait_first, gait_frames = window[0], max(1, window[1] - window[0])
        take_label = args.gait_bvh.name
        print(f"[study] bvh window={window} of {src_first}..{src_last} source_speed={speed:.3f} m/s")
    elif args.take:
        gait = action_named(args.take)
        gait_first = int(gait.frame_range[0])
        gait_frames = int(gait.frame_range[1] - gait.frame_range[0]) or 1
        if args.gait_window:
            gait_first, gait_frames = args.gait_window[0], max(1, args.gait_window[1] - args.gait_window[0])
    else:
        raise SystemExit("need --take or --gait-bvh")

    hips = bone_by_suffix(rig, "hips")
    left = bone_by_suffix(rig, "leftfoot")
    right = bone_by_suffix(rig, "rightfoot")

    feet = {
        "left": world_positions(rig, gait, left, gait_first, gait_frames),
        "right": world_positions(rig, gait, right, gait_first, gait_frames),
    }
    hips_track = {"hips": world_positions(rig, gait, hips, gait_first, gait_frames)}
    cycle = detect_cycle({**feet, **hips_track}, gait_frames)
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
    camera.location = center + Vector((1.3, -1.9, 0.35)) * height
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
