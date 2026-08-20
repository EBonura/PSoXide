# E1M1 geometry playtest

Captured on 2026-08-20 with PSoXide's instrumented editor-playtest guest in the
headless emulator. Emulator timing is useful for regression work but is not a
substitute for an original PlayStation burn.

## Imported content

- 1,214 editable brushes and 7,278 faces from Quake E1M1.
- Quake texture, lightmap, entity, model, sound, and music data are not present.
- The stripped BSP29 sidecar retains render nodes, leaves, PVS, polygon
  topology, and world-model bounds.
- Aletha and Rust Mantis are placed together at the original player start.

## Cook and memory

- PXBSP: 537,748 bytes.
- PVS: 56,723 bytes, with a 199-byte decompressed row.
- Cooked RAM payload estimate: 699,853 / 2,097,152 bytes.
- Resident character assets: 178,708 / 704,512 bytes.
- VRAM estimate: 86,496 / 1,048,576 bytes.
- BSP-only packet arena: 1,536 slots. The captured spawn view used about 799
  slots at its final frame, including the two character draws.

## Runtime result

- The normal editor Play feature set links, loads all streamed character
  assets, initializes PXBSP, and reaches gameplay.
- BSP rendering, PVS lookup, point collision, character animation, enemy AI,
  and movement all run.
- Player and enemy scale is consistent with the imported start room. Both full
  bodies are visible in the engine capture.
- The static world uses Quake's spatial node tree as a compact point-collision
  hull. Authored brush-body expansion is intentionally not rebuilt for this
  benchmark because its linear 1,214-brush tree was too slow on PS1.

## Performance

The stationary spawn capture ran for 120 visual frames with the normal editor
Play feature set plus telemetry:

- Render work: 2,032,923 cycles per visual frame.
- 30 fps budget: 1,128,960 cycles per visual frame.
- Deadline misses: 116 / 120 frames.
- Main cost: BSP room traversal and emission at 1,425,959 cycles per rendered
  frame.
- World flush/sort is already small in the default bucketed path at 16,583
  cycles per rendered frame.

The level is functional, but it is not yet a 30 fps level. The next performance
work should reduce BSP leaf/PVS face processing and face emission before model
or ordering-table work is revisited.
