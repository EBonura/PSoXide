# Infinite horizon on PS1: feasibility study and implementation plan

**Date:** 2026-08-23
**Question:** can the engine render a convincing infinite ground/sea plane meeting a
skybox at the horizon, like the Quake expansion reference shot?
**Answer:** yes, and it does not require anything the hardware can't do. The
trick is that you never render to infinity. You render to the distance where the
plane is already within one pixel of the horizon, then dissolve into the sky.

This is a study for the implementing agent. It ends with milestones. Nothing has
been built.

---

## 1. What the reference image actually contains

Four separate things, and it matters that they're separate:

1. A **sky** filling everything above the horizon (clouds, sun bloom).
2. A **ground/sea plane** filling everything below it, with visible surface
   detail near the camera that fades out with distance.
3. A **horizon line** where the two meet, with no seam and no hard mesh edge.
4. Ordinary **world geometry** (the tower, the rock) standing on the plane.

Item 4 is just normal BSP geometry and needs no new work. Item 1 you're
supplying. The study is about items 2 and 3, and 3 is the one that actually
sells the illusion. A ground plane with a visible outer edge reads as a disc,
not as an ocean, no matter how big you make the disc.

---

## 2. Why the obvious approaches fail here

| Approach | Why it fails on this hardware |
|---|---|
| One enormous quad from camera to far distance | Affine texture mapping. A ground plane seen at a grazing angle is the single worst case for affine warping, and one quad spanning near-to-far has unbounded perspective variation. The texture will swim and shear violently. |
| Paint the ground into the lower half of the skybox | Zero parallax. The ground would move with the camera's rotation but not its translation, so walking forward wouldn't move the surface. Fine for distant mountains, fatally wrong for a floor. |
| A very large flat mesh with uniform grid spacing | Wastes almost all vertices. In screen space, everything past moderate distance collapses into a few pixels near the horizon, so uniform world spacing spends most of its budget on sub-pixel quads. |
| Per-pixel/analytic ground (raymarched plane) | No programmable rasteriser. Not available. |
| Render literally to infinity | `SZ` is 16-bit and the GTE divide has limited range. Also unnecessary; see section 4. |

---

## 3. What the engine already has

Good news: most of the supporting machinery exists, and there's a direct
precedent for camera-locked backdrop geometry.

| Piece | Where | Relevance |
|---|---|---|
| Quake view-ray layered sky | `engine/crates/psx-bsp/src/sky.rs` | 10x12 screen-space lattice, constant cost, drawn at OT slot 2047. Projects by view direction, so its horizon is automatically correct. Shared with quake-psx. |
| Sky cyclorama + panorama | `engine/crates/psx-game-runtime/src/sky.rs` | Two 4bpp pages, a dedicated CLUT row per altitude band. Owns a **rotation-keyed packet cache**, which is exactly the caching pattern the ground plane wants. |
| **Far vista ring** | `psx-level`, `LevelFarVistaRecord` | A camera-following cylinder of textured cards drawn between sky and room geometry, with radius / height / vertical offset / segments / tint. This is the precedent: camera-locked backdrop geometry is already a thing here. It is a *vertical* backdrop though, so it solves distant silhouettes, not the floor. |
| Water surfaces | `engine/crates/psx-game-runtime/src/water.rs` | Sparse tiled quads with material animation, emitting `TriTextured` / `TriTexturedGouraud`. Closest existing thing to a textured animated surface. |
| Adaptive subdivision | `engine/crates/psx-engine/src/classic_affine.rs` | `AdaptiveSubdivisionProfile`, subdivide once at OTZ 136, twice at OTZ 60, with texel-error budgets. The existing answer to affine warping. |
| Forced draw order | `DepthPolicy::Fixed(i32)` in `render3d.rs` | Lets a surface be pinned to an explicit OT slot instead of a computed average depth. Important; see section 6. |
| Texture windows | `psx_gpu::material::TextureWindow` | Tiling within a page, already used by the sky. |

OT depth is 2048 (`runtime_config.rs:193`; 1024 under one config). Sky sits at
2047, the far end.

**Gap:** there is no ground/sea plane of any kind. That's the whole job.

### 3.1 The projection constants you'll be working against

From `engine/examples/editor-playtest/src/runtime_config.rs:75-84`:

| Constant | Value |
|---|---:|
| `SCREEN_W` / `SCREEN_H` | 320 / 240 |
| `SCREEN_CX` / `SCREEN_CY` | 160 / 120 |
| **`FOCAL`** | **320** |
| `NEAR_Z` | 4 |
| **`FAR_Z`** | **1024** |

Two of these need care.

`FOCAL` is **320**, not the 160 mentioned in `psx-bsp/render.rs`. That 160 is the
GTE `H` register for a different consumer's 90-degree projection. Use 320 for
Cortex, and read it from `PROJECTION` rather than hardcoding either number.

`FAR_Z = 1024` is **not a far clip**. `WorldProjection::new` only takes
`near_z` (`render3d.rs:756`), and the only near/far rejection is
`vertex.z <= 0 || vertex.z < near_z` (`render3d.rs:767`). `FAR_Z` feeds
`WORLD_DEPTH_RANGE: DepthRange::new(NEAR_Z, FAR_Z)` (`runtime_config.rs:206`),
which is the **depth-to-OT-slot mapping**, plus room visibility and shadow bias.

That distinction is what makes this feature possible at all: geometry past 1024
units still projects and still draws. What breaks past 1024 is only the *depth
sort*, because every such vertex saturates into the last OT slot. Section 6's
recommendation to bypass computed depth entirely is therefore not a stylistic
preference. It's mandatory.

---

## 4. The key insight: you don't need infinity

This is the result that makes the feature tractable, and the implementing agent
should internalise it before writing code.

The engine's projection is (`render3d.rs:771`):

```
sy = screen_y - (y * focal_length) / z
```

For a camera at height `h` above the plane, a point at horizontal distance `d`
lands this many pixels below the horizon line:

```
offset_pixels = focal_length * h / d
```

Set that to one pixel and solve for `d`:

```
d_horizon ≈ focal_length * h
```

With `FOCAL = 320`:

| camera height above plane | distance at which the plane is within 1 px of the horizon |
|---:|---:|
| 24 units | 7,680 |
| 40 units (roughly Quake eye height) | 12,800 |
| 64 units | 20,480 |
| 128 units | 40,960 |

So the plane does not extend to infinity. It extends to roughly `FOCAL * h`, and
the last ring is drawn in exactly the sky's horizon colour so it dissolves rather
than ends. Anything past that is sub-pixel geometry drawn into a band the sky
already covers.

Three consequences the implementing agent must plan for:

- The required radius **scales linearly with camera height**. Derive it from the
  live camera each frame; don't bake a constant. A flying camera at 128 units
  needs a disc five times wider than a crouched one.
- These radii are **10x to 40x beyond `FAR_Z = 1024`**. That is fine for
  projection (there's no far clip) but fatal for computed depth sorting. See
  section 3.1 and section 6.
- 16-bit `SZ` tops out at 65,535, so heights beyond about 200 units start
  crowding the ceiling. If the design needs a high-flying camera, cap the outer
  radius below `SZ` range and accept a slightly-below-horizon rim, which the
  colour fade hides anyway.

---

## 5. Proposed technique: camera-locked polar mesh with screen-uniform rings

A disc of geometry centred on the camera, built so that each ring occupies
roughly constant screen-space height, textured from world-space UVs, and faded
to the sky colour at the rim.

### 5.1 Ring placement (the part that makes affine mapping survive)

Don't space rings evenly in world space, and don't space them geometrically
either. Space them evenly **in screen space**, then invert the projection to get
world distances.

Pick the screen rows you want ring boundaries to land on, `y_1 > y_2 > ... > y_M`,
measured in pixels below the horizon, evenly spaced. Then:

```
d_k = focal_length * h / y_k
```

That yields ring distances growing hyperbolically, and it's optimal: every ring
is the same height on screen, so no ring is wasted on sub-pixel geometry and no
ring spans a huge perspective range. Bounded perspective variation per quad is
precisely what keeps affine warping under control.

Cost is trivial. It's one divide per ring per frame, about a dozen divides,
recomputed only when camera height changes.

### 5.2 Angular sectors

Uniform angular spacing is already correct here. A sector of angle `θ` at
distance `d` spans an arc of `d·θ`, which projects to `FOCAL · θ` pixels
regardless of `d`. Constant screen width for free.

Only generate the arc inside the view frustum plus a margin, not the full circle.
With `FOCAL = 320` on a 320-wide screen, the horizontal half-angle is
`atan(160/320)`, about 26.6 degrees, so the FOV is roughly 53 degrees and only
about 15% of the full ring is ever visible. Generating the whole circle would
waste something like six sevenths of the mesh.

For 32-pixel-wide quads you want `θ ≈ 32/320` radians, so about 10 sectors
across the visible arc plus a margin sector each side.

### 5.3 World-locked UVs, camera-locked mesh

The mesh follows the camera so it's always centred underfoot. The **UVs derive
from world XZ**, not from mesh position. Otherwise the texture slides with the
camera and the surface reads as a treadmill.

Snap the mesh centre to a whole multiple of the texture tile size in world XZ.
Without snapping you get shimmer as vertices creep across texels; with it, the
tiles stay welded to the world while the mesh glides under them.

### 5.4 The fade is what sells it

Per-vertex Gouraud colour interpolating toward the sky's horizon colour as
distance grows. The outermost ring is *exactly* the horizon colour, so the mesh
has no visible edge. This is fake fog and it is doing two jobs: hiding the rim,
and giving the depth cue that makes a flat plane read as vast.

Get the target colour from the sky asset rather than hand-tuning a constant, or
the two will drift apart whenever the skybox changes.

### 5.5 Waves (optional, matches the reference)

Displace ring vertices in Y from a small sine LUT, sum two or three octaves,
scroll the UVs. **Taper amplitude to zero over the last two or three rings.** If
distant vertices bob, the horizon line becomes a wobbling edge and the illusion
dies instantly.

---

## 6. Draw order: `DepthPolicy::Fixed` is mandatory here

Not a preference. Two independent reasons force it.

First, the whole disc lives past `FAR_Z = 1024`, so every ring's computed depth
saturates into the same OT slot and the rings would have no defined order
relative to each other at all.

Second, even inside range, averaged `SZ` on huge grazing quads is unstable.
Neighbouring ring quads swap order between frames and flicker.

So assign each ring an explicit slot via `DepthPolicy::Fixed`, decreasing
outward, with the outermost ring immediately in front of the sky at 2047. The
order then comes from mesh topology, which is known, monotonic, and free.

Ground goes behind all world geometry and in front of the sky. Reserve a small
contiguous slot band for it and document the reservation, so a later feature
doesn't quietly claim the same slots.

---

## 7. Cost, and how to measure it honestly

Rough shape at 12 sectors x 12 rings across the frustum: about 144 quads, on the
order of 1,400 packet words. Not free, and this engine has no headroom to spare
at ~19.7 fps on the E1M1 route. For scale, today's profiling puts Cortex's whole
model-geometry submission at 26.9% of work instructions and collision at 20.7%,
so a new pass of this size is a material addition, not a rounding error.

Two distinct costs, and they need separate measurement:

- **CPU**, building and projecting the mesh. Mitigate by caching packets keyed on
  snapped camera position and rotation, exactly as `SkyCyclorama` already does.
  A stationary camera should rebuild nothing.
- **GPU fill**, because the plane covers most of the lower half of the screen.
  This is the risk that matters. Today's profiling work found that GPU
  back-pressure is real, measurable, and gets charged to the CPU when it next
  touches GP0 at the frame flip.

Measure the fill cost directly with the tooling added today:

```bash
frontend launch --path <disc> --pc-line-log lines.csv --pc-line-start-route-tick 300 \
  --mmio-stall-line-log mmio.csv --dump-hash
python3 tools/pc_line_attribution.py mmio.csv <guest.map>
```

If `FrameBuffer::begin_swap` climbs after the ground plane lands, the feature has
pushed the frame GPU-bound and the answer is less fill, not faster CPU. See
`docs/phase1-profile-2026-08-23.md` sections 4 and 5.2. Take a baseline capture
**before** starting.

Fill mitigations, cheapest first: keep the texture small and opaque, avoid
semi-transparency entirely, and consider drawing the outer rings flat-shaded
without a texture. They're nearly horizon-coloured anyway, so the texture fetch
buys almost nothing there.

---

## 8. Known traps

Most of these are already-paid-for lessons in this codebase.

- **Under-subdivision.** Prior measurement found the engine under-subdivides:
  37% of surfaces sat at roughly 12 texels of error. A ground plane at a grazing
  angle is the worst case in the whole renderer. Tune
  `AdaptiveSubdivisionProfile` for this surface specifically and verify visually
  at several camera heights, not just one.
- **The GTE divide clamp.** `H/SZ` is capped, which previously shrank models
  within about 160 units of the camera. The innermost ring will be very close to
  the camera. Check the near rings for exactly this.
- **Everything past `FAR_Z`.** Room visibility and shadow bias both key off
  `FAR_Z = 1024`. The ground disc extends well past it. Confirm the ground pass
  doesn't get culled by room visibility, and that it isn't dragged into the
  shadow-bias path.
- **Texture swim.** Covered in 5.3. If the surface appears to slide as you walk,
  the mesh snapping is wrong.
- **Horizon mismatch.** The sky's horizon and the plane's must coincide. The
  view-ray sky in `psx-bsp/sky.rs` gets this right automatically. The cyclorama
  panorama needs its horizon band pinned to the plane height.
- **Popping on rebuild.** Add hysteresis to the snap threshold so a camera
  hovering on a boundary doesn't rebuild every frame.
- **Don't judge it from a single still.** A static screenshot cannot show texture
  swim, popping, or wobble, which are the three most likely failure modes. Every
  visual check needs motion, and prior work here has been burned by single-frame
  A/Bs.
- **RAM.** Verify against the arena and packet budgets before assuming the mesh
  fits; this engine runs close to its limits.

---

## 9. Milestones

Each one should be visually verified in motion before the next starts.

1. **Flat grey disc.** Camera-locked polar mesh, screen-uniform rings, no
   texture, no fade, fixed OT slots. Prove the geometry and ordering are right.
2. **Fade to sky colour.** Add the per-vertex horizon fade. The rim should become
   invisible. This is the go/no-go moment for the whole illusion.
3. **World-locked texture.** Tiled texture with world-space UVs and centre
   snapping. Walk around and confirm no swim.
4. **Subdivision tuning.** Attack affine warping on the near rings. Compare at
   several camera heights.
5. **Capture the cost.** Route log, GPU frame stats, and MMIO attribution against
   the milestone-0 baseline. Decide whether outer rings need to drop to
   flat-shaded.
6. **Waves.** Only after cost is understood, with amplitude tapered to zero at
   the rim.
7. **Author-facing record.** Follow `LevelFarVistaRecord`'s shape: radius policy,
   tile asset, fade colour source, ring/sector counts, wave amplitude.

---

## 10. Open decisions

These need your call before milestone 1.

- **Which engine.** The far vista, cyclorama and water all live in
  `psx-game-runtime` and are used by Cortex through `editor-playtest`. But
  `psx-bsp/sky.rs` is deliberately renderer-neutral and shared with quake-psx.
  Since the reference is a Quake shot, say whether this targets Cortex,
  quake-psx, or a shared crate. It changes where the code goes and whether the
  neutrality constraint applies.
- **Sea or land.** Waves and UV scrolling are only worth building for water. A
  static desert or plain wants milestone 6 skipped entirely.
- **Camera height range.** The disc radius scales with it (section 4). A
  ground-bound player is cheap. A flying camera needs a much larger radius and
  should be designed for now rather than retrofitted.
- **Does the far vista ring stay?** A silhouette ring between sky and ground
  would sit naturally with this and add a lot of depth, but it's extra fill.

## 11. Bottom line

This is achievable, and the engine is closer than it looks because
camera-locked backdrop geometry, packet caching, adaptive subdivision, forced OT
slots and Gouraud fading all already exist. The two things to get right are the
screen-uniform ring spacing, which is what lets affine mapping survive a grazing
plane, and the fade to horizon colour, which is what removes the rim.

The genuine risk isn't correctness, it's fill rate. Budget for measuring it.
