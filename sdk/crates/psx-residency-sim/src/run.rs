//! The route simulator: the real engine against the drive model.

use crate::drive::DriveModel;
use crate::world::{RegionGraph, Route, TICKS_PER_SECOND};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use psx_residency::{
    Category, CategoryConfig, Class, Config, Eviction, Extent, Host, Placement, PoolConfig,
    ReadOutcome, ReadRequest, RequestId, Residency, ResourceKey, Submit, Wanted, PAGE_BYTES,
};

type Engine = Residency<256, 256, 1>;

/// Simulator knobs.
#[derive(Clone, Debug)]
pub struct SimConfig {
    /// The drive.
    pub drive: DriveModel,
    /// Pages in the region pool.
    pub pool_pages: u32,
    /// Regions within this walk distance of the player are requested (lead
    /// ring), nearest first.
    pub lead_radius: u32,
    /// Unwanted regions within this distance are in the hysteresis band.
    pub keep_radius: u32,
    /// Player speed assumed for lead deadlines, units per second. The default
    /// is the design's `v_max` of 470, a design estimate, not a measurement.
    pub speed_units_per_second: u32,
    /// Transport depth.
    pub max_in_flight: u8,
    /// Microseconds per simulated tick (16 667: 60 Hz).
    pub tick_micros: u32,
    /// Ticks of install work after a region lands (0: none, the region is
    /// resident when read). The design lists the install cost as unmeasured.
    pub install_ticks: u32,
    /// Every Nth read fails (0: none).
    pub fail_every: u32,
    /// Placement policy of the pool.
    pub placement: Placement,
    /// Whether compaction may move regions.
    pub movable: bool,
    /// Bookkeeping budget per step (see [`psx_residency::Config::step_budget`]).
    pub step_budget: u32,
    /// Regions pinned for the whole run (the home pin), at demand class.
    pub pinned: Vec<u32>,
    /// Load the start region's needs before the route begins (a loading
    /// screen at boot), not counted in the report.
    pub warm_start: bool,
}

impl SimConfig {
    /// Defaults: double-speed nominal drive, queue depth 2, 60 Hz ticks, no
    /// install cost, no failures, best-fit, movable, warm start, keep radius
    /// one quarter beyond the lead radius.
    pub fn new(pool_pages: u32, lead_radius: u32) -> Self {
        Self {
            drive: DriveModel::double_speed(),
            pool_pages,
            lead_radius,
            keep_radius: CategoryConfig::keep_radius_for(lead_radius, 0),
            speed_units_per_second: 470,
            max_in_flight: 2,
            tick_micros: 16_667,
            install_ticks: 0,
            fail_every: 0,
            placement: Placement::BestFit,
            movable: true,
            step_budget: u32::MAX,
            pinned: Vec::new(),
            warm_start: true,
        }
    }
}

/// The worst deadline miss of a run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorstMiss {
    /// The region that was late.
    pub region: u32,
    /// Tick it became needed.
    pub need_tick: u32,
    /// Negative: ticks the player waited.
    pub slack_ticks: i64,
}

/// What a simulation found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimReport {
    /// Ticks simulated (route length).
    pub ticks: u32,
    /// Times a region became needed.
    pub episodes: u32,
    /// Episodes where the region was not resident when needed (a stall).
    pub misses: u32,
    /// Region-ticks needed but not resident.
    pub miss_ticks: u32,
    /// Ticks with at least one unmet need (the player would be held).
    pub stall_ticks: u32,
    /// Smallest slack over all episodes, in ticks: how long before it was
    /// needed the region was resident (negative: late). `None` without
    /// episodes.
    pub min_slack_ticks: Option<i64>,
    /// The same in milliseconds (0 without episodes).
    pub min_slack_millis: i64,
    /// The worst miss, if any.
    pub worst_miss: Option<WorstMiss>,
    /// Regions evicted while the player needed them (must be 0).
    pub needed_evictions: u32,
    /// Reads started on the drive.
    pub reads: u32,
    /// Reads that needed a seek.
    pub seeks: u32,
    /// Sectors read.
    pub sectors_read: u64,
    /// Sectors of regions evicted before the player ever needed them.
    pub wasted_sectors: u64,
    /// Reads cancelled for a demand.
    pub cancelled: u32,
    /// Reads that failed.
    pub failed_reads: u32,
    /// Regions evicted.
    pub evictions: u32,
    /// Microseconds the drive was busy.
    pub drive_busy_micros: u64,
    /// Microseconds simulated.
    pub run_micros: u64,
    /// Most pages in use at once.
    pub peak_pages: u32,
    /// Worst free-space fragmentation seen, in permille.
    pub max_fragmentation_permille: u32,
    /// Requests that found no room (summed over steps).
    pub protected_full: u32,
    /// Pages moved by compaction.
    pub pages_moved: u32,
    /// Most bookkeeping work units in one step.
    pub max_step_work: u32,
    /// Work units over the run.
    pub total_work: u64,
    /// A fingerprint of every admission, eviction and request, for
    /// determinism checks.
    pub checksum: u32,
}

impl SimReport {
    /// Share of the run the drive was busy, in permille.
    pub fn drive_utilization_permille(&self) -> u32 {
        (self.drive_busy_micros * 1000)
            .checked_div(self.run_micros)
            .unwrap_or(0) as u32
    }
}

struct Queued {
    request: ReadRequest,
    submitted_us: u64,
    order: u32,
}

struct Current {
    request: ReadRequest,
    start_us: u64,
    finish_us: u64,
    outcome: ReadOutcome,
}

struct Transport {
    drive: DriveModel,
    queue: Vec<Queued>,
    current: Option<Current>,
    free_at_us: u64,
    outcomes: Vec<(RequestId, ResourceKey, ReadOutcome)>,
    fail_every: u32,
    submissions: u32,
    reads: u32,
    seeks: u32,
    sectors: u64,
    busy_us: u64,
    failed: u32,
    cancelled: u32,
}

impl Transport {
    fn advance(&mut self, now_us: u64) {
        loop {
            if self.current.is_none() {
                let next = self
                    .queue
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, q)| (q.request.class, q.request.deadline_ticks, q.order))
                    .map(|(index, _)| index);
                let Some(index) = next else { return };
                let start = self.free_at_us.max(self.queue[index].submitted_us);
                if start > now_us {
                    return;
                }
                let queued = self.queue.remove(index);
                let (micros, seeked) = self.drive.read(queued.request.lba, queued.request.sectors);
                self.reads += 1;
                self.seeks += seeked as u32;
                self.sectors += queued.request.sectors as u64;
                let fails = self.fail_every != 0 && self.reads.is_multiple_of(self.fail_every);
                self.current = Some(Current {
                    request: queued.request,
                    start_us: start,
                    finish_us: start + micros,
                    outcome: if fails {
                        ReadOutcome::Failed
                    } else {
                        ReadOutcome::Done
                    },
                });
            }
            let current = self.current.as_ref().unwrap();
            if current.finish_us > now_us {
                return;
            }
            let current = self.current.take().unwrap();
            self.busy_us += current.finish_us - current.start_us;
            self.free_at_us = current.finish_us;
            match current.outcome {
                ReadOutcome::Failed => self.failed += 1,
                ReadOutcome::Cancelled => self.cancelled += 1,
                ReadOutcome::Done => {}
            }
            self.outcomes
                .push((current.request.id, current.request.key, current.outcome));
        }
    }
}

struct SimHost<'a> {
    graph: &'a RegionGraph,
    distances: &'a [u32],
    transport: &'a mut Transport,
    now_us: u64,
}

impl Host for SimHost<'_> {
    fn extent(&self, key: ResourceKey) -> Option<Extent> {
        let id = key.index();
        if id as usize >= self.graph.region_count() {
            return None;
        }
        let region = self.graph.region(id);
        Some(Extent {
            lba: region.lba,
            sectors: region.sectors,
            pages: region.sectors,
            bytes: region.sectors * PAGE_BYTES as u32,
        })
    }

    fn walk_distance(&self, key: ResourceKey) -> u32 {
        self.distances
            .get(key.index() as usize)
            .copied()
            .unwrap_or(u32::MAX)
    }

    fn submit(&mut self, request: &ReadRequest) -> Submit {
        self.transport.submissions += 1;
        let order = self.transport.submissions;
        self.transport.queue.push(Queued {
            request: *request,
            submitted_us: self.now_us,
            order,
        });
        Submit::Accepted
    }

    fn cancel(&mut self, request: RequestId) {
        let transport = &mut *self.transport;
        if let Some(index) = transport.queue.iter().position(|q| q.request.id == request) {
            let queued = transport.queue.remove(index);
            transport.cancelled += 1;
            transport
                .outcomes
                .push((request, queued.request.key, ReadOutcome::Cancelled));
        } else if let Some(current) = transport.current.as_mut() {
            if current.request.id == request {
                current.finish_us = self.now_us + transport.drive.abort_micros() as u64;
                current.outcome = ReadOutcome::Cancelled;
                transport.drive.lose_head();
            }
        }
    }

    fn promote(&mut self, request: RequestId, class: Class) {
        if let Some(queued) = self
            .transport
            .queue
            .iter_mut()
            .find(|q| q.request.id == request)
        {
            queued.request.class = class;
        }
    }
}

struct Fnv(u32);

impl Fnv {
    fn feed(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.0 = (self.0 ^ byte as u32).wrapping_mul(0x0100_0193);
        }
    }
}

struct Sim<'a> {
    graph: &'a RegionGraph,
    config: &'a SimConfig,
    engine: Box<Engine>,
    transport: Transport,
    distance_cache: Vec<Option<Vec<u32>>>,
    resident: Vec<bool>,
    admitted_at: Vec<u32>,
    used: Vec<bool>,
    in_need: Vec<bool>,
    need_tick: Vec<Option<u32>>,
    installing: Option<(ResourceKey, u32)>,
    report: SimReport,
    hash: Fnv,
    /// Absolute tick counter (warm-up included).
    now_tick: u32,
    /// Absolute tick at which the route begins (`u32::MAX` while warming up).
    origin: u32,
}

fn region_key(id: u32) -> ResourceKey {
    ResourceKey::new(Category::WORLD_REGION, id).expect("region id fits a key")
}

impl<'a> Sim<'a> {
    fn new(graph: &'a RegionGraph, config: &'a SimConfig) -> Self {
        assert!(
            graph.region_count() <= 256,
            "the simulator tracks 256 regions"
        );
        let mut engine_config = Config::<1>::new();
        engine_config.pools[0] = PoolConfig {
            capacity_pages: config.pool_pages,
            placement: config.placement,
        };
        engine_config.categories[0] = CategoryConfig {
            pool: 0,
            max_pages: config.pool_pages,
            max_entries: 256,
            movable: config.movable,
            installs: config.install_ticks > 0,
            keep_radius: config.keep_radius,
        };
        engine_config.max_in_flight = config.max_in_flight;
        engine_config.step_budget = config.step_budget;
        let regions = graph.region_count();
        let mut engine = Box::new(Engine::new(engine_config));
        for &region in &config.pinned {
            assert!(engine.pin(region_key(region), Class::Demand));
        }
        Self {
            graph,
            config,
            engine,
            transport: Transport {
                drive: config.drive.clone(),
                queue: Vec::new(),
                current: None,
                free_at_us: 0,
                outcomes: Vec::new(),
                fail_every: config.fail_every,
                submissions: 0,
                reads: 0,
                seeks: 0,
                sectors: 0,
                busy_us: 0,
                failed: 0,
                cancelled: 0,
            },
            distance_cache: vec![None; regions],
            resident: vec![false; regions],
            admitted_at: vec![0; regions],
            used: vec![false; regions],
            in_need: vec![false; regions],
            need_tick: vec![None; regions],
            installing: None,
            report: SimReport::default(),
            hash: Fnv(0x811C_9DC5),
            now_tick: 0,
            origin: u32::MAX,
        }
    }

    fn counting(&self) -> bool {
        self.now_tick >= self.origin
    }

    fn note_resident(&mut self, region: u32) {
        let now = self.engine.is_resident(region_key(region));
        let index = region as usize;
        if now && !self.resident[index] {
            self.admitted_at[index] = self.now_tick;
            self.used[index] = false;
            self.hash.feed(self.now_tick << 8 | 1);
            self.hash.feed(region);
        }
        self.resident[index] = now;
    }

    /// One simulated tick with the player in `player`.
    fn tick(&mut self, player: u32) {
        let now_us = self.now_tick as u64 * self.config.tick_micros as u64;
        self.transport.advance(now_us);
        let outcomes = core::mem::take(&mut self.transport.outcomes);
        for (id, key, outcome) in outcomes {
            self.engine.complete_read(id, outcome);
            self.note_resident(key.index());
        }
        // Install work, one payload at a time.
        if self.config.install_ticks > 0 {
            if let Some((key, done)) = self.installing {
                if self.now_tick >= done {
                    self.engine.finish_install(key);
                    self.note_resident(key.index());
                    self.installing = None;
                }
            }
            if self.installing.is_none() {
                if let Some(key) = self.engine.next_install(None) {
                    self.engine.begin_install(key);
                    self.installing = Some((key, self.now_tick + self.config.install_ticks));
                }
            }
        }

        // What the player wants this tick.
        if self.distance_cache[player as usize].is_none() {
            self.distance_cache[player as usize] = Some(self.graph.distances_from(player));
        }
        let distances = self.distance_cache[player as usize].take().unwrap();
        let needs = self.graph.needs(player);
        let mut wanted = Vec::new();
        let mut needed = vec![false; self.graph.region_count()];
        for &region in needs {
            needed[region as usize] = true;
            wanted.push(Wanted::new(
                region_key(region),
                Class::Demand,
                distances[region as usize],
            ));
        }
        for (region, &distance) in distances.iter().enumerate() {
            if !needed[region] && distance <= self.config.lead_radius {
                let deadline = (distance as u64 * TICKS_PER_SECOND as u64
                    / self.config.speed_units_per_second.max(1) as u64)
                    as u32;
                wanted.push(
                    Wanted::new(region_key(region as u32), Class::Lead, distance)
                        .with_deadline_ticks(deadline),
                );
            }
        }

        let mut evictions: Vec<Eviction> = Vec::with_capacity(16);
        {
            let mut scratch = [Eviction::NONE; 16];
            let mut host = SimHost {
                graph: self.graph,
                distances: &distances,
                transport: &mut self.transport,
                now_us,
            };
            let report = self.engine.step(&wanted, &mut host, &mut scratch);
            for eviction in &scratch[..report.eviction_count] {
                evictions.push(*eviction);
            }
            if self.counting() {
                let stats = report.stats;
                self.report.protected_full += stats.protected_full;
                self.report.pages_moved += stats.pages_moved;
                self.report.max_step_work = self.report.max_step_work.max(stats.work_used);
                self.report.total_work += stats.work_used as u64;
            }
        }
        self.distance_cache[player as usize] = Some(distances);

        for eviction in &evictions {
            let region = eviction.key.index();
            if self.counting() {
                self.report.evictions += 1;
                if needed[region as usize] {
                    self.report.needed_evictions += 1;
                }
                if !self.used[region as usize] {
                    self.report.wasted_sectors += self.graph.region(region).sectors as u64;
                }
            }
            self.hash.feed(self.now_tick << 8 | 2);
            self.hash.feed(region);
            self.note_resident(region);
        }

        if self.counting() {
            self.account_needs(&needed);
            let pool = self.engine.pool(0).unwrap();
            self.report.peak_pages = self.report.peak_pages.max(pool.used_pages());
            self.report.max_fragmentation_permille = self
                .report
                .max_fragmentation_permille
                .max(pool.fragmentation().permille());
        }
        self.now_tick += 1;
    }

    fn account_needs(&mut self, needed: &[bool]) {
        let route_tick = self.now_tick - self.origin;
        let mut stalled = false;
        for (region, &is_needed) in needed.iter().enumerate() {
            let resident = self.resident[region];
            if is_needed {
                if resident {
                    self.used[region] = true;
                } else {
                    self.report.miss_ticks += 1;
                    stalled = true;
                }
                if !self.in_need[region] {
                    self.report.episodes += 1;
                    if resident {
                        let slack = self.now_tick as i64 - self.admitted_at[region] as i64;
                        self.record_slack(region as u32, route_tick, slack);
                    } else {
                        self.need_tick[region] = Some(route_tick);
                    }
                } else if resident {
                    if let Some(since) = self.need_tick[region].take() {
                        let slack = -(route_tick as i64 - since as i64);
                        self.record_slack(region as u32, since, slack);
                    }
                }
            } else if let Some(since) = self.need_tick[region].take() {
                // Left the need set without ever being resident.
                let slack = -(route_tick as i64 - since as i64);
                self.record_slack(region as u32, since, slack);
            }
            self.in_need[region] = is_needed;
        }
        if stalled {
            self.report.stall_ticks += 1;
        }
    }

    fn record_slack(&mut self, region: u32, need_tick: u32, slack: i64) {
        if slack < 0 {
            self.report.misses += 1;
            if self
                .report
                .worst_miss
                .is_none_or(|worst| slack < worst.slack_ticks)
            {
                self.report.worst_miss = Some(WorstMiss {
                    region,
                    need_tick,
                    slack_ticks: slack,
                });
            }
        }
        if self.report.min_slack_ticks.is_none_or(|min| slack < min) {
            self.report.min_slack_ticks = Some(slack);
        }
    }
}

/// Replay `route` over `graph` against the real residency engine and the
/// drive model, and report deadline slack and misses.
///
/// Each tick the player's region fixes the wanted list: the regions the graph
/// says it needs at demand class, every other region within the lead radius at
/// lead class with a deadline of its walk distance at the assumed speed. The
/// engine steps once per tick; the drive model decides when each read lands.
pub fn simulate(graph: &RegionGraph, route: &Route, config: &SimConfig) -> SimReport {
    let mut sim = Sim::new(graph, config);
    let Some(start) = route
        .region_at(0)
        .or_else(|| route.samples().first().map(|s| s.region))
    else {
        return SimReport::default();
    };
    if config.warm_start {
        let limit = 10 * 60 * TICKS_PER_SECOND;
        while sim.now_tick < limit {
            sim.tick(start);
            if graph.needs(start).iter().all(|&r| sim.resident[r as usize]) {
                break;
            }
        }
    }
    sim.origin = sim.now_tick;
    let before = (
        sim.transport.reads,
        sim.transport.seeks,
        sim.transport.sectors,
        sim.transport.busy_us,
        sim.transport.failed,
        sim.transport.cancelled,
    );
    for route_tick in 0..route.end_tick() {
        let player = route.region_at(route_tick).unwrap_or(start);
        sim.tick(player);
    }
    // Needs still unresolved at the end are misses.
    for region in 0..graph.region_count() {
        if let Some(since) = sim.need_tick[region].take() {
            let slack = -(route.end_tick() as i64 - since as i64);
            sim.record_slack(region as u32, since, slack);
        }
    }
    let mut report = sim.report.clone();
    report.ticks = route.end_tick();
    report.reads = sim.transport.reads - before.0;
    report.seeks = sim.transport.seeks - before.1;
    report.sectors_read = sim.transport.sectors - before.2;
    report.drive_busy_micros = sim.transport.busy_us - before.3;
    report.failed_reads = sim.transport.failed - before.4;
    report.cancelled = sim.transport.cancelled - before.5;
    report.run_micros = route.end_tick() as u64 * config.tick_micros as u64;
    report.min_slack_millis = report
        .min_slack_ticks
        .map_or(0, |ticks| ticks * config.tick_micros as i64 / 1000);
    report.checksum = sim.hash.0;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::Variance;
    use crate::world::RegionGraph;

    fn chain(count: u32, sectors: u32, length: u32) -> (RegionGraph, Route) {
        let mut graph = RegionGraph::chain(count, sectors, length);
        graph.layout_in_order(1_000, 0);
        graph.needs_with_neighbors();
        let path: Vec<u32> = (0..count).collect();
        let route = Route::walk(&graph, &path, 375);
        (graph, route)
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn a_sprint_down_a_chain_of_sixteen_sector_regions_has_slack() {
        let (graph, route) = chain(12, 16, 400);
        let report = simulate(&graph, &route, &SimConfig::new(16 * 8, 900));
        assert_eq!(report.misses, 0, "{report:?}");
        assert_eq!(report.stall_ticks, 0);
        assert_eq!(report.needed_evictions, 0);
        assert!(report.min_slack_millis > 0);
        // Contiguous regions chain without a seek: at most the first read
        // seeks.
        assert!(report.seeks <= 1, "{report:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn a_lead_radius_shorter_than_the_need_closure_is_caught() {
        // Entering region n needs region n + 1, two edges (800 units) ahead of
        // region n - 1; a lead radius of 700 asks for it one region too late.
        let (graph, route) = chain(24, 16, 400);
        let report = simulate(&graph, &route, &SimConfig::new(16 * 6, 700));
        assert!(report.misses > 20, "{report:?}");
        // The late read still costs one region of transfer (104 ms), not more.
        assert!(report.min_slack_millis > -300, "{report:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn regions_too_big_for_the_drive_miss_and_the_report_says_which() {
        // 64 sectors take 415 ms to read; one is needed every 107 ms.
        let (graph, route) = chain(12, 64, 100);
        let mut config = SimConfig::new(64 * 6, 300);
        config.speed_units_per_second = 375;
        let report = simulate(&graph, &route, &config);
        assert!(report.misses > 0, "{report:?}");
        assert!(report.stall_ticks > 0);
        let worst = report.worst_miss.unwrap();
        assert!(worst.slack_ticks < 0);
        assert!(report.min_slack_millis < 0);
        assert!(report.drive_utilization_permille() > 800);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn the_same_inputs_give_the_same_report_and_seeded_variance_changes_it() {
        let (graph, route) = chain(10, 16, 400);
        let mut config = SimConfig::new(16 * 8, 900);
        let a = simulate(&graph, &route, &config);
        let b = simulate(&graph, &route, &config);
        assert_eq!(a, b);
        config.drive = DriveModel::double_speed().with_variance(Variance::Seeded(3));
        // A scattered layout makes every read seek, so variance shows.
        let (mut scattered, route2) = chain(10, 16, 400);
        for region in 0..10 {
            scattered.place(region, 1_000 + (region * 7919) % 10_000);
        }
        let nominal = simulate(&scattered, &route2, &SimConfig::new(16 * 8, 900));
        let varied = simulate(&scattered, &route2, &config);
        assert_ne!(nominal.checksum, varied.checksum);
        assert!(varied.drive_busy_micros >= nominal.drive_busy_micros);
        let again = simulate(&scattered, &route2, &config);
        assert_eq!(varied, again);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn the_pessimistic_corner_is_slower_than_the_nominal_one() {
        let (mut graph, route) = chain(10, 16, 400);
        for region in 0..10 {
            graph.place(region, 1_000 + ((region * 4_001) % 20_000));
        }
        let nominal = simulate(&graph, &route, &SimConfig::new(16 * 8, 900));
        let mut config = SimConfig::new(16 * 8, 900);
        config.drive = DriveModel::double_speed().with_variance(Variance::Pessimistic);
        let pessimistic = simulate(&graph, &route, &config);
        assert!(pessimistic.drive_busy_micros > nominal.drive_busy_micros);
        assert!(pessimistic.min_slack_ticks <= nominal.min_slack_ticks);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn backtracking_inside_the_keep_radius_does_not_read_again() {
        let (graph, _) = chain(6, 16, 400);
        let path = [0, 1, 2, 3, 2, 1, 2, 3];
        let route = Route::walk(&graph, &path, 375);
        let config = SimConfig::new(16 * 8, 900);
        let report = simulate(&graph, &route, &config);
        assert_eq!(report.misses, 0);
        let straight = simulate(&graph, &Route::walk(&graph, &[0, 1, 2, 3], 375), &config);
        assert_eq!(report.reads, straight.reads, "no region is read twice");
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn a_pool_smaller_than_the_world_evicts_behind_the_player_and_never_what_is_needed() {
        let (graph, route) = chain(24, 16, 400);
        let report = simulate(&graph, &route, &SimConfig::new(16 * 6, 900));
        assert_eq!(report.misses, 0, "{report:?}");
        assert_eq!(report.needed_evictions, 0);
        assert!(report.evictions >= 10, "{report:?}");
        assert!(report.peak_pages <= 16 * 6);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn failing_reads_are_retried_and_the_run_still_completes() {
        let (graph, route) = chain(10, 16, 400);
        let mut config = SimConfig::new(16 * 8, 900);
        config.fail_every = 4;
        let report = simulate(&graph, &route, &config);
        assert!(report.failed_reads > 0, "{report:?}");
        // The retry backoff (16 windows = 267 ms) costs slack but nothing is
        // abandoned: the route ends with its needs resident.
        let clean = simulate(&graph, &route, &SimConfig::new(16 * 8, 900));
        assert!(report.reads > clean.reads, "failed reads were read again");
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn install_time_eats_slack() {
        let (graph, route) = chain(12, 16, 400);
        let free = simulate(&graph, &route, &SimConfig::new(16 * 8, 900));
        let mut config = SimConfig::new(16 * 8, 900);
        config.install_ticks = 6;
        let slow = simulate(&graph, &route, &config);
        assert!(slow.min_slack_ticks <= free.min_slack_ticks);
        assert_eq!(slow.needed_evictions, 0);
    }

    #[test]
    fn an_empty_route_reports_nothing() {
        let (graph, _) = chain(3, 4, 100);
        let report = simulate(&graph, &Route::default(), &SimConfig::new(32, 300));
        assert_eq!(report, SimReport::default());
    }
}
