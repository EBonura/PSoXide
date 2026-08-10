# Camera-space subdivided affine rendering

Status: reusable implementation and emulator validation complete; physical
console validation remains pending.

This document describes the rendering path extracted from the Quake PSX
comparison and implemented in PSoXide. It is intended as an integration guide
for other PSoXide games.

## What the technique actually is

The PlayStation GPU is still an affine texture mapper. This path does not add
per-pixel perspective division to the GPU. It makes affine mapping closely
approximate perspective-correct mapping by splitting a polygon in camera space,
interpolating UV and lighting values there, projecting every generated point
through the GTE, and drawing the resulting smaller affine primitives.

That distinction matters:

```text
source triangle in camera space
        |
        +-- interpolate position, UV, and light at fixed midpoints
        |
        +-- project original and generated vertices through the GTE
        |
        +-- choose a bounded one-level or two-level topology from depth
        |
        +-- emit compact textured triangles and quads
        |
        +-- link the staged packet stream into the ordering table once
```

True perspective mapping interpolates `u/z`, `v/z`, and `1/z`, then divides per
pixel. The PS1 GPU instead interpolates `u` and `v` linearly in screen space.
Camera-space subdivision reduces the depth variation inside each emitted
primitive, so the affine error becomes much smaller. This is why the result has
far less texture bending and swimming while retaining native PS1 rasterisation.

Do not split edges at their already projected screen coordinates. The projected
location of a camera-space midpoint is generally not the screen-space midpoint.
Screen-space splitting produces more polygons but does not reproduce the
perspective relationship that creates the visual improvement.

## Which PSoXide path to use

PSoXide now has two related integrations. Both reuse the SDK's GTE, material,
primitive, and ordering-table facilities.

### PSoXide-native games

Use the general adaptive world renderer in `psx_engine::render3d` when the game
already uses PSoXide's native scene and world types:

```rust
use psx_engine::render3d::{
    AdaptiveSubdivisionKindMask, CullMode, DepthPolicy, WorldSurfaceOptions,
};
use psx_gpu::material::TextureMaterial;

let material = TextureMaterial::new(clut, tpage).with_dither(true);
material.apply_draw_mode();

let options = WorldSurfaceOptions::new(depth_band, depth_range)
    .with_depth_policy(DepthPolicy::Average)
    .with_cull_mode(CullMode::Back)
    .with_adaptive_subdivision_sector_size(sector_size)
    .with_adaptive_subdivision_kinds(AdaptiveSubdivisionKindMask::ALL);
```

This path is scale-aware and is the preferred starting point for a new game.
The texel-error study in
[`texture-warping-2026-07-27.md`](texture-warping-2026-07-27.md) recommends a
two-to-four-texel predicted-error budget when content provides enough data to
derive one.

### Retained or legacy renderers

Use `psx_engine::classic_affine` when integrating an existing renderer that
already owns visibility, materials, lighting, and a contiguous packet arena.
This is the path used by Quake. Its public entry points are re-exported from
`psx_engine`:

| API | Purpose |
| --- | --- |
| `ClassicAffineVertex` | Camera-space position plus UV, tint, projected screen coordinate, and cached depth. |
| `ClassicAffineProfile` | Viewport, OT depth, subdivision thresholds, and crack-underdraw policy. |
| `submit_classic_affine_fan` | Project and emit a convex world-surface fan. |
| `ClassicAffineBatchSurface` | Vertex range and material words for one fan in a contiguous world batch. |
| `submit_classic_affine_batch` | Project several contiguous fans in shared RTPT groups, then emit them in descriptor order. |
| `ClassicAliasFace` | Packed final packet-space UVs and projected-cache byte offsets for one alias-model triangle. |
| `submit_classic_alias_model` | Project shared model vertices once and emit indexed world-model faces. |
| `submit_classic_alias_view_model` | Emit a first-person model as separately ordered screen packets. |
| `ClassicAffineSubmit` | Returns the next packet pointer plus packet and hardware-triangle counts. |

The implementation is in
`engine/crates/psx-engine/src/classic_affine.rs`. Compact packet types are in
`sdk/crates/psx-gpu/src/prim.rs`, and the batch OT linker is in
`sdk/crates/psx-gpu/src/ot.rs`.

`submit_classic_affine_packed_fan` is currently experimental. It fuses Quake's
packed BSP preparation with projection, but it measured slower than the regular
fan path in the Quake workload. Do not adopt it as the default until another
content layout demonstrates a measured win.

## Exact Quake reference topology

`ClassicAffineProfile::QUAKE_REFERENCE` is configured for a 320x240 viewport,
a 2,048-slot ordering table, and Quake's historical effective GTE depth scale:

```rust
pub const QUAKE_REFERENCE: ClassicAffineProfile = ClassicAffineProfile {
    screen_width: 320,
    screen_height: 240,
    ot_depth: 2048,
    subdivide_once_at: 136,
    subdivide_twice_at: 60,
    underdraw_slot_bias: 8,
};
```

Each convex fan is first decomposed into root triangles. The average cached GTE
depth of a root selects one of three bounded topologies:

| Root OTZ | Geometry emitted per root triangle |
| ---: | --- |
| `136..2047` | One Gouraud textured triangle. |
| `60..135` | Three edge midpoints, reprojected; one quad plus two triangles. |
| `1..59` | A fixed 12-midpoint, two-level lattice; six quads plus four triangles. |

The one-level path represents four hardware triangles. The two-level path
represents sixteen. Compatible leaves are packed into GP0(3Ch) Gouraud textured
quads to reduce packet and command overhead without changing the subdivision
lattice.

At a subdivision-band boundary, independently rounded projected edges can
leave one-pixel cracks. The profile can emit edge-aligned underdraw triangles
eight OT slots behind the surface. These fillers preserve the historical image
without changing the visible surface topology.

The values `60`, `136`, and `8` are reference behavior, not universal camera
distances. OTZ depends on the game's coordinate scale, GTE transform, ZSF3 and
ZSF4 weights, and OT mapping. Copying the thresholds while changing that depth
contract changes both quality and cost.

## Why the Quake image changed so much

The historical and PSoXide comparisons used the same source textures, CLUT
rows, UVs, model triangles, animation frames, and PS1 affine GPU commands. The
original visual mismatch came from depth-scale coupling.

The historical source attempted to write ZSF3 and ZSF4 with `MTC2`, although
those values live in GTE control registers and require `CTC2`. Its effective
weights therefore remained at the legacy initialization values of 341 and 256.
The first PSoXide bridge correctly installed 682 and 512, doubling OTZ. Because
the legacy renderer compared OTZ directly with `60` and `136`, many intended
subdivisions were skipped. Reproducing the historical effective depth scale,
then moving the topology into the reusable Rust path, restored the smoother
mapping.

This also explains why the better screenshot was not evidence of a different
GPU texture-mapping mode. It was evidence that many more camera-correct
subdivision points reached the same affine GPU.

## Integration contract

### 1. Load the existing GTE scene state

The classic path calls PSoXide's scheduled projection and cached-depth helpers.
Before submission, configure the normal scene state with `psx_gte::scene`:

```rust
use psx_gte::{math::Vec3I32, scene};

scene::set_screen_offset(160 << 16, 120 << 16);
scene::set_projection_plane(focal_length);
scene::load_rotation(&camera_rotation);
scene::load_translation(Vec3I32::new(tx, ty, tz));
scene::set_avsz_weights(zsf3, zsf4);
```

Use the same rotation, translation, projection plane, screen offset, and AVSZ
weights that define the rest of the pass. The implementation deliberately
reuses:

- `scene::project_triangle_scheduled` and `scene::project_vertex_scheduled`;
- `scene::average_cached_z3` and `scene::average_cached_z4`;
- `scene::classic_otz3_from_sum` when the classic `ZSF3 = 0x155` depth
  contract is fixed;
- `scene::screen_area_mac0` for software winding tests; and
- `scene::screen_area_and_classic_otz3_scheduled` when an indexed face needs
  both winding and the classic OT key.

It does not replace or duplicate the SDK's GTE implementation.

The combined indexed-face helper loads cached SXY once, issues NCLIP, and uses
the required eight-instruction MAC0 result gap for the exact
`(z0 + z1 + z2) * 0x155 >> 12` shift-add sequence. It therefore returns the
same classic OT key without a separate AVSZ3 command or a second GTE schedule.
Use `screen_area_and_average_cached_z3_scheduled` instead when the active ZSF3
is deliberately configurable. The Quake integration still requires a console
run before the new combined schedule is described as silicon-verified.

`ClassicAffineVertex::position` must be in the pre-projection 3D space expected
by the currently loaded GTE rotation and translation. That can be model space,
world space, or camera space according to the renderer's established contract.
If positions are already in camera space, load an identity rotation and zero
translation. The essential requirement is that midpoint interpolation happens
in 3D before perspective projection. A fixed affine model/view transform
commutes with midpoint interpolation, so model-space subdivision is also valid
when the matching transform remains loaded for every generated point.

### 2. Prepare a convex fan and scratch tail

For `vertex_count` source vertices, allocate `vertex_count + 12`
`ClassicAffineVertex` records. The final 12 records are writable scratch for
the fixed two-level lattice. Populate only the source records:

```rust
use psx_engine::ClassicAffineVertex;

let vertex = ClassicAffineVertex {
    position: [source_x, source_y, source_z],
    uv: [u, v],
    color: r as u32 | ((g as u32) << 8) | ((b as u32) << 16),
    screen: [0, 0],
    depth: 0,
};
```

Vertices must describe one convex polygon in fan order. Concave polygons must
be triangulated or split into convex surfaces before this API is called.

#### Optional cooked-prefix fast path

A retained renderer can remove most per-frame vertex materialisation by making
the immutable prefix of its cooked world vertex match the first 12 bytes of
`ClassicAffineVertex`:

```text
offset  size  field
0       6     position: [i16; 3]
6       2     final packet-space UV: [u8; 2]
8       4     final packet RGB word: u32
```

Keep the record 12 bytes wide so every source vertex remains word-aligned. For
a surface whose texture placement and lightstyles are static, the runtime can
copy these three words directly into the 20-byte submission record; `screen`
and `depth` remain writable outputs filled by PSoXide. Quake marks the UV and
lighting halves independently, so animated textures retain local UVs and
animated lightstyles retain two raw light contributions. The fallback still
copies the aligned position/UV prefix, then resolves only the dynamic fields.

Perform any cooker analyses that need raw light contributions, such as leaf
lighting, before replacing them with the baked RGB word. Version the cooked
format when adding this union and reject older maps at load time. Also audit
the generated MIPS: the common path should be three aligned loads and three
aligned stores, not `LWL`/`LWR` reconstruction. This is a content-layout
optimization in the game cooker, not a replacement for the PSoXide projection
or packet APIs.

#### Optional compact tree references

A retained BSP renderer should compact cold relationships before compacting
the references used by every visible node and leaf. Quake keeps direct pointers
for node children and leaf marksurfaces, but stores the parent as a signed
16-bit node index. The parent index fits in alignment padding between the
one-byte contents field and the four-byte visibility generation, so the
runtime node remains 48 bytes and the leaf remains 36 bytes.

The loader validates every cooked child index, resolves children and
marksurfaces to pointers once, and assigns parent indices after the complete
tree is resident. Traversal and surface collection then use one direct load in
their hot loops. Only the less frequent upward walk performed when the view
enters a new leaf resolves `nodes + parent_index`.

On E1M3 this retains the same 18,524-byte saving as compact children plus a
compact marksurface index, but avoids decoding those references every frame.
This is a useful general rule for pointer-heavy retained renderers: spend
indices on cold navigation and pointers on measured hot navigation. It is a
game data-layout decision and does not replace any PSoXide rendering API.

Custom profiles should maintain these invariants:

- `subdivide_twice_at <= subdivide_once_at < ot_depth`;
- every visible depth key fits in `0..ot_depth`;
- `root_otz + underdraw_slot_bias` remains below `ot_depth` whenever underdraw
  can be emitted; and
- `screen_width` and `screen_height` match the active GPU draw area.

### 3. Reserve worst-case packet memory

The output pointer must be four-byte aligned and remain live until GPU
submission completes. A conservative Quake-reference bound is 832 bytes per
root fan triangle, or `832 * (vertex_count - 2)` bytes. That covers the near
two-level lattice plus all possible crack-underlay packets. Real scenes usually
emit much less, but packet-arena capacity must be checked against worst-case
near-camera content.

`ClassicTriTexturedGouraud` is 40 bytes including its tag.
`ClassicQuadTexturedGouraud` is 52 bytes including its tag. Alias models use
the 32-byte `ClassicTriTextured` packet.

### 4. Keep texture-window state stable for the compact pass

The compact GP0(24h), GP0(34h), and GP0(3Ch) packets omit an inline GP0(E2)
texture-window command. Apply the correct texture-window and draw-mode state
before the pass and do not let another material change it while these packets
are pending.

If a scene needs different texture windows in one ordering table, either group
the passes with explicit state packets in correct draw order or use PSoXide's
standard textured primitive types that include the E2 word. A `tpage` and CLUT
change is safe because those values are present in every compact textured
packet. A texture-window change is not.

### 5. Submit and batch-link the packet stream

The basic retained-renderer call is intentionally small:

```rust
use psx_engine::{submit_classic_affine_fan, ClassicAffineProfile};

let first_packet = packet_cursor;
let submitted = unsafe {
    submit_classic_affine_fan(
        vertices.as_mut_ptr(),
        vertex_count,
        first_packet,
        tpage,
        clut,
        ClassicAffineProfile::QUAKE_REFERENCE,
    )
};
packet_cursor = submitted.next_packet;
```

Small world fans benefit from a contiguous batch because an independent
five-vertex fan pays one RTPT plus two RTPS tails. `submit_classic_affine_batch`
projects across fan boundaries in groups of three, then applies the exact same
per-fan clipping, depth, quad pairing, subdivision, and underdraw policy:

```rust
use psx_engine::{
    submit_classic_affine_batch, ClassicAffineBatchSurface,
    ClassicAffineProfile,
};

let surfaces = [ClassicAffineBatchSurface {
    first_vertex: 0,
    vertex_count: fan_vertex_count as u16,
    tpage,
    clut,
}];
let submitted = unsafe {
    submit_classic_affine_batch(
        batch_vertices.as_mut_ptr(),
        batch_vertex_count,
        surfaces.as_ptr(),
        surfaces.len(),
        packet_cursor,
        ClassicAffineProfile::QUAKE_REFERENCE,
    )
};
packet_cursor = submitted.next_packet;
```

The batch owns one internal packet writer for the complete call. It updates
only the tpage and CLUT words between descriptors, rather than reconstructing
the writer and copying its cursor, counters, and profile into and out of every
fan. Keep that lifetime when adapting the implementation. It is a measurable
part of the optimization, not just an internal refactor.

Reserve 12 full vertices after the entire batch, not after every surface. All
descriptor ranges must point inside the projected prefix. Preserve descriptor
order when same-slot OT order matters. Quake fits 39 root vertices plus the
12-vertex subdivision lattice in the PS1's 1 KiB scratchpad; 39 also avoids a
projection tail because it is divisible by three. Treat that number as a
layout-specific example, not a universal API limit.

Each packet initially stores its GPU data-word count in tag bits 24 through 31
and its target OT slot in tag bits 0 through 15. After all retained code has
finished appending packets, link the complete contiguous stream once:

```rust
unsafe {
    ordering_table.insert_tagged_packet_stream_unchecked(
        frame_packet_start,
        packet_cursor,
    );
}
```

This reproduces repeated classic `addPrim` prepend order without paying for a
Rust or C boundary call per primitive. A staged slot of `0xffff` is a sentinel:
the linker skips that packet. Quake uses the sentinel for first-person weapon
and HUD packets, which it records in a separate screen-space list and attaches
after the world. Skipped packets are not submitted automatically, so a caller
using the sentinel must keep and link that second list itself.

Store the integer depth slot directly. Do not allocate a shadow C ordering
table merely to form `shadow_base + depth` and subtract the base again in the
packet macro. Removing Quake's unused shadow table saved 8,196 bytes of BSS.
A controlled A/B matched all 179 display hashes, all 1,050 GPU-stat rows, and
the final framebuffer; the 45-cycle average render difference was 0.004 percent
and within measurement noise.

`insert_tagged_packet_stream_unchecked` is unsafe because malformed packet
lengths, an invalid end pointer, or an out-of-range non-sentinel OT slot can
walk corrupt memory. Build every staged packet with the compact primitive
constructors or validate an equivalent C layout exactly.

The hottest Gouraud packet path also provides
`with_staged_slot_prepacked_unchecked`. It avoids masking colors that have
already been proven to contain RGB only in bits 0 through 23, and accepts CLUT
and tpage words that have already been shifted into their packet positions.
This is a checked contract at the caller boundary, not permission to pass raw
game words. Keep debug assertions beside the conversion from game data and use
the regular constructor when that invariant is not structurally guaranteed.

### 6. Batch indexed models separately

For alias-style animated models, provide `vertex_count * 3` packed XYZ bytes,
an array of `ClassicAliasFace` records, and `vertex_count` writable
`ClassicAliasProjectedVertex` records. Each face corner stores its final
packet-space U and V bytes alongside the byte offset of its projected-cache
record. With the current eight-byte record, cook this as `vertex_index * 8`;
the runtime must validate that every offset is aligned and below
`vertex_count * 8`. Apply a skin's atlas base in the cooker; if skins occupy
different atlas positions, cook one face stream per skin. These two cooker
steps remove index scaling plus six byte additions and masks from every
submitted triangle. The batch projects each shared vertex once, then performs
winding, cached-depth, screen, and packet work per face. Reserve one 32-byte
`ClassicTriTextured` packet per input face. Culled faces do not consume packet
memory.

`submit_classic_alias_model` writes normal OT slots. The view-model variant
writes the `0xffff` screen sentinel described above, so its returned packets
must also be registered with the caller's separate weapon or HUD ordering
policy.

### 7. Keep frame resources alive across asynchronous submission

If the game overlaps CPU construction of frame N+1 with GPU drawing of frame
N, use at least two packet arenas and two ordering tables. Do not clear or
reuse either resource until its DMA submission has completed. Apply draw state
for a new list only after the preceding list can no longer consume it.

## Reusable GTE helpers around the renderer

The Quake integration exposed two common fixed-point operations that now live
in `psx_gte::scene`. Games should call these SDK helpers instead of carrying
private C or Rust copies.

### Matrix composition

`compose_rotation_scheduled(left, right)` returns the Q12 matrix product using
three scheduled MVMVA operations, one for each column of the right matrix:

```rust
use psx_gte::scene;

let model_view = scene::compose_rotation_scheduled(&view, &model);
scene::load_rotation(&model_view);
```

The helper zeros translation before composing, clamps the three output columns
to `i16`, and clobbers the live GTE rotation and translation registers. It is
appropriate for model, joint, and camera rotation/scale composition. Keep
wide `i32` world translations on the CPU unless they can be represented safely
as GTE vector inputs. PSoXide's native `render3d` joint path uses this same
helper, so a retained renderer does not need its own matrix multiply.

For a C or C-like retained engine, mirror the SDK layouts with naturally
aligned `#[repr(C)]` Rust types and verify the ABI on the C side. Quake asserts
an 8-byte, 2-byte-aligned short vector; a 16-byte, 4-byte-aligned long vector;
and a 32-byte, 4-byte-aligned matrix. Its bridge can therefore use ordinary
typed loads and stores rather than `read_unaligned` and `write_unaligned`.
Those contracts must be proven by the caller. Do not silently apply the same
optimization to packed file records or arbitrary byte buffers.

Quake also routes its hot retained matrix operations through the shared GTE
schedule:

- matrix multiplication and diagonal scaling use
  `compose_rotation_scheduled`;
- vector application loads the rotation with zero translation and calls
  `transform_vertex_scheduled`; and
- full matrix composition reuses the loaded left rotation for one additional
  scheduled transform of the right translation.

The vector, scale, and composed-right-translation inputs in this bridge are
known to fit signed 16-bit GTE inputs. A game with wide world coordinates must
range-check, split, or retain a wide CPU implementation. Keep rotation and
translation loads as separate semantic operations when call sites need only
one of them; an apparently convenient combined loader can increase code size
and instruction-cache pressure across the whole renderer.

### Classic Q12 vector normalisation

`normalize_classic_q12_scheduled(input)` implements the historical PS1
reciprocal-square-root path with GTE SQR and GPF operations plus the exact
192-entry lookup table:

```rust
use psx_gte::{math::Vec3I32, scene};

let normalized = scene::normalize_classic_q12_scheduled(direction_q12);
let unit_q12 = normalized.vector;
let xy_squared = normalized.xy_squared;
let squared = normalized.squared;
```

The input is Q12 `i32`; each component is truncated to its signed integer part
before squaring to preserve classic engine behavior. The returned vector is Q12
`Vec3I16`, accompanied by the integer XY and XYZ squared lengths commonly
needed by movement, AI, and projectile code. A zero vector returns a zero
result. The helper clobbers GTE IR, MAC, and leading-count data registers but
does not replace matrix or projection control state.

This normaliser is not required to draw a subdivided polygon. It belongs in
the same integration guide because it removes another expensive private helper
that retained 3D engines often call in synchronized update bursts. In Quake it
replaced an integer square root plus three divides without changing any of 599
captured display hashes.

## Adapting the profile to another game

Start from a deterministic camera route and a pixel reference, then tune one
variable at a time:

1. Run the far path with subdivision disabled and verify projection, UVs,
   lighting, winding, materials, and OT order.
2. Enable one-level camera-space subdivision and lower its threshold until
   objectionable warping and swimming return. Move back to the last clean
   value.
3. Enable the two-level band only for the nearest or steepest surfaces that
   still exceed the visual error budget.
4. Test band boundaries during camera motion. Enable or tune the underdraw
   bias only where cracks are actually possible.
5. Size the packet arena from the worst visible near-camera case, not the
   average room.
6. Batch-link the staged stream and profile CPU, GTE, GPU, DMA, cache stalls,
   packet count, and bytes per frame separately.
7. Validate multiple spaced frames, not one screenshot, then run the same
   camera route on physical hardware.

For a new PSoXide-native game, prefer a camera-space or texel-error profile over
raw OTZ thresholds. If exact legacy matching is required, first record the
legacy ZSF values and depth-to-OT mapping, then encode those assumptions in a
named `ClassicAffineProfile` instead of scattering constants through draw code.

## Performance rules learned from Quake

The technique spends additional primitives to buy texture stability, so the
CPU path around those primitives must stay lean.

- Project shared source vertices once. Do not repeat RTPS for every face of an
  indexed model.
- Pre-apply model-skin atlas offsets to packet UVs in cooked face streams.
- Use PSoXide's scheduled RTPT/RTPS helpers instead of hand-written GTE command
  wrappers.
- When an indexed face uses the classic depth scale, use
  `screen_area_and_classic_otz3_scheduled` so the exact CPU depth calculation
  fills the silicon-required NCLIP result gap.
- Batch adjacent small world fans so RTPT groups can cross fan boundaries;
  keep fan submission order and a separate shared subdivision tail unchanged.
- For retained static geometry, align the cooked vertex stride and pre-bake
  packet-space UV and RGB fields so the immutable SDK prefix is copied as
  three words instead of reconstructed every frame.
- Keep direct pointers for relationships traversed per visible node or surface;
  move compact indices to cold parent or ownership walks when struct padding
  can hold them without increasing the record size.
- Audit the final MIPS for critical materialisation helpers. A compiler-created
  out-of-line call per surface can erase a data-layout win; use targeted forced
  inlining only after an A/B identifies that exact regression.
- Compose model and joint rotations with `compose_rotation_scheduled` rather
  than maintaining another scalar Q12 matrix multiplier.
- Use `normalize_classic_q12_scheduled` for classic movement and AI vectors
  instead of a private square-root and division path.
- Reuse cached SZ values with `average_cached_z3` and `average_cached_z4`.
- Use `scene::screen_area_mac0` when an exact signed screen-area test is enough.
- Stage OT slots in contiguous packet tags and link the stream once.
- Use compact packets only when texture-window state is constant.
- Pack compatible subdivision leaves into GPU quads when the topology and
  winding are identical to the triangle pair.
- Average two independent `u8` UV lanes as one `u16` with a carry-isolating
  mask. Exhaustively verify all byte pairs, and do not assume the same packed
  formula wins for wider color fields without measuring it.
- Keep visibility and coarse surface rejection outside the inner leaf emitter.
- Measure fused data-preparation paths. Fewer language crossings do not
  guarantee fewer guest cycles.

In the Quake route, the tagged stream linker preserved pixel output while
reducing measured instruction-cache stalls from 56.69 million to 49.52 million,
about 12.6 percent. Moving indexed alias models to a shared-projection Rust
batch also produced a pixel-exact CPU reduction. On Quake's 600-frame Start
route, the 39-vertex world-fan batch reduced the world-surface stage from about
695,937 to 660,260 guest cycles per frame. All 599 compared display hashes,
GPU command counts, and emulator GPU-cycle estimates remained identical. The
prebaked projected offsets and combined NCLIP/AVSZ3 schedule then reduced the
model stage from about 380,256 to 348,830 cycles and total render time from
1,330,871 to 1,299,324 cycles. Again, all 599 display hashes and every recorded
GPU metric matched the world-batch baseline exactly.

Finally, Quake changed its cooked world vertex from an unaligned 10-byte input
to the 12-byte prefix described above. On Start, 71.4 percent of world vertices
are both UV- and light-static and use the direct-copy loop; Episode 1 maps range
up to 88.6 percent. In an identical-executable controlled A/B, all 1,049 GPU
rows and all 179 display hashes matched while the world-surface stage fell from
612,795 to 519,516 cycles and total render time fell from 1,192,375 to
1,099,039 cycles.

The current shared-path snapshot also overlaps alias NCLIP with classic OTZ,
uses GTE matrix composition, and replaces Quake's private integer vector
normaliser with the shared scheduled SDK helper. Replacing the scalar bridge
matrix operations with those GTE schedules reduced the 600-frame Start route's
steady model stage from 299,714 to 286,668 guest cycles and total update plus
render work from 1,061,094 to 1,047,792. It also reduced the executable payload
by 4 KiB. The image cadence crossed a VBlank boundary near the end of that run,
so later same-index animation hashes are not a valid bit-exact comparison; the
full Episode 1 functional regression passed after the change.

Reusing one packet writer across the complete world batch then reduced the
world-draw stage from 484,330 to 470,666 cycles and total work from 1,047,792
to 1,034,262. That controlled A/B matched all 599 display hashes and all 600
guest-visual rows. Across steady frames 31 through 310, the 95th percentile was
1,051,130 cycles and the maximum was 1,073,786, so every frame in that window
fit the 1,128,960-cycle two-VBlank CPU target. This is strong Start-route
evidence, not yet a claim of sustained 30 fps across the complete episode. The
complete 3,400-frame Episode 1 emulator regression covers every map,
transition, weapon, enemy, required animation range, and boss behavior.
Physical-console validation is still required before calling the result
silicon-verified.

Packing the two midpoint UV averages into one carry-isolated halfword operation
then reduced the complete 3,400-frame route's average update-plus-render work
from 1,033,300 to 1,026,561 cycles, its 95th percentile from 1,605,051 to
1,601,003, and its over-budget frames from 836 to 818. Route time fell from
14,486 to 14,392 VBlank ticks. The first 49 presented frames were pixel-exact
before the faster cadence advanced animation timing, the complete functional
regression passed, and an exhaustive host test proves all 65,536 input-byte
pairs match the original independent floor averages. Applying an analogous
packed operation to the RGB midpoint was slower and was rejected.

The original corrected PSoXide comparison ran at 11.8516 fps versus 11.8580 fps
for the historical C SDK build, with 99.948 percent of pixels byte-identical at
the matched frame. The current optimization work changes CPU submission policy
while holding that visual reference. Emulator GPU-cycle estimates are useful
for deterministic A/B tests, but they do not replace physical PlayStation
timing.

## Common mistakes

- Calling the result true hardware perspective-correct texturing. It is a
  bounded piecewise-affine approximation produced by camera-space subdivision.
- Averaging projected X/Y positions instead of camera-space positions.
- Copying Quake's `60` and `136` thresholds into a game with different ZSF
  weights or world scale.
- Emitting GP0(E2) in every leaf even though one texture window is active for
  the entire pass.
- Reprojecting the same indexed model vertex once per face.
- Crossing a C/Rust boundary for each primitive.
- Removing near subdivisions to improve fps and calling the result an
  optimization. That directly spends the visual quality this path exists to
  preserve.
- Treating subdivision as near-plane clipping. A polygon that crosses the near
  plane still needs a real clipping policy.
- Forgetting that translucency and same-slot prepend order remain ordering-table
  concerns.
- Reusing an in-flight packet arena during asynchronous GPU submission.

## Limitations

This path does not provide per-pixel perspective division, z-buffering,
near-plane clipping, arbitrary concave polygon support, or automatic
translucency sorting. It can increase packet count and GPU fill cost sharply
for large close surfaces. The fixed Quake topology also targets visual parity
with one legacy renderer, not the mathematically minimal tessellation for every
camera and asset.

For projects whose art, camera, and room scale differ substantially from
Quake, PSoXide's adaptive `render3d` path is usually the better foundation. Use
the classic path when exact bounded topology, retained C/Rust interoperability,
or legacy visual matching is the primary requirement.

## Validation checklist

- Source positions, UVs, CLUT, tpage, tint, and animation frame match the
  reference.
- Generated midpoints are computed before projection.
- GTE rotation, translation, screen offset, projection plane, ZSF3, and ZSF4
  are known and stable.
- Winding and signed-area culling match the intended GPU packet order.
- OT slot mapping and same-slot prepend order are deterministic.
- Texture-window state is correct for every compact packet.
- Packet arena covers worst-case subdivision and stays alive through DMA.
- Spaced-frame captures show no new cracks, edge slivers, UV drift, or material
  leakage.
- CPU, GTE, GPU, DMA, cache, packet-count, and packet-byte deltas are recorded.
- The physical-console hardware battery passes before the profile is described
  as silicon-verified.

The key reusable idea is small: interpolate in camera space, reproject every
generated point with the existing GTE SDK, and let the PS1 GPU affine-map only
small, bounded regions. The surrounding packet, depth, and frame-lifetime
policy is what makes that idea practical in a full game.
