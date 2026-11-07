//! A heap for a game: a bump allocator that gives freed blocks back.
//!
//! The SDK's allocator (`psx_rt::heap`) never frees. Every growth step of a `Vec`
//! (4, 8, 16, ... entries) leaves its old buffer behind for good, so the room the
//! scratch lists of a level need grows with every new high-water mark a long play
//! reaches, and the heap that is left after the statics (a few KB on this game) runs
//! out. This one keeps a free list per size class: a block of 8, 16, 24, 32, 48, ... 2048
//! bytes that is freed is handed out again to the next request of its class, so the heap
//! stops growing once the live blocks have reached their sizes. Anything larger, or
//! aligned beyond 8 bytes, is carved off the end as before and never reused (a game
//! makes a handful of those, once).
//!
//! A game installs it as its `#[global_allocator]` (leaving `psx-rt`'s `alloc` feature
//! off) and calls [`ReuseHeap::init`] with `__heap_start..__heap_end` first thing in
//! `main`. It is meant to move to the SDK next to `psx_rt::heap`, whose own doc comment
//! asks for a real allocator "when the engine needs deallocation".

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

/// The block sizes, bytes: a power of two and the step halfway to the next, so that a block is
/// never much more than half as large again as what was asked for (a `Vec` doubling its way up uses
/// the powers). Every size is a multiple of [`ALIGN`].
const SIZES: [usize; 16] = [
    8, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048,
];
/// Size classes (free lists).
const CLASSES: usize = SIZES.len();
/// The alignment of every block of a class.
const ALIGN: usize = 8;
/// The largest block a free list serves.
const MAX_BLOCK: usize = SIZES[CLASSES - 1];

/// What the allocator keeps. `#[repr(C)]` so a RAM watch can read `next` and `end` at the
/// symbol's address: the heap used so far, and where it stops.
#[repr(C)]
struct State {
    /// The first byte not handed out yet (0 before the first allocation).
    next: usize,
    /// One past the last byte of the heap.
    end: usize,
    /// Per class, the first freed block (each holds the address of the next in its first
    /// word), or 0.
    free: [usize; CLASSES],
}

/// A bump allocator with a free list per size class.
pub struct ReuseHeap {
    state: UnsafeCell<State>,
}

// SAFETY: the guest has one CPU and no thread, and no interrupt handler allocates (the
// SDK's allocator rests on the same fact), so `state` is never reached twice at once.
unsafe impl Sync for ReuseHeap {}

impl ReuseHeap {
    /// An empty heap, with no room until [`Self::init`].
    pub const fn new() -> Self {
        Self {
            state: UnsafeCell::new(State {
                next: 0,
                end: 0,
                free: [0; CLASSES],
            }),
        }
    }

    /// Give the heap `start..end` to hand out.
    ///
    /// # Safety
    /// Called before the first allocation, with a range nothing else uses.
    pub unsafe fn init(&self, start: usize, end: usize) {
        // SAFETY: nothing else holds the state before the first allocation.
        let state = unsafe { &mut *self.state.get() };
        state.next = start;
        state.end = end.max(start);
    }

    /// The class (index into the free lists) serving `layout`, if it has one: the smallest
    /// block that holds it.
    fn class(layout: Layout) -> Option<usize> {
        if layout.align() > ALIGN || layout.size() > MAX_BLOCK {
            return None;
        }
        SIZES.iter().position(|&block| block >= layout.size())
    }

    /// Carve `size` bytes aligned to `align` off the end of the heap.
    fn bump(state: &mut State, size: usize, align: usize) -> *mut u8 {
        // Checked: on the 32-bit guest a size near `isize::MAX` (legal for a safe caller)
        // would otherwise wrap past the `end` test and hand out a block off the heap.
        let Some(start) = state
            .next
            .checked_add(align - 1)
            .map(|bumped| bumped & !(align - 1))
        else {
            return core::ptr::null_mut();
        };
        match start.checked_add(size) {
            Some(end) if end <= state.end => {
                state.next = end;
                start as *mut u8
            }
            _ => core::ptr::null_mut(),
        }
    }
}

impl Default for ReuseHeap {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: `alloc` returns null or a block of `layout.size()` bytes aligned to
// `layout.align()` inside the range given to `init`, and no two live blocks overlap: a
// block comes either from the bump pointer, which only advances, or from a free list
// that holds only blocks `dealloc` took back, each once.
unsafe impl GlobalAlloc for ReuseHeap {
    #[inline(never)]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: single-threaded, see the `Sync` impl.
        let state = unsafe { &mut *self.state.get() };
        let Some(class) = Self::class(layout) else {
            return Self::bump(state, layout.size(), layout.align());
        };
        let block = state.free[class];
        if block != 0 {
            // SAFETY: a free block holds the address of the next one in its first word.
            state.free[class] = unsafe { *(block as *const usize) };
            return block as *mut u8;
        }
        Self::bump(state, SIZES[class], ALIGN)
    }

    #[inline(never)]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(class) = Self::class(layout) else {
            return;
        };
        // SAFETY: single-threaded, see the `Sync` impl; the block is at least a word long
        // and aligned to one, and the caller gives it up.
        unsafe {
            let state = &mut *self.state.get();
            *ptr.cast::<usize>() = state.free[class];
            state.free[class] = ptr as usize;
        }
    }

    #[inline(never)]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `new_size` is non-zero and fits the alignment, as the caller promises.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let class = Self::class(layout);
        if class.is_some() && class == Self::class(new_layout) {
            // The block is as large as the new size needs.
            return ptr;
        }
        // SAFETY: the new block is `new_size` bytes, the old one is live for
        // `layout.size()`, and the two do not overlap.
        unsafe {
            let new = self.alloc(new_layout);
            if !new.is_null() {
                core::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
            new
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// A heap over `bytes` of aligned memory.
    fn heap(bytes: usize) -> (ReuseHeap, Vec<u64>) {
        let mut memory = std::vec![0u64; bytes / 8];
        let start = memory.as_mut_ptr() as usize;
        let heap = ReuseHeap::new();
        // SAFETY: the vector outlives the heap in each test and nothing else uses it.
        unsafe { heap.init(start, start + bytes) };
        (heap, memory)
    }

    fn used(heap: &ReuseHeap, start: usize) -> usize {
        // SAFETY: single-threaded test.
        unsafe { (*heap.state.get()).next - start }
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size, 4).unwrap()
    }

    #[test]
    fn requests_round_up_to_the_next_block_size() {
        let class = |size| ReuseHeap::class(layout(size));
        assert_eq!(class(1), Some(0));
        assert_eq!(class(8), Some(0));
        assert_eq!(class(9), Some(1));
        assert_eq!(class(16), Some(1));
        assert_eq!(class(17), Some(2));
        assert_eq!(class(24), Some(2));
        assert_eq!(class(25), Some(3));
        assert_eq!(class(372), Some(10));
        assert_eq!(class(2048), Some(CLASSES - 1));
        assert_eq!(class(2049), None);
        // No block is much more than half as large again as the request.
        for size in 9..=MAX_BLOCK {
            let block = SIZES[class(size).unwrap()];
            assert!(
                block >= size && block * 2 <= size * 3 + 16,
                "{size} -> {block}"
            );
        }
        assert_eq!(
            ReuseHeap::class(Layout::from_size_align(8, 16).unwrap()),
            None
        );
    }

    #[test]
    fn a_freed_block_serves_the_next_request_of_its_class() {
        let (heap, memory) = heap(4096);
        let start = memory.as_ptr() as usize;
        // SAFETY: layouts are non-zero; pointers are used once and freed with their layout.
        unsafe {
            let a = heap.alloc(layout(40));
            assert!(!a.is_null());
            let after = used(&heap, start);
            heap.dealloc(a, layout(40));
            let b = heap.alloc(layout(33));
            assert_eq!(a, b, "the block of 48 bytes comes back");
            assert_eq!(used(&heap, start), after);
            // Another class is not served from it.
            let c = heap.alloc(layout(8));
            assert_ne!(c, a);
        }
    }

    #[test]
    fn a_growing_vector_stops_costing_heap() {
        let (heap, memory) = heap(8192);
        let start = memory.as_ptr() as usize;
        // The growth chain of a Vec<u32>: 16, 32, 64, 128 bytes, each step freeing the last.
        let grow = || unsafe {
            let mut block = heap.alloc(layout(16));
            let mut size = 16;
            while size < 128 {
                block = heap.realloc(block, layout(size), size * 2);
                assert!(!block.is_null());
                size *= 2;
            }
            heap.dealloc(block, layout(size));
        };
        grow();
        let first = used(&heap, start);
        // The first chain left 16 + 32 + 64 + 128; the second finds them all again.
        grow();
        grow();
        assert_eq!(used(&heap, start), first);
    }

    #[test]
    fn growing_inside_a_class_keeps_the_block_and_its_bytes() {
        let (heap, memory) = heap(4096);
        let _ = &memory;
        // SAFETY: as above.
        unsafe {
            let a = heap.alloc(layout(18));
            for i in 0..18 {
                *a.add(i) = i as u8;
            }
            let b = heap.realloc(a, layout(18), 22);
            assert_eq!(a, b);
            let c = heap.realloc(b, layout(22), 100);
            assert_ne!(b, c);
            assert!((0..18).all(|i| *c.add(i) == i as u8));
        }
    }

    #[test]
    fn large_and_over_aligned_blocks_are_carved_off_the_end_and_aligned() {
        let (heap, memory) = heap(8192);
        let _ = &memory;
        // SAFETY: as above.
        unsafe {
            let big = heap.alloc(layout(3000));
            assert!(!big.is_null());
            heap.dealloc(big, layout(3000));
            let again = heap.alloc(layout(3000));
            assert_ne!(big, again, "a block over 2048 bytes is not reused");
            let aligned = heap.alloc(Layout::from_size_align(24, 32).unwrap());
            assert_eq!(aligned as usize % 32, 0);
        }
    }

    #[test]
    fn running_out_gives_null_not_a_block_off_the_heap() {
        let (heap, memory) = heap(64);
        let _ = &memory;
        // SAFETY: as above.
        unsafe {
            assert!(!heap.alloc(layout(48)).is_null());
            assert!(heap.alloc(layout(48)).is_null());
            assert!(heap.alloc(layout(isize::MAX as usize - 3)).is_null());
        }
    }

    #[test]
    fn live_blocks_never_overlap() {
        let (heap, memory) = heap(16384);
        let _ = &memory;
        let mut live: Vec<(usize, usize)> = Vec::new();
        let mut seed = 12345u32;
        for _ in 0..400 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let size = 1 + (seed >> 16) as usize % 300;
            // SAFETY: as above.
            unsafe {
                if seed & 1 == 0 && !live.is_empty() {
                    let (p, s) = live.swap_remove((seed >> 8) as usize % live.len());
                    heap.dealloc(p as *mut u8, layout(s));
                } else {
                    let p = heap.alloc(layout(size));
                    if p.is_null() {
                        break;
                    }
                    live.push((p as usize, size));
                }
            }
        }
        live.sort_unstable();
        for pair in live.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0, "{pair:?}");
        }
    }
}
