# Cortex Ignition Tech Demo 0.3

This project is a complete snapshot of Cortex Ignition Tech Demo 0.2, copied as
the starting point for Cortex Ignition Tech Demo 0.3. It keeps
the complete Null Choir art kit and cooked eclipse sky, adds the canonical
Cortex Ignition player and two configured enemy instances, and deliberately
contains no inherited BSP geometry or old gameplay route.

- Open `project.ron` in the Editor.
- All twenty cooked 64 x 64, 4bpp Editor textures live in `assets/textures/`.
- Matching editable PNG sources live in `source_assets/textures/`. The final V3
  sources are already 64 x 64; earlier V1/V2 sources retain their larger masters.
- `Null Choir Eclipse Cube Sky` is already assigned to the World in `Cube` mode.
  Its editable 2:1 panorama lives in `source_assets/sky/`; its 1536 x 256, 4bpp,
  six-face PSXT lives in `assets/textures/sky/`.
- The World uses `ThroughSkySurfaces`. The inward face of the large sealing
  ceiling above the player start uses `Null Choir Eclipse Cube Sky`, so it shows
  the infinite cube sky while the brush remains collision-solid and seals BSP.
- Paint the same material onto other sealed BSP roof/wall faces to create more
  Quake-style sky apertures without opening the level to the void.
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

This 0.3 snapshot is now an independent authored project. Save further level
changes directly into this directory; the original 0.2 generator does not own
or regenerate it.
