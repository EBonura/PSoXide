//! This example's instantiation of the crate-owned
//! [`RuntimeScheduleConfig`] policy knobs.

use psx_game_runtime::cd_stream::drive_sectors_per_background_tick;
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
    // Drain ceiling per background pump, not a request. Must stay at or above
    // what the drive delivers in a pump period (see
    // `drive_sectors_per_background_tick`, currently 5 at double speed) or
    // sectors that already landed would be left for the next tick and the
    // streamer would fall behind the drive. The margin above that bounds how
    // much of one tick a burst may consume.
    stream_pump_sectors_per_tick: 8,
    // No cap: fixed simulation always catches up to real VBlank time, so
    // slow visuals DROP FRAMES instead of dilating gameplay time. The old
    // cap of 2 rationed sim ticks under render overload -- at 20 fps the
    // whole game ran at ~2/3 speed (user-reported "everything slows
    // down"; the benchmark tape's 1,371 ticks consumed 2,157 vblanks).
    // The boot-backlog concern the cap addressed is handled upstream by
    // EngineClock::reset_origin after init.
    max_fixed_ticks_before_visual: if cfg!(feature = "lockstep-visuals") {
        2
    } else {
        0
    },
};

const _: () = assert!(
    RUNTIME_SCHEDULE.stream_pump_sectors_per_tick >= drive_sectors_per_background_tick(),
    "pump would drain slower than the CD delivers"
);
