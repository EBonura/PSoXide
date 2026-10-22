//! The budgeted flood: its distance, its stops, its determinism, its slicing,
//! its block in the core, and the wanted set it gives the streamer.

use alloc::vec::Vec;

use crate::flood::{octagonal_distance, Flood, FloodEnd, FloodQuery, NO_PAGE_BUDGET};
use crate::format::*;
use crate::stream::{plan_pool, StreamWindow, Want};
use crate::view::LevelView;
use crate::write::{aligned_copy, words_as_bytes, FloodBlock, LevelCore};

const S: i32 = 80;

/// `count` rooms of `len` sectors in a row along X, `pages` pages each,
/// joined by an open portal at every shared wall.
fn corridor(count: usize, len: u8, pages: u16, flood: Option<FloodBlock>) -> Vec<u32> {
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
            pack_sectors: pages,
            portal_first: first,
            portal_count: portals.len() as u16 - first,
            min_y: 0,
            max_y: 100,
            ..RoomRecord::default()
        });
    }
    let core = LevelCore {
        sector_units: S as u16,
        draw_far: 0,
        active_radius: 1,
        rooms,
        portals,
        flood,
        ..LevelCore::default()
    };
    aligned_copy(&core.to_bytes())
}

fn block(cap: u16, near: u8, res: u8, portals: usize) -> FloodBlock {
    FloodBlock {
        header: FloodHeader {
            pool_cap_pages: cap,
            near_radius: near,
            res_radius: res,
            bias: 0,
            reserved: [0; 3],
        },
        portal_weight: alloc::vec![FLOOD_WEIGHT_ONE; portals],
    }
}

fn view(words: &[u32]) -> LevelView<'_> {
    LevelView::parse(words_as_bytes(words, words.len() * 4)).unwrap()
}

/// A corridor of `count` two-sector rooms of 3 pages with a flood block.
fn flooded(count: usize, cap: u16, near: u8, res: u8) -> Vec<u32> {
    // Two portals per inner room, one per end room.
    let portals = 2 * (count - 1);
    corridor(count, 2, 3, Some(block(cap, near, res, portals)))
}

fn query(room: usize, x: i32, radius: i32, budget: u32) -> FloodQuery {
    FloodQuery {
        room,
        at: [x, 50, S / 2],
        radius,
        page_budget: budget,
    }
}

#[test]
fn octagonal_distance_stays_within_ten_percent_of_the_straight_distance() {
    let mut worst = 0.0f64;
    for x in (-1200..=1200).step_by(100) {
        for y in (-1200..=1200).step_by(300) {
            for z in (-1200..=1200).step_by(100) {
                if x == 0 && y == 0 && z == 0 {
                    continue;
                }
                let straight = f64::from(x * x + y * y + z * z).sqrt();
                let got = f64::from(octagonal_distance([x, y, z], [0, 0, 0]));
                worst = worst.max((got - straight).abs() / straight);
            }
        }
    }
    assert!(worst < 0.10, "worst error {worst}");
    // Symmetric, and zero at a point.
    assert_eq!(octagonal_distance([3, 9, -4], [3, 9, -4]), 0);
    assert_eq!(
        octagonal_distance([10, 20, 30], [-5, 7, 90]),
        octagonal_distance([-5, 7, 90], [10, 20, 30])
    );
}

#[test]
fn the_flood_stops_at_its_radius_nearest_first() {
    let words = flooded(8, 0, 2, 2);
    let level = view(&words);
    let mut flood = Flood::new(&level);
    // Standing in the middle of room 3: the portals to rooms 2 and 4 are one
    // sector away, the far portals of those rooms three.
    let at = 3 * 160 + 80;
    flood.solve(&level, query(3, at, 2 * S, NO_PAGE_BUDGET));
    let rooms: Vec<u16> = flood.rooms().iter().map(|f| f.room).collect();
    assert_eq!(rooms[0], 3);
    assert_eq!(flood.cost(3), Some(0));
    assert!(rooms.contains(&2) && rooms.contains(&4), "{rooms:?}");
    assert_eq!(rooms.len(), 3, "{rooms:?}");
    assert_eq!(flood.stats().end, FloodEnd::Exhausted);
    // Costs never decrease along the list.
    assert!(flood.rooms().windows(2).all(|w| w[0].cost <= w[1].cost));
}

#[test]
fn the_flood_stops_at_its_page_budget_with_a_round_set() {
    let words = flooded(8, 0, 6, 6);
    let level = view(&words);
    let mut flood = Flood::new(&level);
    // Three pages a room, a budget of ten: three rooms (nine pages) and then
    // the fourth is refused, even though a smaller one further out could fit.
    flood.solve(&level, query(3, 3 * 160 + 80, i32::MAX, 10));
    assert_eq!(flood.rooms().len(), 3);
    assert_eq!(flood.stats().pages, 9);
    assert!(matches!(flood.stats().end, FloodEnd::Budget(_)));
}

#[test]
fn a_flood_is_deterministic_and_a_sliced_one_matches_a_whole_one() {
    let words = flooded(12, 0, 8, 8);
    let level = view(&words);
    let at = 5 * 160 + 40;
    let mut whole = Flood::new(&level);
    whole.solve(&level, query(5, at, 6 * S, NO_PAGE_BUDGET));
    let mut again = Flood::new(&level);
    again.solve(&level, query(5, at, 6 * S, NO_PAGE_BUDGET));
    assert_eq!(whole.rooms(), again.rooms());

    // The same query in slices of two pops: nothing is in force until it
    // finishes, then it is the same list.
    let mut sliced = Flood::new(&level);
    let q = query(5, at, 6 * S, NO_PAGE_BUDGET);
    let mut finished = false;
    let mut calls = 0;
    while !finished {
        finished = sliced.ensure(&level, q, S, 2);
        calls += 1;
        assert!(calls < 100, "never finished");
        if !finished {
            assert!(sliced.rooms().is_empty(), "half a flood was published");
        }
    }
    assert!(calls > 1, "the slice was not small enough to matter");
    assert_eq!(sliced.rooms(), whole.rooms());
    // The same query again does no work and publishes nothing.
    assert!(!sliced.ensure(&level, q, S, 2));
    assert!(!sliced.busy());
}

#[test]
fn the_result_in_force_holds_until_a_moved_query_finishes() {
    let words = flooded(12, 0, 8, 8);
    let level = view(&words);
    let mut flood = Flood::new(&level);
    flood.solve(&level, query(2, 2 * 160 + 80, 3 * S, NO_PAGE_BUDGET));
    let before: Vec<_> = flood.rooms().to_vec();
    // A half sector of drift is within the tolerance: no new search.
    assert!(!flood.ensure(&level, query(2, 2 * 160 + 120, 3 * S, NO_PAGE_BUDGET), S, 1));
    assert!(!flood.busy());
    // Another room starts one, which one pop does not finish: the old set
    // stays in force.
    assert!(!flood.ensure(&level, query(6, 6 * 160 + 80, 3 * S, NO_PAGE_BUDGET), S, 1));
    assert!(flood.busy());
    assert_eq!(flood.rooms(), before.as_slice());
    assert!(flood.ensure(&level, query(6, 6 * 160 + 80, 3 * S, NO_PAGE_BUDGET), S, 1000));
    assert!(flood.contains(6) && !flood.contains(2));
}

#[test]
fn a_flood_block_round_trips_and_a_level_without_one_is_unchanged() {
    let with = flooded(4, 7, 2, 3);
    let level = view(&with);
    let header = level.budget_flood().expect("the flood block");
    assert_eq!(
        (header.pool_cap_pages, header.near_radius, header.res_radius),
        (7, 2, 3)
    );
    assert_eq!(level.flood_weight.len(), level.portals.len());
    assert!(level.flood_weight.iter().all(|&w| w == FLOOD_WEIGHT_ONE));
    assert!(level.validate_full().is_ok());
    assert_ne!(level.header.flags & LEVEL_BUDGET_FLOOD, 0);

    // The same level cooked without the block: no flag, and the bytes are the
    // prefix of the flooded core (the block is a pure tail).
    let without = corridor(4, 2, 3, None);
    let plain = words_as_bytes(&without, without.len() * 4);
    let flooded_bytes = words_as_bytes(&with, with.len() * 4);
    assert!(view(&without).budget_flood().is_none());
    let head = core::mem::size_of::<LevelHeader>();
    // Everything after the header up to the old end is identical.
    assert_eq!(&plain[head..], &flooded_bytes[head..plain.len()]);
}

#[test]
fn a_flood_block_with_the_wrong_weight_count_is_refused_at_write() {
    let core = LevelCore {
        sector_units: S as u16,
        flood: Some(block(0, 1, 2, 5)),
        ..LevelCore::default()
    };
    assert!(core.try_to_bytes().is_err());
}

#[test]
fn a_flooded_level_wants_the_drawn_set_as_need_and_the_resident_set_as_lead() {
    // 12 rooms of 2 sectors, 3 pages each. Draw 3 sectors, resident 6,
    // budget 100 pages (not binding).
    let words = flooded(12, 100, 3, 6);
    let level = view(&words);
    let mut window = StreamWindow::new(&level);
    assert!(window.floods() && !window.unbounded());
    assert_eq!(window.draw_radius(), 3 * S);
    assert_eq!(window.lead_radius(), 6 * S);
    let mut wanted = Vec::new();
    let at = [5 * 160 + 80, 50, S / 2];
    window.wanted(&level, Some((5, at)), Some((5, at)), &mut wanted);
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
    // Portals one sector from the middle of room 5, then three, five and so
    // on. Within 3 sectors (plus the activation walk of 1 sector): rooms
    // 4, 5, 6 and the far sides of 4 and 6, which are 3 sectors away.
    assert!(need.contains(&5) && need.contains(&4) && need.contains(&6), "{need:?}");
    assert!(!need.contains(&2), "{need:?}");
    // The resident flood adds rooms out to 6 sectors, none twice.
    assert!(lead.iter().all(|r| !need.contains(r)), "{lead:?}");
    assert!(lead.contains(&2) || lead.contains(&8), "{lead:?}");
    assert_eq!(window.drawn_rooms().unwrap()[0].room, 5);
    // Need first, nearest first.
    assert_eq!(wanted[0].room, 5);
    assert!(wanted
        .iter()
        .position(|w| w.want == Want::Lead)
        .is_none_or(|p| p >= need.len()));
}

#[test]
fn a_flooded_level_that_fits_its_pool_is_held_whole() {
    let words = flooded(6, 0, 2, 3);
    let level = view(&words);
    let mut window = StreamWindow::new(&level);
    assert!(window.unbounded() && !window.floods());
    let mut wanted = Vec::new();
    window.wanted(
        &level,
        Some((0, [10, 50, 10])),
        Some((0, [10, 50, 10])),
        &mut wanted,
    );
    assert_eq!(wanted.len(), 6);
    assert!(wanted.iter().all(|w| w.want == Want::Need));
    let plan = plan_pool(&level, &[3; 6]);
    assert_eq!((plan.total_pages, plan.pool_pages, plan.peak_rooms), (18, 18, 6));
}

#[test]
fn a_drawn_room_on_the_rim_stays_drawn_for_a_sector_but_one_never_drawn_does_not_join() {
    let words = flooded(12, 100, 3, 6);
    let level = view(&words);
    let mut window = StreamWindow::new(&level);
    let mut wanted = Vec::new();
    // From room 5 the far portal of room 6 is exactly 3 sectors away: drawn.
    let mut at = [5 * 160 + 80, 50, S / 2];
    window.wanted(&level, Some((5, at)), Some((5, at)), &mut wanted);
    let drawn = |window: &StreamWindow, room: u16| {
        window
            .drawn_rooms()
            .unwrap()
            .iter()
            .any(|f| f.room == room)
    };
    assert!(drawn(&window, 7));
    // Walk one sector towards room 4 (and inside room 5 still): room 7's
    // portal is now 4 sectors away, in the hysteresis band, so it stays.
    at[0] -= S;
    window.wanted(&level, Some((5, at)), Some((5, at)), &mut wanted);
    assert!(drawn(&window, 7), "the rim room flickered out");
    // A room that was never drawn does not join in the band: room 8 is two
    // sectors further than 7.
    assert!(!drawn(&window, 8));
    // Two sectors further and it is out for good.
    at[0] -= S;
    window.wanted(&level, Some((5, at)), Some((5, at)), &mut wanted);
    window.wanted(&level, Some((5, at)), Some((5, at)), &mut wanted);
    assert!(!drawn(&window, 7));
}

#[test]
fn the_pool_plan_of_a_flooded_level_is_its_busiest_set_plus_one_room() {
    let words = flooded(40, 100, 3, 6);
    let level = view(&words);
    let pages = [3usize; 40];
    let plan = plan_pool(&level, &pages);
    assert!(plan.peak_pages > 0 && plan.peak_pages < 40 * 3);
    assert_eq!(plan.pool_pages, plan.peak_pages + 3);
}
