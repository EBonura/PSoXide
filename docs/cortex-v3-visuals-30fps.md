# cortex_v3 visual polish + 30 fps campaign

Status 2026-08-01: diagnosis complete from Manny's recorded tape
(`cortex_v3.pxtape`, 3178 editor samples), fixes not yet started.

## Repro harness (all headless, no editor needed)

```sh
make cook-playtest PROJECT=projects/cortex_v3/project.ron
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry"
# then mkisopsx with the usual world+UI pack flags (see CLAUDE.md / playtest-profiling.md)
cd emu && cargo run -p frontend --release -- launch \
  --path ../build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest --input-tape "<tapes>/cortex_v3.pxtape" \
  --steps 4000000000 --profile-log v.csv --counter-log c.csv --guest-debug-log
```

`--guest-frames N` works on this build (telemetry), so exact-frame dumps do too.
`PORTAL_VIS_DEBUG_LOGS = true` in `debug_runtime.rs` prints full visibility
snapshots (reject reasons, per-portal clip windows) plus the stream plan.

## The map

cortex_v3's world is ONE authored room (18) split into 6 runtime chunks
(room index == chunk index, grid in `ROOM_CHUNKS`), portals are synthetic
quads on the open floor seams. `visible_chunk_limit = 10`,
`MAX_ACTIVE_ROOMS = 16`: budget is NOT the constraint. All 6 chunks are
CD-loaded by guest frame 44 and never touched again (stream log is silent
for the rest of the run); `resident_mask = 0x3f` throughout.

## Bug evidence (guest-frame numbers from the tape replay)

Counter CSV mask semantics: `current_room` = the CAMERA's room (visibility
root), `visible` = portal traversal result, `active`/`drawn` = the active
window (chunks with built surface caches).

1. **Edge cull (Manny's two moments)** f2658-2694 and f3066-3102: player at
   room 2's east edge, `visible={2,5}` (correct, verified in the portal
   snapshots: portal 2->5 accepted, others legitimately clipped), but
   `active` collapses from {1,2,5} to **{5}** for ~40 frames. The room under
   the player is resident + visible + NOT drawn. Recovers exactly when the
   player moves again.
2. **Far-room cull** f378-1608 (camera room 4): `visible={1,2,3,4,5}` but
   `active` stuck at {2,3} or {2,3,4} for HUNDREDS of frames, with room 4
   flapping in/out of `drawn` (~every 40-90 frames). Far chunks 1 and 5
   never activate despite being visible, resident, and inside every budget.

## Diagnosis

Portal visibility is CORRECT in these moments; streaming is idle; the chunk
budget is 10 with only 6 chunks. Both bugs live in the active-window policy
(`active_rooms.rs` + `room_window.rs`):

- When a window rebuild fails to build a visible room, `mark_visible_room_unbuilt`
  PRUNES it from the visibility result. `visible_rooms_are_active()` checks
  only the (now pruned) result, so the system believes it is converged.
- `refresh_active_room_window_if_needed` early-returns unless the camera
  moved ~a sector or the view keys changed. Standing still at an edge means
  no retry for as long as you stand there: that is the 40-frame {5}-only
  window, and the stuck {2,3} states.
- Why the builds fail at the rebuild instant is the one OPEN question.
  Streaming is quiet, so the suspects are the stream-slot coupling in
  `reuse_active_room` (stream_slot mismatch defeats reuse) and
  `parse_streamed_compact_collision_room(slot, index)` failing when window
  slot and stream slot disagree. Needs one instrumented run that logs
  each build failure with its reason (add a debug_log in
  `build_active_room` / `reuse_or_build_active_room`).

## Reconciler landed (2026-08-01)

`RoomWindow::reconcile` replaces the begin/step/finish job path and the
movement-gated retriggers: every tick (gated by `active_window_dirty`,
set on visibility refresh / crossing / stream progress, cleared on
convergence) the window diffs actual against a freshly assembled desired
set (current room first, then ring, then visible additions). Failures
retry instead of pruning the goal; entries are freed only when undesired
or their stream slot moved (build-then-swap). Cost: 2.9k cyc/vblank avg
(36k ungated), total frame cycles unchanged, tape final frame
byte-identical. `RECONCILE_DEBUG_LOGS` logs stale/fail/skip anomalies.

## What the tape data actually shows (post-reconciler)

- The long "stuck" mask windows in the counter CSV are SIM-PAUSE
  artifacts: camera + all masks freeze for 36-260 frames (menus or
  triggers in the tape); frame dumps inside them show an intact world.
  The counter log repeats the last emitted values while the world render
  is paused. Do not diagnose those as culls.
- The REAL far-room cull is `portal_within_far`
  (`psx-level/portal_visibility.rs:948`): a room only draws if some
  accepted entry portal has nearest view-space z <= far_z (16384).
  cortex_v3's cave has ~32k sightlines, so far chunks pop binarily at
  the far plane (and hover-flap when a portal sits near the threshold).
  The render gate is `draws_room` = root || within_far; the visible-mask
  counter ignores within_far, which is why counters said "visible" while
  the render skipped.
- Candidate fixes, pending Manny's call: raise far_z / per-room
  draw_distance for this content (costs room-draw perf, needs item C),
  add hysteresis to within_far to kill the flap, and/or per-cell
  distance clamp instead of whole-room gating.
- The two user-visible edge moments still need pinpointing in the tape
  (ask Manny what he did and roughly when); suspect the portal
  front-face test at seams (camera crossing the portal plane) rather
  than the window layer, which now provably converges.

## Fix plan

A. **Self-healing retrigger**: keep unbuilt-but-visible rooms in (or beside)
   the visibility result, and retry the window job every N ticks while
   `visible_rooms_are_active()` is false, independent of camera movement.
   This alone turns both bugs from "stuck until you move" into "one-frame
   hiccup".
B. **Root-cause the build failure** with the instrumented run, fix the
   slot/reuse coupling so a rebuild never drops an already-built chunk.
C. **Performance** (separate lever, same campaign): tape replay shows render
   vblanks at avg 294% / p95 397% of ONE vblank = ~147% of the whole 30 fps
   2-vblank slot; 812/862 render vblanks blow the slot; 548 deadline misses.
   Stage split on render vblanks: room 687k (room_surface_draw 618k!),
   player 336k, present 272k, update 133k, image_props 98k. cortex_v1's room
   stage was 64k; v3's room pass is 10x heavier and is THE target. Note the
   interaction: fixing A/B correctly draws MORE chunks, so the room-draw
   cost must come down at the same time. No visual-quality compromises
   (Manny's constraint): the work is cell-level culling/caching in the room
   surface path, not drawing less.

## Perf artifacts

Session scratchpad holds `cortex_v3-vblank.csv`, `cortex_v3-counters.csv`,
`cortex_v3-portal-debug.log`, and frame dumps. `tools/vblank_chart.py` no
longer exists; `tools/cortex_30fps_report.py` is the current summariser.
