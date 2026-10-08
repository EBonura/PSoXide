//! Fixed-slot residency policy: a handful of equally sized private arenas, a
//! wishlist, and a stored-then-decoded pipeline.
//!
//! Lifted from hk-psx `game/src/room_residency.rs` (a policy independent of
//! the CD transport and of GPU calls) and made generic over the slot count and
//! the wishlist length. The logic and its tests carry over unchanged; what
//! changed is naming (`loading` is [`SlotResidency::is_loading`], `stored` is
//! [`SlotResidency::find_stored`], `Residency` is [`SlotResidency`]) and
//! layout. Region ids are plain `usize`; [`EMPTY`] means none.
//!
//! A slot is empty, being read or decoded (`loading`), holding completed but
//! unverified bytes (`stored`, `len == 0`), or holding a verified room (`len`
//! non-zero). Only a decoder may turn stored bytes into a verified room, so
//! completed transport data can never be mistaken for one. The current slot,
//! the slot an upload is reading from (`protected`) and every loading slot are
//! never reused. The wishlist ranks speculative targets: a lower-priority
//! speculative read never displaces an earlier wishlist entry, and an urgent
//! read (the room needed now) may.
//!
//! Use [`crate::Residency`] when payloads differ in size, several pools or
//! categories are in play, or distance should drive eviction; use this when
//! the game keeps a few interchangeable slots.

/// Sentinel region id: none.
pub const EMPTY: usize = usize::MAX;

/// One arena slot.
#[derive(Copy, Clone)]
pub struct Slot {
    /// Region held (verified or stored), or [`EMPTY`].
    pub region: usize,
    /// Verified length in bytes; zero while the slot is empty or only stored.
    pub len: usize,
    used: u32,
}

impl Slot {
    const fn new() -> Self {
        Self {
            region: EMPTY,
            len: 0,
            used: 0,
        }
    }
}

/// Residency over `SLOTS` arenas with a wishlist of `WANTED` regions (hk-psx
/// uses 5 and 4: the current room's slot plus four others).
pub struct SlotResidency<const SLOTS: usize, const WANTED: usize> {
    /// The arenas.
    pub slots: [Slot; SLOTS],
    /// Index of the slot holding the room being shown.
    pub current: usize,
    protected: usize,
    loading: [usize; SLOTS],
    wanted: [usize; WANTED],
    clock: u32,
    decode_ready: usize,
}

impl<const SLOTS: usize, const WANTED: usize> Default for SlotResidency<SLOTS, WANTED> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize, const WANTED: usize> SlotResidency<SLOTS, WANTED> {
    /// All slots empty, nothing wanted.
    pub const fn new() -> Self {
        Self {
            slots: [Slot::new(); SLOTS],
            current: 0,
            protected: EMPTY,
            loading: [EMPTY; SLOTS],
            wanted: [EMPTY; WANTED],
            clock: 0,
            decode_ready: EMPTY,
        }
    }

    /// Slot holding the verified room `region`.
    pub fn find(&self, region: usize) -> Option<usize> {
        if region == EMPTY {
            None
        } else {
            self.slots
                .iter()
                .position(|s| s.region == region && s.len != 0)
        }
    }

    /// Slot holding completed but unverified bytes of `region`. A completed
    /// CD payload is not yet a room: `len == 0` is exclusively stored data;
    /// only a private decoder may turn it into a verified resident entry.
    pub fn find_stored(&self, region: usize) -> Option<usize> {
        if region == EMPTY {
            None
        } else {
            self.slots
                .iter()
                .position(|s| s.region == region && s.len == 0)
        }
    }

    fn refresh_decode(&mut self) {
        self.decode_ready = self
            .wanted
            .iter()
            .find_map(|&r| self.find_stored(r))
            .unwrap_or(EMPTY);
    }

    /// The stored slot to decode next: `demand` if it is stored, otherwise the
    /// highest-ranked wishlist entry that is.
    pub fn next_decode(&self, demand: usize) -> Option<usize> {
        self.find_stored(demand).or(if self.decode_ready == EMPTY {
            None
        } else {
            Some(self.decode_ready)
        })
    }

    /// Like [`SlotResidency::next_decode`], while a read of `reading` is live.
    /// Preserves speculative read/decode overlap: only an explicit unread
    /// urgent demand can hold the idle decoder while the transport is live.
    pub fn next_decode_during_read(&self, demand: usize, reading: usize) -> Option<usize> {
        let slot = self.next_decode(demand)?;
        if self.slots[slot].region == demand || !self.is_loading(reading) {
            return Some(slot);
        }
        // The demand may await the current transfer's Pause/cancel. Mere
        // wishlist priority is not urgency and must not serialize speculation.
        if demand != EMPTY && self.find(demand).is_none() {
            None
        } else {
            Some(slot)
        }
    }

    /// Slot `i` finished reading `region`. The caller must have observed the
    /// transport done, including its Pause retirement. This releases the IRQ
    /// lease but does not publish or pin unverified bytes.
    pub fn park_completed(&mut self, i: usize, region: usize) {
        assert_eq!(self.loading[i], region);
        assert_ne!(region, EMPTY);
        assert_ne!(i, self.current);
        assert_ne!(i, self.protected);
        self.loading[i] = EMPTY;
        self.slots[i].region = region;
        self.slots[i].len = 0;
        self.touch(i);
        self.refresh_decode();
    }

    /// Hand stored slot `i` to a decoder: it becomes loading again. Returns
    /// the region.
    pub fn claim_stored(&mut self, i: usize) -> usize {
        let region = self.slots[i].region;
        assert_ne!(region, EMPTY);
        assert_eq!(self.slots[i].len, 0);
        assert_eq!(self.loading[i], EMPTY);
        assert_ne!(i, self.current);
        assert_ne!(i, self.protected);
        self.slots[i] = Slot::new();
        self.loading[i] = region;
        self.refresh_decode();
        region
    }

    /// Protect the slot holding `region` from reuse while an upload reads it
    /// (`None` clears the protection). Returns `false`, keeping any earlier
    /// protection, when `region` is not resident.
    pub fn protect_upload(&mut self, region: Option<usize>) -> bool {
        match region {
            None => {
                self.protected = EMPTY;
                true
            }
            Some(region) => match self.find(region) {
                Some(i) => {
                    self.protected = i;
                    true
                }
                None => false,
            },
        }
    }

    /// Replace the wishlist (highest priority first). The current region,
    /// duplicates and [`EMPTY`] are dropped; extra entries are ignored.
    pub fn set_wanted(&mut self, regions: &[usize]) {
        let previous = self.wanted;
        self.wanted = [EMPTY; WANTED];
        let mut count = 0;
        for &region in regions {
            if region == EMPTY
                || region == self.slots[self.current].region
                || self.wanted.contains(&region)
            {
                continue;
            }
            self.wanted[count] = region;
            count += 1;
            if count == self.wanted.len() {
                break;
            }
        }
        if self.wanted != previous {
            self.refresh_decode();
        }
    }

    /// Drop completed or parked payloads outside the keep set. Current,
    /// protected and in-flight slots remain untouched. Returns how many slots
    /// were freed.
    pub fn evict_unwanted(&mut self, keep: &[usize]) -> usize {
        let mut removed = 0;
        for i in 0..SLOTS {
            if i == self.current || i == self.protected || self.loading[i] != EMPTY {
                continue;
            }
            let region = self.slots[i].region;
            if region == EMPTY || keep.contains(&region) {
                continue;
            }
            self.slots[i] = Slot::new();
            removed += 1;
        }
        if removed != 0 {
            self.refresh_decode();
        }
        removed
    }

    /// Whether `region` is being read or decoded.
    pub fn is_loading(&self, region: usize) -> bool {
        region != EMPTY && self.loading.contains(&region)
    }

    /// Give up the loading lease on slot `i` (a cancelled read or a failed
    /// decode); the slot stays empty.
    pub fn release(&mut self, i: usize) {
        self.loading[i] = EMPTY;
    }

    /// The first wishlist entry that is not resident, stored or loading and
    /// has a slot it may take.
    pub fn next_request(&self) -> Option<usize> {
        self.wanted.iter().copied().find(|&r| {
            r != EMPTY
                && self.find(r).is_none()
                && self.find_stored(r).is_none()
                && !self.is_loading(r)
                && self.victim(r, false).is_some()
        })
    }

    fn touch(&mut self, i: usize) {
        // Rebase before wraparound; preserve recent ordering over long sessions.
        self.clock = self.clock.saturating_add(1);
        if self.clock == u32::MAX {
            for slot in &mut self.slots {
                slot.used = slot.used.saturating_sub(u32::MAX / 2);
            }
            self.clock -= u32::MAX / 2;
        }
        self.slots[i].used = self.clock;
    }

    /// Make the verified room `region` current. `Some(changed)` where
    /// `changed` says whether the current slot moved; `None` when `region` is
    /// not resident.
    pub fn select(&mut self, region: usize) -> Option<bool> {
        let i = self.find(region)?;
        let changed = i != self.current;
        self.current = i;
        self.touch(i);
        Some(changed)
    }

    /// Reserve one private arena. Current and an in-progress GPU upload are
    /// never evicted; prefer empty, then no-longer-requested, then least
    /// recent.
    fn victim(&self, region: usize, urgent: bool) -> Option<usize> {
        if self.is_loading(region) || self.find_stored(region).is_some() {
            return None;
        }
        let priority = self
            .wanted
            .iter()
            .position(|&r| r == region)
            .unwrap_or(usize::MAX);
        let mut victim = None;
        for i in 0..SLOTS {
            if i == self.current || i == self.protected || self.loading[i] != EMPTY {
                continue;
            }
            let s = &self.slots[i];
            // A lower-priority speculative target must not displace an earlier
            // wishlist entry when a pinned upload temporarily reduces capacity.
            if !urgent
                && s.region != EMPTY
                && self
                    .wanted
                    .iter()
                    .position(|&r| r == s.region)
                    .is_some_and(|p| p <= priority)
            {
                continue;
            }
            let score = (s.region != EMPTY, self.wanted.contains(&s.region), s.used);
            if victim.is_none_or(|(_, best)| score < best) {
                victim = Some((i, score));
            }
        }
        victim.map(|(i, _)| i)
    }

    /// Whether [`SlotResidency::reserve`] would succeed.
    pub fn can_reserve(&self, region: usize, urgent: bool) -> bool {
        self.victim(region, urgent).is_some()
    }

    /// Take a slot for reading `region` (its previous contents are dropped).
    /// `urgent` lets the read displace wishlist entries.
    pub fn reserve(&mut self, region: usize, urgent: bool) -> Option<usize> {
        let i = self.victim(region, urgent)?;
        self.slots[i] = Slot::new();
        self.loading[i] = region;
        self.refresh_decode();
        Some(i)
    }

    /// Slot `i` now holds the verified room `region` of `len` bytes.
    pub fn admit(&mut self, i: usize, region: usize, len: usize) {
        assert_eq!(self.loading[i], region);
        assert_ne!(len, 0);
        self.loading[i] = EMPTY;
        self.slots[i].region = region;
        self.slots[i].len = len;
        self.touch(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Residency = SlotResidency<5, 4>;

    fn load(r: &mut Residency, region: usize) -> usize {
        let i = r.reserve(region, true).unwrap();
        r.admit(i, region, 100);
        i
    }

    #[test]
    fn decode_admission_only_waits_for_an_urgent_unread_demand_with_live_transport() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10);
        let stored = r.reserve(12, true).unwrap();
        r.park_completed(stored, 12);
        r.set_wanted(&[11, 12]);
        // Missing 11, and even a stale reading id without a private lease, are
        // insufficient reasons to hold a complete candidate indefinitely.
        assert_eq!(r.next_decode_during_read(EMPTY, EMPTY), Some(stored));
        assert_eq!(r.next_decode_during_read(EMPTY, 11), Some(stored));
        let read = r.reserve(11, true).unwrap();
        assert_eq!(r.next_decode_during_read(EMPTY, 11), Some(stored));
        assert_eq!(r.next_decode_during_read(11, 11), None);
        assert_eq!(r.next_decode_during_read(12, 11), Some(stored));
        r.set_wanted(&[12, 11]);
        assert_eq!(r.next_decode_during_read(EMPTY, 11), Some(stored));
        // An unread urgent destination may await the live transport's cancel.
        assert_eq!(r.next_decode_during_read(13, 11), None);
        r.release(read);
        assert_eq!(r.next_decode_during_read(13, EMPTY), Some(stored));
        // A read removed from the plan no longer outranks a wanted candidate.
        r.reserve(11, true).unwrap();
        r.set_wanted(&[12]);
        assert_eq!(r.next_decode_during_read(EMPTY, 11), Some(stored));
    }

    #[test]
    fn obsolete_completed_payload_is_evictable_but_never_visible_as_a_room() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        load(&mut r, 12);
        load(&mut r, 13);
        r.protect_upload(Some(12));
        let old = r.reserve(14, true).unwrap();
        r.set_wanted(&[11, 12, 13, 15]);
        assert_eq!(r.next_request(), None);
        r.park_completed(old, 14);
        assert!(!r.is_loading(14));
        assert_eq!(r.find_stored(14), Some(old));
        assert!(r.find(14).is_none());
        assert_eq!(r.select(14), None);
        assert!(!r.protect_upload(Some(14)));
        assert_eq!(r.next_decode(EMPTY), None);
        assert_eq!(r.next_request(), Some(15));
        let next = r.reserve(15, false).unwrap();
        assert_eq!(next, old);
        assert!(r.find_stored(14).is_none());
        assert_eq!(r.next_decode(EMPTY), None);
        for id in [10, 11, 12, 13] {
            assert!(r.find(id).is_some());
        }
    }

    #[test]
    fn brief_wishlist_reversal_reuses_completed_bytes_without_another_cd_request() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        let read = r.reserve(11, true).unwrap();
        let decoder = r.reserve(12, true).unwrap();
        r.set_wanted(&[13]);
        r.park_completed(read, 11);
        assert!(r.is_loading(12));
        assert_eq!(r.next_decode(EMPTY), None);
        r.set_wanted(&[11, 13]);
        assert_eq!(r.next_decode(EMPTY), Some(read));
        assert_eq!(r.next_request(), Some(13));
        assert!(!r.can_reserve(11, true));
        assert_eq!(r.claim_stored(read), 11);
        assert!(r.find_stored(11).is_none());
        assert!(r.is_loading(11) && r.is_loading(12));
        assert_eq!(r.next_decode(EMPTY), None);
        // Bytes remain private until the actual decoder confirms both hashes.
        assert!(r.find(11).is_none());
        r.admit(read, 11, 100);
        assert!(r.find(11).is_some());
        r.release(decoder);
        assert!(r.find(12).is_none());
    }

    #[test]
    fn decoder_priority_follows_demand_then_latest_wishlist_without_reordering_cd_leases() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        let a = r.reserve(11, true).unwrap();
        let b = r.reserve(12, true).unwrap();
        r.park_completed(a, 11);
        r.park_completed(b, 12);
        r.set_wanted(&[11, 12]);
        assert_eq!(r.next_decode(EMPTY), Some(a));
        assert_eq!(r.next_decode(12), Some(b));
        assert_eq!(r.claim_stored(b), 12);
        assert_eq!(r.next_decode(EMPTY), Some(a));
        r.set_wanted(&[13]);
        assert_eq!(r.next_decode(EMPTY), None);
        assert_eq!(r.next_decode(11), Some(a)); // Urgent, even outside the wishlist.
        r.release(b);
        assert!(r.find(12).is_none() && r.find_stored(12).is_none());
        assert_eq!(r.next_request(), Some(13));
    }

    #[test]
    fn wanted_completed_payload_survives_lower_priority_capacity_pressure() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        let pinned = load(&mut r, 99);
        r.protect_upload(Some(99));
        let decode = r.reserve(12, true).unwrap();
        let read = r.reserve(11, true).unwrap();
        load(&mut r, 13);
        r.set_wanted(&[11, 12, 13, 14]);
        r.park_completed(read, 11);
        assert_eq!(r.next_request(), None);
        assert!(!r.can_reserve(14, false));
        assert_eq!(r.next_decode(EMPTY), Some(read));
        r.protect_upload(None);
        let next = r.reserve(14, false).unwrap();
        assert_eq!(next, pinned);
        assert_ne!(next, read);
        assert_ne!(next, decode);
        assert_eq!(r.next_decode(EMPTY), Some(read));
    }

    #[test]
    fn concurrent_decode_and_read_keep_distinct_private_arenas() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        r.protect_upload(Some(11));
        load(&mut r, 14);
        r.set_wanted(&[14, 12, 13, 15]);
        let decode = r.reserve(12, false).unwrap();
        let read = r.reserve(13, false).unwrap();
        assert_ne!(decode, read);
        assert!(r.is_loading(12) && r.is_loading(13));
        assert_eq!(r.reserve(12, true), None);
        assert!(!r.can_reserve(13, true));
        assert!(r.find(12).is_none() && r.find(13).is_none());
        assert_eq!(r.next_request(), None);
        assert_eq!(r.reserve(15, false), None);
        assert!(r.can_reserve(15, true)); // Urgent demand may replace unpinned 14.
        r.admit(decode, 12, 100);
        assert!(r.find(12).is_some());
        assert!(r.is_loading(13));
        r.release(read);
        assert!(!r.is_loading(13));
        assert_eq!(r.next_request(), Some(13));
        assert!(r.find(10).is_some() && r.find(11).is_some());
    }

    #[test]
    fn cancelled_read_cannot_release_completed_decode_or_gpu_pin() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        let decode = r.reserve(11, true).unwrap();
        let read = r.reserve(12, true).unwrap();
        r.admit(decode, 11, 100);
        r.protect_upload(Some(11));
        r.release(read);
        let replacement = r.reserve(13, true).unwrap();
        assert_ne!(replacement, decode);
        assert!(r.find(11).is_some());
        assert!(r.is_loading(13));
    }

    #[test]
    fn keeps_current_and_background_upload_while_loading_two_ahead() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        assert!(r.protect_upload(Some(11)));
        load(&mut r, 12);
        load(&mut r, 13);
        load(&mut r, 14);
        load(&mut r, 15);
        assert!(r.find(10).is_some());
        assert!(r.find(11).is_some());
        assert!(r.find(12).is_none());
        assert!(r.find(13).is_some());
        assert!(r.find(14).is_some());
    }

    #[test]
    fn queue_deduplicates_and_skips_already_resident_neighbors() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        r.set_wanted(&[10, 11, 11, 12, 13, 14]);
        assert_eq!(r.next_request(), Some(11));
        load(&mut r, 11);
        assert_eq!(r.next_request(), Some(12));
        load(&mut r, 12);
        assert_eq!(r.next_request(), Some(13));
        load(&mut r, 13);
        assert_eq!(r.next_request(), Some(14));
        load(&mut r, 14);
        assert_eq!(r.next_request(), None);
    }

    #[test]
    fn replaces_unwanted_newer_neighbor_before_requested_older_one() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        load(&mut r, 12);
        load(&mut r, 13);
        load(&mut r, 14);
        r.set_wanted(&[11, 12, 15]);
        load(&mut r, 15);
        assert!(r.find(11).is_some());
        assert!(r.find(12).is_some());
        assert!(r.find(13).is_none());
    }

    #[test]
    fn protected_stale_upload_cannot_make_requested_neighbors_thrash() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        r.protect_upload(Some(11));
        r.set_wanted(&[12, 13, 14, 15]);
        for expected in [12, 13, 14] {
            assert_eq!(r.next_request(), Some(expected));
            let i = r.reserve(expected, false).unwrap();
            r.admit(i, expected, 100);
        }
        assert_eq!(r.next_request(), None);
        assert!(r.reserve(15, false).is_none());
        r.protect_upload(None);
        assert_eq!(r.next_request(), Some(15));
        let i = r.reserve(15, false).unwrap();
        r.admit(i, 15, 100);
        for region in [10, 12, 13, 14, 15] {
            assert!(r.find(region).is_some());
        }
    }

    #[test]
    fn missing_protection_request_preserves_prior_pin() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        assert!(r.protect_upload(Some(11)));
        assert!(!r.protect_upload(Some(99)));
        for region in 12..20 {
            load(&mut r, region);
        }
        assert!(r.find(11).is_some());
        assert!(r.protect_upload(None));
        for region in 20..24 {
            load(&mut r, region);
        }
        assert!(r.find(11).is_none());
    }

    #[test]
    fn directed_eviction_keeps_current_upload_and_requested_payloads() {
        let mut r = Residency::new();
        load(&mut r, 10);
        r.select(10).unwrap();
        load(&mut r, 11);
        load(&mut r, 12);
        let parked = r.reserve(13, true).unwrap();
        r.park_completed(parked, 13);
        r.protect_upload(Some(11));
        assert_eq!(r.evict_unwanted(&[12]), 1);
        assert!(r.find(10).is_some());
        assert!(r.find(11).is_some());
        assert!(r.find(12).is_some());
        assert!(r.find_stored(13).is_none());
    }

    /// The residency half of hk-format's `stored_rooms` integration test: a
    /// parked payload becomes visible only when the decoder admits it, and a
    /// failed decode leaves no trace.
    #[test]
    fn parked_payload_is_published_only_by_admit_and_a_failed_decode_leaves_nothing() {
        for fail in [false, true] {
            let mut r = Residency::new();
            let current = r.reserve(0, true).unwrap();
            r.admit(current, 0, 92);
            r.select(0);
            let slot = r.reserve(1, true).unwrap();
            r.set_wanted(&[2]);
            r.park_completed(slot, 1);
            assert_eq!(r.next_decode(EMPTY), None);
            assert_eq!(r.select(1), None);
            assert!(!r.protect_upload(Some(1)));
            assert!(r.find(1).is_none());
            // A later turn requests the same source; no new reserve/read is possible.
            r.set_wanted(&[1, 2]);
            assert_eq!(r.next_decode(EMPTY), Some(slot));
            assert!(!r.can_reserve(1, true));
            assert_eq!(r.claim_stored(slot), 1);
            assert!(r.find(1).is_none() && r.is_loading(1));
            if fail {
                r.release(slot);
                assert!(r.find(1).is_none() && r.find_stored(1).is_none());
                assert!(r.find(0).is_some());
            } else {
                r.admit(slot, 1, 92);
                assert_eq!(r.select(1), Some(true));
            }
        }
    }
}
