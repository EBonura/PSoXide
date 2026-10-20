//! Behaviour of the global-light gather on small worlds.

use super::gather::{solve, FlatAlbedo, GatherInput, SampleKey, FALLBACK_ALBEDO};
use super::probe::{collision_tables, room_origins, CollisionProbe};
use super::{GiField, GlobalLight};
use crate::grid::cook::tests::rid;
use crate::grid::cook::units::RuntimeUnits;
use crate::grid::faces::{all_faces_with, GridFace, GridFaceKind};
use crate::grid::light::GridLight;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

const SCALE: GridScale = GridScale { num: 5, den: 4 };

fn settings(bounces: u8) -> GlobalLight {
    GlobalLight {
        enabled: true,
        occlusion: 100,
        reach: 4,
        bounces,
        bounce_gain: 100,
        rays: 64,
        sky: None,
    }
}

/// A 7 by 7 sector hall of floor, with a pillar (a sector raised to
/// `pillar` grid units) at (3, 3) when `pillar` is not zero, and a ceiling
/// at 1536 over every sector when `roof` is set.
fn hall(pillar: i32, roof: bool, light: GlobalLight) -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 7, 7);
    room.ambient = [96; 3];
    for z in 0..7 {
        for x in 0..7 {
            let height = if (x, z) == (3, 3) { pillar } else { 0 };
            let sector = room.sector_or_insert(x, z).unwrap();
            sector.set_ground_floor(Some(GridSurface::flat(height, Some(rid(7)))));
            if roof {
                sector.set_ground_ceiling(Some(GridSurface::flat(4096, Some(rid(9)))));
            }
        }
    }
    GridWorld {
        scale: SCALE,
        rooms: vec![room],
        global_light: light,
        ..GridWorld::default()
    }
}

fn run(world: &GridWorld, lights: &[GridLight]) -> (GiField, Vec<GridFace>, RuntimeUnits) {
    let units = RuntimeUnits::new(world.scale).unwrap();
    let origins = room_origins(world, &units);
    let cols = collision_tables(world, &units).unwrap();
    let probe = CollisionProbe::new(world, &units, &origins, &cols).unwrap();
    let field = solve(&GatherInput {
        world,
        units,
        lights,
        probe: &probe,
        materials: &FlatAlbedo(FALLBACK_ALBEDO),
        cancel: None,
    })
    .expect("not cancelled");
    (field, all_faces_with(world, &()), units)
}

/// The gathered ambient (red channel) of the floor corner of the face in
/// sector `cell` that lies at grid position `grid_xz`.
fn floor_corner(field: &GiField, faces: &[GridFace], cell: [i32; 2], grid_xz: [i32; 2]) -> f64 {
    let face = faces
        .iter()
        .find(|f| matches!(f.kind, GridFaceKind::Floor { .. }) && f.cell == cell)
        .expect("a floor face");
    let corner = face
        .grid
        .iter()
        .position(|g| g[0] == grid_xz[0] && g[2] == grid_xz[1])
        .expect("the corner");
    field
        .get(&SampleKey::of_corner(face, corner, false))
        .expect("the sample")[0]
}

#[test]
fn a_corner_against_a_pillar_is_darker_than_one_in_the_open() {
    let world = hall(3072, false, settings(0));
    let (field, faces, _) = run(&world, &[]);
    // Cell (0, 0) at its far corner (0, 0): open floor out to the sky; cell
    // (2, 3), its east-most corner on the pillar's west face.
    let open = floor_corner(&field, &faces, [0, 0], [0, 0]);
    let at_pillar = floor_corner(&field, &faces, [2, 3], [3072, 3072]);
    assert!(open > 90.0, "open floor {open}");
    assert!(
        at_pillar < open * 0.85,
        "against the pillar {at_pillar}, in the open {open}"
    );
}

#[test]
fn an_open_sky_brings_the_sky_colour_and_a_roof_keeps_it_out() {
    let sky = [40, 80, 200];
    let open = hall(
        0,
        false,
        GlobalLight {
            sky: Some(sky),
            ..settings(0)
        },
    );
    let (field, faces, _) = run(&open, &[]);
    let centre = floor_corner(&field, &faces, [3, 3], [3072, 3072]);
    // Most of the hemisphere above the middle of a wide floor is sky.
    assert!(centre < 90.0, "red of the sky light {centre}");
    let key = {
        let face = faces
            .iter()
            .find(|f| matches!(f.kind, GridFaceKind::Floor { .. }) && f.cell == [3, 3])
            .unwrap();
        let corner = face
            .grid
            .iter()
            .position(|g| g[0] == 3072 && g[2] == 3072)
            .unwrap();
        SampleKey::of_corner(face, corner, false)
    };
    let blue = field.get(&key).unwrap()[2];
    assert!(blue > 140.0, "blue of the sky light {blue}");
    // The same hall with a roof sees the ceiling for the rays that rise
    // steeply, and so takes more of the room ambient (96) than the sky's.
    let roofed = hall(
        0,
        true,
        GlobalLight {
            sky: Some(sky),
            ..settings(0)
        },
    );
    let (field, faces, _) = run(&roofed, &[]);
    let under = floor_corner(&field, &faces, [3, 3], [3072, 3072]);
    assert!(under > centre + 12.0, "under a roof {under}, open {centre}");
}

#[test]
fn bounced_light_brightens_a_corner_and_each_bounce_adds_less() {
    let lamp = GridLight {
        // Above the pillar's west side, in runtime units (80 per sector).
        position: [2.0 * 80.0, 90.0, 3.5 * 80.0],
        radius: 8.0 * 80.0,
        intensity_q8: 256,
        color: [255, 255, 255],
    };
    let at = |bounces| {
        let world = hall(3072, false, settings(bounces));
        let (field, faces, _) = run(&world, &[lamp]);
        // A floor corner on the shadowed side of the pillar.
        floor_corner(&field, &faces, [4, 3], [4096, 3072])
    };
    let (none, one, two) = (at(0), at(1), at(2));
    eprintln!("bounce {none} {one} {two}");
    assert!(one > none + 0.5, "{none} -> {one}");
    assert!(two > one, "{one} -> {two}");
    assert!(two - one < one - none, "{none} {one} {two}");
}

#[test]
fn the_gather_is_a_pure_function_of_the_world() {
    let world = hall(3072, false, settings(2));
    let lamp = GridLight {
        position: [160.0, 90.0, 280.0],
        radius: 640.0,
        intensity_q8: 256,
        color: [255, 200, 120],
    };
    let (a, _, _) = run(&world, &[lamp]);
    let (b, faces, _) = run(&world, &[lamp]);
    assert!(a.len() > 60, "{} samples", a.len());
    for face in &faces {
        for corner in 0..face.grid.len() {
            let key = SampleKey::of_corner(face, corner, false);
            assert_eq!(a.get(&key), b.get(&key));
        }
    }
}

#[test]
fn a_world_with_the_gather_off_is_the_flat_ambient_it_was() {
    let off = GlobalLight {
        enabled: false,
        ..settings(2)
    };
    assert!(!off.enabled);
    // The cook only gathers when the setting is on; a flat ambient is what
    // `lit_face_corner` has always done, covered by the light tests. The
    // document leaves the default out.
    let world = hall(0, false, GlobalLight::DEFAULT);
    let text = ron::to_string(&world).unwrap();
    assert!(!text.contains("global_light"), "{text}");
    let on = hall(0, false, GlobalLight::NEW_WORLD);
    let text = ron::to_string(&on).unwrap();
    assert!(text.contains("global_light"));
    let back: GridWorld = ron::from_str(&text).unwrap();
    assert_eq!(back.global_light, GlobalLight::NEW_WORLD);
}
