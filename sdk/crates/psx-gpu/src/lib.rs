// SPDX-License-Identifier: GPL-2.0-or-later
//! The PS1 GPU: a driver that owns it, the packets it draws, and safe
//! frame building over its linked-list DMA.
//!
//! # Layers
//!
//! - [`Gpu`] owns GP0, GP1 and DMA channel 2 through the
//!   [`GpuDma`](psx_io::periph::GpuDma) token. Every port write is a method,
//!   so the borrow checker keeps immediate drawing out of a running walk.
//! - [`prim`] holds the packets: `repr(C)` structs whose words are the GP0
//!   wire format. [`Gpu::draw`] sends one now; an [`ot::OrderingTable`]
//!   frame links many for one DMA walk.
//! - [`frame`] builds frames with lifetimes instead of `unsafe`: a packet
//!   added to an [`frame::OtFrame`] stays borrowed until the walk that reads
//!   it has finished.
//! - [`ordered`] streams packets in painter order over static storage.
//! - [`display`] describes what the GPU shows; [`material`] describes how
//!   textured packets sample and blend.
//! - The raw layer, [`chain::submit_async_raw`] and the `*_unchecked`
//!   adds on [`frame::OtFrame`], is `unsafe`: it hands the DMA controller
//!   addresses the type system cannot follow.
//!
//! Register addresses and command-word encoders live in `psx-hw`, shared
//! with the emulator's GPU, so the two cannot disagree on a layout.

#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

#[cfg(all(target_arch = "mips", feature = "asm-kernels"))]
mod asm_kernels;
pub mod chain;
mod compat;
pub mod display;
pub mod frame;
pub mod framebuf;
mod gpu;
pub mod material;
pub mod ordered;
pub mod ot;
pub mod prim;

#[allow(deprecated, reason = "the forwarders kept for one stage")]
pub use compat::{
    arm_draw_done, draw_line_mono, draw_quad_flat, draw_quad_textured,
    draw_quad_textured_gouraud_material, draw_quad_textured_material, draw_rect_flat,
    draw_sprite_material, draw_sync, draw_tri_flat, draw_tri_flat_blended, draw_tri_gouraud,
    fill_rect, init, set_draw_area, set_draw_offset, signal_draw_done, submit_linked_list_async,
    submit_linked_list_async_raw, submit_linked_list_raw, submit_linked_list_raw_async,
    submit_linked_list_wait, vsync, wait_idle,
};
// The chain items' old root paths, kept for one stage: they are the same
// items, so they cannot carry a deprecation of their own.
pub use chain::{DrawDoneNode, StaticChain, StaticPacket, DRAW_DONE_NODE, MAX_NODE_WORDS};
pub use gpu::{Gpu, MaskMode};

use psx_io::gpu::wait_command_ready;

/// Moved to [`display::VideoMode`].
#[deprecated(note = "moved to `psx_gpu::display::VideoMode`")]
pub type VideoMode = display::VideoMode;

/// Moved to [`display::Resolution`].
#[deprecated(note = "moved to `psx_gpu::display::Resolution`")]
pub type Resolution = display::Resolution;

/// [`Gpu::wait_idle`]'s waits, shared with the deprecated free function.
///
/// Waits for DMA channel 2 to finish its walk, then for GPUSTAT bit 28
/// (ready for a DMA block), then for bit 26 (ready for a command word).
/// Bit 28 alone is not a drawing-complete test: on silicon it rises when
/// the walk has pushed its last packet, about one large primitive before
/// the drawing ends. Hardware-tests v1.24 cases 219-226 put bit 28's final
/// rise at the channel's completion (586,354 and 275,124 clocks on the two
/// large-triangle lists) and bit 26's at the list's closing GP0(1Fh)
/// (625,348 and 314,075). PSn00bSDK's `DrawSync` waits the same way.
///
/// Every wait is bounded, with the recovery of [`chain::wait`]
/// and `psx_io::gpu::wait_command_ready`, so a wedged GPU costs a reset
/// instead of a hang.
#[inline]
pub(crate) fn wait_idle_impl() {
    // A frame handed to psx-rt's present queue is still "sent to the GPU":
    // the armed guard waits until it has been kicked and drawn. Without the
    // `present-queue` feature this is nothing.
    psx_io::gpu::run_direct_access_guard();
    chain::wait_walk();
    psx_io::gpu::wait_dma_ready();
    wait_command_ready();
}

/// True once the GPU has executed the GP0(1Fh) that closes the work kicked
/// after the last [`Gpu::arm_draw_done`], so everything before it is drawn.
///
/// This is the completion test psx-rt's queued display flip
/// (`psx_rt::interrupts::queue_display_control_at_vblank`) applies at each VBlank edge.
/// GP0(1Fh) raises GPUSTAT bit 24 only when the GPU reaches it in its
/// command stream, after the drawing before it; the flag stays set until
/// GP1(02h). The v1.24 present-queue probe flipped on this flag with 120 of
/// 120 frames complete on a console. End a DMA chain with it through
/// [`ot::OrderingTable::end_with_draw_done`] or [`chain::DRAW_DONE_NODE`], an
/// ordered stream with `push_packet([gp0::REQUEST_IRQ])`, and immediate
/// drawing with [`Gpu::signal_draw_done`].
///
/// Arm the flag right before kicking a frame whose last command is GP0(1Fh),
/// and only once the previous frame's queued flip has been applied:
/// acknowledging earlier hides the previous frame's completion from psx-rt's
/// VBlank handler.
///
/// GP0(1Fh) also raises interrupt source 1 (GPU) in `I_STAT`; keep it masked
/// in `I_MASK`, since psx-rt's handler does not acknowledge it.
///
/// It only reads GPUSTAT, so it needs no [`Gpu`].
#[inline]
pub fn is_draw_done() -> bool {
    psx_io::gpu::status().contains(psx_hw::gpu::GpuStat::IRQ1)
}

/// How often the SDK recovered a hung GPU since boot, for a debug overlay
/// or a test log. Every GPU wait is bounded and recovers by resetting the
/// GPU; these counts make the recoveries visible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RecoveryStats {
    /// Linked-list walks aborted because they outlived their spin budget.
    pub dma_aborts: u32,
    /// Command-buffer resets after a ready bit stayed low.
    pub command_resets: u32,
}

/// The recovery counts since boot. Reads only, so it needs no [`Gpu`].
pub fn recovery_stats() -> RecoveryStats {
    RecoveryStats {
        dma_aborts: chain::dma_abort_count(),
        command_resets: psx_io::gpu::command_reset_count(),
    }
}
