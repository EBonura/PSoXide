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

## Games, and which side each falls on

Surveyed 2026-10-23 from each repository's flip sites and the measurements already on file
(REGISTRY.md, the lane notes, frame-timing.md). A game picks the cap up when it repins the SDK.
"Cap-safe" means the game never presents faster than 30 fps, so nothing changes;
"cap bounds" means it sometimes does and the cap is the intended fix; "opt out" means it
targets 60 and must call `set_min_flip_vblanks(PresentCap::UNCAPPED.min_vblanks())` when it repins.

| Game | Flip path | Current present rate (source) | Side |
| --- | --- | --- | --- |
| hl-psx | queued flip; direct in menus | 17.1 fps mean on a recorded route, 32% of flips one VBlank after the last (hl-boot-polish/NOTES.md, 10-23) | cap bounds in play; menus opt out (frame-counted loops) |
| cs-psx | queued flip | 26.1 solo, 19.7 split, 4.6 to 15.8 on cs_estate (README, docs/cs-psx-plan.md) | cap bounds; same scoping as HL |
| quake-psx | present queue | E1M1 route 50.7 fps, 82% of frames at 1 VBlank (frame-timing.md, 09-28) | cap bounds (its docs say 30 fps goal) |
| hk-psx | queued flip | mean 1.96 VBlanks per frame, 285 of 2,965 intervals at 1 (docs/PERFORMANCE.md, 09-15) | cap bounds, about 10% of frames. Its scene transitions queue display-enable words through the same queue; those are never held |
| oot-psx | direct swap | 8 to 20 fps, at least 3 VBlanks per frame in play | cap-safe in play; title and cutscene loops flip per VBlank and need the opt-out or VBlank-counted timing |
| wipeout-psx | queued flip | 56 to 59 fps one player, 2P holds 2 VBlanks by hand (docs/STATUS.md) | opt out for one player; two player already agrees |
| nitroxide | engine, every VBlank | works to a 1 VBlank budget (nitroxide-perf/HANDOFF.md, 10-15) | opt out for one player; split screen already 30 |
| voxide | direct swap | tape shows 2 to 5 VBlanks per frame (voxide-render/RESULT.md, 10-15) | cap-safe in play; menus and pause may flip faster |
| celeste, celeste2, collection | direct swap | 59.8 fps, 99.6% of frames at 1 VBlank (frame-timing.md) | opt out (60 fps by design) |
| alttp-psx | direct swap, logic per presented frame | no measurement | opt out until measured: capping would halve game speed |
| psxcel | engine, every VBlank | no measurement | cap bounds (a static spreadsheet UI needs no more than 30) |
| gh-psx | engine, every VBlank | no measurement | opt out (rhythm game, song clock on VBlanks) |
| arcade games, arcade and demo-disc launchers | engine / direct swap, every VBlank | no measurement | opt out for the games; the launchers are the owner's call |
| Cortex (editor playtest) | present queue, every second VBlank | 26.7 fps, 75% at 2 VBlanks (frame-timing.md) | cap-safe |
| ds-psx | engine, every third VBlank | 3.4 fps median | cap-safe |

Engine games set their pacing through psx-engine's `visual_pacing`; the cap sits below it, so
`EveryVBlank` games need the explicit opt-out too. Hardware-test and diagnostic programs that
loop on `wait_vblank` and swap should opt out as well.
