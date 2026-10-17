//! The cook of derived rooms: closure walls collide, risers are left to the
//! step rule, and the overrides decide the rest.

use super::tests::cook;
use crate::grid::derive::edge_key;
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;

fn hall() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 3, 3);
    for z in 0..3 {
        for x in 0..3 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.floor = Some(GridSurface::flat(0, None));
            s.ceiling = Some(GridSurface::flat(1024, None));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

#[test]
fn the_closure_of_a_derived_room_is_solid_and_a_platform_adds_no_wall_records() {
    let mut world = hall();
    let cooked = cook(&world).unwrap();
    let t = &cooked.tables[0];
    // One wall record per outline edge: 4 sides of 3 sectors.
    assert_eq!(t.walls.len(), 12);
    assert!(t.walls.iter().all(|w| w.y == [0, 0, 80, 80]));
    // A raised platform makes risers (drawn) but no wall records.
    world.rooms[0].sector_or_insert(1, 1).unwrap().floor = Some(GridSurface::flat(256, None));
    let with_platform = cook(&world).unwrap();
    assert_eq!(with_platform.tables[0].walls.len(), 12);
    assert_eq!(with_platform.tables[0].quads.len(), t.quads.len() + 4);
}

#[test]
fn overrides_make_ghost_walls_and_solid_risers() {
    let mut world = hall();
    world.rooms[0].sector_or_insert(1, 1).unwrap().floor = Some(GridSurface::flat(256, None));
    // A ghost closure wall: still drawn, no record.
    let ghost = edge_key([0, 0], WallEdge::North);
    world.rooms[0]
        .edge_overrides
        .edit(ghost, |o| o.solid = Some(false));
    // A riser made solid gets a record.
    let riser = edge_key([1, 0], WallEdge::South);
    world.rooms[0]
        .edge_overrides
        .edit(riser, |o| o.solid = Some(true));
    let cooked = cook(&world).unwrap();
    assert_eq!(cooked.tables[0].walls.len(), 12 - 1 + 1);
    let solid_riser = cooked.tables[0].walls.iter().any(|w| w.y == [0, 0, 20, 20]);
    assert!(solid_riser, "{:?}", cooked.tables[0].walls);
}

#[test]
fn a_hidden_face_is_not_cooked_but_a_hidden_closure_still_blocks() {
    let mut world = hall();
    let before = cook(&world).unwrap();
    world.rooms[0]
        .edge_overrides
        .edit(edge_key([0, 0], WallEdge::North), |o| o.hidden = true);
    let after = cook(&world).unwrap();
    assert_eq!(
        after.tables[0].quads.len(),
        before.tables[0].quads.len() - 1
    );
    assert_eq!(after.tables[0].walls.len(), before.tables[0].walls.len());
}

#[test]
fn a_riser_taller_than_a_step_gets_a_one_sided_cliff_wall_and_a_low_one_does_not() {
    use psx_grid::{WALL_BACK_ONLY, WALL_FRONT_ONLY};
    let mut world = hall();
    // 256 grid units are 20 runtime units: a step.
    world.rooms[0].sector_or_insert(1, 1).unwrap().floor = Some(GridSurface::flat(256, None));
    let low = cook(&world).unwrap();
    assert!(low.tables[0].walls.iter().all(|w| w.flags == 0));
    // 768 are 60 runtime units: more than the motor steps up.
    world.rooms[0].sector_or_insert(1, 1).unwrap().floor = Some(GridSurface::flat(768, None));
    let tall = cook(&world).unwrap();
    let cliffs: Vec<_> = tall.tables[0]
        .walls
        .iter()
        .filter(|w| w.flags == WALL_FRONT_ONLY)
        .collect();
    // One per side of the platform, each from the low floor to its top.
    assert_eq!(cliffs.len(), 4, "{:?}", tall.tables[0].walls);
    assert!(cliffs.iter().all(|w| w.y == [0, 0, 60, 60]));
    assert!(tall.tables[0]
        .walls
        .iter()
        .all(|w| w.flags & WALL_BACK_ONLY == 0));
    // A ghost override removes the cliff wall of that side.
    world.rooms[0]
        .edge_overrides
        .edit(edge_key([1, 1], WallEdge::North), |o| o.solid = Some(false));
    let ghost = cook(&world).unwrap();
    assert_eq!(
        ghost.tables[0]
            .walls
            .iter()
            .filter(|w| w.flags == WALL_FRONT_ONLY)
            .count(),
        3
    );
}

#[test]
fn diagonal_closures_and_cliffs_are_cooked_on_the_diagonal_edges() {
    use crate::grid::surface::VOID_A;
    use psx_grid::{edge, WALL_BACK_ONLY};
    // A void triangle: the other half closes along the NW-SE diagonal.
    let mut world = hall();
    {
        let s = world.rooms[0].sector_or_insert(1, 1).unwrap();
        s.floor.as_mut().unwrap().void = VOID_A;
        s.ceiling.as_mut().unwrap().void = VOID_A;
    }
    let cooked = cook(&world).unwrap();
    let diag: Vec<_> = cooked.tables[0]
        .walls
        .iter()
        .filter(|w| w.edge == edge::NWSE)
        .collect();
    assert_eq!(diag.len(), 1);
    // Triangle B is the SW half: the wall is solid from both sides.
    assert_eq!(diag[0].flags, 0);
    assert_eq!(diag[0].y, [0, 0, 80, 80]);

    // A tall split-level riser seen from the back of the diagonal.
    let mut world = hall();
    {
        let f = world.rooms[0]
            .sector_or_insert(1, 1)
            .unwrap()
            .floor
            .as_mut()
            .unwrap();
        // Triangle 0 (NE half, the front of NW-SE) high, triangle 1 low.
        f.heights = [1024; 4];
        f.heights_b = Some([0; 4]);
    }
    let cooked = cook(&world).unwrap();
    let cliff: Vec<_> = cooked.tables[0]
        .walls
        .iter()
        .filter(|w| w.edge == edge::NWSE)
        .collect();
    assert_eq!(cliff.len(), 1, "{:?}", cooked.tables[0].walls);
    assert_eq!(cliff[0].flags, WALL_BACK_ONLY);
    assert_eq!(cliff[0].y, [0, 0, 80, 80]);
}
