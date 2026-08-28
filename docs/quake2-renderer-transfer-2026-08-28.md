# Quake II renderer transfer baseline

This branch promotes the previously ignored Quake-PSX `.psoxide` renderer
delta into tracked PSoXide source. The purpose is reproducibility: a clean
Quake-PSX checkout can pin one PSoXide commit and rebuild every recorded
renderer experiment without depending on a locally edited hydration tree.

## Accepted Quake candidate

The measured Quake-PSX candidate used these downstream features:

```text
renderer-selection-cache
renderer-block-frustum
renderer-gpu-polygon-clip
renderer-cell-policy
```

Only `renderer-gpu-polygon-clip` maps to the shared PSoXide engine. It enables
`psx-engine/classic-affine-gpu-polygon-clip`, which keeps source PVS,
backface, conservative 3D frustum, and whole-surface admission intact. After a
surface is admitted, generated GT3 and GT4 children rely on the PS1 GPU draw
area instead of repeating four CPU screen-edge rejection tests.

The fixed-step Quake E1M1 route measured 23.432 fps for the complete downstream
stack. That number is not a Cortex result and must not be assigned to the
shared engine feature in isolation.

## Cortex transfer boundary

Cortex brush geometry reaches the same classic-affine submitter through the
PXBSP renderer, so the clipping policy is exposed to editor playtests as the
opt-in `classic-affine-gpu-polygon-clip` feature. A candidate build can request
it through `EDITOR_PLAYTEST_FEATURES` alongside its normal telemetry features.

The rest of a Cortex frame uses other paths, including the retained grid world,
models, equipment, effects, and UI. Quake's selection cache, 16-face frustum
blocks, and cell policy are downstream Quake data structures and do not
automatically accelerate those paths.

Do not make the candidate a default until a deterministic Cortex A/B proves:

1. identical route state and stable display/VRAM hashes at matched frames;
2. no missing polygons at viewport crossings or near-camera surfaces;
3. lower CPU render cost on the same route rather than a code-layout fluctuation;
4. acceptable packet counts and no arena overflow; and
5. original PlayStation visual and pacing validation.

PSoXide does not model GPU draw or DMA time, so emulator timing is only the CPU
side of this decision. Original hardware remains the performance and visual
ground truth.

## Preserved experiments

The additional feature-gated classic-affine and ordering-table variants are
retained because Quake-PSX already records build actions and measurements for
them. They remain experiments unless a downstream document explicitly marks a
configuration accepted. Default PSoXide behavior is unchanged.

Focused host validation for the imported delta:

```text
cargo test -p psx-gpu
cargo test -p psx-gpu --features ot-window-insert-coalescing
cargo test -p psx-engine --features classic-affine-gpu-polygon-clip classic_affine::tests
```

The complete `psx-engine` host suite has one unrelated fixture-dependent test,
`grounding_probe_player_lowest_vertex_matches_reference`, which requires the
cooked model directory and fails when that generated directory is absent.
