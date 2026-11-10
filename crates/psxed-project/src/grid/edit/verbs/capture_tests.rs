//! Tests of capturing a bay and repeating it.

use crate::grid::edit::CellRect;
use crate::grid::leaf::LeafKey;
use crate::grid::sector::WallEdge;
use crate::grid::surface::GridWall;

use super::capture::{capture_bay, repeat_bay, Along, CaptureRefusal};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomTarget};
use super::room_tests::empty;
use super::style::{self, Roles};
use super::styled::{lay_styled_room, StyledRoom};

/// A hand-edited bay: a dome room's quarter plus an authored wall piece and a
/// raised cell.
fn source() -> crate::grid::world::GridWorld {
    let mut w = empty();
    let rect = CellRect::from_corners([0, 0], [3, 3]);
    lay_styled_room(
        &mut w,
        &StyledRoom {
            target: RoomTarget::New("sculpt".into()),
            rect,
            base: 512,
            style: style::cloister_room(),
            roles: Roles::default(),
        },
    )
    .unwrap();
    let r = &mut w.rooms[0];
    // A hand wall and a raised cell.
    let (key, _) = r.leaf_at([1, 1]).unwrap();
    r.leaf_mut(key)
        .unwrap()
        .walls_mut(WallEdge::North)
        .push(GridWall::span(512, 1024, None));
    if let Some(f) = r
        .leaf_mut(LeafKey::sector([2, 2]))
        .and_then(|s| s.ground_floor_mut())
    {
        f.heights = [768; 4];
    }
    w
}

#[test]
fn capture_then_repeat_reproduces_the_source_bay_exactly() {
    let mut w = source();
    let rect = CellRect::from_corners([0, 0], [3, 3]);
    let t = capture_bay(&w, 0, rect).unwrap();
    assert_eq!(t.size, [4, 4]);
    assert_eq!(t.reference, 512);
    // Repeat three times along +X at a different height, past the source.
    let laid = repeat_bay(&mut w, 0, &t, [4, 0], Along::X, 3, 1024).unwrap();
    assert_eq!(laid, 3 * t.leaves.len());
    let room = &w.rooms[0];
    for n in 0..3 {
        let copy = capture_bay(
            &w,
            0,
            CellRect::from_corners([4 + 4 * n, 0], [7 + 4 * n, 3]),
        )
        .unwrap();
        // The copy, taken relative to its own lowest floor, is the source.
        assert_eq!(copy.leaves, t.leaves, "copy {n} equals the source");
        assert_eq!(copy.reference, 1024);
    }
    // The first bay is untouched.
    assert_eq!(capture_bay(&w, 0, rect).unwrap(), t);
    assert!(crate::grid::tree::check(room).is_empty());
    crate::grid::cook::tests::cook(&w).expect("cooks");
}

#[test]
fn a_template_round_trips_through_ron() {
    let w = source();
    let t = capture_bay(&w, 0, CellRect::from_corners([0, 0], [3, 3])).unwrap();
    let text = ron::ser::to_string(&t).unwrap();
    let back: super::capture::BayTemplate = ron::from_str(&text).unwrap();
    assert_eq!(back, t);
}

#[test]
fn what_cannot_be_captured_is_refused_whole() {
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [7, 7])),
        "r",
        3072,
    );
    p.leaves = LeafPolicy::Maximal;
    lay_room(&mut w, &p).unwrap();
    // A level-0 leaf crosses the edge of a 2 by 2 rectangle.
    assert!(matches!(
        capture_bay(&w, 0, CellRect::from_corners([0, 0], [1, 1])),
        Err(CaptureRefusal::LeafCrossesEdge(_))
    ));
    assert_eq!(
        capture_bay(&w, 0, CellRect::from_corners([20, 20], [21, 21])),
        Err(CaptureRefusal::Empty)
    );
    assert_eq!(
        capture_bay(&w, 3, CellRect::from_corners([0, 0], [1, 1])),
        Err(CaptureRefusal::NoSuchRoom)
    );
    let t = capture_bay(&w, 0, CellRect::from_corners([0, 0], [3, 3])).unwrap();
    let before = w.clone();
    assert_eq!(
        repeat_bay(&mut w, 0, &t, [0, 0], Along::X, 0, 0),
        Err(CaptureRefusal::NoCopies)
    );
    assert!(w == before);
}
