//! Tests of the Opening verb and the doorway helper.

use crate::grid::derive::DerivedKind;
use crate::grid::edit::CellRect;
use crate::grid::leaf::LeafKey;
use crate::grid::leaf_derive::derived_walls_of;
use crate::grid::sector::WallEdge;
use crate::grid::tree::check;

use super::opening::{
    cut_doorway, cut_opening, OpeningKind, OpeningParams, OpeningProfile, OpeningRefusal,
};
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
        profile: OpeningProfile::Rectangular,
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
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::NoHost));
    p.to = p.from;
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

// ---- arched heads ----

use super::opening::{arch_head, intrados, ArchShape};

/// `(west height, east height)` of every lintel piece on `side` of the leaves
/// of `cell`'s sector, west to east.
fn head_pieces(
    w: &crate::grid::world::GridWorld,
    room: usize,
    side: WallEdge,
    lo: i32,
    hi: i32,
) -> Vec<(i32, i32, i32)> {
    let r = &w.rooms[room];
    let mut out = Vec::new();
    for (key, s) in r.leaves() {
        let along = if matches!(side, WallEdge::North | WallEdge::South) {
            key.x
        } else {
            key.z
        };
        if along < lo || along >= hi {
            continue;
        }
        for wall in s.walls(side) {
            let h = wall.heights;
            // [bottom a, bottom b, top b, top a]; a is the west or north
            // end for North and East, the other end for South and West.
            let (b0, b1) = match side {
                WallEdge::North | WallEdge::East => (h[0], h[1]),
                _ => (h[1], h[0]),
            };
            assert_eq!(h[2], h[3], "flat top");
            out.push((along, b0, b1));
            let _ = h;
        }
    }
    out.sort();
    out
}

fn arched(shape: ArchShape, from: i32, to: i32, apex: i32) -> OpeningParams {
    OpeningParams {
        profile: arch_head(shape, to - from, apex),
        from,
        to,
        ..door()
    }
}

#[test]
fn the_arch_pieces_tile_the_run_and_land_on_the_lattice() {
    for shape in [ArchShape::Round, ArchShape::Pointed] {
        for (from, to) in [(4, 8), (4, 12)] {
            let mut w = hall();
            let mut p = arched(shape, from, to, 2304);
            p.cell = [1, 0];
            let out = cut_opening(&mut w, &p).unwrap();
            let OpeningProfile::Arched { spring, apex, .. } = p.profile else {
                panic!()
            };
            let pcs = head_pieces(&w, 0, WallEdge::North, from, to);
            assert_eq!(pcs.len() as i32, to - from, "one piece a click");
            assert_eq!(out.pieces as i32, to - from);
            // Pieces meet end to end: no gap, no overlap.
            for pair in pcs.windows(2) {
                assert_eq!(pair[0].0 + 1, pair[1].0);
                assert_eq!(pair[0].2, pair[1].1, "{shape:?} {from}..{to}");
            }
            // Spring at the ends, apex in the middle, all on the lattice.
            assert_eq!(pcs.first().unwrap().1, spring);
            assert_eq!(pcs.last().unwrap().2, spring);
            let top = pcs.iter().map(|p| p.1.max(p.2)).max().unwrap();
            assert_eq!(top, apex);
            for (_, a, b) in &pcs {
                assert_eq!((a % 64, b % 64), (0, 0));
            }
            // Symmetric about the middle.
            let n = pcs.len();
            for i in 0..n {
                assert_eq!(pcs[i].1, pcs[n - 1 - i].2);
            }
            assert!(crate::grid::tree::check(&w.rooms[0]).is_empty());
        }
    }
}

#[test]
fn a_pointed_flank_is_leaner_than_a_round_one_of_the_same_rise() {
    let (spring, apex, width) = (1024, 2048, 1024);
    let mut sharper = 0;
    for u in [-384, -256, -128, 0, 128, 256, 384] {
        let r = intrados(ArchShape::Round, spring, apex, width, u);
        let p = intrados(ArchShape::Pointed, spring, apex, width, u);
        assert!(r >= spring && p >= spring && r <= apex && p <= apex);
        assert!(p <= r, "the ellipse is fuller at the shoulder");
        if p < r {
            sharper += 1;
        }
    }
    assert!(sharper > 0, "the pointed flank is leaner");
    assert_eq!(intrados(ArchShape::Round, spring, apex, width, 512), spring);
    assert_eq!(intrados(ArchShape::Pointed, spring, apex, width, 0), apex);
}

#[test]
fn every_side_traces_the_same_arch() {
    // South and west walls walk the other way: the pieces still meet.
    for (cell, side) in [([1, 3], WallEdge::South), ([0, 1], WallEdge::West)] {
        let mut w = hall();
        let along = if matches!(side, WallEdge::South) {
            cell[0]
        } else {
            cell[1]
        };
        let mut p = arched(ArchShape::Round, along * 4, along * 4 + 4, 2304);
        p.cell = cell;
        p.side = side;
        cut_opening(&mut w, &p).unwrap();
        let pcs = head_pieces(&w, 0, side, along * 4, along * 4 + 4);
        assert_eq!(pcs.len(), 4);
        for pair in pcs.windows(2) {
            assert_eq!(pair[0].2, pair[1].1, "{side:?}");
        }
        assert_eq!(pcs[0].1, pcs[3].2);
    }
}

#[test]
fn bad_arches_are_refused_and_a_low_spring_is_too_low_for_a_door() {
    let mut w = hall();
    let before = w.clone();
    let mut p = arched(ArchShape::Round, 4, 8, 2304);
    p.profile = OpeningProfile::Arched {
        shape: ArchShape::Round,
        spring: 2304,
        apex: 2304,
        segment_clicks: 1,
    };
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::BadArch));
    p.profile = OpeningProfile::Arched {
        shape: ArchShape::Round,
        spring: 256,
        apex: 1024,
        segment_clicks: 1,
    };
    assert!(matches!(
        cut_opening(&mut w, &p),
        Err(OpeningRefusal::TooLow { .. })
    ));
    p.profile = OpeningProfile::Arched {
        shape: ArchShape::Round,
        spring: 1536,
        apex: 2304,
        segment_clicks: 3,
    };
    assert_eq!(cut_opening(&mut w, &p), Err(OpeningRefusal::BadArch));
    assert!(w == before);
}

#[test]
fn an_arch_is_mirrored_across_a_room_border() {
    let mut w = hall();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, -3], [3, -1])),
        "annex",
        3072,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    let mut a = arched(ArchShape::Pointed, 4, 8, 2304);
    a.cell = [1, 0];
    let out = cut_opening(&mut w, &a).unwrap();
    assert!(out.mirrored);
    let there = head_pieces(&w, 1, WallEdge::South, 4, 8);
    assert_eq!(there.len(), 4);
    for pair in there.windows(2) {
        assert_eq!(pair[0].2, pair[1].1);
    }
}
