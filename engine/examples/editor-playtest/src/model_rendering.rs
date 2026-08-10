//! Glue over `psx_game_runtime::model_rendering` (and its
//! instances/equipment submodules): re-exports the draw vocabulary and
//! threads the cooked model tables, this example's knob/tuning consts,
//! and the arena-owned draw scratch into the crate policy, keeping the
//! old call-site signatures.

use super::*;
use psx_game_runtime::model_rendering as mr;

pub(super) use psx_game_runtime::model_rendering::{
    accumulate_model_instance_draw_stats, distance_xz_sq, draw_collision_cylinder_debug,
    emit_model_counters, EquipmentDrawStats, ModelInstanceDepthPass, ModelInstanceDrawStats,
    ModelInstancePoseOverride, PlayerModelDrawStats, RuntimeModelAsset,
};

/// The cooked model-family tables bundled for the crate render policy.
pub(super) fn model_tables() -> mr::ModelTables {
    mr::ModelTables {
        model_clip_bounds: MODEL_CLIP_BOUNDS,
        model_frame_bounds: MODEL_FRAME_BOUNDS,
        model_sockets: MODEL_SOCKETS,
        model_instances: MODEL_INSTANCES,
        equipment: EQUIPMENT,
        weapons: WEAPONS,
        weapon_hitboxes: WEAPON_HITBOXES,
        entities: ENTITIES,
    }
}

/// This example's model draw knobs (the `MODEL_*`/`MAX_*` consts in
/// `runtime_config`, as the crate value struct).
const MODEL_DRAW_KNOBS: mr::ModelDrawKnobs = mr::ModelDrawKnobs {
    texture_split_max_edge: MODEL_TEXTURE_SPLIT_MAX_EDGE,
    max_model_instances: MAX_MODEL_INSTANCES,
    max_equipment_draws: MAX_EQUIPMENT_DRAWS,
};

/// This example's actor floor-shadow tuning (the `SHADOW_*` consts in
/// `runtime_config`, as the crate value struct).
const SHADOW_TUNING: mr::ShadowTuning = mr::ShadowTuning {
    floor_lift: SHADOW_FLOOR_LIFT,
    depth_bias: SHADOW_DEPTH_BIAS,
    radius_scale_num: SHADOW_RADIUS_SCALE_NUM,
    radius_scale_den: SHADOW_RADIUS_SCALE_DEN,
    radius_min: SHADOW_RADIUS_MIN,
    radius_max: SHADOW_RADIUS_MAX,
};

impl Playtest {
    #[cfg(feature = "cd-stream-bench")]
    #[inline(never)]
    pub(super) fn step_persistent_model_assets(&mut self) -> bool {
        {
            let assets = persistent_assets_arena_mut();
            assets.begin(UI_PACK_START_LBA, UI_PACK_TOC, ASSETS);
            assets.pump(cd_arena(), RUNTIME_SCHEDULE.stream_pump_sectors_per_tick);
        }
        if persistent_assets_arena().ready() && !self.runtime_models_loaded {
            self.load_runtime_models();
            self.runtime_models_loaded = true;
            // Model parsing owns the CD first. Only now seed portal visibility
            // and the incremental room-window job; the same tick can reconcile
            // and pump WORLD.PAK without two readers sharing the controller.
            self.load_active_room_window();
        }
        self.runtime_models_loaded
    }

    pub(super) fn player_clip_duration_vblanks(
        &self,
        character: RuntimeCharacter,
        clip: ModelClipIndex,
        video_hz: VideoHz,
        speed_q8: u16,
        frame_range: psx_level::CharacterActionFrameRange,
    ) -> Option<u32> {
        mr::player_clip_duration_vblanks(
            &self.models,
            &self.clips,
            character,
            clip,
            video_hz,
            speed_q8,
            frame_range,
        )
    }

    pub(super) fn player_action_push_speed(
        &self,
        character: RuntimeCharacter,
        anim: PlayerAnim,
        local_tick: u32,
        video_hz: VideoHz,
    ) -> Option<i32> {
        mr::player_action_push_speed(
            &self.models,
            &self.clips,
            character,
            anim,
            local_tick,
            video_hz,
        )
    }

    #[inline(never)]
    pub(super) fn load_runtime_models(&mut self) {
        mr::load_runtime_models(
            MODELS,
            MODEL_CLIPS,
            &mut self.models,
            &mut self.clips,
            &mut self.model_faces,
            &mut self.model_face_count,
            &mut self.model_parts,
            &mut self.model_part_count,
            &mut self.model_vertices,
            &mut self.model_vertex_count,
            runtime_model_asset_bytes,
            ensure_model_atlas_uploaded,
        );
    }
}

fn runtime_model_asset_bytes(asset_id: AssetId, kind: AssetKind) -> Option<&'static [u8]> {
    let asset = find_asset_of_kind(ASSETS, asset_id, kind)?;
    if !asset.bytes.is_empty() {
        return Some(asset.bytes);
    }
    #[cfg(feature = "cd-stream-bench")]
    {
        persistent_assets_arena().bytes_for(asset)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        None
    }
}

fn room_reflection_probe_slot(room: RoomIndex) -> Option<VramSlot> {
    let asset = ROOM_REFLECTION_PROBES
        .get(room.to_usize())
        .copied()
        .flatten()?;
    find_room_texture_vram_slot(asset)
}

/// Draw the player's animated model through the crate policy.
pub(super) fn draw_player(
    current_room: RoomIndex,
    character: RuntimeCharacter,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    x: i32,
    y: i32,
    z: i32,
    yaw: Angle,
    anim_action: CharacterAnimationAction,
    clip_local: ModelClipIndex,
    anim_start_tick: SimTick,
    blend: Option<PlayerAnimBlend>,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> PlayerModelDrawStats {
    mr::draw_player::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_BOUNDS_CULLING_ENABLED,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        MODEL_DRAW_KNOBS,
        model_scratch_arena(),
        character,
        models,
        model_faces,
        model_parts,
        model_vertices,
        clips,
        x,
        y,
        z,
        yaw,
        anim_action,
        clip_local,
        anim_start_tick,
        blend,
        elapsed_tick,
        video_hz,
        camera,
        options,
        lighting,
        room_reflection_probe_slot(current_room),
        &mut model_texture_slot,
        triangles,
        world,
    )
}

/// Draw non-player equipment riding its bound model instances (the
/// per-room enemy weapon pass) through the crate policy.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_instance_equipment(
    current_room: RoomIndex,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    pose_overrides: &[ModelInstancePoseOverride],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    mr::draw_instance_equipment::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        MODEL_DRAW_KNOBS,
        model_scratch_arena(),
        current_room,
        elapsed_tick,
        video_hz,
        camera,
        options,
        lighting,
        models,
        model_faces,
        model_parts,
        model_vertices,
        clips,
        pose_overrides,
        triangles,
        world,
    )
}

/// Draw the player's attached equipment through the crate policy.
pub(super) fn draw_player_equipment(
    character: RuntimeCharacter,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    x: i32,
    y: i32,
    z: i32,
    yaw: Angle,
    anim_action: CharacterAnimationAction,
    clip_local: ModelClipIndex,
    anim_start_tick: SimTick,
    blend: Option<PlayerAnimBlend>,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    mr::draw_player_equipment::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        MODEL_DRAW_KNOBS,
        model_scratch_arena(),
        character,
        models,
        model_faces,
        model_parts,
        model_vertices,
        clips,
        x,
        y,
        z,
        yaw,
        anim_action,
        clip_local,
        anim_start_tick,
        blend,
        elapsed_tick,
        video_hz,
        camera,
        options,
        lighting,
        triangles,
        world,
    )
}

/// Animate + draw the placed model instances of `current_room` through
/// the crate policy.
pub(super) fn draw_model_instances(
    current_room: RoomIndex,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    pose_overrides: &[ModelInstancePoseOverride],
    depth_pass: ModelInstanceDepthPass,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    mr::draw_model_instances::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_BOUNDS_CULLING_ENABLED,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        MODEL_DRAW_KNOBS,
        model_scratch_arena(),
        current_room,
        elapsed_tick,
        video_hz,
        camera,
        options,
        lighting,
        room_reflection_probe_slot(current_room),
        models,
        model_faces,
        model_parts,
        model_vertices,
        clips,
        pose_overrides,
        depth_pass,
        &mut model_texture_slot,
        triangles,
        world,
    )
}

/// Draw the floor shadow decal under every placed model instance.
pub(super) fn draw_model_instance_shadows(
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    pose_overrides: &[ModelInstancePoseOverride],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_model_instance_shadows(
        model_tables(),
        MODEL_DRAW_KNOBS,
        SHADOW_TUNING,
        current_room,
        camera,
        options,
        material,
        models,
        pose_overrides,
        triangles,
        world,
    );
}

/// Draw one actor's circular floor shadow decal.
pub(super) fn draw_actor_shadow(
    x: i32,
    floor_y: i32,
    z: i32,
    radius: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_actor_shadow(
        SHADOW_TUNING,
        x,
        floor_y,
        z,
        radius,
        camera,
        options,
        material,
        triangles,
        world,
    );
}

/// Shadow decal radius for an actor's collision radius.
pub(super) fn actor_shadow_radius(base_radius: i32) -> i32 {
    mr::actor_shadow_radius(SHADOW_TUNING, base_radius)
}

/// The player's camera-view depth in `active`'s room frame.
pub(super) fn player_actor_depth_for_room(
    active: ActiveRuntimeRoom,
    character: Option<RuntimeCharacter>,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    player: RoomPoint,
    camera: &WorldCamera,
) -> Option<i32> {
    mr::player_actor_depth_for_room(active, character, models, player, camera)
}
