//! VBlank-kicked presentation: a one-frame-deep queue between the CPU and the GPU.
//!
//! A game publishes each finished frame's DMA chain together with the GP1(05h)
//! word that shows the frame before it, and returns at once. psx-rt's VBlank
//! handler kicks the chain on the first edge where the previous chain's
//! closing GP0(1Fh) has run (GPUSTAT bit 24) and DMA channel 2 is idle: it
//! flips the display to the finished frame, acknowledges the flag with
//! GP1(02h), and starts the new walk. A flip therefore lands only on a blank
//! edge and only on a fully drawn frame, and the CPU builds the next frame
//! while the GPU draws this one. The CPU waits only when it is a whole frame
//! ahead, and before reusing memory a walk may still read.
//!
//! Ported from quake-psx's `platform.rs` (3ca915c, 16e27a9: E1M1 chain bench
//! +25.5%, monster route +46.1% over its blocking present) so every game
//! shares one queue.
//!
//! # Protocol
//!
//! 1. Call [`start`] once, after `interrupts::install_vblank_counter`, with
//!    nothing queued and channel 2 idle. It raises the draw-done flag so the
//!    first published frame finds it set.
//! 2. Every published chain must end on GP0(1Fh) (`psx_gpu::DRAW_DONE_NODE`,
//!    `OrderingTable::end_with_draw_done`), and must carry its own draw area,
//!    offset and clear as leading packets: the handler writes only GP1.
//! 3. Before [`publish`], call [`wait_slot_empty`]. Before rebuilding memory
//!    the frame before the last published one used, call [`wait_arena_free`].
//! 4. Direct GP0 or GP1 writes and GPU DMA setup are safe at any time:
//!    [`publish`] arms psx-io's direct-access guard, so the first such access
//!    after it runs [`quiesce`]. Code that only records (psx-io's GP0
//!    capture) keeps the overlap.
//!
//! Keep interrupt source 1 (GPU) masked in `I_MASK`: GP0(1Fh) raises it and
//! the handler does not acknowledge it. The handler returns past a GTE
//! command an edge interrupted (8055e87f6), so publishing from GTE-heavy code
//! is safe.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::interrupts::{__psx_rt_present_head, __psx_rt_present_kicks, __psx_rt_present_skips};

/// Edges a published frame may wait before the CPU treats the chain ahead of
/// it as stalled (its walk wedged, or it lost its GP0(1Fh)).
pub const STALL_VBLANKS: u32 = 8;

static mut RECOVERIES: u32 = 0;

/// Counters for telemetry and A/B reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentStats {
    /// Chains the handler has kicked.
    pub kicks: u32,
    /// Edges on which a published chain had to wait for the one before it.
    pub skips: u32,
    /// Stalled chains the CPU stopped by hand (see [`STALL_VBLANKS`]).
    pub recoveries: u32,
}

/// Raise the draw-done flag once, so the first published frame's edge finds
/// it set. Nothing may be queued and channel 2 must be idle.
pub fn start() {
    // Any direct GPU access while a published frame may walk waits it out.
    psx_io::gpu::set_direct_access_guard(quiesce);
    #[cfg(target_arch = "mips")]
    {
        psx_io::gpu::wait_cmd_ready();
        psx_io::gpu::write_gp0(psx_hw::gpu::gp0::REQUEST_IRQ);
    }
}

/// True while a published chain is waiting for the handler to kick it.
#[inline]
pub fn slot_full() -> bool {
    unsafe { read_volatile(addr_of!(__psx_rt_present_head)) != 0 }
}

#[inline]
fn channel_busy() -> bool {
    #[cfg(target_arch = "mips")]
    return psx_io::dma::is_busy(psx_io::dma::Channel::Gpu);
    #[cfg(not(target_arch = "mips"))]
    false
}

#[inline]
fn draw_done() -> bool {
    psx_io::gpu::gpustat().contains(psx_hw::gpu::GpuStat::IRQ1)
}

/// Hand one chain, and the GP1(05h) word that shows the frame before it, to
/// the VBlank handler. Pass `display` 0 when nothing is on screen yet.
///
/// # Safety
///
/// The slot must be empty ([`wait_slot_empty`]). `head` must be the first
/// node of a linked-list chain ending on GP0(1Fh), and the chain and every
/// packet it links must stay live and unmodified until its walk finishes.
#[inline]
pub unsafe fn publish(head: *const u32, display: u32) {
    debug_assert!(!slot_full());
    #[cfg(target_arch = "mips")]
    unsafe {
        // Keep the chain's packet stores ahead of the slot write the handler
        // reads. Empty asm is a compiler barrier and emits nothing.
        core::arch::asm!("", options(nostack, preserves_flags));
        write_volatile(
            addr_of_mut!(crate::interrupts::__psx_rt_present_display),
            display,
        );
        write_volatile(addr_of_mut!(__psx_rt_present_head), head as u32);
    }
    psx_io::gpu::arm_direct_access_guard();
    #[cfg(not(target_arch = "mips"))]
    let _ = (head, display);
}

/// Block until the handler has kicked the published frame. Spins only when
/// the CPU is a whole frame ahead of presentation. Not inlined, so a profile
/// can tell the spin from work.
#[inline(never)]
pub fn wait_slot_empty() {
    let start = crate::interrupts::vblank_count();
    while slot_full() {
        if crate::interrupts::vblank_count().wrapping_sub(start) >= STALL_VBLANKS {
            release_stalled_slot();
        }
    }
}

/// Wait until memory used by the frame before the most recently published
/// one is no longer walked. Once the published frame has been kicked (slot
/// empty), the handler saw the older frame's GP0(1Fh), so its walk had
/// ended; while it is still queued, the only walk that can be running is the
/// older frame's.
#[inline(never)]
pub fn wait_arena_free() {
    let start = crate::interrupts::vblank_count();
    while slot_full() && channel_busy() {
        if crate::interrupts::vblank_count().wrapping_sub(start) >= STALL_VBLANKS {
            release_stalled_slot();
        }
    }
    // Keep the rebuild's stores after the completion read.
    #[cfg(target_arch = "mips")]
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
}

/// Wait until the GPU is completely idle: nothing queued, no walk running and
/// the last chain's drawing done. Required before writing the GP0 or GP1
/// ports directly (an image upload, say) while the queue is in use.
#[inline(never)]
pub fn quiesce() {
    wait_slot_empty();
    let start = crate::interrupts::vblank_count();
    while channel_busy() || !draw_done() {
        if crate::interrupts::vblank_count().wrapping_sub(start) >= STALL_VBLANKS {
            stop_walk_and_raise();
            break;
        }
    }
}

/// The published frame has waited [`STALL_VBLANKS`] edges for the chain
/// ahead of it. With the VBlank IRQ masked, so the handler cannot kick in
/// between, stop a walk still running the way `psx_gpu::submit_linked_list_wait`
/// recovers, then raise the flag through the port so the next edge kicks the
/// queued frame.
#[cold]
#[inline(never)]
fn release_stalled_slot() {
    let mask = psx_io::irq::mask();
    psx_io::irq::set_mask(0);
    if slot_full() && !draw_done() {
        stop_walk_and_raise();
    }
    psx_io::irq::set_mask(mask);
}

/// Port writes bypass the direct-access guard: this runs inside it
/// ([`quiesce`]), and from the paired-arena fence, whose scratchpad stack
/// `stack_guard` must bound without the guard's function pointer.
#[cold]
fn stop_walk_and_raise() {
    #[cfg(target_arch = "mips")]
    {
        use psx_io::dma::{self, Channel};
        if dma::is_busy(Channel::Gpu) && !dma::wait_done(Channel::Gpu, dma::DEFAULT_DMA_SPINS) {
            dma::abort(Channel::Gpu);
            psx_io::gpu::write_gp1_unguarded(0x0100_0000);
        }
        // `wait_cmd_ready`, with its timeout reset written past the guard.
        if !psx_io::gpu::try_wait_cmd_ready(psx_io::gpu::READY_SPINS) {
            psx_io::gpu::write_gp1_unguarded(0x0100_0000);
        }
        psx_io::gpu::write_gp0_unguarded(psx_hw::gpu::gp0::REQUEST_IRQ);
    }
    unsafe {
        write_volatile(
            addr_of_mut!(RECOVERIES),
            read_volatile(addr_of!(RECOVERIES)).wrapping_add(1),
        );
    }
}

/// Kick, skip and recovery counts since boot.
pub fn stats() -> PresentStats {
    unsafe {
        PresentStats {
            kicks: read_volatile(addr_of!(__psx_rt_present_kicks)),
            skips: read_volatile(addr_of!(__psx_rt_present_skips)),
            recoveries: read_volatile(addr_of!(RECOVERIES)),
        }
    }
}
