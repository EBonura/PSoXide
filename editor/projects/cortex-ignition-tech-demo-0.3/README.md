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
- Paint the sky material onto sealed BSP roof/wall brushes wherever you want an
  aperture. It has `sky_aperture` enabled and the World uses
  `ThroughSkySurfaces`, so the geometry remains collision-solid while showing
  the cooked image sky.
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

This 0.3 snapshot is now an independent authored project. Save further level
changes directly into this directory; the original 0.2 generator does not own
or regenerate it.
