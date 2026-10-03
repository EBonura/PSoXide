//! What the GPU shows: video standard, resolution, and where the picture
//! sits in the TV signal.
//!
//! A [`DisplayConfig`] is a plain value; [`crate::Gpu::new`] programs it at
//! reset and [`crate::Gpu::set_display`] reprograms it, so a "screen
//! position" option changes one field and calls one method instead of
//! repeating the arguments `init` was given.

use psx_hw::gpu::gp1;

/// Video standard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoMode {
    /// 60 Hz NTSC.
    Ntsc,
    /// 50 Hz PAL.
    Pal,
}

/// A display resolution the GPU can produce.
///
/// Only the presets exist, so an invalid width or a 480-line mode without
/// interlacing can't be asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resolution {
    width: u16,
    height: u16,
}

impl Resolution {
    /// 320×240, the default for most PS1 games.
    pub const R320X240: Self = Self::new(320, 240);
    /// 256×240.
    pub const R256X240: Self = Self::new(256, 240);
    /// 512×240.
    pub const R512X240: Self = Self::new(512, 240);
    /// 640×240.
    pub const R640X240: Self = Self::new(640, 240);
    /// 320×256, PAL's natural vertical resolution.
    pub const R320X256: Self = Self::new(320, 256);
    /// 640×480, interlaced.
    pub const R640X480: Self = Self::new(640, 480);

    const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }

    /// Width in pixels.
    pub const fn width(self) -> u16 {
        self.width
    }

    /// Height in pixels.
    pub const fn height(self) -> u16 {
        self.height
    }

    /// True for the 480-line modes, which the GPU only shows interlaced.
    pub const fn is_interlaced(self) -> bool {
        self.height >= 480
    }

    /// Scanlines per field: the height, or half of it when interlaced.
    /// GP1(07h) counts scanlines of one field, so a 480-line picture spans
    /// 240 of them (psx-spx, GP1(07h)).
    pub(crate) const fn field_lines(self) -> u32 {
        if self.is_interlaced() {
            self.height as u32 / 2
        } else {
            self.height as u32
        }
    }

    /// The GP1(08h) horizontal-resolution field.
    const fn horizontal_field(self) -> u32 {
        match self.width {
            256 => 0,
            320 => 1,
            512 => 2,
            _ => 3,
        }
    }
}

/// GPU clocks per displayed pixel at the standard PSX dot clock that
/// [`crate::Gpu::new`] programs. The horizontal display window therefore
/// spans `width * H_CLOCKS_PER_PIXEL` GPU clocks.
const H_CLOCKS_PER_PIXEL: i32 = 8;

/// Default left edge (GP1 06h X1) of the horizontal display window, in GPU
/// clocks from start-of-line: the standard centred NTSC picture.
const H_DISPLAY_WINDOW_START: i32 = 0x260;
/// Default top edge (GP1 07h Y1) of the NTSC vertical display window.
const NTSC_V_DISPLAY_WINDOW_START: i32 = 0x10;
/// Default top edge (GP1 07h Y1) of the PAL vertical display window.
const PAL_V_DISPLAY_WINDOW_START: i32 = 0x23;

/// Video standard, resolution and picture position, as one value.
///
/// ```
/// use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
/// let centred = DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240);
/// let nudged = centred.with_offset((4, -2));
/// assert_eq!(nudged.offset, (4, -2));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DisplayConfig {
    /// Video standard.
    pub mode: VideoMode,
    /// Resolution.
    pub resolution: Resolution,
    /// Picture shift from the standard position: pixels right, scanlines
    /// down. It moves the picture in the TV signal (GP1(06h) and GP1(07h)),
    /// the way period games recentred an image inside a CRT's overscan;
    /// VRAM and the draw offset are untouched. The window never starts
    /// before the blanking edge.
    pub offset: (i16, i16),
}

impl DisplayConfig {
    /// The standard, centred picture for `mode` and `resolution`.
    pub const fn new(mode: VideoMode, resolution: Resolution) -> Self {
        Self {
            mode,
            resolution,
            offset: (0, 0),
        }
    }

    /// The same display with the picture shifted by `offset`.
    pub const fn with_offset(self, offset: (i16, i16)) -> Self {
        Self { offset, ..self }
    }

    /// GP1(08h): resolution, standard, colour depth, interlace.
    ///
    /// psx-spx, GP1(08h): bit 2 selects 480 lines only "when Bit5=1", so a
    /// 480-line resolution sets the interlace bit as well.
    pub(crate) const fn mode_command(self) -> u32 {
        let interlaced = self.resolution.is_interlaced();
        gp1::display_mode(
            self.resolution.horizontal_field(),
            interlaced as u32,
            matches!(self.mode, VideoMode::Pal),
            false,
            interlaced,
        )
    }

    /// GP1(06h): the horizontal display window.
    pub(crate) const fn horizontal_range_command(self) -> u32 {
        let start = H_DISPLAY_WINDOW_START + self.offset.0 as i32 * H_CLOCKS_PER_PIXEL;
        let start = if start < 0 { 0 } else { start as u32 };
        let end = start + self.resolution.width as u32 * H_CLOCKS_PER_PIXEL as u32;
        gp1::h_display_range(start, end)
    }

    /// GP1(07h): the vertical display window, in scanlines of one field.
    pub(crate) const fn vertical_range_command(self) -> u32 {
        let top = match self.mode {
            VideoMode::Ntsc => NTSC_V_DISPLAY_WINDOW_START,
            VideoMode::Pal => PAL_V_DISPLAY_WINDOW_START,
        };
        let start = top + self.offset.1 as i32;
        let start = if start < 0 { 0 } else { start as u32 };
        gp1::v_display_range(start, start + self.resolution.field_lines())
    }
}

/// VRAM lines the GPU has; two buffers must fit in them.
const VRAM_LINES: u16 = 512;

/// Two framebuffers stacked in VRAM: the GPU shows one while the other is
/// drawn, and [`swap`](Self::swap) exchanges them.
///
/// The first buffer sits at VRAM line 0 and the second `stride` lines
/// below it, so a 320×240 pair fits beside textures in the 1024×512 VRAM.
///
/// ```
/// use psx_gpu::display::{DoubleBuffer, Resolution};
/// let buffers = DoubleBuffer::with_stride(Resolution::R320X240, 256);
/// assert_eq!(buffers.draw_origin(), (0, 0));
/// assert_eq!(buffers.display_origin(), (0, 256));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DoubleBuffer {
    size: (u16, u16),
    stride: u16,
    drawing_second: bool,
}

impl DoubleBuffer {
    /// Buffers of `resolution`, the second directly below the first.
    pub const fn new(resolution: Resolution) -> Self {
        Self::with_stride(resolution, resolution.height)
    }

    /// Buffers `stride` VRAM lines apart, leaving the lines between them
    /// free (palettes, say). A stride below the height is raised to it so
    /// the buffers never overlap.
    ///
    /// # Panics
    ///
    /// If the second buffer would end past VRAM line 512 (a 480-line
    /// resolution can't be double-buffered this way).
    pub const fn with_stride(resolution: Resolution, stride: u16) -> Self {
        let height = resolution.height;
        let stride = if stride < height { height } else { stride };
        assert!(
            stride + height <= VRAM_LINES,
            "two buffers must fit in VRAM's 512 lines"
        );
        Self {
            size: (resolution.width, height),
            stride,
            drawing_second: false,
        }
    }

    /// Width and height of each buffer, in pixels.
    pub const fn size(&self) -> (u16, u16) {
        self.size
    }

    /// VRAM lines between the two buffers' top edges.
    pub const fn stride(&self) -> u16 {
        self.stride
    }

    /// Top-left VRAM corner of the buffer being drawn.
    pub const fn draw_origin(&self) -> (u16, u16) {
        (0, self.top(self.drawing_second))
    }

    /// Top-left VRAM corner of the buffer being shown.
    pub const fn display_origin(&self) -> (u16, u16) {
        (0, self.top(!self.drawing_second))
    }

    const fn top(&self, second: bool) -> u16 {
        if second {
            self.stride
        } else {
            0
        }
    }

    /// Show the buffer just drawn and draw into the other one from now on.
    ///
    /// Flips at once (GP1(05h)); the hardware latches the start at the next
    /// frame. Drawing must be finished ([`crate::Gpu::wait_idle`]) first.
    #[doc(alias = "PutDispEnv")]
    pub fn swap(&mut self, gpu: &mut crate::Gpu) {
        let shown = self.draw_origin();
        self.drawing_second = !self.drawing_second;
        gpu.set_display_start(shown);
        self.apply_draw_target(gpu);
    }

    /// Switch the draw side now and program its draw area and offset, and
    /// return the GP1(05h) word that shows the finished buffer, for the
    /// caller to apply at a blank edge (psx-rt's queued flip). Drawing must
    /// be finished ([`crate::Gpu::wait_idle`]) first.
    pub fn begin_swap(&mut self, gpu: &mut crate::Gpu) -> u32 {
        let display = self.begin_deferred_swap();
        self.apply_draw_target(gpu);
        display
    }

    /// Select the next draw buffer without touching the GPU, and return the
    /// GP1(05h) word that shows the finished one.
    ///
    /// The non-blocking first half of a pipelined swap: queue the word for a
    /// VBlank edge whose handler applies it once the frame's closing
    /// GP0(1Fh) has run (`psx_rt::interrupts::queue_display_control_at_vblank`,
    /// see [`crate::is_draw_done`]), wait until it has been applied, then
    /// call [`apply_draw_target`](Self::apply_draw_target) before drawing.
    /// Safe to call while the previous frame still rasterises.
    pub fn begin_deferred_swap(&mut self) -> u32 {
        let (x, y) = self.draw_origin();
        self.drawing_second = !self.drawing_second;
        gp1::display_start(x as u32, y as u32)
    }

    /// Program the draw area and draw offset for the buffer being drawn.
    pub fn apply_draw_target(&self, _gpu: &mut crate::Gpu) {
        let (_, top) = self.draw_origin();
        let (width, height) = self.size;
        crate::gpu::write_draw_target(top, width, height);
    }

    /// Fill the buffer being drawn with `color` (GP0(02h)).
    pub fn clear(&self, gpu: &mut crate::Gpu, color: (u8, u8, u8)) {
        gpu.draw(&crate::prim::FillRect::new(
            self.draw_origin(),
            self.size,
            color,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERLACE: u32 = 1 << 5;
    const LINES_480: u32 = 1 << 2;

    #[test]
    fn a_240_line_mode_is_progressive() {
        let word = DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240).mode_command();
        assert_eq!(word & (INTERLACE | LINES_480), 0);
        assert_eq!(word & 3, 1, "320-pixel horizontal mode");
    }

    #[test]
    fn a_480_line_mode_sets_the_interlace_bit_it_needs() {
        let word = DisplayConfig::new(VideoMode::Ntsc, Resolution::R640X480).mode_command();
        assert_eq!(word & (INTERLACE | LINES_480), INTERLACE | LINES_480);
        assert_eq!(word & 3, 3, "640-pixel horizontal mode");
    }

    #[test]
    fn a_480_line_mode_spans_one_field_of_scanlines() {
        // psx-spx GP1(07h): NTSC Y1/Y2 = 88h -/+ 240/2 in either line mode.
        let word =
            DisplayConfig::new(VideoMode::Ntsc, Resolution::R640X480).vertical_range_command();
        assert_eq!(word, gp1::v_display_range(0x88 - 120, 0x88 + 120));
    }

    #[test]
    fn the_standard_ntsc_picture_starts_at_260h() {
        // psx-spx GP1(06h): 260h is the first visible pixel on normal TVs,
        // and 320-pixel mode spans 320 * 8 clocks.
        let word =
            DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240).horizontal_range_command();
        assert_eq!(word, gp1::h_display_range(0x260, 0x260 + 320 * 8));
    }

    #[test]
    fn an_offset_moves_both_windows_and_stops_at_the_blanking_edge() {
        let display = DisplayConfig::new(VideoMode::Pal, Resolution::R320X256);
        let moved = display.with_offset((2, 3));
        assert_eq!(
            moved.horizontal_range_command(),
            gp1::h_display_range(0x260 + 16, 0x260 + 16 + 320 * 8)
        );
        assert_eq!(
            moved.vertical_range_command(),
            gp1::v_display_range(0x23 + 3, 0x23 + 3 + 256)
        );
        let clamped = display.with_offset((-200, -100));
        assert_eq!(
            clamped.horizontal_range_command(),
            gp1::h_display_range(0, 320 * 8)
        );
        assert_eq!(
            clamped.vertical_range_command(),
            gp1::v_display_range(0, 256)
        );
    }

    #[test]
    fn a_deferred_swap_alternates_buffers_without_touching_the_gpu() {
        let mut buffers = DoubleBuffer::with_stride(Resolution::R320X240, 256);
        assert_eq!(buffers.begin_deferred_swap(), gp1::display_start(0, 0));
        assert_eq!(buffers.draw_origin(), (0, 256));
        assert_eq!(buffers.display_origin(), (0, 0));
        assert_eq!(buffers.begin_deferred_swap(), gp1::display_start(0, 256));
        assert_eq!(buffers.draw_origin(), (0, 0));
    }

    #[test]
    fn a_short_stride_is_raised_so_buffers_never_overlap() {
        let buffers = DoubleBuffer::with_stride(Resolution::R320X240, 128);
        assert_eq!(buffers.stride(), 240);
        assert_eq!(buffers.size(), (320, 240));
    }

    #[test]
    #[should_panic(expected = "two buffers must fit")]
    fn a_480_line_double_buffer_does_not_fit_vram() {
        let _ = DoubleBuffer::new(Resolution::R640X480);
    }
}
