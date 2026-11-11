//! Move: translate a selection of cells by whole sectors in X and Z and by
//! lattice steps in Y, carrying everything that stands on them.
//!
//! What goes with a cell: its leaf (every floor, ceiling and authored wall,
//! shifted by the Y step), the derived-wall overrides on the edges it owns,
//! the meshes and planes whose anchor lies in it, and, when a scene is
//! given, every top level node (lights, entities, props, logic slabs)
//! standing in it. Nodes that hang under another node move with their
//! parent. Nothing is half done: a refusal leaves both documents untouched,
//! and a success is one logical step (the caller snapshots once).

use std::collections::BTreeSet;

use crate::grid::leaf::{LeafKey, CLICKS_PER_SECTOR};
use crate::grid::overrides::EdgeKey;
use crate::grid::tree::normalize;
use crate::grid::units::{FINE_UNITS, SECTOR_UNITS};
use crate::grid::world::GridWorld;
use crate::{NodeId, Scene};

use super::rects::{grow_room_to, CellRect};
use super::verbs::capture::shift;

/// Parameters of a move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveParams {
    /// The room that owns the cells.
    pub room: usize,
    /// World sectors to move.
    pub cells: BTreeSet<[i32; 2]>,
    /// Whole sectors east (+X) and south (+Z).
    pub delta: [i32; 2],
    /// Lattice steps up (positive) or down; a step is [`FINE_UNITS`].
    pub dy_steps: i32,
    /// Replace the cells already standing where the selection lands.
    pub overwrite: bool,
}

/// Why a move was refused. Nothing changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveRefusal {
    /// No such room.
    NoSuchRoom,
    /// The selection holds no cell.
    Empty,
    /// The move is zero.
    NoChange,
    /// A leaf is partly inside the selection: split it to fit first.
    LeafCrossesEdge(LeafKey),
    /// A coarse leaf would land off its own grid.
    Misaligned(LeafKey),
    /// A cell that stays stands where the selection would land.
    Collision([i32; 2]),
    /// The room would pass its size cap.
    RoomTooLarge,
}

impl MoveRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::Empty => "Nothing selected to move".to_string(),
            Self::NoChange => "Move by at least one sector or one step".to_string(),
            Self::LeafCrossesEdge(_) => {
                "A cell crosses the edge of the selection: split it to fit first".to_string()
            }
            Self::Misaligned(_) => "A large cell cannot move by a single sector".to_string(),
            Self::Collision(c) => {
                format!(
                    "Cell {},{} is in the way (hold Overwrite to replace it)",
                    c[0], c[1]
                )
            }
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
        }
    }
}

/// What a move carried.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoveOutcome {
    /// Leaves moved.
    pub leaves: usize,
    /// Leaves replaced at the destination.
    pub replaced: usize,
    /// Mesh instances carried.
    pub meshes: usize,
    /// Free planes carried.
    pub planes: usize,
    /// Wall overrides carried.
    pub overrides: usize,
    /// Scene nodes carried (top level ones).
    pub nodes: usize,
}

fn cell_of(grid: i32) -> i32 {
    grid.div_euclid(SECTOR_UNITS)
}

/// Top level scene nodes standing in `cells` (editor units, room Y within
/// `y_range` grid units of the world).
fn nodes_in(
    world: &GridWorld,
    scene: &Scene,
    cells: &BTreeSet<[i32; 2]>,
    y_range: (i32, i32),
) -> Vec<NodeId> {
    let sector = world.scale.sector_editor_units().max(1);
    let (y_lo, y_hi) = (
        world.scale.to_editor(y_range.0),
        world.scale.to_editor(y_range.1),
    );
    scene
        .nodes()
        .iter()
        .filter(|n| n.parent == Some(scene.root))
        .filter(|n| {
            let t = n.transform.translation;
            let c = [
                (t[0].floor() as i32).div_euclid(sector),
                (t[2].floor() as i32).div_euclid(sector),
            ];
            let y = t[1].floor() as i32;
            cells.contains(&c) && y >= y_lo && y <= y_hi
        })
        .map(|n| n.id)
        .collect()
}

/// Move the cells of `p` (and what stands on them) in `world` and `scene`.
pub fn move_cells(
    world: &mut GridWorld,
    scene: Option<&mut Scene>,
    p: &MoveParams,
) -> Result<MoveOutcome, MoveRefusal> {
    if p.cells.is_empty() {
        return Err(MoveRefusal::Empty);
    }
    if p.delta == [0, 0] && p.dy_steps == 0 {
        return Err(MoveRefusal::NoChange);
    }
    let source = world.rooms.get(p.room).ok_or(MoveRefusal::NoSuchRoom)?;
    let dy = p.dy_steps * FINE_UNITS;
    let click_delta = [
        p.delta[0] * CLICKS_PER_SECTOR,
        p.delta[1] * CLICKS_PER_SECTOR,
    ];

    // Which leaves go: wholly inside the selection. A leaf that is partly
    // inside stops the move.
    let mut moving: Vec<LeafKey> = Vec::new();
    for (key, _) in source.leaves() {
        let (lo, hi) = key.sector_range();
        let all = CellRect { min: lo, max: hi }
            .cells()
            .all(|c| p.cells.contains(&c));
        let any = CellRect { min: lo, max: hi }
            .cells()
            .any(|c| p.cells.contains(&c));
        if all {
            moving.push(key);
        } else if any {
            return Err(MoveRefusal::LeafCrossesEdge(key));
        }
    }
    if moving.is_empty() {
        return Err(MoveRefusal::Empty);
    }
    let mut landing: Vec<(LeafKey, LeafKey)> = Vec::new();
    for key in &moving {
        let to = LeafKey::new(key.level, key.x + click_delta[0], key.z + click_delta[1])
            .ok_or(MoveRefusal::Misaligned(*key))?;
        landing.push((*key, to));
    }
    let mut y_lo = i32::MAX;
    let mut y_hi = i32::MIN;
    for key in &moving {
        if let Some((lo, hi)) = source.leaf(*key).and_then(|s| s.height_range()) {
            y_lo = y_lo.min(lo);
            y_hi = y_hi.max(hi);
        }
    }
    let y_offset = source.y_offset;
    let y_range = if y_lo > y_hi {
        (y_offset - 1024, y_offset + 1024)
    } else {
        (y_offset + y_lo - 1024, y_offset + y_hi + 1024)
    };

    let mut work = world.clone();
    let mut out = MoveOutcome::default();
    {
        let r = &mut work.rooms[p.room];
        // Take the leaves out, then check and clear the landing.
        let mut taken: Vec<(LeafKey, LeafKey, crate::grid::sector::GridSector)> = Vec::new();
        for (from, to) in &landing {
            if let Some(s) = r.remove_leaf(*from) {
                taken.push((*from, *to, s));
            }
        }
        let landing_cells: BTreeSet<[i32; 2]> = p
            .cells
            .iter()
            .map(|c| [c[0] + p.delta[0], c[1] + p.delta[1]])
            .collect();
        let doomed: Vec<LeafKey> = r
            .leaves()
            .map(|(k, _)| k)
            .filter(|k| {
                let (lo, hi) = k.sector_range();
                CellRect { min: lo, max: hi }
                    .cells()
                    .any(|c| landing_cells.contains(&c))
            })
            .collect();
        if let Some(k) = doomed.first() {
            if !p.overwrite {
                let (lo, hi) = k.sector_range();
                let hit = CellRect { min: lo, max: hi }
                    .cells()
                    .find(|c| landing_cells.contains(c))
                    .unwrap_or(lo);
                return Err(MoveRefusal::Collision(hit));
            }
        }
        for k in &doomed {
            r.remove_leaf(*k);
            out.replaced += 1;
        }
        // The room grows to hold the landing.
        let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
        for c in &landing_cells {
            lo = [lo[0].min(c[0]), lo[1].min(c[1])];
            hi = [hi[0].max(c[0]), hi[1].max(c[1])];
        }
        grow_room_to(r, CellRect { min: lo, max: hi }).map_err(|_| MoveRefusal::RoomTooLarge)?;
        for (_, to, mut s) in taken {
            shift(&mut s, dy);
            r.insert_leaf(to, s)
                .map_err(|_| MoveRefusal::RoomTooLarge)?;
            out.leaves += 1;
        }
        // Meshes and planes anchored in the selection.
        for m in &mut r.meshes {
            if p.cells
                .contains(&[cell_of(m.position[0]), cell_of(m.position[2])])
            {
                m.position[0] += p.delta[0] * SECTOR_UNITS;
                m.position[1] += dy;
                m.position[2] += p.delta[1] * SECTOR_UNITS;
                out.meshes += 1;
            }
        }
        for pl in &mut r.planes {
            if p.cells
                .contains(&[cell_of(pl.center[0]), cell_of(pl.center[2])])
            {
                pl.center[0] += p.delta[0] * SECTOR_UNITS;
                pl.center[1] += dy;
                pl.center[2] += p.delta[1] * SECTOR_UNITS;
                out.planes += 1;
            }
        }
        // Wall overrides on the edges the selection owns: an edge belongs to
        // the cell its start click lies in, and to the cell before it.
        let owned = |k: &EdgeKey| -> bool {
            let c = [
                k.x.div_euclid(CLICKS_PER_SECTOR),
                k.z.div_euclid(CLICKS_PER_SECTOR),
            ];
            p.cells.contains(&c)
        };
        let moved: Vec<(EdgeKey, crate::grid::overrides::EdgeOverride)> = r
            .edge_overrides
            .iter()
            .filter(|(k, _)| owned(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (k, _) in &moved {
            r.edge_overrides.remove(k);
        }
        for (k, v) in moved {
            let key = EdgeKey {
                x: k.x + click_delta[0],
                z: k.z + click_delta[1],
                ..k
            };
            r.edge_overrides.edit(key, |e| *e = v);
            out.overrides += 1;
        }
        normalize(r);
    }
    let node_ids = scene
        .as_deref()
        .map(|s| nodes_in(world, s, &p.cells, y_range))
        .unwrap_or_default();
    *world = work;
    if let Some(scene) = scene {
        let to_editor = |gu: i32| world.scale.to_editor(gu) as f32;
        for id in node_ids {
            if let Some(n) = scene.node_mut(id) {
                n.transform.translation[0] += to_editor(p.delta[0] * SECTOR_UNITS);
                n.transform.translation[1] += to_editor(dy);
                n.transform.translation[2] += to_editor(p.delta[1] * SECTOR_UNITS);
                out.nodes += 1;
            }
        }
    }
    Ok(out)
}
