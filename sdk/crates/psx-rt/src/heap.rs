//! Bump allocation.
//!
//! [`Bump`] is the allocator itself: a range, a tip, no free. [`alloc_raw`], [`resize_tip`] and
//! [`remaining`] work on the program's one heap, the range between `__heap_start` and
//! `__heap_end` that `psoxide.ld` leaves above `.bss`, and need no cargo feature: a program that
//! brings its own allocation API (the restored games' libc layer) calls them without enabling
//! `alloc`, which would pull in `build-std` `alloc`. On the guest the heap's state is initialised
//! by the linker (the address of `__heap_start` and `__heap_end`, in `.data`), so there is no
//! seeding code and no test for it; a program that never calls these links none of this.
//!
//! The `alloc` feature adds a `GlobalAlloc` over the same heap, seeded by `_start`, for homebrew
//! that wants `Vec` and `Box`. It never frees, so it suits a permanent arena for assets and
//! scratch buffers; replace it with a real allocator (`linked_list_allocator`, `talc`, …) when
//! a program needs deallocation.
//!
//! The last block can change size in place: when a block ends where the heap's tip is, growing it
//! moves the tip and keeps the pointer, so a `Vec` that doubles stops abandoning its old buffer
//! at every step ([`resize_tip`], and `realloc` of the `GlobalAlloc`).

use core::cell::UnsafeCell;

/// A bump allocator over `start..end`: a tip that only moves up, except that the block ending at
/// the tip may grow or shrink in place ([`resize_tip`](Self::resize_tip)). Nothing is freed.
#[derive(Clone, Copy, Debug)]
pub struct Bump {
    // Addresses, held as pointers so that a static can start out as the linker's symbols (an
    // address is not an integer until the program runs). Never dereferenced: blocks are handed
    // out as `usize` arithmetic on them.
    next: *mut u8,
    end: *mut u8,
}

// SAFETY: a `Bump` is two addresses and owns no memory; moving it to another thread is what
// moving the two integers it used to hold was.
unsafe impl Send for Bump {}

impl Bump {
    /// An allocator with no memory: every allocation fails.
    pub const EMPTY: Self = Self::new(0, 0);

    /// An allocator over `start..end`.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            next: start as *mut u8,
            end: end as *mut u8,
        }
    }

    /// A block of `size` bytes aligned to `align` (a power of two), or null when it does not fit.
    /// The block is not cleared.
    #[inline(always)]
    pub fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        // Checked: on the 32-bit guest a size near `isize::MAX` (legal for a safe caller) would
        // otherwise wrap past the `end` test and hand out a block running off the heap.
        let Some(end) = (self.next as usize)
            .checked_add(align - 1)
            .map(|bumped| bumped & !(align - 1))
            .and_then(|aligned| aligned.checked_add(size))
        else {
            return core::ptr::null_mut();
        };
        let aligned = end - size;
        if end > self.end as usize {
            return core::ptr::null_mut();
        }
        self.next = end as *mut u8;
        aligned as *mut u8
    }

    /// Resize the block at `ptr`, `old_size` bytes, to `new_size` in place when it is the last
    /// one handed out (its end is the tip). Returns whether it was: on `false` nothing changed
    /// (the block is not the last, or the new end does not fit). Growing moves the tip up and
    /// the new bytes are not cleared; shrinking gives the tail back to the next allocation.
    ///
    /// # Safety
    /// `ptr` and `old_size` are a block this allocator returned (the size it was asked for, or
    /// since resized to), still owned by the caller.
    #[inline(always)]
    pub unsafe fn resize_tip(&mut self, ptr: *mut u8, old_size: usize, new_size: usize) -> bool {
        let start = ptr as usize;
        if start.wrapping_add(old_size) != self.next as usize {
            return false;
        }
        match start.checked_add(new_size) {
            Some(end) if end <= self.end as usize => {
                self.next = end as *mut u8;
                true
            }
            _ => false,
        }
    }

    /// Bytes left between the tip and the end.
    #[must_use]
    pub fn remaining(&self) -> usize {
        (self.end as usize).saturating_sub(self.next as usize)
    }
}

/// The program's heap: the `GlobalAlloc` under the `alloc` feature, the state behind
/// [`alloc_raw`] either way.
struct Allocator {
    state: UnsafeCell<Bump>,
}

// SAFETY: `state` is reached only through the functions below and the `GlobalAlloc` impl. The
// PS1 has one CPU and no threads, and nothing here masks interrupts, so the real invariant is
// that no interrupt handler allocates: psx-rt's exception handler is pure assembly and never
// does. A game-installed handler that allocated could interleave with an in-progress call and
// break this.
unsafe impl Sync for Allocator {}

#[cfg(target_arch = "mips")]
extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

/// Only registered as *the* allocator when targeting PS1 hardware with the `alloc` feature, for
/// the same reason the panic handler is (see [`crate`]): on the host it would span `0..0` and fail
/// the first allocation any linking test harness makes. Host builds use `std`'s allocator.
///
/// On the guest the state starts as the linker's heap range, `__heap_start..__heap_end`. When
/// `.bss` ends in the 0x100-byte window above `__heap_end` the start lies past the end: that
/// leaves no heap (every allocation fails, `remaining` is 0), not a wrapped one.
#[cfg_attr(all(target_arch = "mips", feature = "alloc"), global_allocator)]
static ALLOCATOR: Allocator = Allocator {
    state: UnsafeCell::new(INITIAL),
};

#[cfg(target_arch = "mips")]
const INITIAL: Bump = Bump {
    next: &raw const __heap_start as *mut u8,
    end: &raw const __heap_end as *mut u8,
};
#[cfg(not(target_arch = "mips"))]
const INITIAL: Bump = Bump::EMPTY;

/// The heap's state.
///
/// # Safety
/// Single-threaded access per the `Sync` impl: no other reference to the state is live.
#[inline(always)]
unsafe fn state() -> &'static mut Bump {
    // SAFETY: the caller upholds this fn's `# Safety`.
    unsafe { &mut *ALLOCATOR.state.get() }
}

/// A block of `size` bytes aligned to `align` (a power of two) from the program's heap, or null
/// when it does not fit. Not cleared, never freed.
///
/// The heap is `__heap_start..__heap_end`. With the `alloc` feature the same heap is the global
/// allocator's, so a block from here and a `Vec` share it.
///
/// Inlined: the one body a program with its own allocation API wraps (`malloc(size)` is
/// `alloc_raw(size, 8)`) folds the alignment to a constant, which is smaller than the shared
/// body called with a variable one.
#[must_use]
#[inline]
pub fn alloc_raw(size: usize, align: usize) -> *mut u8 {
    // SAFETY: single-threaded foreground access per the `Sync` impl.
    unsafe { state().alloc(size, align) }
}

/// Resize the last block taken from the program's heap in place; see [`Bump::resize_tip`].
/// Returns whether `ptr` was the last block and the new size fitted.
///
/// # Safety
/// `ptr` and `old_size` are a block from [`alloc_raw`] (or the global allocator), still owned by
/// the caller.
#[inline(never)]
pub unsafe fn resize_tip(ptr: *mut u8, old_size: usize, new_size: usize) -> bool {
    // SAFETY: single-threaded access per the `Sync` impl; the block is the caller's per `# Safety`.
    unsafe { state().resize_tip(ptr, old_size, new_size) }
}

/// Bytes left in the program's heap.
#[must_use]
#[inline(never)]
pub fn remaining() -> usize {
    // SAFETY: single-threaded foreground access per the `Sync` impl.
    unsafe { state().remaining() }
}

/// Diag builds: when the program's heap has fewer than `floor` bytes left, call `report` with
/// what is left; it does not return (a fatal screen). Call it right after a scene or level load,
/// with the game's floor, so running short of RAM fails at the load in a headless gate instead
/// of later in play.
///
/// Only with the `ram-floor` feature (a `[diag]` tap); without it the call compiles to nothing
/// and the program is byte-identical.
#[inline(always)]
pub fn check_floor(floor: usize, report: fn(usize) -> !) {
    #[cfg(feature = "ram-floor")]
    {
        let left = remaining();
        if left < floor {
            report(left);
        }
    }
    #[cfg(not(feature = "ram-floor"))]
    let _ = (floor, report);
}

/// Seed the allocator from `start`, spanning `size` bytes.
///
/// # Safety
/// Called exactly once from [`crate::_start`] with a heap range that
/// doesn't overlap anything in use.
pub unsafe fn init(start: usize, size: usize) {
    // SAFETY: the caller upholds this fn's `# Safety` (once, from `_start`, before any
    // allocation), so no other reference to the state exists.
    unsafe {
        *ALLOCATOR.state.get() = Bump::new(start, start + size);
    }
}

#[cfg(feature = "alloc")]
mod global {
    use super::Allocator;
    use core::alloc::{GlobalAlloc, Layout};

    // SAFETY: `alloc` returns null or a block aligned to `layout.align()` inside the range handed
    // to `init`; the tip only grows, except that the block ending at it may change size in place
    // (`realloc`), so blocks never overlap, and `dealloc` never reuses memory.
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: single-threaded access per the `Sync` impl, so this is the only live
            // reference to the state.
            unsafe { (*self.state.get()).alloc(layout.size(), layout.align()) }
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
            // Bump allocator -- nothing to release until a full reset.
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: single-threaded access per the `Sync` impl; `ptr` and `layout` are a live
            // block of this allocator per `GlobalAlloc::realloc`'s contract.
            unsafe {
                if (*self.state.get()).resize_tip(ptr, layout.size(), new_size) {
                    return ptr;
                }
                // SAFETY: the caller guarantees `new_size` rounded up to `align` does not
                // overflow `isize`.
                let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
                let new = self.alloc(new_layout);
                if !new.is_null() {
                    core::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                }
                new
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 256 bytes of host memory, aligned like the guest heap; `range` is valid while it lives
    /// and stays where it is.
    #[repr(align(16))]
    struct Arena([u8; 256]);

    impl Arena {
        fn range(&mut self) -> (usize, usize) {
            let start = self.0.as_mut_ptr() as usize;
            (start, start + 256)
        }
    }

    #[test]
    fn alloc_aligns_and_fails_past_the_end() {
        let mut arena = Arena([0; 256]);
        let (start, end) = arena.range();
        let mut b = Bump::new(start, end);
        let a = b.alloc(3, 1);
        assert_eq!(a as usize, start);
        let c = b.alloc(4, 16);
        assert_eq!(c as usize % 16, 0);
        assert!(c as usize >= start + 3);
        assert!(b.alloc(usize::MAX - 8, 4).is_null());
        assert!(b.alloc(10_000, 1).is_null());
        let left = b.remaining();
        assert!(!b.alloc(left, 1).is_null());
        assert_eq!(b.remaining(), 0);
        assert!(b.alloc(1, 1).is_null());
        assert!(!b.alloc(0, 1).is_null());
    }

    #[test]
    fn the_last_block_grows_in_place() {
        let mut arena = Arena([0; 256]);
        let (start, end) = arena.range();
        let mut b = Bump::new(start, end);
        let p = b.alloc(16, 4);
        // SAFETY: `p` and 16 are the block just allocated.
        unsafe {
            assert!(b.resize_tip(p, 16, 40));
            assert_eq!(b.remaining(), 256 - 40);
            // The next block starts after the grown one.
            let q = b.alloc(8, 1);
            assert_eq!(q as usize, p as usize + 40);
            // `p` is no longer the last block: it cannot grow, and nothing moved.
            assert!(!b.resize_tip(p, 40, 48));
            assert_eq!(b.remaining(), 256 - 48);
            // The last block cannot grow past the end, and a refusal changes nothing.
            assert!(!b.resize_tip(q, 8, 8 + 256));
            assert_eq!(b.remaining(), 256 - 48);
            assert!(b.resize_tip(q, 8, 8 + (256 - 48)));
            assert_eq!(b.remaining(), 0);
        }
    }

    #[test]
    fn the_last_block_shrinks_in_place() {
        let mut arena = Arena([0; 256]);
        let (start, end) = arena.range();
        let mut b = Bump::new(start, end);
        let p = b.alloc(100, 1);
        // SAFETY: `p` and 100 are the block just allocated.
        unsafe { assert!(b.resize_tip(p, 100, 10)) };
        assert_eq!(b.remaining(), 256 - 10);
        assert_eq!(b.alloc(1, 1) as usize, start + 10);
    }

    /// The global heap's raw entry (one test: the state is a static).
    #[test]
    fn the_program_heap_serves_raw_blocks_and_grows_the_last() {
        let mut arena = Arena([0; 256]);
        let (start, end) = arena.range();
        // SAFETY: once in this test binary, before any use of the heap.
        unsafe { init(start, end - start) };
        let a = alloc_raw(10, 4);
        assert_eq!(a as usize, start);
        assert_eq!(remaining(), 246);
        // SAFETY: `a`, 10 is the block just taken.
        unsafe {
            assert!(resize_tip(a, 10, 100));
            assert!(!resize_tip(a, 10, 100));
        }
        assert_eq!(remaining(), 156);
        let b = alloc_raw(4, 4);
        assert_eq!(b as usize, start + 100);

        // The floor check: 152 bytes left passes a floor of 152 and stops below 153.
        fn short(left: usize) -> ! {
            panic!("heap short: {left}");
        }
        check_floor(152, short);
        #[cfg(feature = "ram-floor")]
        {
            extern crate std;
            let stopped = std::panic::catch_unwind(|| check_floor(153, short));
            assert!(stopped.is_err());
        }
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn realloc_grows_the_last_block_in_place_and_copies_otherwise() {
        use core::alloc::{GlobalAlloc, Layout};
        let mut arena = Arena([0; 256]);
        let (start, end) = arena.range();
        let al = Allocator {
            state: UnsafeCell::new(Bump::new(start, end)),
        };
        let l = |n| Layout::from_size_align(n, 4).unwrap();
        // SAFETY: the blocks are this allocator's own and live.
        unsafe {
            let a = al.alloc(l(8));
            a.write_bytes(7, 8);
            let a2 = al.realloc(a, l(8), 16);
            assert_eq!(a2, a, "the last block grows without moving");
            let b = al.alloc(l(8));
            // `a` is no longer last: it moves and keeps its bytes.
            let a3 = al.realloc(a2, l(16), 32);
            assert_ne!(a3, a2);
            assert_eq!(core::slice::from_raw_parts(a3, 8), &[7u8; 8]);
            // A grow that cannot fit returns null and leaves the block alone.
            assert!(al.realloc(a3, l(32), 10_000).is_null());
            let _ = b;
            // A Vec-like doubling at the tip never advances more than the final size.
            let mut p = al.alloc(l(4));
            let mut cap = 4;
            while cap < 64 {
                let q = al.realloc(p, l(cap), cap * 2);
                assert_eq!(q, p);
                p = q;
                cap *= 2;
            }
        }
    }
}
