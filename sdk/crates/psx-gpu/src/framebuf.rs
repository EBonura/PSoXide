//! Double-buffered framebuffer management.
//!
//! A `FrameBuffer` tracks two on-screen regions in VRAM -- one being
//! displayed, one being drawn into. `swap()` flips them at a VBlank
//! boundary. Layout is vertical: buffer A at Y=0, buffer B at Y=height.
//! That fits 2×(640×240) side-by-side inside the 1024×512 VRAM on
//! standard NTSC resolutions and gives the engine a natural tear-free
//! presentation.

use psx_hw::gpu::{gp0, gp1};
use psx_io::gpu::{write_gp0, write_gp1};

/// Tracks display-start between two vertically stacked buffers.
pub struct FrameBuffer {
    /// Display width in pixels (set by [`FrameBuffer::new`]).
    pub width: u16,
    /// Display height in pixels.
    pub height: u16,
    /// Vertical distance in VRAM rows between the two buffers.
    pub stride: u16,
    /// Index (0 or 1) of the buffer currently drawn TO.
    pub drawing: u8,
}

impl FrameBuffer {
    /// Create a framebuffer for the given active display size. Buffer
    /// A lives at VRAM Y=0, buffer B at Y=`height`.
    pub const fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            stride: height,
            drawing: 0,
        }
    }

    /// Create vertically stacked buffers with an explicit row stride.
    ///
    /// This supports layouts which reserve rows between the visible buffers,
    /// such as a 320x240 display at Y=0 and Y=256 with palettes in the gap.
    /// A stride smaller than `height` is clamped so the buffers cannot overlap.
    pub const fn new_strided(width: u16, height: u16, stride: u16) -> Self {
        Self {
            width,
            height,
            stride: if stride < height { height } else { stride },
            drawing: 0,
        }
    }

    /// Y-coordinate of buffer `idx` in VRAM.
    #[inline]
    pub const fn buffer_y(&self, idx: u8) -> u16 {
        if idx == 0 {
            0
        } else {
            self.stride
        }
    }

    /// Push a display-start command for the buffer we're NOT currently
    /// drawing to -- flipping the display at the next VBlank.
    pub fn swap(&mut self) {
        // Show the buffer we were drawing into; drain into the other.
        let show = self.drawing;
        self.drawing ^= 1;
        let show_y = self.buffer_y(show);
        write_gp1(gp1::display_start(0, show_y as u32));

        // Re-set the draw-area / draw-offset to match the new target buffer.
        let target_y = self.buffer_y(self.drawing);
        write_gp0(gp0::draw_area_top_left(0, target_y as u32));
        write_gp0(gp0::draw_area_bottom_right(
            (self.width - 1) as u32,
            (target_y + self.height - 1) as u32,
        ));
        write_gp0(gp0::draw_offset(0, target_y as i32));
    }

    /// Deferred-present half of [`FrameBuffer::swap`]: switch the DRAW side
    /// now and return the GP1 display-start word for the finished buffer,
    /// for the caller to apply exactly at a blank edge (e.g. via psx-rt's
    /// queued-GP1 VBlank hook). The GPU must be idle (`draw_sync`) before
    /// this call, because it rewrites the draw area/offset directly.
    pub fn begin_swap(&mut self) -> u32 {
        let display_start = self.begin_deferred_swap();
        self.apply_draw_target();
        display_start
    }

    /// Select the next draw buffer without writing any GP0 commands.
    ///
    /// This is the non-blocking first half of a pipelined swap. Queue the
    /// returned GP1 word for a VBlank edge whose handler applies it only once
    /// the GPU is idle, wait until that queue entry is consumed, then call
    /// [`FrameBuffer::apply_draw_target`] before clearing or drawing into the
    /// newly selected buffer.
    ///
    /// Unlike [`FrameBuffer::begin_swap`], this method is safe to call while
    /// the previous frame is still rasterising: it only changes CPU-owned
    /// bookkeeping and does not touch the GPU command port.
    pub fn begin_deferred_swap(&mut self) -> u32 {
        let show = self.drawing;
        self.drawing ^= 1;
        gp1::display_start(0, self.buffer_y(show) as u32)
    }

    /// Program GP0 draw area and offset for the currently selected draw side.
    ///
    /// Call this after a display-start queued by
    /// [`FrameBuffer::begin_deferred_swap`] has been applied at a GPU-idle
    /// VBlank edge. It deliberately remains separate from buffer selection so
    /// a busy raster never turns these three state writes into a synchronous
    /// CPU wait at the start of the next frame.
    pub fn apply_draw_target(&self) {
        let target_y = self.buffer_y(self.drawing);
        write_gp0(gp0::draw_area_top_left(0, target_y as u32));
        write_gp0(gp0::draw_area_bottom_right(
            (self.width - 1) as u32,
            (target_y + self.height - 1) as u32,
        ));
        write_gp0(gp0::draw_offset(0, target_y as i32));
    }

    /// Clear the back-buffer (the one currently being drawn to) to `(r, g, b)`.
    pub fn clear(&self, r: u8, g: u8, b: u8) {
        super::fill_rect(
            0,
            self.buffer_y(self.drawing),
            self.width,
            self.height,
            r,
            g,
            b,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_stacks_buffers_at_visible_height() {
        let framebuffer = FrameBuffer::new(320, 240);
        assert_eq!(framebuffer.buffer_y(0), 0);
        assert_eq!(framebuffer.buffer_y(1), 240);
    }

    #[test]
    fn strided_layout_preserves_reserved_vram_rows() {
        let framebuffer = FrameBuffer::new_strided(320, 240, 256);
        assert_eq!(framebuffer.buffer_y(0), 0);
        assert_eq!(framebuffer.buffer_y(1), 256);
        assert_eq!(framebuffer.height, 240);
    }

    #[test]
    fn deferred_swap_selects_the_opposite_buffer_without_gpu_state() {
        let mut framebuffer = FrameBuffer::new_strided(320, 240, 256);

        let first_display = framebuffer.begin_deferred_swap();
        assert_eq!(first_display, gp1::display_start(0, 0));
        assert_eq!(framebuffer.drawing, 1);

        let second_display = framebuffer.begin_deferred_swap();
        assert_eq!(second_display, gp1::display_start(0, 256));
        assert_eq!(framebuffer.drawing, 0);
    }

    #[test]
    fn strided_layout_prevents_buffer_overlap() {
        let framebuffer = FrameBuffer::new_strided(320, 240, 128);
        assert_eq!(framebuffer.stride, 240);
    }
}
