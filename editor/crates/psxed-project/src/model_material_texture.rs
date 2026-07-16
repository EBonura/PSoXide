//! Host-side generation for compact model material layer textures.

use crate::ProceduralNoiseTexture;

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
}
