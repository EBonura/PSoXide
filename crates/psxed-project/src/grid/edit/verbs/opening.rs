//! Opening (design 7.9) and the doorway helper: a Door or a Window cut in a
//! wall run, mirrored on the neighbouring room's wall across a room border.
//!
//! **Rectangular only.** The grid has no opening record: a derived wall is a
//! whole segment per leaf edge. The verb therefore works with the data that
//! exists. It subdivides the host sector along the wall to the coarsest cell
//! size the opening's ends fall on (so a door is a whole number of half or
//! quarter sector segments), then for each segment it
//!
//! * hides the derived face and makes it a ghost for collision (an override
//!   on the segment), and
//! * writes the pieces that stay as ordinary authored walls beside it: a
//!   **lintel** above the opening, and for a window a **sill** below it.
//!   The lintel never blocks (the opening is taller than the body), the sill
//!   blocks.
//!
//! An authored wall already on the edge is split the same way. Nothing here
//! is stored about the opening: it is plain, hand-tweakable grid data.
//!
//! **Arched profiles are not built.** A round or pointed head needs a data
//! record (an opening with a profile and an `Intrados` height anchor, design
//! 4.5 and natural-terrain N9) that the derive step and the cook read, and a
//! collision wall that spans part of its segment (`ColWall.reserved`, a
//! format change in `psx-grid`). Both reach outside the grid editor crate.
//! Until then an arch is the [`super::arcade`] arch mesh set in a doorway.

use crate::grid::derive::DerivedKind;
use crate::grid::leaf::{edge_clicks, LeafKey, CLICKS_PER_SECTOR};
use crate::grid::leaf_derive::derived_walls_of;
use crate::grid::limits::MAX_WALLS_PER_EDGE;
use crate::grid::overrides::Axis;
use crate::grid::sector::WallEdge;
use crate::grid::surface::{GridWall, WallFacing};
use crate::grid::tree::{normalize, subdivide_to};
use crate::grid::units::CLICK_UNITS;
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::fit::{split_to_fit, ClickRect};
use super::common::body_headroom;

/// What is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpeningKind {
    /// From the floor up to `top`.
    Door,
    /// From `sill` to `top`, both above the floor.
    Window {
        /// Height of the sill above the floor, grid units.
        sill: i32,
    },
}

/// Parameters of an opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpeningParams {
    /// The room that holds the wall.
    pub room: usize,
    /// The sector the wall is seen from.
    pub cell: [i32; 2],
    /// Its side (a cardinal edge).
    pub side: WallEdge,
    /// Start of the opening along the wall, world clicks.
    pub from: i32,
    /// End of the opening along the wall, world clicks (exclusive).
    pub to: i32,
    /// Door or window.
    pub kind: OpeningKind,
    /// Height of the lintel's underside above the floor, grid units.
    pub top: i32,
    /// Material of the sill and lintel (the wall's own when `None`).
    pub material: Option<ResourceId>,
}

/// Why an opening was not cut. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpeningRefusal {
    /// The room does not exist.
    NoSuchRoom,
    /// Only the four cardinal sides take an opening.
    NotCardinal,
    /// The range is empty or leaves the sector's side.
    BadRange,
    /// The sector has no floor and ceiling to cut a wall in.
    NoHost,
    /// Nothing stands on that stretch: floors and ceilings agree.
    NotAWall,
    /// The stretch is already open.
    AlreadyOpen,
    /// A wall face on the stretch is not a rectangle.
    NotRectangular,
    /// The opening is shorter than the body.
    TooLow {
        /// Least clear height, grid units.
        least: i32,
    },
    /// The lintel would pass the ceiling.
    TopAboveCeiling {
        /// Most, grid units above the floor.
        max: i32,
    },
    /// The sill is not under the lintel.
    SillAboveTop,
    /// Finer cells make the edge impossible to cut.
    FineCells,
    /// The edge already holds as many walls as it may.
    TooManyWalls,
}

impl OpeningRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::NotCardinal => "Openings go in north, east, south or west walls".to_string(),
            Self::BadRange => "The opening must lie inside the sector's side".to_string(),
            Self::NoHost => "That sector has no floor and ceiling".to_string(),
            Self::NotAWall => "There is no wall there: the floors and ceilings agree".to_string(),
            Self::AlreadyOpen => "That stretch is already open".to_string(),
            Self::NotRectangular => "The wall there is not a rectangle".to_string(),
            Self::TooLow { least } => format!("A door must be at least {least} units tall"),
            Self::TopAboveCeiling { max } => {
                format!("The top is above the ceiling: at most {max} units")
            }
            Self::SillAboveTop => "The sill must be under the top".to_string(),
            Self::FineCells => "Finer cells in the way".to_string(),
            Self::TooManyWalls => "That edge holds as many walls as it may".to_string(),
        }
    }
}

/// The plane a door entity fills, data only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoorPlane {
    /// The room the wall belongs to.
    pub room: usize,
    /// Centre of the opening at floor level, world grid units `[x, y, z]`
    /// (`y` includes the room's vertical offset).
    pub center: [i32; 3],
    /// The direction the plane runs along.
    pub axis: Axis,
    /// Horizontal unit step out of the host sector into the doorway
    /// (towards the room the door leads to).
    pub through: [i32; 2],
    /// Clear width along the axis, grid units.
    pub width: i32,
    /// Clear height, grid units.
    pub height: i32,
}

/// What an opening did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpeningOutcome {
    /// Wall segments opened (both rooms counted).
    pub segments: usize,
    /// Authored wall pieces written (lintels and sills).
    pub pieces: usize,
    /// Whether a neighbouring room's wall was cut as well.
    pub mirrored: bool,
    /// The door plane, for a door.
    pub door: Option<DoorPlane>,
}

struct Side {
    start: i32,
}

fn side_of(cell: [i32; 2], side: WallEdge) -> Option<Side> {
    match side {
        WallEdge::North | WallEdge::South => Some(Side {
            start: cell[0] * CLICKS_PER_SECTOR,
        }),
        WallEdge::East | WallEdge::West => Some(Side {
            start: cell[1] * CLICKS_PER_SECTOR,
        }),
        _ => None,
    }
}

/// The click on the host's side line at `c` along the wall.
fn edge_click(cell: [i32; 2], side: WallEdge, c: i32) -> [i32; 2] {
    let n = CLICKS_PER_SECTOR;
    match side {
        WallEdge::North => [c, cell[1] * n],
        WallEdge::South => [c, cell[1] * n + n - 1],
        WallEdge::West => [cell[0] * n, c],
        _ => [cell[0] * n + n - 1, c],
    }
}

/// Split a wall piece `[bottom a, bottom b, top b, top a]` around an opening
/// from `lo` to `hi` (absolute heights, per end `[a, b]`): the lintel and,
/// when `lo` is a sill, the sill.
fn pieces(h: [i32; 4], lo: Option<[i32; 2]>, hi: [i32; 2]) -> Vec<[i32; 4]> {
    let mut out = Vec::new();
    // Lintel: from the opening's top to the wall's top.
    let l = [hi[0].max(h[0]), hi[1].max(h[1])];
    if l[0] < h[3] || l[1] < h[2] {
        out.push([l[0].min(h[3]), l[1].min(h[2]), h[2], h[3]]);
    }
    if let Some(lo) = lo {
        let t = [lo[0].min(h[3]), lo[1].min(h[2])];
        if t[0] > h[0] || t[1] > h[1] {
            out.push([h[0], h[1], t[1].max(h[1]), t[0].max(h[0])]);
        }
    }
    out
}

/// Cut an opening. One logical edit: the caller snapshots the document once.
pub fn cut_opening(
    world: &mut GridWorld,
    params: &OpeningParams,
) -> Result<OpeningOutcome, OpeningRefusal> {
    let mut work = world.clone();
    let mut out = OpeningOutcome {
        segments: 0,
        pieces: 0,
        mirrored: false,
        door: None,
    };
    let door = cut_one(&mut work, params, None, &mut out)?;
    out.door = match params.kind {
        OpeningKind::Door => door.map(|d| d.0),
        OpeningKind::Window { .. } => None,
    };
    // The same stretch of the room across the border, if one is there.
    if let Some((_, abs_top, abs_sill)) = door {
        let outward = {
            let d = params.side.front_direction();
            [-d[0], -d[1]]
        };
        let mid = (params.from + params.to - 1) / 2;
        let probe = edge_click(params.cell, params.side, mid);
        let step = [probe[0] + outward[0], probe[1] + outward[1]];
        let neighbour: Vec<usize> = (0..work.rooms.len())
            .filter(|&j| j != params.room)
            .filter(|&j| {
                work.rooms[j].leaf_at(step).is_some_and(|(_, s)| {
                    s.ground_floor().is_some() && s.ground_ceiling().is_some()
                })
            })
            .collect();
        if let Some(&j) = neighbour.first() {
            let cell = [
                step[0].div_euclid(CLICKS_PER_SECTOR),
                step[1].div_euclid(CLICKS_PER_SECTOR),
            ];
            let mirror = OpeningParams {
                room: j,
                cell,
                side: params.side.opposite().map_or(params.side, |(e, _)| e),
                ..*params
            };
            let before = out.segments;
            if cut_one(&mut work, &mirror, Some((abs_top, abs_sill)), &mut out).is_ok() {
                out.mirrored = out.segments > before;
            }
        }
    }
    *world = work;
    Ok(out)
}

/// `(plane, absolute top, absolute sill)` of what was cut.
type Cut = (DoorPlane, i32, Option<i32>);

fn cut_one(
    world: &mut GridWorld,
    p: &OpeningParams,
    absolute: Option<(i32, Option<i32>)>,
    out: &mut OpeningOutcome,
) -> Result<Option<Cut>, OpeningRefusal> {
    let side = side_of(p.cell, p.side).ok_or(OpeningRefusal::NotCardinal)?;
    if p.from >= p.to || p.from < side.start || p.to > side.start + CLICKS_PER_SECTOR {
        return Err(OpeningRefusal::BadRange);
    }
    let least = body_headroom(world).unwrap_or(CLICK_UNITS * 4);
    let y_offset = world
        .rooms
        .get(p.room)
        .ok_or(OpeningRefusal::NoSuchRoom)?
        .y_offset;
    {
        let room = &mut world.rooms[p.room];
        let probe = edge_click(p.cell, p.side, p.from);
        if room.leaf_at(probe).is_some_and(|(k, _)| k.level < 2) {
            split_to_fit(room, ClickRect::of_cells(2, p.cell, p.cell), 2);
        }
        let has = room
            .leaf_at(probe)
            .is_some_and(|(_, s)| s.ground_floor().is_some() && s.ground_ceiling().is_some());
        if !has {
            return Err(OpeningRefusal::NoHost);
        }
        // The coarsest size both ends fall on (the sector, half, quarter).
        let level = (2..=4u8)
            .find(|&l| {
                let e = edge_clicks(l);
                (p.from - side.start) % e == 0 && (p.to - side.start) % e == 0
            })
            .unwrap_or(4);
        for c in p.from..p.to {
            subdivide_to(room, edge_click(p.cell, p.side, c), level)
                .map_err(|_| OpeningRefusal::FineCells)?;
        }
        normalize(room);
    }
    // The leaves along the stretch, in order.
    let mut keys: Vec<LeafKey> = Vec::new();
    let mut c = p.from;
    while c < p.to {
        let (key, _) = world.rooms[p.room]
            .leaf_at(edge_click(p.cell, p.side, c))
            .ok_or(OpeningRefusal::NoHost)?;
        keys.push(key);
        c += key.edge_clicks();
    }
    let mut floor_ref = i32::MIN;
    let mut ceil_min = i32::MAX;
    let mut plan: Vec<(LeafKey, Vec<crate::grid::derive::DerivedWall>)> = Vec::new();
    for &key in &keys {
        let room = &world.rooms[p.room];
        let walls: Vec<_> = derived_walls_of(room, key)
            .into_iter()
            .filter(|w| w.edge == p.side && w.kind != DerivedKind::Slab)
            .collect();
        let sector = room.leaf(key).ok_or(OpeningRefusal::NoHost)?;
        let (Some(f), Some(c)) = (sector.ground_floor(), sector.ground_ceiling()) else {
            return Err(OpeningRefusal::NoHost);
        };
        let fe = crate::grid::edit::walls::surface_edge_heights(f, p.side);
        let ce = crate::grid::edit::walls::surface_edge_heights(c, p.side);
        let authored = !sector.walls(p.side).is_empty();
        if walls.is_empty() && !authored {
            return Err(OpeningRefusal::NotAWall);
        }
        if !walls.is_empty() && walls.iter().all(|w| w.hidden && w.solid == Some(false)) {
            return Err(OpeningRefusal::AlreadyOpen);
        }
        floor_ref = floor_ref.max(fe[0]).max(fe[1]);
        ceil_min = ceil_min.min(ce[0]).min(ce[1]);
        plan.push((key, walls));
    }
    let (top_abs, sill_abs) = match absolute {
        Some((t, s)) => (t - y_offset, s.map(|s| s - y_offset)),
        None => {
            let sill = match p.kind {
                OpeningKind::Door => None,
                OpeningKind::Window { sill } => Some(floor_ref + sill),
            };
            (floor_ref + p.top, sill)
        }
    };
    let clear_from = sill_abs.unwrap_or(floor_ref);
    if matches!(p.kind, OpeningKind::Door) && top_abs - floor_ref < least {
        return Err(OpeningRefusal::TooLow { least });
    }
    if top_abs > ceil_min {
        return Err(OpeningRefusal::TopAboveCeiling {
            max: ceil_min - floor_ref,
        });
    }
    if top_abs <= clear_from {
        return Err(OpeningRefusal::SillAboveTop);
    }
    for (key, walls) in plan {
        let room = &mut world.rooms[p.room];
        // Authored walls on the edge: split around the opening.
        let sector = room.leaf_mut(key).ok_or(OpeningRefusal::NoHost)?;
        let old: Vec<GridWall> = std::mem::take(sector.walls_mut(p.side));
        let mut new_pieces: Vec<GridWall> = Vec::new();
        for w in old {
            let split = pieces(w.heights, sill_abs.map(|s| [s, s]), [top_abs, top_abs]);
            for h in split {
                new_pieces.push(GridWall {
                    heights: h,
                    ..w.clone()
                });
            }
        }
        for dw in &walls {
            let Some(h) = dw.edge_heights() else {
                return Err(OpeningRefusal::NotRectangular);
            };
            if dw.hidden && dw.solid == Some(false) {
                continue;
            }
            if matches!(dw.kind, DerivedKind::Closure | DerivedKind::Drop) {
                let split = pieces(h, sill_abs.map(|s| [s, s]), [top_abs, top_abs]);
                for hh in split {
                    // Lintel and sill are solid: heights are part of the
                    // collision record, so the body passes under the lintel.
                    new_pieces.push(GridWall {
                        heights: hh,
                        material: p.material.or(dw.material),
                        facing: WallFacing::Both,
                        solid: dw.kind == DerivedKind::Closure,
                        ..GridWall::span(0, 0, None)
                    });
                    out.pieces += 1;
                }
            }
        }
        let sector = room.leaf_mut(key).ok_or(OpeningRefusal::NoHost)?;
        if new_pieces.len() > MAX_WALLS_PER_EDGE {
            return Err(OpeningRefusal::TooManyWalls);
        }
        *sector.walls_mut(p.side) = new_pieces;
        for dw in walls {
            room.edge_overrides.edit(dw.key, |o| {
                o.hidden = true;
                if dw.kind == DerivedKind::Closure {
                    o.solid = Some(false);
                }
            });
            out.segments += 1;
        }
    }
    let width = (p.to - p.from) * CLICK_UNITS;
    let mid = p.from * CLICK_UNITS + width / 2;
    let line = |click: i32| click * CLICK_UNITS;
    let (cx, cz) = match p.side {
        WallEdge::North => (mid, line(p.cell[1] * CLICKS_PER_SECTOR)),
        WallEdge::South => (mid, line(p.cell[1] * CLICKS_PER_SECTOR + CLICKS_PER_SECTOR)),
        WallEdge::West => (line(p.cell[0] * CLICKS_PER_SECTOR), mid),
        _ => (line(p.cell[0] * CLICKS_PER_SECTOR + CLICKS_PER_SECTOR), mid),
    };
    let d = p.side.front_direction();
    let plane = DoorPlane {
        room: p.room,
        center: [cx, floor_ref + y_offset, cz],
        axis: if matches!(p.side, WallEdge::North | WallEdge::South) {
            Axis::X
        } else {
            Axis::Z
        },
        through: [-d[0], -d[1]],
        width,
        height: top_abs - clear_from,
    };
    Ok(Some((
        plane,
        top_abs + y_offset,
        sill_abs.map(|s| s + y_offset),
    )))
}

/// Cut a door and return the plane a door entity needs (data only: no logic
/// node is made). A thin wrapper over [`cut_opening`] with a door kind.
pub fn cut_doorway(
    world: &mut GridWorld,
    params: &OpeningParams,
) -> Result<DoorPlane, OpeningRefusal> {
    let mut p = *params;
    p.kind = OpeningKind::Door;
    cut_opening(world, &p).map(|o| o.door.expect("a door reports its plane"))
}
