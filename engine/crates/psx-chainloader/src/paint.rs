//! Diagnostic painting for the chain-load blob: exact-pixel rectangles
//! and a small text renderer built out of them.
//!
//! The first debug burn drew panels with GP0(02h) FillVram, whose X
//! coordinate and width snap to 16-pixel steps on hardware: shapes fused
//! and grids slid off their own columns. Everything here goes through
//! GP0(60h) monochrome rectangles instead, which honour exact
//! coordinates, at the cost of needing the drawing area configured after
//! the GPU reset `quiesce()` performs.

use psx_font::fonts::basic::BASIC_BITMAP;

/// GP0/GP1 ports.
const GP1: u32 = 0x1F80_1814;

/// One GP0 packet: wait for GPUSTAT bit 26 (ready for a new command)
/// once, then write every word of the packet back to back.
///
/// Both halves of that are silicon lessons. Unpaced bursts of hundreds
/// of words overflowed the FIFO and desynced the command stream into
/// garbage rectangles (first debug burn). But pacing EVERY word is as
/// wrong the other way: bit 26 drops while the GPU ingests the rest of
/// a multi-word packet, so a per-word wait spins its whole bound before
/// each vertex word -- the 2026-08-01 recording shows the fail panel
/// painting at seconds per cell because of exactly that, while the
/// emulator forces the bit on and renders it instantly. One wait per
/// packet is the protocol the SDK uses for every draw the launcher
/// makes on this same console. Bounded so a dead GPU cannot hang the
/// panel that is trying to report on it.
fn gp0_packet(words: &[u32]) {
    // Preserve best-effort diagnostics on timeout; no reset or unbounded wait.
    let _ = psx_io::gpu::try_wait_cmd_ready(1_000_000);
    for &word in words {
        psx_io::gpu::write_gp0(word);
    }
}

/// Configure drawing after a GP1(00h) reset: drawing area covering the
/// whole displayed framebuffer, zero offset, display area at 0,0.
pub fn setup(right: u32, bottom: u32) {
    unsafe { psx_io::write32(GP1, 0x0300_0001) }; // display off while painting

    // Program the display the way the launcher's gpu::init does, rather
    // than inheriting whatever GP1(00h) reset leaves behind. Without
    // these the loader's 320x240 image was shown through the reset
    // defaults, so a bar centred in the framebuffer landed off-centre on
    // the TV. GP1(08h) 320x240 NTSC, GP1(06h) X 0x260..0x260+320*8,
    // GP1(07h) Y 0x10..0x10+240 -- the standard centred NTSC picture.
    unsafe {
        psx_io::write32(GP1, 0x0800_0001); // display mode: 320x240, NTSC
        psx_io::write32(GP1, 0x0600_0000 | 0x260 | ((0x260 + 320 * 8) << 12));
        psx_io::write32(GP1, 0x0700_0000 | 0x10 | ((0x10 + 240) << 10));
        psx_io::write32(GP1, 0x0500_0000); // display area starts at VRAM 0,0
    }
    gp0_packet(&[0xE100_0000]); // texpage/draw-mode defaults
    gp0_packet(&[0xE300_0000]);
    gp0_packet(&[0xE400_0000 | (bottom << 10) | right]);
    gp0_packet(&[0xE500_0000]);
}

/// Enable the configured display.
pub fn show() {
    unsafe { psx_io::write32(GP1, 0x0300_0000) }; // display on
}

/// Select one of the two vertically stacked 320x240 loading buffers.
/// Coordinates sent afterwards stay screen-relative because the draw offset
/// supplies the VRAM row.
pub fn set_draw_buffer(y: i16) {
    let top = y as u32 & 0x1ff;
    let bottom = (y as u32 + 239) & 0x1ff;
    gp0_packet(&[0xE300_0000 | (top << 10)]);
    gp0_packet(&[0xE400_0000 | (bottom << 10) | 319]);
    gp0_packet(&[0xE500_0000 | ((y as u32 & 0x7ff) << 11)]);
}

fn display_buffer(y: i16) {
    unsafe { psx_io::write32(GP1, 0x0500_0000 | ((y as u32 & 0x1ff) << 10)) };
}

/// Show a completed loading buffer exactly at a fresh VBlank edge. I_STAT is
/// sticky, so clear any old edge before waiting. Otherwise a long CD read can
/// leave the bit set and make an apparently guarded flip happen mid-scanout.
pub fn present_at_next_vblank(y: i16) {
    ack_vblank();
    while !vblank_pending() {}
    display_buffer(y);
    ack_vblank();
}

/// Restore the first buffer for a failure checklist.
pub fn diagnostic_mode() {
    display_buffer(0);
    set_draw_buffer(0);
}

/// Whether the interrupt controller has latched a VBlank edge. Interrupts
/// remain masked while the loader runs; the pending bit can still be polled.
pub fn vblank_pending() -> bool {
    psx_io::irq::stat() & (1 << psx_io::irq::source::VBLANK) != 0
}

/// Acknowledge the pending VBlank edge.
pub fn ack_vblank() {
    psx_io::irq::ack(1 << psx_io::irq::source::VBLANK);
}

/// Wait until every submitted primitive has drained before presenting its
/// buffer. GPUSTAT bit 28 is the same idle gate used by the SDK's deferred
/// framebuffer flip.
pub fn draw_sync() {
    let _ = psx_io::gpu::try_wait_dma_ready(1_000_000);
}

/// Whether the diagnostic checklist has been revealed.
///
/// The screen itself is always on now -- a clean load shows a loading
/// bar rather than the dark screen that read as a hung console. This
/// flag only decides whether the STAGE CHECKLIST is drawn, which stays
/// out of the way until something fails. Deliberately initialised
/// non-zero: a zero-initialised static would land in .bss, and loader.ld
/// forbids .bss because nothing zeroes it for the blob.
static mut CHECKLIST_HIDDEN: u8 = 1;

/// Return whether a load failure revealed the checklist.
pub fn checklist_shown() -> bool {
    unsafe { core::ptr::read_volatile(&raw const CHECKLIST_HIDDEN) == 0 }
}

/// Reveal the checklist for this copied blob instance.
pub fn show_checklist() {
    unsafe { core::ptr::write_volatile(&raw mut CHECKLIST_HIDDEN, 0) }
}

/// Solid rectangle at exact pixel coordinates. `rgb` is `0xBBGGRR`.
pub fn rect(x: i16, y: i16, w: i16, h: i16, rgb: u32) {
    gp0_packet(&[
        0x6000_0000 | rgb,
        ((y as u32) << 16) | (x as u32 & 0xFFFF),
        ((h as u32) << 16) | (w as u32 & 0xFFFF),
    ]);
}

fn packed_rgb((r, g, b): (u8, u8, u8)) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

/// One Gouraud triangle for the procedural globe.
pub fn tri_gouraud(verts: [(i16, i16); 3], colors: [(u8, u8, u8); 3]) {
    gp0_packet(&[
        0x3000_0000 | packed_rgb(colors[0]),
        ((verts[0].1 as u32) << 16) | (verts[0].0 as u32 & 0xffff),
        packed_rgb(colors[1]),
        ((verts[1].1 as u32) << 16) | (verts[1].0 as u32 & 0xffff),
        packed_rgb(colors[2]),
        ((verts[2].1 as u32) << 16) | (verts[2].0 as u32 & 0xffff),
    ]);
}

/// Bright diagnostic text and payload bar color.
pub const WHITE: u32 = 0x00FF_FFFF;
/// Successful stage color.
pub const GREEN: u32 = 0x0000_D000;
/// Failure and warning color.
pub const YELLOW: u32 = 0x0000_D8FF;
/// Pending / de-emphasised text. Light enough to survive a phone photo
/// of a CRT, dark enough to read as "not yet".
pub const DIM: u32 = 0x0080_8080;
/// Diagnostic background color.
pub const RED_BASE: u32 = 0x0000_0040;

/// The SDK's 8x8 public-domain font (dhepper font8x8), reused as plain
/// data: glyphs are drawn as runs of GP0 rectangles, so the blob needs
/// no VRAM upload and leaves no texture state behind for the game.
static FONT: [u8; 1024] = BASIC_BITMAP;

/// Draw `s` at (x, y), every glyph pixel a `scale`-square rectangle.
/// Returns the x just past the text, so calls chain along one line.
pub fn text(x: i16, y: i16, scale: i16, s: &str, rgb: u32) -> i16 {
    text_bytes(x, y, scale, s.as_bytes(), rgb)
}

/// Draw byte glyphs and return the next horizontal coordinate.
pub fn text_bytes(mut x: i16, y: i16, scale: i16, s: &[u8], rgb: u32) -> i16 {
    for &b in s {
        glyph(x, y, scale, b, rgb);
        x += 8 * scale;
    }
    x
}

/// `v` as eight hex digits. Returns the x just past them.
pub fn hex32(mut x: i16, y: i16, scale: i16, v: u32, rgb: u32) -> i16 {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for i in 0..8 {
        glyph(x, y, scale, HEX[((v >> (28 - 4 * i)) & 0xF) as usize], rgb);
        x += 8 * scale;
    }
    x
}

/// One glyph as horizontal runs of set pixels, one rectangle per run:
/// a fifth of the GP0 traffic of a rect per pixel, on a port every word
/// of which is paced against GPUSTAT.
fn glyph(x: i16, y: i16, scale: i16, code: u8, rgb: u32) {
    let rows = &FONT[(code as usize & 0x7F) * 8..][..8];
    for (row, bits) in rows.iter().enumerate() {
        let mut col: u32 = 0;
        while col < 8 {
            if bits & (1 << col) == 0 {
                col += 1;
                continue;
            }
            let start = col;
            while col < 8 && bits & (1 << col) != 0 {
                col += 1;
            }
            // Bit 0 is the leftmost pixel (the font8x8 convention).
            rect(
                x + start as i16 * scale,
                y + row as i16 * scale,
                (col - start) as i16 * scale,
                scale,
                rgb,
            );
        }
    }
}
