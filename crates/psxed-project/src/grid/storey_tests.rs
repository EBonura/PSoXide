//! Storey primitives on hand-built columns and rooms: the slab split, the
//! sliver rule, the refusals, the round trips and the storey list.

use super::edit::spans::{
    column_is_ordered, cut_hole, cut_hole_column, fill_hole, insert_slab, insert_slab_column,
    shift_above, shift_above_column, CutScope, SpanRefusal, SpanSkip,
};
use super::edit::CellRect;
use super::room::GridRoom;
use super::sector::{GridSector, GridSpan, WallEdge};
use super::storeys::{normalize_column, storeys, Storey, MIN_HEADROOM, SLAB_THICKNESS};
use super::surface::{GridSurface, GridWall};
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

/// Floor heights of a column, bottom to top, `None` for a pit.
fn floors(sector: &GridSector) -> Vec<Option<i32>> {
    sector
        .spans()
        .iter()
        .map(|s| s.floor.as_ref().map(|f| f.heights[0]))
        .collect()
}

/// Ceiling heights of a column, bottom to top, `None` for open sky.
fn ceilings(sector: &GridSector) -> Vec<Option<i32>> {
    sector
        .spans()
        .iter()
        .map(|s| s.ceiling.as_ref().map(|c| c.heights[0]))
        .collect()
}

fn hall(side: usize, top: i32) -> GridRoom {
    let mut room = GridRoom::new_derived("hall", side, side);
    for z in 0..side {
        for x in 0..side {
            *room.sector_or_insert(x, z).unwrap() = tall(top);
        }
    }
    room
}

fn world(room: GridRoom) -> GridWorld {
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

#[test]
fn a_slab_splits_a_tall_span_into_two() {
    let mut sector = tall(2048);
    insert_slab_column(&mut sector, 1024, SLAB_THICKNESS, Some(rid(3))).unwrap();
    assert_eq!(sector.span_count(), 2);
    // The walking surfaces are at 0 and 1024; the slab is solid between
    // 768 and 1024.
    assert_eq!(floors(&sector), vec![Some(0), Some(1024)]);
    assert_eq!(
        ceilings(&sector),
        vec![Some(1024 - SLAB_THICKNESS), Some(2048)]
    );
    assert!(column_is_ordered(&sector));
}

#[test]
fn a_sliver_below_the_slab_is_filled_and_the_floor_rises() {
    // A floor at 0 with a slab at 512 would leave 256 under it: a sliver.
    let mut sector = tall(2048);
    insert_slab_column(&mut sector, 512, SLAB_THICKNESS, None).unwrap();
    assert_eq!(sector.span_count(), 1);
    assert_eq!(floors(&sector), vec![Some(512)]);
    assert_eq!(ceilings(&sector), vec![Some(2048)]);
}

#[test]
fn a_slab_that_leaves_too_little_headroom_above_is_refused() {
    let mut sector = tall(2048);
    let before = sector.clone();
    let y = 2048 - MIN_HEADROOM + 1;
    assert_eq!(
        insert_slab_column(&mut sector, y, SLAB_THICKNESS, None),
        Err(SpanSkip::Headroom)
    );
    assert_eq!(sector, before);
}

#[test]
fn a_height_outside_every_span_is_skipped() {
    let mut sector = tall(1024);
    // Exactly at the ceiling: no open volume holds it.
    assert_eq!(
        insert_slab_column(&mut sector, 1024, SLAB_THICKNESS, None),
        Err(SpanSkip::NoSpanAtHeight)
    );
    // A floor already sits there: nothing to insert.
    assert_eq!(
        insert_slab_column(&mut sector, 0, SLAB_THICKNESS, None),
        Err(SpanSkip::NoSpanAtHeight)
    );
}

#[test]
fn a_sloped_surface_across_the_height_is_skipped_not_cut() {
    let mut sector = GridSector::with_surfaces(
        Some({
            let mut floor = GridSurface::flat(0, None);
            floor.heights = [0, 0, 1024, 1024];
            floor
        }),
        Some(GridSurface::flat(2048, None)),
    );
    let before = sector.clone();
    assert_eq!(
        insert_slab_column(&mut sector, 512, SLAB_THICKNESS, None),
        Err(SpanSkip::Sloped)
    );
    assert_eq!(sector, before);
}

#[test]
fn cutting_the_slab_restores_the_column() {
    let original = tall(2048);
    let mut sector = original.clone();
    insert_slab_column(&mut sector, 1024, SLAB_THICKNESS, None).unwrap();
    assert_eq!(cut_hole_column(&mut sector, 1024, CutScope::One), Ok(1));
    assert_eq!(sector, original);
    // Nothing above the height: the second cut has no slab to take.
    assert_eq!(
        cut_hole_column(&mut sector, 1024, CutScope::One),
        Err(SpanSkip::NoSlab)
    );
}

#[test]
fn every_slab_above_is_cut_at_once() {
    let mut sector = tall(4096);
    insert_slab_column(&mut sector, 1024, SLAB_THICKNESS, None).unwrap();
    insert_slab_column(&mut sector, 2048, SLAB_THICKNESS, None).unwrap();
    assert_eq!(sector.span_count(), 3);
    assert_eq!(
        cut_hole_column(&mut sector, 1024, CutScope::AllAbove),
        Ok(2)
    );
    assert_eq!(sector.span_count(), 1);
    assert_eq!(ceilings(&sector), vec![Some(4096)]);
}

#[test]
fn a_fill_undoes_a_cut_in_the_room() {
    let mut room_world = world(hall(2, 2048));
    let rect = CellRect::from_corners([0, 0], [1, 1]);
    let original = room_world.clone();
    let slab = insert_slab(&mut room_world, 0, rect, 1024, SLAB_THICKNESS, None);
    assert_eq!(slab.changed, 4);
    let cut = cut_hole(&mut room_world, 0, rect, 1024, CutScope::One);
    assert_eq!(cut.changed, 4);
    let mut back = room_world.clone();
    assert_eq!(
        fill_hole(&mut back, 0, rect, 1024, SLAB_THICKNESS, None).changed,
        4
    );
    assert_eq!(
        cut_hole(&mut room_world, 0, rect, 1024, CutScope::One).changed,
        0
    );
    assert_eq!(
        room_world.rooms[0].sector(0, 0),
        original.rooms[0].sector(0, 0)
    );
}

#[test]
fn shifting_above_moves_the_upper_storey_and_back() {
    let mut sector = tall(2048);
    insert_slab_column(&mut sector, 1024, SLAB_THICKNESS, None).unwrap();
    let mut world = world(hall(1, 0));
    *world.rooms[0].sector_mut(0, 0).unwrap() = sector.clone();
    let before = world.clone();
    let shift = shift_above(&mut world, 0, None, 1024, 256);
    assert_eq!(shift.changed, 1);
    assert_eq!(
        floors(world.rooms[0].sector(0, 0).unwrap()),
        vec![Some(0), Some(1280)]
    );
    assert_eq!(
        ceilings(world.rooms[0].sector(0, 0).unwrap()),
        vec![Some(1024 - SLAB_THICKNESS), Some(2304)]
    );
    shift_above(&mut world, 0, None, 1280, -256);
    assert_eq!(world.rooms[0].sector(0, 0), before.rooms[0].sector(0, 0));
}

#[test]
fn a_shift_that_would_overlap_is_refused_and_changes_nothing() {
    let mut room = hall(2, 2048);
    let mut sector = tall(2048);
    insert_slab_column(&mut sector, 1024, SLAB_THICKNESS, None).unwrap();
    *room.sector_mut(1, 1).unwrap() = sector;
    let mut world = world(room);
    let before = world.clone();
    let shift = shift_above(&mut world, 0, None, 1024, -400);
    assert!(matches!(
        shift.refused,
        Some(SpanRefusal::Interpenetrates { .. })
    ));
    assert_eq!(shift.changed, 0);
    assert_eq!(world.rooms[0].sector(1, 1), before.rooms[0].sector(1, 1));
}

#[test]
fn shift_above_moves_authored_walls_too() {
    let mut sector = tall(2048);
    sector
        .walls_mut(WallEdge::North)
        .push(GridWall::span(1100, 1500, None));
    shift_above_column(&mut sector, 1024, 256);
    let wall = &sector.walls(WallEdge::North)[0];
    assert_eq!(wall.heights, [1356, 1356, 1756, 1756]);
}

#[test]
fn normalize_removes_a_sliver_span_and_keeps_a_lone_one() {
    let mut sector = tall(2048);
    sector.insert_span(
        1,
        GridSpan {
            floor: Some(GridSurface::flat(1024, None)),
            ceiling: Some(GridSurface::flat(1024 + 128, None)),
            wall_material: None,
        },
    );
    sector.insert_span(
        2,
        GridSpan {
            floor: Some(GridSurface::flat(1500, None)),
            ceiling: Some(GridSurface::flat(2048 + 512, None)),
            wall_material: None,
        },
    );
    assert_eq!(normalize_column(&mut sector), 1);
    assert_eq!(floors(&sector), vec![Some(0), Some(1500)]);
    let mut lone = GridSector::with_surfaces(
        Some(GridSurface::flat(0, None)),
        Some(GridSurface::flat(64, None)),
    );
    assert_eq!(normalize_column(&mut lone), 0);
    assert_eq!(lone.span_count(), 1);
}

#[test]
fn storeys_are_the_floor_heights_many_cells_share() {
    let mut room = hall(4, 4096);
    // Sixteen cells at 3072 above the ground, one stray cell at 512.
    for z in 0..4 {
        for x in 0..4 {
            let sector = room.sector_mut(x, z).unwrap();
            insert_slab_column(sector, 3072, SLAB_THICKNESS, None).unwrap();
        }
    }
    room.sector_mut(0, 0).unwrap().spans_mut()[0].floor = Some(GridSurface::flat(512, None));
    let found = storeys(&world(room));
    assert_eq!(
        found,
        vec![
            Storey {
                height: 0,
                cells: 15
            },
            Storey {
                height: 3072,
                cells: 16
            },
        ]
    );
}

#[test]
fn a_height_on_fewer_than_the_threshold_cells_is_not_a_storey() {
    let mut room = hall(2, 2048);
    room.sector_mut(0, 0).unwrap().spans_mut()[0].floor = Some(GridSurface::flat(3072, None));
    // Three cells share the ground: fewer than the threshold, so none at all.
    assert!(storeys(&world(room)).is_empty());
}

#[test]
fn an_insert_counts_each_reason_it_skipped_a_cell() {
    let mut room = hall(3, 2048);
    *room.sector_mut(2, 2).unwrap() = tall(1024);
    let mut world = world(room);
    let rect = CellRect::from_corners([0, 0], [2, 2]);
    let out = insert_slab(&mut world, 0, rect, 1024, SLAB_THICKNESS, None);
    assert_eq!(out.changed, 8);
    assert_eq!(out.skipped.no_span, 1);
    assert_eq!(out.refused, None);
}

#[test]
fn a_bad_thickness_refuses_before_any_cell_changes() {
    let mut world = world(hall(2, 2048));
    let before = world.clone();
    let out = insert_slab(
        &mut world,
        0,
        CellRect::from_corners([0, 0], [1, 1]),
        1024,
        -1,
        None,
    );
    assert_eq!(out.refused, Some(SpanRefusal::BadThickness));
    assert_eq!(world.rooms[0].sector(0, 0), before.rooms[0].sector(0, 0));
}

#[test]
fn a_missing_room_is_refused() {
    let mut world = world(hall(2, 2048));
    let out = insert_slab(
        &mut world,
        5,
        CellRect::from_corners([0, 0], [1, 1]),
        1024,
        0,
        None,
    );
    assert_eq!(out.refused, Some(SpanRefusal::NoSuchRoom));
}
