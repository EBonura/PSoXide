# Cortex Ignition Tech Demo 0.2

This is the clean authoring project for Cortex Ignition Tech Demo 0.2. It keeps
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

Regenerate the project from the Null Choir material source and the canonical
Cortex Ignition character source with:

```sh
cargo run -p psxed-project --bin gen_cortex_ignition_tech_demo_0_2
```
