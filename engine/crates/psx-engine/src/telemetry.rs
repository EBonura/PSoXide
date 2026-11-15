//! Lightweight guest-runtime telemetry for PSoXide host tooling.
//!
//! One writer: the emitters are `psx_telemetry::emit`'s, re-exported here under the names the
//! engine's callers use. The engine's own features forward to that crate's, so a shipping build
//! has no store to the telemetry ports and a host build compiles every call to nothing.

pub use psx_telemetry::emit::{
    counter, debug_bytes, debug_line, debug_log, frame_begin, stage_begin, stage_end, task_begin,
    task_end,
};
pub use psx_telemetry::{counter, stage, task};

/// Read the emulator-observed guest cycle counter.
///
/// Only meaningful under PSoXide's emulator telemetry port; zero unless the `emulator-telemetry`
/// feature is on and the emulator provides the Expansion 2 cycle register.
#[inline(always)]
pub fn cycle_counter() -> u32 {
    psx_telemetry::emit::cycles()
}

#[cfg(test)]
mod tests {
    use super::counter;

    #[test]
    fn frame_pacing_counter_ids_extend_existing_room_counters() {
        assert_eq!(counter::SIM_TICKS, counter::MODEL_ATLAS_UPLOADS + 1);
        assert_eq!(counter::VISUAL_FRAMES, counter::SIM_TICKS + 1);
        assert_eq!(
            counter::VISUAL_MAX_LATENESS_VBLANKS,
            counter::VISUAL_INTERVAL_VBLANKS + 1
        );
    }
}
