+++
title = "psx-telemetry"
description = "Guest profiling events and shared identifiers"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-telemetry` when you want a program to label work for PSoXide's emulator profiler.

## How the crate is organized

The root defines shared stage, task and counter identifiers and their descriptions. `emit` contains event-writing helpers and cycle readings. Sharing these identifiers keeps guest instrumentation and host decoding aligned.

## Integration notes

The `emit` feature enables the guest-side Expansion 2 writes on MIPS. Without it, emitters are no-ops; cycle readings return zero outside the supported configuration. These values are emulator instrumentation and aren't a portable hardware performance counter. Give event scopes stable meanings across recordings so comparisons stay useful.

There is no standalone telemetry example in the SDK set. Use the API's emitter contracts when instrumenting your own frame loop, and read [the development workflow](@/docs/development-workflow.md) for how profiling fits into iteration.

## API, dependencies and source structure

{{<sdk_crate name="psx-telemetry" />}}
