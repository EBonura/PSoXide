# The presentation cap

Every PSoXide game presents at most 30 frames a second (25 on PAL): the
display never flips sooner than two VBlanks after the previous flip. A frame
that finishes early waits for its slot instead of flipping at 60 for a moment
and falling back, which reads as a stutter. Frames slower than the cap are not
delayed, so a game that runs at 12 fps still runs at 12 fps. The cap bounds the
rate from above and never speeds anything up.

A game that deliberately targets the full display rate opts out by name, once
at boot:

```rust
psx_rt::interrupts::set_min_flip_vblanks(psx_tick::PresentCap::UNCAPPED.min_vblanks());
```

`psx_tick::PresentCap` names the settings (`HZ30`, the default, and
`UNCAPPED`); `psx_tick::FlipGate` is the same rule as plain logic for host
tests.

## Where it is enforced

| Flip path | How |
| --- | --- |
| `queue_display_control_at_vblank` (hl-psx, cs-psx, hk-psx, wipeout-psx) | The VBlank handler applies a queued GP1(05h) word only when the hold is zero. Other queued words (display enable) are never held |
| `present-queue` (`publish_raw`, quake-psx, the engine's present queue) | The handler kicks a published chain, and flips with it, only when the hold is zero |
| `DoubleBuffer::swap` / `Gpu::set_display_start` (direct flips) | A bounded wait in front of the flip until the handler has counted the cap's VBlanks off. Skipped with interrupts masked or before `install_vblank_counter` |

The handler arms a hold of `cap` edges at every applied flip and counts one off
per VBlank, so the cost is a few instructions per VBlank. A direct flip written
as a raw GP1(05h) word, bypassing `set_display_start`, is not paced.

## Checking it

The handler keeps a histogram of VBlanks between flips
(`psx_rt::interrupts::flip_histogram`, reset with `reset_flip_histogram`).
`psx_tick::FlipHistogram::from_buckets(..).below(2)` is zero for a game that
honours the cap. `hello-flipcap` runs the queued path, the direct path and the
opt-out and fails when a capped flip comes early (`make hello-flipcap-gate
FRONTEND=...`); `hello-present-queue` checks the same on the present queue.

## Games that loop on a frame count

A loop that flips once per iteration (`wait_vblank(); swap()`), with fades or
timers counted in iterations, takes twice as long under the cap. A game with such
loops either opts out, or counts VBlanks (`vblank_count`) for its timing, which
is also the only form that survives the 25 fps PAL cap.
