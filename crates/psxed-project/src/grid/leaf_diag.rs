//! Derived walls along the fold diagonal of one leaf.
//!
//! The same three rules as [`super::derive_diag`] (riser and drop between the
//! two triangles of a split-level surface, closure of the triangle next to a
//! void one), but for a leaf of a sector or smaller: the segment is the leaf's
//! own diagonal, `n` clicks long in plan, its key is the leaf's corner and
//! size, and the material chain reads the leaf. A leaf above a sector is one
//! plane and has no diagonal wall.

use super::derive::{between_len, open_top, DerivedKind, DerivedWall, Face};
use super::derive_diag::fold_edge;
use super::edit::walls::edge_corner_indices;
use super::faces_derived::DerivedGeom;
use super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::leaf_derive::LeafWall;
use super::overrides::{Axis, EdgeKey};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::{GridSurface, VOID_A, VOID_B};
use super::units::SECTOR_UNITS;

/// Grid units in one click.
const CLICK: i32 = SECTOR_UNITS / CLICKS_PER_SECTOR;

fn profile(surface: &GridSurface, tri: usize) -> [i32; 2] {
    let [a, b] = edge_corner_indices(fold_edge(surface.split));
    let h = surface.heights_for(tri);
    [h[a], h[b]]
}

/// The key of the diagonal of `key` on the edge `edge`.
fn diag_key(key: LeafKey, edge: WallEdge) -> EdgeKey {
    EdgeKey {
        x: key.x,
        z: key.z,
        axis: if edge == WallEdge::Nwse {
            Axis::NwSe
        } else {
            Axis::NeSw
        },
        len: key.edge_clicks() as u8,
    }
}

/// Every derived face on the fold diagonals of the leaf `key`, which must be
/// a sector or smaller.
pub(crate) fn leaf_diag_walls(room: &GridRoom, key: LeafKey, sector: &GridSector) -> Vec<LeafWall> {
    let split_level =
        |s: Option<&GridSurface>| s.is_some_and(|s| s.heights_b.is_some() || s.void != 0);
    if key.level < super::leaf::SECTOR_LEVEL
        || (!split_level(sector.ground_floor()) && !split_level(sector.ground_ceiling()))
    {
        return Vec::new();
    }
    let cell = key.cell();
    let len = key.edge_clicks() * CLICK;
    let min = [key.x * CLICK, key.z * CLICK];
    let face = |kind, edge: WallEdge, back, surface| Face {
        room,
        kind,
        host: cell,
        edge,
        back,
        owner: cell,
        key: diag_key(key, edge),
        surface,
    };
    let mut out: Vec<DerivedWall> = Vec::new();
    let mut push = |face: Face<'_>, ring: &[[i32; 2]], flip: bool| {
        out.push(face.build_with(ring, flip, len, Some(sector), Some(sector)));
    };
    if let Some(floor) = sector.ground_floor() {
        let edge = fold_edge(floor.split);
        let (a, b) = (profile(floor, 0), profile(floor, 1));
        if floor.void == 0 && a != b {
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
        let near = match floor.void & 3 {
            VOID_A => Some(1),
            VOID_B => Some(0),
            _ => None,
        };
        if let Some(near) = near {
            let k = diag_key(key, edge);
            let low = profile(floor, near);
            let top = match sector.ground_ceiling() {
                Some(ceiling) if !ceiling.is_void(near) => {
                    let tri = if ceiling.split == floor.split {
                        near
                    } else {
                        0
                    };
                    Some(profile(ceiling, tri))
                }
                _ if room.defaults.open_wall > 0 => Some(open_top(room, k, low)),
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
    if let Some(ceiling) = sector.ground_ceiling() {
        let edge = fold_edge(ceiling.split);
        let (a, b) = (profile(ceiling, 0), profile(ceiling, 1));
        if ceiling.void == 0 && a != b {
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
    out.into_iter()
        .map(|wall| {
            let (ea, eb) = wall.edge.endpoints();
            let (ea, eb) = if wall.back { (eb, ea) } else { (ea, eb) };
            let at = |e: [i32; 2]| [min[0] + e[0] * len, min[1] + e[1] * len];
            let ys = wall.ring.iter().map(|p| p[1]);
            let run_y = [ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0)];
            LeafWall {
                host: key,
                geom: DerivedGeom {
                    a: at(ea),
                    b: at(eb),
                    len,
                    // The whole diagonal, as the sector version measures it.
                    run_u: [0.0, f64::from(len) * core::f64::consts::SQRT_2],
                    run_y,
                    neighbours: [Vec::new(), Vec::new()],
                },
                wall,
            }
        })
        .collect()
}

/// How many derived diagonal faces the leaf `key` has, of one rule or of
/// all of them (for tools that report what an edit made).
pub fn diagonal_face_count(
    room: &GridRoom,
    key: LeafKey,
    kind: Option<super::derive::DerivedKind>,
) -> usize {
    room.leaf(key).map_or(0, |sector| {
        leaf_diag_walls(room, key, sector)
            .iter()
            .filter(|w| kind.is_none_or(|k| w.wall.kind == k))
            .count()
    })
}
