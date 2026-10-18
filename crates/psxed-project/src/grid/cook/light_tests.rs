//! The editor preview and a Play build bake the same light into the same
//! corners: the preview bakes in editor units with no shadows, the cook in
//! runtime units (a sixteenth) with the runtime's line of sight, and on an
//! open floor, where nothing shadows, the two agree to rounding.

use super::tests::{cook_with_lights, rid};
use crate::grid::faces::{all_faces, GridFaceKind};
use crate::grid::light::{bake_face_corners, GridLight};
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

fn open_floor() -> GridWorld {
    let mut room = GridRoom::new("hall", 4, 4);
    room.ambient = [96; 3];
    for z in 0..4 {
        for x in 0..4 {
            room.sector_or_insert(x, z).unwrap().floor = Some(GridSurface::flat(0, Some(rid(7))));
        }
    }
    GridWorld {
        scale: GridScale { num: 5, den: 4 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

#[test]
fn preview_and_cook_bake_the_same_floor_corners() {
    let world = open_floor();
    let lamp = |unit: f64| GridLight {
        position: [2560.0 / unit, 1100.0 / unit, 2200.0 / unit],
        radius: 3600.0 / unit,
        intensity_q8: 256,
        color: [255, 150, 60],
    };
    let cooked = cook_with_lights(&world, &[lamp(16.0)]).unwrap();
    let table = &cooked.tables[0];
    let mut worst = 0i32;
    let mut compared = 0;
    for face in all_faces(&world) {
        if !matches!(face.kind, GridFaceKind::Floor { .. }) {
            continue;
        }
        let preview = bake_face_corners(
            &face,
            [128; 3],
            world.rooms[0].ambient,
            &[lamp(1.0)],
            world.shading,
        );
        for (corner, point) in face.points.iter().enumerate() {
            // The cooked vertex at this corner, in runtime units.
            let want = [point[0] / 16, point[1] / 16, point[2] / 16];
            let Some(quad) = table.quads.iter().find(|q| {
                q.v.iter().any(|&v| {
                    let vtx = table.vertices[usize::from(v)];
                    [i32::from(vtx.x), i32::from(vtx.y), i32::from(vtx.z)] == want
                })
            }) else {
                continue;
            };
            let slot = quad
                .v
                .iter()
                .position(|&v| {
                    let vtx = table.vertices[usize::from(v)];
                    [i32::from(vtx.x), i32::from(vtx.y), i32::from(vtx.z)] == want
                })
                .unwrap();
            let c = quad.color[slot];
            let cooked_rgb = [
                (c & 0xff) as i32,
                ((c >> 8) & 0xff) as i32,
                ((c >> 16) & 0xff) as i32,
            ];
            for ch in 0..3 {
                worst = worst.max((cooked_rgb[ch] - i32::from(preview[corner][ch])).abs());
            }
            compared += 1;
        }
    }
    assert!(compared > 30, "compared {compared} corners");
    // Rounding of positions to runtime units moves a lit corner by a step
    // or two at most.
    assert!(worst <= 3, "worst channel difference {worst}");
}
