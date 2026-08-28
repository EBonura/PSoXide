#!/usr/bin/env python3
"""Inspect disconnected components and skin-weight ownership in a GLB.

Run through Blender so the report uses Blender's production glTF importer:

    blender --background --python tools/inspect_blender_mesh.py -- model.glb
"""

from __future__ import annotations

import collections
import sys
from pathlib import Path

import bpy


def _arguments() -> list[str]:
    if "--" not in sys.argv:
        return []
    return sys.argv[sys.argv.index("--") + 1 :]


def _connected_components(mesh: bpy.types.Mesh) -> list[list[int]]:
    neighbours: list[set[int]] = [set() for _ in mesh.vertices]
    for edge in mesh.edges:
        a, b = edge.vertices
        neighbours[a].add(b)
        neighbours[b].add(a)

    remaining = set(range(len(mesh.vertices)))
    components: list[list[int]] = []
    while remaining:
        seed = remaining.pop()
        stack = [seed]
        component = [seed]
        while stack:
            vertex = stack.pop()
            attached = neighbours[vertex] & remaining
            remaining.difference_update(attached)
            stack.extend(attached)
            component.extend(attached)
        components.append(component)
    return sorted(components, key=len, reverse=True)


def main() -> None:
    args = _arguments()
    if len(args) != 1:
        raise SystemExit("usage: blender --background --python inspect_blender_mesh.py -- MODEL.glb")

    source = Path(args[0]).resolve()
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=str(source))

    for obj in (candidate for candidate in bpy.context.scene.objects if candidate.type == "MESH"):
        mesh = obj.data
        group_names = {group.index: group.name for group in obj.vertex_groups}
        print(f"MESH {obj.name!r}: {len(mesh.vertices)} vertices, {len(mesh.polygons)} polygons")
        for index, component in enumerate(_connected_components(mesh)):
            coords = [mesh.vertices[vertex].co for vertex in component]
            minimum = tuple(min(co[axis] for co in coords) for axis in range(3))
            maximum = tuple(max(co[axis] for co in coords) for axis in range(3))
            weights: collections.Counter[str] = collections.Counter()
            for vertex_index in component:
                for membership in mesh.vertices[vertex_index].groups:
                    weights[group_names.get(membership.group, str(membership.group))] += membership.weight
            owners = ", ".join(f"{name}={weight:.1f}" for name, weight in weights.most_common(5))
            print(
                f"  COMPONENT {index:02}: vertices={len(component):4} "
                f"min={tuple(round(value, 4) for value in minimum)} "
                f"max={tuple(round(value, 4) for value in maximum)} owners=[{owners}]"
            )

    for armature in (
        candidate for candidate in bpy.context.scene.objects if candidate.type == "ARMATURE"
    ):
        print(f"ARMATURE {armature.name!r}: {len(armature.data.bones)} bones")
        for bone in armature.data.bones:
            if bone.name in {"mixamorig:LeftForeArm", "mixamorig:LeftHand"}:
                head = tuple(round(value, 5) for value in bone.head_local)
                tail = tuple(round(value, 5) for value in bone.tail_local)
                print(f"  BONE {bone.name!r}: head={head} tail={tail}")


if __name__ == "__main__":
    main()
