//! This example's instantiation of the crate-owned
//! [`RuntimeScheduleConfig`] policy knobs.

use psx_game_runtime::schedule::RuntimeScheduleConfig;

/// Central runtime scheduling policy.
///
/// Keep memory residency, render visibility, and background work pacing as
/// separate knobs. The stream pool may hold many rooms, but the active render
/// window should stay tied to the authored visible-room budget.
pub(crate) const RUNTIME_SCHEDULE: RuntimeScheduleConfig = RuntimeScheduleConfig {
    portal_max_depth: 8,
    portal_min_width_q12: 4,
    active_refresh_sectors: 4,
    active_job_builds_per_tick: 1,
    retained_inactive_rooms: 0,
    post_cross_render_debug_frames: 0,
    stream_load_batch_count: 4,
    stream_pump_sectors_per_tick: 8,
    stream_bootstrap_pump_limit: 4096,
    // No cap: fixed simulation always catches up to real VBlank time, so
    // slow visuals DROP FRAMES instead of dilating gameplay time. The old
    // cap of 2 rationed sim ticks under render overload -- at 20 fps the
    // whole game ran at ~2/3 speed (user-reported "everything slows
    // down"; the benchmark tape's 1,371 ticks consumed 2,157 vblanks).
    // The boot-backlog concern the cap addressed is handled upstream by
    // EngineClock::reset_origin after init.
    max_fixed_ticks_before_visual: 0,
};
