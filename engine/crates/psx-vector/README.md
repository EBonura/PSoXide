# psx-vector

Vector graphics for the PS1. An SVG subset is cooked on the host by
[`psx-vector-cook`](../psx-vector-cook) into a compact binary asset (PSVG) of fixed-point Gouraud
triangles and quads grouped by SVG group id. This crate reads the asset in place and draws any group
with an offset, scale, rotation, colour state and clip, as GPU polygons. Polygons stay crisp at the
native resolution, cost no VRAM and animate per piece.

```rust
static HUD: &[u8] = psx_vector::include_vector!("../assets/hud.psvg"); // 4-byte aligned
let asset = VectorAsset::parse(HUD)?;                  // validates once
let plate = asset.group("plate").unwrap();              // look up once, keep the id
let mut scratch = Scratch::<64>::new();                 // >= asset.max_group_vertices()
asset.draw_group(gpu, plate, &Placement::at(6, 6), &Style::tinted([234, 82, 48]), &mut scratch)?;
```

## Cost

Measured in the headless emulator. The stance HUD is 62 GPU triangles in 24 groups (a quad counts as
two); `examples/showcase-vector/run.sh` prints the table.

- Building packets costs about 60 cycles per primitive plus about 500 per group call (+17 for a
  scale, +40 for a rotation, about +55 per vertex for a tint). The whole HUD is about 45k cycles.
  Do it when the HUD's state changes, not every frame.
- **Relinking cached packets** ([`PacketCache`]) costs about 68 cycles per packet: 2.4k cycles for the
  HUD's 35. The GPU then needs about 19k cycles to draw it, overlapping the CPU.
- Drawing straight to the GPU instead (an immediate overlay) blocks the CPU on the GPU FIFO: about
  29k cycles per rendered frame in Cortex, measured as MMIO stall at the draw call.
- A clip rectangle that contains a group costs nothing; one that cuts it is decided per vertex by
  outcode and, for triangles that cross an edge, Sutherland-Hodgman (about 300 cycles per cut
  triangle). For a rectangle clip prefer the GPU draw area (`Gpu::set_draw_area`), which is free.
- Two ordering tables in flight cannot share one cached packet (linking rewrites its tag), so a
  present-queue scene keeps one cache per frame buffer.

## Rules

- 32-bit integer maths only; no float, no 64-bit integers, no allocation (the writer behind the
  `encode` feature is host-only).
- Untextured semi-transparency reads the GPU draw mode, so a draw writes GP0(E1h). Packets carry
  their own mode word, so a linked packet does not depend on what ran before it.
- Screen coordinates must stay inside the GPU's -1024..1023 range unless a clip rectangle is given.
