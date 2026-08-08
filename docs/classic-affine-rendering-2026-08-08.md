# Classic PS1 affine rendering profile

2026-08-08. Derived from the matched Quake shareware renderer comparison.

## Finding

The better historical image was not produced by a different texture mapper.
Both builds submit ordinary textured polygons to the PlayStation GPU, which
interpolates UV coordinates affinely. The visible quality came from the whole
submission policy around that rasterizer:

1. dithered 15-bit textured output;
2. bounded camera-space subdivision, with every generated midpoint projected
   again before packet emission;
3. exact signed screen-area culling;
4. AVSZ-equivalent depth keys from cached projected depths;
5. deterministic same-slot ordering-table prepend semantics; and
6. hardware-safe GTE input scheduling.

The comparison ruled out different source art or UV conversion. At the matched
camera, transformed weapon vertices, atlas texels, CLUT rows, model UVs,
triangle indices, and animation frames were byte-identical. A traced blue
sliver was isolated to one back-facing weapon triangle accepted by a scheduled
NCLIP wrapper and rejected by the exact signed-area test.

The final image gap was caused by depth-scale coupling in the game-specific
subdivision thresholds. The historical Quake source tried to replace ZSF3 and
ZSF4 with 682 and 512 using `MTC2`, but those weights live in the GTE control
register bank and require `CTC2`. Its effective weights remained at the legacy
initialization defaults of 341 and 256. The first PSoXide bridge correctly
applied 682 and 512, doubled OTZ, and therefore skipped many `otz < 60` and
`otz < 136` subdivisions. Matching the historical effective weights restored
the reference topology.

## Use the existing Rust renderer

PSoXide already implements the general form of the technique. Games should
compose these existing APIs instead of copying a game-specific subdivision
loop:

```rust
use psx_engine::render3d::{
    AdaptiveSubdivisionKindMask, CullMode, DepthPolicy, WorldSurfaceOptions,
};
use psx_gpu::material::TextureMaterial;

let material = TextureMaterial::new(clut, tpage).with_dither(true);
material.apply_draw_mode();

let surfaces = WorldSurfaceOptions::new(depth_band, depth_range)
    .with_depth_policy(DepthPolicy::Average)
    .with_cull_mode(CullMode::Back)
    .with_adaptive_subdivision_sector_size(sector_size)
    .with_adaptive_subdivision_kinds(AdaptiveSubdivisionKindMask::ALL);
```

`WorldSurfaceOptions` routes to the existing adaptive textured-Gouraud pass.
It averages positions in camera space, interpolates UV/light attributes, and
reprojects generated vertices. Splitting an already projected polygon does
not correct affine distortion and must not be substituted for that path.

The named depth profile is deliberately scale-aware. Use
`with_adaptive_subdivision_sector_size` for room-based games, or provide an
`AdaptiveSubdivisionProfile` measured in the game's camera-space units. The
existing texel-error study in `texture-warping-2026-07-27.md` recommends a
two-to-four-texel predicted-error budget when a content-derived profile is
available.

Do not copy raw OTZ thresholds between games unless their ZSF weights and
camera-space scale also match. Prefer the engine's camera-space profile so
ordering-table precision can change without silently changing texture quality.

For indexed model geometry, use `scene::screen_area_mac0` for exact culling and
`scene::average_cached_z3` / `scene::average_cached_z4` when projection depths
are already cached. Those helpers reload the GTE SZ FIFO and use hardware AVSZ;
the `_otz` variants remain available when a pure-software calculation is
required.

For retained packet streams, use
`OrderingTable::insert_packed_commands_unchecked` to reproduce repeated
prepend/add-primitive ordering, or the reverse variant only when caller order
must be preserved within a slot.

## Matched Quake evidence

The deterministic 320x240 tick-7000 capture was compared after a two-billion
instruction route using the stable `route_tick = 1500..7000` performance
window.

| build stage | steady fps | versus historical |
| --- | ---: | ---: |
| historical C SDK | 11.8580 | baseline |
| original PSoXide bridge | 6.5847 | -44.5% |
| visual-corrected PSoXide | 8.4761 | -28.5% |
| asynchronous double buffer | 9.8848 | -16.6% |
| hardware cached-depth AVSZ | 10.6828 | -9.9% |
| C LTO plus direct PSoXide OT, doubled ZSF | 11.8616 | +0.03% |
| corrected historical ZSF and subdivision | 11.8516 | -0.05% |

The dominant gap was a serialized frame boundary. The current path retains two
packet arenas and ordering tables, kicks `OrderingTable::submit_async`, and
waits for the preceding frame only before display/reuse. Hardware cached-depth
AVSZ removed the next per-face CPU cost. Whole-program optimization of the
retained C core and direct world insertion through PSoXide's OT API closed the
remaining compiler and packet-link overhead.

The corrected PSoXide frame has no pixels from the rejected weapon triangle.
Against the historical frame, 76,760 of 76,800 pixels (99.948%) are
byte-identical and the mean absolute difference is 0.020 out of 255 per color
channel. The complete floor region is byte-identical. Both paths emit 26
textured floor quads at the matched camera, versus only 3 in the doubled-ZSF
PSoXide build.

The emulator's GPU-model cycle count is a deterministic estimator, not a
replacement for timing on a physical PlayStation. Keep the GTE schedule gaps
documented in `psx-gte::scene` and complete the hardware battery before calling
the profile silicon-verified.
