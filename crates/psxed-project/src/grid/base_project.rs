//! The base project of a new grid project.
//!
//! A starter is an existing project (Graybox Reach by default) minus its
//! level: the base's world nodes, lights, props and pickups go; the player
//! stays and is moved to the new world's spawn. The base's `assets` folder is
//! linked, so the player, camera, sky and UI come along.

use std::path::{Path, PathBuf};

use crate::{NodeId, NodeKind, ProjectDocument, Scene};

use super::units::SECTOR_UNITS;
use super::world::GridWorld;

/// Height above the floor the spawn is placed at, editor units, so the
/// character never starts inside the floor.
const SPAWN_LIFT: i32 = 64;

/// Where to put the spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    /// Room index.
    pub room: usize,
    /// Local sector `[x, z]`.
    pub cell: [usize; 2],
}

fn entity_role(scene: &Scene, id: NodeId) -> Option<bool> {
    let node = scene.node(id)?;
    if !matches!(node.kind, NodeKind::Entity) {
        return None;
    }
    node.children
        .iter()
        .find_map(|child| match &scene.node(*child)?.kind {
            NodeKind::CharacterController { player, .. } => Some(*player),
            _ => None,
        })
}

/// The base project minus its level: world, lights, props and pickups go;
/// the player and the first `enemies` enemies stay so frame costs match a
/// real playthrough. The once-per-launch world message is dropped.
pub(super) fn strip_base(project: &mut ProjectDocument, enemies: usize) {
    let scene = project.active_scene_mut();
    let top: Vec<NodeId> = scene
        .node(NodeId::ROOT)
        .map(|root| root.children.clone())
        .unwrap_or_default();
    let mut kept_enemies = 0;
    for id in top {
        let keep = match entity_role(scene, id) {
            Some(true) => true,
            Some(false) if kept_enemies < enemies => {
                kept_enemies += 1;
                true
            }
            _ => false,
        };
        if !keep {
            scene.remove_node(id);
        }
    }
    if let Some(root) = scene.node_mut(NodeId::ROOT) {
        if let NodeKind::World { world_message, .. } = &mut root.kind {
            *world_message = None;
        }
    }
}

/// Highest floor corner of a sector, grid units.
fn floor_top(world: &GridWorld, room: usize, cell: [usize; 2]) -> Option<i32> {
    let floor = world
        .rooms
        .get(room)?
        .sector(cell[0], cell[1])?
        .floor
        .as_ref()?;
    floor.heights.iter().copied().max()
}

/// Editor-space spawn point for a sector: its centre, just above the
/// highest floor corner.
pub(super) fn spawn_point(world: &GridWorld, spawn: SpawnRequest) -> Option<[i32; 3]> {
    let room = world.rooms.get(spawn.room)?;
    let top = floor_top(world, spawn.room, spawn.cell)?;
    let half = SECTOR_UNITS / 2;
    Some([
        world
            .scale
            .to_editor((room.origin[0] + spawn.cell[0] as i32) * SECTOR_UNITS + half),
        world.scale.to_editor(room.y_offset + top) + SPAWN_LIFT,
        world
            .scale
            .to_editor((room.origin[1] + spawn.cell[1] as i32) * SECTOR_UNITS + half),
    ])
}

/// How far above the spawn an actor may still be dropped onto a floor.
const ACTOR_FLOOR_REACH: i32 = 512;

pub(super) fn place_actors(
    project: &mut ProjectDocument,
    world: &GridWorld,
    at: [i32; 3],
    yaw: f32,
    step: i32,
) {
    let scene = project.active_scene_mut();
    let mut actors: Vec<(bool, NodeId)> = scene
        .node(NodeId::ROOT)
        .map(|root| root.children.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| entity_role(scene, id).map(|player| (!player, id)))
        .collect();
    actors.sort_by_key(|(enemy, _)| *enemy);
    if !actors.iter().any(|(enemy, _)| !enemy) {
        // The base had no player: fall back to a plain spawn point.
        let id = scene.add_node(
            NodeId::ROOT,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        actors.insert(0, (false, id));
    }
    let (forward_x, forward_z) = (yaw.to_radians().sin(), yaw.to_radians().cos());
    for (index, (_, id)) in actors.iter().enumerate() {
        let ahead = index as f32 * 4.0 * step as f32;
        let side = if index == 2 { -1.5 } else { 1.5 }
            * step as f32
            * (index.min(1) as f32 + (index / 2) as f32);
        // Stand each actor on a floor: if the spot has none (past the edge of
        // the level), pull it back towards the spawn until it does.
        let mut pull = 1.0f32;
        let (x, y, z) = loop {
            let x = at[0] + ((forward_x * ahead + forward_z * side) * pull) as i32;
            let z = at[2] + ((forward_z * ahead - forward_x * side) * pull) as i32;
            match world.floor_height_at(x, z, at[1] + ACTOR_FLOOR_REACH) {
                Some(floor) => break (x, floor + SPAWN_LIFT, z),
                None if pull > 0.1 => pull *= 0.5,
                None => break (at[0], at[1], at[2]),
            }
        };
        let position = [x, y, z];
        if let Some(node) = scene.node_mut(*id) {
            node.transform.translation = position.map(|v| v as f32);
            node.transform.rotation_degrees = [0.0, yaw, 0.0];
        }
    }
}

#[cfg(unix)]
pub(super) fn link_assets(base: &Path, out: &Path) -> std::io::Result<()> {
    let link = out.join("assets");
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)?;
    }
    // A sibling base links relatively so the pair can move together.
    let base_real = std::fs::canonicalize(base)?;
    let out_parent = std::fs::canonicalize(out.parent().unwrap_or(out))?;
    let target = if base_real.parent() == Some(out_parent.as_path()) {
        PathBuf::from("..")
            .join(base_real.file_name().unwrap_or_default())
            .join("assets")
    } else {
        base_real.join("assets")
    };
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
pub(super) fn link_assets(base: &Path, out: &Path) -> std::io::Result<()> {
    // No symlinks here: copy the shared assets instead.
    let _ = (base, out);
    Err(std::io::Error::other(
        "linking the base assets needs a unix host",
    ))
}
