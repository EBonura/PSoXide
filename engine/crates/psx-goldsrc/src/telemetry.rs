//! PSoXide host-side telemetry hooks for headless screenshots and profiling.
//!
//! The event ids come from the shared `psx-telemetry` crate and so do the writers: every store into
//! the Expansion 2 telemetry window lives in `psx_telemetry::emit`, compiled only with its `emit`
//! feature (this crate's `emulator-telemetry`, `performance-telemetry` and `warp-probe` forward to it),
//! so a shipping build holds none.

pub use psx_telemetry::emit::{
    console, counter, debug_log, frame_begin, stage_begin, stage_end, task_begin, task_end,
};
pub use psx_telemetry::{counter, stage, task};

/// hl-psx owns these otherwise-unused room micro-profiler slots. PSoXide's
/// generic profile CSV still exposes the legacy column names; hl-build writes
/// the same values to `affine-error.csv` with their semantic names.
pub mod affine_counter {
    pub const ERROR_MAX_Q8: u16 = 85;
    pub const ERROR_P50_Q8: u16 = 86;
    pub const ERROR_P95_Q8: u16 = 87;
    pub const ERROR_P99_Q8: u16 = 88;
    pub const SPLIT_CANDIDATES: u16 = 89;
    pub const EXTRA_PACKETS_REQUESTED: u16 = 90;
    pub const EXTRA_PACKETS_EMITTED: u16 = 91;
    // Patch-priority proof. Projected boundary span is the primary risk key; a
    // correct top-K has either no rejection or a highest rejected priority no
    // greater than the lowest selected priority.
    pub const LOWEST_SELECTED_PRIORITY: u16 = 107;
    pub const HIGHEST_REJECTED_PRIORITY: u16 = 108;
    // Rejected packets are exactly requested-emitted, and the disabled legacy
    // recursive path no longer has level-two splits. Reuse those two exported
    // slots for the native patch metrics instead of requiring a newer PSoXide
    // profiler schema.
    pub const SPLIT_PATCHES: u16 = 95;
    pub const ADDED_GTE_TRANSFORMS: u16 = 96;
    pub const REMAINING_ERROR_MAX_Q8: u16 = 103;
    pub const REMAINING_ERROR_P50_Q8: u16 = 104;
    pub const REMAINING_ERROR_P95_Q8: u16 = 105;
    pub const REMAINING_ERROR_P99_Q8: u16 = 106;
}
