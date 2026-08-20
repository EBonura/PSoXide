# E1M1 geometry source

This project contains brush-plane geometry derived from the original Quake E1M1 map source released by John Romero in October 2006.

- Source: https://github.com/fzwoch/quake_map_source
- Source revision: `27abebaa3886bb0e3156cce3a604673d22b243f8`
- License: GPL-2.0, reproduced in `SOURCE-COPYING`
- Imported brushes: 1214
- Imported faces: 7278
- Quake coordinate scale: 16x (canonical PSoXide authored scale)
- Actor visual/controller scale: 1.0x canonical PSoXide
- Runtime geometry: cooked from the editable brushes through PSoXide's standard BSP pipeline

No Quake texture names, texture pixels, lightmaps, gameplay entities, triggers, monsters, items, model assets, or audio are included. PSoXide's temporary 4bpp BRICK_1A material is assigned to every imported face with Quake-compatible world-space texel density. Runtime geometry, collision, visibility, and editor-light shading are all cooked from the editable brushes.
