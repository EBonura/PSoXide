//! Streamed-world glue for the resident BSP runtime (design 2026-10-08, M7).
//!
//! With the `world-stream` feature a cook run with `PSXED_STREAM_WORLD=stream`
//! ships the world as a small resident *top* container plus a region pack in
//! UI.PAK. [`load_streamed_world`] loads the top into a slotted map with every
//! region absent; [`BspRuntime::step_world_stream`] then reads the regions off
//! the disc and installs them, a few sectors per pump, while the loading
//! screen runs. Gameplay starts once every region is linked, so a streamed
//! world plays exactly like the whole-map world of the same geometry.

use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::render::PxbspTextureBinding;
use psx_bsp::{SliceReader, Vec3I32};
use psx_engine::RoomPoint;
use psx_game_runtime::cd_stream::{world_pack_chunk, CdController};
use psx_game_runtime::region_stream::{RegionStreamer, StreamStatus, PUMP_HISTORY};
use psx_level::MAX_ROOM_MATERIALS;

use super::{resolve_material_binding, BspRuntime, BspRuntimeInitError};
use crate::generated::{
    PXBSP_STREAM_PACK_CHUNK, PXBSP_STREAM_REGIONS, PXBSP_WORLD, UI_PACK_START_LBA, UI_PACK_TOC,
};

/// Sectors one pump reads and installs. Small enough that the CPU work of a
/// pump (checksum, checks, relocation of 4 x 2 KiB) stays a fraction of a
/// frame, so the same cadence is safe beside gameplay.
pub(super) const SECTORS_PER_PUMP: usize = 4;

/// Streamer state beside the map.
pub(super) struct WorldStream {
    pub(super) streamer: RegionStreamer,
    /// Absolute LBA of the region pack.
    pub(super) pack_lba: u32,
    /// Materials (bit per material) of every region linked so far; the only
    /// ones the renderer needs bound.
    pub(super) required_materials: u32,
    /// Ticks on which the player stood in a region that was not resident.
    pub(super) stub_hits: u32,
    /// Whether the map is a streamed one at all.
    pub(super) streamed: bool,
}

impl WorldStream {
    /// The state of a whole-map world: nothing to stream.
    pub(super) const fn whole() -> Self {
        Self {
            streamer: RegionStreamer::new(),
            pack_lba: 0,
            required_materials: 0,
            stub_hits: 0,
            streamed: false,
        }
    }
}

/// Words the emulator reads back from RAM by symbol (`PSX_WORLD_STREAM`):
/// the gates' counters.
///
/// 0 magic `WSTR`; 1 regions; 2 installed; 3 sectors; 4 pumps; 5 retries;
/// 6 last error; 7 texture waits; 8 stub hits; 9 complete; 10 slot pool bytes;
/// 11 slot count; 12.. pumps each region took, in install order; the word
/// after those is the number of read restarts.
#[no_mangle]
pub static mut PSX_WORLD_STREAM: [u32; 13 + PUMP_HISTORY] = [0; 13 + PUMP_HISTORY];

fn publish(bsp: &BspRuntime) {
    let stats = bsp.stream.streamer.stats();
    let (image, slots) = bsp.map.streaming().map_or((0, 0), |state| {
        (
            (state.slot_bytes() * state.slot_count()) as u32,
            state.slot_count() as u32,
        )
    });
    let words = [
        u32::from_le_bytes(*b"WSTR"),
        PXBSP_STREAM_REGIONS as u32,
        stats.installed,
        stats.sectors,
        stats.steps,
        stats.retries,
        stats.last_error,
        stats.texture_waits,
        bsp.stream.stub_hits,
        u32::from(bsp.stream.streamer.is_complete()),
        image,
        slots,
    ];
    let pumps = stats.region_pumps.map(u32::from);
    let words = words.into_iter().chain(pumps).chain([stats.read_restarts]);
    for (i, word) in words.enumerate() {
        // SAFETY: single-threaded guest; the symbol is written only here.
        unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(PSX_WORLD_STREAM[i]), word) };
    }
}

/// Load the cooked top container into a slotted map, every region absent.
pub(super) fn load_streamed_world() -> Result<(PxbspResidentMap, WorldStream), BspRuntimeInitError>
{
    let chunk = world_pack_chunk(UI_PACK_TOC, PXBSP_STREAM_PACK_CHUNK)
        .ok_or(BspRuntimeInitError::MissingRegionPack)?;
    let mut map = PxbspResidentMap::with_capacity(0);
    // One slot per region: the pool is the whole world, nothing is evicted.
    map.load_streamed_exact(
        0,
        &mut SliceReader::<'_>::new(PXBSP_WORLD),
        PXBSP_STREAM_REGIONS as u16,
    )
    .map_err(BspRuntimeInitError::StreamLoad)?;
    let mut streamer = RegionStreamer::new();
    if !streamer.begin(&map) {
        return Err(BspRuntimeInitError::MissingRegionPack);
    }
    Ok((
        map,
        WorldStream {
            streamer,
            pack_lba: UI_PACK_START_LBA.saturating_add(chunk.sector_offset),
            required_materials: 0,
            stub_hits: 0,
            streamed: true,
        },
    ))
}

/// Leave the reason a boot-time load failed where the emulator reads the
/// counters: the init panic compiles to a bare BREAK, so nothing else says
/// which check refused the world. Word 6 carries a variant number and word 7
/// the variant's own detail.
pub(super) fn record_init_error(error: &BspRuntimeInitError) {
    use BspRuntimeInitError as E;
    let (code, detail) = match error {
        E::EmptyWorld => (1, 0),
        E::NoMaterials => (2, 0),
        E::TooManyMaterials { count, .. } => (3, *count as u32),
        E::Map(_) => (4, 0),
        E::StreamedWorldNeedsTheFeature => (5, 0),
        E::StreamLoad(error) => (
            6,
            match error {
                psx_bsp::pxbsp_resident::stream::StreamLoadError::Map(_) => 1,
                psx_bsp::pxbsp_resident::stream::StreamLoadError::Stream(_) => 2,
                psx_bsp::pxbsp_resident::stream::StreamLoadError::Slots => 3,
            },
        ),
        E::MissingRegionPack => (7, 0),
        E::Doors(_) => (8, 0),
        E::Destructibles(_) => (9, 0),
        E::InvalidDestructibleTarget { .. } => (10, 0),
        E::MoverMappingLength => (11, 0),
        E::MoverCount { .. } => (12, 0),
        E::MoverModel { .. } => (13, 0),
        E::DuplicateMoverNode(_) => (14, 0),
        E::InvalidBodyHullTable => (15, 0),
        E::MissingWorldHull(hull) => (16, *hull as u32),
        E::MissingMoverHull { hull, .. } => (17, *hull as u32),
    };
    // SAFETY: single-threaded guest; the symbol is written only here and in `publish`.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(PSX_WORLD_STREAM[0]),
            u32::from_le_bytes(*b"WSTR"),
        );
        core::ptr::write_volatile(core::ptr::addr_of_mut!(PSX_WORLD_STREAM[6]), 1000 + code);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(PSX_WORLD_STREAM[7]), detail);
    }
}

impl BspRuntime {
    /// Whether the world can be played: always for a whole map, and for a
    /// streamed one once every region is linked.
    pub(crate) fn world_stream_ready(&self) -> bool {
        !self.stream.streamed || self.stream.streamer.is_complete()
    }

    /// Read and install regions for one pump; `true` once all are linked.
    pub(crate) fn step_world_stream(&mut self, cd: &mut CdController) -> bool {
        if !self.stream.streamed || self.stream.streamer.is_complete() {
            return true;
        }
        let Self {
            map,
            materials,
            stream,
            ..
        } = self;
        let mut textures = |map: &PxbspResidentMap, mask: u32| -> bool {
            stream.required_materials |= mask;
            resolve_mask(map, materials, mask)
        };
        let status = stream.streamer.step(
            cd,
            map,
            stream.pack_lba,
            SECTORS_PER_PUMP,
            true,
            &mut textures,
        );
        let done = match status {
            StreamStatus::Working => false,
            StreamStatus::Complete => true,
            StreamStatus::Failed(code) => {
                publish(self);
                panic!("streamed world failed to load: error {code}");
            }
        };
        publish(self);
        done
    }

    /// Whether material `index` has to be bound: every material of a whole
    /// map, and those of the linked regions of a streamed one.
    pub(super) fn material_required(&self, index: usize) -> bool {
        !self.stream.streamed || (index < 32 && self.stream.required_materials & (1 << index) != 0)
    }

    /// Count a tick on which the player stands in a region that is not
    /// resident: a stub hit, which the gates require to be zero.
    pub(super) fn probe_stub(&mut self, at: RoomPoint) {
        if !self.stream.streamed {
            return;
        }
        let point = Vec3I32 {
            x: at.x.saturating_mul(4096),
            y: at.y.saturating_mul(4096),
            z: at.z.saturating_mul(4096),
        };
        if self.map.unresident_region_at(point).is_some() {
            self.stream.stub_hits = self.stream.stub_hits.saturating_add(1);
        }
    }
}

/// Resolve the bindings of every material in `mask`; `true` when all are
/// resident in VRAM.
fn resolve_mask(
    map: &PxbspResidentMap,
    materials: &mut [Option<PxbspTextureBinding>; MAX_ROOM_MATERIALS],
    mask: u32,
) -> bool {
    let mut ready = true;
    for index in 0..MAX_ROOM_MATERIALS.min(32) {
        if mask & (1 << index) == 0 {
            continue;
        }
        let Some(material) = map.materials().get(index) else {
            continue;
        };
        ready &= resolve_material_binding(&material, index, &mut materials[index]);
    }
    ready
}
