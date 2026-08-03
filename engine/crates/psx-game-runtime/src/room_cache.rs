//! Active-room payload vocabulary and surface-cache policy, carved out
//! of `editor-playtest`'s `active_room_cache` module (phase 1, slice 2
//! of docs/game-runtime-plan.md). Owns the window record types
//! ([`ActiveRuntimeRoom`], [`ActiveRoomWindowJob`]), the cooked
//! surface-cache resolution, and the material/prebuilt-quad pools as
//! owned structs; streamed-slot resolution stays with the game's
//! streaming layer (it moves in the next slice) and arrives here as
//! already-resolved values.

use psx_engine::{
    cached_room_cells_from_level_records, cached_room_surfaces_from_level_records,
    cached_room_vertices_from_level_records, CachedRoomCell, CachedRoomSurface, ProjectedVertex,
    RoomRender, RuntimeCollisionRoom, RuntimeRoom, WorldRenderMaterial, WorldVertex,
};
use psx_gpu::material::TextureMaterial;
use psx_gpu::prim::QuadTexturedGouraud;
use psx_level::{
    find_asset_of_kind, AssetKind, LevelAssetRecord, LevelCachedRoomCellRecord,
    LevelCachedRoomSurfaceRecord, LevelCachedRoomVertexRecord, LevelRoomRecord,
    LevelRoomSurfaceCacheRecord, RoomIndex, RuntimeDebugMask, MAX_ROOM_MATERIALS,
};

/// Sentinel for "no room" in fixed-size room-index arrays.
pub const INVALID_ROOM_INDEX: RoomIndex = RoomIndex(u16::MAX);

/// Horizontal X origin of a room in engine units (`origin_x` is stored
/// in sectors).
pub fn room_origin_x(record: &LevelRoomRecord) -> i32 {
    record.origin_x.saturating_mul(record.sector_size)
}

/// Horizontal Z origin of a room in engine units (`origin_z` is stored
/// in sectors).
pub fn room_origin_z(record: &LevelRoomRecord) -> i32 {
    record.origin_z.saturating_mul(record.sector_size)
}

/// Vertical origin of a room in engine units. Unlike X/Z (`origin_*` in
/// sectors), `origin_y` is already stored in engine units, so it is used
/// directly. Drives Y rebasing across room transitions for stacked floors.
pub fn room_origin_y(record: &LevelRoomRecord) -> i32 {
    record.origin_y
}

/// Resolved surface-cache window of one active room.
#[derive(Copy, Clone)]
pub struct ActiveRoomSurfaceCache {
    /// First cached cell record for the room.
    pub cell_first: usize,
    /// Cached cell record count.
    pub cell_count: usize,
    /// First per-cell vertex index for the room.
    pub cell_vertex_first: usize,
    /// Per-cell vertex index count.
    pub cell_vertex_count: usize,
    /// First cached vertex record for the room.
    pub vertex_first: usize,
    /// Cached vertex record count.
    pub vertex_count: usize,
    /// First cached surface record for the room.
    pub surface_first: usize,
    /// Cached surface record count.
    pub surface_count: usize,
    /// Resolution status.
    pub status: ActiveRoomCacheStatus,
    /// Whether the cache resolved and is drawable through the cached path.
    pub ready: bool,
}

impl ActiveRoomSurfaceCache {
    /// Unresolved cache window.
    pub const EMPTY: Self = Self {
        cell_first: 0,
        cell_count: 0,
        cell_vertex_first: 0,
        cell_vertex_count: 0,
        vertex_first: 0,
        vertex_count: 0,
        surface_first: 0,
        surface_count: 0,
        status: ActiveRoomCacheStatus::NotBuilt,
        ready: false,
    };
}

/// Surface-cache resolution status.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ActiveRoomCacheStatus {
    /// Cache resolved; the cached draw path can run.
    Ready,
    /// No cache record was cooked (or resolution has not run).
    NotBuilt,
    /// Cache exceeds a runtime budget or its record pool bounds.
    Overflow,
    /// Cache record exists but carries no drawable geometry.
    Empty,
}

/// One resident room in the active render/collision window.
#[derive(Copy, Clone)]
pub struct ActiveRuntimeRoom {
    /// Index in the cooked `ROOMS` table.
    pub index: RoomIndex,
    /// Resident streamed slot, or the game's "none" sentinel.
    pub stream_slot: u16,
    /// Parsed render payload; `None` when only collision is resident.
    pub render_room: Option<RuntimeRoom<'static>>,
    /// Parsed collision payload.
    pub collision_room: RuntimeCollisionRoom<'static>,
    /// Room width in sectors.
    pub width: u16,
    /// Room depth in sectors.
    pub depth: u16,
    /// Sector size in engine units.
    pub sector_size: i32,
    /// Ambient RGB for the room.
    pub ambient_rgb: [u8; 3],
    /// Non-streamed builds keep materials inline; streamed builds pool them by
    /// `stream_slot` (see [`RoomMaterialPool`]) to keep this struct small.
    #[cfg(not(feature = "cd-stream-bench"))]
    pub materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    /// In-use length of `materials`.
    #[cfg(not(feature = "cd-stream-bench"))]
    pub material_count: usize,
    /// Offset from the current chunk's origin to this chunk's
    /// origin, in engine units.
    pub offset_x: i32,
    /// See `offset_x`.
    pub offset_z: i32,
    /// Vertical offset from the current room's elevation to this room's,
    /// in engine units. Stacked floors cook to distinct `origin_y`; this
    /// places the room's geometry at its real height relative to the
    /// camera so an upper floor renders a storey up, not on top of the
    /// current one at Y=0.
    pub offset_y: i32,
    /// Resolved surface-cache window for this room.
    pub surface_cache: ActiveRoomSurfaceCache,
}

impl ActiveRuntimeRoom {
    /// Render view over the parsed room payload, when resident.
    pub fn render(&self) -> Option<RoomRender<'static, '_>> {
        self.render_room.as_ref().map(|room| room.render())
    }

    /// In-use room-surface materials. Streamed builds read the `stream_slot`
    /// pool; non-streamed builds read the inline array.
    #[cfg(feature = "cd-stream-bench")]
    pub fn materials<'p, const SLOTS: usize>(
        &self,
        pool: &'p RoomMaterialPool<SLOTS>,
    ) -> &'p [WorldRenderMaterial] {
        let slot = self.stream_slot as usize;
        if slot < SLOTS {
            return &pool.slots[slot].materials[..pool.slots[slot].count];
        }
        &[]
    }

    /// In-use room-surface materials. Streamed builds read the `stream_slot`
    /// pool; non-streamed builds read the inline array.
    #[cfg(not(feature = "cd-stream-bench"))]
    pub fn materials(&self) -> &[WorldRenderMaterial] {
        &self.materials[..self.material_count]
    }

    /// Rebase this room's chunk offsets relative to `current_record`.
    pub fn with_current_room_offsets(
        mut self,
        record: &LevelRoomRecord,
        current_record: &LevelRoomRecord,
    ) -> Self {
        self.offset_x = room_origin_x(record).saturating_sub(room_origin_x(current_record));
        self.offset_z = room_origin_z(record).saturating_sub(room_origin_z(current_record));
        // `origin_y` is absolute engine units (not sector-scaled like
        // x/z), so the vertical offset is a plain record difference.
        self.offset_y = record.origin_y.saturating_sub(current_record.origin_y);
        self
    }
}

/// Reuse a previous window entry for `index` when it still occupies the
/// same stream slot, re-based onto the current room's frame; `None`
/// sends the caller down its build path.
#[inline]
pub fn reuse_active_room<const MAX_ACTIVE_ROOMS: usize>(
    previous_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    index: RoomIndex,
    stream_slot: u16,
    record: &LevelRoomRecord,
    current_record: &LevelRoomRecord,
) -> Option<ActiveRuntimeRoom> {
    for previous in previous_rooms.iter().flatten().copied() {
        if previous.index != index || previous.stream_slot != stream_slot {
            continue;
        }
        return Some(previous.with_current_room_offsets(record, current_record));
    }
    None
}

/// Incremental active-room window rebuild. The old window stays
/// drawable until the staged replacement is ready.
#[derive(Copy, Clone)]
pub struct ActiveRoomWindowJob<const MAX_ACTIVE_ROOMS: usize> {
    /// Whether a rebuild is in progress.
    pub active: bool,
    /// Whether the job should also refresh streaming when it lands.
    pub update_streaming: bool,
    /// Room the job was started for; a room change abandons the job.
    pub current_room: RoomIndex,
    /// Rooms the job intends to build, in request order.
    pub requested_rooms: [RoomIndex; MAX_ACTIVE_ROOMS],
    /// In-use length of `requested_rooms`.
    pub requested_count: usize,
    /// Next `requested_rooms` entry to build.
    pub cursor: usize,
    /// Next free slot in `rooms`.
    pub next_slot: usize,
    /// The staged replacement window.
    pub rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    /// The window that was live when the job began (reuse source).
    pub previous_rooms: [Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
}

impl<const MAX_ACTIVE_ROOMS: usize> ActiveRoomWindowJob<MAX_ACTIVE_ROOMS> {
    /// Idle job.
    pub const EMPTY: Self = Self {
        active: false,
        update_streaming: false,
        current_room: RoomIndex::ZERO,
        requested_rooms: [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS],
        requested_count: 0,
        cursor: 0,
        next_slot: 0,
        rooms: [const { None }; MAX_ACTIVE_ROOMS],
        previous_rooms: [const { None }; MAX_ACTIVE_ROOMS],
    };
}

/// Parse a room's cooked `.psxw` world payload out of the asset table.
pub fn parse_runtime_room(
    assets: &'static [LevelAssetRecord],
    record: &LevelRoomRecord,
) -> Option<RuntimeRoom<'static>> {
    let asset = find_asset_of_kind(assets, record.world_asset, AssetKind::RoomWorld)?;
    RuntimeRoom::from_bytes(asset.bytes).ok()
}

// Retained after the BFS-ring residency rewrite (the desired-set is now copied
// from the cached stream ring); kept for other build paths / future reuse.
/// Untextured fallback material on the game's shared room texture page.
pub const fn room_material_fallback(tpage_word: u16) -> WorldRenderMaterial {
    WorldRenderMaterial::both(TextureMaterial::opaque(0, tpage_word, (0x80, 0x80, 0x80)))
}

/// Refactor B: room-surface materials live in a pool keyed by the resident
/// `stream_slot` rather than inline in [`ActiveRuntimeRoom`], so the
/// per-crossing copy of the active-room window stays small. An entry is
/// (re)built whenever a room becomes active in its slot and read at render
/// through [`ActiveRuntimeRoom::materials`].
#[cfg(feature = "cd-stream-bench")]
pub struct RoomMaterialPool<const SLOTS: usize> {
    slots: [ResidentRoomMaterials; SLOTS],
}

#[cfg(feature = "cd-stream-bench")]
#[derive(Copy, Clone)]
struct ResidentRoomMaterials {
    materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
    count: usize,
}

#[cfg(feature = "cd-stream-bench")]
impl<const SLOTS: usize> RoomMaterialPool<SLOTS> {
    /// Pool with every slot on `fallback` and a zero in-use count.
    pub const fn new(fallback: WorldRenderMaterial) -> Self {
        Self {
            slots: [ResidentRoomMaterials {
                materials: [fallback; MAX_ROOM_MATERIALS],
                count: 0,
            }; SLOTS],
        }
    }

    /// All-zero-bytes placeholder so a game can hold this pool inside a
    /// link-time-zero (`.bss`) arena static instead of storing `new`'s
    /// fallback-material image in the flat PSX-EXE. The value is NOT
    /// ready for use: call [`Self::init`] over it (once, before first
    /// use) to stamp the fallback state.
    pub const fn zeroed() -> Self {
        // SAFETY: the pool is plain old data plus fieldless enums whose
        // discriminant 0 is a valid variant (`BlendMode::Opaque`,
        // `SurfaceSidedness::Front`); every material read is gated by its
        // slot's `count`, zero until `init`/`store` writes it.
        unsafe { core::mem::zeroed() }
    }

    /// Stamp `new(fallback)`'s state onto zeroed storage, slot by slot
    /// (avoids materializing the whole pool as a temporary).
    pub fn init(&mut self, fallback: WorldRenderMaterial) {
        let empty = ResidentRoomMaterials {
            materials: [fallback; MAX_ROOM_MATERIALS],
            count: 0,
        };
        for slot in self.slots.iter_mut() {
            *slot = empty;
        }
    }

    /// Store a room's material table in its resident stream slot.
    pub fn store(
        &mut self,
        stream_slot: u16,
        materials: [WorldRenderMaterial; MAX_ROOM_MATERIALS],
        count: usize,
    ) {
        let slot = stream_slot as usize;
        if slot < SLOTS {
            self.slots[slot] = ResidentRoomMaterials { materials, count };
        }
    }
}

/// Prebuilt room-quad pool (docs/perf-30fps.md): precompiled GP0(3Ch)
/// packets for static room surfaces. One slot per recently drawn room;
/// a packet is fully constructed the first frame its surface is drawn
/// for the owning room (per-surface validity bytes, zeroed on claim)
/// and only position/colour-patched afterwards. Lives OUTSIDE the
/// per-frame arena; the present flip's DMA drain makes in-place
/// patching safe.
pub struct PrebuiltRoomQuads<const SLOTS: usize, const CAP: usize> {
    quads: [[QuadTexturedGouraud; CAP]; SLOTS],
    valid: [[u8; CAP]; SLOTS],
    rooms: [RoomIndex; SLOTS],
    next: u8,
}

impl<const SLOTS: usize, const CAP: usize> PrebuiltRoomQuads<SLOTS, CAP> {
    /// Empty pool with no rooms claimed.
    pub const EMPTY: Self = Self {
        quads: [const { [QuadTexturedGouraud::EMPTY; CAP] }; SLOTS],
        valid: [[0u8; CAP]; SLOTS],
        rooms: [INVALID_ROOM_INDEX; SLOTS],
        next: 0,
    };

    /// All-zero-bytes placeholder so a game can hold this pool inside a
    /// link-time-zero (`.bss`) arena static instead of storing `EMPTY`'s
    /// image (~`SLOTS * CAP` packets) in the flat PSX-EXE. Built from
    /// honest zero-value literals: `EMPTY`'s packet and validity words
    /// are already all-zero, so only the room sentinels differ. The value
    /// is NOT ready for use: call [`Self::reset_claims`] over it (once,
    /// before first use) to stamp the sentinels, which makes it equal to
    /// `EMPTY` bit for bit.
    pub const fn zeroed() -> Self {
        Self {
            quads: [const { [QuadTexturedGouraud::EMPTY; CAP] }; SLOTS],
            valid: [[0u8; CAP]; SLOTS],
            rooms: [RoomIndex(0); SLOTS],
            next: 0,
        }
    }

    /// Stamp `EMPTY`'s claim state (no rooms claimed) onto zeroed
    /// storage. Only the room sentinels and the round-robin cursor are
    /// written: the packet and validity words of `EMPTY` are all-zero
    /// already, and every packet read is gated by its validity byte.
    pub fn reset_claims(&mut self) {
        self.rooms = [INVALID_ROOM_INDEX; SLOTS];
        self.next = 0;
    }

    /// Prebuilt-quad pool slices for `room`, claiming a slot round-robin
    /// on first use. A claim ZEROES the slot's per-surface validity bytes,
    /// so every surface fully reconstructs its packet on its first visible
    /// frame for this room and is position/colour-patched afterwards. With
    /// 8 slots and at most `visible_chunk_limit` (6) rooms drawn per
    /// frame, a slot claimed this frame cannot be re-stolen before its
    /// draw runs.
    pub fn claim(&mut self, room: RoomIndex) -> (&mut [QuadTexturedGouraud], &mut [u8]) {
        let mut i = 0usize;
        while i < SLOTS {
            if self.rooms[i] == room {
                return (&mut self.quads[i][..], &mut self.valid[i][..]);
            }
            i += 1;
        }
        let slot = (self.next as usize) % SLOTS;
        self.next = self.next.wrapping_add(1);
        self.rooms[slot] = room;
        let valid = &mut self.valid[slot];
        let mut j = 0usize;
        while j < valid.len() {
            valid[j] = 0;
            j += 1;
        }
        (&mut self.quads[slot][..], &mut self.valid[slot][..])
    }
}

/// The four parallel slices a resolved room-surface cache hands back:
/// cells, the cell-to-vertex index, vertices, and surfaces.
pub type ResolvedRoomSurfaceCache = (
    &'static [CachedRoomCell],
    &'static [u16],
    &'static [WorldVertex],
    &'static [CachedRoomSurface],
);

/// Resolve `index`'s surface-cache window. A streamed candidate (already
/// resolved by the game's streaming layer) wins; otherwise the cooked
/// surface-cache table is validated against the cache record pools.
pub fn active_room_surface_cache_for<const MAX_CACHED_ROOM_VERTICES: usize>(
    streamed: Option<ActiveRoomSurfaceCache>,
    room_surface_caches: &'static [LevelRoomSurfaceCacheRecord],
    cache_cells: &'static [LevelCachedRoomCellRecord],
    cache_cell_vertices: &'static [u16],
    cache_vertices: &'static [LevelCachedRoomVertexRecord],
    cache_surfaces: &'static [LevelCachedRoomSurfaceRecord],
    index: RoomIndex,
) -> ActiveRoomSurfaceCache {
    if let Some(cache) = streamed {
        return cache;
    }

    let Some(cache) = room_surface_caches.iter().find(|cache| cache.room == index) else {
        return ActiveRoomSurfaceCache::EMPTY;
    };
    let cell_first = cache.cell_first as usize;
    let cell_count = cache.cell_count as usize;
    let cell_vertex_first = cache.cell_vertex_first as usize;
    let cell_vertex_count = cache.cell_vertex_count as usize;
    let vertex_first = cache.vertex_first as usize;
    let vertex_count = cache.vertex_count as usize;
    let surface_first = cache.surface_first as usize;
    let surface_count = cache.surface_count as usize;
    if vertex_count > MAX_CACHED_ROOM_VERTICES
        || cell_first.saturating_add(cell_count) > cache_cells.len()
        || cell_vertex_first.saturating_add(cell_vertex_count) > cache_cell_vertices.len()
        || vertex_first.saturating_add(vertex_count) > cache_vertices.len()
        || surface_first.saturating_add(surface_count) > cache_surfaces.len()
    {
        return ActiveRoomSurfaceCache {
            status: ActiveRoomCacheStatus::Overflow,
            ..ActiveRoomSurfaceCache::EMPTY
        };
    }
    if cell_count == 0 || vertex_count == 0 || surface_count == 0 {
        return ActiveRoomSurfaceCache {
            status: ActiveRoomCacheStatus::Empty,
            ..ActiveRoomSurfaceCache::EMPTY
        };
    }
    ActiveRoomSurfaceCache {
        cell_first,
        cell_count,
        cell_vertex_first,
        cell_vertex_count,
        vertex_first,
        vertex_count,
        surface_first,
        surface_count,
        status: ActiveRoomCacheStatus::Ready,
        ready: true,
    }
}

/// Slice the cooked cache record pools for a generated-path cache
/// window, re-checking bounds against the pools.
pub fn generated_room_surface_cache_slices<const MAX_CACHED_ROOM_VERTICES: usize>(
    cache_cells: &'static [LevelCachedRoomCellRecord],
    cache_cell_vertices: &'static [u16],
    cache_vertices: &'static [LevelCachedRoomVertexRecord],
    cache_surfaces: &'static [LevelCachedRoomSurfaceRecord],
    cache: ActiveRoomSurfaceCache,
) -> Option<ResolvedRoomSurfaceCache> {
    if !cache.ready || cache.vertex_count > MAX_CACHED_ROOM_VERTICES {
        return None;
    }
    let cell_end = cache.cell_first.checked_add(cache.cell_count)?;
    let cell_vertex_end = cache
        .cell_vertex_first
        .checked_add(cache.cell_vertex_count)?;
    let vertex_end = cache.vertex_first.checked_add(cache.vertex_count)?;
    let surface_end = cache.surface_first.checked_add(cache.surface_count)?;
    let cells = cache_cells.get(cache.cell_first..cell_end)?;
    let cell_vertices = cache_cell_vertices.get(cache.cell_vertex_first..cell_vertex_end)?;
    let vertices = cache_vertices.get(cache.vertex_first..vertex_end)?;
    let surfaces = cache_surfaces.get(cache.surface_first..surface_end)?;
    Some((
        cached_room_cells_from_level_records(cells),
        cell_vertices,
        cached_room_vertices_from_level_records(vertices),
        cached_room_surfaces_from_level_records(surfaces),
    ))
}

/// A cache failed when it is not ready for a reason other than being
/// legitimately empty.
pub fn active_surface_cache_failed(cache: ActiveRoomSurfaceCache) -> bool {
    !cache.ready && cache.status != ActiveRoomCacheStatus::Empty
}

/// Whether the active window holds `index` with a drawable payload (the
/// current room is always considered drawable).
pub fn active_room_contains_drawable<const MAX_ACTIVE_ROOMS: usize>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    current_room: RoomIndex,
    index: RoomIndex,
) -> bool {
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if active.index == index
                && (index == current_room
                    || active.render_room.is_some()
                    || active.surface_cache.ready)
            {
                return true;
            }
        }
        slot += 1;
    }
    false
}

/// Debug mask of every room in the active window.
pub fn active_room_mask<const MAX_ACTIVE_ROOMS: usize>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
) -> RuntimeDebugMask {
    let mut mask = RuntimeDebugMask::EMPTY;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            mask.insert_room(active.index);
        }
        slot += 1;
    }
    mask
}

/// Debug mask of every drawable room in the active window.
pub fn active_room_drawable_mask<const MAX_ACTIVE_ROOMS: usize>(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    current_room: RoomIndex,
) -> RuntimeDebugMask {
    let mut mask = RuntimeDebugMask::EMPTY;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if active.index == current_room
                || active.render_room.is_some()
                || active.surface_cache.ready
            {
                mask.insert_room(active.index);
            }
        }
        slot += 1;
    }
    mask
}

/// Per-frame projected-vertex scratch for the indexed cached-room draw
/// paths (formerly the example's `CACHED_ROOM_PROJECTED_*` statics).
/// One set is enough: rooms are drawn one at a time. The renderer uses
/// the depth array's small prefix as a temporary vertex-seen bitset while
/// collecting indices, then overwrites every depth it will read.
pub struct CachedRoomProjection<const MAX_CACHED_ROOM_VERTICES: usize> {
    /// Projected-vertex slot indices.
    pub indices: [u16; MAX_CACHED_ROOM_VERTICES],
    /// Projected vertices.
    pub vertices: [ProjectedVertex; MAX_CACHED_ROOM_VERTICES],
    /// Per-vertex camera depths. Its first `ceil(vertex_count / 32)` words
    /// are reused as transient deduplication bits before projection.
    pub depths: [i32; MAX_CACHED_ROOM_VERTICES],
}

impl<const MAX_CACHED_ROOM_VERTICES: usize> CachedRoomProjection<MAX_CACHED_ROOM_VERTICES> {
    /// All-zero scratch (link-time `.bss`-safe); every use overwrites
    /// before reading.
    pub const fn zeroed() -> Self {
        Self {
            indices: [0; MAX_CACHED_ROOM_VERTICES],
            vertices: [ProjectedVertex::new(0, 0, 0); MAX_CACHED_ROOM_VERTICES],
            depths: [0; MAX_CACHED_ROOM_VERTICES],
        }
    }
}
