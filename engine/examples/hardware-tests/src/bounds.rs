// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounds for the suite's busy-waits, and the skip key.
//!
//! Every wait on a device in this suite is a loop with a counter, never a
//! timer (a timer that is not running makes a time bound a hang too). A wait
//! that runs out is a measurement: the caller stops the device, records what
//! it saw and goes on. The counts are in loop iterations, and each constant
//! says what it covers where it is used.
//!
//! Holding SELECT while a step runs makes every bounded wait from then on
//! in that step give up after one pass (`scale` returns 1 while the key is
//! down). The key is sampled at the start of each record (`record_start`) and
//! at each step; it clears when the next step begins. It is read from the
//! pad port directly.

use crate::sio_timing::transaction;
use crate::ui;

static mut SKIPPING: bool = false;

/// Pad polls are 5 bytes behind a 8,000-cycle select delay: a little over half
/// a millisecond, once a record.
const POLL_SETUP_CYCLES: u16 = 8_000;

/// A step is starting: forget a skip from the step before.
pub(crate) fn begin_step() {
    heartbeat();
    // SAFETY: single thread; plain statics.
    unsafe { SKIPPING = false };
}

/// Whether SELECT was down at the last look.
pub(crate) fn skipping() -> bool {
    // SAFETY: single thread; plain static.
    unsafe { SKIPPING }
}

/// `limit` iterations, or 1 once the skip key has been seen this step.
pub(crate) fn scale(limit: u32) -> u32 {
    if skipping() {
        1
    } else {
        limit
    }
}

/// Look at SELECT now. Sticky until the next step.
pub(crate) fn poll() {
    // SAFETY: single thread; plain statics.
    if unsafe { SKIPPING } {
        return;
    }
    let seen = transaction(
        false,
        POLL_SETUP_CYCLES,
        &[0x01, 0x42, 0x00, 0x00, 0x00],
        true,
    );
    let down = seen.bytes[2].reply == 0x5A && seen.bytes[3].reply & 1 == 0;
    if down {
        // SAFETY: single thread; plain static.
        unsafe { SKIPPING = true };
    }
}

/// A record is about to run: put its id on the screen (a photo of a hang
/// then names it) and look at the skip key.
pub(crate) fn record_start(id: u16) {
    heartbeat();
    ui::record_id(id);
    poll();
}

/// Tell the SDK stop panel's watchdog (a `console-diag` build) that the suite
/// is alive: a step that only pokes registers for many seconds shows no GP1
/// write, GPU DMA kick or CD command otherwise, and the panel would take the
/// screen as a stall. Counts as a GP1 write; touches no hardware, and compiles
/// to nothing without the feature.
pub(crate) fn heartbeat() {
    psx_io::diag::gp1_write();
}
