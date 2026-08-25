// Placeholder checked into source control so the editor-playtest
// example can compile before the editor cooks a project. Runtime
// builds overwrite this file with cooked asset records.

use psx_level::{
    AssetId, CombatCapsuleRecord, EntityRecord, EquipmentRecord, FlowState, GameFlow,
    InteractableMessageRecord, InteractableRecord, LevelArchPropCollisionRecord,
    LevelArchPropRecord, LevelArchPropSurfaceRecord, LevelAssetRecord, LevelBoxPropRecord,
    LevelBoxPropSurfaceRecord, LevelCachedRoomCellRecord, LevelCachedRoomSurfaceRecord,
    LevelCachedRoomVertexRecord, LevelCameraRecord, LevelCharacterRecord, LevelChunkRecord,
    LevelCylinderPropRecord, LevelCylinderPropSurfaceRecord, LevelFarVistaRecord,
    LevelGameEntityRecord, LevelImagePropRecord, LevelLogicRecord, LevelMaterialRecord,
    LevelModelClipBoundsRecord, LevelModelClipRecord, LevelModelFrameBoundsRecord,
    LevelModelInstanceRecord, LevelModelRecord, LevelModelSocketRecord, LevelOptionDef,
    LevelRoomPortalRecord, LevelRoomRecord, LevelRoomSurfaceCacheRecord, LevelRoomVisibilityRecord,
    LevelSceneState, LevelSkyRecord, LevelUiNodeRecord, LevelUiPaintRecord, LevelUiScene,
    LevelUiSfxCueRecord, LevelUiSfxSampleRecord, LevelVisibilityCellRecord,
    LevelVisibilityPvsRecord, LevelWaterCellRecord, LevelWeaponRecord, LevelWorldPackEntryRecord,
    ParticleEmitterRecord, PlayerControllerRecord, PlayerSpawnRecord, PointLightRecord, RoomIndex,
    RoomResidencyRecord, WeaponAppearanceRecord, WeaponHitboxRecord,
};

pub const WORLD_RESIDENT_CHUNK_LIMIT: usize = 1;
pub const WORLD_PACK_MAX_CHUNK_BYTES: usize = 0;
pub const WORLD_STREAM_SLOT_COUNT: usize = 1;
pub const WORLD_RESIDENT_PAGE_COUNT: usize = 1;
pub const PERSISTENT_ASSET_SLOT_COUNT: usize = 1;
pub const UI_PACK_MAX_CHUNK_BYTES: usize = 0;
pub const UI_PACK_IMAGE_CACHE_SLOTS: usize = 1;
pub const GAMEPLAY_PACK_MAX_CHUNK_BYTES: usize = 0;
pub const UI_PACK_START_LBA: u32 = 1024;
pub static UI_PACK_TOC: &[LevelWorldPackEntryRecord] = &[];

pub const BOX_PROP_STATE_COUNT: usize = 1;
pub const PERSISTENT_ASSET_PAGE_COUNT: usize = 1;
pub const CACHED_ROOM_DEPTH_MODE: u8 = 2;
pub const CACHED_ROOM_TEXTURE_SPLIT_MODE: u8 = 0;
pub const CACHED_ROOM_DRAW_ORDER_MODE: u8 = 0;
pub const CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;
pub const PLAYTEST_USES_PXBSP: bool = false;
pub const PXBSP_AMBIENT_RGB: [u8; 3] = [0; 3];
pub const PXBSP_FACE_CHAIN_CAPACITY: usize = 0;
pub const PLAYTEST_PACKET_CAPACITY: usize = 1536;
pub static PXBSP_WORLD: &[u8] = &[];
pub static PXBSP_MOVER_NODE_IDS: &[u32] = &[];
pub static PXBSP_MOVER_MODEL_INDICES: &[u16] = &[];
pub static PXBSP_BODY_HULLS: &[psx_bsp::collision_provider::CookedBodyHull] = &[];
pub static ASSETS: &[LevelAssetRecord] = &[];
pub static MATERIALS: &[LevelMaterialRecord] = &[];
pub static ROOMS: &[LevelRoomRecord] = &[];
pub static ROOM_CHUNKS: &[LevelChunkRecord] = &[];
pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[];
pub static WATER_CELLS: &[LevelWaterCellRecord] = &[];
pub static ROOM_NEAR_ROOMS: &[RoomIndex] = &[];
pub static ROOM_OVERLAPPED_ROOMS: &[RoomIndex] = &[];
pub const WORLD_PACK_START_LBA: u32 = 1024;
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
pub static ROOM_REFLECTION_PROBES: &[Option<AssetId>] = &[];

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
pub static BOX_PROPS: &[LevelBoxPropRecord] = &[];
pub static BOX_PROP_SURFACES: &[LevelBoxPropSurfaceRecord] = &[];
pub static CYLINDER_PROPS: &[LevelCylinderPropRecord] = &[];
pub static CYLINDER_PROP_SURFACES: &[LevelCylinderPropSurfaceRecord] = &[];
pub static ARCH_PROPS: &[LevelArchPropRecord] = &[];
pub static ARCH_PROP_SURFACES: &[LevelArchPropSurfaceRecord] = &[];
pub static ARCH_PROP_COLLISIONS: &[LevelArchPropCollisionRecord] = &[];
pub static UI_FONTS: &[&psx_font::BitmapFont] = &[&psx_font::fonts::BASIC];
pub static UI_PAINTS: &[LevelUiPaintRecord] = &[];
pub static UI_NODES: &[LevelUiNodeRecord] = &[];
pub static UI_SFX_SAMPLES: &[LevelUiSfxSampleRecord] = &[];
pub static UI_SFX_CUES: &[LevelUiSfxCueRecord] = &[];
pub static UI_SCENES: &[LevelUiScene] = &[];
pub static SCENE_STATES: &[LevelSceneState] = &[];
pub static GAME_FLOW: GameFlow = GameFlow {
    states: &[FlowState::Gameplay],
    scene_states: SCENE_STATES,
    entry: 0,
};
pub static OPTIONS: &[LevelOptionDef] = &[];
pub static WEAPON_HITBOXES: &[WeaponHitboxRecord] = &[];
pub static WEAPONS: &[LevelWeaponRecord] = &[];
pub static EQUIPMENT: &[EquipmentRecord] = &[];
pub static WEAPON_APPEARANCES: &[WeaponAppearanceRecord] = &[];
pub static LIGHTS: &[PointLightRecord] = &[];
pub static PARTICLE_EMITTERS: &[ParticleEmitterRecord] = &[];
pub static INTERACTABLE_MESSAGES: &[InteractableMessageRecord] = &[];
pub static INTERACTABLES: &[InteractableRecord] = &[];
pub static LOGIC: &[LevelLogicRecord] = &[];
pub static GAME_ENTITIES: &[LevelGameEntityRecord] = &[];
pub static CHARACTERS: &[LevelCharacterRecord] = &[];
pub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = None;
pub static ENTITIES: &[EntityRecord] = &[];
pub static COMBAT_CAPSULES: &[CombatCapsuleRecord] = &[];
pub const LOADING_UI_SCENE: u16 = psx_level::UI_SCENE_NONE;

macro_rules! draw_project_cached_room {
    (
        $lighting:expr,
        $draw:path,
        [$($before:expr),* $(,)?],
        [$($after:expr),* $(,)?]
    ) => {
        $draw($($before,)* $lighting, false, $($after,)*)
    };
}
pub(crate) use draw_project_cached_room;
