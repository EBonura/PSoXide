//! Bump allocator gated behind the `alloc` feature.
//!
//! A tiny `GlobalAlloc` that never frees -- fine for PS1 homebrew that
//! uses a permanent arena for assets and scratch buffers. Replace
//! with a real allocator (`linked_list_allocator`, `talc`, …) when the
//! engine needs deallocation.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

struct BumpAllocator {
    state: UnsafeCell<BumpState>,
}

struct BumpState {
    next: usize,
    end: usize,
}

// SAFETY: `state` is reached only through `alloc` and `init`. The PS1 has one
// CPU and no threads, and `alloc` does not mask interrupts, so the real
// invariant is that no interrupt handler allocates: psx-rt's exception handler
// is pure assembly and never does. A game-installed handler that allocated
// could interleave with an in-progress `alloc` and break this.
unsafe impl Sync for BumpAllocator {}

// SAFETY: `alloc` returns null or a block aligned to `layout.align()` inside
// the range handed to `init`; `next` only grows, so blocks never overlap, and
// `dealloc` never reuses memory. Subject to the overflow caveat in `alloc`.
unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: single-threaded access per the `Sync` impl, so this is the
        // only live reference to the state. NOT checked: on the 32-bit guest
        // `aligned + size` wraps for a layout near `isize::MAX` (legal for a
        // safe caller) and then passes the `end` test, returning a block that
        // runs off the heap.
        unsafe {
            let state = &mut *self.state.get();
            let align = layout.align();
            let size = layout.size();
            let aligned = (state.next + align - 1) & !(align - 1);
            let end = aligned + size;
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

/// Only registered as *the* allocator when targeting PS1 hardware, for the
/// same reason the panic handler is (see [`crate`]): only [`crate::_start`]
/// seeds it, so on the host it would be installed still spanning
/// `0..0` and fail the first allocation any linking test harness makes.
/// Host builds use `std`'s allocator. The cfg is true for every guest build,
/// so the PS1 artifact is unaffected.
#[cfg_attr(target_arch = "mips", global_allocator)]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    state: UnsafeCell::new(BumpState { next: 0, end: 0 }),
};

/// Seed the allocator from `start`, spanning `size` bytes.
///
/// # Safety
/// Called exactly once from [`crate::_start`] with a heap range that
/// doesn't overlap anything in use.
pub unsafe fn init(start: usize, size: usize) {
    // SAFETY: the caller upholds this fn's `# Safety` (once, from `_start`,
    // before any allocation), so no other reference to the state exists.
    unsafe {
        let state = &mut *ALLOCATOR.state.get();
        state.next = start;
        state.end = start + size;
    }
}
