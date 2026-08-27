# Null Choir Texture Kit

This is a deliberately blank PSoXide Editor project containing all twenty Null
Choir surface-material versions, the cooked image-backed eclipse sky, and no
inherited BSP geometry, lights, entities, or gameplay route.

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

The twelve un-suffixed Editor surface-material names are the recommended V3 set. Eight
older alternatives are clearly labelled `V1 Draft` or `V2 Draft` in the resource
browser, but remain available for remixing.

Regenerate this clean project from the original texture source with:

```sh
cargo run -p psxed-project --bin gen_null_choir_texture_kit
```
