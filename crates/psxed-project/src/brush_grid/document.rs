//! The grid document as the cook's face source.
//!
//! [`crate::grid::faces::all_faces`] already turns the document into flat
//! polygons with integer editor-unit corners that neighbouring faces share
//! exactly, so this module only adapts them to the pack writer: winding, the
//! lattice the cells sit on, per-room ambient light and double-sided walls.

use crate::grid::faces::{all_faces, GridFace};
use crate::grid::units::SECTOR_UNITS;
use crate::grid::GridWorld;

use super::{Face, FaceMaterial, FaceSet};

/// The pack's cells are whole PXBSP units wide, so a sector must come out as
/// a multiple of this many editor units.
const ENGINE_UNIT: i32 = 16;

/// A face in the pack writer's convention.
///
/// A [`GridFace`] is wound clockwise seen from its visible side, so the
/// right-handed cross product of its ring points away from the viewer. The
/// pack writer wants the cross product to point at the visible side, which
/// is the same ring reversed.
fn pack_face(face: &GridFace, min_cell: [i32; 2], back: bool) -> Face {
    let mut ring: Vec<[f64; 3]> = face.points.iter().map(|p| p.map(f64::from)).collect();
    let mut texels = face.texels.clone();
    if !back {
        ring.reverse();
        texels.reverse();
    }
    Face {
        ring,
        material: FaceMaterial::Id(face.material),
        cell: (
            (face.cell[0] - min_cell[0]) as usize,
            (face.cell[1] - min_cell[1]) as usize,
        ),
        room: face.room,
        texels: Some(texels),
    }
}

/// Check the world can sit on the pack's lattice and say why not.
pub fn check_lattice(world: &GridWorld) -> Result<i32, String> {
    let scale = world.scale;
    let exact = i64::from(SECTOR_UNITS) * i64::from(scale.num) % i64::from(scale.den.max(1)) == 0;
    let sector = world.sector_editor_units();
    if !exact || sector % ENGINE_UNIT != 0 {
        return Err(format!(
            "a sector is {sector} editor units at scale {}/{}; it must be a whole multiple of {ENGINE_UNIT} so cells fall on the world lattice",
            scale.num, scale.den
        ));
    }
    Ok(sector)
}

/// Faces, cell grid and ambient light of `world`.
pub(super) fn face_set(world: &GridWorld) -> Result<FaceSet, String> {
    let cell_editor = check_lattice(world)?;
    let mut min = [i32::MAX; 2];
    let mut max = [i32::MIN; 2];
    for room in &world.rooms {
        for (x, z, _) in room.iter() {
            let cell = room.world_cell(x, z);
            for axis in 0..2 {
                min[axis] = min[axis].min(cell[axis]);
                max[axis] = max[axis].max(cell[axis]);
            }
        }
    }
    if min[0] > max[0] {
        return Err("the world has no sectors".to_string());
    }
    let grid = (
        (max[0] - min[0] + 1) as usize,
        (max[1] - min[1] + 1) as usize,
    );
    let mut faces = Vec::new();
    for face in all_faces(world) {
        faces.push(pack_face(&face, min, false));
        if face.double_sided {
            // The runtime culls back faces per quad: a two-sided wall is two
            // quads with opposite windings.
            faces.push(pack_face(&face, min, true));
        }
    }
    Ok(FaceSet {
        faces,
        origin_editor: (
            world.scale.to_editor(min[0].saturating_mul(SECTOR_UNITS)),
            world.scale.to_editor(min[1].saturating_mul(SECTOR_UNITS)),
        ),
        grid,
        cell_editor,
        room_ambient: world.rooms.iter().map(|room| room.ambient).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::{GridSurface, GridWall, WallFacing};
    use crate::grid::units::GridScale;
    use crate::grid::WallEdge;

    fn world(configure: impl FnOnce(&mut GridRoom)) -> GridWorld {
        let mut room = GridRoom::new("r", 2, 1);
        configure(&mut room);
        GridWorld {
            scale: GridScale { num: 21, den: 16 },
            rooms: vec![room],
        }
    }

    fn cross_up(face: &Face) -> f64 {
        let p = |i: usize| face.ring[i];
        let (a, b, c) = (p(0), p(1), p(2));
        (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2])
    }

    #[test]
    fn floors_wind_so_the_cross_product_points_up() {
        let w = world(|r| {
            r.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        });
        let set = face_set(&w).unwrap();
        assert_eq!(set.faces.len(), 1);
        let normal = super::super::face_normal(&set.faces[0].ring);
        assert!(normal[1] > 0.99, "{normal:?}");
        assert!(cross_up(&set.faces[0]).abs() > 0.0);
    }

    #[test]
    fn the_lattice_origin_and_cell_grid_follow_the_sectors() {
        let w = world(|r| {
            r.origin = [3, -2];
            r.sector_or_insert(1, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        });
        let set = face_set(&w).unwrap();
        assert_eq!(set.grid, (1, 1));
        assert_eq!(set.cell_editor, 1344);
        assert_eq!(set.origin_editor, (4 * 1344, -2 * 1344));
        assert_eq!(set.faces[0].cell, (0, 0));
    }

    #[test]
    fn two_sided_walls_become_two_opposite_quads() {
        let w = world(|r| {
            let mut wall = GridWall::span(0, 256, None);
            wall.facing = WallFacing::Both;
            r.sector_or_insert(0, 0)
                .unwrap()
                .walls_mut(WallEdge::North)
                .push(wall);
        });
        let set = face_set(&w).unwrap();
        assert_eq!(set.faces.len(), 2);
        let a = super::super::face_normal(&set.faces[0].ring);
        let b = super::super::face_normal(&set.faces[1].ring);
        assert!(a[2] * b[2] < -0.9, "{a:?} {b:?}");
    }

    #[test]
    fn an_off_lattice_scale_is_refused_with_a_reason() {
        let mut w = world(|r| {
            r.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        });
        w.scale = GridScale { num: 5, den: 3 };
        let error = face_set(&w).err().unwrap();
        assert!(error.contains("multiple of 16"), "{error}");
    }

    #[test]
    fn an_empty_world_is_refused() {
        let w = GridWorld::new();
        assert!(face_set(&w).err().unwrap().contains("no sectors"));
    }

    #[test]
    fn room_ambient_travels_with_the_faces() {
        let w = world(|r| {
            r.ambient = [10, 20, 30];
            r.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        });
        let set = face_set(&w).unwrap();
        assert_eq!(set.room_ambient, vec![[10, 20, 30]]);
        assert_eq!(set.faces[0].room, 0);
    }
}
