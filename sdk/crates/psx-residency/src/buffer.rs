//! RAM backing for a page pool.

use crate::pool::{PageRun, PAGE_BYTES};

const PAGE_WORDS: usize = PAGE_BYTES / 4;

/// `PAGES` pages of [`PAGE_BYTES`] bytes, word aligned and contiguous.
///
/// Pair it with a [`crate::PagePool`] of the same capacity: the pool decides
/// which pages belong to which allocation, the buffer owns the bytes. The
/// all-zero image means a static buffer stays in `.bss` rather than the
/// executable.
///
/// A byte view borrows the buffer, so it cannot outlive it, but it describes
/// its run only until the pool compacts or the run is freed and reused:
/// re-resolve through [`crate::Residency::try_run`] each use unless the entry
/// is pinned.
pub struct PageBuffer<const PAGES: usize> {
    pages: [[u32; PAGE_WORDS]; PAGES],
}

impl<const PAGES: usize> Default for PageBuffer<PAGES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const PAGES: usize> PageBuffer<PAGES> {
    /// A zero-filled buffer.
    pub const fn new() -> Self {
        Self {
            pages: [[0; PAGE_WORDS]; PAGES],
        }
    }

    /// Number of pages.
    pub const fn page_count(&self) -> usize {
        PAGES
    }

    fn contains(run: PageRun, len: usize) -> bool {
        (run.end_page() as usize) <= PAGES && len <= run.capacity_bytes()
    }

    /// The first `len` bytes of `run`, or `None` when the run is outside the
    /// buffer or shorter than `len`.
    pub fn bytes(&self, run: PageRun, len: usize) -> Option<&[u8]> {
        if !Self::contains(run, len) {
            return None;
        }
        // SAFETY: `self.pages.as_ptr()` carries provenance for the whole page
        // array, and `contains` proved `first_page * PAGE_BYTES + len` lies
        // inside it. Pages are contiguous `u32` arrays, valid as `u8`.
        Some(unsafe {
            core::slice::from_raw_parts(
                self.pages
                    .as_ptr()
                    .cast::<u8>()
                    .add(run.first_page as usize * PAGE_BYTES),
                len,
            )
        })
    }

    /// Mutable form of [`PageBuffer::bytes`].
    pub fn bytes_mut(&mut self, run: PageRun, len: usize) -> Option<&mut [u8]> {
        if !Self::contains(run, len) {
            return None;
        }
        // SAFETY: as in `bytes`; `&mut self` makes the view exclusive.
        Some(unsafe {
            core::slice::from_raw_parts_mut(
                self.pages
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(run.first_page as usize * PAGE_BYTES),
                len,
            )
        })
    }

    /// Copy `data` to `offset` bytes into `run`. `false` when it does not fit.
    pub fn write(&mut self, run: PageRun, offset: usize, data: &[u8]) -> bool {
        let Some(end) = offset.checked_add(data.len()) else {
            return false;
        };
        match self.bytes_mut(run, end) {
            Some(bytes) => {
                bytes[offset..].copy_from_slice(data);
                true
            }
            None => false,
        }
    }

    /// Move `from` to start at `to_page` (a `memmove`: the ranges may
    /// overlap). The owner calls this for each [`crate::PagePool::compact_step`].
    /// `false` when either range is outside the buffer.
    pub fn move_pages(&mut self, from: PageRun, to_page: u32) -> bool {
        let to = PageRun {
            first_page: to_page,
            page_count: from.page_count,
        };
        if from.end_page() as usize > PAGES || to.end_page() as usize > PAGES {
            return false;
        }
        self.pages.copy_within(
            from.first_page as usize..from.end_page() as usize,
            to_page as usize,
        );
        true
    }

    /// Address of the first byte of `page`, for a transport that lands
    /// sectors in place. `None` past the end.
    pub fn page_address(&mut self, page: u32) -> Option<*mut u8> {
        ((page as usize) < PAGES).then(|| {
            self.pages
                .as_mut_ptr()
                .cast::<u8>()
                .wrapping_add(page as usize * PAGE_BYTES)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_across_a_page_boundary_without_staging() {
        let mut buffer = PageBuffer::<3>::new();
        let run = PageRun {
            first_page: 0,
            page_count: 2,
        };
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert!(buffer.write(run, PAGE_BYTES - 4, &payload));
        let bytes = buffer.bytes(run, PAGE_BYTES + 4).unwrap();
        assert_eq!(&bytes[PAGE_BYTES - 4..], &payload);
    }

    #[test]
    fn refuses_views_outside_the_buffer_or_run() {
        let mut buffer = PageBuffer::<2>::new();
        let run = PageRun {
            first_page: 1,
            page_count: 1,
        };
        assert!(buffer.bytes(run, PAGE_BYTES + 1).is_none());
        assert!(!buffer.write(run, PAGE_BYTES - 1, &[0, 0]));
        let outside = PageRun {
            first_page: 1,
            page_count: 2,
        };
        assert!(buffer.bytes(outside, 1).is_none());
        assert!(!buffer.move_pages(outside, 0));
        assert!(buffer.page_address(2).is_none());
        assert!(buffer.page_address(1).is_some());
    }

    #[test]
    fn move_pages_preserves_bytes_even_when_ranges_overlap() {
        let mut buffer = PageBuffer::<4>::new();
        let source = PageRun {
            first_page: 1,
            page_count: 3,
        };
        buffer.write(source, 0, &[9; 8]);
        buffer.write(source, 3 * PAGE_BYTES - 2, &[7, 8]);
        assert!(buffer.move_pages(source, 0));
        let moved = PageRun {
            first_page: 0,
            page_count: 3,
        };
        assert_eq!(&buffer.bytes(moved, 8).unwrap()[..8], &[9; 8]);
        assert_eq!(
            &buffer.bytes(moved, 3 * PAGE_BYTES).unwrap()[3 * PAGE_BYTES - 2..],
            &[7, 8]
        );
    }
}
