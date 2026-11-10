//! Room (design 7.7): a footprint, a base height and a room height become
//! floor and ceiling leaves, with no authored walls: the derivation closes
//! the perimeter.
//!
//! The footprint is a rectangle, an octagon (a rectangle with 45 degree
//! corners, integer), a polygon (the editor's existing polygon rule) or any
//! set of cells. Fresh ground is covered by the **maximal aligned leaves**
//! (a 12 by 12 room is nine four-sector leaves); sectors that already hold
//! cells are overwritten as sectors, keeping their walls. The room is made
//! as a new [`GridRoom`] or inside an existing one. The verb refuses before
//! it changes anything, and the editor takes one undo snapshot around it.

use std::collections::BTreeSet;

use crate::grid::leaf::{edge_clicks, LeafKey, CLICKS_PER_SECTOR};
use crate::grid::overrides::WallMode;
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSector;
use crate::grid::surface::GridSurface;
use crate::grid::tree::normalize;
use crate::grid::units::CLICK_UNITS;
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::focus::FocusRefusal;
use super::super::outline::polygon_cells;
use super::super::rects::{grow_room_to, CellRect, RectRefusal};
use super::common::{aimed, body_headroom, bounds_of, octagon_cells, sector_ready};

/// The ground a room covers.
#[derive(Clone, Debug, PartialEq)]
pub enum Footprint {
    /// An inclusive rectangle of world sectors.
    Rect(CellRect),
    /// A rectangle with each corner cut `cut` cells deep (octagon towers).
    Octagon {
        /// The bounding rectangle.
        rect: CellRect,
        /// Chamfer depth in cells.
        cut: i32,
    },
    /// A polygon in sector coordinates (`[x, z]`, integers are lattice
    /// corners): the cells whose centres are inside, even-odd.
    Polygon(Vec<[f64; 2]>),
    /// An explicit set of world sectors.
    Cells(BTreeSet<[i32; 2]>),
}

impl Footprint {
    /// The world sectors the footprint covers.
    pub fn cells(&self) -> BTreeSet<[i32; 2]> {
        match self {
            Self::Rect(r) => r.cells().collect(),
            Self::Octagon { rect, cut } => octagon_cells(*rect, *cut),
            Self::Polygon(points) => polygon_cells(points),
            Self::Cells(cells) => cells.clone(),
        }
    }
}

/// Where the room goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomTarget {
    /// Inside the room with this index.
    Existing(usize),
    /// A new derived room with this name.
    New(String),
}

/// How fresh ground is cut into leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LeafPolicy {
    /// The biggest aligned leaves that fit (the default).
    #[default]
    Maximal,
    /// One leaf per sector.
    Sectors,
}

/// Parameters of a room.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomParams {
    /// The ground.
    pub footprint: Footprint,
    /// New room or an existing one.
    pub target: RoomTarget,
    /// Floor height, grid units relative to the room's offset.
    pub base: i32,
    /// Floor to ceiling, grid units (at least the body's headroom).
    pub height: i32,
    /// Floor material.
    pub floor_material: Option<ResourceId>,
    /// Ceiling material.
    pub ceiling_material: Option<ResourceId>,
    /// What derived walls owned by these leaves look like.
    pub wall_material: Option<ResourceId>,
    /// Leaf cutting.
    pub leaves: LeafPolicy,
    /// The storey the edit is aimed at in a column of several floors
    /// (defaults to `base`).
    pub aim: Option<i32>,
}

impl RoomParams {
    /// A room of `footprint`, in a new room named `name`, floor at 0.
    pub fn new(footprint: Footprint, name: &str, height: i32) -> Self {
        Self {
            footprint,
            target: RoomTarget::New(name.to_string()),
            base: 0,
            height,
            floor_material: None,
            ceiling_material: None,
            wall_material: None,
            leaves: LeafPolicy::Maximal,
            aim: None,
        }
    }
}

/// Why a room was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum RoomRefusal {
    /// The target room does not exist.
    NoSuchRoom,
    /// The footprint holds no cell.
    EmptyFootprint,
    /// The room is shorter than the body.
    TooLow {
        /// Least height, grid units.
        least: i32,
    },
    /// The room would pass its size cap.
    RoomTooLarge,
    /// The target room draws only authored walls: convert it first.
    AuthoredRoom,
    /// Finer cells hold part of this sector: merge them first.
    FineCells {
        /// The sector.
        cell: [i32; 2],
    },
    /// The storey aim failed.
    Aim(FocusRefusal),
    /// The room would hold more leaves than the format allows.
    TooManyLeaves,
}

impl RoomRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::EmptyFootprint => "The footprint covers no cell".to_string(),
            Self::TooLow { least } => {
                format!("A room must be at least {least} units tall for the body to fit")
            }
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
            Self::AuthoredRoom => {
                "That room draws authored walls only: convert it to derived walls first".to_string()
            }
            Self::FineCells { cell } => format!(
                "Finer cells stand in sector ({}, {}): merge them first",
                cell[0], cell[1]
            ),
            Self::Aim(f) => f.describe(),
            Self::TooManyLeaves => "The room would hold too many cells".to_string(),
        }
    }
}

/// What a room made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomOutcome {
    /// The room that holds it.
    pub room: usize,
    /// Whether the room is new.
    pub created: bool,
    /// Sectors covered.
    pub cells: usize,
    /// Fresh leaves inserted (any size).
    pub leaves: usize,
    /// Sectors that already held cells and were overwritten.
    pub overwritten: usize,
}

/// Leaves a room may hold (14 bit leaf ids, one reserved).
pub const MAX_LEAVES_PER_ROOM: usize = 16383;

/// Greedy decomposition of `cells` into the biggest aligned leaves.
pub fn decompose(cells: &BTreeSet<[i32; 2]>, policy: LeafPolicy) -> Vec<LeafKey> {
    let mut covered: BTreeSet<[i32; 2]> = BTreeSet::new();
    let mut out = Vec::new();
    let top = if policy == LeafPolicy::Maximal { 0 } else { 2 };
    for &cell in cells {
        if covered.contains(&cell) {
            continue;
        }
        for level in top..=2u8 {
            let side = edge_clicks(level) / CLICKS_PER_SECTOR.max(1);
            let side = side.max(1);
            let origin = [
                cell[0].div_euclid(side) * side,
                cell[1].div_euclid(side) * side,
            ];
            let block = CellRect {
                min: origin,
                max: [origin[0] + side - 1, origin[1] + side - 1],
            };
            let fits = block
                .cells()
                .all(|c| cells.contains(&c) && !covered.contains(&c));
            if fits || level == 2 {
                covered.extend(block.cells().filter(|_| fits));
                if !fits {
                    covered.insert(cell);
                    out.push(LeafKey::sector(cell));
                } else {
                    out.push(LeafKey {
                        level,
                        x: origin[0] * CLICKS_PER_SECTOR,
                        z: origin[1] * CLICKS_PER_SECTOR,
                    });
                }
                break;
            }
        }
    }
    out
}

fn surfaces(p: &RoomParams) -> (GridSurface, GridSurface) {
    (
        GridSurface::flat_auto(p.base, p.floor_material),
        GridSurface::flat_auto(p.base + p.height, p.ceiling_material),
    )
}

/// Lay a room. One logical edit: the caller snapshots the document once.
pub fn lay_room(world: &mut GridWorld, params: &RoomParams) -> Result<RoomOutcome, RoomRefusal> {
    let cells = params.footprint.cells();
    let rect = bounds_of(&cells).ok_or(RoomRefusal::EmptyFootprint)?;
    let least = body_headroom(world).unwrap_or(CLICK_UNITS * 4);
    if params.height < least {
        return Err(RoomRefusal::TooLow { least });
    }
    if let RoomTarget::Existing(i) = params.target {
        let r = world.rooms.get(i).ok_or(RoomRefusal::NoSuchRoom)?;
        if r.wall_mode != WallMode::Derived {
            return Err(RoomRefusal::AuthoredRoom);
        }
    }
    // Work on a copy of the world so a refusal half way leaves nothing.
    if matches!(params.target, RoomTarget::New(_))
        && world.rooms.len() >= crate::grid::limits::MAX_ROOMS
    {
        return Err(RoomRefusal::RoomTooLarge);
    }
    let mut work = world.clone();
    let (index, created) = match &params.target {
        RoomTarget::Existing(i) => (*i, false),
        RoomTarget::New(name) => {
            let mut room =
                GridRoom::new_derived(name.clone(), rect.width() as usize, rect.depth() as usize);
            room.origin = rect.min;
            work.rooms.push(room);
            (work.rooms.len() - 1, true)
        }
    };
    grow_room_to(&mut work.rooms[index], rect).map_err(|e| match e {
        RectRefusal::NoSuchRoom => RoomRefusal::NoSuchRoom,
        RectRefusal::RoomTooLarge { .. } => RoomRefusal::RoomTooLarge,
    })?;
    let aim = Some(params.aim.unwrap_or(params.base));
    let keys = decompose(&cells, params.leaves);
    let result = aimed(&mut work, index, &cells, aim, |w| {
        write(w, index, &keys, params)
    })
    .map_err(RoomRefusal::Aim)??;
    let room = &mut work.rooms[index];
    if room.leaf_count() > MAX_LEAVES_PER_ROOM {
        return Err(RoomRefusal::TooManyLeaves);
    }
    normalize(room);
    *world = work;
    Ok(RoomOutcome {
        room: index,
        created,
        cells: cells.len(),
        leaves: result.0,
        overwritten: result.1,
    })
}

fn write(
    world: &mut GridWorld,
    index: usize,
    keys: &[LeafKey],
    p: &RoomParams,
) -> Result<(usize, usize), RoomRefusal> {
    let room = &mut world.rooms[index];
    let (mut fresh, mut over) = (0, 0);
    for &key in keys {
        let sectors = sectors_of(key);
        if room.leaves.conflict(key).is_none() {
            let (f, c) = surfaces(p);
            let mut s = GridSector::with_surfaces(Some(f), Some(c));
            s.set_ground_wall_material(p.wall_material);
            room.insert_leaf(key, s)
                .map_err(|_| RoomRefusal::RoomTooLarge)?;
            fresh += 1;
            continue;
        }
        for cell in sectors {
            if !sector_ready(room, cell) {
                return Err(RoomRefusal::FineCells { cell });
            }
            let [x, z] = room.local(cell).ok_or(RoomRefusal::RoomTooLarge)?;
            let (f, c) = surfaces(p);
            match room.sector_mut(x, z) {
                Some(s) => {
                    s.set_ground_floor(Some(f));
                    s.set_ground_ceiling(Some(c));
                    if p.wall_material.is_some() {
                        s.set_ground_wall_material(p.wall_material);
                    }
                    over += 1;
                }
                None => {
                    let mut s = GridSector::with_surfaces(Some(f), Some(c));
                    s.set_ground_wall_material(p.wall_material);
                    room.insert_leaf(LeafKey::sector(cell), s)
                        .map_err(|_| RoomRefusal::RoomTooLarge)?;
                    fresh += 1;
                }
            }
        }
    }
    Ok((fresh, over))
}

/// The world sectors a leaf of sector size or more covers.
fn sectors_of(key: LeafKey) -> Vec<[i32; 2]> {
    let side = (key.edge_clicks() / CLICKS_PER_SECTOR).max(1);
    let origin = [
        key.x.div_euclid(CLICKS_PER_SECTOR),
        key.z.div_euclid(CLICKS_PER_SECTOR),
    ];
    CellRect {
        min: origin,
        max: [origin[0] + side - 1, origin[1] + side - 1],
    }
    .cells()
    .collect()
}
