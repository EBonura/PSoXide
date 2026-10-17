//! The drawable faces of free planes.
//!
//! A plane in [`PlaneUvMode::Tile`] is cut into pieces at every sector of
//! its width and height, measured from its top-left corner. That keeps each
//! piece within one texture repeat (the renderer's packet UVs are bytes),
//! gives the light bake corners to work with and matches the way walls are
//! banded. A plane in [`PlaneUvMode::Stretch`] is one quad.
//!
//! Every corner is [`GridPlane::point_grid`], rounded to grid units once, so
//! neighbouring pieces of one plane share corners exactly and the cook welds
//! them like any other vertex.

use crate::face_uv::{FaceUv, UvMode};

use super::faces::{editor_point, Builder, Corner, GridFace, GridFaceKind, UvDefaults};
use super::plane::{GridPlane, PlaneUvMode};
use super::units::{SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::uv::{Frame, Mapper};
use super::world::GridWorld;

/// Texels one whole texture spans in [`PlaneUvMode::Stretch`] (the grid
/// renderer tiles 64 by 64 textures).
pub const STRETCH_TEXELS: f64 = 64.0;

/// The tiling mode a plane uses: its own shared-model mode when set, else
/// the legacy Tile (world aligned at its top-left corner) or Stretch (one
/// whole texture across).
pub fn effective_mode(plane: &GridPlane) -> UvMode {
    match plane.uv.mode {
        UvMode::Inherit => match plane.uv_mode {
            PlaneUvMode::Tile => UvMode::LEGACY,
            PlaneUvMode::Stretch => UvMode::fit(1, 1),
        },
        own => own,
    }
}

/// Cut positions along an axis of `length` grid units, from 0 to `length`:
/// at every sector for a world mapping, so each piece stays inside one
/// texture repeat and the light bake has corners to work with.
fn cuts(length: i32, mode: UvMode) -> Vec<f64> {
    let mut out = vec![0.0];
    if matches!(mode, UvMode::World { .. }) {
        let mut at = SECTOR_UNITS;
        while at < length {
            out.push(f64::from(at));
            at += SECTOR_UNITS;
        }
    }
    out.push(f64::from(length));
    out
}

/// Append the faces of plane `index` of `room`. A missing or disabled plane
/// appends nothing.
pub fn plane_faces(world: &GridWorld, room: usize, index: usize, out: &mut Vec<GridFace>) {
    plane_faces_with(world, &(), room, index, out);
}

/// [`plane_faces`] with the materials' tiling defaults (planes name their
/// mode, so they never take one; kept for one signature across face kinds).
pub fn plane_faces_with(
    world: &GridWorld,
    defaults: &dyn UvDefaults,
    room: usize,
    index: usize,
    out: &mut Vec<GridFace>,
) {
    let Some(r) = world.rooms.get(room) else {
        return;
    };
    let Some(plane) = r.planes.get(index).filter(|p| p.enabled) else {
        return;
    };
    let first = out.len();
    let mut builder = Builder {
        scale: world.scale,
        room,
        cell: plane.center_cell(),
        y_offset: r.y_offset,
        defaults,
        legacy_uv: false,
        room_ref: None,
        frame: Frame::around([[0.0, 0.0]]),
        jitter: None,
        tint: None,
        out,
    };
    let (scale, y_offset) = (builder.scale, builder.y_offset);
    let mode = effective_mode(plane);
    // The frame is the plane rectangle, measured from its top-left corner.
    let frame = Frame {
        min: [0.0, 0.0],
        max: [f64::from(plane.size[0]), f64::from(plane.size[1])],
    };
    // Legacy centre of a world mapping: half the plane at the default
    // density.
    let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
    let legacy_centre = [
        f64::from(plane.size[0]) / 2.0 * texel,
        f64::from(plane.size[1]) / 2.0 * texel,
    ];
    let uv = FaceUv { mode, ..plane.uv };
    let mapper = Mapper::new(&uv, None, frame, legacy_centre);
    builder.legacy_uv = mapper.is_legacy();
    builder.frame = frame;
    let corner = |s: f64, t: f64| {
        let g = plane.point_grid(s, t);
        Corner {
            p: editor_point(scale, y_offset, g[0], g[1], g[2]),
            g: [g[0], y_offset + g[1], g[2]],
            uv: mapper.texel([s, t]),
        }
    };
    let (us, vs) = (cuts(plane.size[0], mode), cuts(plane.size[1], mode));
    let toward = plane.normal();
    for v in vs.windows(2) {
        for u in us.windows(2) {
            let ring = vec![
                corner(u[0], v[0]),
                corner(u[1], v[0]),
                corner(u[1], v[1]),
                corner(u[0], v[1]),
            ];
            builder.push(
                GridFaceKind::Plane { index },
                ring,
                toward,
                plane.material,
                plane.double_sided,
            );
        }
    }
    for face in &mut out[first..] {
        face.unlit = plane.fullbright;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::units::GridScale;

    fn world_with(plane: GridPlane) -> GridWorld {
        let mut room = GridRoom::new("r", 4, 4);
        room.planes.push(plane);
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn faces(plane: GridPlane) -> Vec<GridFace> {
        let world = world_with(plane);
        let mut out = Vec::new();
        plane_faces(&world, 0, 0, &mut out);
        out
    }

    #[test]
    fn a_tiled_plane_is_cut_at_every_sector_and_shares_corners() {
        let plane = GridPlane::new([1536, 1024, 512], [3072, 2048]);
        let out = faces(plane);
        // Three by two sectors.
        assert_eq!(out.len(), 6);
        let mut corners: Vec<[i32; 3]> = out.iter().flat_map(|f| f.points.clone()).collect();
        corners.sort();
        corners.dedup();
        assert_eq!(corners.len(), 4 * 3);
        // Texels run 32 per sector from the top-left corner.
        let top_left = out
            .iter()
            .find(|f| f.points.contains(&[0, 2048, 512]))
            .unwrap();
        let min_u = top_left
            .texels
            .iter()
            .map(|t| t[0])
            .fold(f64::MAX, f64::min);
        let max_u = top_left
            .texels
            .iter()
            .map(|t| t[0])
            .fold(f64::MIN, f64::max);
        assert_eq!((min_u, max_u), (0.0, 32.0));
    }

    #[test]
    fn a_stretched_plane_is_one_quad_with_one_texture_across() {
        let mut plane = GridPlane::new([1024, 1024, 0], [2048, 1024]);
        plane.uv_mode = PlaneUvMode::Stretch;
        let out = faces(plane);
        assert_eq!(out.len(), 1);
        let us: Vec<f64> = out[0].texels.iter().map(|t| t[0]).collect();
        let vs: Vec<f64> = out[0].texels.iter().map(|t| t[1]).collect();
        assert_eq!(us.iter().cloned().fold(f64::MIN, f64::max), STRETCH_TEXELS);
        assert_eq!(vs.iter().cloned().fold(f64::MIN, f64::max), STRETCH_TEXELS);
        assert_eq!(us.iter().cloned().fold(f64::MAX, f64::min), 0.0);
    }

    #[test]
    fn faces_look_along_the_plane_normal_and_carry_the_flags() {
        let mut plane = GridPlane::new([512, 512, 512], [1024, 1024]);
        plane.rotation = [90, 0, 0];
        plane.double_sided = true;
        plane.fullbright = true;
        let out = faces(plane);
        assert_eq!(out.len(), 1);
        assert!(out[0].normal[0] > 0.99, "{:?}", out[0].normal);
        assert!(out[0].double_sided && out[0].unlit);
        assert_eq!(out[0].kind, GridFaceKind::Plane { index: 0 });
    }

    #[test]
    fn a_flat_plane_faces_up_and_a_flipped_one_down() {
        let mut plane = GridPlane::new([512, 256, 512], [1024, 1024]);
        plane.rotation = [0, 90, 0];
        assert!(faces(plane.clone())[0].normal[1] > 0.99);
        plane.rotation = [0, -90, 0];
        assert!(faces(plane)[0].normal[1] < -0.99);
    }

    #[test]
    fn the_room_offset_lifts_the_grid_coordinates_and_hidden_planes_vanish() {
        let mut plane = GridPlane::new([512, 0, 512], [1024, 1024]);
        plane.rotation = [0, 90, 0];
        let mut world = world_with(plane);
        world.rooms[0].y_offset = 2048;
        let mut out = Vec::new();
        plane_faces(&world, 0, 0, &mut out);
        assert!(out[0].grid.iter().all(|g| g[1] == 2048));
        world.rooms[0].planes[0].enabled = false;
        out.clear();
        plane_faces(&world, 0, 0, &mut out);
        assert!(out.is_empty());
    }
}
