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

This example uses 16 slots. The GPU receives slot 15 first and slot 0 last, so lower slots appear in front. You choose the sort keys; the GPU doesn't compute them.

```rust
let mut ot_frame = ot.frame();
let [back, middle, front] = &mut tris;
*back = red;
*middle = green;
*front = blue;
ot_frame.add(10, back);
ot_frame.add(8, middle);
ot_frame.add(6, front);
```

The third triangle is in front of the second, which is in front of the first. Within one bucket, insertion order matters too: `add` prepends the packet, so the most recently added packet in a slot draws first. Do not treat equal-depth triangles as automatically sorted.

## Keep packets alive until DMA finishes

`TriGouraud` stores a GPU packet and its linked-list tag. `ot.frame()` clears the table and returns an `OtFrame` that borrows the table, and `add` takes each packet by exclusive reference for the length of that borrow. The compiler therefore refuses to let you move, reuse or drop a packet, or clear the table, while the frame is still linked.

`submit` consumes the frame, starts the DMA and waits for the walk to finish, which releases the borrow. A frame that overlaps the next one's work needs packet storage that outlives a single call; the SDK has a `FrameStorage` type for that, documented in the [psx-gpu](@/docs/crates/psx-gpu.md) API reference.

## Submit the frame

After updating vertices and colours and adding the packets, clear the back buffer and submit through the GPU's DMA token:

```rust
fb.clear(&mut gpu, (0, 0, 48));
ot_frame.submit(gpu.dma_mut());
```

The checked-in example then waits for vertical blank and swaps buffers. Keep its complete frame loop when experimenting; copying only the packet creation leaves out the display and synchronization setup.

## Try a change

Swap the first and third depth slots (`10` and `6`), rebuild, and compare the overlap. The geometry has not moved, but the triangle in front changes. Then put all three in one slot and observe how insertion order affects the result.

For 3D geometry, choose a sort key from transformed depth before inserting the primitive. The [GTE guide](@/docs/project-3d.md) introduces transforming and projecting vertices. Ordering tables do not solve every visibility problem: intersecting polygons can still require splitting or a different draw strategy.

## Troubleshooting

If an entire list disappears, check packet word counts and linked-list addresses. If the order is reversed, check the slot convention before changing the geometry. If a later frame corrupts an earlier one, check packet lifetime and synchronization before adding more primitives.
