//! The Graybox surface set of a new grid project.
//!
//! The base project's three Graybox materials are one grid texture in three
//! blue-grey tints, so a trim, a main wall and a frieze barely differ. A new
//! grid project also gets six materials that read apart at a glance: warm
//! stone, dark trim, a light frieze, wood, metal and moss. They are
//! generated materials (a base colour plus value noise the editor and the
//! cook bake to a 64 texel 4bpp texture), so they need no files and no
//! imported images, and they sit in the palette beside the originals because
//! their names start with `Graybox`.

use crate::{
    GeneratedMaterialTexture, GeneratedTextureUv, MaterialResource, MaterialTextureMode,
    ProceduralNoiseTexture, ProjectDocument, ResourceData, ResourceId,
};

/// One material of the set.
struct Entry {
    /// Name, after the `Graybox / ` prefix.
    name: &'static str,
    /// Dark end of the baked palette.
    base: [u8; 3],
    /// Bright end of the baked palette.
    bright: [u8; 3],
    /// Noise seed.
    seed: u32,
    /// Broadest feature, in texels.
    feature: u8,
    /// Detail layers.
    octaves: u8,
    /// Contrast around the midpoint.
    contrast: u8,
}

/// The set, in palette order.
const SET: [Entry; 6] = [
    Entry {
        name: "Warm Stone",
        base: [128, 104, 80],
        bright: [196, 168, 132],
        seed: 11,
        feature: 20,
        octaves: 3,
        contrast: 150,
    },
    Entry {
        name: "Dark Trim",
        base: [28, 24, 26],
        bright: [72, 62, 60],
        seed: 23,
        feature: 12,
        octaves: 2,
        contrast: 140,
    },
    Entry {
        name: "Light Frieze",
        base: [188, 176, 150],
        bright: [244, 236, 212],
        seed: 37,
        feature: 16,
        octaves: 2,
        contrast: 130,
    },
    Entry {
        name: "Wood",
        base: [88, 52, 26],
        bright: [152, 100, 52],
        seed: 41,
        feature: 8,
        octaves: 4,
        contrast: 190,
    },
    Entry {
        name: "Metal",
        base: [64, 70, 78],
        bright: [150, 158, 170],
        seed: 53,
        feature: 6,
        octaves: 3,
        contrast: 120,
    },
    Entry {
        name: "Moss",
        base: [38, 62, 28],
        bright: [104, 140, 58],
        seed: 67,
        feature: 24,
        octaves: 4,
        contrast: 180,
    },
];

/// The full name of a set entry.
fn full_name(entry: &Entry) -> String {
    format!("Graybox / {} 64", entry.name)
}

/// Names of the set, as the palette lists them.
pub fn names() -> Vec<String> {
    SET.iter().map(full_name).collect()
}

fn material(entry: &Entry) -> MaterialResource {
    let mut m = MaterialResource::opaque(None);
    m.texture_mode = MaterialTextureMode::Generated;
    m.generated = GeneratedMaterialTexture {
        size: 64,
        base_color: entry.base,
        noise_enabled: true,
        noise_color: entry.bright,
        noise: ProceduralNoiseTexture {
            seed: entry.seed,
            feature_size: entry.feature,
            octaves: entry.octaves,
            contrast: entry.contrast,
        },
        noise_uv: GeneratedTextureUv::default(),
    };
    m
}

/// Add the set to `project`, skipping any name it already has. Returns the
/// ids of the set's materials in palette order.
pub fn add_to(project: &mut ProjectDocument) -> Vec<ResourceId> {
    SET.iter()
        .map(|entry| {
            let name = full_name(entry);
            let existing = project
                .resources
                .iter()
                .find(|r| r.name == name && matches!(r.data, ResourceData::Material(_)))
                .map(|r| r.id);
            existing.unwrap_or_else(|| {
                project.add_resource(name, ResourceData::Material(material(entry)))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::palette::palette_materials;

    fn colour_distance(a: [u8; 3], b: [u8; 3]) -> i32 {
        (0..3)
            .map(|i| (i32::from(a[i]) - i32::from(b[i])).abs())
            .sum()
    }

    #[test]
    fn the_set_is_in_the_palette_and_reads_apart() {
        // A bare document has none of them; the starter project has all.
        let mut project = ProjectDocument::new("set");
        let ids = add_to(&mut project);
        assert_eq!(ids.len(), 6);
        assert_eq!(project.resources.len(), 6);
        // Adding twice changes nothing.
        assert_eq!(add_to(&mut project), ids);
        assert_eq!(project.resources.len(), 6);
        let starter = ProjectDocument::starter();
        for name in names() {
            let count = starter.resources.iter().filter(|r| r.name == name).count();
            assert_eq!(count, 1, "{name} in a new grid project");
        }
        let palette = palette_materials(&project, false);
        for id in &ids {
            assert!(palette.iter().any(|(p, _)| p == id), "{id:?} is offered");
        }
        // The mean colour of every pair differs by a wide margin.
        let mean = |e: &Entry| -> [u8; 3] {
            core::array::from_fn(|i| ((u16::from(e.base[i]) + u16::from(e.bright[i])) / 2) as u8)
        };
        for (i, a) in SET.iter().enumerate() {
            for b in &SET[i + 1..] {
                assert!(
                    colour_distance(mean(a), mean(b)) >= 60,
                    "{} and {} look alike",
                    a.name,
                    b.name
                );
            }
        }
    }

    #[test]
    fn every_entry_bakes_a_64_texel_texture_without_files() {
        let mut project = ProjectDocument::new("set");
        let ids = add_to(&mut project);
        for id in ids {
            let resolved = crate::resolve_material_texture_psxt(
                &project,
                id,
                std::path::Path::new("/nonexistent"),
            )
            .expect("generated materials need no file")
            .expect("a texture");
            assert!(
                resolved.0.starts_with("@material-generated:"),
                "{}",
                resolved.0
            );
            assert!(!resolved.1.is_empty());
        }
    }
}
