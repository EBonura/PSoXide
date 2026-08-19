"""Cut a mesh variant of the Rust Mantis by dropping bone-weighted islands.

    blender -b --python mantis_variant.py -- <fbx> <out.glb> <bone> [bone...]

The mesh is 54 disconnected islands and every vertex belongs wholly to one
bone's island, so dropping a limb removes whole islands with no torn faces and
nothing to reweight.
"""
import sys, bpy, bmesh

argv = sys.argv[sys.argv.index("--") + 1:]
fbx_path, out_glb, drop = argv[0], argv[1], set(argv[2:])

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.fbx(filepath=fbx_path)
for action in list(bpy.data.actions):
    bpy.data.actions.remove(action)
mesh = next(o for o in bpy.data.objects if o.type == 'MESH')
armature = next(o for o in bpy.data.objects if o.type == 'ARMATURE')
bpy.ops.object.select_all(action="DESELECT")
for obj in (mesh, armature):
    obj.select_set(True)
bpy.context.view_layer.objects.active = armature
bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)

groups = {g.index: g.name for g in mesh.vertex_groups}
targets = {f"mixamorig:{b}" for b in drop}
doomed = []
for v in mesh.data.vertices:
    if not v.groups:
        continue
    name = groups[max(v.groups, key=lambda e: e.weight).group]
    if name in targets:
        doomed.append(v.index)
print(f"DROPPING {len(doomed)} of {len(mesh.data.vertices)} verts for {sorted(targets)}")

bm = bmesh.new()
bm.from_mesh(mesh.data)
bm.verts.ensure_lookup_table()
bmesh.ops.delete(bm, geom=[bm.verts[i] for i in doomed], context='VERTS')
bm.to_mesh(mesh.data)
bm.free()
print(f"REMAINS verts={len(mesh.data.vertices)} faces={len(mesh.data.polygons)}")

bpy.ops.export_scene.gltf(filepath=out_glb, export_format='GLB',
                          export_animations=False, export_yup=True)
print("VARIANT", out_glb)
