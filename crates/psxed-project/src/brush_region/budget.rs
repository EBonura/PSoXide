//! What a world asks of the RAM pool, and what the RAM can give.
//!
//! The cook gate asks that the union of the viewer's requirement, the lead
//! ring and the home pin fits the pool. That union is a number of regions
//! (the layout decides how many), so a world needs a pool of
//! `K * region + skeleton`, and the RAM either has it or the arena must give
//! some back. [`PoolPlan`] states that arithmetic once.
//!
//! `K` follows from the door degree `d` (no region sees more than its `d`
//! door neighbours):
//!
//! * the viewer ball (radius 165) can touch four cells at a corner, each
//!   needing itself and its neighbours: `4 (1 + d)`,
//! * the lead ring adds the requirement of the neighbour being approached:
//!   at most `d` regions the ball does not hold,
//! * the home pin holds the start region and its neighbours: `1 + d`.
//!
//! `K = 6 d + 5`: 23 at `d = 3`. [D from the pool gate in `gate.rs`; the
//! stress world measures 24 at its worst position]

use super::{skeleton_bytes, RamBudget};

/// Regions co-resident in the worst Need, lead and home-pin union when no
/// region sees more than `max_door_degree` neighbours. [D]
pub const fn union_regions(max_door_degree: u32) -> u32 {
    6 * max_door_degree + 5
}

/// The pool, its skeleton and the union, with the arithmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolPlan {
    pub ram: RamBudget,
    /// Regions the skeleton was sized for.
    pub regions: usize,
    pub max_door_degree: u32,
    pub union_regions: u32,
    /// Resident skeleton for `regions` regions, each seeing at most
    /// `1 + max_door_degree` regions. [D from wire sizes]
    pub skeleton_bytes: u64,
    pub pool_bytes: u32,
    /// What is left for region pages.
    pub pool_available: u64,
}

impl PoolPlan {
    pub fn derive(ram: &RamBudget, regions: usize, max_door_degree: u32) -> Self {
        let skeleton = skeleton_bytes(regions, regions * (1 + max_door_degree as usize));
        let pool = ram.world_pool_bytes();
        // The guest bakes the top container into `.data` and copies it onto
        // the heap, so the skeleton is paid twice. [M, 259 regions: container
        // 24,032 B against 26,140 B estimated for 256]
        Self {
            ram: *ram,
            regions,
            max_door_degree,
            union_regions: union_regions(max_door_degree),
            skeleton_bytes: skeleton,
            pool_bytes: pool,
            pool_available: u64::from(pool).saturating_sub(2 * skeleton),
        }
    }

    /// The largest region a union of `K` of them fits in. [D]
    pub fn region_bytes_that_fit(&self) -> u64 {
        self.pool_available / u64::from(self.union_regions)
    }

    /// The pool a world of regions up to `region_bytes` needs. [D]
    pub fn required_pool(&self, region_bytes: u32) -> u64 {
        u64::from(self.union_regions) * u64::from(region_bytes) + 2 * self.skeleton_bytes
    }

    /// What the arena must give back for `region_bytes` regions to fit. [D]
    pub fn reclaim_needed(&self, region_bytes: u32) -> u64 {
        self.required_pool(region_bytes)
            .saturating_sub(u64::from(self.pool_bytes))
    }

    /// The derivation as report lines.
    pub fn describe(&self, region_bytes: u32) -> String {
        format!(
            "pool {} B = {}\nskeleton for {} regions: {} B [D]; available for pages {} B\nunion of {} regions (door degree {}): fits regions of {} B; regions of {} B need a pool of {} B, {} B more than the RAM gives",
            self.pool_bytes,
            self.ram.describe(),
            self.regions,
            self.skeleton_bytes,
            self.pool_available,
            self.union_regions,
            self.max_door_degree,
            self.region_bytes_that_fit(),
            region_bytes,
            self.required_pool(region_bytes),
            self.reclaim_needed(region_bytes)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush_region::{P_WORLD_ESTIMATE_BYTES, RAM_BUDGET};

    #[test]
    fn the_pool_is_the_measured_arithmetic() {
        // 331,676 - 4,281 + 93,808 - 35,436 - 16,384
        assert_eq!(P_WORLD_ESTIMATE_BYTES, 369_383);
        assert_eq!(RAM_BUDGET.world_pool_bytes(), 369_383);
        let more = RamBudget {
            arena_reclaim_bytes: 100_000,
            ..RAM_BUDGET
        };
        assert_eq!(more.world_pool_bytes(), 469_383);
    }

    #[test]
    fn the_pool_requirement_grows_with_the_world_and_the_region_size() {
        let small = PoolPlan::derive(&RAM_BUDGET, 100, 3);
        let large = PoolPlan::derive(&RAM_BUDGET, 400, 3);
        assert!(large.skeleton_bytes > small.skeleton_bytes);
        assert!(large.region_bytes_that_fit() < small.region_bytes_that_fit());
        assert_eq!(union_regions(3), 23);
        assert_eq!(union_regions(2), 17);
        // 23 regions of 32 KB (the hard cap) do not fit the RAM of today; the
        // shortfall is exactly what the arena would have to give back.
        let need = small.reclaim_needed(32_768);
        assert!(need > 0);
        assert_eq!(
            need,
            small.required_pool(32_768) - u64::from(small.pool_bytes)
        );
        let roomy = PoolPlan::derive(
            &RamBudget {
                arena_reclaim_bytes: need as u32,
                ..RAM_BUDGET
            },
            100,
            3,
        );
        assert_eq!(roomy.reclaim_needed(32_768), 0);
        assert!(roomy.region_bytes_that_fit() >= 32_768);
    }
}
