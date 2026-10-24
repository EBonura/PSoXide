// SPDX-License-Identifier: GPL-2.0-or-later
//! Ownership of a VRAM layout that is known at build time.
//!
//! [`crate::map::VramMap`] is the general ledger: regions are claimed at run time with rectangles
//! the game computes, which a streaming pool needs. Most games do not: their framebuffers, texture
//! bands, HUD pages and the pictures a menu or a cutscene draws over them are all constants. For
//! those, this module moves the work to compile time.
//!
//! * [`Layout`] is a `const` table of one rectangle per [`Owner`] and, computed while the table is
//!   built, which owners' rectangles overlap. A pair that is meant to coexist can be asserted
//!   disjoint in a `const _: () = assert!(..)`, so an overlap fails the build, not one map on one
//!   disc.
//! * [`Ledger`] is the run-time state: which owners are live, which a loan has displaced, and how
//!   many uploads went to the wrong place. Twelve bytes, no heap, and every operation is a few mask
//!   instructions (about 250 bytes of code in all).
//!
//! The vocabulary is the same as the map's: an owner [`claim`](Ledger::claim)s its region; a scene
//! that draws over another owner's region [`borrow`](Ledger::borrow)s it, which marks the owners
//! underneath displaced; [`end`](Ledger::end) hands them back and says which of them lost their
//! pixels and need uploading again; every upload asks [`permit`](Ledger::permit) first. Release
//! builds count a refused upload and let it through, debug builds assert.
//!
//! ```
//! use psx_vram::layout::{Layout, Ledger};
//! use psx_vram::map::Owner;
//! use psx_vram::VramRect;
//!
//! const ROOM: Owner = Owner::new(0);
//! const HUD: Owner = Owner::new(1);
//! const PANORAMA: Owner = Owner::new(2);
//! const LAYOUT: Layout<3> = Layout::new([
//!     VramRect::new(320, 0, 320, 256), // ROOM
//!     VramRect::new(960, 0, 64, 256),  // HUD
//!     VramRect::new(320, 0, 384, 256), // PANORAMA: over the room band
//! ]);
//! // The two that are always up are disjoint, checked at build time.
//! const _: () = assert!(LAYOUT.disjoint((1 << ROOM.index()) | (1 << HUD.index())));
//!
//! let mut ledger = Ledger::new();
//! ledger.claim(&LAYOUT, ROOM);
//! ledger.claim(&LAYOUT, HUD);
//! assert!(ledger.permit(&LAYOUT, ROOM, VramRect::new(320, 0, 64, 64)));
//!
//! ledger.borrow(&LAYOUT, PANORAMA);
//! // The panorama is on top of the room; uploads to it would be refused.
//! assert_eq!(ledger.displaced(), 1 << ROOM.index());
//! let restore = ledger.end(&LAYOUT, 1 << PANORAMA.index());
//! // The room needs its pixels again; the HUD was never touched.
//! assert_eq!(restore, 1 << ROOM.index());
//! ```

use crate::map::Owner;
use crate::VramRect;

/// The rectangles of up to 32 owners and which of them overlap, fixed at build time.
pub struct Layout<const N: usize> {
    /// Each owner's corners `[x0, y0, x1, y1]`, so a containment test needs no sums.
    corners: [[u16; 4]; N],
    /// Bit `j` of `overlaps[i]` is set when owner `i`'s rectangle and owner `j`'s share a pixel
    /// (`i != j`).
    overlaps: [u32; N],
}

impl<const N: usize> Layout<N> {
    /// Build the table; `rects[n]` is owner `n`'s region. Fails the build for more than 32 owners.
    pub const fn new(rects: [VramRect; N]) -> Self {
        assert!(N <= 32, "a layout holds at most 32 owners");
        let mut corners = [[0u16; 4]; N];
        let mut overlaps = [0u32; N];
        let mut i = 0;
        while i < N {
            corners[i] = [
                rects[i].x,
                rects[i].y,
                rects[i].x + rects[i].w,
                rects[i].y + rects[i].h,
            ];
            let mut j = 0;
            while j < N {
                if i != j && rects[i].overlaps(rects[j]) {
                    overlaps[i] |= 1 << j;
                }
                j += 1;
            }
            i += 1;
        }
        Self { corners, overlaps }
    }

    /// Whether the owners in `group` (bit `n` = owner `n`) are pairwise disjoint. `const`, for an
    /// assertion.
    pub const fn disjoint(&self, group: u32) -> bool {
        let mut i = 0;
        while i < N {
            if group & (1 << i) != 0 && self.overlaps[i] & group != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The owner's rectangle.
    pub const fn rect(&self, owner: Owner) -> VramRect {
        let c = self.corners[owner.index()];
        VramRect {
            x: c[0],
            y: c[1],
            w: c[2] - c[0],
            h: c[3] - c[1],
        }
    }

    /// The owners whose regions overlap `owner`'s, as a bit set.
    pub const fn overlapping(&self, owner: Owner) -> u32 {
        self.overlaps[owner.index()]
    }
}

/// The run-time state of a [`Layout`]: twelve bytes.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct Ledger {
    live: u32,
    displaced: u32,
    violations: u16,
    last_violation: u8,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger {
    /// Nothing live.
    pub const fn new() -> Self {
        Self {
            live: 0,
            displaced: 0,
            violations: 0,
            last_violation: 0,
        }
    }

    /// The owners in `live` (bit `n` = owner `n`) live from the start: the regions that exist for
    /// the whole run, so a `static` needs no start-up code to claim them.
    pub const fn with_live(live: u32) -> Self {
        Self {
            live,
            ..Self::new()
        }
    }

    /// Violations seen so far: uploads to a region that is not the writer's, or that a loan
    /// displaced, and claims over a live region.
    pub const fn violations(&self) -> u16 {
        self.violations
    }

    /// The most recent violator (meaningful when [`violations`](Self::violations) is not zero).
    pub const fn last_violation(&self) -> Owner {
        Owner::new(self.last_violation)
    }

    /// Whether the owner is live.
    pub const fn is_live(&self, owner: Owner) -> bool {
        self.live & (1 << owner.index()) != 0
    }

    /// The live owners whose pixels a loan took and [`end`](Self::end) has not handed back, as a
    /// bit set.
    pub const fn displaced(&self) -> u32 {
        self.displaced
    }

    #[cold]
    #[inline(never)]
    #[optimize(size)]
    fn violation(&mut self, owner: Owner) {
        self.violations = self.violations.saturating_add(1);
        self.last_violation = owner.index() as u8;
        debug_assert!(false, "vram ownership violation by owner {}", owner.index());
    }

    /// The owner's region becomes live. A live region that overlaps it is a violation (a scene that
    /// means to draw over another owner's region [`borrow`](Self::borrow)s).
    #[inline(never)]
    #[optimize(size)]
    pub fn claim<const N: usize>(&mut self, layout: &Layout<N>, owner: Owner) {
        if self.live & layout.overlaps[owner.index()] != 0 {
            self.violation(owner);
        }
        self.live |= 1 << owner.index();
    }

    /// The owner's region becomes live on top of whatever overlaps it: those owners are displaced,
    /// their uploads refused, until [`end`](Self::end).
    #[inline(never)]
    #[optimize(size)]
    pub fn borrow<const N: usize>(&mut self, layout: &Layout<N>, owner: Owner) {
        self.displaced |= self.live & layout.overlaps[owner.index()];
        self.live |= 1 << owner.index();
    }

    /// The owners in `ending` (bit `n` = owner `n`) stop being live. Returns the owners a loan had
    /// displaced that nothing still covers: they are usable again, and their pixels are the
    /// caller's to upload.
    #[inline(never)]
    #[optimize(size)]
    pub fn end<const N: usize>(&mut self, layout: &Layout<N>, ending: u32) -> u32 {
        self.live &= !ending;
        self.displaced &= !ending;
        let layout = core::hint::black_box(layout);
        let mut covered = 0;
        let mut live = self.live;
        let mut i = 0;
        while live != 0 {
            if live & 1 != 0 {
                covered |= layout.overlaps[i];
            }
            live >>= 1;
            i += 1;
        }
        let restore = self.displaced & !covered;
        self.displaced &= covered;
        restore
    }

    /// The gate every upload passes: `true` when `owner` is live, no loan displaced it and `rect`
    /// lies inside its region. A `false` is counted, and asserts in a debug build; the caller
    /// may still go ahead.
    #[inline(never)]
    #[optimize(size)]
    pub fn permit<const N: usize>(
        &mut self,
        layout: &Layout<N>,
        owner: Owner,
        rect: VramRect,
    ) -> bool {
        let bit = 1 << owner.index();
        let own = layout.corners[owner.index()];
        let ok = (self.live & !self.displaced) & bit != 0
            && own[0] <= rect.x
            && own[1] <= rect.y
            && rect.x + rect.w <= own[2]
            && rect.y + rect.h <= own[3];
        if !ok {
            self.violation(owner);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Owner = Owner::new(0);
    const B: Owner = Owner::new(1);
    const C: Owner = Owner::new(2);
    const D: Owner = Owner::new(3);
    const L: Layout<4> = Layout::new([
        VramRect::new(0, 0, 64, 64),
        VramRect::new(64, 0, 64, 64),
        VramRect::new(32, 0, 64, 32), // over both
        VramRect::new(0, 64, 64, 64),
    ]);

    fn bit(o: Owner) -> u32 {
        1 << o.index()
    }

    #[test]
    fn overlaps_are_computed_at_build_time() {
        assert_eq!(L.overlapping(A), bit(C));
        assert_eq!(L.overlapping(B), bit(C));
        assert_eq!(L.overlapping(C), bit(A) | bit(B));
        assert_eq!(L.overlapping(D), 0);
        const _: () = assert!(L.disjoint(0b1011));
        assert!(!L.disjoint(0b0111));
    }

    #[test]
    fn a_loan_displaces_and_end_names_what_to_restore() {
        let mut l = Ledger::with_live(bit(A) | bit(B) | bit(D));
        l.borrow(&L, C);
        assert_eq!(l.displaced(), bit(A) | bit(B));
        assert_eq!(l.end(&L, bit(C)), bit(A) | bit(B));
        assert_eq!(l.displaced(), 0);
        assert!(l.permit(&L, A, VramRect::new(0, 0, 8, 8)));
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn uploads_outside_the_writers_region_are_counted() {
        let mut l = Ledger::with_live(bit(A) | bit(B));
        assert!(l.permit(&L, A, VramRect::new(0, 0, 64, 64)));
        assert!(!l.permit(&L, A, VramRect::new(0, 0, 65, 64)));
        assert!(
            !l.permit(&L, C, VramRect::new(32, 0, 8, 8)),
            "C is not live"
        );
        l.borrow(&L, C);
        assert!(
            !l.permit(&L, A, VramRect::new(0, 0, 8, 8)),
            "A is displaced"
        );
        assert_eq!(l.violations(), 3);
        assert_eq!(l.last_violation(), A);
    }

    #[test]
    fn end_restores_only_what_nothing_covers() {
        let mut l = Ledger::with_live(bit(A) | bit(B));
        l.borrow(&L, C);
        // A second loan over A keeps A displaced when the first one ends.
        const L2: Layout<4> = Layout::new([
            VramRect::new(0, 0, 64, 64),
            VramRect::new(64, 0, 64, 64),
            VramRect::new(32, 0, 64, 32),
            VramRect::new(0, 0, 16, 16),
        ]);
        let mut l2 = Ledger::with_live(bit(A) | bit(B));
        l2.borrow(&L2, C);
        l2.borrow(&L2, D);
        assert_eq!(l2.end(&L2, bit(C)), bit(B), "D still covers A");
        assert_eq!(l2.end(&L2, bit(D)), bit(A));
        assert_eq!(l2.displaced(), 0);
        assert_eq!(l.end(&L, bit(C)), bit(A) | bit(B));
        assert!(l.permit(&L, A, VramRect::new(0, 0, 8, 8)));
        assert_eq!(l.end(&L, bit(C)), 0, "ending twice changes nothing");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn claiming_over_a_live_region_is_a_violation() {
        let mut l = Ledger::with_live(bit(A));
        l.claim(&L, C);
        assert_eq!(l.violations(), 1);
        let mut m = Ledger::new();
        m.claim(&L, A);
        m.claim(&L, D);
        assert_eq!(m.violations(), 0);
    }

    #[test]
    fn a_ledger_is_twelve_bytes() {
        assert_eq!(core::mem::size_of::<Ledger>(), 12);
    }
}
