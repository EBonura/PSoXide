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
    while !status().contains(flag) {
        if spins >= READY_SPINS {
            count_command_reset();
            write_display_control(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
            return;
        }
        spins += 1;
    }
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

/// The draw-done sequence ([`psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE`]) as one
/// linked-list node, then the end of the list. Shared by
/// `psx_gpu::chain::DRAW_DONE_NODE` and [`signal_draw_done`]. A `static mut`
/// only so that [`set_draw_done_scratch`] can move the filled block; nothing
/// else writes it.
static mut DRAW_DONE_LIST: [u32; 1 + psx_hw::gpu::gp0::DRAW_DONE_WORDS] = {
    let mut list = [0u32; 1 + psx_hw::gpu::gp0::DRAW_DONE_WORDS];
    list[0] = ((psx_hw::gpu::gp0::DRAW_DONE_WORDS as u32) << psx_hw::dma::linked_list::LENGTH_SHIFT)
        | psx_hw::dma::linked_list::END;
    let mut i = 0;
    while i < psx_hw::gpu::gp0::DRAW_DONE_WORDS {
        list[1 + i] = psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE[i];
        i += 1;
    }
    list
};

/// Pointer to the draw-done list (a node chains can end on).
#[inline]
pub fn draw_done_list() -> *const u32 {
    core::ptr::addr_of!(DRAW_DONE_LIST).cast::<u32>()
}

/// Move the 16 x 8 block the draw-done sequence fills with black to `(x, y)`,
/// for a game that keeps data in the default one (the bottom right corner of
/// VRAM, [`psx_hw::gpu::gp0::DRAW_DONE_SCRATCH`]). Call it once at start, before
/// the first frame is submitted: it waits for channel 2 to go idle (bounded)
/// and rewrites the three fill coordinates in place.
///
/// `x` must be a multiple of 16 (GP0(02h) rounds it down) and the block must
/// lie inside VRAM and stay unused.
pub fn set_draw_done_scratch(x: u16, y: u16) {
    assert!(x % 16 == 0 && x <= 1008 && y <= 504, "the scratch block must fit VRAM");
    #[cfg(target_arch = "mips")]
    let _ = crate::dma::wait_done(crate::dma::Channel::Gpu, crate::dma::DEFAULT_SPINS);
    let at = psx_hw::gpu::pack_xy(x, y);
    // SAFETY: the list is only read by a channel 2 walk, which is idle; the three words
    // are the fills' coordinate words in the sequence (list index = word index + 1).
    unsafe {
        let list = core::ptr::addr_of_mut!(DRAW_DONE_LIST).cast::<u32>();
        for fill in 0..3 {
            list.add(1 + 4 * fill + 2).write_volatile(at);
        }
    }
}

/// Close immediate drawing so that GPUSTAT bit 24 ([`GpuStat::IRQ1`]) rises
/// once it is drawn: send the draw-done sequence
/// ([`psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE`], GP0(1Fh) requests with fills of
/// a scratch block between them, see its documentation for why one request is
/// not enough on an FPGA PSX core).
///
/// The words go to the GPU as one DMA channel 2 linked-list node, back to
/// back, because the fix depends on the later requests already being queued
/// behind the fills; a write-by-write port path waiting on GPUSTAT bit 26
/// would hand each request to an idle GPU. With a command recording open
/// (`present-queue`) the words are recorded instead. Otherwise an armed
/// direct-access guard runs first, a walk still running on the channel is
/// waited out (aborted if it is wedged: [`psx_hw::dma`] spin budget), the
/// list is kicked and waited out again. Every wait is bounded.
#[inline(never)]
pub fn signal_draw_done() {
    #[cfg(any(feature = "present-queue", test))]
    if handoff::is_recording() {
        write_commands(&psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE);
        return;
    }
    run_direct_access_guard();
    kick_draw_done_list();
}

/// [`signal_draw_done`] past the direct-access guard and any recording, for
/// code that has already made the port and the channel safe (the present
/// queue's stall recovery, which the guard itself runs).
#[cold]
#[inline(never)]
pub fn signal_draw_done_unguarded() {
    kick_draw_done_list();
}

fn kick_draw_done_list() {
    use crate::dma::{self, Channel};
    if !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
        dma::abort(Channel::Gpu);
        write_display_control_unguarded(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
    }
    // Games re-route DMA for VRAM readback and do not always restore it.
    write_display_control_unguarded(psx_hw::gpu::gp1::dma_direction(
        psx_hw::gpu::DmaDirection::CpuToGp0 as u32,
    ));
    dma::enable_channel(Channel::Gpu);
    // SAFETY: the channel is idle (waited out or aborted above) and the list is
    // immutable static RAM that outlives the walk, which is waited out below.
    unsafe {
        dma::start_unguarded(
            Channel::Gpu,
            dma::Transfer {
                address: draw_done_list() as usize as u32,
                size: dma::size_words(0),
                control: psx_hw::dma::CHCR_TO_DEVICE
                    | psx_hw::dma::CHCR_SYNC_LINKED
                    | psx_hw::dma::CHCR_START,
            },
        );
    }
    if !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
        dma::abort(Channel::Gpu);
        write_display_control_unguarded(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
    }
    dma::compiler_barrier();
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

/// Renamed to [`wait_command_ready`].
#[deprecated(note = "renamed to `wait_command_ready`")]
#[inline(always)]
pub fn wait_cmd_ready() {
    wait_command_ready()
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
    fn the_scratch_block_can_be_moved_and_the_list_stays_well_formed() {
        let words = psx_hw::gpu::gp0::DRAW_DONE_WORDS;
        let list = || unsafe { core::slice::from_raw_parts(draw_done_list(), 1 + words) };
        assert_eq!(&list()[1..], &psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE);
        set_draw_done_scratch(960, 488);
        let at = psx_hw::gpu::pack_xy(960, 488);
        for fill in 0..3 {
            assert_eq!(list()[1 + 4 * fill], psx_hw::gpu::gp0::REQUEST_IRQ);
            assert_eq!(list()[1 + 4 * fill + 1], psx_hw::gpu::gp0::FILL_RECT);
            assert_eq!(list()[1 + 4 * fill + 2], at);
            assert_eq!(list()[1 + 4 * fill + 3], psx_hw::gpu::pack_xy(16, 8));
        }
        assert_eq!(list()[words], psx_hw::gpu::gp0::REQUEST_IRQ);
        assert_eq!(list()[0] >> 24, words as u32);
        set_draw_done_scratch(1008, 504);
        assert_eq!(&list()[1..], &psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE);
    }

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
