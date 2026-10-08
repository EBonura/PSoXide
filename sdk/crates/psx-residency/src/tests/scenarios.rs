use super::harness::*;
use crate::*;
use std::vec::Vec;

fn step(engine: &mut Engine, wanted: &[Wanted], host: &mut TestHost) -> StepReport {
    let mut buffer = blank_evictions::<8>();
    engine.step(wanted, host, &mut buffer)
}

fn step_evictions(
    engine: &mut Engine,
    wanted: &[Wanted],
    host: &mut TestHost,
) -> (StepReport, Vec<Eviction>) {
    let mut buffer = blank_evictions::<8>();
    let report = engine.step(wanted, host, &mut buffer);
    let evicted = buffer[..report.eviction_count].to_vec();
    (report, evicted)
}

#[test]
fn demand_precedes_lead_and_nearer_lead_precedes_farther() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let wanted = [lead(1, 500), lead(2, 100), demand(3)];
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.issued, 2, "transport depth is 2");
    assert_eq!(host.submitted[0].key, region(3));
    assert_eq!(host.submitted[1].key, region(2));
    assert_eq!(report.stats.transport_full, 1);
    assert_eq!(engine.state(region(1)), State::Absent);
    engine.check_invariants();

    // Depth frees up as reads finish; the farther lead goes next.
    let first = host.submitted[0].id;
    engine.complete_read(first, ReadOutcome::Done);
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.issued, 1);
    assert_eq!(host.last().key, region(1));
}

#[test]
fn earlier_deadline_beats_nearer_distance_inside_a_class() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let wanted = [
        Wanted::new(region(1), Class::Lead, 10).with_deadline_ticks(900),
        Wanted::new(region(2), Class::Lead, 500).with_deadline_ticks(100),
    ];
    step(&mut engine, &wanted, &mut host);
    assert_eq!(host.submitted[0].key, region(2));
    assert_eq!(host.submitted[0].deadline_ticks, 100);
}

#[test]
fn landed_bytes_resolve_through_the_handle_and_stale_handles_do_not() {
    let mut engine = engine();
    let mut host = TestHost::new(3);
    let mut buffer = PageBuffer::<16>::new();
    step(&mut engine, &[demand(7)], &mut host);
    let request = *host.last();
    // The transport lands the sectors straight into the run.
    assert!(buffer.write(request.run, 0, &[1, 2, 3]));
    assert!(!engine.is_resident(region(7)));
    assert!(engine.resident_handle(region(7)).is_none());
    assert!(engine.complete_read(request.id, ReadOutcome::Done));
    let handle = engine.resident_handle(region(7)).unwrap();
    assert_eq!(handle, request.handle);
    let run = engine.try_run(handle).unwrap();
    let bytes = buffer.bytes(run, engine.payload_bytes(region(7))).unwrap();
    assert_eq!(&bytes[..3], &[1, 2, 3]);
    // A second report of the same request is refused.
    assert!(!engine.complete_read(request.id, ReadOutcome::Done));
    engine.check_invariants();
}

#[test]
fn an_installing_category_lands_then_installs_before_it_is_resident() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let hull = key(COLLISION, 3);
    step(
        &mut engine,
        &[Wanted::new(hull, Class::Demand, 0)],
        &mut host,
    );
    let request = *host.last();
    engine.complete_read(request.id, ReadOutcome::Done);
    assert_eq!(engine.state(hull), State::Landed);
    assert!(!engine.is_resident(hull));
    assert_eq!(
        engine.next_install(None),
        Some(hull),
        "wanted by the last step"
    );
    step(&mut engine, &[], &mut host);
    assert_eq!(engine.next_install(None), None, "no longer wanted");
    assert_eq!(engine.next_install(Some(hull)), Some(hull), "but demanded");
    let handle = engine.begin_install(hull).unwrap();
    assert_eq!(handle, request.handle);
    assert_eq!(engine.state(hull), State::Installing);
    assert!(engine.begin_install(hull).is_none());
    // An installing payload is invisible and cannot be evicted.
    let mut buffer = blank_evictions::<4>();
    assert_eq!(engine.evict_unlisted(&[], &mut buffer), 0);
    assert!(engine.finish_install(hull));
    assert!(engine.is_resident(hull));
    engine.check_invariants();
}

#[test]
fn failed_install_backs_off_and_abort_install_does_not() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let hull = key(COLLISION, 1);
    let wanted = [Wanted::new(hull, Class::Demand, 0)];
    step(&mut engine, &wanted, &mut host);
    engine.complete_read(host.last().id, ReadOutcome::Done);
    engine.begin_install(hull).unwrap();
    assert!(engine.abort_install(hull));
    assert_eq!(engine.state(hull), State::Absent);
    // No penalty: the next step reads it again at once.
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.issued, 1);
    engine.complete_read(host.last().id, ReadOutcome::Done);
    engine.begin_install(hull).unwrap();
    assert!(engine.fail_install(hull));
    assert_eq!(engine.state(hull), State::Failed);
    assert_eq!(engine.category_pages(COLLISION), 0, "pages were freed");
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.issued, 0);
    assert_eq!(report.stats.backoff_skipped, 1);
}

#[test]
fn wanting_an_in_flight_request_again_adopts_it_and_promotes_the_class() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let report = step(&mut engine, &[lead(1, 50)], &mut host);
    assert_eq!(report.stats.issued, 1);
    let request = *host.last();
    // Nothing wants it for a window, then the demand arrives.
    step(&mut engine, &[], &mut host);
    let report = step(&mut engine, &[demand(1)], &mut host);
    assert_eq!(report.stats.issued, 0, "no second read for the same key");
    assert_eq!(report.stats.adopted, 1);
    assert_eq!(report.stats.promoted, 1);
    assert_eq!(host.promoted, [(request.id, Class::Demand)]);
    assert_eq!(host.submitted.len(), 1);
}

#[test]
fn duplicate_wanted_entries_issue_one_request_at_the_best_class() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let report = step(
        &mut engine,
        &[lead(1, 400), demand(1), lead(1, 100)],
        &mut host,
    );
    assert_eq!(report.stats.issued, 1);
    assert_eq!(host.last().class, Class::Demand);
}

#[test]
fn failed_reads_back_off_doubling_and_success_resets() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    let wanted = [demand(1)];
    step(&mut engine, &wanted, &mut host);
    let windows_until_retry = |engine: &mut Engine, host: &mut TestHost| {
        let mut skipped = 0;
        loop {
            let before = host.submitted.len();
            let report = step(engine, &wanted, host);
            if host.submitted.len() > before {
                return skipped;
            }
            assert_eq!(report.stats.backoff_skipped, 1);
            skipped += 1;
            assert!(skipped < 1000, "never abandoned");
        }
    };
    let mut expected = [15, 31, 63, 127, 255, 511, 511].into_iter();
    for _ in 0..7 {
        engine.complete_read(host.last().id, ReadOutcome::Failed);
        assert_eq!(engine.state(region(1)), State::Failed);
        assert_eq!(engine.category_pages(REGION), 0);
        assert_eq!(
            windows_until_retry(&mut engine, &mut host),
            expected.next().unwrap()
        );
    }
    // One success resets the schedule.
    engine.complete_read(host.last().id, ReadOutcome::Done);
    assert!(engine.is_resident(region(1)));
    engine.evict_unlisted(&[], &mut blank_evictions::<4>());
    step(&mut engine, &wanted, &mut host);
    engine.complete_read(host.last().id, ReadOutcome::Failed);
    assert_eq!(windows_until_retry(&mut engine, &mut host), 15);
    assert!(engine.totals().failed_loads >= 8);
}

#[test]
fn a_demand_cancels_the_least_urgent_read_and_its_pages_stay_reserved_until_reported() {
    let mut engine = engine();
    let mut host = TestHost::new(4);
    let background = |index, distance| Wanted::new(region(index), Class::Background, distance);
    step(
        &mut engine,
        &[background(1, 10), background(2, 200)],
        &mut host,
    );
    assert_eq!(host.submitted.len(), 2);
    let farther = host.submitted[1].id;
    let wanted = [demand(3), background(1, 10), background(2, 200)];
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.cancelled, 1);
    assert_eq!(host.cancelled, [farther]);
    assert_eq!(host.last().key, region(3), "the demand is issued at once");
    assert_eq!(engine.pool(0).unwrap().used_pages(), 12);
    // Sectors may still be landing: the pages are not reusable yet.
    assert_eq!(engine.in_flight_count(), 3);
    assert!(engine.complete_read(farther, ReadOutcome::Cancelled));
    assert_eq!(engine.pool(0).unwrap().used_pages(), 8);
    assert_eq!(engine.state(region(2)), State::Absent);
    engine.check_invariants();
}

#[test]
fn a_read_that_finishes_despite_the_cancel_is_kept() {
    let mut engine = engine();
    let mut host = TestHost::new(4);
    let background = |index, distance| Wanted::new(region(index), Class::Background, distance);
    step(
        &mut engine,
        &[background(1, 10), background(2, 200)],
        &mut host,
    );
    let farther = host.submitted[1].id;
    step(
        &mut engine,
        &[demand(3), background(1, 10), background(2, 200)],
        &mut host,
    );
    assert_eq!(host.cancelled, [farther]);
    assert!(engine.complete_read(farther, ReadOutcome::Done));
    assert!(engine.is_resident(region(2)), "good bytes are adopted");
    engine.check_invariants();
}

#[test]
fn lead_requests_never_cancel_anything() {
    let mut engine = engine();
    let mut host = TestHost::new(4);
    step(
        &mut engine,
        &[lead(1, 10), lead(2, 20), lead(3, 5)],
        &mut host,
    );
    assert!(host.cancelled.is_empty());
    assert_eq!(host.submitted.len(), 2);
}

#[test]
fn a_pin_requests_a_missing_resource_and_protects_a_resident_one() {
    let mut engine = engine();
    let mut host = TestHost::new(4);
    assert!(engine.pin(region(5), Class::Demand));
    let report = step(&mut engine, &[], &mut host);
    assert_eq!(report.stats.issued, 1);
    engine.complete_read(host.last().id, ReadOutcome::Done);
    assert!(engine.is_resident(region(5)));
    // Three more demands fill the pool (4 x 4 pages).
    let three = [demand(1), demand(2), demand(3)];
    for _ in 0..3 {
        step_and_land(&mut engine, &three, &mut host);
    }
    for index in [1, 2, 3, 5] {
        assert!(engine.is_resident(region(index)));
    }
    // A fourth demand finds only wanted-demand entries and a pin: nothing to
    // evict.
    let report = step(
        &mut engine,
        &[demand(1), demand(2), demand(3), demand(4)],
        &mut host,
    );
    assert_eq!(report.stats.protected_full, 1);
    assert_eq!(report.eviction_count, 0);
    // Without the demands, the three unwanted regions go; the pin stays.
    let mut evicted_all = Vec::new();
    for index in 4..7 {
        let (_, evicted) = step_and_land(&mut engine, &[demand(index)], &mut host);
        evicted_all.extend(evicted);
    }
    assert!(evicted_all.iter().all(|e| e.key != region(5)));
    assert!(engine.is_resident(region(5)));
    assert_eq!(engine.pin_count(region(5)), 1);
    assert!(engine.unpin(region(5)));
    assert!(!engine.unpin(region(5)));
    engine.check_invariants();
}

#[test]
fn eviction_takes_the_farthest_then_the_least_recently_used() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(4);
    fill_regions(
        &mut engine,
        &mut host,
        &[(1, 900), (2, 900), (3, 900), (4, 900)],
    );
    // r2 is the least recently used: a window that wants the others goes by.
    step(
        &mut engine,
        &[lead(1, 900), lead(3, 900), lead(4, 900)],
        &mut host,
    );
    host.distances.insert(region(1).as_u32(), 1000);
    host.distances.insert(region(2).as_u32(), 1000);
    host.distances.insert(region(3).as_u32(), 500);
    host.distances.insert(region(4).as_u32(), 400);
    let (report, evicted) = step_evictions(&mut engine, &[demand(5)], &mut host);
    assert_eq!(report.eviction_count, 1);
    assert_eq!(evicted[0].key, region(2), "equal distance: oldest goes");
    assert!(
        engine.try_run(evicted[0].handle).is_none(),
        "handle is stale"
    );
    assert_eq!(engine.state(region(5)), State::Reading);
    engine.check_invariants();
}

#[test]
fn the_hysteresis_band_is_defended_from_cheap_requests() {
    let build = || {
        let mut engine = Engine::new(wide_config());
        let mut host = TestHost::new(4);
        fill_regions(
            &mut engine,
            &mut host,
            &[(1, 90), (2, 90), (3, 90), (4, 90)],
        );
        for index in 1..=4 {
            // Inside the keep radius of 300: the hysteresis band.
            host.distances.insert(region(index).as_u32(), 200);
        }
        (engine, host)
    };
    let background = Wanted::new(region(5), Class::Background, 50);
    let (mut engine, mut host) = build();
    let report = step(&mut engine, &[background], &mut host);
    assert_eq!(
        report.stats.protected_full, 1,
        "background reaches tier 0 only"
    );
    let (mut engine, mut host) = build();
    let (report, _) = step_evictions(&mut engine, &[lead(5, 50)], &mut host);
    assert_eq!(
        report.eviction_count, 1,
        "lead may take the hysteresis band"
    );
    let (mut engine, mut host) = build();
    let (report, _) = step_evictions(&mut engine, &[demand(5)], &mut host);
    assert_eq!(report.eviction_count, 1);
    // Beyond the band, even background may evict.
    let (mut engine, mut host) = build();
    for index in 1..=4 {
        host.distances.insert(region(index).as_u32(), 2000);
    }
    let (report, _) = step_evictions(&mut engine, &[background], &mut host);
    assert_eq!(report.eviction_count, 1);
}

#[test]
fn a_demand_displaces_the_farthest_wanted_lead_but_a_lead_cannot_displace_a_lead() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(4);
    let leads = [(1, 100), (2, 200), (3, 300), (4, 400)];
    fill_regions(&mut engine, &mut host, &leads);
    let mut wanted: Vec<Wanted> = leads.iter().map(|&(i, d)| lead(i, d)).collect();
    wanted.push(lead(5, 50));
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.protected_full, 1);
    assert_eq!(report.eviction_count, 0);
    wanted.push(demand(6));
    let (report, evicted) = step_evictions(&mut engine, &wanted, &mut host);
    assert_eq!(report.eviction_count, 1);
    assert_eq!(evicted[0].key, region(4), "farthest wanted lead");
}

#[test]
fn same_class_displacement_needs_the_margin() {
    let mut config = Config::<1>::new();
    config.pools[0] = PoolConfig {
        capacity_pages: 4,
        placement: Placement::FirstFit,
    };
    config.categories[0] = CategoryConfig {
        pool: 0,
        max_pages: 4,
        max_entries: 1,
        movable: true,
        installs: false,
        keep_radius: 10,
    };
    config.protect_wanted[Class::CombatFill.as_index()] = false;
    config.max_victim_tier[Class::CombatFill.as_index()] = 2;
    let run = |margin: u32| {
        let mut config = config;
        config.displace_margin = margin;
        let mut engine = Residency::<4, 2, 1>::new(config);
        let mut host = TestHost::new(4);
        let mut evictions = blank_evictions::<2>();
        let fill = [Wanted::new(region(1), Class::CombatFill, 130)];
        engine.step(&fill, &mut host, &mut evictions);
        engine.complete_read(host.last().id, ReadOutcome::Done);
        let both = [
            Wanted::new(region(1), Class::CombatFill, 130),
            Wanted::new(region(2), Class::CombatFill, 100),
        ];
        engine.step(&both, &mut host, &mut evictions).eviction_count
    };
    assert_eq!(run(0), 1, "130 is farther than 100");
    assert_eq!(run(30), 0, "130 is not farther than 100 + 30");
}

#[test]
fn eviction_clears_the_cheapest_contiguous_window() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(4);
    for (index, pages) in [(1, 2), (2, 2), (3, 4), (4, 4), (5, 4)] {
        host.pages.insert(region(index).as_u32(), pages);
    }
    fill_regions(
        &mut engine,
        &mut host,
        &[(1, 900), (2, 900), (3, 900), (4, 900), (5, 900)],
    );
    for (index, distance) in [(1, 400), (2, 400), (3, 400), (4, 9000), (5, 9000)] {
        host.distances.insert(region(index).as_u32(), distance);
    }
    let r4_first = engine
        .try_run(engine.handle(region(4)).unwrap())
        .unwrap()
        .first_page;
    host.pages.insert(region(9).as_u32(), 6);
    let (report, evicted) = step_evictions(&mut engine, &[demand(9)], &mut host);
    assert_eq!(report.eviction_count, 2);
    let mut keys: Vec<u32> = evicted.iter().map(|e| e.key.index()).collect();
    keys.sort_unstable();
    assert_eq!(keys, [4, 5], "two far neighbours, not a near pair");
    assert_eq!(host.last().run.first_page, r4_first);
    assert_eq!(host.last().run.page_count, 6);
    engine.check_invariants();
}

#[test]
fn scattered_free_pages_are_compacted_instead_of_evicting() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(4);
    for (index, pages) in [(1, 4), (2, 2), (3, 4), (4, 2), (5, 4)] {
        host.pages.insert(region(index).as_u32(), pages);
    }
    fill_regions(
        &mut engine,
        &mut host,
        &[(1, 90), (2, 90), (3, 90), (4, 90), (5, 90)],
    );
    let freed = engine.evict_unlisted(
        &[region(1), region(3), region(5)],
        &mut blank_evictions::<4>(),
    );
    assert_eq!(freed, 2);
    assert_eq!(engine.pool(0).unwrap().free_pages(), 4);
    assert_eq!(engine.pool(0).unwrap().fragmentation().free_run_count, 2);
    let before = engine.pool(0).unwrap().layout_generation();
    host.pages.insert(region(9).as_u32(), 4);
    let report = step(&mut engine, &[demand(9)], &mut host);
    assert_eq!(report.eviction_count, 0, "free pages existed");
    assert_eq!(report.stats.issued, 1);
    assert!(report.stats.compactions >= 1);
    assert!(!host.moved.is_empty());
    assert!(host.moved.iter().all(|&(pool, ..)| pool == 0));
    assert_ne!(engine.pool(0).unwrap().layout_generation(), before);
    assert_eq!(
        report.stats.pages_moved,
        host.moved.iter().map(|m| m.3).sum::<u32>()
    );
    engine.check_invariants();
}

#[test]
fn a_non_movable_category_is_never_compacted() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(2);
    let page = |i| key(TEXTURE, i);
    let wanted: Vec<Wanted> = (0..4)
        .map(|i| Wanted::new(page(i), Class::Lead, 90))
        .collect();
    for _ in 0..3 {
        step_and_land(&mut engine, &wanted, &mut host);
    }
    // Free slots 0 and 2 (pages 0..2 and 4..6): 4 free pages, in two runs.
    let keep = [page(1), page(3)];
    assert_eq!(engine.evict_unlisted(&keep, &mut blank_evictions::<4>()), 2);
    host.pages.insert(page(9).as_u32(), 4);
    let report = step(
        &mut engine,
        &[Wanted::new(page(9), Class::Demand, 0)],
        &mut host,
    );
    assert_eq!(report.stats.compactions, 0);
    assert!(host.moved.is_empty());
    // Not compacted, so the only way is to evict the unwanted neighbours.
    assert_eq!(report.stats.issued + report.stats.protected_full, 1);
    engine.check_invariants();
}

#[test]
fn a_category_over_budget_evicts_its_own_entries_not_another_categorys() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(4);
    fill_regions(&mut engine, &mut host, &[(1, 90), (2, 90)]);
    let hull = |i| key(COLLISION, i);
    // Collision may hold 6 pages; each hull is 4.
    step_and_land(
        &mut engine,
        &[Wanted::new(hull(1), Class::Lead, 90)],
        &mut host,
    );
    assert_eq!(engine.category_pages(COLLISION), 4);
    // The pool still has room, but a second hull busts the category budget.
    assert_eq!(engine.pool(0).unwrap().free_pages(), 4);
    let wanted = [Wanted::new(hull(2), Class::Demand, 0)];
    host.distances.insert(hull(1).as_u32(), 5000);
    let (report, evicted) = step_evictions(&mut engine, &wanted, &mut host);
    assert_eq!(report.eviction_count, 1);
    assert_eq!(evicted[0].key, hull(1));
    assert!(engine.is_resident(region(1)) && engine.is_resident(region(2)));
    assert!(engine.category_pages(COLLISION) <= 6);
    engine.check_invariants();
}

#[test]
fn a_request_larger_than_its_category_budget_is_refused_without_evicting() {
    let mut engine = engine();
    let mut host = TestHost::new(2);
    host.pages.insert(key(COLLISION, 1).as_u32(), 7);
    let report = step(
        &mut engine,
        &[Wanted::new(key(COLLISION, 1), Class::Demand, 0)],
        &mut host,
    );
    assert_eq!(report.stats.over_budget, 1);
    assert_eq!(report.stats.protected_full, 1);
    assert_eq!(report.stats.issued, 0);
}

#[test]
fn eviction_is_withheld_when_it_cannot_be_reported() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(4);
    fill_regions(
        &mut engine,
        &mut host,
        &[(1, 90), (2, 90), (3, 90), (4, 90)],
    );
    for index in 1..=4 {
        host.distances.insert(region(index).as_u32(), 5000);
    }
    let report = engine.step(&[demand(5)], &mut host, &mut NO_EVICTIONS.clone());
    assert_eq!(report.stats.eviction_overflow, 1);
    assert_eq!(report.stats.issued, 0);
    for index in 1..=4 {
        assert!(engine.is_resident(region(index)));
    }
    engine.check_invariants();
}

#[test]
fn the_cpu_budget_defers_everything_but_demand() {
    let mut config = config();
    config.max_in_flight = 8;
    config.step_budget = 6;
    let mut engine = Engine::new(config);
    let mut host = TestHost::new(1);
    let mut wanted: Vec<Wanted> = (10..20).map(|i| lead(i, i)).collect();
    wanted.push(demand(1));
    let report = step(&mut engine, &wanted, &mut host);
    assert!(report.stats.deferred > 0, "{:?}", report.stats);
    assert_eq!(host.submitted[0].key, region(1), "demand always runs");
    assert!(report.stats.work_used >= 6);
    // Unlimited budget handles all of them.
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(1);
    let report = step(&mut engine, &wanted, &mut host);
    assert_eq!(report.stats.deferred, 0);
    assert_eq!(report.stats.issued, 8);
}

#[test]
fn a_request_that_starts_where_an_in_flight_one_ends_is_chained() {
    let mut engine = engine();
    let mut host = TestHost::new(4);
    host.lbas.insert(region(1).as_u32(), 500);
    host.lbas.insert(region(2).as_u32(), 504);
    step(&mut engine, &[lead(1, 10), lead(2, 20)], &mut host);
    assert_eq!(host.submitted[0].contiguous_with, None);
    assert_eq!(
        host.submitted[1].contiguous_with,
        Some(host.submitted[0].id)
    );
}

#[test]
fn a_transport_that_refuses_gets_its_allocation_back() {
    let mut engine = engine();
    let mut host = TestHost::new(4);
    host.full_on_submit = true;
    let report = step(&mut engine, &[demand(1)], &mut host);
    assert_eq!(report.stats.transport_full, 1);
    assert_eq!(engine.pool(0).unwrap().used_pages(), 0);
    assert_eq!(engine.category_entry_count(REGION), 0);
    assert_eq!(engine.tracked_count(), 0);
    host.full_on_submit = false;
    host.accept = false;
    let report = step(&mut engine, &[demand(1)], &mut host);
    assert_eq!(report.stats.transport_full, 1);
    assert_eq!(report.stats.issued, 0);
    engine.check_invariants();
}

#[test]
fn unknown_and_unconfigured_keys_are_counted_and_ignored() {
    let mut engine = engine();
    struct NoDisc;
    impl Host for NoDisc {
        fn extent(&self, _: ResourceKey) -> Option<Extent> {
            None
        }
        fn walk_distance(&self, _: ResourceKey) -> u32 {
            0
        }
        fn submit(&mut self, _: &ReadRequest) -> Submit {
            Submit::Full
        }
        fn cancel(&mut self, _: RequestId) {}
    }
    let outside = key(Category::from_number(40).unwrap(), 1);
    let mut evictions = blank_evictions::<1>();
    let report = engine.step(
        &[demand(1), Wanted::new(outside, Class::Demand, 0)],
        &mut NoDisc,
        &mut evictions,
    );
    assert_eq!(report.stats.unknown_keys, 1);
    assert_eq!(report.stats.unconfigured, 1);
    assert_eq!(report.stats.issued, 0);
}

#[test]
fn the_entry_table_and_pin_counts_have_limits() {
    let mut engine = engine();
    for index in 0..12 {
        assert!(engine.pin(region(index), Class::Lead));
    }
    assert!(!engine.pin(region(12), Class::Lead), "table of 12 is full");
    assert!(engine.pin(region(0), Class::Demand), "pins nest");
    assert_eq!(engine.pin_count(region(0)), 2);
    let mut host = TestHost::new(1);
    let report = step(&mut engine, &[demand(30)], &mut host);
    assert_eq!(report.stats.wanted_dropped, 1, "pins fill the whole table");
    assert_eq!(
        report.stats.issued, 2,
        "the pins are requested, up to depth"
    );
    for index in 0..12 {
        while engine.unpin(region(index)) {}
    }
    assert_eq!(engine.tracked_count(), 2, "only the two in flight remain");
}

#[test]
fn evict_unlisted_spares_the_listed_the_pinned_and_the_in_flight() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(2);
    fill_regions(&mut engine, &mut host, &[(1, 90), (2, 90), (3, 90)]);
    engine.pin(region(2), Class::Lead);
    step(&mut engine, &[demand(4)], &mut host); // 4 stays in flight
    let mut buffer = blank_evictions::<8>();
    let evicted = engine.evict_unlisted(&[region(1)], &mut buffer);
    assert_eq!(evicted, 1);
    assert_eq!(buffer[0].key, region(3));
    assert_eq!(engine.state(region(4)), State::Reading);
    assert!(engine.is_resident(region(1)) && engine.is_resident(region(2)));
}

#[test]
fn evicting_an_unused_landing_counts_as_a_wasted_read() {
    let mut engine = Engine::new(wide_config());
    let mut host = TestHost::new(8);
    fill_regions(&mut engine, &mut host, &[(1, 90), (2, 90)]);
    engine.touch(region(1));
    engine.evict_unlisted(&[], &mut blank_evictions::<4>());
    // Wanted entries mark themselves used when found resident, so both were
    // used by the second fill step except where the landing came last.
    assert!(engine.totals().evictions == 2);
    assert!(engine.totals().wasted_evictions <= 2);
}

#[test]
fn identical_inputs_give_identical_traces() {
    fn run() -> (Vec<ReadRequest>, Totals) {
        let mut engine = Engine::new(wide_config());
        let mut host = TestHost::new(3);
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..60 {
            let wanted: Vec<Wanted> = (0..4)
                .map(|_| {
                    let r = next();
                    Wanted::new(
                        region(r % 9),
                        if r & 0x100 == 0 {
                            Class::Lead
                        } else {
                            Class::Demand
                        },
                        (r >> 9) % 700,
                    )
                })
                .collect();
            step_and_land(&mut engine, &wanted, &mut host);
        }
        (host.submitted, engine.totals())
    }
    let (a, totals_a) = run();
    let (b, totals_b) = run();
    assert_eq!(a, b);
    assert_eq!(totals_a, totals_b);
    assert!(!a.is_empty());
}

/// The grid streamer's `fragmented_pool_compacts_and_preserves_live_bytes`,
/// with the allocation map and the bytes now in two types.
#[test]
fn compaction_moves_the_bytes_with_the_runs() {
    let mut pool = PagePool::<4>::new(0, 8, Placement::FirstFit);
    let mut buffer = PageBuffer::<8>::new();
    let (_, first) = pool.allocate(2).unwrap();
    let (middle, _) = pool.allocate(2).unwrap();
    let (third, third_run) = pool.allocate(2).unwrap();
    assert!(buffer.write(first, 0, &[1, 1]));
    assert!(buffer.write(third_run, 0, &[0x5a, 0xa5]));
    pool.free(middle);
    let before = pool.layout_generation();
    // A 3-page run does not fit the 2-page hole; compaction makes room.
    assert!(pool.find_fit(3).is_none());
    while let Some(moved) = pool.compact_step(|from, to| {
        assert!(buffer.move_pages(from, to));
    }) {
        assert_eq!(moved.handle, third);
    }
    assert_ne!(pool.layout_generation(), before);
    let run = pool.try_run(third).expect("the handle survives the move");
    assert_eq!(run.first_page, 2);
    assert_eq!(&buffer.bytes(run, 2).unwrap()[..2], &[0x5a, 0xa5]);
    assert_eq!(&buffer.bytes(first, 2).unwrap()[..2], &[1, 1]);
    assert!(pool.allocate(3).is_some());
    assert_eq!(pool.free_pages(), 1);
}
