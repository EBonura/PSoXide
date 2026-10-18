//! Faces, face references and wall gaps in columns of several spans: a floor
//! or ceiling is addressed by its span, an edit through a reference touches
//! that span alone, deleting a slab's surface cuts the slab out, and a wall
//! fills the gap of the span it is placed in.

use super::edit::focus::{Focus, FocusRefusal};
use super::edit::refs::{CornerRef, FaceRef, FaceTarget, SurfaceKind};
use super::edit::spans::insert_slab_column;
use super::edit::{delete_faces, faces_of_cell, next_wall_heights, range_surfaces};
use super::faces::{all_faces, GridFaceKind};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::storeys::SLAB_THICKNESS;
use super::surface::{GridSurface, GridWall};
use super::units::GridScale;
use super::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

fn tall(top: i32) -> GridSector {
    GridSector::with_surfaces(
        Some(GridSurface::flat(0, Some(rid(1)))),
        Some(GridSurface::flat(top, Some(rid(2)))),
    )
}

/// A 3 by 1 derived room of columns 12 clicks tall: sector 0 has one span,
/// sector 1 a slab at 6 clicks (two spans), sector 2 slabs at 4 and 8 (three).
fn world() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 3, 1);
    for x in 0..3 {
        *room.sector_or_insert(x, 0).unwrap() = tall(3072);
    }
    insert_slab_column(room.sector_mut(1, 0).unwrap(), 1536, SLAB_THICKNESS, None).unwrap();
    insert_slab_column(room.sector_mut(2, 0).unwrap(), 2048, SLAB_THICKNESS, None).unwrap();
    insert_slab_column(room.sector_mut(2, 0).unwrap(), 1024, SLAB_THICKNESS, None).unwrap();
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

fn ceilings(world: &GridWorld, x: usize) -> Vec<i32> {
    world.rooms[0]
        .sector(x, 0)
        .unwrap()
        .spans()
        .iter()
        .filter_map(|s| s.ceiling.as_ref().map(|c| c.heights[0]))
        .collect()
}

#[test]
fn a_floor_or_ceiling_face_carries_the_span_it_comes_from() {
    let w = world();
    let faces = all_faces(&w);
    let of = |cell: [i32; 2]| -> Vec<(bool, u8)> {
        faces
            .iter()
            .filter(|f| f.cell == cell)
            .filter_map(|f| match f.kind {
                GridFaceKind::Floor { .. } => Some((true, f.span)),
                GridFaceKind::Ceiling { .. } => Some((false, f.span)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(of([0, 0]), vec![(true, 0), (false, 0)]);
    assert_eq!(
        of([1, 0]),
        vec![(true, 0), (false, 0), (true, 1), (false, 1)]
    );
    assert_eq!(of([2, 0]).len(), 6);
    assert!(faces
        .iter()
        .filter(|f| !matches!(
            f.kind,
            GridFaceKind::Floor { .. } | GridFaceKind::Ceiling { .. }
        ))
        .all(|f| f.span == 0));
}

#[test]
fn a_face_reference_resolves_its_own_span() {
    let w = world();
    let upper = FaceRef::floor_in(0, [1, 0], 1);
    assert!(upper.exists(&w));
    assert_eq!(upper.surface(&w).unwrap().heights, [1536; 4]);
    assert_eq!(upper.heights(&w, 0), Some([1536; 4]));
    assert_eq!(FaceRef::floor(0, [1, 0]).heights(&w, 0), Some([0; 4]));
    assert_eq!(
        FaceRef::ceiling_in(0, [1, 0], 0).heights(&w, 0),
        Some([1280; 4])
    );
    // A span the column does not have is no face.
    assert!(!FaceRef::floor_in(0, [1, 0], 2).exists(&w));
    assert!(!FaceRef::floor_in(0, [0, 0], 1).exists(&w));
    assert_eq!(upper.target.span(), Some(1));
    assert_eq!(
        FaceTarget::floor(3),
        FaceTarget::Surface {
            kind: SurfaceKind::Floor,
            span: 3
        }
    );
    // The cell lists every surface of every span: floor and ceiling each.
    assert_eq!(faces_of_cell(&w, 0, [0, 0]).len(), 2);
    assert_eq!(faces_of_cell(&w, 0, [1, 0]).len(), 4);
    assert_eq!(faces_of_cell(&w, 0, [2, 0]).len(), 6);
}

#[test]
fn a_height_edit_through_a_reference_moves_that_span_alone() {
    let mut w = world();
    let corner = CornerRef {
        face: FaceRef::floor_in(0, [1, 0], 1),
        layer: 0,
        corner: 2,
    };
    assert_eq!(corner.height(&w), Some(1536));
    assert!(corner.set_world_height(&mut w, 1792));
    let upper = FaceRef::floor_in(0, [1, 0], 1).surface(&w).unwrap();
    assert_eq!(upper.heights, [1536, 1536, 1792, 1536]);
    assert_eq!(floors(&w, 1)[0], 0, "the ground floor did not move");
}

#[test]
fn deleting_the_surface_of_a_slab_cuts_the_slab_out_and_the_ends_become_open() {
    // The slab's top: the spans merge.
    let mut w = world();
    assert_eq!(delete_faces(&mut w, &[FaceRef::floor_in(0, [1, 0], 1)]), 1);
    assert_eq!((floors(&w, 1), ceilings(&w, 1)), (vec![0], vec![3072]));
    // Its underside: the same.
    let mut w = world();
    assert_eq!(
        delete_faces(&mut w, &[FaceRef::ceiling_in(0, [1, 0], 0)]),
        1
    );
    assert_eq!((floors(&w, 1), ceilings(&w, 1)), (vec![0], vec![3072]));
    // Both faces of one slab are one cut.
    let mut w = world();
    let both = [
        FaceRef::floor_in(0, [1, 0], 1),
        FaceRef::ceiling_in(0, [1, 0], 0),
    ];
    assert_eq!(delete_faces(&mut w, &both), 1);
    assert_eq!(floors(&w, 1), vec![0]);
    // Two slabs of one column, named in any order, both go.
    let mut w = world();
    let slabs = [
        FaceRef::floor_in(0, [2, 0], 1),
        FaceRef::ceiling_in(0, [2, 0], 1),
        FaceRef::floor_in(0, [2, 0], 2),
    ];
    assert_eq!(delete_faces(&mut w, &slabs), 2);
    assert_eq!((floors(&w, 2), ceilings(&w, 2)), (vec![0], vec![3072]));
    // The outermost surfaces open the column: no floor is a pit, no
    // ceiling open sky.
    let mut w = world();
    assert_eq!(delete_faces(&mut w, &[FaceRef::floor(0, [1, 0])]), 1);
    assert_eq!(floors(&w, 1), vec![1536]);
    let mut w = world();
    assert_eq!(
        delete_faces(&mut w, &[FaceRef::ceiling_in(0, [1, 0], 1)]),
        1
    );
    assert_eq!(ceilings(&w, 1), vec![1280]);
}

#[test]
fn shift_click_ranges_stay_on_one_span() {
    let w = world();
    let upper = range_surfaces(&w, 0, SurfaceKind::Floor, 1, &[[1, 0]], [2, 0]);
    assert_eq!(
        upper,
        vec![
            FaceRef::floor_in(0, [1, 0], 1),
            FaceRef::floor_in(0, [2, 0], 1)
        ]
    );
    // Sector 0 has no second span: it is not in the range.
    let from_zero = range_surfaces(&w, 0, SurfaceKind::Floor, 1, &[[0, 0]], [2, 0]);
    assert_eq!(from_zero.len(), 2);
}

#[test]
fn an_aimed_edit_fills_a_sliver_it_leaves_and_says_how_many() {
    let mut w = world();
    let focus = Focus::capture(&w, 0, [[1, 0]], Some(100)).unwrap();
    assert!(focus.is_aimed() && focus.index_at([1, 0]) == 0);
    // The ground floor climbs to within a click of the slab's underside.
    let aimed = focus
        .run(&mut w, |world| {
            world.rooms[0]
                .sector_mut(1, 0)
                .unwrap()
                .ground_floor_mut()
                .unwrap()
                .heights = [1024; 4];
        })
        .unwrap();
    assert_eq!(aimed.slivers, 1);
    assert_eq!(aimed.floor, Some(1024));
    assert_eq!(floors(&w, 1), vec![1536], "the sliver is solid now");
}

#[test]
fn an_aim_names_the_floor_and_a_column_of_several_without_one_is_refused() {
    let w = world();
    assert_eq!(
        Focus::capture(&w, 0, [[0, 0], [1, 0]], None),
        Err(FocusRefusal::SeveralSpans {
            cell: [1, 0],
            count: 2
        })
    );
    // Single spans need no aim at all.
    assert_eq!(
        Focus::capture(&w, 0, [[0, 0]], None).map(|f| f.is_aimed()),
        Ok(false)
    );
    let focus = Focus::capture(&w, 0, [[2, 0]], Some(2048)).unwrap();
    assert_eq!(focus.index_at([2, 0]), 2);
    // The view an edit's snapshot is taken from has that floor first.
    let view = focus.view(&w);
    assert_eq!(
        view.rooms[0]
            .sector(2, 0)
            .unwrap()
            .ground_floor()
            .unwrap()
            .heights,
        [2048; 4]
    );
    assert_eq!(
        floors(&w, 2),
        vec![0, 1024, 2048],
        "the real world is as it was"
    );
    assert_eq!(
        Focus::capture(&w, 0, [[2, 0]], Some(900)),
        Err(FocusRefusal::NoSpan { cell: [2, 0] }),
        "inside the lowest slab nothing is open"
    );
}

#[test]
fn a_wall_fills_the_gap_of_the_span_at_the_height_and_ignores_the_others_walls() {
    let w = world();
    let room = &w.rooms[0];
    let bounds = (0, 3072);
    let column = room.sector(1, 0).unwrap();
    // Without a height the ground span's gap; with one, that span's.
    assert_eq!(
        next_wall_heights(column, WallEdge::North, bounds, None),
        Some([0, 0, 1280, 1280])
    );
    assert_eq!(
        next_wall_heights(column, WallEdge::North, bounds, Some(1700)),
        Some([1536, 1536, 3072, 3072])
    );
    // A wall already standing in the lower storey leaves the upper one whole.
    let mut with_wall = column.clone();
    with_wall
        .walls_mut(WallEdge::North)
        .push(GridWall::span(0, 1280, None));
    assert_eq!(
        next_wall_heights(&with_wall, WallEdge::North, bounds, Some(1700)),
        Some([1536, 1536, 3072, 3072])
    );
    assert_eq!(
        next_wall_heights(&with_wall, WallEdge::North, bounds, Some(100)),
        None,
        "the lower storey's edge is covered"
    );
    // A column of one span is exactly as it was.
    let single = room.sector(0, 0).unwrap();
    assert_eq!(
        next_wall_heights(single, WallEdge::North, bounds, Some(1700)),
        Some([0, 0, 3072, 3072])
    );
}

#[test]
fn a_slab_over_a_half_cell_keeps_the_void_and_the_cell_closes_itself_on_each_storey() {
    use super::derive::{cell_walls, DerivedKind};
    use super::surface::VOID_A;
    let mut w = world();
    {
        let sector = w.rooms[0].sector_mut(0, 0).unwrap();
        sector.ground_floor_mut().unwrap().void = VOID_A;
        sector.ground_ceiling_mut().unwrap().void = VOID_A;
        insert_slab_column(sector, 1536, SLAB_THICKNESS, None).unwrap();
        // Both new surfaces and the underside carry the void triangle.
        assert!(sector
            .spans()
            .iter()
            .flat_map(|s| s.floor.iter().chain(s.ceiling.iter()))
            .all(|s| s.void == VOID_A));
    }
    // The diagonal closure of the empty half stands on both storeys.
    let closures: Vec<[i32; 2]> = cell_walls(&w.rooms[0], [0, 0])
        .into_iter()
        .filter(|wall| wall.edge.is_diagonal() && wall.kind == DerivedKind::Closure)
        .map(|wall| {
            let lows = wall.ring.iter().map(|p| p[1]);
            [lows.clone().min().unwrap(), lows.max().unwrap()]
        })
        .collect();
    assert_eq!(closures, vec![[0, 1280], [1536, 3072]]);
    // A column of one span is exactly the old answer.
    let mut single = world();
    let s = single.rooms[0].sector_mut(0, 0).unwrap();
    s.ground_floor_mut().unwrap().void = VOID_A;
    s.ground_ceiling_mut().unwrap().void = VOID_A;
    let one = cell_walls(&single.rooms[0], [0, 0])
        .into_iter()
        .filter(|wall| wall.edge.is_diagonal())
        .count();
    assert_eq!(one, 1);
}
