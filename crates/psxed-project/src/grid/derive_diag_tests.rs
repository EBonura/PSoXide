//! Derived diagonal walls: the riser of a split-level floor, the drop of a
//! split-level ceiling, the closure of a void triangle, and that none of
//! them leaves a hole.

use super::derive::{cell_walls, edge_key, physical_edge_walls, DerivedKind};
use super::faces::{all_faces, GridFace, GridFaceKind};
use super::overrides::WallMode;
use super::room::GridRoom;
use super::sector::WallEdge;
use super::surface::{GridSplit, GridSurface, VOID_A, VOID_B};
use super::units::GridScale;
use super::world::GridWorld;

fn world(room: GridRoom) -> GridWorld {
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// A derived 3 by 3 hall, floor 0, ceiling 2048, with the middle cell
/// changed by `f`.
fn hall(f: impl FnOnce(&mut super::sector::GridSector)) -> GridRoom {
    let mut room = GridRoom::new_derived("hall", 3, 3);
    for z in 0..3 {
        for x in 0..3 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_floor(Some(GridSurface::flat(0, None)));
            s.set_ceiling(Some(GridSurface::flat(2048, None)));
        }
    }
    f(room.sector_or_insert(1, 1).unwrap());
    room
}

fn diagonal(faces: &[GridFace], kind: DerivedKind) -> Vec<&GridFace> {
    faces
        .iter()
        .filter(|f| {
            matches!(f.kind, GridFaceKind::Derived { edge, kind: k }
                if k == kind && edge.is_diagonal())
        })
        .collect()
}

#[test]
fn a_split_level_floor_grows_a_diagonal_riser_seen_from_the_lower_triangle() {
    // NW-SE split: triangle 0 (NW, NE, SE) is the NE half, triangle 1 the
    // SW half. Triangle 1 sits two clicks up.
    let room = hall(|s| {
        let floor = s.floor_mut().unwrap();
        floor.heights_b = Some([512; 4]);
    });
    let faces = all_faces(&world(room));
    let risers = diagonal(&faces, DerivedKind::Riser);
    assert_eq!(risers.len(), 1, "{risers:?}");
    let riser = risers[0];
    assert_eq!(
        riser.kind,
        GridFaceKind::Derived {
            edge: WallEdge::Nwse,
            kind: DerivedKind::Riser
        }
    );
    // On the diagonal from (1024,1024) to (2048,2048), between 0 and 512.
    for p in &riser.points {
        assert_eq!(p[0], p[2], "{p:?}");
        assert!((0..=512).contains(&p[1]));
    }
    // Seen from the lower (NE) triangle: the normal points north-east.
    assert!(
        riser.normal[0] > 0.5 && riser.normal[2] < -0.5,
        "{:?}",
        riser.normal
    );
    assert_eq!(riser.points.len(), 4);
}

#[test]
fn the_riser_sits_exactly_on_the_edges_of_the_two_floor_triangles() {
    let room = hall(|s| {
        let floor = s.floor_mut().unwrap();
        floor.heights = [0, 0, 0, 0];
        floor.heights_b = Some([256, 256, 256, 256]);
    });
    let faces = all_faces(&world(room));
    let corners: Vec<[i32; 3]> = faces
        .iter()
        .filter(|f| f.cell == [1, 1] && matches!(f.kind, GridFaceKind::Floor { .. }))
        .flat_map(|f| f.points.iter().copied())
        .collect();
    let riser = diagonal(&faces, DerivedKind::Riser)[0];
    for p in &riser.points {
        assert!(
            corners.contains(p),
            "riser corner {p:?} is not a floor corner"
        );
    }
}

#[test]
fn a_pure_fold_grows_no_diagonal_face() {
    // Both triangles meet at the same heights along the diagonal.
    let room = hall(|s| {
        let floor = s.floor_mut().unwrap();
        floor.heights = [0, 0, 0, 256];
        floor.heights_b = Some([0, 256, 0, 256]);
    });
    let faces = all_faces(&world(room));
    assert!(diagonal(&faces, DerivedKind::Riser).is_empty());
}

#[test]
fn the_other_split_puts_the_riser_on_the_other_diagonal() {
    let room = hall(|s| {
        let floor = s.floor_mut().unwrap();
        floor.split = GridSplit::NeSw;
        floor.heights_b = Some([256; 4]);
    });
    let w = world(room);
    let walls = cell_walls(&w.rooms[0], [1, 1]);
    assert_eq!(walls.len(), 1);
    assert_eq!(walls[0].edge, WallEdge::Nesw);
    assert_eq!(walls[0].key, edge_key([1, 1], WallEdge::Nesw));
    // Triangle 1 (NE, SE, SW) is the SE half and is higher: hosted by the
    // front (NW) triangle, so not the back.
    assert!(!walls[0].back);
}

#[test]
fn a_split_level_ceiling_grows_a_diagonal_drop() {
    let room = hall(|s| {
        let ceiling = s.ceiling_mut().unwrap();
        ceiling.heights_b = Some([1536; 4]);
    });
    let faces = all_faces(&world(room));
    let drops = diagonal(&faces, DerivedKind::Drop);
    assert_eq!(drops.len(), 1);
    // The lower ceiling is triangle 1 (SW half) and owns it; seen from the
    // higher (NE) side.
    assert!(drops[0].normal[0] > 0.5 && drops[0].normal[2] < -0.5);
}

#[test]
fn a_void_triangle_draws_no_floor_and_the_other_half_closes_along_the_diagonal() {
    let room = hall(|s| {
        let (floor, ceiling) = s.floor_and_ceiling_mut();
        for surface in [floor.unwrap(), ceiling.unwrap()] {
            surface.void = VOID_A;
        }
    });
    let faces = all_faces(&world(room));
    // The cell has one floor triangle and one ceiling triangle.
    let cell_surfaces = faces
        .iter()
        .filter(|f| {
            f.cell == [1, 1]
                && matches!(
                    f.kind,
                    GridFaceKind::Floor { .. } | GridFaceKind::Ceiling { .. }
                )
        })
        .count();
    assert_eq!(cell_surfaces, 2);
    let closure = diagonal(&faces, DerivedKind::Closure);
    assert_eq!(closure.len(), 1);
    // Triangle B (the SW half) is what is left, so the wall faces south-west.
    let n = closure[0].normal;
    assert!(n[0] < -0.5 && n[2] > 0.5, "{n:?}");
    // Floor to ceiling.
    let ys: Vec<i32> = closure[0].points.iter().map(|p| p[1]).collect();
    assert_eq!((ys.iter().min(), ys.iter().max()), (Some(&0), Some(&2048)));
}

#[test]
fn neighbours_of_a_void_triangle_close_themselves_across_the_cardinal_edge() {
    // Triangle A (NW, NE, SE) holds the north and east edges.
    let room = hall(|s| {
        s.floor_mut().unwrap().void = VOID_A;
        s.ceiling_mut().unwrap().void = VOID_A;
    });
    let w = world(room);
    let north_of_it = physical_edge_walls(&w.rooms[0], [1, 0], WallEdge::South);
    assert_eq!(
        north_of_it.len(),
        1,
        "the sector north of the void must close"
    );
    assert_eq!(north_of_it[0].kind, DerivedKind::Closure);
    assert_eq!(north_of_it[0].host, [1, 0]);
    // The void sector itself makes no wall on its void edges.
    assert!(physical_edge_walls(&w.rooms[0], [1, 1], WallEdge::North)
        .iter()
        .all(|d| d.host != [1, 1]));
}

#[test]
fn a_fully_void_cell_is_empty_space() {
    let room = hall(|s| {
        s.floor_mut().unwrap().void = VOID_A | VOID_B;
        s.ceiling_mut().unwrap().void = VOID_A | VOID_B;
    });
    let w = world(room);
    let faces = all_faces(&w);
    assert!(faces.iter().all(|f| f.cell != [1, 1]));
    // Four neighbours each close their side of the hole.
    let closure = faces
        .iter()
        .filter(|f| {
            matches!(f.kind, GridFaceKind::Derived { kind: DerivedKind::Closure, edge }
                if [([1, 0], WallEdge::South), ([0, 1], WallEdge::East),
                    ([2, 1], WallEdge::West), ([1, 2], WallEdge::North)]
                    .contains(&(f.cell, edge)))
        })
        .count();
    assert_eq!(closure, 4);
    assert!(diagonal(&faces, DerivedKind::Closure).is_empty());
}

#[test]
fn an_authored_room_draws_no_derived_diagonals() {
    let mut room = hall(|s| {
        s.floor_mut().unwrap().heights_b = Some([512; 4]);
    });
    room.wall_mode = WallMode::Authored;
    let faces = all_faces(&world(room));
    assert!(diagonal(&faces, DerivedKind::Riser).is_empty());
}

#[test]
fn an_open_top_void_closes_to_the_rooms_open_wall_height() {
    let mut room = hall(|s| {
        s.set_ceiling(None);
        s.floor_mut().unwrap().void = VOID_B;
    });
    for z in 0..3 {
        for x in 0..3 {
            if (x, z) != (1, 1) {
                room.sector_or_insert(x, z).unwrap().set_ceiling(None);
            }
        }
    }
    room.defaults.open_wall = 768;
    let faces = all_faces(&world(room));
    let closure = diagonal(&faces, DerivedKind::Closure);
    assert_eq!(closure.len(), 1);
    let top = closure[0].points.iter().map(|p| p[1]).max().unwrap();
    assert_eq!(top, 768);
}
