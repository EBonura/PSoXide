+++
title = "psx-residency-sim"
description = "Host-side route simulator for the residency policy"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-residency-sim` on the host to check a streaming plan before it exists on a console. It runs the real `Residency` engine from [psx-residency](@/docs/crates/psx-residency.md) against a model of the drive, replays a route through a graph of regions, and reports deadline slack and misses. It is not part of a guest build: games depend on `psx-residency` only.

## How the crate is organized

`RegionGraph` describes the regions (size in sectors, position, disc layout) and `Route` walks through them at a speed. `SimConfig` sets the budgets, and `simulate` returns a `SimReport` with the number of misses, the minimum slack and the `WorstMiss`. `DriveModel` is the drive: read time per sector at double and single speed, and a seek-time table. The `churn` module measures how a page pool fragments under variable-length allocations with and without compaction.

## What is measured and what is assumed

The read times (6.49 ms per sector at double speed, 13.20 ms at single speed) and the seek times at 1, 16, 128 and 512 sectors (11, 79, 137 and 310 ms) are silicon measurements from the streaming survey, and the seek figures swing about 2x between runs because rotational phase dominates. Everything else is an assumption the types make visible: seeks between measured points are interpolated linearly, seeks beyond 512 sectors are clamped unless you supply a figure, a cancelled read stops instantly unless you supply an abort time, and install work costs nothing unless you supply ticks per region. A result is only as good as these inputs.

## API, dependencies and source structure

{{<sdk_crate name="psx-residency-sim" />}}
