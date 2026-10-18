//! Derived walls along the fold diagonal of one leaf.
//!
//! The same rules as [`super::derive_diag`] (riser and drop between the two
//! triangles of a split-level surface, closure of the triangle next to a void
//! one), through the same code ([`derive_diag::segment_walls`]), but for a
//! leaf of a sector or smaller: the segment is the leaf's own diagonal, `n`
//! clicks long in plan, its key is the leaf's corner and size, and the
//! material chain reads the leaf. A leaf above a sector is one plane and has
//! no diagonal wall.

use super::derive::{DerivedKind, DerivedWall};
use super::derive_diag::{has_diagonal_faces, segment_walls, DiagSegment};
use super::faces_derived::DerivedGeom;
use super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::leaf_derive::LeafWall;
use super::room::GridRoom;
use super::sector::GridSector;
use super::units::SECTOR_UNITS;

/// Grid units in one click.
const CLICK: i32 = SECTOR_UNITS / CLICKS_PER_SECTOR;

/// Every derived face on the fold diagonals of the leaf `key`, which must be
/// a sector or smaller.
pub(crate) fn leaf_diag_walls(room: &GridRoom, key: LeafKey, sector: &GridSector) -> Vec<LeafWall> {
    if key.level < super::leaf::SECTOR_LEVEL || !has_diagonal_faces(sector) {
        return Vec::new();
    }
    let len = key.edge_clicks() * CLICK;
    let min = [key.x * CLICK, key.z * CLICK];
    let mut out: Vec<DerivedWall> = Vec::new();
    segment_walls(
        &DiagSegment {
            room,
            cell: key.cell(),
            sector,
            len,
            leaf: Some(key),
        },
        &mut out,
    );
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
pub fn diagonal_face_count(room: &GridRoom, key: LeafKey, kind: Option<DerivedKind>) -> usize {
    room.leaf(key).map_or(0, |sector| {
        leaf_diag_walls(room, key, sector)
            .iter()
            .filter(|w| kind.is_none_or(|k| w.wall.kind == k))
            .count()
    })
}
