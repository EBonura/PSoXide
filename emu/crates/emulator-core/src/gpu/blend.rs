/// Convert a 24-bit RGB value (as written by the CPU in GP0 packets)
/// into the 15-bit BGR word VRAM stores. Matches Redux / PS1
/// hardware: the 3 high bits of each channel are discarded.
pub(super) fn rgb24_to_bgr15(rgb24: u32) -> u16 {
    let r = ((rgb24 >> 3) & 0x1F) as u16;
    let g = (((rgb24 >> 8) >> 3) & 0x1F) as u16;
    let b = (((rgb24 >> 16) >> 3) & 0x1F) as u16;
    r | (g << 5) | (b << 10)
}

/// The PS1's 4×4 ordered-dither matrix, indexed by
/// `(y & 3) * 4 + (x & 3)`. These are the *signed* per-pixel offsets
/// the PSX-SPX spec defines for 24-bit-to-15-bit dithering: each
/// channel's 8-bit value has the offset added before the high 3 bits
/// are dropped. The offsets sum to zero over the tile, so dithering
/// is brightness-neutral -- it rounds individual pixels both up and
/// down to approximate intermediate shades, which is what produces
/// the characteristic checkerboard on a flat mid-tone.
const DITHER_OFFSETS: [i32; 16] = [-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2];

/// Dither an 8-bit RGB triple to 15bpp using the PS1's signed
/// additive 4×4 ordered-dither matrix (PSX-SPX '24bit-to-15bit
/// dithering').
///
/// The algorithm per channel: add this pixel's signed matrix offset
/// to the 8-bit value, clamp the result to `0..=255`, then take the
/// high 5 bits (`>> 3`). The clamp doubles as the saturation guard:
/// a 255 channel with a `+3` offset clamps back to 255 (still 31),
/// and a 0 channel with a `-4` offset clamps to 0. Because the
/// offsets are signed (range `-4..=+3`) a pixel can round *down* as
/// well as up -- e.g. a flat 24-bit mid-grey dithers to an
/// alternating 15/16 checkerboard rather than a uniform value,
/// matching PS1 hardware.
pub(super) fn dither_rgb(r: i32, g: i32, b: i32, x: i32, y: i32) -> u16 {
    let off = DITHER_OFFSETS[((y & 3) * 4 + (x & 3)) as usize];
    let ch = |c: i32| -> u16 { ((c + off).clamp(0, 255) >> 3) as u16 };
    ch(r) | (ch(g) << 5) | (ch(b) << 10)
}

/// PSX semi-transparency mode. The four non-`Opaque` variants map
/// directly to the four encodings in GP0 0xE1 / tpage bits 5-6.
/// `Opaque` is our shortcut for "don't touch the destination -- just
/// overwrite" so primitives share one rasterizer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlendMode {
    /// Write the foreground pixel directly, ignoring the background.
    Opaque,
    /// `(B + F) / 2` -- 50% average. Smoke, translucent glass.
    Average,
    /// `B + F`, channel-clamped to 31. Additive blending -- fire, lights.
    Add,
    /// `B - F`, channel-clamped to 0. Subtractive -- shadows.
    Sub,
    /// `B + F/4`, channel-clamped to 31. Low-intensity additive --
    /// subtle glow / haze.
    AddQuarter,
}

impl BlendMode {
    /// Decode the 2-bit tpage/E1-command "semi-transparency" field
    /// (bits 5-6 of GP0 0xE1, or of a textured-primitive tpage word).
    /// Always returns a non-`Opaque` variant -- whether the primitive
    /// actually blends is determined by the caller's "is this prim
    /// semi-transparent?" flag.
    pub(super) fn from_tpage_bits(bits: u32) -> Self {
        match bits & 0x3 {
            0 => Self::Average,
            1 => Self::Add,
            2 => Self::Sub,
            _ => Self::AddQuarter,
        }
    }
}

/// `true` if the primitive-command word's cmd-bit-1 ("semi-trans
/// flag") is set. GP0 primitive opcodes are laid out as
/// `0b001XXPTC` where bit 1 (= `P`) is the semi-trans flag: 0 means
/// opaque, 1 means the primitive blends per the active tpage mode.
#[inline]
pub(super) fn prim_is_semi_trans(cmd_word: u32) -> bool {
    (cmd_word >> 25) & 1 != 0
}

/// Resolve the blend mode for a non-textured primitive: opaque if
/// the cmd-bit-1 flag is clear, otherwise the current tpage's
/// active semi-transparency mode. Textured primitives don't use
/// this helper -- per-texel bit-15 controls blending there.
#[inline]
pub(super) fn prim_blend_mode(cmd_word: u32, tpage_mode: BlendMode) -> BlendMode {
    if prim_is_semi_trans(cmd_word) {
        tpage_mode
    } else {
        BlendMode::Opaque
    }
}

/// Modulate a 15bpp BGR texel by a 24-bit RGB tint. PSX formula:
/// `result_channel = (tint_8bit * texel_5bit * 2) / 0x100`, which
/// makes tint value `0x80` per channel act as identity (no-change)
/// and `0xFF` act as double-brightness (clamped to 31 per channel).
///
/// Called with `tint = 0x80_80_80` when the primitive is a "raw
/// texture" (cmd bit 0 set) -- that passes the texel through
/// unchanged. Callers of flat-tint textured primitives derive the
/// tint from the cmd word's low 24 bits; Gouraud-textured primitives
/// interpolate per pixel and call us with the per-pixel colour.
///
/// The texel's mask bit (bit 15) is preserved so downstream
/// semi-transparency detection still sees it.
pub(super) fn modulate_tint(texel: u16, tint_r: u32, tint_g: u32, tint_b: u32) -> u16 {
    let tr = (texel & 0x1F) as u32;
    let tg = ((texel >> 5) & 0x1F) as u32;
    let tb = ((texel >> 10) & 0x1F) as u32;
    let r = (tint_r * tr / 0x80).min(0x1F) as u16;
    let g = (tint_g * tg / 0x80).min(0x1F) as u16;
    let b = (tint_b * tb / 0x80).min(0x1F) as u16;
    r | (g << 5) | (b << 10) | (texel & 0x8000)
}

/// Dithered variant of [`modulate_tint`] -- computes the modulated
/// RGB in 8-bit space, applies the 4×4 Bayer dither offset for the
/// pixel position, then truncates to 5 bits per channel. Used by
/// textured-Gouraud primitives when GP0 0xE1 bit 9 is on.
pub(super) fn modulate_tint_dithered(
    texel: u16,
    tint_r: u32,
    tint_g: u32,
    tint_b: u32,
    x: i32,
    y: i32,
) -> u16 {
    // Scale 5-bit texel channels to 8-bit, apply the tint (which
    // is 0x80 = identity at 8-bit scale), then dither + truncate.
    let tr = ((texel & 0x1F) as u32) << 3;
    let tg = (((texel >> 5) & 0x1F) as u32) << 3;
    let tb = (((texel >> 10) & 0x1F) as u32) << 3;
    let r = (tint_r * tr / 0x80).min(0xFF) as i32;
    let g = (tint_g * tg / 0x80).min(0xFF) as i32;
    let b = (tint_b * tb / 0x80).min(0xFF) as i32;
    dither_rgb(r, g, b, x, y) | (texel & 0x8000)
}

/// Split a 24-bit RGB tint word (from the low 24 bits of a textured
/// primitive's command) into the three channels the modulator
/// expects. Returns `(tint_r, tint_g, tint_b)` with each in 0..=255.
/// For "raw texture" primitives the caller substitutes `(128, 128,
/// 128)` directly -- one code path through [`modulate_tint`].
#[inline]
pub(super) fn split_tint(tint24: u32) -> (u32, u32, u32) {
    (tint24 & 0xFF, (tint24 >> 8) & 0xFF, (tint24 >> 16) & 0xFF)
}

/// Identity tint -- pass-through for raw-texture primitives. Each
/// channel at `0x80` means modulation returns the texel unchanged.
pub(super) const RAW_TEXTURE_TINT: (u32, u32, u32) = (0x80, 0x80, 0x80);

/// Blend a foreground pixel over a background pixel per `mode`.
/// Both pixels are 15-bit BGR with a mask bit at bit 15. The mask
/// bit of the result comes from the foreground so semi-transparent
/// texels keep marking themselves.
///
/// **Average** (Mode 0) follows the PSX-SPX spec definition
/// `0.5*B + 0.5*F`, i.e. sum the two operands then halve: `(B + F) >> 1`
/// per channel. Summing before the shift keeps the low bit, so an
/// odd+odd pair such as `(3 + 3) >> 1 = 3` does not lose 1 LSB the way
/// a per-operand `(B >> 1) + (F >> 1)` approximation would (that gives
/// `1 + 1 = 2`, one step too dark). Range stays 0..=31, so no clamp is
/// needed.
pub(super) fn blend_pixel(bg: u16, fg: u16, mode: BlendMode) -> u16 {
    if mode == BlendMode::Opaque {
        return fg;
    }
    let br = (bg & 0x1F) as i16;
    let bgg = ((bg >> 5) & 0x1F) as i16;
    let bb = ((bg >> 10) & 0x1F) as i16;
    let fr = (fg & 0x1F) as i16;
    let fgg = ((fg >> 5) & 0x1F) as i16;
    let fb = ((fg >> 10) & 0x1F) as i16;
    let (r, g, b) = match mode {
        BlendMode::Opaque => unreachable!(),
        // Half-back + half-front -- sum the operands THEN halve, per the
        // PSX-SPX `0.5*B + 0.5*F` definition. Max is (31+31)>>1 = 31, so
        // the result never needs clamping.
        BlendMode::Average => (((br + fr) >> 1), ((bgg + fgg) >> 1), ((bb + fb) >> 1)),
        BlendMode::Add => ((br + fr).min(31), (bgg + fgg).min(31), (bb + fb).min(31)),
        BlendMode::Sub => ((br - fr).max(0), (bgg - fgg).max(0), (bb - fb).max(0)),
        // Full-back + quarter-front -- `fg / 4` via integer division
        // is the same as Redux's `(fg & 0x1c) >> 2` for 5-bit
        // channels: both truncate the low 2 bits then shift.
        BlendMode::AddQuarter => (
            (br + fr / 4).min(31),
            (bgg + fgg / 4).min(31),
            (bb + fb / 4).min(31),
        ),
    };
    (r as u16) | ((g as u16) << 5) | ((b as u16) << 10) | (fg & 0x8000)
}
