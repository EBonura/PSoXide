//! The editing layer across leaf sizes (T9): corner drags, Raise and the
//! face lists work on leaves of any size.

use super::heights::{HeightDrag, WeldIndex};
use super::refs::{faces_of_cell, CornerRef, FaceRef, FaceTarget, SurfaceKind};
use super::region::Region;
use super::selection::all_faces_of_room;
use super::verbs::raise::{raise_region, RaiseMode, RaiseParams};
use crate::grid::leaf::LeafKey;
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSector;
use crate::grid::surface::GridSurface;
use crate::grid::tree::{check, merge_all, subdivide, subdivide_to};
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

fn world(w: usize, d: usize) -> GridWorld {
    let mut room = GridRoom::new_derived("r", w, d);
    for z in 0..d {
        for x in 0..w {
            room.set_sector(
                x,
                z,
                Some(GridSector::with_surfaces(
                    Some(GridSurface::flat(0, None)),
                    Some(GridSurface::flat(2560, None)),
                )),
            );
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn floor_corner(room: usize, leaf: LeafKey, corner: u8) -> CornerRef {
    CornerRef {
        face: FaceRef::new(room, leaf, FaceTarget::ground(SurfaceKind::Floor)),
        layer: 0,
        corner,
    }
}

#[test]
fn faces_are_listed_once_under_the_sector_a_leaf_starts_in() {
    let mut w = world(8, 8);
    merge_all(&mut w.rooms[0], None, 0);
    let all = all_faces_of_room(&w, 0);
    // Four big leaves, a floor and a ceiling each.
    assert_eq!(all.len(), 8);
    // The corner sector of the first big leaf lists them; its neighbours
    // inside the same leaf list nothing.
    assert_eq!(faces_of_cell(&w, 0, [0, 0]).len(), 2);
    assert!(faces_of_cell(&w, 0, [1, 0]).is_empty());
    subdivide_to(&mut w.rooms[0], [0, 0], 4).unwrap();
    let all = all_faces_of_room(&w, 0);
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "no face is named twice");
}

#[test]
fn a_weld_finds_the_corners_of_leaves_of_every_size_at_one_point() {
    let mut w = world(8, 8);
    subdivide(&mut w.rooms[0], LeafKey::sector([1, 1])).unwrap();
    let index = WeldIndex::build(&w);
    // The middle of sector (1,1) is the corner of its four children, and
    // the point (1024 * 1.5) is on no other leaf's corner.
    let kids = LeafKey::sector([1, 1]).children().unwrap();
    let c = floor_corner(0, kids[0], 2);
    let welded = index.coincident(&w, &c);
    assert!(
        welded
            .iter()
            .filter(|x| x.face.target == c.face.target)
            .count()
            >= 4
    );
    // A sector corner is shared with the neighbours' corners too.
    let corner = floor_corner(0, kids[0], 0);
    let welded = index.coincident(&w, &corner);
    assert!(welded.len() >= 4, "{welded:?}");
}

#[test]
fn dragging_a_corner_of_a_big_leaf_splits_it_to_sectors_and_moves_only_that_corner() {
    let mut w = world(8, 8);
    merge_all(&mut w.rooms[0], None, 0);
    let big = w.rooms[0].leaf_at([0, 0]).unwrap().0;
    assert_eq!(big.level, 0);
    // The south-east corner of the first big leaf: shared with three others.
    let corner = floor_corner(0, big, 2);
    let drag = HeightDrag::begin_refining(&mut w, &[corner], true);
    assert!(
        drag.len() >= 4,
        "the corner of four leaves moves: {}",
        drag.len()
    );
    drag.apply(&mut w, 256);
    let room = &w.rooms[0];
    assert!(check(room).is_empty(), "{:?}", check(room));
    // Exactly the point (4 sectors, 4 sectors) is lifted.
    let (leaf, _) = room.leaf_at([15, 15]).unwrap();
    assert!(leaf.level >= 2, "{leaf:?}");
    let lifted: i32 = room
        .leaves()
        .flat_map(|(_, s)| s.ground_floor().into_iter().flat_map(|f| f.heights))
        .filter(|h| *h == 256)
        .count() as i32;
    assert!(lifted >= 4);
    // Back to zero restores every height.
    drag.apply(&mut w, 0);
    assert!(w.rooms[0]
        .leaves()
        .all(|(_, s)| s.ground_floor().unwrap().heights == [0; 4]));
}

#[test]
fn lifting_a_coarse_corner_carries_the_hanging_corners_of_finer_neighbours() {
    let mut w = world(4, 4);
    // Sector (2,1) fine; its west neighbour (1,1) stays a sector.
    subdivide(&mut w.rooms[0], LeafKey::sector([2, 1])).unwrap();
    let coarse = LeafKey::sector([1, 1]);
    // Lift the coarse sector's north-east corner (shared with the fine leaf
    // above the hanging corner's edge end).
    let c = floor_corner(0, coarse, 1);
    HeightDrag::begin_refining(&mut w, &[c], true).apply(&mut w, 512);
    let room = &w.rooms[0];
    assert!(check(room).is_empty(), "{:?}", check(room));
    // The hanging corner (middle of the coarse east edge) is the average of
    // the edge's ends: 512 and 0.
    let nw_child = LeafKey::new(3, 8, 4).unwrap();
    let f = room.leaf(nw_child).unwrap().ground_floor().unwrap();
    assert_eq!(f.heights[3], 256, "{:?}", f.heights);
}

#[test]
fn raise_moves_every_leaf_in_a_region_and_splits_a_big_leaf_to_fit() {
    let mut w = world(8, 8);
    merge_all(&mut w.rooms[0], None, 0);
    subdivide_to(&mut w.rooms[0], [20, 0], 4).unwrap();
    // A region of two sectors of the first big leaf plus the fine corner.
    let region = Region::from_cells(0, [[0, 0], [1, 0], [5, 0]]);
    let params = RaiseParams {
        mode: RaiseMode::Offset,
        follow_ceiling: true,
        wall_material: None,
        reference: 0,
    };
    let applied = raise_region(&mut w, &region, params, 256).unwrap();
    assert!(applied.moved > 0);
    let room = &w.rooms[0];
    assert!(check(room).is_empty(), "{:?}", check(room));
    // The two sectors rose; their neighbour in the same big leaf did not.
    let height = |click: [i32; 2]| {
        let (k, s) = room.leaf_at(click).unwrap();
        crate::grid::tree::surface_split::height_in_leaf(
            s.ground_floor().unwrap(),
            click[0] - k.x,
            click[1] - k.z,
            k.edge_clicks(),
        )
    };
    assert_eq!(height([1, 1]), 256);
    assert_eq!(height([5, 1]), 256);
    assert_eq!(height([9, 1]), 0, "the third sector of the leaf is outside");
    assert_eq!(height([21, 1]), 256, "a fine leaf in a region moves too");
}

#[test]
fn lifting_every_corner_of_a_big_leaf_keeps_it_one_plane() {
    let mut w = world(8, 8);
    merge_all(&mut w.rooms[0], None, 0);
    let big = w.rooms[0].leaf_at([0, 0]).unwrap().0;
    let corners: Vec<CornerRef> = (0..4).map(|c| floor_corner(0, big, c)).collect();
    // Unlinked: only the leaf's own four corners move, alike, so the leaf is
    // still a plane and stays one leaf.
    let drag = HeightDrag::begin_refining(&mut w, &corners, false);
    drag.apply(&mut w, 256);
    let room = &w.rooms[0];
    assert_eq!(
        room.leaf(big).unwrap().ground_floor().unwrap().heights,
        [256; 4]
    );
    assert_eq!(room.leaf_count(), 4);
    assert!(check(room).is_empty());
}
