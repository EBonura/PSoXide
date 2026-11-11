//! Corridor (design 7.8): a waypoint path, a width, a headroom and a floor
//! height per waypoint become floor and ceiling over the swept footprint.
//!
//! Legs are axis aligned and corners are mitred squares: every waypoint is a
//! flat landing `width` by `width` sectors, so a turn needs no special case.
//! A leg between two waypoints at different heights becomes a [`LegShape`]:
//! a **ramp** (every cell between the landings slopes, held under the
//! walkable slope) or **stairs** (every cell between the landings is one
//! tread, the riser held under the motor's step-up, the shared
//! [`psx_engine::WalkLimits`]). The ceiling follows the floor by a constant
//! headroom. Where the corridor meets cells that exist it keeps them (the
//! junction opens by derivation where floors agree, and builds a riser or a
//! drop where they do not) unless `overwrite` is set.
//!
//! Widths are whole sectors; a narrower passage is made by subdividing the
//! edge cells afterwards (the cells verbs). Nothing is written on refusal.

use std::collections::{BTreeMap, BTreeSet};

use psx_engine::WalkLimits;

use crate::grid::cook::RuntimeUnits;
use crate::grid::overrides::WallMode;
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSector;
use crate::grid::surface::GridSurface;
use crate::grid::tree::normalize;
use crate::grid::units::{snap_to, FINE_UNITS, SECTOR_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::focus::FocusRefusal;
use super::super::rects::{grow_room_to, RectRefusal};
use super::common::{aimed, body_headroom, bounds_of, sector_ready};
use super::room::RoomTarget;
use super::stairs::max_riser;

/// What a leg of different heights becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LegShape {
    /// Sloped cells.
    #[default]
    Ramp,
    /// One flat tread per cell.
    Stairs,
}

/// A point of the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waypoint {
    /// World sector.
    pub cell: [i32; 2],
    /// Floor height here, grid units; `None` keeps the previous one.
    pub floor: Option<i32>,
}

impl Waypoint {
    /// A waypoint that keeps the running height.
    pub const fn at(cell: [i32; 2]) -> Self {
        Self { cell, floor: None }
    }

    /// A waypoint at a floor height.
    pub const fn at_height(cell: [i32; 2], floor: i32) -> Self {
        Self {
            cell,
            floor: Some(floor),
        }
    }
}

/// Parameters of a corridor.
#[derive(Clone, Debug, PartialEq)]
pub struct CorridorParams {
    /// Where it goes.
    pub target: RoomTarget,
    /// The path.
    pub waypoints: Vec<Waypoint>,
    /// Width in whole sectors (at least 1).
    pub width: i32,
    /// Floor to ceiling, grid units.
    pub headroom: i32,
    /// Floor height of the first waypoint when it names none.
    pub base: i32,
    /// Shape of a leg between two heights.
    pub leg: LegShape,
    /// Floor material.
    pub floor_material: Option<ResourceId>,
    /// Ceiling material.
    pub ceiling_material: Option<ResourceId>,
    /// Material of the derived walls the corridor owns.
    pub wall_material: Option<ResourceId>,
    /// Replace the floors and ceilings of cells that exist.
    pub overwrite: bool,
    /// The storey the edit is aimed at (defaults to `base`).
    pub aim: Option<i32>,
}

impl CorridorParams {
    /// A corridor through `waypoints`, one sector wide.
    pub fn new(target: RoomTarget, waypoints: Vec<Waypoint>, headroom: i32) -> Self {
        Self {
            target,
            waypoints,
            width: 1,
            headroom,
            base: 0,
            leg: LegShape::Ramp,
            floor_material: None,
            ceiling_material: None,
            wall_material: None,
            overwrite: false,
            aim: None,
        }
    }
}

/// Why a corridor was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum CorridorRefusal {
    /// The target room does not exist.
    NoSuchRoom,
    /// Fewer than two waypoints.
    TooFewWaypoints,
    /// The width is under one sector.
    NoWidth,
    /// A leg is not along X or Z.
    DiagonalLeg {
        /// Index of the leg's first waypoint.
        leg: usize,
    },
    /// Less headroom than the body needs.
    TooLow {
        /// Least headroom, grid units.
        least: i32,
    },
    /// A leg changes height but has no cell between its landings.
    NoRoomForChange {
        /// Index of the leg's first waypoint.
        leg: usize,
    },
    /// The treads would pass the step-up (or the ramp the walkable slope).
    TooSteep {
        /// Index of the leg's first waypoint.
        leg: usize,
        /// Rise per cell, grid units.
        rise_per_cell: i32,
        /// The most one cell may rise in this shape.
        limit: i32,
    },
    /// The room would pass its size cap.
    RoomTooLarge,
    /// The target room draws authored walls only.
    AuthoredRoom,
    /// Finer cells hold part of this sector.
    FineCells {
        /// The sector.
        cell: [i32; 2],
    },
    /// The storey aim failed.
    Aim(FocusRefusal),
}

impl CorridorRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::TooFewWaypoints => "A corridor needs at least two waypoints".to_string(),
            Self::NoWidth => "A corridor is at least one sector wide".to_string(),
            Self::DiagonalLeg { leg } => {
                format!("Leg {leg} is not along X or Z: legs are axis aligned")
            }
            Self::TooLow { least } => format!("Headroom must be at least {least} units"),
            Self::NoRoomForChange { leg } => format!(
                "Leg {leg} changes height but has no sector between its landings: lengthen it"
            ),
            Self::TooSteep {
                leg,
                rise_per_cell,
                limit,
            } => format!(
                "Leg {leg} would rise {rise_per_cell} units a sector, the limit is {limit}: lengthen it"
            ),
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
            Self::AuthoredRoom => "That room draws authored walls only".to_string(),
            Self::FineCells { cell } => format!(
                "Finer cells stand in sector ({}, {}): merge them first",
                cell[0], cell[1]
            ),
            Self::Aim(f) => f.describe(),
        }
    }
}

/// What a leg came out as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegReport {
    /// Flat, ramp or stairs.
    pub kind: Option<LegShape>,
    /// Steps (stairs) or sloped cells (ramp), 0 for a flat leg.
    pub cells: usize,
    /// Rise of the leg, grid units.
    pub rise: i32,
}

/// What a corridor made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorridorOutcome {
    /// The room that holds it.
    pub room: usize,
    /// Whether the room is new.
    pub created: bool,
    /// Sectors of the footprint.
    pub cells: usize,
    /// Sectors that existed and were left alone.
    pub kept: usize,
    /// One report per leg.
    pub legs: Vec<LegReport>,
}

/// Floor corners `[NW, NE, SE, SW]` of a tread sloping from `entry` to `exit`
/// in direction `dir`.
fn slope_corners(dir: [i32; 2], entry: i32, exit: i32) -> [i32; 4] {
    match dir {
        [1, 0] => [entry, exit, exit, entry],
        [-1, 0] => [exit, entry, entry, exit],
        [0, 1] => [entry, entry, exit, exit],
        _ => [exit, exit, entry, entry],
    }
}

fn offsets(width: i32) -> std::ops::RangeInclusive<i32> {
    -((width - 1) / 2)..=(width / 2)
}

/// `dh * k / n` rounded half away from zero, to the fine step.
fn lerp(dh: i32, k: i32, n: i32) -> i32 {
    let num = i64::from(dh) * i64::from(k);
    let den = i64::from(n.max(1));
    let r = if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    };
    snap_to(r as i32, FINE_UNITS)
}

/// Plan every cell's floor corners. Pure; also what the tests read.
#[allow(clippy::type_complexity)]
pub fn plan(
    world: &GridWorld,
    params: &CorridorParams,
) -> Result<(BTreeMap<[i32; 2], [i32; 4]>, Vec<LegReport>), CorridorRefusal> {
    if params.waypoints.len() < 2 {
        return Err(CorridorRefusal::TooFewWaypoints);
    }
    if params.width < 1 {
        return Err(CorridorRefusal::NoWidth);
    }
    let least = body_headroom(world).unwrap_or(1024);
    if params.headroom < least {
        return Err(CorridorRefusal::TooLow { least });
    }
    let units = RuntimeUnits::new(world.scale).ok();
    let limits = WalkLimits::DEFAULT;
    let mut heights = Vec::new();
    let mut running = params.base;
    for w in &params.waypoints {
        running = w.floor.unwrap_or(running);
        heights.push(snap_to(running, FINE_UNITS));
    }
    let mut cells: BTreeMap<[i32; 2], [i32; 4]> = BTreeMap::new();
    let mut legs = Vec::new();
    for i in 0..params.waypoints.len() - 1 {
        let (a, b) = (params.waypoints[i].cell, params.waypoints[i + 1].cell);
        if a[0] != b[0] && a[1] != b[1] {
            return Err(CorridorRefusal::DiagonalLeg { leg: i });
        }
        let d = [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
        let len = (b[0] - a[0]).abs() + (b[1] - a[1]).abs();
        let dh = heights[i + 1] - heights[i];
        let between = (len - 1).max(0);
        if dh != 0 && between == 0 {
            return Err(CorridorRefusal::NoRoomForChange { leg: i });
        }
        let per_cell = if between > 0 { dh.abs() / between } else { 0 };
        let rem = dh.abs() - per_cell * between.max(1);
        let per_cell = per_cell + i32::from(rem > 0);
        let limit = match params.leg {
            LegShape::Stairs => units.as_ref().map_or(512, |u| max_riser(u, &limits)),
            // A slope of at most 19 in 20, safely under the walkable normal.
            LegShape::Ramp => SECTOR_UNITS * 19 / 20,
        };
        if dh != 0 && per_cell > limit {
            return Err(CorridorRefusal::TooSteep {
                leg: i,
                rise_per_cell: per_cell,
                limit,
            });
        }
        let kind = (dh != 0).then_some(params.leg);
        legs.push(LegReport {
            kind,
            cells: if dh != 0 { between as usize } else { 0 },
            rise: dh,
        });
        // A wide leg grows towards +X (a leg along Z) or +Z (a leg along X)
        // whichever way it runs, like the landing squares at its ends.
        let lateral = [d[1].abs(), d[0].abs()];
        for k in 1..len {
            let (entry, exit) = (
                heights[i] + lerp(dh, k - 1, between),
                heights[i] + lerp(dh, k, between),
            );
            let corners = match (dh, params.leg) {
                (0, _) => [heights[i]; 4],
                (_, LegShape::Stairs) => [exit; 4],
                (_, LegShape::Ramp) => slope_corners(d, entry, exit),
            };
            for o in offsets(params.width) {
                let cell = [
                    a[0] + d[0] * k + lateral[0] * o,
                    a[1] + d[1] * k + lateral[1] * o,
                ];
                cells.insert(cell, corners);
            }
        }
    }
    // Landings last: every waypoint is a flat square.
    for (w, &h) in params.waypoints.iter().zip(&heights) {
        for ox in offsets(params.width) {
            for oz in offsets(params.width) {
                cells.insert([w.cell[0] + ox, w.cell[1] + oz], [h; 4]);
            }
        }
    }
    Ok((cells, legs))
}

/// Lay a corridor. One logical edit: the caller snapshots the document once.
pub fn lay_corridor(
    world: &mut GridWorld,
    params: &CorridorParams,
) -> Result<CorridorOutcome, CorridorRefusal> {
    let (plan, legs) = plan(world, params)?;
    let set: BTreeSet<[i32; 2]> = plan.keys().copied().collect();
    let rect = bounds_of(&set).ok_or(CorridorRefusal::TooFewWaypoints)?;
    if let RoomTarget::Existing(i) = params.target {
        let r = world.rooms.get(i).ok_or(CorridorRefusal::NoSuchRoom)?;
        if r.wall_mode != WallMode::Derived {
            return Err(CorridorRefusal::AuthoredRoom);
        }
    }
    let mut work = world.clone();
    let (index, created) = match &params.target {
        RoomTarget::Existing(i) => (*i, false),
        RoomTarget::New(name) => {
            if work.rooms.len() >= crate::grid::limits::MAX_ROOMS {
                return Err(CorridorRefusal::RoomTooLarge);
            }
            let mut room =
                GridRoom::new_derived(name.clone(), rect.width() as usize, rect.depth() as usize);
            room.origin = rect.min;
            work.rooms.push(room);
            (work.rooms.len() - 1, true)
        }
    };
    grow_room_to(&mut work.rooms[index], rect).map_err(|e| match e {
        RectRefusal::NoSuchRoom => CorridorRefusal::NoSuchRoom,
        RectRefusal::RoomTooLarge { .. } => CorridorRefusal::RoomTooLarge,
    })?;
    let aim = Some(params.aim.unwrap_or(params.base));
    let kept = aimed(&mut work, index, &set, aim, |w| {
        write(w, index, &plan, params)
    })
    .map_err(CorridorRefusal::Aim)??;
    normalize(&mut work.rooms[index]);
    *world = work;
    Ok(CorridorOutcome {
        room: index,
        created,
        cells: set.len(),
        kept,
        legs,
    })
}

fn write(
    world: &mut GridWorld,
    index: usize,
    plan: &BTreeMap<[i32; 2], [i32; 4]>,
    p: &CorridorParams,
) -> Result<usize, CorridorRefusal> {
    let room = &mut world.rooms[index];
    let mut kept = 0;
    for (&cell, &corners) in plan {
        if !sector_ready(room, cell) {
            return Err(CorridorRefusal::FineCells { cell });
        }
        let [x, z] = room.local(cell).ok_or(CorridorRefusal::RoomTooLarge)?;
        let existed = room
            .sector(x, z)
            .is_some_and(|s| s.ground_floor().is_some());
        if existed && !p.overwrite {
            kept += 1;
            continue;
        }
        let mut floor = GridSurface::flat_auto(corners[0], p.floor_material);
        floor.heights = corners;
        let mut ceiling = GridSurface::flat_auto(corners[0] + p.headroom, p.ceiling_material);
        ceiling.heights = corners.map(|h| h + p.headroom);
        crate::grid::auto_split::resplit(&mut floor);
        crate::grid::auto_split::resplit(&mut ceiling);
        match room.sector_mut(x, z) {
            Some(s) => {
                s.set_ground_floor(Some(floor));
                s.set_ground_ceiling(Some(ceiling));
                if p.wall_material.is_some() {
                    s.set_ground_wall_material(p.wall_material);
                }
            }
            None => {
                let mut s = GridSector::with_surfaces(Some(floor), Some(ceiling));
                s.set_ground_wall_material(p.wall_material);
                room.insert_leaf(crate::grid::leaf::LeafKey::sector(cell), s)
                    .map_err(|_| CorridorRefusal::RoomTooLarge)?;
            }
        }
    }
    Ok(kept)
}
