+++
title = "psx-pad"
description = "Controller polling, button edges and action mapping"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-pad` to read digital controllers and DualShock analog input over SIO0. It returns active-high button masks, even though the controller wire protocol uses active-low values.

## How the crate is organized

The root defines button masks, pad state, polling, analog helpers and action mapping. `tracker::PadTracker`, also reexported at the root, tracks per-button edges and repeat timing.

## Integration notes

Poll once for an update and share that result with your game systems. Use held state for continuous motion and an edge for a one-shot action. A group-level `pressed_since` check and a per-button tracker have different semantics; choose deliberately when testing multiple buttons. Analog mode is not guaranteed: inspect the pad state and support a fallback. Memory-card traffic shares SIO0, so coordinate transactions rather than polling from competing owners.

Read the [controller how-to](@/docs/controller-input.md) and complete [hello-input source](@/docs/examples/hello-input.md).

## API, dependencies and source structure

{{<sdk_crate name="psx-pad" />}}
