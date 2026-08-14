//! Bump allocator gated behind the `alloc` feature.
//!
//! A tiny `GlobalAlloc` that never frees -- fine for PS1 homebrew that
//! uses a permanent arena for assets and scratch buffers. Replace
//! with a real allocator (`linked_list_allocator`, `talc`, …) when the
//! engine needs deallocation.

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

struct BumpAllocator {
    state: UnsafeCell<BumpState>,
}

struct BumpState {
    start: usize,
    next: usize,
    end: usize,
}

// Single-threaded environment (interrupts masked during alloc, no
// SMP on PS1) -- `Sync` is sound for the bump allocator.
unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let state = &mut *self.state.get();
            let align = layout.align();
            let size = layout.size();
            let Some(aligned) = state
                .next
                .checked_add(align - 1)
                .map(|next| next & !(align - 1))
            else {
                return core::ptr::null_mut();
            };
            let Some(end) = aligned.checked_add(size) else {
                return core::ptr::null_mut();
            };
            if end > state.end {
                return core::ptr::null_mut();
            }
            state.next = end;
            aligned as *mut u8
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator -- nothing to release until a full reset.
    }
}

#[cfg_attr(target_arch = "mips", global_allocator)]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    state: UnsafeCell::new(BumpState {
        start: 0,
        next: 0,
        end: 0,
    }),
};

/// Read-only snapshot of the monotonic bump allocator.
///
/// `next` is both the first never-issued byte and the heap high-water mark:
/// deallocation deliberately does not move it backwards. Alignment padding is
/// therefore included in [`used`](Self::used), exactly matching RAM that later
/// allocations can no longer consume.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HeapUsage {
    start: usize,
    next: usize,
    end: usize,
}

impl HeapUsage {
    const fn from_state(state: &BumpState) -> Self {
        Self {
            start: state.start,
            next: state.next,
            end: state.end,
        }
    }

    /// First byte in the linker-provided heap range.
    pub const fn start(self) -> usize {
        self.start
    }

    /// First never-issued byte, including alignment padding consumed so far.
    pub const fn next(self) -> usize {
        self.next
    }

    /// Exclusive end of the linker-provided heap range.
    pub const fn end(self) -> usize {
        self.end
    }

    /// Total linker-provided heap capacity.
    pub const fn capacity(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Monotonic high-water bytes, including allocator alignment padding.
    pub const fn used(self) -> usize {
        self.next.saturating_sub(self.start)
    }

    /// Bytes still available after the high-water mark.
    pub const fn remaining(self) -> usize {
        self.end.saturating_sub(self.next)
    }
}

/// Snapshot the current heap bounds and monotonic allocation high-water mark.
///
/// This does not allocate, mutate the allocator, or reclaim memory. The PS1
/// runtime is single-threaded and its interrupt handlers never allocate, so the
/// three machine-word reads cannot race an allocator call. Before [`init`] the
/// snapshot is all zeroes.
#[inline]
pub fn usage() -> HeapUsage {
    // SAFETY: the allocator's single-thread/no-allocation-in-IRQ contract is the
    // same contract that makes its `GlobalAlloc` implementation sound.
    unsafe { HeapUsage::from_state(&*ALLOCATOR.state.get()) }
}

/// Seed the allocator from `start`, spanning `size` bytes.
///
/// # Safety
/// Called exactly once from [`crate::_start`] with a heap range that
/// doesn't overlap anything in use.
pub unsafe fn init(start: usize, size: usize) {
    unsafe {
        let state = &mut *ALLOCATOR.state.get();
        state.start = start;
        state.next = start;
        state.end = start + size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_reports_exact_bounds_alignment_high_water_and_remaining_bytes() {
        unsafe { init(0x1000, 0x100) };
        assert_eq!(
            usage(),
            HeapUsage {
                start: 0x1000,
                next: 0x1000,
                end: 0x1100,
            }
        );
        assert_eq!(usage().capacity(), 0x100);
        assert_eq!(usage().used(), 0);
        assert_eq!(usage().remaining(), 0x100);

        let first = unsafe { ALLOCATOR.alloc(Layout::from_size_align(3, 8).unwrap()) };
        assert_eq!(first as usize, 0x1000);
        assert_eq!(usage().next(), 0x1003);
        let second = unsafe { ALLOCATOR.alloc(Layout::from_size_align(4, 16).unwrap()) };
        assert_eq!(second as usize, 0x1010);

        let snapshot = usage();
        assert_eq!(snapshot.start(), 0x1000);
        assert_eq!(snapshot.next(), 0x1014);
        assert_eq!(snapshot.end(), 0x1100);
        assert_eq!(snapshot.used(), 0x14);
        assert_eq!(snapshot.remaining(), 0xec);

        unsafe { ALLOCATOR.dealloc(first, Layout::from_size_align(3, 8).unwrap()) };
        assert_eq!(
            usage(),
            snapshot,
            "bump deallocation never lowers high-water"
        );

        unsafe { init(0x2000, 16) };
        let before = usage();
        let failed = unsafe { ALLOCATOR.alloc(Layout::from_size_align(17, 1).unwrap()) };
        assert!(failed.is_null());
        assert_eq!(usage(), before);
    }
}
