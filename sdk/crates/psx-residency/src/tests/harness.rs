//! A scripted host and small builders shared by the engine tests.

use crate::*;
use std::collections::BTreeMap;
use std::vec::Vec;

pub const REGION: Category = Category::WORLD_REGION;
pub const COLLISION: Category = Category::COLLISION_REGION;
pub const TEXTURE: Category = Category::TEXTURE_PAGE;

pub fn key(category: Category, index: u32) -> ResourceKey {
    ResourceKey::new(category, index).unwrap()
}

pub fn region(index: u32) -> ResourceKey {
    key(REGION, index)
}

pub type Engine = Residency<12, 8, 3>;

/// Pool 0: 16 RAM pages shared by regions (cap 16) and collision (cap 6).
/// Pool 1: 8 texture slots, not movable.
pub fn config() -> Config<3> {
    let mut config = Config::<3>::new();
    config.pools[0] = PoolConfig {
        capacity_pages: 16,
        placement: Placement::BestFit,
    };
    config.pools[1] = PoolConfig {
        capacity_pages: 8,
        placement: Placement::FirstFit,
    };
    config.categories[REGION.number() as usize] = CategoryConfig {
        pool: 0,
        max_pages: 16,
        max_entries: 8,
        movable: true,
        installs: false,
        keep_radius: 300,
    };
    config.categories[COLLISION.number() as usize] = CategoryConfig {
        pool: 0,
        max_pages: 6,
        max_entries: 4,
        movable: true,
        installs: true,
        keep_radius: 300,
    };
    config.categories[TEXTURE.number() as usize] = CategoryConfig {
        pool: 1,
        max_pages: 8,
        max_entries: 8,
        movable: false,
        installs: false,
        keep_radius: 300,
    };
    config
}

pub fn engine() -> Engine {
    Engine::new(config())
}

/// A host over a table of extents, with a scripted transport.
pub struct TestHost {
    /// Pages per key unless overridden.
    pub default_pages: u32,
    pub pages: BTreeMap<u32, u32>,
    pub distances: BTreeMap<u32, u32>,
    pub lbas: BTreeMap<u32, u32>,
    pub accept: bool,
    pub full_on_submit: bool,
    pub submitted: Vec<ReadRequest>,
    pub cancelled: Vec<RequestId>,
    pub promoted: Vec<(RequestId, Class)>,
    pub moved: Vec<(u8, u32, u32, u32)>,
}

impl TestHost {
    pub fn new(default_pages: u32) -> Self {
        Self {
            default_pages,
            pages: BTreeMap::new(),
            distances: BTreeMap::new(),
            lbas: BTreeMap::new(),
            accept: true,
            full_on_submit: false,
            submitted: Vec::new(),
            cancelled: Vec::new(),
            promoted: Vec::new(),
            moved: Vec::new(),
        }
    }

    pub fn last(&self) -> &ReadRequest {
        self.submitted.last().unwrap()
    }
}

impl Host for TestHost {
    fn extent(&self, key: ResourceKey) -> Option<Extent> {
        let pages = self
            .pages
            .get(&key.as_u32())
            .copied()
            .unwrap_or(self.default_pages);
        Some(Extent {
            lba: self
                .lbas
                .get(&key.as_u32())
                .copied()
                .unwrap_or(key.index() * 1000),
            sectors: pages,
            pages,
            bytes: pages * PAGE_BYTES as u32 - 5,
        })
    }

    fn walk_distance(&self, key: ResourceKey) -> u32 {
        self.distances.get(&key.as_u32()).copied().unwrap_or(10_000)
    }

    fn can_accept(&self) -> bool {
        self.accept
    }

    fn submit(&mut self, request: &ReadRequest) -> Submit {
        if self.full_on_submit {
            return Submit::Full;
        }
        self.submitted.push(*request);
        Submit::Accepted
    }

    fn cancel(&mut self, request: RequestId) {
        self.cancelled.push(request);
    }

    fn promote(&mut self, request: RequestId, class: Class) {
        self.promoted.push((request, class));
    }

    fn move_pages(&mut self, pool: u8, from_page: u32, to_page: u32, page_count: u32) {
        self.moved.push((pool, from_page, to_page, page_count));
    }
}

pub const NO_EVICTIONS: [Eviction; 0] = [];

pub fn blank_evictions<const N: usize>() -> [Eviction; N] {
    [Eviction::NONE; N]
}

/// Step and finish every issued read as done (a perfect instant transport).
pub fn step_and_land(
    engine: &mut Engine,
    wanted: &[Wanted],
    host: &mut TestHost,
) -> (StepReport, Vec<Eviction>) {
    let mut buffer = blank_evictions::<8>();
    let before = host.submitted.len();
    let report = engine.step(wanted, host, &mut buffer);
    let evicted = buffer[..report.eviction_count].to_vec();
    for request in host.submitted[before..].iter().copied() {
        assert!(engine.complete_read(request.id, ReadOutcome::Done));
    }
    (report, evicted)
}

pub fn demand(index: u32) -> Wanted {
    Wanted::new(region(index), Class::Demand, 0)
}

pub fn lead(index: u32, distance: u32) -> Wanted {
    Wanted::new(region(index), Class::Lead, distance)
}

/// A config that lets many reads run at once, for filling pools quickly.
pub fn wide_config() -> Config<3> {
    let mut config = config();
    config.max_in_flight = 8;
    config
}

/// Make `indices` resident as lead-class regions at the given distances.
pub fn fill_regions(
    engine: &mut Engine,
    host: &mut TestHost,
    regions: &[(u32, u32)],
) -> Vec<Eviction> {
    let wanted: Vec<Wanted> = regions
        .iter()
        .map(|&(index, distance)| lead(index, distance))
        .collect();
    let mut all = Vec::new();
    for _ in 0..regions.len() + 1 {
        let (_, evicted) = step_and_land(engine, &wanted, host);
        all.extend(evicted);
    }
    for &(index, _) in regions {
        assert!(engine.is_resident(region(index)), "region {index} resident");
    }
    all
}
