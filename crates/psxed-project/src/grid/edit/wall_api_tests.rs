//! Tests of the one-wall API.

use std::collections::BTreeMap;

use crate::grid::edit::verbs::opening::{OpeningKind, OpeningProfile};
use crate::grid::edit::verbs::room::{lay_room, Footprint, LeafPolicy, RoomParams};
use crate::grid::edit::verbs::room_tests::empty;
use crate::grid::edit::CellRect;
use crate::grid::faces::all_faces;
use crate::grid::leaf::LeafKey;
use crate::grid::sector::WallEdge;
use crate::grid::world::GridWorld;

use super::wall_api::*;

/// Two cells: a floor at 0 on the left, raised to 768 on the right, so the
/// edge between them has a derived riser.
fn level() -> GridWorld {
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [3, 1])),
        "r",
        4096,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    raise(&mut w, [2, 0], 768);
    w
}

fn raise(w: &mut GridWorld, cell: [i32; 2], to: i32) {
    let s = w.rooms[0].leaf_mut(LeafKey::sector(cell)).unwrap();
    s.ground_floor_mut().unwrap().heights = [to; 4];
}

/// A multiset of the drawn polygons, material included, for comparing worlds.
fn faces(w: &GridWorld) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for f in all_faces(w) {
        *out.entry(format!("{:?} {:?}", f.points, f.material))
            .or_insert(0) += 1;
    }
    out
}

/// The riser the raised cell (2, 0) casts, seen from the lower cell (1, 0).
fn riser(w: &GridWorld) -> WallView {
    let views = walls_at(w, 0, LeafKey::sector([1, 0]), WallEdge::East);
    views
        .into_iter()
        .find(|v| !v.addr.authored)
        .expect("a riser")
}

#[test]
fn a_generated_wall_reads_as_automatic_and_a_record_as_set() {
    let mut w = level();
    let r = riser(&w);
    assert!(!r.heights.is_set());
    assert_eq!((r.heights.value()[0], r.heights.value()[3]), (0, 768));
    // A hand-placed wall on the same side reads the same way, as Set.
    let a = add_wall(
        &mut w,
        0,
        LeafKey::sector([2, 1]),
        WallEdge::South,
        [0, 0, 1024, 1024],
        None,
    )
    .unwrap();
    let v = walls_at(&w, 0, LeafKey::sector([2, 1]), WallEdge::South)
        .into_iter()
        .find(|v| v.addr == a)
        .unwrap();
    assert!(v.heights.is_set() && v.material.is_set() && v.solid.is_set());
}

#[test]
fn setting_heights_on_a_generated_wall_equals_the_same_on_a_hand_placed_one() {
    // World A: the generated riser, raised on top by 256.
    let mut a = level();
    let ra = riser(&a).addr;
    let (_, warn_a) = set_wall_heights(&mut a, ra, [None, None, Some(1024), Some(1024)]).unwrap();
    // World B: the same wall hand-placed first (the generated face hidden
    // and a record at the original heights), then the same edit.
    let mut b = level();
    let r = riser(&b);
    let (placed, _) = set_wall_heights(&mut b, r.addr, [None; 4]).unwrap();
    let (_, warn_b) =
        set_wall_heights(&mut b, placed, [None, None, Some(1024), Some(1024)]).unwrap();
    assert_eq!(faces(&a), faces(&b), "one visible result");
    assert_eq!(warn_a, warn_b);
    // The wall is taller than the step: it stands above the neighbour's
    // floor without a gap below.
    assert!(faces(&a) != faces(&level()));
}

#[test]
fn a_set_wall_stays_when_cells_move_and_an_automatic_one_follows() {
    let mut w = level();
    // Another riser further along follows the cells; this one is set.
    raise(&mut w, [3, 0], 768);
    let ra = riser(&w).addr;
    let (placed, _) = set_wall_heights(&mut w, ra, [None, None, Some(1024), Some(1024)]).unwrap();
    let before = walls_at(&w, 0, LeafKey::sector([1, 0]), WallEdge::East);
    let set_before = before
        .iter()
        .find(|v| v.addr == placed)
        .unwrap()
        .heights
        .value();
    // Raise the cell it stands on: the set wall stays, a riser elsewhere follows.
    raise(&mut w, [2, 0], 1280);
    let after = walls_at(&w, 0, LeafKey::sector([1, 0]), WallEdge::East);
    assert_eq!(
        after
            .iter()
            .find(|v| v.addr == placed)
            .unwrap()
            .heights
            .value(),
        set_before,
        "a set wall stays where it was put"
    );
    // Between (2,0) and (3,0) the riser the move made follows the cells.
    let east = walls_at(&w, 0, LeafKey::sector([3, 0]), WallEdge::West);
    let auto = east
        .iter()
        .find(|v| !v.addr.authored)
        .expect("a derived riser");
    assert!(!auto.heights.is_set());
    assert!(auto.heights.value().contains(&1280), "{:?}", auto.heights);
    raise(&mut w, [2, 0], 1536);
    let again = walls_at(&w, 0, LeafKey::sector([3, 0]), WallEdge::West);
    let auto = again.iter().find(|v| !v.addr.authored).unwrap();
    assert!(
        auto.heights.value().contains(&1536),
        "automatic follows again"
    );
}

#[test]
fn clear_returns_a_wall_to_derivation() {
    let original = faces(&level());
    let mut w = level();
    let ra = riser(&w).addr;
    let (placed, _) = set_wall_heights(&mut w, ra, [None, None, Some(1024), Some(1024)]).unwrap();
    assert_ne!(faces(&w), original);
    clear_wall_property(&mut w, placed, WallProp::Heights).unwrap();
    assert_eq!(faces(&w), original, "back to what the cells give");
    // Material: set, then cleared.
    let r = riser(&w);
    let m: crate::ResourceId = ron::from_str("(9)").unwrap();
    set_wall_material(&mut w, r.addr, Some(m)).unwrap();
    assert!(riser(&w).material.is_set());
    let ra = riser(&w).addr;
    clear_wall_property(&mut w, ra, WallProp::Material).unwrap();
    assert_eq!(faces(&w), original);
}

#[test]
fn gaps_are_reported_not_refused() {
    let mut w = level();
    // A closure wall on the room's outside: a top under the ceiling leaves
    // the room open above it.
    let closure = walls_at(&w, 0, LeafKey::sector([0, 0]), WallEdge::North)
        .into_iter()
        .find(|v| !v.addr.authored)
        .unwrap();
    let (_, warn) =
        set_wall_heights(&mut w, closure.addr, [None, None, Some(1024), Some(1024)]).unwrap();
    assert_eq!(warn, vec![WallWarning::GapAbove(4096 - 1024)]);
    // A bottom above the floor leaves a gap under it.
    let (_, warn) =
        set_wall_heights(&mut w, closure.addr, [Some(512), Some(512), None, None]).unwrap();
    assert!(
        warn.iter().any(|x| matches!(x, WallWarning::GapBelow(512))),
        "{warn:?}"
    );
}

#[test]
fn add_and_remove_a_wall_where_none_is_generated() {
    let mut w = level();
    let before = faces(&w);
    let a = add_wall(
        &mut w,
        0,
        LeafKey::sector([0, 0]),
        WallEdge::East,
        [0, 0, 512, 512],
        None,
    )
    .unwrap();
    assert_ne!(faces(&w), before);
    remove_wall(&mut w, a).unwrap();
    assert_eq!(faces(&w), before);
    // Removing a generated wall hides it.
    let ra = riser(&w).addr;
    remove_wall(&mut w, ra).unwrap();
    assert!(faces(&w).len() < before.len());
}

#[test]
fn an_opening_can_be_cut_and_closed_on_any_storey() {
    use crate::grid::edit::spans::insert_slab;
    let mut w = empty();
    let mut p = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([0, 0], [3, 3])),
        "tall",
        6144,
    );
    p.leaves = LeafPolicy::Sectors;
    lay_room(&mut w, &p).unwrap();
    let out = insert_slab(
        &mut w,
        0,
        CellRect::from_corners([0, 0], [3, 3]),
        3072,
        256,
        None,
    );
    assert!(out.changed > 0);
    let leaf = LeafKey::sector([1, 0]);
    let before = faces(&w);
    let upper = walls_at(&w, 0, leaf, WallEdge::North)
        .into_iter()
        .find(|v| v.addr.span == 1)
        .expect("an upper storey wall");
    let spec = OpeningSpec {
        kind: OpeningKind::Window { sill: 768 },
        from: 4,
        to: 8,
        top: 1792,
        profile: OpeningProfile::Rectangular,
        material: None,
    };
    set_wall_opening(&mut w, upper.addr, Some(spec)).unwrap();
    // The lower storey's wall still stands: one derived face per storey is
    // visible on that side (the lower one is a record or still derived).
    let views = walls_at(&w, 0, leaf, WallEdge::North);
    let lower_standing = views.iter().any(|v| v.addr.span == 0);
    assert!(lower_standing, "{views:?}");
    assert!(
        views.iter().any(|v| v.opening.is_some()),
        "an opening reads back"
    );
    assert_ne!(faces(&w), before);
    // Closing it gives every storey back to derivation.
    let open = views.iter().find(|v| v.opening.is_some()).unwrap().addr;
    set_wall_opening(&mut w, open, None).unwrap();
    assert_eq!(faces(&w), before, "closed again");
}
