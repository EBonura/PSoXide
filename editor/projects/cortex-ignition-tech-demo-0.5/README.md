# Cortex Ignition Tech Demo 0.5

This project is a complete snapshot of Cortex Ignition Tech Demo 0.4, created as
a non-destructive visual-direction sandbox for the Comic Con build. It
keeps the complete Null Choir art kit, the 64 x 64 DP BSP material set, the
canonical Cortex Ignition player, and two configured enemy instances. The active
scene is currently a deliberately minimal skybox-review stage: one DP-textured
player pedestal in an otherwise open world.

- Open `project.ron` in the Editor.
- All cooked 64 x 64, 4bpp Editor textures live in `assets/textures/`.
- Matching editable PNG sources live in `source_assets/textures/`. The final V3
  sources are already 64 x 64; earlier V1/V2 sources retain their larger masters.
- `DP Fog City Cube Sky` is assigned to the World in `Cube` mode. Its editable
  1774 x 887 (2:1) panorama lives in `source_assets/sky/`; its runtime-ready
  1536 x 256, 4bpp, six-face/six-CLUT PSXT lives in `assets/textures/sky/`.
- The World uses `Always` visibility so the cube sky fills the open review stage
  without requiring a sky-aperture brush. The previous 271-brush scene is backed
  up in `logs/project.ron.pre-dp-skybox.bak`.
- `DP_SKYBOX.md` records the conversion contract, source art direction, scene
  simplification, and real in-engine capture paths.
- `DP_CITY_TEXTURE_KIT.md` documents the eight-texture BSP construction kit,
  including its bright signals, dense teal bays, and two transparent overlays.
- `V05_ART_DIRECTION.md` fixes the short-deadline art-direction contract: keep
  the green, blue, and teal signal language and revise high-coverage structural
  textures in place so no level faces need repainting.
- `TEXTURE_SET_V3.md` describes the intended architectural role of each material.
- `FUTURE_GOTHIC_TEXTURES.md` documents the optional grittier future-Gothic
  material pass now painted across the current level geometry.
- `concept/reference_set_v2/` contains the original hero-room reference and four
  matching room concepts to build from.
- `Aletha (Player)` is a complete player entity with model renderer, animation,
  character controller, third-person camera, and light sword equipment.
- `Intake Custodian` and `Gallery Custodian` are two separate enemy entities
  using the canonical `Rust Mantis Enemy` profile, combat AI settings,
  animations, and heavy sword equipment.
- Their cooked models, textures, and animation clips are copied into
  `assets/models/` and `assets/animations/`; the project does not rely on the
  old tech-demo directory at runtime.

The twelve un-suffixed Editor surface-material names are the recommended V3 set. Eight
older alternatives are clearly labelled `V1 Draft` or `V2 Draft` in the resource
browser, but remain available for remixing.

Six additional materials prefixed `Future Gothic /` provide brighter,
lighting-neutral 64 x 64, 4bpp alternatives for the primary beam, bulkhead,
deck, rib, service-panel, and wall-plinth surfaces. Their masters and cooked
textures live beside the original set; no original material or texture was
removed.

`Future Gothic / Exterior Rock` extends the same set for outdoor brushwork. A
six-face `Future Gothic / Computer Terminal` kit provides front, left, right,
back, top, and bottom materials for brush-built environment props. The front
cycles between two independently selectable CRT readout materials while
preserving the same casing and face UVs. The cooker creates the shared 4bpp
runtime atlas automatically. These resources are registered in the material
browser and intentionally start unassigned.

Sixteen additional materials prefixed `DP BSP /` translate selected 32 x 32
tiles from DP Complete into 64 x 64, 4bpp BSP-ready textures. They are registered
in the material browser and intentionally start unassigned. See
`DP_BSP_TEXTURES.md` for source-cell provenance, rebuild instructions, and
suggested architectural roles.

Eight focused materials prefixed `DP City /` are the recommended kit for the
new fog-city mockups. They are 64 x 64, seamless and 4bpp. The cable and hanging
lattice reserve transparent palette index zero, use PS1 `Average` blending and
are double-sided. The remaining review-stage platform now uses the new deck,
edge and megastructure-wall materials so ordinary project cooking exercises the
kit immediately. See `DP_CITY_TEXTURE_KIT.md` for the contact sheet and roles.

This 0.5 snapshot is now an independent authored project. Save further level
changes directly into this directory; the original 0.2 generator does not own
or regenerate it.
