//! Glue over `psx_game_runtime::model_rendering` (and its
//! instances/equipment submodules): re-exports the draw vocabulary and
//! threads the cooked model tables, this example's knob/tuning consts,
//! and the arena-owned draw scratch into the crate policy, keeping the
//! old call-site signatures.

use super::*;
use psx_game_runtime::model_rendering as mr;

pub(super) use psx_game_runtime::model_rendering::{
    accumulate_model_instance_draw_stats, distance_xz_sq, draw_collision_cylinder_debug,
    emit_model_counters, EquipmentDrawStats, InstanceActorPoseSnapshot, ModelInstanceDepthPass,
    ModelInstanceDrawStats, ModelInstancePoseOverride, PlayerActorPoseSnapshot,
    PlayerModelDrawStats, RuntimeModelAsset,
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
    equipment_wire_q12: [mr::ASSEMBLED_Q12; mr::MAX_PLAYER_EQUIPMENT],
    equipment_wireframe: false,
};

/// One weapon's appearance in a swing: which weapon, which hand, and the clip
/// frames it materialises on and dissolves away at.
struct WeaponBeat {
    heavy: bool,
    off_hand: bool,
    /// Clip frame the arm is thrown to the side, where the blade is whole.
    throw_frame: u16,
    /// Clip frame it is gone by. `u16::MAX` means the end of the clip, which
    /// is what the last weapon in a swing uses.
    until_frame: u16,
}

const CLIP_END: u16 = u16::MAX;

/// Which weapons appear when, per attack.
///
/// The structure is yours: level 1 throws the right arm once and the light
/// sword rides it. Level 2 throws it twice, light on the first, heavy on the
/// second, and the light one goes away as the heavy arrives. Level 3 is level 2
/// plus the left arm's own throw, which brings the light sword to that hand.
///
/// The frames come from the cooked clips (peaks of the right and left hand's
/// distance from the hips), but which peak counts as a throw is a judgement,
/// not a measurement: the vertical clips show the two-beat structure cleanly
/// while the horizontal ones have a wind-up bump of nearly the same size. Move
/// a number here if a weapon lands on the wrong beat.
fn weapon_beats(anim: PlayerAnim) -> &'static [WeaponBeat] {
    const LIGHT_ATTACK: &[WeaponBeat] = &[WeaponBeat {
        heavy: false,
        off_hand: false,
        throw_frame: 25,
        until_frame: CLIP_END,
    }];
    const HEAVY_ATTACK: &[WeaponBeat] = &[
        WeaponBeat {
            heavy: false,
            off_hand: false,
            throw_frame: 23,
            until_frame: 35,
        },
        WeaponBeat {
            heavy: true,
            off_hand: false,
            throw_frame: 35,
            until_frame: CLIP_END,
        },
    ];
    const COMBO_ATTACK: &[WeaponBeat] = &[
        WeaponBeat {
            heavy: false,
            off_hand: false,
            throw_frame: 25,
            until_frame: 44,
        },
        WeaponBeat {
            heavy: true,
            off_hand: false,
            throw_frame: 44,
            until_frame: CLIP_END,
        },
        WeaponBeat {
            heavy: false,
            off_hand: true,
            throw_frame: 35,
            until_frame: CLIP_END,
        },
    ];
    const VERT_LIGHT_ATTACK: &[WeaponBeat] = &[WeaponBeat {
        heavy: false,
        off_hand: false,
        throw_frame: 33,
        until_frame: CLIP_END,
    }];
    const VERT_HEAVY_ATTACK: &[WeaponBeat] = &[
        WeaponBeat {
            heavy: false,
            off_hand: false,
            throw_frame: 20,
            until_frame: 39,
        },
        WeaponBeat {
            heavy: true,
            off_hand: false,
            throw_frame: 39,
            until_frame: CLIP_END,
        },
    ];
    const VERT_COMBO_ATTACK: &[WeaponBeat] = &[
        WeaponBeat {
            heavy: false,
            off_hand: false,
            throw_frame: 15,
            until_frame: 40,
        },
        WeaponBeat {
            heavy: true,
            off_hand: false,
            throw_frame: 40,
            until_frame: CLIP_END,
        },
        WeaponBeat {
            heavy: false,
            off_hand: true,
            throw_frame: 26,
            until_frame: CLIP_END,
        },
    ];
    match anim {
        PlayerAnim::LightAttack => LIGHT_ATTACK,
        PlayerAnim::HeavyAttack => HEAVY_ATTACK,
        PlayerAnim::ComboAttack => COMBO_ATTACK,
        PlayerAnim::VertLightAttack => VERT_LIGHT_ATTACK,
        PlayerAnim::VertHeavyAttack => VERT_HEAVY_ATTACK,
        PlayerAnim::VertComboAttack => VERT_COMBO_ATTACK,
        _ => &[],
    }
}

/// How long a weapon takes to grow up the blade, in CLIP FRAMES. Clips run at
/// roughly a quarter of a frame per tick here, so 8 frames is about half a
/// second. The same ramp runs backwards when it goes away.
const MATERIALISE_FRAMES: u32 = 8;

/// How far up the blade this beat's weapon has reached at `phase_q12`, Q12.
///
/// The ramp runs INTO the throw, so the blade is whole at the moment the arm is
/// out, holds, then retreats into `until_frame` the way it came. Taking the
/// smaller of the two ramps means a short window simply never reaches full
/// rather than snapping.
fn wire_for_beat(beat: &WeaponBeat, phase_q12: u32, frame_count: u16) -> u16 {
    let ramp_q12 = MATERIALISE_FRAMES << 12;
    let open_q12 = u32::from(beat.throw_frame)
        .saturating_mul(4096)
        .saturating_sub(ramp_q12);
    let until = if beat.until_frame == CLIP_END {
        frame_count.saturating_sub(1)
    } else {
        beat.until_frame
    };
    let until_q12 = u32::from(until).saturating_mul(4096);
    if phase_q12 < open_q12 || phase_q12 >= until_q12 {
        return 0;
    }
    let rising = (phase_q12 - open_q12) / MATERIALISE_FRAMES;
    let falling = (until_q12 - phase_q12) / MATERIALISE_FRAMES;
    rising.min(falling).min(4096) as u16
}

/// How far up the blade every player equipment record has grown this frame.
///
/// Each cooked record is matched to a beat by the weapon's NAME and its socket,
/// not by table order, so reordering the scene cannot arm the wrong hand.
pub(super) fn equipment_wire_q12(
    anim: PlayerAnim,
    phase_q12: u32,
    frame_count: u16,
) -> [u16; mr::MAX_PLAYER_EQUIPMENT] {
    let mut wire = [0u16; mr::MAX_PLAYER_EQUIPMENT];
    let beats = weapon_beats(anim);
    for (index, record) in EQUIPMENT.iter().enumerate().take(mr::MAX_PLAYER_EQUIPMENT) {
        let Some(weapon) = WEAPONS.get(record.weapon.to_usize()) else {
            continue;
        };
        let heavy = weapon.name.ends_with("Heavy");
        let off_hand = record.character_socket == "left_hand_grip";
        let Some(beat) = beats
            .iter()
            .find(|beat| beat.heavy == heavy && beat.off_hand == off_hand)
        else {
            continue;
        };
        wire[index] = wire_for_beat(beat, phase_q12, frame_count);
    }
    wire
}

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
            if self.bsp.is_none() {
                self.load_active_room_window();
            }
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
        self.sort_weapon_faces_hilt_first();
    }

    /// Order every weapon model's faces along its blade, hilt first.
    ///
    /// This is what makes the materialise effect cheap: with the faces in this
    /// order, "filled up to here" is a slice of the face list, so the solid
    /// part goes through the ordinary model path in one call instead of being
    /// submitted face by face. Face order is otherwise arbitrary, since the
    /// ordering table sorts by depth.
    fn sort_weapon_faces_hilt_first(&mut self) {
        for weapon in WEAPONS {
            let Some(model_index) = weapon.model else {
                continue;
            };
            let Some(Some(model)) = self.models.get(model_index.to_usize()) else {
                continue;
            };
            let first = model.face_first as usize;
            let count = model.face_count as usize;
            let vertex_first = model.vertex_first as usize;
            if first + count > self.model_face_count {
                continue;
            }
            // Insertion sort: a weapon has fewer than a hundred faces, and the
            // guest does not need a general sort dragged in for it.
            let axis = |face: &TexturedModelRenderFace| -> i32 {
                let mut sum = 0i32;
                for corner in 0..3 {
                    let index = vertex_first + face.vertex_indices()[corner] as usize;
                    if let Some(vertex) = self.model_vertices.get(index) {
                        sum += i32::from(vertex.position.y);
                    }
                }
                sum
            };
            for i in first + 1..first + count {
                let mut j = i;
                while j > first && axis(&self.model_faces[j - 1]) > axis(&self.model_faces[j]) {
                    self.model_faces.swap(j - 1, j);
                    j -= 1;
                }
            }
        }
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

impl Playtest {
    /// Freeze actor presentation after the simulation tick. Rendering,
    /// equipment sockets, and combat consume these values until the next tick.
    pub(super) fn refresh_actor_pose_snapshots(&mut self, ctx: &Ctx) {
        let player = self.motor.position();
        self.player_actor_pose = self.character.and_then(|character| {
            mr::resolve_player_actor_pose(
                model_tables(),
                character,
                &self.models,
                &self.clips,
                player.x,
                player.y,
                player.z,
                self.motor.yaw(),
                self.anim_state.action(),
                character.clip_for(self.anim_state),
                self.anim_start_tick,
                self.player_anim_blend(ctx.sim_tick),
                ctx.sim_tick,
                ctx.video_hz,
            )
        });

        // Where rendering samples the phase is where it is worth measuring:
        // this is the number that says which cooked frame is on screen.
        if let Some(pose) = self.player_actor_pose {
            telemetry::counter(
                telemetry::counter::PLAYER_ANIM_PHASE_Q12,
                pose.pose().phase_q12(),
            );
        }

        for pose in self.instance_actor_poses.iter_mut() {
            *pose = None;
        }
        let mut overrides = [ModelInstancePoseOverride {
            instance: u16::MAX,
            x: 0,
            y: 0,
            z: 0,
            yaw: 0,
            clip: psx_level::OptionalModelClipIndex::NONE,
            phase_ticks: 0,
            one_shot: false,
        }; MAX_GAME_ENTITIES];
        let override_count = self.game_entity_pose_overrides(&mut overrides);
        let overrides = &overrides[..override_count];
        let elapsed_tick = self.gameplay_tick(ctx.sim_tick);
        let count = MODEL_INSTANCES.len().min(self.instance_actor_poses.len());
        let mut index = 0usize;
        while index < count {
            self.instance_actor_poses[index] = mr::resolve_instance_actor_pose(
                model_tables(),
                &self.models,
                &self.clips,
                overrides,
                index,
                elapsed_tick,
                ctx.video_hz,
            );
            if self.bsp.is_some() && self.bsp_instance_visible_mask & (1u16 << index) == 0 {
                self.instance_actor_poses[index] = None;
            }
            index += 1;
        }
    }

    pub(super) fn clear_actor_pose_snapshots(&mut self) {
        self.player_actor_pose = None;
        for pose in self.instance_actor_poses.iter_mut() {
            *pose = None;
        }
    }
}

/// Draw the player's animated model through the crate policy.
pub(super) fn draw_player(
    current_room: RoomIndex,
    character: RuntimeCharacter,
    player_pose: PlayerActorPoseSnapshot,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> PlayerModelDrawStats {
    mr::draw_player_from_pose::<
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
        player_pose,
        model_faces,
        model_parts,
        model_vertices,
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
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    max_draws: usize,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let mut remaining = max_draws.min(MODEL_DRAW_KNOBS.max_equipment_draws);
    for pose in instance_poses.iter().copied().flatten() {
        if remaining == 0 {
            break;
        }
        let mut knobs = MODEL_DRAW_KNOBS;
        knobs.max_equipment_draws = remaining;
        let stats = mr::draw_instance_equipment_from_pose::<
            MAX_RUNTIME_MODELS,
            MAX_RUNTIME_MODEL_CLIPS,
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            MODEL_PROFILE_ENABLED,
        >(
            model_tables(),
            knobs,
            model_scratch_arena(),
            current_room,
            pose,
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
            triangles,
            world,
        );
        mr::accumulate_equipment_draw_stats(&mut out, stats);
        remaining = remaining.saturating_sub(stats.draws as usize);
        if stats.stats.primitive_overflow || stats.stats.command_overflow {
            break;
        }
    }
    out
}

/// Draw the player's attached equipment through the crate policy.
pub(super) fn draw_player_equipment(
    wire_q12: [u16; mr::MAX_PLAYER_EQUIPMENT],
    player_pose: PlayerActorPoseSnapshot,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut knobs = MODEL_DRAW_KNOBS;
    knobs.equipment_wire_q12 = wire_q12;
    knobs.equipment_wireframe = true;
    mr::draw_player_equipment_from_pose::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        knobs,
        model_scratch_arena(),
        player_pose,
        models,
        model_faces,
        model_parts,
        model_vertices,
        clips,
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
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    depth_pass: ModelInstanceDepthPass,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    let mut out = ModelInstanceDrawStats::default();
    for pose in instance_poses
        .iter()
        .take(MODEL_DRAW_KNOBS.max_model_instances)
        .copied()
        .flatten()
    {
        let stats = mr::draw_model_instance_from_pose::<
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
            pose,
            elapsed_tick,
            video_hz,
            camera,
            options,
            lighting,
            room_reflection_probe_slot(current_room),
            model_faces,
            model_parts,
            model_vertices,
            depth_pass,
            &mut model_texture_slot,
            triangles,
            world,
        );
        accumulate_model_instance_draw_stats(&mut out, stats);
        if stats.stats.primitive_overflow || stats.stats.command_overflow {
            break;
        }
    }
    out
}

/// Draw the floor shadow decal under every placed model instance.
pub(super) fn draw_model_instance_shadows(
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    pose_overrides: &[ModelInstancePoseOverride],
    // psx-numeric-allow-next-line: one bit per model instance; the width IS the instance capacity
    visible_instance_mask: u64,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
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
        visible_instance_mask,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
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
