//! Tests of the Opening verb and the doorway helper.

use crate::grid::derive::DerivedKind;
use crate::grid::edit::CellRect;
use crate::grid::leaf::LeafKey;
use crate::grid::leaf_derive::derived_walls_of;
use crate::grid::sector::WallEdge;
use crate::grid::tree::check;

use super::opening::{cut_doorway, cut_opening, OpeningKind, OpeningParams, OpeningRefusal};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams};
use super::room_tests::empty;

fn hall() -> crate::grid::world::GridWorld {
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [3, 3])),
        "hall",
        3072,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    w
}

/// A door two clicks wide in the middle of the north wall of sector (1, 0).
fn door() -> OpeningParams {
    OpeningParams {
        room: 0,
        cell: [1, 0],
        side: WallEdge::North,
        from: 6,
        to: 8,
        kind: OpeningKind::Door,
        top: 1536,
        material: None,
    }
}

fn open_segments(w: &crate::grid::world::GridWorld, room: usize) -> usize {
    w.rooms[room]
        .leaves()
        .flat_map(|(k, _)| derived_walls_of(&w.rooms[room], k))
        .filter(|d| d.kind == DerivedKind::Closure && d.edge == WallEdge::North && d.hidden)
        .count()
}

#[test]
fn a_door_hides_its_segment_and_leaves_a_lintel() {
    let mut w = hall();
    let out = cut_opening(&mut w, &door()).unwrap();
    assert_eq!((out.segments, out.pieces), (1, 1));
    assert_eq!(open_segments(&w, 0), 1);
    let room = &w.rooms[0];
    assert!(check(room).is_empty());
    // The two clicks became a half-sector leaf with the lintel on it.
    let (key, s) = room.leaf_at([6, 0]).unwrap();
    assert_eq!(key.level, 3);
    let wall = &s.walls(WallEdge::North)[0];
    assert_eq!(wall.heights, [1536, 1536, 3072, 3072]);
    // The rest of the wall is still derived and standing.
    assert!(derived_walls_of(room, LeafKey::sector([2, 0]))
        .iter()
        .any(|d| d.edge == WallEdge::North && !d.hidden));
}

#[test]
fn a_window_leaves_a_sill_and_a_lintel() {
    let mut w = hall();
    let mut p = door();
    p.kind = OpeningKind::Window { sill: 768 };
    p.top = 1792;
    let out = cut_opening(&mut w, &p).unwrap();
    assert_eq!(out.pieces, 2);
    assert!(out.door.is_none());
    let room = &w.rooms[0];
    let (_, s) = room.leaf_at([6, 0]).unwrap();
    let mut got: Vec<[i32; 4]> = s.walls(WallEdge::North).iter().map(|x| x.heights).collect();
    got.sort();
    assert_eq!(got, vec![[0, 0, 768, 768], [1792, 1792, 3072, 3072]]);
}

#[test]
fn the_doorway_helper_reports_the_plane_a_door_entity_needs() {
    let mut w = hall();
    let plane = cut_doorway(&mut w, &door()).unwrap();
    assert_eq!(plane.width, 512);
    assert_eq!(plane.height, 1536);
    assert_eq!(plane.axis, crate::grid::overrides::Axis::X);
    assert_eq!(plane.through, [0, -1]);
    assert_eq!(plane.center, [7 * 256, 0, 0]);
}

#[test]
fn refusals_leave_the_document_alone() {
    let mut w = hall();
    let before = w.clone();
    let mut p = door();
    p.top = 300;
    assert!(matches!(
        cut_opening(&mut w, &p),
        Err(OpeningRefusal::TooLow { .. })
    ));
    p.top = 4000;
    assert!(matches!(
        cut_opening(&mut w, &p),
        Err(OpeningRefusal::TopAboveCeiling { .. })
    ));
    p = door();
    p.to = 20;
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::BadRange));
    p = door();
    p.side = WallEdge::Nwse;
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::NotCardinal));
    // An interior side where floors agree has no wall.
    p = door();
    p.side = WallEdge::South;
    p.from = 6;
    p.to = 8;
    p.cell = [1, 1];
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::NotAWall));
    p = door();
    p.room = 3;
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::NoSuchRoom));
    assert!(w == before);
    // Cutting the same door twice refuses the second.
    cut_opening(&mut w, &door()).unwrap();
    assert_eq!(
        cut_opening(&mut w, &door()),
        Err(OpeningRefusal::AlreadyOpen)
    );
}

#[test]
fn a_door_in_a_shared_border_is_mirrored_on_the_next_room() {
    let mut w = hall();
    // A second room directly north of sector (1, 0).
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, -3], [3, -1])),
        "annex",
        3072,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    let out = cut_opening(&mut w, &door()).unwrap();
    assert!(out.mirrored);
    assert_eq!(out.segments, 2);
    let annex = &w.rooms[1];
    let (_, s) = annex.leaf_at([6, -1]).unwrap();
    assert_eq!(s.walls(WallEdge::South).len(), 1);
    assert_eq!(s.walls(WallEdge::South)[0].heights[3], 3072);
}
