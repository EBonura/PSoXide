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

use super::derive::{between, open_top, sector_at, DerivedKind, DerivedWall, Face};
use super::edit::walls::edge_corner_indices;
use super::room::GridRoom;
use super::sector::WallEdge;
use super::surface::{GridSplit, GridSurface};

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

/// Every derived face on the diagonals of `cell`.
pub(super) fn diagonal_walls(room: &GridRoom, cell: [i32; 2]) -> Vec<DerivedWall> {
    let Some(sector) = sector_at(room, cell) else {
        return Vec::new();
    };
    let split_level = |s: &Option<GridSurface>| {
        s.as_ref()
            .is_some_and(|s| s.heights_b.is_some() || s.void != 0)
    };
    if !split_level(&sector.floor) && !split_level(&sector.ceiling) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let face = |kind, edge: WallEdge, back, surface| Face {
        room,
        kind,
        host: cell,
        edge,
        back,
        owner: cell,
        key: super::derive::edge_key(cell, edge),
        surface,
    };
    if let Some(floor) = &sector.floor {
        let edge = fold_edge(floor.split);
        let (a, b) = (profile(floor, 0), profile(floor, 1));
        if floor.void == 0 && a != b {
            // Where A is above B the front triangle owns the strip and the
            // back one hosts it; where B is above A, the other way round.
            if let Some(ring) = between(b, a, a) {
                out.push(
                    face(DerivedKind::Riser, edge, true, floor.tri(0).material).build(&ring, true),
                );
            }
            if let Some(ring) = between(a, b, a) {
                out.push(
                    face(DerivedKind::Riser, edge, false, floor.tri(1).material)
                        .build(&ring, false),
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
            let key = super::derive::edge_key(cell, edge);
            let low = profile(floor, near);
            let top = match &sector.ceiling {
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
            if let Some(ring) = top.and_then(|top| between(low, top, low)) {
                let back = near == 1;
                out.push(
                    face(DerivedKind::Closure, edge, back, floor.tri(near).material)
                        .build(&ring, back),
                );
            }
        }
    }
    if let Some(ceiling) = &sector.ceiling {
        let edge = fold_edge(ceiling.split);
        let (a, b) = (profile(ceiling, 0), profile(ceiling, 1));
        if ceiling.void == 0 && a != b {
            // The lower ceiling owns the strip, the higher side hosts it.
            if let Some(ring) = between(a, b, a) {
                out.push(
                    face(DerivedKind::Drop, edge, true, ceiling.tri(0).material).build(&ring, true),
                );
            }
            if let Some(ring) = between(b, a, a) {
                out.push(
                    face(DerivedKind::Drop, edge, false, ceiling.tri(1).material)
                        .build(&ring, false),
                );
            }
        }
    }
    out
}
