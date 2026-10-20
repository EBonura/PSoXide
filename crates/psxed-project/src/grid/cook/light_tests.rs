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

// ---- the global light ----------------------------------------------------

use crate::grid::lighting::gather::{solve, FlatAlbedo, GatherInput, FALLBACK_ALBEDO};
use crate::grid::lighting::{Gather, GlobalLight};

/// The pillar hall, gathering its light.
fn gathering_hall() -> GridWorld {
    let mut world = hall_with_pillar();
    world.global_light = GlobalLight::NEW_WORLD;
    world
}

/// What the editor preview bakes for `face` in a world that gathers: the
/// field solved on its own (as the preview's worker does), then the shared
/// rig over the document's scene.
fn gathered_preview_corners(
    scene: &LightScene,
    world: &GridWorld,
    field: &crate::grid::lighting::GiField,
    face: &crate::grid::faces::GridFace,
    lamp: GridLight,
) -> Vec<[u8; 3]> {
    scene
        .with_rig(
            &[lamp],
            world.shading,
            Some(Gather { field: Some(field) }),
            |rig| rig.corners(face, Some(face), [128; 3], world.rooms[0].ambient),
        )
        .unwrap()
}

#[test]
fn preview_and_cook_bake_the_same_gathered_light() {
    let world = gathering_hall();
    let scene = LightScene::new(&world).unwrap();
    let lamp = pillar_lamp(16.0);
    let field = scene
        .with_rig(&[lamp], world.shading, None, |_| ())
        .map(|()| {
            solve(&GatherInput {
                world: &world,
                units: scene.units(),
                lights: &[lamp],
                probe: scene.probe(),
                materials: &FlatAlbedo(FALLBACK_ALBEDO),
                cancel: None,
            })
            .expect("solved")
        })
        .unwrap();
    let (compared, worst) = compare_floor_corners(&world, pillar_lamp, &|face, lamp| {
        gathered_preview_corners(&scene, &world, &field, face, lamp)
    });
    assert!(compared > 100, "compared {compared} corners");
    assert_eq!(worst, 0, "worst channel difference {worst}");
}

#[test]
fn the_gathered_cook_differs_from_the_flat_one_and_stays_in_range() {
    let flat = cook_with_lights(&hall_with_pillar(), &[pillar_lamp(16.0)]).unwrap();
    let gathered = cook_with_lights(&gathering_hall(), &[pillar_lamp(16.0)]).unwrap();
    let corners = |c: &super::CookedLevel| -> Vec<u32> {
        c.tables[0].quads.iter().flat_map(|q| q.color).collect()
    };
    assert_ne!(corners(&flat), corners(&gathered));
    // Same geometry: the gather only changes colours (and, below, the order
    // a quad's corners are written in).
    assert_eq!(flat.tables[0].quads.len(), gathered.tables[0].quads.len());
    assert_eq!(
        flat.tables[0].vertices.len(),
        gathered.tables[0].vertices.len()
    );
}

/// Every quad of `gathered` has the corners of the same quad of `flat`, in
/// the same cyclic order: the corner order may start one corner later, and
/// each corner keeps its vertex and its texel.
fn assert_same_corners_cyclically(
    flat: &super::CookedLevel,
    gathered: &super::CookedLevel,
) -> usize {
    let (a, b) = (&flat.tables[0], &gathered.tables[0]);
    assert_eq!(a.quads.len(), b.quads.len());
    assert_eq!(a.vertices, b.vertices);
    let mut turned = 0;
    for (qa, qb) in a.quads.iter().zip(&b.quads) {
        let ring =
            |q: &psx_grid::Quad| -> Vec<(u16, u16)> { (0..4).map(|k| (q.v[k], q.uv[k])).collect() };
        let (ra, rb) = (ring(qa), ring(qb));
        if ra == rb {
            continue;
        }
        let rotated: Vec<(u16, u16)> = (0..4).map(|k| ra[(k + 1) % 4]).collect();
        assert_eq!(rotated, rb, "a quad's corners changed beyond a turn");
        turned += 1;
    }
    turned
}

#[test]
fn turning_a_quads_diagonal_keeps_every_corner_with_its_texel() {
    // A lamp low over one corner of each floor tile makes the light across
    // the tile far from linear along either diagonal.
    let lamps: Vec<GridLight> = (0..4)
        .flat_map(|i| {
            (0..4).map(move |j| GridLight {
                position: [
                    (i as f64 * 2.0 + 0.3) * 80.0,
                    14.0,
                    (j as f64 * 2.0 + 0.2) * 80.0,
                ],
                radius: 160.0,
                intensity_q8: 256,
                color: [255, 255, 255],
            })
        })
        .collect();
    let mut flat_world = hall_with_pillar();
    let mut gathered_world = hall_with_pillar();
    gathered_world.global_light = GlobalLight {
        occlusion: 0,
        bounces: 0,
        ..GlobalLight::NEW_WORLD
    };
    flat_world.global_light = GlobalLight::DEFAULT;
    let flat = cook_with_lights(&flat_world, &lamps).unwrap();
    let gathered = cook_with_lights(&gathered_world, &lamps).unwrap();
    let turned = assert_same_corners_cyclically(&flat, &gathered);
    assert!(turned > 0, "no quad turned");
    // Counts as in the flat cook.
    assert_eq!(flat.report.rooms[0].quads, gathered.report.rooms[0].quads);
    assert_eq!(
        flat.report.rooms[0].triangles,
        gathered.report.rooms[0].triangles
    );
}

#[test]
fn the_reference_levels_cook_to_the_same_quads_and_vertices_with_the_gather_on() {
    let mut table =
        String::from("level: rooms, quads+tris, vertices (flat) | (gathered), turned\n");
    for name in [
        "grid-starter",
        "grid-cathedral",
        "grid-dungeon",
        "grid-cave",
    ] {
        let project = super::tests::load_reference(name).unwrap();
        let flat_world = project.active_scene().world.grid().unwrap().clone();
        let mut gathered_world = flat_world.clone();
        gathered_world.global_light = GlobalLight::NEW_WORLD;
        let flat = super::tests::cook(&flat_world).unwrap();
        let gathered = super::tests::cook(&gathered_world).unwrap();
        assert_eq!(flat.tables.len(), gathered.tables.len(), "{name}");
        let mut turned = 0;
        for (room, (a, b)) in flat.tables.iter().zip(&gathered.tables).enumerate() {
            assert_eq!(a.vertices, b.vertices, "{name} room {room}");
            assert_eq!(a.quads.len(), b.quads.len(), "{name} room {room}");
            for (qa, qb) in a.quads.iter().zip(&b.quads) {
                let ring = |q: &psx_grid::Quad| -> Vec<(u16, u16)> {
                    (0..4).map(|k| (q.v[k], q.uv[k])).collect()
                };
                let (ra, rb) = (ring(qa), ring(qb));
                if ra != rb {
                    let rotated: Vec<(u16, u16)> = (0..4).map(|k| ra[(k + 1) % 4]).collect();
                    assert_eq!(
                        rotated, rb,
                        "{name} room {room}: corners changed beyond a turn"
                    );
                    turned += 1;
                }
            }
        }
        let count = |c: &super::CookedLevel| -> (usize, usize, usize) {
            (
                c.report.rooms.iter().map(|r| r.quads).sum(),
                c.report.rooms.iter().map(|r| r.triangles).sum(),
                c.report.rooms.iter().map(|r| r.vertices).sum(),
            )
        };
        assert_eq!(count(&flat), count(&gathered), "{name}");
        let (q, t, v) = count(&flat);
        table.push_str(&format!(
            "{name}: {} rooms, {q}+{t}, {v} | {:?}, {turned} turned\n",
            flat.tables.len(),
            count(&gathered)
        ));
    }
    eprintln!("{table}");
}

#[test]
fn the_chosen_diagonal_tracks_the_light_better_than_the_default_one() {
    // Many low lamps: the light across a tile is far from linear. The error
    // of a diagonal is how far the mean of its two corners is from the light
    // computed at the tile's middle.
    let lamps: Vec<GridLight> = (0..4)
        .flat_map(|i| {
            (0..4).map(move |j| GridLight {
                position: [
                    (i as f64 * 2.0 + 0.3) * 80.0,
                    14.0,
                    (j as f64 * 2.0 + 0.2) * 80.0,
                ],
                radius: 160.0,
                intensity_q8: 256,
                color: [255, 255, 255],
            })
        })
        .collect();
    let mut world = hall_with_pillar();
    world.global_light = GlobalLight {
        occlusion: 0,
        bounces: 0,
        ..GlobalLight::NEW_WORLD
    };
    let mut flat_world = world.clone();
    flat_world.global_light = GlobalLight::DEFAULT;
    let gathered = cook_with_lights(&world, &lamps).unwrap();
    let flat = cook_with_lights(&flat_world, &lamps).unwrap();
    let scene = LightScene::new(&world).unwrap();
    let (table, flat_table) = (&gathered.tables[0], &flat.tables[0]);
    let (mut chosen, mut default, mut turned) = (0.0f64, 0.0f64, 0);
    scene
        .with_rig(&lamps, world.shading, Some(Gather { field: None }), |rig| {
            for (q, f) in table.quads.iter().zip(&flat_table.quads) {
                if q.n[1] <= 0 {
                    continue;
                }
                let vtx = |k: usize| {
                    let v = table.vertices[usize::from(q.v[k])];
                    [f64::from(v.x), f64::from(v.y), f64::from(v.z)]
                };
                let centre: [f64; 3] =
                    core::array::from_fn(|a| (0..4).map(|k| vtx(k)[a]).sum::<f64>() / 4.0);
                // A floor face with the quad's corners.
                let face = all_faces(&world)
                    .into_iter()
                    .find(|face| {
                        matches!(face.kind, GridFaceKind::Floor { .. })
                            && face.grid.iter().all(|g| {
                                (0..4).any(|k| {
                                    let p = vtx(k);
                                    f64::from(rig.units.rt(g[0])) == p[0]
                                        && f64::from(rig.units.rt(g[2])) == p[2]
                                })
                            })
                    })
                    .expect("the floor face of the quad");
                let truth = rig.colour_at(
                    &face,
                    None,
                    Some(&face),
                    centre,
                    [128; 3],
                    world.rooms[0].ambient,
                );
                let rgb = |c: u32| {
                    [
                        (c & 255) as f64,
                        ((c >> 8) & 255) as f64,
                        ((c >> 16) & 255) as f64,
                    ]
                };
                let err = |a: u32, b: u32| -> f64 {
                    (0..3)
                        .map(|ch| {
                            let guess = (rgb(a)[ch] + rgb(b)[ch]) / 2.0;
                            (guess - f64::from(truth[ch])).powi(2)
                        })
                        .sum()
                };
                let this = err(q.color[0], q.color[2]);
                let other = err(q.color[1], q.color[3]);
                chosen += this;
                let was_turned = q.v != f.v;
                turned += usize::from(was_turned);
                default += if was_turned { other } else { this };
            }
        })
        .unwrap();
    assert!(turned > 3, "{turned} quads turned");
    assert!(
        chosen < default * 0.9,
        "diagonal error {chosen:.1} against the default {default:.1}"
    );
}
