//! Contiguous-run page allocation with generation handles.
//!
//! Lifted from the retired grid streamer's `StreamedRoomPages` (PSoXide-editor
//! `psx-game-runtime`), split in two: [`PagePool`] here is only the allocation
//! map (which pages belong to which allocation), so it works for RAM pages,
//! VRAM texture slots and SPU RAM blocks alike; the bytes of RAM pages live in
//! [`crate::PageBuffer`]. What carried over: each allocation is one contiguous
//! run (a CD transfer lands in its final place), a [`Handle`] carries the slot
//! and a generation so a stale handle stops resolving the moment its run is
//! freed or reused, and compaction slides runs toward page 0. What is new:
//! first-fit or best-fit placement, locked runs that compaction and callers
//! must not move, one compaction step at a time (so it fits a CPU budget), and
//! fragmentation statistics.

/// Bytes in one page: one CD-ROM Mode 1 sector.
pub const PAGE_BYTES: usize = 2048;

/// A run of pages: `page_count` pages starting at `first_page`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageRun {
    /// Index of the first page.
    pub first_page: u32,
    /// Number of pages.
    pub page_count: u32,
}

impl PageRun {
    /// Index one past the last page.
    pub const fn end_page(self) -> u32 {
        self.first_page + self.page_count
    }

    /// Capacity of the run in bytes, for pages of [`PAGE_BYTES`].
    pub const fn capacity_bytes(self) -> usize {
        self.page_count as usize * PAGE_BYTES
    }
}

/// Generation-checked identity of one allocation. It stops resolving as soon
/// as its allocation is freed, even if the slot is reused for another run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Handle {
    pool: u8,
    slot: u16,
    generation: u32,
}

impl Handle {
    /// A handle that is never current, for filling buffers.
    pub const NONE: Handle = Handle {
        pool: u8::MAX,
        slot: u16::MAX,
        generation: 0,
    };

    /// Pool number the handle was allocated from.
    pub const fn pool(self) -> u8 {
        self.pool
    }

    /// Allocation slot inside that pool.
    pub const fn slot(self) -> u16 {
        self.slot
    }

    /// Generation of the slot when the handle was issued.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Which free gap a new run goes into.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The lowest-addressed gap that is large enough.
    FirstFit,
    /// The smallest gap that is large enough (ties go to the lowest address).
    BestFit,
}

/// Free-space shape of a pool, for fragmentation reports.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Fragmentation {
    /// Free pages in total.
    pub free_pages: u32,
    /// Pages in the largest free run.
    pub largest_free_run: u32,
    /// Number of separate free runs.
    pub free_run_count: u32,
}

impl Fragmentation {
    /// Share of free space that is not in the largest run, in permille
    /// (0 when all free space is one run, or there is none).
    pub const fn permille(self) -> u32 {
        if self.free_pages == 0 {
            return 0;
        }
        // 32-bit only: scale huge pools down so the product cannot overflow.
        let mut free = self.free_pages;
        let mut largest = self.largest_free_run;
        while free > u32::MAX / 1000 {
            free >>= 1;
            largest >>= 1;
        }
        1000 - largest * 1000 / free
    }
}

/// One run moved by [`PagePool::compact_step`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The moved allocation (its handle stays valid).
    pub handle: Handle,
    /// Where it was.
    pub from: PageRun,
    /// First page it occupies now.
    pub to_page: u32,
}

#[derive(Copy, Clone)]
struct Run {
    first: u32,
    /// Zero means the slot is free.
    count: u32,
    generation: u32,
    locked: bool,
}

const FREE_RUN: Run = Run {
    first: 0,
    count: 0,
    generation: 0,
    locked: false,
};

/// Allocation map of `capacity_pages` pages shared by up to `SLOTS` runs.
///
/// Pure bookkeeping: the pool never touches page contents. All state is plain
/// integers, so a static pool can live in `.bss`.
pub struct PagePool<const SLOTS: usize> {
    pool: u8,
    capacity_pages: u32,
    placement: Placement,
    runs: [Run; SLOTS],
    /// Live slots sorted by first page; only `order[..live]` is meaningful.
    order: [u16; SLOTS],
    live: usize,
    used_pages: u32,
    layout_generation: u32,
}

impl<const SLOTS: usize> PagePool<SLOTS> {
    /// An empty pool numbered `pool` (stamped into its handles) of
    /// `capacity_pages` pages.
    pub const fn new(pool: u8, capacity_pages: u32, placement: Placement) -> Self {
        Self {
            pool,
            capacity_pages,
            placement,
            runs: [FREE_RUN; SLOTS],
            order: [0; SLOTS],
            live: 0,
            used_pages: 0,
            layout_generation: 0,
        }
    }

    /// Total pages in the pool.
    pub const fn capacity_pages(&self) -> u32 {
        self.capacity_pages
    }

    /// Pages held by live allocations.
    pub const fn used_pages(&self) -> u32 {
        self.used_pages
    }

    /// Pages not held by any allocation (possibly scattered).
    pub const fn free_pages(&self) -> u32 {
        self.capacity_pages - self.used_pages
    }

    /// Number of live allocations.
    pub const fn live_count(&self) -> usize {
        self.live
    }

    /// Placement policy for new runs.
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// Change the placement policy; existing runs stay where they are.
    pub fn set_placement(&mut self, placement: Placement) {
        self.placement = placement;
    }

    /// Bumped every time compaction moves a run. A byte view taken before a
    /// bump may point at the wrong pages: re-resolve it.
    pub const fn layout_generation(&self) -> u32 {
        self.layout_generation
    }

    /// Allocate a contiguous run of `page_count` pages, or `None` when no gap
    /// is large enough or every slot is taken. Never moves another run.
    pub fn allocate(&mut self, page_count: u32) -> Option<(Handle, PageRun)> {
        if page_count == 0 {
            return None;
        }
        let slot = self.runs.iter().position(|run| run.count == 0)?;
        let first = self.find_fit(page_count)?;
        let generation = self.runs[slot].generation.wrapping_add(1);
        self.runs[slot] = Run {
            first,
            count: page_count,
            generation,
            locked: false,
        };
        let mut position = self.live;
        while position > 0 && self.runs[self.order[position - 1] as usize].first > first {
            self.order[position] = self.order[position - 1];
            position -= 1;
        }
        self.order[position] = slot as u16;
        self.live += 1;
        self.used_pages += page_count;
        Some((
            Handle {
                pool: self.pool,
                slot: slot as u16,
                generation,
            },
            PageRun {
                first_page: first,
                page_count,
            },
        ))
    }

    /// First page of the gap a run of `page_count` pages would get.
    pub fn find_fit(&self, page_count: u32) -> Option<u32> {
        if page_count == 0 || page_count > self.free_pages() {
            return None;
        }
        let mut best: Option<(u32, u32)> = None;
        for (start, length) in self.gaps() {
            if length < page_count {
                continue;
            }
            match self.placement {
                Placement::FirstFit => return Some(start),
                Placement::BestFit => {
                    if best.is_none_or(|(_, best_length)| length < best_length) {
                        best = Some((start, length));
                    }
                }
            }
        }
        best.map(|(start, _)| start)
    }

    /// Free `handle`. `false` (and no change) when the handle is stale.
    pub fn free(&mut self, handle: Handle) -> bool {
        if !self.is_current(handle) {
            return false;
        }
        let slot = handle.slot as usize;
        let position = self.order[..self.live]
            .iter()
            .position(|&s| s as usize == slot)
            .expect("a live run is always in the order table");
        self.order.copy_within(position + 1..self.live, position);
        self.live -= 1;
        self.used_pages -= self.runs[slot].count;
        self.runs[slot] = Run {
            generation: self.runs[slot].generation.wrapping_add(1),
            ..FREE_RUN
        };
        true
    }

    /// Whether `handle` still names the allocation it was issued for.
    pub fn is_current(&self, handle: Handle) -> bool {
        handle.pool == self.pool
            && self
                .runs
                .get(handle.slot as usize)
                .is_some_and(|run| run.count > 0 && run.generation == handle.generation)
    }

    /// The pages `handle` names, or `None` when it is stale.
    pub fn try_run(&self, handle: Handle) -> Option<PageRun> {
        if !self.is_current(handle) {
            return None;
        }
        let run = self.runs[handle.slot as usize];
        Some(PageRun {
            first_page: run.first,
            page_count: run.count,
        })
    }

    /// Lock or unlock the run. A locked run is never moved by
    /// [`PagePool::compact_step`] (a transfer is landing in it, or a consumer
    /// holds a raw view of it). `false` when the handle is stale.
    pub fn set_locked(&mut self, handle: Handle, locked: bool) -> bool {
        if !self.is_current(handle) {
            return false;
        }
        self.runs[handle.slot as usize].locked = locked;
        true
    }

    /// Whether the run is locked (`false` for a stale handle).
    pub fn is_locked(&self, handle: Handle) -> bool {
        self.is_current(handle) && self.runs[handle.slot as usize].locked
    }

    /// Live allocations in address order as `(slot, run, locked)`.
    pub fn iter_runs(&self) -> impl Iterator<Item = (u16, PageRun, bool)> + '_ {
        self.order[..self.live].iter().map(move |&slot| {
            let run = self.runs[slot as usize];
            (
                slot,
                PageRun {
                    first_page: run.first,
                    page_count: run.count,
                },
                run.locked,
            )
        })
    }

    /// Handle of the live allocation in `slot`.
    pub fn handle_of_slot(&self, slot: u16) -> Option<Handle> {
        let run = self.runs.get(slot as usize)?;
        (run.count > 0).then_some(Handle {
            pool: self.pool,
            slot,
            generation: run.generation,
        })
    }

    /// Free gaps in address order as `(first_page, page_count)`.
    pub fn gaps(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let mut cursor = 0u32;
        let mut position = 0usize;
        let mut done = false;
        core::iter::from_fn(move || {
            while !done {
                let (end, next_cursor) = if position < self.live {
                    let run = self.runs[self.order[position] as usize];
                    position += 1;
                    (run.first, run.first + run.count)
                } else {
                    done = true;
                    (self.capacity_pages, self.capacity_pages)
                };
                let start = cursor;
                cursor = next_cursor;
                if end > start {
                    return Some((start, end - start));
                }
            }
            None
        })
    }

    /// Shape of the free space.
    pub fn fragmentation(&self) -> Fragmentation {
        let mut result = Fragmentation::default();
        for (_, length) in self.gaps() {
            result.free_pages += length;
            result.free_run_count += 1;
            result.largest_free_run = result.largest_free_run.max(length);
        }
        result
    }

    /// Slide the lowest unlocked run that has free space below it down to the
    /// lowest page it can reach, calling `copy(from, to_page)` so the owner of
    /// the bytes moves them (the destination may overlap the source; copy as
    /// `memmove`). Locked runs stay put and act as barriers. One run per call,
    /// so a caller can spend a CPU budget on it. `None` when nothing can move.
    pub fn compact_step(&mut self, mut copy: impl FnMut(PageRun, u32)) -> Option<Moved> {
        let mut cursor = 0u32;
        for position in 0..self.live {
            let slot = self.order[position] as usize;
            let run = self.runs[slot];
            if !run.locked && run.first > cursor {
                let from = PageRun {
                    first_page: run.first,
                    page_count: run.count,
                };
                copy(from, cursor);
                self.runs[slot].first = cursor;
                self.layout_generation = self.layout_generation.wrapping_add(1);
                return Some(Moved {
                    handle: Handle {
                        pool: self.pool,
                        slot: slot as u16,
                        generation: run.generation,
                    },
                    from,
                    to_page: cursor,
                });
            }
            cursor = run.first + run.count;
        }
        None
    }

    /// Check the structural invariants (host tests and debug assertions).
    #[cfg(any(test, debug_assertions))]
    pub fn check_invariants(&self) {
        let mut used = 0u32;
        let mut previous_end = 0u32;
        for position in 0..self.live {
            let run = self.runs[self.order[position] as usize];
            assert!(run.count > 0, "order lists a free slot");
            assert!(run.first >= previous_end, "runs overlap or are unsorted");
            previous_end = run.first + run.count;
            assert!(previous_end <= self.capacity_pages, "run past capacity");
            used += run.count;
        }
        assert_eq!(used, self.used_pages, "used_pages out of step");
        let live = self.runs.iter().filter(|run| run.count > 0).count();
        assert_eq!(live, self.live, "live count out of step");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_exact_page_count() {
        let mut pool = PagePool::<3>::new(0, 8, Placement::FirstFit);
        let (_, a) = pool.allocate(1).unwrap();
        let (_, b) = pool.allocate(2).unwrap();
        assert_eq!(a.page_count, 1);
        assert_eq!(b.page_count, 2);
        assert_eq!(pool.free_pages(), 5);
        pool.check_invariants();
    }

    #[test]
    fn released_or_reused_slot_invalidates_old_handle() {
        let mut pool = PagePool::<1>::new(0, 4, Placement::FirstFit);
        let (first, _) = pool.allocate(1).unwrap();
        assert!(pool.try_run(first).is_some());
        assert!(pool.free(first));
        assert!(pool.try_run(first).is_none());
        assert!(!pool.free(first), "double free is refused");
        let (second, _) = pool.allocate(1).unwrap();
        assert_eq!(first.slot(), second.slot());
        assert_ne!(first.generation(), second.generation());
        assert!(pool.try_run(first).is_none(), "reuse must not revive it");
    }

    #[test]
    fn handle_from_another_pool_is_stale() {
        let mut a = PagePool::<2>::new(0, 4, Placement::FirstFit);
        let b = PagePool::<2>::new(1, 4, Placement::FirstFit);
        let (handle, _) = a.allocate(1).unwrap();
        assert!(!b.is_current(handle));
    }

    #[test]
    fn placement_policies_choose_different_gaps() {
        // Layout: [A 2][gap 4][B 1][gap 2][C 1]; capacity 10.
        let build = |placement| {
            let mut pool = PagePool::<8>::new(0, 10, placement);
            let (a, _) = pool.allocate(2).unwrap();
            let (gap1, _) = pool.allocate(4).unwrap();
            let (b, _) = pool.allocate(1).unwrap();
            let (gap2, _) = pool.allocate(2).unwrap();
            let (c, _) = pool.allocate(1).unwrap();
            pool.free(gap1);
            pool.free(gap2);
            (pool, [a, b, c])
        };
        let (first_fit, _) = build(Placement::FirstFit);
        let (best_fit, _) = build(Placement::BestFit);
        assert_eq!(first_fit.find_fit(2), Some(2));
        assert_eq!(best_fit.find_fit(2), Some(7));
        assert_eq!(best_fit.find_fit(4), Some(2));
        assert_eq!(best_fit.find_fit(5), None);
    }

    #[test]
    fn fragmented_pool_compacts_and_keeps_handles() {
        let mut pool = PagePool::<4>::new(0, 8, Placement::FirstFit);
        let (a, _) = pool.allocate(2).unwrap();
        let (b, _) = pool.allocate(2).unwrap();
        let (c, _) = pool.allocate(2).unwrap();
        pool.free(b);
        assert_eq!(pool.find_fit(4), None);
        assert_eq!(pool.fragmentation().free_run_count, 2);
        let before = pool.layout_generation();
        let mut copies = [None; 2];
        let mut count = 0;
        while let Some(moved) = pool.compact_step(|from, to| {
            copies[count] = Some((from.first_page, to));
        }) {
            assert_eq!(moved.handle, c);
            count += 1;
        }
        assert_eq!(copies[0], Some((4, 2)));
        assert_ne!(pool.layout_generation(), before);
        assert_eq!(pool.try_run(a).unwrap().first_page, 0);
        assert_eq!(pool.try_run(c).unwrap().first_page, 2);
        assert_eq!(pool.find_fit(4), Some(4));
        pool.check_invariants();
    }

    #[test]
    fn locked_run_is_a_compaction_barrier() {
        let mut pool = PagePool::<5>::new(0, 12, Placement::FirstFit);
        let (a, _) = pool.allocate(2).unwrap();
        let (b, _) = pool.allocate(2).unwrap();
        let (c, _) = pool.allocate(2).unwrap();
        let (d, _) = pool.allocate(2).unwrap();
        pool.free(a);
        pool.free(c);
        assert!(pool.set_locked(b, true));
        assert!(pool.is_locked(b));
        // Layout: [gap 0..2][b locked 2..4][gap 4..6][d 6..8]. d slides to 4,
        // b stays, and nothing jumps over b into the first gap.
        let moved = pool.compact_step(|_, _| {}).unwrap();
        assert_eq!(moved.handle, d);
        assert_eq!(moved.to_page, 4);
        assert!(pool.compact_step(|_, _| {}).is_none());
        assert_eq!(pool.try_run(b).unwrap().first_page, 2);
        assert_eq!(pool.try_run(d).unwrap().first_page, 4);
        pool.check_invariants();
    }

    #[test]
    fn free_space_shape_is_reported() {
        let mut pool = PagePool::<4>::new(0, 10, Placement::FirstFit);
        let (a, _) = pool.allocate(3).unwrap();
        let (_b, _) = pool.allocate(3).unwrap();
        pool.free(a);
        let shape = pool.fragmentation();
        assert_eq!(shape.free_pages, 7);
        assert_eq!(shape.largest_free_run, 4);
        assert_eq!(shape.free_run_count, 2);
        assert_eq!(shape.permille(), 1000 - 4 * 1000 / 7);
    }

    #[test]
    fn full_slot_table_refuses_even_when_pages_are_free() {
        let mut pool = PagePool::<2>::new(0, 100, Placement::FirstFit);
        pool.allocate(1).unwrap();
        pool.allocate(1).unwrap();
        assert!(pool.allocate(1).is_none());
        assert!(pool.allocate(0).is_none());
    }
}
