//! Direct scaled text from PSoXide's clean, proportional game font. This is
//! used for modal overlays where uploading a second generic font atlas would
//! trample gameplay textures resident in VRAM.
//!
//! The drawing functions take the port's [`GpuText`], the three GPU commands the text needs,
//! so each port keeps its own capture or direct path for them; the sink is a zero-sized type
//! and every call is resolved at compile time.

use psx_font::fonts::BASIC_8X16;
use psx_gpu::material::TextureMaterial;
use psx_vram::{upload_16bpp, upload_bytes, Clut, TextureDepth, TexturePage, VramRect};

/// The three GPU commands the text drawing needs. A port implements them on a zero-sized type
/// that routes to its own command path.
pub trait GpuText {
    /// A flat, opaque quad.
    fn quad_flat(&self, verts: [(i16, i16); 4], r: u8, g: u8, b: u8);
    /// A textured quad.
    fn quad_textured_material(
        &self,
        verts: [(i16, i16); 4],
        uvs: [(u8, u8); 4],
        material: TextureMaterial,
    );
    /// A textured rectangle of `w` by `h` at `(x, y)`.
    fn sprite_textured_material(
        &self,
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        uv: (u8, u8),
        material: TextureMaterial,
    );
}

pub const SMALL_Q8: u16 = 256; // keep thin strokes intact at the native 8x16 size

// The native HUD packer reserves u=160..255, v=0..127 in the last 4-bit
// gameplay tpage. That rectangle holds all 95 printable ASCII glyphs at the
// font's native 8x16 resolution: 12 columns x 8 rows. Keeping the source pixels
// intact lets the common title path use the same 1:1 sampling as the menu font;
// fractional PS1 texture sampling and hand-compacted glyphs both damage thin
// strokes. HUD weapon icons may use the right strip below v=127.
const GAME_TPAGE_X: u16 = 960;
const GAME_TPAGE_Y: u16 = 256;
const GAME_ATLAS_U: u8 = 160;
const GAME_CELL_W: usize = 8;
const GAME_CELL_H: usize = 16;
const GAME_COLS: usize = 12;
const GAME_ATLAS_W: usize = 96;
const GAME_ROW_BYTES: usize = GAME_ATLAS_W * GAME_CELL_H / 2;
const GAME_CLUT_X: u16 = 976; // immediately after the HUD's x=960 CLUT
const GAME_CLUT_Y: u16 = 504;
const FIRST_PRINTABLE: usize = 32;
const PRINTABLE_GLYPHS: usize = 95;
/// Match the menu atlas' native fixed-width metrics.
const GAMEPLAY_TRACKING_PX: i16 = 0;

fn glyph_w() -> u8 {
    BASIC_8X16.glyph_w
}

fn glyph_h() -> u8 {
    BASIC_8X16.glyph_h
}

fn glyph_count() -> usize {
    PRINTABLE_GLYPHS
}

fn bitmap() -> &'static [u8] {
    BASIC_8X16.bitmap
}

fn row_bytes() -> usize {
    BASIC_8X16.row_bytes()
}

fn glyph_index(ch: char) -> Option<usize> {
    let cp = ch as u32;
    let first = FIRST_PRINTABLE as u32;
    let end = first + PRINTABLE_GLYPHS as u32;
    if cp >= first && cp < end {
        Some((cp - first) as usize)
    } else {
        None
    }
}

fn glyph_advance(ch: char) -> u8 {
    BASIC_8X16.glyph_advance(ch)
}

fn scale_i16(v: i16, scale_q8: u16) -> i16 {
    ((v as i32 * scale_q8 as i32 + 128) >> 8) as i16
}

#[inline(always)]
fn set_4bpp_pixel(dst: &mut [u8], stride_px: usize, x: usize, y: usize) {
    let p = y * stride_px + x;
    let b = &mut dst[p >> 1];
    if p & 1 == 0 {
        *b |= 1;
    } else {
        *b |= 0x10;
    }
}

/// Upload the native 8x16 gameplay font atlas into the HUD tpage's unused
/// strip. The temporary row is stack-only; resident main-RAM cost is zero.
#[optimize(size)]
pub fn upload_gameplay_atlas() {
    let gw = glyph_w();
    let gh = glyph_h();
    let src_row_bytes = row_bytes();
    let src = bitmap();
    let mut row_pixels = [0u8; GAME_ROW_BYTES];
    let rows = glyph_count().div_ceil(GAME_COLS);

    let mut atlas_row = 0usize;
    while atlas_row < rows {
        row_pixels.fill(0);
        let mut atlas_col = 0usize;
        while atlas_col < GAME_COLS {
            let gi = atlas_row * GAME_COLS + atlas_col;
            if gi >= glyph_count() {
                break;
            }
            let base = (FIRST_PRINTABLE + gi) * src_row_bytes * gh as usize;
            let cell_x = atlas_col * GAME_CELL_W;
            let mut row = 0u8;
            while row < gh {
                let mut col = 0u8;
                while col < gw {
                    let byte = src[base + row as usize * src_row_bytes + col as usize / 8];
                    let mask = 0x80u8 >> (col & 7);
                    if byte & mask != 0 {
                        set_4bpp_pixel(
                            &mut row_pixels,
                            GAME_ATLAS_W,
                            cell_x + col as usize,
                            row as usize,
                        );
                    }
                    col += 1;
                }
                row += 1;
            }
            atlas_col += 1;
        }
        upload_bytes(
            VramRect::new(
                GAME_TPAGE_X + GAME_ATLAS_U as u16 / 4,
                GAME_TPAGE_Y + (atlas_row * GAME_CELL_H) as u16,
                (GAME_ATLAS_W / 4) as u16,
                GAME_CELL_H as u16,
            ),
            &row_pixels,
        );
        atlas_row += 1;
    }

    // Index 0 remains transparent. Index 1 is exact mid-grey (RGB5=16):
    // normal PS1 texture modulation by an 8-bit tint `g` yields
    // floor(16*g/128) == g>>3, exactly matching flat-primitive colour
    // conversion while keeping even a black fade endpoint nontransparent.
    let mut clut = [0u16; 16];
    clut[1] = 0x4210;
    upload_16bpp(VramRect::new(GAME_CLUT_X, GAME_CLUT_Y, 16, 1), &clut);
}

/// Draw the clean gameplay face through the compact atlas (one transparent
/// textured primitive per non-space character instead of one flat primitive
/// per bitmap run). The normal 1:1 title size uses the same native textured
/// rectangle command as the menu font. Narrower emergency scales use quads
/// only for authored lines which exceed the safe display width.
pub fn draw_text_gameplay_scaled<S: GpuText>(
    sink: &S,
    x: i16,
    y: i16,
    text: &str,
    scale_q8: u16,
    color: (u8, u8, u8),
) {
    let tp = TexturePage::new(GAME_TPAGE_X, GAME_TPAGE_Y, TextureDepth::Bit4);
    let cl = Clut::new(GAME_CLUT_X, GAME_CLUT_Y);
    let mat = TextureMaterial::opaque(cl.uv_word(), tp.uv_word(0), color);
    let mut cx = x;
    for ch in text.chars() {
        if let Some(index) = glyph_index(ch) {
            if ch != ' ' {
                let u0 = GAME_ATLAS_U as usize + (index % GAME_COLS) * GAME_CELL_W;
                let v0 = (index / GAME_COLS) * GAME_CELL_H;
                if scale_q8 == 256 {
                    sink.sprite_textured_material(
                        cx,
                        y,
                        GAME_CELL_W as u16,
                        GAME_CELL_H as u16,
                        (u0 as u8, v0 as u8),
                        mat,
                    );
                    cx += glyph_advance(ch) as i16 + GAMEPLAY_TRACKING_PX;
                    continue;
                }
                let x0 = cx;
                let y0 = y;
                let x1 = x0 + scale_i16(glyph_w() as i16, scale_q8).max(1);
                let y1 = y0 + scale_i16(glyph_h() as i16, scale_q8).max(1);
                let u1 = (u0 + GAME_CELL_W).min(u8::MAX as usize);
                let v1 = (v0 + GAME_CELL_H).min(u8::MAX as usize);
                sink.quad_textured_material(
                    [(x0, y0), (x1, y0), (x0, y1), (x1, y1)],
                    [
                        (u0 as u8, v0 as u8),
                        (u1 as u8, v0 as u8),
                        (u0 as u8, v1 as u8),
                        (u1 as u8, v1 as u8),
                    ],
                    mat,
                );
            }
        }
        cx += scale_i16(glyph_advance(ch) as i16, scale_q8) + GAMEPLAY_TRACKING_PX;
    }
}

/// Width of the tracked gameplay-title face. Keep this separate from
/// `text_width_scaled`: menus deliberately retain the font's native metrics.
pub fn text_width_gameplay_scaled(text: &str, scale_q8: u16) -> i16 {
    let mut width = 0i16;
    let mut first = true;
    for ch in text.chars() {
        if !first {
            width += GAMEPLAY_TRACKING_PX;
        }
        width += scale_i16(glyph_advance(ch) as i16, scale_q8);
        first = false;
    }
    width
}

pub fn line_height_scaled(scale_q8: u16) -> i16 {
    scale_i16(glyph_h() as i16 + 2, scale_q8).max(1)
}

pub fn text_width_scaled(text: &str, scale_q8: u16) -> i16 {
    let mut width = 0i16;
    for ch in text.chars() {
        width += scale_i16(glyph_advance(ch) as i16, scale_q8);
    }
    width
}

#[optimize(size)]
pub fn draw_text_scaled<S: GpuText>(
    sink: &S,
    x: i16,
    y: i16,
    text: &str,
    scale_q8: u16,
    color: (u8, u8, u8),
) {
    let gw = glyph_w();
    let gh = glyph_h();
    let row_bytes = row_bytes();
    let bitmap = bitmap();
    let mut cx = x;

    for ch in text.chars() {
        if let Some(index) = glyph_index(ch) {
            let base = (FIRST_PRINTABLE + index) * row_bytes * gh as usize;
            let mut row = 0u8;
            while row < gh {
                let mut col = 0u8;
                while col < gw {
                    let byte = bitmap[base + row as usize * row_bytes + (col as usize / 8)];
                    let mask = 0x80u8 >> (col & 7);
                    if byte & mask == 0 {
                        col += 1;
                        continue;
                    }

                    let run_start = col;
                    while col < gw {
                        let byte = bitmap[base + row as usize * row_bytes + (col as usize / 8)];
                        let mask = 0x80u8 >> (col & 7);
                        if byte & mask == 0 {
                            break;
                        }
                        col += 1;
                    }

                    let x0 = cx + scale_i16(run_start as i16, scale_q8);
                    let mut x1 = cx + scale_i16(col as i16, scale_q8);
                    let y0 = y + scale_i16(row as i16, scale_q8);
                    let mut y1 = y + scale_i16(row as i16 + 1, scale_q8);
                    if x1 <= x0 {
                        x1 = x0 + 1;
                    }
                    if y1 <= y0 {
                        y1 = y0 + 1;
                    }
                    sink.quad_flat(
                        [(x0, y0), (x1, y0), (x0, y1), (x1, y1)],
                        color.0,
                        color.1,
                        color.2,
                    );
                }
                row += 1;
            }
        }
        cx += scale_i16(glyph_advance(ch) as i16, scale_q8);
    }
}

pub fn draw_centered_scaled<S: GpuText>(
    sink: &S,
    y: i16,
    text: &str,
    scale_q8: u16,
    color: (u8, u8, u8),
) {
    draw_text_scaled(
        sink,
        160 - text_width_scaled(text, scale_q8) / 2,
        y,
        text,
        scale_q8,
        color,
    );
}

/// `hltext` as a [`psx_font::TextSink`], so widgets written against the SDK
/// trait can draw through the in-game atlas exactly as they do through a
/// `FontAtlas`. The scale is fixed per instance because a widget positions rows
/// by `line_height` and must not have that change under it mid-list.
pub struct Sink<G> {
    gpu: G,
    scale_q8: u16,
}

impl<G: GpuText> Sink<G> {
    /// A sink drawing through `gpu` at `scale_q8`.
    pub const fn new(gpu: G, scale_q8: u16) -> Self {
        Self { gpu, scale_q8 }
    }
}

impl<G: GpuText> psx_font::TextSink for Sink<G> {
    fn draw(&self, x: i16, y: i16, text: &str, tint: (u8, u8, u8)) {
        draw_text_scaled(&self.gpu, x, y, text, self.scale_q8, tint);
    }

    fn width(&self, text: &str) -> i16 {
        text_width_scaled(text, self.scale_q8)
    }

    fn line_height(&self) -> i16 {
        // The atlas is 8x16; rows are spaced by the scaled glyph height plus the
        // 4px gap the pause pages already use, so existing layouts are unchanged.
        ((16 * self.scale_q8 as i32) >> 8) as i16 + 4
    }
}
