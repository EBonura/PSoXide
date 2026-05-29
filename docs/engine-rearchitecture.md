# Engine Re-architecture Notes

Findings + plan for the runtime engine, focused on three goals: spreading work
across frames, 60Hz controls / 30fps visuals, and fixing the portal-visibility
debug-map false-positives + flicker. Grounded in the current code; every claim
carries a `file:line`.

## What is already done (don't rebuild it)

- **60Hz sim / 30fps render is live.** `SimTick` advances once per NTSC VBlank
  (fixed timestep off the hardware vblank counter, `scheduler.rs` +
  `frames.rs:40`); the pad is polled inside the fixed update at 60Hz
  (`app.rs:243-247`); rendering is gated to `VisualPacing::EveryNVBlanks(2)` =
  30fps (`engine/examples/editor-playtest/src/main.rs:13358`). Catch-up drops
  frames without slowing the gameplay clock.
- **Workload is already time-sliced.** Streaming (`stream_pump_sectors_per_tick
  = 8`), VRAM upload (8 rows/tick), and the active-room-window rebuild
  (`active_job_builds_per_tick = 1`) are budgeted per tick; background work runs
  on odd ticks only (`main.rs:1781`). PVS is baked offline
  (`fill_precomputed_visible_cells`, `main.rs:8127`).
- **The engine is allocation-free and float-free on device**, GTE-batched
  (RTPT), vertex-projection deduped per frame, portal-culled. It is already
  console-grade; the remaining wins are removing *repeated* work, not
  restructuring.

The one missing scheduling knob: the **catastrophic-backlog clamp is disabled**
(`max_fixed_ticks_before_visual = 0`, `main.rs:13374`). For a hard 30fps target
set it to ~3-4 (`scheduler.rs:282` consumes it) so one slow frame can't spiral
the sim clock.

## The portal-visibility bug — root cause and the design fork

### What green means

Debug-map GREEN = `chunk_drawn_mask` = "drew geometry this frame"
(`editor/crates/psxed-ui/src/lib.rs:25401,25413`), sourced from
`ROOM_DRAWN_CHUNK_MASK`. It is a strict subset of the portal-visible set: a room
only draws if it passed `portal_visibility_draws_room`
(`main.rs:3726,3997,8852`). So green false-positives are genuine — a room the
player can't see is being drawn.

### The key realisation: visibility is *intentionally* loose

The acceptance path uses a loose axis-aligned bounding box of the projected
portal corners (`projected_portal_surface_bounds`,
`engine/crates/psx-level/src/portal_visibility.rs:1030-1067`) with edge padding,
and the **accurate clipped polygon is computed and then discarded**
(`clipped_bounds` vs `result_bounds = fallback_bounds`, `:826-832`). There is
**no far-plane cull** — and `accepts_projected_portal_beyond_render_far_plane`
(`:1578`) *deliberately asserts* a portal at z=8192 is accepted with far=2048.

That test name is the tell: the looseness is on purpose. Portal visibility
**feeds streaming/residency**, and you want rooms slightly beyond the draw
distance (and slightly off-axis) streamed in early so they don't pop when the
player turns or approaches. Tightening visibility directly would trade the
flicker bug for a pop-in bug.

### The fix: decouple streaming-visibility from draw-visibility

Today one set does two jobs. Split them:

- **Streaming visibility** (loose, padded, no far cull, slightly off-axis) —
  unchanged. Drives residency/prefetch. Keeps pop-in away.
- **Draw visibility** (tight) — a room turns green / submits geometry only if it
  passes the *accurate* polygon clip (the `clipped_bounds` already computed at
  `:826`) **and** is within `far_z`. A room beyond the draw distance is
  GPU-far-clipped anyway, so excluding it from the draw set cannot pop; an
  off-axis room whose true projection misses the frustum genuinely isn't
  visible, so excluding it cannot pop either.

Concretely: add a `draws` predicate alongside the existing `visible` decision in
`portal_visibility.rs` (use the accurate clip + far test), and have
`portal_visibility_draws_room` (`main.rs`) gate green/draw on `draws`, while
residency keeps gating on `visible`. This removes the green false-positives
without touching streaming. The permissive `accepts_*` tests stay valid (they
describe *streaming* visibility); new tests assert the tighter *draw* predicate.

### The flicker fix (independent, safe, additive)

Flicker is the recompute trigger, not the acceptance rule: visibility is only
recomputed when a `/64`-quantized view-direction bucket flips
(`portal_visibility_view_keys`, `main.rs:9252`; `view_changed` strict `!=`,
`:6549`) and a large position jump rebuilds from *stale* visibility (early
return `:6553-6555`). Two additive fixes, both erring toward *keeping* rooms
(no pop, no draw removal):

1. Recompute portal visibility on `moved_far` too (don't reuse stale), or every
   frame — the BFS is heap-free and depth-bounded, so per-frame is affordable
   and removes the staleness class.
2. Add hysteresis: hold a room in the draw set for N frames after it was last
   accepted, or use a wider quantization bucket for *drop* than for *add*. Kills
   the knife-edge accept/reject oscillation at bucket and Q12 boundaries
   (`portal_clip_is_tiny` uses `&&` and per-edge `+16/4096` pad, `:1062,1273`).

## Spreading workload across frames

The big lever is **not** caching projection across frames — a tracking camera is
never still, so a "camera unchanged" cache hits ~0% in gameplay (overhead, no
hit). The way to cut projection cost with a moving camera is to **project fewer
rooms** — which the draw-visibility tightening above delivers every frame.

Beyond that, the un-amortised costs and their fixes:

- **Portal BFS is a synchronous spike** when it runs (`rebuild_portal_visibility`,
  `main.rs:5147`, depth 8). If moved to per-frame for freshness, time-slice it
  (a `portal_builds_per_tick` budget mirroring `active_job_builds_per_tick`) or
  rely on its bounded cost. Formalise the spare odd sim-tick as a dedicated
  build phase using the inert `TaskLane`/`TaskCadence::EveryNTicks` vocabulary in
  `scheduler.rs:37-65` (currently defined but unused).
- **Double-buffer the OT** so frame N's GPU DMA drains while the CPU builds frame
  N+1; today `draw_sync` blocks every render frame (`app.rs:294-298`). Reclaims
  the GPU-drain stall at 30fps. Needs a 2-deep OT/framebuffer and moving
  `draw_sync` ahead of the next `OtFrame::begin`.

## Intra-frame waste (small, behaviour-identical, survives a moving camera)

Honest ROI: each is low single-digit % of a 30fps budget, but free (same
pixels), provable by the engine unit tests:

- `camera_gte_view_matrix` recomputed ~34×/frame (`render3d.rs:2754,4288,4411,
  6176,6227`) — the camera is constant across a room's actors; compute once and
  thread it in. ~5-8k cycles/frame.
- Per-instance model setup runs twice (BehindPlayer/InFrontOfPlayer passes each
  recompute every instance's transform/bounds before the depth gate rejects it,
  `main.rs:10805`). Compute per-instance state once into scratch, both passes
  index it. ~5-15k cycles/frame, scales with actor count.
- Per-triangle GTE NCLIP backface test (`world_render.rs:3156`) → CPU
  screen-area sign on already-projected XY. NOTE: this *changes* the culling
  computation (integer area vs NCLIP can disagree at edges), so it is **not**
  behaviour-identical and needs visual validation like the draw-gate change.

## Validation constraint

Anything that changes which rooms draw (the draw-visibility split, the NCLIP
swap) changes rendered output and must be confirmed on a running playtest by
watching the chunk debug map — the automated suite (engine unit tests,
`commercial-visual-guards`) does not cover the 3D world portal render. The
behaviour-identical items (view-matrix hoist, per-instance dedup, double-buffer
OT, backlog clamp) are provable without that.

## Suggested order

1. **Flicker fix** (recompute-on-`moved_far` + hysteresis) — additive, no draw
   removal, fixes half the reported bug. Validate on playtest.
2. **Draw/streaming visibility split** — the green false-positive fix, lower pop
   risk than tightening visibility. Validate on playtest.
3. **Backlog clamp** (one constant) + **off-tick build phase** via `TaskLane`.
4. **Double-buffer OT**; then the intra-frame dedup wins if the budget warrants.
