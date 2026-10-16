//! Per-corner light bake of grid faces.
//!
//! Uses the same formula as the brush bake (ambient plus lambert-weighted
//! linear falloff, modulated by the material tint), evaluated at each corner
//! of a [`GridFace`]. There are no shadow occluders yet: a grid world's
//! walls and floors are surfaces, not solids, and an occluder source for the
//! cook is part of the later cook phase. Lights are in the same editor units
//! as the faces.

use crate::brush_light::{lit_point_color, BrushPointLight};

use super::faces::GridFace;

/// Lit colour of every corner of `face`.
///
/// `tint` is the material's modulation colour (`[128; 3]` leaves the texture
/// as authored) and `ambient` the room's ambient light.
pub fn bake_face_corners(
    face: &GridFace,
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[BrushPointLight],
) -> Vec<[u8; 3]> {
    face.points
        .iter()
        .map(|p| lit_point_color(p.map(f64::from), face.normal, tint, ambient, lights, &[]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::faces::all_faces;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;
    use crate::grid::world::GridWorld;

    fn floor_face() -> GridFace {
        let mut room = GridRoom::new("r", 1, 1);
        room.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
        };
        all_faces(&world).remove(0)
    }

    #[test]
    fn unlit_faces_show_ambient_modulated_by_tint() {
        let face = floor_face();
        let colors = bake_face_corners(&face, [128; 3], [64; 3], &[]);
        assert_eq!(colors, vec![[64; 3]; 4]);
        let tinted = bake_face_corners(&face, [255, 128, 0], [128; 3], &[]);
        assert_eq!(tinted[0], [255, 128, 0]);
    }

    #[test]
    fn a_lamp_above_lights_the_near_corners_more() {
        let face = floor_face();
        let lamp = BrushPointLight {
            position: [0.0, 400.0, 0.0],
            radius: 2000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let colors = bake_face_corners(&face, [128; 3], [32; 3], &[lamp]);
        let by_point = |x: i32, z: i32| {
            let i = face
                .points
                .iter()
                .position(|p| p[0] == x && p[2] == z)
                .unwrap();
            colors[i][0]
        };
        assert!(by_point(0, 0) > by_point(1024, 1024));
        assert!(by_point(1024, 1024) >= 32);
    }

    #[test]
    fn faces_turned_away_from_the_lamp_stay_at_ambient() {
        let face = floor_face();
        let below = BrushPointLight {
            position: [512.0, -400.0, 512.0],
            radius: 4000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let colors = bake_face_corners(&face, [128; 3], [32; 3], &[below]);
        assert!(colors.iter().all(|c| *c == [32; 3]));
    }
}
