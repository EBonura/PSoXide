//! The starter room of File > New Grid Project.
//!
//! A new grid project is the Graybox Reach project minus its level (the same
//! base the bonnie importer uses, so it carries the Cortex player, camera,
//! sky and UI) with a small walled room as the world: a floor, a ceiling,
//! walls on every edge of the outline, one point light, and the player
//! standing in the middle. It Plays as soon as it is created.

use std::path::{Path, PathBuf};

use crate::{NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId};

use super::bonnie_project::{link_assets, place_actors, spawn_point, strip_base, SpawnRequest};
use super::room::GridRoom;
use super::sector::WallEdge;
use super::surface::{GridSurface, GridWall};
use super::units::{GridScale, SECTOR_UNITS};
use super::world::{GridWorld, SceneWorld};

/// Sectors along each side of the starter room.
pub const STARTER_SIDE: usize = 6;
/// Height of the starter ceiling above its floor, grid units.
pub const STARTER_HEIGHT: i32 = 2 * SECTOR_UNITS;
/// Grid to editor scale of a new grid project: 5/4, a sector of 1280 editor
/// units (the design docs' default for new worlds; `GridScale::DEFAULT`, which
/// imports and empty worlds still use, moves with the next phase).
pub const STARTER_SCALE: GridScale = GridScale { num: 5, den: 4 };
/// The base project the starter borrows its player, camera and sky from.
pub const STARTER_BASE: &str = "graybox-reach";
/// Light intensity and radius (in World sectors) of the starter lamp.
const LAMP_INTENSITY: f32 = 1.4;
const LAMP_RADIUS: f32 = 5.0;

/// Why the starter could not be made.
#[derive(Debug)]
pub enum StarterError {
    /// The base project is missing or does not load.
    Base(String),
    /// A file operation failed.
    Io(String),
}

impl core::fmt::Display for StarterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Base(e) => write!(f, "base project {STARTER_BASE}: {e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StarterError {}

fn find_material(project: &ProjectDocument, names: &[&str]) -> Option<ResourceId> {
    names.iter().find_map(|name| {
        project
            .resources
            .iter()
            .find(|r| r.name == *name && matches!(r.data, ResourceData::Material(_)))
            .map(|r| r.id)
    })
}

/// The first material that is neither a sky nor a UI image, as a last resort.
fn any_world_material(project: &ProjectDocument) -> Option<ResourceId> {
    project.resources.iter().find_map(|r| match &r.data {
        ResourceData::Material(m) if super::palette::is_world_material(&r.name, m) => Some(r.id),
        _ => None,
    })
}

/// The starter room: floor, ceiling and a wall on every outline edge.
pub fn starter_world(
    floor: Option<ResourceId>,
    wall: Option<ResourceId>,
    ceiling: Option<ResourceId>,
) -> GridWorld {
    let side = STARTER_SIDE;
    let mut room = GridRoom::new("Starter room", side, side);
    for z in 0..side {
        for x in 0..side {
            let Some(sector) = room.sector_or_insert(x, z) else {
                continue;
            };
            sector.floor = Some(GridSurface::flat(0, floor));
            sector.ceiling = Some(GridSurface::flat(STARTER_HEIGHT, ceiling));
            let outline = [
                (z == 0, WallEdge::North),
                (x == side - 1, WallEdge::East),
                (z == side - 1, WallEdge::South),
                (x == 0, WallEdge::West),
            ];
            for (on_edge, edge) in outline {
                if on_edge {
                    sector
                        .walls_mut(edge)
                        .push(GridWall::span(0, STARTER_HEIGHT, wall));
                }
            }
        }
    }
    GridWorld {
        scale: STARTER_SCALE,
        rooms: vec![room],
    }
}

/// Create the starter project at `out_dir` from the base project in
/// `base_dir`, named `name`. Returns the written `project.ron`.
pub fn create_project(
    base_dir: &Path,
    out_dir: &Path,
    name: &str,
) -> Result<PathBuf, StarterError> {
    let mut project = ProjectDocument::load_from_path(base_dir.join("project.ron"))
        .map_err(|e| StarterError::Base(e.to_string()))?;
    strip_base(&mut project, 0);
    project.name = name.to_string();
    std::fs::create_dir_all(out_dir)
        .map_err(|e| StarterError::Io(format!("create {}: {e}", out_dir.display())))?;
    link_assets(base_dir, out_dir)
        .map_err(|e| StarterError::Io(format!("link the base assets: {e}")))?;

    let ground =
        find_material(&project, &["Graybox / Ground 64"]).or_else(|| any_world_material(&project));
    let walls = find_material(&project, &["Graybox / Original 64"]).or(ground);
    let roof = find_material(&project, &["Graybox / Cliff 64"]).or(ground);
    let world = starter_world(ground, walls, roof);

    let centre = SpawnRequest {
        room: 0,
        cell: [STARTER_SIDE / 2, STARTER_SIDE / 2],
    };
    let at = spawn_point(&world, centre)
        .ok_or_else(|| StarterError::Io("the starter room has no floor".to_string()))?;
    let step = world.scale.sector_editor_units();
    place_actors(&mut project, &world, at, 0.0, step);

    // One lamp above the player, scaled to the World node's sector size.
    let scene = project.active_scene_mut();
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(crate::DEFAULT_WORLD_SECTOR_SIZE)
        .max(1) as f32;
    let lamp = scene.add_node(
        NodeId::ROOT,
        "Starter Light",
        NodeKind::PointLight {
            color: [255, 214, 160],
            intensity: LAMP_INTENSITY,
            radius: LAMP_RADIUS * world.scale.sector_editor_units() as f32 / unit,
        },
    );
    if let Some(node) = scene.node_mut(lamp) {
        let height = world.scale.to_editor(STARTER_HEIGHT * 3 / 4);
        node.transform.translation = [at[0] as f32, height as f32, at[2] as f32];
    }
    scene.world = SceneWorld::Grid(world);
    project.editor_camera = crate::EditorCameraState::default();
    project.editor_viewport = crate::EditorViewportState::default();

    let path = out_dir.join("project.ron");
    project
        .save_to_path(&path)
        .map_err(|e| StarterError::Io(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::faces::all_faces;
    use crate::grid::limits;

    #[test]
    fn the_starter_room_is_valid_and_closed() {
        let world = starter_world(None, None, None);
        assert!(!limits::has_errors(&limits::validate(&world)));
        let room = &world.rooms[0];
        assert_eq!(room.sector_count(), STARTER_SIDE * STARTER_SIDE);
        // Every outline edge has a wall: 4 sides of STARTER_SIDE sectors.
        let walls: usize = room.iter().map(|(_, _, s)| s.wall_count()).sum();
        assert_eq!(walls, 4 * STARTER_SIDE);
        // A floor and a ceiling per sector, one band per wall (2 sectors
        // tall gives 2 bands).
        assert_eq!(
            all_faces(&world).len(),
            2 * STARTER_SIDE * STARTER_SIDE + 4 * STARTER_SIDE * 2
        );
    }
}
