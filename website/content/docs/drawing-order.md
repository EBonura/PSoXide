+++
title = "Control drawing order with an ordering table"
description = "Sort overlapping primitives and submit a frame through a DMA linked list."
weight = 5
[extra]
kind = "How-to"
eyebrow = "How-to · SDK"
+++

{{<example_player name="hello-ot" />}}

## Build and try it

After [setting up the SDK](@/docs/first-ps1-program.md#1-install-the-tools):

```sh
make disc EXAMPLE=hello-ot
```

Open `build/examples/mipsel-sony-psx/release/hello-ot.cue`. Three coloured triangles overlap and drift across the screen. Watch which triangle covers which at their intersections.

## Choose what draws first

The PS1 does not give these primitives a per-pixel depth buffer. Drawing order determines what covers what. An ordering table is a set of linked-list buckets: put each primitive into a depth slot, then submit the list to the GPU.

This example uses 16 slots. The GPU receives slot 15 first and slot 0 last, so lower slots appear in front. These are chosen sort keys, not a measurement the GPU computes for you.

```rust
OT.clear();
OT.add(10, &mut TRIS[0], TriGouraud::WORDS);
OT.add(8, &mut TRIS[1], TriGouraud::WORDS);
OT.add(6, &mut TRIS[2], TriGouraud::WORDS);
```

The third triangle is in front of the second, which is in front of the first. Within one bucket, insertion order matters too: `add` prepends the packet. Do not treat equal-depth triangles as automatically sorted.

## Keep packets alive until DMA finishes

`TriGouraud` stores a GPU packet and its linked-list tag. The example puts the ordering table and triangle packets in static RAM so their addresses remain valid while DMA reads them.

The `unsafe` block covers access to these mutable statics. This example has one writer. In a larger renderer, use a clearly owned frame arena and wait for the previous submission to finish before clearing or reusing its memory. A Rust reference alone cannot tell you whether the GPU is still reading a packet.

## Submit the frame

After updating vertices and colours, clear the table, link the packets, clear the back buffer and submit:

```rust
fb.clear(0, 0, 48);
OT.submit();
```

The checked-in example then waits for vertical blank and swaps buffers. Keep its complete frame loop when experimenting; copying only the packet creation leaves out the display and synchronization setup.

## Try a change

Swap the first and third depth slots (`10` and `6`), rebuild, and compare the overlap. The geometry has not moved, but the triangle in front changes. Then put all three in one slot and observe how insertion order affects the result.

For 3D geometry, choose a sort key from transformed depth before inserting the primitive. The [GTE guide](@/docs/project-3d.md) introduces transforming and projecting vertices. Ordering tables do not solve every visibility problem: intersecting polygons can still require splitting or a different draw strategy.

## Troubleshooting

If an entire list disappears, check packet word counts and linked-list addresses. If the order is reversed, check the slot convention before changing the geometry. If a later frame corrupts an earlier one, check packet lifetime and synchronization before adding more primitives.
