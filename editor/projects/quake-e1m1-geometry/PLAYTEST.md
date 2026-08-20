# E1M1 geometry playtest

Validated on 2026-08-20 with PSoXide's editor-playtest guest in the headless
emulator. Emulator timing is useful for regression work but is not a substitute
for an original PlayStation burn.

## Imported content

- 1,214 editable brushes and 7,278 authored faces from Quake E1M1.
- Quake texture, lightmap, entity, model, sound, and music data are not present.
- Every imported face currently uses PSoXide's 64x64 4bpp BRICK_1A texture as
  a temporary visual placeholder.
- Aletha and Rust Mantis start side by side in E1M1's first large room for the
  third-person level-scale test. The original lift-alcove spawn is too narrow
  for the playtest camera boom.
- Geometry and actors use the canonical 16:1 Quake-to-PSoXide authored scale.

## Standard brush cook

There is no E1M1 runtime sidecar or project-specific geometry path. The normal
PSoXide brush cooker builds render geometry, collision, PVS, materials, and
vertex lighting from the same editable brushes shown in the editor. Therefore
face/material edits, added brushes, and editor point lights all affect Play.
The map seals successfully: Quake-style outside fill removes 879 unreachable
exterior leaves and no `brush_world.pts` leak file is produced.

- Release PXBSP: 508,864 bytes.
- Quake portal-flow PVS: 54,367 bytes, with a 169-byte decompressed row.
- Baked vertex lighting: 34,620 bytes.
- Packed render faces: 2,901 (5,730 authored triangles).
- Cooked RAM estimate: 666,269 / 2,097,152 bytes.
- Resident character assets: 174,008 / 704,512 bytes.
- VRAM estimate: 86,496 / 1,048,576 bytes.
- Shipping/default-feature headless display hash: `0x914b42c4d5543943`.

The earlier Quake-BSP sidecar benchmarks are intentionally not retained here:
they measured geometry that bypassed the editable-brush pipeline and therefore
do not describe this project anymore.
