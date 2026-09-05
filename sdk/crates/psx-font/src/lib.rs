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
    /// Pre-encoded CLUT word. Storing the two-byte packet value instead of
    /// the four-byte coordinate handle pays for `uv_origin` without growing
    /// `FontAtlas` in PS1 RAM, and removes repeated encoding from draw calls.
    clut_word: u16,
    /// How many glyphs per row of the atlas texture. Picked at
    /// upload time so the texture fits within a single 4bpp tpage.
    glyphs_per_row: u16,
    /// Page-local texel origin of this atlas inside a packed tpage.
    uv_origin: (u8, u8),
}

// The packed origin replaces the space recovered by caching the CLUT packet
// word, so adding shared-page placement must not grow every resident atlas.
#[cfg(target_arch = "mips")]
const _: () = assert!(core::mem::size_of::<FontAtlas>() == 16);

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

/// GPU semi-transparency equation for [`FontAtlas::draw_text_blended`].
///
/// The PSX blends a semi-transparent primitive against the framebuffer
/// with one of four fixed equations. The choice is *not* per-primitive:
/// the primitive only carries a "blend me" bit, and the equation comes
/// from the two-bit ABR field of the current GP0(E1h) draw mode. These
/// are the same four the rest of the SDK exposes as `psx_gpu::BlendMode`,
/// restated here so the font crate does not take a dependency on the
/// primitive layer to name two bits.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TextBlend {
    /// `(background + foreground) / 2`. A fixed 50% wash; the tint still
    /// recolours, but it cannot fade a glyph away -- at tint zero the
    /// glyph still halves whatever is behind it.
    Average,
    /// `background + foreground`, clamped per channel. The mode to reach
    /// for when text has to fade: scaling the tint toward black scales
    /// the contribution toward nothing, so the glyph dissolves instead
    /// of darkening. Reads as emissive, which suits HUD and combat text.
    Add,
    /// `background - foreground`, clamped per channel. Darkens whatever
    /// is behind the glyph, so it works as a drop shadow over a light
    /// background as well as a dark one.
    Subtract,
    /// `background + foreground / 4`, clamped per channel. A quarter-
    /// strength [`Self::Add`] for a subtler glow.
    AddQuarter,
}

impl TextBlend {
    /// The two-bit draw-mode semi-transparency ("ABR") field selecting
    /// this equation.
    pub const fn abr(self) -> u8 {
        match self {
            Self::Average => 0,
            Self::Add => 1,
            Self::Subtract => 2,
            Self::AddQuarter => 3,
        }
    }
}

/// GP0 command byte for a variable-size textured rectangle with the
/// semi-transparency bit (25) set. The opaque path uses `0x6400_0000`.
const SEMI_TRANSPARENT_RECT_CMD: u32 = 0x6600_0000;

/// GP0 command byte for a variable-size MONOCHROME rectangle with the
/// semi-transparency bit (25) set. The opaque form is `0x6000_0000`.
///
/// Untextured, so it costs three data words however large it is --
/// which is what makes a backing plate cheaper than a second pass over
/// the glyphs it sits behind.
const SEMI_TRANSPARENT_FLAT_RECT_CMD: u32 = 0x6200_0000;

/// The GP0(E1h) word that installs `tpage` with `blend`'s equation in
/// the ABR field. Split out from the draw call so the bit layout is
/// checkable on the host, where no GPU exists to observe.
const fn blended_draw_mode_word(tpage: Tpage, blend: TextBlend) -> u32 {
    gp0::draw_mode(
        (tpage.x() / 64) as u32,
        if tpage.y() == 256 { 1 } else { 0 },
        blend.abr() as u32,
        tpage.depth() as u32,
        false,
        true,
    )
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

/// Atlas cell width: the glyph width rounded up to a whole halfword (4
/// texels at 4bpp), so every glyph starts on a halfword boundary. See the
/// note in [`FontAtlas::upload`] for why that matters on real hardware.
pub(crate) fn cell_width(font: &BitmapFont) -> u16 {
    (font.glyph_w as u16).max(1).div_ceil(4) * 4
}

/// Atlas layout for `font`: `(glyphs_per_row, atlas_w_texels, atlas_h_texels,
/// halfwords_per_row)`. Mirrors the layout [`FontAtlas::upload`] computes.
fn font_atlas_dims(font: &BitmapFont) -> (u16, u16, u16, u16) {
    let cell_w = cell_width(font);
    let glyph_h = font.glyph_h as u16;
    let max_cols = (FontAtlas::MAX_ATLAS_W_TEXELS / cell_w).max(1);
    let glyphs_per_row = font.glyph_count.min(max_cols).max(1);
    let glyph_rows = font.glyph_count.div_ceil(glyphs_per_row);
    let atlas_w = glyphs_per_row * cell_w;
    let atlas_h = glyph_rows * glyph_h;
    // Even stride: `upload_16bpp` moves whole words and rejects an odd
    // pixel count, and cell padding can leave an odd halfword row (KENNEY
    //_PIXEL lands on 63). The spare column stays zero.
    let halfwords_per_row = atlas_w.div_ceil(4).next_multiple_of(2);
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

/// Maximum number of font atlases accepted by one packed upload. The game
/// manifest currently caps a scene at eight; keeping this bound at sixteen
/// also matches the number of 4bpp page columns in 1024-wide VRAM.
const MAX_PACKED_FONT_ATLASES: usize = 16;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct AtlasPlacement {
    page: u16,
    x_halfwords: u16,
    y: u16,
}

/// VRAM and scratch requirements for one packed font-set upload.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FontPackMetrics {
    /// Minimum contiguous page run requested by the bounded packer.
    pub pages: u16,
    /// Rows transferred by the single combined `GP0(A0h)` upload.
    pub upload_rows: u16,
    /// Caller-owned scratch halfwords required for that upload.
    pub scratch_halfwords: usize,
}

#[inline]
fn placements_overlap(
    a: AtlasPlacement,
    a_w: u16,
    a_h: u16,
    b: AtlasPlacement,
    b_w: u16,
    b_h: u16,
) -> bool {
    a.page == b.page
        && a.x_halfwords < b.x_halfwords + b_w
        && b.x_halfwords < a.x_halfwords + a_w
        && a.y < b.y + b_h
        && b.y < a.y + a_h
}

/// Plan page-local atlas rectangles with a deterministic, bounded
/// bottom-left bin layout. Tall atlases are placed first; candidate corners
/// come only from page edges and already-placed rectangle edges, avoiding a
/// texel-by-texel search on the PS1 boot path.
fn plan_font_pack(
    fonts: &[&'static BitmapFont],
    placements: &mut [Option<AtlasPlacement>; MAX_PACKED_FONT_ATLASES],
) -> Option<FontPackMetrics> {
    if fonts.is_empty() || fonts.len() > MAX_PACKED_FONT_ATLASES {
        return None;
    }
    placements.fill(None);

    let mut page_count = 0u16;
    for _ in 0..fonts.len() {
        // Height-first, then width-first is a shelf/bin heuristic that keeps
        // the page count low while preserving the caller's output ordering.
        let mut selected = None;
        let mut selected_key = (0u16, 0u16);
        for (index, &font) in fonts.iter().enumerate() {
            if placements[index].is_some() {
                continue;
            }
            let (_, _, height, width) = font_atlas_dims(font);
            if width == 0 || width as usize > FONT_PAGE_HALFWORDS || height == 0 || height > 256 {
                return None;
            }
            let key = (height, width);
            if selected.is_none() || key > selected_key {
                selected = Some(index);
                selected_key = key;
            }
        }
        let index = selected?;
        let (_, _, height, width) = font_atlas_dims(fonts[index]);

        let mut chosen = None;
        // Include one new page; allocations below may grow page_count.
        let available_pages = page_count;
        for page in 0..=available_pages {
            if page == page_count && page_count >= 16 {
                break;
            }
            let mut page_best = None;

            // In a bottom-left-stable layout, a rectangle's left/bottom edge
            // touches either the page edge or another rectangle's right/bottom
            // edge. Enumerating those corners is exhaustive for this policy.
            for y_source in 0..=fonts.len() {
                let y = if y_source == 0 {
                    0
                } else {
                    let prior_index = y_source - 1;
                    let Some(prior) = placements[prior_index] else {
                        continue;
                    };
                    if prior.page != page {
                        continue;
                    }
                    let (_, _, prior_h, _) = font_atlas_dims(fonts[prior_index]);
                    prior.y + prior_h
                };
                if y + height > 256 {
                    continue;
                }
                for x_source in 0..=fonts.len() {
                    let x_halfwords = if x_source == 0 {
                        0
                    } else {
                        let prior_index = x_source - 1;
                        let Some(prior) = placements[prior_index] else {
                            continue;
                        };
                        if prior.page != page {
                            continue;
                        }
                        let (_, _, _, prior_w) = font_atlas_dims(fonts[prior_index]);
                        prior.x_halfwords + prior_w
                    };
                    if x_halfwords + width > FONT_PAGE_HALFWORDS as u16 {
                        continue;
                    }
                    let candidate = AtlasPlacement {
                        page,
                        x_halfwords,
                        y,
                    };
                    let overlaps = placements.iter().enumerate().any(|(prior_index, prior)| {
                        let Some(prior) = prior else {
                            return false;
                        };
                        let (_, _, prior_h, prior_w) = font_atlas_dims(fonts[prior_index]);
                        placements_overlap(candidate, width, height, *prior, prior_w, prior_h)
                    });
                    if overlaps {
                        continue;
                    }
                    if page_best.is_none_or(|best: AtlasPlacement| {
                        (candidate.y, candidate.x_halfwords) < (best.y, best.x_halfwords)
                    }) {
                        page_best = Some(candidate);
                    }
                }
            }
            if let Some(best) = page_best {
                chosen = Some(best);
                if page == page_count {
                    page_count += 1;
                }
                break;
            }
        }
        placements[index] = Some(chosen?);
    }

    let upload_rows = placements
        .iter()
        .enumerate()
        .filter_map(|(index, placement)| {
            let placement = placement.as_ref()?;
            let (_, _, height, _) = font_atlas_dims(fonts[index]);
            Some(placement.y + height)
        })
        .max()?;
    let scratch_halfwords = usize::from(page_count)
        .checked_mul(FONT_PAGE_HALFWORDS)?
        .checked_mul(usize::from(upload_rows))?;
    Some(FontPackMetrics {
        pages: page_count,
        upload_rows,
        scratch_halfwords,
    })
}

/// Return the page-run and scratch requirements for [`upload_fonts`] without
/// reserving VRAM or touching the GPU.
pub fn font_pack_metrics(fonts: &[&'static BitmapFont]) -> Option<FontPackMetrics> {
    let mut placements = [None; MAX_PACKED_FONT_ATLASES];
    plan_font_pack(fonts, &mut placements)
}

fn pack_font_bits(
    fonts: &[&'static BitmapFont],
    placements: &[Option<AtlasPlacement>; MAX_PACKED_FONT_ATLASES],
    metrics: FontPackMetrics,
    scratch: &mut [u16],
) -> Option<()> {
    if metrics.scratch_halfwords > scratch.len() {
        return None;
    }
    let stride = usize::from(metrics.pages) * FONT_PAGE_HALFWORDS;
    scratch[..metrics.scratch_halfwords].fill(0);
    for (slot, &font) in fonts.iter().enumerate() {
        let placement = placements[slot]?;
        let (glyphs_per_row, _, _, _) = font_atlas_dims(font);
        let col0 =
            usize::from(placement.page) * FONT_PAGE_HALFWORDS + usize::from(placement.x_halfwords);
        for gi in 0..font.glyph_count {
            let atlas_col = gi % glyphs_per_row;
            let atlas_row = gi / glyphs_per_row;
            let base_x = atlas_col * cell_width(font);
            let base_y = placement.y + atlas_row * font.glyph_h as u16;
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
    }
    Some(())
}

/// Upload several fonts in a **single** `GP0(A0h)` image transfer.
///
/// Repeated per-font VRAM uploads desync the GPU command stream on some
/// targets and corrupt subsequent (world) rendering; packing every glyph
/// atlas side-by-side into one reserved page run and emitting one transfer
/// avoids that entirely. Multiple atlas rectangles share each 256x256 4bpp
/// tpage through page-local UV origins; all fonts share one 2-entry CLUT
/// (transparent, white) which `tint` recolours per draw.
///
/// `scratch` is a caller-owned buffer (put it in `static mut` BSS, **not** on
/// the stack); query [`font_pack_metrics`] for the exact required length.
/// `out` receives one `Some(FontAtlas)` per font; trailing slots
/// are set to `None`. Returns the handles to free the set later, or `None`
/// (uploading nothing) if a font is too wide, `scratch`/`out` is too small,
/// or the allocator is out of space. Every failure clears all of `out`, so a
/// caller can never mistake stale or partially prepared handles for the set.
pub fn upload_fonts<R: VramRegionSource>(
    fonts: &[&'static BitmapFont],
    alloc: &mut R,
    scratch: &mut [u16],
    out: &mut [Option<FontAtlas>],
) -> Option<FontSetVram> {
    let n = fonts.len();
    out.fill(None);
    if n == 0 || n > out.len() {
        return None;
    }
    let mut placements = [None; MAX_PACKED_FONT_ATLASES];
    let metrics = plan_font_pack(fonts, &mut placements)?;
    if metrics.scratch_halfwords > scratch.len() {
        return None;
    }

    let (base, pages) = alloc.alloc_page_run(metrics.pages, TexDepth::Bit4, 0)?;
    // CLUT allocation can't fail at boot (the band is empty); if it ever does
    // the page run is left reserved, which is harmless at boot.
    let (clut, clut_handle) = alloc.alloc_clut(2)?;

    pack_font_bits(fonts, &placements, metrics, scratch)?;
    for (slot, &font) in fonts.iter().enumerate() {
        let placement = placements[slot]?;
        let (glyphs_per_row, _, _, _) = font_atlas_dims(font);
        out[slot] = Some(FontAtlas {
            font,
            tpage: Tpage::new(
                base.x() + placement.page * FONT_PAGE_HALFWORDS as u16,
                base.y(),
                TexDepth::Bit4,
            ),
            clut_word: clut.uv_clut_word(),
            glyphs_per_row,
            uv_origin: ((placement.x_halfwords * 4) as u8, placement.y as u8),
        });
    }
    for slot in out.iter_mut().skip(n) {
        *slot = None;
    }

    // One image transfer for the whole combined atlas, then the shared CLUT.
    let stride = usize::from(metrics.pages) * FONT_PAGE_HALFWORDS;
    upload_16bpp(
        VramRect::new(base.x(), base.y(), stride as u16, metrics.upload_rows),
        &scratch[..metrics.scratch_halfwords],
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

        // Cells are padded to a whole halfword (4 texels at 4bpp) so every
        // glyph STARTS on a halfword boundary. A 5-wide font otherwise puts
        // each glyph at an arbitrary nibble, and on real hardware the demo
        // disc's SPLEEN 5x8 description text rendered lowercase 'f' as a
        // bare crossbar while every emulator drew it whole (hwtest 0xB3-
        // 0xB5, 2026-08-07 console capture). The padding costs VRAM and
        // changes nothing about the drawn pixels: `draw_text` still emits
        // glyph_w-wide rects, only the atlas coordinates move.
        let cell_w = glyph_w.div_ceil(4) * 4;

        // Atlas width = glyphs_per_row × cell_w, capped at the
        // MAX to stay within a single 4bpp tpage.
        let max_cols = (Self::MAX_ATLAS_W_TEXELS / cell_w).max(1);
        let glyphs_per_row = glyph_count.min(max_cols);
        let glyph_rows = glyph_count.div_ceil(glyphs_per_row);
        let atlas_w = glyphs_per_row * cell_w;
        let atlas_h = glyph_rows * glyph_h;

        // Pack 1bpp source → 4bpp VRAM texture into a stack buffer.
        // Each 16-bit halfword holds 4 texels (nibble 0 = leftmost).
        // The stride is rounded to an even number of halfwords because
        // `upload_16bpp` moves whole words and rejects an odd pixel count;
        // cell padding can otherwise leave an odd row (KENNEY_PIXEL lands
        // on 63). The spare column stays zero.
        let halfwords_per_row = atlas_w.div_ceil(4).next_multiple_of(2);
        let total_halfwords = halfwords_per_row as usize * atlas_h as usize;
        assert!(
            total_halfwords <= Self::MAX_PACK_HALFWORDS,
            "font atlas too large for stack buffer",
        );
        let mut packed = [0u16; Self::MAX_PACK_HALFWORDS];

        for gi in 0..glyph_count {
            let atlas_col = gi % glyphs_per_row;
            let atlas_row = gi / glyphs_per_row;
            let base_x = atlas_col * cell_w;
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
            clut_word: clut.uv_clut_word(),
            glyphs_per_row,
            uv_origin: (0, 0),
        }
    }

    /// Pixel width of `text` when rendered with this atlas.
    pub fn text_width(&self, text: &str) -> u16 {
        self.font.text_width(text)
    }

    /// Pixel advance of one character, the per-glyph term
    /// [`FontAtlas::text_width`] sums.
    ///
    /// Exposed so a caller scanning a string can carry a running width
    /// instead of re-measuring every prefix: word wrap is quadratic
    /// otherwise, which on R3000 is the most expensive thing a text layout
    /// does. `text_width(s)` stays exactly
    /// `s.chars().map(glyph_advance).fold(0, u16::saturating_add)`.
    pub fn glyph_advance(&self, ch: char) -> u8 {
        self.font.glyph_advance(ch)
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
        let u = u16::from(self.uv_origin.0) + col * cell_width(self.font);
        let v = u16::from(self.uv_origin.1) + row * self.font.glyph_h as u16;
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
        let clut_word = self.clut_word;
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

    /// Draw `text` with the GPU's semi-transparency enabled, blending
    /// each glyph against the framebuffer with `blend`'s equation.
    ///
    /// **Fast path** -- same GP0 0x64 textured rectangle and same 4
    /// words per glyph as [`Self::draw_text`], with the primitive's
    /// semi-transparency bit set and the draw mode's ABR field carrying
    /// the equation. The only extra cost is the one draw-mode word at
    /// the start and the one that restores the opaque mode at the end.
    ///
    /// Why this exists: the opaque path can only recolour a glyph, and
    /// a tint ramped toward black draws a *black* glyph, not an absent
    /// one. [`TextBlend::Add`] plus a ramped tint is the PS1's real
    /// fade -- at tint zero the glyph contributes nothing and vanishes.
    ///
    /// Restores the atlas's opaque draw mode before returning, so a
    /// blended run cannot leave a stray blend equation on the GPU for
    /// whatever draws next. As with [`Self::draw_text`], the tpage slot
    /// is left pointing at this atlas.
    pub fn draw_text_blended(
        &self,
        x: i16,
        y: i16,
        text: &str,
        tint: (u8, u8, u8),
        blend: TextBlend,
    ) {
        self.draw_text_blended_with_spacing(x, y, text, 0, tint, blend);
    }

    /// [`Self::draw_text_blended`] with signed inter-character spacing,
    /// measured in screen pixels and not applied after the last glyph.
    pub fn draw_text_blended_with_spacing(
        &self,
        x: i16,
        y: i16,
        text: &str,
        letter_spacing: i8,
        tint: (u8, u8, u8),
        blend: TextBlend,
    ) {
        wait_cmd_ready();
        write_gp0(blended_draw_mode_word(self.tpage, blend));
        reset_texture_window();

        let font = self.font;
        let clut_word = self.clut_word;
        // 0x66 rather than 0x64: same variable-size textured rectangle,
        // with bit 25 (semi-transparency) set so the ABR field above
        // actually applies.
        let color_cmd = SEMI_TRANSPARENT_RECT_CMD | pack_color(tint.0, tint.1, tint.2);
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
            write_gp0(color_cmd);
            write_gp0(pack_vertex(cursor_x, y));
            write_gp0(pack_texcoord(u, v, clut_word));
            write_gp0(glyph_size);

            cursor_x = cursor_x
                .wrapping_add(font.glyph_advance(ch) as i16)
                .wrapping_add(gap);
        }

        // Leave the GPU in the opaque mode every other draw path assumes.
        self.tpage.apply_as_draw_mode();
    }

    /// Draw a flat semi-transparent rectangle in `blend`'s equation --
    /// a backing plate for a run of text drawn on top of it.
    ///
    /// This is a method on the atlas rather than a free function
    /// because the blend equation is carried by the GP0(E1h) draw-mode
    /// word, which also names a texture page. Setting one means setting
    /// the other, so whatever changes the equation has to know which
    /// page the glyphs that follow will want back -- and the atlas is
    /// what knows that. It restores the atlas's opaque draw mode on the
    /// way out, exactly as [`Self::draw_text_blended`] does.
    ///
    /// [`TextBlend::Subtract`] is the equation to reach for behind
    /// text: it darkens whatever is under it and, because it scales
    /// with the tint, it fades to nothing along with the text it seats.
    /// [`TextBlend::Average`] cannot fade -- at tint zero it still
    /// halves the background, so a plate drawn with it outlives the
    /// glyphs and is left behind as a grey box.
    pub fn draw_backdrop_blended(
        &self,
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        tint: (u8, u8, u8),
        blend: TextBlend,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        wait_cmd_ready();
        write_gp0(blended_draw_mode_word(self.tpage, blend));
        write_gp0(SEMI_TRANSPARENT_FLAT_RECT_CMD | pack_color(tint.0, tint.1, tint.2));
        write_gp0(pack_vertex(x, y));
        write_gp0(pack_xy(w, h));
        self.tpage.apply_as_draw_mode();
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
        let clut = self.clut_word;
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
        let clut = self.clut_word;
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
        let clut = self.clut_word;
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
        let clut = self.clut_word;
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
        let clut = self.clut_word;
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
    extern crate std;

    use super::{
        blended_draw_mode_word, TextBlend, SEMI_TRANSPARENT_FLAT_RECT_CMD,
        SEMI_TRANSPARENT_RECT_CMD,
    };
    use psx_vram::{TexDepth, Tpage};

    /// The blend equation is selected by the draw mode, not the
    /// primitive, so these two bits are the whole mechanism. Pin them
    /// against the hardware's documented ABR encoding.
    #[test]
    fn text_blend_maps_to_the_hardware_abr_field() {
        assert_eq!(TextBlend::Average.abr(), 0);
        assert_eq!(TextBlend::Add.abr(), 1);
        assert_eq!(TextBlend::Subtract.abr(), 2);
        assert_eq!(TextBlend::AddQuarter.abr(), 3);
    }

    /// The blended draw mode must differ from the opaque one in the ABR
    /// field and NOWHERE else: same page, same depth, same flags. A
    /// stray bit here would silently repoint the atlas or change its
    /// colour depth mid-string.
    #[test]
    fn blended_draw_mode_only_moves_the_abr_bits() {
        for tpage in [
            Tpage::new(0, 0, TexDepth::Bit4),
            Tpage::new(640, 256, TexDepth::Bit4),
            Tpage::new(896, 0, TexDepth::Bit8),
        ] {
            let opaque = blended_draw_mode_word(tpage, TextBlend::Average);
            for blend in [
                TextBlend::Average,
                TextBlend::Add,
                TextBlend::Subtract,
                TextBlend::AddQuarter,
            ] {
                let word = blended_draw_mode_word(tpage, blend);
                assert_eq!(
                    (word >> 5) & 3,
                    u32::from(blend.abr()),
                    "{blend:?} ABR field"
                );
                assert_eq!(
                    word & !(3 << 5),
                    opaque & !(3 << 5),
                    "{blend:?} changed something other than ABR"
                );
            }
            // The low nine bits are the same page/blend/depth encoding
            // the SDK embeds in a textured primitive's UV word, so the
            // rect path and the quad path address one atlas identically.
            for blend in [TextBlend::Average, TextBlend::Add, TextBlend::Subtract] {
                assert_eq!(
                    blended_draw_mode_word(tpage, blend) & 0x1FF,
                    u32::from(tpage.uv_tpage_word(blend.abr())),
                    "{blend:?} page/blend/depth encoding"
                );
            }
        }
    }

    /// The blended rectangle differs from the opaque one only by the
    /// semi-transparency bit. If these ever diverge further, the blended
    /// path has stopped being the same 4-word primitive.
    #[test]
    fn blended_rect_command_is_the_opaque_one_plus_bit_25() {
        const OPAQUE_RECT_CMD: u32 = 0x6400_0000;
        assert_eq!(SEMI_TRANSPARENT_RECT_CMD, OPAQUE_RECT_CMD | (1 << 25));
    }

    /// The backdrop is the MONOCHROME rectangle, not the textured one:
    /// same bit-25 rule, one opcode lower, and three data words instead
    /// of four however large the plate is. That word count is the whole
    /// reason a plate is cheaper than a second pass over the glyphs.
    #[test]
    fn blended_backdrop_command_is_the_flat_rect_plus_bit_25() {
        const OPAQUE_FLAT_RECT_CMD: u32 = 0x6000_0000;
        assert_eq!(
            SEMI_TRANSPARENT_FLAT_RECT_CMD,
            OPAQUE_FLAT_RECT_CMD | (1 << 25)
        );
        // Untextured, so it must NOT carry the textured rect's opcode.
        assert_ne!(SEMI_TRANSPARENT_FLAT_RECT_CMD, SEMI_TRANSPARENT_RECT_CMD);
    }

    /// The wrap scan in `psx-engine`'s UI carries a running sum of
    /// `glyph_advance` instead of re-measuring each prefix with
    /// `text_width`. That is only bit-exact while `text_width` stays the
    /// saturating fold of the per-character advances, so pin it here --
    /// including the proportional table, the out-of-range fallback and the
    /// u16 saturation.
    #[test]
    fn text_width_is_the_saturating_fold_of_glyph_advance() {
        const BITMAP: [u8; 8] = [0; 8];
        const ADVANCES: [u8; 4] = [3, 9, 0, 255];
        let proportional = super::BitmapFont {
            glyph_w: 8,
            glyph_h: 8,
            first_char: b'a' as u16,
            glyph_count: 4,
            bitmap: &BITMAP,
            glyph_advances: Some(&ADVANCES),
            advance_x: 7,
            line_height: 8,
            bit_order: super::BitOrder::Lsb,
        };
        let fixed = super::BitmapFont {
            glyph_advances: None,
            ..proportional
        };

        assert_eq!(proportional.glyph_advance('a'), 3);
        assert_eq!(proportional.glyph_advance('d'), 255);
        // Outside the covered range: falls back to `advance_x`.
        assert_eq!(proportional.glyph_advance('z'), 7);
        assert_eq!(fixed.glyph_advance('a'), 7);

        for font in [&proportional, &fixed] {
            for text in ["", "a", "ad", "abcd", "abcdz", "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"] {
                let folded = text
                    .chars()
                    .map(|ch| u16::from(font.glyph_advance(ch)))
                    .fold(0u16, u16::saturating_add);
                assert_eq!(font.text_width(text), folded, "text_width({text:?})");
            }
        }
        // The long run really does saturate, so the case above is load-bearing.
        assert_eq!(
            proportional.text_width(&std::string::String::from("d").repeat(258)),
            u16::MAX
        );
    }

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

    const TOO_TALL_FONT: BitmapFont = BitmapFont {
        glyph_w: 255,
        glyph_h: 255,
        glyph_count: 2,
        bitmap: &[],
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

    #[test]
    fn zen_dots_display_is_native_size_and_fits_the_ps1_upload_budget() {
        let font = &crate::fonts::ZEN_DOTS_DISPLAY;
        assert_eq!(font.first_char, b'A' as u16);
        assert_eq!(font.glyph_count, 26);
        assert_eq!(font.glyph_h, 27);
        assert_eq!(font.text_width("CORTEX"), 162);

        let (_, atlas_w, atlas_h, halfwords_per_row) = font_atlas_dims(font);
        assert!(atlas_w <= FontAtlas::MAX_ATLAS_W_TEXELS);
        assert!(atlas_h <= u8::MAX as u16);
        assert!(
            usize::from(halfwords_per_row) * usize::from(atlas_h) <= FontAtlas::MAX_PACK_HALFWORDS
        );
    }

    #[test]
    fn drippy_space_is_display_only_and_fits_the_ps1_upload_budget() {
        for font in [
            &crate::fonts::DRIPPY_SPACE,
            &crate::fonts::DRIPPY_SPACE_DISPLAY,
        ] {
            let expected_first = if font.glyph_count == 26 { b'A' } else { b' ' };
            assert_eq!(font.first_char, expected_first as u16);
            let (_, atlas_w, atlas_h, halfwords_per_row) = font_atlas_dims(font);
            assert!(atlas_w <= FontAtlas::MAX_ATLAS_W_TEXELS);
            assert!(atlas_h <= u8::MAX as u16);
            assert!(
                usize::from(halfwords_per_row) * usize::from(atlas_h)
                    <= FontAtlas::MAX_PACK_HALFWORDS
            );
        }
        assert_eq!(crate::fonts::DRIPPY_SPACE.glyph_count, 0x5B - 0x20);
        assert_eq!(crate::fonts::DRIPPY_SPACE_DISPLAY.glyph_count, 26);
    }

    const ARENA_FONTS: &[&BitmapFont] = &[
        &crate::fonts::ZEN_DOTS_DISPLAY,
        &crate::fonts::ZEN_DOTS,
        &crate::fonts::KENNEY_FUTURE_NARROW,
        &crate::fonts::KENNEY_PIXEL,
        &crate::fonts::KENNEY_FUTURE,
        &crate::fonts::BASIC,
    ];

    #[test]
    fn arena_six_font_set_has_a_provably_minimal_two_page_pack() {
        let mut placements = [None; MAX_PACKED_FONT_ATLASES];
        let metrics = plan_font_pack(ARENA_FONTS, &mut placements).unwrap();
        assert_eq!(metrics.pages, 2, "six fonts must share two tpages");
        assert_eq!(metrics.upload_rows, 241);
        assert_eq!(metrics.scratch_halfwords, 2 * 64 * 241);
        assert_eq!(font_pack_metrics(ARENA_FONTS), Some(metrics));

        // More than one complete page of rectangle area is occupied, so the
        // two-page result is not merely heuristic for this shipping set: it is
        // the mathematical minimum without rotating or modifying an atlas.
        let occupied_halfwords: usize = ARENA_FONTS
            .iter()
            .map(|font| {
                let (_, _, h, w) = font_atlas_dims(font);
                usize::from(h) * usize::from(w)
            })
            .sum();
        assert!(occupied_halfwords > FONT_PAGE_HALFWORDS * 256);
        assert!(occupied_halfwords <= 2 * FONT_PAGE_HALFWORDS * 256);

        for (a_index, a) in placements[..ARENA_FONTS.len()].iter().enumerate() {
            let a = a.unwrap();
            let (_, _, a_h, a_w) = font_atlas_dims(ARENA_FONTS[a_index]);
            assert!(a.x_halfwords + a_w <= FONT_PAGE_HALFWORDS as u16);
            assert!(a.y + a_h <= 256);
            for (b_index, b) in placements[..a_index].iter().enumerate() {
                let b = b.unwrap();
                let (_, _, b_h, b_w) = font_atlas_dims(ARENA_FONTS[b_index]);
                assert!(!placements_overlap(a, a_w, a_h, b, b_w, b_h));
            }
        }
    }

    #[test]
    fn packed_arena_atlases_preserve_every_glyph_and_padding_texel() {
        let mut placements = [None; MAX_PACKED_FONT_ATLASES];
        let metrics = plan_font_pack(ARENA_FONTS, &mut placements).unwrap();
        let mut scratch = std::vec![0xFFFF; metrics.scratch_halfwords];
        pack_font_bits(ARENA_FONTS, &placements, metrics, &mut scratch).unwrap();
        let stride = usize::from(metrics.pages) * FONT_PAGE_HALFWORDS;

        for (font_index, &font) in ARENA_FONTS.iter().enumerate() {
            let placement = placements[font_index].unwrap();
            let (glyphs_per_row, atlas_w, atlas_h, _) = font_atlas_dims(font);
            let cell_w = cell_width(font);
            for local_y in 0..atlas_h {
                let glyph_row = local_y / u16::from(font.glyph_h);
                let glyph_y = (local_y % u16::from(font.glyph_h)) as u8;
                for local_x in 0..atlas_w {
                    let glyph_col = local_x / cell_w;
                    let glyph_x = local_x % cell_w;
                    let glyph_index = glyph_row * glyphs_per_row + glyph_col;
                    let expected =
                        if glyph_index < font.glyph_count && glyph_x < u16::from(font.glyph_w) {
                            ((font.glyph_row_packed(glyph_index, glyph_y) >> glyph_x) & 1) as u16
                        } else {
                            0
                        };
                    let global_halfword = usize::from(placement.page) * FONT_PAGE_HALFWORDS
                        + usize::from(placement.x_halfwords)
                        + usize::from(local_x / 4);
                    let word =
                        scratch[usize::from(placement.y + local_y) * stride + global_halfword];
                    let actual = (word >> ((local_x & 3) * 4)) & 0xF;
                    assert_eq!(
                        actual, expected,
                        "font {font_index} texel ({local_x},{local_y})"
                    );
                }
            }
        }
    }

    #[test]
    fn packed_origins_feed_exact_rect_and_quad_packet_uvs() {
        let fonts = [&TEST_FONT, &TEST_FONT];
        let mut placements = [None; MAX_PACKED_FONT_ATLASES];
        let metrics = plan_font_pack(&fonts, &mut placements).unwrap();
        assert_eq!(metrics.pages, 1);
        assert_eq!(placements[0].unwrap().x_halfwords, 0);
        assert_eq!(placements[1].unwrap().x_halfwords, 4);

        let clut = Clut::new(960, 500);
        for (index, &font) in fonts.iter().enumerate() {
            let placement = placements[index].unwrap();
            let (glyphs_per_row, _, _, _) = font_atlas_dims(font);
            let atlas = FontAtlas {
                font,
                tpage: Tpage::new(placement.page * 64, 0, TexDepth::Bit4),
                clut_word: clut.uv_clut_word(),
                glyphs_per_row,
                uv_origin: ((placement.x_halfwords * 4) as u8, placement.y as u8),
            };
            for glyph_index in 0..font.glyph_count {
                let ch = char::from_u32(u32::from(font.first_char + glyph_index)).unwrap();
                let (u, v) = atlas.glyph_uv(ch).unwrap();
                let expected_u = u16::from(atlas.uv_origin.0)
                    + (glyph_index % glyphs_per_row) * cell_width(font);
                let expected_v = u16::from(atlas.uv_origin.1)
                    + (glyph_index / glyphs_per_row) * u16::from(font.glyph_h);
                assert_eq!((u16::from(u), u16::from(v)), (expected_u, expected_v));

                // GP0(64h) takes this packed UV/CLUT word. Every quad path
                // shares `glyph_uv` and only extends the same top-left corner.
                let rect_uv_word = pack_texcoord(u, v, atlas.clut_word);
                assert_eq!(rect_uv_word & 0xFFFF, u32::from(u) | (u32::from(v) << 8));
                assert_eq!((rect_uv_word >> 16) as u16, atlas.clut_word);
                let quad = glyph_quad_uvs(u, v, font.glyph_w, font.glyph_h);
                assert_eq!(quad[0], (u, v));
                assert_eq!(quad[3].0, u.saturating_add(font.glyph_w));
                assert_eq!(quad[3].1, v.saturating_add(font.glyph_h));
            }
        }
    }

    #[test]
    fn font_pack_planner_fails_closed_outside_its_bound() {
        assert_eq!(font_pack_metrics(&[]), None);
        let too_many = [&TEST_FONT; MAX_PACKED_FONT_ATLASES + 1];
        assert_eq!(font_pack_metrics(&too_many), None);

        assert_eq!(font_pack_metrics(&[&TOO_TALL_FONT]), None);
    }

    #[test]
    fn every_pre_upload_failure_clears_stale_atlas_outputs() {
        struct NoAllocationExpected;
        impl VramRegionSource for NoAllocationExpected {
            fn alloc_page_run(
                &mut self,
                _count: u16,
                _depth: TexDepth,
                _page_y: u16,
            ) -> Option<(Tpage, VramHandle)> {
                panic!("preflight failure must not reserve pages")
            }

            fn alloc_clut(&mut self, _entries: u16) -> Option<(Clut, VramHandle)> {
                panic!("preflight failure must not reserve a CLUT")
            }
        }

        let stale = FontAtlas {
            font: &TEST_FONT,
            tpage: Tpage::new(0, 0, TexDepth::Bit4),
            clut_word: Clut::new(0, 480).uv_clut_word(),
            glyphs_per_row: 2,
            uv_origin: (0, 0),
        };
        let mut alloc = NoAllocationExpected;

        let mut out = [Some(stale); 2];
        assert!(upload_fonts(&[], &mut alloc, &mut [], &mut out).is_none());
        assert!(out.iter().all(Option::is_none));

        out.fill(Some(stale));
        assert!(upload_fonts(
            &[&TEST_FONT, &TEST_FONT, &TEST_FONT],
            &mut alloc,
            &mut [],
            &mut out,
        )
        .is_none());
        assert!(out.iter().all(Option::is_none));

        out.fill(Some(stale));
        assert!(upload_fonts(&[&TEST_FONT], &mut alloc, &mut [], &mut out).is_none());
        assert!(out.iter().all(Option::is_none));
    }
}
