//! Aiming an edit at one span of a column, the cut that takes the slab above
//! a height, and the storey queries the editor's cursor is built on.

use super::edit::focus::{with_span_at, FocusRefusal};
use super::edit::spans::{cut_hole_column, insert_slab_column, CutScope, SpanSkip};
use super::edit::CellRect;
use super::room::GridRoom;
use super::sector::GridSector;
use super::storeys::{storey_above, storey_at, storey_below, Storey, SLAB_THICKNESS};
use super::surface::GridSurface;
use super::units::GridScale;
use super::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A sector with one span: floor at 0, ceiling at `top`.
fn tall(top: i32) -> GridSector {
    GridSector::with_surfaces(
        Some(GridSurface::flat(0, Some(rid(1)))),
        Some(GridSurface::flat(top, Some(rid(2)))),
    )
}

/// A 3 by 1 derived room of tall columns; sector (1, 0) holds a slab at 1536.
fn world_with_slab() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 3, 1);
    for x in 0..3 {
        *room.sector_or_insert(x, 0).unwrap() = tall(3072);
    }
    insert_slab_column(room.sector_mut(1, 0).unwrap(), 1536, SLAB_THICKNESS, None).unwrap();
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn floors(world: &GridWorld, x: usize) -> Vec<i32> {
    world.rooms[0]
        .sector(x, 0)
        .unwrap()
        .spans()
        .iter()
        .filter_map(|s| s.floor.as_ref().map(|f| f.heights[0]))
        .collect()
}

#[test]
fn the_span_open_at_the_height_stands_in_for_the_ground_span_during_the_edit() {
    let mut world = world_with_slab();
    let cell = CellRect::from_corners([1, 0], [1, 0]);
    // The edit sees an ordinary world: it raises "the floor".
    let seen = with_span_at(&mut world, 0, cell, 1536, |w| {
        let sector = w.rooms[0].sector_mut(1, 0).unwrap();
        let before = sector.ground_floor().unwrap().heights[0];
        sector.ground_floor_mut().unwrap().heights = [1792; 4];
        before
    })
    .unwrap();
    assert_eq!(seen, 1536, "the edit saw the upper span's floor");
    // The upper floor moved; the ground floor and the order are as they were.
    assert_eq!(floors(&world, 1), vec![0, 1792]);
    assert_eq!(floors(&world, 0), vec![0]);
}

#[test]
fn the_ground_span_is_aimed_at_by_a_height_inside_it() {
    let mut world = world_with_slab();
    let cell = CellRect::from_corners([1, 0], [1, 0]);
    with_span_at(&mut world, 0, cell, 256, |w| {
        w.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .ground_floor_mut()
            .unwrap()
            .heights = [128; 4];
    })
    .unwrap();
    assert_eq!(floors(&world, 1), vec![128, 1536]);
}

#[test]
fn a_column_with_no_span_open_at_the_height_refuses_and_nothing_moves() {
    let mut world = world_with_slab();
    let before = world.clone();
    // 1400 is inside the slab (1280 to 1536): no span is open there.
    let cell = CellRect::from_corners([1, 0], [1, 0]);
    let refused = with_span_at(&mut world, 0, cell, 1400, |_| ());
    assert_eq!(refused, Err(FocusRefusal::NoSpan { cell: [1, 0] }));
    assert_eq!(world, before);
    assert!(refused.unwrap_err().describe().contains("[1, 0]"));
}

#[test]
fn an_edit_that_breaks_the_order_of_the_column_is_undone() {
    let mut world = world_with_slab();
    let before = world.clone();
    let cell = CellRect::from_corners([1, 0], [1, 0]);
    // The upper floor through its own ceiling.
    let refused = with_span_at(&mut world, 0, cell, 1536, |w| {
        w.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .ground_floor_mut()
            .unwrap()
            .heights = [4096; 4];
    });
    assert_eq!(refused, Err(FocusRefusal::OutOfOrder { cell: [1, 0] }));
    assert_eq!(world, before, "the room is put back");
}

#[test]
fn an_edit_that_adds_a_span_is_refused() {
    let mut world = world_with_slab();
    let before = world.clone();
    let cell = CellRect::from_corners([1, 0], [1, 0]);
    let refused = with_span_at(&mut world, 0, cell, 1536, |w| {
        let sector = w.rooms[0].sector_mut(1, 0).unwrap();
        sector.remove_span(1);
    });
    assert_eq!(refused, Err(FocusRefusal::SpansChanged { cell: [1, 0] }));
    assert_eq!(world, before);
}

#[test]
fn single_span_columns_are_left_alone_and_a_missing_room_is_refused() {
    let mut world = world_with_slab();
    let row = CellRect::from_corners([0, 0], [0, 0]);
    // Height 100 is open in the single span of sector 0: nothing swaps.
    let ran = with_span_at(&mut world, 0, row, 100, |_| 7).unwrap();
    assert_eq!(ran, 7);
    assert_eq!(
        with_span_at(&mut world, 3, row, 0, |_| ()),
        Err(FocusRefusal::NoSuchRoom)
    );
}

#[test]
fn next_cuts_the_first_slab_above_the_height_not_the_one_under_it() {
    let mut sector = tall(6144);
    insert_slab_column(&mut sector, 1536, SLAB_THICKNESS, None).unwrap();
    insert_slab_column(&mut sector, 3072, SLAB_THICKNESS, None).unwrap();
    assert_eq!(sector.span_count(), 3);
    // From the ground, the slab at 1536 is the one over the head.
    assert_eq!(cut_hole_column(&mut sector, 0, CutScope::Next), Ok(1));
    assert_eq!(sector.span_count(), 2);
    let floors: Vec<i32> = sector
        .spans()
        .iter()
        .filter_map(|s| s.floor.as_ref().map(|f| f.heights[0]))
        .collect();
    assert_eq!(floors, vec![0, 3072]);
    // From the slab's own floor, the next one up is the one at 3072; the
    // floor stood on is not "above".
    assert_eq!(
        cut_hole_column(&mut sector, 3072, CutScope::Next),
        Err(SpanSkip::NoSlab)
    );
    assert_eq!(cut_hole_column(&mut sector, 0, CutScope::Next), Ok(1));
    assert_eq!(sector.span_count(), 1);
}

fn list() -> Vec<Storey> {
    [0, 1536, 3072]
        .into_iter()
        .map(|height| Storey { height, cells: 10 })
        .collect()
}

#[test]
fn storeys_are_found_above_below_and_at_a_height() {
    let list = list();
    assert_eq!(storey_above(&list, -1), Some(0));
    assert_eq!(storey_above(&list, 0), Some(1536));
    assert_eq!(storey_above(&list, 1536), Some(3072));
    assert_eq!(storey_above(&list, 3072), None);
    assert_eq!(storey_below(&list, 3072), Some(1536));
    assert_eq!(storey_below(&list, 1536), Some(0));
    assert_eq!(storey_below(&list, 0), None);
    // A surface within the slack above a floor belongs to it; a ceiling just
    // under the next floor still belongs to the storey below.
    assert_eq!(storey_at(&list, 1536, 64), Some(1536));
    assert_eq!(storey_at(&list, 1500, 64), Some(1536));
    assert_eq!(storey_at(&list, 1472, 64), Some(1536));
    assert_eq!(storey_at(&list, 1471, 64), Some(0));
    assert_eq!(storey_at(&list, -300, 64), None);
}
