//! Free functions from before [`crate::Gpu`], kept for one stage so games
//! that have not repinned keep compiling. Each forwards to the code the
//! driver method runs; `sdk/docs/MIGRATION-psx-gpu.md` lists the
//! replacements and every downstream call site.

#![allow(
    deprecated,
    reason = "the forwarders use the deprecated types they kept"
)]

use crate::display::{DisplayConfig, Resolution, VideoMode};
use crate::gpu;
use crate::material::{BlendMode, TextureMaterial};
use crate::prim::{
    FillRect, LineGouraud, LineMono, QuadFlat, QuadTexturedGouraud, QuadTexturedMaterial, Sprite,
    TriFlat, TriGouraud, TriTextured,
};
use psx_io::timers;

/// Initialise the GPU: reset, display mode, display window, DMA direction,
/// display on.
#[deprecated(note = "use `Gpu::new(dma, DisplayConfig::new(mode, res))`")]
#[doc(alias = "ResetGraph")]
pub fn init(mode: VideoMode, res: Resolution) {
    gpu::reset(DisplayConfig::new(mode, res));
}

/// Move the picture by `dx` pixels and `dy` scanlines from the standard
/// position, keeping `mode` and `res`.
#[deprecated(note = "use `Gpu::set_display` with `DisplayConfig::with_offset`")]
pub fn set_display_offset(mode: VideoMode, res: Resolution, dx: i16, dy: i16) {
    gpu::write_display_window(DisplayConfig::new(mode, res).with_offset((dx, dy)));
}

/// Shift the picture horizontally by `offset_px` pixels (NTSC window).
#[deprecated(note = "use `Gpu::set_display` with `DisplayConfig::with_offset`")]
pub fn set_screen_h_offset(offset_px: i16, res: Resolution) {
    let display = DisplayConfig::new(VideoMode::Ntsc, res).with_offset((offset_px, 0));
    psx_io::gpu::write_display_control(display.horizontal_range_command());
}

/// Shift the picture vertically by `offset_px` scanlines.
#[deprecated(note = "use `Gpu::set_display` with `DisplayConfig::with_offset`")]
pub fn set_screen_v_offset(offset_px: i16, mode: VideoMode, res: Resolution) {
    let display = DisplayConfig::new(mode, res).with_offset((0, offset_px));
    psx_io::gpu::write_display_control(display.vertical_range_command());
}

/// Block until the GPU has finished drawing everything sent to it.
#[deprecated(note = "use `Gpu::wait_idle`")]
#[doc(alias = "DrawSync")]
#[inline]
pub fn wait_idle() {
    crate::wait_idle_impl();
}

/// Renamed to `Gpu::wait_idle`.
#[deprecated(note = "use `Gpu::wait_idle`")]
#[inline(always)]
pub fn draw_sync() {
    crate::wait_idle_impl();
}

/// Clear GPUSTAT bit 24 with GP1(02h).
#[deprecated(note = "use `Gpu::arm_draw_done`")]
#[inline]
pub fn arm_draw_done() {
    psx_io::gpu::write_display_control(psx_hw::gpu::gp1::ACK_IRQ);
}

/// Send GP0(1Fh) through the command port.
#[deprecated(note = "use `Gpu::signal_draw_done`")]
#[inline]
pub fn signal_draw_done() {
    psx_io::gpu::wait_command_ready();
    psx_io::gpu::write_command(psx_hw::gpu::gp0::REQUEST_IRQ);
}

/// Kick a `'static` chain without waiting for it.
#[deprecated(note = "use `Gpu::submit_static`")]
#[inline]
pub fn submit_static(dma: &mut psx_io::periph::GpuDma, chain: &'static impl crate::StaticChain) {
    crate::Gpu::from_dma_mut(dma).submit_static(chain);
}

/// Set the drawing-area rectangle.
#[deprecated(note = "use `Gpu::set_draw_area`")]
pub fn set_draw_area(x0: u16, y0: u16, x1: u16, y1: u16) {
    gpu::set_draw_area((x0, y0), (x1, y1));
}

/// Set the drawing offset.
#[deprecated(note = "use `Gpu::set_draw_offset`")]
pub fn set_draw_offset(x: i16, y: i16) {
    gpu::set_draw_offset((x, y));
}

/// Set the mask-bit mode.
#[deprecated(note = "use `Gpu::set_mask_mode` with `MaskMode` flags")]
pub fn set_mask_mode(set_on_draw: bool, check_before_draw: bool) {
    psx_io::gpu::wait_command_ready();
    psx_io::gpu::write_command(psx_hw::gpu::gp0::mask_bit(set_on_draw, check_before_draw));
}

/// Select a texture page for textured rectangles.
#[deprecated(note = "use `Gpu::set_draw_mode` with a `TextureMaterial` for the page")]
pub fn set_texture_page(tpage_x: u16, tpage_y: u16, depth: TextureDepth) {
    psx_io::gpu::wait_command_ready();
    psx_io::gpu::write_command(psx_hw::gpu::gp0::draw_mode(
        (tpage_x / 64) as u32,
        (tpage_y / 256) as u32,
        0,
        depth as u32,
        false,
        true,
    ));
}

/// Fill a VRAM rectangle with a solid color.
#[deprecated(note = "use `gpu.draw(&FillRect::new(origin, size, color))`")]
pub fn fill_rect(x: u16, y: u16, w: u16, h: u16, r: u8, g: u8, b: u8) {
    gpu::draw(&FillRect::new((x, y), (w, h), (r, g, b)));
}

/// Draw a flat-shaded triangle.
#[deprecated(note = "use `gpu.draw(&TriFlat::new(..))`")]
pub fn draw_tri_flat(verts: [(i16, i16); 3], r: u8, g: u8, b: u8) {
    gpu::draw(&TriFlat::new(verts, r, g, b));
}

/// Draw a semi-transparent flat-shaded triangle.
#[deprecated(
    note = "use `gpu.set_draw_mode(material)` and `gpu.draw(&TriFlat::new(..).translucent())`"
)]
pub fn draw_tri_flat_blended(verts: [(i16, i16); 3], r: u8, g: u8, b: u8, blend_mode: BlendMode) {
    if !blend_mode.is_translucent() {
        gpu::draw(&TriFlat::new(verts, r, g, b));
        return;
    }
    gpu::set_draw_mode(TextureMaterial::blended(0, 0, (r, g, b), blend_mode));
    gpu::draw(&TriFlat::new(verts, r, g, b).translucent());
}

/// Draw a Gouraud-shaded triangle.
#[deprecated(note = "use `gpu.draw(&TriGouraud::new(..))`")]
pub fn draw_tri_gouraud(verts: [(i16, i16); 3], colors: [(u8, u8, u8); 3]) {
    gpu::draw(&TriGouraud::new(verts, colors));
}

/// Draw a semi-transparent Gouraud-shaded triangle.
#[deprecated(
    note = "use `gpu.set_draw_mode(material)` and `gpu.draw(&TriGouraud::new(..).translucent())`"
)]
pub fn draw_tri_gouraud_blended(
    verts: [(i16, i16); 3],
    colors: [(u8, u8, u8); 3],
    blend_mode: BlendMode,
) {
    if !blend_mode.is_translucent() {
        gpu::draw(&TriGouraud::new(verts, colors));
        return;
    }
    gpu::set_draw_mode(TextureMaterial::blended(0, 0, colors[0], blend_mode));
    gpu::draw(&TriGouraud::new(verts, colors).translucent());
}

/// Draw a monochrome line.
#[deprecated(note = "use `gpu.draw(&LineMono::new(..))`")]
pub fn draw_line_mono(x0: i16, y0: i16, x1: i16, y1: i16, r: u8, g: u8, b: u8) {
    gpu::draw(&LineMono::new(x0, y0, x1, y1, r, g, b));
}

/// Draw a semi-transparent monochrome line.
#[deprecated(
    note = "use `gpu.set_draw_mode(material)` and `gpu.draw(&LineMono::new(..).translucent())`"
)]
pub fn draw_line_mono_blended(
    from: (i16, i16),
    to: (i16, i16),
    color: (u8, u8, u8),
    blend_mode: BlendMode,
) {
    let line = LineMono::new(from.0, from.1, to.0, to.1, color.0, color.1, color.2);
    if !blend_mode.is_translucent() {
        gpu::draw(&line);
        return;
    }
    gpu::set_draw_mode(TextureMaterial::blended(0, 0, color, blend_mode));
    gpu::draw(&line.translucent());
}

/// Draw a Gouraud-shaded line.
#[deprecated(note = "use `gpu.draw(&LineGouraud::new(..))`")]
pub fn draw_line_gouraud(x0: i16, y0: i16, c0: (u8, u8, u8), x1: i16, y1: i16, c1: (u8, u8, u8)) {
    gpu::draw(&LineGouraud::new((x0, y0), c0, (x1, y1), c1));
}

/// Fill an axis-aligned rectangle with a flat color, as a polygon.
#[deprecated(note = "use `gpu.draw(&QuadFlat::rect(origin, size, color))`")]
pub fn draw_rect_flat(x: i16, y: i16, w: u16, h: u16, r: u8, g: u8, b: u8) {
    gpu::draw(&QuadFlat::rect((x, y), (w, h), (r, g, b)));
}

/// Draw a flat-shaded quad.
#[deprecated(note = "use `gpu.draw(&QuadFlat::new(..))`")]
pub fn draw_quad_flat(verts: [(i16, i16); 4], r: u8, g: u8, b: u8) {
    gpu::draw(&QuadFlat::new(verts, r, g, b));
}

/// Draw a textured quad with a single tint.
#[deprecated(note = "use `gpu.draw(&QuadTexturedMaterial::with_material(..))`")]
pub fn draw_quad_textured(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    clut_word: u16,
    tpage_word: u16,
    tint: (u8, u8, u8),
) {
    let material = TextureMaterial::opaque(clut_word, tpage_word, tint);
    gpu::draw(&QuadTexturedMaterial::with_material(verts, uvs, material));
}

/// Draw a textured quad using a [`TextureMaterial`].
#[deprecated(note = "use `gpu.draw(&QuadTexturedMaterial::with_material(..))`")]
pub fn draw_quad_textured_material(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    material: TextureMaterial,
) {
    gpu::draw(&QuadTexturedMaterial::with_material(verts, uvs, material));
}

/// Draw a textured triangle using a [`TextureMaterial`].
#[deprecated(note = "use `gpu.draw(&TriTextured::with_material(..))`")]
pub fn draw_tri_textured_material(
    verts: [(i16, i16); 3],
    uvs: [(u8, u8); 3],
    material: TextureMaterial,
) {
    gpu::draw(&TriTextured::with_material(verts, uvs, material));
}

/// Draw a Gouraud-shaded textured quad.
#[deprecated(note = "use `gpu.draw(&QuadTexturedGouraud::with_material(..))`")]
pub fn draw_quad_textured_gouraud(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    clut_word: u16,
    tpage_word: u16,
) {
    let material = TextureMaterial::new(clut_word, tpage_word);
    gpu::draw(&QuadTexturedGouraud::with_material(
        verts, uvs, colors, material,
    ));
}

/// Draw a Gouraud-shaded textured quad using a [`TextureMaterial`].
#[deprecated(note = "use `gpu.draw(&QuadTexturedGouraud::with_material(..))`")]
pub fn draw_quad_textured_gouraud_material(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    material: TextureMaterial,
) {
    gpu::draw(&QuadTexturedGouraud::with_material(
        verts, uvs, colors, material,
    ));
}

/// Draw a variable-size textured sprite using a [`TextureMaterial`].
#[deprecated(note = "use `gpu.set_draw_mode(material)` and `gpu.draw(&Sprite::with_material(..))`")]
pub fn draw_sprite_material(
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    uv: (u8, u8),
    material: TextureMaterial,
) {
    gpu::set_draw_mode(material);
    gpu::draw(&Sprite::with_material(x, y, w, h, uv, material));
}

/// Configure Timer 1 as an HBlank-counting scanline counter.
///
/// WARNING: writing a timer's mode register resets its counter, so every
/// call restarts the count from zero. That is why the helpers below cannot
/// observe the real display position: they reconfigure before reading.
#[deprecated(note = "Timer 1 belongs to `psx_io::timers`; set its mode there")]
#[inline]
pub fn configure_scanline_timer() {
    // Sync enabled with sync mode 1 (reset at VBlank), clock source 1
    // (HBlank).
    use psx_hw::timers::mode;
    timers::set_mode(
        timers::Timer::Timer1,
        mode::SYNC_ENABLE | mode::sync_mode(1) | mode::clock_source(1),
    );
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

/// Texture color depth for the deprecated `set_texture_page`.
#[deprecated(
    note = "only `set_texture_page` used it; `TextureMaterial` and psx-vram's `TextureDepth` describe texture pages"
)]
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

/// Kick a linked-list chain without waiting for the walk.
///
/// # Safety
///
/// As [`crate::chain::submit_async_raw`].
#[deprecated(note = "use `chain::submit_async_raw`, which takes the `GpuDma` token")]
#[doc(alias = "DrawOTag")]
#[inline(always)]
pub unsafe fn submit_linked_list_async_raw(head: *const u32) {
    // SAFETY: forwarded contract.
    unsafe { crate::chain::start_walk(head) }
}

/// Renamed to [`submit_linked_list_async_raw`].
///
/// # Safety
///
/// As [`crate::chain::submit_async_raw`].
#[deprecated(note = "use `chain::submit_async_raw`, which takes the `GpuDma` token")]
#[inline(always)]
pub unsafe fn submit_linked_list_raw_async(head: *const u32) {
    // SAFETY: forwarded contract.
    unsafe { crate::chain::start_walk(head) }
}

/// Old name of [`submit_linked_list_async_raw`].
///
/// # Safety
///
/// As [`crate::chain::submit_async_raw`].
#[deprecated(
    note = "use `OrderingTable::frame`, `Gpu::submit_static`, or the unsafe `chain::submit_async_raw`"
)]
#[inline(always)]
pub unsafe fn submit_linked_list_async(head: *const u32) {
    // SAFETY: forwarded contract.
    unsafe { crate::chain::start_walk(head) }
}

/// Kick a linked-list chain and wait for the walk.
///
/// # Safety
///
/// As [`crate::chain::submit_raw`].
#[deprecated(note = "use `chain::submit_raw`, which takes the `GpuDma` token")]
#[doc(alias = "DrawOTag")]
#[inline(always)]
pub unsafe fn submit_linked_list_raw(head: *const u32) {
    // SAFETY: forwarded contract; the wait ends the walk before return.
    unsafe { crate::chain::start_walk(head) };
    crate::chain::wait_walk();
}

/// Old name of [`submit_linked_list_raw`].
///
/// # Safety
///
/// As [`crate::chain::submit_raw`].
#[deprecated(
    note = "use `OrderingTable::frame`, `Gpu::submit_static`, or the unsafe `chain::submit_raw`"
)]
#[inline(always)]
pub unsafe fn submit_linked_list(head: *const u32) {
    // SAFETY: forwarded contract; the wait ends the walk before return.
    unsafe { crate::chain::start_walk(head) };
    crate::chain::wait_walk();
}

/// Wait until the linked-list walk kicked last has finished.
#[deprecated(note = "use `chain::wait`, which takes the `GpuDma` token")]
#[inline(always)]
pub fn submit_linked_list_wait() {
    crate::chain::wait_walk();
}

/// Renamed to [`crate::is_draw_done`].
#[deprecated(note = "renamed to `is_draw_done`")]
#[inline(always)]
pub fn draw_done() -> bool {
    crate::is_draw_done()
}
/// Each forwarder now sends a packet's payload; these tests check that
/// payload against the word sequence the free function wrote before it
/// forwarded, taken from its old body.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::TextureWindow;
    use crate::prim::GpuPacket;
    use psx_hw::gpu::{gp0, pack_color, pack_texcoord, pack_vertex, pack_xy};

    fn payload<P: GpuPacket>(packet: &P) -> &[u32] {
        // SAFETY: `GpuPacket` promises a tag word, then `WORDS` initialised
        // `u32`s, inside `packet`, which outlives the slice.
        unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(packet).cast::<u32>().add(1),
                P::WORDS as usize,
            )
        }
    }

    const VERTS3: [(i16, i16); 3] = [(1, -2), (300, 4), (-5, 239)];
    const VERTS4: [(i16, i16); 4] = [(1, 2), (100, 3), (4, 90), (101, 91)];
    const UVS3: [(u8, u8); 3] = [(0, 1), (63, 2), (4, 255)];
    const UVS4: [(u8, u8); 4] = [(0, 1), (63, 2), (4, 255), (200, 100)];
    const COLORS4: [(u8, u8, u8); 4] = [(1, 2, 3), (40, 50, 60), (70, 80, 90), (255, 0, 128)];

    fn material() -> TextureMaterial {
        TextureMaterial::blended(0x1234, 0x0105, (90, 100, 110), BlendMode::Add)
            .with_texture_window(TextureWindow::new(1, 2, 3, 4))
    }

    #[test]
    fn fill_rect_matches_the_old_words() {
        let old = [gp0::fill_rect(9, 8, 7), pack_xy(16, 32), pack_xy(320, 240)];
        assert_eq!(
            payload(&FillRect::new((16, 32), (320, 240), (9, 8, 7))),
            old
        );
    }

    #[test]
    fn flat_and_gouraud_triangles_match_the_old_words() {
        let flat = [
            gp0::polygon_opcode(false, false, false, false, false) | pack_color(9, 8, 7),
            pack_vertex(1, -2),
            pack_vertex(300, 4),
            pack_vertex(-5, 239),
        ];
        assert_eq!(payload(&TriFlat::new(VERTS3, 9, 8, 7)), flat);
        let blended_op = gp0::polygon_opcode(false, false, false, true, false);
        assert_eq!(
            payload(&TriFlat::new(VERTS3, 9, 8, 7).translucent())[0],
            blended_op | pack_color(9, 8, 7)
        );

        let colors = [(1, 2, 3), (4, 5, 6), (7, 8, 9)];
        let gouraud = [
            gp0::polygon_opcode(true, false, false, false, false) | pack_color(1, 2, 3),
            pack_vertex(1, -2),
            pack_color(4, 5, 6),
            pack_vertex(300, 4),
            pack_color(7, 8, 9),
            pack_vertex(-5, 239),
        ];
        assert_eq!(payload(&TriGouraud::new(VERTS3, colors)), gouraud);
        assert_eq!(
            payload(&TriGouraud::new(VERTS3, colors).translucent())[0],
            gp0::polygon_opcode(true, false, false, true, false) | pack_color(1, 2, 3)
        );
    }

    #[test]
    fn lines_match_the_old_words() {
        let mono = [
            0x4000_0000 | pack_color(9, 8, 7),
            pack_vertex(1, 2),
            pack_vertex(3, 4),
        ];
        assert_eq!(payload(&LineMono::new(1, 2, 3, 4, 9, 8, 7)), mono);
        assert_eq!(
            payload(&LineMono::new(1, 2, 3, 4, 9, 8, 7).translucent())[0],
            0x4200_0000 | pack_color(9, 8, 7)
        );
        let gouraud = [
            0x5000_0000 | pack_color(1, 2, 3),
            pack_vertex(-1, 2),
            pack_color(4, 5, 6),
            pack_vertex(3, -4),
        ];
        assert_eq!(
            payload(&LineGouraud::new((-1, 2), (1, 2, 3), (3, -4), (4, 5, 6))),
            gouraud
        );
    }

    #[test]
    fn flat_quads_and_rects_match_the_old_words() {
        let quad = [
            gp0::polygon_opcode(false, true, false, false, false) | pack_color(9, 8, 7),
            pack_vertex(1, 2),
            pack_vertex(100, 3),
            pack_vertex(4, 90),
            pack_vertex(101, 91),
        ];
        assert_eq!(payload(&QuadFlat::new(VERTS4, 9, 8, 7)), quad);
        // draw_rect_flat: corners (x, y), (x + w, y), (x, y + h), (x + w, y + h).
        let rect = QuadFlat::new([(10, 20), (50, 20), (10, 25), (50, 25)], 1, 2, 3);
        assert_eq!(QuadFlat::rect((10, 20), (40, 5), (1, 2, 3)), rect);
    }

    #[test]
    fn textured_packets_match_the_old_words() {
        let m = material();
        let quad = [
            m.texture_window_word(),
            m.flat_textured_polygon_header(true),
            pack_vertex(1, 2),
            pack_texcoord(0, 1, m.clut_word()),
            pack_vertex(100, 3),
            pack_texcoord(63, 2, m.texture_page_word()),
            pack_vertex(4, 90),
            pack_texcoord(4, 255, 0),
            pack_vertex(101, 91),
            pack_texcoord(200, 100, 0),
        ];
        assert_eq!(
            payload(&QuadTexturedMaterial::with_material(VERTS4, UVS4, m)),
            quad
        );

        let tri = [
            m.texture_window_word(),
            m.flat_textured_polygon_header(false),
            pack_vertex(1, -2),
            pack_texcoord(0, 1, m.clut_word()),
            pack_vertex(300, 4),
            pack_texcoord(63, 2, m.texture_page_word()),
            pack_vertex(-5, 239),
            pack_texcoord(4, 255, 0),
        ];
        assert_eq!(payload(&TriTextured::with_material(VERTS3, UVS3, m)), tri);

        let c = COLORS4;
        let gouraud = [
            m.texture_window_word(),
            m.textured_polygon_command(true, true) | pack_color(c[0].0, c[0].1, c[0].2),
            pack_vertex(1, 2),
            pack_texcoord(0, 1, m.clut_word()),
            pack_color(c[1].0, c[1].1, c[1].2),
            pack_vertex(100, 3),
            pack_texcoord(63, 2, m.texture_page_word()),
            pack_color(c[2].0, c[2].1, c[2].2),
            pack_vertex(4, 90),
            pack_texcoord(4, 255, 0),
            pack_color(c[3].0, c[3].1, c[3].2),
            pack_vertex(101, 91),
            pack_texcoord(200, 100, 0),
        ];
        assert_eq!(
            payload(&QuadTexturedGouraud::with_material(VERTS4, UVS4, c, m)),
            gouraud
        );

        let sprite = [
            m.textured_rect_header(),
            pack_vertex(-3, 4),
            pack_texcoord(5, 6, m.clut_word()),
            pack_xy(16, 24),
        ];
        assert_eq!(
            payload(&Sprite::with_material(-3, 4, 16, 24, (5, 6), m)),
            sprite
        );
    }
}
