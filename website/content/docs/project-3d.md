+++
title = "Project a 3D cube with the GTE"
description = "Rotate fixed-point vertices, project them into screen coordinates, and draw a wireframe cube."
weight = 6
[extra]
kind = "How-to"
eyebrow = "How-to · SDK"
+++

{{<example_player name="hello-gte" />}}

## Build and try it

After [setting up the SDK](@/docs/first-ps1-program.md#1-install-the-tools):

```sh
make disc EXAMPLE=hello-gte
```

Open `build/examples/mipsel-sony-psx/release/hello-gte.cue`. A white wireframe cube rotates against a dark background. It uses the PS1's Geometry Transformation Engine (GTE) for vertex projection, then the GPU for drawing lines.

## Describe the cube

`CUBE_VERTS` contains eight corners. The example uses fixed-point values with 12 fractional bits: `0x1000` represents 1.0 and `0x0800` represents 0.5. `CUBE_EDGES` contains twelve pairs of vertex indices. Keeping shape data separate from drawing code makes it easy to replace the cube later.

## Set up the projection

The example centres the view at `(160, 120)`, chooses a projection distance and moves the object forward:

```rust
scene::set_screen_offset(160 << 16, 120 << 16);
scene::set_projection_plane(200);
scene::load_translation(Vec3I32::new(0, 0, 0x3000));
```

The screen offsets use 16 fractional bits, which is why they are shifted by 16. The vertex coordinates use a different scale. Follow the API's units rather than applying one fixed-point format everywhere.

Moving the cube away from the camera keeps its depth positive for the perspective divide. This small example is not a general near-plane clipping system.

## Rotate and project each frame

```rust
let yaw = Mat3I16::rotate_y(frame.wrapping_mul(YAW_STEP));
let pitch = Mat3I16::rotate_x(frame.wrapping_mul(PITCH_STEP));
let rot = yaw.mul(&pitch);
scene::load_rotation(&rot);
```

These rotation helpers use 256 angle units per turn. The GTE transforms each vertex and returns its projected screen position:

```rust
let p = scene::project_vertex(*v);
projected[i] = (p.sx, p.sy);
```

The loop then connects the projected endpoints using `LineMono` primitives drawn with `gpu.draw`. Projection and rasterization are separate operations: the GTE gives you coordinates, and the GPU draws them.

## Try a change

Set `PITCH_STEP` to `0` and leave `YAW_STEP` at `4` to see rotation around one axis. Then reduce `YAW_STEP` to `1`. Rebuild and compare. Try changing the projection plane from `200` to `160`: the cube should occupy less of the screen without changing its vertices.

Keep a positive translation large enough for the whole cube to stay in front of the view. Moving vertices through the camera without clipping can produce extreme or invalid projected coordinates.

## Next: filled geometry

Replace edges with triangle faces and sort those faces before drawing. The [ordering-table guide](@/docs/drawing-order.md) explains the submission step. A solid renderer also needs clipping and back-face handling; this wireframe example does not implement them.
