//! Cooking a room with leaves of several sizes (T9): the render tables come
//! from the leaves, collision and portals from the sector view.

use super::mesh::open_edges;
use super::tests::{cook, rid};
use super::*;
use crate::grid::leaf::LeafKey;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::tree::{merge_all, normalize, subdivide_to};
use crate::grid::units::GridScale;

/// An 8 by 8 derived hall, floor 0 and ceiling 2560, merged into big leaves.
fn big_hall() -> GridRoom {
    let mut room = GridRoom::new_derived("hall", 8, 8);
    for z in 0..8 {
        for x in 0..8 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat(0, Some(rid(1)))));
            s.set_ground_ceiling(Some(GridSurface::flat(2560, Some(rid(2)))));
        }
    }
    merge_all(&mut room, None, 0);
    room
}

fn world_of(room: GridRoom) -> GridWorld {
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn holes(world: &GridWorld) -> Vec<String> {
    let cooked = cook(world).expect("the world cooks");
    open_edges(&cooked.tables[0])
        .iter()
        .filter(|e| e.hole)
        .map(|e| format!("{:?} -> {:?}", e.a, e.b))
        .collect()
}

#[test]
fn a_hall_of_big_leaves_cooks_to_the_quads_of_the_sector_hall() {
    let big = world_of(big_hall());
    assert_eq!(big.rooms[0].leaf_count(), 4);
    let mut flat = big_hall();
    // The same hall as whole sectors.
    let keys: Vec<LeafKey> = flat.leaves.keys().collect();
    crate::grid::tree::subdivide_all_to(&mut flat, &keys, 2);
    let flat = world_of(flat);
    let (a, b) = (cook(&big).unwrap(), cook(&flat).unwrap());
    // Same collision, same render tables, same size: big leaves cost
    // nothing until T14 merges quads.
    assert_eq!(a.tables[0].sectors, b.tables[0].sectors);
    assert_eq!(a.tables[0].quads.len(), b.tables[0].quads.len());
    assert_eq!(a.tables[0].vertices, b.tables[0].vertices);
    assert!(a.report.warnings.is_empty(), "{:?}", a.report.warnings);
    assert!(holes(&big).is_empty());
}

#[test]
fn a_subdivided_corner_with_fine_steps_cooks_without_a_hole() {
    let mut room = big_hall();
    // Fine steps in the north-west corner: quarter-sector treads.
    subdivide_to(&mut room, [0, 0], 4).unwrap();
    for i in 0..4 {
        let key = LeafKey::new(4, i, 0).unwrap();
        for z in 0..4 {
            let key = LeafKey::new(4, key.x, z).unwrap();
            if let Some(f) = room.leaf_mut(key).and_then(|s| s.ground_floor_mut()) {
                f.heights = [64 * (i + 1); 4];
            }
        }
    }
    normalize(&mut room);
    let world = world_of(room);
    let cooked = cook(&world).expect("a room with fine leaves cooks");
    assert!(holes(&world).is_empty(), "{:?}", holes(&world));
    // The steps cost quads: more than the plain hall.
    let plain = cook(&world_of(big_hall())).unwrap();
    assert!(cooked.tables[0].quads.len() > plain.tables[0].quads.len());
    // The sector that holds them is collided at its corners, and says so.
    assert!(
        cooked
            .report
            .warnings
            .iter()
            .any(|w| w.contains("sector 0,0")),
        "{:?}",
        cooked.report.warnings
    );
}

#[test]
fn a_planar_detail_inside_a_sector_is_collided_exactly() {
    // One sector subdivided but not changed: it merges back into a sector
    // for collision, exactly, and no warning is made.
    let mut room = big_hall();
    subdivide_to(&mut room, [0, 0], 3).unwrap();
    let world = world_of(room);
    let cooked = cook(&world).unwrap();
    assert!(
        cooked.report.warnings.is_empty(),
        "{:?}",
        cooked.report.warnings
    );
}
