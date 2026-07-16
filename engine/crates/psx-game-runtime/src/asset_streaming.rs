//! Persistent CD-backed gameplay assets.
//!
//! Model meshes, atlases, and animation clips are read into sector-page RAM
//! during the initial loading scene. Their addresses remain stable for the
//! gameplay lifetime, so parsed model and animation views can safely borrow
//! them without baking the source blobs into the executable.

use psx_level::{asset_flags, LevelAssetRecord, LevelWorldPackEntryRecord};

use crate::cd_stream::{
    CdController, WorldChunkDestination, WorldRoomSlotsReadJob, ROOM_CHUNK_STATUS_OK, SECTOR_BYTES,
};

struct PersistentAssetStorage<const PAGES: usize, const ASSETS: usize> {
    pages: [[u32; SECTOR_BYTES / 4]; PAGES],
    offsets: [u32; ASSETS],
    lengths: [u32; ASSETS],
    used_bytes: usize,
}

impl<const PAGES: usize, const ASSETS: usize> PersistentAssetStorage<PAGES, ASSETS> {
    const fn new() -> Self {
        Self {
            pages: [[0; SECTOR_BYTES / 4]; PAGES],
            offsets: [0; ASSETS],
            lengths: [0; ASSETS],
            used_bytes: 0,
        }
    }

    fn prepare_slot(&mut self, slot: usize, byte_count: usize) -> bool {
        if slot >= ASSETS || byte_count == 0 || self.lengths[slot] != 0 {
            return false;
        }
        let offset = self.used_bytes.next_multiple_of(4);
        let Some(end) = offset.checked_add(byte_count) else {
            return false;
        };
        if end > PAGES.saturating_mul(SECTOR_BYTES) {
            return false;
        }
        self.offsets[slot] = offset as u32;
        self.lengths[slot] = byte_count as u32;
        self.used_bytes = end;
        true
    }

    fn bytes_for(&self, slot: usize, byte_count: usize) -> Option<&[u8]> {
        if slot >= ASSETS || self.lengths[slot] as usize != byte_count || byte_count == 0 {
            return None;
        }
        let base = self.pages.as_ptr().cast::<u8>();
        // SAFETY: `prepare_slot` validated this stable range inside `pages`.
        Some(unsafe {
            core::slice::from_raw_parts(base.add(self.offsets[slot] as usize), byte_count)
        })
    }
}

impl<const PAGES: usize, const ASSETS: usize> WorldChunkDestination
    for PersistentAssetStorage<PAGES, ASSETS>
{
    fn slot_capacity_bytes(&self, slot: usize) -> usize {
        self.lengths.get(slot).copied().unwrap_or(0) as usize
    }

    fn write_chunk_bytes(&mut self, slot: usize, offset: usize, bytes: &[u8]) -> bool {
        let capacity = self.slot_capacity_bytes(slot);
        let Some(end) = offset.checked_add(bytes.len()) else {
            return false;
        };
        if end > capacity {
            return false;
        }
        let base = self.pages.as_mut_ptr().cast::<u8>();
        let destination = self.offsets[slot] as usize + offset;
        // SAFETY: the prepared slot and checked subrange lie inside `pages`;
        // the controller's sector buffer cannot overlap this asset arena.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(destination), bytes.len())
        };
        true
    }
}

/// Loading state for all persistent gameplay assets in one asset pack.
pub struct PersistentAssetStreamer<const PAGES: usize, const ASSETS: usize> {
    storage: PersistentAssetStorage<PAGES, ASSETS>,
    job: WorldRoomSlotsReadJob<ASSETS>,
    ids: [u16; ASSETS],
    slots: [usize; ASSETS],
    capacities: [usize; ASSETS],
    asset_count: usize,
    started: bool,
    ready: bool,
    failed: bool,
}

impl<const PAGES: usize, const ASSETS: usize> PersistentAssetStreamer<PAGES, ASSETS> {
    /// All-zero link-time image for a `.bss` runtime arena.
    pub const fn zeroed() -> Self {
        Self {
            storage: PersistentAssetStorage::new(),
            job: WorldRoomSlotsReadJob::zeroed(),
            ids: [0; ASSETS],
            slots: [0; ASSETS],
            capacities: [0; ASSETS],
            asset_count: 0,
            started: false,
            ready: false,
            failed: false,
        }
    }

    /// Initialized idle streamer.
    pub const fn new() -> Self {
        Self {
            storage: PersistentAssetStorage::new(),
            job: WorldRoomSlotsReadJob::new(),
            ids: [0; ASSETS],
            slots: [0; ASSETS],
            capacities: [0; ASSETS],
            asset_count: 0,
            started: false,
            ready: false,
            failed: false,
        }
    }

    /// Reserve stable sector runs and arm one grouped pack read.
    pub fn begin(
        &mut self,
        pack_lba: u32,
        toc: &[LevelWorldPackEntryRecord],
        assets: &[LevelAssetRecord],
    ) {
        if self.started || self.ready {
            return;
        }
        *self = Self::new();

        let mut count = 0usize;
        for asset in assets {
            if asset.flags & asset_flags::STREAMED_GAMEPLAY_PERSISTENT == 0 {
                continue;
            }
            let slot = asset.id.0 as usize;
            let byte_count = asset.ram_bytes as usize;
            if count >= ASSETS
                || slot >= ASSETS
                || byte_count == 0
                || !self.storage.prepare_slot(slot, byte_count)
            {
                self.started = true;
                self.failed = true;
                return;
            }
            self.ids[count] = asset.id.0;
            self.slots[count] = slot;
            self.capacities[count] = byte_count;
            count += 1;
        }

        self.asset_count = count;
        self.started = true;
        if count == 0 {
            self.ready = true;
            return;
        }
        self.job.start(
            pack_lba,
            toc,
            &self.ids[..count],
            &self.slots[..count],
            &self.capacities[..count],
        );
        self.finish_if_done();
    }

    /// Advance the grouped CD read by at most `max_sectors` sectors.
    pub fn pump(&mut self, cd: &mut CdController, max_sectors: usize) -> bool {
        if !self.started || self.ready || self.failed {
            return false;
        }
        self.job.poll_into(cd, &mut self.storage, max_sectors);
        self.finish_if_done()
    }

    fn finish_if_done(&mut self) -> bool {
        if !self.job.is_done() {
            return false;
        }
        let completed = self.job.completed_entries();
        let statuses = self.job.statuses();
        let mut index = 0usize;
        while index < self.asset_count {
            if statuses[index] != ROOM_CHUNK_STATUS_OK || !completed[index] {
                self.failed = true;
                return false;
            }
            index += 1;
        }
        self.ready = true;
        true
    }

    /// Whether all persistent assets have been checksum-verified in RAM.
    pub const fn ready(&self) -> bool {
        self.ready
    }

    /// Whether the load failed or the generated memory budget was invalid.
    pub const fn failed(&self) -> bool {
        self.failed
    }

    /// Loading completion in Q0.12, suitable for the loading progress bar.
    pub fn progress_q12(&self) -> i32 {
        if self.ready {
            return 4096;
        }
        if !self.started || self.asset_count == 0 {
            return 0;
        }
        let completed = self.job.completed_entries();
        let mut count = 0usize;
        let mut index = 0usize;
        while index < self.asset_count {
            count += completed[index] as usize;
            index += 1;
        }
        (count as i32).saturating_mul(4096) / self.asset_count as i32
    }

    /// Stable bytes for one persistent asset after the full load completes.
    pub fn bytes_for(&self, asset: &LevelAssetRecord) -> Option<&[u8]> {
        if !self.ready || asset.flags & asset_flags::STREAMED_GAMEPLAY_PERSISTENT == 0 {
            return None;
        }
        self.storage
            .bytes_for(asset.id.0 as usize, asset.ram_bytes as usize)
    }
}

impl<const PAGES: usize, const ASSETS: usize> Default for PersistentAssetStreamer<PAGES, ASSETS> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_assets_are_word_aligned_stable_and_independent() {
        let mut storage = PersistentAssetStorage::<1, 4>::new();
        assert!(storage.prepare_slot(1, 3));
        assert!(storage.prepare_slot(3, 5));
        assert_eq!(storage.offsets[1], 0);
        assert_eq!(storage.offsets[3], 4);
        assert!(storage.write_chunk_bytes(1, 0, &[1, 2, 3]));
        assert!(storage.write_chunk_bytes(3, 0, &[4, 5, 6, 7, 8]));
        assert_eq!(storage.bytes_for(1, 3), Some(&[1, 2, 3][..]));
        assert_eq!(storage.bytes_for(3, 5), Some(&[4, 5, 6, 7, 8][..]));
        assert!(!storage.write_chunk_bytes(1, 2, &[9, 9]));
    }

    #[test]
    fn packed_asset_budget_rejects_overflow_without_aliasing() {
        let mut storage = PersistentAssetStorage::<1, 2>::new();
        assert!(storage.prepare_slot(0, SECTOR_BYTES - 3));
        assert!(!storage.prepare_slot(1, 4));
        assert_eq!(storage.slot_capacity_bytes(1), 0);
    }
}
