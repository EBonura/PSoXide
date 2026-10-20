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

#[test]
fn cooking_a_gathering_level_twice_gives_identical_bytes() {
    for name in ["grid-cathedral", "grid-dungeon"] {
        let project = super::tests::load_reference(name).unwrap();
        let mut world = project.active_scene().world.grid().unwrap().clone();
        world.global_light = GlobalLight::NEW_WORLD;
        let a = super::tests::cook(&world).unwrap();
        let b = super::tests::cook(&world).unwrap();
        assert_eq!(a.core, b.core, "{name}");
        assert_eq!(a.chunks, b.chunks, "{name}");
        let note = a.report.global_light.expect("the cook reports the solve");
        assert!(note.samples > 500 && note.rays > 10_000, "{name}: {note:?}");
        assert!(a.report.summary().contains("global light:"));
    }
}

// ---- tuning survey (ignored: prints, asserts nothing) ---------------------

/// Binds every material as a textured 64 by 64 one and reads the albedo from
/// the project's real textures.
struct SurveyBinder {
    albedo: crate::grid::lighting::AlbedoTable,
}

impl super::MaterialBinder for SurveyBinder {
    fn bind(
        &mut self,
        material: Option<crate::ResourceId>,
    ) -> Result<super::BoundMaterial, String> {
        Ok(super::BoundMaterial {
            asset: material.map_or(0, |m| m.raw() as u16),
            flags: 0,
            blend: 0,
            uv_motion: 0,
            tint: [128; 3],
            textured: material.is_some(),
            texels: [64, 64],
        })
    }

    fn albedo(&mut self, material: Option<crate::ResourceId>) -> [f32; 3] {
        use crate::grid::lighting::BakeMaterials;
        self.albedo.albedo(material)
    }
}

/// Mean of the red, green and blue of every cooked corner, 0..=255, and how
/// many corners reach 250 or more.
fn mean_corner(cooked: &super::CookedLevel) -> (f64, usize, usize) {
    let (mut sum, mut n, mut hot) = (0.0, 0usize, 0usize);
    for table in &cooked.tables {
        for q in &table.quads {
            for c in q.color {
                let ch = [
                    (c & 255) as f64,
                    ((c >> 8) & 255) as f64,
                    ((c >> 16) & 255) as f64,
                ];
                sum += (ch[0] + ch[1] + ch[2]) / 3.0;
                hot += usize::from(ch.iter().any(|v| *v >= 250.0));
                n += 1;
            }
        }
    }
    (sum / n.max(1) as f64, hot, n)
}

#[test]
#[ignore = "prints the brightness of the reference levels under different settings"]
fn tuning_survey() {
    for name in [
        "grid-starter",
        "grid-cathedral",
        "grid-dungeon",
        "grid-cave",
    ] {
        let dir = crate::projects_dir().join(name);
        let project = crate::ProjectDocument::load_from_path(dir.join("project.ron")).unwrap();
        let scene = project.active_scene();
        let lights = crate::grid::lighting::scene_grid_lights(scene, 16.0);
        let world = scene.world.grid().unwrap().clone();
        let faces = all_faces(&world);
        let albedo = crate::grid::lighting::AlbedoTable::from_project(
            &project,
            &dir,
            faces.iter().map(|f| f.material),
        );
        let cook = |light: GlobalLight| {
            let mut w = world.clone();
            w.global_light = light;
            let mut binder = SurveyBinder {
                albedo: crate::grid::lighting::AlbedoTable::from_project(
                    &project,
                    &dir,
                    faces.iter().map(|f| f.material),
                ),
            };
            super::cook_level(
                super::CookInput {
                    world: &w,
                    binder: &mut binder,
                    lights: &lights,
                    first_pack_sector: 0,
                    lod_far_cells: 0,
                    texture_repeat: true,
                },
                None,
            )
            .unwrap()
        };
        let _ = &albedo;
        let (flat, flat_hot, n) = mean_corner(&cook(GlobalLight::DEFAULT));
        eprintln!("{name}: {n} corners, flat mean {flat:.1}, {flat_hot} hot");
        for occlusion in [50u8, 60, 70] {
            for gain in [100u8, 130, 150, 170] {
                let light = GlobalLight {
                    occlusion,
                    bounce_gain: gain,
                    ..GlobalLight::NEW_WORLD
                };
                let (mean, hot, _) = mean_corner(&cook(light));
                eprintln!(
                    "   occlusion {occlusion:3} gain {gain:3}: mean {mean:.1} ({:+.1}%), {hot} hot",
                    (mean / flat - 1.0) * 100.0
                );
            }
        }
    }
}

// ---- the whole level: cook against the preview --------------------------------

/// Every corner of every quad the cook writes for `world`, against the colour
/// the editor preview's rig gives the same point on the same face: a face's
/// corner, in runtime units, and the way the face looks. Returns `(corners,
/// matched, differing)`.
fn cook_against_preview(world: &GridWorld, lights: &[GridLight]) -> (usize, usize, usize) {
    let cooked = cook_with_lights(world, lights).unwrap();
    let scene = LightScene::new(world).unwrap();
    let units = scene.units();
    let field = world.global_light.enabled.then(|| {
        solve(&GatherInput {
            world,
            units,
            lights,
            probe: scene.probe(),
            materials: &FlatAlbedo(FALLBACK_ALBEDO),
            cancel: None,
        })
        .expect("solved")
    });
    let faces = all_faces(world);
    let gather = world.global_light.enabled.then_some(Gather {
        field: field.as_ref(),
    });
    // Preview colour per (point, facing), as the preview would bake the face.
    type Facing = ([i32; 3], [i8; 3]);
    let mut preview: std::collections::HashMap<Facing, Vec<[u8; 3]>> = Default::default();
    scene
        .with_rig(lights, world.shading, gather, |rig| {
            for face in &faces {
                let tint = [128; 3];
                let room = &world.rooms[face.room];
                let colours = rig.corners(face, Some(face), tint, room.ambient);
                for (corner, colour) in colours.into_iter().enumerate() {
                    let point = face.grid[corner].map(|g| units.rt(g));
                    let facing = face.normal.map(|v| (v * 127.0).round() as i8);
                    preview.entry((point, facing)).or_default().push(colour);
                }
            }
        })
        .unwrap();
    let (mut corners, mut matched, mut differing) = (0, 0, 0);
    for (room, table) in cooked.tables.iter().enumerate() {
        let origin = {
            let r = &world.rooms[room];
            [
                r.origin[0] * units.sector() as i32,
                units.rt(r.y_offset),
                r.origin[1] * units.sector() as i32,
            ]
        };
        for q in &table.quads {
            for k in 0..4 {
                corners += 1;
                let v = table.vertices[usize::from(q.v[k])];
                let point = [
                    i32::from(v.x) + origin[0],
                    i32::from(v.y) + origin[1],
                    i32::from(v.z) + origin[2],
                ];
                let c = q.color[k];
                let got = [
                    (c & 255) as u8,
                    ((c >> 8) & 255) as u8,
                    ((c >> 16) & 255) as u8,
                ];
                // The facing the cook wrote, against the faces at the point.
                let facing = q.n;
                let candidates = preview
                    .iter()
                    .filter(|((p, n), _)| {
                        *p == point
                            && (0..3)
                                .map(|a| i32::from(n[a]) * i32::from(facing[a]))
                                .sum::<i32>()
                                > 127 * 127 * 9 / 10
                    })
                    .flat_map(|(_, colours)| colours.iter());
                let mut seen = false;
                let mut equal = false;
                for want in candidates {
                    seen = true;
                    equal |= *want == got;
                }
                if seen {
                    matched += 1;
                    differing += usize::from(!equal);
                }
            }
        }
    }
    (corners, matched, differing)
}

#[test]
fn every_corner_of_the_reference_levels_is_baked_alike_by_the_cook_and_the_preview() {
    let mut report = String::new();
    for name in [
        "grid-starter",
        "grid-cathedral",
        "grid-dungeon",
        "grid-cave",
    ] {
        let project = super::tests::load_reference(name).unwrap();
        let scene = project.active_scene();
        let lights = crate::grid::lighting::scene_grid_lights(scene, 16.0);
        let flat_world = scene.world.grid().unwrap().clone();
        let mut gathered_world = flat_world.clone();
        gathered_world.global_light = GlobalLight::NEW_WORLD;
        for (label, world) in [("flat", &flat_world), ("gathered", &gathered_world)] {
            let (corners, matched, differing) = cook_against_preview(world, &lights);
            report.push_str(&format!(
                "{name} {label}: {corners} cooked corners, {matched} found in the preview, {differing} differ\n"
            ));
            assert!(
                matched * 10 >= corners * 9,
                "{name} {label}: only {matched} of {corners} corners matched a preview face\n{report}"
            );
            // Rounding of a lit corner never reaches a step.
            assert_eq!(differing, 0, "{name} {label}\n{report}");
        }
    }
    eprintln!("{report}");
}
