# RAM and VRAM survey

Date: 2026-07-25
Project under measurement: `editor/projects/cortex_v1`
Build: `editor-playtest`, features `cd-stream-bench emulator-telemetry`
Branch: `perf/engine-30fps`

Every number below is measured, not estimated. Sources are the MIPS link
output (`mipsel-none-elf-size`, `nm -S`), `rustc -Zprint-type-sizes` over the
guest build, the cooker's stream-memory report, and a `--dump-vram` capture
from the standard 900-frame route.

## Headline

Two resources are near opposite extremes.

- **RAM is 92.4% committed before the game allocates anything dynamic.** The
  static image is 1,877,256 bytes against roughly 2,031,616 usable, leaving
  about 130 KB. Most of that is fixed-capacity arenas sized for budgets the
  project does not use.
- **VRAM is roughly half idle, and textures are being dropped anyway.** At most
  49.4% of the 1 MB is occupied, a contiguous 256 KB quarter is never touched,
  and the run still reports 28 room-material texture drops.

Those two facts point at the same underlying issue: capacities are compile-time
constants chosen for a worst case, not derived from what the cooker actually
emitted.

## RAM

### Sections

| Section | bytes | share of 2 MB |
|---|---:|---:|
| `.text` | 693,088 | 34.1% |
| `.data` | 169,120 | 8.3% |
| `.bss` | 1,015,048 | 50.0% |
| **static total** | **1,877,256** | **92.4%** |
| free gap before the 32 KB stack reserve | ~130,000 | ~6.4% |

### The four statics that matter

| Symbol | bytes | share of static |
|---|---:|---:|
| `RUNTIME_ARENAS` | 665,584 | 35.5% |
| `SCENE` (`Playtest`) | 238,864 | 12.7% |
| `PRIMITIVE_PACKETS` (`PrimitivePacketScratch<1536>`) | 86,016 | 4.6% |
| `WORLD_COMMANDS` | 24,576 | 1.3% |

### Inside `RUNTIME_ARENAS` (665,584 bytes)

| Field | type | bytes |
|---|---|---:|
| `persistent_assets` | `PersistentAssetStreamer<154, 56>` | 318,364 |
| `overlay` | union of `UiImageCache<4815, 7>` (134,884) and `GameplayRoomArenas` (174,100) | 174,100 |
| ↳ `prebuilt_quads` | `PrebuiltRoomQuads<8, 256>` | 116,756 |
| ↳ `room_projection` | `CachedRoomProjection<4096>` | 57,344 |
| `font_scratch` | `FontPackScratch<32910>` | 65,820 |
| `streamed_slots` | `StreamedRoomPages<30, 8>` | 61,492 |
| `debris_cache` | `DebrisCache` | 16,388 |
| everything else | vram runtime, schedulers, materials, sky, model scratch, CD | ~29,420 |

The menu/gameplay union is already doing real work: it saves 134,884 bytes by
overlapping the UI image cache with the gameplay room arenas. That pattern is
the right one and is under-used elsewhere.

### Inside `SCENE` (238,864 bytes)

| Field | type | bytes | share |
|---|---|---:|---:|
| box props | `BoxProps<128, 4, 16>` | 180,000 | 75.4% |
| everything else | | 58,864 | 24.6% |

`MAX_BOX_PROP_STATE` is a hardcoded `128` in `runtime_config.rs:447`.
**cortex_v1 cooks 5 box props.** At 1,406 bytes per slot, 123 unused slots cost
**172,900 bytes, 8.5% of the console's RAM**, for props that do not exist.

### Cooked versus hardcoded capacities

Some budgets already come from the manifest and are correctly project-sized:
`PERSISTENT_ASSET_PAGE_COUNT` (154), `PERSISTENT_ASSET_SLOT_COUNT` (56),
`WORLD_STREAM_SLOT_COUNT` (8), `WORLD_RESIDENT_PAGE_COUNT` (30).

Others are fixed constants that no project can shrink:

| Constant | value | cortex_v1 actual use |
|---|---:|---|
| `MAX_BOX_PROP_STATE` | 128 | 5 cooked box props |
| `MAX_MODEL_INSTANCES` | 16 | 1 placed instance, 25 draws over the whole route |
| `MAX_RUNTIME_MODELS` | 8 | 2 model records |
| `MAX_RUNTIME_MODEL_CLIPS` | 128 | — |
| `MAX_GAME_ENTITIES` | `psx_level::MAX_GAME_ENTITY_RECORDS` | 1 cooked entity |
| `MAX_CYLINDER_PROP_BLOCKERS` | 32 | 1 cylinder prop |
| `MAX_ACTIVE_ROOMS` | 16 | 8 rooms in the whole level |
| `MAX_PORTAL_ROOM_BOUNDS` | 256 | 8 rooms |
| `MAX_STREAMED_ROOM_SLOT_COUNT` | 256 | clamps a cooked 8 |
| `MAX_CACHED_ROOM_VERTICES` | 4096 | 38 projected per gameplay frame |

### Streaming costs more RAM than the data it streams

The cooker reports cortex_v1's entire world:

```
payload=54,220B  sectors=30  stream=61,440B
collision=17,164B (31.7%)  render-cache=36,538B (67.4%)
largest room: payload=17,740B
```

The paged residency pool (`StreamedRoomPages<30, 8>`, 61,492 bytes) is
**larger than the whole level's room payload (54,220 bytes)**. For a level this
size, loading every room once and never evicting would use less RAM than the
machinery that avoids doing so, and would remove 2,450 room requests, 2,100
prefetches and 138 misses per route.

This is not an argument against streaming, which is correct and well tested for
large levels. It is an argument that the cooker should choose: below a
threshold, emit a whole-level residency plan instead of a paging plan.

### UI sound effects are linked into the executable

Five ADPCM samples sit in `.data`:

| Symbol | bytes |
|---|---:|
| `UI_SFX_SAMPLE_003` | 13,584 |
| `UI_SFX_SAMPLE_004` | 12,064 |
| `UI_SFX_SAMPLE_002` | 11,840 |
| `UI_SFX_SAMPLE_000` | 11,200 |
| `UI_SFX_SAMPLE_001` | 8,016 |
| **total** | **56,704** |

That is 2.8% of main RAM holding audio whose only destination is the SPU's
separate 512 KB. They should reach SPU RAM from the disc, not via a permanent
main-RAM copy.

## VRAM

Capture: 1024 x 512 halfwords, occupancy sampled on a 32 x 32 block grid.
**253 of 512 blocks occupied, 49.4%.** Block granularity over-counts, so true
usage is at or below that.

```
      x=  0    128   256   384   512   640   768   896  1023
y=   0  ###################.###.........
y=  32  ################..#.###.........
y=  64  ##########..####....##..........
y=  96  ##########..#.......##..........
y= 128  ##########..........##..........
y= 160  ##########..........##..........
y= 192  ##########..........##..........
y= 224  ##########..........##..........
y= 256  ##########..#######.............
y= 288  ##########..#######.............
y= 320  ##########..#######.............
y= 352  ##########..#######.............
y= 384  ##########..####................
y= 416  ##########..####................
y= 448  ##########..####................
y= 480  ########################........
```

| Region | extent | contents |
|---|---|---|
| x 0-320, y 0-240 | 150 KB | framebuffer 0 |
| x 0-320, y 240-480 | 150 KB | framebuffer 1 |
| x 320-384, all y | 64 KB | **unused gap** |
| x 384-640, y 0-130 | ~65 KB | UI font glyph pages |
| x 384-640, y 130-256 | ~63 KB | **unused** |
| x 384-640, y 256-480 | ~112 KB | model atlases (8bpp) |
| x 640-768, y 0-240 | ~60 KB | room material tpages (4bpp) |
| x 768-1024, all y | **256 KB** | **never touched** |
| x 0-768, y 480-512 | 48 KB | CLUT band |

### The contradiction

`ROOM_TPAGE_BASE_X = 640`, `ROOM_TPAGE_LIMIT_X = 1024`, stride 64 halfwords,
so the room material allocator has 6 tpages to work with. The capture shows it
using roughly the first one and a half, and the same run reports:

```
room material texture drops  total=28  per_frame=0  latest=1
```

Room textures are being dropped while a quarter of VRAM has never been written.
Either the allocator is failing to use the pages it owns, or the drop is
happening on a different axis (CLUT rows, upload queue depth) than raw page
space. The run also reports `vram upload queue full total=128`, which points at
the second explanation: the bottleneck is the upload path, not the address
space.

## Ranked improvements

Ordered by measured bytes recovered per unit of risk.

| # | Change | Recovers | Risk | Notes |
|---:|---|---:|---|---|
| 1 | Cook `MAX_BOX_PROP_STATE` from the box-prop count instead of hardcoding 128 | ~172,900 B RAM | low | Same pattern the manifest already uses for asset pages and stream slots |
| 2 | Move UI SFX from `.data` to disc, stream to SPU RAM | 56,704 B RAM | low | Audio's destination is SPU RAM regardless |
| 3 | Cook the remaining fixed capacities (`MAX_MODEL_INSTANCES`, `MAX_RUNTIME_MODELS`, `MAX_GAME_ENTITIES`, `MAX_CYLINDER_PROP_BLOCKERS`, `MAX_ACTIVE_ROOMS`, `MAX_PORTAL_ROOM_BOUNDS`) | to be measured, likely tens of KB | low | Mechanical once #1 establishes the pattern |
| 4 | Diagnose the 28 texture drops and 128 upload-queue-full events | correctness first, then VRAM | medium | The address space is not the constraint; the upload path is |
| 5 | Whole-level residency plan below a cooked payload threshold | ~61,492 B RAM on small levels, plus 2,450 room requests/route | medium | Keep paging for large levels; let the cooker choose |
| 6 | Make `FontPackScratch<32910>` transient rather than permanently resident | up to 65,820 B RAM | medium | It is upload staging; it does not need to outlive the upload |
| 7 | Extend the menu/gameplay union pattern to other mutually exclusive arenas | varies | medium | The existing union already saves 134,884 B |
| 8 | Pack room tpages into the unused x 768-1024 quarter, or shrink the reservation | 256 KB VRAM available | low | Only worth doing once #4 explains what the real limit is |

Items 1 to 3 are independent, mechanical, and together recover roughly 230 KB,
which is more than the entire remaining free gap. They should land first.

## What this does not cover

- Per-frame allocation churn: there is none, by design; every arena is static.
- SPU RAM occupancy: not measured here.
- Whether `.text` at 693 KB can be reduced. Code overlays were assessed on
  2026-07-23 and rejected as a frame-time measure, but the RAM case for them
  gets stronger as the free gap shrinks.
