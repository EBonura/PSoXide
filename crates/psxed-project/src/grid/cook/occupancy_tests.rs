//! Sector occupancy in the cooked core: only levels whose room rectangles
//! overlap over an empty sector carry it.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, LEVEL_HAS_OCCUPANCY};

use super::tests::cook;
use super::*;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;

/// A room at world sector `origin` floored and roofed over `cells` (local
/// `(x, z)`); side portals need a ceiling to bound their opening.
fn room(origin: [i32; 2], w: usize, d: usize, cells: &[(usize, usize)]) -> GridRoom {
    let mut room = GridRoom::new("r", w, d);
    room.origin = origin;
    for &(x, z) in cells {
        let sector = room.sector_or_insert(x, z).unwrap();
        sector.set_ground_floor(Some(GridSurface::flat(0, None)));
        sector.set_ground_ceiling(Some(GridSurface::flat(1024, None)));
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

#[test]
fn rooms_meeting_inside_a_rectangle_get_portals_both_ways() {
    let a = room([0, 0], 2, 2, &[(0, 0), (1, 0), (0, 1)]);
    let b = room([1, 1], 1, 1, &[(0, 0)]);
    let cooked = cook(&world(vec![a, b])).unwrap();
    let dests = |room: usize| -> Vec<(u16, [i16; 3])> {
        let rec = &cooked.level.rooms[room];
        let first = usize::from(rec.portal_first);
        cooked.level.portals[first..first + usize::from(rec.portal_count)]
            .iter()
            .map(|p| (p.dest, p.normal))
            .collect()
    };
    // `a` meets the corner along its south and east inner edges; `b` meets
    // `a` along its north and west edges. None is vertical.
    let mut from_a = dests(0);
    from_a.sort();
    assert_eq!(from_a, vec![(1, [0, 0, 4096]), (1, [4096, 0, 0])]);
    let mut from_b = dests(1);
    from_b.sort();
    assert_eq!(from_b, vec![(0, [-4096, 0, 0]), (0, [0, 0, -4096])]);
}

#[test]
fn open_air_rooms_side_by_side_get_a_portal() {
    // Two roofless one-sector rooms: no ceiling bounds the opening, which
    // stands one sector tall over the shared floor.
    let mut a = room([0, 0], 1, 1, &[(0, 0)]);
    let mut b = room([1, 0], 1, 1, &[(0, 0)]);
    for r in [&mut a, &mut b] {
        r.sector_mut(0, 0).unwrap().set_ground_ceiling(None);
    }
    let cooked = cook(&world(vec![a, b])).unwrap();
    let s = cooked.level.sector_units as i16;
    for (from, dest) in [(0usize, 1u16), (1, 0)] {
        let rec = &cooked.level.rooms[from];
        assert_eq!(rec.portal_count, 1, "room {from}");
        let portal = cooked.level.portals[usize::from(rec.portal_first)];
        assert_eq!(portal.dest, dest);
        let (lo, hi) = (portal.v[0][1], portal.v[2][1]);
        assert_eq!(hi - lo, s);
    }
}
