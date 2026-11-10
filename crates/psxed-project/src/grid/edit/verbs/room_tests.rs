//! Tests of the Room verb.

use crate::grid::edit::CellRect;
use crate::grid::faces::all_faces;
use crate::grid::tree::check;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

use super::room::{
    decompose, lay_room, Footprint, LeafPolicy, RoomParams, RoomRefusal, RoomTarget,
};

pub(super) fn empty() -> GridWorld {
    GridWorld {
        scale: GridScale::DEFAULT,
        ..GridWorld::default()
    }
}

fn rect(a: [i32; 2], b: [i32; 2]) -> Footprint {
    Footprint::Rect(CellRect::from_corners(a, b))
}

#[test]
fn a_twelve_by_twelve_room_is_nine_big_leaves_and_derivation_closes_it() {
    let mut w = empty();
    let p = RoomParams::new(rect([0, 0], [11, 11]), "hall", 3072);
    let out = lay_room(&mut w, &p).unwrap();
    assert!(out.created);
    assert_eq!((out.cells, out.leaves), (144, 9));
    let room = &w.rooms[out.room];
    assert!(room.leaves().all(|(k, _)| k.level == 0));
    assert!(check(room).is_empty());
    // No authored walls; the closure walls are derived.
    assert!(room.leaves().all(|(_, s)| s.wall_count() == 0));
    let faces = all_faces(&w);
    assert!(faces.len() > 18, "floor, ceiling and the perimeter walls");
}

#[test]
fn leaves_stay_aligned_when_the_footprint_is_not() {
    let mut w = empty();
    let p = RoomParams::new(rect([1, 1], [6, 5]), "r", 3072);
    let out = lay_room(&mut w, &p).unwrap();
    let room = &w.rooms[out.room];
    assert_eq!(
        room.leaves()
            .map(|(k, _)| k.edge_clicks() as i64 * k.edge_clicks() as i64 / 16)
            .sum::<i64>(),
        30
    );
    assert!(room.leaves().all(|(k, _)| k.is_aligned()));
}

#[test]
fn an_octagon_cuts_its_corners_and_a_polygon_follows_its_outline() {
    let mut w = empty();
    let r = CellRect::from_corners([0, 0], [7, 7]);
    let p = RoomParams::new(Footprint::Octagon { rect: r, cut: 3 }, "tower", 3072);
    let out = lay_room(&mut w, &p).unwrap();
    assert_eq!(out.cells, 64 - 4 * 6);
    let room = &w.rooms[out.room];
    assert!(room
        .local([0, 0])
        .and_then(|[x, z]| room.sector(x, z))
        .is_none());
    assert!(room.leaf_at([3 * 4, 0]).is_some());
    // Polygon rule: a triangle.
    let mut w = empty();
    let tri = Footprint::Polygon(vec![[0.0, 0.0], [6.0, 0.0], [0.0, 6.0]]);
    let out = lay_room(&mut w, &RoomParams::new(tri, "t", 3072)).unwrap();
    assert!(out.cells > 10 && out.cells < 36);
}

#[test]
fn a_room_can_go_inside_an_existing_one_and_overwrites_sectors() {
    let mut w = empty();
    let mut p = RoomParams::new(rect([0, 0], [7, 7]), "base", 3072);
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    let mut q = RoomParams::new(rect([2, 2], [5, 5]), "ignored", 4096);
    q.target = RoomTarget::Existing(0);
    q.base = 512;
    let out = lay_room(&mut w, &q).unwrap();
    assert!(!out.created);
    assert_eq!(out.overwritten, 16);
    let room = &w.rooms[0];
    let [x, z] = room.local([3, 3]).unwrap();
    let f = room.sector(x, z).unwrap().ground_floor().unwrap();
    assert_eq!(f.heights, [512; 4]);
    assert!(check(room).is_empty());
}

#[test]
fn refusals_leave_the_document_alone() {
    let mut w = empty();
    let p = RoomParams::new(rect([0, 0], [3, 3]), "r", 100);
    assert!(matches!(
        lay_room(&mut w, &p),
        Err(RoomRefusal::TooLow { .. })
    ));
    let p = RoomParams::new(Footprint::Cells(Default::default()), "r", 3072);
    assert_eq!(lay_room(&mut w, &p), Err(RoomRefusal::EmptyFootprint));
    let mut q = RoomParams::new(rect([0, 0], [3, 3]), "r", 3072);
    q.target = RoomTarget::Existing(4);
    assert_eq!(lay_room(&mut w, &q), Err(RoomRefusal::NoSuchRoom));
    let huge = RoomParams::new(rect([0, 0], [200, 3]), "r", 3072);
    assert_eq!(lay_room(&mut w, &huge), Err(RoomRefusal::RoomTooLarge));
    assert!(w.rooms.is_empty());
}

#[test]
fn decompose_is_deterministic_and_disjoint() {
    let cells: std::collections::BTreeSet<[i32; 2]> =
        CellRect::from_corners([-3, -3], [9, 4]).cells().collect();
    let a = decompose(&cells, LeafPolicy::Maximal);
    assert_eq!(a, decompose(&cells, LeafPolicy::Maximal));
    let total: i32 = a.iter().map(|k| (k.edge_clicks() / 4).max(1).pow(2)).sum();
    assert_eq!(total as usize, cells.len());
}
