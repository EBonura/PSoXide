//! PSoXide host-side telemetry hooks for headless screenshots and profiling.
//!
//! The event IDs come from the shared `psx-telemetry` crate. The writes are
//! gated behind a telemetry feature, so normal PS1 builds only pay no-op calls.

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

const EVENT_KIND_FRAME_BEGIN: u8 = 1;
const EVENT_KIND_STAGE_BEGIN: u8 = 2;
const EVENT_KIND_STAGE_END: u8 = 3;
const EVENT_KIND_COUNTER: u8 = 4;
const EVENT_KIND_TASK_BEGIN: u8 = 5;
const EVENT_KIND_TASK_END: u8 = 6;

#[cfg(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
))]
const EVENT_ADDR: *mut u32 = 0xBF80_2F00 as *mut u32;
#[cfg(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
))]
const VALUE_ADDR: *mut u32 = 0xBF80_2F04 as *mut u32;
#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
const LOG_ADDR: *mut u32 = 0xBF80_2F0C as *mut u32;

#[inline(always)]
pub fn frame_begin(frame: u32) {
    emit_value(frame);
    emit_event(EVENT_KIND_FRAME_BEGIN, 0);
}

#[inline(always)]
pub fn stage_begin(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_BEGIN, stage_id);
}

#[inline(always)]
pub fn stage_end(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_END, stage_id);
}

#[inline(always)]
pub fn counter(counter_id: u16, value: u32) {
    emit_value(value);
    emit_event(EVENT_KIND_COUNTER, counter_id);
}

#[inline(always)]
pub fn task_begin(task_id: u16) {
    emit_event(EVENT_KIND_TASK_BEGIN, task_id);
}

#[inline(always)]
pub fn task_end(task_id: u16) {
    emit_event(EVENT_KIND_TASK_END, task_id);
}

// Keep the byte loop out of the already-large gameplay routine.  With
// telemetry enabled, forcing this helper inline lets constant strings expand
// into hundreds of volatile stores inside `play`, which can push a MIPS-I
// conditional branch beyond its signed PC16 range.  Logging is a cold
// diagnostic path, so one shared call is also smaller without affecting the
// profiled render/simulation stages.
#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(never)]
pub fn debug_log(message: &str) {
    debug_bytes(message.as_bytes());
    debug_byte(b'\n');
}

// In shipping and host builds every call still compiles away completely.
#[cfg(not(all(target_arch = "mips", feature = "emulator-telemetry")))]
#[inline(always)]
pub fn debug_log(_message: &str) {}

/// Write a line to the emulator's guest debug-log port (0xBF80_2F0C),
/// UNCONDITIONALLY -- unlike `debug_log`, this is not gated behind the
/// `emulator-telemetry` feature, so it reaches PSoXide's Play debug terminal
/// from a normal (non-telemetry) build. Use sparingly (debug tooling only).
#[inline(always)]
pub fn console(message: &str) {
    #[cfg(target_arch = "mips")]
    {
        const PORT: *mut u32 = 0xBF80_2F0C as *mut u32;
        for &byte in message.as_bytes() {
            unsafe { core::ptr::write_volatile(PORT, byte as u32) };
        }
        unsafe { core::ptr::write_volatile(PORT, b'\n' as u32) };
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let _ = message;
    }
}

// Only the telemetry build's `debug_log` writes bytes.
#[cfg_attr(
    not(all(target_arch = "mips", feature = "emulator-telemetry")),
    allow(dead_code)
)]
#[inline(always)]
fn debug_bytes(bytes: &[u8]) {
    for &byte in bytes {
        debug_byte(byte);
    }
}

#[cfg(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
))]
#[inline(always)]
fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << 24) | id as u32
}

#[cfg(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
))]
#[inline(always)]
fn emit_value(value: u32) {
    unsafe {
        core::ptr::write_volatile(VALUE_ADDR, value);
    }
}

#[cfg(not(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
)))]
#[inline(always)]
fn emit_value(_value: u32) {}

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(always)]
fn debug_byte(byte: u8) {
    unsafe {
        core::ptr::write_volatile(LOG_ADDR, byte as u32);
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emulator-telemetry")))]
#[allow(dead_code)]
#[inline(always)]
fn debug_byte(_byte: u8) {}

#[cfg(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
))]
#[inline(always)]
fn emit_event(kind: u8, id: u16) {
    unsafe {
        core::ptr::write_volatile(EVENT_ADDR, encode_event(kind, id));
    }
}

#[cfg(not(all(
    target_arch = "mips",
    any(feature = "emulator-telemetry", feature = "performance-telemetry")
)))]
#[inline(always)]
fn emit_event(_kind: u8, _id: u16) {}
