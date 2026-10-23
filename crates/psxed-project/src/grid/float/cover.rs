//! The height of a grid world's ground over the ray lattice of a model.
//!
//! The model is sampled as vertical rays, [`COLUMNS_PER_SECTOR`] to a sector
//! each way. Rasterising the upward faces of a world on the same lattice
//! gives, per ray, the height of the highest floor over it: the number to
//! hold against the top of the solid the model has there.

use std::collections::BTreeMap;

use super::columns::COLUMNS_PER_SECTOR;
use crate::grid::faces::{cell_faces_with, GridFace};
use crate::grid::world::GridWorld;

/// The faces of the cells of every room: the ground fit alone, without the
/// planes and the static meshes (slabs) placed over it.
pub fn ground_faces(world: &GridWorld) -> Vec<GridFace> {
    let mut out = Vec::new();
    for (room_index, room) in world.rooms.iter().enumerate() {
        for [x, z] in room.face_cells() {
            cell_faces_with(world, &(), room_index, x, z, &mut out);
        }
    }
    out
}

/// Highest upward face over each ray column, by the column's world index
/// `(x, z)`, in editor units. `sector_editor` is the editor length of a
/// sector (1280 at the default scale).
pub fn raster_tops(faces: &[GridFace], sector_editor: f64) -> BTreeMap<(i32, i32), f64> {
    let per = COLUMNS_PER_SECTOR as f64;
    let mut top: BTreeMap<(i32, i32), f64> = BTreeMap::new();
    for f in faces.iter().filter(|f| f.normal[1] > 0.02) {
        let p: Vec<[f64; 3]> = f
            .points
            .iter()
            .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
            .collect();
        for t in 1..p.len() - 1 {
            let tri = [p[0], p[t], p[t + 1]];
            let col = |v: f64| (v / sector_editor * per).floor() as i32;
            let (x0, x1) = (
                col(tri.iter().map(|v| v[0]).fold(f64::MAX, f64::min)),
                col(tri.iter().map(|v| v[0]).fold(f64::MIN, f64::max)),
            );
            let (z0, z1) = (
                col(tri.iter().map(|v| v[2]).fold(f64::MAX, f64::min)),
                col(tri.iter().map(|v| v[2]).fold(f64::MIN, f64::max)),
            );
            for cz in z0..=z1 {
                for cx in x0..=x1 {
                    let (x, z) = (
                        (f64::from(cx) + 0.5) / per * sector_editor,
                        (f64::from(cz) + 0.5) / per * sector_editor,
                    );
                    // Barycentric test in plan, then the plane's height.
                    let d = (tri[1][2] - tri[2][2]) * (tri[0][0] - tri[2][0])
                        + (tri[2][0] - tri[1][0]) * (tri[0][2] - tri[2][2]);
                    if d.abs() < 1e-9 {
                        continue;
                    }
                    let a = ((tri[1][2] - tri[2][2]) * (x - tri[2][0])
                        + (tri[2][0] - tri[1][0]) * (z - tri[2][2]))
                        / d;
                    let b = ((tri[2][2] - tri[0][2]) * (x - tri[2][0])
                        + (tri[0][0] - tri[2][0]) * (z - tri[2][2]))
                        / d;
                    let c = 1.0 - a - b;
                    if a < -1e-6 || b < -1e-6 || c < -1e-6 {
                        continue;
                    }
                    let y = a * tri[0][1] + b * tri[1][1] + c * tri[2][1];
                    let e = top.entry((cx, cz)).or_insert(f64::MIN);
                    *e = e.max(y);
                }
            }
        }
    }
    top
}

/// [`raster_tops`] of the ground fit of `world` alone, in grid units.
pub fn ground_tops(world: &GridWorld) -> BTreeMap<(i32, i32), f64> {
    let sector_editor = f64::from(world.scale.to_editor(1024));
    let to_grid = 1024.0 / sector_editor;
    raster_tops(&ground_faces(world), sector_editor)
        .into_iter()
        .map(|(key, y)| (key, y * to_grid))
        .collect()
}
