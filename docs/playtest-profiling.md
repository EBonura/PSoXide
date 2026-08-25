# Playtest capture and profiling

Record a live embedded-Play session to an input tape, replay that tape headlessly
to dump deterministic per-vblank profiling data to a file, then chart it. This is
the canonical way to profile a real play-through (for example the 60 Hz sim /
30 Hz render vblank load split) instead of a synthetic input hold.

## 1. Record a tape in the editor

Embedded Play viewport overlay buttons (top-right; `editor/crates/psxed-ui/src/workspace_viewport.rs`):

| Button | Icon | Action (`EditorPlaytestRequest`) |
| --- | --- | --- |
| **Record** | dot (dark red) | `StartInputRecording` / `StopInputRecording`, appends live pad input, saves on stop |
| **Replay** | play / square | `StartInputReplay` / `StopInputReplay`, drives the game from the saved tape |
| **Dump profiler** | file | `DumpProfilerHistory`, writes the rolling profiler ring buffer to the project logs |

Saved tape path (`emu/crates/frontend/src/app.rs::editor_playtest_input_tape_path`):

```
<config>/editor/playtest_tapes/<project>.pxtape          # format magic: PXITAPE1
```

`<config>` = `directories::ProjectDirs::from("com","psoxide","PSoXide").config_dir()`:
- macOS: `~/Library/Application Support/com.psoxide.PSoXide/`
- Linux: `~/.config/PSoXide/`
- override anywhere with `frontend --config-dir <dir> ...`

On **Stop recording** the editor status bar shows `Input recording saved: N frames`.
A tape records from the menu (the project boot flow), so it includes the
menu-to-Play transition. Replay it against the normal menu-boot disc (no
boot-into-gameplay manifest hack needed).

## 2. Replay the tape headless and save results to a file

Build the same embedded-playtest disc the editor builds (menu-boot is correct
here; the tape drives the menu). Template: the `profile-demo3-disc-stream`
target in the `Makefile`, with the project swapped:

```sh
make cook-playtest PROJECT=editor/projects/default/project.ron  # regenerates engine/examples/editor-playtest/generated/
make build-editor-playtest                                  # MIPS guest exe (picks up uncommitted engine changes)
cd tools/mkisopsx && cargo run --release -- \
  --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
  --volume CORTEX_IGNITION_V1 --cdtest-sectors 32 \
  --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
  --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
  --ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
  --ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
  --cdda-track-list ../../engine/examples/editor-playtest/generated/cdda_tracks.txt
```

**The UI pack flags are NOT optional.** Persistent gameplay assets live in the
UI pack, so a disc built without `--ui-pack-dir`/`--ui-pack-order-file` has
nothing at `UI_PACK_START_LBA` and the asset arena waits forever for sectors
nobody wrote. It looks exactly like an engine streaming stall: loading screen
forever, zero sectors, no error. Cost a full session on 2026-07-25.

Then replay and dump (`emu/crates/frontend/src/cli.rs`, `launch` subcommand):

```sh
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest \
  --input-tape "$HOME/Library/Application Support/com.psoxide.PSoXide/editor/playtest_tapes/cortex_ignition_v1.pxtape" \
  --profile-log /tmp/cortex-ignition-v1-vblank.csv \
  --dump-hw /tmp/cortex-ignition-v1-vblank.ppm --dump-hash
```

Result flags:
- `--profile-log <csv>`: one row per vblank. `frame_cycles` total plus per-stage cycle columns (`update`, `render`, `present`, `player`, `room`, `sky`, `image_props`, `world_flush`, `ot_submit`, ...). This is the per-vblank chart source.
- `--counter-log <csv>`: per-frame telemetry counters (room masks, stream stats, camera pose). Used for runtime diagnosis (see `docs/floors-plan.md`).
- `--dump-guest-profile`: aggregate stage/GTE summary to stdout.
- `--dump-hw <ppm>` and `--dump-hash`: final frame image plus vram/display hashes. Look at the frame to confirm gameplay rendered, not menu or sky-only.

Low-level, emulator-owned profiling is opt-in and does not change guest RAM or
emulated timing:

- `--cpu-cycle-profile-log <csv>` writes one row per route tick with disjoint
  issue, main-RAM load/store, MMIO, I-cache-refill, uncached-fetch, GTE-busy,
  multiply/divide-interlock and residual cycle buckets. The
  `stack_ram_load_stall_cycles` column is a subset of RAM-load stalls and must
  not be added to `profiled_cpu_cycles` a second time.
- `--pc-line-log <csv>` counts every retired instruction by canonical 16-byte
  I-cache line. This is exact execution density rather than the periodic sample
  emitted by `--pc-sample-log`. Set occupancy alone is not evidence of a cache
  conflict: it cannot say which incoming line actually displaced which victim.
  Add `--pc-line-start-route-tick <N>` to exclude deterministic boot/loading
  work and rank only the gameplay tail of a route.
- `--icache-event-log <csv>` records every real refill with its direct-mapped
  set, incoming line/tag, previous victim line/tag/valid mask, miss kind, fill
  width and charged stall cycles. Add `--icache-event-start-route-tick <N>` to
  isolate a gameplay window. Rank exact temporal victim-to-incoming pairs with
  `python3 tools/pc_line_attribution.py <pc-lines.csv> <link.map>
  --icache-events <events.csv>`; always resolve against the matching executable
  map.
- `--instruction-class-log <csv>` writes exact per-route-tick dynamic counts
  for NOP/delay-slot kinds, memory widths and regions, stack-relative memory,
  LUI, jumps/branches, multiply/divide and GTE transfers/commands. Use this to
  bound a low-level proposal before changing generated MIPS.
- `--stack-profile-log <csv>` observes `$sp` without writing a canary into guest
  memory. Add `--stack-profile-root-pc 0xADDRESS` to measure each completed call
  of a render root from its entry stack pointer through its return; omit the
  root to measure the complete main-RAM run. Rooted profiles also accept the
  scratchpad's KUSEG/KSEG0 aliases; the uncached KSEG1 alias remains unmapped on
  the real CPU and is rejected.
- `--pc-sample-callsite-log <csv>`: out-of-band PC, `$ra`, `$sp+20`, and
  `$sp+36` samples. The two stack words recover callers through the standard
  compiler-builtins wrapper and its inner 16-byte frame, so time inside
  `memcpy`/`memset` can be attributed without changing guest timing.

Gotchas:
- `--input-tape` cannot combine with `--hold-forward` or `--hold-run`.
- With `--input-tape`, `--guest-frames` defaults to the tape length (one sample per vblank), so it can be omitted.
- The headless replay clock applies one tape sample per `step_one_frame` tick (vblank-period budget), mirroring the editor frame-for-frame. Keep it that way; it desyncs camera/collision otherwise (see the note in `cli.rs`).
- Without a tape, `--guest-frames N` stops on the guest's telemetry frame
  markers (`telemetry::frame_begin`, compiled out unless the guest has the
  `emulator-telemetry` feature). On a non-telemetry build (e.g. the burn
  feature set `cd-stream-bench fps-overlay`) the counter never advances and
  the run silently continues to the `--steps` cap; the dump is then whatever
  was last displayed at the cap, NOT frame N. Same for
  `--guest-visual-frames`. Pad pulses are unaffected (they ride the host
  vblank-period clock), so the game still progresses. For frame-addressed
  dumps, use a disc built with `emulator-telemetry`.

## 3. Render the per-vblank chart

```sh
cargo run --manifest-path tools/psoxide-dev/Cargo.toml --release -- \
  vblank-chart --in /tmp/cortex-ignition-v1-vblank.csv \
  --out /tmp/cortex-ignition-v1-vblank.html --title "cortex_ignition_v1 per-vblank work"
```

Self-contained HTML: stacked per-vblank stage bars, 1-vblank (564,480 cyc NTSC)
and 2-vblank/30 fps budget lines, render-vs-sim split, deadline-miss markers
(red tick at the baseline) and off-scale stall markers (red tick at the top),
hover tooltips, scroll=zoom / drag=pan / dbl-click=reset. Also prints a summary
table (render-vblank avg/p50/% over budget, sim-only avg, spread target) to stdout.

## In-editor alternative (coarser)

The **Dump profiler** (file icon) button writes the live profiler ring buffer to:

```
editor/projects/<project>/logs/play_profiler_history.csv
```

This is one row per profiler sample (each row aggregates several vblanks of
host+guest stats: `host_*`, `egui_*`, `psx_vblanks`, `guest_visual_*`,
`stage_N_*_cycles`), so it is good for a host+guest overview but too coarse for
the per-vblank alternation chart. Use the headless `--profile-log` path for
per-vblank work.

## Diagnosing missing geometry from a recorded tape

Guest telemetry only counts what the engine believes it did. A surface that is
legitimately culled is not an error, so a wrongly culled surface moves no
overflow, drop or cull counter and reads as a healthy frame. Chasing one of
those through the stage counters alone will falsify hypothesis after hypothesis.

Census the GPU instead. It is the only observer that sees what was actually
drawn:

```sh
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest \
  --input-tape "$HOME/Library/Application Support/com.psoxide.PSoXide/editor/playtest_tapes/<project>.pxtape" \
  --steps 6000000000 \
  --gpu-frame-stats-log /tmp/gpu.csv \
  --route-screenshot-dir /tmp/shots --route-screenshot-interval 40
```

`--gpu-frame-stats-log` writes a per-route-tick GP0 command census. Bucket it by
window and read `textured_quads` against `textured_tris`: room surfaces are
quads, models and the player are triangles, so a quad count that falls while the
triangle count holds steady means room geometry is being lost. Only ticks with a
non-zero `commands` count are rendered frames, so divide by those rather than by
tick count or a 30 Hz cadence will halve every average.

Bisect with a blunt instrument once the census confirms geometry is missing.
Forcing `CullMode::None` across the cached room path separates "not submitted"
from "submitted and rejected" in a single run, and is far faster than reasoning
about which predicate fired. That is how the cortex_v1 disappearing-wall report
was localised after five stage-counter hypotheses had been falsified: cardinal
wall windings made the owning cell's interior the back face, so backface culling
deleted every wall that bounds the playable area from the only side a player can
stand on.

Two cautions from that investigation:

- Host unit tests cannot reproduce GTE defects. Host projection does not
  saturate screen coordinates to signed 11 bits and does not overflow MAC0, so a
  screen-space defect passes on the host and fails on the guest.
- Some micro-profile counters (`room_surf_screen_culled`,
  `room_surf_backface_culled`) only populate under the `room-surface-profile`
  feature, and the warmed quad path does not feed them at all. A zero there
  means "not measured", not "did not happen".

## Related docs
- `docs/frontend.md`: FrameProfiler fields; `make profile-demo7-camera-sweep` benchmark.
- `docs/floors-plan.md`: example of `--input-tape` plus `--counter-log` headless tape diagnosis.
- `docs/demo10-low-level-hot-paths-2026-06-02.md`: demo10 render/update cost baseline.
