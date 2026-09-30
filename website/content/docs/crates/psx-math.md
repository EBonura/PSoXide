+++
title = "psx-math"
description = "Fixed-point numbers, clipping and text formatting"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-math` for integer and fixed-point calculations that recur in gameplay, rendering and user interfaces.

## How the crate is organized

`sincos` provides angle and trigonometric helpers. `int32` covers scalar arithmetic, integer square roots and wide multiply/divide operations. `fmt` writes decimal text without `core::fmt`. `color` contains colour scaling and interpolation helpers. `attributed_clip` supplies allocation-free clipping traversal with caller-defined attributes.

## Integration notes

Choose arithmetic by its rounding and overflow contract. Saturation, wrapping, truncation toward zero and arithmetic shifts can produce different results, especially for negative values. The clipping traversal leaves distance calculations and attribute interpolation to your adapter; it does not choose a game's coordinate format.

The [ordering-table example](@/docs/examples/hello-ot.md) uses the math crate alongside rendering. A complete host-side calculation looks like this:

```rust
fn main() {
    assert_eq!(psx_math::int32::isqrt_u32(81), 9);
    assert_eq!(psx_math::int32::isqrt_u32(80), 8);
}
```

Place this in a host binary with a path dependency on `sdk/crates/psx-math`.

## API, dependencies and source structure

{{<sdk_crate name="psx-math" />}}
