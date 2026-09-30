+++
title = "psx-fx"
description = "Particles, deterministic randomness and screen shake"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-fx` for small arcade effects without adopting the engine's scene system.

## How the crate is organized

`rng::LcgRng` produces deterministic pseudo-random values. `particles::ParticlePool` owns a fixed-capacity pool of short-lived coloured particles and emits rectangle primitives into an ordering table. `shake::ShakeState` produces a decaying screen offset. These main types are reexported at the root.

## Integration notes

Choose particle capacity for your memory and command budget. Use a stable RNG seed when you need repeatable runs. Update effects on a deliberate simulation tick, then apply shake to the positions you draw and emit particles at the intended ordering-table depth. This crate does not supply collision handling or a general emitter editor.

The [ordering-table example](@/docs/examples/hello-ot.md) is the prerequisite for integrating its particle output. The API reference contains the effect constructors and update/draw contracts; there is no dedicated `psx-fx` program in the SDK example set yet.

## API, dependencies and source structure

{{<sdk_crate name="psx-fx" />}}
