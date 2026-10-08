//! Budgets and policy knobs.

use crate::backoff::Backoff;
use crate::key::Class;
use crate::pool::Placement;

/// One page pool: its size and how new runs are placed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PoolConfig {
    /// Pages in the pool (0 disables it).
    pub capacity_pages: u32,
    /// Gap choice for new runs.
    pub placement: Placement,
}

impl PoolConfig {
    /// A pool that holds nothing.
    pub const UNUSED: PoolConfig = PoolConfig {
        capacity_pages: 0,
        placement: Placement::FirstFit,
    };
}

/// Budget and behaviour of one category.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CategoryConfig {
    /// The pool (see [`Config::pools`]) this category allocates from. Several
    /// categories may share one.
    pub pool: u8,
    /// Most pages this category may hold, across all its entries.
    pub max_pages: u32,
    /// Most entries (resident or in flight) this category may hold. 0 disables
    /// the category.
    pub max_entries: u16,
    /// Whether compaction may move this category's unpinned entries. A
    /// category whose bytes are addressed by hardware or by long-lived raw
    /// pointers (VRAM, SPU RAM, archetype packs) is not movable.
    pub movable: bool,
    /// Whether a landed payload needs an install step (verify, relocate,
    /// upload) before it is resident. When false a finished read is resident.
    pub installs: bool,
    /// Unwanted entries farther than this are free victims; nearer ones are
    /// in the hysteresis band and go last. Set it to the lead radius plus the
    /// hysteresis, see [`CategoryConfig::keep_radius_for`].
    pub keep_radius: u32,
}

impl CategoryConfig {
    /// A disabled category.
    pub const UNUSED: CategoryConfig = CategoryConfig {
        pool: 0,
        max_pages: 0,
        max_entries: 0,
        movable: false,
        installs: false,
        keep_radius: 0,
    };

    /// The eviction keep radius the design prescribes: the lead radius plus
    /// a hysteresis of `max(region_width, lead_radius / 4)`, so a player
    /// standing in a doorway cannot make two regions thrash.
    pub const fn keep_radius_for(lead_radius: u32, region_width: u32) -> u32 {
        let quarter = lead_radius / 4;
        let hysteresis = if region_width > quarter {
            region_width
        } else {
            quarter
        };
        lead_radius.saturating_add(hysteresis)
    }
}

/// Everything configurable about a [`crate::Residency`]. `CATEGORIES` is both
/// the number of category numbers in use and the number of pools.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Config<const CATEGORIES: usize> {
    /// Page pools, indexed by pool number.
    pub pools: [PoolConfig; CATEGORIES],
    /// Category budgets, indexed by [`crate::Category::number`]. A key whose
    /// category number is out of range is ignored (and counted).
    pub categories: [CategoryConfig; CATEGORIES],
    /// Requests the transport may hold at once (the design's queue depth
    /// is 2). Cancelling requests do not count.
    pub max_in_flight: u8,
    /// Bookkeeping budget per [`crate::Residency::step`], in work units (see
    /// the crate docs). `u32::MAX` means unlimited.
    pub step_budget: u32,
    /// Retry schedule for failed reads and installs.
    pub backoff: Backoff,
    /// A wanted entry of the same class as a requester is displaced only when
    /// it is farther than the requester by more than this (distance units),
    /// so two entries of nearly equal distance cannot trade places each frame.
    pub displace_margin: u32,
    /// Per requester class, the highest victim tier it may evict: 0 is an
    /// unwanted entry beyond the keep radius, 1 an unwanted entry inside the
    /// hysteresis band, 2 a wanted entry of lower priority than the
    /// requester. Indexed by [`Class::as_index`].
    pub max_victim_tier: [u8; Class::COUNT],
    /// Per class, whether an entry wanted at that class this frame can never
    /// be evicted this frame. Indexed by [`Class::as_index`].
    pub protect_wanted: [bool; Class::COUNT],
    /// Let a [`Class::Demand`] miss cancel the least urgent in-flight request
    /// when the transport is at depth.
    pub cancel_for_demand: bool,
}

impl<const CATEGORIES: usize> Config<CATEGORIES> {
    /// All pools and categories disabled, the design's default knobs: queue
    /// depth 2, unlimited step budget, the grid backoff, tiers
    /// `[2, 2, 1, 0, 0]`, demand and combat fill protected, cancel for demand.
    pub const fn new() -> Self {
        Self {
            pools: [PoolConfig::UNUSED; CATEGORIES],
            categories: [CategoryConfig::UNUSED; CATEGORIES],
            max_in_flight: 2,
            step_budget: u32::MAX,
            backoff: Backoff::GRID_DEFAULT,
            displace_margin: 0,
            max_victim_tier: [2, 2, 1, 0, 0],
            protect_wanted: [true, true, false, false, false],
            cancel_for_demand: true,
        }
    }
}

impl<const CATEGORIES: usize> Default for Config<CATEGORIES> {
    fn default() -> Self {
        Self::new()
    }
}
