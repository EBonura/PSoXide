//! Walk distance over the portal graph.

use crate::format::*;
use crate::view::LevelView;
use crate::walk::WalkSet;
use crate::write::{aligned_copy, words_as_bytes, LevelCore};
use alloc::vec;
use alloc::vec::Vec;

const S: i32 = 80;

/// `lengths[i]` sectors long rooms in a row along X, one sector deep, joined
/// by an open portal at every shared wall (both directions).
fn corridor(lengths: &[u8]) -> (Vec<u32>, usize) {
    let mut rooms = Vec::new();
    let mut portals = Vec::new();
    let mut x = 0;
    for (i, &len) in lengths.iter().enumerate() {
        let first = portals.len() as u16;
        let wall = |at: i16| Portal {
            dest: 0,
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
            portals.push(Portal {
                dest: (i - 1) as u16,
                ..wall(0)
            });
        }
        if i + 1 < lengths.len() {
            portals.push(Portal {
                dest: (i + 1) as u16,
                ..wall(i16::from(len) * S as i16)
            });
        }
        rooms.push(RoomRecord {
            origin: [x, 0, 0],
            width: len,
            depth: 1,
            portal_first: first,
            portal_count: portals.len() as u16 - first,
            min_y: 0,
            max_y: 100,
            ..RoomRecord::default()
        });
        x += i32::from(len) * S;
    }
    let core = LevelCore {
        sector_units: S as u16,
        rooms,
        portals,
        ..LevelCore::default()
    };
    let bytes = core.to_bytes();
    (aligned_copy(&bytes), bytes.len())
}

#[test]
fn distance_follows_portal_centres_not_hop_counts() {
    // Three one-sector rooms then a ten sector hall then a one-sector room.
    let (words, len) = corridor(&[1, 1, 1, 10, 1]);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    let mut walk = WalkSet::new(&level);
    walk.compute(&level, 0, [S / 2, 50, S / 2], 100 * S);
    assert_eq!(walk.cost(0), Some(0));
    // Portal centres sit at x = S, 2S, 3S, 13S: the first is S/2 away (the
    // height difference of 0 is the same height as the start's 50).
    assert_eq!(walk.cost(1), Some(S / 2));
    assert_eq!(walk.cost(2), Some(S / 2 + S));
    assert_eq!(walk.cost(3), Some(S / 2 + 2 * S));
    assert_eq!(walk.cost(4), Some(S / 2 + 12 * S));
    // Two hops from room 0 are rooms 1 and 2, 1.5 sectors of walking; from
    // the start of the hall the next room is nine and a half sectors away.
    walk.compute(&level, 3, [3 * S + S / 2, 50, S / 2], 100 * S);
    assert_eq!(walk.cost(4), Some(9 * S + S / 2));
    // A radius cuts the search off by distance.
    walk.compute(&level, 0, [S / 2, 50, S / 2], 2 * S);
    assert!(walk.contains(2) && !walk.contains(3));
    let reached: Vec<usize> = walk.rooms().collect();
    assert_eq!(reached.len(), 3);
}

#[test]
fn a_cached_search_is_reused_until_the_start_moves() {
    let (words, len) = corridor(&[2, 2, 2]);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    let mut walk = WalkSet::new(&level);
    assert!(walk.ensure(&level, 0, [10, 50, 10], 10 * S, S));
    let epoch = walk.epoch();
    assert!(!walk.ensure(&level, 0, [10 + S - 1, 50, 10], 10 * S, S));
    assert_eq!(walk.epoch(), epoch);
    assert!(walk.ensure(&level, 0, [10 + S, 50, 10], 10 * S, S));
    assert!(walk.ensure(&level, 1, [10 + S, 50, 10], 10 * S, S));
    assert!(walk.ensure(&level, 1, [10 + S, 50, 10], 4 * S, S));
    // Searching again leaves no stale cost behind.
    walk.compute(&level, 2, [5 * S, 50, 10], S);
    assert_eq!(walk.cost(0), None);
    assert_eq!(walk.cost(2), Some(0));
    // A start outside the level reaches nothing.
    walk.compute(&level, 99, [0; 3], 10 * S);
    assert!(walk.is_empty());
}

#[test]
fn a_search_costs_what_it_reaches() {
    // 400 rooms in a row; a short radius touches a handful.
    let (words, len) = corridor(&vec![1u8; 400]);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    let mut walk = WalkSet::new(&level);
    walk.compute(&level, 200, [200 * S + 10, 50, 10], 3 * S);
    assert_eq!(walk.len(), 7);
    assert!(walk.contains(197) && walk.contains(203) && !walk.contains(204));
}
