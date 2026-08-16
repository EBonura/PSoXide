# Legacy-grid project archive

This directory separates the frozen grid-era projects from active PXBSP
authoring. New levels belong directly under `editor/projects/` and must use
`world_format: Bsp`.

The archive payload is intentionally local and gitignored. These projects
predate the shared Quake/PSoXide BSP world and remain useful only as historical
content donors, diagnostics, and performance comparisons.

## Compatibility anchors that remain outside this directory

- `editor/projects/default` is the legacy resource catalogue used by Starter
  Characters and compatibility tests. It is support data, not a new-level
  template.
- `editor/samples/cortex_v1` is the tracked legacy Cortex Ignition demo-disc
  fallback. Its location and payload remain stable until the new BSP tech demo
  has separately passed disc and hardware acceptance.

## Local archive map

| Archived path | Previous path | Role |
|---|---|---|
| `archive/cortex/cortex_anim` | `projects/cortex_anim` | Animation and moveset experiment |
| `archive/cortex/cortex_v1` | `projects/cortex_v1` | Full local Cortex working project |
| `archive/cortex/cortex_v2` | `projects/cortex_v2` | Grid streaming and water experiment |
| `archive/cortex/cortex_v3` | `projects/cortex_v3` | Renderer/performance benchmark |
| `archive/cortex/cortex_v4` | `projects/cortex_v4` | Small model-import sandbox |
| `archive/cortex/cortex_v5` | `projects/cortex_v5` | Asset-packaging stress project |
| `archive/demos/demo_02` | `projects/demo_02` | Early combat sandbox |
| `archive/demos/demo_03` | `projects/demo_03` | Multi-room streaming benchmark |
| `archive/demos/demo_04` | `projects/demo_04` | Portal and image-prop sandbox |
| `archive/demos/demo_05` | `projects/demo_05` | Dense cemetery/prop scene |
| `archive/demos/demo_07` | `projects/demo_07` | Camera/portal benchmark |
| `archive/demos/demo_11` | `projects/demo_11` | Stacked-floor regression scene |
| `archive/tools/prefab_gallery` | `projects/prefab_gallery` | Generated prefab catalogue |
| `archive/tools/prefab_lab` | `projects/prefab_lab` | Prefab sandbox |
| `archive/scratch/test1` | `projects/test1` | Historical scratch project |
| `archive/benchmarks/vis_corridor` | `projects/vis_corridor` | Visibility stress corridor |

Do not copy any of these as the basis of a new level. Use File > New Project,
which creates a PXBSP project from `brush-open-courtyard`, or inspect
`souls-bsp-vertical-slice` for the combat/door/checkpoint pattern.
