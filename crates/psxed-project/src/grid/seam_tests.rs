//! Walls across the border of two rooms: in a chunked world the border is
//! not an edge of the world, so a step that falls on it gets its wall.

use super::edit::chunks::{rechunk_hex, HexLayout};
use super::leaf_derive::{
    derived_range, derived_range_in, leaf_walls, open_edges, seam_walls, Around, OpenKind,
};
use super::room::GridRoom;
use super::surface::GridSurface;
use super::world::GridWorld;

/// A flat field with a raised block in the middle.
fn field_with_block(size: usize, block: std::ops::Range<usize>, height: i32) -> GridWorld {
    let mut room = GridRoom::new_derived("field", size, size);
    for z in 0..size {
        for x in 0..size {
            let h = if block.contains(&x) && block.contains(&z) {
                height
            } else {
                0
            };
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(h, None)));
        }
    }
    GridWorld {
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// Area of every derived wall of the world, grid units squared, derived
/// the way the cook does it (against sibling chunks when the world is
/// chunked).
fn wall_area(world: &GridWorld, alone: bool) -> i64 {
    let mut total = 0i64;
    for (index, room) in world.rooms.iter().enumerate() {
        let around = if alone {
            Around::room(room)
        } else {
            Around::in_world(room, world, index)
        };
        for (key, sector) in room.leaves() {
            for w in leaf_walls(&around, key, sector) {
                let r = &w.wall.ring;
                let twice: i64 = (0..r.len())
                    .map(|i| {
                        let (a, b) = (r[i], r[(i + 1) % r.len()]);
                        i64::from(a[0]) * i64::from(b[1]) - i64::from(b[0]) * i64::from(a[1])
                    })
                    .sum();
                total += twice.abs() / 2;
            }
        }
    }
    total
}

#[test]
fn rechunking_keeps_every_wall_of_the_step() {
    let whole = field_with_block(16, 5..11, 2048);
    let before = wall_area(&whole, true);
    assert!(before > 0);
    let mut chunked = whole.clone();
    let report = rechunk_hex(&mut chunked, HexLayout { size: 2.0 }).unwrap();
    assert!(report.rooms > 1, "the field must be cut into several rooms");
    assert!(chunked.is_chunked());
    assert_eq!(wall_area(&chunked, false), before);
    // Each room taken alone loses the walls that fall on a border.
    assert!(wall_area(&chunked, true) < before);
    assert!(!seam_walls(&chunked).is_empty());
}

#[test]
fn a_world_that_is_not_chunked_keeps_its_borders_open() {
    let mut world = field_with_block(16, 5..11, 2048);
    rechunk_hex(&mut world, HexLayout { size: 2.0 }).unwrap();
    world.stream.ring = false;
    assert!(!world.is_chunked());
    assert_eq!(wall_area(&world, false), wall_area(&world, true));
}

#[test]
fn a_room_that_sits_higher_makes_a_wall_of_the_difference() {
    let mut a = GridRoom::new_derived("a", 1, 1);
    a.sector_or_insert(0, 0)
        .unwrap()
        .set_ground_floor(Some(GridSurface::flat(0, None)));
    let mut b = GridRoom::new_derived("b", 1, 1);
    b.origin = [1, 0];
    b.y_offset = 1024;
    assert!(b.translate([0, 0]));
    b.sector_or_insert(0, 0)
        .unwrap()
        .set_ground_floor(Some(GridSurface::flat(0, None)));
    let mut world = GridWorld {
        rooms: vec![a, b],
        ..GridWorld::default()
    };
    world.stream.ring = true;
    let walls = seam_walls(&world);
    assert_eq!(walls.len(), 1, "{walls:?}");
    assert_eq!(walls[0].room, 0, "the lower room hosts the riser");
    assert_eq!(walls[0].y, [0, 1024]);
    assert_eq!(walls[0].len, 1024);
}

#[test]
fn open_edges_are_the_rim_of_the_world_only() {
    let mut world = field_with_block(8, 2..6, 1024);
    rechunk_hex(&mut world, HexLayout { size: 2.0 }).unwrap();
    let open = open_edges(&world);
    // 4 sides of 8 sectors, quarter-sector leaves not used here: one stretch
    // per sector edge on the outline.
    assert_eq!(open.len(), 32);
    assert!(open.iter().all(|e| e.kind == OpenKind::Absent));
}

#[test]
fn the_culling_range_of_a_cell_counts_the_wall_it_hosts_across_a_border() {
    let mut world = field_with_block(16, 5..11, 2048);
    rechunk_hex(&mut world, HexLayout { size: 2.0 }).unwrap();
    let wall = seam_walls(&world).into_iter().next().expect("a seam wall");
    let cell = wall.wall.host;
    let tall = derived_range_in(&world, wall.room, cell).expect("walls here");
    assert_eq!(tall.1 - tall.0, 2048, "the riser runs the whole step");
    // Taken alone the room would not know the wall.
    let alone = derived_range(&world.rooms[wall.room], cell);
    assert!(alone.is_none_or(|(lo, hi)| hi - lo < 2048), "{alone:?}");
}
