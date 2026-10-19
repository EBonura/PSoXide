//! The records one level owns, built from the cooked package as typed values
//! and written as one chunk of `WORLD.PAK` (see `psx_level::wire`).
//!
//! These used to be Rust source in the generated manifest and so lived in the
//! executable. Building them as values rather than text means the compiler
//! checks every field of every record, and the chunk is the only copy.

use psx_level::wire::{caps, LevelRecords};
use psx_level::{
    AssetId, CharacterIndex, CombatCapsuleIndex, EntityKind, EntityRecord, InteractableKind,
    InteractableMessageRecord, InteractableRecord, LevelArchPropCollisionRecord,
    LevelArchPropRecord, LevelArchPropSurfaceRecord, LevelBoxPropRecord, LevelBoxPropSurfaceRecord,
    LevelCameraProfile, LevelCameraRecord, LevelCloudLayerRecord, LevelCycloramaQuadRecord,
    LevelCylinderPropRecord, LevelCylinderPropSurfaceRecord, LevelDestructibleRecord,
    LevelFarVistaRecord, LevelGameEntityRecord, LevelImagePropRecord, LevelLogicRecord,
    LevelMaterialUvMotion, LevelModelInstanceRecord, LevelModelMaterialOverride,
    LevelModelSecondaryLayer, LevelRoomRecord, LevelSkyRecord, LevelVitalityCircleRecord,
    LevelWorldObjectRecord, ModelIndex, OptionalModelClipIndex, ParticleEmitterRecord,
    PlayerControllerRecord, PlayerSpawnRecord, PointLightRecord, ResourceSlot, RoomIndex, StableId,
};

use super::manifest::{model_material_flags, model_override_blend_code, reflection_material_flags};
use super::*;

/// Every table of a level's records, owned. [`Self::records`] borrows them in
/// the shape the chunk writer takes.
pub struct OwnedLevelRecords {
    rooms: Vec<LevelRoomRecord>,
    room_reflection_probes: Vec<Option<AssetId>>,
    lights: Vec<PointLightRecord>,
    particle_emitters: Vec<ParticleEmitterRecord>,
    interactable_messages: Vec<InteractableMessageRecord>,
    interactable_message_pages: Vec<&'static str>,
    interactable_message_pages_it: Vec<&'static str>,
    interactables: Vec<InteractableRecord>,
    vitality_circles: Vec<LevelVitalityCircleRecord>,
    logic: Vec<LevelLogicRecord>,
    logic_ids: Vec<StableId>,
    game_entities: Vec<LevelGameEntityRecord>,
    game_entity_ids: Vec<StableId>,
    entities: Vec<EntityRecord>,
    destructibles: Vec<LevelDestructibleRecord>,
    world_objects: Vec<LevelWorldObjectRecord>,
    image_props: Vec<LevelImagePropRecord>,
    box_props: Vec<LevelBoxPropRecord>,
    box_prop_surfaces: Vec<LevelBoxPropSurfaceRecord>,
    cylinder_props: Vec<LevelCylinderPropRecord>,
    cylinder_prop_surfaces: Vec<LevelCylinderPropSurfaceRecord>,
    arch_props: Vec<LevelArchPropRecord>,
    arch_prop_surfaces: Vec<LevelArchPropSurfaceRecord>,
    arch_prop_collisions: Vec<LevelArchPropCollisionRecord>,
    model_instances: Vec<LevelModelInstanceRecord>,
    player_spawn: PlayerSpawnRecord,
    player_controller: Option<PlayerControllerRecord>,
    world_message: Option<InteractableMessageRecord>,
}

/// A string that lives for the rest of the process. The cook builds a handful
/// of names per run; the record types borrow them as `&'static str`.
fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn asset(index: usize) -> AssetId {
    AssetId(u16::try_from(index).unwrap_or(u16::MAX))
}

fn asset_or_none(index: Option<usize>) -> AssetId {
    index.map_or(AssetId(u16::MAX), asset)
}

fn optional_asset(index: Option<usize>) -> Option<AssetId> {
    index.map(asset)
}

fn assets_of<const N: usize>(indices: &[Option<usize>; N]) -> [Option<AssetId>; N] {
    indices.map(optional_asset)
}

fn message_record(message: &PlaytestInteractableMessage) -> InteractableMessageRecord {
    InteractableMessageRecord {
        title: leak(&message.title),
        body: leak(&message.body),
        page_first: message.page_first,
        page_count: message.page_count,
    }
}

fn uv_motion(motion: &crate::MaterialUvMotion) -> LevelMaterialUvMotion {
    LevelMaterialUvMotion {
        enabled: motion.enabled,
        speed_u_q8: motion.speed_u_q8,
        speed_v_q8: motion.speed_v_q8,
        phase_u: motion.phase_u,
        phase_v: motion.phase_v,
    }
}

fn material_override(
    material: &Option<PlaytestModelMaterialOverride>,
) -> Option<LevelModelMaterialOverride> {
    let o = material.as_ref()?;
    Some(LevelModelMaterialOverride {
        texture_asset: optional_asset(o.texture_asset_index),
        blend_mode: model_override_blend_code(o.blend_mode),
        tint_rgb: o.tint_rgb,
        motion: uv_motion(&o.motion),
        secondary_layer: o.secondary_layer.map(|layer| LevelModelSecondaryLayer {
            texture_asset: optional_asset(layer.texture_asset_index),
            blend_mode: model_override_blend_code(layer.blend_mode),
            tint_rgb: layer.tint_rgb,
            motion: uv_motion(&layer.motion),
            flags: reflection_material_flags(layer.reflection_probe),
        }),
        flags: model_material_flags(o),
    })
}

impl OwnedLevelRecords {
    /// The level's records from the cooked package.
    pub fn build(package: &PlaytestPackage) -> Self {
        let rooms = package
            .rooms
            .iter()
            .map(|room| {
                let quads: Vec<LevelCycloramaQuadRecord> = room
                    .sky
                    .cyclorama_quads
                    .iter()
                    .map(|quad| LevelCycloramaQuadRecord {
                        direction_q12: quad.direction_q12,
                        rgb: quad.rgb,
                        flags: 0,
                    })
                    .collect();
                let vista: Vec<AssetId> = room
                    .far_vista
                    .texture_asset_indices
                    .iter()
                    .map(|index| asset_or_none(*index))
                    .collect();
                LevelRoomRecord {
                    name: leak(&room.name),
                    sector_size: room.sector_size,
                    draw_distance: room.draw_distance,
                    gravity_per_tick_q8: room.gravity_per_tick_q8,
                    fog_rgb: room.fog_rgb,
                    fog_near: room.fog_near,
                    fog_far: room.fog_far,
                    atmosphere_rgb: room.atmosphere_rgb,
                    atmosphere_density: room.atmosphere_density,
                    atmosphere_fall_speed_q4: room.atmosphere_fall_speed_q4,
                    atmosphere_wind_speed_q4: room.atmosphere_wind_speed_q4,
                    sky: LevelSkyRecord {
                        top_rgb: room.sky.top_rgb,
                        horizon_rgb: room.sky.horizon_rgb,
                        bottom_rgb: room.sky.bottom_rgb,
                        horizon_percent: room.sky.horizon_percent,
                        horizon_thickness_percent: room.sky.horizon_thickness_percent,
                        skybox_columns: room.sky.skybox_columns,
                        skybox_rows: room.sky.skybox_rows,
                        flags: room.sky.flags,
                        texture_asset: asset_or_none(room.sky.texture_asset_index),
                        cyclorama_quads: Box::leak(quads.into_boxed_slice()),
                        cloud_layer: LevelCloudLayerRecord {
                            texture_asset: asset_or_none(room.sky.cloud_layer.texture_asset_index),
                            color_rgb: room.sky.cloud_layer.color_rgb,
                            density: room.sky.cloud_layer.density,
                            altitude: room.sky.cloud_layer.altitude,
                            extent: room.sky.cloud_layer.extent,
                            tile_count: room.sky.cloud_layer.tile_count,
                            scroll_speed: room.sky.cloud_layer.scroll_speed,
                            noise_seed: room.sky.cloud_layer.noise_seed,
                            flags: room.sky.cloud_layer.flags,
                        },
                    },
                    far_vista: LevelFarVistaRecord {
                        texture_assets: Box::leak(vista.into_boxed_slice()),
                        radius: room.far_vista.radius,
                        height: room.far_vista.height,
                        vertical_offset: room.far_vista.vertical_offset,
                        segments: room.far_vista.segments,
                        rotation_degrees: room.far_vista.rotation_degrees,
                        tint_rgb: room.far_vista.tint_rgb,
                        flags: room.far_vista.flags,
                    },
                    camera: LevelCameraRecord {
                        distance: room.camera.distance,
                        height: room.camera.height,
                        target_height: room.camera.target_height,
                        lock_rise_percent: room.camera.lock_rise_percent,
                        min_floor_clearance: room.camera.min_floor_clearance,
                        orbit_speed_level: room.camera.orbit_speed_level,
                        accelerated_orbit: room.camera.accelerated_orbit,
                        recenter_preserves_pitch: room.camera.recenter_preserves_pitch,
                        fov_y_degrees: room.camera.fov_y_degrees,
                        blend_profiles: room.camera.blend_profiles,
                        lock_target_framing: room.camera.lock_target_framing,
                        lock_profile: room.camera.lock_profile.map(|p| LevelCameraProfile {
                            distance: p.distance,
                            height: p.height,
                            target_height: p.target_height,
                            shoulder_offset: p.shoulder_offset,
                            fov_y_degrees: p.fov_y_degrees,
                        }),
                        position_lag_shift: room.camera.position_lag_shift,
                        position_vertical_lag_shift: room.camera.position_vertical_lag_shift,
                        focus_lag_shift: room.camera.focus_lag_shift,
                        focus_vertical_lag_shift: room.camera.focus_vertical_lag_shift,
                        distance_lag_shift: room.camera.distance_lag_shift,
                    },
                    flags: room.flags,
                }
            })
            .collect();
        let spawn = package.spawn.unwrap_or(PlaytestSpawn {
            room: 0,
            x: 0,
            y: 0,
            z: 0,
            yaw: 0,
            flags: 0,
        });
        let spawn_record = |spawn: PlaytestSpawn| PlayerSpawnRecord {
            room: RoomIndex(spawn.room),
            x: spawn.x,
            y: spawn.y,
            z: spawn.z,
            yaw: spawn.yaw,
            flags: spawn.flags,
        };
        Self {
            rooms,
            room_reflection_probes: package
                .rooms
                .iter()
                .map(|room| optional_asset(room.reflection_probe_asset_index))
                .collect(),
            lights: package
                .lights
                .iter()
                .map(|light| PointLightRecord {
                    room: RoomIndex(light.room),
                    x: light.x,
                    y: light.y,
                    z: light.z,
                    radius: light.radius,
                    intensity_q8: light.intensity_q8,
                    color: light.color,
                    flags: 0,
                })
                .collect(),
            particle_emitters: package
                .particle_emitters
                .iter()
                .map(|e| ParticleEmitterRecord {
                    room: RoomIndex(e.room),
                    x: e.x,
                    y: e.y,
                    z: e.z,
                    max_particles: e.max_particles,
                    spawn_rate_q8: e.spawn_rate_q8,
                    lifetime_frames: e.lifetime_frames,
                    start_size: e.start_size,
                    end_size: e.end_size,
                    start_color: e.start_color,
                    end_color: e.end_color,
                    blend_mode: e.blend_mode,
                    base_velocity_q4: e.base_velocity_q4,
                    random_velocity_q4: e.random_velocity_q4,
                    acceleration_q4: e.acceleration_q4,
                    spawn_radius: e.spawn_radius,
                    flags: e.flags,
                })
                .collect(),
            interactable_messages: package
                .interactable_messages
                .iter()
                .map(message_record)
                .collect(),
            interactable_message_pages: package
                .interactable_message_pages
                .iter()
                .map(|page| leak(page))
                .collect(),
            interactable_message_pages_it: package
                .interactable_message_pages_it
                .iter()
                .map(|page| leak(page))
                .collect(),
            interactables: package
                .interactables
                .iter()
                .map(|i| InteractableRecord {
                    room: RoomIndex(i.room),
                    kind: match i.kind {
                        PlaytestInteractableKind::Message => InteractableKind::Message,
                        PlaytestInteractableKind::Checkpoint => InteractableKind::Checkpoint,
                        PlaytestInteractableKind::PointOfInterest => {
                            InteractableKind::PointOfInterest
                        }
                    },
                    x: i.x,
                    y: i.y,
                    z: i.z,
                    yaw: i.yaw,
                    radius: i.radius,
                    marker_height: i.marker_height,
                    prompt: leak(&i.prompt),
                    message: i.message,
                    logic: i.logic,
                    checkpoint_id: leak(&i.checkpoint_id),
                    read_flag: i.read_flag,
                    reward_flag: i.reward_flag,
                    reward_resource: i.reward_resource,
                    reward_quantity: i.reward_quantity,
                    flags: i.flags,
                })
                .collect(),
            vitality_circles: package
                .vitality_circles
                .iter()
                .map(|c| LevelVitalityCircleRecord {
                    room: RoomIndex(c.room),
                    x: c.x,
                    y: c.y,
                    z: c.z,
                    radius: c.radius,
                    axis: c.axis,
                    refill_per_second: c.refill_per_second,
                    drain_per_second: c.drain_per_second,
                })
                .collect(),
            logic: package
                .logic
                .iter()
                .map(|l| LevelLogicRecord {
                    room: RoomIndex(l.room),
                    kind: l.kind,
                    spawnflags: l.spawnflags,
                    targetname: l.targetname,
                    target: l.target,
                    killtarget: l.killtarget,
                    master: l.master,
                    delay_ticks: l.delay_ticks,
                    wait_ticks: l.wait_ticks,
                    arg0: l.arg0,
                    arg1: l.arg1,
                    link: l.link,
                    message: l.message,
                    x: l.x,
                    y: l.y,
                    z: l.z,
                    min: l.min,
                    max: l.max,
                    flags: l.flags,
                })
                .collect(),
            logic_ids: package.logic_ids.iter().map(|id| StableId(*id)).collect(),
            game_entities: package
                .game_entities
                .iter()
                .map(|e| LevelGameEntityRecord {
                    room: RoomIndex(e.room),
                    kind: e.kind,
                    targetname: e.targetname,
                    model_instance: e.model_instance,
                    idle_clip: e.idle_clip,
                    alert_clip: e.alert_clip,
                    turn_clip: e.turn_clip,
                    walk_clip: e.walk_clip,
                    walk_backward_clip: e.walk_backward_clip,
                    strafe_left_clip: e.strafe_left_clip,
                    strafe_right_clip: e.strafe_right_clip,
                    run_clip: e.run_clip,
                    attack_clip: e.attack_clip,
                    attack_speed_q8: e.attack_speed_q8,
                    attack_frame_range: e.attack_frame_range,
                    heavy_attack_clip: e.heavy_attack_clip,
                    heavy_attack_speed_q8: e.heavy_attack_speed_q8,
                    heavy_attack_frame_range: e.heavy_attack_frame_range,
                    ranged_attack_clip: e.ranged_attack_clip,
                    ranged_attack_speed_q8: e.ranged_attack_speed_q8,
                    ranged_attack_frame_range: e.ranged_attack_frame_range,
                    stagger_clip: e.stagger_clip,
                    stagger_speed_q8: e.stagger_speed_q8,
                    stagger_frame_range: e.stagger_frame_range,
                    stagger_ticks: e.stagger_ticks,
                    death_clip: e.death_clip,
                    combat_capsule_first: CombatCapsuleIndex(e.combat_capsule_first),
                    combat_capsule_count: e.combat_capsule_count,
                    ranged_attack_action: e.ranged_attack_action,
                    x: e.x,
                    y: e.y,
                    z: e.z,
                    yaw: e.yaw,
                    radius: e.radius,
                    height: e.height,
                    walk_speed: e.walk_speed,
                    run_speed: e.run_speed,
                    patrol_x: e.patrol[0],
                    patrol_y: e.patrol[1],
                    patrol_z: e.patrol[2],
                    patrol_wait_ticks: e.patrol_wait_ticks,
                    aggro_radius: e.aggro_radius,
                    reaction_ticks: e.reaction_ticks,
                    preferred_distance: e.preferred_distance,
                    spacing_tolerance: e.spacing_tolerance,
                    spacing_speed_percent: e.spacing_speed_percent,
                    decision_interval_ticks: e.decision_interval_ticks,
                    circle_chance: e.circle_chance,
                    attack_priority: e.attack_priority,
                    attack_cooldown_ticks: e.attack_cooldown_ticks,
                    group_attack_delay_ticks: e.group_attack_delay_ticks,
                    windup_ticks: e.windup_ticks,
                    attack_active_ticks: e.attack_active_ticks,
                    heavy_attack_active_ticks: e.heavy_attack_active_ticks,
                    ranged_attack_active_ticks: e.ranged_attack_active_ticks,
                    recovery_ticks: e.recovery_ticks,
                    attack_min_range: e.attack_min_range,
                    attack_max_range: e.attack_max_range,
                    poise: e.poise,
                    touch_damage: e.touch_damage,
                    max_health: e.max_health,
                    max_health_secondary: e.max_health_secondary,
                    soul_value: e.soul_value,
                    flags: e.flags,
                })
                .collect(),
            game_entity_ids: package
                .game_entity_ids
                .iter()
                .map(|id| StableId(*id))
                .collect(),
            entities: package
                .entities
                .iter()
                .map(|e| EntityRecord {
                    room: RoomIndex(e.room),
                    kind: match e.kind {
                        PlaytestEntityKind::HookPoint => EntityKind::HookPoint,
                        PlaytestEntityKind::Marker => EntityKind::Marker,
                        PlaytestEntityKind::StaticMesh => EntityKind::StaticMesh,
                    },
                    x: e.x,
                    y: e.y,
                    z: e.z,
                    yaw: e.yaw,
                    resource_slot: ResourceSlot(e.resource_slot),
                    flags: e.flags,
                })
                .collect(),
            destructibles: package
                .destructibles
                .iter()
                .map(|d| LevelDestructibleRecord {
                    max_health: d.max_health,
                    persistent_flag: d.persistent_flag,
                    damage_affinity: d.damage_affinity,
                    flags: d.flags,
                })
                .collect(),
            world_objects: package
                .world_objects
                .iter()
                .map(|o| LevelWorldObjectRecord {
                    room: RoomIndex(o.room),
                    kind: o.kind,
                    flags: o.flags,
                    source_index: o.source_index,
                    destructible: o.destructible,
                    bounds_min: o.bounds_min,
                    bounds_max: o.bounds_max,
                })
                .collect(),
            image_props: package
                .image_props
                .iter()
                .map(|p| LevelImagePropRecord {
                    room: RoomIndex(p.room),
                    texture_asset: asset(p.texture_asset_index),
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    pitch: p.pitch,
                    yaw: p.yaw,
                    roll: p.roll,
                    width: p.width,
                    height: p.height,
                    tint_rgb: p.tint_rgb,
                    baked_vertex_rgb: p.baked_vertex_rgb,
                    collision_min: p.collision_min,
                    collision_max: p.collision_max,
                    flags: p.flags,
                })
                .collect(),
            box_props: package
                .box_props
                .iter()
                .map(|p| LevelBoxPropRecord {
                    room: RoomIndex(p.room),
                    texture_assets: assets_of(&p.texture_asset_indices),
                    blend_modes: p.blend_modes,
                    uvs: p.uvs,
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    ground_y: p.ground_y,
                    pitch: p.pitch,
                    yaw: p.yaw,
                    roll: p.roll,
                    vertices: p.vertices,
                    collision_min: p.collision_min,
                    collision_max: p.collision_max,
                    surface_first: p.surface_first,
                    surface_count: p.surface_count,
                    tint_rgb: p.tint_rgb,
                    baked_vertex_rgb: p.baked_vertex_rgb,
                    flags: p.flags,
                })
                .collect(),
            box_prop_surfaces: package
                .box_prop_surfaces
                .iter()
                .map(|s| LevelBoxPropSurfaceRecord {
                    vertices: s.vertices,
                    center: s.center,
                    normal: s.normal,
                    uv_q8: s.uv_q8,
                    baked_vertex_rgb: s.baked_vertex_rgb,
                    source_face: s.source_face,
                    flags: s.flags,
                })
                .collect(),
            cylinder_props: package
                .cylinder_props
                .iter()
                .map(|p| LevelCylinderPropRecord {
                    room: RoomIndex(p.room),
                    texture_assets: assets_of(&p.texture_asset_indices),
                    blend_modes: p.blend_modes,
                    uvs: p.uvs,
                    surface_first: p.surface_first,
                    surface_count: p.surface_count,
                    center: p.center,
                    cull_radius: p.cull_radius,
                    bounds_min: p.bounds_min,
                    bounds_max: p.bounds_max,
                    flags: p.flags,
                })
                .collect(),
            cylinder_prop_surfaces: package
                .cylinder_prop_surfaces
                .iter()
                .map(|s| LevelCylinderPropSurfaceRecord {
                    vertices: s.vertices,
                    center: s.center,
                    normal: s.normal,
                    uv_q8: s.uv_q8,
                    baked_vertex_rgb: s.baked_vertex_rgb,
                    material_slot: s.material_slot,
                    vertex_count: s.vertex_count,
                })
                .collect(),
            arch_props: package
                .arch_props
                .iter()
                .map(|p| LevelArchPropRecord {
                    room: RoomIndex(p.room),
                    texture_assets: assets_of(&p.texture_asset_indices),
                    blend_modes: p.blend_modes,
                    uvs: p.uvs,
                    surface_first: p.surface_first,
                    surface_count: p.surface_count,
                    collision_first: p.collision_first,
                    collision_count: p.collision_count,
                    center: p.center,
                    cull_radius: p.cull_radius,
                    flags: p.flags,
                })
                .collect(),
            arch_prop_surfaces: package
                .arch_prop_surfaces
                .iter()
                .map(|s| LevelArchPropSurfaceRecord {
                    vertices: s.vertices,
                    center: s.center,
                    normal: s.normal,
                    uv_q8: s.uv_q8,
                    baked_vertex_rgb: s.baked_vertex_rgb,
                    material_slot: s.material_slot,
                })
                .collect(),
            arch_prop_collisions: package
                .arch_prop_collisions
                .iter()
                .map(|c| LevelArchPropCollisionRecord {
                    min: c.min,
                    max: c.max,
                })
                .collect(),
            model_instances: package
                .model_instances
                .iter()
                .map(|inst| LevelModelInstanceRecord {
                    room: RoomIndex(inst.room),
                    model: ModelIndex(inst.model),
                    clip: if inst.clip == MODEL_CLIP_INHERIT {
                        OptionalModelClipIndex::INHERIT
                    } else {
                        OptionalModelClipIndex::some(psx_level::ModelClipIndex(inst.clip))
                    },
                    pose_frame: inst.pose_frame,
                    x: inst.x,
                    y: inst.y,
                    z: inst.z,
                    yaw: inst.yaw,
                    visual_yaw: inst.visual_yaw,
                    pitch: inst.pitch,
                    roll: inst.roll,
                    visual_offset: inst.visual_offset,
                    visual_scale_q8: inst.visual_scale_q8,
                    material_override: material_override(&inst.material_override),
                    flags: inst.flags,
                })
                .collect(),
            player_spawn: spawn_record(spawn),
            player_controller: package.player_controller.map(|pc| PlayerControllerRecord {
                spawn: spawn_record(pc.spawn),
                character: CharacterIndex(pc.character),
                flags: 0,
            }),
            world_message: package.world_message.as_ref().map(message_record),
        }
    }

    /// The records in the shape the chunk writer takes.
    pub fn records(&self) -> LevelRecords<'_> {
        LevelRecords {
            rooms: &self.rooms,
            room_reflection_probes: &self.room_reflection_probes,
            lights: &self.lights,
            particle_emitters: &self.particle_emitters,
            interactable_messages: &self.interactable_messages,
            interactable_message_pages: &self.interactable_message_pages,
            interactable_message_pages_it: &self.interactable_message_pages_it,
            interactables: &self.interactables,
            vitality_circles: &self.vitality_circles,
            logic: &self.logic,
            logic_ids: &self.logic_ids,
            game_entities: &self.game_entities,
            game_entity_ids: &self.game_entity_ids,
            entities: &self.entities,
            destructibles: &self.destructibles,
            world_objects: &self.world_objects,
            image_props: &self.image_props,
            box_props: &self.box_props,
            box_prop_surfaces: &self.box_prop_surfaces,
            cylinder_props: &self.cylinder_props,
            cylinder_prop_surfaces: &self.cylinder_prop_surfaces,
            arch_props: &self.arch_props,
            arch_prop_surfaces: &self.arch_prop_surfaces,
            arch_prop_collisions: &self.arch_prop_collisions,
            model_instances: &self.model_instances,
            player_spawn: self.player_spawn,
            player_controller: self.player_controller,
            world_message: self.world_message,
            world_ambient_rgb: [32; 3],
        }
    }

    /// What the level holds beyond a runtime pool cap, one line per table over
    /// it, each naming the cap to raise. Empty when the level fits.
    pub fn over_caps(&self) -> Vec<String> {
        let mut over = Vec::new();
        let mut check = |what: &str, have: usize, cap: usize, name: &str| {
            if have > cap {
                over.push(format!(
                    "{what}: {have} placed, the runtime holds {cap} (psx_level::wire::caps::{name})"
                ));
            }
        };
        check(
            "enemies",
            self.game_entities.len(),
            caps::GAME_ENTITIES,
            "GAME_ENTITIES",
        );
        check("logic entities", self.logic.len(), caps::LOGIC, "LOGIC");
        check(
            "model instances",
            self.model_instances.len(),
            caps::MODEL_INSTANCES,
            "MODEL_INSTANCES",
        );
        check(
            "interactables",
            self.interactables.len(),
            caps::INTERACTABLES,
            "INTERACTABLES",
        );
        check(
            "box props",
            self.box_props.len(),
            caps::BOX_PROPS,
            "BOX_PROPS",
        );
        check("rooms", self.rooms.len(), caps::ROOMS, "ROOMS");
        over
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectDocument;

    /// The default project holds enemies, lights, interactables, model
    /// instances and a player: what the chunk decodes to is what the package
    /// holds, record for record.
    #[test]
    fn the_default_levels_records_survive_the_chunk() {
        let root = crate::default_project_dir();
        let project =
            ProjectDocument::load_from_path(root.join("project.ron")).expect("the default loads");
        let (package, report) = super::super::build_package(&project, &root);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let records = super::super::tests::decode_level_records(&package);

        // The manifest keeps the shape of the level, and it matches the chunk.
        let manifest = super::super::manifest::render_manifest_source(&package);
        for (name, len) in psx_level::wire::TABLE_NAMES
            .iter()
            .zip(records.table_lens())
        {
            assert!(
                manifest.contains(&format!(
                    "pub const LEN_{}: usize = {len};",
                    name.to_uppercase()
                )),
                "{name}: {len}"
            );
        }

        assert_eq!(records.rooms.len(), package.rooms.len());
        for (record, room) in records.rooms.iter().zip(&package.rooms) {
            assert_eq!(record.name, room.name);
            assert_eq!(record.fog_far, room.fog_far);
            assert_eq!(record.sky.top_rgb, room.sky.top_rgb);
            assert_eq!(
                record.sky.cyclorama_quads.len(),
                room.sky.cyclorama_quads.len()
            );
            assert_eq!(record.camera.distance, room.camera.distance);
        }
        assert_eq!(records.lights.len(), package.lights.len());
        assert!(!records.lights.is_empty());
        for (record, light) in records.lights.iter().zip(&package.lights) {
            assert_eq!(
                (
                    record.room.raw(),
                    record.x,
                    record.y,
                    record.z,
                    record.radius
                ),
                (light.room, light.x, light.y, light.z, light.radius)
            );
            assert_eq!(record.color, light.color);
        }
        assert_eq!(records.game_entities.len(), package.game_entities.len());
        assert!(!records.game_entities.is_empty());
        for (record, entity) in records.game_entities.iter().zip(&package.game_entities) {
            assert_eq!(record.max_health, entity.max_health);
            assert_eq!(
                (record.patrol_x, record.patrol_y, record.patrol_z),
                (entity.patrol[0], entity.patrol[1], entity.patrol[2])
            );
            assert_eq!(record.attack_frame_range, entity.attack_frame_range);
            assert_eq!(
                record.combat_capsule_first.raw(),
                entity.combat_capsule_first
            );
        }
        assert_eq!(records.game_entity_ids.len(), package.game_entity_ids.len());
        assert_eq!(records.interactables.len(), package.interactables.len());
        assert_eq!(records.model_instances.len(), package.model_instances.len());
        assert_eq!(records.world_objects.len(), package.world_objects.len());
        let spawn = package.spawn.expect("the default has a spawn");
        assert_eq!(records.player_spawn.x, spawn.x);
        assert_eq!(
            records.player_controller.map(|pc| pc.character.raw()),
            package.player_controller.map(|pc| pc.character)
        );
        assert_eq!(records.world_ambient_rgb, [32; 3]);
    }

    /// Every reference project: the chunk decodes to exactly what the cook
    /// built, table by table (the default project has no sky, far vista or
    /// props to speak of, these do).
    #[test]
    fn every_reference_projects_records_survive_the_chunk() {
        let mut seen = 0;
        for entry in std::fs::read_dir(crate::projects_dir()).expect("projects dir") {
            let root = entry.expect("entry").path();
            let Ok(project) = ProjectDocument::load_from_path(root.join("project.ron")) else {
                continue;
            };
            let (package, report) = super::super::build_package(&project, &root);
            let Some(package) = package else {
                continue;
            };
            assert!(report.is_ok(), "{root:?}: {:?}", report.error_messages());
            let built = OwnedLevelRecords::build(&package);
            let decoded = super::super::tests::decode_level_records(&package);
            assert_eq!(
                format!("{:#?}", built.records()),
                format!("{decoded:#?}"),
                "{root:?}"
            );
            seen += 1;
        }
        assert!(seen >= 8, "only {seen} reference projects cooked");
    }

    #[test]
    fn a_level_over_a_pool_cap_names_the_cap() {
        let package = PlaytestPackage {
            box_props: vec![Default::default(); caps::BOX_PROPS + 1],
            ..PlaytestPackage::default()
        };
        let over = OwnedLevelRecords::build(&package).over_caps();
        assert_eq!(over.len(), 1, "{over:?}");
        assert!(over[0].contains("box props") && over[0].contains("caps::BOX_PROPS"));
        let fits = PlaytestPackage::default();
        assert!(OwnedLevelRecords::build(&fits).over_caps().is_empty());
    }
}
