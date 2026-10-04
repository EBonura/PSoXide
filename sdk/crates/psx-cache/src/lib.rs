// SPDX-License-Identifier: GPL-2.0-or-later
//! A generic, fixed-capacity slot cache for `no_std`/no-heap targets (PS1).
//!
//! This captures the shape shared by every residency pool in the engine (the
//! room stream scheduler, VRAM texture slots, asset residency): a small set of
//! slots, a dense key->slot reverse map for O(1) lookup, least-recently-used
//! eviction, and pinning to protect the in-use working set. It owns the *index*
//! (which key lives in which slot, and the slot's load state); the caller owns
//! the *memory* the value points at. Keep variable-size allocation (VRAM
//! rectangles, CD byte ranges) in a separate allocator layer.
//!
//! - `V` is the per-slot payload (`Copy`); typically a small handle/metadata.
//! - `N` is the slot capacity (how many values can be resident at once).
//! - `MAX_KEY` is the key space; keys are `u16` in `0..MAX_KEY`, used to index
//!   the reverse map. Pick the smallest bound that covers your id space.
//!
//! Slots move `Empty -> Loading -> Ready`. Synchronous consumers use
//! [`SlotCache::get_or_insert_with`]; consumers whose creation is asynchronous
//! (streaming a room off the CD over several frames) use [`SlotCache::begin_load`]
//! then [`SlotCache::finish_load`] when the load completes.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Sentinel stored in the reverse map for "no slot".
const NONE: u16 = u16::MAX;

/// Load state of a slot.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SlotState {
    /// Free; holds no key or value.
    Empty,
    /// Reserved for a key; a value is being produced (async load in flight).
    Loading,
    /// Holds a usable value for its key.
    Ready,
}

/// A slot reserved for a key by [`SlotCache::begin_load`], to be completed by
/// [`SlotCache::finish_load`]. It remembers the key as well as the slot, so a
/// load that finishes after its slot was evicted and reused is refused instead
/// of storing its value under another key.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Reservation {
    slot: u16,
    key: u16,
}

impl Reservation {
    fn new(slot: usize, key: u16) -> Self {
        // `begin_load` only builds this from a slot index below `N`, which the
        // cache keeps under `u16::MAX`.
        Self {
            slot: slot as u16,
            key,
        }
    }

    /// The reserved slot index.
    pub const fn slot(&self) -> usize {
        self.slot as usize
    }

    /// The key the slot was reserved for.
    pub const fn key(&self) -> u16 {
        self.key
    }
}

/// [`SlotCache::finish_load`] refused a reservation whose key no longer owns
/// its slot.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Stale;

#[derive(Copy, Clone)]
struct Slot<V: Copy> {
    key: u16,
    value: Option<V>,
    state: SlotState,
    /// Epoch of the last `get`/touch; the LRU eviction key.
    last_used: u32,
    /// Pinned slots are never chosen for eviction.
    pinned: bool,
}

/// Keyed, fixed-capacity cache with LRU eviction and pinning.
pub struct SlotCache<V: Copy, const N: usize, const MAX_KEY: usize> {
    slots: [Slot<V>; N],
    key_to_slot: [u16; MAX_KEY],
    epoch: u32,
}

impl<V: Copy, const N: usize, const MAX_KEY: usize> Default for SlotCache<V, N, MAX_KEY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Copy, const N: usize, const MAX_KEY: usize> SlotCache<V, N, MAX_KEY> {
    /// An empty cache. `const` so it can back a `static`.
    pub const fn new() -> Self {
        Self {
            slots: [Slot {
                key: NONE,
                value: None,
                state: SlotState::Empty,
                last_used: 0,
                pinned: false,
            }; N],
            key_to_slot: [NONE; MAX_KEY],
            epoch: 0,
        }
    }

    /// Slot capacity.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Advance the LRU clock. Call once per cache cycle (e.g. per frame) so
    /// `get` touches and eviction comparisons share a monotonic timeline.
    /// Never returns 0, so a freshly touched slot always beats the initial
    /// `last_used` of an untouched one.
    pub fn bump_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
    }

    /// The current epoch (the value `get` stamps into `last_used`).
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    fn key_ok(key: u16) -> bool {
        (key as usize) < MAX_KEY && key != NONE
    }

    /// Slot index currently mapped to `key` in any non-empty state, validated
    /// against the forward store (so a stale reverse entry never lies).
    pub fn slot_of(&self, key: u16) -> Option<usize> {
        if !Self::key_ok(key) {
            return None;
        }
        let raw = self.key_to_slot[key as usize];
        if raw == NONE {
            return None;
        }
        let s = raw as usize;
        if s < N && self.slots[s].state != SlotState::Empty && self.slots[s].key == key {
            Some(s)
        } else {
            None
        }
    }

    /// Is `key` resident and usable (`Ready`)?
    pub fn contains_ready(&self, key: u16) -> bool {
        self.slot_of(key)
            .is_some_and(|s| self.slots[s].state == SlotState::Ready)
    }

    /// Is `key` reserved with a load in flight (`Loading`)?
    pub fn is_loading(&self, key: u16) -> bool {
        self.slot_of(key)
            .is_some_and(|s| self.slots[s].state == SlotState::Loading)
    }

    /// Load state of `key`, or `Empty` if absent.
    pub fn state_of(&self, key: u16) -> SlotState {
        match self.slot_of(key) {
            Some(s) => self.slots[s].state,
            None => SlotState::Empty,
        }
    }

    /// Get a `Ready` value, marking it most-recently-used. Returns `None` for an
    /// absent or still-`Loading` key.
    pub fn get(&mut self, key: u16) -> Option<&V> {
        let s = self.slot_of(key)?;
        if self.slots[s].state != SlotState::Ready {
            return None;
        }
        self.slots[s].last_used = self.epoch;
        self.slots[s].value.as_ref()
    }

    /// Like [`get`](Self::get) but without touching the LRU clock.
    pub fn peek(&self, key: u16) -> Option<&V> {
        let s = self.slot_of(key)?;
        if self.slots[s].state != SlotState::Ready {
            return None;
        }
        self.slots[s].value.as_ref()
    }

    /// Reserve a slot for `key` and mark it `Loading`, evicting the
    /// least-recently-used unpinned `Ready` slot if the cache is full. Returns
    /// a [`Reservation`] for the caller to complete with
    /// [`finish_load`](Self::finish_load), or `None` if `key` is out of range
    /// or every slot is pinned or already loading. If `key` already has a
    /// slot, a reservation for that slot is returned unchanged (no duplicate
    /// load is started).
    pub fn begin_load(&mut self, key: u16) -> Option<Reservation> {
        if !Self::key_ok(key) {
            return None;
        }
        if let Some(s) = self.slot_of(key) {
            return Some(Reservation::new(s, key));
        }
        let s = self.alloc_slot()?;
        self.detach(s);
        self.slots[s] = Slot {
            key,
            value: None,
            state: SlotState::Loading,
            last_used: self.epoch,
            pinned: false,
        };
        self.key_to_slot[key as usize] = s as u16;
        Some(Reservation::new(s, key))
    }

    /// Complete a [`begin_load`](Self::begin_load): store the value and mark
    /// the slot `Ready` and most-recently-used. Returns [`Stale`] when the
    /// reservation's key no longer owns its slot (it was evicted, and the slot
    /// may now be loading another key), and stores nothing.
    pub fn finish_load(&mut self, reservation: Reservation, value: V) -> Result<(), Stale> {
        let slot = reservation.slot();
        if slot < N
            && self.slots[slot].state != SlotState::Empty
            && self.slots[slot].key == reservation.key
        {
            self.slots[slot].value = Some(value);
            self.slots[slot].state = SlotState::Ready;
            self.slots[slot].last_used = self.epoch;
            Ok(())
        } else {
            Err(Stale)
        }
    }

    /// Renamed to [`begin_load`](Self::begin_load), which returns the key with
    /// the slot so a stale completion can be told from a live one.
    #[deprecated(note = "renamed to `begin_load`")]
    pub fn reserve(&mut self, key: u16) -> Option<usize> {
        self.begin_load(key).map(|r| r.slot())
    }

    /// Renamed to [`finish_load`](Self::finish_load). A bare slot index cannot
    /// tell that the slot was evicted and reused for another key since
    /// [`reserve`](Self::reserve), so a late completion stores its value under
    /// the wrong key; this keeps that behaviour for one stage.
    #[deprecated(note = "renamed to `finish_load`, which checks the key still owns the slot")]
    pub fn mark_ready(&mut self, slot: usize, value: V) {
        if slot < N && self.slots[slot].state != SlotState::Empty {
            self.slots[slot].value = Some(value);
            self.slots[slot].state = SlotState::Ready;
            self.slots[slot].last_used = self.epoch;
        }
    }

    /// Return the cached value for `key`, or synchronously build and insert it.
    /// Evicts LRU on a miss when full. If `build` returns `None` the reservation
    /// is released and this returns `None`.
    pub fn get_or_insert_with<F>(&mut self, key: u16, build: F) -> Option<&V>
    where
        F: FnOnce() -> Option<V>,
    {
        if self.contains_ready(key) {
            return self.get(key);
        }
        let r = self.begin_load(key)?;
        let s = r.slot();
        match build() {
            Some(v) => {
                // A fresh reservation owns its slot, so this cannot be stale.
                let _ = self.finish_load(r, v);
                self.slots[s].value.as_ref()
            }
            None => {
                self.evict_slot(s);
                None
            }
        }
    }

    /// Pin `key`'s slot so it is never evicted. No-op if absent.
    pub fn pin(&mut self, key: u16) {
        if let Some(s) = self.slot_of(key) {
            self.slots[s].pinned = true;
        }
    }

    /// Clear all pins.
    pub fn unpin_all(&mut self) {
        for s in self.slots.iter_mut() {
            s.pinned = false;
        }
    }

    /// Replace the pin set: unpin everything, then pin each present key in
    /// `keys`. This is the "declare the working set" call a streaming reconcile
    /// makes each cycle.
    pub fn set_pinned(&mut self, keys: &[u16]) {
        self.unpin_all();
        for &k in keys {
            self.pin(k);
        }
    }

    /// Evict `key` (any state) back to `Empty`. No-op if absent.
    pub fn evict(&mut self, key: u16) {
        if let Some(s) = self.slot_of(key) {
            self.evict_slot(s);
        }
    }

    /// Evict every `Ready`, unpinned slot whose key is not in `keep`. Used by a
    /// reconcile to drop rooms that left the desired set while protecting the
    /// pinned working set. Returns the number evicted.
    pub fn evict_ready_outside(&mut self, keep: &[u16]) -> usize {
        let mut evicted = 0;
        for s in 0..N {
            if self.slots[s].state == SlotState::Ready
                && !self.slots[s].pinned
                && !keep.contains(&self.slots[s].key)
            {
                self.evict_slot(s);
                evicted += 1;
            }
        }
        evicted
    }

    /// Number of `Ready` slots.
    pub fn ready_len(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.state == SlotState::Ready)
            .count()
    }

    /// Number of occupied (`Loading` or `Ready`) slots.
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.state != SlotState::Empty)
            .count()
    }

    /// Whether no slot is occupied.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pick a slot to (re)use: first any `Empty`, else the least-recently-used
    /// unpinned `Ready` slot. `Loading` and pinned slots are never taken, so an
    /// in-flight load or the protected working set is never clobbered.
    fn alloc_slot(&mut self) -> Option<usize> {
        let mut victim: Option<usize> = None;
        for s in 0..N {
            match self.slots[s].state {
                SlotState::Empty => return Some(s),
                SlotState::Ready if !self.slots[s].pinned => {
                    let better = match victim {
                        None => true,
                        Some(v) => self.slots[s].last_used < self.slots[v].last_used,
                    };
                    if better {
                        victim = Some(s);
                    }
                }
                _ => {}
            }
        }
        victim
    }

    /// Drop a slot's reverse-map entry if it still points here.
    fn detach(&mut self, slot: usize) {
        let s = &self.slots[slot];
        if s.state != SlotState::Empty {
            let k = s.key as usize;
            if k < MAX_KEY && self.key_to_slot[k] == slot as u16 {
                self.key_to_slot[k] = NONE;
            }
        }
    }

    fn evict_slot(&mut self, slot: usize) {
        self.detach(slot);
        self.slots[slot] = Slot {
            key: NONE,
            value: None,
            state: SlotState::Empty,
            last_used: 0,
            pinned: false,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Cache = SlotCache<u32, 3, 16>;

    /// Build and `Ready` a key in one step.
    fn put(c: &mut Cache, key: u16, val: u32) {
        let r = c.begin_load(key).expect("begin_load");
        c.finish_load(r, val).unwrap();
    }

    #[test]
    fn empty_start() {
        let c = Cache::new();
        assert!(c.is_empty());
        assert_eq!(c.ready_len(), 0);
        assert!(!c.contains_ready(0));
    }

    #[test]
    fn insert_then_get_roundtrips_and_slot_is_stable() {
        let mut c = Cache::new();
        let r = c.begin_load(5).unwrap();
        let s = r.slot();
        assert!(c.is_loading(5));
        c.finish_load(r, 500).unwrap();
        assert!(c.contains_ready(5));
        assert_eq!(c.get(5), Some(&500));
        assert_eq!(c.slot_of(5), Some(s));
        // re-reserving an existing key returns the same slot, no second load
        assert_eq!(c.begin_load(5), Some(r));
    }

    #[test]
    fn a_load_that_finishes_after_its_slot_was_reused_is_refused() {
        // The async case: a CD load for key 1 is in flight, key 1 is evicted,
        // key 2 takes the slot, then key 1's load completes.
        let mut c = SlotCache::<u32, 1, 16>::new();
        let stale = c.begin_load(1).unwrap();
        c.evict(1);
        let live = c.begin_load(2).unwrap();
        assert_eq!(live.slot(), stale.slot(), "the one slot is reused");

        assert_eq!(c.finish_load(stale, 111), Err(Stale));
        // Key 2 is still loading and holds nothing of key 1's.
        assert!(c.is_loading(2));
        assert_eq!(c.get(2), None);

        c.finish_load(live, 222).unwrap();
        assert_eq!(c.get(2), Some(&222));
        assert!(!c.contains_ready(1));
    }

    #[test]
    fn finishing_a_reservation_whose_slot_was_emptied_is_refused() {
        let mut c = Cache::new();
        let r = c.begin_load(3).unwrap();
        c.evict(3);
        assert_eq!(c.finish_load(r, 30), Err(Stale));
        assert!(c.is_empty());
    }

    /// The deprecated pair keeps the old behaviour for one stage: a bare slot
    /// index cannot tell the slot changed hands, so key 2 reads key 1's value.
    #[allow(deprecated)]
    #[test]
    fn the_deprecated_pair_still_stores_under_the_wrong_key() {
        let mut c = SlotCache::<u32, 1, 16>::new();
        let slot = c.reserve(1).unwrap();
        c.evict(1);
        assert_eq!(c.reserve(2), Some(slot));
        c.mark_ready(slot, 111);
        assert_eq!(c.get(2), Some(&111));
    }

    #[test]
    fn get_or_insert_builds_on_miss_returns_on_hit() {
        let mut c = Cache::new();
        let mut builds = 0;
        let v = *c
            .get_or_insert_with(7, || {
                builds += 1;
                Some(70)
            })
            .unwrap();
        assert_eq!(v, 70);
        // hit: the build closure must NOT run; the cached value is returned
        let v2 = *c
            .get_or_insert_with(7, || {
                builds += 1;
                Some(999)
            })
            .unwrap();
        assert_eq!(v2, 70);
        assert_eq!(builds, 1, "build runs only on a miss");
    }

    #[test]
    fn get_or_insert_build_failure_leaves_slot_empty() {
        let mut c = Cache::new();
        assert_eq!(c.get_or_insert_with(3, || None), None);
        assert!(!c.contains_ready(3));
        assert!(c.is_empty());
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let mut c = Cache::new();
        put(&mut c, 1, 10);
        c.bump_epoch();
        put(&mut c, 2, 20);
        c.bump_epoch();
        put(&mut c, 3, 30);
        // touch 1 and 3 so 2 becomes the LRU
        c.bump_epoch();
        let _ = c.get(1);
        let _ = c.get(3);
        // inserting a 4th evicts key 2
        c.bump_epoch();
        put(&mut c, 4, 40);
        assert!(!c.contains_ready(2), "LRU victim should be evicted");
        assert!(c.contains_ready(1) && c.contains_ready(3) && c.contains_ready(4));
        assert_eq!(c.slot_of(2), None);
    }

    #[test]
    fn pin_protects_from_eviction() {
        let mut c = Cache::new();
        put(&mut c, 1, 10);
        put(&mut c, 2, 20);
        put(&mut c, 3, 30);
        // pin the otherwise-oldest key
        c.bump_epoch();
        let _ = c.get(2);
        let _ = c.get(3);
        c.pin(1); // 1 is LRU but pinned
        c.bump_epoch();
        put(&mut c, 4, 40);
        assert!(c.contains_ready(1), "pinned key must survive");
        assert!(
            !c.contains_ready(2),
            "an unpinned key is the victim instead"
        );
    }

    #[test]
    fn reserve_fails_when_all_slots_pinned() {
        let mut c = Cache::new();
        put(&mut c, 1, 10);
        put(&mut c, 2, 20);
        put(&mut c, 3, 30);
        c.set_pinned(&[1, 2, 3]);
        assert_eq!(c.begin_load(4), None, "no evictable slot");
        assert!(!c.contains_ready(4));
    }

    #[test]
    fn evict_ready_outside_keeps_listed_and_pinned() {
        let mut c = Cache::new();
        put(&mut c, 1, 10);
        put(&mut c, 2, 20);
        put(&mut c, 3, 30);
        c.pin(3);
        let evicted = c.evict_ready_outside(&[1]);
        assert_eq!(evicted, 1); // only 2 leaves
        assert!(c.contains_ready(1)); // in keep list
        assert!(!c.contains_ready(2)); // dropped
        assert!(c.contains_ready(3)); // pinned
    }

    #[test]
    fn reverse_map_stays_consistent_across_reuse() {
        let mut c = Cache::new();
        put(&mut c, 1, 10);
        put(&mut c, 2, 20);
        put(&mut c, 3, 30);
        c.bump_epoch();
        put(&mut c, 4, 40); // evicts LRU (1), reuses its slot
        assert_eq!(c.slot_of(1), None, "evicted key has no slot");
        assert!(c.contains_ready(4));
        // the slot 4 took must not still answer for 1
        let s4 = c.slot_of(4).unwrap();
        assert_ne!(c.slot_of(2), Some(s4));
    }

    #[test]
    fn out_of_range_key_is_rejected() {
        let mut c = Cache::new();
        assert_eq!(c.begin_load(16), None); // == MAX_KEY
        assert_eq!(c.begin_load(NONE), None);
        assert!(!c.contains_ready(16));
    }
}
