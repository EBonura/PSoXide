//! Persistent CD-backed gameplay assets.
//!
//! Model meshes, atlases, and animation clips are read into sector-page RAM
//! during the initial loading scene. Their addresses remain stable for the
//! gameplay lifetime, so parsed model and animation views can safely borrow
//! them without baking the source blobs into the executable.

use psx_engine::telemetry;
use psx_level::{
    asset_flags, AssetId, AssetKind, LevelAssetRecord, LevelWorldPackEntryRecord, RoomIndex,
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
    /// This MOVES live bytes, so it bumps `layout_generation`. Allocation
    /// deliberately does NOT call it: `RuntimeModelAsset` holds a
    /// `Model<'static>` borrowed from the pool, and relocating under it would
    /// dangle. Reserved for a boundary where no parsed view is live, such as a
    /// level change, and the caller must rebuild views on a generation change.
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

    /// Lowest offset at which `byte_count` fits between live allocations.
    ///
    /// First fit, and deliberately never relocates anything: `RuntimeModelAsset`
    /// holds a `Model<'static>` borrowed from this pool, so moving live bytes
    /// would dangle it. Fragmentation is the accepted cost, and
    /// [`Self::prepare_slot`] reports the failure rather than papering over it.
    fn find_gap(&self, byte_count: usize) -> Option<usize> {
        let mut cursor = 0usize;
        loop {
            // Lowest live allocation starting at or after the cursor.
            let mut next_start = Self::capacity_bytes();
            let mut next_end = Self::capacity_bytes();
            let mut slot = 0usize;
            while slot < ASSETS {
                if self.lengths[slot] != 0 {
                    let start = self.offsets[slot] as usize;
                    if start >= cursor && start < next_start {
                        next_start = start;
                        next_end = start
                            .saturating_add(self.lengths[slot] as usize)
                            .next_multiple_of(4);
                    }
                }
                slot += 1;
            }
            if next_start.saturating_sub(cursor) >= byte_count {
                return Some(cursor);
            }
            if next_end >= Self::capacity_bytes() {
                return None;
            }
            cursor = next_end;
        }
    }

    fn prepare_slot(&mut self, slot: usize, byte_count: usize) -> bool {
        if slot >= ASSETS || byte_count == 0 || self.lengths[slot] != 0 {
            return false;
        }
        let Some(offset) = self.find_gap(byte_count) else {
            return false;
        };
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

/// Failure reason: the asset's cooked record is unusable (slot id out of
/// range, or a zero byte count). A cook problem, not a disc problem.
pub const ASSET_FAIL_BAD_RECORD: u32 = 100;

/// Failure reason: the pool had no gap large enough. Either the cooked budget
/// is too small for the neighbourhood, or first-fit fragmented past serving it.
pub const ASSET_FAIL_NO_SPACE: u32 = 101;

/// Failure reason: the read reported no error but delivered fewer bytes than
/// the TOC promised, or the payload did not checksum.
pub const ASSET_FAIL_SHORT_READ: u32 = 102;

/// Reported in place of an asset id when a failure cannot be pinned to one.
pub const ASSET_ID_UNKNOWN: u16 = u16::MAX;

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
    /// Asset id of the first failure, [`ASSET_ID_UNKNOWN`] if unattributed.
    failed_asset: u16,
    /// Chunk status or `ASSET_FAIL_*` reason behind that first failure.
    failed_reason: u32,
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
            failed_asset: ASSET_ID_UNKNOWN,
            failed_reason: 0,
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
            failed_asset: ASSET_ID_UNKNOWN,
            failed_reason: 0,
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
            if count >= ASSETS || slot >= ASSETS || byte_count == 0 {
                self.started = true;
                self.fail(asset.id.0, ASSET_FAIL_BAD_RECORD);
                return;
            }
            if !self.storage.prepare_slot(slot, byte_count) {
                self.started = true;
                self.fail(asset.id.0, ASSET_FAIL_NO_SPACE);
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

    /// Whether `asset` must keep its RAM bytes for any room in `desired`.
    ///
    /// The keep-set spans all four cooked sets, not just the RAM pair. A texture
    /// is listed only under `required_vram`/`warm_vram` -- cortex_v1's persistent
    /// textures are ids 27 and 33, and neither appears in any `required_ram` --
    /// but VRAM is uploaded FROM these RAM bytes, so dropping them leaves the
    /// texture unable to re-upload once its VRAM slot is evicted. Keying
    /// eviction on the RAM sets alone silently discarded every persistent
    /// texture on the first residency pass and only looked correct because the
    /// already-uploaded VRAM copies survived.
    ///
    /// The warm sets are included so crossing a portal does not evict exactly
    /// what the neighbour is about to want.
    pub fn ram_asset_required(
        asset: AssetId,
        desired: &[RoomIndex],
        room_residency: &[RoomResidencyRecord],
    ) -> bool {
        for &room in desired {
            let Some(res) = room_residency.iter().find(|r| r.room == room) else {
                continue;
            };
            let listed = res.required_ram.iter().any(|&a| a == asset)
                || res.warm_ram.iter().any(|&a| a == asset)
                || res.required_vram.iter().any(|&a| a == asset)
                || res.warm_vram.iter().any(|&a| a == asset);
            if listed {
                return true;
            }
        }
        false
    }

    /// Whether an asset of this kind may be released while gameplay runs.
    ///
    /// Texture payloads are read once at VRAM upload and the uploaded copy is
    /// what gets sampled, so dropping the RAM bytes is safe and they reload from
    /// the keep-set if a room wants them again.
    ///
    /// Model meshes and animation clips are NOT safe. `RuntimeModelAsset` holds a
    /// `Model<'static>` borrowed from this pool, and clips are read the same way.
    /// Releasing one leaves that view over a hole, which the next first-fit
    /// allocation would overwrite underneath it. They stay pinned for the level;
    /// paging them needs a rebuild path that does not exist yet.
    const fn evictable(kind: AssetKind) -> bool {
        matches!(kind, AssetKind::Texture | AssetKind::RoomWorld)
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
            if !Self::evictable(asset.kind) {
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
                self.fail(asset.id.0, ASSET_FAIL_BAD_RECORD);
                return false;
            }
            if self.storage.resident(slot) {
                continue;
            }
            if count >= ASSETS {
                self.fail(asset.id.0, ASSET_FAIL_BAD_RECORD);
                return false;
            }
            if !self.storage.prepare_slot(slot, byte_count) {
                self.fail(asset.id.0, ASSET_FAIL_NO_SPACE);
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
        // The session-lifetime asset read happens behind a loading screen, so
        // there is nothing to stay responsive for and every reason to keep up
        // with the drive: sectors that land between frames are stepped over,
        // and the only evidence is a checksum failure later.
        self.job.set_wait_for_sectors(true);
        self.job.poll_into(cd, &mut self.storage, max_sectors);
        self.finish_if_done()
    }

    /// Latch the failure and report it exactly once.
    ///
    /// `failed` is sticky and makes `pump` return early forever, so without
    /// this the only symptom is a loading screen that never advances. Every
    /// path that gives up routes through here, and each names the asset and the
    /// reason: a bare failure count leaves the whole streamed asset set as the
    /// search space.
    fn fail(&mut self, asset: u16, reason: u32) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.failed_asset = asset;
        self.failed_reason = reason;
        telemetry::counter(telemetry::counter::PERSISTENT_ASSET_LOAD_FAILURES, 1);
        telemetry::counter(telemetry::counter::PERSISTENT_ASSET_FAILED_ID, asset as u32);
        telemetry::counter(telemetry::counter::PERSISTENT_ASSET_FAILED_REASON, reason);
    }

    /// Asset id behind the first failure, [`ASSET_ID_UNKNOWN`] if unattributed.
    pub const fn failed_asset(&self) -> u16 {
        self.failed_asset
    }

    /// Chunk status or `ASSET_FAIL_*` reason behind the first failure.
    pub const fn failed_reason(&self) -> u32 {
        self.failed_reason
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
                // A completed-but-unverified entry has an OK status, so report
                // the read timeout that a short delivery amounts to.
                let reason = if statuses[index] != ROOM_CHUNK_STATUS_OK {
                    statuses[index]
                } else {
                    ASSET_FAIL_SHORT_READ
                };
                self.fail(self.ids[index], reason);
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
        persistent_asset_of(id, ram_bytes, AssetKind::Texture)
    }

    const fn persistent_asset_of(id: u16, ram_bytes: u32, kind: AssetKind) -> LevelAssetRecord {
        LevelAssetRecord {
            id: AssetId(id),
            kind,
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

    /// Model bytes back a live `Model<'static>`, so they must survive an
    /// eviction pass even when no desired room lists them.
    #[test]
    fn model_assets_are_never_evicted() {
        type Streamer = PersistentAssetStreamer<4, 8>;
        const ROOMS: &[RoomResidencyRecord] = &[residency(0, &[AssetId(1)], &[])];
        let assets = [
            persistent_asset(1, 64),
            persistent_asset(2, 64),
            persistent_asset_of(3, 64, AssetKind::ModelMesh),
            persistent_asset_of(4, 64, AssetKind::ModelAnimation),
        ];
        let mut streamer = Streamer::new();
        for slot in 1..=4 {
            assert!(streamer.storage.prepare_slot(slot, 64));
        }
        streamer.request_rooms(0, &[], &assets, &[RoomIndex(0)], ROOMS);
        assert!(streamer.storage.resident(1), "required texture kept");
        assert!(!streamer.storage.resident(2), "unwanted texture evicted");
        assert!(streamer.storage.resident(3), "model mesh pinned");
        assert!(streamer.storage.resident(4), "animation clip pinned");
    }

    #[test]
    fn keep_set_spans_required_and_warm_across_every_desired_room() {
        type Streamer = PersistentAssetStreamer<4, 8>;
        const ROOMS: &[RoomResidencyRecord] = &[
            residency(0, &[AssetId(1)], &[AssetId(2)]),
            RoomResidencyRecord {
                room: RoomIndex(1),
                required_ram: &[AssetId(3)],
                required_vram: &[AssetId(5)],
                warm_ram: &[],
                warm_vram: &[],
            },
        ];
        let desired = [RoomIndex(0), RoomIndex(1)];
        // Asset 5 is listed only under required_vram, which still needs its RAM
        // bytes alive to upload from.
        for (id, want) in [(1u16, true), (2, true), (3, true), (5, true), (4, false)] {
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

    /// The property whole-level residency was missing: a freed asset's bytes
    /// must become available again. Crucially the hole is reused IN PLACE, with
    /// no relocation, because `RuntimeModelAsset` holds a `Model<'static>`
    /// borrowed from this pool.
    #[test]
    fn a_released_hole_is_reused_without_moving_live_assets() {
        let mut storage = PersistentAssetStorage::<1, 4>::new();
        let third = SECTOR_BYTES / 4;
        assert!(storage.prepare_slot(0, third));
        assert!(storage.prepare_slot(1, third));
        assert!(storage.prepare_slot(2, third));
        let live_offset = storage.offsets[2];
        assert!(storage.write_chunk_bytes(2, 0, &[7u8; 8]));

        // Free the MIDDLE asset, so reuse requires finding an interior gap
        // rather than just bumping down from the end.
        storage.release_slot(1);
        assert!(!storage.resident(1));

        assert!(storage.prepare_slot(3, third), "interior hole must be reused");
        assert_eq!(
            storage.offsets[3], storage.offsets[0] + third as u32,
            "new asset lands in the freed hole"
        );
        assert_eq!(storage.layout_generation, 0, "nothing may be relocated");
        assert_eq!(storage.offsets[2], live_offset, "live asset did not move");
        assert_eq!(&storage.bytes_for(2, third).expect("live")[..8], &[7u8; 8]);
    }

    #[test]
    fn allocation_fails_rather_than_relocating_when_fragmented() {
        let mut storage = PersistentAssetStorage::<1, 4>::new();
        let quarter = SECTOR_BYTES / 4;
        for slot in 0..4 {
            assert!(storage.prepare_slot(slot, quarter));
        }
        // Free two non-adjacent quarters: half the pool is free, but no single
        // gap is larger than a quarter.
        storage.release_slot(0);
        storage.release_slot(2);
        assert_eq!(storage.live_bytes(), SECTOR_BYTES / 2);

        // A half-pool request cannot be satisfied without moving live bytes,
        // which is forbidden, so it must fail visibly.
        let mut wide = PersistentAssetStorage::<1, 5>::new();
        core::mem::swap(&mut wide.pages, &mut storage.pages);
        wide.offsets[..4].copy_from_slice(&storage.offsets[..4]);
        wide.lengths[..4].copy_from_slice(&storage.lengths[..4]);
        assert!(!wide.prepare_slot(4, SECTOR_BYTES / 2));
        assert_eq!(wide.layout_generation, 0, "no relocation attempted");
        // The quarter-sized holes are still usable.
        assert!(wide.prepare_slot(4, quarter));
    }

    /// `compact` stays available for a boundary where no parsed view is live.
    #[test]
    fn explicit_compaction_closes_gaps_and_flags_the_move() {
        let mut storage = PersistentAssetStorage::<1, 4>::new();
        let quarter = SECTOR_BYTES / 4;
        assert!(storage.prepare_slot(0, quarter));
        assert!(storage.prepare_slot(1, quarter));
        assert!(storage.write_chunk_bytes(1, 0, &[9u8; 8]));
        storage.release_slot(0);

        assert!(storage.compact());
        assert!(storage.layout_generation > 0, "callers must rebuild views");
        assert_eq!(storage.offsets[1], 0, "survivor packed to zero");
        assert_eq!(&storage.bytes_for(1, quarter).expect("live")[..8], &[9u8; 8]);
        // A full-pool allocation now fits where it previously could not.
        assert!(storage.prepare_slot(2, SECTOR_BYTES - quarter));
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

    /// A pack the streamer cannot satisfy must latch `failed`, not sit in a
    /// half-started state that `pump` keeps returning early from with nothing
    /// to show for it.
    #[test]
    fn an_unsatisfiable_pack_fails_instead_of_waiting() {
        // One page of pool, one asset asking for four.
        let mut streamer = PersistentAssetStreamer::<1, 2>::new();
        streamer.begin(0, &[], &[persistent_asset(0, 4 * SECTOR_BYTES as u32)]);
        assert!(streamer.failed(), "an unallocatable asset must fail at begin");
        assert!(!streamer.ready());
        assert!(!streamer.pump(&mut CdController::zeroed(), 8), "no progress");
        // Naming the asset is the point: a bare count leaves every streamed
        // asset in the level as the search space.
        assert_eq!(streamer.failed_asset(), 0);
        assert_eq!(streamer.failed_reason(), ASSET_FAIL_NO_SPACE);

        // A record the cooker should never emit is a different diagnosis.
        let mut bad = PersistentAssetStreamer::<1, 2>::new();
        bad.begin(0, &[], &[persistent_asset(7, 64)]);
        assert_eq!(bad.failed_asset(), 7, "slot id out of range names the asset");
        assert_eq!(bad.failed_reason(), ASSET_FAIL_BAD_RECORD);
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
