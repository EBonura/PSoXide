//! Building: a footprint, a storey count and height, doors, a window rhythm,
//! an interior spiral stair linking the storeys and an optional flat roof
//! with a parapet, composed from [`lay_room`], the storey primitive
//! ([`insert_slab`]), [`lay_spiral`] and [`cut_opening`].
//!
//! The building is one room of whole sectors: a single tall volume, with a
//! slab at every storey height over the footprint except the stairwell (the
//! multi-storey span model, `docs/grid/multi-storey.md`). The flat roof is
//! the slab at the top; the span over it has no ceiling, so the room's open
//! wall height is the parapet. Closure walls are drawn from both sides (a
//! building is seen from outside).
//!
//! Limits, each with what lifting it needs:
//!
//! * **No wall thickness.** A wall is a zero thickness derived face; there
//!   is no inner and outer skin in the data model.
//! * **With several storeys, openings are a whole sector side and flat.**
//!   A storey column stays whole sectors, so a head or a narrow opening
//!   that needs finer cells is only available in a one storey building.
//! * **Doors and windows on the ground storey only.** An opening is an
//!   override on the wall segment plus authored pieces, and both address an
//!   edge, not a span: cutting one on an upper storey would also open the
//!   storey below. Lifting this needs the edge override and the authored
//!   wall to name a span (`EdgeKey` plus a span index).
//! * **The stair is a spiral** round a ring of sectors, so it needs a ring
//!   of at least 3 by 3 sectors inside the footprint.
//!
//! Every random choice is a hash of `(seed, operation, position)`: the seed
//! moves the doors and the window phase, nothing else.

use std::collections::BTreeSet;

use crate::grid::edit::spans::insert_slab;
use crate::grid::overrides::ClosureFacing;
use crate::grid::sector::WallEdge;
use crate::grid::units::CLICK_UNITS;
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::rects::CellRect;
use super::common::{bounds_of, hash};
use super::opening::{
    arch_head, cut_opening, ArchShape, DoorPlane, OpeningKind, OpeningParams, OpeningProfile,
};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomRefusal, RoomTarget};
use super::spiral::{lay_spiral, ring_cells, SpiralParams};
use super::stairs::RiserOption;

const OP_DOOR: u32 = 201;
const OP_WINDOW: u32 = 202;

/// Parameters of a building.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildingParams {
    /// A new room or an existing derived one.
    pub target: RoomTarget,
    /// The ground plan.
    pub footprint: Footprint,
    /// Floor height of the ground storey, grid units.
    pub base: i32,
    /// Storeys, 1 to 4.
    pub storeys: i32,
    /// Floor to floor, grid units (a multiple of a click, at least 2048).
    pub storey_height: i32,
    /// Sides that get a door (a sector near the middle of each, by seed).
    pub door_sides: Vec<WallEdge>,
    /// Door clear height, grid units.
    pub door_height: i32,
    /// A window every this many sectors along each side (0 for none).
    pub window_every: i32,
    /// Window sill above the floor, grid units.
    pub window_sill: i32,
    /// Window top above the floor, grid units (the apex of an arched one).
    pub window_top: i32,
    /// Arched window heads, a sector wide (flat lintels two clicks wide when
    /// `None`).
    pub window_arch: Option<ArchShape>,
    /// An interior spiral stair linking the storeys (needs more than one).
    pub stair: bool,
    /// A flat roof; the value is the parapet height, grid units.
    pub roof_parapet: Option<i32>,
    /// Moves the doors and the window phase.
    pub seed: u32,
    /// Floor material.
    pub floor_material: Option<ResourceId>,
    /// Ceiling material.
    pub ceiling_material: Option<ResourceId>,
    /// Wall material.
    pub wall_material: Option<ResourceId>,
}

impl BuildingParams {
    /// A two storey building of 3072 units a storey with a door on the
    /// south, a window every second sector, a stair and a roof with a
    /// parapet 768 units tall.
    pub fn new(target: RoomTarget, footprint: Footprint) -> Self {
        Self {
            target,
            footprint,
            base: 0,
            storeys: 2,
            storey_height: 3072,
            door_sides: vec![WallEdge::South],
            door_height: 2048,
            window_every: 2,
            window_sill: 768,
            window_top: 2048,
            window_arch: None,
            stair: true,
            roof_parapet: Some(768),
            seed: 0,
            floor_material: None,
            ceiling_material: None,
            wall_material: None,
        }
    }
}

/// Why a building was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum BuildingRefusal {
    /// A number is out of range (the message names it).
    BadParameter(&'static str),
    /// The room step was refused.
    Room(RoomRefusal),
    /// No ring of sectors fits inside the footprint for the stair.
    NoStairRing,
    /// The stair was refused (the message says why).
    Stair(String),
}

impl BuildingRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::BadParameter(what) => format!("{what} is out of range"),
            Self::Room(r) => r.describe(),
            Self::NoStairRing => {
                "No 3 by 3 ring of sectors fits inside the footprint for the stair".to_string()
            }
            Self::Stair(why) => why.clone(),
        }
    }
}

/// What a building made.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildingReport {
    /// The room that holds it.
    pub room: usize,
    /// Sectors of the footprint.
    pub cells: usize,
    /// Slab cells laid (floors, stair treads and the roof).
    pub slabs: usize,
    /// Treads of the stair.
    pub treads: usize,
    /// The stair's ring rectangle.
    pub stairwell: Option<CellRect>,
    /// Door planes.
    pub doors: Vec<DoorPlane>,
    /// Windows cut.
    pub windows: usize,
    /// Things left out and why.
    pub notes: Vec<String>,
}

fn find_ring(cells: &BTreeSet<[i32; 2]>) -> Option<CellRect> {
    let b = bounds_of(cells)?;
    for z in b.min[1]..=b.max[1] - 2 {
        for x in b.min[0]..=b.max[0] - 2 {
            let r = CellRect {
                min: [x, z],
                max: [x + 2, z + 2],
            };
            if r.cells().all(|c| cells.contains(&c)) {
                return Some(r);
            }
        }
    }
    None
}

/// Boundary edges of the footprint: `(cell, side)` with no cell beyond.
fn boundary(cells: &BTreeSet<[i32; 2]>) -> Vec<([i32; 2], WallEdge)> {
    let mut out = Vec::new();
    for &c in cells {
        for side in [
            WallEdge::North,
            WallEdge::East,
            WallEdge::South,
            WallEdge::West,
        ] {
            let d = side.front_direction();
            let beyond = [c[0] - d[0], c[1] - d[1]];
            if !cells.contains(&beyond) {
                out.push((c, side));
            }
        }
    }
    out
}

/// Build a building. One logical edit: the caller snapshots once.
pub fn build_building(
    world: &mut GridWorld,
    p: &BuildingParams,
) -> Result<BuildingReport, BuildingRefusal> {
    let bad = BuildingRefusal::BadParameter;
    if !(1..=4).contains(&p.storeys) {
        return Err(bad("storeys"));
    }
    if p.storey_height < 2048 || p.storey_height % CLICK_UNITS != 0 {
        return Err(bad("storey height"));
    }
    if p.window_every < 0 || p.window_sill < 0 || p.window_top <= p.window_sill {
        return Err(bad("window"));
    }
    if p.window_top > p.storey_height - 256 {
        return Err(bad("window top"));
    }
    if p.door_height > p.storey_height - 256 {
        return Err(bad("door height"));
    }
    let cells = p.footprint.cells();
    if cells.is_empty() {
        return Err(BuildingRefusal::Room(RoomRefusal::EmptyFootprint));
    }
    let mut work = world.clone();
    let mut notes = Vec::new();
    let top = p.storeys * p.storey_height;
    // The roof slab needs room above it: a volume a storey and a bit tall.
    let height = if p.roof_parapet.is_some() {
        top + 1024
    } else {
        top
    };
    let made = lay_room(
        &mut work,
        &RoomParams {
            footprint: p.footprint.clone(),
            target: p.target.clone(),
            base: p.base,
            height,
            floor_material: p.floor_material,
            ceiling_material: p.ceiling_material,
            wall_material: p.wall_material,
            leaves: LeafPolicy::Sectors,
            aim: None,
        },
    )
    .map_err(BuildingRefusal::Room)?;
    let room = made.room;
    if made.created {
        let r = &mut work.rooms[room];
        r.defaults.closure_facing = ClosureFacing::Both;
        r.defaults.wall_material = p.wall_material;
        if let Some(h) = p.roof_parapet {
            r.defaults.open_wall = h;
        }
    }
    // The stairwell ring, if the building has a stair and a storey to reach.
    let mut well: Option<CellRect> = None;
    if p.stair && p.storeys > 1 {
        well = Some(find_ring(&cells).ok_or(BuildingRefusal::NoStairRing)?);
    }
    let hole: BTreeSet<[i32; 2]> = well.map(|r| r.cells().collect()).unwrap_or_default();
    // Floors: a slab at each storey height over everything but the well.
    let mut slabs = 0;
    for k in 1..p.storeys {
        for &c in cells.difference(&hole) {
            let one = CellRect { min: c, max: c };
            let out = insert_slab(
                &mut work,
                room,
                one,
                p.base + k * p.storey_height,
                CLICK_UNITS,
                p.floor_material,
            );
            slabs += out.changed;
        }
    }
    // The stair.
    let mut treads = 0;
    if let Some(r) = well {
        let corners = [r.min, [r.max[0], r.min[1]], r.max, [r.min[0], r.max[1]]];
        let ring = ring_cells(&corners).ok_or(BuildingRefusal::NoStairRing)?;
        let rep = lay_spiral(
            &mut work,
            room,
            &ring,
            SpiralParams {
                storeys: (p.storeys - 1) as usize,
                riser: RiserOption::Click,
                material: p.floor_material,
            },
            p.storey_height,
        );
        if let Some(why) = rep.refused {
            return Err(BuildingRefusal::Stair(why.describe()));
        }
        treads = rep.treads;
        slabs += rep.slabs.changed;
        if rep.per_storey != p.storey_height {
            notes.push(format!(
                "the stair climbs {} a storey, the storeys are {} apart",
                rep.per_storey, p.storey_height
            ));
        }
    }
    // The roof: a slab at the top, open to the sky.
    if p.roof_parapet.is_some() {
        for &c in &cells {
            let one = CellRect { min: c, max: c };
            let out = insert_slab(
                &mut work,
                room,
                one,
                p.base + top,
                CLICK_UNITS,
                p.floor_material,
            );
            slabs += out.changed;
            if let Some(s) = work.rooms[room]
                .local(c)
                .and_then(|[x, z]| work.rooms[room].sector_mut(x, z))
            {
                if let Some(span) = s.spans_mut().last_mut() {
                    if out.changed > 0 {
                        span.ceiling = None;
                    }
                }
            }
        }
    }
    // Doors: near the middle of each chosen side, the seed picks which.
    let edges = boundary(&cells);
    let mut doors = Vec::new();
    let mut door_cells: BTreeSet<[i32; 2]> = BTreeSet::new();
    for &side in &p.door_sides {
        let mut on_side: Vec<[i32; 2]> = edges
            .iter()
            .filter(|(_, s)| *s == side)
            .map(|(c, _)| *c)
            .collect();
        on_side.sort_by_key(|c| match side {
            WallEdge::North | WallEdge::South => (c[0], c[1]),
            _ => (c[1], c[0]),
        });
        if on_side.is_empty() {
            notes.push(format!("no {} wall for a door", side_name(side)));
            continue;
        }
        let mid = on_side.len() / 2;
        let jitter = (hash(p.seed, OP_DOOR, [side as i32, 0], 0) % 3) as usize;
        let pick = on_side[(mid + jitter).saturating_sub(1).min(on_side.len() - 1)];
        let (from, to) = along(pick, side);
        let op = OpeningParams {
            profile: OpeningProfile::Rectangular,
            room,
            cell: pick,
            side,
            from,
            to,
            kind: OpeningKind::Door,
            top: p.door_height,
            material: p.wall_material,
        };
        match cut_opening(&mut work, &op) {
            Ok(o) => {
                doors.extend(o.door);
                door_cells.insert(pick);
            }
            Err(e) => notes.push(format!("door skipped: {}", e.describe())),
        }
    }
    // Windows: a rhythm along every side, ground storey.
    let mut windows = 0;
    if p.window_every > 0 {
        let phase = (hash(p.seed, OP_WINDOW, [0, 0], 0) % p.window_every as u32) as i32;
        for (c, side) in &edges {
            // Not on the stairwell: the treads lower the ceiling under them.
            if door_cells.contains(c) || well.is_some_and(|r| r.contains(*c)) {
                continue;
            }
            let along_coord = match side {
                WallEdge::North | WallEdge::South => c[0],
                _ => c[1],
            };
            if (along_coord + phase).rem_euclid(p.window_every) != 0 {
                continue;
            }
            let (s, _) = along(*c, *side);
            // A storey column stays whole sectors, so with several storeys a
            // window is the whole sector side and flat; an arched or
            // narrower one needs finer cells.
            let (from, to, profile) = match (p.window_arch, p.storeys) {
                (_, n) if n > 1 => (s, s + 4, OpeningProfile::Rectangular),
                (Some(shape), _) => (s, s + 4, arch_head(shape, 4, p.window_top)),
                _ => (s + 1, s + 3, OpeningProfile::Rectangular),
            };
            let op = OpeningParams {
                profile,
                room,
                cell: *c,
                side: *side,
                from,
                to,
                kind: OpeningKind::Window {
                    sill: p.window_sill,
                },
                top: p.window_top,
                material: p.wall_material,
            };
            match cut_opening(&mut work, &op) {
                Ok(_) => windows += 1,
                Err(e) => notes.push(format!("window skipped: {}", e.describe())),
            }
        }
    }
    *world = work;
    Ok(BuildingReport {
        room,
        cells: cells.len(),
        slabs,
        treads,
        stairwell: well,
        doors,
        windows,
        notes,
    })
}

/// The click range of the whole side of sector `c`.
fn along(c: [i32; 2], side: WallEdge) -> (i32, i32) {
    let s = match side {
        WallEdge::North | WallEdge::South => c[0] * 4,
        _ => c[1] * 4,
    };
    (s, s + 4)
}

fn side_name(side: WallEdge) -> &'static str {
    match side {
        WallEdge::North => "north",
        WallEdge::East => "east",
        WallEdge::South => "south",
        _ => "west",
    }
}
