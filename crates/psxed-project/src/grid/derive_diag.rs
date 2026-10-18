//! Derived walls along the diagonal of one sector.
//!
//! The same three rules as [`super::derive`], with the two triangles of a
//! surface as the two sides of the edge. The fold diagonal of a surface is the
//! edge its split names ([`GridSplit::NwSe`] runs `NW -> SE`, [`GridSplit::NeSw`]
//! runs `NE -> SW`); triangle 0 always lies on the front side of that edge
//! (see [`WallEdge::front_direction`]) and triangle 1 on the back.
//!
//! * **R1d riser**: a split-level floor whose triangles meet the diagonal at
//!   different heights. The higher triangle owns the strip, the lower one
//!   hosts it. Equal profiles (a pure fold) make nothing.
//! * **R2d drop**: the same for a split-level ceiling; the lower triangle owns
//!   it and the higher one hosts it.
//! * **R3d closure**: exactly one floor triangle is void. The other closes
//!   itself along the diagonal, from its floor to its ceiling (or the room's
//!   open wall height).
//!
//! Profiles run `a -> b` of the edge. A face seen from the back of the edge
//! walks `b -> a`, which [`DerivedWall::back`] records.

use super::derive::{between_len, edge_key, open_top, sector_at, DerivedKind, DerivedWall, Face};
use super::edit::walls::edge_corner_indices;
use super::leaf::LeafKey;
use super::overrides::{Axis, EdgeKey};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::{GridSplit, GridSurface};
use super::units::SECTOR_UNITS;

/// The edge a surface's split folds along.
pub const fn fold_edge(split: GridSplit) -> WallEdge {
    match split {
        GridSplit::NwSe => WallEdge::Nwse,
        GridSplit::NeSw => WallEdge::Nesw,
    }
}

/// Heights of triangle `tri` at the two ends of the fold diagonal.
fn profile(surface: &GridSurface, tri: usize) -> [i32; 2] {
    let [a, b] = edge_corner_indices(fold_edge(surface.split));
    let h = surface.heights_for(tri);
    [h[a], h[b]]
}

/// Whether a surface has anything to say along its diagonal: two heights or
/// a void triangle.
fn is_split_level(surface: Option<&GridSurface>) -> bool {
    surface.is_some_and(|s| s.heights_b.is_some() || s.void != 0)
}

/// Whether any span of `sector` is split-level, so its diagonals can make
/// a face.
pub(super) fn has_diagonal_faces(sector: &GridSector) -> bool {
    sector
        .spans()
        .iter()
        .any(|span| is_split_level(span.floor.as_ref()) || is_split_level(span.ceiling.as_ref()))
}

/// Where a diagonal segment lies: the one place the whole-sector and the
/// per-leaf derivations differ.
#[derive(Clone, Copy)]
pub(super) struct DiagSegment<'a> {
    /// Room the segment is in.
    pub room: &'a GridRoom,
    /// World sector the segment is hosted by.
    pub cell: [i32; 2],
    /// The leaf's column; the material chain reads it.
    pub sector: &'a GridSector,
    /// Length of the diagonal's projection, grid units: a sector's edge for
    /// a whole sector, less for a finer leaf.
    pub len: i32,
    /// The leaf, when the segment is one below a whole sector (or a sector in
    /// a room of cells); it addresses the segment.
    pub leaf: Option<LeafKey>,
}

impl<'a> DiagSegment<'a> {
    /// A whole sector at `cell`.
    pub(super) fn sector(room: &'a GridRoom, cell: [i32; 2], sector: &'a GridSector) -> Self {
        Self {
            room,
            cell,
            sector,
            len: SECTOR_UNITS,
            leaf: None,
        }
    }

    /// The address of the segment on `edge`: an override's key.
    pub(super) fn key(&self, edge: WallEdge) -> EdgeKey {
        match self.leaf {
            None => edge_key(self.cell, edge),
            Some(key) => EdgeKey {
                x: key.x,
                z: key.z,
                axis: if edge == WallEdge::Nwse {
                    Axis::NwSe
                } else {
                    Axis::NeSw
                },
                len: key.edge_clicks() as u8,
            },
        }
    }
}

/// Every derived face on the diagonals of `cell`.
pub(super) fn diagonal_walls(room: &GridRoom, cell: [i32; 2]) -> Vec<DerivedWall> {
    let Some(sector) = sector_at(room, cell) else {
        return Vec::new();
    };
    if !has_diagonal_faces(sector) {
        return Vec::new();
    }
    let mut out = Vec::new();
    segment_walls(&DiagSegment::sector(room, cell, sector), &mut out);
    out
}

/// The derived faces on the diagonal of one segment, every span of its
/// column: a half cell closes itself on each storey. The whole-sector and
/// the per-leaf derivations both come through here.
pub(super) fn segment_walls(seg: &DiagSegment<'_>, out: &mut Vec<DerivedWall>) {
    for (index, span) in seg.sector.spans().iter().enumerate() {
        span_diagonal_walls(
            seg,
            index as u8,
            span.floor.as_ref(),
            span.ceiling.as_ref(),
            out,
        );
    }
}

/// The diagonal faces of one span of the column of `seg`.
fn span_diagonal_walls(
    seg: &DiagSegment<'_>,
    index: u8,
    floor: Option<&GridSurface>,
    ceiling: Option<&GridSurface>,
    out: &mut Vec<DerivedWall>,
) {
    let DiagSegment {
        room,
        cell,
        sector,
        len,
        ..
    } = *seg;
    let face = |kind, edge: WallEdge, back, surface| Face {
        room,
        kind,
        host: cell,
        edge,
        back,
        owner: cell,
        span: index,
        key: seg.key(edge),
        surface,
    };
    let mut push = |face: Face<'_>, ring: &[[i32; 2]], flip: bool| {
        out.push(face.build_with(ring, flip, len, Some(sector), Some(sector)));
    };
    if let Some(floor) = floor {
        let edge = fold_edge(floor.split);
        let (a, b) = (profile(floor, 0), profile(floor, 1));
        if floor.void == 0 && a != b {
            // Where A is above B the front triangle owns the strip and the
            // back one hosts it; where B is above A, the other way round.
            if let Some(ring) = between_len(b, a, a, len) {
                push(
                    face(DerivedKind::Riser, edge, true, floor.tri(0).material),
                    &ring,
                    true,
                );
            }
            if let Some(ring) = between_len(a, b, a, len) {
                push(
                    face(DerivedKind::Riser, edge, false, floor.tri(1).material),
                    &ring,
                    false,
                );
            }
        }
        // R3d: one void triangle, the other closes itself.
        let near = match floor.void & 3 {
            super::surface::VOID_A => Some(1),
            super::surface::VOID_B => Some(0),
            _ => None,
        };
        if let Some(near) = near {
            let key = seg.key(edge);
            let low = profile(floor, near);
            let top = match ceiling {
                Some(ceiling) if !ceiling.is_void(near) => {
                    let tri = if ceiling.split == floor.split {
                        near
                    } else {
                        0
                    };
                    Some(profile(ceiling, tri))
                }
                _ if room.defaults.open_wall > 0 => Some(open_top(room, key, low)),
                _ => None,
            };
            if let Some(ring) = top.and_then(|top| between_len(low, top, low, len)) {
                let back = near == 1;
                push(
                    face(DerivedKind::Closure, edge, back, floor.tri(near).material),
                    &ring,
                    back,
                );
            }
        }
    }
    if let Some(ceiling) = ceiling {
        let edge = fold_edge(ceiling.split);
        let (a, b) = (profile(ceiling, 0), profile(ceiling, 1));
        if ceiling.void == 0 && a != b {
            // The lower ceiling owns the strip, the higher side hosts it.
            if let Some(ring) = between_len(a, b, a, len) {
                push(
                    face(DerivedKind::Drop, edge, true, ceiling.tri(0).material),
                    &ring,
                    true,
                );
            }
            if let Some(ring) = between_len(b, a, a, len) {
                push(
                    face(DerivedKind::Drop, edge, false, ceiling.tri(1).material),
                    &ring,
                    false,
                );
            }
        }
    }
}
