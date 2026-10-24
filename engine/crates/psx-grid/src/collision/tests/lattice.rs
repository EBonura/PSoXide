//! The world lattice and the resident set: many-room worlds answer exactly
//! like the all-rooms loop, and rooms can be added and removed at any time.

use super::*;
use crate::resident::{ResidentError, ResidentRooms};

/// A 6 by 5 plaza of 2 by 2 sector rooms on a 3 sector pitch, with a few
/// features so the rooms differ, plus one room stacked over room 7.
pub(super) fn plaza() -> Vec<Room> {
    let mut rooms = Vec::new();
    for iz in 0..5 {
        for ix in 0..6 {
            let origin = [ix * 3 * S, 0, iz * 3 * S];
            let mut room = Room::flat(origin, 2, 2, ((ix + iz) % 3) as i16 * 10);
            if (ix + 2 * iz) % 5 == 0 {
                room.pit(1, 1);
            }
            if (ix + iz) % 4 == 1 {
                room.ceiling(0, 0, 90, true);
            }
            if ix % 2 == 0 && iz % 2 == 1 {
                room.wall(1, 0, edge::EAST, 0, 120);
            }
            rooms.push(room);
        }
    }
    rooms.push(Room::flat([7 * S - S, 160, 0], 2, 2, 0));
    rooms
}

pub(super) fn queries(rng: &mut Lcg, count: usize) -> Vec<([i32; 3], [i32; 3])> {
    let (wx, wz) = (6 * 3 * S, 5 * 3 * S);
    (0..count)
        .map(|i| {
            let a = [
                rng.range(-60, wx + 60),
                rng.range(-40, 260),
                rng.range(-60, wz + 60),
            ];
            let b = match i % 3 {
                0 => [
                    rng.range(-60, wx + 60),
                    rng.range(-40, 260),
                    rng.range(-60, wz + 60),
                ],
                1 => [
                    a[0] + rng.range(-200, 200),
                    a[1],
                    a[2] + rng.range(-200, 200),
                ],
                _ => [a[0], rng.range(-3000, 3000), a[2]],
            };
            (a, b)
        })
        .collect()
}

/// Every query kind, lattice against a walk of every room.
fn assert_same_answers(w: &GridCollision<'_>, seed: u32) {
    let (fast, slow) = (w.with_lattice(true), w.with_lattice(false));
    let mut rng = Lcg(seed);
    for (a, b) in queries(&mut rng, 1500) {
        for query in [
            point(a, b),
            body(a, b),
            CollisionTraceQuery::body(RoomPoint::from_array(a), RoomPoint::from_array(b), 30, 70),
        ] {
            assert_eq!(trace(&fast, query), trace(&slow, query), "{query:?}");
        }
        assert_eq!(
            fast.line_of_sight(a, b),
            slow.line_of_sight(a, b),
            "{a:?} {b:?}"
        );
        assert_eq!(fast.body_blocked(a, 12, 48), slow.body_blocked(a, 12, 48));
        assert_eq!(fast.point_in_solid(a), slow.point_in_solid(a));
        assert_eq!(fast.contents_at(a), slow.contents_at(a));
        for hint in [0, 7, 20] {
            assert_eq!(
                fast.locate(hint, a),
                slow.locate(hint, a),
                "{a:?} hint {hint}"
            );
        }
    }
}

#[test]
fn the_lattice_answers_like_walking_every_room() {
    let built = build(&plaza());
    for resident in [vec![], vec![true; 31]] {
        built.run_with(&resident, |w| {
            assert!(w.lattice, "a plaza this size must use the lattice");
            assert_same_answers(w, 99);
        });
    }
    // The same with every third room evicted: they stand in as solid boxes.
    let evicted: Vec<bool> = (0..31).map(|i| i % 3 != 0).collect();
    built.run_with(&evicted, |w| assert_same_answers(w, 4242));
}

#[test]
fn candidates_cover_every_overlapping_room_once() {
    let built = build(&plaza());
    built.run(|w| {
        let mut rng = Lcg(5);
        for _ in 0..800 {
            let lo = (rng.range(-300, 1300), rng.range(-300, 1100));
            let hi = (lo.0 + rng.range(0, 600), lo.1 + rng.range(0, 500));
            let near: Vec<usize> = w.rooms_near(lo, hi).collect();
            let mut seen = near.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(
                seen.len(),
                near.len(),
                "a room came twice for {lo:?} {hi:?}"
            );
            for (index, rec) in w.level().rooms.iter().enumerate() {
                let x1 = rec.origin[0] + i32::from(rec.width) * S;
                let z1 = rec.origin[2] + i32::from(rec.depth) * S;
                let touches =
                    rec.origin[0] <= hi.0 && x1 > lo.0 && rec.origin[2] <= hi.1 && z1 > lo.1;
                if touches {
                    assert!(
                        near.contains(&index),
                        "room {index} missing for {lo:?} {hi:?}"
                    );
                }
            }
        }
    });
}

#[test]
fn the_cell_of_a_body_centre_names_every_room_the_body_can_touch() {
    let built = build(&plaza());
    built.run(|w| {
        assert!(w.lattice);
        let mut rng = Lcg(77);
        for _ in 0..3000 {
            let (x, z) = (rng.range(-300, 1700), rng.range(-300, 1300));
            // Bodies are at most a sector wide (the provider clamps them).
            let r = rng.range(0, S);
            let visit = w.visit(x, z, r);
            let rooms = w.level().rooms.len();
            let listed: Vec<usize> = (0..visit.len(rooms)).map(|i| visit.room(i)).collect();
            for (index, rec) in w.level().rooms.iter().enumerate() {
                let x1 = rec.origin[0] + i32::from(rec.width) * S;
                let z1 = rec.origin[2] + i32::from(rec.depth) * S;
                let touches =
                    rec.origin[0] <= x + r && x1 > x - r && rec.origin[2] <= z + r && z1 > z - r;
                assert!(
                    !touches || listed.contains(&index),
                    "room {index} touched by a body at ({x}, {z}) radius {r} but not listed"
                );
            }
        }
        // A wider-than-margin query falls back to every room.
        let wide = w.visit(10, 10, 5 * S);
        assert_eq!(wide.len(w.level().rooms.len()), w.level().rooms.len());
    });
}

#[test]
fn rooms_can_be_loaded_and_evicted_while_queries_run() {
    let built = build(&plaza());
    let level = LevelView::parse(words_as_bytes(&built.level, built.level_len)).unwrap();
    let views: Vec<CollisionView<'_>> = built
        .rooms
        .iter()
        .map(|(w, l)| CollisionView::parse(words_as_bytes(w, *l)).unwrap())
        .collect();
    let mut set = ResidentRooms::with_capacity(level, 8).unwrap();
    let probe = |set: &ResidentRooms<'_>, room: usize| {
        let rec = set.level().rooms[room];
        let (x, z) = (rec.origin[0] + S, rec.origin[2] + S / 2);
        let w = set.collision();
        (
            w.body_blocked([x, 40, z], 12, 48),
            w.line_of_sight([x, 40, z - 20], [x, 40, z + 20]),
        )
    };
    // Nothing resident: every room is a solid box, nothing is in sight-blocking.
    assert!(set.is_empty());
    assert_eq!(probe(&set, 3), (true, true));
    let epoch = set.epoch();
    // Load rooms 0..8; the room under test stops blocking once it is in.
    for (room, view) in views.iter().enumerate().take(8) {
        set.insert_collision(room, *view).unwrap();
    }
    assert_ne!(set.epoch(), epoch);
    assert_eq!(set.len(), 8);
    assert_eq!(probe(&set, 3), (false, true));
    assert_eq!(probe(&set, 8), (true, true), "room 8 is still not resident");
    // Capacity is a per-set limit, a duplicate is refused.
    assert_eq!(set.insert_collision(8, views[8]), Err(ResidentError::Full));
    assert_eq!(
        set.insert_collision(3, views[3]),
        Err(ResidentError::AlreadyResident)
    );
    assert_eq!(
        set.insert_collision(99, views[3]),
        Err(ResidentError::NoSuchRoom)
    );
    // Evict room 3: it is solid again, and room 8 can take the freed slot.
    let slot = set.remove(3).unwrap();
    assert_eq!(slot.room, 3);
    assert!(!set.contains(3) && set.remove(3).is_none());
    assert_eq!(probe(&set, 3), (true, true));
    set.insert_collision(8, views[8]).unwrap();
    assert_eq!(probe(&set, 8), (false, true));
    assert_eq!(set.len(), 8);
    // Reload it at a "new address" (a compaction): same answers as before.
    set.remove(5).unwrap();
    set.insert_collision(3, views[3]).unwrap();
    assert_eq!(probe(&set, 3), (false, true));
    assert_eq!(probe(&set, 5), (true, true));
    // Throughout, a body is never reported inside a room that is not here.
    let w = set.collision();
    assert!(w.foreign && !set.is_complete());
    assert_same_answers(&w, 17);
}
