//! Fragmentation under churn: how a pool behaves when variable-length runs
//! come and go.
//!
//! Measured on the host with the synthetic workload of the `churn_table` test
//! (256-page pool, runs of 2 to 40 pages uniformly, 4000 allocations per
//! seed with random victims freed until each fits, averaged over seeds 1 to 8;
//! this is a stress shape, not a game trace):
//!
//! | policy | blocked by fragmentation | mean free-space fragmentation | mean pool use |
//! | --- | --- | --- | --- |
//! | first-fit | 1846 of 4000 | 558 permille | 811 permille |
//! | best-fit | 1732 of 4000 | 520 permille | 823 permille |
//! | first-fit + compaction | 0 | 107 permille | 946 permille |
//! | best-fit + compaction | 0 | 103 permille | 946 permille |
//!
//! Placement policy matters little; compaction (about 450 000 pages moved per
//! 4000 allocations here) is what turns scattered free pages back into usable
//! ones. That is why the engine compacts movable categories before it evicts,
//! and why pinned and in-flight runs, which cannot move, are the cost.

use alloc::vec::Vec;
use psx_residency::{PagePool, Placement};

/// A churn experiment.
#[derive(Copy, Clone, Debug)]
pub struct ChurnConfig {
    /// Pool size in pages.
    pub pages: u32,
    /// Smallest and largest run, in pages (uniform).
    pub run_pages: (u32, u32),
    /// Operations to perform. Each allocates one run; when the pool is too
    /// full to place it, random live runs are freed until it fits.
    pub operations: u32,
    /// Placement policy.
    pub placement: Placement,
    /// Whether a failed placement first tries compaction (one run per step,
    /// as the engine does) before freeing runs.
    pub compact: bool,
    /// Generator seed.
    pub seed: u64,
}

/// What a churn run found.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ChurnReport {
    /// Placements attempted.
    pub allocations: u32,
    /// Placements that failed although enough pages were free in total, after
    /// compaction if it is on: the definition of fragmentation loss.
    pub fragmentation_failures: u32,
    /// Runs freed to make room (evictions).
    pub forced_frees: u32,
    /// Pages moved by compaction.
    pub pages_moved: u64,
    /// Mean free-space fragmentation after each operation, in permille.
    pub mean_fragmentation_permille: u32,
    /// Worst fragmentation seen, in permille.
    pub max_fragmentation_permille: u32,
    /// Mean share of the pool in use after each operation, in permille.
    pub mean_utilization_permille: u32,
}

/// Run the experiment. Deterministic for a given configuration.
pub fn churn_report(config: ChurnConfig) -> ChurnReport {
    let mut pool = PagePool::<128>::new(0, config.pages, config.placement);
    let mut live = Vec::new();
    let mut rng = config.seed | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as u32
    };
    let (low, high) = config.run_pages;
    let mut report = ChurnReport::default();
    let mut fragmentation_sum = 0u64;
    let mut utilization_sum = 0u64;
    for _ in 0..config.operations {
        let pages = low + next() % (high - low + 1);
        report.allocations += 1;
        let mut counted = false;
        loop {
            if pool.live_count() < 128 {
                if let Some((handle, _)) = pool.allocate(pages) {
                    live.push(handle);
                    break;
                }
            }
            if config.compact {
                if let Some(moved) = pool.compact_step(|_, _| {}) {
                    report.pages_moved += moved.from.page_count as u64;
                    continue;
                }
            }
            if !counted && pool.free_pages() >= pages {
                report.fragmentation_failures += 1;
                counted = true;
            }
            // Evict a random live run.
            let victim = next() as usize % live.len();
            let handle = live.swap_remove(victim);
            pool.free(handle);
            report.forced_frees += 1;
        }
        let shape = pool.fragmentation();
        fragmentation_sum += shape.permille() as u64;
        report.max_fragmentation_permille = report.max_fragmentation_permille.max(shape.permille());
        utilization_sum += pool.used_pages() as u64 * 1000 / config.pages as u64;
    }
    let operations = config.operations.max(1) as u64;
    report.mean_fragmentation_permille = (fragmentation_sum / operations) as u32;
    report.mean_utilization_permille = (utilization_sum / operations) as u32;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(placement: Placement, compact: bool, seed: u64) -> ChurnReport {
        churn_report(ChurnConfig {
            pages: 256,
            run_pages: (2, 40),
            operations: 4_000,
            placement,
            compact,
            seed,
        })
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn churn_is_deterministic() {
        assert_eq!(
            run(Placement::BestFit, false, 7),
            run(Placement::BestFit, false, 7)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn compaction_removes_fragmentation_failures() {
        for seed in 1..=5 {
            let without = run(Placement::FirstFit, false, seed);
            let with = run(Placement::FirstFit, true, seed);
            assert!(without.fragmentation_failures > 0, "{without:?}");
            // With compaction a placement fails only when the pool is truly
            // short of pages, never because they are scattered.
            assert_eq!(with.fragmentation_failures, 0, "{with:?}");
            assert!(with.mean_utilization_permille >= without.mean_utilization_permille);
            assert!(with.pages_moved > 0);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn best_fit_and_first_fit_both_keep_the_pool_busy() {
        for placement in [Placement::FirstFit, Placement::BestFit] {
            let report = run(placement, false, 3);
            assert!(report.mean_utilization_permille > 500, "{report:?}");
            assert!(report.max_fragmentation_permille <= 1000);
        }
    }

    /// Prints the fragmentation comparison quoted in the crate docs
    /// (`cargo test -p psx-residency --features sim -- --nocapture churn_table`).
    #[test]
    #[cfg_attr(miri, ignore)] // exhaustive sweep, runs natively
    fn churn_table() {
        for (name, placement, compact) in [
            ("first-fit", Placement::FirstFit, false),
            ("best-fit", Placement::BestFit, false),
            ("first-fit + compaction", Placement::FirstFit, true),
            ("best-fit + compaction", Placement::BestFit, true),
        ] {
            let mut total = ChurnReport::default();
            let seeds = 8u32;
            for seed in 1..=seeds as u64 {
                let report = run(placement, compact, seed);
                total.fragmentation_failures += report.fragmentation_failures;
                total.forced_frees += report.forced_frees;
                total.pages_moved += report.pages_moved;
                total.mean_fragmentation_permille += report.mean_fragmentation_permille;
                total.max_fragmentation_permille = total
                    .max_fragmentation_permille
                    .max(report.max_fragmentation_permille);
                total.mean_utilization_permille += report.mean_utilization_permille;
            }
            std::println!(
                "{name:24} frag-fail {:5}  forced-free {:6}  moved {:8}  mean-frag {:4}  max-frag {:4}  mean-util {:4}",
                total.fragmentation_failures / seeds,
                total.forced_frees / seeds,
                total.pages_moved / seeds as u64,
                total.mean_fragmentation_permille / seeds,
                total.max_fragmentation_permille,
                total.mean_utilization_permille / seeds,
            );
        }
    }
}
