//! Bitmap-font atlases for the PS1.
//!
//! One crate, two types, three steps:
//!
//! 1. Declare (or pick) a [`BitmapFont`] -- a `static` descriptor
//!    that carries glyph dimensions, a 1-bit-per-pixel bitmap, and
//!    layout metadata (advance, line height, bit order). The
//!    built-in fonts in [`fonts`] cover Public-Domain IBM-VGA-style
//!    8×8 for ASCII / Latin-1 / box-drawing.
//!
//! 2. Upload it into VRAM once with [`FontAtlas::upload`] -- the
//!    crate picks a sensible atlas layout, expands the 1bpp source
//!    into a 4bpp CLUT texture, uploads that + a two-entry CLUT
//!    (transparent + white), and returns a handle.
//!
//! 3. Call one of the [`FontAtlas`] draw methods every frame. The
//!    same atlas supports both the fast rectangle path and the
//!    flexible quad path -- choose based on what the call needs.
//!
//! ## Draw-path cheat sheet
//!
//! | Method | Hardware | Cost / glyph | Does |
//! |---|---|---|---|
//! | [`FontAtlas::draw_text`] | GP0 0x64 textured rect | 4 words | 1:1 axis-aligned, single tint |
//! | [`FontAtlas::draw_text_scaled`] | GP0 0x2C textured quad | 9 words | Integer scale (2×, 3×, …), single tint |
//! | [`FontAtlas::draw_text_scaled_q8`] | GP0 0x2C textured quad | 9 words | Q8 fractional scale (1.5× = 384), single tint |
//! | [`FontAtlas::draw_text_rotated`] | GP0 0x2C textured quad | 9 words | Arbitrary angle rotation, single tint |
//! | [`FontAtlas::draw_text_affine`] | GP0 0x2C textured quad | 9 words | Arbitrary 2×2 matrix, single tint |
//! | [`FontAtlas::draw_text_gradient`] | GP0 0x3C gouraud-textured quad | 12 words | 1:1, top/bottom gradient |
//! | [`FontAtlas::draw_text_scaled_gradient`] | GP0 0x3C gouraud-textured quad | 12 words | Scaled + top/bottom gradient |
//!
//! `draw_text` is the fast default. Everything below it is a quad
//! primitive that pays ~2–3× the GP0 bandwidth for transforms or
//! per-corner colour. At PS1 scale, this matters for text-heavy UIs
//! (credit crawls, RPG dialogue walls) and doesn't for a HUD of a
//! few dozen glyphs. Callers pick consciously.
//!
//! All methods share the same atlas and CLUT -- no duplicate VRAM.
//! `draw_text_*` variants can freely mix in one frame, and tints
//! compose with the PSX per-texel multiplier (`output = texel *
//! tint / 128`).
//!
//! ## Generic over glyph dimensions
//!
//! Nothing in [`BitmapFont`] or [`FontAtlas`] hard-codes 8×8.
//! Declare a second font at 6×12 or 8×16, drop it into VRAM at a
//! different tpage, and you can mix sizes on the same screen.
//! What's size-dependent:
//!
//! - **Atlas layout**: the uploader picks `glyphs_per_row` so the
//!   atlas fits within its tpage.
//! - **Upload buffer**: the stack-only scratch buffer inside
//!   [`FontAtlas::upload`] is 16 KiB, enough for 128 glyphs at 16×16
//!   or 64 glyphs at 32×16. Larger atlases panic today; use
//!   [`upload_fonts`] with caller-owned scratch for them.
//!
//! ## Why 4bpp and not 15bpp direct
//!
//! 4bpp uses 1/4 the VRAM of 15bpp and opens the standard PSX
//! "colour a monochrome glyph via tint" trick. A 96-glyph ASCII
//! 8×8 atlas fits in 6×8 halfwords = 96 halfwords = 192 bytes of
//! VRAM. Direct-15bpp would be 768 bytes, and you'd lose the free
//! recolouring.
//!
//! ## Coordinate conventions
//!
//! - `draw_text` / `draw_text_scaled` / `draw_text_gradient`:
//!   `(x, y)` is the **top-left** of the string's first glyph.
//! - `draw_text_rotated`: `(cx, cy)` is the rotation **pivot** --
//!   the centre of the baseline. Positive angles rotate
//!   counter-clockwise in screen coords.
//! - `draw_text_affine`: `origin` is the point the 2×2 transform
//!   maps to the top-left of the first glyph. The matrix is applied
//!   in glyph-local space before translating to `origin`.
//!
//! ## Fixed-point conventions (rotation + affine)
//!
//! Rotation uses a Q0.12 angle: `u16` in `[0, 4096)` mapping to
//! `[0°, 360°)`. Sin/cos come from the shared SDK sin LUT in
//! [`psx_math::sincos`] -- see that crate for the precision /
//! Q-format specifics.
//!
//! Affine matrices are Q3.12 -- `i16` with 12 fractional bits, so
//! `4096` = 1.0, `-4096` = -1.0, `8192` = 2.0, and the usable
//! range is `±7.999…`. That's enough headroom for any visually
//! reasonable 2×2 transform a bitmap font would want.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use psx_hw::gpu::{gp0, pack_color, pack_texcoord, pack_vertex, pack_xy};
use psx_io::gpu::{wait_cmd_ready, write_gp0};
use psx_math::sincos;
use psx_vram::{
    upload_16bpp, upload_clut, Clut, Color555, TexDepth, Tpage, VramHandle, VramRect,
    VramRegionSource,
};

pub mod fonts;
pub mod hex;

pub use hex::{u16_hex, HexU16};

// ======================================================================
// BitmapFont -- the static descriptor
// ======================================================================

/// Bit-packing convention within each bitmap byte.
///
/// - [`BitOrder::Lsb`]: bit 0 of each byte is the **leftmost** pixel.
///   Matches the dhepper/font8x8 convention our built-in fonts use.
/// - [`BitOrder::Msb`]: bit 7 is the leftmost pixel. Matches the IBM
///   VGA BIOS ROM / Linux `fbcon` `font_8x8.c` / GRUB conventions.
///
/// Fonts imported from different sources carry different bit orders;
/// the uploader handles both transparently.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BitOrder {
    /// Bit 0 is the leftmost pixel.
    Lsb,
    /// Bit 7 is the leftmost pixel.
    Msb,
}

/// A bitmap font as a static data descriptor.
///
/// All fields are compile-time constants so fonts can live in
/// `.rodata` and a `BitmapFont` value can be declared as a `const`
/// right next to its bitmap. The type stays size-agnostic -- 6×8,
/// 8×8, 8×16, 12×16 all work; the uploader reads `glyph_w` /
/// `glyph_h` and lays out the atlas from there.
///
/// Glyph `i` in the `bitmap` slice occupies
/// `bitmap[i * row_bytes * glyph_h .. i * row_bytes * glyph_h +
/// row_bytes * glyph_h]`, where `row_bytes = ceil(glyph_w / 8)`.
/// For 8-wide glyphs that's one byte per row. Wider glyphs (12,
/// 16 px) use multiple bytes per row, MSB-first within the row
/// (i.e., byte 0 covers columns 0..=7, byte 1 covers columns
/// 8..=15, regardless of [`BitOrder`] within each byte).
///
/// The `first_char` / `glyph_count` window is a codepoint range --
/// code `c` is looked up at offset `c - first_char` in the bitmap
/// as long as `first_char <= c < first_char + glyph_count`.
/// Anything outside falls back to the missing-glyph box.
#[derive(Copy, Clone, Debug)]
pub struct BitmapFont {
    /// Glyph cell width in pixels.
    pub glyph_w: u8,
    /// Glyph cell height in pixels.
    pub glyph_h: u8,
    /// First codepoint covered. For a basic-Latin font this is
    /// usually `0x00` (null) or `0x20` (space).
    pub first_char: u16,
    /// Number of glyphs in this font. The range
    /// `first_char..first_char + glyph_count` is the supported set.
    pub glyph_count: u16,
    /// Row-major bitmap bytes. `glyph_count * glyph_h *
    /// ceil(glyph_w / 8)` bytes total.
    pub bitmap: &'static [u8],
    /// Optional per-glyph pixel advances. Proportional TTF imports fill this;
    /// fixed-cell bitmap fonts leave it `None` and use `advance_x`.
    pub glyph_advances: Option<&'static [u8]>,
    /// Pixel step between adjacent characters on a text line.
    /// Usually `== glyph_w` for fixed-width fonts. Proportional fonts use this
    /// as the fallback for missing/out-of-range glyphs.
    pub advance_x: u8,
    /// Pixel step between text lines. Usually `== glyph_h`.
    pub line_height: u8,
    /// Bit packing within each bitmap byte.
    pub bit_order: BitOrder,
}

impl BitmapFont {
    /// Bytes per row of a single glyph in the source bitmap.
    /// Derived from [`BitmapFont::glyph_w`] -- 1 byte for ≤ 8-wide
    /// fonts, 2 bytes for 9..16-wide, etc.
    pub const fn row_bytes(&self) -> usize {
        (self.glyph_w as usize).div_ceil(8)
    }

    /// Total bitmap bytes per glyph.
    pub const fn glyph_stride(&self) -> usize {
        self.row_bytes() * self.glyph_h as usize
    }

    /// Glyph index for `ch` if the font covers its codepoint.
    pub fn glyph_index(&self, ch: char) -> Option<u16> {
        let cp = ch as u32;
        let first = self.first_char as u32;
        let end = first.saturating_add(self.glyph_count as u32);
        if cp >= first && cp < end {
            Some((cp - first) as u16)
        } else {
            None
        }
    }

    /// Pixel advance for `ch`, falling back to [`BitmapFont::advance_x`] when
    /// the font has no per-glyph table or `ch` is outside the covered range.
    pub fn glyph_advance(&self, ch: char) -> u8 {
        self.glyph_index(ch)
            .and_then(|index| {
                self.glyph_advances
                    .and_then(|advances| advances.get(index as usize).copied())
            })
            .unwrap_or(self.advance_x)
    }

    /// Pixel width of `text` with this font's advance metrics.
    pub fn text_width(&self, text: &str) -> u16 {
        text.chars()
            .map(|ch| self.glyph_advance(ch) as u16)
            .fold(0u16, u16::saturating_add)
    }

    /// Pixel width of `text` with an extra signed spacing value inserted
    /// between adjacent characters. The spacing is in final screen pixels,
    /// so callers that scale glyphs can decide whether or not to scale the
    /// spacing separately.
    pub fn text_width_with_spacing(&self, text: &str, letter_spacing: i8) -> u16 {
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            return 0;
        };
        let mut width = i32::from(self.glyph_advance(first));
        for ch in chars {
            width = width
                .saturating_add(i32::from(letter_spacing))
                .saturating_add(i32::from(self.glyph_advance(ch)));
        }
        width.clamp(0, i32::from(u16::MAX)) as u16
    }

    /// Fetch row `r` of glyph `i` as 4 bytes LSB-packed little-
    /// endian -- the row comes out as a `u32` with pixel 0 in bit
    /// 0, pixel 1 in bit 1, … up to pixel 31 (enough for any
    /// realistic cell width). Handles [`BitOrder`] normalisation
    /// in one place so callers don't have to branch.
    pub fn glyph_row_packed(&self, i: u16, r: u8) -> u32 {
        let base = i as usize * self.glyph_stride() + r as usize * self.row_bytes();
        let mut out: u32 = 0;
        for byte_idx in 0..self.row_bytes() {
            let raw = self.bitmap[base + byte_idx] as u32;
            let normalised = match self.bit_order {
                BitOrder::Lsb => raw,
                BitOrder::Msb => raw.reverse_bits() >> 24,
            };
            out |= normalised << (byte_idx * 8);
        }
        out
    }
}

// ======================================================================
// FontAtlas -- the VRAM handle
// ======================================================================

/// A [`BitmapFont`] installed in VRAM and ready to draw from.
///
/// Hold on to the returned handle for the lifetime you want glyphs
/// to stay drawable. Uploading costs ~1 GP0 0xA0 transfer of
/// `glyph_count × glyph_w × glyph_h / 4` bytes (plus 2 halfwords
/// for the CLUT); draw calls cost 4 GP0 words per glyph.
#[derive(Copy, Clone, Debug)]
pub struct FontAtlas {
    font: &'static BitmapFont,
    tpage: Tpage,
    clut: Clut,
    /// How many glyphs per row of the atlas texture. Picked at
    /// upload time so the texture fits within a single 4bpp tpage.
    glyphs_per_row: u16,
}

fn scale_q8_i16(value: i16, scale_q8: u16) -> i16 {
    let scaled = i32::from(value)
        .saturating_mul(i32::from(scale_q8))
        .saturating_add(128)
        >> 8;
    scaled.clamp(1, i32::from(i16::MAX)) as i16
}

fn round_q8_to_i16(value_q8: i32) -> i16 {
    let rounded = if value_q8 >= 0 {
        value_q8.saturating_add(128) >> 8
    } else {
        -(value_q8.saturating_neg().saturating_add(128) >> 8)
    };
    rounded.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[inline]
fn reset_texture_window() {
    wait_cmd_ready();
    write_gp0(gp0::tex_window(0, 0, 0, 0));
}

#[inline]
fn pack_uv_word(u: u8, v: u8, high_word: u16) -> u32 {
    u as u32 | ((v as u32) << 8) | ((high_word as u32) << 16)
}

#[inline]
fn write_textured_quad_packet(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    color_cmd: u32,
    clut_word: u16,
    tpage_word: u16,
) {
    wait_cmd_ready();
    write_gp0(color_cmd);
    write_gp0(pack_vertex(verts[0].0, verts[0].1));
    write_gp0(pack_uv_word(uvs[0].0, uvs[0].1, clut_word));
    write_gp0(pack_vertex(verts[1].0, verts[1].1));
    write_gp0(pack_uv_word(uvs[1].0, uvs[1].1, tpage_word));
    write_gp0(pack_vertex(verts[2].0, verts[2].1));
    write_gp0(pack_uv_word(uvs[2].0, uvs[2].1, 0));
    write_gp0(pack_vertex(verts[3].0, verts[3].1));
    write_gp0(pack_uv_word(uvs[3].0, uvs[3].1, 0));
}

#[inline]
fn write_textured_gouraud_quad_packet(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    color0_cmd: u32,
    clut_word: u16,
    tpage_word: u16,
) {
    wait_cmd_ready();
    write_gp0(color0_cmd);
    write_gp0(pack_vertex(verts[0].0, verts[0].1));
    write_gp0(pack_uv_word(uvs[0].0, uvs[0].1, clut_word));
    write_gp0(pack_color(colors[1].0, colors[1].1, colors[1].2));
    write_gp0(pack_vertex(verts[1].0, verts[1].1));
    write_gp0(pack_uv_word(uvs[1].0, uvs[1].1, tpage_word));
    write_gp0(pack_color(colors[2].0, colors[2].1, colors[2].2));
    write_gp0(pack_vertex(verts[2].0, verts[2].1));
    write_gp0(pack_uv_word(uvs[2].0, uvs[2].1, 0));
    write_gp0(pack_color(colors[3].0, colors[3].1, colors[3].2));
    write_gp0(pack_vertex(verts[3].0, verts[3].1));
    write_gp0(pack_uv_word(uvs[3].0, uvs[3].1, 0));
}

/// VRAM reservations backing a set of fonts uploaded together by
/// [`upload_fonts`]. Free both handles through the allocator to reclaim the
/// VRAM when the owning scene is torn down.
#[derive(Copy, Clone, Debug)]
pub struct FontSetVram {
    /// The contiguous page-run reservation that holds every atlas.
    pub pages: VramHandle,
    /// The shared 2-entry CLUT reservation.
    pub clut: VramHandle,
}

/// Atlas layout for `font`: `(glyphs_per_row, atlas_w_texels, atlas_h_texels,
/// halfwords_per_row)`. Mirrors the layout [`FontAtlas::upload`] computes.
fn font_atlas_dims(font: &BitmapFont) -> (u16, u16, u16, u16) {
    let glyph_w = (font.glyph_w as u16).max(1);
    let glyph_h = font.glyph_h as u16;
    let max_cols = (FontAtlas::MAX_ATLAS_W_TEXELS / glyph_w).max(1);
    let glyphs_per_row = font.glyph_count.min(max_cols).max(1);
    let glyph_rows = font.glyph_count.div_ceil(glyphs_per_row);
    let atlas_w = glyphs_per_row * glyph_w;
    let atlas_h = glyph_rows * glyph_h;
    let halfwords_per_row = atlas_w.div_ceil(4);
    (glyphs_per_row, atlas_w, atlas_h, halfwords_per_row)
}

/// The four corner UVs (texel coords) of a glyph cell whose top-left is
/// `(u, v)` and size is `gw × gh`, with the far edge **saturated at 255**
/// instead of wrapping. Corner order matches the quad-path vertices: top-left,
/// top-right, bottom-left, bottom-right.
///
/// A full-width 256-texel atlas (e.g. 16 columns of 16px glyphs) puts the
/// rightmost column at `u = 240`, so the far U is `240 + 16 = 256`, which
/// wraps to `0` in the `u8` UV field and makes the GPU interpolate U from 240
/// down to 0 across the quad -- smearing that glyph (the classic 256-wide
/// texture right-edge bug). Saturating pins the far edge to the last texel.
fn glyph_quad_uvs(u: u8, v: u8, gw: u8, gh: u8) -> [(u8, u8); 4] {
    let u1 = u.saturating_add(gw);
    let v1 = v.saturating_add(gh);
    [(u, v), (u1, v), (u, v1), (u1, v1)]
}

/// Halfwords spanned by one 4bpp texture page (64 halfwords = 256 texels).
const FONT_PAGE_HALFWORDS: usize = 64;

/// Upload several fonts in a **single** `GP0(A0h)` image transfer.
///
/// Repeated per-font VRAM uploads desync the GPU command stream on some
/// targets and corrupt subsequent (world) rendering; packing every glyph
/// atlas side-by-side into one reserved page run and emitting one transfer
/// avoids that entirely. Each font keeps its own 4bpp tpage (one 64-halfword
/// page column) so glyph UVs stay local; all fonts share one 2-entry CLUT
/// (transparent, white) which `tint` recolours per draw.
///
/// `scratch` is a caller-owned buffer (put it in `static mut` BSS, **not** on
/// the stack); it must hold at least `fonts.len() * 64 * max_atlas_height`
/// halfwords. `out` receives one `Some(FontAtlas)` per font; trailing slots
/// are set to `None`. Returns the handles to free the set later, or `None`
/// (uploading nothing) if a font is too wide, `scratch`/`out` is too small,
/// or the allocator is out of space.
pub fn upload_fonts<R: VramRegionSource>(
    fonts: &[&'static BitmapFont],
    alloc: &mut R,
    scratch: &mut [u16],
    out: &mut [Option<FontAtlas>],
) -> Option<FontSetVram> {
    let n = fonts.len();
    if n == 0 || n > out.len() {
        return None;
    }
    let mut max_h = 0u16;
    for &font in fonts {
        let (_, _, atlas_h, halfwords_per_row) = font_atlas_dims(font);
        if halfwords_per_row as usize > FONT_PAGE_HALFWORDS {
            return None; // a glyph atlas wider than one page can't be paged
        }
        max_h = max_h.max(atlas_h);
    }
    let stride = n * FONT_PAGE_HALFWORDS; // halfwords per combined row
    let total = stride * max_h as usize;
    if max_h == 0 || total > scratch.len() {
        return None;
    }

    let (base, pages) = alloc.alloc_page_run(n as u16, TexDepth::Bit4, 0)?;
    // CLUT allocation can't fail at boot (the band is empty); if it ever does
    // the page run is left reserved, which is harmless at boot.
    let (clut, clut_handle) = alloc.alloc_clut(2)?;

    scratch[..total].fill(0);
    for (slot, &font) in fonts.iter().enumerate() {
        let (glyphs_per_row, _, _, _) = font_atlas_dims(font);
        let col0 = slot * FONT_PAGE_HALFWORDS;
        for gi in 0..font.glyph_count {
            let atlas_col = gi % glyphs_per_row;
            let atlas_row = gi / glyphs_per_row;
            let base_x = atlas_col * font.glyph_w as u16;
            let base_y = atlas_row * font.glyph_h as u16;
            for row in 0..font.glyph_h {
                let row_bits = font.glyph_row_packed(gi, row);
                for col in 0..font.glyph_w as u16 {
                    if (row_bits >> col) & 1 == 0 {
                        continue;
                    }
                    let x = base_x + col;
                    let y = base_y + row as u16;
                    let hw_idx = y as usize * stride + col0 + (x as usize / 4);
                    let nibble_shift = (x & 3) * 4;
                    scratch[hw_idx] |= 1u16 << nibble_shift; // CLUT index 1 = white
                }
            }
        }
        out[slot] = Some(FontAtlas {
            font,
            tpage: Tpage::new(base.x() + (slot as u16) * 64, base.y(), TexDepth::Bit4),
            clut,
            glyphs_per_row,
        });
    }
    for slot in out.iter_mut().skip(n) {
        *slot = None;
    }

    // One image transfer for the whole combined atlas, then the shared CLUT.
    upload_16bpp(
        VramRect::new(base.x(), base.y(), stride as u16, max_h),
        &scratch[..total],
    );
    upload_clut(clut, &[Color555::TRANSPARENT, Color555::rgb5(31, 31, 31)]);

    Some(FontSetVram {
        pages,
        clut: clut_handle,
    })
}

impl FontAtlas {
    /// Maximum texels a 4bpp tpage covers horizontally. 64 pixels
    /// is the PSX-SPX value (tpage is 256×256 in texel units, but
    /// in 4bpp mode the effective horizontal texel range per page
    /// maps onto 64 VRAM halfwords = 64 × 4 = 256 texels).
    const MAX_ATLAS_W_TEXELS: u16 = 256;
    /// Stack buffer for the packed 4bpp atlas. 8192 halfwords =
    /// 16 KiB leaves real call-frame headroom inside the SDK's 32 KiB
    /// minimum stack reserve while covering the common cases:
    /// - 128 glyphs at 8×8   (2048 hw)
    /// - 128 glyphs at 8×16  (4096 hw)
    /// - 64 glyphs  at 16×16 (4096 hw)
    /// - 128 glyphs at 12×16 (5040 hw)
    /// - 128 glyphs at 16×16 (8192 hw)
    /// - 64 glyphs  at 32×16 (8192 hw) -- the largest supported
    ///
    /// A full 32 KiB local is not safe: the linker guarantees 32 KiB for the
    /// *whole* call stack, so the uploader's caller/interrupt frames would
    /// cross into static RAM. Larger imports should use [`upload_fonts`] and
    /// provide scratch storage with an explicitly audited lifetime.
    const MAX_PACK_HALFWORDS: usize = 8192;

    /// Upload `font` as a 4bpp CLUT texture at `tpage`, with a
    /// 2-entry CLUT (transparent, white) at `clut`.
    ///
    /// The caller picks the tpage / clut locations -- typically
    /// `Tpage::new(768, 0, TexDepth::Bit4)` for the standard
    /// off-display region, and a `Clut::new(768, 480)` for the
    /// CLUT row. Both must live inside VRAM and not overlap the
    /// active framebuffer.
    ///
    /// Atlas layout picks `glyphs_per_row` so the texture's pixel
    /// width stays within `MAX_ATLAS_W_TEXELS`. For 8-wide fonts
    /// that's 32 glyphs per row; 16-wide fonts get 16 per row.
    pub fn upload(font: &'static BitmapFont, tpage: Tpage, clut: Clut) -> Self {
        assert!(
            matches!(tpage.depth(), TexDepth::Bit4),
            "FontAtlas::upload requires a 4bpp tpage",
        );

        let glyph_w = font.glyph_w as u16;
        let glyph_h = font.glyph_h as u16;
        let glyph_count = font.glyph_count;

        // Atlas width = glyphs_per_row × glyph_w, capped at the
        // MAX to stay within a single 4bpp tpage.
        let max_cols = Self::MAX_ATLAS_W_TEXELS / glyph_w;
        let glyphs_per_row = glyph_count.min(max_cols);
        let glyph_rows = glyph_count.div_ceil(glyphs_per_row);
        let atlas_w = glyphs_per_row * glyph_w;
        let atlas_h = glyph_rows * glyph_h;

        // Pack 1bpp source → 4bpp VRAM texture into a stack buffer.
        // Each 16-bit halfword holds 4 texels (nibble 0 = leftmost).
        let halfwords_per_row = atlas_w.div_ceil(4);
        let total_halfwords = halfwords_per_row as usize * atlas_h as usize;
        assert!(
            total_halfwords <= Self::MAX_PACK_HALFWORDS,
            "font atlas too large for stack buffer",
        );
        let mut packed = [0u16; Self::MAX_PACK_HALFWORDS];

        for gi in 0..glyph_count {
            let atlas_col = gi % glyphs_per_row;
            let atlas_row = gi / glyphs_per_row;
            let base_x = atlas_col * glyph_w;
            let base_y = atlas_row * glyph_h;
            for row in 0..glyph_h as u8 {
                let row_bits = font.glyph_row_packed(gi, row);
                for col in 0..glyph_w {
                    let bit = (row_bits >> col) & 1;
                    let x = base_x + col;
                    let y = base_y + row as u16;
                    // 4 texels per halfword; pick nibble by x & 3.
                    let hw_idx = y as usize * halfwords_per_row as usize + (x as usize / 4);
                    let nibble_shift = (x & 3) * 4;
                    packed[hw_idx] |= (bit as u16) << nibble_shift;
                }
            }
        }

        // upload_16bpp takes pixel count = w*h, which for a 4bpp
        // texture uploaded as a 16bpp rect works out to
        // halfwords_per_row × atlas_h halfwords = the 4bpp pixel
        // count. Upload is by halfwords regardless of bit-depth;
        // the GPU doesn't inspect bits inside words.
        //
        // VRAM rect semantics expose the *halfword* footprint when
        // depth is 4bpp -- so width is `atlas_w / 4`, not `atlas_w`.
        let vram_rect = VramRect::new(tpage.x(), tpage.y(), halfwords_per_row, atlas_h);
        upload_16bpp(vram_rect, &packed[..total_halfwords]);

        // Upload 2-entry CLUT: idx 0 = transparent, idx 1 = white.
        // The white texel will be tinted per-draw_call via the
        // sprite's per-vertex colour (GP0 0x64 tint byte).
        let clut_entries = [Color555::TRANSPARENT, Color555::rgb5(31, 31, 31)];
        upload_clut(clut, &clut_entries);

        Self {
            font,
            tpage,
            clut,
            glyphs_per_row,
        }
    }

    /// Pixel width of `text` when rendered with this atlas.
    pub fn text_width(&self, text: &str) -> u16 {
        self.font.text_width(text)
    }

    /// Pixel width of `text` using this atlas's font metrics plus signed
    /// spacing inserted between adjacent characters.
    pub fn text_width_with_spacing(&self, text: &str, letter_spacing: i8) -> u16 {
        self.font.text_width_with_spacing(text, letter_spacing)
    }

    /// Pixel height of a single text line. Usually `== glyph_h`.
    pub fn line_height(&self) -> u16 {
        self.font.line_height as u16
    }

    /// Look up the atlas UV for a character, returning the
    /// top-left `(u, v)` of the glyph in texel coords, or `None`
    /// if the char is outside the font's range.
    ///
    /// Shared by every draw method so the codepoint-window logic
    /// lives in one place.
    #[inline]
    fn glyph_uv(&self, ch: char) -> Option<(u8, u8)> {
        let cp = ch as u32;
        let first = self.font.first_char as u32;
        if cp < first || cp >= first + self.font.glyph_count as u32 {
            return None;
        }
        let idx = (cp - first) as u16;
        let col = idx % self.glyphs_per_row;
        let row = idx / self.glyphs_per_row;
        let u = col * self.font.glyph_w as u16;
        let v = row * self.font.glyph_h as u16;
        Some((u as u8, v as u8))
    }

    /// Draw `text` at screen-space `(x, y)` with the given tint.
    ///
    /// `(0x80, 0x80, 0x80)` = unmodulated white; any other value
    /// recolours every glyph via the PSX per-texel multiplier
    /// (`output = texel * tint / 128`).
    ///
    /// **Fast path** -- uses textured rectangles (GP0 0x64, 4 words
    /// per glyph). For any transform or per-vertex colour needs,
    /// reach for [`Self::draw_text_scaled`], [`Self::draw_text_rotated`],
    /// [`Self::draw_text_affine`], or [`Self::draw_text_gradient`].
    ///
    /// Characters outside the font's codepoint range are skipped --
    /// they still advance the cursor so the rest of the string
    /// lines up as the caller intended. Iteration is per-`char`,
    /// so any `&str` is valid input; a Latin-1 atlas with
    /// `first_char = 0xA0` picks up `é`, `ü`, etc. automatically.
    ///
    /// Sets the GP0(0xE1) draw-mode tpage to our atlas once at the
    /// start of the call -- if the caller was rendering with a
    /// different tpage, they'll need to re-apply theirs after
    /// draw_text returns.
    ///
    /// # Example
    ///
    /// ```ignore
    /// atlas.draw_text(8, 8, "SCORE 0042", (0x80, 0x80, 0x80));
    /// ```
    pub fn draw_text(&self, x: i16, y: i16, text: &str, tint: (u8, u8, u8)) {
        self.draw_text_with_spacing(x, y, text, 0, tint);
    }

    /// Draw `text` with signed spacing inserted between adjacent characters.
    /// `letter_spacing` is measured in final screen pixels and is not applied
    /// after the last character.
    pub fn draw_text_with_spacing(
        &self,
        x: i16,
        y: i16,
        text: &str,
        letter_spacing: i8,
        tint: (u8, u8, u8),
    ) {
        // Our atlas tpage takes over the current draw-mode slot; the
        // per-glyph GP0 0x64 rectangles all sample from it.
        self.tpage.apply_as_draw_mode();

        let font = self.font;
        let clut_word = self.clut.uv_clut_word();
        let color_cmd = 0x6400_0000 | pack_color(tint.0, tint.1, tint.2);
        let glyph_size = pack_xy(font.glyph_w as u16, font.glyph_h as u16);
        let gap = i16::from(letter_spacing);
        let mut cursor_x = x;
        for ch in text.chars() {
            let Some((u, v)) = self.glyph_uv(ch) else {
                cursor_x = cursor_x
                    .wrapping_add(font.glyph_advance(ch) as i16)
                    .wrapping_add(gap);
                continue;
            };

            wait_cmd_ready();
            // GP0 0x64 = variable-size textured rectangle, no blend,
            // opaque. First word: 0x64_BB_GG_RR (color is the tint
            // multiplier -- NOT a fill colour: CLUT index 1's white
            // texel gets modulated by this).
            write_gp0(color_cmd);
            write_gp0(pack_vertex(cursor_x, y));
            // Second word packs (U, V, CLUT) -- our `pack_texcoord`
            // takes (u, v, extra) where `extra` is the CLUT field
            // (high halfword). The tpage is implied by the current
            // draw mode.
            write_gp0(pack_texcoord(u, v, clut_word));
            // Third word: rectangle size.
            write_gp0(glyph_size);

            cursor_x = cursor_x
                .wrapping_add(font.glyph_advance(ch) as i16)
                .wrapping_add(gap);
        }
    }

    /// Draw `text` at screen-space `(x, y)` scaled by `(scale_x,
    /// scale_y)`. `scale=(1, 1)` matches [`Self::draw_text`]'s
    /// output, but via the quad path instead of the rect path --
    /// so prefer `draw_text` for native-size.
    ///
    /// **Quad path** -- uses textured quads (GP0 0x2C, 9 words per
    /// glyph). PSX samples textures with nearest-neighbour, so
    /// integer scales (2×, 3×, 4×) produce crisp pixel-doubled
    /// output. Use [`Self::draw_text_scaled_q8`] for fractional
    /// screen-space sizes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// atlas.draw_text_scaled(80, 100, "GAME OVER", 3, 3, (220, 40, 40));
    /// ```
    pub fn draw_text_scaled(
        &self,
        x: i16,
        y: i16,
        text: &str,
        scale_x: u8,
        scale_y: u8,
        tint: (u8, u8, u8),
    ) {
        self.draw_text_scaled_with_spacing(x, y, text, scale_x, scale_y, 0, tint);
    }

    /// Draw `text` with Q8 fixed-point scale factors (`256` = 1.0x).
    /// This keeps the font atlas monochrome while allowing fractional
    /// screen-space glyph sizes such as 1.5x (`384`).
    pub fn draw_text_scaled_q8(
        &self,
        x: i16,
        y: i16,
        text: &str,
        scale_x_q8: u16,
        scale_y_q8: u16,
        tint: (u8, u8, u8),
    ) {
        self.draw_text_scaled_with_spacing_q8(x, y, text, scale_x_q8, scale_y_q8, 0, tint);
    }

    /// Draw scaled text with signed spacing inserted between adjacent
    /// characters. `letter_spacing` is measured in final screen pixels, not
    /// source-glyph pixels.
    pub fn draw_text_scaled_with_spacing(
        &self,
        x: i16,
        y: i16,
        text: &str,
        scale_x: u8,
        scale_y: u8,
        letter_spacing: i8,
        tint: (u8, u8, u8),
    ) {
        assert!(scale_x > 0 && scale_y > 0, "scale must be > 0 in both axes");
        self.draw_text_scaled_with_spacing_q8(
            x,
            y,
            text,
            u16::from(scale_x) * 256,
            u16::from(scale_y) * 256,
            letter_spacing,
            tint,
        );
    }

    /// Draw scaled text with Q8 fixed-point scale and signed spacing inserted
    /// between adjacent characters. `letter_spacing` is measured in final
    /// screen pixels, not source-glyph pixels.
    pub fn draw_text_scaled_with_spacing_q8(
        &self,
        x: i16,
        y: i16,
        text: &str,
        scale_x_q8: u16,
        scale_y_q8: u16,
        letter_spacing: i8,
        tint: (u8, u8, u8),
    ) {
        assert!(
            scale_x_q8 > 0 && scale_y_q8 > 0,
            "scale must be > 0 in both axes"
        );
        if text.is_empty() {
            return;
        }
        let font = self.font;
        let gw = font.glyph_w as i16;
        let gh = font.glyph_h as i16;
        let sw = scale_q8_i16(gw, scale_x_q8);
        let sh = scale_q8_i16(gh, scale_y_q8);
        let clut = self.clut.uv_clut_word();
        let tpage = self.tpage.uv_tpage_word(0);
        let color_cmd = gp0::polygon_opcode(false, true, true, false, false)
            | pack_color(tint.0, tint.1, tint.2);
        let gap_q8 = i32::from(letter_spacing) << 8;
        let mut cursor_x_q8 = i32::from(x) << 8;
        reset_texture_window();
        for ch in text.chars() {
            let cursor_x = round_q8_to_i16(cursor_x_q8);
            let Some((u, v)) = self.glyph_uv(ch) else {
                cursor_x_q8 = cursor_x_q8
                    .saturating_add(i32::from(font.glyph_advance(ch)) * i32::from(scale_x_q8))
                    .saturating_add(gap_q8);
                continue;
            };
            let verts = [
                (cursor_x, y),
                (cursor_x + sw, y),
                (cursor_x, y + sh),
                (cursor_x + sw, y + sh),
            ];
            let uvs = glyph_quad_uvs(u, v, font.glyph_w, font.glyph_h);
            write_textured_quad_packet(verts, uvs, color_cmd, clut, tpage);
            cursor_x_q8 = cursor_x_q8
                .saturating_add(i32::from(font.glyph_advance(ch)) * i32::from(scale_x_q8))
                .saturating_add(gap_q8);
        }
    }

    /// Draw `text` rotated around the pivot `(cx, cy)` by
    /// `angle_q12` (Q0.12, one revolution = 4096). The string is
    /// centred on the pivot at angle 0 -- its natural extent is
    /// `text_width × glyph_h`, anchored so `(cx, cy)` sits at the
    /// centre of the baseline.
    ///
    /// **Quad path** -- 9 GP0 words per glyph. Sin/cos come from a
    /// compact 256-entry Q1.12 table ([`sincos`]), good to ~1.4° --
    /// imperceptible at 8px glyph scale.
    ///
    /// See crate-level docs for the Q0.12 angle convention.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Spin a title once per second (frame_idx updates each vsync).
    /// let angle = ((frame_idx * 68) & 0xFFF) as u16; // ~4096/60 ≈ 68
    /// atlas.draw_text_rotated(160, 120, "SPIN", angle, (255, 255, 255));
    /// ```
    pub fn draw_text_rotated(
        &self,
        cx: i16,
        cy: i16,
        text: &str,
        angle_q12: u16,
        tint: (u8, u8, u8),
    ) {
        if text.is_empty() {
            return;
        }
        let font = self.font;
        let gw = font.glyph_w as i32;
        let gh = font.glyph_h as i32;
        let total_w = font.text_width(text) as i32;
        // Centre the string on the pivot -- baseline (top edge of
        // first glyph) is `gh/2` above the pivot so that the glyph
        // midline runs through `(cx, cy)`.
        let origin_x = -total_w / 2;
        let origin_y = -gh / 2;
        let s = sincos::sin_q12(angle_q12);
        let c = sincos::cos_q12(angle_q12);
        let clut = self.clut.uv_clut_word();
        let tpage = self.tpage.uv_tpage_word(0);
        let color_cmd = gp0::polygon_opcode(false, true, true, false, false)
            | pack_color(tint.0, tint.1, tint.2);

        // Transform helper: local (lx, ly) → screen (sx, sy), with
        // Q1.12 rotation matrix and integer translate to (cx, cy).
        let rot = |lx: i32, ly: i32| -> (i16, i16) {
            let rx = (lx * c - ly * s) >> 12;
            let ry = (lx * s + ly * c) >> 12;
            ((cx as i32 + rx) as i16, (cy as i32 + ry) as i16)
        };

        let mut cursor_x = 0i32;
        reset_texture_window();
        for ch in text.chars() {
            let Some((u, v)) = self.glyph_uv(ch) else {
                cursor_x += font.glyph_advance(ch) as i32;
                continue;
            };
            let lx0 = origin_x + cursor_x;
            let lx1 = lx0 + gw;
            let ly0 = origin_y;
            let ly1 = origin_y + gh;
            let verts = [rot(lx0, ly0), rot(lx1, ly0), rot(lx0, ly1), rot(lx1, ly1)];
            let uvs = glyph_quad_uvs(u, v, font.glyph_w, font.glyph_h);
            write_textured_quad_packet(verts, uvs, color_cmd, clut, tpage);
            cursor_x += font.glyph_advance(ch) as i32;
        }
    }

    /// Draw `text` through an arbitrary 2×2 affine transform.
    ///
    /// The matrix `m` is Q3.12 fixed-point -- `m = [[4096, 0], [0,
    /// 4096]]` is the identity (native size, axis-aligned). Each
    /// glyph's local corner `(lx, ly)` maps onto screen space as
    /// `(origin.0 + (m[0][0]*lx + m[0][1]*ly) >> 12,
    ///   origin.1 + (m[1][0]*lx + m[1][1]*ly) >> 12)`.
    ///
    /// Covers rotation, non-uniform scale, shear, reflection, and
    /// any combination -- the other quad-path methods are all
    /// specializations of this one.
    ///
    /// **Quad path** -- 9 GP0 words per glyph.
    ///
    /// # Example: horizontal shear
    ///
    /// ```ignore
    /// // Skew 30° right: x' = x + 0.577·y. 0.577 × 4096 ≈ 2365.
    /// let m = [[4096, 2365], [0, 4096]];
    /// atlas.draw_text_affine((40, 40), "SKEW", m, (200, 200, 200));
    /// ```
    pub fn draw_text_affine(
        &self,
        origin: (i16, i16),
        text: &str,
        m: [[i16; 2]; 2],
        tint: (u8, u8, u8),
    ) {
        if text.is_empty() {
            return;
        }
        let font = self.font;
        let gw = font.glyph_w as i32;
        let gh = font.glyph_h as i32;
        let (m00, m01) = (m[0][0] as i32, m[0][1] as i32);
        let (m10, m11) = (m[1][0] as i32, m[1][1] as i32);
        let clut = self.clut.uv_clut_word();
        let tpage = self.tpage.uv_tpage_word(0);
        let color_cmd = gp0::polygon_opcode(false, true, true, false, false)
            | pack_color(tint.0, tint.1, tint.2);

        let tx = |lx: i32, ly: i32| -> (i16, i16) {
            let sx = origin.0 as i32 + ((m00 * lx + m01 * ly) >> 12);
            let sy = origin.1 as i32 + ((m10 * lx + m11 * ly) >> 12);
            (sx as i16, sy as i16)
        };

        let mut cursor_x = 0i32;
        reset_texture_window();
        for ch in text.chars() {
            let Some((u, v)) = self.glyph_uv(ch) else {
                cursor_x += font.glyph_advance(ch) as i32;
                continue;
            };
            let lx0 = cursor_x;
            let lx1 = lx0 + gw;
            let ly0 = 0;
            let ly1 = gh;
            let verts = [tx(lx0, ly0), tx(lx1, ly0), tx(lx0, ly1), tx(lx1, ly1)];
            let uvs = glyph_quad_uvs(u, v, font.glyph_w, font.glyph_h);
            write_textured_quad_packet(verts, uvs, color_cmd, clut, tpage);
            cursor_x += font.glyph_advance(ch) as i32;
        }
    }

    /// Draw `text` with a top-to-bottom colour gradient.
    ///
    /// Top of each glyph is tinted `top`; bottom is tinted
    /// `bottom`. The GPU gouraud-interpolates down each glyph,
    /// producing a smooth vertical gradient across the whole line.
    ///
    /// **Gouraud quad path** -- 12 GP0 words per glyph (GP0 0x3C).
    /// Use when you want a rainbow title, a torch-lit dialogue
    /// box, or any per-vertex colour effect; prefer the single-
    /// tint [`Self::draw_text`] otherwise.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Classic "red hot" gradient.
    /// atlas.draw_text_gradient(
    ///     40, 10, "INFERNO",
    ///     (255, 240, 80),  // bright yellow at the top
    ///     (180, 30, 20),   // deep red at the bottom
    /// );
    /// ```
    pub fn draw_text_gradient(
        &self,
        x: i16,
        y: i16,
        text: &str,
        top: (u8, u8, u8),
        bottom: (u8, u8, u8),
    ) {
        self.draw_text_scaled_gradient(x, y, text, 1, 1, top, bottom);
    }

    /// Draw `text` with a top-to-bottom gradient, scaled by
    /// `(scale_x, scale_y)`. Combines [`Self::draw_text_scaled`]
    /// and [`Self::draw_text_gradient`] in one draw -- a 3× title
    /// with a fire-colour sweep costs the same 12 words per glyph
    /// as a 1× gradient.
    ///
    /// Nearest-neighbour sampling still applies, so integer
    /// scales produce crisp pixel-doubled output.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // 3× "TITLE" with yellow→red gradient.
    /// atlas.draw_text_scaled_gradient(
    ///     64, 10, "TITLE", 3, 3,
    ///     (255, 220, 80), (200, 40, 20),
    /// );
    /// ```
    pub fn draw_text_scaled_gradient(
        &self,
        x: i16,
        y: i16,
        text: &str,
        scale_x: u8,
        scale_y: u8,
        top: (u8, u8, u8),
        bottom: (u8, u8, u8),
    ) {
        assert!(scale_x > 0 && scale_y > 0, "scale must be > 0 in both axes");
        if text.is_empty() {
            return;
        }
        let font = self.font;
        let gw = font.glyph_w as i16;
        let gh = font.glyph_h as i16;
        let sw = gw * scale_x as i16;
        let sh = gh * scale_y as i16;
        let clut = self.clut.uv_clut_word();
        let tpage = self.tpage.uv_tpage_word(0);
        let color0_cmd =
            gp0::polygon_opcode(true, true, true, false, false) | pack_color(top.0, top.1, top.2);
        let mut cursor_x = x;
        reset_texture_window();
        for ch in text.chars() {
            let Some((u, v)) = self.glyph_uv(ch) else {
                cursor_x = cursor_x.wrapping_add(font.glyph_advance(ch) as i16 * scale_x as i16);
                continue;
            };
            let verts = [
                (cursor_x, y),
                (cursor_x + sw, y),
                (cursor_x, y + sh),
                (cursor_x + sw, y + sh),
            ];
            let uvs = glyph_quad_uvs(u, v, font.glyph_w, font.glyph_h);
            let colors = [top, top, bottom, bottom];
            write_textured_gouraud_quad_packet(verts, uvs, colors, color0_cmd, clut, tpage);
            cursor_x = cursor_x.wrapping_add(font.glyph_advance(ch) as i16 * scale_x as i16);
        }
    }

    /// Draw `text` with per-vertex glyph colours, Q8 scaling, and
    /// authored letter spacing.
    ///
    /// `colors` are ordered top-left, top-right, bottom-left,
    /// bottom-right, matching [`psx_gpu::draw_quad_textured_gouraud`].
    /// This is the general-purpose path behind gradient UI text:
    /// vertical gradients use `[top, top, bottom, bottom]`, horizontal
    /// gradients use `[left, right, left, right]`.
    pub fn draw_text_scaled_gouraud_with_spacing_q8(
        &self,
        x: i16,
        y: i16,
        text: &str,
        scale_x_q8: u16,
        scale_y_q8: u16,
        letter_spacing: i8,
        colors: [(u8, u8, u8); 4],
    ) {
        assert!(
            scale_x_q8 > 0 && scale_y_q8 > 0,
            "scale must be > 0 in both axes"
        );
        if text.is_empty() {
            return;
        }
        let font = self.font;
        let gw = font.glyph_w as i16;
        let gh = font.glyph_h as i16;
        let sw = scale_q8_i16(gw, scale_x_q8);
        let sh = scale_q8_i16(gh, scale_y_q8);
        let clut = self.clut.uv_clut_word();
        let tpage = self.tpage.uv_tpage_word(0);
        let color0_cmd = gp0::polygon_opcode(true, true, true, false, false)
            | pack_color(colors[0].0, colors[0].1, colors[0].2);
        let gap_q8 = i32::from(letter_spacing) << 8;
        let mut cursor_x_q8 = i32::from(x) << 8;
        reset_texture_window();
        for ch in text.chars() {
            let cursor_x = round_q8_to_i16(cursor_x_q8);
            let Some((u, v)) = self.glyph_uv(ch) else {
                cursor_x_q8 = cursor_x_q8
                    .saturating_add(i32::from(font.glyph_advance(ch)) * i32::from(scale_x_q8))
                    .saturating_add(gap_q8);
                continue;
            };
            let verts = [
                (cursor_x, y),
                (cursor_x + sw, y),
                (cursor_x, y + sh),
                (cursor_x + sw, y + sh),
            ];
            let uvs = glyph_quad_uvs(u, v, font.glyph_w, font.glyph_h);
            write_textured_gouraud_quad_packet(verts, uvs, colors, color0_cmd, clut, tpage);
            cursor_x_q8 = cursor_x_q8
                .saturating_add(i32::from(font.glyph_advance(ch)) * i32::from(scale_x_q8))
                .saturating_add(gap_q8);
        }
    }

    /// Access the underlying [`BitmapFont`]. Useful for UI code
    /// that needs glyph dimensions for its own layout math.
    pub fn font(&self) -> &'static BitmapFont {
        self.font
    }

    /// Tpage the atlas is installed at -- useful if the caller wants
    /// to restore it after drawing with a different tpage. Always
    /// 4bpp, always inside a valid VRAM page-aligned slot.
    pub fn tpage(&self) -> Tpage {
        self.tpage
    }
}

// ======================================================================
// Tests (host-side -- pure data-transform checks)
// ======================================================================

// ======================================================================
// TextSink -- one text interface for UI code
// ======================================================================

/// The minimum a widget needs from a text renderer: draw a run at a position
/// in a colour, and measure it.
///
/// UI code written against this works with any renderer -- [`FontAtlas`], a
/// scaled bitmap font, or a game's own atlas -- instead of being written twice
/// because two renderers happen to have different inherent methods. That is the
/// situation this exists to prevent: a caller with a list, a menu or a dialog to
/// draw should not care which one is behind it.
///
/// Coordinates are screen pixels with the origin top-left, matching the rest of
/// the SDK's immediate-mode drawing.
pub trait TextSink {
    /// Draw `text` with its top-left corner at `(x, y)`.
    fn draw(&self, x: i16, y: i16, text: &str, tint: (u8, u8, u8));

    /// Width of `text` in screen pixels, using the same metrics `draw` will.
    fn width(&self, text: &str) -> i16;

    /// Baseline-to-baseline distance for stacked rows.
    fn line_height(&self) -> i16;

    /// Draw `text` centred on `x`. Provided so every caller does not repeat the
    /// half-width arithmetic, and so a renderer with non-integer metrics can
    /// override the rounding.
    fn draw_centered(&self, x: i16, y: i16, text: &str, tint: (u8, u8, u8)) {
        self.draw(x - self.width(text) / 2, y, text, tint);
    }
}

impl TextSink for FontAtlas {
    fn draw(&self, x: i16, y: i16, text: &str, tint: (u8, u8, u8)) {
        FontAtlas::draw_text(self, x, y, text, tint);
    }

    fn width(&self, text: &str) -> i16 {
        FontAtlas::text_width(self, text) as i16
    }

    fn line_height(&self) -> i16 {
        self.font().line_height as i16
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn centered_text_is_symmetric_about_the_anchor() {
        // A stub renderer keeps the contract test independent of any atlas
        // upload, which needs hardware.
        struct Fixed(i16);
        impl super::TextSink for Fixed {
            fn draw(&self, _x: i16, _y: i16, _t: &str, _c: (u8, u8, u8)) {}
            fn width(&self, text: &str) -> i16 {
                self.0 * text.len() as i16
            }
            fn line_height(&self) -> i16 {
                8
            }
        }
        struct Spy(core::cell::Cell<i16>);
        impl super::TextSink for Spy {
            fn draw(&self, x: i16, _y: i16, _t: &str, _c: (u8, u8, u8)) {
                self.0.set(x);
            }
            fn width(&self, text: &str) -> i16 {
                4 * text.len() as i16
            }
            fn line_height(&self) -> i16 {
                8
            }
        }
        let f = Fixed(4);
        assert_eq!(super::TextSink::width(&f, "abcd"), 16);
        let spy = Spy(core::cell::Cell::new(-1));
        spy.draw_centered(160, 0, "abcd", (0, 0, 0));
        // 4 glyphs * 4 px = 16 wide, so centred on 160 starts at 152.
        assert_eq!(spy.0.get(), 152);
    }

    use super::*;

    /// Synthetic font: two 8×2 glyphs, LSB-first, for bit-unpacking
    /// verification.
    const TEST_FONT: BitmapFont = BitmapFont {
        glyph_w: 8,
        glyph_h: 2,
        first_char: b'A' as u16,
        glyph_count: 2,
        bitmap: &[
            // 'A': row 0 = all pixels, row 1 = leftmost only
            0xFF, 0x01, // 'B': row 0 = alternating, row 1 = rightmost
            0x55, 0x80,
        ],
        glyph_advances: None,
        advance_x: 8,
        line_height: 2,
        bit_order: BitOrder::Lsb,
    };

    const TEST_PROPORTIONAL_ADVANCES: [u8; 2] = [3, 7];
    const TEST_PROPORTIONAL_FONT: BitmapFont = BitmapFont {
        glyph_advances: Some(&TEST_PROPORTIONAL_ADVANCES),
        ..TEST_FONT
    };

    #[test]
    fn row_bytes_handles_widths_under_and_over_8() {
        let mut f = TEST_FONT;
        assert_eq!(f.row_bytes(), 1);
        f.glyph_w = 12;
        assert_eq!(f.row_bytes(), 2);
        f.glyph_w = 16;
        assert_eq!(f.row_bytes(), 2);
        f.glyph_w = 24;
        assert_eq!(f.row_bytes(), 3);
    }

    #[test]
    fn lsb_row_unpacks_as_expected() {
        let row_a0 = TEST_FONT.glyph_row_packed(0, 0);
        assert_eq!(row_a0, 0xFF);
        let row_a1 = TEST_FONT.glyph_row_packed(0, 1);
        assert_eq!(row_a1, 0x01);
        let row_b0 = TEST_FONT.glyph_row_packed(1, 0);
        assert_eq!(row_b0, 0x55);
        let row_b1 = TEST_FONT.glyph_row_packed(1, 1);
        assert_eq!(row_b1, 0x80);
    }

    #[test]
    fn msb_order_mirrors_the_byte() {
        let msb_font = BitmapFont {
            bit_order: BitOrder::Msb,
            ..TEST_FONT
        };
        // 0xFF mirrors to 0xFF.
        assert_eq!(msb_font.glyph_row_packed(0, 0), 0xFF);
        // 0x01 in MSB-first becomes 0x80 after reversing.
        assert_eq!(msb_font.glyph_row_packed(0, 1), 0x80);
        // 0x55 (0b01010101) mirrors to 0xAA (0b10101010).
        assert_eq!(msb_font.glyph_row_packed(1, 0), 0xAA);
    }

    #[test]
    fn glyph_advances_override_fixed_advance_when_present() {
        assert_eq!(TEST_PROPORTIONAL_FONT.glyph_advance('A'), 3);
        assert_eq!(TEST_PROPORTIONAL_FONT.glyph_advance('B'), 7);
        assert_eq!(TEST_PROPORTIONAL_FONT.glyph_advance('Z'), 8);
        assert_eq!(TEST_PROPORTIONAL_FONT.text_width("ABA"), 13);
    }

    #[test]
    fn text_width_with_spacing_inserts_gaps_between_glyphs_only() {
        assert_eq!(TEST_PROPORTIONAL_FONT.text_width_with_spacing("", 5), 0);
        assert_eq!(TEST_PROPORTIONAL_FONT.text_width_with_spacing("A", 5), 3);
        assert_eq!(TEST_PROPORTIONAL_FONT.text_width_with_spacing("ABA", 2), 17);
        assert_eq!(TEST_PROPORTIONAL_FONT.text_width_with_spacing("ABA", -2), 9);
    }

    #[test]
    fn q8_scale_helpers_round_to_screen_pixels() {
        assert_eq!(scale_q8_i16(8, 256), 8);
        assert_eq!(scale_q8_i16(8, 384), 12);
        assert_eq!(scale_q8_i16(17, 384), 26);
        assert_eq!(round_q8_to_i16(384), 2);
        assert_eq!(round_q8_to_i16(-384), -2);
    }

    #[test]
    fn glyph_quad_uvs_saturate_the_rightmost_atlas_column() {
        // A 256-texel-wide atlas (16 columns of 16px glyphs) places the last
        // column's glyph at u=240, so its far U is 240+16=256, which wraps to 0
        // in the u8 UV field and smears the glyph across the quad. It must
        // saturate to 255 instead. (This was the "garbled O in MUSIC VOLUME"
        // bug: 'O' is the only rightmost-column glyph in uppercase menu text.)
        let edge = glyph_quad_uvs(240, 0, 16, 14);
        assert_eq!(edge[1].0, 255, "far U must saturate, not wrap to 0");
        assert_eq!(edge[3].0, 255);
        // Interior columns stay exact (no clamping kicks in).
        assert_eq!(
            glyph_quad_uvs(224, 0, 16, 14),
            [(224, 0), (240, 0), (224, 14), (240, 14)]
        );
        // V saturates the same way near the bottom edge of a tall atlas.
        let tall = glyph_quad_uvs(0, 248, 16, 14);
        assert_eq!(tall[2].1, 255);
        assert_eq!(tall[3].1, 255);
    }
}
