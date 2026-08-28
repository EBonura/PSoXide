#!/usr/bin/env python3
"""Build arm-integrated Rust Mantis GLB variants from the canonical source.

Only vertices rigidly owned by ``mixamorig:LeftHand`` are removed. The
shoulder, upper arm, and forearm remain byte-for-byte source geometry; the new
weapon shell is joined into the same skinned mesh and rigidly follows the
surviving forearm.

Run with Blender's factory startup so user add-ons cannot alter the export::

    blender --background --factory-startup \
      --python tools/build_rust_mantis_arm_variants.py -- \
      SOURCE.glb OUTPUT_DIRECTORY
"""

from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path

import bmesh
import bpy

HAND_BONE = "mixamorig:LeftHand"
MOUNT_BONE = "mixamorig:LeftForeArm"

# Pixel rectangles in the original 256x256 Rust Mantis atlas. Reusing the
# source atlas keeps every variant on the same PSX texture page.
UV_DARK_METAL = (152, 4, 207, 52)
UV_WORN_METAL = (98, 154, 132, 205)
UV_HAZARD = (129, 166, 174, 210)
UV_RED_CORE = (54, 72, 94, 108)


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output_dir", type=Path)
    return parser.parse_args(argv)


def atlas_rect(rect: tuple[int, int, int, int]) -> tuple[float, float, float, float]:
    x0, y0, x1, y1 = rect
    return (x0 / 256.0, 1.0 - y1 / 256.0, x1 / 256.0, 1.0 - y0 / 256.0)


def remove_hand_ending(mesh_obj: bpy.types.Object) -> int:
    group_names = {group.index: group.name for group in mesh_obj.vertex_groups}
    doomed: list[int] = []
    for vertex in mesh_obj.data.vertices:
        if not vertex.groups:
            continue
        owner = max(vertex.groups, key=lambda membership: membership.weight)
        if group_names.get(owner.group) == HAND_BONE:
            doomed.append(vertex.index)

    bm = bmesh.new()
    bm.from_mesh(mesh_obj.data)
    bm.verts.ensure_lookup_table()
    bmesh.ops.delete(bm, geom=[bm.verts[index] for index in doomed], context="VERTS")
    bm.to_mesh(mesh_obj.data)
    bm.free()
    mesh_obj.data.update()
    return len(doomed)


def create_part(
    source_obj: bpy.types.Object,
    armature: bpy.types.Object,
    name: str,
    vertices: list[tuple[float, float, float]],
    faces: list[tuple[int, ...]],
    uv_pixels: tuple[int, int, int, int],
) -> bpy.types.Object:
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(vertices, [], faces)
    mesh.update()
    part = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(part)
    part.matrix_world = source_obj.matrix_world.copy()
    if source_obj.data.materials:
        mesh.materials.append(source_obj.data.materials[0])

    u0, v0, u1, v1 = atlas_rect(uv_pixels)
    uv_layer = mesh.uv_layers.new(name="UVMap")
    quad_uvs = ((u0, v0), (u1, v0), (u1, v1), (u0, v1))
    for polygon in mesh.polygons:
        for corner, loop_index in enumerate(polygon.loop_indices):
            uv_layer.data[loop_index].uv = quad_uvs[corner % 4]

    group = part.vertex_groups.new(name=MOUNT_BONE)
    group.add(range(len(vertices)), 1.0, "REPLACE")
    modifier = part.modifiers.new(name="Armature", type="ARMATURE")
    modifier.object = armature
    part.parent = source_obj.parent
    return part


def prism_geometry(
    rings: list[tuple[float, float, float]],
    center_y: float = 0.012,
    center_z: float = 1.414,
    sides: int = 8,
) -> tuple[list[tuple[float, float, float]], list[tuple[int, ...]]]:
    vertices: list[tuple[float, float, float]] = []
    for x, radius_y, radius_z in rings:
        for index in range(sides):
            angle = math.tau * index / sides
            vertices.append(
                (
                    x,
                    center_y + math.cos(angle) * radius_y,
                    center_z + math.sin(angle) * radius_z,
                )
            )
    faces: list[tuple[int, ...]] = []
    for ring in range(len(rings) - 1):
        start = ring * sides
        following = (ring + 1) * sides
        for index in range(sides):
            nxt = (index + 1) % sides
            faces.append((start + index, start + nxt, following + nxt, following + index))
    faces.append(tuple(reversed(range(sides))))
    final = (len(rings) - 1) * sides
    faces.append(tuple(final + index for index in range(sides)))
    return vertices, faces


def box_geometry(
    x0: float,
    x1: float,
    y0: float,
    y1: float,
    z0: float,
    z1: float,
    *,
    nose_slope: float = 0.0,
) -> tuple[list[tuple[float, float, float]], list[tuple[int, ...]]]:
    vertices = [
        (x0, y0, z0),
        (x0, y1, z0),
        (x0, y1, z1),
        (x0, y0, z1),
        (x1, y0, z0 + nose_slope),
        (x1, y1, z0 + nose_slope),
        (x1, y1, z1 - nose_slope),
        (x1, y0, z1 - nose_slope),
    ]
    faces = [
        (0, 3, 2, 1),
        (4, 5, 6, 7),
        (0, 1, 5, 4),
        (1, 2, 6, 5),
        (2, 3, 7, 6),
        (3, 0, 4, 7),
    ]
    return vertices, faces


def add_prism(
    parts: list[bpy.types.Object],
    source_obj: bpy.types.Object,
    armature: bpy.types.Object,
    name: str,
    rings: list[tuple[float, float, float]],
    uv: tuple[int, int, int, int],
    *,
    center_y: float = 0.012,
    center_z: float = 1.414,
    sides: int = 8,
) -> None:
    vertices, faces = prism_geometry(rings, center_y, center_z, sides)
    parts.append(create_part(source_obj, armature, name, vertices, faces, uv))


def add_box(
    parts: list[bpy.types.Object],
    source_obj: bpy.types.Object,
    armature: bpy.types.Object,
    name: str,
    bounds: tuple[float, float, float, float, float, float],
    uv: tuple[int, int, int, int],
    *,
    nose_slope: float = 0.0,
) -> None:
    vertices, faces = box_geometry(*bounds, nose_slope=nose_slope)
    parts.append(create_part(source_obj, armature, name, vertices, faces, uv))


def build_light_weapon(
    source_obj: bpy.types.Object, armature: bpy.types.Object
) -> list[bpy.types.Object]:
    parts: list[bpy.types.Object] = []
    add_prism(parts, source_obj, armature, "Light_Sleeve", [(0.64, 0.085, 0.09), (0.91, 0.082, 0.087)], UV_WORN_METAL)
    # The 1.486 muzzle tip deliberately preserves the source claw's maximum-X
    # model bound. Cooked animation translations are decoded through that
    # bound, so making a visually shorter replacement would distort every
    # existing clip even though the skeleton itself stayed identical.
    add_prism(parts, source_obj, armature, "Light_Barrel", [(0.84, 0.068, 0.072), (1.36, 0.044, 0.048)], UV_DARK_METAL)
    add_prism(parts, source_obj, armature, "Light_Muzzle", [(1.31, 0.072, 0.078), (1.478, 0.066, 0.072)], UV_HAZARD)
    add_prism(parts, source_obj, armature, "Light_Core", [(1.478, 0.038, 0.041), (1.486, 0.038, 0.041)], UV_RED_CORE)
    add_box(parts, source_obj, armature, "Light_Dorsal_Rail", (0.72, 1.28, -0.010, 0.034, 1.486, 1.535), UV_DARK_METAL, nose_slope=0.015)
    add_box(parts, source_obj, armature, "Light_Ventral_Fin", (0.79, 1.20, -0.004, 0.029, 1.319, 1.366), UV_HAZARD, nose_slope=0.012)
    return parts


def build_heavy_weapon(
    source_obj: bpy.types.Object, armature: bpy.types.Object
) -> list[bpy.types.Object]:
    parts: list[bpy.types.Object] = []
    add_prism(parts, source_obj, armature, "Heavy_Sleeve", [(0.59, 0.125, 0.135), (0.91, 0.132, 0.145)], UV_WORN_METAL)
    add_prism(parts, source_obj, armature, "Heavy_Chamber", [(0.78, 0.16, 0.175), (1.09, 0.145, 0.16)], UV_HAZARD)
    add_prism(parts, source_obj, armature, "Heavy_Barrel", [(1.01, 0.105, 0.115), (1.38, 0.09, 0.10)], UV_DARK_METAL)
    add_prism(parts, source_obj, armature, "Heavy_Muzzle", [(1.30, 0.145, 0.155), (1.478, 0.13, 0.14)], UV_WORN_METAL)
    add_prism(parts, source_obj, armature, "Heavy_Core", [(1.478, 0.068, 0.074), (1.486, 0.068, 0.074)], UV_RED_CORE)
    add_box(parts, source_obj, armature, "Heavy_Dorsal_Armor", (0.66, 1.29, -0.055, 0.079, 1.565, 1.625), UV_DARK_METAL, nose_slope=0.018)
    add_box(parts, source_obj, armature, "Heavy_Ventral_Armor", (0.69, 1.25, -0.050, 0.074, 1.205, 1.267), UV_HAZARD, nose_slope=0.018)
    add_box(parts, source_obj, armature, "Heavy_Outer_Brace", (0.68, 1.24, 0.145, 0.205, 1.33, 1.50), UV_DARK_METAL, nose_slope=0.025)
    return parts


def export_variant(source: Path, output: Path, kind: str) -> None:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=str(source))
    mesh_obj = next(obj for obj in bpy.context.scene.objects if obj.type == "MESH")
    armature = next(obj for obj in bpy.context.scene.objects if obj.type == "ARMATURE")
    removed = remove_hand_ending(mesh_obj)
    if removed == 0:
        raise RuntimeError(f"{HAND_BONE} owned no vertices")

    parts = (
        build_light_weapon(mesh_obj, armature)
        if kind == "light"
        else build_heavy_weapon(mesh_obj, armature)
    )
    bpy.ops.object.select_all(action="DESELECT")
    mesh_obj.select_set(True)
    for part in parts:
        part.select_set(True)
    bpy.context.view_layer.objects.active = mesh_obj
    bpy.ops.object.join()
    mesh_obj.name = f"rust_mantis_{kind}_arm"
    mesh_obj.data.name = mesh_obj.name

    for action in list(bpy.data.actions):
        bpy.data.actions.remove(action)
    output.parent.mkdir(parents=True, exist_ok=True)
    bpy.ops.export_scene.gltf(
        filepath=str(output),
        export_format="GLB",
        export_animations=False,
        export_yup=True,
    )
    print(
        f"[{kind}] removed {removed} hand/claw vertices; "
        f"kept arm and exported {len(mesh_obj.data.vertices)} vertices -> {output}"
    )


def main() -> None:
    args = parse_args()
    source = args.source.resolve()
    output_dir = args.output_dir.resolve()
    export_variant(source, output_dir / "rust_mantis_light_arm.glb", "light")
    export_variant(source, output_dir / "rust_mantis_heavy_arm.glb", "heavy")


if __name__ == "__main__":
    main()
