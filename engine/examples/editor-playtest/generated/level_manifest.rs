// Placeholder checked into source control so the editor-playtest
// example can compile before the editor cooks a project. Runtime
// builds overwrite this file with cooked asset records.

use psx_level::{
    EntityRecord, EquipmentRecord, LevelAssetRecord, LevelCachedRoomCellRecord,
    LevelCachedRoomSurfaceRecord, LevelCachedRoomVertexRecord, LevelCameraRecord,
    LevelCharacterRecord, LevelChunkRecord, LevelFarVistaRecord, LevelImagePropRecord,
    LevelMaterialRecord, LevelModelClipBoundsRecord, LevelModelClipRecord,
    LevelModelFrameBoundsRecord, LevelModelInstanceRecord, LevelModelRecord,
    LevelModelSocketRecord, LevelRoomPortalRecord, LevelRoomRecord, LevelRoomSurfaceCacheRecord,
    LevelRoomVisibilityRecord, LevelSkyRecord, LevelVisibilityCellRecord, LevelVisibilityPvsRecord,
    LevelWeaponRecord, LevelWorldPackEntryRecord, PlayerControllerRecord, PlayerSpawnRecord,
    PointLightRecord, RoomIndex, RoomResidencyRecord, WeaponHitboxRecord,
};

pub const WORLD_RESIDENT_CHUNK_LIMIT: usize = 1;
pub const WORLD_PACK_MAX_CHUNK_BYTES: usize = 0;
pub static ASSETS: &[LevelAssetRecord] = &[];
pub static MATERIALS: &[LevelMaterialRecord] = &[];
pub static ROOMS: &[LevelRoomRecord] = &[];
pub static ROOM_CHUNKS: &[LevelChunkRecord] = &[];
pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[];
pub static ROOM_NEAR_ROOMS: &[RoomIndex] = &[];
pub static ROOM_OVERLAPPED_ROOMS: &[RoomIndex] = &[];
pub const WORLD_PACK_START_LBA: u32 = 54;
pub static WORLD_PACK_TOC: &[LevelWorldPackEntryRecord] = &[];
pub static ROOM_VISIBILITY: &[LevelRoomVisibilityRecord] = &[];
pub static VISIBILITY_PVS: &[LevelVisibilityPvsRecord] = &[];
pub static VISIBILITY_PVS_BITS: &[u8] = &[];
pub static VISIBILITY_CELLS: &[LevelVisibilityCellRecord] = &[];
pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[];
pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[];
pub static ROOM_CACHE_CELL_VERTICES: &[u16] = &[];
pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[];
pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[];
pub static ROOM_RESIDENCY: &[RoomResidencyRecord] = &[];

pub static PLAYER_SPAWN: PlayerSpawnRecord = PlayerSpawnRecord {
    room: RoomIndex(0),
    x: 0,
    y: 0,
    z: 0,
    yaw: 0,
    flags: 0,
};

pub static MODEL_CLIPS: &[LevelModelClipRecord] = &[];
pub static MODEL_CLIP_BOUNDS: &[LevelModelClipBoundsRecord] = &[];
pub static MODEL_FRAME_BOUNDS: &[LevelModelFrameBoundsRecord] = &[];
pub static MODEL_SOCKETS: &[LevelModelSocketRecord] = &[];
pub static MODELS: &[LevelModelRecord] = &[];
pub static MODEL_INSTANCES: &[LevelModelInstanceRecord] = &[];
pub static IMAGE_PROPS: &[LevelImagePropRecord] = &[];
pub static WEAPON_HITBOXES: &[WeaponHitboxRecord] = &[];
pub static WEAPONS: &[LevelWeaponRecord] = &[];
pub static EQUIPMENT: &[EquipmentRecord] = &[];
pub static LIGHTS: &[PointLightRecord] = &[];
pub static CHARACTERS: &[LevelCharacterRecord] = &[];
pub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = None;
pub static ENTITIES: &[EntityRecord] = &[];
