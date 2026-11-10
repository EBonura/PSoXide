//! Tests of the Cathedral verb.

use crate::grid::tree::check;

use super::cathedral::{build_cathedral, CathedralParams, CathedralRefusal, Transept};
use super::room::RoomTarget;
use super::room_tests::empty;

fn params() -> CathedralParams {
    CathedralParams::new(RoomTarget::New("cathedral".into()), [0, 0])
}

#[test]
fn the_bay_sets_the_arches_the_windows_and_the_door() {
    let mut w = empty();
    let r = build_cathedral(&mut w, &params()).unwrap();
    assert_eq!(r.arches, 10, "one arch a bay on each border: {:?}", r.notes);
    assert_eq!(
        r.windows, 10,
        "one window a bay on each side: {:?}",
        r.notes
    );
    assert_eq!(r.doors.len(), 2, "{:?}", r.notes);
    assert!(r.notes.is_empty(), "{:?}", r.notes);
    assert_eq!(r.nave.width(), 10);
    assert_eq!(r.nave.depth(), 4);
    let room = &w.rooms[r.room];
    assert_eq!(room.meshes.len(), 10);
    assert!(check(room).is_empty());
    // Aisles are lower than the nave.
    let ceil = |cell: [i32; 2]| {
        let [x, z] = room.local(cell).unwrap();
        room.sector(x, z).unwrap().ground_ceiling().unwrap().heights[0]
    };
    assert_eq!(ceil([3, 0]), 5120);
    assert_eq!(ceil([3, 3]), 9216);
}

#[test]
fn a_bigger_bay_count_scales_everything_and_is_deterministic() {
    let mut p = params();
    p.bays = 8;
    let (mut a, mut b) = (empty(), empty());
    let ra = build_cathedral(&mut a, &p).unwrap();
    build_cathedral(&mut b, &p).unwrap();
    assert_eq!((ra.arches, ra.windows), (16, 16));
    assert!(a == b);
}

#[test]
fn the_transept_interrupts_the_arcade_and_the_clerestory() {
    let mut p = params();
    p.transept = Some(Transept { at_bay: 2, arm: 3 });
    let mut w = empty();
    let r = build_cathedral(&mut w, &p).unwrap();
    assert_eq!(r.arches, 8, "bay 2 has no arcade: {:?}", r.notes);
    assert_eq!(r.windows, 8);
    // The arms stand at the nave height, north and south of the aisles.
    let room = &w.rooms[r.room];
    let [x, z] = room.local([4, -2]).unwrap();
    assert!(room.sector(x, z).is_some());
    let [x, z] = room.local([4, 9]).unwrap();
    assert!(room.sector(x, z).is_some());
}

#[test]
fn the_chancel_is_raised_and_reached_by_steps() {
    let mut p = params();
    p.chancel_rise = 768;
    let mut w = empty();
    let r = build_cathedral(&mut w, &p).unwrap();
    let room = &w.rooms[r.room];
    let floor = |cell: [i32; 2]| {
        let [x, z] = room.local(cell).unwrap();
        room.sector(x, z).unwrap().ground_floor().unwrap().heights[0]
    };
    // Chancel is the last bay: x 8 and 9. Two treads before it.
    assert_eq!(floor([9, 3]), 768);
    assert_eq!(floor([8, 3]), 768);
    assert_eq!(floor([7, 3]), 512);
    assert_eq!(floor([6, 3]), 256);
    assert_eq!(floor([5, 3]), 0);
    // The apse continues at the chancel height and is polygonal.
    assert_eq!(floor([10, 3]), 768);
}

#[test]
fn bad_parameters_are_refused_and_nothing_changes() {
    let mut w = empty();
    let mut p = params();
    p.bays = 0;
    assert_eq!(
        build_cathedral(&mut w, &p),
        Err(CathedralRefusal::BadParameter("bays"))
    );
    let mut p = params();
    p.aisle_height = p.nave_height;
    assert!(build_cathedral(&mut w, &p).is_err());
    let mut p = params();
    p.chancel_rise = 300;
    assert!(build_cathedral(&mut w, &p).is_err());
    assert!(w.rooms.is_empty());
}

#[test]
fn a_cathedral_cooks() {
    let mut w = empty();
    let mut p = params();
    p.transept = Some(Transept { at_bay: 2, arm: 2 });
    build_cathedral(&mut w, &p).unwrap();
    crate::grid::cook::tests::cook(&w).expect("a cathedral cooks");
}
