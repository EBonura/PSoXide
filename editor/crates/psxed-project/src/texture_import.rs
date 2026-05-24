//! Cooked texture registration + image import.
//!
//! Two entry points feed the same end product --
//! [`ResourceData::Texture`] -- from different sources:
//!
//! * [`register_cooked_texture`] adopts an existing cooked `.psxt`
//!   file, validates it through `psx_asset`, and records a
//!   project-relative path when possible.
//!
//! * [`import_texture`] runs PNG/JPG/BMP source bytes through the
//!   texture cooker (`psxed_tex::convert`), writes the cooked output
//!   under `project/assets/textures/<safe_name>.psxt`, then registers
//!   it as a Texture resource.

use std::path::{Path, PathBuf};

use psxed_format::{texture::TextureHeader, AssetHeader};

use crate::{ProjectDocument, ResourceData, ResourceId};

pub use psxed_format::texture::Depth as TextureDepth;
pub use psxed_tex::{CropMode, CropRect, Resampler};

/// Configuration for one editor texture import.
#[derive(Clone, Debug)]
pub struct TextureImportConfig {
    /// Target texture width in texels.
    pub width: u16,
    /// Target texture height in texels.
    pub height: u16,
    /// PSX colour depth.
    pub depth: TextureDepth,
    /// Source-side crop behaviour.
    pub crop: CropMode,
    /// Resampling kernel.
    pub resampler: Resampler,
    /// Baked RGB tint. `[255, 255, 255]` leaves the source unchanged.
    pub tint: [u8; 3],
    /// Reserve palette index 0 for source-alpha transparency.
    pub transparent_index_zero: bool,
}

impl TextureImportConfig {
    fn cooker_config(&self) -> psxed_tex::Config {
        psxed_tex::Config {
            width: self.width,
            height: self.height,
            depth: self.depth,
            crop: self.crop,
            resampler: self.resampler,
            transparent_index_zero: self.transparent_index_zero,
        }
    }
}

/// Header-derived statistics about a `.psxt` blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureStats {
    /// On-disk byte count.
    pub bytes: usize,
    /// Texel width.
    pub width: u16,
    /// Texel height.
    pub height: u16,
    /// Bits per pixel -- `4`, `8`, or `15`.
    pub depth: u8,
    /// CLUT entry count (`16` for 4bpp, `256` for 8bpp, `0`
    /// for direct 15bpp).
    pub clut_entries: u16,
    /// Indexed palette entry 0 is reserved for transparent texels.
    pub index_zero_transparent: bool,
}

/// Cooked preview returned by [`preview_texture_import`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureImportPreview {
    /// Cooked `.psxt` bytes ready to write to disk.
    pub texture: Vec<u8>,
    /// Parsed stats from the cooked blob.
    pub stats: TextureStats,
}

/// Summary of a cooked indexed texture colour-key edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureColorKeyReport {
    /// Palette index at the clicked texel.
    pub picked_index: u8,
    /// RGB888 display colour represented by the picked palette entry.
    pub picked_rgb: [u8; 3],
    /// Number of palette indices with the same RGB555 colour as the
    /// picked texel.
    pub keyed_indices: u16,
    /// Number of visible texels rewritten to palette index 0.
    pub rewritten_texels: usize,
}

/// Failure modes for cooked texture registration/import.
#[derive(Debug)]
pub enum TextureImportError {
    /// Source image or cooked texture path is not a regular file.
    SourceNotAFile(PathBuf),
    /// `psx_asset::Texture::from_bytes` rejected the cooked bytes.
    InvalidTexture {
        /// Path that failed to parse, or the source path when the
        /// failing blob came from an in-memory preview cook.
        path: PathBuf,
        /// Diagnostic message.
        detail: String,
    },
    /// Filesystem error reading or writing an import artifact.
    Io {
        /// Path the IO error originated at.
        path: PathBuf,
        /// Underlying error message.
        detail: String,
    },
    /// Source image conversion failed inside `psxed_tex`.
    ConversionFailed {
        /// Source image path.
        source: PathBuf,
        /// Diagnostic message.
        detail: String,
    },
    /// The destination path exists but is not a regular file that
    /// can be replaced by a cooked `.psxt`.
    OutputExists(PathBuf),
}

impl std::fmt::Display for TextureImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceNotAFile(path) => write!(f, "{} is not a file", path.display()),
            Self::InvalidTexture { path, detail } => {
                write!(f, "{}: invalid .psxt: {detail}", path.display())
            }
            Self::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::ConversionFailed { source, detail } => {
                write!(
                    f,
                    "{}: texture conversion failed: {detail}",
                    source.display()
                )
            }
            Self::OutputExists(path) => write!(
                f,
                "output path {} exists with conflicting content",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TextureImportError {}

/// Compute [`TextureStats`] from `.psxt` bytes.
pub fn texture_stats_from_bytes(bytes: &[u8]) -> Result<TextureStats, TextureImportError> {
    let texture =
        psx_asset::Texture::from_bytes(bytes).map_err(|e| TextureImportError::InvalidTexture {
            path: PathBuf::new(),
            detail: format!("{:?}", e),
        })?;
    let depth = match texture.depth() {
        psxed_format::texture::Depth::Bit4 => 4,
        psxed_format::texture::Depth::Bit8 => 8,
        psxed_format::texture::Depth::Bit15 => 15,
    };
    Ok(TextureStats {
        bytes: bytes.len(),
        width: texture.width(),
        height: texture.height(),
        depth,
        clut_entries: texture.clut_entries(),
        index_zero_transparent: texture.index_zero_transparent(),
    })
}

/// Treat the colour under `(x, y)` in a cooked indexed `.psxt` as a
/// colour key: all texels using palette entries with the same RGB555
/// colour are rewritten to index 0, CLUT entry 0 is cleared to raw
/// transparent black, and the asset header's transparent-zero flag is
/// set.
///
/// This is intentionally a cooked-asset edit. It lets the editor add
/// PS1-style cutout transparency to imported opaque sources without
/// requiring the original PNG/BMP to carry alpha.
pub fn apply_texel_color_key_transparency(
    bytes: &mut [u8],
    x: u16,
    y: u16,
) -> Result<TextureColorKeyReport, TextureImportError> {
    let (depth, width, height, clut_entries, pixel_len, clut_len) = {
        let texture = psx_asset::Texture::from_bytes(bytes).map_err(|e| {
            TextureImportError::InvalidTexture {
                path: PathBuf::new(),
                detail: format!("{:?}", e),
            }
        })?;
        (
            texture.depth(),
            texture.width(),
            texture.height(),
            texture.clut_entries(),
            texture.pixel_bytes().len(),
            texture.clut_bytes().len(),
        )
    };
    if x >= width || y >= height {
        return Err(TextureImportError::InvalidTexture {
            path: PathBuf::new(),
            detail: format!("texel ({x}, {y}) outside {width}x{height} texture"),
        });
    }
    let Some(expected_clut_entries) = depth.clut_entries() else {
        return Err(TextureImportError::InvalidTexture {
            path: PathBuf::new(),
            detail: "colour-key transparency requires an indexed texture".to_string(),
        });
    };
    if clut_entries != expected_clut_entries {
        return Err(TextureImportError::InvalidTexture {
            path: PathBuf::new(),
            detail: format!(
                "colour-key transparency expects {} CLUT entries for {}bpp, found {}",
                expected_clut_entries, depth as u8, clut_entries
            ),
        });
    }

    let pixel_start = AssetHeader::SIZE + TextureHeader::SIZE;
    let pixel_end = pixel_start
        .checked_add(pixel_len)
        .ok_or_else(|| invalid_texture("pixel block length overflow"))?;
    let clut_end = pixel_end
        .checked_add(clut_len)
        .ok_or_else(|| invalid_texture("CLUT block length overflow"))?;
    if clut_end > bytes.len() || clut_len < usize::from(clut_entries) * 2 {
        return Err(invalid_texture("PSXT payload shorter than declared blocks"));
    }

    let halfwords_per_row = usize::from(TextureHeader::halfwords_per_row(depth, width));
    let expected_pixel_len = halfwords_per_row
        .checked_mul(usize::from(height))
        .and_then(|halfwords| halfwords.checked_mul(2))
        .ok_or_else(|| invalid_texture("indexed pixel block length overflow"))?;
    if pixel_len < expected_pixel_len {
        return Err(invalid_texture(
            "indexed pixel block shorter than texture dimensions",
        ));
    }

    let picked_index = indexed_texel_at(
        bytes,
        pixel_start,
        halfwords_per_row,
        depth,
        usize::from(x),
        usize::from(y),
    )
    .ok_or_else(|| invalid_texture("failed to sample indexed texel"))?;
    let target_rgb555 = palette_rgb555(bytes, pixel_end, picked_index)
        .ok_or_else(|| invalid_texture("failed to sample CLUT entry"))?;
    let picked_rgb = rgb555_to_rgb888(target_rgb555);
    let mut keyed = [false; 256];
    let mut keyed_indices = 0_u16;
    for index in 0..usize::from(clut_entries) {
        if palette_rgb555(bytes, pixel_end, index as u8) == Some(target_rgb555) {
            keyed[index] = true;
            keyed_indices += 1;
        }
    }

    let rewritten_texels = rewrite_indexed_texels_to_zero(
        bytes,
        pixel_start,
        halfwords_per_row,
        depth,
        width,
        height,
        &keyed,
    );

    bytes[pixel_end] = 0;
    bytes[pixel_end + 1] = 0;
    let flags_offset = 6;
    let mut flags = u16::from_le_bytes([bytes[flags_offset], bytes[flags_offset + 1]]);
    flags |= psxed_format::texture::flags::INDEX_ZERO_TRANSPARENT;
    bytes[flags_offset..flags_offset + 2].copy_from_slice(&flags.to_le_bytes());

    Ok(TextureColorKeyReport {
        picked_index,
        picked_rgb,
        keyed_indices,
        rewritten_texels,
    })
}

/// Adopt an existing cooked `.psxt` as a [`ResourceData::Texture`].
pub fn register_cooked_texture(
    project: &mut ProjectDocument,
    psxt_path: &Path,
    display_name: &str,
    project_root: Option<&Path>,
) -> Result<ResourceId, TextureImportError> {
    if !psxt_path.is_file() {
        return Err(TextureImportError::SourceNotAFile(psxt_path.to_path_buf()));
    }

    let bytes = std::fs::read(psxt_path).map_err(|e| TextureImportError::Io {
        path: psxt_path.to_path_buf(),
        detail: e.to_string(),
    })?;
    psx_asset::Texture::from_bytes(&bytes).map_err(|e| TextureImportError::InvalidTexture {
        path: psxt_path.to_path_buf(),
        detail: format!("{:?}", e),
    })?;

    let name = display_name_from_input(display_name, psxt_path, "Texture");
    Ok(project.add_resource(
        name,
        ResourceData::Texture {
            psxt_path: relativise(psxt_path, project_root),
        },
    ))
}

/// Convert a PNG/JPG/BMP source into cooked `.psxt` bytes without
/// writing files or mutating a project. The editor uses this for
/// import preview before committing the asset.
pub fn preview_texture_import(
    source_path: &Path,
    config: &TextureImportConfig,
) -> Result<TextureImportPreview, TextureImportError> {
    let texture = convert_source_texture(source_path, config)?;
    let stats = texture_stats_from_bytes(&texture).map_err(|e| match e {
        TextureImportError::InvalidTexture { detail, .. } => TextureImportError::InvalidTexture {
            path: source_path.to_path_buf(),
            detail,
        },
        other => other,
    })?;
    Ok(TextureImportPreview { texture, stats })
}

/// Convert a PNG/JPG/BMP source through the texture cooker, write the
/// cooked output under `project_root/assets/textures/<safe_name>.psxt`,
/// then register it as a [`ResourceData::Texture`].
///
/// Existing `.psxt` files at the exact output path are replaced, which
/// mirrors the model importer's "same cooked bundle output" workflow.
/// Directories or other non-file entries at that path are rejected.
pub fn import_texture(
    project: &mut ProjectDocument,
    source_path: &Path,
    output_name: &str,
    project_root: &Path,
    config: &TextureImportConfig,
) -> Result<ResourceId, TextureImportError> {
    let preview = preview_texture_import(source_path, config)?;
    let display_name = display_name_from_input(output_name, source_path, "Texture");
    let safe = safe_file_stem(&display_name);
    let texture_dir = project_root.join("assets").join("textures");
    std::fs::create_dir_all(&texture_dir).map_err(|e| TextureImportError::Io {
        path: texture_dir.clone(),
        detail: e.to_string(),
    })?;

    let texture_path = texture_dir.join(format!("{safe}.psxt"));
    if texture_path.exists() && !texture_path.is_file() {
        return Err(TextureImportError::OutputExists(texture_path));
    }
    std::fs::write(&texture_path, &preview.texture).map_err(|e| TextureImportError::Io {
        path: texture_path.clone(),
        detail: e.to_string(),
    })?;

    register_cooked_texture(project, &texture_path, &display_name, Some(project_root))
}

fn convert_source_texture(
    source_path: &Path,
    config: &TextureImportConfig,
) -> Result<Vec<u8>, TextureImportError> {
    if !source_path.is_file() {
        return Err(TextureImportError::SourceNotAFile(
            source_path.to_path_buf(),
        ));
    }
    let src_bytes = std::fs::read(source_path).map_err(|e| TextureImportError::Io {
        path: source_path.to_path_buf(),
        detail: e.to_string(),
    })?;
    let mut texture = psxed_tex::convert(&src_bytes, &config.cooker_config()).map_err(|e| {
        TextureImportError::ConversionFailed {
            source: source_path.to_path_buf(),
            detail: e.to_string(),
        }
    })?;
    apply_tint_to_psxt(&mut texture, config.tint).map_err(|detail| {
        TextureImportError::InvalidTexture {
            path: source_path.to_path_buf(),
            detail,
        }
    })?;
    Ok(texture)
}

fn apply_tint_to_psxt(bytes: &mut [u8], tint: [u8; 3]) -> Result<(), String> {
    if tint == [255, 255, 255] {
        return Ok(());
    }
    if bytes.len() < AssetHeader::SIZE + TextureHeader::SIZE {
        return Err("truncated PSXT header".to_string());
    }

    let header = &bytes[AssetHeader::SIZE..AssetHeader::SIZE + TextureHeader::SIZE];
    let depth = TextureDepth::from_byte(header[0])
        .ok_or_else(|| format!("unsupported depth byte {}", header[0]))?;
    let pixel_bytes = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    let clut_bytes = u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as usize;

    let pixel_start = AssetHeader::SIZE + TextureHeader::SIZE;
    let pixel_end = pixel_start
        .checked_add(pixel_bytes)
        .ok_or_else(|| "pixel block length overflow".to_string())?;
    let clut_end = pixel_end
        .checked_add(clut_bytes)
        .ok_or_else(|| "CLUT block length overflow".to_string())?;
    if clut_end > bytes.len() {
        return Err("PSXT payload shorter than declared blocks".to_string());
    }

    match depth {
        TextureDepth::Bit4 | TextureDepth::Bit8 => {
            for chunk in bytes[pixel_end..clut_end].chunks_exact_mut(2) {
                tint_rgb555_bytes(chunk, tint);
            }
        }
        TextureDepth::Bit15 => {
            for chunk in bytes[pixel_start..pixel_end].chunks_exact_mut(2) {
                tint_rgb555_bytes(chunk, tint);
            }
        }
    }
    Ok(())
}

fn indexed_texel_at(
    bytes: &[u8],
    pixel_start: usize,
    halfwords_per_row: usize,
    depth: TextureDepth,
    x: usize,
    y: usize,
) -> Option<u8> {
    match depth {
        TextureDepth::Bit4 => {
            let off = pixel_start + (y * halfwords_per_row + x / 4) * 2;
            let word = u16::from_le_bytes([*bytes.get(off)?, *bytes.get(off + 1)?]);
            Some(((word >> ((x & 3) * 4)) & 0xF) as u8)
        }
        TextureDepth::Bit8 => {
            let off = pixel_start + (y * halfwords_per_row + x / 2) * 2 + (x & 1);
            bytes.get(off).copied()
        }
        TextureDepth::Bit15 => None,
    }
}

fn palette_rgb555(bytes: &[u8], clut_start: usize, index: u8) -> Option<u16> {
    let off = clut_start + usize::from(index) * 2;
    Some(u16::from_le_bytes([*bytes.get(off)?, *bytes.get(off + 1)?]) & 0x7FFF)
}

fn rgb555_to_rgb888(raw: u16) -> [u8; 3] {
    let r5 = (raw & 0x1F) as u8;
    let g5 = ((raw >> 5) & 0x1F) as u8;
    let b5 = ((raw >> 10) & 0x1F) as u8;
    [
        (r5 << 3) | (r5 >> 2),
        (g5 << 3) | (g5 >> 2),
        (b5 << 3) | (b5 >> 2),
    ]
}

fn rewrite_indexed_texels_to_zero(
    bytes: &mut [u8],
    pixel_start: usize,
    halfwords_per_row: usize,
    depth: TextureDepth,
    width: u16,
    height: u16,
    keyed: &[bool; 256],
) -> usize {
    let mut rewritten = 0;
    match depth {
        TextureDepth::Bit4 => {
            for row in 0..usize::from(height) {
                for hw in 0..halfwords_per_row {
                    let off = pixel_start + (row * halfwords_per_row + hw) * 2;
                    let mut word = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
                    for nibble in 0..4 {
                        if hw * 4 + nibble >= usize::from(width) {
                            break;
                        }
                        let shift = nibble * 4;
                        let index = ((word >> shift) & 0xF) as usize;
                        if keyed[index] {
                            rewritten += 1;
                            word &= !(0xF << shift);
                        }
                    }
                    bytes[off..off + 2].copy_from_slice(&word.to_le_bytes());
                }
            }
        }
        TextureDepth::Bit8 => {
            for row in 0..usize::from(height) {
                for x in 0..usize::from(width) {
                    let off = pixel_start + (row * halfwords_per_row + x / 2) * 2 + (x & 1);
                    let index = bytes[off] as usize;
                    if keyed[index] {
                        rewritten += 1;
                        bytes[off] = 0;
                    }
                }
            }
        }
        TextureDepth::Bit15 => {}
    }
    rewritten
}

fn invalid_texture(detail: impl Into<String>) -> TextureImportError {
    TextureImportError::InvalidTexture {
        path: PathBuf::new(),
        detail: detail.into(),
    }
}

fn tint_rgb555_bytes(bytes: &mut [u8], tint: [u8; 3]) {
    let raw = u16::from_le_bytes([bytes[0], bytes[1]]);
    let stp = raw & 0x8000;
    let r = tint_5bit((raw & 0x1F) as u8, tint[0]);
    let g = tint_5bit(((raw >> 5) & 0x1F) as u8, tint[1]);
    let b = tint_5bit(((raw >> 10) & 0x1F) as u8, tint[2]);
    let tinted = stp | (r as u16) | ((g as u16) << 5) | ((b as u16) << 10);
    bytes.copy_from_slice(&tinted.to_le_bytes());
}

fn tint_5bit(channel: u8, tint: u8) -> u8 {
    (((channel as u16) * (tint as u16) + 127) / 255).min(31) as u8
}

fn display_name_from_input(name: &str, source_path: &Path, fallback: &str) -> String {
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    source_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

fn relativise(path: &Path, project_root: Option<&Path>) -> String {
    if let Some(root) = project_root {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.to_string_lossy().into_owned()
}

fn safe_file_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "texture".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logo_source() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("assets")
            .join("branding")
            .join("logo-icon-player.png")
    }

    fn temp_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "psxed-texture-import-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_config() -> TextureImportConfig {
        TextureImportConfig {
            width: 16,
            height: 16,
            depth: TextureDepth::Bit4,
            crop: CropMode::CentreSquare,
            resampler: Resampler::Nearest,
            tint: [255, 255, 255],
            transparent_index_zero: false,
        }
    }

    #[test]
    fn preview_texture_import_cooks_psxt() {
        let preview =
            preview_texture_import(&logo_source(), &test_config()).expect("preview cooks");
        assert_eq!(preview.stats.width, 16);
        assert_eq!(preview.stats.height, 16);
        assert_eq!(preview.stats.depth, 4);
        assert_eq!(preview.stats.clut_entries, 16);

        let stats = texture_stats_from_bytes(&preview.texture).expect("preview parses");
        assert_eq!(stats, preview.stats);
    }

    #[test]
    fn import_texture_writes_project_relative_resource() {
        let root = temp_project("relative-resource");
        let mut project = ProjectDocument::new("texture-test");
        let id = import_texture(
            &mut project,
            &logo_source(),
            "Logo Icon",
            &root,
            &test_config(),
        )
        .expect("import succeeds");

        let resource = project.resource(id).expect("resource exists");
        let ResourceData::Texture { psxt_path } = &resource.data else {
            panic!("expected Texture resource, got {:?}", resource.data);
        };
        assert_eq!(resource.name, "Logo Icon");
        assert_eq!(psxt_path, "assets/textures/logo_icon.psxt");

        let bytes = std::fs::read(root.join(psxt_path)).expect("psxt written");
        let stats = texture_stats_from_bytes(&bytes).expect("written psxt parses");
        assert_eq!(stats.width, 16);
        assert_eq!(stats.height, 16);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn register_cooked_texture_rejects_missing_file() {
        let mut project = ProjectDocument::new("texture-test");
        let missing = PathBuf::from("/tmp/definitely-not-a-psoxide-texture.psxt");
        match register_cooked_texture(&mut project, &missing, "Missing", None) {
            Err(TextureImportError::SourceNotAFile(path)) => assert_eq!(path, missing),
            other => panic!("expected SourceNotAFile, got {other:?}"),
        }
    }

    #[test]
    fn safe_file_stem_strips_punctuation() {
        assert_eq!(safe_file_stem("Brick Wall"), "brick_wall");
        assert_eq!(safe_file_stem("floor-tile_01"), "floor_tile_01");
        assert_eq!(safe_file_stem("!!!"), "texture");
    }

    #[test]
    fn tint_rgb555_preserves_stp_and_scales_channels() {
        let mut bytes = (0x8000_u16 | 31 | (16 << 5) | (8 << 10)).to_le_bytes();
        tint_rgb555_bytes(&mut bytes, [255, 0, 128]);
        let tinted = u16::from_le_bytes(bytes);
        assert_eq!(tinted & 0x8000, 0x8000);
        assert_eq!(tinted & 0x1F, 31);
        assert_eq!((tinted >> 5) & 0x1F, 0);
        assert_eq!((tinted >> 10) & 0x1F, 4);
    }

    #[test]
    fn texel_color_key_rewrites_matching_palette_entries_to_zero() {
        let indices = [
            1, 2, 3, 1, //
            2, 3, 1, 2, //
        ];
        let palette = [[0, 0, 0], [16, 24, 32], [255, 0, 0], [255, 0, 0]];
        let mut bytes =
            psxed_tex::encode_indexed_psxt(4, 2, TextureDepth::Bit4, &indices, &palette, false)
                .expect("encode indexed psxt");

        let report =
            apply_texel_color_key_transparency(&mut bytes, 1, 0).expect("colour key applies");
        assert_eq!(report.picked_index, 2);
        assert_eq!(report.keyed_indices, 2);
        assert_eq!(report.rewritten_texels, 5);

        let stats = texture_stats_from_bytes(&bytes).expect("rewritten texture parses");
        assert!(stats.index_zero_transparent);
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("texture parses");
        assert!(texture.index_zero_transparent());
        assert_eq!(&texture.clut_bytes()[0..2], &[0, 0]);

        let pixel_start = AssetHeader::SIZE + TextureHeader::SIZE;
        let row0 = u16::from_le_bytes([bytes[pixel_start], bytes[pixel_start + 1]]);
        let row1 = u16::from_le_bytes([bytes[pixel_start + 2], bytes[pixel_start + 3]]);
        assert_eq!(row0 & 0x000F, 1);
        assert_eq!((row0 >> 4) & 0x000F, 0);
        assert_eq!((row0 >> 8) & 0x000F, 0);
        assert_eq!((row0 >> 12) & 0x000F, 1);
        assert_eq!(row1 & 0x000F, 0);
        assert_eq!((row1 >> 4) & 0x000F, 0);
        assert_eq!((row1 >> 8) & 0x000F, 1);
        assert_eq!((row1 >> 12) & 0x000F, 0);
    }
}
