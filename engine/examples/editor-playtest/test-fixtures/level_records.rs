// The records a level owns, for the host tests of the guest. The runtime reads
// them off the disc; the tests install these in their place (see `level_data`).

pub static INTERACTABLE_MESSAGE_PAGES_IT: &[&str] = &[];
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
pub static VITALITY_CIRCLES: &[LevelVitalityCircleRecord] = &[];
pub static LIGHTS: &[PointLightRecord] = &[];
pub static PARTICLE_EMITTERS: &[ParticleEmitterRecord] = &[];
pub static INTERACTABLE_MESSAGES: &[InteractableMessageRecord] = &[];
pub static INTERACTABLE_MESSAGE_PAGES: &[&str] = &["First welcome page.", "Second welcome page."];
pub static INTERACTABLES: &[InteractableRecord] = &[];
pub static LOGIC: &[LevelLogicRecord] = &[];
pub static LOGIC_IDS: &[psx_level::StableId] = &[];
pub static GAME_ENTITY_IDS: &[psx_level::StableId] = &[psx_level::StableId(0x5eed_0001)];
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
pub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = None;
pub static ENTITIES: &[EntityRecord] = &[];
pub static WORLD_MESSAGE: Option<InteractableMessageRecord> = Some(InteractableMessageRecord {
    title: "Welcome",
    body: "First welcome page.",
    page_first: 0,
    page_count: 2,
});
