# Worker 3 Prompt: Exact-Output Demo3 Room Rendering Performance

You are working in `/Users/ebonura/Desktop/repos/PSoXide` on branch `feature/ram-scratch-reuse-baseline`.

## Goal

Improve demo3 room rendering performance without sacrificing any visual quality. The output image must remain byte-for-byte visually identical according to the display hash unless you can prove a hash change is caused by an unrelated deterministic baseline shift.

Your ownership is the room rendering path only. Prefer changes in:

- `engine/crates/psx-engine/src/world_render.rs`
- `engine/crates/psx-engine/src/render3d.rs` only if a room-render packet/submission helper needs an exact-output optimization
- `engine/examples/editor-playtest/src/main.rs` only for telemetry around room rendering or call-site cleanup

Do not work on camera collision, movement update, editor preview, model texture mapping, or visibility selection unless you need read-only context.

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

- render total/frame: `3,605,667`
- update/frame: `89,343`
- camera/frame: `151,202`
- room/frame: `2,686,970`
- player/frame: `582,522`
- world flush/sort/frame: `109,003`
- ot submit/frame: `15,929`
- tri prims/frame: `1,297`
- room cells drawn/frame: `235`
- room surfaces/frame: `573`
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

- render total/frame: `2,320,259`
- update/frame: `1,526,485`
- camera/frame: `213,909`
- room/frame: `1,361,078`
- player/frame: `606,082`
- world flush/sort/frame: `78,043`
- ot submit/frame: `10,061`
- tri prims/frame: `763`
- room cells drawn/frame: `127`
- room surfaces/frame: `269`
- room chunks/frame: `4`

## What To Optimize

Room rendering is the largest render-stage cost:

- default: `2,686,970 / 3,605,667` cycles = about 74.5% of render cost
- forward: `1,361,078 / 2,320,259` cycles = about 58.7% of render cost

The current system already uses generated room surface caches, so do not rebuild room surface caches at runtime. Focus on exact-output savings in the cached draw path:

- reduce repeated work per cached surface
- reduce repeated material/sidedness/UV/light/sample reconstruction where possible
- avoid repeated bounds/range checks in inner loops when already proven by cache metadata
- look for duplicated projection/depth/light work across cells and surfaces
- preserve draw ordering, primitive content, material flags, lighting, fog, culling, and packet order

Likely files/functions to inspect:

- `draw_indexed_cached_room_vertex_lit_visible_cells`
- `draw_cached_room_surface_vertex_lit`
- `indexed_vertex_lighting_colors`
- cached room surface record conversion helpers
- world command submission and sorting only if output order remains identical

## Hard Constraints

- Do not reduce geometry, cull more cells, change active chunks, or alter visibility.
- Do not change hashes.
- Do not increase `MAX_TEXTURED_TRIS`.
- Do not add heap allocation or dynamic allocation in runtime paths.
- Keep PS1 constraints: fixed-size scratch, bounded loops, no expensive divisions in hot loops if avoidable.
- Keep architecture simple. Prefer smaller hot-path helpers over broad new cache layers.
- Do not revert unrelated worktree changes.

## Required Verification

Run these before reporting back:

```sh
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml world_render
cargo test --manifest-path engine/crates/psx-engine/Cargo.toml render3d
cargo check --manifest-path engine/examples/editor-playtest/Cargo.toml
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-worker3-room.ppm
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-worker3-room-forward.ppm
git diff --check
graphify update .
```

Acceptance:

- demo3 default display hash must remain `0x807c5debd2e9bf8a`
- demo3 default VRAM hash must remain `0xa5c5a996b781b8b0`
- demo3 forward display hash must remain `0xc10fd4b6892df758`
- demo3 forward VRAM hash must remain `0x736412dbc4da1148`
- report before/after per-frame cycles for `render total`, `room`, `world flush/sort`, `tri prims`, `room surfaces`, and `room cells drawn`

## Final Report Format

Return:

- changed files
- implementation summary
- tests run
- baseline vs result table for default and forward
- exact hashes from both profile commands
- any residual risks
