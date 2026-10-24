//! The trace memo answers like a fresh trace, whatever happened to the
//! resident set in between.

use super::lattice::{plaza, queries};
use super::*;
use crate::resident::ResidentRooms;

fn ask(
    provider: &mut GridTraceProvider<'_, '_>,
    query: CollisionTraceQuery,
) -> Option<CollisionTrace> {
    let mut out = CollisionTrace::default();
    provider.trace_into(query, &mut out).then_some(out)
}

/// Point and body requests over the plaza, each asked three times with other
/// requests between, so most answers come from the memo.
fn requests(seed: u32) -> Vec<CollisionTraceQuery> {
    let mut rng = Lcg(seed);
    let mut out = Vec::new();
    for (a, b) in queries(&mut rng, 24) {
        out.push(point(a, b));
        out.push(body(a, b));
        out.push(CollisionTraceQuery::body(
            RoomPoint::from_array(a),
            RoomPoint::from_array(b),
            30,
            70,
        ));
    }
    let round = out.clone();
    out.extend(round.iter().rev());
    out.extend(round);
    out
}

/// One body probe straight down into the middle of each of the first twelve
/// rooms: as many requests as the memo holds, so none is replaced while the
/// resident set changes, and each answer depends on whether its room is in.
fn room_probes(set: &ResidentRooms<'_>) -> Vec<CollisionTraceQuery> {
    (0..TraceMemo::CAPACITY)
        .map(|room| {
            let rec = set.level().rooms[room];
            let (x, z) = (rec.origin[0] + S, rec.origin[2] + S);
            body([x, 40, z], [x, -40, z])
        })
        .collect()
}

#[test]
fn a_memoized_provider_answers_like_a_fresh_one_across_set_changes() {
    let built = build(&plaza());
    let level = LevelView::parse(words_as_bytes(&built.level, built.level_len)).unwrap();
    let views: Vec<CollisionView<'_>> = built
        .rooms
        .iter()
        .map(|(w, l)| CollisionView::parse(words_as_bytes(w, *l)).unwrap())
        .collect();
    let mut set = ResidentRooms::with_capacity(level, 12).unwrap();
    let mut memo = TraceMemo::new();
    let mut changed = 0;
    let mut previous: Vec<Option<CollisionTrace>> = Vec::new();
    // Rooms come and go between batches; the memo must follow.
    for phase in 0..8usize {
        match phase % 4 {
            0 | 1 => {
                for room in (phase * 3)..(phase * 3 + 3) {
                    set.insert_collision(room % 12, views[room % 12]).ok();
                }
            }
            2 => {
                for room in 0..4 {
                    set.remove(room);
                }
            }
            _ => {
                set.remove(7);
                set.insert_collision(0, views[0]).ok();
            }
        }
        let world = set.collision();
        let probes = room_probes(&set);
        let mut answers = Vec::new();
        // Twice: the first batch fills the memo (or hits what the last phase
        // left, if the set had not changed), the second is all hits.
        for _ in 0..2 {
            answers.clear();
            for query in &probes {
                let fresh = ask(&mut GridTraceProvider::new(&world), *query);
                let memoized = ask(
                    &mut GridTraceProvider::new(&world).with_memo(&mut memo),
                    *query,
                );
                assert_eq!(memoized, fresh, "phase {phase} {query:?}");
                answers.push(fresh);
            }
        }
        changed += usize::from(!previous.is_empty() && previous != answers);
        previous = answers;
        assert_eq!(memo.len(), probes.len(), "phase {phase}");
    }
    assert!(
        changed >= 4,
        "the set changes must change answers: {changed}"
    );
}

#[test]
fn a_memoized_provider_answers_like_a_fresh_one_on_mixed_requests() {
    let built = build(&plaza());
    for resident in [
        vec![],
        vec![true; 31],
        (0..31).map(|i| i % 3 != 0).collect(),
    ] {
        built.run_with(&resident, |w| {
            let mut memo = TraceMemo::new();
            for query in requests(1000) {
                let fresh = ask(&mut GridTraceProvider::new(w), query);
                let memoized = ask(&mut GridTraceProvider::new(w).with_memo(&mut memo), query);
                assert_eq!(memoized, fresh, "{query:?}");
            }
            assert_eq!(memo.len(), TraceMemo::CAPACITY);
        });
    }
}

#[test]
fn a_memo_holds_the_most_recent_answers_and_hits_return_them_untouched() {
    let built = build(&plaza());
    built.run(|w| {
        let mut memo = TraceMemo::new();
        let queries: Vec<CollisionTraceQuery> = requests(7).into_iter().take(40).collect();
        for query in &queries {
            let first = ask(&mut GridTraceProvider::new(w).with_memo(&mut memo), *query);
            let again = ask(&mut GridTraceProvider::new(w).with_memo(&mut memo), *query);
            assert_eq!(first, again, "{query:?}");
            assert_eq!(first, ask(&mut GridTraceProvider::new(w), *query));
        }
        assert_eq!(memo.len(), TraceMemo::CAPACITY, "old answers are replaced");
    });
}

#[test]
fn a_request_out_of_range_fails_and_leaves_the_output_alone() {
    let built = build(&plaza());
    built.run(|w| {
        let mut memo = TraceMemo::new();
        let far = point([0, 0, 0], [COORD_LIMIT + 1, 0, 0]);
        let mut out = CollisionTrace::unobstructed(RoomPoint::new(1, 2, 3));
        let before = out;
        let mut provider = GridTraceProvider::new(w).with_memo(&mut memo);
        assert!(!provider.trace_into(far, &mut out));
        assert_eq!(out, before);
        assert_eq!(memo.len(), 0);
    });
}
