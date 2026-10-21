//! The wanted set and the pool plan over a corridor of rooms.

use crate::format::*;
use crate::stream::{pages_of, plan_pool, StreamWindow, Want, WantedRoom};
use crate::view::LevelView;
use crate::write::{aligned_copy, words_as_bytes, LevelCore};
use alloc::vec::Vec;

const S: i32 = 80;

/// `count` rooms of `len` sectors in a row along X, one sector deep, joined by
/// an open portal at every shared wall (both directions).
fn corridor(count: usize, len: u8, draw_far: u8, active: u8, lead: u8) -> Vec<u32> {
    corridor_flagged(count, len, draw_far, active, lead, 0)
}

/// [`corridor`] with level `flags`.
fn corridor_flagged(
    count: usize,
    len: u8,
    draw_far: u8,
    active: u8,
    lead: u8,
    flags: u32,
) -> Vec<u32> {
    let mut rooms = Vec::new();
    let mut portals = Vec::new();
    for i in 0..count {
        let first = portals.len() as u16;
        let wall = |at: i16, dest: usize| Portal {
            dest: dest as u16,
            flags: PORTAL_ALWAYS_OPEN,
            v: [
                [at, 0, 0],
                [at, 0, S as i16],
                [at, 100, S as i16],
                [at, 100, 0],
            ],
            normal: [0; 3],
            reserved: 0,
        };
        if i > 0 {
            portals.push(wall(0, i - 1));
        }
        if i + 1 < count {
            portals.push(wall(i16::from(len) * S as i16, i + 1));
        }
        rooms.push(RoomRecord {
            origin: [i as i32 * i32::from(len) * S, 0, 0],
            width: len,
            depth: 1,
            portal_first: first,
            portal_count: portals.len() as u16 - first,
            min_y: 0,
            max_y: 100,
            ..RoomRecord::default()
        });
    }
    let core = LevelCore {
        occupancy: Vec::new(),
        flags,
        sector_units: S as u16,
        draw_far,
        active_radius: active,
        lead_radius: lead,
        rooms,
        portals,
        ..LevelCore::default()
    };
    aligned_copy(&core.to_bytes())
}

fn level(words: &[u32]) -> LevelView<'_> {
    LevelView::parse(words_as_bytes(words, words.len() * 4)).unwrap()
}

#[test]
fn need_is_the_draw_radius_and_lead_is_the_ring_beyond_it() {
    // Eight rooms of two sectors. Draw 2 sectors, activation 2, lead 2 more.
    let words = corridor(8, 2, 2, 2, 2);
    let level = level(&words);
    let mut window = StreamWindow::new(&level);
    let mut wanted = Vec::new();
    // Standing in the middle of room 3 (x = 3 * 160 + 80).
    let at = [3 * 160 + 80, 50, S / 2];
    window.wanted(&level, Some((3, at)), Some((3, at)), &mut wanted);
    let need: Vec<u16> = wanted
        .iter()
        .filter(|w| w.want == Want::Need)
        .map(|w| w.room)
        .collect();
    let lead: Vec<u16> = wanted
        .iter()
        .filter(|w| w.want == Want::Lead)
        .map(|w| w.room)
        .collect();
    // Room 3 is at 0. The portals to rooms 2 and 4 are one sector away (80)
    // and the far portals of those rooms 240 away: inside the draw distance
    // (160) only the neighbours; the lead ring (320) adds rooms 1 and 5.
    assert_eq!(need.len(), 3, "need {need:?}");
    assert!(need.contains(&3) && need.contains(&2) && need.contains(&4));
    assert_eq!(lead.len(), 2, "lead {lead:?}");
    assert!(lead.contains(&1) && lead.contains(&5));
    // Need rooms come first, nearest first.
    assert_eq!(wanted[0].room, 3);
    assert_eq!(wanted[0].distance, 0);
    assert!(wanted.iter().position(|w| w.want == Want::Lead) >= Some(3));
}

#[test]
fn an_unbounded_level_wants_everything_and_the_pool_is_the_whole_level() {
    let words = corridor(5, 2, 0, 0, 0);
    let level = level(&words);
    let mut window = StreamWindow::new(&level);
    assert!(window.unbounded());
    let mut wanted = Vec::new();
    window.wanted(
        &level,
        Some((0, [10, 50, 10])),
        Some((0, [10, 50, 10])),
        &mut wanted,
    );
    assert_eq!(wanted.len(), 5);
    let plan = plan_pool(&level, &[3, 3, 3, 3, 3]);
    assert_eq!(
        (plan.total_pages, plan.pool_pages, plan.peak_rooms),
        (15, 15, 5)
    );
}

#[test]
fn the_pool_follows_the_busiest_position_not_the_level() {
    // Twenty rooms of two sectors, 6 pages each, a window of about five rooms.
    let words = corridor(20, 2, 3, 2, 2);
    let level = level(&words);
    let pages = [6usize; 20];
    let plan = plan_pool(&level, &pages);
    assert_eq!(plan.total_pages, 120);
    assert_eq!(plan.max_room_pages, 6);
    // At a doorway the window holds the two rooms either side, the next two
    // out in the lead ring and one more: the plan must cover the peak it saw.
    assert!(plan.peak_rooms >= 4 && plan.peak_rooms <= 8, "{plan:?}");
    assert_eq!(plan.peak_pages, plan.peak_rooms * 6);
    assert!(plan.pool_pages >= plan.peak_pages + plan.max_room_pages);
    assert!(plan.pool_pages < plan.total_pages, "{plan:?}");
}

#[test]
fn a_small_level_never_asks_for_more_than_it_has() {
    let words = corridor(3, 2, 3, 2, 2);
    let level = level(&words);
    let plan = plan_pool(&level, &[5, 5, 5]);
    assert_eq!(plan.pool_pages, 15);
}

#[test]
fn an_observer_outside_every_room_wants_nothing_new() {
    let words = corridor(4, 2, 3, 2, 2);
    let level = level(&words);
    let mut window = StreamWindow::new(&level);
    let mut wanted = Vec::new();
    window.wanted(&level, None, None, &mut wanted);
    assert!(wanted.is_empty());
}

#[test]
fn pages_round_up_to_whole_sectors() {
    assert_eq!(pages_of(0), 0);
    assert_eq!(pages_of(1), 1);
    assert_eq!(pages_of(2048), 1);
    assert_eq!(pages_of(2049), 2);
    let _ = WantedRoom {
        room: 0,
        want: Want::Need,
        distance: 0,
    };
}

#[test]
fn a_point_is_located_from_the_core_alone() {
    use crate::stream::room_at;
    let words = corridor(4, 2, 3, 2, 2);
    let level = level(&words);
    // Room 2 spans x 320..480.
    assert_eq!(room_at(&level, 0, [400, 50, 40]), Some(2));
    assert_eq!(room_at(&level, 2, [400, 50, 40]), Some(2));
    // The corridor is one sector deep: past it, or past the ends, is outside.
    assert_eq!(room_at(&level, 0, [400, 50, S + 40]), None);
    assert_eq!(room_at(&level, 0, [-1, 50, 40]), None);
    assert_eq!(room_at(&level, 0, [4 * 160 + 1, 50, 40]), None);
    // Above or below the room's vertical range it still belongs to it, the
    // nearest room standing in for a body that jumped or fell.
    assert_eq!(room_at(&level, 0, [400, 5_000, 40]), Some(2));
    // The window reports distances for what it searched.
    let mut window = StreamWindow::new(&level);
    let mut wanted = Vec::new();
    window.wanted(&level, Some((2, [400, 50, 40])), None, &mut wanted);
    assert_eq!(window.distance_of(2), 0);
    assert_eq!(window.distance_of(3), 80);
    assert_eq!(window.distance_of(0), 240);
    // Past the lead radius (400) a room has no distance.
    let words = corridor(8, 2, 3, 2, 2);
    let level = self::level(&words);
    let mut window = StreamWindow::new(&level);
    window.wanted(&level, Some((0, [40, 50, 40])), None, &mut wanted);
    assert_eq!(window.distance_of(7), u32::MAX);
}

#[test]
fn a_ring_level_wants_and_plans_the_observers_room_and_its_neighbours() {
    // Long rooms and a draw distance that would reach five rooms each way:
    // the ring ignores it.
    let words = corridor_flagged(8, 2, 10, 2, 4, LEVEL_RING_STREAM);
    let level = level(&words);
    let mut window = StreamWindow::new(&level);
    assert!(!window.unbounded());
    let mut wanted = Vec::new();
    let at = [3 * 160 + 80, 50, S / 2];
    window.wanted(&level, Some((3, at)), Some((3, at)), &mut wanted);
    let mut rooms: Vec<u16> = wanted.iter().map(|w| w.room).collect();
    assert!(wanted.iter().all(|w| w.want == Want::Need));
    assert_eq!(wanted[0].room, 3);
    rooms.sort();
    assert_eq!(rooms, [2, 3, 4]);
    // The camera in another room adds its own ring.
    let eye = [6 * 160 + 80, 50, S / 2];
    window.wanted(&level, Some((3, at)), Some((6, eye)), &mut wanted);
    let mut rooms: Vec<u16> = wanted.iter().map(|w| w.room).collect();
    rooms.sort();
    assert_eq!(rooms, [2, 3, 4, 5, 6, 7]);
    // The pool holds the busiest ring: three rooms of one page.
    let plan = plan_pool(&level, &[1; 8]);
    assert_eq!((plan.peak_pages, plan.peak_rooms), (3, 3));
}
