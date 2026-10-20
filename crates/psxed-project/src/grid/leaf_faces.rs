//! The faces of a room whose leaves are not all whole sectors.
//!
//! [`cell_faces_with`](super::faces::cell_faces_with) hands a room with
//! leaves of several sizes to [`cell_faces`], which lists under sector
//! `(x, z)` every face that lies in it:
//!
//! * the floor and ceiling of each leaf that touches the sector. A leaf of
//!   a sector or smaller is one polygon; a larger one is **cut at the
//!   sector border**, each piece a plane sampled at its own corners, so the
//!   pieces meet exactly and the cook's per-sector tables work unchanged.
//! * **stitching** (`docs/grid/level-tools-design.md` 9.3a). Where a leaf has
//!   finer neighbours across a side, the middle of that side is a vertex of
//!   theirs; the polygon gets it too, on its own edge, and a polygon of
//!   more than four corners is fanned from it into triangles. With 2:1
//!   balance there is at most one hanging vertex per side, so the preview and the cook
//!   have no T-junction to leave a one pixel crack.
//! * authored walls of leaves of a sector or smaller.
//! * derived walls per segment ([`super::leaf_derive`]).
//!
//! A leaf of a whole sector is built by exactly the code of a room without
//! leaves of other sizes, so a mixed room's untouched part is face for face
//! what it was.

use super::derive_diag;
use super::faces::{Builder, GridFace, GridFaceKind, Square, UvDefaults};
use super::leaf::{LeafKey, SECTOR_LEVEL};
use super::leaf_derive;
use super::leaf_diag;
use super::overrides::WallMode;
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::GridSurface;
use super::tree::neighbours::{has_finer, SIDES};
use super::tree::surface_split::{half, height_in_leaf};
use super::units::{GridScale, SECTOR_UNITS};
use super::uv::Frame;
use super::world::GridWorld;

/// A vertex hanging on the middle of one side of a square: the side's two
/// ends in grid units (`x`, `z`), whichever way a ring walks them.
#[derive(Clone, Copy, Debug)]
struct Hang {
    ends: [[i32; 2]; 2],
    mid: [i32; 2],
}

/// The sides of `square` that finer leaves lie across, as hanging vertices.
fn hangs(room: &GridRoom, piece: LeafKey, square: Square) -> Vec<Hang> {
    let s = square.size;
    let [x, z] = square.min;
    SIDES
        .into_iter()
        .filter(|side| has_finer(&room.leaves, piece, *side))
        .map(|side| {
            let ends = match side {
                WallEdge::North => [[x, z], [x + s, z]],
                WallEdge::South => [[x, z + s], [x + s, z + s]],
                WallEdge::West => [[x, z], [x, z + s]],
                _ => [[x + s, z], [x + s, z + s]],
            };
            Hang {
                ends,
                mid: [(ends[0][0] + ends[1][0]) / 2, (ends[0][1] + ends[1][1]) / 2],
            }
        })
        .collect()
}

/// Put the hanging vertices into the rings of `faces[from..]` that run
/// along their side, and fan any ring that grew past four corners into
/// triangles. Only floors and ceilings are stitched.
fn stitch(faces: &mut Vec<GridFace>, from: usize, hangs: &[Hang], scale: GridScale) {
    if hangs.is_empty() {
        return;
    }
    let tail: Vec<GridFace> = faces.drain(from..).collect();
    for mut face in tail {
        if !matches!(
            face.kind,
            GridFaceKind::Floor { .. } | GridFaceKind::Ceiling { .. }
        ) {
            faces.push(face);
            continue;
        }
        let mut inserted: Vec<usize> = Vec::new();
        for hang in hangs {
            let n = face.grid.len();
            let (e0, e1, mid) = (hang.ends[0], hang.ends[1], hang.mid);
            let edge = (0..n).find(|&i| {
                let (p, q) = (face.grid[i], face.grid[(i + 1) % n]);
                let ends = [[p[0], p[2]], [q[0], q[2]]];
                (ends == [e0, e1] || ends == [e1, e0])
                    && !face.grid.iter().any(|g| [g[0], g[2]] == mid)
            });
            let Some(i) = edge else { continue };
            let j = (i + 1) % n;
            let (p, q) = (face.grid[i], face.grid[j]);
            let y = half(p[1], q[1]);
            let at = i + 1;
            // The hanging vertex moves with the rock like every other corner.
            let (mx, mz) = (mid[0], mid[1]);
            face.grid.insert(at, [mx, y, mz]);
            face.points.insert(
                at,
                [scale.to_editor(mx), scale.to_editor(y), scale.to_editor(mz)],
            );
            let (tu, tv) = (face.texels[i], face.texels[j]);
            face.texels
                .insert(at, [(tu[0] + tv[0]) / 2.0, (tu[1] + tv[1]) / 2.0]);
            for k in &mut inserted {
                if *k >= at {
                    *k += 1;
                }
            }
            inserted.push(at);
        }
        let n = face.grid.len();
        if n <= 4 || inserted.is_empty() {
            faces.push(face);
            continue;
        }
        // Fan from the first hanging vertex: no triangle is flat, because a
        // side carries at most one.
        let apex = inserted[0];
        for k in 1..n - 1 {
            let ring = [apex, (apex + k) % n, (apex + k + 1) % n];
            faces.push(GridFace {
                points: ring.iter().map(|&i| face.points[i]).collect(),
                grid: ring.iter().map(|&i| face.grid[i]).collect(),
                texels: ring.iter().map(|&i| face.texels[i]).collect(),
                ..face.clone()
            });
        }
    }
}

/// The part of a surface of a leaf above a sector that lies over `square`:
/// the plane sampled at the square's corners. The leaf is one plane, so
/// neighbouring pieces agree at the shared corners.
fn clip(surface: &GridSurface, leaf: LeafKey, square: Square) -> GridSurface {
    let click = SECTOR_UNITS / super::leaf::CLICKS_PER_SECTOR;
    let n = leaf.edge_clicks();
    let at = |e: [i32; 2]| {
        let p = square.at(e);
        height_in_leaf(surface, p[0] / click - leaf.x, p[1] / click - leaf.z, n)
    };
    let mut piece = surface.clone();
    piece.heights = [at([0, 0]), at([1, 0]), at([1, 1]), at([0, 1])];
    piece.heights_b = None;
    piece
}

/// Append the faces listed under local sector `(x, z)` of a room with
/// leaves of several sizes.
pub(super) fn cell_faces(
    world: &GridWorld,
    defaults: &dyn UvDefaults,
    room_index: usize,
    x: usize,
    z: usize,
    out: &mut Vec<GridFace>,
) {
    let Some(r) = world.rooms.get(room_index) else {
        return;
    };
    let cell = r.world_cell(x, z);
    let leaves = r.leaves.in_sector(cell);
    let mut builder = Builder {
        scale: world.scale,
        room: room_index,
        cell,
        leaf: LeafKey::sector(cell),
        span: 0,
        square: Square::sector(cell),
        y_offset: r.y_offset,
        defaults,
        world_uv: world.default_uv(),
        legacy_uv: true,
        frame: Frame::around([[0.0, 0.0]]),
        room_ref: Some(r),
        out,
    };
    for &(key, sector) in &leaves {
        leaf_faces(&mut builder, r, cell, key, sector);
    }
    if r.wall_mode == WallMode::Derived {
        for w in leaf_derive::cell_walls(r, cell, &leaves) {
            builder.leaf = w.host;
            builder.derived_wall_in(r, &w.wall, w.geom);
        }
        // A split-level or void sector keeps its diagonal faces.
        for &(key, _) in leaves.iter().filter(|(k, _)| k.is_sector()) {
            builder.leaf = key;
            builder.square = Square::sector(cell);
            for wall in derive_diag::diagonal_walls(r, cell) {
                builder.derived_wall(r, &wall);
            }
        }
        // So does a leaf below a sector, along its own diagonal.
        for &(key, sector) in leaves.iter().filter(|(k, _)| k.level > SECTOR_LEVEL) {
            builder.leaf = key;
            builder.square = Square::leaf(key);
            for w in leaf_diag::leaf_diag_walls(r, key, sector) {
                builder.derived_wall_in(r, &w.wall, w.geom);
            }
        }
    }
}

fn leaf_faces(
    builder: &mut Builder<'_>,
    room: &GridRoom,
    cell: [i32; 2],
    key: LeafKey,
    sector: &GridSector,
) {
    builder.leaf = key;
    let whole = key.level >= SECTOR_LEVEL;
    // Above a sector the leaf is cut at the sector border; the piece over
    // this sector is what is drawn.
    let square = if whole {
        Square::leaf(key)
    } else {
        Square::sector(cell)
    };
    builder.square = square;
    let piece_key = if whole { key } else { LeafKey::sector(cell) };
    let hanging = hangs(room, piece_key, square);
    let from = builder.out.len();
    if let Some(floor) = sector.ground_floor() {
        builder.surface(
            &if whole {
                floor.clone()
            } else {
                clip(floor, key, square)
            },
            false,
        );
    }
    if let Some(ceiling) = sector.ground_ceiling() {
        builder.surface(
            &if whole {
                ceiling.clone()
            } else {
                clip(ceiling, key, square)
            },
            true,
        );
    }
    let scale = builder.scale;
    stitch(builder.out, from, &hanging, scale);
    if whole {
        for edge in WallEdge::ALL {
            for (index, wall) in sector.walls(edge).iter().enumerate() {
                builder.wall(edge, index, wall);
            }
        }
    }
}
