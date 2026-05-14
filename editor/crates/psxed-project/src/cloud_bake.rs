//! Procedural cloud-texture bake used by the cook flow.
//!
//! Generates a `64 × 64` tileable Perlin-style value-noise field
//! deterministic in `seed`, builds a 16-entry CLUT that ramps from
//! `horizon_rgb` to `cloud_rgb` weighted by `density`, quantizes
//! each noise sample to a CLUT index, and emits a 4bpp PSXT blob
//! ready to drop into the cooked playtest package as a Texture
//! asset.
//!
//! The renderer side (runtime + editor preview) treats this texture
//! like any other 64×64 4bpp room material; the only thing
//! "different" about it is that it never had a `.psxt` source file
//! on disk — it's generated per room at cook time.
//!
//! Tileability is what makes the cloud plane drift seamlessly: the
//! gradient lattice samples its values modulo `LATTICE_SIZE`, so
//! `noise(0, y) == noise(LATTICE_SIZE, y)` exactly.

use psxed_tex::{encode_indexed_psxt, Error as TexError, PsxtDepth as Depth};

/// Side length of the baked cloud texture in pixels. Chosen to be a
/// PSX-friendly power of two so the rasterizer's texture-window
/// hardware can tile it without an extra UV-clamp pass.
pub const CLOUD_TEXTURE_SIZE: u16 = 64;

/// Number of CLUT entries. Drives the gradient resolution between
/// horizon and cloud colour; 16 reads as smooth at PS1 distance
/// while staying inside the 4bpp budget.
pub const CLOUD_CLUT_ENTRIES: usize = 16;

/// Period of the underlying lattice. Two cells across the 64-pixel
/// texture gives reasonably "puffy" clouds without an obvious tile
/// repeat; raise for finer wisps, lower for big slow blobs.
const LATTICE_SIZE: u32 = 8;

/// Number of octaves layered into the noise. Each octave doubles
/// the lattice frequency and halves the amplitude.
const OCTAVES: u32 = 3;

/// Bake a cloud texture for one room.
///
/// The returned `Vec<u8>` is a PSXT blob (AssetHeader + TextureHeader
/// + 4bpp pixels + 16-entry CLUT). Callers add it to the playtest
/// package's asset list verbatim.
pub fn bake_cloud_texture_psxt(
    seed: u32,
    horizon_rgb: [u8; 3],
    cloud_rgb: [u8; 3],
    density: u8,
) -> Result<Vec<u8>, TexError> {
    let palette = build_cloud_clut(horizon_rgb, cloud_rgb, density);
    let mut indices = vec![0u8; (CLOUD_TEXTURE_SIZE as usize) * (CLOUD_TEXTURE_SIZE as usize)];
    let width = CLOUD_TEXTURE_SIZE as u32;
    let height = CLOUD_TEXTURE_SIZE as u32;
    let last = (CLOUD_CLUT_ENTRIES - 1) as f32;
    for y in 0..height {
        for x in 0..width {
            let n = tileable_value_noise(x, y, seed);
            let n = n.clamp(0.0, 1.0);
            // Slight contrast curve so dense regions read as solid
            // cloud and sparse regions as pure horizon, rather than
            // every pixel landing in the muddy middle of the ramp.
            let n = (n * n * (3.0 - 2.0 * n)).clamp(0.0, 1.0);
            let index = (n * last).round() as u32;
            indices[(y * width + x) as usize] = index.min(last as u32) as u8;
        }
    }
    encode_indexed_psxt(
        CLOUD_TEXTURE_SIZE,
        CLOUD_TEXTURE_SIZE,
        Depth::Bit4,
        &indices,
        &palette,
        // Index zero is the deepest horizon color — keep it opaque so
        // the cloud plane renders as a continuous sky band rather
        // than punching transparent holes where the noise dips low.
        false,
    )
}

/// Build a 16-entry palette ramping from `horizon_rgb` (index 0) to
/// `cloud_rgb` (index 15). `density` (0..=255) controls where in the
/// 0..1 ramp the cloud colour starts dominating: 0 = ramp stays
/// horizon-color almost everywhere, 255 = ramp jumps to cloud
/// almost immediately, 128 = linear blend.
fn build_cloud_clut(horizon_rgb: [u8; 3], cloud_rgb: [u8; 3], density: u8) -> Vec<[u8; 3]> {
    let pivot = (density as f32 / 255.0).clamp(0.0, 1.0);
    // Map "pivot" to a gamma so high-density skies bias the ramp
    // toward `cloud_rgb` earlier in the index range.
    let gamma = if pivot <= 0.5 {
        // 0 → 4 (heavily flat-horizon), 0.5 → 1 (linear).
        1.0 + (0.5 - pivot) * 6.0
    } else {
        // 0.5 → 1 (linear), 1 → 0.25 (heavily skewed toward cloud).
        1.0 - (pivot - 0.5) * 1.5
    };
    let last = (CLOUD_CLUT_ENTRIES - 1) as f32;
    (0..CLOUD_CLUT_ENTRIES)
        .map(|i| {
            let t = (i as f32 / last).clamp(0.0, 1.0).powf(gamma);
            lerp_rgb(horizon_rgb, cloud_rgb, t)
        })
        .collect()
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let lerp = |x: u8, y: u8| -> u8 {
        let lo = x as f32;
        let hi = y as f32;
        (lo + (hi - lo) * t).round().clamp(0.0, 255.0) as u8
    };
    [lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2])]
}

/// Tileable value noise sampled at (x, y) for a `CLOUD_TEXTURE_SIZE`
/// canvas. Returns roughly in `[0, 1]` for typical seeds; clamp at
/// the call site.
fn tileable_value_noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut amplitude = 1.0f32;
    let mut max_amplitude = 0.0f32;
    let mut total = 0.0f32;
    for octave in 0..OCTAVES {
        let lattice = LATTICE_SIZE << octave;
        total += value_noise_octave(
            x,
            y,
            lattice,
            seed.wrapping_add(octave.wrapping_mul(0x9e37_79b9)),
        ) * amplitude;
        max_amplitude += amplitude;
        amplitude *= 0.5;
    }
    if max_amplitude > 0.0 {
        total / max_amplitude
    } else {
        0.5
    }
}

/// One octave of tileable value noise. `lattice` is how many noise
/// cells span the texture; corner samples wrap modulo `lattice` so
/// the result is seamless at both x and y edges.
fn value_noise_octave(x: u32, y: u32, lattice: u32, seed: u32) -> f32 {
    let lattice = lattice.max(1);
    let scale = lattice as f32 / CLOUD_TEXTURE_SIZE as f32;
    let fx = x as f32 * scale;
    let fy = y as f32 * scale;
    let ix0 = (fx.floor() as i32).rem_euclid(lattice as i32) as u32;
    let iy0 = (fy.floor() as i32).rem_euclid(lattice as i32) as u32;
    let ix1 = (ix0 + 1) % lattice;
    let iy1 = (iy0 + 1) % lattice;
    let tx = fx - fx.floor();
    let ty = fy - fy.floor();
    let s = smoothstep(tx);
    let t = smoothstep(ty);
    let n00 = lattice_value(ix0, iy0, seed);
    let n10 = lattice_value(ix1, iy0, seed);
    let n01 = lattice_value(ix0, iy1, seed);
    let n11 = lattice_value(ix1, iy1, seed);
    let nx0 = n00 * (1.0 - s) + n10 * s;
    let nx1 = n01 * (1.0 - s) + n11 * s;
    nx0 * (1.0 - t) + nx1 * t
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Hash one lattice grid point to a deterministic value in `[0, 1]`.
fn lattice_value(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = seed
        .wrapping_add(x.wrapping_mul(0x8da6_b343))
        .wrapping_add(y.wrapping_mul(0xd8163_841));
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    (h as f32) / (u32::MAX as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_bake_is_deterministic_for_seed() {
        let a = bake_cloud_texture_psxt(0xdead_beef, [16, 18, 22], [220, 220, 232], 128).unwrap();
        let b = bake_cloud_texture_psxt(0xdead_beef, [16, 18, 22], [220, 220, 232], 128).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn cloud_bake_changes_with_seed() {
        let a = bake_cloud_texture_psxt(1, [16, 18, 22], [220, 220, 232], 128).unwrap();
        let b = bake_cloud_texture_psxt(2, [16, 18, 22], [220, 220, 232], 128).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn cloud_bake_has_psxt_header() {
        let bytes = bake_cloud_texture_psxt(0, [0, 0, 0], [255, 255, 255], 128).unwrap();
        assert_eq!(&bytes[..4], b"PSXT");
    }

    #[test]
    fn tileable_value_noise_wraps_horizontally() {
        for y in [0u32, 17, 33, 60] {
            let left = tileable_value_noise(0, y, 7);
            let right = tileable_value_noise(CLOUD_TEXTURE_SIZE as u32, y, 7);
            // Same lattice coord after modulo: identical sample.
            assert!((left - right).abs() < 1e-5, "row {y}: {left} vs {right}");
        }
    }

    #[test]
    fn cloud_clut_starts_at_horizon_and_ends_at_cloud() {
        let palette = build_cloud_clut([10, 20, 30], [200, 210, 220], 128);
        assert_eq!(palette[0], [10, 20, 30]);
        assert_eq!(palette[CLOUD_CLUT_ENTRIES - 1], [200, 210, 220]);
    }
}
