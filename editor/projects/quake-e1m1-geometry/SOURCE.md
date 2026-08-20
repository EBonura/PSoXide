# E1M1 geometry source

This project contains brush-plane geometry derived from the original Quake E1M1 map source released by John Romero in October 2006.

- Source: https://github.com/fzwoch/quake_map_source
- Source revision: `27abebaa3886bb0e3156cce3a604673d22b243f8`
- License: GPL-2.0, reproduced in `SOURCE-COPYING`
- Imported brushes: 1214
- Imported faces: 7278
- Quake coordinate scale: 4x
- Actor visual/controller scale: 0.25x canonical PSoXide
- Runtime BSP topology/PVS: textureless geometry-only derivative of the released `bsp/e1m1.bsp`

No Quake texture names, texture pixels, lightmaps, gameplay entities, triggers, monsters, items, model assets, or audio are included. PSoXide's generated grey material is assigned to every imported face. The stripped BSP sidecar retains only planes, vertices, faces, nodes, leaves, mark-surfaces, edges, visibility, and world-model bounds needed to reuse E1M1's partition/PVS.
