//! Runtime scheduling policy knobs, carved out of `editor-playtest`'s
//! `runtime_schedule` module (phase 1, slice 2 of
//! docs/game-runtime-plan.md). [`RuntimeScheduleConfig`] is the shared
//! knob struct the room window, streaming, and visibility runtimes
//! read; the game keeps its own `const` instantiation and threads the
//! individual knobs into runtime methods as plain values.

/// Central runtime scheduling policy: memory residency, render
/// visibility, and background work pacing as separate knobs.
#[derive(Copy, Clone)]
pub struct RuntimeScheduleConfig {
    /// Portal-hop depth cap for the frustum-clipped portal traversal.
    pub portal_max_depth: u8,
    /// Minimum clipped portal extent (Q12) before a portal is rejected as tiny.
    pub portal_min_width_q12: i32,
    /// Player travel, in sectors, that forces an active-window rebuild.
    pub active_refresh_sectors: i32,
    /// Room builds the incremental window job performs per tick.
    pub active_job_builds_per_tick: usize,
    /// Previously active rooms retained in the window beyond the requested set.
    pub retained_inactive_rooms: usize,
    /// Frames of host-visible render breadcrumbs after a room crossing.
    pub post_cross_render_debug_frames: u8,
    /// Rooms schedulable per streaming load plan.
    pub stream_load_batch_count: usize,
    /// CD sectors pumped per background streaming tick.
    pub stream_pump_sectors_per_tick: usize,
    /// Pump-step cap for the boot-time streamed-window bootstrap.
    pub stream_bootstrap_pump_limit: usize,
    /// Scheduler cap on fixed sim ticks before a visual frame (0 = uncapped).
    pub max_fixed_ticks_before_visual: u16,
}
