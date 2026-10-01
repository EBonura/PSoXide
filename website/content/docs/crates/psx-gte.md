+++
title = "psx-gte"
description = "PS1 geometry coprocessor access"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-gte` for matrix transforms, projection and lighting through the Geometry Transformation Engine. On PS1, it emits COP2 operations; on a host, the same wrappers use a software GTE.

## How the crate is organized

`regs` and the exported register macros move data to and from COP2. `ops` wraps individual GTE commands. `scene` provides setup and projection helpers, while `math`, `transform` and `lighting` expose higher-level types and operations. The `host` backend is available only on non-MIPS targets; use its separate API reference below.

## Integration notes

Low-level operations assume that the caller loaded the required input registers. Preserve the documented fixed-point units: screen offsets, matrix coefficients, positions and angle constructors do not all use the same scale. The 256-unit rotation helpers and the interpolated 4096-unit helpers are distinct APIs.

Start with [projecting a cube](@/docs/project-3d.md) and the complete [hello-gte source](@/docs/examples/hello-gte.md). Projection alone does not implement clipping or visibility handling.

## API, dependencies and source structure

{{<sdk_crate name="psx-gte" />}}
