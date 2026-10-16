//! Cooking a grid world for Play.
//!
//! A grid project's world is its grid document and nothing else (plan
//! decision D2). Play derives everything the runtime needs from it:
//!
//! * [`prepare`] validates the document against the shared caps, then adds
//!   the thin collision brushes (and the sky lid) the PXBSP cook turns into
//!   collision, activation and camera geometry.
//! * [`cook_pack`] runs after the PXBSP cook and writes the render pack the
//!   grid renderer draws, with per-corner light baked against the same
//!   lights and the derived brushes as occluders.
//!
//! Brush projects pass through both untouched.

use crate::brush_grid::{self, collision, GridCookInput, GridSource};
use crate::grid::limits::{self, Severity};
use crate::{ProjectDocument, ResourceData};

use super::{PlaytestValidationReport, PlaytestWorldRender};

/// Texel size of the tile the grid renderer maps one material onto.
const TILE_TEXELS: u16 = 64;

/// Validate a grid scene and add its derived collision to `project`, which
/// must still be at authored (editor) scale. Returns `false` when the world
/// cannot be cooked; the reasons are in `report`. Brush scenes return `true`
/// and are left alone.
pub(super) fn prepare(
    project: &mut ProjectDocument,
    report: &mut PlaytestValidationReport,
) -> bool {
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
    if let Err(error) = brush_grid::document::check_lattice(world) {
        report.error(format!("grid world: {error}"));
        return false;
    }
    let sky = project.resources.iter().find_map(|resource| {
        matches!(&resource.data, ResourceData::Material(m) if m.sky_aperture).then_some(resource.id)
    });
    let derived = collision::derive(world, sky);
    if derived.brushes.is_empty() {
        report.error(
            "grid world: nothing to stand on or bump into; add a floor or a wall with collision on",
        );
        return false;
    }
    if derived.rejected > 0 {
        report.warn(format!(
            "grid world: {} faces gave no valid collision slab and are walk-through",
            derived.rejected
        ));
    }
    if derived.passable > 0 {
        report.warn(format!(
            "grid world: {} faces have collision turned off and are walk-through",
            derived.passable
        ));
    }
    let scene = project.active_scene_mut();
    scene.brushes.extend(derived.brushes);
    scene.brushes.extend(derived.lid);
    true
}

/// Cook the render pack for the active scene. Grid scenes cook their
/// document; brush scenes get an empty pack unless the open-hall bench
/// variable `OPEN_HALL_GRID_LEVEL` names a raw bonnie-32 level.
pub(super) fn cook_pack(
    project: &ProjectDocument,
    compiled: &crate::brush_world::CompiledBrushWorld,
    ambient: [u8; 3],
    report: &mut PlaytestValidationReport,
) -> (PlaytestWorldRender, Vec<u8>) {
    let scene = project.active_scene();
    let raw = std::env::var_os("OPEN_HALL_GRID_LEVEL");
    let (source, ambient) = match (scene.world.grid(), raw.as_deref()) {
        (Some(world), _) => {
            let ambient = world.rooms.first().map_or(ambient, |room| room.ambient);
            warn_tile_sizes(compiled, report);
            (GridSource::Document(world), ambient)
        }
        (None, Some(path)) => (GridSource::RawLevel(std::path::Path::new(path)), ambient),
        (None, None) => return (PlaytestWorldRender::Pxbsp, Vec::new()),
    };
    let (lights, _) = crate::brush_world::scene_lights(scene);
    let tints = crate::brush_world::material_tints(project);
    let texture_assets: Vec<(String, u16)> = compiled
        .textures
        .iter()
        .map(|texture| (texture.key.clone(), texture.asset_id))
        .collect();
    let input = GridCookInput {
        project,
        source,
        ambient,
        lights: &lights,
        tints: &tints,
        texture_assets: &texture_assets,
    };
    match brush_grid::cook_grid_pack(&input) {
        Ok((bytes, stats)) => {
            report.warn(stats.summary());
            if stats.dropped_unmapped > 0 {
                report.warn(format!(
                    "grid world: {} faces have no material the cook could bind and are not drawn",
                    stats.dropped_unmapped
                ));
            }
            (PlaytestWorldRender::Grid, bytes)
        }
        Err(error) => {
            report.error(format!("grid pack cook failed: {error}"));
            (PlaytestWorldRender::Pxbsp, Vec::new())
        }
    }
}

/// The grid renderer maps a material onto a 64 by 64 texel tile; any other
/// texture size would draw stretched or cropped, so say so.
fn warn_tile_sizes(
    compiled: &crate::brush_world::CompiledBrushWorld,
    report: &mut PlaytestValidationReport,
) {
    for texture in &compiled.textures {
        if texture.key.starts_with('@') {
            continue;
        }
        if texture.size != [TILE_TEXELS, TILE_TEXELS] {
            report.warn(format!(
                "grid world: texture {} is {}x{}; the grid renderer tiles 64x64 textures",
                texture.key, texture.size[0], texture.size[1]
            ));
        }
    }
}
