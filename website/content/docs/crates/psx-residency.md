+++
title = "psx-residency"
description = "Residency policy for streamed data: page pools, priority classes, pinning and eviction"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-residency` when a game streams its world and has to decide, every frame, what must be in memory, what to read next, what to throw out to make room, and what to do when a read fails. The crate makes those decisions and nothing else. It never touches the drive: requests leave through the `Host` trait and outcomes come back through `Residency::complete_read`, so the same policy runs against the interrupt-driven transport in [psx-cdstream](@/docs/crates/psx-cdstream.md) on the console and against the drive model in [psx-residency-sim](@/docs/crates/psx-residency-sim.md) on the host.

## How the crate is organized

A `ResourceKey` names one streamed resource: a `Category` (world region, collision region, texture page, archetype pack, audio bank, or one you define) and an index inside it. Each category has a `CategoryConfig` budget and allocates from a `PagePool`, which hands out contiguous runs of pages. The `Handle` for a run stops resolving the moment the run is freed or reused, so a stale handle can never read another resource's bytes. `PageBuffer` is the memory behind a RAM pool. `Residency` is the engine, and `slots` holds a smaller fixed-slot policy for games that keep a handful of equally sized slots.

Each frame the game passes a list of `Wanted` entries (key, `Class`, walk distance, optional deadline) to `Residency::step`. Misses are ordered by class, deadline and distance and issued to the host. A request for a resource that is already in flight is adopted rather than repeated, and a more urgent class promotes the request in flight. Room comes from free pages first, then from sliding movable runs down, then from evicting the cheapest contiguous window. The evictions come back in a slice you apply by unlinking whatever pointed at the resource.

## Integration notes

A resource that needs install work (verify, relocate, upload) lands as `State::Landed`; call `begin_install`, do the work in slices, then `finish_install` or `fail_install`. Only then is it resident. Pinned resources are never victims, nor are resources being read or installed. A cancelled request keeps its pages reserved until the transport reports it finished, because sectors may still be landing.

A failed read or install frees its pages and puts the key in backoff, which starts at 16 windows and doubles to 512. The key is never abandoned. Per-category budgets are enforced before anything is allocated, so a category over budget evicts its own entries. `Config::step_budget` caps the bookkeeping a step does; it counts work units, not cycles, so measure on the target before converting.

The policy and the pool are host-tested, including property tests over random request sequences. Nothing in the crate has been run on a console, because it makes no hardware access of its own.

## API, dependencies and source structure

{{<sdk_crate name="psx-residency" />}}
