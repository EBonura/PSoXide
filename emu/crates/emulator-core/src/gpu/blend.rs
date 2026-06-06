/// Convert a 24-bit RGB value (as written by the CPU in GP0 packets)
/// into the 15-bit BGR word VRAM stores. Matches Redux / PS1
/// hardware: the 3 high bits of each channel are discarded.
pub(super) fn rgb24_to_bgr15(rgb24: u32) -> u16 {
    let r = ((rgb24 >> 3) & 0x1F) as u16;
    let g = (((rgb24 >> 8) >> 3) & 0x1F) as u16;
    let b = (((rgb24 >> 16) >> 3) & 0x1F) as u16;
    r | (g << 5) | (b << 10)
}

/// Redux's 4×4 dither coefficient table, indexed by
/// `(y & 3) * 4 + (x & 3)`. See `s_dithertable` in
/// `pcsx-redux/src/gpu/soft/soft.cc`. Note these are NOT the signed
/// Bayer offsets you'll see quoted in some PSX-SPX derivatives --
/// they're threshold coefficients for Redux's "conditional round-up"
/// dither model, which produces the exact bit pattern PSX hardware
/// uses.
const DITHER_COEFFS: [u8; 16] = [7, 0, 6, 1, 2, 5, 3, 4, 1, 6, 0, 7, 4, 3, 5, 2];

/// Dither an 8-bit RGB triple to 15bpp, matching Redux's
/// `prepareDitherLut` / `applyDither` byte-for-byte.
///
/// The algorithm: for each channel split into a 5-bit quotient and a
/// 3-bit remainder; if the remainder beats the coefficient for this
/// pixel AND the quotient isn't already saturated (0x1F), round the
/// quotient up by one. That produces the characteristic PSX 4×4
/// dither pattern -- fundamentally different from the additive
/// `-4..+3` offset model PSX-SPX sometimes describes, and
/// producing different bit patterns. Matching Redux's algorithm
/// exactly is the only way to hit pixel-exact parity on Gouraud
/// gradients (which is most of what the Sony logo is).
pub(super) fn dither_rgb(r: i32, g: i32, b: i32, x: i32, y: i32) -> u16 {
    let coeff = DITHER_COEFFS[((y & 3) * 4 + (x & 3)) as usize] as u32;
    let r = r.clamp(0, 255) as u32;
    let g = g.clamp(0, 255) as u32;
    let b = b.clamp(0, 255) as u32;
    let mut rc = r >> 3;
    let mut gc = g >> 3;
    let mut bc = b >> 3;
    // Round-up rule: if the low 3 bits exceed the coefficient AND we
    // have headroom, increment. The saturation guard is essential --
    // without it pure-white pixels would get stuck rounding up past
    // 0x1F and wrapping (or in Redux's case, indexing past the
    // precomputed LUT).
    if rc < 0x1F && (r & 7) > coeff {
        rc += 1;
    }
    if gc < 0x1F && (g & 7) > coeff {
        gc += 1;
    }
    if bc < 0x1F && (b & 7) > coeff {
        bc += 1;
    }
    (bc << 10) as u16 | ((gc << 5) as u16) | rc as u16
}

/// PSX semi-transparency mode. The four non-`Opaque` variants map
/// directly to the four encodings in GP0 0xE1 / tpage bits 5-6.
/// `Opaque` is our shortcut for "don't touch the destination -- just
/// overwrite" so primitives share one rasterizer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
/// Matches Redux's per-channel arithmetic in
/// `pcsx-redux/src/gpu/soft/soft.cc` byte-for-byte. The subtle one
/// is **Average**: Redux computes `(bg >> 1) + (fg >> 1)` independent
/// per-channel, dropping each operand's LSB *before* summing. The
/// naive `(bg + fg) / 2` rounds differently when both inputs are
/// odd -- e.g. `(3 + 3) / 2 = 3` vs Redux's `1 + 1 = 2`. That bug
/// alone produces off-by-1 diffs on the Sony logo's semi-
/// transparent gradient edges.
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
        // Half-back + half-front -- per-channel right-shift before
        // summing, matching Redux's `& 0x7bde >> 1` pattern.
        BlendMode::Average => (
            (br >> 1) + (fr >> 1),
            (bgg >> 1) + (fgg >> 1),
            (bb >> 1) + (fb >> 1),
        ),
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
