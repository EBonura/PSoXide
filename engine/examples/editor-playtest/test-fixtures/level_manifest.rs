// Hermetic manifest used by `cargo test` in place of whatever project was last
// cooked into `generated/`. It is `generated/level_manifest.rs` (the checked-in
// placeholder) plus the minimum content the host tests assert on: one room, one
// enemy and a two-page welcome message. Keep the two files in step when
// `psx-level` records change; a stale copy fails to compile under `cargo test`.

use psx_level::{
    AssetId, BoostModuleRecord, CombatCapsuleRecord, EntityRecord, EquipmentRecord, FlowState,
    GameFlow, InteractableMessageRecord, InteractableRecord, LevelArchPropCollisionRecord,
    LevelArchPropRecord, LevelArchPropSurfaceRecord, LevelAssetRecord, LevelBoxPropRecord,
    LevelBoxPropSurfaceRecord, LevelCameraRecord, LevelCharacterRecord, LevelCylinderPropRecord, LevelCylinderPropSurfaceRecord, LevelDestructibleRecord,
    LevelGameEntityRecord, LevelGameplaySfxCueRecord, LevelImagePropRecord,
    LevelLogicRecord, LevelModelClipBoundsRecord, LevelModelClipRecord,
    LevelModelFrameBoundsRecord, LevelModelInstanceRecord, LevelModelRecord,
    LevelModelSocketRecord, LevelOptionDef, LevelRoomRecord,
    LevelSceneState, LevelSkyRecord, CharacterActionFrameRange, CombatCapsuleIndex,
    LevelTransition, LevelUiNodeRecord, LevelUiPaintRecord, LevelUiScene, LevelUiSfxCueRecord,
    LevelUiSfxSampleRecord, LevelVitalityCircleRecord, LevelWeaponRecord, LevelWorldObjectRecord,
    LevelWorldPackEntryRecord, ParticleEmitterRecord, PlayerControllerRecord, PlayerSpawnRecord,
    PointLightRecord, RoomIndex, WeaponAppearanceRecord, WeaponHitboxRecord,
};

pub const PERSISTENT_ASSET_SLOT_COUNT: usize = 1;
pub const UI_PACK_MAX_CHUNK_BYTES: usize = 0;
pub const UI_PACK_IMAGE_CACHE_SLOTS: usize = 1;
pub const UI_SFX_PACK_FIRST_CHUNK: u32 = 0;
pub const UI_SFX_MAX_SAMPLE_BYTES: usize = 0;
pub const GAMEPLAY_PACK_MAX_CHUNK_BYTES: usize = 0;
pub const WORLD_PACK_START_LBA: u32 = 1024;
pub const GRID_ROOM_COUNT: usize = 0;
pub const GRID_ROOM_POOL_PAGES: usize = 1;
pub const LEN_ROOMS: usize = 1;
pub const LEN_ROOM_REFLECTION_PROBES: usize = 0;
pub const LEN_LIGHTS: usize = 0;
pub const LEN_PARTICLE_EMITTERS: usize = 0;
pub const LEN_INTERACTABLE_MESSAGES: usize = 0;
pub const LEN_INTERACTABLE_MESSAGE_PAGES: usize = 2;
pub const LEN_INTERACTABLE_MESSAGE_PAGES_IT: usize = 0;
pub const LEN_INTERACTABLES: usize = 0;
pub const LEN_VITALITY_CIRCLES: usize = 0;
pub const LEN_LOGIC: usize = 0;
pub const LEN_LOGIC_IDS: usize = 0;
pub const LEN_GAME_ENTITIES: usize = 1;
pub const LEN_GAME_ENTITY_IDS: usize = 1;
pub const LEN_ENTITIES: usize = 0;
pub const LEN_DESTRUCTIBLES: usize = 0;
pub const LEN_WORLD_OBJECTS: usize = 0;
pub const LEN_IMAGE_PROPS: usize = 0;
pub const LEN_BOX_PROPS: usize = 0;
pub const LEN_BOX_PROP_SURFACES: usize = 0;
pub const LEN_CYLINDER_PROPS: usize = 0;
pub const LEN_CYLINDER_PROP_SURFACES: usize = 0;
pub const LEN_ARCH_PROPS: usize = 0;
pub const LEN_ARCH_PROP_SURFACES: usize = 0;
pub const LEN_ARCH_PROP_COLLISIONS: usize = 0;
pub const LEN_MODEL_INSTANCES: usize = 0;
pub const UI_PACK_START_LBA: u32 = 1024;
pub static UI_PACK_TOC: &[LevelWorldPackEntryRecord] = &[];

pub const MODEL_PROJECTED_VERTEX_CAPACITY: usize = 1;
pub const MODEL_FACE_CAPACITY: usize = 1;
pub const MODEL_PART_CAPACITY: usize = 1;
pub const MODEL_DECODED_VERTEX_CAPACITY: usize = 1;
pub static BOOST_MODULES_IT: &[(&str, &str)] = &[];
pub const PERSISTENT_ASSET_PAGE_COUNT: usize = 1;
pub const CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;
pub const PLAYTEST_HAS_WORLD: bool = false;
pub const PLAYTEST_PACKET_CAPACITY: usize = 1536;
pub static ASSETS: &[LevelAssetRecord] = &[];


pub static MODEL_CLIPS: &[LevelModelClipRecord] = &[];
pub static MODEL_CLIP_BOUNDS: &[LevelModelClipBoundsRecord] = &[];
pub static MODEL_FRAME_BOUNDS: &[LevelModelFrameBoundsRecord] = &[];
pub static MODEL_SOCKETS: &[LevelModelSocketRecord] = &[];
pub static MODELS: &[LevelModelRecord] = &[];
pub static UI_FONTS: &[&psx_font::BitmapFont] = &[&psx_font::fonts::BASIC];
pub static UI_PAINTS: &[LevelUiPaintRecord] = &[];
pub static UI_NODES: &[LevelUiNodeRecord] = &[];
pub static UI_SFX_SAMPLES: &[LevelUiSfxSampleRecord] = &[];
pub static UI_SFX_CUES: &[LevelUiSfxCueRecord] = &[];
pub static GAMEPLAY_SFX_CUES: &[LevelGameplaySfxCueRecord] = &[];
pub static MUSIC_TRACKS: &[psx_level::LevelMusicTrack] = &[];
pub static UI_SCENES: &[LevelUiScene] = &[];
pub static SCENE_STATES: &[LevelSceneState] = &[];
pub static GAME_FLOW: GameFlow = GameFlow {
    states: &[FlowState::Gameplay],
    scene_states: SCENE_STATES,
    entry: 0,
    transition: LevelTransition::NONE,
};
pub static OPTIONS: &[LevelOptionDef] = &[];
pub static WEAPON_HITBOXES: &[WeaponHitboxRecord] = &[];
pub static WEAPONS: &[LevelWeaponRecord] = &[];
pub static EQUIPMENT: &[EquipmentRecord] = &[];
pub static WEAPON_APPEARANCES: &[WeaponAppearanceRecord] = &[];
pub static BOOST_MODULES: &[BoostModuleRecord] = &[];
pub static CHARACTERS: &[LevelCharacterRecord] = &[];
pub static COMBAT_CAPSULES: &[CombatCapsuleRecord] = &[];
pub const PERSISTENT_FLAG_COUNT: u16 = 1;
pub const PROJECT_SAVE_NAME: &str = "BESLES-PSOXIDE";
pub const PROJECT_SAVE_TITLE: &str = "PSOXIDE PLAYTEST";
pub const LOADING_UI_SCENE: u16 = psx_level::UI_SCENE_NONE;
