//! Cooking a grid world for Play.
//!
//! TRANSITIONAL: this and `brush_grid::collision` bridge the grid document to
//! the PXBSP cook (collision, activation, camera) until grid worlds have their
//! own level format. Nothing else should build on that path.
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
    // The grid renderer has no portal visibility, so the PXBSP only serves
    // collision and activation: the cheap connected-region visibility is
    // enough. (The portal flow over a level's worth of thin slabs ran for
    // more than a quarter of an hour on one of the imported levels.)
    project.bsp_cook_mode = crate::brush_world::BrushWorldCookMode::Draft;
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
        // `@` textures are the cook's built-ins; `#page` ones were tiled up by
        // the cook from a smaller source and still show their first 64x64.
        if texture.key.starts_with('@') || texture.key.contains("#page") {
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
    fn the_starter_room_cooks_for_play_with_a_grid_pack() {
        let (dir, project) = starter_project("cook");
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let world = &package.world_geometry;
        assert_eq!(world.render, PlaytestWorldRender::Grid);
        // The reader needs a word-aligned buffer (the guest image provides
        // one); check the header the cook wrote instead.
        let pack = &world.grid_pack;
        assert!(pack.starts_with(b"GRD1"));
        let word = |i: usize| u32::from_le_bytes(pack[i * 4..i * 4 + 4].try_into().unwrap());
        let side = crate::grid::starter::STARTER_SIDE as u32;
        assert_eq!(word(2), side | (side << 16), "cell grid");
        // 36 floor and 36 ceiling tiles, 24 wall segments of two bands.
        assert_eq!(word(7), 2 * side * side + 4 * side * 2, "quads");
    }

    #[test]
    fn a_grid_world_that_breaks_the_caps_fails_with_a_readable_reason() {
        let (dir, mut project) = starter_project("caps");
        if let Some(world) = project.active_scene_mut().world.grid_mut() {
            world.rooms[0].sectors.truncate(3);
        }
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(package.is_none());
        let message = report.error_messages().join("; ");
        assert!(message.contains("grid world: room 0"), "{message}");
        assert!(message.contains("sector slots stored"), "{message}");
    }

    #[test]
    fn a_world_with_no_collision_is_refused() {
        let mut project = ProjectDocument::new("t");
        let mut world = crate::grid::GridWorld::new();
        let mut room = crate::grid::GridRoom::new("r", 1, 1);
        let mut floor = crate::grid::GridSurface::flat(0, None);
        floor.walkable = false;
        room.sector_or_insert(0, 0).unwrap().floor = Some(floor);
        world.rooms.push(room);
        project.active_scene_mut().world = crate::grid::SceneWorld::Grid(world);
        let mut report = PlaytestValidationReport::default();
        assert!(!prepare(&mut project, &mut report));
        assert!(report.error_messages()[0].contains("nothing to stand on"));
    }
}
