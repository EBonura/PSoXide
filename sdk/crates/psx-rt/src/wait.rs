//! Bounded spin waits shared by the VBlank and present-queue waits.

/// Reads of a condition a wait makes before it gives up when nothing else
/// bounds it. A display period is under 700,000 CPU cycles and a read takes
/// at least one, so this is more than five periods even at that floor; a wait
/// that is going to end is over after one.
pub const SPIN_LIMIT: u32 = 4_000_000;

/// Spin while `pending()` holds. Once `stalled()` reports true, call
/// `release()` on every pass, so the caller can recover what it waits on
/// (the present queue stops a wedged walk). Gives up after `spin_limit`
/// passes. Returns `true` when `pending()` cleared and `false` when the
/// limit ended the wait.
///
/// `stalled` usually counts VBlank edges, and the count only moves while
/// psx-rt's handler runs, so the spin limit is the bound that holds without
/// it (interrupts masked, or the counter never installed).
pub(crate) fn wait_while(
    mut pending: impl FnMut() -> bool,
    mut stalled: impl FnMut() -> bool,
    mut release: impl FnMut(),
    spin_limit: u32,
) -> bool {
    let mut spins = 0u32;
    while pending() {
        if stalled() {
            release();
        }
        spins += 1;
        if spins >= spin_limit {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn a_wait_whose_condition_never_clears_gives_up() {
        // The case with no handler: nothing ever changes. This used to spin forever.
        let reads = Cell::new(0u32);
        let done = wait_while(
            || {
                reads.set(reads.get() + 1);
                true
            },
            || false,
            || {},
            1000,
        );
        assert!(!done);
        assert_eq!(reads.get(), 1000);
    }

    #[test]
    fn a_wait_that_clears_reports_it_and_stops_reading() {
        let reads = Cell::new(0u32);
        let done = wait_while(
            || {
                reads.set(reads.get() + 1);
                reads.get() < 5
            },
            || false,
            || {},
            1000,
        );
        assert!(done);
        assert_eq!(reads.get(), 5);
    }

    #[test]
    fn a_stalled_wait_releases_on_every_pass_until_it_clears() {
        let polls = Cell::new(0u32);
        let releases = Cell::new(0u32);
        let done = wait_while(
            || {
                polls.set(polls.get() + 1);
                releases.get() < 3
            },
            || polls.get() > 2,
            || releases.set(releases.get() + 1),
            1000,
        );
        assert!(done);
        assert_eq!(releases.get(), 3);
        assert_eq!(
            polls.get(),
            6,
            "released from poll 3 on, cleared after 3 releases"
        );
    }
}
