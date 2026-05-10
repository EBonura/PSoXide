# Worker 4 Prompt: Exact-Output Demo3 Camera/Update Performance

You are working in `/Users/ebonura/Desktop/repos/PSoXide` on branch `feature/ram-scratch-reuse-baseline`.

## Goal

Improve demo3 camera/update performance without sacrificing any visual quality. The output image must remain byte-for-byte visually identical according to the display hash unless you can prove a hash change is caused by an unrelated deterministic baseline shift.

Your ownership is the camera and movement/update collision path. Prefer changes in:

- `engine/crates/psx-engine/src/third_person_camera.rs`
- `engine/crates/psx-engine/src/character_motor.rs` only if update profiling proves motor collision is the bottleneck and output remains identical
- `engine/examples/editor-playtest/src/main.rs` only for call-site plumbing or telemetry

Do not work on room visibility, room rendering packet emission, model texture mapping, or editor preview unless you need read-only context.

## Current Baseline

These numbers are from the current working tree after the generated room cache, visibility, diagonal-wall sidedness override, and restored chunked camera collision changes.

### Demo3 Default

Command:

```sh
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-current-baseline.ppm
```

Hashes:

- display hash: `0x807c5debd2e9bf8a`
- VRAM hash: `0xa5c5a996b781b8b0`
- screenshot: `/tmp/psoxide-demo3-current-baseline.ppm`

Profile:

- update/frame: `89,343`
- render total/frame: `3,605,667`
- camera/frame: `151,202`
- room/frame: `2,686,970`
- player/frame: `582,522`
- world flush/sort/frame: `109,003`
- tri prims/frame: `1,297`
- room chunks/frame: `4`

### Demo3 Hold Forward

Command:

```sh
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-forward-current-baseline.ppm
```

Hashes:

- display hash: `0xc10fd4b6892df758`
- VRAM hash: `0x736412dbc4da1148`
- screenshot: `/tmp/psoxide-demo3-forward-current-baseline.ppm`
- note: profile stops at `guest_profile_frames=79`; this is current expected behavior.

Profile:

- update/frame: `1,526,485`
- render total/frame: `2,320,259`
- camera/frame: `213,909`
- room/frame: `1,361,078`
- player/frame: `606,082`
- world flush/sort/frame: `78,043`
- tri prims/frame: `763`
- room chunks/frame: `4`

## What To Optimize

The restored chunked camera collision fixed correctness but introduced visible render-stage camera cost:

- default camera/frame: `151,202`
- forward camera/frame: `213,909`

The hold-forward profile also has a very expensive update stage:

- forward update/frame: `1,526,485`

Focus on exact-output optimizations:

- Precompute per-active-room camera collision ray data once per camera substep instead of rebuilding it for every sample.
- Combine the multi-room "is sample inside any floor" pass and "nearest wall hit around sample" pass without changing the old semantics: if the sample is outside all active rooms, it must stop at `last_clear_distance` and ignore wall hits at that sample.
- Avoid duplicate room/local coordinate transforms in the camera ray loop.
- Keep the fixed bounds: at most `MAX_ACTIVE_ROOMS`, no heap allocation.
- If update-stage cost is from character collision, inspect repeated multi-room scans and active-room collection, but preserve movement result, room transitions, animation state, camera position, and hashes.

Likely files/functions to inspect:

- `ThirdPersonCameraState::update_vblanks_with_collision_rooms`
- `probe_clear_distance_rooms`
- `point_outside_camera_rooms`
- `nearest_wall_hit_around_rooms`
- `collect_collision_rooms` in `editor-playtest/src/main.rs`
- `CharacterMotorState::update_vblanks_with_collision` if profiling proves update cost is there

## Hard Constraints

- Do not disable camera collision.
- Do not change camera behavior, smoothing, wall-following, or player movement.
- Do not reduce geometry, visibility, active chunks, or draw order.
- Do not change hashes.
- No heap allocation or dynamic allocation in runtime paths.
- Keep bounded PS1-friendly loops.
- Do not revert unrelated worktree changes.

## Required Verification

Run these before reporting back:

```sh
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml third_person_camera
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml character_motor
cargo check --manifest-path engine/examples/editor-playtest/Cargo.toml
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-worker4-camera.ppm
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-worker4-camera-forward.ppm
git diff --check
graphify update .
```

Acceptance:

- demo3 default display hash must remain `0x807c5debd2e9bf8a`
- demo3 default VRAM hash must remain `0xa5c5a996b781b8b0`
- demo3 forward display hash must remain `0xc10fd4b6892df758`
- demo3 forward VRAM hash must remain `0x736412dbc4da1148`
- report before/after per-frame cycles for `update`, `camera`, `render total`, `player`, `room`, and `tri prims`

## Final Report Format

Return:

- changed files
- implementation summary
- tests run
- baseline vs result table for default and forward
- exact hashes from both profile commands
- any residual risks
