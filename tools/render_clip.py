#!/usr/bin/env python3
"""Render one GLB/FBX take to PNG frames. No retarget, no assembly, no video.

Frames rather than a movie so the caller can line takes up against each other
(a source clip and an in-game capture do not start at the same moment or run
at the same rate; aligning them on the strike is the whole point).

    blender --background --factory-startup --python tools/render_clip.py -- \
        --clip raw.glb --out /tmp/frames/light --view threequarter [--take NAME]

Writes <out>0001.png upward, one per frame of the take's own range.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import bpy
from mathutils import Vector

VIEWS = {
    "threequarter": Vector((1.0, -1.45, 0.25)),
    "side": Vector((1.75, 0.0, 0.15)),
    "front": Vector((0.0, -1.75, 0.15)),
    # The game camera sits behind the player, so a back view is the one that
    # compares like for like against a capture.
    "back": Vector((0.0, 1.75, 0.35)),
    "backquarter": Vector((0.75, 1.5, 0.35)),
}


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clip", required=True, type=Path)
    parser.add_argument("--take", help="Action name inside the file")
    parser.add_argument("--out", required=True, type=Path, help="Frame path prefix")
    parser.add_argument("--view", default="threequarter", choices=sorted(VIEWS))
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument("--size", type=int, default=480)
    return parser.parse_args(argv)


def main() -> None:
    args = parse_args()
    scene = bpy.context.scene
    scene.render.fps = args.fps
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete()

    path = args.clip.expanduser()
    if path.suffix.lower() == ".fbx":
        bpy.ops.import_scene.fbx(filepath=str(path), use_anim=True)
    else:
        bpy.ops.import_scene.gltf(filepath=str(path))
    rigs = [o for o in bpy.data.objects if o.type == "ARMATURE"]
    if not rigs:
        raise SystemExit(f"no armature in {path}")
    rig = rigs[0]
    actions = list(bpy.data.actions)
    if args.take:
        actions = [a for a in actions if a.name == args.take or a.name.startswith(args.take)]
    if rig.animation_data is None:
        rig.animation_data_create()
    for track in rig.animation_data.nla_tracks:
        track.mute = True
    action = actions[0]
    rig.animation_data.action = action
    first, last = int(action.frame_range[0]), int(action.frame_range[1])
    scene.frame_start, scene.frame_end = first, last

    camera_data = bpy.data.cameras.new("Clip_Camera")
    camera = bpy.data.objects.new("Clip_Camera", camera_data)
    bpy.context.collection.objects.link(camera)
    scene.camera = camera
    camera_data.lens = 58.0

    # Frame from the bounds over the WHOLE take: a swing leaves the first
    # frame's box, and a clipped weapon arm is exactly what this is meant to
    # show.
    meshes = [child for child in rig.children_recursive if child.type == "MESH"]
    corners = []
    for frame in range(first, last + 1, max((last - first) // 12, 1)):
        scene.frame_set(frame)
        bpy.context.view_layer.update()
        corners += [
            child.matrix_world @ Vector(corner)
            for child in meshes
            for corner in child.bound_box
        ]
    lo = Vector((min(c.x for c in corners), min(c.y for c in corners), min(c.z for c in corners)))
    hi = Vector((max(c.x for c in corners), max(c.y for c in corners), max(c.z for c in corners)))
    center = (lo + hi) * 0.5
    height = max(hi.z - lo.z, 1e-3)

    aim = bpy.data.objects.new("Clip_Aim", None)
    bpy.context.collection.objects.link(aim)
    aim.location = center
    camera.location = center + VIEWS[args.view] * height * 1.35
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
    scene.render.resolution_x = args.size
    scene.render.resolution_y = args.size
    scene.render.image_settings.file_format = "PNG"
    out = args.out.expanduser()
    out.parent.mkdir(parents=True, exist_ok=True)
    scene.render.filepath = str(out)
    bpy.ops.render.render(animation=True)
    print(f"[render] {path.name}: frames {first}..{last} -> {out}####.png")


main()
