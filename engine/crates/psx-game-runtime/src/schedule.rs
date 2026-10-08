//! Runtime scheduling policy knobs, carved out of `editor-playtest`'s
//! `runtime_schedule` module (phase 1, slice 2 of
//! docs/game-runtime-plan.md). [`RuntimeScheduleConfig`] is the shared
//! knob struct the streaming runtime reads; the game keeps its own `const`
//! instantiation and threads the individual knobs into runtime methods as
//! plain values.

/// Central runtime scheduling policy: background work pacing and the
/// fixed-tick catch-up cap.
#[derive(Copy, Clone)]
pub struct RuntimeScheduleConfig {
    /// CD sectors pumped per background streaming tick.
    pub stream_pump_sectors_per_tick: usize,
    /// Scheduler cap on fixed sim ticks before a visual frame (0 = uncapped).
    pub max_fixed_ticks_before_visual: u16,
}
