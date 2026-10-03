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
//! - The raw layer, [`submit_linked_list_async_raw`] and the `*_unchecked`
//!   adds on [`frame::OtFrame`], is `unsafe`: it hands the DMA controller
//!   addresses the type system cannot follow.
//!
//! Register addresses and command-word encoders live in `psx-hw`, shared
//! with the emulator's GPU, so the two cannot disagree on a layout.

#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

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
    arm_draw_done, draw_line_gouraud, draw_line_mono, draw_line_mono_blended, draw_quad_flat,
    draw_quad_textured, draw_quad_textured_gouraud, draw_quad_textured_gouraud_material,
    draw_quad_textured_material, draw_rect_flat, draw_sprite_material, draw_sync, draw_tri_flat,
    draw_tri_flat_blended, draw_tri_gouraud, draw_tri_gouraud_blended, draw_tri_textured_material,
    fill_rect, init, set_display_offset, set_draw_area, set_draw_offset, set_mask_mode,
    set_screen_h_offset, set_screen_v_offset, set_texture_page, signal_draw_done, submit_static,
    wait_idle,
};
pub use gpu::{Gpu, MaskMode};

use psx_hw::gpu::{gp0, gp1, DmaDirection};
use psx_io::dma::{self, Channel};
use psx_io::gpu::{wait_command_ready, write_display_control};
use psx_io::timers;

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
/// Every wait is bounded, with the recovery of [`submit_linked_list_wait`]
/// and `psx_io::gpu::wait_command_ready`, so a wedged GPU costs a reset
/// instead of a hang.
#[inline]
pub(crate) fn wait_idle_impl() {
    submit_linked_list_wait();
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
/// [`ot::OrderingTable::end_with_draw_done`] or [`DRAW_DONE_NODE`], an
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

/// Renamed to [`is_draw_done`].
#[deprecated(note = "renamed to `is_draw_done`")]
#[inline(always)]
pub fn draw_done() -> bool {
    is_draw_done()
}

/// A linked-list DMA node holding only GP0(1Fh), for the end of a chain.
///
/// Link it as the chain's last node and the GPU raises [`is_draw_done`] when
/// it gets there. It is immutable and shared: every chain can end on
/// [`DRAW_DONE_NODE`].
#[repr(C, align(4))]
pub struct DrawDoneNode([u32; 2]);

impl DrawDoneNode {
    /// The node's tag word, the address a chain links to.
    #[inline]
    pub fn as_ptr(&self) -> *const u32 {
        self.0.as_ptr()
    }
}

/// The shared GP0(1Fh) node: one payload word, then the end of the list.
pub static DRAW_DONE_NODE: DrawDoneNode = DrawDoneNode([(1 << 24) | 0x00FF_FFFF, gp0::REQUEST_IRQ]);

/// Configure Timer 1 as an HBlank-counting scanline counter.
///
/// WARNING: writing a timer's mode register resets its counter, so every
/// call restarts the count from zero. That is why the helpers below cannot
/// observe the real display position: they reconfigure before reading.
#[inline]
pub fn configure_scanline_timer() {
    // Mode: bit0=sync enable, bits1-2=01 (reset at VBlank), bit8=1
    // (clock source = HBlank).
    timers::set_mode(timers::Timer::Timer1, 0x0103);
}

/// Renamed to [`configure_scanline_timer`]: it programs Timer 1 to count
/// HBlanks and has nothing to do with vertical sync.
#[deprecated(note = "renamed to `configure_scanline_timer`")]
#[inline(always)]
pub fn configure_vsync_timer() {
    configure_scanline_timer()
}

/// Timer-1 scanline counter used by the VBlank wait helpers.
#[deprecated(
    note = "reconfigures Timer 1 before reading, which resets the counter, \
            so this returns ~0 rather than the display scanline; use \
            psx_rt::interrupts for display timing"
)]
#[inline]
pub fn scanline_counter() -> u16 {
    configure_scanline_timer();
    timers::counter(timers::Timer::Timer1)
}

/// Whether Timer 1 currently reports the VBlank scanline region.
#[deprecated(note = "built on scanline_counter(), whose reconfigure-before-read \
            resets the counter, so this is almost always false; use \
            psx_rt::interrupts for display timing")]
#[allow(deprecated)]
#[inline]
pub fn in_vblank() -> bool {
    scanline_counter() >= 242
}

/// Wait 242 HBlank periods (~15.4ms) from the moment of the call.
///
/// Despite the name, this does NOT sync to the display: reconfiguring
/// Timer 1 resets its counter, so the wait starts from zero at the call
/// site. Frame time becomes `work + 15.4ms` instead of snapping to the
/// next VBlank -- nearly right for light frames, badly slow for heavy
/// ones. It cannot be repaired here: syncing needs the VBlank IRQ, which
/// the runtime owns.
#[deprecated(note = "busy-waits a fixed 242 HBlanks from the call site instead of \
            syncing to the display; use psx_rt::interrupts::wait_vblank()")]
pub fn vsync() {
    configure_scanline_timer();
    while timers::counter(timers::Timer::Timer1) < 242 {}
}

/// Texture color depth passed to [`Gpu::set_texture_page`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TextureDepth {
    /// 4-bit CLUT-indexed.
    Bit4 = 0,
    /// 8-bit CLUT-indexed.
    Bit8 = 1,
    /// 15-bit direct color.
    Bit15 = 2,
}

/// Most payload words one linked-list DMA node may carry (the words after
/// its tag), the depth of the GPU's command FIFO.
///
/// Silicon very likely loses words from longer nodes while it draws.
/// Hardware-tests v1.24 drew the same 16 half-screen Gouraud triangles as
/// 16 nodes and as 4 nodes of 24 words: the packed list's closing GP0(1Fh)
/// arrived at 314,075 clocks against 625,348, about 8 triangles' worth at
/// the 39,084 clocks each costs, and its last-node drain matched the
/// unpacked list's one-triangle gap, so roughly half its drawing never
/// happened. Every SDK node builder stays at or under this limit:
/// [`ordered::OrderedCommandStream`] caps nodes at
/// [`ordered::NODE_PAYLOAD_WORDS`], [`ot::OrderingTable::insert`] refuses
/// longer packets, and every [`prim`] packet is shorter.
pub const MAX_NODE_WORDS: usize = 16;

/// Kick a linked-list chain to GPU GP0 via DMA channel 2 in
/// linked-list mode **without** waiting for the walk to finish.
///
/// Returns as soon as the DMA transfer is started, so the CPU can do
/// other work (build the next frame, run a sim tick) while the GPU
/// rasterises this one. A walk already running on the channel is waited
/// out first (or aborted if it wedged).
///
/// This is the unchecked layer under [`frame::OtFrame`], [`frame::FrameStorage`]
/// and [`submit_static`], which prove the contract below with lifetimes.
///
/// # Safety
///
/// `head` must point at a 4-byte-aligned node tag in RAM. Each tag holds
/// the next node's address in bits 23..=0 (`0x00FF_FFFF` ends the list) and
/// its payload word count, at most [`MAX_NODE_WORDS`], in bits 31..=24.
/// Every node reachable from `head`, and its payload, must stay live and
/// unmodified until [`submit_linked_list_wait`] returns (or a later kick,
/// which waits for this walk first).
#[doc(alias = "DrawOTag")]
pub unsafe fn submit_linked_list_async_raw(head: *const u32) {
    // A completed DMA walk does not imply that the GPU has finished
    // rasterising the commands it consumed. Do not call `wait_idle()` here:
    // channel 2's request handshake can queue the next list behind that work,
    // which is how PsyQ/PSn00bSDK keep the GPU fed. Only the DMA channel and
    // the list's backing storage must be free before starting another walk.
    //
    // Bounded: a wedged channel (see `dma::abort`) would otherwise hang
    // the frame loop forever. Aborting costs at most the tail of a walk
    // that was never going to finish.
    if !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
        dma::abort(Channel::Gpu);
        // The walker stopped mid-packet, so the GPU is still waiting for
        // the rest of a command. Discard it or every later ready-wait
        // blocks on a GPU that can never become ready.
        write_display_control(gp1::RESET_CMD_BUFFER);
    }

    // Make sure the GPU's DMA direction is CPU→GP0 before we kick off the
    // walker. `gpu::init` sets this, but games occasionally re-route DMA for
    // VRAM readback and forget to reset it.
    write_display_control(gp1::dma_direction(DmaDirection::CpuToGp0 as u32));
    dma::enable_channel(Channel::Gpu);
    // SAFETY: the channel is idle (waited out or aborted above); the caller
    // keeps the chain live and unmodified until the walk is waited out.
    // `dma::start` publishes the payload and tag stores before the CHCR
    // store.
    unsafe {
        dma::start(
            Channel::Gpu,
            dma::Transfer {
                address: head.expose_provenance() as u32,
                // BCR is ignored in linked-list mode but must be written to
                // some value on real hardware; zero is conventional.
                size: dma::size_words(0),
                control: psx_hw::dma::CHCR_TO_DEVICE
                    | psx_hw::dma::CHCR_SYNC_LINKED
                    | psx_hw::dma::CHCR_START,
            },
        )
    };
}

/// Renamed to [`submit_linked_list_async_raw`].
///
/// # Safety
/// See [`submit_linked_list_async_raw`].
#[deprecated(note = "renamed to `submit_linked_list_async_raw`")]
#[inline(always)]
pub unsafe fn submit_linked_list_raw_async(head: *const u32) {
    // SAFETY: same contract as the renamed function.
    unsafe { submit_linked_list_async_raw(head) }
}

/// Old name of [`submit_linked_list_async_raw`].
///
/// # Safety
///
/// As [`submit_linked_list_async_raw`].
#[deprecated(
    note = "takes an unchecked pointer; use `OrderingTable::frame`, `submit_static`, or the unsafe `submit_linked_list_async_raw`"
)]
pub unsafe fn submit_linked_list_async(head: *const u32) {
    // SAFETY: forwarded contract.
    unsafe { submit_linked_list_async_raw(head) }
}

/// Block until the GPU-DMA linked-list walk kicked by
/// [`submit_linked_list_async`] has drained the whole chain. This is
/// the CPU-blocked-on-GPU portion of an ordering-table submission;
/// profiling code times it separately from the kick to split GPU-draw
/// cost from CPU build cost.
#[inline]
pub fn submit_linked_list_wait() {
    if !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
        dma::abort(Channel::Gpu);
        // Past the direct-access guard (`present-queue` feature): the guard
        // would wait on the walk that just wedged, and the paired-arena
        // fence reaches this from a scratchpad stack that stack-guard bounds
        // only through direct calls.
        psx_io::gpu::write_display_control_unguarded(gp1::RESET_CMD_BUFFER);
    }
    // Keep the caller's buffer-reuse stores after the completion read (or
    // the abort).
    dma::compiler_barrier();
}

/// Submit a linked-list chain starting at `head` to GPU GP0 via
/// DMA channel 2 in linked-list mode. Blocks until the walker hits
/// the `0x00FFFFFF` terminator (or aborts a wedged walk).
///
/// This is [`submit_linked_list_async_raw`] immediately followed by
/// [`submit_linked_list_wait`].
///
/// # Safety
///
/// As [`submit_linked_list_async_raw`], for the duration of this call.
#[doc(alias = "DrawOTag")]
pub unsafe fn submit_linked_list_raw(head: *const u32) {
    // SAFETY: forwarded contract; the wait below ends the walk before return.
    unsafe { submit_linked_list_async_raw(head) };
    submit_linked_list_wait();
}

/// Old name of [`submit_linked_list_raw`].
///
/// # Safety
///
/// As [`submit_linked_list_raw`].
#[deprecated(
    note = "takes an unchecked pointer; use `OrderingTable::frame`, `submit_static`, or the unsafe `submit_linked_list_raw`"
)]
pub unsafe fn submit_linked_list(head: *const u32) {
    // SAFETY: forwarded contract.
    unsafe { submit_linked_list_raw(head) }
}

/// One immutable linked-list node: `W` GP0 words, then the end of the list.
///
/// Built in a `static`, it is a chain that stays valid for the whole run, so
/// [`submit_static`] can kick it from safe code without waiting.
///
/// ```
/// use psx_gpu::StaticPacket;
/// // GP0(E1h) draw mode, then GP0(1Fh).
/// static MODE_THEN_IRQ: StaticPacket<2> = StaticPacket::new([0xE100_0000, 0x1F00_0000]);
/// assert_eq!(MODE_THEN_IRQ.words(), &[0xE100_0000, 0x1F00_0000]);
/// ```
#[repr(C, align(4))]
pub struct StaticPacket<const W: usize> {
    tag: u32,
    words: [u32; W],
}

impl<const W: usize> StaticPacket<W> {
    /// A node carrying `words` that ends the list.
    pub const fn new(words: [u32; W]) -> Self {
        const { assert!(W <= MAX_NODE_WORDS, "packet longer than one GPU DMA node") };
        Self {
            tag: ((W as u32) << 24) | 0x00FF_FFFF,
            words,
        }
    }

    /// The payload words.
    pub const fn words(&self) -> &[u32; W] {
        &self.words
    }

    /// The node's tag word, the address a chain links to.
    #[inline]
    pub fn as_ptr(&self) -> *const u32 {
        &self.tag
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A complete, immutable linked list: [`StaticPacket`] or [`DrawDoneNode`].
///
/// Sealed: implementors guarantee that a shared reference to them is a
/// whole chain whose nodes never change while the reference lives.
pub trait StaticChain: sealed::Sealed {
    /// Address of the first node's tag.
    fn head(&self) -> *const u32;
}

impl sealed::Sealed for DrawDoneNode {}
impl StaticChain for DrawDoneNode {
    #[inline]
    fn head(&self) -> *const u32 {
        self.as_ptr()
    }
}

impl<const W: usize> sealed::Sealed for StaticPacket<W> {}
impl<const W: usize> StaticChain for StaticPacket<W> {
    #[inline]
    fn head(&self) -> *const u32 {
        self.as_ptr()
    }
}
