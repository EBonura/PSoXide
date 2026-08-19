"""Retarget a MoMask / HumanML3D BVH onto the Rust Mantis rig, in Blender.

    blender -b --python tools/mantis_bvh_retarget.py -- \
        <repo> <mantis.fbx> <take.bvh> <action name> <out.blend> \
        [max frames] [start frame]

Take a SETTLED window, not the head of the file. Generated takes begin from
rest and spend their opening on a wind-up: the walk take here travels 0.02 m/s
over its first 60 frames and 0.50 m/s over its whole length, so sampling from
frame 1 cooks a shuffle and calls it a walk. The reported source speed is the
check.

The mantis carries a stock Mixamo humanoid armature, so once the `mixamorig:`
prefix is stripped its bones answer to the same names `aletha_bvh_retarget`'s
joint map already speaks. This module therefore loads that one and swaps only
the map: the METHODS are the valuable part and they transfer unchanged.

Why not something simpler. The two rests are nowhere near each other, measured
per bone: the hips are inverted 154 degrees, and the arms point the same way
(2-7 degrees apart) while their rest ROLL differs by about 258 degrees, the
usual Mixamo-FBX versus BVH axis convention. So:

  * copying absolute world rotations (a Copy Rotation constraint) injects every
    one of those numbers as permanent distortion, and no amount of clean baking
    saves it;
  * taking the delta is right for hips, legs and feet, where the rests differ
    mostly in roll and the delta cancels it;
  * the arms need direction-aiming, which points the target bone along the
    source bone's world direction while keeping the target's OWN rest roll.
    That is what neutralises the 258 degrees;
  * the spine needs the torso treated as one rigid frame, because a template's
    spine rest is a zig-zag and per-bone deltas transplant that zig-zag as sag.

One implementation detail is worth repeating because getting it wrong is
invisible until it is catastrophic: a bone's target matrix must take its
CURRENTLY EVALUATED head position, not its rest position. Pinning a child to
its rest head detaches it from the parent the moment the parent rotates, which
looks like limbs flying off on their own.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import bpy

# Same methods as the Aletha bridge; both sides speak bare Mixamo names here.
MANTIS_JOINT_MAP: dict[str, tuple[str, str]] = {
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


def load_aletha_module(repo: Path):
    """Import the proven retargeter as a module, not a copy of its maths."""
    spec = importlib.util.spec_from_file_location(
        "aletha_bvh_retarget", str(repo / "tools" / "aletha_bvh_retarget.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def import_mantis(fbx_path: str, prefix: str) -> bpy.types.Object:
    """Take the NORMALISED glb, not the raw FBX.

    In the FBX the mesh object sits at scale 100 and the armature at 0.01, and
    that mismatch survives retargeting: cooked clips came out with pose
    translations of 2,827,904 against roughly 20,000 for a known-good Aletha
    clip, 130x too large, which drove the cooked clip bounds to a floor 154714
    units below a model only 18616 units tall and threw the character clean out
    of frame. A glTF round trip puts mesh and rig in one space.
    """
    bpy.ops.wm.read_factory_settings(use_empty=True)
    if fbx_path.lower().endswith((".glb", ".gltf")):
        bpy.ops.import_scene.gltf(filepath=fbx_path)
    else:
        bpy.ops.import_scene.fbx(filepath=fbx_path)
    armature = next(o for o in bpy.data.objects if o.type == "ARMATURE")
    armature.name = "MantisRig"
    # Bake the object scale into the bone data. The cooker reads local bone
    # positions and ignores node transforms, the same blind spot that put the
    # mesh on its back; left alone, an armature sitting at 0.01 scale beside a
    # mesh at 100 cooks pose translations ~94x too large.
    bpy.ops.object.select_all(action="DESELECT")
    for obj in bpy.data.objects:
        if obj.type in {"ARMATURE", "MESH"}:
            obj.select_set(True)
    bpy.context.view_layer.objects.active = armature
    bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)
    for action in list(bpy.data.actions):
        bpy.data.actions.remove(action)
    for bone in armature.data.bones:
        if bone.name.startswith(prefix):
            bone.name = bone.name[len(prefix) :]
    return armature


def mirror_action(action) -> None:
    """Flip a baked action left-to-right, in place.

    Generated side-steps are unreliable: all three "steps to their left" takes
    turned the body instead of staying square, while the right-hand take was
    clean. Mirroring the good one gives a matched pair for free and guarantees
    the two directions agree.

    Bones swap with their opposite number and each local quaternion mirrors
    across the rig's lateral plane, which for Blender's pose convention is
    (w, x, -y, -z).
    """
    import re

    curves: dict[str, dict[int, object]] = {}
    for curve in action.fcurves:
        match = re.match(r'pose\.bones\["([^"]+)"\]\.rotation_quaternion', curve.data_path)
        if match:
            curves.setdefault(match.group(1), {})[curve.array_index] = curve

    def partner(name: str) -> str:
        if "Left" in name:
            return name.replace("Left", "Right")
        if "Right" in name:
            return name.replace("Right", "Left")
        return name

    # snapshot first: bones read from each other, so rewriting in place mid-pass
    # would feed already-mirrored values back in
    frames = sorted(
        {int(key.co[0]) for axes in curves.values() for c in axes.values() for key in c.keyframe_points}
    )
    snapshot = {
        bone: {f: [axes[i].evaluate(f) for i in range(4)] for f in frames}
        for bone, axes in curves.items()
    }
    for bone, axes in curves.items():
        source = partner(bone) if partner(bone) in snapshot else bone
        for f in frames:
            w, x, y, z = snapshot[source][f]
            for i, value in enumerate((w, x, -y, -z)):
                for key in axes[i].keyframe_points:
                    if int(key.co[0]) == f:
                        key.co[1] = value
                        key.handle_left[1] = value
                        key.handle_right[1] = value
        for i in range(4):
            axes[i].update()


def main() -> None:
    argv = sys.argv[sys.argv.index("--") + 1 :]
    repo, fbx_path, bvh_path, action_name, out_blend = argv[:5]
    max_frames = int(argv[5]) if len(argv) > 5 else 90
    start_at = int(argv[6]) if len(argv) > 6 else 0
    mirror = "mirror" in argv[7:8]

    retargeter = load_aletha_module(Path(repo))
    target = import_mantis(fbx_path, retargeter.MIXAMO_PREFIX)
    source = retargeter.import_animation_source(Path(bvh_path))
    retargeter.JOINT_MAP = MANTIS_JOINT_MAP

    scene = bpy.context.scene
    start = int(scene.frame_start) + start_at
    end = min(int(scene.frame_end), start + max_frames - 1)
    action, speed = retargeter.retarget(source, target, action_name, start, end)
    # The source's own travel, reported because a generated take that barely
    # moves reads as a shuffle no matter how clean the transfer is.
    if mirror:
        mirror_action(action)
        print("MIRRORED", action_name)
    print(f"RETARGETED {action_name} frames {start}..{end} source speed {speed:.2f} m/s")

    # Drop the hips vertical bob. The Aletha bridge scales it by the ratio of
    # the two rigs' leg lengths, which assumes both are in the same units; this
    # BVH is in metres and the mantis armature in centimetres, so the bob comes
    # out roughly 90x too big and the cooked clip bounds report a floor 154738
    # units down. The character motor owns translation anyway, so the safe form
    # of this clip has no root translation at all.
    for curve in list(action.fcurves):
        if curve.data_path.endswith("location"):
            action.fcurves.remove(curve)

    # Optional GLB export for import-locomotion, which cooks one take per file.
    # Same export flags as the walk study so both paths produce identical rigs.
    # Explicit flag: the SOURCE model is a .glb too, so scanning by extension
    # would happily export over the input.
    glb_out = argv[argv.index("--glb") + 1] if "--glb" in argv else None
    if glb_out:
        bpy.data.objects.remove(source, do_unlink=True)
        bpy.ops.object.select_all(action="DESELECT")
        target.select_set(True)
        for child in target.children_recursive:
            child.select_set(True)
        bpy.context.view_layer.objects.active = target
        scene.frame_start, scene.frame_end = start, end
        result = bpy.ops.export_scene.gltf(
            filepath=glb_out,
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
            raise RuntimeError(f"GLB export failed: {glb_out}")
        print("EXPORTED", glb_out)
        return

    bpy.data.objects.remove(source, do_unlink=True)
    for other in list(bpy.data.actions):
        if other is not action:
            bpy.data.actions.remove(other)
    scene.frame_start, scene.frame_end = start, end
    bpy.ops.wm.save_as_mainfile(filepath=out_blend)
    print("SAVED", out_blend)


main()
