//! The GPU driver.

use crate::display::DisplayConfig;
use crate::material::{TextureMaterial, TextureWindow};
use crate::prim::GpuPacket;
use crate::{StaticChain, TextureDepth};
use psx_hw::gpu::{gp0, gp1, DmaDirection};
use psx_io::gpu::{wait_command_ready, write_command, write_display_control};
use psx_io::periph::GpuDma;

bitflags::bitflags! {
    /// GP0(E6h) mask-bit state.
    ///
    /// With both flags set the GPU gives front-to-back occlusion without a
    /// Z-buffer: draw nearest first, and farther pixels that would overdraw
    /// are rejected. The state applies until changed, so return to
    /// [`MaskMode::empty`] before translucent passes that must not mask.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct MaskMode: u32 {
        /// Force bit 15 on every pixel written.
        const SET_ON_DRAW = 1 << 0;
        /// Skip pixels whose bit 15 is already set.
        const CHECK_BEFORE_DRAW = 1 << 1;
    }
}

/// The PS1 GPU: its command port (GP0), its control port (GP1) and DMA
/// channel 2, which feeds GP0.
///
/// `Gpu` owns the [`GpuDma`] token, so every operation that writes a GPU
/// port is a method that needs `&mut Gpu`. That puts the borrow checker
/// between two users of one command stream: drawing immediately while a
/// linked-list walk feeds GP0 is a compile error, not a corrupted frame.
///
/// ```compile_fail,E0500
/// # use psx_gpu::{ot::OrderingTable, prim::TriFlat, Gpu};
/// # fn f(gpu: &mut Gpu, ot: &mut OrderingTable<8>) {
/// let frame = ot.frame();
/// frame.submit_with(gpu.dma_mut(), || {
///     gpu.draw(&TriFlat::new([(0, 0), (8, 0), (0, 8)], 255, 0, 0)); // GP0 mid-walk
/// });
/// # }
/// ```
///
/// It is zero-sized, so holding and passing it costs nothing. Reading
/// status (`psx_gpu::is_draw_done`, `psx_io::gpu::status`) needs no
/// handle: a read can't disturb the command stream.
///
/// ```no_run
/// use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
/// use psx_gpu::prim::FillRect;
/// use psx_gpu::Gpu;
///
/// fn start(dma: psx_io::periph::GpuDma) -> Gpu {
///     let mut gpu = Gpu::new(dma, DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240));
///     gpu.draw(&FillRect::new((0, 0), (320, 240), (0, 0, 32)));
///     gpu
/// }
/// ```
#[derive(Debug)]
#[repr(transparent)]
pub struct Gpu(GpuDma);

impl Gpu {
    /// Take over the GPU: reset it, program `display`, point DMA at GP0 and
    /// turn the display on.
    #[doc(alias = "ResetGraph")]
    pub fn new(dma: GpuDma, display: DisplayConfig) -> Self {
        reset(display);
        Self(dma)
    }

    /// The driver as a view of a token held elsewhere, such as the one a
    /// `FramePair` owns, for immediate drawing between its frames.
    #[inline(always)]
    pub fn from_dma_mut(dma: &mut GpuDma) -> &mut Self {
        // SAFETY: `Gpu` is a `repr(transparent)` wrapper of `GpuDma`, so the
        // two types have the same layout and the cast keeps the exclusive
        // borrow.
        unsafe { &mut *core::ptr::from_mut(dma).cast::<Self>() }
    }

    /// The token, for APIs that start channel 2 themselves (frame
    /// submission, VRAM DMA). The driver stays borrowed meanwhile.
    #[inline(always)]
    pub fn dma_mut(&mut self) -> &mut GpuDma {
        &mut self.0
    }

    /// Give the token back.
    #[inline(always)]
    pub fn release(self) -> GpuDma {
        self.0
    }

    /// Reprogram the display mode and the picture position.
    #[doc(alias = "PutDispEnv")]
    pub fn set_display(&mut self, display: DisplayConfig) {
        write_display_control(display.mode_command());
        write_display_window(display);
    }

    /// Show VRAM from `origin` (GP1(05h)): a double-buffer flip.
    #[inline]
    pub fn set_display_start(&mut self, origin: (u16, u16)) {
        write_display_control(gp1::display_start(origin.0 as u32, origin.1 as u32));
    }

    /// Turn the video output on or off (GP1(03h)).
    #[doc(alias = "SetDispMask")]
    #[inline]
    pub fn set_display_enabled(&mut self, enabled: bool) {
        write_display_control(gp1::display_enable(enabled));
    }

    /// Clip drawing to the rectangle from `top_left` to `bottom_right`
    /// (inclusive VRAM coordinates).
    #[inline]
    pub fn set_draw_area(&mut self, top_left: (u16, u16), bottom_right: (u16, u16)) {
        set_draw_area(top_left, bottom_right);
    }

    /// Add `offset` to every vertex drawn from now on (GP0(E5h)), so a
    /// back buffer can be drawn in its own coordinates.
    #[inline]
    pub fn set_draw_offset(&mut self, offset: (i16, i16)) {
        set_draw_offset(offset);
    }

    /// Set the mask-bit state (GP0(E6h)).
    #[inline]
    pub fn set_mask_mode(&mut self, mode: MaskMode) {
        wait_command_ready();
        write_command(gp0::mask_bit(
            mode.contains(MaskMode::SET_ON_DRAW),
            mode.contains(MaskMode::CHECK_BEFORE_DRAW),
        ));
    }

    /// Select the texture page at VRAM `origin` with `depth` for textured
    /// rectangles (GP0(E1h)). Textured polygons carry their own page.
    #[inline]
    pub fn set_texture_page(&mut self, origin: (u16, u16), depth: TextureDepth) {
        set_texture_page(origin, depth);
    }

    /// Apply `material`'s texture page, blend equation, dither and
    /// texture window (GP0(E1h), GP0(E2h)).
    #[inline]
    pub fn set_draw_mode(&mut self, material: TextureMaterial) {
        set_draw_mode(material);
    }

    /// Set the texture window (GP0(E2h)).
    #[inline]
    pub fn set_texture_window(&mut self, window: TextureWindow) {
        wait_command_ready();
        write_command(window.word());
    }

    /// Send `packet` to GP0 now, without an ordering table.
    ///
    /// The same packet types go into an `OtFrame`, so a primitive has one
    /// encoder whether it is drawn now or linked into a frame.
    #[inline(always)]
    pub fn draw<P: GpuPacket>(&mut self, packet: &P) {
        draw(packet);
    }

    /// Wait until the GPU has drawn everything sent to it.
    ///
    /// See [`crate::wait_idle`] for what that waits on.
    #[doc(alias = "DrawSync")]
    #[inline]
    pub fn wait_idle(&mut self) {
        crate::wait_idle_impl();
    }

    /// Clear GPUSTAT bit 24 (GP1(02h)) before kicking work that ends in
    /// GP0(1Fh); see `psx_gpu::is_draw_done`.
    #[inline]
    pub fn arm_draw_done(&mut self) {
        write_display_control(gp1::ACK_IRQ);
    }

    /// Send GP0(1Fh), closing immediate drawing so `is_draw_done` rises
    /// once it is drawn.
    #[inline]
    pub fn signal_draw_done(&mut self) {
        wait_command_ready();
        write_command(gp0::REQUEST_IRQ);
    }

    /// Kick a `'static`, immutable chain, such as
    /// [`DRAW_DONE_NODE`](crate::DRAW_DONE_NODE), without waiting for it.
    ///
    /// The chain outlives any walk, so there is nothing to wait for before
    /// reusing memory; a later kick waits for this walk on its own.
    #[inline]
    pub fn submit_static(&mut self, chain: &'static impl StaticChain) {
        // SAFETY: `StaticChain` guarantees a well-formed list that stays live
        // and unmodified for 'static.
        unsafe { crate::submit_linked_list_async_raw(chain.head()) }
    }
}

/// [`Gpu::new`]'s port writes.
pub(crate) fn reset(display: DisplayConfig) {
    write_display_control(gp1::RESET);
    write_display_control(display.mode_command());
    write_display_window(display);
    write_display_control(gp1::dma_direction(DmaDirection::CpuToGp0 as u32));
    write_display_control(gp1::display_enable(true));
}

/// GP1(06h) and GP1(07h) for `display`.
pub(crate) fn write_display_window(display: DisplayConfig) {
    write_display_control(display.horizontal_range_command());
    write_display_control(display.vertical_range_command());
}

pub(crate) fn set_draw_area(top_left: (u16, u16), bottom_right: (u16, u16)) {
    wait_command_ready();
    write_command(gp0::draw_area_top_left(
        top_left.0 as u32,
        top_left.1 as u32,
    ));
    write_command(gp0::draw_area_bottom_right(
        bottom_right.0 as u32,
        bottom_right.1 as u32,
    ));
}

pub(crate) fn set_draw_offset(offset: (i16, i16)) {
    wait_command_ready();
    write_command(gp0::draw_offset(offset.0 as i32, offset.1 as i32));
}

pub(crate) fn set_texture_page(origin: (u16, u16), depth: TextureDepth) {
    wait_command_ready();
    write_command(gp0::draw_mode(
        (origin.0 / 64) as u32,
        (origin.1 / 256) as u32,
        0,
        depth as u32,
        false,
        true,
    ));
}

pub(crate) fn set_draw_mode(material: TextureMaterial) {
    wait_command_ready();
    write_command(material.draw_mode_word());
    write_command(material.texture_window_word());
}

/// One ready-wait, then the packet's payload words, in order.
#[inline(always)]
pub(crate) fn draw<P: GpuPacket>(packet: &P) {
    let words = core::ptr::from_ref(packet).cast::<u32>();
    wait_command_ready();
    let mut index = 1;
    while index <= P::WORDS as usize {
        // SAFETY: `GpuPacket` guarantees a `repr(C)` layout of a tag word and
        // then at least `WORDS` initialised `u32`s, so words 1..=WORDS are
        // inside `packet`, which the borrow keeps alive and unchanged.
        write_command(unsafe { words.add(index).read() });
        index += 1;
    }
}

/// Draw area and offset for a buffer at VRAM line `top`, written without a
/// ready-wait, as `FrameBuffer` always has: state commands between frames.
pub(crate) fn write_draw_target(top: u16, width: u16, height: u16) {
    write_command(gp0::draw_area_top_left(0, top as u32));
    write_command(gp0::draw_area_bottom_right(
        (width - 1) as u32,
        (top + height - 1) as u32,
    ));
    write_command(gp0::draw_offset(0, top as i32));
}
