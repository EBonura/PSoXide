//! Tests of the Building verb.

use crate::grid::edit::CellRect;
use crate::grid::sector::WallEdge;

use super::building::{build_building, BuildingParams, BuildingRefusal};
use super::room::{Footprint, RoomTarget};
use super::room_tests::empty;

fn rect(w: i32, d: i32) -> Footprint {
    Footprint::Rect(CellRect::from_corners([0, 0], [w - 1, d - 1]))
}

fn params(fp: Footprint) -> BuildingParams {
    BuildingParams::new(RoomTarget::New("house".into()), fp)
}

#[test]
fn a_one_storey_house_has_a_door_windows_and_derived_walls() {
    let mut w = empty();
    let mut p = params(rect(6, 6));
    p.storeys = 1;
    p.stair = false;
    p.roof_parapet = None;
    let r = build_building(&mut w, &p).unwrap();
    assert_eq!(r.doors.len(), 1, "{:?}", r.notes);
    assert!(r.windows >= 4, "{r:?}");
    assert!(r.notes.is_empty(), "{:?}", r.notes);
    assert_eq!(r.slabs, 0);
    let room = &w.rooms[r.room];
    assert_eq!(
        room.defaults.closure_facing,
        crate::grid::overrides::ClosureFacing::Both
    );
}

#[test]
fn two_storeys_get_a_floor_a_stairwell_a_spiral_and_a_roof() {
    let mut w = empty();
    let p = params(rect(6, 6));
    let r = build_building(&mut w, &p).unwrap();
    assert!(r.stairwell.is_some());
    assert!(r.treads > 0, "{r:?}");
    assert!(r.slabs > 30, "floor cells, treads and the roof: {r:?}");
    let room = &w.rooms[r.room];
    assert_eq!(room.defaults.open_wall, 768);
    // A cell away from the well has three spans: ground, upper floor, roof.
    let far = [5, 5];
    let [x, z] = room.local(far).unwrap();
    let s = room.sector(x, z).unwrap();
    assert_eq!(s.span_count(), 3, "{:?}", s.spans().len());
    assert!(
        s.spans().last().unwrap().ceiling.is_none(),
        "open to the sky"
    );
    crate::grid::cook::tests::cook(&w).expect("a house cooks");
}

#[test]
fn an_octagonal_tower_and_the_seed_are_deterministic() {
    let fp = Footprint::Octagon {
        rect: CellRect::from_corners([0, 0], [7, 7]),
        cut: 2,
    };
    let mut p = params(fp);
    p.storeys = 3;
    let (mut a, mut b, mut c) = (empty(), empty(), empty());
    build_building(&mut a, &p).unwrap();
    build_building(&mut b, &p).unwrap();
    assert!(a == b);
    p.seed = 5;
    build_building(&mut c, &p).unwrap();
    assert!(a != c, "another seed moves doors or windows");
}

#[test]
fn refusals_leave_the_document_alone() {
    let mut w = empty();
    let mut p = params(rect(6, 6));
    p.storeys = 9;
    assert_eq!(
        build_building(&mut w, &p),
        Err(BuildingRefusal::BadParameter("storeys"))
    );
    // Too small for a stair ring.
    let p = params(rect(2, 6));
    assert_eq!(
        build_building(&mut w, &p),
        Err(BuildingRefusal::NoStairRing)
    );
    let mut p = params(rect(6, 6));
    p.door_sides = vec![WallEdge::North];
    p.storey_height = 1000;
    assert!(build_building(&mut w, &p).is_err());
    assert!(w.rooms.is_empty());
}

#[test]
fn arched_windows_cook_and_flat_stays_the_default() {
    use super::opening::ArchShape;
    let mut p = params(rect(6, 6));
    p.storeys = 1;
    p.stair = false;
    p.roof_parapet = None;
    p.window_arch = Some(ArchShape::Round);
    let mut w = empty();
    let r = build_building(&mut w, &p).unwrap();
    assert!(r.windows >= 4 && r.notes.is_empty(), "{r:?}");
    crate::grid::cook::tests::cook(&w).expect("cooks");
    assert!(BuildingParams::new(RoomTarget::New("x".into()), rect(3, 3))
        .window_arch
        .is_none());
}
