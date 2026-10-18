//! Derived walls across leaves of different sizes.
//!
//! The same three rules as [`super::derive`] (riser, drop, closure), applied
//! to **segments of the finest common subdivision** of an edge: where a
//! coarse leaf meets two finer ones there are two segments, each with its
//! own pair of profiles, and a leaf larger than a sector is cut at sector
//! borders so every face lies in one sector (the cell it is listed under).
//! A coarse leaf's profile at a segment end is read off its edge
//! (linear, the triangle that holds the edge), so the walls pass through the
//! hanging vertex the floors have there.
//!
//! Faces are made from one leaf's point of view at a time and only the
//! faces that leaf hosts are kept; the other side's faces come from the
//! other leaf's turn. A face is listed under the sector its host leaf lies
//! in, exactly where an authored front-facing wall would sit.

use super::derive::{between_len, open_top, DerivedKind, DerivedWall, Face};
use super::faces_derived::DerivedGeom;
use super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::overrides::{Axis, EdgeKey};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::GridSurface;
use super::tree::neighbours::{across, along, facing, outside_click, runs_x, SIDES};
use super::tree::normalize::{side_corners, side_height, side_triangle};
use super::tree::surface_split::round_div;
use super::units::SECTOR_UNITS;

/// Grid units in one click.
const CLICK: i32 = SECTOR_UNITS / CLICKS_PER_SECTOR;

/// A derived wall of a leaf below or above a sector (or any leaf in a mixed
/// room), with where it lies.
pub(super) struct LeafWall {
    /// The leaf that hosts (sees) the face.
    pub host: LeafKey,
    pub wall: DerivedWall,
    pub geom: DerivedGeom,
}

/// What one side of a segment offers.
#[derive(Clone, Copy, Default)]
struct Side {
    space: bool,
    floor: Option<[i32; 2]>,
    ceil: Option<[i32; 2]>,
}

/// A surface's heights at the two ends of the segment `u` (clicks) of
/// `edge` of the leaf `key`, in increasing click order. `None` when the
/// triangle holding the edge is void.
fn profile(s: &GridSurface, key: LeafKey, edge: WallEdge, u: [i32; 2]) -> Option<[i32; 2]> {
    if s.is_void(side_triangle(s, edge)) {
        return None;
    }
    let [c0, c1] = side_corners(edge);
    let (a, b) = (
        i64::from(side_height(s, edge, c0)),
        i64::from(side_height(s, edge, c1)),
    );
    let (start, len) = along(key, edge);
    let at = |u: i32| {
        round_div(
            a * i64::from(start + len - u) + b * i64::from(u - start),
            i64::from(len),
        ) as i32
    };
    Some([at(u[0]), at(u[1])])
}

fn side_of(sector: Option<&GridSector>, key: LeafKey, edge: WallEdge, u: [i32; 2]) -> Side {
    let Some(s) = sector else {
        return Side::default();
    };
    let floor = s.ground_floor().and_then(|f| profile(f, key, edge, u));
    let ceil = s.ground_ceiling().and_then(|c| profile(c, key, edge, u));
    Side {
        space: floor.is_some() || ceil.is_some(),
        floor,
        ceil,
    }
}

/// Material of the owner's surface at the segment: the last link of the
/// chain.
fn surface_material(
    sector: Option<&GridSector>,
    edge: WallEdge,
    ceiling: bool,
) -> Option<crate::ResourceId> {
    let s = sector?;
    let surface = if ceiling {
        s.ground_ceiling()
    } else {
        s.ground_floor()
    }?;
    surface.tri(side_triangle(surface, edge)).material
}

/// Whether a wall on `edge` walks in the increasing direction.
const fn forward(edge: WallEdge) -> bool {
    matches!(edge, WallEdge::North | WallEdge::East)
}

/// The derived walls hosted by the leaves in `cell`.
pub(super) fn cell_walls(
    room: &GridRoom,
    cell: [i32; 2],
    leaves: &[(LeafKey, &GridSector)],
) -> Vec<LeafWall> {
    let mut out = Vec::new();
    for &(key, sector) in leaves {
        for side in SIDES {
            side_walls(room, cell, key, sector, side, &mut out);
        }
    }
    out
}

fn side_walls(
    room: &GridRoom,
    cell: [i32; 2],
    key: LeafKey,
    sector: &GridSector,
    side: WallEdge,
    out: &mut Vec<LeafWall>,
) {
    let n = key.edge_clicks();
    let (start, _) = along(key, side);
    // The part of the side that lies in this sector, if the leaf's row or
    // column next to the side does.
    let (cell_along, cell_perp) = if runs_x(side) {
        (cell[0] * CLICKS_PER_SECTOR, cell[1] * CLICKS_PER_SECTOR)
    } else {
        (cell[1] * CLICKS_PER_SECTOR, cell[0] * CLICKS_PER_SECTOR)
    };
    let inside = match side {
        WallEdge::North => key.z,
        WallEdge::West => key.x,
        WallEdge::South => key.z + n - 1,
        _ => key.x + n - 1,
    };
    if inside < cell_perp || inside >= cell_perp + CLICKS_PER_SECTOR {
        return;
    }
    let lo = start.max(cell_along);
    let hi = (start + n).min(cell_along + CLICKS_PER_SECTOR);
    if lo >= hi {
        return;
    }
    // Segment ends: the clipped ends and the neighbours' borders.
    let mut breaks = vec![lo, hi];
    for nb in across(&room.leaves, key, side) {
        let (s, len) = along(nb, side);
        breaks.extend([s, s + len].into_iter().filter(|b| *b > lo && *b < hi));
    }
    breaks.sort_unstable();
    breaks.dedup();
    let line = match side {
        WallEdge::North => key.z,
        WallEdge::South => key.z + n,
        WallEdge::West => key.x,
        _ => key.x + n,
    };
    for pair in breaks.windows(2) {
        let u = [pair[0], pair[1]];
        let neighbour = room.leaves.leaf_at(outside_click(key, side, u[0] - start));
        segment_walls(room, cell, (key, sector), neighbour, side, line, u, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn segment_walls(
    room: &GridRoom,
    cell: [i32; 2],
    me: (LeafKey, &GridSector),
    other: Option<(LeafKey, &GridSector)>,
    side: WallEdge,
    line: i32,
    u: [i32; 2],
    out: &mut Vec<LeafWall>,
) {
    let (key, sector) = me;
    let mine = side_of(Some(sector), key, side, u);
    let theirs = match other {
        Some((nk, ns)) => side_of(Some(ns), nk, facing(side), u),
        None => Side::default(),
    };
    // Canonical sides: the north or west one is "lo".
    let me_is_lo = matches!(side, WallEdge::South | WallEdge::East);
    let (lo, hi) = if me_is_lo {
        (mine, theirs)
    } else {
        (theirs, mine)
    };
    let len = (u[1] - u[0]) * CLICK;
    let seg_key = if runs_x(side) {
        EdgeKey {
            x: u[0],
            z: line,
            axis: Axis::X,
            len: (u[1] - u[0]) as u8,
        }
    } else {
        EdgeKey {
            x: line,
            z: u[0],
            axis: Axis::Z,
            len: (u[1] - u[0]) as u8,
        }
    };
    // (kind, host is lo, owner is lo, ring)
    let mut found: Vec<(DerivedKind, bool, bool, Vec<[i32; 2]>)> = Vec::new();
    if let (Some(lf), Some(hf)) = (lo.floor, hi.floor) {
        if let Some(ring) = between_len(hf, lf, lf, len) {
            found.push((DerivedKind::Riser, false, true, ring));
        }
        if let Some(ring) = between_len(lf, hf, lf, len) {
            found.push((DerivedKind::Riser, true, false, ring));
        }
    }
    if let (Some(lc), Some(hc)) = (lo.ceil, hi.ceil) {
        if let Some(ring) = between_len(lc, hc, lc, len) {
            found.push((DerivedKind::Drop, false, true, ring));
        }
        if let Some(ring) = between_len(hc, lc, lc, len) {
            found.push((DerivedKind::Drop, true, false, ring));
        }
    }
    for (is_lo, near, far) in [(true, lo, hi), (false, hi, lo)] {
        if !near.space || far.space {
            continue;
        }
        let Some(floor) = near.floor else { continue };
        let top = match near.ceil {
            Some(ceil) => ceil,
            None if room.defaults.open_wall > 0 => open_top(room, seg_key, floor),
            None => continue,
        };
        if let Some(ring) = between_len(floor, top, floor, len) {
            found.push((DerivedKind::Closure, is_lo, is_lo, ring));
        }
    }
    let [(ax, az), (bx, bz)] = if runs_x(side) {
        [(u[0], line), (u[1], line)]
    } else {
        [(line, u[0]), (line, u[1])]
    }
    .map(|(x, z)| (x * CLICK, z * CLICK));
    // The wall walks the host edge's own direction.
    let (a, b) = if forward(side) {
        ([ax, az], [bx, bz])
    } else {
        ([bx, bz], [ax, az])
    };
    let signed = |p: [i32; 2]| -> f64 {
        f64::from(match side {
            WallEdge::North => p[0],
            WallEdge::East => p[1],
            WallEdge::South => -p[0],
            _ => -p[1],
        })
    };
    for (kind, host_is_lo, owner_is_lo, ring) in found {
        if host_is_lo != me_is_lo {
            continue;
        }
        let (owner_sector, owner_edge, owner_cell) = if owner_is_lo == me_is_lo {
            (Some(sector), side, cell)
        } else {
            match other {
                Some((nk, ns)) => (Some(ns), facing(side), nk.cell()),
                None => (None, facing(side), cell),
            }
        };
        let face = Face {
            room,
            kind,
            host: cell,
            edge: side,
            back: false,
            owner: owner_cell,
            key: seg_key,
            surface: surface_material(owner_sector, owner_edge, kind == DerivedKind::Drop),
        };
        let wall = face.build_with(&ring, !forward(side), len, owner_sector, Some(sector));
        let ys = wall.ring.iter().map(|p| p[1]);
        let run_y = [ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0)];
        let (s0, s1) = (signed(a), signed(b));
        out.push(LeafWall {
            host: key,
            geom: DerivedGeom {
                a,
                b,
                len,
                run_u: [s0.min(s1), s0.max(s1)],
                run_y,
                neighbours: [Vec::new(), Vec::new()],
            },
            wall,
        });
    }
}

/// One derived wall of a room with leaves of several sizes, for drawing in
/// the plan: where it lies and what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallSegment {
    /// Start and end in grid units on the plan, in the host's walking
    /// direction.
    pub a: [i32; 2],
    pub b: [i32; 2],
    /// Which rule made it.
    pub kind: DerivedKind,
    /// The user hid it.
    pub hidden: bool,
    /// It has a collision record.
    pub blocks: bool,
}

/// The derived walls of a room listed under world sector `cell`, as plan
/// segments. Meant for rooms with leaves of several sizes.
pub fn cell_wall_segments(room: &GridRoom, cell: [i32; 2]) -> Vec<WallSegment> {
    let leaves = room.leaves.in_sector(cell);
    cell_walls(room, cell, &leaves)
        .into_iter()
        .map(|w| WallSegment {
            a: w.geom.a,
            b: w.geom.b,
            kind: w.wall.kind,
            hidden: w.wall.hidden,
            blocks: w.wall.blocks(),
        })
        .collect()
}
