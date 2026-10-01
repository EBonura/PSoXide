+++
title = "psx-tick"
description = "Fixed-timestep game clock and frame consistency counters"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-tick` to run game logic in fixed ticks while rendering as often as the frame cost allows. Logic speed then never depends on how long a frame takes to draw.

## How the crate is organized

The single `no_std` module has no dependencies. `TickRate` sets the tick length in whole VBlanks: `HZ60`, `HZ30`, `HZ20` or `every_vblanks(n)`. `CatchUp` decides what happens when a frame falls behind, and `TickConfig` pairs the two. `FixedClock` applies a config, and `TickStats` holds its consistency counters: frames, ticks, dropped ticks, the most ticks any frame ran, and a histogram of ticks per frame.

## Integration notes

The clock never reads hardware. Pass it the current VBlank count, which on the console is `psx_rt::interrupts::vblank_count()` from [psx-rt](@/docs/crates/psx-rt.md); that keeps every rule testable on the host. Each frame, call `due(now)` in a loop and run one tick every time it returns true, then call `end_frame` once.

`CatchUp::Unbounded`, the default, runs every owed tick before the next render. `Interleave(n)` runs at most `n` per frame and carries the rest forward, so a long stall is caught up over several rendered frames. Neither loses logic time. `Cap(n)` drops the excess and counts it in `TickStats::dropped`, which lets game time fall behind real time; keep it for cases where a burst would be worse than a slowdown. After a load or a pause menu, call `realign` so that time isn't caught up; it isn't counted as dropped.

At tick rates slower than one per VBlank, `phase_q12` reports how far the current VBlank is from the last tick toward the next, in Q12. Drawing `lerp(previous, latest, phase)` costs one tick of latency and moves something on every rendered frame. A game that ticks every VBlank can draw the latest state instead. Schedulers that decide before committing can use `is_due` and `consume`, which ignore the catch-up policy.

There is no standalone `psx-tick` example in the SDK set.

## Complete host example

```rust
use psx_tick::{FixedClock, TickConfig, TickRate};

// One rendered frame: run every tick the clock allows, then close the frame.
fn frame(clock: &mut FixedClock, now: u32) -> u32 {
    let mut ran = 0;
    while clock.due(now) {
        ran += 1; // run one game-logic tick here
    }
    clock.end_frame();
    ran
}

fn main() {
    // 20 Hz logic, at most four ticks per rendered frame.
    let config = TickConfig::new(TickRate::HZ20).with_interleave(4);
    let mut clock = FixedClock::new(config, 0);
    assert_eq!(frame(&mut clock, 0), 1);
    // Two thirds of the way to the next tick (Q12): draw lerp(previous, latest, phase).
    assert_eq!(clock.phase_q12(2), 2730);
    assert_eq!(frame(&mut clock, 3), 1);
    // A stall until VBlank 30 owes nine ticks; they run four per frame.
    assert_eq!(frame(&mut clock, 30), 4);
    assert_eq!(frame(&mut clock, 30), 4);
    assert_eq!(frame(&mut clock, 31), 1);
    assert_eq!(clock.tick(), 11);
    assert_eq!(clock.stats().max_frame_ticks, 4);
    assert_eq!(clock.stats().dropped, 0);
}
```

Run as a host binary with a path dependency on `sdk/crates/psx-tick`; it does not touch hardware.

## API, dependencies and source structure

{{<sdk_crate name="psx-tick" />}}
