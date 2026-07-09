// SPDX-License-Identifier: GPL-2.0-or-later
//! Guest-side telemetry event emitters (Expansion 2 MMIO ports).
//!
//! Every game carried a hand-copied shim of these writes (voxide and hl-psx
//! byte-identical, oot-psx a subset); this module is that shim, once. The
//! MMIO writes compile only with the crate's `emit` feature AND a MIPS
//! target; games forward their own flag
//! (`emulator-telemetry = ["psx-telemetry/emit"]`) so shipping builds pay
//! only empty inlined calls.
//!
//! Ports (decoded by `emulator-core`): `0xBF80_2F00` event word
//! (`kind << 24 | id`), `0xBF80_2F04` value latch (written before the event
//! that consumes it), `0xBF80_2F0C` debug-log byte stream.
//!
//! [`console`] is deliberately NOT feature-gated (mips-gated only): it is the
//! Play debug terminal used from normal builds. Use sparingly.

const EVENT_KIND_FRAME_BEGIN: u8 = 1;
const EVENT_KIND_STAGE_BEGIN: u8 = 2;
const EVENT_KIND_STAGE_END: u8 = 3;
const EVENT_KIND_COUNTER: u8 = 4;
const EVENT_KIND_TASK_BEGIN: u8 = 5;
const EVENT_KIND_TASK_END: u8 = 6;

#[cfg(all(target_arch = "mips", feature = "emit"))]
const EVENT_ADDR: *mut u32 = 0xBF80_2F00 as *mut u32;
#[cfg(all(target_arch = "mips", feature = "emit"))]
const VALUE_ADDR: *mut u32 = 0xBF80_2F04 as *mut u32;
#[cfg(all(target_arch = "mips", feature = "emit"))]
const LOG_ADDR: *mut u32 = 0xBF80_2F0C as *mut u32;

/// Mark the start of guest frame `frame` (drives `--guest-frames` stops).
#[inline(always)]
pub fn frame_begin(frame: u32) {
    emit_value(frame);
    emit_event(EVENT_KIND_FRAME_BEGIN, 0);
}

/// Enter a profiling stage (see [`crate::stage`] for ids).
#[inline(always)]
pub fn stage_begin(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_BEGIN, stage_id);
}

/// Leave a profiling stage.
#[inline(always)]
pub fn stage_end(stage_id: u16) {
    emit_event(EVENT_KIND_STAGE_END, stage_id);
}

/// Record `value` under a counter id (see [`crate::counter`]).
#[inline(always)]
pub fn counter(counter_id: u16, value: u32) {
    emit_value(value);
    emit_event(EVENT_KIND_COUNTER, counter_id);
}

/// Enter a background-task span (see [`crate::task`]).
#[inline(always)]
pub fn task_begin(task_id: u16) {
    emit_event(EVENT_KIND_TASK_BEGIN, task_id);
}

/// Leave a background-task span.
#[inline(always)]
pub fn task_end(task_id: u16) {
    emit_event(EVENT_KIND_TASK_END, task_id);
}

/// Write a line to the emulator debug log, telemetry builds only.
#[inline(always)]
pub fn debug_log(message: &str) {
    debug_bytes(message.as_bytes());
    debug_byte(b'\n');
}

/// Write a line to the emulator's guest debug-log port UNCONDITIONALLY
/// (mips-gated, not feature-gated), so it reaches PSoXide's Play debug
/// terminal from a normal build. Use sparingly (debug tooling only).
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

#[inline(always)]
fn debug_bytes(bytes: &[u8]) {
    for &byte in bytes {
        debug_byte(byte);
    }
}

#[cfg(all(target_arch = "mips", feature = "emit"))]
#[inline(always)]
fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << 24) | id as u32
}

#[cfg(all(target_arch = "mips", feature = "emit"))]
#[inline(always)]
fn emit_value(value: u32) {
    unsafe {
        core::ptr::write_volatile(VALUE_ADDR, value);
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emit")))]
#[inline(always)]
fn emit_value(_value: u32) {}

#[cfg(all(target_arch = "mips", feature = "emit"))]
#[inline(always)]
fn debug_byte(byte: u8) {
    unsafe {
        core::ptr::write_volatile(LOG_ADDR, byte as u32);
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emit")))]
#[inline(always)]
fn debug_byte(_byte: u8) {}

#[cfg(all(target_arch = "mips", feature = "emit"))]
#[inline(always)]
fn emit_event(kind: u8, id: u16) {
    unsafe {
        core::ptr::write_volatile(EVENT_ADDR, encode_event(kind, id));
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emit")))]
#[inline(always)]
fn emit_event(_kind: u8, _id: u16) {}
