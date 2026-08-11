# `editor/` (editor & content pipeline)

The host-side authoring stack: the project model, the asset cookers that
turn source content (OBJ/glTF meshes, PNG/JPG textures, WAV audio) into
binary blobs the runtime consumes, and the reusable egui UI panels. The
editor's Play action cooks the active scene and hands it to
[`engine/examples/editor-playtest`](../engine/examples/editor-playtest).

These crates run on the host, not on PSX target; they are members of
the repo-root HOST workspace (one lockfile shared with `crates/`,
`emu/`, and `tools/`).

## BSP level quickstart

This is the shortest supported create/edit/play loop. From the repository
root, launch the native editor directly:

```bash
cd emu
cargo run -p frontend -- --editor --windowed
```

1. Choose **File → New Project…**, enter a name, leave **Draft** selected,
   and press **Create**. The project is copied from the buildable BSP-first
   template and opens in the Top orthographic view with the Brush tool active.
2. The template is already playable. To add another closed room, drag a solid
   footprint with **Tool → Brush**, select it, then press **Hollow** in the
   Inspector. New brushes inherit the selected Material (or the project's
   first Material), and Hollow carries that material onto the six room slabs.
3. Shape the room in **Top**, **Front**, or **Side**. Choose **Face**, **Edge**,
   or **Vertex** from the Selection control and drag. Shift-drag moves the
   selected brush; the Inspector also provides exact Origin/plane values,
   Duplicate, Snap, and Delete.
4. To change a surface, click the brush face, choose a Material in the
   Inspector, then press **Apply to face**. Imported textures appear as
   Materials in the Resources panel.
5. A project may have one player source. Move the template's **Player Spawn**,
   or delete it and choose **Tool → Add → Player Spawn**, then click an
   upward-facing brush floor. **Point Light** is available from the same Add
   menu. In Top view, placement uses the upward surface closest to the shared
   Y focus; in 3D, click the visible floor directly.
   For a moving door, add **Logic**, change its Inspector kind to **Door**,
   then select the door brush and choose that node under **Model owner**.
6. Save with **File → Save** (`Cmd/Ctrl+S`) and press **Play**. Play saves the
   authored state, cooks the PXBSP, builds the real PS1 runtime, and boots it
   in the editor viewport. The first build is slower because it compiles the
   guest; later iterations reuse it.
7. Stop or return to editing, make a change, then press **Rebuild & Play**.
   No generated-file copying or command-line cook is required. Use the Play
   button's chevron to switch between fast **Draft** and lit **Release** cooks
   and to inspect the latest PS1 memory/packet budget.

If Play refuses the map, the status strip reports the cook error and focuses
the first actionable brush/node/resource where possible. A Player Spawn inside
solid is normally fixed by placing it again on the room's upward floor face.

For a windowless acceptance of this exact author/save/Play contract, run
`make editor-blank-playtest-check` from the repository root. It authors a fresh
BSP room through the editor command APIs, verifies that the tracked starter
project, input tape, and neutral texture match their generator byte for byte,
persists and cooks the authored artifact, builds the real `mipsel-sony-psx`
guest and disc, then boots it in the headless PSoXide emulator and proves
rendered frames plus player movement. This is an emulator gate, not a GUI
automation or original-hardware claim.

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

- [Root README](../README.md#quick-start). Launching the editor.
- [`docs/editor-architecture.md`](../docs/editor-architecture.md). Editor internals.
- [`docs/editor-model-authoring.md`](../docs/editor-model-authoring.md), [`docs/editor-lighting.md`](../docs/editor-lighting.md), [`docs/editor-runtime-coordinates.md`](../docs/editor-runtime-coordinates.md). Authoring workflows.
