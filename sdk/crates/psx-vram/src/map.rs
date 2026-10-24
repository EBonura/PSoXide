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
//! * RAM: `16 * N + 4` bytes. An all-zero map is empty ([`VramMap::new`]), so a game keeps it in
//!   `.bss`.
//! * One slot per [`Owner`]; the owner id is the table index, so the hot calls
//!   ([`VramMap::permit`], [`VramMap::ensure`]) never scan. Only claims and borrows scan the table
//!   (`N` compares), and those happen at scene or level boundaries, never per frame.
//! * Debug builds assert on every violation; release builds only count them
//!   ([`VramMap::violations`]) and still let the upload through, so a counter can never change
//!   shipped behaviour.
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

/// One owner's region. The rectangle is kept as its corners so the overlap and containment tests
/// (the hot ones) need no sums.
#[derive(Copy, Clone)]
#[repr(C)]
struct Slot<const N: usize> {
    x0: u16,
    y0: u16,
    x1: u16,
    y1: u16,
    restore: Option<RestoreFn<N>>,
    key: u16,
    flags: u8,
    policy: Policy,
}

impl<const N: usize> Slot<N> {
    const EMPTY: Self = Self {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
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

    const fn rect(&self) -> VramRect {
        VramRect {
            x: self.x0,
            y: self.y0,
            w: self.x1 - self.x0,
            h: self.y1 - self.y0,
        }
    }

    /// Whether this region shares a pixel with the corners `[x0, x1) x [y0, y1)`.
    #[inline(always)]
    const fn overlaps(&self, c: &Corners) -> bool {
        self.x0 < c.x1 && c.x0 < self.x1 && self.y0 < c.y1 && c.y0 < self.y1
    }
}

/// A rectangle as its corners.
#[derive(Copy, Clone)]
struct Corners {
    x0: u16,
    y0: u16,
    x1: u16,
    y1: u16,
}

impl Corners {
    const fn of(rect: VramRect) -> Self {
        Self {
            x0: rect.x,
            y0: rect.y,
            x1: rect.x + rect.w,
            y1: rect.y + rect.h,
        }
    }
}

/// The ownership ledger. `N` is the number of distinct owners (at most 256).
///
/// `Copy` on purpose: `let checkpoint = map;` / `map = checkpoint;` is the
/// checkpoint and restore of a pre-load pool. `repr(C)`: `N` slots of 16 bytes, then the
/// violation count (`u16`) and the last violator (`u8`), so a debugger or a test gate can
/// read a map in RAM by offset.
#[derive(Copy, Clone)]
#[repr(C)]
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

    #[cold]
    #[inline(never)]
    #[optimize(size)]
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
        slot.live().then(|| slot.rect())
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
        let probe = Corners::of(VramRect { x, y, w: 1, h: 1 });
        (0..N)
            .rev()
            .find(|&i| self.slots[i].live() && self.slots[i].overlaps(&probe))
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
    #[optimize(size)]
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
        let mut clash = None;
        let c = Corners::of(rect);
        let slots = core::hint::black_box(&mut self.slots[..]);
        for (i, other) in slots.iter_mut().enumerate() {
            if i != index && other.flags & LIVE != 0 && other.overlaps(&c) {
                if !borrowing {
                    clash = Some(i);
                    break;
                }
                other.flags |= DISPLACED;
            }
        }
        if let Some(i) = clash {
            self.note_violation(owner);
            debug_assert!(false, "vram owner {index} overlaps owner {i}");
            return Err(ClaimError::Overlap(Owner(i as u8)));
        }
        let slot = &mut self.slots[index];
        slot.x0 = c.x0;
        slot.y0 = c.y0;
        slot.x1 = c.x1;
        slot.y1 = c.y1;
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

    /// End a loan: release the borrower, then run the restore callback of every
    /// displaced region that no live region overlaps any more, in owner-number
    /// order (or reset its key when it has none, so the next
    /// [`ensure`](Self::ensure) misses). A callback may write its own region
    /// (it is permitted again); regions the same loan displaced that come later
    /// in owner order are still marked displaced when it runs.
    #[optimize(size)]
    pub fn end_borrow(&mut self, owner: Owner) {
        self.release(owner);
        self.restore_uncovered();
    }

    #[inline(never)]
    #[optimize(size)]
    fn restore_uncovered(&mut self) {
        let mut from = 0;
        while let Some(i) = self.take_uncovered(from) {
            match self.slots[i].restore {
                Some(restore) => restore(self, Owner(i as u8)),
                None => self.slots[i].key = NO_KEY,
            }
            from = i + 1;
        }
    }

    /// [`end_borrow`](Self::end_borrow) without callbacks, for games that
    /// re-upload from one place: returns the owners (bit n = owner n, the first
    /// 32) whose regions the loan displaced and nothing covers any more. They are
    /// usable again; their pixels are the caller's to put back.
    #[optimize(size)]
    pub fn end_borrow_mask(&mut self, owner: Owner) -> u32 {
        self.release(owner);
        let mut mask = 0;
        let mut from = 0;
        while let Some(i) = self.take_uncovered(from) {
            self.slots[i].key = NO_KEY;
            if i < 32 {
                mask |= 1 << i;
            }
            from = i + 1;
        }
        mask
    }

    /// The first displaced region at or after `from` that no other live region
    /// overlaps: marks it no longer displaced and returns its index.
    #[inline(never)]
    #[optimize(size)]
    fn take_uncovered(&mut self, from: usize) -> Option<usize> {
        take_uncovered(&mut self.slots, from)
    }

    /// Make `rect` the owner's region holding `key`. [`Ensure::Hit`] when it
    /// already does (same rectangle, live, not displaced, same key, or a
    /// [`Policy::Pool`] container), so the caller skips the upload;
    /// [`Ensure::Miss`] after (re)claiming it.
    #[optimize(size)]
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
                && slot.rect() == rect
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
    #[optimize(size)]
    pub fn release(&mut self, owner: Owner) {
        if let Some(slot) = self.slots.get_mut(owner.index()) {
            slot.flags = 0;
        }
    }

    /// Drop every [`Policy::Level`] region (a level or scene change).
    #[optimize(size)]
    pub fn reset_scene(&mut self) {
        drop_level(&mut self.slots);
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
    #[optimize(size)]
    pub fn permit(&mut self, owner: Owner, rect: VramRect) -> bool {
        let ok = self.slots.get(owner.index()).is_some_and(|slot| {
            let c = Corners::of(rect);
            slot.flags & (LIVE | DISPLACED) == LIVE
                && slot.x0 <= c.x0
                && slot.y0 <= c.y0
                && c.x1 <= slot.x1
                && c.y1 <= slot.y1
        });
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

// The scans take the table as a slice, out of line, and read it through `black_box`, so each
// compiles once and runs as a loop; written against the `[Slot; N]` array, or with the constant
// length propagated into them, they unroll N times (about 2 KB of code at N = 24 on the PS1).

/// The first displaced region at or after `from` that no other live region overlaps; clears its
/// displaced flag.
#[inline(never)]
#[optimize(size)]
fn take_uncovered<const N: usize>(slots: &mut [Slot<N>], from: usize) -> Option<usize> {
    let slots = core::hint::black_box(slots);
    for i in from..slots.len() {
        if slots[i].flags & (LIVE | DISPLACED) != LIVE | DISPLACED {
            continue;
        }
        let c = Corners {
            x0: slots[i].x0,
            y0: slots[i].y0,
            x1: slots[i].x1,
            y1: slots[i].y1,
        };
        let covered = slots
            .iter()
            .enumerate()
            .any(|(j, other)| j != i && other.flags & LIVE != 0 && other.overlaps(&c));
        if !covered {
            slots[i].flags = LIVE;
            return Some(i);
        }
    }
    None
}

/// Drop every [`Policy::Level`] region.
#[inline(never)]
#[optimize(size)]
fn drop_level<const N: usize>(slots: &mut [Slot<N>]) {
    let slots = core::hint::black_box(slots);
    for slot in slots.iter_mut() {
        if slot.policy == Policy::Level {
            slot.flags = 0;
        }
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
            // The callback uploads through the map: its own region is permitted
            // again by the time it runs.
            assert!(map.permit(owner, map.rect_of(owner).unwrap()));
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

    #[test]
    fn end_borrow_mask_names_the_regions_to_put_back() {
        let mut map = VramMap::<8>::new();
        map.claim(A, r(0, 0, 64, 64), Policy::Pool).unwrap();
        map.claim(B, r(64, 0, 64, 64), Policy::Pool).unwrap();
        map.claim(Owner::new(5), r(0, 64, 64, 64), Policy::Pool)
            .unwrap();
        map.borrow(C, r(32, 0, 64, 32), Policy::Persist).unwrap();
        assert_eq!(map.end_borrow_mask(C), 0b11);
        assert!(!map.is_displaced(A) && !map.is_displaced(B));
        assert_eq!(map.end_borrow_mask(C), 0, "a second end changes nothing");
    }
}
