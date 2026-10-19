//! Cooking a grid world for Play.
//!
//! A grid project's world is its grid document and nothing else (plan
//! decision D2). Play cooks the document straight into the PGL level format
//! ([`crate::grid::cook`]), with no derived geometry. This
//! module is the glue to the playtest package: the cap check, the material
//! binder that turns document materials into texture assets, the scene's
//! point lights, and the package record the manifest writer emits.

use std::collections::HashMap;
use std::path::Path;

use psx_asset::Texture;
use psx_grid::{MATERIAL_CUTOUT, SECTOR_BYTES};

use crate::grid::cook::{self, BoundMaterial, CookInput, MaterialBinder, RuntimeUnits};
use crate::grid::light::GridLight;
use crate::grid::limits::{self, Severity};
use crate::grid::GridWorld;
use crate::{
    MaterialAnimationMode, MaterialFaceSidedness, NodeKind, ProjectDocument, PsxBlendMode,
    ResourceData, ResourceId, Scene,
};

use super::{
    PlaytestAsset, PlaytestAssetKind, PlaytestGridWorld, PlaytestValidationReport, StreamedClass,
};

/// Texel size of the tile the grid renderer maps one material onto.
const TILE_TEXELS: u16 = 64;
/// Light reach for a scene with no World node, runtime units.
const DEFAULT_LIGHT_RADIUS_UNITS: f64 = 1024.0;

/// Check a grid scene against the shared caps and the runtime lattice.
/// Returns `false` when the world cannot be cooked; the reasons are in
/// `report`. A scene without a grid world is refused later, by the cook.
pub(super) fn validate(project: &ProjectDocument, report: &mut PlaytestValidationReport) -> bool {
    let Some(world) = project.active_scene().world.grid() else {
        return true;
    };
    let issues = limits::validate(world);
    for issue in &issues {
        match issue.severity() {
            Severity::Error => report.error(format!("grid world: {}", issue.describe())),
            Severity::Warning => report.warn(format!("grid world: {}", issue.describe())),
        }
    }
    if limits::has_errors(&issues) {
        return false;
    }
    if let Err(error) = RuntimeUnits::new(world.scale) {
        report.error(format!("grid world: {error}"));
        return false;
    }
    true
}

/// Point lights of the scene in the scaled (runtime) project.
fn scene_lights(scene: &Scene) -> Vec<GridLight> {
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .map(f64::from)
        .unwrap_or(DEFAULT_LIGHT_RADIUS_UNITS);
    scene
        .nodes()
        .iter()
        .filter_map(|node| {
            let NodeKind::PointLight {
                color,
                intensity,
                radius,
            } = &node.kind
            else {
                return None;
            };
            Some(GridLight {
                position: node.transform.translation.map(f64::from),
                radius: f64::from(*radius) * radius_units,
                intensity_q8: (f64::from(*intensity) * 256.0)
                    .round()
                    .clamp(0.0, f64::from(u16::MAX)) as u16,
                color: *color,
            })
        })
        .filter(GridLight::is_valid)
        .collect()
}

/// The 8 by 8 white texture a face without a material is drawn with.
fn flat_white_psxt() -> Vec<u8> {
    psxed_tex::encode_indexed_psxt(
        8,
        8,
        psxed_format::texture::Depth::Bit4,
        &[1; 64],
        &[[0, 0, 0], [255, 255, 255]],
        false,
    )
    .expect("fixed fallback texture is valid")
}

/// Binds document materials to texture assets in the package.
struct Binder<'a> {
    project: &'a ProjectDocument,
    root: &'a Path,
    assets: &'a mut Vec<PlaytestAsset>,
    by_path: &'a mut HashMap<String, usize>,
    /// Asset indices the level's materials use, in first-use order.
    used: Vec<usize>,
    warnings: Vec<String>,
}

impl MaterialBinder for Binder<'_> {
    fn see_through(&self, material: Option<ResourceId>) -> bool {
        let Some(id) = material else {
            return false;
        };
        let Some(ResourceData::Material(data)) = self.project.resource(id).map(|r| &r.data) else {
            return false;
        };
        if data.blend_mode != PsxBlendMode::Opaque {
            return true;
        }
        if data.sky_aperture {
            return false;
        }
        crate::resolve_material_texture_psxt(self.project, id, self.root)
            .ok()
            .flatten()
            .is_some_and(|(_, bytes)| {
                Texture::from_bytes(&bytes).is_ok_and(|t| t.is_index_zero_transparent())
            })
    }

    fn uv_default(&self, material: ResourceId) -> Option<crate::face_uv::UvMode> {
        crate::grid::UvDefaults::default_uv(
            &crate::grid::uv::ProjectUvDefaults(self.project),
            material,
        )
    }

    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        let mut tint = [128; 3];
        let mut blend = PsxBlendMode::Opaque;
        let mut sided = MaterialFaceSidedness::Front;
        let mut textured = false;
        let mut key = "@flat-white".to_string();
        let mut bytes = flat_white_psxt();
        if let Some(id) = material {
            let resource = self
                .project
                .resource(id)
                .ok_or_else(|| format!("material #{} does not exist", id.raw()))?;
            let ResourceData::Material(data) = &resource.data else {
                return Err(format!("resource #{} is not a material", id.raw()));
            };
            tint = data.tint;
            blend = data.blend_mode;
            sided = data.sidedness();
            if data.animation.mode != MaterialAnimationMode::Static {
                self.warnings.push(format!(
                    "material {:?} is animated; the grid renderer draws it still",
                    resource.name
                ));
            }
            if !data.sky_aperture {
                if let Some((found_key, found)) =
                    crate::resolve_material_texture_psxt(self.project, id, self.root)
                        .map_err(|e| format!("material {:?}: {e}", resource.name))?
                {
                    key = found_key;
                    bytes = found;
                    textured = true;
                }
            }
        }
        let texture =
            Texture::from_bytes(&bytes).map_err(|e| format!("invalid PSXT {key}: {e:?}"))?;
        let (width, height) = (texture.width(), texture.height());
        let valid = (8..=128).contains(&width)
            && (8..=128).contains(&height)
            && width.is_power_of_two()
            && height.is_power_of_two();
        if texture.depth() != psxed_format::texture::Depth::Bit4 || !valid {
            return Err(format!(
                "texture {key} must be 4bpp power-of-two 8..128, got {width}x{height}"
            ));
        }
        if textured && !key.contains("#page") && (width, height) != (TILE_TEXELS, TILE_TEXELS) {
            let note = format!(
                "texture {key} is {width}x{height}; the grid renderer tiles 64x64 textures"
            );
            if !self.warnings.contains(&note) {
                self.warnings.push(note);
            }
        }
        let cutout = texture.is_index_zero_transparent();
        let index = match self.by_path.get(&key) {
            Some(&existing) => existing,
            None => {
                let index = self.assets.len();
                self.assets.push(PlaytestAsset {
                    kind: PlaytestAssetKind::Texture,
                    bytes,
                    filename: format!("texture_{index:03}.psxt"),
                    source_label: key.clone(),
                    streamed_class: StreamedClass::None,
                });
                self.by_path.insert(key, index);
                index
            }
        };
        if !self.used.contains(&index) {
            self.used.push(index);
        }
        let asset =
            u16::try_from(index).map_err(|_| "texture asset index exceeds u16".to_string())?;
        Ok(BoundMaterial {
            asset,
            flags: match sided {
                MaterialFaceSidedness::Front => 0,
                MaterialFaceSidedness::Back => 1,
                MaterialFaceSidedness::Both => 2,
            } | if cutout { MATERIAL_CUTOUT } else { 0 },
            blend: match blend {
                PsxBlendMode::Opaque => 0,
                PsxBlendMode::Average => 1,
                PsxBlendMode::Add => 2,
                PsxBlendMode::Subtract => 3,
                PsxBlendMode::AddQuarter => 4,
            },
            uv_motion: 0,
            tint,
            textured,
            texels: [width, height],
        })
    }
}

/// Distance LOD for far floors, off by default: `PSXED_GRID_LOD_FAR=<sectors>`
/// turns the merged far quads on beyond that distance (`cook::lod::LOD_FAR_CELLS`
/// is the measured setting). Merged quads cut the Cathedral route's world cost
/// at the tail (p99 and worst frame) but leave one-pixel gaps along some merged
/// edges, so they wait for a crack-free stitch before becoming the default.
fn lod_far_cells() -> u8 {
    std::env::var("PSXED_GRID_LOD_FAR")
        .ok()
        .and_then(|v| v.trim().parse::<u8>().ok())
        .unwrap_or(0)
}

/// Cook the active scene's grid world into a package record, registering the
/// textures it uses in `assets`. `project` is the scaled project (lights and
/// nodes at runtime scale); the document is read as authored.
pub(super) fn cook(
    project: &ProjectDocument,
    world: &GridWorld,
    project_root: &Path,
    assets: &mut Vec<PlaytestAsset>,
    texture_asset_for_path: &mut HashMap<String, usize>,
    report: &mut PlaytestValidationReport,
) -> Option<PlaytestGridWorld> {
    let lights = scene_lights(project.active_scene());
    let mut binder = Binder {
        project,
        root: project_root,
        assets,
        by_path: texture_asset_for_path,
        used: Vec::new(),
        warnings: Vec::new(),
    };
    let cooked = cook::cook_level(
        CookInput {
            world,
            binder: &mut binder,
            lights: &lights,
            first_pack_sector: 0,
            lod_far_cells: lod_far_cells(),
            texture_repeat: cook::texture_repeat_default(),
        },
        None,
    );
    let used = std::mem::take(&mut binder.used);
    for warning in std::mem::take(&mut binder.warnings) {
        report.warn(format!("grid world: {warning}"));
    }
    let cooked = match cooked {
        Ok(cooked) => cooked,
        Err(error) => {
            report.error(format!("grid world: {error}"));
            return None;
        }
    };
    for warning in &cooked.report.warnings {
        report.warn(warning.clone());
    }
    let mut rooms = Vec::new();
    for chunk in &cooked.chunks {
        rooms.extend_from_slice(chunk);
        while !rooms.len().is_multiple_of(SECTOR_BYTES) {
            rooms.push(0);
        }
    }
    // The pool the guest reserves for the rooms streamed off the disc.
    let chunk_pages: Vec<usize> = cooked
        .chunks
        .iter()
        .map(|chunk| psx_grid::stream::pages_of(chunk.len()))
        .collect();
    let core_words = psx_grid::write::aligned_copy(&cooked.core);
    let pool = match psx_grid::LevelView::parse(psx_grid::write::words_as_bytes(
        &core_words,
        cooked.core.len(),
    )) {
        Ok(level) => psx_grid::stream::plan_pool(&level, &chunk_pages),
        Err(error) => {
            report.error(format!("grid world: the cooked core does not parse: {error:?}"));
            return None;
        }
    };
    let summary = cooked.report.summary();
    report.warn(summary.trim_end().to_string());
    let surfaces = |room: &cook::RoomReport| room.quads + room.triangles;
    Some(PlaytestGridWorld {
        core: cooked.core,
        rooms,
        room_chunks: cooked.chunks,
        pool,
        texture_asset_indices: used,
        faces: cooked.report.rooms.iter().map(surfaces).sum(),
        triangles: cooked
            .report
            .rooms
            .iter()
            .map(|room| room.quads * 2 + room.triangles)
            .sum(),
        max_room_surfaces: cooked.report.rooms.iter().map(surfaces).max().unwrap_or(0),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::starter;

    /// A starter project made beside its base so shared asset paths resolve.
    fn starter_project(tag: &str) -> (std::path::PathBuf, ProjectDocument) {
        let projects = crate::projects_dir();
        let out = projects.join(format!("test-starter-{tag}-{}", std::process::id()));
        let path = starter::create_project(&projects.join(starter::STARTER_BASE), &out, "Test")
            .expect("the starter project is created");
        (
            out,
            ProjectDocument::load_from_path(path).expect("the starter loads"),
        )
    }

    #[test]
    fn the_starter_room_cooks_for_play_as_a_grid_level() {
        let (dir, project) = starter_project("cook");
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let grid = &package.world_geometry;
        assert!(grid.core.starts_with(b"PGL1"));
        assert!(grid.rooms.starts_with(b"PGRM"));
        assert_eq!(grid.rooms.len() % SECTOR_BYTES, 0);
        assert!(grid.faces > 0 && grid.max_room_surfaces <= grid.faces);
    }

    #[test]
    fn a_grid_world_that_breaks_the_caps_fails_with_a_readable_reason() {
        let (dir, mut project) = starter_project("caps");
        if let Some(world) = project.active_scene_mut().world.grid_mut() {
            // A leaf outside the rectangle, which the document cannot hold.
            let far = [world.rooms[0].origin[0] + 100, world.rooms[0].origin[1]];
            world.rooms[0]
                .leaves
                .insert(
                    crate::grid::leaf::LeafKey::sector(far),
                    crate::grid::GridSector::default(),
                )
                .unwrap();
        }
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(package.is_none());
        let message = report.error_messages().join("; ");
        assert!(message.contains("grid world: room 0"), "{message}");
        assert!(message.contains("outside the room rectangle"), "{message}");
    }
}
