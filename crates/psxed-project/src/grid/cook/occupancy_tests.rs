//! Sector occupancy in the cooked core: only levels whose room rectangles
//! overlap over an empty sector carry it.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, LEVEL_HAS_OCCUPANCY};

use super::tests::cook;
use super::*;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;

/// A room at world sector `origin` floored over `cells` (local `(x, z)`).
fn room(origin: [i32; 2], w: usize, d: usize, cells: &[(usize, usize)]) -> GridRoom {
    let mut room = GridRoom::new("r", w, d);
    room.origin = origin;
    for &(x, z) in cells {
        let sector = room.sector_or_insert(x, z).unwrap();
        sector.set_ground_floor(Some(GridSurface::flat(0, None)));
    }
    room
}

fn world(rooms: Vec<GridRoom>) -> GridWorld {
    GridWorld {
        rooms,
        ..GridWorld::default()
    }
}

#[test]
fn an_l_shaped_room_around_another_room_owns_only_its_own_sectors() {
    // `a` is 2x2 without its south-east sector, which `b` fills.
    let a = room([0, 0], 2, 2, &[(0, 0), (1, 0), (0, 1)]);
    let b = room([1, 1], 1, 1, &[(0, 0)]);
    let cooked = cook(&world(vec![a, b])).unwrap();
    assert_eq!(cooked.level.occupancy, vec![vec![0b0111], vec![0b1]]);
    let words = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&words, cooked.core.len())).unwrap();
    assert_ne!(level.header.flags & LEVEL_HAS_OCCUPANCY, 0);
    let s = level.sector_units() as i32;
    let corner = (s + s / 2, s + s / 2);
    assert!(!level.room_has_sector(0, corner.0, corner.1));
    assert!(level.room_has_sector(1, corner.0, corner.1));
    assert!(level.room_has_sector(0, s / 2, s / 2));
}

#[test]
fn rooms_that_do_not_overlap_carry_no_occupancy() {
    // `a` has an empty sector, but no other room's rectangle covers it.
    let a = room([0, 0], 2, 1, &[(0, 0)]);
    let b = room([3, 0], 1, 1, &[(0, 0)]);
    let cooked = cook(&world(vec![a, b])).unwrap();
    assert!(cooked.level.occupancy.is_empty());
    let words = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&words, cooked.core.len())).unwrap();
    assert_eq!(level.header.flags & LEVEL_HAS_OCCUPANCY, 0);
}
