//! Cooking columns of several spans: the span block of a dense room, the
//! single-span room that carries none, and the refusal of a leaf-tree room.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{RoomView, ROOM_SPANS};

use super::tests::{cook, rid};
use crate::grid::edit::spans::insert_slab_column;
use crate::grid::leaf::LeafKey;
use crate::grid::room::GridRoom;
use crate::grid::storeys::SLAB_THICKNESS;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

/// A 2 by 1 derived hall, a tall column of spans at (0, 0) and a plain one
/// at (1, 0).
fn two_storey_world() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 2, 1);
    for x in 0..2 {
        let sector = room.sector_or_insert(x, 0).unwrap();
        sector.set_ground_floor(Some(GridSurface::flat(0, Some(rid(1)))));
        sector.set_ground_ceiling(Some(GridSurface::flat(3072, Some(rid(2)))));
    }
    insert_slab_column(
        room.sector_mut(0, 0).unwrap(),
        1024,
        SLAB_THICKNESS,
        Some(rid(3)),
    )
    .unwrap();
    GridWorld {
        scale: GridScale { num: 5, den: 4 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

#[test]
fn a_column_of_spans_is_cooked_into_the_span_block() {
    let cooked = cook(&two_storey_world()).expect("cook");
    let chunk = &cooked.chunks[0];
    let words = aligned_copy(chunk);
    let view = RoomView::parse(words_as_bytes(&words, chunk.len())).expect("room parses");
    assert_eq!(view.col.header.flags & ROOM_SPANS, ROOM_SPANS);
    // Record 0 is the cell (0, 0) with the upper span; record 1 is plain.
    let upper = view.col.extra_spans(0);
    assert_eq!(upper.len(), 1);
    assert!(view.col.extra_spans(1).is_empty());
    // The upper floor sits above the ground ceiling of the slab.
    let ground = view.col.sectors[0];
    assert!(upper[0].floor[0] > ground.ceil[0]);
    assert_eq!(
        cooked.tables[0].spans.as_ref().map(|s| s.spans.len()),
        Some(1)
    );
}

#[test]
fn a_single_span_room_carries_no_span_block() {
    let mut world = two_storey_world();
    let sector = world.rooms[0].sector_mut(0, 0).unwrap();
    *sector = crate::grid::sector::GridSector::with_surfaces(
        Some(GridSurface::flat(0, Some(rid(1)))),
        Some(GridSurface::flat(3072, Some(rid(2)))),
    );
    let cooked = cook(&world).expect("cook");
    assert!(cooked.tables[0].spans.is_none());
    let chunk = &cooked.chunks[0];
    let words = aligned_copy(chunk);
    let view = RoomView::parse(words_as_bytes(&words, chunk.len())).expect("room parses");
    assert_eq!(view.col.header.flags & ROOM_SPANS, 0);
}

#[test]
fn a_column_of_spans_in_a_room_with_fine_leaves_is_refused_for_now() {
    let mut world = two_storey_world();
    let room = &mut world.rooms[0];
    let fine = room.sector(0, 0).unwrap().clone();
    // Make a leaf finer than a sector so the room is no longer whole sectors.
    room.leaves
        .remove(LeafKey::sector([0, 0]))
        .expect("the sector leaf");
    let quarter = LeafKey::new(4, 0, 0).expect("a quarter leaf");
    room.leaves
        .insert(quarter, fine)
        .expect("insert quarter leaf");
    let err = cook(&world).expect_err("refused");
    assert!(err.contains("several sizes"), "{err}");
}
