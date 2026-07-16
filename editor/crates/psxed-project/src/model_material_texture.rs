//! Host-side generation for compact model material layer textures.

use crate::ProceduralNoiseTexture;

const MATERIAL_NEUTRAL_TINT: u8 = 128;

/// Fixed texture size for the generated secondary model layer.
pub const MODEL_NOISE_TEXTURE_SIZE: u16 = 64;

/// Bake deterministic multi-octave value noise into a 64x64 CLUT16 PSXT.
///
/// Palette entry zero remains transparent, while entries 1..15 form a neutral
/// grayscale ramp. Runtime material tint supplies the authored colour.
pub fn generate_model_noise_psxt(settings: ProceduralNoiseTexture) -> Vec<u8> {
    let indices = generate_model_noise_indices(settings);
    let mut palette = [[0u8; 3]; 16];
    for (index, rgb) in palette.iter_mut().enumerate().skip(1) {
        let value = (index as u8).saturating_mul(17);
        *rgb = [value, value, value];
    }
    psxed_tex::encode_indexed_psxt(
        MODEL_NOISE_TEXTURE_SIZE,
        MODEL_NOISE_TEXTURE_SIZE,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        true,
    )
    .expect("fixed-size procedural noise PSXT input is valid")
}

/// Generate the raw 4bpp indices used by [`generate_model_noise_psxt`].
pub fn generate_model_noise_indices(settings: ProceduralNoiseTexture) -> Vec<u8> {
    const SIZE: usize = MODEL_NOISE_TEXTURE_SIZE as usize;
    let feature_size = settings.feature_size.clamp(2, 64) as u32;
    let octaves = settings.octaves.clamp(1, 5);
    let contrast = settings.contrast.max(1) as i32;
    let mut pixels = vec![0u8; SIZE * SIZE];

    for y in 0..SIZE as u32 {
        for x in 0..SIZE as u32 {
            let mut value = 0u32;
            let mut weight_sum = 0u32;
            let mut weight = 256u32;
            let mut period = feature_size;
            for octave in 0..octaves {
                value = value.saturating_add(
                    value_noise_2d(
                        settings.seed ^ u32::from(octave).wrapping_mul(0x9e37_79b9),
                        x,
                        y,
                        period,
                    )
                    .saturating_mul(weight),
                );
                weight_sum = weight_sum.saturating_add(weight);
                weight = (weight / 2).max(1);
                period = (period / 2).max(2);
            }
            let normalized = (value / weight_sum.max(1)) as i32;
            let contrasted = 128 + ((normalized - 128) * contrast / 128);
            let index = ((contrasted.clamp(0, 255) * 15 + 127) / 255) as u8;
            pixels[y as usize * SIZE + x as usize] = index;
        }
    }
    pixels
}

/// Collapse an `Average` primary pass followed by an `AddQuarter` secondary
/// pass into one 4bpp texture for an `Average` draw.
///
/// The PS1 blend equation is preserved for pixels covered by both layers:
/// `background / 2 + primary / 2 + secondary / 4` becomes
/// `background / 2 + (primary + secondary / 2) / 2`. The authored tints are
/// baked into the fused palette, so the runtime material must use neutral
/// `[128, 128, 128]` modulation afterwards.
pub fn fuse_average_add_quarter_psxt(
    primary_bytes: &[u8],
    primary_tint: [u8; 3],
    secondary_bytes: &[u8],
    secondary_tint: [u8; 3],
) -> Result<Vec<u8>, String> {
    let primary = psx_asset::Texture::from_bytes(primary_bytes)
        .map_err(|error| format!("primary texture is not a valid PSXT: {error:?}"))?;
    let secondary = psx_asset::Texture::from_bytes(secondary_bytes)
        .map_err(|error| format!("secondary texture is not a valid PSXT: {error:?}"))?;
    validate_fusion_texture("primary", primary)?;
    validate_fusion_texture("secondary", secondary)?;

    let width = primary.width();
    let height = primary.height();
    let mut rgba = Vec::with_capacity(usize::from(width) * usize::from(height));
    for y in 0..height {
        for x in 0..width {
            let primary_index = texture_4bpp_index(primary, x, y);
            let secondary_index =
                texture_4bpp_index(secondary, x % secondary.width(), y % secondary.height());
            let primary_visible = !(primary.index_zero_transparent() && primary_index == 0);
            let secondary_visible = !(secondary.index_zero_transparent() && secondary_index == 0);
            if !primary_visible && !secondary_visible {
                rgba.push([0, 0, 0, 0]);
                continue;
            }

            let primary_rgb = if primary_visible {
                tinted_clut_rgb(primary, primary_index, primary_tint)
            } else {
                [0; 3]
            };
            let secondary_rgb = if secondary_visible {
                tinted_clut_rgb(secondary, secondary_index, secondary_tint)
            } else {
                [0; 3]
            };
            rgba.push([
                primary_rgb[0].saturating_add(secondary_rgb[0] / 2),
                primary_rgb[1].saturating_add(secondary_rgb[1] / 2),
                primary_rgb[2].saturating_add(secondary_rgb[2] / 2),
                255,
            ]);
        }
    }

    let (palette, indices) = psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
        .map_err(|error| format!("could not quantise fused material: {error}"))?;
    psxed_tex::encode_indexed_psxt(
        width,
        height,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        true,
    )
    .map_err(|error| format!("could not encode fused material: {error}"))
}

/// Modulation value the runtime should use after
/// [`fuse_average_add_quarter_psxt`] bakes both layer tints.
pub const fn fused_material_neutral_tint() -> [u8; 3] {
    [MATERIAL_NEUTRAL_TINT; 3]
}

fn validate_fusion_texture(label: &str, texture: psx_asset::Texture<'_>) -> Result<(), String> {
    if texture.depth() != psxed_format::texture::Depth::Bit4 || texture.clut_entries() != 16 {
        return Err(format!("{label} texture must be a single-CLUT 4bpp PSXT"));
    }
    if texture.width() == 0 || texture.height() == 0 {
        return Err(format!("{label} texture has zero dimensions"));
    }
    Ok(())
}

fn texture_4bpp_index(texture: psx_asset::Texture<'_>, x: u16, y: u16) -> u8 {
    let halfword = usize::from(y) * usize::from(texture.halfwords_per_row()) + usize::from(x / 4);
    let offset = halfword * 2;
    let packed = u16::from_le_bytes([
        texture.pixel_bytes()[offset],
        texture.pixel_bytes()[offset + 1],
    ]);
    ((packed >> ((x & 3) * 4)) & 0x0f) as u8
}

fn tinted_clut_rgb(texture: psx_asset::Texture<'_>, index: u8, tint: [u8; 3]) -> [u8; 3] {
    let offset = usize::from(index) * 2;
    let raw = u16::from_le_bytes([
        texture.clut_bytes()[offset],
        texture.clut_bytes()[offset + 1],
    ]);
    let rgb = [
        expand_5bit((raw & 0x1f) as u8),
        expand_5bit(((raw >> 5) & 0x1f) as u8),
        expand_5bit(((raw >> 10) & 0x1f) as u8),
    ];
    [
        modulate(rgb[0], tint[0]),
        modulate(rgb[1], tint[1]),
        modulate(rgb[2], tint[2]),
    ]
}

const fn expand_5bit(value: u8) -> u8 {
    (value << 3) | (value >> 2)
}

fn modulate(value: u8, tint: u8) -> u8 {
    let product = value as u16 * tint as u16;
    ((product + 64) / 128).min(255) as u8
}

fn value_noise_2d(seed: u32, x: u32, y: u32, period: u32) -> u32 {
    let x0 = x / period;
    let y0 = y / period;
    let fx = (x % period) * 256 / period;
    let fy = (y % period) * 256 / period;
    let x1 = x0.wrapping_add(1);
    let y1 = y0.wrapping_add(1);
    let sx = smooth_q8(fx);
    let sy = smooth_q8(fy);
    let top = lerp_q8(hash_noise(seed, x0, y0), hash_noise(seed, x1, y0), sx);
    let bottom = lerp_q8(hash_noise(seed, x0, y1), hash_noise(seed, x1, y1), sx);
    lerp_q8(top, bottom, sy)
}

fn smooth_q8(value: u32) -> u32 {
    // 3t² - 2t³, in Q8.
    let square = value.saturating_mul(value) >> 8;
    square.saturating_mul(768u32.saturating_sub(value.saturating_mul(2))) >> 8
}

fn lerp_q8(a: u32, b: u32, t: u32) -> u32 {
    if b >= a {
        a + ((b - a) * t >> 8)
    } else {
        a - ((a - b) * t >> 8)
    }
}

fn hash_noise(seed: u32, x: u32, y: u32) -> u32 {
    let mut value = seed ^ x.wrapping_mul(0x85eb_ca6b) ^ y.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value & 0xff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_noise_is_deterministic_and_uses_full_4bpp_range() {
        let settings = ProceduralNoiseTexture::default();
        let first = generate_model_noise_indices(settings);
        let second = generate_model_noise_indices(settings);
        assert_eq!(first, second);
        assert!(first.iter().any(|&index| index == 0));
        assert!(first.iter().any(|&index| index >= 14));
        assert!(first.iter().all(|&index| index < 16));
    }

    #[test]
    fn seed_changes_generated_texture() {
        let first = generate_model_noise_indices(ProceduralNoiseTexture::default());
        let second = generate_model_noise_indices(ProceduralNoiseTexture {
            seed: 2,
            ..ProceduralNoiseTexture::default()
        });
        assert_ne!(first, second);
    }

    #[test]
    fn generated_noise_psxt_is_4bpp_clut16() {
        let bytes = generate_model_noise_psxt(ProceduralNoiseTexture::default());
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("generated PSXT parses");
        assert_eq!(texture.width(), MODEL_NOISE_TEXTURE_SIZE);
        assert_eq!(texture.height(), MODEL_NOISE_TEXTURE_SIZE);
        assert_eq!(texture.clut_entries(), 16);
        assert!(texture.index_zero_transparent());
    }

    #[test]
    fn compatible_layers_fuse_to_repeating_4bpp_texture() {
        let primary = psxed_tex::encode_indexed_psxt(
            4,
            2,
            psxed_tex::PsxtDepth::Bit4,
            &[0, 1, 1, 1, 1, 1, 1, 1],
            &[[0, 0, 0], [128, 64, 32]],
            true,
        )
        .unwrap();
        let secondary = psxed_tex::encode_indexed_psxt(
            2,
            1,
            psxed_tex::PsxtDepth::Bit4,
            &[0, 1],
            &[[0, 0, 0], [64, 128, 192]],
            true,
        )
        .unwrap();

        let fused =
            fuse_average_add_quarter_psxt(&primary, [128, 128, 128], &secondary, [128, 128, 128])
                .unwrap();
        let texture = psx_asset::Texture::from_bytes(&fused).unwrap();
        assert_eq!((texture.width(), texture.height()), (4, 2));
        assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit4);
        assert_eq!(texture.clut_entries(), 16);
        assert!(texture.index_zero_transparent());
        assert_eq!(texture_4bpp_index(texture, 0, 0), 0);
        assert_ne!(texture_4bpp_index(texture, 1, 0), 0);
        assert_ne!(texture_4bpp_index(texture, 2, 0), 0);
    }

    #[test]
    fn fusion_rejects_non_4bpp_input() {
        let indexed_8bpp = psxed_tex::encode_indexed_psxt(
            2,
            1,
            psxed_tex::PsxtDepth::Bit8,
            &[1, 1],
            &[[0, 0, 0], [255, 255, 255]],
            true,
        )
        .unwrap();
        let secondary = generate_model_noise_psxt(ProceduralNoiseTexture::default());
        assert!(
            fuse_average_add_quarter_psxt(&indexed_8bpp, [128; 3], &secondary, [128; 3]).is_err()
        );
    }
}
