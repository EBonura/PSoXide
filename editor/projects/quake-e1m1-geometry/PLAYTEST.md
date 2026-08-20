# E1M1 geometry playtest

Captured on 2026-08-20 with PSoXide's instrumented editor-playtest guest in the
headless emulator. Emulator timing is useful for regression work but is not a
substitute for an original PlayStation burn.

## Imported content

- 1,214 editable brushes and 7,278 faces from Quake E1M1.
- Quake texture, lightmap, entity, model, sound, and music data are not present.
- Every imported face currently uses PSoXide's 64x64 4bpp BRICK_1A texture as
  a temporary visual placeholder.
- The stripped BSP29 sidecar retains render nodes, leaves, PVS, polygon
  topology, and world-model bounds.
- Aletha and Rust Mantis are placed together at the original player start.

## Cook and memory

- PXBSP v5: 566,868 bytes.
- The 2,912 render nodes use a 46,592-byte table. Retaining conservative
  quantized bounds and exact face ranges costs 29,120 bytes over the old
  6-byte nodes, while remaining 17,472 bytes smaller than the rejected
  exact-bounds layout.
- PVS: 56,723 bytes, with a 199-byte decompressed row.
- Cooked RAM payload estimate: 728,973 / 2,097,152 bytes.
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
Play feature set plus telemetry. All measurements are from the same textured
map and camera route.

| Measurement | Original scan | Compact PVS chain | Node traversal | Tight node bounds | Cross-part seam batch | Final vs original |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Render cycles / visual frame | 2,032,738 | 1,819,167 | 1,628,418 | 1,395,396 | 1,338,026 | -34.18% |
| BSP room cycles / rendered frame | 1,424,780 | 1,209,761 | 998,377 | 779,725 | 778,009 | -45.40% |
| Visual-render task cycles / hit | 2,339,795 | 2,069,804 | 1,905,772 | 1,869,795 | 1,438,038 | -38.54% |
| Skipped vblanks | 318 | 284 | 263 | 258 | 204 | -35.85% |
| Accumulated lateness vblanks | 194 | 162 | 140 | 136 | 82 | -57.73% |
| Deadline misses / 120 | 116 | 116 | 117 | 116 | 63 | still over budget |

The first change replaces the 5,724-face table scan with the current leaf's
sorted, deduplicated PVS chain: 484 faces at the captured camera, or 11.83x
fewer loop entries. The chain is rebuilt only when the camera changes leaf.

PXBSP v5 then retains conservative node bounds and exact node face ranges. On
each leaf change the renderer stamps only nodes leading to PVS-visible leaves;
each frame it rejects stamped subtrees against the view frustum before entering
the existing material, backface, exact polygon-clip, batching, and packet path.
Staticized `func_*` faces remain on the compact fallback chain. Packed scratch
state keeps this traversal below 5 KiB of guest heap.

The node-bound change tightens the six packed coordinates from a
256-world-unit grid to an 8-world-unit grid. The node remains exactly 16 bytes,
so PXBSP and resident-memory sizes do not change. Saturated coordinates still
decode to the full signed 16-bit extent, preserving conservative culling for
maps larger than the packed range. At the E1M1 spawn camera this reduces the
post-traversal face list from 231 to 118. Relative to the previous node build,
render time falls 14.31% and BSP-room time falls 21.90%.

Animated seam vertices previously flushed their 32-entry deferred GTE batch
at the end of every model part. The player has many small parts, so most
batches paid secondary-joint, identity-projection, and primary-restore matrix
loads for only a handful of vertices. The retained renderer now captures each
primary-joint contribution while that part's matrix is live and carries the
same fixed batch across part boundaries. No cooked format or resident asset
changes are required. The hot function's stack frame grows from 720 to 1,216
bytes, still within the explicitly reserved 32 KiB runtime stack.

Relative to the tight-node build, combined animated-model projection falls
from 195,831 to 144,907 cycles per rendered frame (-26.00%), player rendering
falls 15.63%, and total render work falls 4.11%. The cadence improvement is
larger because this crosses the point where many frames no longer wait for a
later presentation slot: visual-render task time falls 23.09%, and deadline
misses fall from 116 to 63 in the same 120-frame run.

Equivalence checks keep all 24 non-time simulation fields exact across the 408
common guest frames and all 17 model/BSP geometry and packet counters exact on
the 28 guest frames rendered by both builds. The host GTE regression also
matches the old per-vertex path bit-for-bit when one deferred chunk crosses two
different primary joints. Fixed-route screenshots retain the static BSP; actor
silhouettes appear at different animation phases because the faster build
reaches 120 visual frames 110 route ticks earlier.

The first room render still pays a one-time node/PVS rebuild hitch, but tighter
culling lowers it from 2.204M to 1.985M room cycles. After it, steady room work
is 0.769M cycles, 22.14% below the previous node build.

Quake-PSX uses the same high-level strategy: rebuild PVS ancestry only on a
leaf change, recursively reject non-visible or out-of-frustum BSP nodes, then
draw only the resulting surface chain. The worthwhile transplant was that
spatial selection, not Quake's packet arenas or blocking submission model;
PSoXide keeps its existing material formats and exact clipping paths.

The level is functional and materially faster, but it is not yet a 30 fps
level. Final render work is still 1.19x the 1,128,960-cycle budget. BSP room
work is again the largest single render stage, so the next steady-state target
is the exact clipping/projection work on the 118 faces surviving the spawn-view
node traversal. A separate short follow-up should move or amortize the
node-visible rebuild before the first presented gameplay frame so the
level-entry hitch is hidden.
