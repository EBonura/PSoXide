//! Residency scheduler policy pieces.
//!
//! Salvaged from the retired grid-world `RoomStreamScheduler`: the failure
//! backoff, the pinned-window flags that keep a protected prefix from being
//! evicted, the epoch and residency-generation pattern, and the per-window
//! telemetry counters. The room-specific job plumbing is gone; a scheduler
//! keys these by whatever dense index its payloads use.
//!
//! Every type is all-zero valid except where noted, so an owning static stays
//! in `.bss`.

/// First retry comes after 16 reconcile windows (~0.27s at 60Hz);
/// each consecutive failure doubles the hold up to 16 << 5 = 512
/// windows (~8.5s). A success resets the count, so transient CD
/// hiccups recover fast while a permanently bad chunk settles into
/// one retry every few seconds instead of one per frame.
pub const RETRY_BACKOFF_BASE_WINDOWS: u32 = 16;
/// Largest doubling applied to [`RETRY_BACKOFF_BASE_WINDOWS`].
pub const RETRY_BACKOFF_MAX_SHIFT: u32 = 5;

/// Hold, in reconcile windows, after `count` consecutive failures.
pub fn retry_backoff_windows(count: u8) -> u32 {
    let shift = (count.saturating_sub(1) as u32).min(RETRY_BACKOFF_MAX_SHIFT);
    RETRY_BACKOFF_BASE_WINDOWS << shift
}

/// Advance a reconcile epoch. Never returns zero, so zero stays "unset".
pub const fn next_epoch(epoch: u32) -> u32 {
    let next = epoch.wrapping_add(1);
    if next == 0 {
        1
    } else {
        next
    }
}

/// Per-key consecutive-failure counts and retry holds.
///
/// Consecutive failed loads per key saturate and reset on success. The
/// count drives the retry backoff so a permanently bad chunk (TOC mismatch,
/// unreadable sector) cannot churn the CD and the single job pipeline every
/// frame (streaming audit finding 2). A key is never abandoned: the hold
/// doubles per consecutive failure up to a cap, then retries keep coming at
/// the capped interval.
#[derive(Copy, Clone)]
pub struct FailureBackoff<const KEYS: usize> {
    counts: [u8; KEYS],
    /// Epoch until which a previously failed key is not rescheduled
    /// (exclusive, wrap-safe compare).
    hold_until: [u32; KEYS],
}

impl<const KEYS: usize> Default for FailureBackoff<KEYS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const KEYS: usize> FailureBackoff<KEYS> {
    /// No failures recorded. All-zero valid.
    pub const fn new() -> Self {
        Self {
            counts: [0; KEYS],
            hold_until: [0; KEYS],
        }
    }

    /// True while `key` is inside its failure-retry hold window.
    /// Wrap-safe: holds shorter than 2^31 epochs compare correctly
    /// across the u32 epoch wrap.
    pub fn hold_active(&self, key: usize, epoch: u32) -> bool {
        if key >= KEYS || self.counts[key] == 0 {
            return false;
        }
        self.hold_until[key].wrapping_sub(epoch) as i32 > 0
    }

    /// Record a successful load of `key`.
    pub fn note_success(&mut self, key: usize, epoch: u32) {
        if key < KEYS {
            self.counts[key] = 0;
            self.hold_until[key] = epoch;
        }
    }

    /// Record a failed load of `key` and start (or extend) its hold.
    pub fn note_failure(&mut self, key: usize, epoch: u32) {
        if key >= KEYS {
            return;
        }
        let count = self.counts[key].saturating_add(1);
        self.counts[key] = count;
        self.hold_until[key] = epoch.wrapping_add(retry_backoff_windows(count));
    }

    /// Consecutive failures recorded for `key`.
    pub fn failure_count(&self, key: usize) -> u8 {
        if key < KEYS {
            self.counts[key]
        } else {
            0
        }
    }
}

/// Keys declared part of the resident window. Pinned keys are never chosen
/// for eviction regardless of LRU age, so the residency owner can keep them
/// resident without re-requesting them. This is the primitive both policies
/// build on: full residency pins every key, a sliding window pins the
/// current region plus its near neighbours.
#[derive(Copy, Clone)]
pub struct PinFlags<const KEYS: usize> {
    pinned: [bool; KEYS],
}

impl<const KEYS: usize> Default for PinFlags<KEYS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const KEYS: usize> PinFlags<KEYS> {
    /// Nothing pinned. All-zero valid.
    pub const fn new() -> Self {
        Self {
            pinned: [false; KEYS],
        }
    }

    /// Replace the pinned set with `keys`. Keys no longer in the set become
    /// evictable again; out-of-range keys are ignored.
    pub fn set(&mut self, keys: &[usize]) {
        self.pinned = [false; KEYS];
        let mut i = 0usize;
        while i < keys.len() {
            if keys[i] < KEYS {
                self.pinned[keys[i]] = true;
            }
            i += 1;
        }
    }

    /// Whether `key` is currently pinned.
    pub fn is_pinned(&self, key: usize) -> bool {
        key < KEYS && self.pinned[key]
    }
}

/// Full-width identity for caches that depend on residency content. The
/// 64-bit telemetry masks cannot carry a whole key-to-slot map, this can.
///
/// Zero is "never advanced"; [`ResidencyGeneration::bump`] never lands on it.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct ResidencyGeneration(u32);

impl ResidencyGeneration {
    /// Generation before any residency change. All-zero valid.
    pub const fn zeroed() -> Self {
        Self(0)
    }

    /// Generation a freshly initialised scheduler starts at.
    pub const fn initial() -> Self {
        Self(1)
    }

    /// Current raw value.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Record a residency change.
    pub fn bump(&mut self) {
        self.0 = next_epoch(self.0);
    }
}

/// Telemetry counters for one reconcile window.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct WindowCounters {
    /// Wanted entries examined.
    pub requests: u16,
    /// Wanted entries that were not resident.
    pub misses: u16,
    /// Wanted entries past the protected prefix.
    pub prefetch_requests: u16,
    /// Resident entries evicted to make room.
    pub evictions: u16,
    /// Loads that completed with an error.
    pub failed_loads: u16,
    /// Loads queued or in flight.
    pub pending_loads: u16,
    /// Protected entries that found no slot.
    pub protected_full: u16,
}

impl WindowCounters {
    /// All counters zero.
    pub const fn new() -> Self {
        Self {
            requests: 0,
            misses: 0,
            prefetch_requests: 0,
            evictions: 0,
            failed_loads: 0,
            pending_loads: 0,
            protected_full: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(retry_backoff_windows(1), 16);
        assert_eq!(retry_backoff_windows(2), 32);
        assert_eq!(retry_backoff_windows(6), 512);
        assert_eq!(retry_backoff_windows(200), 512);
    }

    #[test]
    fn failure_hold_survives_epoch_wrap_and_success_clears_it() {
        let mut backoff = FailureBackoff::<4>::new();
        let epoch = u32::MAX - 3;
        backoff.note_failure(2, epoch);
        assert!(backoff.hold_active(2, epoch));
        // Past the u32 wrap but still inside the 16 window hold.
        assert!(backoff.hold_active(2, epoch.wrapping_add(10)));
        assert!(!backoff.hold_active(2, epoch.wrapping_add(16)));
        assert!(!backoff.hold_active(1, epoch));
        backoff.note_failure(2, epoch);
        assert_eq!(backoff.failure_count(2), 2);
        backoff.note_success(2, epoch);
        assert_eq!(backoff.failure_count(2), 0);
        assert!(!backoff.hold_active(2, epoch));
    }

    #[test]
    fn pinned_set_is_replaced_not_accumulated() {
        let mut pins = PinFlags::<4>::new();
        pins.set(&[0, 2, 99]);
        assert!(pins.is_pinned(0) && pins.is_pinned(2));
        assert!(!pins.is_pinned(99));
        pins.set(&[3]);
        assert!(!pins.is_pinned(0) && !pins.is_pinned(2) && pins.is_pinned(3));
    }

    #[test]
    fn generation_and_epoch_never_land_on_zero() {
        assert_eq!(next_epoch(u32::MAX), 1);
        let mut generation = ResidencyGeneration::zeroed();
        generation.bump();
        assert_eq!(generation.get(), 1);
        let mut wrapped = ResidencyGeneration(u32::MAX);
        wrapped.bump();
        assert_eq!(wrapped.get(), 1);
    }
}
