//! Rough on hand-built worlds: the jitter is watertight, bounded, leaves
//! collision alone, and is the same in the preview faces and the cook.

use std::collections::BTreeMap;

use super::tests::cook;
use crate::grid::bands::{Anchor, Band, BandSet};
use crate::grid::edit::rough::{set_level, set_params};
use crate::grid::faces::{all_faces, GridFace};
use crate::grid::room::GridRoom;
use crate::grid::surface::{GridSurface, VOID_A};
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A 6 by 6 derived room with a raised block, a banded default wall set, a
/// cut corner and a ramp: every face kind the jitter touches.
fn rocky() -> GridWorld {
    let mut room = GridRoom::new_derived("rock", 6, 6);
    for z in 0..6 {
        for x in 0..6 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat(0, Some(rid(1)))));
            s.set_ground_ceiling(Some(GridSurface::flat(3072, Some(rid(2)))));
        }
    }
    for z in 2..4 {
        for x in 2..4 {
            room.sector_mut(x, z)
                .unwrap()
                .ground_floor_mut()
                .unwrap()
                .heights = [768; 4];
        }
    }
    room.sector_mut(0, 5)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .heights = [0, 0, 256, 256];
    room.sector_mut(5, 5)
        .unwrap()
        .ground_floor_mut()
        .unwrap()
        .void = VOID_A;
    room.band_sets.push(BandSet {
        name: "Strata".into(),
        lines: vec![Anchor::Abs(256), Anchor::Abs(512)],
        bands: vec![
            Band {
                material: Some(rid(3)),
                ..Band::default()
            },
            Band {
                material: Some(rid(4)),
                ..Band::default()
            },
            Band::default(),
        ],
    });
    room.defaults.bands = Some(0);
    GridWorld {
        scale: GridScale { num: 5, den: 4 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn rough(world: &mut GridWorld, level: u8) {
    let cells: Vec<[i32; 2]> = (0..6).flat_map(|z| (0..6).map(move |x| [x, z])).collect();
    set_level(world, 0, &cells, level);
    set_params(world, 0, Some(11), None, Some(30));
}

fn key(face: &GridFace, i: usize) -> [i32; 3] {
    face.grid[i]
}

#[test]
fn jitter_is_a_function_of_the_plan_position_so_the_surface_stays_watertight() {
    let plain = rocky();
    let mut moved = plain.clone();
    rough(&mut moved, 3);
    let (a, b) = (all_faces(&plain), all_faces(&moved));
    assert_eq!(a.len(), b.len());
    // The same undisplaced corner always lands in the same place, whichever
    // face it belongs to: no vertex is pulled two ways, so no crack opens.
    let mut map: BTreeMap<[i32; 3], [i32; 3]> = BTreeMap::new();
    let mut shifted = 0;
    for (fa, fb) in a.iter().zip(&b) {
        assert_eq!(fa.points.len(), fb.points.len());
        for i in 0..fa.points.len() {
            let (from, to) = (key(fa, i), key(fb, i));
            assert_eq!(from[1], to[1], "height is never displaced");
            let prior = map.insert(from, to);
            assert!(
                prior.is_none_or(|p| p == to),
                "{from:?} moved to both {prior:?} and {to:?}"
            );
            shifted += usize::from(from != to);
        }
    }
    assert!(shifted > 50, "the jitter moves corners: {shifted}");
    // Bounded by the room's level-3 amplitude, either way on each axis.
    let amp = i32::from(moved.rooms[0].rough.amp[2]);
    assert!(amp > 0);
    for (from, to) in &map {
        assert!((from[0] - to[0]).abs() <= amp && (from[2] - to[2]).abs() <= amp);
    }
    // Faces keep facing the way they did.
    for (fa, fb) in a.iter().zip(&b) {
        let dot: f64 = (0..3).map(|k| fa.normal[k] * fb.normal[k]).sum();
        assert!(dot > 0.0, "{:?} flipped", fa.kind);
    }
}

#[test]
fn a_room_with_no_rough_sectors_is_untouched_and_the_tint_follows_the_level() {
    let plain = rocky();
    let mut none = plain.clone();
    // Amplitudes but no sector painted: nothing moves, nothing tints.
    set_params(&mut none, 0, Some(5), Some([10, 20, 40]), Some(50));
    assert_eq!(all_faces(&plain), all_faces(&none));
    let mut some = none.clone();
    set_level(&mut some, 0, &[[2, 2]], 3);
    let faces = all_faces(&some);
    assert!(faces
        .iter()
        .any(|f| f.cell == [2, 2] && f.tint_noise == Some((5, 50))));
    assert!(faces
        .iter()
        .filter(|f| f.cell != [2, 2])
        .all(|f| f.tint_noise.is_none()));
}

#[test]
fn the_cook_places_the_same_corners_and_never_touches_collision() {
    let plain = rocky();
    let mut moved = plain.clone();
    rough(&mut moved, 2);
    let (a, b) = (cook(&plain).unwrap(), cook(&moved).unwrap());
    // Collision comes from the document, not from the faces.
    assert_eq!(a.tables[0].sectors.len(), b.tables[0].sectors.len());
    assert_eq!(a.tables[0].walls, b.tables[0].walls);
    for (sa, sb) in a.tables[0].sectors.iter().zip(&b.tables[0].sectors) {
        assert_eq!(sa, sb);
    }
    // The vertex table keeps its size: shared lattice points still weld.
    assert_eq!(a.tables[0].vertices.len(), b.tables[0].vertices.len());
    // Every cooked vertex is within the amplitude (in runtime units) of an
    // undisplaced one.
    let units = super::RuntimeUnits::new(moved.scale).unwrap();
    let amp_ru = units.rt(i32::from(moved.rooms[0].rough.amp[1])) + 1;
    let origin = |t: &psx_grid::write::RoomChunk| {
        t.vertices
            .iter()
            .map(|v| [i32::from(v.x), i32::from(v.y), i32::from(v.z)])
            .collect::<Vec<_>>()
    };
    let (va, vb) = (origin(&a.tables[0]), origin(&b.tables[0]));
    for p in &vb {
        let near = va.iter().any(|q| {
            q[1] == p[1] && (q[0] - p[0]).abs() <= amp_ru && (q[2] - p[2]).abs() <= amp_ru
        });
        assert!(
            near,
            "cooked vertex {p:?} is not within {amp_ru} of the plain level"
        );
    }
}
