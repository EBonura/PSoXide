//! The editor preview and a Play build bake the same light into the same
//! corners: the preview bakes in editor units with no shadows, the cook in
//! runtime units (a sixteenth) with the runtime's line of sight, and on an
//! open floor, where nothing shadows, the two agree to rounding.

use super::tests::{cook_with_lights, rid};
use crate::grid::faces::{all_faces, GridFaceKind};
use crate::grid::light::GridLight;
use crate::grid::lighting::LightScene;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

fn open_floor() -> GridWorld {
    let mut room = GridRoom::new("hall", 4, 4);
    room.ambient = [96; 3];
    for z in 0..4 {
        for x in 0..4 {
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, Some(rid(7)))));
        }
    }
    GridWorld {
        scale: GridScale { num: 5, den: 4 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// What the editor preview bakes for `face`: the shared rig over the
/// document's own scene, lights in runtime units.
fn preview_corners(
    scene: &LightScene,
    world: &GridWorld,
    face: &crate::grid::faces::GridFace,
    lamp: GridLight,
) -> Vec<[u8; 3]> {
    scene
        .with_rig(&[lamp], world.shading, None, |rig| {
            rig.corners(face, None, [128; 3], world.rooms[0].ambient)
        })
        .unwrap()
}

/// Compare every floor corner the preview bakes with the cooked colour of the
/// same vertex. Returns `(corners compared, worst channel difference)`.
fn compare_floor_corners(
    world: &GridWorld,
    lamp: impl Fn(f64) -> GridLight,
    preview: &dyn Fn(&crate::grid::faces::GridFace, GridLight) -> Vec<[u8; 3]>,
) -> (usize, i32) {
    let cooked = cook_with_lights(world, &[lamp(16.0)]).unwrap();
    let table = &cooked.tables[0];
    let mut worst = 0i32;
    let mut compared = 0;
    for face in all_faces(world) {
        if !matches!(face.kind, GridFaceKind::Floor { .. }) {
            continue;
        }
        let preview = preview(&face, lamp(16.0));
        for (corner, point) in face.points.iter().enumerate() {
            // The cooked vertex at this corner, in runtime units.
            let want = [point[0] / 16, point[1] / 16, point[2] / 16];
            let at_corner = |q: &psx_grid::Quad| {
                q.v.iter().position(|&v| {
                    let vtx = table.vertices[usize::from(v)];
                    [i32::from(vtx.x), i32::from(vtx.y), i32::from(vtx.z)] == want
                })
            };
            // Of the quads sharing the vertex, the one that is this face.
            let Some((quad, slot)) = table
                .quads
                .iter()
                .filter(|q| q.n[1] > 0)
                .find_map(|q| at_corner(q).map(|s| (q, s)))
            else {
                continue;
            };
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
    (compared, worst)
}

#[test]
fn preview_and_cook_bake_the_same_floor_corners() {
    let world = open_floor();
    let scene = LightScene::new(&world).unwrap();
    let lamp = |unit: f64| GridLight {
        position: [2560.0 / unit, 1100.0 / unit, 2200.0 / unit],
        radius: 3600.0 / unit,
        intensity_q8: 256,
        color: [255, 150, 60],
    };
    let (compared, worst) = compare_floor_corners(&world, lamp, &|face, lamp| {
        preview_corners(&scene, &world, face, lamp)
    });
    assert!(compared > 30, "compared {compared} corners");
    // Rounding of positions to runtime units moves a lit corner by a step
    // or two at most.
    assert!(worst <= 3, "worst channel difference {worst}");
}

/// A room with a tall block (a pillar of one sector) in the middle.
fn hall_with_pillar() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 8, 8);
    room.ambient = [48; 3];
    for z in 0..8 {
        for x in 0..8 {
            let height = if (x, z) == (3, 3) { 3072 } else { 0 };
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(height, Some(rid(7)))));
        }
    }
    GridWorld {
        scale: GridScale { num: 5, den: 4 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// The lamp of the pillar tests: west of the pillar, lower than its top, so
/// the floor east of it lies in its shadow.
fn pillar_lamp(unit: f64) -> GridLight {
    GridLight {
        position: [1500.0 / unit, 1800.0 / unit, 4480.0 / unit],
        radius: 9000.0 / unit,
        intensity_q8: 256,
        color: [255, 255, 255],
    }
}

#[test]
fn preview_and_cook_bake_the_same_shadow_behind_a_pillar() {
    let world = hall_with_pillar();
    let scene = LightScene::new(&world).unwrap();
    let (compared, worst) = compare_floor_corners(&world, pillar_lamp, &|face, lamp| {
        preview_corners(&scene, &world, face, lamp)
    });
    assert!(compared > 100, "compared {compared} corners");
    assert!(worst <= 3, "worst channel difference {worst}");
}

#[test]
fn the_pillar_shadow_is_there_in_the_cook() {
    // Guards the test above: the pillar must really shade corners, or the
    // equality proves nothing.
    let world = hall_with_pillar();
    let cooked = cook_with_lights(&world, &[pillar_lamp(16.0)]).unwrap();
    let table = &cooked.tables[0];
    let floor_reds: Vec<u32> = table
        .quads
        .iter()
        .filter(|q| q.n[1] > 0)
        .flat_map(|q| q.color)
        .map(|c| c & 0xff)
        .collect();
    let lit = floor_reds.iter().filter(|&&r| r > 100).count();
    let dark = floor_reds.iter().filter(|&&r| r <= 48).count();
    assert!(lit > 4, "{lit} lit corners");
    assert!(dark > 4, "{dark} shadowed corners");
}
