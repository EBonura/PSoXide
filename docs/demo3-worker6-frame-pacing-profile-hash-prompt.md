# Worker 6 Prompt: Frame Pacing Telemetry, Hashes, And 20Hz Acceptance

You are working in `/Users/ebonura/Desktop/repos/PSoXide` on branch `feature/ram-scratch-reuse-baseline`.

## Goal

Add the measurement and validation layer needed for the new 60Hz-control / 20Hz-visual architecture. Worker 5 owns the runtime cadence. Your job is to make it measurable, hard to misread, and safe to benchmark.

We need to know:

- how many simulation ticks ran
- how many visual frames were actually rendered
- whether visual frames landed on a steady every-3-VBlank cadence
- how many visual deadlines were missed
- how much CPU budget is spent per simulation tick and per rendered visual frame
- whether hashes changed only because the sampled frame number changed, not because the image regressed

## Ownership

Prefer changes in:

- `engine/crates/psx-engine/src/telemetry.rs`
- `emu/crates/emulator-core/src/telemetry.rs`
- `emu/crates/frontend/src/cli.rs`
- `emu/crates/frontend/src/ui/profiler.rs`
- `Makefile`
- docs or benchmark notes under `docs/`

Touch `engine/crates/psx-engine/src/app.rs` only if Worker 5 has exposed pacing events/counters there and you need to wire telemetry calls. Do not make cadence policy decisions in this worker.

Do not work on:

- room visibility
- room rendering
- model rendering
- motor/camera behavior
- texture/material fixes

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

- guest_profile_frames: `60`
- update/frame: `89,344`
- render total/frame: `3,583,147`
- camera/frame: `151,192`
- room/frame: `2,664,464`
- player/frame: `582,506`
- world flush/sort/frame: `109,003`
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

- guest_profile_frames: `79`
- update/frame: `1,061,904`
- render total/frame: `2,307,399`
- camera/frame: `213,894`
- room/frame: `1,348,226`
- player/frame: `606,065`
- world flush/sort/frame: `78,044`
- tri prims/frame: `763`

## Required Telemetry

Add compact guest telemetry counters/stages that let the CLI and profiler report frame pacing clearly.

Suggested additions:

### Counters

- `SIM_TICKS`: number of fixed simulation/control updates run.
- `VISUAL_FRAMES`: number of rendered visual frames.
- `VISUAL_SKIPPED_VBLANKS`: skipped visual slots or held-display ticks.
- `VISUAL_DEADLINE_MISSES`: number of times a visual frame was late for its target cadence.
- `VISUAL_INTERVAL_VBLANKS`: configured visual interval, e.g. `3` for NTSC 20fps.
- `VISUAL_MAX_LATENESS_VBLANKS`: worst observed lateness.

Use names that fit existing telemetry conventions if you choose different ids.

### Stages

Only add new stages if needed. Existing stages may be enough:

- `update`
- `render total`
- `present/wait`

If Worker 5 creates a specific fixed-update loop, consider adding a `sim tick` stage only if it makes the profile clearer and does not add meaningful runtime overhead.

Keep engine and emulator telemetry ids in sync:

- `engine/crates/psx-engine/src/telemetry.rs`
- `emu/crates/emulator-core/src/telemetry.rs`
- `stage_name` / `counter_name`
- `STAGE_COUNT` / `COUNTER_COUNT`

## CLI / Makefile Work

The existing commands profile by guest frame count:

```sh
make profile-demo3
make profile-demo3-forward
```

Under paced 20Hz visuals, `guest_profile_frames` may mean telemetry frame markers rather than rendered visual frames depending on Worker 5's implementation. Make the output unambiguous.

Add or adjust commands so we can benchmark:

- demo3 current/default pacing, if useful for comparison
- demo3 paced 20Hz default view
- demo3 paced 20Hz hold-forward

Suggested names:

```sh
make profile-demo3-paced20 PROFILE_DEMO3_PACED20_HW=/tmp/psoxide-demo3-paced20.ppm
make profile-demo3-paced20-forward PROFILE_DEMO3_PACED20_FORWARD_HW=/tmp/psoxide-demo3-paced20-forward.ppm
```

If Worker 5 configures demo3 itself to paced 20Hz, these targets can simply make the pacing explicit in filenames and frame counts.

Make sure the printed profile lets us answer:

- rendered visual frames captured
- simulation ticks captured
- configured visual interval
- missed visual deadlines
- cycles per simulation tick
- cycles per rendered visual frame
- cycles per 3-VBlank budget

## Hash Policy

With a different render cadence, hashes may change because the screenshot is taken at a different visual sample. That is not automatically a regression.

Create a stable hash policy:

- Capture a baseline hash for paced20 default.
- Capture a baseline hash for paced20 hold-forward.
- Compare future paced20 work against those paced20 hashes, not against the old every-frame hashes.
- Keep the old hashes in the report as pre-pacing reference only.

If you add docs, include:

- command used
- display hash
- VRAM hash
- screenshot path
- guest profile summary
- whether the profile met or missed the 20fps budget

## Budget Reporting

Use NTSC budget numbers:

- one VBlank budget: about `564k` CPU cycles
- paced 20Hz visual budget: about `1.69M` CPU cycles across 3 VBlanks

The report should explicitly say whether the measured work fits under the 3-VBlank budget.

Do not hide misses. If demo3 renders late, the correct result is a clear report like:

```text
configured interval: 3 vblanks
visual frames: 60
sim ticks: 180
deadline misses: 42
max lateness: 2 vblanks
rendered-frame cost: 2.31M cycles
3-vblank budget: 1.69M cycles
status: over budget
```

## Required Verification

Run:

```sh
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml telemetry
cargo test --manifest-path emu/Cargo.toml -p emulator-core telemetry
cargo check --manifest-path engine/examples/editor-playtest/Cargo.toml
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-worker6-reference.ppm
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-worker6-reference-forward.ppm
make profile-demo3-paced20 PROFILE_DEMO3_PACED20_HW=/tmp/psoxide-demo3-worker6-paced20.ppm
make profile-demo3-paced20-forward PROFILE_DEMO3_PACED20_FORWARD_HW=/tmp/psoxide-demo3-worker6-paced20-forward.ppm
git diff --check
graphify update .
```

If Worker 5 has not landed yet, implement only the telemetry ids/names/docs/Makefile pieces that compile against the current tree, and state what remains blocked on Worker 5.

## Acceptance

Return:

- changed files
- telemetry ids/names added
- Makefile commands added or changed
- old reference hashes
- new paced20 hashes
- budget table for default and forward
- explicit pass/fail against the 3-VBlank visual budget
- any ambiguity left in the profile output

