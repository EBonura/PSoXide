# Worker 5 Prompt: 60Hz Control With Paced 20Hz Visuals

You are working in `/Users/ebonura/Desktop/repos/PSoXide` on branch `feature/ram-scratch-reuse-baseline`.

## Goal

Add a real engine-level visual pacing mode so demo3 can run gameplay/control at a fixed 60Hz simulation cadence while rendering visuals at a stable 20Hz cadence on NTSC.

The target architecture is:

```text
Every display VBlank:
  poll input
  advance simulation/control by one fixed 60Hz tick

Every third VBlank:
  clear
  render latest state
  present/swap

Between rendered frames:
  keep the previous framebuffer visible
```

This is not just a cosmetic FPS cap. The engine must avoid clearing/swapping skipped visual frames, otherwise the displayed image will blink or blank. The previous rendered frame should remain on screen until the next visual frame is due.

## Ownership

Prefer changes in:

- `engine/crates/psx-engine/src/app.rs`
- `engine/crates/psx-engine/src/scene.rs`
- `engine/crates/psx-engine/src/time.rs`
- `engine/examples/editor-playtest/src/main.rs`

Only touch other files if the engine API requires small plumbing.

Do not work on:

- room visibility
- room rendering hot paths
- model rendering
- material/texture mapping
- editor preview
- emulator profile formatting, except if you need a tiny compile fix caused by engine telemetry constants

Worker 6 owns measurement/profiler/reporting. Coordinate by keeping your runtime API simple and easy to instrument.

## Current Baseline

Current committed baseline after `e742a5e Improve demo3 camera collision and hot paths`.

### Demo3 Default

Command:

```sh
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-current-baseline.ppm
```

Hashes:

- display hash: `0x807c5debd2e9bf8a`
- VRAM hash: `0xa5c5a996b781b8b0`

Profile:

- update/frame: `89,344`
- render total/frame: `3,583,147`
- camera/frame: `151,192`
- room/frame: `2,664,464`
- player/frame: `582,506`
- world flush/sort/frame: `109,003`
- ot submit/frame: `15,929`
- tri prims/frame: `1,297`

### Demo3 Hold Forward

Command:

```sh
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-forward-current-baseline.ppm
```

Hashes:

- display hash: `0xc10fd4b6892df758`
- VRAM hash: `0x736412dbc4da1148`

Profile:

- update/frame: `1,061,904`
- render total/frame: `2,307,399`
- camera/frame: `213,894`
- room/frame: `1,348,226`
- player/frame: `606,065`
- world flush/sort/frame: `78,044`
- ot submit/frame: `10,061`
- tri prims/frame: `763`

## Frame Budget Context

Approximate NTSC PS1 CPU budget:

- 60Hz: about `564k` cycles per VBlank
- 30fps: about `1.13M` cycles per visual frame
- 20fps: about `1.69M` cycles per visual frame
- 15fps: about `2.26M` cycles per visual frame

The current renderer is still over the 20fps budget in heavy demo3 views. Your job is to build the correct cadence architecture, not to fake a stable 20fps result. If a frame misses the 3-VBlank window, the engine should recover deterministically and expose enough state for Worker 6 to report missed visual deadlines.

## Required Design

Add a pacing mode to the engine that defaults to the current behavior.

Suggested API shape:

```rust
pub struct Config {
    // existing fields...
    pub visual_pacing: VisualPacing,
}

pub enum VisualPacing {
    EveryVBlank,
    EveryNVBlanks(u16),
}
```

or an equivalent compact API.

Requirements:

- `Config::default()` must preserve existing one-update/one-render-per-vblank behavior.
- Demo3 should opt into `EveryNVBlanks(3)` for NTSC.
- For PAL, do not silently pretend 20fps divides cleanly into 50Hz. Either leave demo3 at default pacing for PAL or choose a clear deterministic fallback such as `EveryNVBlanks(2)` for 25fps. Document the choice in code comments.
- Skipped visual frames must not call `ctx.fb.clear`, `Scene::render`, `gpu::draw_sync`, or `ctx.fb.swap`.
- The last completed framebuffer must remain displayed during skipped visual frames.
- Input and gameplay update should still run once per display tick whenever the CPU reaches that tick.
- If render work overruns and multiple VBlanks elapsed, catch simulation up with fixed one-VBlank update ticks before rendering again. Do not pass a giant `delta_vblanks` into demo3 update when the chosen mode is fixed 60Hz control.
- If multiple visual intervals were missed, render once from the latest state; do not try to render multiple catch-up visual frames.

## Engine Time Semantics

Be careful with existing semantics:

- `Ctx::frame` currently means visible/rendered app frame in comments and common examples.
- `EngineTime::rendered_frame()` currently mirrors `ctx.frame`.
- `EngineTime::elapsed_vblanks()` is display time.
- `EngineTime::delta_vblanks()` is currently elapsed VBlanks since the previous rendered app frame.

For the paced mode, introduce clearer semantics without breaking simple examples.

Acceptable approaches:

- Add a simulation tick counter to `Ctx` while keeping `ctx.frame` as rendered frame.
- Add `EngineTime` constructors/helpers for fixed one-VBlank simulation ticks.
- Add `EngineTime::simulation_tick()` or equivalent if useful.
- Keep `delta_vblanks()` equal to `1` for each fixed simulation update in the paced path.

Avoid a broad trait redesign unless necessary. Adding a default method to `Scene` is fine if existing examples keep compiling unchanged.

## Demo3 Integration

In `engine/examples/editor-playtest/src/main.rs`:

- Configure demo3/editor-playtest to use the new 20Hz visual pacing on NTSC.
- Preserve gameplay behavior as much as possible.
- `Playtest::update` should observe `ctx.time.delta_vblanks() == 1` in the normal paced path.
- Existing motor/camera catch-up paths should remain valid for missed ticks, but normal paced operation should not lump three VBlanks into one update.
- Animation should continue using `ctx.time.elapsed_vblanks()` so visual samples still use display-time animation.

## Testing

Add focused tests where practical.

Minimum expected tests:

- `EngineTime` test showing fixed one-VBlank update snapshots are possible.
- App/pacing logic test if the app loop can be factored into a host-testable helper.
- Existing engine tests must still pass.

Run:

```sh
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml time
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml
cargo check --manifest-path engine/examples/editor-playtest/Cargo.toml
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-worker5-paced20.ppm
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-worker5-paced20-forward.ppm
git diff --check
graphify update .
```

## Acceptance

Return your result even if demo3 is still above the 20fps budget. The acceptance criteria for this worker are architectural:

- Default engine pacing remains unchanged for other examples.
- Demo3 has a real 20Hz visual pacing mode on NTSC.
- Update/control simulation advances in fixed one-VBlank steps in the paced path.
- Skipped visual frames preserve the previous displayed framebuffer.
- The code has no heap allocation in runtime cadence paths.
- The implementation is small and understandable.

Report:

- changed files
- API summary
- exact behavior for NTSC and PAL
- tests run
- demo3 default and forward hashes
- demo3 profile output, especially `update`, `render total`, `present/wait`, and `guest_profile_frames`
- any missed visual-frame behavior you observed

