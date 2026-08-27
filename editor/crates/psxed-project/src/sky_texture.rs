//! Authoring helper for the image-backed six-face cube-sky format.
//!
//! The runtime stores the six 256 x 256 faces side-by-side in one 4bpp PSXT
//! atlas. Each face owns a 16-colour CLUT row; this cooker deliberately writes
//! the same palette into every row so a shared edge always resolves to the
//! same palette index on both faces.

use image::RgbImage;
use psxed_format::texture::Depth;

const FACE_WIDTH: usize = psx_bsp::sky::CUBE_SKY_FACE_WIDTH as usize;
const FACE_HEIGHT: usize = psx_bsp::sky::CUBE_SKY_FACE_HEIGHT as usize;
const ATLAS_WIDTH: usize = psx_bsp::sky::CUBE_SKY_ATLAS_SIZE[0] as usize;
const ATLAS_HEIGHT: usize = psx_bsp::sky::CUBE_SKY_ATLAS_SIZE[1] as usize;
const FACE_COUNT: usize = 6;
const FACE_PALETTE_COLORS: usize = 16;

/// Convert a 2:1 equirectangular PNG/JPEG into the runtime cube-sky PSXT.
pub fn cook_equirectangular_cube_sky(source_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let source = image::load_from_memory(source_bytes)
        .map_err(|error| format!("decode equirectangular sky: {error}"))?
        .to_rgb8();
    if source.width() != source.height().saturating_mul(2) {
        return Err(format!(
            "cube-sky source must be an exact 2:1 equirectangular image, got {} x {}",
            source.width(),
            source.height()
        ));
    }

    let faces = (0..FACE_COUNT)
        .map(|face| sample_cube_face(&source, face))
        .collect::<Vec<_>>();
    let palette_source = faces
        .iter()
        .flat_map(|face| face.iter().copied())
        .collect::<Vec<_>>();
    let (mut palette, _) = psxed_tex::quantize_rgb(&palette_source, FACE_PALETTE_COLORS)
        .map_err(|error| format!("quantize cube-sky palette: {error}"))?;
    pad_palette(&mut palette, FACE_PALETTE_COLORS);

    let mut pixels = vec![0u8; ATLAS_WIDTH * ATLAS_HEIGHT];
    for (face, face_pixels) in faces.iter().enumerate() {
        for y in 0..FACE_HEIGHT {
            let row = y * ATLAS_WIDTH;
            for x in 0..FACE_WIDTH {
                pixels[row + face * FACE_WIDTH + x] =
                    nearest_palette(face_pixels[y * FACE_WIDTH + x], &palette);
            }
        }
    }
    reconcile_polar_face_edges(&mut pixels);

    let palette_rows = vec![palette; FACE_COUNT];
    psxed_tex::encode_indexed_psxt_with_clut_rows(
        ATLAS_WIDTH as u16,
        ATLAS_HEIGHT as u16,
        Depth::Bit4,
        &pixels,
        &palette_rows,
        false,
    )
    .map_err(|error| format!("encode cube-sky PSXT: {error}"))
}

fn sample_cube_face(source: &RgbImage, face: usize) -> Vec<[u8; 3]> {
    let mut pixels = Vec::with_capacity(FACE_WIDTH * FACE_HEIGHT);
    for y in 0..FACE_HEIGHT {
        let v = y as f32 * 2.0 / (FACE_HEIGHT - 1) as f32 - 1.0;
        for x in 0..FACE_WIDTH {
            let u = x as f32 * 2.0 / (FACE_WIDTH - 1) as f32 - 1.0;
            pixels.push(sample_equirectangular(source, cube_direction(face, u, v)));
        }
    }
    pixels
}

fn cube_direction(face: usize, u: f32, v: f32) -> [f32; 3] {
    match face {
        0 => [1.0, -v, -u],
        1 => [-1.0, -v, u],
        2 => [u, 1.0, v],
        3 => [u, -1.0, -v],
        4 => [u, -v, 1.0],
        5 => [-u, -v, -1.0],
        _ => unreachable!("cube face index"),
    }
}

fn sample_equirectangular(source: &RgbImage, direction: [f32; 3]) -> [u8; 3] {
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt()
            .max(f32::EPSILON);
    let longitude = direction[0].atan2(direction[2]);
    let latitude = (direction[1] / length).clamp(-1.0, 1.0).asin();
    let width = source.width() as f32;
    let height = source.height() as f32;
    let source_x = (longitude / std::f32::consts::TAU + 0.5) * width - 0.5;
    let source_y = (0.5 - latitude / std::f32::consts::PI) * height - 0.5;
    bilinear_wrapped(source, source_x, source_y)
}

fn bilinear_wrapped(source: &RgbImage, x: f32, y: f32) -> [u8; 3] {
    let width = source.width() as i32;
    let height = source.height() as i32;
    let x0 = x.floor() as i32;
    let y0 = (y.floor() as i32).clamp(0, height - 1);
    let x1 = x0 + 1;
    let y1 = (y0 + 1).clamp(0, height - 1);
    let fx = x - x.floor();
    let fy = y - y.floor();
    let wrap_x = |value: i32| value.rem_euclid(width) as u32;
    let sample = |sx: i32, sy: i32| source.get_pixel(wrap_x(sx), sy as u32).0;
    let p00 = sample(x0, y0);
    let p10 = sample(x1, y0);
    let p01 = sample(x0, y1);
    let p11 = sample(x1, y1);
    core::array::from_fn(|channel| {
        let top = p00[channel] as f32 * (1.0 - fx) + p10[channel] as f32 * fx;
        let bottom = p01[channel] as f32 * (1.0 - fx) + p11[channel] as f32 * fx;
        (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
    })
}

fn pad_palette(palette: &mut Vec<[u8; 3]>, length: usize) {
    let fill = palette.last().copied().unwrap_or([0, 0, 0]);
    palette.resize(length, fill);
}

fn nearest_palette(rgb: [u8; 3], palette: &[[u8; 3]]) -> u8 {
    let mut best_index = 0usize;
    let mut best_distance = u32::MAX;
    for (index, color) in palette.iter().enumerate() {
        let dr = i32::from(rgb[0]) - i32::from(color[0]);
        let dg = i32::from(rgb[1]) - i32::from(color[1]);
        let db = i32::from(rgb[2]) - i32::from(color[2]);
        let distance = (dr * dr + dg * dg + db * db) as u32;
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    }
    best_index as u8
}

fn atlas_index(face: usize, x: usize, y: usize) -> usize {
    y * ATLAS_WIDTH + face * FACE_WIDTH + x
}

fn reconcile_polar_face_edges(pixels: &mut [u8]) {
    for y in 0..FACE_HEIGHT {
        let side_x = (y * (FACE_WIDTH - 1) + (FACE_HEIGHT - 1) / 2) / (FACE_HEIGHT - 1);
        let reversed = FACE_WIDTH - 1 - side_x;
        for (to, from) in [
            ((2, 0, y), (1, side_x, 0)),
            ((2, FACE_WIDTH - 1, y), (0, reversed, 0)),
            ((3, 0, y), (1, reversed, FACE_HEIGHT - 1)),
            ((3, FACE_WIDTH - 1, y), (0, side_x, FACE_HEIGHT - 1)),
        ] {
            pixels[atlas_index(to.0, to.1, to.2)] = pixels[atlas_index(from.0, from.1, from.2)];
        }
    }
}
