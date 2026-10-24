// SPDX-License-Identifier: GPL-2.0-or-later
//! Exact-rectangle VRAM ownership ledger.
//!
//! VRAM is the one PS1 resource with no allocator between a writer and the
//! hardware: an upload to the wrong rectangle silently turns somebody else's
//! texture into confetti. [`VramMap`] is the ledger that says who owns which
//! rectangle, lets a scene borrow a rectangle another owner holds (a cutscene
//! panorama over the room textures) and puts the lender's pixels back when the
//! loan ends, and gates every upload through [`VramMap::permit`].
//!
//! It deliberately knows nothing about *how* a region is filled. Streaming
//! games sub-allocate inside a region with [`crate::VramAllocator`] (or their
//! own LRU), whole-level games claim one region per offline-cooked atlas, and a
//! single-scene game can claim a handful of constants once. All of them call
//! the same verbs; what differs is the region's [`Policy`], chosen where the
//! region is declared.
//!
//! ## Cost
//!
//! * No heap, no `dyn`, no float, no 64-bit arithmetic.
//! * RAM: `16 * N + 4` bytes. An all-zero map is empty ([`VramMap::new`]), so a
//!   game keeps it in `.bss`.
//! * One slot per [`Owner`]; the owner id is the table index, so the hot calls
//!   ([`VramMap::permit`], [`VramMap::ensure`]) never scan. Only claims and
//!   borrows scan the table (`N` compares), and those happen at scene or level
//!   boundaries, never per frame.
//! * Debug builds assert on every violation; release builds only count them
//!   ([`VramMap::violations`]) and still let the upload through, so a counter
//!   can never change shipped behaviour.
//!
//! ## One loader, any loading model
//!
//! ```
//! use psx_vram::map::{Ensure, Owner, Policy, VramMap};
//! use psx_vram::VramRect;
//!
//! const ATLAS: Owner = Owner::new(0);
//! const HUD: Owner = Owner::new(1);
//!
//! let mut map = VramMap::<4>::new();
//! let hud = VramRect::new(960, 0, 64, 256);
//! let atlas = VramRect::new(320, 0, 256, 256);
//!
//! // Boot: the HUD survives level changes. (A `Persist` region is only
//! // re-uploaded when its key changes.)
//! assert_eq!(map.ensure(HUD, hud, Policy::Persist, 1), Ok(Ensure::Miss));
//!
//! // Level load: same code whatever the policy. `Level` re-uploads every
//! // level; change this one word to `Persist` to keep the atlas when the
//! // next level has the same key.
//! map.reset_scene();
//! assert_eq!(map.ensure(ATLAS, atlas, Policy::Level, 7), Ok(Ensure::Miss));
//! map.reset_scene();
//! assert_eq!(map.ensure(ATLAS, atlas, Policy::Level, 7), Ok(Ensure::Miss));
//! assert_eq!(map.ensure(HUD, hud, Policy::Persist, 1), Ok(Ensure::Hit));
//! ```

use crate::VramRect;

/// Who owns a region: a game-defined small integer, one region per owner.
///
/// The value is the slot index in [`VramMap`], so it must be below the map's
/// capacity. Games name theirs with `const`s (`const FONT: Owner = Owner::new(3)`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Owner(u8);

impl Owner {
    /// Owner number `index`.
    pub const fn new(index: u8) -> Self {
        Self(index)
    }

    /// The owner's number.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// How long a region's contents are expected to live. This is the whole
/// difference between the loading models; the calls around it never change.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Dropped by [`VramMap::reset_scene`]: whole-level load, per-scene content.
    Level = 0,
    /// Survives [`VramMap::reset_scene`]: pre-loaded or resident content that
    /// is only uploaded again when its key changes.
    Persist = 1,
    /// Survives [`VramMap::reset_scene`] and is a container the game
    /// sub-allocates inside (streaming). The map tracks the container, not what
    /// is in it, so [`VramMap::ensure`] ignores the key.
    Pool = 2,
}

/// Result of [`VramMap::ensure`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Ensure {
    /// The region already holds the content for this key: skip the upload.
    Hit,
    /// The region was (re)claimed or holds something else: upload now.
    Miss,
}

/// Why a claim or borrow was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// The owner number is not below the map's capacity.
    OwnerOutOfRange,
    /// The rectangle overlaps this owner's live region.
    Overlap(Owner),
}

/// Restore callback: put `owner`'s region contents back (re-upload from the
/// game's own source). It receives the map, so the uploads it makes go through
/// [`VramMap::permit`] like any other. Every region a loan
/// displaced is already usable when the first callback runs.
pub type RestoreFn<const N: usize> = fn(&mut VramMap<N>, Owner);

/// `key` value that matches nothing: the region is live but its contents are
/// unknown. Keys passed to [`VramMap::ensure`] must not equal it.
pub const NO_KEY: u16 = u16::MAX;

const LIVE: u8 = 1;
const DISPLACED: u8 = 2;
const PENDING: u8 = 4;

#[derive(Copy, Clone)]
struct Slot<const N: usize> {
    rect: VramRect,
    restore: Option<RestoreFn<N>>,
    key: u16,
    flags: u8,
    policy: Policy,
}

impl<const N: usize> Slot<N> {
    const EMPTY: Self = Self {
        rect: VramRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        restore: None,
        key: 0,
        flags: 0,
        policy: Policy::Level,
    };

    const fn live(&self) -> bool {
        self.flags & LIVE != 0
    }

    const fn displaced(&self) -> bool {
        self.flags & DISPLACED != 0
    }
}

const fn overlaps(a: VramRect, b: VramRect) -> bool {
    a.overlaps(b)
}

const fn contains(outer: VramRect, inner: VramRect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x as u32 + inner.w as u32 <= outer.x as u32 + outer.w as u32
        && inner.y as u32 + inner.h as u32 <= outer.y as u32 + outer.h as u32
}

/// The ownership ledger. `N` is the number of distinct owners (at most 256).
///
/// `Copy` on purpose: `let checkpoint = map;` / `map = checkpoint;` is the
/// checkpoint and restore of a pre-load pool.
#[derive(Copy, Clone)]
pub struct VramMap<const N: usize> {
    slots: [Slot<N>; N],
    violations: u16,
    last_violation: u8,
}

impl<const N: usize> Default for VramMap<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> VramMap<N> {
    /// An empty map. Every byte is zero, so a `static` of it is `.bss`.
    pub const fn new() -> Self {
        Self {
            slots: [Slot::<N>::EMPTY; N],
            violations: 0,
            last_violation: 0,
        }
    }

    fn note_violation(&mut self, owner: Owner) {
        self.violations = self.violations.saturating_add(1);
        self.last_violation = owner.0;
    }

    /// Violations seen so far: uploads outside a live, undisplaced region, and
    /// refused claims. Release builds count; debug builds also assert.
    pub const fn violations(&self) -> u16 {
        self.violations
    }

    /// Owner number of the most recent violation (meaningful when
    /// [`violations`](Self::violations) is not zero).
    pub const fn last_violation(&self) -> Owner {
        Owner(self.last_violation)
    }

    /// The owner's live region, if any.
    pub fn rect_of(&self, owner: Owner) -> Option<VramRect> {
        let slot = self.slots.get(owner.index())?;
        slot.live().then_some(slot.rect)
    }

    /// Whether the owner has a live region.
    pub fn is_live(&self, owner: Owner) -> bool {
        self.rect_of(owner).is_some()
    }

    /// Whether the owner's region lost pixels to a borrower and has not been
    /// restored yet.
    pub fn is_displaced(&self, owner: Owner) -> bool {
        self.slots
            .get(owner.index())
            .is_some_and(|slot| slot.live() && slot.displaced())
    }

    /// The live, undisplaced owner of the pixel `(x, y)`: a debugging aid for
    /// "who wrote this".
    pub fn owner_at(&self, x: u16, y: u16) -> Option<Owner> {
        let probe = VramRect { x, y, w: 1, h: 1 };
        (0..N)
            .rev()
            .find(|&i| self.slots[i].live() && overlaps(self.slots[i].rect, probe))
            .map(|i| Owner(i as u8))
    }

    /// Register what to run when `owner`'s region has to be put back after a
    /// loan. Without one, the region's key is reset so the next
    /// [`ensure`](Self::ensure) misses and the game uploads as usual.
    pub fn set_restore(&mut self, owner: Owner, restore: RestoreFn<N>) {
        if let Some(slot) = self.slots.get_mut(owner.index()) {
            slot.restore = Some(restore);
        }
    }

    #[inline(never)]
    fn claim_inner(
        &mut self,
        owner: Owner,
        rect: VramRect,
        policy: Policy,
        borrowing: bool,
    ) -> Result<(), ClaimError> {
        let index = owner.index();
        if index >= N {
            self.note_violation(owner);
            debug_assert!(false, "vram owner {index} out of range");
            return Err(ClaimError::OwnerOutOfRange);
        }
        let mut i = 0;
        while i < N {
            let other = &self.slots[i];
            if i != index && other.live() && overlaps(other.rect, rect) {
                if !borrowing {
                    self.note_violation(owner);
                    debug_assert!(false, "vram owner {index} overlaps owner {i}");
                    return Err(ClaimError::Overlap(Owner(i as u8)));
                }
                self.slots[i].flags |= DISPLACED;
            }
            i += 1;
        }
        let slot = &mut self.slots[index];
        slot.rect = rect;
        slot.policy = policy;
        slot.key = NO_KEY;
        slot.flags = LIVE;
        Ok(())
    }

    /// Claim `rect` for `owner`, replacing the owner's previous region. Refused
    /// when it overlaps another owner's live region (use
    /// [`borrow`](Self::borrow) when that is intended).
    pub fn claim(
        &mut self,
        owner: Owner,
        rect: VramRect,
        policy: Policy,
    ) -> Result<(), ClaimError> {
        self.claim_inner(owner, rect, policy, false)
    }

    /// Claim `rect` for `owner` even though it overlaps other owners' live
    /// regions. Each overlapped region is marked displaced: its pixels are no
    /// longer its own, [`permit`](Self::permit) refuses uploads to it, and
    /// [`end_borrow`](Self::end_borrow) puts it back.
    pub fn borrow(
        &mut self,
        owner: Owner,
        rect: VramRect,
        policy: Policy,
    ) -> Result<(), ClaimError> {
        self.claim_inner(owner, rect, policy, true)
    }

    /// End a loan: release the borrower, then restore every displaced region
    /// that no live region overlaps any more, in owner-number order, by running
    /// its restore callback (or resetting its key when it has none).
    pub fn end_borrow(&mut self, owner: Owner) {
        self.release(owner);
        self.restore_uncovered();
    }

    fn restore_uncovered(&mut self) {
        // Pass one clears every flag, so a callback may write any region the
        // loan displaced (its upload is permitted); pass two runs them in order.
        let mut i = 0;
        while i < N {
            if self.slots[i].live() && self.slots[i].displaced() && !self.is_covered(i) {
                self.slots[i].flags = LIVE | PENDING;
            }
            i += 1;
        }
        let mut i = 0;
        while i < N {
            let slot = self.slots[i];
            if slot.flags & PENDING != 0 {
                self.slots[i].flags = LIVE;
                match slot.restore {
                    Some(restore) => restore(self, Owner(i as u8)),
                    None => self.slots[i].key = NO_KEY,
                }
            }
            i += 1;
        }
    }

    /// Whether any other live region overlaps slot `i`.
    fn is_covered(&self, i: usize) -> bool {
        let rect = self.slots[i].rect;
        (0..N).any(|j| j != i && self.slots[j].live() && overlaps(self.slots[j].rect, rect))
    }

    /// Make `rect` the owner's region holding `key`. [`Ensure::Hit`] when it
    /// already does (same rectangle, live, not displaced, same key, or a
    /// [`Policy::Pool`] container), so the caller skips the upload;
    /// [`Ensure::Miss`] after (re)claiming it.
    pub fn ensure(
        &mut self,
        owner: Owner,
        rect: VramRect,
        policy: Policy,
        key: u16,
    ) -> Result<Ensure, ClaimError> {
        debug_assert!(key != NO_KEY, "NO_KEY is not a content key");
        if let Some(slot) = self.slots.get(owner.index()) {
            let same = slot.live()
                && !slot.displaced()
                && slot.rect == rect
                && slot.policy == policy
                && (slot.key == key || policy == Policy::Pool);
            if same {
                return Ok(Ensure::Hit);
            }
        }
        self.release(owner);
        self.claim_inner(owner, rect, policy, false)?;
        self.slots[owner.index()].key = key;
        Ok(Ensure::Miss)
    }

    /// Drop the owner's region. Whatever it displaced is not restored (that is
    /// [`end_borrow`](Self::end_borrow)).
    pub fn release(&mut self, owner: Owner) {
        if let Some(slot) = self.slots.get_mut(owner.index()) {
            slot.flags = 0;
        }
    }

    /// Drop every [`Policy::Level`] region (a level or scene change).
    pub fn reset_scene(&mut self) {
        for slot in &mut self.slots {
            if slot.policy == Policy::Level {
                slot.flags = 0;
            }
        }
        self.restore_uncovered();
    }

    /// Drop every region.
    pub fn reset_all(&mut self) {
        *self = Self {
            violations: self.violations,
            last_violation: self.last_violation,
            ..Self::new()
        };
    }

    /// The gate every upload passes: `true` when `rect` lies inside the owner's
    /// live, undisplaced region. A `false` is a violation: counted in release,
    /// asserted in debug. Callers may still proceed (see the module docs).
    #[inline(never)]
    pub fn permit(&mut self, owner: Owner, rect: VramRect) -> bool {
        let ok = self
            .slots
            .get(owner.index())
            .is_some_and(|slot| slot.live() && !slot.displaced() && contains(slot.rect, rect));
        if !ok {
            self.note_violation(owner);
            debug_assert!(false, "vram owner {} wrote outside its region", owner.0);
        }
        ok
    }

    /// [`permit`](Self::permit), then [`crate::upload_bytes`].
    pub fn upload_bytes(&mut self, owner: Owner, rect: VramRect, bytes: &[u8]) {
        self.permit(owner, rect);
        crate::upload_bytes(rect, bytes);
    }

    /// [`permit`](Self::permit), then [`crate::upload_bytes_aligned`].
    pub fn upload_bytes_aligned(&mut self, owner: Owner, rect: VramRect, bytes: &[u8]) {
        self.permit(owner, rect);
        crate::upload_bytes_aligned(rect, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Owner = Owner::new(0);
    const B: Owner = Owner::new(1);
    const C: Owner = Owner::new(2);

    fn r(x: u16, y: u16, w: u16, h: u16) -> VramRect {
        VramRect::new(x, y, w, h)
    }

    #[test]
    fn claims_must_be_disjoint() {
        let mut map = VramMap::<4>::new();
        assert_eq!(map.claim(A, r(0, 0, 100, 100), Policy::Level), Ok(()));
        assert_eq!(map.claim(B, r(100, 0, 100, 100), Policy::Level), Ok(()));
        assert_eq!(map.owner_at(150, 50), Some(B));
        assert_eq!(map.owner_at(500, 50), None);
        // Violations assert in debug builds; the count is checked in release
        // by the borrow test below.
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn overlapping_claim_is_refused_and_counted() {
        let mut map = VramMap::<4>::new();
        map.claim(A, r(0, 0, 100, 100), Policy::Level).unwrap();
        assert_eq!(
            map.claim(B, r(50, 50, 100, 100), Policy::Level),
            Err(ClaimError::Overlap(A))
        );
        assert_eq!(map.violations(), 1);
        assert!(!map.is_live(B));
    }

    #[test]
    fn permit_accepts_inside_only() {
        let mut map = VramMap::<4>::new();
        map.claim(A, r(10, 10, 20, 20), Policy::Level).unwrap();
        assert!(map.permit(A, r(10, 10, 20, 20)));
        assert!(map.permit(A, r(29, 29, 1, 1)));
        assert_eq!(map.violations(), 0);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn permit_counts_outside_and_foreign_writes() {
        let mut map = VramMap::<4>::new();
        map.claim(A, r(10, 10, 20, 20), Policy::Level).unwrap();
        assert!(!map.permit(A, r(10, 10, 21, 20)));
        assert!(!map.permit(B, r(10, 10, 1, 1)));
        assert_eq!(map.violations(), 2);
        assert_eq!(map.last_violation(), B);
    }

    #[test]
    fn borrow_displaces_and_end_borrow_restores_in_order() {
        use core::sync::atomic::{AtomicU32, Ordering};
        static ORDER: AtomicU32 = AtomicU32::new(0);
        fn restore(map: &mut VramMap<4>, owner: Owner) {
            // The callback uploads through the map, and every region the loan
            // displaced is permitted again by the time the first one runs.
            assert!(map.permit(owner, map.rect_of(owner).unwrap()));
            assert!(map.permit(B, map.rect_of(B).unwrap()));
            ORDER.store(
                ORDER.load(Ordering::Relaxed) * 10 + owner.index() as u32 + 1,
                Ordering::Relaxed,
            );
        }
        let mut map = VramMap::<4>::new();
        map.claim(A, r(0, 0, 64, 256), Policy::Pool).unwrap();
        map.claim(B, r(64, 0, 64, 256), Policy::Persist).unwrap();
        map.set_restore(A, restore);
        map.set_restore(B, restore);
        map.borrow(C, r(32, 0, 64, 256), Policy::Level).unwrap();
        assert!(map.is_displaced(A) && map.is_displaced(B));
        map.end_borrow(C);
        assert!(!map.is_displaced(A) && !map.is_displaced(B) && !map.is_live(C));
        // A (slot 0) restored before B (slot 1).
        assert_eq!(ORDER.load(Ordering::Relaxed), 12);
    }

    #[test]
    fn stacked_borrows_restore_only_when_nothing_covers_the_lender() {
        let mut map = VramMap::<4>::new();
        map.claim(A, r(0, 0, 64, 64), Policy::Persist).unwrap();
        map.ensure(A, r(0, 0, 64, 64), Policy::Persist, 5).unwrap();
        map.borrow(B, r(0, 0, 32, 32), Policy::Level).unwrap();
        map.borrow(C, r(16, 16, 32, 32), Policy::Level).unwrap();
        map.end_borrow(B);
        assert!(map.is_displaced(A), "C still covers A");
        map.end_borrow(C);
        assert!(!map.is_displaced(A));
        // No restore callback: the key is reset so the loader uploads again.
        assert_eq!(
            map.ensure(A, r(0, 0, 64, 64), Policy::Persist, 5),
            Ok(Ensure::Miss)
        );
        assert_eq!(
            map.ensure(A, r(0, 0, 64, 64), Policy::Persist, 5),
            Ok(Ensure::Hit)
        );
    }

    #[test]
    fn policies_decide_what_a_level_change_keeps() {
        let mut map = VramMap::<4>::new();
        let level = r(0, 0, 64, 64);
        let kept = r(64, 0, 64, 64);
        let pool = r(128, 0, 64, 64);
        assert_eq!(map.ensure(A, level, Policy::Level, 1), Ok(Ensure::Miss));
        assert_eq!(map.ensure(B, kept, Policy::Persist, 1), Ok(Ensure::Miss));
        assert_eq!(map.ensure(C, pool, Policy::Pool, 1), Ok(Ensure::Miss));
        assert_eq!(map.ensure(A, level, Policy::Level, 1), Ok(Ensure::Hit));
        map.reset_scene();
        assert_eq!(map.ensure(A, level, Policy::Level, 1), Ok(Ensure::Miss));
        assert_eq!(map.ensure(B, kept, Policy::Persist, 1), Ok(Ensure::Hit));
        assert_eq!(map.ensure(B, kept, Policy::Persist, 2), Ok(Ensure::Miss));
        // A pool ignores the key: occupants belong to the allocator inside it.
        assert_eq!(map.ensure(C, pool, Policy::Pool, 99), Ok(Ensure::Hit));
    }

    #[test]
    fn loading_model_is_a_one_word_change() {
        // The same loader under Level (whole-level) and Persist (pre-load).
        fn run(policy: Policy) -> u32 {
            let mut map = VramMap::<2>::new();
            let atlas = r(320, 0, 256, 256);
            let mut uploads = 0;
            for level_key in [7, 7, 8] {
                map.reset_scene();
                if map.ensure(A, atlas, policy, level_key) == Ok(Ensure::Miss) {
                    uploads += 1;
                }
            }
            uploads
        }
        assert_eq!(run(Policy::Level), 3);
        assert_eq!(run(Policy::Persist), 2);
    }

    #[test]
    fn checkpoint_is_a_copy() {
        let mut map = VramMap::<4>::new();
        map.claim(A, r(0, 0, 8, 8), Policy::Persist).unwrap();
        let checkpoint = map;
        map.claim(B, r(8, 0, 8, 8), Policy::Persist).unwrap();
        map = checkpoint;
        assert!(map.is_live(A) && !map.is_live(B));
    }
}
