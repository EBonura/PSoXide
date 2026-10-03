//! GPU MMIO: `GP0`, `GP1`, `GPUREAD`, `GPUSTAT`.
//!
//! Thin wrappers over [`crate::read_u32`] / [`crate::write_u32`] that use
//! the register addresses from `psx-hw`. Each helper commits exactly
//! one MMIO access; higher-level SDK code composes them into commands.

use psx_hw::gpu::{GpuStat, GP0, GP1, GPUREAD, GPUSTAT};

#[cfg(any(feature = "present-queue", test))]
mod handoff;
#[cfg(any(feature = "present-queue", test))]
pub use handoff::{
    arm_direct_access_guard, begin_recording_raw, end_recording, is_recording,
    run_direct_access_guard, set_direct_access_guard, CommandRecording, RecordingOverflow,
    RECORDING_NODE_WORDS,
};
#[cfg(any(feature = "present-queue", test))]
pub use handoff::{pause_recording, RecordingPause};

/// Returned by [`pause_recording`]. Without the `present-queue` feature
/// nothing can record, so it holds nothing and does nothing.
#[cfg(not(any(feature = "present-queue", test)))]
#[must_use = "the recording resumes as soon as this is dropped"]
#[derive(Debug)]
pub struct RecordingPause(());

/// Pause any open command recording until the returned guard is dropped,
/// so the command writes in between reach the port; psx-vram's uploads
/// take one. Without the `present-queue` feature it compiles to nothing.
#[cfg(not(any(feature = "present-queue", test)))]
#[inline(always)]
pub fn pause_recording() -> RecordingPause {
    RecordingPause(())
}

/// Push a drawing or VRAM command word, or one of its parameters, to the
/// GPU's command port (`GP0`). Pairs with the word builders in
/// `psx_hw::gpu::gp0`: `write_command(gp0::draw_mode(...))`.
///
/// With the `present-queue` feature, the word goes into the open command
/// recording instead, if there is one, and an armed direct-access guard runs
/// before the port write.
#[doc(alias = "GP0")]
#[inline(always)]
pub fn write_command(word: u32) {
    #[cfg(any(feature = "present-queue", test))]
    if handoff::is_slow() {
        handoff::write_command_slow(word);
        return;
    }
    // SAFETY: GP0 (0x1F80_1810) is the GPU's aligned 32-bit command/data port on every PS1. Any
    // word is a legal write: the GPU parses it as a command or parameter, with no effect on
    // CPU-visible memory.
    unsafe { crate::write_u32(GP0, word) }
}

/// [`write_command`] straight to the port: never recorded, never behind the
/// direct-access guard. For code that has already made the port safe, such
/// as the present queue's stall recovery, which the guard itself runs.
/// Without the `present-queue` feature it is the same store as [`write_command`].
#[inline(always)]
pub fn write_command_unguarded(word: u32) {
    // SAFETY: GP0 (0x1F80_1810) is the GPU's aligned 32-bit command/data port on every PS1. Any
    // word is a legal write: the GPU parses it as a command or parameter, with no effect on
    // CPU-visible memory.
    unsafe { crate::write_u32(GP0, word) }
}

/// Write a display-control command (reset, display mode, display area,
/// DMA direction) to the GPU's control port (`GP1`).
///
/// With the `present-queue` feature an armed direct-access guard runs first,
/// unless a command recording is open (display control is never recorded).
#[doc(alias = "GP1")]
#[inline(always)]
pub fn write_display_control(word: u32) {
    #[cfg(any(feature = "present-queue", test))]
    if handoff::is_slow() {
        handoff::write_display_control_slow(word);
        return;
    }
    // SAFETY: GP1 (0x1F80_1814) is the GPU's aligned 32-bit control port on every PS1; any word is
    // a legal write and only changes GPU state.
    unsafe { crate::write_u32(GP1, word) }
}

/// [`write_display_control`] straight to the port, past the direct-access
/// guard; see [`write_command_unguarded`].
#[inline(always)]
pub fn write_display_control_unguarded(word: u32) {
    // SAFETY: GP1 (0x1F80_1814) is the GPU's aligned 32-bit control port on every PS1; any word is
    // a legal write and only changes GPU state.
    unsafe { crate::write_u32(GP1, word) }
}

/// Read the GPU status register.
#[doc(alias = "GPUSTAT")]
#[inline(always)]
pub fn status() -> GpuStat {
    // SAFETY: GPUSTAT (0x1F80_1814 on read) is the GPU's aligned 32-bit status register; reading it
    // has no side effects.
    GpuStat::from_bits_retain(unsafe { crate::read_u32(GPUSTAT) })
}

/// Read the GPU's response latch: VRAM-to-CPU transfer data or a GP1(10h)
/// info reply.
#[doc(alias = "GPUREAD")]
#[inline(always)]
pub fn read_data() -> u32 {
    // SAFETY: GPUREAD (0x1F80_1810 on read) is the GPU's aligned 32-bit response latch. A read may
    // advance a VRAM-to-CPU transfer, which is its purpose, and touches no CPU memory.
    unsafe { crate::read_u32(GPUREAD) }
}

/// Spin until the GPU is ready to accept a new command word.
///
/// Polls `GPUSTAT.READY_CMD`. On real hardware this bit is briefly
/// cleared while the GPU is busy ingesting a multi-word packet; our
/// emulator forces it on, so the loop is essentially a single read.
/// Do not use this between GP0(0xA0) image payload words: after the
/// transfer setup packet, the GPU is waiting for data, not a new
/// normal command.
#[inline]
pub fn wait_command_ready() {
    wait_ready(GpuStat::READY_CMD);
}

/// Spin budget for one GPUSTAT ready-wait. Long enough for the slowest
/// legitimate primitive, short enough that a stuck GPU hands control
/// back well inside a frame.
pub const READY_SPINS: u32 = 500_000;

/// Bounded ready-wait with recovery.
///
/// A GPU left mid-command never re-asserts its ready bits, so a bare
/// `while !ready {}` hangs forever. That is not hypothetical: aborting a
/// wedged linked-list DMA (see `psx_io::dma::abort`) stops the walker
/// partway through a packet, and the GPU then sits waiting for command
/// words that will never arrive. On timeout, GP1(01h) resets the command
/// buffer, which is the documented way to discard that partial command
/// and make the GPU accept work again.
fn wait_ready(flag: GpuStat) {
    #[cfg(any(feature = "present-queue", test))]
    if handoff::is_recording() {
        return;
    }
    let mut spins = 0u32;
    while !status().contains(flag) {
        if spins >= READY_SPINS {
            write_display_control(0x0100_0000);
            return;
        }
        spins += 1;
    }
}

/// Spin until the GPU can start a DMA block transfer.
#[inline]
pub fn wait_dma_ready() {
    wait_ready(GpuStat::READY_DMA_RECV);
}

/// Poll command readiness without resetting the GPU on timeout.
/// Reads once, then retries at most `spin_limit` times.
#[inline]
pub fn try_wait_command_ready(spin_limit: u32) -> bool {
    try_wait_ready(GpuStat::READY_CMD, spin_limit)
}
/// Poll DMA receive readiness without resetting the GPU on timeout.
#[inline]
pub fn try_wait_dma_ready(spin_limit: u32) -> bool {
    try_wait_ready(GpuStat::READY_DMA_RECV, spin_limit)
}
fn try_wait_ready(flag: GpuStat, spin_limit: u32) -> bool {
    #[cfg(any(feature = "present-queue", test))]
    if handoff::is_recording() {
        return true;
    }
    poll_ready(spin_limit, || status().contains(flag))
}

/// Renamed to [`write_command`].
#[deprecated(note = "renamed to `write_command`")]
#[inline(always)]
pub fn write_gp0(word: u32) {
    write_command(word)
}

/// Renamed to [`write_display_control`].
#[deprecated(note = "renamed to `write_display_control`")]
#[inline(always)]
pub fn write_gp1(word: u32) {
    write_display_control(word)
}

/// Renamed to [`status`].
#[deprecated(note = "renamed to `status`")]
#[inline(always)]
pub fn gpustat() -> GpuStat {
    status()
}

/// Renamed to [`read_data`].
#[deprecated(note = "renamed to `read_data`")]
#[inline(always)]
pub fn gpuread() -> u32 {
    read_data()
}

/// Renamed to [`wait_command_ready`].
#[deprecated(note = "renamed to `wait_command_ready`")]
#[inline(always)]
pub fn wait_cmd_ready() {
    wait_command_ready()
}

/// Renamed to [`try_wait_command_ready`].
#[deprecated(note = "renamed to `try_wait_command_ready`")]
#[inline(always)]
pub fn try_wait_cmd_ready(spin_limit: u32) -> bool {
    try_wait_command_ready(spin_limit)
}

fn poll_ready(spin_limit: u32, mut ready: impl FnMut() -> bool) -> bool {
    let mut remaining = spin_limit;
    loop {
        if ready() {
            return true;
        }
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_budget_includes_initial_probe() {
        for limit in 0..5 {
            let mut reads = 0;
            assert!(!poll_ready(limit, || {
                reads += 1;
                false
            }));
            assert_eq!(reads, limit + 1);
        }
        let mut reads = 0;
        assert!(poll_ready(2, || {
            reads += 1;
            reads == 3
        }));
        assert_eq!(reads, 3);
    }
}
