# `editor/` — editor & content pipeline

The host-side authoring stack: the project model, the asset cookers that
turn source content (OBJ/glTF meshes, PNG/JPG textures, WAV audio) into
binary blobs the runtime consumes, and the reusable egui UI panels. The
editor's Play action cooks the active scene and hands it to
[`engine/examples/editor-playtest`](../engine/examples/editor-playtest).

`editor/Cargo.toml` is its own workspace. These crates run on the host, not
on PSX target.

## Crates

| Crate | Purpose |
|-------|---------|
| [`psxed`](crates/psxed) | Content-pipeline CLI. Cooks source assets into binary blobs the runtime consumes. |
| [`psxed-project`](crates/psxed-project) | Editor project model: scenes, nodes, resources, and PS1-facing authoring metadata. |
| [`psxed-ui`](crates/psxed-ui) | Reusable egui workspace panels for the editor. |
| [`psxed-format`](crates/psxed-format) | Cooked-asset binary formats. Shared with the SDK's `psx-asset` parser so layout drift is impossible. |
| [`psxed-obj`](crates/psxed-obj) | OBJ parser + vertex-cluster decimator. Emits PSXM blobs. |
| [`psxed-gltf`](crates/psxed-gltf) | glTF/GLB mesh importer. Cooks scene meshes into PSXM blobs. |
| [`psxed-tex`](crates/psxed-tex) | PNG/JPG → PSXT texture cooker: crop, resample, quantise to 4/8-bit CLUT, pack for VRAM. |
| [`psxed-audio`](crates/psxed-audio) | WAV → PS1 SPU ADPCM audio cooker. |

## The format contract

`psxed-format` is the single source of truth for cooked-asset layout: the
editor crates here are the **producers**, and the SDK's
[`psx-asset`](../sdk/crates/psx-asset) is the **consumer**. Both depend on
`psxed-format`, so a layout change can't drift between the two sides.

## See also

- [Root README](../README.md#6-open-the-editor) — launching the editor.
- [`docs/editor-architecture.md`](../docs/editor-architecture.md) — editor internals.
- [`docs/editor-model-authoring.md`](../docs/editor-model-authoring.md), [`docs/editor-lighting.md`](../docs/editor-lighting.md), [`docs/editor-runtime-coordinates.md`](../docs/editor-runtime-coordinates.md) — authoring workflows.
