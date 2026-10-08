//! Page pool for streamed payloads.
//!
//! Salvaged from the retired grid-world room cache (`StreamedRoomPages`).
//! Contiguous runs of 2 KiB CD-sector pages, generation-checked handles, and
//! sectors that land directly at their final RAM address. The room-specific
//! chunk views that used to live beside it are gone; callers parse the byte
//! slices this pool hands out.

use crate::cd_stream::{WorldChunkDestination, SECTOR_BYTES};

/// One allocation in the sector-page pool.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PageRun {
    first_page: u16,
    page_count: u16,
    generation: u16,
}

impl PageRun {
    const EMPTY: Self = Self {
        first_page: 0,
        page_count: 0,
        generation: 0,
    };
}

/// Generation-safe identity of one logical slot allocation.
/// A handle stops resolving as soon as its slot is released or reused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageHandle {
    /// Logical slot.
    pub slot: u16,
    /// Allocation generation in that slot.
    pub generation: u16,
}

/// Streamed-payload RAM as contiguous runs of 2 KiB CD-sector pages.
///
/// Fixed max-payload rows waste `largest_payload * slot_count` bytes. This
/// pool instead pays `ceil(actual_bytes / 2048)` per resident payload while
/// preserving contiguous byte slices for zero-copy parsers. CD sectors
/// therefore land directly in their final RAM address with no assembly
/// buffer and less than one sector of internal waste per payload.
///
/// `SLOTS` is the logical-payload capacity; `PAGES` is the
/// independently generated RAM budget. Both arrays are all-zero valid so the
/// owning static remains in `.bss`.
///
/// The resolvers are lifetime-honest: every returned slice borrows
/// `self`, so it cannot outlive the buffer it points into. The
/// STALENESS caveat from the streaming audit (finding 3) still applies
/// across calls: a resolved slice describes the slot's contents only
/// until the owning scheduler overwrites that slot, so re-resolve per use;
/// holding one longer is sound only for payloads the scheduler has pinned
/// against eviction, together with full-width residency generations so a
/// payload leaving the window forces a re-gather before its pages can be
/// reused.
pub struct PagePool<const PAGES: usize, const SLOTS: usize> {
    pages: [[u32; SECTOR_BYTES / 4]; PAGES],
    runs: [PageRun; SLOTS],
    layout_generation: u32,
}

impl<const PAGES: usize, const SLOTS: usize> Default for PagePool<PAGES, SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const PAGES: usize, const SLOTS: usize> PagePool<PAGES, SLOTS> {
    /// Zero-initialized page pool; `const` so the game's static
    /// instance stays in `.bss`.
    pub const fn new() -> Self {
        Self {
            pages: [[0; SECTOR_BYTES / 4]; PAGES],
            runs: [PageRun::EMPTY; SLOTS],
            // Keep the whole pool all-zero valid so arena statics remain in
            // `.bss` instead of duplicating tens of kilobytes into PSX-EXE.
            layout_generation: 0,
        }
    }

    const fn pages_for_bytes(byte_count: usize) -> usize {
        byte_count.div_ceil(SECTOR_BYTES)
    }

    fn page_used_except(&self, page: usize, excluded_slot: usize) -> bool {
        let mut slot = 0usize;
        while slot < SLOTS {
            if slot != excluded_slot {
                let run = self.runs[slot];
                let first = run.first_page as usize;
                let end = first.saturating_add(run.page_count as usize);
                if run.page_count > 0 && page >= first && page < end {
                    return true;
                }
            }
            slot += 1;
        }
        false
    }

    fn find_contiguous_run(&self, pages: usize, excluded_slot: usize) -> Option<usize> {
        if pages == 0 || pages > PAGES {
            return None;
        }
        let mut first = 0usize;
        while first.saturating_add(pages) <= PAGES {
            let mut offset = 0usize;
            while offset < pages && !self.page_used_except(first + offset, excluded_slot) {
                offset += 1;
            }
            if offset == pages {
                return Some(first);
            }
            first = first.saturating_add(offset).saturating_add(1);
        }
        None
    }

    /// Pack all allocations except the slot being replaced toward page zero.
    /// Returns whether any live allocation moved. Callers must invalidate and
    /// rebuild pointer-bearing parsed views when `layout_generation` changes.
    fn compact_except(&mut self, excluded_slot: usize) -> bool {
        let mut cursor = 0usize;
        let mut packed = [false; SLOTS];
        let mut changed = false;
        loop {
            let mut next_slot = None;
            let mut next_page = usize::MAX;
            let mut slot = 0usize;
            while slot < SLOTS {
                let run = self.runs[slot];
                let first = run.first_page as usize;
                if slot != excluded_slot && !packed[slot] && run.page_count > 0 && first < next_page
                {
                    next_slot = Some(slot);
                    next_page = first;
                }
                slot += 1;
            }
            let Some(slot) = next_slot else {
                break;
            };
            packed[slot] = true;
            let count = self.runs[slot].page_count as usize;
            if next_page != cursor {
                let src = self.pages.as_ptr().wrapping_add(next_page);
                let dst = self.pages.as_mut_ptr().wrapping_add(cursor);
                // SAFETY: both ranges are within `pages`; `ptr::copy` permits
                // overlap while packing toward lower addresses.
                unsafe { core::ptr::copy(src, dst, count) };
                self.runs[slot].first_page = cursor as u16;
                changed = true;
            }
            cursor += count;
        }
        if changed {
            self.layout_generation = self.layout_generation.wrapping_add(1).max(1);
        }
        changed
    }

    /// Reserve enough contiguous sector pages for a new payload in `slot`.
    /// Existing bytes in that logical slot are invalidated atomically by a
    /// generation bump. Returns `None` without changing the slot when the page
    /// budget has no sufficiently large run.
    pub fn prepare_slot(&mut self, slot: usize, byte_count: usize) -> Option<PageHandle> {
        if slot >= SLOTS || byte_count == 0 {
            return None;
        }
        let page_count = Self::pages_for_bytes(byte_count);
        let first_page = match self.find_contiguous_run(page_count, slot) {
            Some(first) => first,
            None => {
                let available = self
                    .free_page_count()
                    .saturating_add(self.runs[slot].page_count as usize);
                if available < page_count {
                    return None;
                }
                self.compact_except(slot);
                self.find_contiguous_run(page_count, slot)?
            }
        };
        let generation = self.runs[slot].generation.wrapping_add(1).max(1);
        self.runs[slot] = PageRun {
            first_page: u16::try_from(first_page).ok()?,
            page_count: u16::try_from(page_count).ok()?,
            generation,
        };
        Some(PageHandle {
            slot: slot as u16,
            generation,
        })
    }

    /// Whether `slot` can be replaced by a payload of `byte_count`, either in
    /// an existing contiguous run or after a capacity-sufficient compaction.
    pub fn can_prepare_slot(&self, slot: usize, byte_count: usize) -> bool {
        if slot >= SLOTS || byte_count == 0 {
            return false;
        }
        let pages = Self::pages_for_bytes(byte_count);
        self.find_contiguous_run(pages, slot).is_some()
            || self
                .free_page_count()
                .saturating_add(self.runs[slot].page_count as usize)
                >= pages
    }

    /// Identity of the physical page layout. A change requires parsed views
    /// that contain direct pointers into the pool to be rebuilt.
    pub const fn layout_generation(&self) -> u32 {
        self.layout_generation
    }

    /// Release one logical slot and invalidate every handle to its old pages.
    pub fn release_slot(&mut self, slot: usize) {
        if slot >= SLOTS {
            return;
        }
        let generation = self.runs[slot].generation.wrapping_add(1).max(1);
        self.runs[slot] = PageRun {
            generation,
            ..PageRun::EMPTY
        };
    }

    /// Current allocation handle for `slot`.
    pub fn slot_handle(&self, slot: usize) -> Option<PageHandle> {
        let run = *self.runs.get(slot)?;
        (run.page_count > 0).then_some(PageHandle {
            slot: slot as u16,
            generation: run.generation,
        })
    }

    /// Writable capacity of `slot` rounded to whole CD sectors.
    pub fn slot_capacity_bytes(&self, slot: usize) -> usize {
        self.runs
            .get(slot)
            .map_or(0, |run| run.page_count as usize * SECTOR_BYTES)
    }

    /// Number of unallocated sector pages.
    pub fn free_page_count(&self) -> usize {
        let mut used = 0usize;
        let mut slot = 0usize;
        while slot < SLOTS {
            used = used.saturating_add(self.runs[slot].page_count as usize);
            slot += 1;
        }
        PAGES.saturating_sub(used)
    }

    fn write_slot_bytes(&mut self, slot: usize, offset: usize, bytes: &[u8]) -> bool {
        let Some(run) = self.runs.get(slot).copied() else {
            return false;
        };
        let capacity = run.page_count as usize * SECTOR_BYTES;
        let Some(end) = offset.checked_add(bytes.len()) else {
            return false;
        };
        if run.page_count == 0 || end > capacity {
            return false;
        }
        let base = self.pages[run.first_page as usize]
            .as_mut_ptr()
            .cast::<u8>();
        // SAFETY: the page run is contiguous inside `pages`; `end` was checked
        // against its allocation capacity and the source cannot overlap it.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(offset), bytes.len()) };
        true
    }

    /// Byte view of `slot`'s first `byte_count` bytes.
    #[inline]
    pub fn slot_bytes(&self, slot: usize, byte_count: usize) -> Option<&[u8]> {
        let run = *self.runs.get(slot)?;
        if run.page_count == 0 || byte_count > self.slot_capacity_bytes(slot) {
            return None;
        }
        let base = self.pages[run.first_page as usize].as_ptr().cast::<u8>();
        // SAFETY: `prepare_slot` only records runs fully inside `pages`; nested
        // arrays are contiguous and `byte_count` fits the run capacity.
        Some(unsafe { core::slice::from_raw_parts(base, byte_count) })
    }

    /// Resolve bytes only if `handle` still names the current allocation.
    pub fn slot_bytes_for_handle(&self, handle: PageHandle, byte_count: usize) -> Option<&[u8]> {
        let run = *self.runs.get(handle.slot as usize)?;
        if run.generation != handle.generation {
            return None;
        }
        self.slot_bytes(handle.slot as usize, byte_count)
    }
}

impl<const PAGES: usize, const SLOTS: usize> WorldChunkDestination for PagePool<PAGES, SLOTS> {
    fn slot_capacity_bytes(&self, slot: usize) -> usize {
        Self::slot_capacity_bytes(self, slot)
    }

    fn write_chunk_bytes(&mut self, slot: usize, offset: usize, bytes: &[u8]) -> bool {
        self.write_slot_bytes(slot, offset, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_pool_charges_exact_sector_count() {
        let mut pool = PagePool::<8, 3>::new();
        pool.prepare_slot(0, 1).unwrap();
        pool.prepare_slot(1, SECTOR_BYTES + 1).unwrap();
        assert_eq!(pool.slot_capacity_bytes(0), SECTOR_BYTES);
        assert_eq!(pool.slot_capacity_bytes(1), SECTOR_BYTES * 2);
        assert_eq!(pool.free_page_count(), 5);
    }

    #[test]
    fn released_or_reused_slot_invalidates_old_handle() {
        let mut pool = PagePool::<4, 1>::new();
        let first = pool.prepare_slot(0, 16).unwrap();
        assert!(pool.slot_bytes_for_handle(first, 16).is_some());
        pool.release_slot(0);
        assert!(pool.slot_bytes_for_handle(first, 16).is_none());
        let second = pool.prepare_slot(0, 16).unwrap();
        assert_ne!(first.generation, second.generation);
    }

    #[test]
    fn fragmented_pool_compacts_and_preserves_live_bytes() {
        let mut pool = PagePool::<8, 4>::new();
        pool.prepare_slot(0, SECTOR_BYTES * 2).unwrap();
        pool.prepare_slot(1, SECTOR_BYTES * 2).unwrap();
        pool.prepare_slot(2, SECTOR_BYTES * 2).unwrap();
        assert!(pool.write_slot_bytes(2, 0, &[0x5a, 0xa5]));
        pool.release_slot(1);
        let third = pool.slot_handle(2).unwrap();
        let before = pool.layout_generation();
        assert!(pool.prepare_slot(3, SECTOR_BYTES * 3).is_some());
        assert_ne!(pool.layout_generation(), before);
        assert_eq!(pool.slot_handle(2), Some(third));
        assert_eq!(
            &pool.slot_bytes_for_handle(third, 2).unwrap()[..2],
            &[0x5a, 0xa5]
        );
        assert_eq!(pool.free_page_count(), 1);
    }

    #[test]
    fn writes_and_reads_across_page_boundary_without_staging() {
        let mut pool = PagePool::<3, 1>::new();
        let handle = pool.prepare_slot(0, SECTOR_BYTES + 8).unwrap();
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert!(pool.write_slot_bytes(0, SECTOR_BYTES - 4, &payload));
        let bytes = pool
            .slot_bytes_for_handle(handle, SECTOR_BYTES + 4)
            .unwrap();
        assert_eq!(&bytes[SECTOR_BYTES - 4..], &payload);
    }
}
