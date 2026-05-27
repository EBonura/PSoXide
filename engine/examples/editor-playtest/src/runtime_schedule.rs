#[derive(Copy, Clone)]
pub(crate) struct RuntimeScheduleConfig {
    pub(crate) portal_max_depth: u8,
    pub(crate) portal_min_width_q12: i32,
    pub(crate) active_refresh_sectors: i32,
    pub(crate) active_job_builds_per_tick: usize,
    pub(crate) retained_inactive_rooms: usize,
    pub(crate) post_cross_render_debug_frames: u8,
    pub(crate) stream_prefetch_count: usize,
    pub(crate) stream_prefetch_max_extra_sectors: u32,
    pub(crate) stream_prefetch_max_total_sectors: u32,
    pub(crate) stream_load_batch_count: usize,
    pub(crate) stream_pump_sectors_per_tick: usize,
    pub(crate) stream_bootstrap_pump_limit: usize,
    pub(crate) max_fixed_ticks_before_visual: u16,
}

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
    stream_prefetch_count: 1,
    stream_prefetch_max_extra_sectors: 8,
    stream_prefetch_max_total_sectors: 48,
    stream_load_batch_count: 4,
    stream_pump_sectors_per_tick: 8,
    stream_bootstrap_pump_limit: 4096,
    max_fixed_ticks_before_visual: 0,
};
