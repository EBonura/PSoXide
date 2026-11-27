// SPDX-License-Identifier: GPL-2.0-or-later
//! A disc image behind the loader's [`Transport`], for host tests: it serves
//! a pack the real writer built and counts what the loader asked the drive
//! for (seeks, sectors, where each sector was popped to), so a test can
//! state the CD cost of a load, not just its result.

extern crate std;

use super::{Transport, SECTOR_WORDS};
use crate::SECTOR_BYTES;
use std::vec::Vec;

/// One `start_read` and the sectors popped after it: `(lba, sectors)`.
pub(crate) type Run = (u32, u32);

/// A pack image placed at `pack_lba`, read through the [`Transport`] steps.
pub(crate) struct ImageDrive {
    image: Vec<u8>,
    pack_lba: u32,
    /// Next LBA the running read delivers, `None` when no read runs.
    head: Option<u32>,
    /// Every read started, with its sector count.
    pub(crate) runs: Vec<Run>,
    /// Sectors popped into a buffer inside [`Self::watch`].
    pub(crate) direct: u32,
    /// Sectors popped anywhere else (a bounce buffer).
    pub(crate) bounced: u32,
    watch: core::ops::Range<usize>,
    /// Fail the read of this absolute LBA once.
    pub(crate) fail_lba: Option<u32>,
}

impl ImageDrive {
    pub(crate) fn new(image: Vec<u8>, pack_lba: u32) -> Self {
        Self {
            image,
            pack_lba,
            head: None,
            runs: Vec::new(),
            direct: 0,
            bounced: 0,
            watch: 0..0,
            fail_lba: None,
        }
    }

    /// Count pops into `dst` as direct, all others as bounced.
    pub(crate) fn watch(&mut self, dst: &[u32]) {
        let start = dst.as_ptr() as usize;
        self.watch = start..start + dst.len() * 4;
    }

    /// Reads started (one seek each).
    pub(crate) fn seeks(&self) -> u32 {
        self.runs.len() as u32
    }

    /// Sectors delivered.
    pub(crate) fn sectors(&self) -> u32 {
        self.runs.iter().map(|r| r.1).sum()
    }

    /// Forget the counts, keep the image.
    pub(crate) fn reset_counts(&mut self) {
        self.runs.clear();
        self.direct = 0;
        self.bounced = 0;
    }
}

impl Transport for ImageDrive {
    fn prepare(&mut self) -> bool {
        true
    }

    fn start_read(&mut self, lba: u32) -> bool {
        assert!(
            self.head.is_none(),
            "a read started while another runs (chained without stop)"
        );
        self.head = Some(lba);
        self.runs.push((lba, 0));
        true
    }

    fn read_sector(&mut self, buffer: &mut [u32; SECTOR_WORDS]) -> bool {
        let lba = self.head.expect("read_sector with no read running");
        if self.fail_lba == Some(lba) {
            self.fail_lba = None;
            return false;
        }
        let at = (lba - self.pack_lba) as usize * SECTOR_BYTES;
        let mut k = 0;
        while k < SECTOR_WORDS {
            let b = |i: usize| self.image.get(at + k * 4 + i).copied().unwrap_or(0) as u32;
            buffer[k] = b(0) | (b(1) << 8) | (b(2) << 16) | (b(3) << 24);
            k += 1;
        }
        let addr = buffer.as_ptr() as usize;
        if self.watch.contains(&addr) {
            self.direct += 1;
        } else {
            self.bounced += 1;
        }
        self.head = Some(lba + 1);
        self.runs.last_mut().expect("a run").1 += 1;
        true
    }

    fn stop(&mut self) {
        self.head = None;
    }
}
