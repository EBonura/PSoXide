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
make cook-playtest PROJECT=editor/projects/cortex_ignition_v1/project.ron  # regenerates engine/examples/editor-playtest/generated/
make build-editor-playtest                                  # MIPS guest exe (picks up uncommitted engine changes)
cd tools/mkisopsx && cargo run --release -- \
  --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
  --volume CORTEX_IGNITION_V1 --cdtest-sectors 32 \
  --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
  --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
  --cdda-track-list ../../engine/examples/editor-playtest/generated/cdda_tracks.txt
```

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

## Related docs
- `docs/frontend.md`: FrameProfiler fields; `make profile-demo7-camera-sweep` benchmark.
- `docs/floors-plan.md`: example of `--input-tape` plus `--counter-log` headless tape diagnosis.
- `docs/demo10-low-level-hot-paths-2026-06-02.md`: demo10 render/update cost baseline.
