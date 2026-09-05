# `editor/` (editor & content pipeline)

The editor authors PXBSP levels and PSoXide gameplay records, including
[Cortex Ignition 0.4b](projects/cortex-ignition-tech-demo-0.4b/). Quake-PSX
maintains its own game-specific content pipeline.

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
   and press **Create**. The project copies a buildable 16384 × 16384 roofless
   open courtyard with cobbles underfoot, brick perimeter walls, a valid
   Player Spawn, and two blockout lights. It opens in the Top orthographic
   view with the Brush tool active. This template is used only when creating
   a project; opening an existing project never rewrites its scene or assets.
2. The template is already playable. To add another closed room, drag a solid
   footprint with **Tool → Brush**, select it, then press **Hollow** in the
   Inspector. New brushes inherit the selected Material (or the project's
   first Material), and Hollow carries that material onto the six room slabs.
3. Shape the room in **Top**, **Front**, **Side**, or **3D**. Choose the
   always-visible **Move**, **Resize**, **Edge**, or **Vertex** mode and drag the
   corresponding brush or handle. Move uses a plain drag. Shift/Ctrl/Cmd stay
   available for additive selection and marquee gestures in Select. The
   Inspector also provides exact Origin, axis-aligned Size, and face-plane
   values, plus Duplicate, Snap, and Delete.
   To cut a brush, select it and Cmd/Ctrl-click twice with the Brush tool:
   the two points define a vertical plane through the brush. **Clip keeps**
   in the toolbar chooses whether the cut leaves both halves, only the near
   one, or only the far one. Each cut is one undo step.
4. To change a surface, click the brush face, choose a Material in the
   Inspector, then press **Apply to face**. Imported textures appear as
   Materials in the Resources panel. **Face UV** offset, rotation, and per-axis
   scale sit under the same section, with **Reset UV** to return to the
   identity mapping; each edit is one undo step.
   To texture faster, switch to **Tool → Paint** and click brush faces in the
   3D view. Paint uses the same Material picker, and a drag across several
   faces is a single undo step. Its eyedropper reads the material off the face
   under the cursor back into the picker.
5. To author a liquid, select a closed brush and change **BSP contents** in the
   Brush Inspector from **Solid** to **Water**, **Slime**, or **Lava**. Liquid
   brushes render their boundary but do not block movement. Every liquid
   retains 60 percent movement speed; water is harmless, slime deals 4 health
   every 30 simulation ticks, and lava deals 10 every 15 ticks. The strongest
   overlapping liquid wins, and contiguous feet/torso/head samples determine
   depth. Liquid brushes are static:
   changing a Door-bound brush to liquid removes its mover binding, and the
   cooker rejects any malformed liquid mover that reaches it.
6. A project may have one player source. Move the template's **Player Spawn**,
   or delete it and choose **Tool → Add → Player Spawn**, then click an
   upward-facing brush floor. **Point Light** is available from the same Add
   menu. In Top view, placement uses the upward surface closest to the shared
   Y focus; in 3D, click the visible floor directly.
   For a moving door, add **Logic**, change its Inspector kind to **Door**,
   then select the door brush and choose that node under **Model owner**.
7. Save with **File → Save** (`Cmd/Ctrl+S`) and press **Play**. Play saves the
   authored state, cooks the PXBSP, builds the real PS1 runtime, and boots it
   in the editor viewport. The first build is slower because it compiles the
   guest; later iterations reuse it.
8. Stop or return to editing, make a change, then press **Rebuild & Play**.
   No generated-file copying or command-line cook is required. Use the Play
   button's chevron to switch between fast **Draft** and lit **Release** cooks
   and to inspect the latest PS1 memory/packet budget.

If Play refuses the map, the status strip reports the cook error and focuses
the first actionable brush/node/resource where possible. A Player Spawn inside
solid is normally fixed by placing it again on the room's upward floor face.

For a windowless acceptance of this exact author/save/Play contract, run
`make editor-blank-playtest-check` from the repository root. It authors a fresh
BSP room through the editor command APIs and also drives the visible starter,
3D selection, Move, and Resize controls with real `egui::RawInput`. It verifies
that the roofless courtyard and its neutral cobble/brick textures match their
generator byte for byte, persists and cooks the authored artifact, builds a
real `mipsel-sony-psx` guest and disc, then replays the disc twice in the
headless PSoXide emulator. The gate pins the spawn, movement into the authored
wall, player-radius collision stop, frame and triangle counters, VRAM/display
hashes, and a byte-identical GPU command census. It rejects any emitted image
artifact. This is programmatic UI and emulator evidence, not native manual GUI
or original-hardware evidence.

For a no-image runtime proof of the BSP liquid contract, run
`make editor-bsp-liquid-check`. It generates a temporary lava-brush project,
cooks the real PXBSP, builds the `mipsel-sony-psx` guest and disc, boots it
headlessly, and requires guest telemetry showing deterministic lava damage and
checkpoint/spawn respawn. It writes no screenshots or framebuffer dumps.

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
