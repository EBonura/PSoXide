// SPDX-License-Identifier: GPL-2.0-or-later
//! PNG/JPG → PSXT texture converter.
//!
//! Runs the standard PSX-era texture-cook pipeline:
//!
//! 1. **Decode** the source image (any format the `image` crate
//!    supports; we compile in PNG + JPG + BMP).
//! 2. **Crop** to an optional sub-rectangle of the source.
//! 3. **Resample** to the target texel dimensions. Uses Lanczos3
//!    for downscale -- the PSX-era "bilinear" look but with less
//!    ringing.
//! 4. **Quantise** to `N` colours via median-cut, producing a
//!    palette + per-pixel index table.
//! 5. **Pack** the indices into the nibble-order the PSX GPU reads
//!    (4 texels per halfword for 4bpp, 2 for 8bpp).
//! 6. **Emit** the PSXT binary blob matching
//!    [`psxed_format::texture`].
//!
//! # Usage
//!
//! ```ignore
//! use psxed_tex::{Config, CropMode, Depth, Resampler, convert};
//!
//! let src = std::fs::read("brick-wall.jpg").unwrap();
//! let cfg = Config {
//!     width: 64,
//!     height: 64,
//!     depth: Depth::Bit4,
//!     crop: CropMode::CentreSquare,
//!     resampler: Resampler::Lanczos3,
//!     transparent_index_zero: false,
//!     clut_rows: 1,
//! };
//! let psxt = convert(&src, &cfg).unwrap();
//! std::fs::write("brick-wall.psxt", psxt).unwrap();
//! ```

use image::{imageops, DynamicImage, GenericImageView};
use psxed_format::texture::{Depth, TextureHeader, MAGIC, VERSION};
use psxed_format::AssetHeader;

pub use psxed_format::texture::Depth as PsxtDepth;

const TRANSPARENT_ALPHA_THRESHOLD: u8 = 128;

/// Rectangle into the source image, pre-resize.
#[derive(Copy, Clone, Debug)]
pub struct CropRect {
    /// X offset from the left of the source, in source pixels.
    pub x: u32,
    /// Y offset from the top.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

impl CropRect {
    /// Compute the centred square crop of a `src_w × src_h` image
    /// -- the largest square that fits, positioned in the middle.
    /// Used as the default when the caller hasn't specified an
    /// explicit crop, so arbitrary-aspect sources don't get
    /// distorted by the resize step.
    pub const fn centred_square(src_w: u32, src_h: u32) -> Self {
        let size = if src_w < src_h { src_w } else { src_h };
        Self {
            x: (src_w - size) / 2,
            y: (src_h - size) / 2,
            w: size,
            h: size,
        }
    }
}

/// Resampling kernel used for the resize step.
#[derive(Copy, Clone, Debug)]
pub enum Resampler {
    /// Nearest-neighbour. Blocky, fast. Good for already-pixel-art
    /// sources or when you want a crisp aliased look.
    Nearest,
    /// Lanczos3. Smooth, handles downscale well. Default.
    Lanczos3,
    /// Triangle filter (bilinear). Middle ground.
    Triangle,
}

/// Configuration for one texture conversion.
#[derive(Clone, Debug)]
pub struct Config {
    /// Target texture width in texels. Should be a power of two
    /// for tileable textures; the cooker doesn't enforce this so
    /// UI sprites can be any size.
    pub width: u16,
    /// Target height in texels.
    pub height: u16,
    /// PSX colour depth. 4bpp is the default; 8bpp for detail;
    /// 15bpp for true-colour reference images.
    pub depth: Depth,
    /// Source-side crop behaviour. Default
    /// [`CropMode::CentreSquare`] centre-crops to a square so
    /// arbitrary-aspect sources don't get distorted by the resize
    /// step; [`CropMode::Explicit`] locks to a specific rect;
    /// [`CropMode::None`] stretches the full source to the target
    /// aspect ratio (rarely what you want, but useful for
    /// previously-cropped sources and UI sprites).
    pub crop: CropMode,
    /// Resampling kernel. `Lanczos3` is a good default.
    pub resampler: Resampler,
    /// Reserve indexed palette entry 0 for alpha-transparent pixels.
    ///
    /// PS1 textured polygons treat sampled texel value 0 as
    /// transparent. Model atlases use this to keep unused UV gutters
    /// from drawing black, while regular room textures leave it off so
    /// palette index 0 remains an ordinary opaque colour.
    pub transparent_index_zero: bool,
    /// Number of identical 16-entry CLUT rows written for a 4bpp texture.
    /// Banked models may address rows 1..3 even when every bank uses the same
    /// generated image. Other texture depths require one row.
    pub clut_rows: u8,
}

/// How the cooker should handle source aspect vs target aspect.
#[derive(Copy, Clone, Debug, Default)]
pub enum CropMode {
    /// Centre-crop to the largest square that fits -- the default.
    /// Guarantees no aspect distortion for arbitrary-aspect sources.
    #[default]
    CentreSquare,
    /// Use the caller-specified rect.
    Explicit(CropRect),
    /// No crop -- resize-stretch the full source. Produces distorted
    /// output for non-square sources.
    None,
}

/// Errors the cooker can surface.
#[derive(Debug)]
pub enum Error {
    /// Source image couldn't be decoded.
    Decode(image::ImageError),
    /// Crop rectangle extends past source bounds.
    CropOutOfBounds {
        /// The offending crop.
        crop: CropRect,
        /// Source image dimensions.
        source: (u32, u32),
    },
    /// Target dimensions are zero.
    ZeroSize,
    /// Indexed-bake input length doesn't match `width * height`, or
    /// the requested depth has no CLUT (e.g. Bit15).
    InvalidIndexedInput,
    /// Input bytes are not a valid cooked PSXT texture.
    InvalidPsxt,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Decode(e) => write!(f, "decode source image: {e}"),
            Error::CropOutOfBounds { crop, source } => write!(
                f,
                "crop rect {:?}+{}×{} extends past {}×{} source",
                (crop.x, crop.y),
                crop.w,
                crop.h,
                source.0,
                source.1,
            ),
            Error::ZeroSize => write!(f, "target width/height is zero"),
            Error::InvalidIndexedInput => {
                write!(f, "indexed bake: index buffer or depth is invalid")
            }
            Error::InvalidPsxt => write!(f, "invalid cooked PSXT texture"),
        }
    }
}

impl std::error::Error for Error {}

/// Encode an indexed-color image (4bpp or 8bpp) into a PSXT blob
/// without going through PNG / median-cut quantization. Lets callers
/// that already know their palette (procedural noise bakes, palette
/// transcoders, etc.) skip the lossy image-to-indexed step.
///
/// `indices` must contain `width * height` palette indices, each
/// `< palette.len()`. `palette` must hold at most `depth.clut_entries()`
/// entries; missing entries are padded as black (transparent).
pub fn encode_indexed_psxt(
    width: u16,
    height: u16,
    depth: Depth,
    indices: &[u8],
    palette: &[[u8; 3]],
    transparent_index_zero: bool,
) -> Result<Vec<u8>, Error> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    let Some(n_entries) = depth.clut_entries() else {
        // Bit15 has no CLUT - caller should use `convert` for direct
        // 15bpp instead.
        return Err(Error::InvalidIndexedInput);
    };
    let n_entries = n_entries as usize;
    let pixel_count = (width as usize) * (height as usize);
    if indices.len() != pixel_count {
        return Err(Error::InvalidIndexedInput);
    }
    let mut padded_palette: Vec<[u8; 3]> = Vec::with_capacity(n_entries);
    padded_palette.extend(palette.iter().take(n_entries).copied());
    while padded_palette.len() < n_entries {
        padded_palette.push([0, 0, 0]);
    }
    let pixel_halfwords = pack_indices(indices, width, height, depth);
    let clut_halfwords = encode_clut(&padded_palette, n_entries);
    let flags = if transparent_index_zero {
        psxed_format::texture::flags::INDEX_ZERO_TRANSPARENT
    } else {
        0
    };
    Ok(assemble_blob(
        width,
        height,
        depth,
        flags,
        n_entries as u16,
        &pixel_halfwords,
        &clut_halfwords,
    ))
}

/// Encode an indexed-color image with multiple CLUT rows.
///
/// This is useful for PS1-style skies and backdrops where each
/// horizontal band can use its own 16-colour 4bpp palette while the
/// pixel data stays one continuous texture. `palette_rows` are
/// concatenated in order in the CLUT block; runtime code chooses the
/// row-specific CLUT handle per primitive.
pub fn encode_indexed_psxt_with_clut_rows(
    width: u16,
    height: u16,
    depth: Depth,
    indices: &[u8],
    palette_rows: &[Vec<[u8; 3]>],
    transparent_index_zero: bool,
) -> Result<Vec<u8>, Error> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    let Some(entries_per_row) = depth.clut_entries() else {
        return Err(Error::InvalidIndexedInput);
    };
    if palette_rows.is_empty() {
        return Err(Error::InvalidIndexedInput);
    }
    let entries_per_row = entries_per_row as usize;
    let pixel_count = (width as usize) * (height as usize);
    if indices.len() != pixel_count {
        return Err(Error::InvalidIndexedInput);
    }
    let mut clut_halfwords = Vec::with_capacity(entries_per_row * palette_rows.len());
    for palette in palette_rows {
        let mut padded_palette: Vec<[u8; 3]> = Vec::with_capacity(entries_per_row);
        padded_palette.extend(palette.iter().take(entries_per_row).copied());
        while padded_palette.len() < entries_per_row {
            padded_palette.push([0, 0, 0]);
        }
        clut_halfwords.extend(encode_clut(&padded_palette, entries_per_row));
    }
    let clut_entries = entries_per_row
        .checked_mul(palette_rows.len())
        .and_then(|entries| u16::try_from(entries).ok())
        .ok_or(Error::InvalidIndexedInput)?;
    let pixel_halfwords = pack_indices(indices, width, height, depth);
    let flags = if transparent_index_zero {
        psxed_format::texture::flags::INDEX_ZERO_TRANSPARENT
    } else {
        0
    };
    Ok(assemble_blob(
        width,
        height,
        depth,
        flags,
        clut_entries,
        &pixel_halfwords,
        &clut_halfwords,
    ))
}

/// End-to-end convert: bytes of a PNG/JPG/BMP → bytes of a PSXT
/// blob. The returned `Vec<u8>` is ready to write to disk and
/// `include_bytes!` into a game.
pub fn convert(src: &[u8], cfg: &Config) -> Result<Vec<u8>, Error> {
    if cfg.width == 0 || cfg.height == 0 {
        return Err(Error::ZeroSize);
    }
    let img = image::load_from_memory(src).map_err(Error::Decode)?;
    let (sw, sh) = img.dimensions();
    let crop_rect = match cfg.crop {
        CropMode::Explicit(c) => {
            if c.x + c.w > sw || c.y + c.h > sh {
                return Err(Error::CropOutOfBounds {
                    crop: c,
                    source: (sw, sh),
                });
            }
            Some(c)
        }
        CropMode::CentreSquare => Some(CropRect::centred_square(sw, sh)),
        CropMode::None => None,
    };
    let cropped = if let Some(c) = crop_rect {
        DynamicImage::ImageRgba8(imageops::crop_imm(&img, c.x, c.y, c.w, c.h).to_image())
    } else {
        img
    };
    let resized = resize(&cropped, cfg.width as u32, cfg.height as u32, cfg.resampler);

    encode_psxt(
        &resized,
        cfg.width,
        cfg.height,
        cfg.depth,
        cfg.transparent_index_zero,
        cfg.clut_rows,
    )
}

/// Resize `img` to `w × h` using the chosen resampling kernel.
fn resize(img: &DynamicImage, w: u32, h: u32, r: Resampler) -> image::RgbaImage {
    let filter = match r {
        Resampler::Nearest => imageops::FilterType::Nearest,
        Resampler::Lanczos3 => imageops::FilterType::Lanczos3,
        Resampler::Triangle => imageops::FilterType::Triangle,
    };
    img.resize_exact(w, h, filter).to_rgba8()
}

/// Heart of the pipeline -- take a resized RgbaImage and emit the
/// full PSXT blob (12-byte AssetHeader + 16-byte TextureHeader +
/// pixel block + CLUT block).
fn encode_psxt(
    img: &image::RgbaImage,
    width: u16,
    height: u16,
    depth: Depth,
    transparent_index_zero: bool,
    clut_rows: u8,
) -> Result<Vec<u8>, Error> {
    if clut_rows == 0 || clut_rows > 4 || (depth != Depth::Bit4 && clut_rows != 1) {
        return Err(Error::InvalidIndexedInput);
    }
    let pixels: Vec<[u8; 4]> = img.pixels().map(|p| [p[0], p[1], p[2], p[3]]).collect();

    match depth {
        Depth::Bit4 | Depth::Bit8 => {
            let n_entries = depth.clut_entries().unwrap() as usize;
            let (palette, indices) = if transparent_index_zero {
                median_cut_quantize_with_transparent_zero(&pixels, n_entries)
            } else {
                let rgb_pixels: Vec<[u8; 3]> = pixels.iter().map(|p| [p[0], p[1], p[2]]).collect();
                median_cut_quantize(&rgb_pixels, n_entries)
            };
            let pixel_halfwords = pack_indices(&indices, width, height, depth);
            let base_clut = encode_clut(&palette, n_entries);
            let mut clut_halfwords = Vec::with_capacity(base_clut.len() * usize::from(clut_rows));
            for _ in 0..clut_rows {
                clut_halfwords.extend_from_slice(&base_clut);
            }
            let flags = if transparent_index_zero {
                psxed_format::texture::flags::INDEX_ZERO_TRANSPARENT
            } else {
                0
            };
            Ok(assemble_blob(
                width,
                height,
                depth,
                flags,
                (n_entries * usize::from(clut_rows)) as u16,
                &pixel_halfwords,
                &clut_halfwords,
            ))
        }
        Depth::Bit15 => {
            // Direct colour -- each pixel becomes one Color555 halfword.
            let pixel_halfwords: Vec<u16> = pixels
                .iter()
                .map(|rgb| rgb_to_555(rgb[0], rgb[1], rgb[2]))
                .collect();
            Ok(assemble_blob(
                width,
                height,
                depth,
                0,
                0,
                &pixel_halfwords,
                &[],
            ))
        }
    }
}

fn median_cut_quantize_with_transparent_zero(
    pixels: &[[u8; 4]],
    n_entries: usize,
) -> (Vec<[u8; 3]>, Vec<u8>) {
    assert!(
        (2..=256).contains(&n_entries),
        "transparent indexed textures need at least two palette entries"
    );

    let opaque_pixels: Vec<[u8; 3]> = pixels
        .iter()
        .filter(|p| p[3] >= TRANSPARENT_ALPHA_THRESHOLD)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    if opaque_pixels.is_empty() {
        return (vec![[0, 0, 0]; n_entries], vec![0; pixels.len()]);
    }

    let (opaque_palette, _) = median_cut_quantize(&opaque_pixels, n_entries - 1);
    let mut palette = Vec::with_capacity(n_entries);
    palette.push([0, 0, 0]);
    palette.extend_from_slice(&opaque_palette);
    palette.truncate(n_entries);
    while palette.len() < n_entries {
        palette.push([0, 0, 0]);
    }

    let indices = pixels
        .iter()
        .map(|p| {
            if p[3] < TRANSPARENT_ALPHA_THRESHOLD {
                0
            } else {
                nearest_index(&[p[0], p[1], p[2]], &palette[1..]).saturating_add(1)
            }
        })
        .collect();

    (palette, indices)
}

/// Quantise RGBA pixels into an indexed palette while reserving index zero
/// for transparent pixels.
///
/// This is the in-memory counterpart of the indexed image conversion path.
/// Procedural/cook-time texture combiners can use it without encoding an
/// intermediate PNG merely to reuse the deterministic median-cut quantiser.
pub fn quantize_rgba_with_transparent_zero(
    pixels: &[[u8; 4]],
    n_entries: usize,
) -> Result<(Vec<[u8; 3]>, Vec<u8>), Error> {
    if !(2..=256).contains(&n_entries) {
        return Err(Error::InvalidIndexedInput);
    }
    Ok(median_cut_quantize_with_transparent_zero(pixels, n_entries))
}

/// Quantise opaque RGB pixels into an indexed palette without reserving a
/// transparency entry. Host-side material bakers use this when every selected
/// source texel is opaque so all 16 colours remain available to a 4bpp result.
pub fn quantize_rgb(
    pixels: &[[u8; 3]],
    n_entries: usize,
) -> Result<(Vec<[u8; 3]>, Vec<u8>), Error> {
    if !(2..=256).contains(&n_entries) || pixels.is_empty() {
        return Err(Error::InvalidIndexedInput);
    }
    Ok(median_cut_quantize(pixels, n_entries))
}

/// Requantise an existing cooked texture to 4bpp while preserving its texel
/// dimensions and index-zero transparency contract.
///
/// This is intended for model atlases whose original source image is no longer
/// available independently from the model source. Indexed and direct-colour
/// inputs are decoded to their exact PS1 RGB555 colours before the deterministic
/// median-cut quantiser produces a 16-colour atlas. Existing 4bpp input is
/// returned byte-for-byte unchanged.
pub fn requantize_psxt_to_4bpp(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let texture = psx_asset::Texture::from_bytes(bytes).map_err(|_| Error::InvalidPsxt)?;
    if texture.depth() == Depth::Bit4 {
        return Ok(bytes.to_vec());
    }

    let width = texture.width();
    let height = texture.height();
    let pixel_count = usize::from(width) * usize::from(height);
    let transparent_zero = texture.index_zero_transparent();
    let mut rgba = Vec::with_capacity(pixel_count);

    match texture.depth() {
        Depth::Bit4 => unreachable!("4bpp input returned above"),
        Depth::Bit8 => {
            let palette = decode_clut_rgb(texture.clut_bytes())?;
            if palette.len() < usize::from(texture.clut_entries()) {
                return Err(Error::InvalidPsxt);
            }
            let halfwords_per_row = usize::from(texture.halfwords_per_row());
            let packed = texture.pixel_bytes();
            for y in 0..usize::from(height) {
                for x in 0..usize::from(width) {
                    let byte_offset = (y * halfwords_per_row + x / 2) * 2 + (x & 1);
                    let index = *packed.get(byte_offset).ok_or(Error::InvalidPsxt)? as usize;
                    let rgb = *palette.get(index).ok_or(Error::InvalidPsxt)?;
                    let alpha = if transparent_zero && index == 0 {
                        0
                    } else {
                        255
                    };
                    rgba.push([rgb[0], rgb[1], rgb[2], alpha]);
                }
            }
        }
        Depth::Bit15 => {
            let packed = texture.pixel_bytes();
            for index in 0..pixel_count {
                let offset = index * 2;
                let word = u16::from_le_bytes([
                    *packed.get(offset).ok_or(Error::InvalidPsxt)?,
                    *packed.get(offset + 1).ok_or(Error::InvalidPsxt)?,
                ]);
                let rgb = rgb555_to_rgb8(word);
                rgba.push([rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }

    let (palette, indices) = if transparent_zero {
        quantize_rgba_with_transparent_zero(&rgba, 16)?
    } else {
        let rgb = rgba
            .iter()
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect::<Vec<_>>();
        quantize_rgb(&rgb, 16)?
    };
    encode_indexed_psxt(
        width,
        height,
        Depth::Bit4,
        &indices,
        &palette,
        transparent_zero,
    )
}

fn decode_clut_rgb(bytes: &[u8]) -> Result<Vec<[u8; 3]>, Error> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::InvalidPsxt);
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| rgb555_to_rgb8(u16::from_le_bytes([pair[0], pair[1]])))
        .collect())
}

fn rgb555_to_rgb8(word: u16) -> [u8; 3] {
    let expand = |value: u16| -> u8 {
        let value = value as u8;
        (value << 3) | (value >> 2)
    };
    [
        expand(word & 0x1f),
        expand((word >> 5) & 0x1f),
        expand((word >> 10) & 0x1f),
    ]
}

/// Median-cut colour quantisation. Input: per-pixel RGB. Output:
/// (palette, indices) where each index is `< n_entries`.
///
/// Algorithm: start with one box holding every unique colour.
/// Repeatedly split the box with the largest RGB range along its
/// longest channel's median until we have `n_entries` boxes. Each
/// box contributes its per-channel mean as the final palette entry.
///
/// Output is deterministic for a given input -- `BTreeMap` + stable
/// sorts -- so repeated builds produce byte-identical PSXT blobs.
fn median_cut_quantize(pixels: &[[u8; 3]], n_entries: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    assert!(
        (2..=256).contains(&n_entries),
        "n_entries must be in [2, 256]"
    );

    // Start with one box containing all pixels (as owned Vec so we
    // can sort in place per split).
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![pixels.to_vec()];

    while boxes.len() < n_entries {
        // Find the box with the largest range along any channel.
        let (idx, axis) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() >= 2)
            .map(|(i, b)| {
                let (axis, range) = widest_axis(b);
                (i, axis, range)
            })
            .max_by_key(|&(_, _, r)| r)
            .map(|(i, a, _)| (i, a))
            .unwrap_or_else(|| {
                // Can't split any further (every box has <2 colours).
                // Pad with duplicates of existing entries to hit
                // n_entries -- quantiser caller expects exactly that count.
                (0, 0)
            });

        let b = &mut boxes[idx];
        if b.len() < 2 {
            // Pad by duplicating. We only hit this when the input
            // has fewer than n_entries unique colours.
            let dup = b.clone();
            boxes.push(dup);
            continue;
        }

        // Sort along the chosen axis, split at the median.
        b.sort_by_key(|c| c[axis]);
        let mid = b.len() / 2;
        let hi = b.split_off(mid);
        boxes.push(hi);
    }

    // Compute each box's mean → palette entry.
    let palette: Vec<[u8; 3]> = boxes
        .iter()
        .map(|b| {
            let (mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32);
            for c in b {
                sr += c[0] as u32;
                sg += c[1] as u32;
                sb += c[2] as u32;
            }
            let n = b.len().max(1) as u32;
            [(sr / n) as u8, (sg / n) as u8, (sb / n) as u8]
        })
        .collect();

    // Map each source pixel to nearest palette entry.
    let indices: Vec<u8> = pixels.iter().map(|p| nearest_index(p, &palette)).collect();

    (palette, indices)
}

/// Return the (axis_index, range) of the colour channel with the
/// widest spread in the given box.
fn widest_axis(b: &[[u8; 3]]) -> (usize, u32) {
    let mut lo = [u8::MAX; 3];
    let mut hi = [0u8; 3];
    for c in b {
        for ch in 0..3 {
            lo[ch] = lo[ch].min(c[ch]);
            hi[ch] = hi[ch].max(c[ch]);
        }
    }
    let ranges = [
        hi[0] as u32 - lo[0] as u32,
        hi[1] as u32 - lo[1] as u32,
        hi[2] as u32 - lo[2] as u32,
    ];
    let (i, &r) = ranges.iter().enumerate().max_by_key(|(_, r)| *r).unwrap();
    (i, r)
}

/// Linear scan of palette, squared-distance metric. `256 * 64 ≈
/// 16K ops per image -- fast enough that we don't need a kd-tree
/// for texture sizes the PSX can actually use.
fn nearest_index(p: &[u8; 3], palette: &[[u8; 3]]) -> u8 {
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    for (i, c) in palette.iter().enumerate() {
        let dr = p[0] as i32 - c[0] as i32;
        let dg = p[1] as i32 - c[1] as i32;
        let db = p[2] as i32 - c[2] as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// Pack a linear `indices` buffer into halfword-aligned PSX
/// texture memory. Each output u16 holds N texels where N matches
/// the depth's `texels_per_halfword`. Row ends pad up to a full
/// halfword so every row starts on a halfword boundary.
fn pack_indices(indices: &[u8], width: u16, height: u16, depth: Depth) -> Vec<u16> {
    let hw_per_row = TextureHeader::halfwords_per_row(depth, width);
    let total = (hw_per_row as usize) * (height as usize);
    let mut out = vec![0u16; total];

    match depth {
        Depth::Bit4 => {
            for y in 0..height {
                for x in 0..width {
                    let idx = indices[(y as usize) * (width as usize) + (x as usize)] & 0x0F;
                    let hw = (y as usize) * (hw_per_row as usize) + (x as usize) / 4;
                    let shift = (x & 3) * 4;
                    out[hw] |= (idx as u16) << shift;
                }
            }
        }
        Depth::Bit8 => {
            for y in 0..height {
                for x in 0..width {
                    let idx = indices[(y as usize) * (width as usize) + (x as usize)];
                    let hw = (y as usize) * (hw_per_row as usize) + (x as usize) / 2;
                    let shift = (x & 1) * 8;
                    out[hw] |= (idx as u16) << shift;
                }
            }
        }
        Depth::Bit15 => unreachable!("indexed packer called with 15bpp depth"),
    }
    out
}

/// Encode a palette as RGB555 halfwords. If `entries < n_entries`
/// (rare -- only on inputs with <16 unique colours), pad with zero
/// entries to hit the required CLUT row width.
fn encode_clut(palette: &[[u8; 3]], n_entries: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(n_entries);
    for c in palette {
        out.push(rgb_to_555(c[0], c[1], c[2]));
    }
    while out.len() < n_entries {
        out.push(0);
    }
    out
}

/// 8-bit per channel RGB → PSX 5-5-5 + mask bit. Low 5 bits = red,
/// next 5 = green, next 5 = blue, bit 15 = mask (kept zero here --
/// masks are a render-time concern, not asset-time).
fn rgb_to_555(r: u8, g: u8, b: u8) -> u16 {
    let r5 = (r as u16 >> 3) & 0x1F;
    let g5 = (g as u16 >> 3) & 0x1F;
    let b5 = (b as u16 >> 3) & 0x1F;
    r5 | (g5 << 5) | (b5 << 10)
}

/// Concatenate the header + pixel + CLUT blocks into the final
/// PSXT byte sequence.
fn assemble_blob(
    width: u16,
    height: u16,
    depth: Depth,
    flags: u16,
    clut_entries: u16,
    pixel_hw: &[u16],
    clut_hw: &[u16],
) -> Vec<u8> {
    let pixel_bytes = (pixel_hw.len() * 2) as u32;
    let clut_bytes = (clut_hw.len() * 2) as u32;
    let payload_len = (TextureHeader::SIZE as u32) + pixel_bytes + clut_bytes;

    let mut out = Vec::with_capacity(AssetHeader::SIZE + payload_len as usize);
    // AssetHeader.
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    // TextureHeader.
    out.push(depth as u8);
    out.push(0); // _pad
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&clut_entries.to_le_bytes());
    out.extend_from_slice(&pixel_bytes.to_le_bytes());
    out.extend_from_slice(&clut_bytes.to_le_bytes());
    // Pixel halfwords.
    for hw in pixel_hw {
        out.extend_from_slice(&hw.to_le_bytes());
    }
    // CLUT halfwords.
    for hw in clut_hw {
        out.extend_from_slice(&hw.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centred_square_landscape() {
        // 3:2 landscape source → square is the short side (height),
        // centred horizontally.
        let c = CropRect::centred_square(6000, 4000);
        assert_eq!(c.w, 4000);
        assert_eq!(c.h, 4000);
        assert_eq!(c.x, 1000);
        assert_eq!(c.y, 0);
    }

    #[test]
    fn centred_square_portrait() {
        // 3:4 portrait → square is the short side (width), centred
        // vertically.
        let c = CropRect::centred_square(3000, 4000);
        assert_eq!(c.w, 3000);
        assert_eq!(c.h, 3000);
        assert_eq!(c.x, 0);
        assert_eq!(c.y, 500);
    }

    #[test]
    fn centred_square_already_square_is_identity() {
        let c = CropRect::centred_square(512, 512);
        assert_eq!(c.w, 512);
        assert_eq!(c.h, 512);
        assert_eq!(c.x, 0);
        assert_eq!(c.y, 0);
    }

    #[test]
    fn rgb_conversion_full_scale() {
        assert_eq!(rgb_to_555(0, 0, 0), 0);
        assert_eq!(rgb_to_555(255, 255, 255), 0x7FFF);
        assert_eq!(rgb_to_555(255, 0, 0), 0x001F);
        assert_eq!(rgb_to_555(0, 255, 0), 0x03E0);
        assert_eq!(rgb_to_555(0, 0, 255), 0x7C00);
    }

    #[test]
    fn pack_4bpp_nibble_order() {
        // 4 texels packed into one halfword: nibble 0 = leftmost.
        let indices = vec![0x1, 0x2, 0x3, 0x4];
        let packed = pack_indices(&indices, 4, 1, Depth::Bit4);
        assert_eq!(packed, vec![0x4321]);
    }

    #[test]
    fn pack_8bpp_byte_order() {
        let indices = vec![0x12, 0x34];
        let packed = pack_indices(&indices, 2, 1, Depth::Bit8);
        assert_eq!(packed, vec![0x3412]);
    }

    #[test]
    fn quantiser_exactly_n_entries_when_enough_colours() {
        // 16 distinct greys → median-cut should produce ~16 entries.
        let pixels: Vec<[u8; 3]> = (0..16).map(|i| [i * 16, i * 16, i * 16]).collect();
        let (palette, indices) = median_cut_quantize(&pixels, 16);
        assert_eq!(palette.len(), 16);
        assert_eq!(indices.len(), 16);
        for &idx in &indices {
            assert!((idx as usize) < palette.len());
        }
    }

    #[test]
    fn end_to_end_encoded_blob_roundtrips_via_header_offsets() {
        // Small synthetic PNG -- solid red 4×4.
        let mut buf = Vec::new();
        {
            let img = image::RgbaImage::from_fn(4, 4, |_, _| image::Rgba([255, 0, 0, 255]));
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .unwrap();
        }
        let cfg = Config {
            width: 4,
            height: 4,
            depth: Depth::Bit4,
            crop: CropMode::None,
            resampler: Resampler::Nearest,
            transparent_index_zero: false,
            clut_rows: 1,
        };
        let psxt = convert(&buf, &cfg).unwrap();

        // Header sanity.
        assert_eq!(&psxt[0..4], b"PSXT");
        assert_eq!(u16::from_le_bytes([psxt[4], psxt[5]]), 1);
        // TextureHeader starts at offset 12.
        assert_eq!(psxt[12], 4); // depth
        assert_eq!(u16::from_le_bytes([psxt[14], psxt[15]]), 4); // width
        assert_eq!(u16::from_le_bytes([psxt[16], psxt[17]]), 4); // height
        assert_eq!(u16::from_le_bytes([psxt[18], psxt[19]]), 16); // clut_entries
                                                                  // pixel_bytes = halfwords_per_row(4bpp, 4) × height × 2 = 1 × 4 × 2 = 8
        assert_eq!(
            u32::from_le_bytes([psxt[20], psxt[21], psxt[22], psxt[23]]),
            8
        );
        // clut_bytes = 16 × 2 = 32
        assert_eq!(
            u32::from_le_bytes([psxt[24], psxt[25], psxt[26], psxt[27]]),
            32
        );
    }

    #[test]
    fn four_bank_4bpp_repeats_one_quantized_palette_without_repacking_pixels() {
        let mut source = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(4, 4, |x, y| {
            image::Rgba([(x * 63) as u8, (y * 63) as u8, 192, 255])
        }))
        .write_to(
            &mut std::io::Cursor::new(&mut source),
            image::ImageFormat::Png,
        )
        .unwrap();
        let psxt = convert(
            &source,
            &Config {
                width: 4,
                height: 4,
                depth: Depth::Bit4,
                crop: CropMode::None,
                resampler: Resampler::Nearest,
                transparent_index_zero: false,
                clut_rows: 4,
            },
        )
        .unwrap();

        assert_eq!(u16::from_le_bytes([psxt[18], psxt[19]]), 64);
        let pixel_bytes = u32::from_le_bytes(psxt[20..24].try_into().unwrap()) as usize;
        assert_eq!(u32::from_le_bytes(psxt[24..28].try_into().unwrap()), 128);
        let clut = &psxt[28 + pixel_bytes..];
        assert_eq!(&clut[0..32], &clut[32..64]);
        assert_eq!(&clut[0..32], &clut[64..96]);
        assert_eq!(&clut[0..32], &clut[96..128]);
    }

    #[test]
    fn indexed_multi_clut_rows_concatenate_palettes() {
        let indices = vec![1, 2, 3, 4, 4, 3, 2, 1];
        let rows = vec![vec![[0, 0, 0], [255, 0, 0]], vec![[0, 0, 0], [0, 0, 255]]];
        let psxt =
            encode_indexed_psxt_with_clut_rows(4, 2, Depth::Bit4, &indices, &rows, false).unwrap();

        assert_eq!(psxt[12], 4); // depth
        assert_eq!(u16::from_le_bytes([psxt[14], psxt[15]]), 4); // width
        assert_eq!(u16::from_le_bytes([psxt[16], psxt[17]]), 2); // height
        assert_eq!(u16::from_le_bytes([psxt[18], psxt[19]]), 32); // 2 x 16-entry CLUT rows
        assert_eq!(
            u32::from_le_bytes([psxt[20], psxt[21], psxt[22], psxt[23]]),
            4
        );
        assert_eq!(
            u32::from_le_bytes([psxt[24], psxt[25], psxt[26], psxt[27]]),
            64
        );
    }

    #[test]
    fn indexed_alpha_can_reserve_palette_zero_for_transparency() {
        let mut buf = Vec::new();
        {
            let img = image::RgbaImage::from_fn(2, 1, |x, _| {
                if x == 0 {
                    image::Rgba([255, 0, 0, 0])
                } else {
                    image::Rgba([0, 0, 255, 255])
                }
            });
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                .unwrap();
        }
        let cfg = Config {
            width: 2,
            height: 1,
            depth: Depth::Bit4,
            crop: CropMode::None,
            resampler: Resampler::Nearest,
            transparent_index_zero: true,
            clut_rows: 1,
        };

        let psxt = convert(&buf, &cfg).unwrap();
        let first_halfword = u16::from_le_bytes([psxt[28], psxt[29]]);
        let left = first_halfword & 0x000F;
        let right = (first_halfword >> 4) & 0x000F;

        assert_eq!(
            u16::from_le_bytes([psxt[6], psxt[7]]),
            psxed_format::texture::flags::INDEX_ZERO_TRANSPARENT
        );
        assert_eq!(left, 0);
        assert_ne!(right, 0);
        assert_eq!(u16::from_le_bytes([psxt[30], psxt[31]]), 0);
    }

    #[test]
    fn in_memory_transparent_quantizer_reserves_zero() {
        let pixels = [[255, 0, 0, 0], [0, 0, 255, 255], [0, 255, 0, 255]];
        let (palette, indices) = quantize_rgba_with_transparent_zero(&pixels, 16).unwrap();
        assert_eq!(palette[0], [0, 0, 0]);
        assert_eq!(indices[0], 0);
        assert_ne!(indices[1], 0);
        assert_ne!(indices[2], 0);
    }

    #[test]
    fn in_memory_quantizer_rejects_invalid_palette_sizes() {
        let pixels = [[0, 0, 0, 255]];
        assert!(quantize_rgba_with_transparent_zero(&pixels, 1).is_err());
        assert!(quantize_rgba_with_transparent_zero(&pixels, 257).is_err());
    }

    #[test]
    fn opaque_in_memory_quantizer_keeps_all_palette_entries_available() {
        let pixels = (0..16)
            .map(|index| [index * 16, 255 - index * 16, index * 7])
            .collect::<Vec<_>>();
        let (palette, indices) = quantize_rgb(&pixels, 16).unwrap();
        assert_eq!(palette.len(), 16);
        assert_eq!(indices.len(), pixels.len());
        assert!(indices.iter().all(|index| *index < 16));
    }

    #[test]
    fn requantize_8bpp_to_4bpp_preserves_size_and_transparency() {
        let indices = vec![0, 1, 2, 3, 3, 2, 1, 0];
        let palette = (0..256)
            .map(|index| [index as u8, 255 - index as u8, (index / 2) as u8])
            .collect::<Vec<_>>();
        let source = encode_indexed_psxt(4, 2, Depth::Bit8, &indices, &palette, true).unwrap();

        let output = requantize_psxt_to_4bpp(&source).unwrap();
        let texture = psx_asset::Texture::from_bytes(&output).unwrap();
        assert_eq!(texture.depth(), Depth::Bit4);
        assert_eq!((texture.width(), texture.height()), (4, 2));
        assert_eq!(texture.clut_entries(), 16);
        assert!(texture.index_zero_transparent());
        assert!(output.len() < source.len());
    }

    #[test]
    fn requantize_4bpp_is_byte_identical() {
        let source = encode_indexed_psxt(
            4,
            1,
            Depth::Bit4,
            &[0, 1, 2, 3],
            &[[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]],
            true,
        )
        .unwrap();
        assert_eq!(requantize_psxt_to_4bpp(&source).unwrap(), source);
    }
}
