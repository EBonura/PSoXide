//! Ruin: a seeded, re-rollable pass that makes a region (or a whole
//! building) look broken.
//!
//! It is built on the existing [`break_region`] (crumbled lips, cracked and
//! tilted tiles, ragged wall tops, ceiling damage) and adds the structural
//! damage Break does not do: sunk tiles, missing floor bays, missing roof
//! bays, breached wall sections and fallen arches (the arch instance goes,
//! its piers stay as columns, rubble optional). Every decision is a hash of
//! `(seed, operation, cell)`, so the same seed gives the same ruin whatever
//! the visiting order, another seed is a re-roll, and a higher intensity only
//! adds damage. `intensity` 0 changes nothing.
//!
//! The **keep mask** is a set of cells the destructive operations never
//! touch (the critical path). With spawns given, each floor bay and sunk
//! tile is also tested against the reachability flood and put back when it
//! would cut a sector off that a spawn reached before.
//!
//! The editor's Rough (vertex jitter on the click lattice) does not exist in
//! this tree; the pass uses Break's vertex tilts for that look.

use std::collections::BTreeSet;

use crate::grid::derive::DerivedKind;
use crate::grid::leaf_derive::derived_walls_of;
use crate::grid::mesh::{ColumnSpec, GridMesh, MeshAsset, MeshKind, RubbleSpec};
use crate::grid::units::FINE_UNITS;
use crate::grid::walk::{analyse, Spawn};
use crate::grid::world::GridWorld;
use psx_engine::WalkLimits;

use super::super::meshes::{add_asset, delete_meshes, place_mesh};
use super::super::region::Region;
use super::break_verb::{break_region, BreakReport, BreakStyle, BreakTargets};
use super::common::{chance, hash};

const OP_SINK: u32 = 101;
const OP_BAY: u32 = 102;
const OP_ROOF: u32 = 103;
const OP_BREACH: u32 = 104;
const OP_ARCH: u32 = 105;

/// Parameters of a ruin.
#[derive(Clone, Debug)]
pub struct RuinParams {
    /// The room.
    pub room: usize,
    /// The sectors it acts on.
    pub cells: BTreeSet<[i32; 2]>,
    /// The seed; another seed is a re-roll.
    pub seed: u32,
    /// 0 to 100.
    pub intensity: u32,
    /// Cells no operation touches (floor, roof, walls, arches): the path.
    pub keep: BTreeSet<[i32; 2]>,
    /// Where the player starts, for the reachability test.
    pub spawns: Vec<Spawn>,
    /// Run Break (lips, cracks, tilts, wall tops, ceiling damage).
    pub crumble: bool,
    /// Lower whole tiles by a few fine steps.
    pub sink: bool,
    /// Drop whole floor tiles into pits of `pit` units.
    pub floor_bays: bool,
    /// Remove ceilings over whole tiles.
    pub roof_bays: bool,
    /// Open wall segments (a ghost wall: no face, no collision).
    pub breaches: bool,
    /// Replace arch instances by their piers.
    pub fallen_arches: bool,
    /// Leave rubble where an arch fell.
    pub rubble: bool,
    /// Depth of a floor bay, grid units.
    pub pit: i32,
}

impl RuinParams {
    /// Every kind of damage at `intensity`, nothing protected.
    pub fn new(room: usize, cells: BTreeSet<[i32; 2]>, seed: u32, intensity: u32) -> Self {
        Self {
            room,
            cells,
            seed,
            intensity,
            keep: BTreeSet::new(),
            spawns: Vec::new(),
            crumble: true,
            sink: true,
            floor_bays: true,
            roof_bays: true,
            breaches: true,
            fallen_arches: true,
            rubble: true,
            pit: 768,
        }
    }
}

/// Why a ruin was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuinRefusal {
    /// The room does not exist.
    NoSuchRoom,
    /// No cell.
    EmptyRegion,
    /// The intensity is over 100.
    BadIntensity,
}

impl RuinRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::EmptyRegion => "Pick the cells to ruin".to_string(),
            Self::BadIntensity => "Intensity is 0 to 100".to_string(),
        }
    }
}

/// What a ruin did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuinReport {
    /// Break's own report, when it ran.
    pub crumble: Option<BreakReport>,
    /// Tiles sunk.
    pub sunk: usize,
    /// Floor bays dropped.
    pub floor_bays: usize,
    /// Roof bays opened.
    pub roof_bays: usize,
    /// Wall segments breached.
    pub breaches: usize,
    /// Arches fallen (their piers stay).
    pub arches: usize,
    /// Rubble instances placed.
    pub rubble: usize,
    /// Operations put back because they cut the route.
    pub reverted: usize,
}

fn unreachable(world: &GridWorld, spawns: &[Spawn]) -> Option<usize> {
    if spawns.is_empty() {
        return None;
    }
    let a = analyse(world, spawns, &WalkLimits::DEFAULT)?;
    a.reach_ran.then(|| a.counts()[4])
}

/// Break the region. One logical edit: the caller snapshots once.
pub fn ruin_region(world: &mut GridWorld, p: &RuinParams) -> Result<RuinReport, RuinRefusal> {
    if p.room >= world.rooms.len() {
        return Err(RuinRefusal::NoSuchRoom);
    }
    if p.cells.is_empty() {
        return Err(RuinRefusal::EmptyRegion);
    }
    if p.intensity > 100 {
        return Err(RuinRefusal::BadIntensity);
    }
    let mut report = RuinReport::default();
    if p.intensity == 0 {
        return Ok(report);
    }
    let i = p.intensity;
    if p.crumble {
        let cells: Vec<[i32; 2]> = p.cells.difference(&p.keep).copied().collect();
        let mut bp = BreakStyle::Cathedral.params();
        bp.targets = BreakTargets {
            walls: true,
            floor: true,
            ceiling: true,
        };
        bp.spawns = p.spawns.clone();
        bp.keep_walkable = true;
        let region = Region::from_cells(p.room, cells);
        report.crumble = break_region(world, &region, bp, i, p.seed);
    }
    let mut baseline = unreachable(world, &p.spawns);
    // Sunk tiles and floor bays, each tested against the route.
    for &cell in &p.cells {
        if p.keep.contains(&cell) {
            continue;
        }
        let drop = if p.sink && chance(p.seed, OP_SINK, cell, (i * 3).min(400)) {
            Some(FINE_UNITS * (1 + (hash(p.seed, OP_SINK, cell, 1) % 3) as i32))
        } else {
            None
        };
        let bay = p.floor_bays && chance(p.seed, OP_BAY, cell, (i * 3 / 2).min(250));
        let Some(depth) = bay.then_some(p.pit).or(drop) else {
            continue;
        };
        let Some(room) = world.rooms.get_mut(p.room) else {
            continue;
        };
        let Some([x, z]) = room.local(cell) else {
            continue;
        };
        let Some(sector) = room.sector_mut(x, z) else {
            continue;
        };
        let before = sector.clone();
        let Some(floor) = sector.ground_floor_mut() else {
            continue;
        };
        for h in floor.heights.iter_mut() {
            *h -= depth;
        }
        if let Some(b) = floor.heights_b.as_mut() {
            for h in b.iter_mut() {
                *h -= depth;
            }
        }
        if let Some(now) = unreachable(world, &p.spawns) {
            if baseline.is_some_and(|b| now > b) {
                if let Some(room) = world.rooms.get_mut(p.room) {
                    room.set_sector(x, z, Some(before));
                }
                report.reverted += 1;
                continue;
            }
            baseline = Some(now);
        }
        if bay {
            report.floor_bays += 1;
        } else {
            report.sunk += 1;
        }
    }
    // Roof bays: the ceiling goes, the sky shows.
    if p.roof_bays {
        for &cell in &p.cells {
            if p.keep.contains(&cell) || !chance(p.seed, OP_ROOF, cell, (i * 2).min(300)) {
                continue;
            }
            let Some(room) = world.rooms.get_mut(p.room) else {
                continue;
            };
            if let Some(s) = room.local(cell).and_then(|[x, z]| room.sector_mut(x, z)) {
                if s.take_ground_ceiling().is_some() {
                    report.roof_bays += 1;
                }
            }
        }
    }
    // Breaches: closure wall segments opened for good.
    if p.breaches {
        let Some(room) = world.rooms.get_mut(p.room) else {
            return Ok(report);
        };
        let keys: Vec<_> = room
            .leaves()
            .map(|(k, _)| k)
            .filter(|k| p.cells.contains(&k.cell()) && !p.keep.contains(&k.cell()))
            .collect();
        for key in keys {
            for w in derived_walls_of(room, key) {
                if w.kind != DerivedKind::Closure || w.hidden {
                    continue;
                }
                let at = [w.key.x, w.key.z];
                if !chance(p.seed, OP_BREACH, at, (i / 4).min(60)) {
                    continue;
                }
                room.edge_overrides.edit(w.key, |o| {
                    o.hidden = true;
                    o.solid = Some(false);
                });
                report.breaches += 1;
            }
        }
    }
    // Fallen arches: the instance goes, the piers stay.
    if p.fallen_arches {
        fall_arches(world, p, &mut report);
    }
    Ok(report)
}

fn fall_arches(world: &mut GridWorld, p: &RuinParams, report: &mut RuinReport) {
    let Some(room) = world.rooms.get(p.room) else {
        return;
    };
    let picks: Vec<GridMesh> = room
        .meshes
        .iter()
        .filter(|m| p.cells.contains(&m.center_cell()) && !p.keep.contains(&m.center_cell()))
        .filter(|m| {
            world
                .mesh_assets
                .get(usize::from(m.asset))
                .is_some_and(|a| matches!(a.kind, MeshKind::Arch(_)))
        })
        .filter(|m| {
            let at = [m.position[0] / 256, m.position[2] / 256];
            chance(p.seed, OP_ARCH, at, (p.intensity * 6).min(700))
        })
        .cloned()
        .collect();
    for m in picks {
        let MeshKind::Arch(spec) = world.mesh_assets[usize::from(m.asset)].kind.clone() else {
            continue;
        };
        let yaw = f64::from(m.rotation[0]).to_radians();
        let dir = [yaw.cos(), -yaw.sin()];
        let off = f64::from(spec.span / 2 + spec.thickness / 2);
        let column = ColumnSpec {
            radius: (spec.thickness / 2).clamp(64, 1024),
            height: spec.spring.max(640),
            sides: 8,
            plinth: 64,
            capital: 64,
        };
        if delete_meshes(world, &[m.id]) == 0 {
            continue;
        }
        let Ok(asset) = add_asset(world, MeshAsset::new("pier", MeshKind::Column(column))) else {
            continue;
        };
        for sign in [-1.0f64, 1.0] {
            let at = [
                m.position[0] + (sign * off * dir[0]).round() as i32,
                m.position[1],
                m.position[2] + (sign * off * dir[1]).round() as i32,
            ];
            let _ = place_mesh(world, p.room, GridMesh::new(0, asset, at));
        }
        report.arches += 1;
        if p.rubble {
            let spec = RubbleSpec {
                seed: (hash(p.seed, OP_ARCH, [m.position[0], m.position[2]], 7) & 0xffff) as u16,
                ..RubbleSpec::default()
            };
            if let Ok(a) = add_asset(world, MeshAsset::new("rubble", MeshKind::Rubble(spec))) {
                if place_mesh(world, p.room, GridMesh::new(0, a, m.position)).is_ok() {
                    report.rubble += 1;
                }
            }
        }
    }
}
