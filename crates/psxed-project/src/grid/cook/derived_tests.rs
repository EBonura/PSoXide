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
