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
    arm_direct_access_guard, is_recording, run_direct_access_guard, set_direct_access_guard,
    start_recording_raw, ActiveRecording, CommandRecording, RecordingOverflow,
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

/// Run the direct-access guard if a queued frame armed it. Without the
/// `present-queue` feature nothing can arm it, so it compiles to nothing.
#[cfg(not(any(feature = "present-queue", test)))]
#[inline(always)]
pub fn run_direct_access_guard() {}

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
    // SAFETY: GP0 (`psx_hw::gpu::GP0`) is the GPU's aligned 32-bit command/data port on every PS1. Any
    // word is a legal write: the GPU parses it as a command or parameter, with no effect on
    // CPU-visible memory.
    unsafe { crate::write_u32(GP0, word) }
}

/// [`write_command`] for a run of words: the same words, in order, to the
/// port or into the open recording.
///
/// A recording is walked once per run instead of once per word, which is
/// what a HUD primitive of five to nine words wants: recording one word at
/// a time costs a call, a flag test and a reload of the whole recording
/// state for each of them.
#[inline(always)]
pub fn write_commands(words: &[u32]) {
    #[cfg(any(feature = "present-queue", test))]
    if handoff::is_slow() {
        handoff::write_commands_slow(words);
        return;
    }
    for &word in words {
        // SAFETY: GP0 (`psx_hw::gpu::GP0`) is the GPU's aligned 32-bit command/data port on every PS1. Any
        // word is a legal write: the GPU parses it as a command or parameter, with no effect on
        // CPU-visible memory.
        unsafe { crate::write_u32(GP0, word) }
    }
}

/// [`write_command`] straight to the port: never recorded, never behind the
/// direct-access guard. For code that has already made the port safe, such
/// as the present queue's stall recovery, which the guard itself runs.
/// Without the `present-queue` feature it is the same store as [`write_command`].
#[inline(always)]
pub fn write_command_unguarded(word: u32) {
    // SAFETY: GP0 (`psx_hw::gpu::GP0`) is the GPU's aligned 32-bit command/data port on every PS1. Any
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
    crate::diag::gp1_write();
    // SAFETY: GP1 (`psx_hw::gpu::GP1`) is the GPU's aligned 32-bit control port on every PS1; any word is
    // a legal write and only changes GPU state.
    unsafe { crate::write_u32(GP1, word) }
}

/// The function a display flip waits in, installed by the runtime that owns the
/// VBlank clock (`psx_rt::interrupts::install_vblank_counter`).
static mut FLIP_PACER: Option<fn()> = None;

/// Install (or with `None` remove) the pacer [`pace_flip`] calls. Called by
/// psx-rt; a program without that runtime never has one and flips unpaced.
pub fn set_flip_pacer(pacer: Option<fn()>) {
    // SAFETY: a volatile aligned store of a plain function pointer to a private static; the only
    // reader is `pace_flip`, which runs in the same foreground thread.
    unsafe { core::ptr::write_volatile(&raw mut FLIP_PACER, pacer) }
}

/// Wait, if the runtime says so, until a display flip is allowed (the
/// presentation cap, `psx_tick::PresentCap`), and count the flip. Call it
/// immediately before writing a GP1(05h) display-start word directly; a no-op
/// without a pacer.
#[inline]
pub fn pace_flip() {
    // SAFETY: a volatile aligned read of the private static written by `set_flip_pacer`.
    if let Some(pacer) = unsafe { core::ptr::read_volatile(&raw const FLIP_PACER) } {
        pacer();
    }
}

/// [`write_display_control`] straight to the port, past the direct-access
/// guard; see [`write_command_unguarded`].
#[inline(always)]
pub fn write_display_control_unguarded(word: u32) {
    // SAFETY: GP1 (`psx_hw::gpu::GP1`) is the GPU's aligned 32-bit control port on every PS1; any word is
    // a legal write and only changes GPU state.
    unsafe { crate::write_u32(GP1, word) }
}

/// Read the GPU status register.
#[doc(alias = "GPUSTAT")]
#[inline(always)]
pub fn status() -> GpuStat {
    // SAFETY: GPUSTAT (`psx_hw::gpu::GP1` on read) is the GPU's aligned 32-bit status register; reading it
    // has no side effects.
    GpuStat::from_bits_retain(unsafe { crate::read_u32(GPUSTAT) })
}

/// Read the GPU's response latch: VRAM-to-CPU transfer data or a GP1(10h)
/// info reply.
#[doc(alias = "GPUREAD")]
#[inline(always)]
pub fn read_data() -> u32 {
    // SAFETY: GPUREAD (`psx_hw::gpu::GP0` on read) is the GPU's aligned 32-bit response latch. A read may
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
    let diag_site = if flag == GpuStat::READY_CMD {
        crate::diag::site::GPU_READY_CMD
    } else {
        crate::diag::site::GPU_READY_DMA
    };
    crate::diag::wait_begin(diag_site, 0);
    while !status().contains(flag) {
        if spins >= READY_SPINS {
            crate::diag::wait_timeout(diag_site, 0, READY_SPINS);
            count_command_reset();
            write_display_control(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
            return;
        }
        spins += 1;
    }
    crate::diag::wait_end();
}

/// GP1(01h) resets [`wait_command_ready`] and [`wait_dma_ready`] have issued
/// after a ready bit stayed low for [`READY_SPINS`] polls.
///
/// In `.data`, not `.bss`, so a bare chain-loader image that nothing zeroes a
/// `.bss` for counts from zero (see `irq::POLLING_KEEP`).
#[cfg_attr(target_arch = "mips", link_section = ".data")]
static mut COMMAND_RESETS: u32 = 0;

#[cold]
fn count_command_reset() {
    // SAFETY: volatile aligned accesses to this private static; only this
    // function writes it and the program is single threaded (no handler
    // touches it).
    unsafe {
        let count = core::ptr::addr_of_mut!(COMMAND_RESETS);
        count.write_volatile(count.read_volatile().wrapping_add(1));
    }
}

/// How many times a ready-wait has timed out and reset the GPU's command
/// buffer since boot: a count of GPU hangs the SDK recovered from.
pub fn command_reset_count() -> u32 {
    // SAFETY: a volatile aligned read of a private static.
    unsafe { core::ptr::addr_of!(COMMAND_RESETS).read_volatile() }
}

/// Send the draw-done sequence ([`psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE`]) to
/// the command port, bit 26 awaited before each word so the 16-word FIFO
/// never overflows behind a big draw. This is what closes immediate drawing
/// so that [`GpuStat::IRQ1`] rises once it is drawn; the sequence holds
/// GP0(1Fh) several times, with VRAM self-copies between them, because the
/// FPGA PSX core misses a lone GP0(1Fh) (see the sequence's documentation).
/// On silicon the repeats are ignored once the flag is set.
#[inline(never)]
pub fn signal_draw_done() {
    for &word in &psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE {
        wait_command_ready();
        write_command(word);
    }
}

/// [`signal_draw_done`] straight to the port, past the direct-access guard
/// and any recording, for code that has already made the port safe (the
/// present queue's stall recovery). A word whose ready wait times out is
/// preceded by a command-buffer reset, as that recovery does.
#[cold]
#[inline(never)]
pub fn signal_draw_done_unguarded() {
    for &word in &psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE {
        if !try_wait_command_ready(READY_SPINS) {
            write_display_control_unguarded(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
        }
        write_command_unguarded(word);
    }
}

/// Times a [`DrawDoneWatch`] has said a frame is done without GPUSTAT bit 24.
/// In `.data` like the other counters here. It stays 0 on silicon and in the
/// emulator.
#[cfg_attr(target_arch = "mips", link_section = ".data")]
static mut DRAW_DONE_FALLBACKS: u32 = 0;

/// How many times a [`DrawDoneWatch`] has fired since boot.
pub fn draw_done_fallback_count() -> u32 {
    // SAFETY: a volatile aligned read of a private static.
    unsafe { core::ptr::addr_of!(DRAW_DONE_FALLBACKS).read_volatile() }
}

/// A bounded fallback for a frame whose GP0(1Fh) never raised GPUSTAT bit 24.
///
/// A caller that waits for bit 24 (a queued display flip) polls this too.
/// It says the frame is done when DMA channel 2 is idle and GPUSTAT bits 26
/// and 28 (ready for a command, ready for a DMA block) have stayed up from one
/// VBlank count to at least [`Self::STRETCH_VBLANKS`] counts later. Bit 28
/// alone rises about one large primitive before the drawing ends and bit 26
/// can pulse high between primitives, but with the channel idle nothing new
/// arrives, so a stretch longer than any one primitive (a whole VBlank
/// interval and more) proves the GPU is finished.
///
/// It must not be needed on silicon: bit 24 rises first and the caller stops
/// asking. Its calls cost two register reads. The caller resets it whenever it
/// starts a new wait, and counts a `true` with [`Self::fired`].
#[derive(Debug)]
pub struct DrawDoneWatch {
    quiet_since: u32,
    quiet: bool,
}

impl DrawDoneWatch {
    /// VBlank counts the GPU must stay quiet for.
    pub const STRETCH_VBLANKS: u32 = 2;

    pub const fn new() -> Self {
        Self {
            quiet_since: 0,
            quiet: false,
        }
    }

    /// Forget any quiet seen: a new frame's work is starting.
    pub fn reset(&mut self) {
        self.quiet = false;
    }

    /// True when the GPU has been quiet for the stretch, `vblank_count` being
    /// the current VBlank counter (`psx_rt::interrupts::vblank_count`).
    pub fn poll(&mut self, vblank_count: u32) -> bool {
        let status = status();
        let quiet = status.contains(GpuStat::READY_CMD | GpuStat::READY_DMA_RECV)
            && !crate::dma::is_busy(crate::dma::Channel::Gpu);
        if !quiet {
            self.quiet = false;
            return false;
        }
        if !self.quiet {
            self.quiet = true;
            self.quiet_since = vblank_count;
            return false;
        }
        vblank_count.wrapping_sub(self.quiet_since) >= Self::STRETCH_VBLANKS
    }

    /// Count a fallback the caller acted on.
    pub fn fired(&mut self) {
        self.quiet = false;
        // SAFETY: volatile aligned accesses to a private static; the program
        // is single threaded and no handler touches it.
        unsafe {
            let count = core::ptr::addr_of_mut!(DRAW_DONE_FALLBACKS);
            count.write_volatile(count.read_volatile().wrapping_add(1));
        }
    }
}

impl Default for DrawDoneWatch {
    fn default() -> Self {
        Self::new()
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
