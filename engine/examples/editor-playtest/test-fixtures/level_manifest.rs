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
/// Per room, the sector of its baked sparse cube sky relative to the start of
/// WORLD.PAK (0 for none), and the sectors it occupies.
pub const GRID_ROOM_SKY_SECTORS: [u32; GRID_ROOM_COUNT] = [];
pub const GRID_ROOM_SKY_COUNT: [u32; GRID_ROOM_COUNT] = [];
/// The index of every texel of the sky no ray hit (`psx_level::sky_tiles::Background`):
/// where each row of each face starts in the runs, and the runs. Empty: the
/// world streams no chunk skies.
pub const GRID_SKY_BG_ROWS: &[u16] = &[];
pub const GRID_SKY_BG_RUNS: &[u8] = &[];
/// The tiles of the sky VRAM starts with (room 0's) that differ from the background.
pub const GRID_SKY_INITIAL_MASK: &[u32] = &[];
/// Far cards (`psx_grid::render::cards`): per room `[x, z, base, height, half width, 0]`
/// in world runtime units (a zero height: no card), and the sector of its pictures
/// relative to the start of WORLD.PAK (0 for none). Empty: the world has no cards.
pub const GRID_CARDS: &[[i16; 6]] = &[];
pub const GRID_ROOM_CARD_SECTORS: &[u32] = &[];
/// Sectors from a room's picture of one view to its picture of the next.
pub const GRID_CARD_VIEW_STRIDE: u32 = 0;
/// Cards show out to this many runtime units from the camera, fading in over the
/// last `GRID_CARD_FADE`; the colours behind them (below the horizon, at it, at the
/// top of the screen).
pub const GRID_CARD_FAR: i32 = 0;
pub const GRID_CARD_FADE: i32 = 0;
/// No card is drawn closer than this (the coarse band stands there).
pub const GRID_CARD_NEAR: i32 = 0;
pub const GRID_CARD_BACKDROP: [[u8; 3]; 3] = [[0, 0, 0], [0, 0, 0], [0, 0, 0]];
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
