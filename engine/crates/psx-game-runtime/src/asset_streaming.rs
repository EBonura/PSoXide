//! Persistent CD-backed gameplay assets.
//!
//! Model meshes, atlases, and animation clips are read into sector-page RAM
//! during the initial loading scene. Their addresses remain stable for the
//! gameplay lifetime, so parsed model and animation views can safely borrow
//! them without baking the source blobs into the executable.

use psx_level::{
    asset_flags, AssetId, LevelAssetRecord, LevelWorldPackEntryRecord, RoomIndex,
    RoomResidencyRecord,
};

use crate::cd_stream::{
    CdController, WorldChunkDestination, WorldRoomSlotsReadJob, ROOM_CHUNK_STATUS_OK, SECTOR_BYTES,
};

struct PersistentAssetStorage<const PAGES: usize, const ASSETS: usize> {
    pages: [[u32; SECTOR_BYTES / 4]; PAGES],
    offsets: [u32; ASSETS],
    lengths: [u32; ASSETS],
    /// Identity of the physical byte layout. Any change means live assets
    /// moved, so parsed views holding pointers into the pool must be rebuilt.
    layout_generation: u32,
}

impl<const PAGES: usize, const ASSETS: usize> PersistentAssetStorage<PAGES, ASSETS> {
    const fn new() -> Self {
        Self {
            pages: [[0; SECTOR_BYTES / 4]; PAGES],
            offsets: [0; ASSETS],
            lengths: [0; ASSETS],
            layout_generation: 0,
        }
    }

    const fn capacity_bytes() -> usize {
        PAGES * SECTOR_BYTES
    }

    /// First byte past the highest live allocation, 4-aligned.
    ///
    /// Allocation always appends here and relies on [`Self::compact`] to close
    /// gaps, which keeps the allocator O(ASSETS) with no free list. Assets are
    /// placed once per residency change, not per frame, so a linear bump plus
    /// an occasional compaction is the cheap answer.
    fn allocation_end(&self) -> usize {
        let mut end = 0usize;
        let mut slot = 0usize;
        while slot < ASSETS {
            if self.lengths[slot] != 0 {
                let slot_end = (self.offsets[slot] as usize)
                    .saturating_add(self.lengths[slot] as usize)
                    .next_multiple_of(4);
                if slot_end > end {
                    end = slot_end;
                }
            }
            slot += 1;
        }
        end
    }

    /// Total live bytes, 4-aligned per asset, ignoring gaps.
    fn live_bytes(&self) -> usize {
        let mut total = 0usize;
        let mut slot = 0usize;
        while slot < ASSETS {
            if self.lengths[slot] != 0 {
                total = total.saturating_add((self.lengths[slot] as usize).next_multiple_of(4));
            }
            slot += 1;
        }
        total
    }

    fn resident(&self, slot: usize) -> bool {
        slot < ASSETS && self.lengths[slot] != 0
    }

    /// Drop `slot`'s allocation. Its bytes stay in place until the next
    /// compaction reclaims them.
    fn release_slot(&mut self, slot: usize) {
        if slot >= ASSETS || self.lengths[slot] == 0 {
            return;
        }
        self.lengths[slot] = 0;
        self.offsets[slot] = 0;
    }

    /// Pack every live allocation toward byte zero in slot order, closing the
    /// gaps that releases left behind.
    ///
    /// This MOVES live bytes, so it bumps `layout_generation` and must not run
    /// while a CD job is writing into the pool. Allocation is the only caller,
    /// and it always completes before a job is armed.
    fn compact(&mut self) -> bool {
        let base = self.pages.as_mut_ptr().cast::<u8>();
        let mut cursor = 0usize;
        let mut moved = false;
        let mut slot = 0usize;
        while slot < ASSETS {
            let length = self.lengths[slot] as usize;
            if length != 0 {
                let from = self.offsets[slot] as usize;
                if from != cursor {
                    // SAFETY: both ranges were validated on allocation and lie
                    // inside `pages`; `cursor <= from` always, because the
                    // cursor only ever trails the slot order it is packing.
                    unsafe { core::ptr::copy(base.add(from), base.add(cursor), length) };
                    self.offsets[slot] = cursor as u32;
                    moved = true;
                }
                cursor = cursor.saturating_add(length).next_multiple_of(4);
            }
            slot += 1;
        }
        if moved {
            self.layout_generation = self.layout_generation.wrapping_add(1).max(1);
        }
        moved
    }

    fn prepare_slot(&mut self, slot: usize, byte_count: usize) -> bool {
        if slot >= ASSETS || byte_count == 0 || self.lengths[slot] != 0 {
            return false;
        }
        if self.live_bytes().saturating_add(byte_count) > Self::capacity_bytes() {
            // No compaction can create room that does not exist.
            return false;
        }
        let mut offset = self.allocation_end();
        if offset.saturating_add(byte_count) > Self::capacity_bytes() {
            self.compact();
            offset = self.allocation_end();
            if offset.saturating_add(byte_count) > Self::capacity_bytes() {
                return false;
            }
        }
        self.offsets[slot] = offset as u32;
        self.lengths[slot] = byte_count as u32;
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
        // `begin` is a one-shot transition from the zeroed/new idle state.
        // Replacing the complete streamer here materialised its sector-page
        // storage as a ~290 KiB stack temporary on MIPS, even on later calls
        // that returned through the guard above. Besides wasting cycles, that
        // left normal builds with only a few KiB between the live stack and
        // `.bss`. The fields below and `job.start` initialise every piece of
        // live metadata without ever copying the page arena itself.

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

    /// Whether `asset` is needed by any room in `desired`.
    ///
    /// The union of `required_ram` and `warm_ram` is the keep-set: an asset a
    /// neighbour will want next must survive eviction, or crossing a portal
    /// would re-read what we just discarded.
    pub fn ram_asset_required(
        asset: AssetId,
        desired: &[RoomIndex],
        room_residency: &[RoomResidencyRecord],
    ) -> bool {
        for &room in desired {
            let Some(res) = room_residency.iter().find(|r| r.room == room) else {
                continue;
            };
            if res.required_ram.iter().any(|&a| a == asset)
                || res.warm_ram.iter().any(|&a| a == asset)
            {
                return true;
            }
        }
        false
    }

    /// Make the persistent set match `desired`'s residency needs.
    ///
    /// Releases anything no desired room needs, then arms one grouped read for
    /// only the assets that are missing. An asset already resident is skipped,
    /// so walking into a neighbour that shares assets with the current room
    /// costs no CD traffic at all. Returns whether a new read was armed.
    ///
    /// Does nothing while a read is already in flight, so the caller may call
    /// this every frame and let the window settle.
    pub fn request_rooms(
        &mut self,
        pack_lba: u32,
        toc: &[LevelWorldPackEntryRecord],
        assets: &[LevelAssetRecord],
        desired: &[RoomIndex],
        room_residency: &[RoomResidencyRecord],
    ) -> bool {
        if self.failed || self.job.is_active() {
            return false;
        }
        // Evict first: a release can free the very bytes the delta needs.
        for asset in assets {
            if asset.flags & asset_flags::STREAMED_GAMEPLAY_PERSISTENT == 0 {
                continue;
            }
            let slot = asset.id.0 as usize;
            if slot < ASSETS
                && self.storage.resident(slot)
                && !Self::ram_asset_required(asset.id, desired, room_residency)
            {
                self.storage.release_slot(slot);
            }
        }

        let mut count = 0usize;
        for asset in assets {
            if asset.flags & asset_flags::STREAMED_GAMEPLAY_PERSISTENT == 0 {
                continue;
            }
            if !Self::ram_asset_required(asset.id, desired, room_residency) {
                continue;
            }
            let slot = asset.id.0 as usize;
            let byte_count = asset.ram_bytes as usize;
            if slot >= ASSETS || byte_count == 0 {
                self.failed = true;
                return false;
            }
            if self.storage.resident(slot) {
                continue;
            }
            if count >= ASSETS || !self.storage.prepare_slot(slot, byte_count) {
                self.failed = true;
                return false;
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
            return false;
        }
        self.ready = false;
        self.job.start(
            pack_lba,
            toc,
            &self.ids[..count],
            &self.slots[..count],
            &self.capacities[..count],
        );
        self.finish_if_done();
        true
    }

    /// Identity of the pool's byte layout; changes when compaction moved live
    /// assets, so anything holding decoded pointers must rebuild.
    pub const fn layout_generation(&self) -> u32 {
        self.storage.layout_generation
    }

    /// Live persistent asset bytes, for residency telemetry.
    pub fn resident_bytes(&self) -> usize {
        self.storage.live_bytes()
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

    const fn persistent_asset(id: u16, ram_bytes: u32) -> LevelAssetRecord {
        LevelAssetRecord {
            id: AssetId(id),
            kind: psx_level::AssetKind::Texture,
            bytes: &[],
            ram_bytes,
            vram_bytes: 0,
            flags: asset_flags::STREAMED_GAMEPLAY_PERSISTENT,
        }
    }

    const fn residency(
        room: u16,
        required_ram: &'static [AssetId],
        warm_ram: &'static [AssetId],
    ) -> RoomResidencyRecord {
        RoomResidencyRecord {
            room: RoomIndex(room),
            required_ram,
            required_vram: &[],
            warm_ram,
            warm_vram: &[],
        }
    }

    /// The point of the whole exercise: crossing into a neighbour that shares
    /// assets must not re-read them, and must drop only what nothing needs.
    ///
    /// Each case starts from a fresh streamer with residency staged directly, so
    /// this exercises the delta plan rather than CD job mechanics.
    #[test]
    fn requesting_a_neighbourhood_plans_only_the_missing_assets() {
        type Streamer = PersistentAssetStreamer<4, 8>;
        const ROOMS: &[RoomResidencyRecord] = &[
            // Room 0 needs asset 1 and warms asset 2 (its neighbour's).
            residency(0, &[AssetId(1)], &[AssetId(2)]),
            // Room 2 needs asset 2 and warms asset 3. Asset 1 becomes unwanted.
            residency(2, &[AssetId(2)], &[AssetId(3)]),
        ];
        let assets = [
            persistent_asset(1, 64),
            persistent_asset(2, 64),
            persistent_asset(3, 64),
        ];

        // Cold entry to room 0: both its required and warm assets are planned,
        // and the asset only room 2 wants is left alone.
        let mut cold = Streamer::new();
        assert!(cold.request_rooms(0, &[], &assets, &[RoomIndex(0)], ROOMS));
        assert_eq!(cold.asset_count, 2, "asset 3 is in neither of room 0's sets");
        assert_eq!(&cold.ids[..2], &[1, 2]);

        // Nothing missing: planning must arm no read at all.
        let mut warm = Streamer::new();
        assert!(warm.storage.prepare_slot(1, 64));
        assert!(warm.storage.prepare_slot(2, 64));
        assert!(!warm.request_rooms(0, &[], &assets, &[RoomIndex(0)], ROOMS));
        assert_eq!(warm.asset_count, 0, "resident assets are not re-read");
        assert!(warm.ready(), "an already-satisfied window is ready");

        // Crossing to room 2 with 1 and 2 resident: asset 2 is shared and must
        // survive, asset 1 is wanted by nobody, only asset 3 is read.
        let mut crossing = Streamer::new();
        assert!(crossing.storage.prepare_slot(1, 64));
        assert!(crossing.storage.prepare_slot(2, 64));
        assert!(crossing.request_rooms(0, &[], &assets, &[RoomIndex(2)], ROOMS));
        assert_eq!(crossing.asset_count, 1, "only the delta is read");
        assert_eq!(crossing.ids[0], 3);
        assert!(!crossing.storage.resident(1), "asset 1 evicted");
        assert!(crossing.storage.resident(2), "shared asset retained");
    }

    #[test]
    fn keep_set_spans_required_and_warm_across_every_desired_room() {
        type Streamer = PersistentAssetStreamer<4, 8>;
        const ROOMS: &[RoomResidencyRecord] = &[
            residency(0, &[AssetId(1)], &[AssetId(2)]),
            residency(1, &[AssetId(3)], &[]),
        ];
        let desired = [RoomIndex(0), RoomIndex(1)];
        for (id, want) in [(1u16, true), (2, true), (3, true), (4, false)] {
            assert_eq!(
                Streamer::ram_asset_required(AssetId(id), &desired, ROOMS),
                want,
                "asset {id}"
            );
        }
        // A room with no residency record contributes nothing rather than
        // dragging in everything.
        assert!(!Streamer::ram_asset_required(
            AssetId(1),
            &[RoomIndex(9)],
            ROOMS
        ));
    }

    #[test]
    fn released_asset_bytes_are_reclaimed_by_compaction() {
        // A pool exactly two allocations wide, so reclaiming is the only way a
        // third can fit. This is the property whole-level asset residency was
        // missing: the old bump allocator could never reuse a freed asset.
        let mut storage = PersistentAssetStorage::<1, 4>::new();
        let third = SECTOR_BYTES / 2;
        assert!(storage.prepare_slot(0, third));
        assert!(storage.prepare_slot(1, third));
        assert!(!storage.prepare_slot(2, third), "pool should be full");

        // Free the FIRST allocation, leaving a hole below a live one so the
        // reclaim genuinely requires moving bytes rather than just bumping down.
        assert!(storage.write_chunk_bytes(1, 0, &[7u8; 8]));
        storage.release_slot(0);
        assert!(!storage.resident(0));
        assert!(storage.resident(1));

        let before = storage.layout_generation;
        assert!(storage.prepare_slot(2, third), "compaction should reclaim");
        assert!(storage.layout_generation > before, "layout must be invalidated");

        // The surviving asset kept its contents across the move.
        assert_eq!(&storage.bytes_for(1, third).expect("live")[..8], &[7u8; 8]);
        assert_eq!(storage.offsets[1], 0, "live asset packed to zero");
    }

    #[test]
    fn compaction_cannot_invent_absent_capacity() {
        let mut storage = PersistentAssetStorage::<1, 4>::new();
        assert!(storage.prepare_slot(0, SECTOR_BYTES / 2));
        // Live bytes plus the request exceed the pool, so this must fail
        // outright rather than compacting and then aliasing.
        assert!(!storage.prepare_slot(1, SECTOR_BYTES));
        assert_eq!(storage.layout_generation, 0, "no pointless compaction");
        assert!(storage.resident(0));
        assert_eq!(storage.live_bytes(), SECTOR_BYTES / 2);
    }

    #[test]
    fn releasing_every_asset_returns_the_whole_pool() {
        let mut storage = PersistentAssetStorage::<2, 4>::new();
        assert!(storage.prepare_slot(0, SECTOR_BYTES));
        assert!(storage.prepare_slot(1, SECTOR_BYTES));
        storage.release_slot(0);
        storage.release_slot(1);
        assert_eq!(storage.live_bytes(), 0);
        assert_eq!(storage.allocation_end(), 0);
        // A single allocation spanning the entire pool now fits.
        assert!(storage.prepare_slot(2, 2 * SECTOR_BYTES));
    }

    #[test]
    fn packed_asset_budget_rejects_overflow_without_aliasing() {
        let mut storage = PersistentAssetStorage::<1, 2>::new();
        assert!(storage.prepare_slot(0, SECTOR_BYTES - 3));
        assert!(!storage.prepare_slot(1, 4));
        assert_eq!(storage.slot_capacity_bytes(1), 0);
    }

    #[test]
    fn zeroed_streamer_begins_in_place_and_is_idempotent() {
        let mut streamer = PersistentAssetStreamer::<1, 2>::zeroed();
        streamer.begin(0, &[], &[]);
        assert!(streamer.ready());
        assert!(!streamer.failed());
        assert_eq!(streamer.progress_q12(), 4096);

        // The one-shot guard must leave the completed state untouched.
        streamer.begin(99, &[], &[]);
        assert!(streamer.ready());
        assert!(!streamer.failed());
    }
}
