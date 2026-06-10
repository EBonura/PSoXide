//! Lightweight guest-runtime telemetry for PSoXide host tooling.
//!
//! The runtime emits compact stage/counter events through an emulator-observed
//! Expansion 2 port. On non-MIPS host builds these functions compile to no-ops,
//! so editor-side preview code can depend on `psx-engine` without touching host
//! memory.

pub use psx_telemetry::{counter, stage, task};

const EVENT_KIND_FRAME_BEGIN: u8 = 1;
const EVENT_KIND_STAGE_BEGIN: u8 = 2;
const EVENT_KIND_STAGE_END: u8 = 3;
const EVENT_KIND_COUNTER: u8 = 4;
const EVENT_KIND_TASK_BEGIN: u8 = 5;
const EVENT_KIND_TASK_END: u8 = 6;

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
const EVENT_ADDR: *mut u32 = 0xBF80_2F00 as *mut u32;
#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
const VALUE_ADDR: *mut u32 = 0xBF80_2F04 as *mut u32;
#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
const CYCLE_ADDR: *const u32 = 0xBF80_2F08 as *const u32;
#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
const LOG_ADDR: *mut u32 = 0xBF80_2F0C as *mut u32;

/// Mark the start of a guest frame.
#[inline(always)]
pub fn frame_begin(frame: u32) {
    emit_value(frame);
    emit_event(EVENT_KIND_FRAME_BEGIN, 0);
}

/// Mark the start of a named stage.
#[inline(always)]
pub fn stage_begin(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_BEGIN, stage_id);
}

/// Mark the end of a named stage.
#[inline(always)]
pub fn stage_end(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_END, stage_id);
}

/// Emit a numeric counter value.
#[inline(always)]
pub fn counter(counter_id: u16, value: u32) {
    emit_value(value);
    emit_event(EVENT_KIND_COUNTER, counter_id);
}

/// Mark the start of a scheduled task.
#[inline(always)]
pub fn task_begin(task_id: u16) {
    emit_event(EVENT_KIND_TASK_BEGIN, task_id);
}

/// Mark the end of a scheduled task.
#[inline(always)]
pub fn task_end(task_id: u16) {
    emit_event(EVENT_KIND_TASK_END, task_id);
}

/// Read the emulator-observed guest cycle counter.
///
/// This is only meaningful under PSoXide's emulator telemetry port. It
/// returns zero unless the `emulator-telemetry` feature is enabled and the
/// emulator provides the Expansion 2 cycle register.
#[inline(always)]
pub fn cycle_counter() -> u32 {
    read_cycle_counter()
}

/// Emit one complete debug line to the PSoXide host terminal.
///
/// This is an emulator-only diagnostic path. On host builds it compiles to a
/// no-op unless the `emulator-telemetry` feature is enabled, in which
/// case PS1/MIPS builds write ASCII bytes through the Expansion 2
/// telemetry page.
#[inline(always)]
pub fn debug_log(message: &str) {
    debug_bytes(message.as_bytes());
    debug_byte(b'\n');
}

/// Emit one complete debug line from an ASCII byte slice.
#[inline(always)]
pub fn debug_line(bytes: &[u8]) {
    debug_bytes(bytes);
    debug_byte(b'\n');
}

/// Emit raw debug bytes without appending a newline.
#[inline(always)]
pub fn debug_bytes(bytes: &[u8]) {
    for &byte in bytes {
        debug_byte(byte);
    }
}

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(always)]
fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << 24) | id as u32
}

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(always)]
fn emit_value(value: u32) {
    unsafe {
        core::ptr::write_volatile(VALUE_ADDR, value);
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emulator-telemetry")))]
#[inline(always)]
fn emit_value(_value: u32) {}

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(always)]
fn read_cycle_counter() -> u32 {
    unsafe { core::ptr::read_volatile(CYCLE_ADDR) }
}

#[cfg(not(all(target_arch = "mips", feature = "emulator-telemetry")))]
#[inline(always)]
fn read_cycle_counter() -> u32 {
    0
}

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(always)]
fn debug_byte(byte: u8) {
    unsafe {
        core::ptr::write_volatile(LOG_ADDR, byte as u32);
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emulator-telemetry")))]
#[inline(always)]
fn debug_byte(_byte: u8) {}

#[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
#[inline(always)]
fn emit_event(kind: u8, id: u16) {
    unsafe {
        core::ptr::write_volatile(EVENT_ADDR, encode_event(kind, id));
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emulator-telemetry")))]
#[inline(always)]
fn emit_event(_kind: u8, _id: u16) {}

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
