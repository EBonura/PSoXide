//! Retry backoff for failed reads.
//!
//! Lifted from the grid scheduler (`stream_retry_backoff_windows`): the first
//! retry waits `base_windows` scheduling windows, every further consecutive
//! failure doubles the wait up to `base_windows << max_shift`. A success
//! resets the count, so a transient drive hiccup recovers fast while a
//! permanently bad chunk settles into one retry every few seconds instead of
//! one per frame. A resource is never abandoned.

/// Backoff schedule, in scheduling windows (one window per
/// [`crate::Residency::step`] call).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// Wait after the first failure.
    pub base_windows: u32,
    /// The wait doubles per failure at most this many times.
    pub max_shift: u32,
}

impl Backoff {
    /// The grid scheduler's schedule: 16 windows, doubling up to 16 << 5 = 512
    /// (about 0.27 s to 8.5 s when a window is one 60 Hz frame).
    pub const GRID_DEFAULT: Backoff = Backoff {
        base_windows: 16,
        max_shift: 5,
    };

    /// Windows to wait after `failure_count` consecutive failures (0 when the
    /// count is 0).
    pub const fn windows(self, failure_count: u8) -> u32 {
        if failure_count == 0 {
            return 0;
        }
        let shift = failure_count as u32 - 1;
        let shift = if shift > self.max_shift {
            self.max_shift
        } else {
            shift
        };
        self.base_windows << shift
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::GRID_DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_per_failure_up_to_the_cap() {
        let backoff = Backoff::GRID_DEFAULT;
        assert_eq!(backoff.windows(0), 0);
        assert_eq!(backoff.windows(1), 16);
        assert_eq!(backoff.windows(2), 32);
        assert_eq!(backoff.windows(5), 256);
        assert_eq!(backoff.windows(6), 512);
        assert_eq!(backoff.windows(7), 512);
        assert_eq!(backoff.windows(255), 512);
    }
}
