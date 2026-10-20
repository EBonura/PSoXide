//! The cook bakes a light probe into every sector of every room: what a
//! character standing there is lit by. The probes come out of the rig that
//! colours the quads, so they carry its lamps and shadows, and the editor
//! preview, which bakes them again from the document, lights a model the way
//! the cooked level does.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{
    probe_pack, probe_unpack, LevelView, ResidentRooms, RoomView, PROBE_MULTI, ROOM_PROBES,
};

use super::tests::{cook, cook_with_lights, rid};
use super::CookedLevel;
use crate::grid::edit::spans::insert_slab_column;
use crate::grid::light::GridLight;
use crate::grid::lighting::gather::{solve, FlatAlbedo, GatherInput};
use crate::grid::lighting::{Gather, GlobalLight, LightScene, CHEST_UNITS};
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::storeys::SLAB_THICKNESS;
use crate::grid::surface::{GridSurface, GridWall};
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

/// Runtime units per sector at the scale the tests use (5/4).
const S: i32 = 80;

/// A `width` by `depth` hall: floor everywhere, no ceiling.
fn hall(name: &str, width: usize, depth: usize, ambient: u8) -> GridRoom {
    hall_at(name, width, depth, ambient, [0, 0])
}

/// [`hall`] with its west-north corner at sector `origin` of the world.
fn hall_at(name: &str, width: usize, depth: usize, ambient: u8, origin: [i32; 2]) -> GridRoom {
    let mut room = GridRoom::new(name, width, depth);
    room.origin = origin;
    room.ambient = [ambient; 3];
    for z in 0..depth {
        for x in 0..width {
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, Some(rid(7)))));
        }
    }
    room
}

fn world_of(rooms: Vec<GridRoom>) -> GridWorld {
    GridWorld {
        scale: GridScale { num: 5, den: 4 },
        rooms,
        ..GridWorld::default()
    }
}

/// A lamp over sector `(cx, cz)`, at the height of a chest, reaching `reach`
/// sectors.
fn lamp(cx: f64, cz: f64, reach: f64) -> GridLight {
    GridLight {
        position: [cx * f64::from(S), 40.0, cz * f64::from(S)],
        radius: reach * f64::from(S),
        intensity_q8: 256,
        color: [255; 3],
    }
}

/// Run `f` over the cooked level held the way the guest holds it.
fn with_cooked<R>(cooked: &CookedLevel, f: impl FnOnce(&ResidentRooms<'_>) -> R) -> R {
    let core = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&core, cooked.core.len())).unwrap();
    let chunks: Vec<Vec<u32>> = cooked.chunks.iter().map(|c| aligned_copy(c)).collect();
    let mut set = ResidentRooms::new(level).unwrap();
    for (i, words) in chunks.iter().enumerate() {
        let view = RoomView::parse(words_as_bytes(words, cooked.chunks[i].len())).unwrap();
        view.validate_full(level.materials.len()).unwrap();
        view.render.probes.validate_full().unwrap();
        set.insert(i, view).unwrap();
    }
    f(&set)
}

/// The light of the probe of sector `(cx, cz)` of room `room`.
fn probe_of(cooked: &CookedLevel, room: usize, cx: usize, cz: usize) -> [u8; 3] {
    let tables = cooked.tables[room]
        .probes
        .as_ref()
        .expect("a room with probes");
    let width = usize::from(cooked.tables[room].width);
    let entry = tables.cells[cz * width + cx];
    assert_eq!(
        entry.rgb & PROBE_MULTI,
        0,
        "sector {cx},{cz} is a single floor"
    );
    probe_unpack(entry.rgb)
}

#[test]
fn every_sector_with_a_floor_gets_a_probe_a_chest_above_it() {
    let mut world = world_of(vec![hall("hall", 4, 3, 96)]);
    // A pit has no probe.
    world.rooms[0]
        .sector_mut(1, 1)
        .unwrap()
        .set_ground_floor(None);
    let cooked = cook(&world).unwrap();
    let chunk = &cooked.tables[0];
    let probes = chunk.probes.as_ref().expect("the room has probes");
    assert_eq!(probes.cells.len(), 12);
    assert!(probes.multi.is_empty());
    for (cell, entry) in probes.cells.iter().enumerate() {
        if cell == 4 + 1 {
            assert_eq!(entry.rgb, PROBE_MULTI, "the pit holds no probe");
            continue;
        }
        assert_eq!(entry.rgb & PROBE_MULTI, 0);
        assert_eq!(i32::from(entry.y), CHEST_UNITS, "sector {cell}");
        // No lamps: the room's own ambient (a multiple of eight, so exact).
        assert_eq!(probe_unpack(entry.rgb), [96; 3]);
    }
    // The chunk and the room record say so.
    assert_eq!(cooked.level.rooms[0].flags & ROOM_PROBES, ROOM_PROBES);
    with_cooked(&cooked, |set| {
        assert_eq!(
            set.get(0).unwrap().col.header.flags & ROOM_PROBES,
            ROOM_PROBES
        );
        // Over the pit the neighbours light the character; far outside there
        // is nothing.
        assert_eq!(set.probe_light([S + S / 2, 32, S + S / 2]), Some([96; 3]));
        assert_eq!(set.probe_light([-500, 32, 10]), None);
    });
    // The report counts them.
    assert_eq!(cooked.report.rooms[0].probes, 11);
    assert_eq!(cooked.report.rooms[0].probe_bytes, 20 + 4 * 12);
}

#[test]
fn a_probe_is_lowered_under_a_low_ceiling() {
    let mut world = world_of(vec![hall("low", 2, 2, 96)]);
    // A ceiling 30 runtime units over the floor (384 grid units): the probe
    // sits halfway up.
    for z in 0..2 {
        for x in 0..2 {
            world.rooms[0]
                .sector_mut(x, z)
                .unwrap()
                .set_ground_ceiling(Some(GridSurface::flat(384, Some(rid(9)))));
        }
    }
    let cooked = cook(&world).unwrap();
    let entry = cooked.tables[0].probes.as_ref().unwrap().cells[0];
    assert_eq!(i32::from(entry.y), 15);
}

#[test]
fn the_probe_values_are_the_rigs_at_those_points() {
    // One lamp; the probes carry its falloff and nothing else on top of the
    // ambient. Computed here straight from the formula.
    let world = world_of(vec![hall("hall", 6, 3, 40)]);
    let lamp = lamp(1.5, 1.5, 4.0);
    let cooked = cook_with_lights(&world, &[lamp]).unwrap();
    for (cx, cz) in [(0usize, 0usize), (1, 1), (3, 1), (5, 2)] {
        let at = [
            f64::from(cx as i32 * S + S / 2),
            f64::from(CHEST_UNITS),
            f64::from(cz as i32 * S + S / 2),
        ];
        let d = ((at[0] - lamp.position[0]).powi(2)
            + (at[1] - lamp.position[1]).powi(2)
            + (at[2] - lamp.position[2]).powi(2))
        .sqrt();
        let falloff = (1.0 - d / lamp.radius).max(0.0);
        let want = 40.0 + 255.0 * falloff;
        let got = probe_of(&cooked, 0, cx, cz);
        let want = probe_unpack(probe_pack([want.round().min(255.0) as u8; 3]));
        assert_eq!(got, want, "sector {cx},{cz}");
    }
}

#[test]
fn a_probe_in_a_shadowed_alcove_is_darker_than_one_under_the_lamp() {
    // A hall cut in two by a wall along the east edge of sector column 2; the
    // lamp hangs in the west half.
    let mut room = hall("hall", 6, 3, 40);
    for z in 0..3 {
        room.sector_mut(2, z)
            .unwrap()
            .walls_mut(WallEdge::East)
            .push(GridWall::span(0, 1024, Some(rid(8))));
    }
    let world = world_of(vec![room]);
    let lamp = lamp(1.5, 1.5, 6.0);
    let cooked = cook_with_lights(&world, &[lamp]).unwrap();
    let under = probe_of(&cooked, 0, 1, 1);
    let across = probe_of(&cooked, 0, 3, 1);
    let far_side = probe_of(&cooked, 0, 5, 1);
    assert!(under[0] > 200, "under the lamp: {under:?}");
    // Behind the wall the lamp does not reach: only the ambient.
    assert_eq!(across, [40; 3]);
    assert_eq!(far_side, [40; 3]);
    // The same distance on the open side is still lit.
    let open = probe_of(&cooked, 0, 0, 2);
    assert!(open[0] > across[0] + 60, "{open:?} against {across:?}");
}

#[test]
fn a_column_of_two_floors_has_a_probe_on_each() {
    let mut room = GridRoom::new_derived("hall", 2, 1);
    for x in 0..2 {
        let sector = room.sector_or_insert(x, 0).unwrap();
        sector.set_ground_floor(Some(GridSurface::flat(0, Some(rid(1)))));
        sector.set_ground_ceiling(Some(GridSurface::flat(3072, Some(rid(2)))));
    }
    insert_slab_column(
        room.sector_mut(0, 0).unwrap(),
        1024,
        SLAB_THICKNESS,
        Some(rid(3)),
    )
    .unwrap();
    room.ambient = [64; 3];
    let world = world_of(vec![room]);
    // A lamp on the upper floor only: the slab shadows the floor below.
    let upper_lamp = GridLight {
        position: [S as f64 / 2.0, 120.0, S as f64 / 2.0],
        radius: 6.0 * f64::from(S),
        intensity_q8: 256,
        color: [255; 3],
    };
    let cooked = cook_with_lights(&world, &[upper_lamp]).unwrap();
    let probes = cooked.tables[0].probes.as_ref().unwrap();
    // Cell 0 is a pointer to two probes, cell 1 a plain one.
    assert_eq!(probes.cells[0].rgb, PROBE_MULTI | 2);
    assert_eq!(probes.multi.len(), 2);
    assert_eq!(probes.cells[1].rgb & PROBE_MULTI, 0);
    let (low, high) = (probes.multi[0], probes.multi[1]);
    assert!(i32::from(high.y) > i32::from(low.y) + S, "{low:?} {high:?}");
    // The slab is low over the lower floor: its probe sits halfway up.
    assert!((16..=CHEST_UNITS).contains(&i32::from(low.y)), "{low:?}");
    // The lamp lights the upper floor and the slab keeps it off the lower.
    assert!(probe_unpack(high.rgb)[0] > 120, "{high:?}");
    assert_eq!(probe_unpack(low.rgb), [64; 3]);
    // And the lookup tells the floors apart by height.
    with_cooked(&cooked, |set| {
        let below = set.probe_light([S / 2, 30, S / 2]).unwrap();
        let above = set.probe_light([S / 2, i32::from(high.y), S / 2]).unwrap();
        assert!(above[0] > below[0] + 40, "{above:?} {below:?}");
    });
}

#[test]
fn the_editor_preview_lights_a_model_the_way_the_cooked_level_does() {
    // The preview bakes its probes from the document through the shared
    // light scene; the cook bakes them into the chunk. Sampled through the
    // same lookup, the two give the same colour at every point.
    let mut room = hall("hall", 6, 3, 40);
    for z in 0..3 {
        room.sector_mut(2, z)
            .unwrap()
            .walls_mut(WallEdge::East)
            .push(GridWall::span(0, 1024, Some(rid(8))));
    }
    let world = world_of(vec![room, hall_at("annex", 3, 3, 72, [6, 0])]);
    let lights = [lamp(1.5, 1.5, 5.0), lamp(7.5, 1.0, 3.0)];
    let cooked = cook_with_lights(&world, &lights).unwrap();
    let scene = LightScene::new(&world).unwrap();
    let store = scene.bake_probes(&world, &lights, None).unwrap();
    with_cooked(&cooked, |set| {
        let mut compared = 0;
        for x in (0..9 * S).step_by(7) {
            for z in (0..3 * S).step_by(5) {
                for y in [10, 32, 60] {
                    let from_cook = set.probe_light([x, y, z]);
                    let from_preview = store.light_at([x, y, z]);
                    assert_eq!(from_preview, from_cook, "at {x},{y},{z}");
                    compared += 1;
                }
            }
        }
        assert!(compared > 3000);
    });
}

#[test]
fn a_world_that_gathers_its_light_bakes_the_occlusion_around_the_probe() {
    // A closet and an open yard with the same ambient: the sphere around a
    // probe in the closet is walls on every side, the yard's is half sky.
    let mut closet = GridRoom::new("closet", 1, 1);
    closet.ambient = [128; 3];
    let sector = closet.sector_or_insert(0, 0).unwrap();
    sector.set_ground_floor(Some(GridSurface::flat(0, Some(rid(7)))));
    sector.set_ground_ceiling(Some(GridSurface::flat(1024, Some(rid(9)))));
    for edge in [
        WallEdge::North,
        WallEdge::East,
        WallEdge::South,
        WallEdge::West,
    ] {
        sector
            .walls_mut(edge)
            .push(GridWall::span(0, 1024, Some(rid(8))));
    }
    let yard = hall_at("yard", 7, 7, 128, [10, 0]);
    let mut world = world_of(vec![closet, yard]);
    world.global_light = GlobalLight::NEW_WORLD;
    let cooked = cook(&world).unwrap();
    let shut_in = probe_of(&cooked, 0, 0, 0)[0];
    let open_air = probe_of(&cooked, 1, 3, 3)[0];
    assert!(shut_in + 24 < open_air, "closet {shut_in}, yard {open_air}");
    // The cook and the preview solve the same field: the same probes.
    let scene = LightScene::new(&world).unwrap();
    let field = {
        let lights: [GridLight; 0] = [];
        let probe = crate::grid::lighting::CollisionProbe::new(
            &world,
            &scene.units(),
            &crate::grid::lighting::probe::room_origins(&world, &scene.units()),
            &crate::grid::lighting::probe::collision_tables(&world, &scene.units()).unwrap(),
        )
        .unwrap();
        solve(&GatherInput {
            world: &world,
            units: scene.units(),
            lights: &lights,
            probe: &probe,
            materials: &FlatAlbedo([0.5; 3]),
            cancel: None,
        })
        .unwrap()
    };
    let store = scene
        .bake_probes(
            &world,
            &[],
            Some(Gather {
                field: Some(&field),
            }),
        )
        .unwrap();
    with_cooked(&cooked, |set| {
        for (x, z) in [(40, 40), (10 * S + 3 * S + 40, 3 * S + 40), (11 * S, 2 * S)] {
            assert_eq!(
                store.light_at([x, 32, z]),
                set.probe_light([x, 32, z]),
                "{x},{z}"
            );
        }
    });
}

#[test]
fn cooking_probes_twice_gives_identical_bytes() {
    let world = world_of(vec![hall("hall", 5, 4, 48)]);
    let lights = [lamp(2.5, 2.0, 4.0)];
    let a = cook_with_lights(&world, &lights).unwrap();
    let b = cook_with_lights(&world, &lights).unwrap();
    assert_eq!(a.chunks, b.chunks);
}

/// Every reference level plans, bakes and reads back its probes; packing the
/// colours to five bits a channel moves none of them by more than the step of
/// the packing, which the 15 bit framebuffer cannot show.
#[test]
fn the_reference_levels_bake_probes_and_five_bits_lose_nothing_the_framebuffer_keeps() {
    use crate::grid::lighting::scene_grid_lights;
    let mut total = 0usize;
    let mut sum_error = 0u64;
    let mut worst = 0i32;
    for name in [
        "grid-starter",
        "grid-gate",
        "grid-tower",
        "grid-cave",
        "grid-dungeon",
        "grid-sewers",
        "grid-west",
        "grid-level008",
        "grid-cathedral",
        "grid-corridor",
    ] {
        let mut project = super::tests::load_reference(name).unwrap();
        crate::units::scale_project_to_engine_units(&mut project);
        let world = project.active_scene().world.grid().expect("a grid project");
        let lights = scene_grid_lights(project.active_scene(), 1.0);
        let scene = LightScene::new(world).unwrap();
        let plan =
            crate::grid::lighting::ProbePlan::build(world, &scene.units(), scene.probe()).unwrap();
        assert!(!plan.is_empty(), "{name} has floors");
        let raw: Vec<[u8; 3]> = scene
            .with_rig(&lights, world.shading, None, |rig| {
                plan.sites
                    .iter()
                    .enumerate()
                    .map(|(i, site)| {
                        rig.probe(i, site, world.rooms[usize::from(site.room)].ambient)
                    })
                    .collect()
            })
            .unwrap();
        for packed in &raw {
            let back = probe_unpack(probe_pack(*packed));
            for k in 0..3 {
                let e = (i32::from(back[k]) - i32::from(packed[k])).abs();
                worst = worst.max(e);
                sum_error += e as u64;
                total += 1;
            }
        }
        eprintln!("{name}: {} probes", plan.len());
    }
    eprintln!(
        "probe packing: {total} channels, mean error {:.2}, worst {worst}",
        sum_error as f64 / total as f64
    );
    // The step is eight, so half of it, except above 252 where it clips.
    assert!(worst <= 7, "worst error {worst}");
}
