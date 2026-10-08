//! Randomized property tests: random wanted lists, random transport outcomes,
//! random pins; after every step the structural invariants hold and the
//! safety rules were obeyed.

use super::harness::*;
use crate::*;
use std::collections::BTreeSet;
use std::vec::Vec;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }
}

#[cfg(miri)]
const STEPS: usize = 12;
#[cfg(not(miri))]
const STEPS: usize = 600;

fn random_key(rng: &mut Rng) -> ResourceKey {
    match rng.below(10) {
        0..=5 => region(rng.below(14)),
        6..=7 => key(COLLISION, rng.below(6)),
        _ => key(TEXTURE, rng.below(10)),
    }
}

fn random_class(rng: &mut Rng) -> Class {
    match rng.below(10) {
        0 => Class::Demand,
        1 => Class::CombatFill,
        2..=5 => Class::Lead,
        6..=8 => Class::Background,
        _ => Class::Audio,
    }
}

/// What a churn run exercised, to prove the properties were not vacuous.
#[derive(Default, Debug)]
struct Seen {
    evictions: u32,
    compactions: u32,
    cancelled: u32,
    backoff_skipped: u32,
    protected_full: u32,
    adopted: u32,
    transport_full: u32,
    deferred: u32,
}

impl Seen {
    fn add(&mut self, stats: &WindowStats) {
        self.evictions += stats.evictions;
        self.compactions += stats.compactions;
        self.cancelled += stats.cancelled;
        self.backoff_skipped += stats.backoff_skipped;
        self.protected_full += stats.protected_full;
        self.adopted += stats.adopted;
        self.transport_full += stats.transport_full;
        self.deferred += stats.deferred;
    }
}

/// Drive the engine with random traffic and check the rules after each step.
fn churn(seed: u64, config: Config<3>, seen: &mut Seen) {
    let mut rng = Rng(seed | 1);
    let mut engine = Engine::new(config);
    let mut host = TestHost::new(1);
    for index in 0..14 {
        host.pages.insert(region(index).as_u32(), 1 + rng.below(6));
    }
    for index in 0..6 {
        host.pages
            .insert(key(COLLISION, index).as_u32(), 1 + rng.below(3));
    }
    for index in 0..10 {
        host.pages
            .insert(key(TEXTURE, index).as_u32(), 1 + rng.below(2));
        host.distances
            .insert(key(TEXTURE, index).as_u32(), rng.below(900));
    }
    let mut pinned: Vec<ResourceKey> = Vec::new();
    let mut handles: Vec<Handle> = Vec::new();
    let mut open: Vec<ReadRequest> = Vec::new();
    let mut installing: BTreeSet<u32> = BTreeSet::new();

    for _ in 0..STEPS {
        // Random pins and unpins.
        if rng.below(6) == 0 {
            let candidate = random_key(&mut rng);
            if engine.pin(candidate, random_class(&mut rng)) {
                pinned.push(candidate);
            }
        }
        if rng.below(6) == 0 && !pinned.is_empty() {
            let at = rng.below(pinned.len() as u32) as usize;
            assert!(engine.unpin(pinned.swap_remove(at)));
        }
        // Distances for the oracle move around.
        for index in 0..14 {
            host.distances
                .insert(region(index).as_u32(), rng.below(900));
        }
        for index in 0..6 {
            host.distances
                .insert(key(COLLISION, index).as_u32(), rng.below(900));
        }
        let wanted: Vec<Wanted> = (0..rng.below(8))
            .map(|_| {
                Wanted::new(random_key(&mut rng), random_class(&mut rng), rng.below(900))
                    .with_deadline_ticks(rng.below(500))
            })
            .collect();

        // Snapshot what must survive this step.
        let protected: BTreeSet<u32> = (0..14)
            .map(region)
            .chain((0..6).map(|i| key(COLLISION, i)))
            .chain((0..10).map(|i| key(TEXTURE, i)))
            .filter(|k| {
                matches!(engine.state(*k), State::Reading | State::Installing)
                    || engine.pin_count(*k) > 0
            })
            .map(|k| k.as_u32())
            .collect();
        let demand_resident: Vec<ResourceKey> = wanted
            .iter()
            .filter(|w| w.class <= Class::CombatFill && engine.is_resident(w.key))
            .map(|w| w.key)
            .collect();

        let mut buffer = blank_evictions::<6>();
        let before = host.submitted.len();
        let report = engine.step(&wanted, &mut host, &mut buffer);
        seen.add(&report.stats);
        engine.check_invariants();

        for eviction in &buffer[..report.eviction_count] {
            assert!(
                !protected.contains(&eviction.key.as_u32()),
                "evicted a pinned or in-flight resource {:?}",
                eviction.key
            );
            assert!(
                !demand_resident.contains(&eviction.key),
                "evicted a resident demand/combat entry wanted this window"
            );
            assert!(engine.try_run(eviction.handle).is_none(), "stale handle");
            assert_ne!(engine.state(eviction.key), State::Resident);
        }
        // Every issued request landed in a run that is current and in budget.
        for request in &host.submitted[before..] {
            handles.push(request.handle);
            open.push(*request);
            assert_eq!(engine.try_run(request.handle), Some(request.run));
        }
        // Only the newest handle of a slot may resolve, and only for the
        // entry that holds it.
        let live_handles: Vec<Handle> = (0..14)
            .map(region)
            .chain((0..6).map(|i| key(COLLISION, i)))
            .chain((0..10).map(|i| key(TEXTURE, i)))
            .filter_map(|k| engine.handle(k))
            .collect();
        for handle in &handles {
            assert_eq!(
                engine.try_run(*handle).is_some(),
                live_handles.contains(handle),
                "a stale handle resolved, or a live one did not"
            );
        }
        if handles.len() > 200 {
            handles.drain(..100);
        }

        // Random transport outcomes.
        let mut still_open = Vec::new();
        for request in open.drain(..) {
            if host.cancelled.contains(&request.id) && rng.below(2) == 0 {
                assert!(engine.complete_read(request.id, ReadOutcome::Cancelled));
                continue;
            }
            match rng.below(6) {
                0 => {} // stays in flight
                1 => {
                    assert!(engine.complete_read(request.id, ReadOutcome::Failed));
                    continue;
                }
                _ => {
                    assert!(engine.complete_read(request.id, ReadOutcome::Done));
                    continue;
                }
            }
            still_open.push(request);
        }
        open = still_open;
        // Random install progress for landed payloads.
        for index in 0..6 {
            let hull = key(COLLISION, index);
            match engine.state(hull) {
                State::Landed if rng.below(2) == 0 => {
                    assert!(engine.begin_install(hull).is_some());
                    installing.insert(hull.as_u32());
                }
                State::Installing => match rng.below(3) {
                    0 => assert!(engine.finish_install(hull)),
                    1 => assert!(engine.fail_install(hull)),
                    _ => {}
                },
                _ => {}
            }
        }
        engine.check_invariants();
    }
    // Everything the transport still holds can be reported later.
    for request in open {
        assert!(engine.complete_read(request.id, ReadOutcome::Done));
    }
    engine.check_invariants();
}

#[test]
fn random_traffic_never_breaks_the_invariants() {
    let mut seen = Seen::default();
    for seed in 1..=if cfg!(miri) { 1 } else { 24 } {
        churn(seed * 0x9E37_79B9, config(), &mut seen);
        churn(seed * 0x85EB_CA6B, wide_config(), &mut seen);
    }
    if !cfg!(miri) {
        // The run really fought over room, backed off, cancelled and moved.
        assert!(seen.evictions > 100, "{seen:?}");
        assert!(seen.compactions > 0, "{seen:?}");
        assert!(seen.cancelled > 0, "{seen:?}");
        assert!(seen.backoff_skipped > 0, "{seen:?}");
        assert!(seen.protected_full > 0, "{seen:?}");
        assert!(seen.adopted > 0, "{seen:?}");
        assert!(seen.transport_full > 0, "{seen:?}");
    }
}

#[test]
#[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
fn random_traffic_with_a_tight_cpu_budget_still_never_breaks_them() {
    let mut tight = wide_config();
    tight.step_budget = 12;
    let mut seen = Seen::default();
    for seed in 1..=if cfg!(miri) { 1 } else { 10 } {
        churn(seed * 0xC2B2_AE35, tight, &mut seen);
    }
    if !cfg!(miri) {
        assert!(seen.deferred > 0, "{seen:?}");
    }
}

#[test]
#[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
fn budgets_are_never_exceeded_and_pages_are_conserved() {
    // check_invariants asserts the category and pool budgets after each step
    // of churn(); here one more explicit sweep on a pool that is far too
    // small for the demand, so every request is a fight over room.
    let mut config = wide_config();
    config.pools[0].capacity_pages = 8;
    config.categories[REGION.number() as usize].max_pages = 8;
    config.categories[COLLISION.number() as usize].max_pages = 3;
    let mut seen = Seen::default();
    for seed in 100..if cfg!(miri) { 101 } else { 116 } {
        churn(seed, config, &mut seen);
    }
    if !cfg!(miri) {
        assert!(seen.protected_full > 0 && seen.evictions > 0, "{seen:?}");
    }
}

#[test]
fn identical_seeds_replay_identically() {
    fn trace(seed: u64) -> (u32, u32, u32, u32) {
        let mut rng = Rng(seed);
        let mut engine = Engine::new(wide_config());
        let mut host = TestHost::new(3);
        for _ in 0..if cfg!(miri) { 20 } else { 200 } {
            let wanted: Vec<Wanted> = (0..4)
                .map(|_| Wanted::new(random_key(&mut rng), random_class(&mut rng), rng.below(900)))
                .collect();
            super::harness::step_and_land(&mut engine, &wanted, &mut host);
        }
        let totals = engine.totals();
        (
            totals.issued,
            totals.completed,
            totals.evictions,
            totals.wasted_evictions,
        )
    }
    assert_eq!(trace(7), trace(7));
    assert_ne!(trace(7), trace(8), "different seeds diverge");
}
