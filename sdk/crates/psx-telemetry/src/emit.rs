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
//! Ports (decoded by `emulator-core`, addresses in `psx_hw::memory::expansion2::telemetry`):
//! the event word (`kind << 24 | id`), the value latch (written before the
//! event that consumes it) and the debug-log byte stream.
//!
//! [`console`] is deliberately NOT feature-gated (mips-gated only): it is the
//! Play debug terminal used from normal builds. Use sparingly.

#[cfg(target_arch = "mips")]
use psx_hw::memory::{expansion2::telemetry, to_kseg1};

const EVENT_KIND_FRAME_BEGIN: u8 = 1;
const EVENT_KIND_STAGE_BEGIN: u8 = 2;
const EVENT_KIND_STAGE_END: u8 = 3;
const EVENT_KIND_COUNTER: u8 = 4;
const EVENT_KIND_TASK_BEGIN: u8 = 5;
const EVENT_KIND_TASK_END: u8 = 6;

/// The uncached view of one of the emulator's telemetry ports.
#[cfg(target_arch = "mips")]
const fn uncached(physical: u32) -> *mut u32 {
    to_kseg1(physical) as *mut u32
}

#[cfg(all(target_arch = "mips", feature = "emit"))]
const EVENT_ADDR: *mut u32 = uncached(telemetry::EVENT);
#[cfg(all(target_arch = "mips", feature = "emit"))]
const VALUE_ADDR: *mut u32 = uncached(telemetry::VALUE);
#[cfg(all(target_arch = "mips", feature = "emit"))]
const LOG_ADDR: *mut u32 = uncached(telemetry::LOG);

/// Emulator cycle counter, wrapping at 32 bits. Returns zero on host builds
/// and when the `emit` feature is disabled, without touching telemetry MMIO.
#[inline(always)]
pub fn cycles() -> u32 {
    #[cfg(all(target_arch = "mips", feature = "emit"))]
    {
        // SAFETY: `telemetry::CYCLES` is the word-aligned cycle-counter port in the
        // uncached Expansion 2 window, decoded by the emulator; the read has
        // no side effects and touches no Rust-owned memory.
        unsafe { core::ptr::read_volatile(uncached(telemetry::CYCLES) as *const u32) }
    }
    #[cfg(not(all(target_arch = "mips", feature = "emit")))]
    {
        0
    }
}

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
        const PORT: *mut u32 = uncached(telemetry::LOG);
        for &byte in message.as_bytes() {
            // SAFETY: PORT is the word-aligned debug-log port in the uncached
            // Expansion 2 window, not Rust-owned memory; each write only
            // appends one byte to the emulator's log stream.
            unsafe { core::ptr::write_volatile(PORT, byte as u32) };
        }
        // SAFETY: same debug-log port as the loop above.
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

#[cfg(any(all(target_arch = "mips", feature = "emit"), test))]
#[inline(always)]
fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << 24) | id as u32
}

#[cfg(all(target_arch = "mips", feature = "emit"))]
#[inline(always)]
fn emit_value(value: u32) {
    // SAFETY: VALUE_ADDR is the word-aligned value-latch port in the uncached
    // Expansion 2 window, not Rust-owned memory; the write only latches a word.
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
    // SAFETY: LOG_ADDR is the word-aligned debug-log port in the uncached
    // Expansion 2 window, not Rust-owned memory; the write appends one byte.
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
    // SAFETY: EVENT_ADDR is the word-aligned event port in the uncached
    // Expansion 2 window, not Rust-owned memory; the write records one event.
    unsafe {
        core::ptr::write_volatile(EVENT_ADDR, encode_event(kind, id));
    }
}

#[cfg(not(all(target_arch = "mips", feature = "emit")))]
#[inline(always)]
fn emit_event(_kind: u8, _id: u16) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The emulator decodes `kind = word >> 24` and `id = word & 0xFFFF`
    /// (`emulator-core/src/telemetry.rs`: KIND_SHIFT 24, ID_MASK 0xFFFF) and
    /// names kinds 1 to 6 FrameBegin, StageBegin, StageEnd, Counter,
    /// TaskBegin, TaskEnd.
    #[test]
    fn event_words_decode_the_way_the_emulator_does() {
        let kinds = [
            (EVENT_KIND_FRAME_BEGIN, 1u8),
            (EVENT_KIND_STAGE_BEGIN, 2),
            (EVENT_KIND_STAGE_END, 3),
            (EVENT_KIND_COUNTER, 4),
            (EVENT_KIND_TASK_BEGIN, 5),
            (EVENT_KIND_TASK_END, 6),
        ];
        for (kind, number) in kinds {
            assert_eq!(kind, number);
            for id in [0u16, 1, 51, 266, 0xFFFF] {
                let word = encode_event(kind, id);
                assert_eq!((word >> 24) & 0xFF, number as u32);
                assert_eq!(word & 0xFFFF, id as u32);
            }
        }
    }

    #[test]
    fn emitters_are_silent_no_ops_off_target() {
        // Host builds and builds without `emit` touch no port and read no
        // cycle counter.
        assert_eq!(cycles(), 0);
        frame_begin(7);
        stage_begin(crate::stage::UPDATE);
        stage_end(crate::stage::UPDATE);
        counter(crate::counter::TRI_PRIMITIVES, 9);
        task_begin(crate::task::FIXED_UPDATE);
        task_end(crate::task::FIXED_UPDATE);
        debug_log("x");
        console("x");
    }
}
