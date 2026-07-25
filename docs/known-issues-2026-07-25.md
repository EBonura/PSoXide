# Known issues carried forward from the 30 fps branch

Date: 2026-07-25
Branch of origin: `perf/engine-30fps`

Three items are deliberately merged unfixed. Each has a measured cost and a
diagnosed cause, so none of them needs rediscovering.

## 1. Cardinal walls draw both faces, costing render time

**State:** shipped as a correctness fix, not the final form.

`wall_material_for_direction` forces `SurfaceSidedness::Both` for every cardinal
wall ([`world_render.rs:241`](../engine/crates/psx-engine/src/world_render.rs#L241)).
That fixed the cortex_v1 report where a wall vanished as soon as the player
entered the cell that owns it: the winding made the owning cell's interior the
back face, and for a wall bounding the playable area that is the only side a
player can stand on.

**Cost, measured on the fixed 900-frame route:** room surface draw +9.09%,
render +4.34%, visual frames 532 -> 518, deadline misses 65 -> 87. Emitted
primitives rise 302 -> 325 per gameplay render, and those 23 are the wall faces
that were previously dropped, so much of the cost is legitimate work.

**The proper fix, and why it is not hard.** The cooker has the grid, so for any
wall it knows which neighbouring cell is open and which is solid. Emitting each
wall single-sided facing its open side is correct *and* free. Only a wall with
walkable cells on both sides needs both faces.

**Do not repeat this mistake.** An attempt to take the cheap path removed the
sidedness swap *globally*, which flipped every wall including those already
facing correctly, so more walls disappeared and the experiment was misread as
"the data needs both faces". Orientation must be decided per wall from grid
adjacency, never by a global flip.

## 2. Box props reserve 8 pre-baked break shards each

**State:** capacity is now cooked from the authored prop count
(`BOX_PROP_STATE_COUNT`), which recovered 172,900 bytes. The per-slot cost is
untouched.

Each `BoxPropRuntime` slot is about 1,406 bytes
([`box_props.rs:186`](../engine/crates/psx-game-runtime/src/box_props.rs#L186)):

| Field | Count | Contents |
|---|---:|---|
| `faces` | 6 | four `WorldVertex`, a centre and an `[i32; 3]` normal each |
| `break_shards` | **8** | pre-baked destruction fragments |
| bounds | — | cull sphere, `floor_y`, `ground_y`, debris bounds, AABB min/max |

Six faces of derived geometry is only a few hundred bytes. **The eight break
shards are the bulk, and every box carries a full set whether or not it can
break.**

All of it is derived data, rebuilt from `LevelBoxPropRecord`, which already
holds the eight authored vertices, tints and baked vertex RGB. It is a cache,
not source, so it can be shrunk or streamed. Cheapest first:

1. **Allocate shards only for breakable boxes.** A static crate needs none.
2. **Scope box-prop runtime to the active room window.** The machinery now
   exists: `required_ram`/`warm_ram`, a reclaiming asset pool, and delta
   residency. Box props are per-room and fit the same pattern as textures.
3. Rebuild faces on demand rather than caching, if the CPU cost measures
   acceptable.

This is also the best candidate for explaining an unresolved regression: cooking
the capacity cost 10 delivered frames and +13 deadline misses, and padding the
arena back did not recover them, which points at code/data layout rather than
size. Removing the cache that should not exist is more likely to come out net
positive than resizing it was.

## 3. Floor tiles vanish near the player in cortex_v1 rooms 6/7

**State:** open, but **localised to floor subdivision, and pre-existing.**

Two experiments closed this down on 2026-07-25.

*Provenance.* Built at `e78b44fe`, the last commit before any render change on
`perf/engine-30fps`, and replayed the same tape. The holes are identical. None
of the three render-path changes on that branch (double-sided walls, material
stand-in, texture eviction) caused it. Getting there needed a workaround: the
older cooker hard-errors on cortex_v1's empty `.wav` files while the current one
tolerates them, so valid 16-bit stubs were written into a throwaway copy. That
cook produced identical geometry (8 rooms, 234 cells, 889 tris), which is what
makes the comparison meaningful.

*Cause.* Setting `ROOM_ADAPTIVE_SUBDIVISION_KINDS` to `WALL` only, disabling
floor subdivision, **removes every hole**. The floor renders textured and
continuous across the whole route. So the loss is in the adaptive subdivision
path for floor surfaces: the generated leaf quads, not the authored surface,
which is consistent with everything else already ruled out.

The original report said "only when tessellating them" and was correct. It was
argued down on a `split_tris` reading of 0, but that counter measures the
HARDWARE splitter, not TR subdivision, so it was never evidence either way.

Disabling floor subdivision is not the fix -- it exists for affine correction on
near floors. The remaining work is in the leaf emission itself; start at
`submit_adaptive_cached_room_quad` and the leaf path in
`world_pass_gouraud.rs`, with `tessellation-debug` colouring to see which leaves
survive.

**Original state, for the record:** open, and provenance unknown. Reported against a build from this
branch; never checked against `main`. Three changes here touch the room render
path (double-sided walls, the material stand-in, live texture eviction), so this
work cannot rule itself out. **Establish whether it reproduces on `main` first.**

The symptom follows the player: tiles disappear in the area *around* them as they
walk. Cooked data is static, so this is a per-frame decision keyed on camera
proximity, not missing data.

**Ruled out, each by experiment:**

- Surface submission. With `room-surface-profile`, `room_surf_profiled` equals
  `room_surfaces_considered` in rooms 6/7, with backface culls ~0.1, zero
  primitive overflows and zero submit fallbacks.
- Cell selection. Forcing `cell_aabb_visible` to accept every cell changed 21 of
  35 route frames but left the floor unchanged.
- Material resolution. Seeding unresolved slots from a resolved material in the
  same room left 31 of 35 frames byte-identical.

**Methodology warning.** The three results above were read as "the geometry was
never cooked", which the follows-the-player symptom contradicts. The error was
aggregating: means over 253 renders cannot see a handful of tiles vanishing per
frame, and `considered == profiled` only proves everything *considered* was
drawn, saying nothing about what was considered. A `split_tris` reading of 0 was
also used to dismiss tessellation, but that counter measures the hardware
splitter, not adaptive subdivision.

**Correct approach:** capture *every* route tick through rooms 6/7, find the tick
a tile blinks out, and diff that frame against tick-1 where the floor is whole.
Then run `tessellation-debug` over the same span; leaves colour cyan, underdraw
yellow, un-subdivided roots red. Proximity narrows the suspects to code that only
runs near the camera: the TR subdivision band,
`draw_near_clipped_cached_room_surface`, and near-plane extent clamping.

## Unrelated, also open

`cortex_v3` does not cook: `sector 1,13 East wall has invalid heights
[0, 0, -64, 320]`. A negative height in a wall quad; one sector to fix in the
editor.
