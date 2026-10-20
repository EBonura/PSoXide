//! How much of the light a surface sends back: the average colour of its
//! texture, times its tint.

use std::collections::HashMap;
use std::path::Path;

use psx_asset::Texture;
use psxed_format::texture::Depth;

use super::gather::{BakeMaterials, FALLBACK_ALBEDO};
use crate::{ProjectDocument, ResourceData, ResourceId};

/// Average colour of the visible texels of a PSXT, each channel 0..=1, or
/// `None` when the bytes are not a texture or hold no opaque texel.
///
/// A texel is skipped when it is the transparent index of a cut-out texture;
/// a texture with no transparency counts every texel.
pub fn mean_texel_colour(psxt: &[u8]) -> Option<[f32; 3]> {
    let texture = Texture::from_bytes(psxt).ok()?;
    let (width, height) = (usize::from(texture.width()), usize::from(texture.height()));
    let pixels = texture.pixel_bytes();
    let clut = texture.clut_bytes();
    let stride = usize::from(texture.halfwords_per_row()) * 2;
    let cut_out = texture.is_index_zero_transparent();
    let colour555 = |lo: u8, hi: u8| -> [u32; 3] {
        let v = u32::from(u16::from_le_bytes([lo, hi]));
        // 5 bits per channel to 8, replicating the top bits.
        let expand = |c: u32| (c << 3) | (c >> 2);
        [
            expand(v & 31),
            expand((v >> 5) & 31),
            expand((v >> 10) & 31),
        ]
    };
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for y in 0..height {
        let row = pixels.get(y * stride..)?;
        for x in 0..width {
            let rgb = match texture.depth() {
                Depth::Bit4 => {
                    let byte = *row.get(x / 2)?;
                    let index = usize::from(if x % 2 == 0 { byte & 15 } else { byte >> 4 });
                    if cut_out && index == 0 {
                        continue;
                    }
                    colour555(*clut.get(index * 2)?, *clut.get(index * 2 + 1)?)
                }
                Depth::Bit8 => {
                    let index = usize::from(*row.get(x)?);
                    if cut_out && index == 0 {
                        continue;
                    }
                    colour555(*clut.get(index * 2)?, *clut.get(index * 2 + 1)?)
                }
                _ => {
                    let (lo, hi) = (*row.get(x * 2)?, *row.get(x * 2 + 1)?);
                    if cut_out && u16::from_le_bytes([lo, hi]) == 0 {
                        continue;
                    }
                    colour555(lo, hi)
                }
            };
            for c in 0..3 {
                sum[c] += u64::from(rgb[c]);
            }
            count += 1;
        }
    }
    (count > 0).then(|| core::array::from_fn(|c| sum[c] as f32 / (count as f32 * 255.0)))
}

/// Reflectance of a surface from its texture bytes (when it has any) and
/// tint: an untextured surface shows the tint on white.
pub fn reflectance(psxt: Option<&[u8]>, tint: [u8; 3]) -> [f32; 3] {
    let base = psxt.and_then(mean_texel_colour).unwrap_or([1.0; 3]);
    core::array::from_fn(|c| (base[c] * f32::from(tint[c]) / 128.0).clamp(0.0, 1.0))
}

/// Albedo per material, read once from the project's texture files.
#[derive(Default)]
pub struct AlbedoTable {
    table: HashMap<Option<ResourceId>, [f32; 3]>,
    /// Materials light passes through: sky apertures and translucent ones.
    open: std::collections::HashSet<Option<ResourceId>>,
}

impl AlbedoTable {
    /// The albedo of each of `materials`, from the same texture bytes the cook
    /// binds, so the editor preview and a Play build bounce the same colours.
    pub fn from_project(
        project: &ProjectDocument,
        project_root: &Path,
        materials: impl IntoIterator<Item = Option<ResourceId>>,
    ) -> Self {
        let mut table = HashMap::new();
        let mut open = std::collections::HashSet::new();
        for id in materials {
            if let Some(ResourceData::Material(data)) =
                id.and_then(|id| project.resource(id)).map(|r| &r.data)
            {
                if data.sky_aperture || data.blend_mode != crate::PsxBlendMode::Opaque {
                    open.insert(id);
                }
            }
            table.entry(id).or_insert_with(|| {
                let Some(id) = id else {
                    return FALLBACK_ALBEDO;
                };
                let Some(ResourceData::Material(data)) = project.resource(id).map(|r| &r.data)
                else {
                    return FALLBACK_ALBEDO;
                };
                let bytes = if data.sky_aperture {
                    None
                } else {
                    crate::resolve_material_texture_psxt(project, id, project_root)
                        .ok()
                        .flatten()
                        .map(|(_, bytes)| bytes)
                };
                reflectance(bytes.as_deref(), data.tint)
            });
        }
        Self { table, open }
    }

    /// Mark `material` as one light passes through.
    pub fn set_open(&mut self, material: Option<ResourceId>) {
        self.open.insert(material);
    }

    /// An entry added by hand, for tests and binders.
    pub fn insert(&mut self, material: Option<ResourceId>, albedo: [f32; 3]) {
        self.table.insert(material, albedo);
    }
}

impl BakeMaterials for AlbedoTable {
    fn albedo(&self, material: Option<ResourceId>) -> [f32; 3] {
        self.table
            .get(&material)
            .copied()
            .unwrap_or(FALLBACK_ALBEDO)
    }

    fn occludes(&self, material: Option<ResourceId>) -> bool {
        !self.open.contains(&material)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psxt(pixels: &[u8], palette: &[[u8; 3]]) -> Vec<u8> {
        psxed_tex::encode_indexed_psxt(8, 8, Depth::Bit4, pixels, palette, false)
            .expect("a valid texture")
    }

    #[test]
    fn the_mean_of_a_two_colour_texture_is_halfway() {
        let mut pixels = vec![0u8; 64];
        for p in pixels.iter_mut().skip(32) {
            *p = 1;
        }
        let bytes = psxt(&pixels, &[[0, 0, 0], [255, 255, 255]]);
        let mean = mean_texel_colour(&bytes).expect("a mean");
        for c in mean {
            assert!((c - 0.5).abs() < 0.03, "{c}");
        }
    }

    #[test]
    fn a_tint_scales_the_reflectance_and_it_stays_a_fraction() {
        let bytes = psxt(&[1; 64], &[[0, 0, 0], [255, 255, 255]]);
        let half = reflectance(Some(&bytes), [64, 64, 64]);
        assert!((half[0] - 0.5).abs() < 0.03);
        let over = reflectance(Some(&bytes), [255, 255, 255]);
        assert!(over.iter().all(|&c| c <= 1.0));
        assert_eq!(reflectance(None, [128; 3]), [1.0; 3]);
    }
}
