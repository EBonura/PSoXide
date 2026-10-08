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
    LevelFarVistaRecord, LevelGameEntityRecord, LevelGameplaySfxCueRecord, LevelImagePropRecord,
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
pub const UI_PACK_START_LBA: u32 = 1024;
pub static UI_PACK_TOC: &[LevelWorldPackEntryRecord] = &[];

pub const BOX_PROP_STATE_COUNT: usize = 1;
pub const MODEL_PROJECTED_VERTEX_CAPACITY: usize = 1;
pub const MODEL_FACE_CAPACITY: usize = 1;
pub const MODEL_PART_CAPACITY: usize = 1;
pub const MODEL_DECODED_VERTEX_CAPACITY: usize = 1;
pub static INTERACTABLE_MESSAGE_PAGES_IT: &[&str] = &[];
pub static BOOST_MODULES_IT: &[(&str, &str)] = &[];
pub const PERSISTENT_ASSET_PAGE_COUNT: usize = 1;
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
pub static ROOMS: &[LevelRoomRecord] = &[LevelRoomRecord {
    name: "Fixture",
    sector_size: 64,
    draw_distance: 4096,
    gravity_per_tick_q8: 32,
    fog_rgb: [0; 3],
    fog_near: 0,
    fog_far: 4096,
    atmosphere_rgb: [0; 3],
    atmosphere_density: 0,
    atmosphere_fall_speed_q4: 0,
    atmosphere_wind_speed_q4: 0,
    sky: LevelSkyRecord::DEFAULT,
    far_vista: LevelFarVistaRecord::DEFAULT,
    camera: LevelCameraRecord::DEFAULT,
    flags: 0,
}];
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
pub static DESTRUCTIBLES: &[LevelDestructibleRecord] = &[];
pub static WORLD_OBJECTS: &[LevelWorldObjectRecord] = &[];
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
pub static GAMEPLAY_SFX_CUES: &[LevelGameplaySfxCueRecord] = &[];
pub static VITALITY_CIRCLES: &[LevelVitalityCircleRecord] = &[];
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
pub static LIGHTS: &[PointLightRecord] = &[];
pub static PARTICLE_EMITTERS: &[ParticleEmitterRecord] = &[];
pub static INTERACTABLE_MESSAGES: &[InteractableMessageRecord] = &[];
pub static INTERACTABLE_MESSAGE_PAGES: &[&str] = &["First welcome page.", "Second welcome page."];
pub static INTERACTABLES: &[InteractableRecord] = &[];
pub static BOOST_MODULES: &[BoostModuleRecord] = &[];
pub static LOGIC: &[LevelLogicRecord] = &[];
pub static GAME_ENTITIES: &[LevelGameEntityRecord] = &[LevelGameEntityRecord {
    room: RoomIndex(0),
    kind: 1,
    targetname: 0,
    model_instance: 0,
    idle_clip: 0,
    alert_clip: 0,
    turn_clip: 0,
    walk_clip: 0,
    walk_backward_clip: 0,
    strafe_left_clip: 0,
    strafe_right_clip: 0,
    run_clip: 0,
    attack_clip: 0,
    attack_speed_q8: 256,
    attack_frame_range: CharacterActionFrameRange::FULL,
    heavy_attack_clip: 0,
    heavy_attack_speed_q8: 256,
    heavy_attack_frame_range: CharacterActionFrameRange::FULL,
    ranged_attack_clip: 0,
    ranged_attack_speed_q8: 256,
    ranged_attack_frame_range: CharacterActionFrameRange::FULL,
    stagger_clip: 0,
    stagger_speed_q8: 256,
    stagger_frame_range: CharacterActionFrameRange::FULL,
    stagger_ticks: 85,
    death_clip: 0,
    combat_capsule_first: CombatCapsuleIndex(0),
    combat_capsule_count: 0,
    ranged_attack_action: 0,
    x: 0,
    y: 0,
    z: 0,
    yaw: 0,
    radius: 14,
    height: 77,
    walk_speed: 2,
    run_speed: 6,
    patrol_x: 0,
    patrol_y: 0,
    patrol_z: 0,
    patrol_wait_ticks: 60,
    aggro_radius: 400,
    reaction_ticks: 120,
    preferred_distance: 64,
    spacing_tolerance: 12,
    spacing_speed_percent: 25,
    decision_interval_ticks: 12,
    circle_chance: 65,
    attack_priority: 4,
    attack_cooldown_ticks: 45,
    group_attack_delay_ticks: 18,
    windup_ticks: 16,
    attack_active_ticks: 56,
    heavy_attack_active_ticks: 92,
    ranged_attack_active_ticks: 148,
    recovery_ticks: 24,
    attack_min_range: 32,
    attack_max_range: 256,
    poise: 25,
    touch_damage: 10,
    max_health: 300,
    max_health_secondary: 300,
    soul_value: 50,
    flags: 0,
}];
pub static CHARACTERS: &[LevelCharacterRecord] = &[];
pub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = None;
pub static ENTITIES: &[EntityRecord] = &[];
pub static COMBAT_CAPSULES: &[CombatCapsuleRecord] = &[];
pub static WORLD_MESSAGE: Option<InteractableMessageRecord> = Some(InteractableMessageRecord {
    title: "Welcome",
    body: "First welcome page.",
    page_first: 0,
    page_count: 2,
});
pub const PERSISTENT_FLAG_COUNT: u16 = 1;
pub const PROJECT_SAVE_NAME: &str = "BESLES-PSOXIDE";
pub const PROJECT_SAVE_TITLE: &str = "PSOXIDE PLAYTEST";
pub const LOADING_UI_SCENE: u16 = psx_level::UI_SCENE_NONE;
