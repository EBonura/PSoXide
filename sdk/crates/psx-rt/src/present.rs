//! VBlank-kicked presentation: a one-frame-deep queue between the CPU and the
//! GPU. Built with the `present-queue` feature.
//!
//! A game publishes each finished frame's DMA chain together with the
//! GP1(05h) word that shows the frame before it, and returns at once.
//! psx-rt's VBlank handler kicks the chain on the first edge where the
//! previous chain's closing GP0(1Fh) has run (GPUSTAT bit 24) and DMA channel
//! 2 is idle: it flips the display to the finished frame, acknowledges the
//! flag with GP1(02h), and starts the new walk. A flip therefore lands only on
//! a blank edge and only on a fully drawn frame, and the CPU builds the next
//! frame while the GPU draws this one. The CPU waits only when it is a whole
//! frame ahead, and before reusing memory a walk may still read.
//!
//! Ported from quake-psx's `platform.rs` (3ca915c, 16e27a9: E1M1 chain bench
//! +25.5%, monster route +46.1% over its blocking present) so every game
//! shares one queue.
//!
//! # Protocol
//!
//! 1. Call [`start`] once, after [`crate::interrupts::install_vblank_counter`],
//!    with nothing queued and channel 2 idle. It raises the draw-done flag so
//!    the first published frame finds it set.
//! 2. Every published chain must end on GP0(1Fh) (`psx_gpu::DRAW_DONE_NODE`,
//!    `OrderingTable::end_with_draw_done`), and must carry its own draw
//!    area, offset and clear as leading packets: the handler writes only
//!    GP1. `psx_io::gpu::start_recording_raw` records those from the usual
//!    immediate calls.
//! 3. Before [`publish_raw`], call [`wait_slot_empty`]. Before rebuilding
//!    memory that the frame before the last published one used, call
//!    [`wait_arena_free`].
//! 4. Direct command or display-control writes and GPU DMA starts are safe
//!    at any time: [`publish_raw`] arms psx-io's direct-access guard, so the
//!    first such access after it runs [`wait_idle`]. Code that only records
//!    keeps the overlap.
//!
//! Keep interrupt source 1 (GPU) masked in `I_MASK`: GP0(1Fh) raises it and
//! the handler, which owns VBlank only, would acknowledge it as a stray. The handler returns past a GTE
//! command an edge interrupted (8055e87f6), so publishing from GTE-heavy code
//! is safe.
//!
//! # With `psx_gpu::frame`
//!
//! A frame kicked from the CPU (`psx_gpu::frame::OtFrame::submit`,
//! `FrameStorage::draw_async`, or any other GPU DMA start) first runs the
//! guard, so it starts only once every published frame has been kicked and
//! has finished drawing: the channel is never kicked twice, and no queued
//! walk reads memory the CPU path is about to reuse. The reverse also holds:
//! the handler never kicks while a CPU-kicked walk runs, since it needs
//! channel 2 idle. A queued flip waits on GPUSTAT bit 24, which only chains
//! ending in GP0(1Fh) raise, so a CPU-kicked chain should end the same way
//! when a published frame follows it.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::interrupts::{
    __psx_rt_present_head, __psx_rt_present_kick_count, __psx_rt_present_skip_count,
};
use crate::wait;

/// Edges a published frame may wait before the CPU treats the chain ahead of
/// it as stalled (its walk wedged, or it lost its GP0(1Fh)).
pub const STALL_VBLANKS: u32 = 8;

static mut RECOVERY_COUNT: u32 = 0;

/// Counters for telemetry and A/B reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentStats {
    /// Chains the handler has kicked.
    pub kick_count: u32,
    /// Edges on which a published chain had to wait for the one before it.
    pub skip_count: u32,
    /// Stalled chains the CPU stopped by hand (see [`STALL_VBLANKS`]).
    pub recovery_count: u32,
}

/// Register the direct-access guard and raise the draw-done flag once, so
/// the first published frame's edge finds it set. Nothing may be queued and
/// channel 2 must be idle.
pub fn start() {
    psx_io::gpu::set_direct_access_guard(wait_idle);
    #[cfg(target_arch = "mips")]
    {
        psx_io::gpu::wait_command_ready();
        psx_io::gpu::write_command(psx_hw::gpu::gp0::REQUEST_IRQ);
    }
}

/// True while a published chain is waiting for the handler to kick it.
#[inline]
pub fn is_slot_full() -> bool {
    // SAFETY: a volatile aligned u32 read through a raw pointer; the VBlank
    // handler is the only other accessor and an aligned word load cannot
    // tear.
    unsafe { read_volatile(addr_of!(__psx_rt_present_head)) != 0 }
}

#[inline]
fn is_channel_busy() -> bool {
    #[cfg(target_arch = "mips")]
    return psx_io::dma::is_busy(psx_io::dma::Channel::Gpu);
    #[cfg(not(target_arch = "mips"))]
    false
}

#[inline]
fn is_draw_done() -> bool {
    psx_io::gpu::status().contains(psx_hw::gpu::GpuStat::IRQ1)
}

/// Hand one chain, and the GP1(05h) word that shows the frame before it, to
/// the VBlank handler. Pass `display` 0 when nothing is on screen yet.
///
/// # Safety
///
/// The slot must be empty ([`wait_slot_empty`]). `head` must be the first
/// node tag of a linked-list chain ending on GP0(1Fh), every node at most
/// 16 payload words, and the chain and every packet it links must stay live
/// and unmodified until its walk has finished ([`wait_arena_free`] after
/// the next publish, or [`wait_idle`]).
#[inline]
pub unsafe fn publish_raw(head: *const u32, display: u32) {
    debug_assert!(!is_slot_full());
    #[cfg(target_arch = "mips")]
    {
        // Keep the chain's packet stores ahead of the slot write the handler
        // reads.
        compiler_barrier();
        // SAFETY: volatile aligned u32 stores through raw pointers. The slot
        // is empty (this fn's `# Safety`), so the handler ignores both words
        // until the head store, which comes last.
        unsafe {
            write_volatile(
                addr_of_mut!(crate::interrupts::__psx_rt_present_display),
                display,
            );
            write_volatile(addr_of_mut!(__psx_rt_present_head), head as u32);
        }
    }
    #[cfg(not(target_arch = "mips"))]
    let _ = (head, display);
    psx_io::gpu::arm_direct_access_guard();
}

/// Block until the handler has kicked the published frame. Spins only when
/// the CPU is a whole frame ahead of presentation. Not inlined, so a profile
/// can tell the spin from work.
///
/// Bounded: with no psx-rt handler running (the counter never installed, or
/// interrupts masked) nothing kicks the frame and the VBlank stall test never
/// fires, so the wait gives up after a few million reads and asserts in a
/// debug build.
#[inline(never)]
pub fn wait_slot_empty() {
    let start = crate::interrupts::vblank_count();
    let done = wait::wait_while(
        is_slot_full,
        || is_stalled(start),
        release_stalled_slot,
        wait::SPIN_LIMIT,
    );
    debug_assert!(
        done,
        "present: the slot never emptied; is the VBlank handler installed?"
    );
}

/// Wait until memory used by the frame before the most recently published
/// one is no longer walked. Once the published frame has been kicked (slot
/// empty), the handler saw the older frame's GP0(1Fh), so its walk had
/// ended; while it is still queued, the only walk that can be running is the
/// older frame's. Bounded like [`wait_slot_empty`].
#[inline(never)]
pub fn wait_arena_free() {
    let start = crate::interrupts::vblank_count();
    let done = wait::wait_while(
        || is_slot_full() && is_channel_busy(),
        || is_stalled(start),
        release_stalled_slot,
        wait::SPIN_LIMIT,
    );
    debug_assert!(
        done,
        "present: the arena never freed; is the VBlank handler installed?"
    );
    // Keep the rebuild's stores after the completion read.
    compiler_barrier();
}

/// Wait until the GPU is completely idle: nothing queued, no walk running and
/// the last chain's drawing done. The direct-access guard runs this before
/// the first direct GPU access after a publish. Bounded like
/// [`wait_slot_empty`]; a walk it gives up on is stopped by hand.
#[doc(alias = "DrawSync")]
#[inline(never)]
pub fn wait_idle() {
    wait_slot_empty();
    let start = crate::interrupts::vblank_count();
    let mut spins = 0u32;
    while is_channel_busy() || !is_draw_done() {
        spins += 1;
        if is_stalled(start) || spins >= wait::SPIN_LIMIT {
            stop_walk_and_raise();
            break;
        }
    }
    compiler_barrier();
}

/// True once [`STALL_VBLANKS`] edges have passed since `start`.
fn is_stalled(start: u32) -> bool {
    crate::interrupts::vblank_count().wrapping_sub(start) >= STALL_VBLANKS
}

/// The published frame has waited [`STALL_VBLANKS`] edges for the chain
/// ahead of it. With interrupts masked, so the handler cannot kick in
/// between, stop a walk still running the way
/// `psx_gpu::submit_linked_list_wait` recovers, then raise the flag through
/// the port so the next edge kicks the queued frame.
#[cold]
#[inline(never)]
fn release_stalled_slot() {
    crate::critical_section::with(|_| {
        if is_slot_full() && !is_draw_done() {
            stop_walk_and_raise();
        }
    });
}

/// Port writes bypass the direct-access guard: this runs inside it
/// ([`wait_idle`]), and from the paired-arena fence, whose scratchpad stack
/// stack-guard must bound without the guard's function pointer.
#[cold]
fn stop_walk_and_raise() {
    #[cfg(target_arch = "mips")]
    {
        use psx_io::dma::{self, Channel};
        if dma::is_busy(Channel::Gpu) && !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
            dma::abort(Channel::Gpu);
            psx_io::gpu::write_display_control_unguarded(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
        }
        // `wait_command_ready`, with its timeout reset written past the guard.
        if !psx_io::gpu::try_wait_command_ready(psx_io::gpu::READY_SPINS) {
            psx_io::gpu::write_display_control_unguarded(psx_hw::gpu::gp1::RESET_CMD_BUFFER);
        }
        psx_io::gpu::write_command_unguarded(psx_hw::gpu::gp0::REQUEST_IRQ);
    }
    // SAFETY: volatile aligned u32 accesses to this module's private static;
    // the program is single threaded and no handler touches it.
    unsafe {
        write_volatile(
            addr_of_mut!(RECOVERY_COUNT),
            read_volatile(addr_of!(RECOVERY_COUNT)).wrapping_add(1),
        );
    }
}

/// Kick, skip and recovery counts since boot.
pub fn stats() -> PresentStats {
    // SAFETY: volatile aligned u32 reads through raw pointers; the handler
    // writes the first two with single word stores, which cannot tear.
    unsafe {
        PresentStats {
            kick_count: read_volatile(addr_of!(__psx_rt_present_kick_count)),
            skip_count: read_volatile(addr_of!(__psx_rt_present_skip_count)),
            recovery_count: read_volatile(addr_of!(RECOVERY_COUNT)),
        }
    }
}

/// Compiler-only barrier (see `psx_io::dma`'s): the pinned MIPS-I backend
/// lowers a compiler fence to `SYNC`, which the R3000 lacks, so the target
/// uses an empty `asm!` with its default memory clobber.
#[inline(always)]
fn compiler_barrier() {
    #[cfg(target_arch = "mips")]
    // SAFETY: an empty asm block; it only constrains compiler ordering.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "mips"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
