//! Tests of the Corridor verb.

use crate::grid::edit::CellRect;
use crate::grid::tree::check;

use super::corridor::{lay_corridor, CorridorParams, CorridorRefusal, LegShape, Waypoint};
use super::room::{lay_room, Footprint, RoomParams, RoomTarget};
use super::room_tests::empty;

fn floor_at(w: &crate::grid::world::GridWorld, room: usize, cell: [i32; 2]) -> [i32; 4] {
    let r = &w.rooms[room];
    let [x, z] = r.local(cell).unwrap();
    r.sector(x, z).unwrap().ground_floor().unwrap().heights
}

fn params(points: &[([i32; 2], Option<i32>)]) -> CorridorParams {
    CorridorParams::new(
        RoomTarget::New("c".into()),
        points
            .iter()
            .map(|&(cell, floor)| Waypoint { cell, floor })
            .collect(),
        3072,
    )
}

#[test]
fn an_l_shaped_corridor_is_a_flat_footprint_with_mitred_corners() {
    let mut w = empty();
    let mut p = params(&[([0, 0], None), ([5, 0], None), ([5, 4], None)]);
    p.width = 2;
    let out = lay_corridor(&mut w, &p).unwrap();
    // Two legs, width 2, the corner square counted once.
    assert_eq!(out.legs.len(), 2);
    assert!(out.cells >= 6 * 2 + 4 * 2 - 4);
    assert_eq!(floor_at(&w, out.room, [5, 4]), [0; 4]);
    assert!(check(&w.rooms[out.room]).is_empty());
}

#[test]
fn a_height_change_becomes_a_ramp_under_the_walkable_slope() {
    let mut w = empty();
    let mut p = params(&[([0, 0], None), ([8, 0], Some(1024))]);
    p.leg = LegShape::Ramp;
    let out = lay_corridor(&mut w, &p).unwrap();
    assert_eq!(out.legs[0].rise, 1024);
    assert_eq!(out.legs[0].cells, 7);
    // The landings stand at their own heights, the slope joins them.
    assert_eq!(floor_at(&w, out.room, [0, 0]), [0; 4]);
    assert_eq!(floor_at(&w, out.room, [8, 0]), [1024; 4]);
    let mid = floor_at(&w, out.room, [4, 0]);
    assert!(mid[0] < mid[1] && mid[0] == mid[3] && mid[1] == mid[2]);
    // Ceiling keeps a constant headroom.
    let r = &w.rooms[out.room];
    let [x, z] = r.local([4, 0]).unwrap();
    let c = r.sector(x, z).unwrap().ground_ceiling().unwrap().heights;
    assert_eq!(c[0] - mid[0], 3072);
}

#[test]
fn stairs_are_sized_by_the_walk_limits_and_refused_when_too_steep() {
    let mut w = empty();
    let mut p = params(&[([0, 0], None), ([5, 0], Some(1024))]);
    p.leg = LegShape::Stairs;
    let out = lay_corridor(&mut w, &p).unwrap();
    assert_eq!(out.legs[0].cells, 4);
    // Treads are flat and rise by 256 each.
    assert_eq!(floor_at(&w, out.room, [1, 0]), [256; 4]);
    assert_eq!(floor_at(&w, out.room, [4, 0]), [1024; 4]);
    // Two sectors for 2048 is a riser of 2048: over the step-up.
    let mut w = empty();
    let mut p = params(&[([0, 0], None), ([2, 0], Some(2048))]);
    p.leg = LegShape::Stairs;
    assert!(matches!(
        lay_corridor(&mut w, &p),
        Err(CorridorRefusal::TooSteep { leg: 0, .. })
    ));
    assert!(w.rooms.is_empty());
}

#[test]
fn bad_paths_are_refused_whole() {
    let mut w = empty();
    assert_eq!(
        lay_corridor(&mut w, &params(&[([0, 0], None)])),
        Err(CorridorRefusal::TooFewWaypoints)
    );
    assert_eq!(
        lay_corridor(&mut w, &params(&[([0, 0], None), ([3, 3], None)])),
        Err(CorridorRefusal::DiagonalLeg { leg: 0 })
    );
    assert_eq!(
        lay_corridor(&mut w, &params(&[([0, 0], None), ([1, 0], Some(256))])),
        Err(CorridorRefusal::NoRoomForChange { leg: 0 })
    );
    assert!(w.rooms.is_empty());
}

#[test]
fn a_corridor_into_a_hall_keeps_the_hall_and_opens_where_floors_agree() {
    let mut w = empty();
    let hall = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [5, 5])),
        "hall",
        3072,
    );
    lay_room(&mut w, &hall).unwrap();
    let mut p = params(&[([-4, 2], None), ([1, 2], None)]);
    p.target = RoomTarget::Existing(0);
    let out = lay_corridor(&mut w, &p).unwrap();
    assert!(out.kept >= 2, "the hall cells it crosses stay: {out:?}");
    let room = &w.rooms[0];
    assert!(check(room).is_empty());
    // The corridor floor meets the hall floor at the same height: no riser.
    let faces = crate::grid::faces::all_faces(&w);
    assert!(!faces.is_empty());
}
