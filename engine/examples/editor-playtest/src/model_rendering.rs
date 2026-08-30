//! Glue over `psx_game_runtime::model_rendering` (and its
//! instances/equipment submodules): re-exports the draw vocabulary and
//! threads the cooked model tables, this example's knob/tuning consts,
//! and the arena-owned draw scratch into the crate policy, keeping the
//! old call-site signatures.

use super::*;
use crate::playtest_runtime::live_action_speed_q8;
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
    equipment_materialization_skins: [None; mr::MAX_PLAYER_EQUIPMENT],
    instance_equipment_skin: None,
    equipment_materialization: false,
};

/// The Horizon palette, read back from the authored track that already
/// defines it: the player's R1 (Horizon-lane) blade trail. Enemy blades
/// take this so a held weapon reads as the same energy lane the player
/// swings on R1/R2, without a second authored copy of the colour to drift.
fn horizon_skin() -> Option<mr::EquipmentMaterializationSkin> {
    let controller = PLAYER_CONTROLLER?;
    let [r, g, b] = WEAPON_APPEARANCES
        .iter()
        .find(|appearance| {
            appearance.flags & psx_level::weapon_appearance_flags::TRAIL != 0
                && appearance.character == controller.character
                && appearance.action == CharacterAnimationAction::LightAttack
        })
        .map(|appearance| appearance.trail_tip_color)?;
    Some(mr::EquipmentMaterializationSkin::opaque((r, g, b)))
}

/// Resolve Horizon/Zenith presentation from the same action-authored colours
/// that drive the blade trail. This keeps the cage, textured weapon and trail
/// on one palette without adding a second gameplay classification.
fn equipment_materialization_skins(
    anim: PlayerAnim,
) -> [Option<mr::EquipmentMaterializationSkin>; mr::MAX_PLAYER_EQUIPMENT] {
    let mut skins = [None; mr::MAX_PLAYER_EQUIPMENT];
    let Some(controller) = PLAYER_CONTROLLER else {
        return skins;
    };
    let action = anim.action();
    // Dual attacks may materialise a second blade on a track that does not
    // author its own trail. Resolve the Horizon/Zenith colour once from the
    // action's trail-bearing track, then apply it to every participating
    // weapon/socket pair.
    let Some([r, g, b]) = WEAPON_APPEARANCES
        .iter()
        .find(|appearance| {
            appearance.flags & psx_level::weapon_appearance_flags::TRAIL != 0
                && appearance.character == controller.character
                && appearance.action == action
        })
        .map(|appearance| appearance.trail_tip_color)
    else {
        return skins;
    };
    for (index, record) in EQUIPMENT.iter().enumerate().take(mr::MAX_PLAYER_EQUIPMENT) {
        let participates = WEAPON_APPEARANCES.iter().any(|appearance| {
            appearance.character == controller.character
                && appearance.action == action
                && appearance.weapon == record.weapon
                && appearance.character_socket == record.character_socket
        });
        if !participates {
            continue;
        }
        skins[index] = Some(mr::EquipmentMaterializationSkin::opaque((r, g, b)));
    }
    skins
}

/// How far up a weapon this authored visibility beat has reached, Q12.
/// The same sampled-frame transition runs into the fully-visible marker and
/// backwards into the hidden marker. A zero transition is a deliberate cut.
fn wire_for_appearance(
    appearance: &psx_level::WeaponAppearanceRecord,
    phase_q12: u32,
    frame_count: u16,
) -> u16 {
    let until = if appearance.hidden_frame == psx_level::CHARACTER_ACTION_FRAME_END_FULL {
        frame_count.saturating_sub(1)
    } else {
        appearance.hidden_frame.min(frame_count.saturating_sub(1))
    };
    let visible_q12 = u32::from(appearance.fully_visible_frame) << 12;
    let until_q12 = u32::from(until).saturating_mul(4096);
    if appearance.transition_frames == 0 {
        return if phase_q12 >= visible_q12 && phase_q12 < until_q12 {
            mr::ASSEMBLED_Q12
        } else {
            0
        };
    }
    let transition = u32::from(appearance.transition_frames);
    let ramp_q12 = transition << 12;
    let open_q12 = visible_q12.saturating_sub(ramp_q12);
    if phase_q12 < open_q12 || phase_q12 >= until_q12 {
        return 0;
    }
    let rising = (phase_q12 - open_q12) / transition;
    let falling = (until_q12 - phase_q12) / transition;
    rising.min(falling).min(u32::from(mr::ASSEMBLED_Q12)) as u16
}

/// How far up the blade every player equipment record has grown this frame.
///
/// Each cooked record is matched to an Animation Studio track by character,
/// action, weapon id, and socket. Scene/table reordering therefore cannot arm
/// the wrong hand.
pub(super) fn equipment_wire_q12(
    anim: PlayerAnim,
    phase_q12: u32,
    frame_count: u16,
) -> [u16; mr::MAX_PLAYER_EQUIPMENT] {
    let mut wire = [0u16; mr::MAX_PLAYER_EQUIPMENT];
    let Some(controller) = PLAYER_CONTROLLER else {
        return wire;
    };
    let action = anim.action();
    for (index, record) in EQUIPMENT.iter().enumerate().take(mr::MAX_PLAYER_EQUIPMENT) {
        let Some(appearance) = WEAPON_APPEARANCES.iter().find(|appearance| {
            appearance.character == controller.character
                && appearance.action == action
                && appearance.weapon == record.weapon
                && appearance.character_socket == record.character_socket
        }) else {
            continue;
        };
        wire[index] = wire_for_appearance(appearance, phase_q12, frame_count);
    }
    wire
}

/// This example's actor floor-shadow tuning (the `SHADOW_*` consts in
/// `runtime_config`, as the crate value struct).
/// Tuning for the projected (flattened-geometry) actor shadow.
///
/// `depth_bias` is ADDED to the actor clearance the caller already applied,
/// so the shadow sits just behind the actor's own body but still in front of
/// the floor it lands on.
#[cfg(feature = "actor-shadows-projected")]
const PROJECTED_SHADOW_TUNING: mr::ProjectedShadowTuning = mr::ProjectedShadowTuning {
    light: mr::ShadowLight::OVERHEAD,
    floor_lift: SHADOW_FLOOR_LIFT,
    depth_bias: 0,
    blend: psx_gpu::material::BlendMode::Average,
    tint: (0, 0, 0),
    max_drop: 512,
};

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
        character: &RuntimeCharacter,
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
        character: &RuntimeCharacter,
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
        #[cfg(not(feature = "cd-stream-bench"))]
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
        #[cfg(feature = "cd-stream-bench")]
        self.load_streamed_runtime_models();
        assert!(
            self.models.iter().take(MODELS.len()).all(Option::is_some),
            "runtime model load dropped a cooked model"
        );
        self.sort_weapon_faces_hilt_first();
    }

    /// Decode model source blobs from the shared loading scratch, retaining
    /// only compact geometry pools and VRAM atlas slots for gameplay.
    #[cfg(feature = "cd-stream-bench")]
    fn load_streamed_runtime_models(&mut self) {
        mr::reset_runtime_model_tables(
            &mut self.models,
            &mut self.clips,
            &mut self.model_face_count,
            &mut self.model_part_count,
            &mut self.model_vertex_count,
        );
        mr::load_runtime_model_clips(MODEL_CLIPS, &mut self.clips, runtime_model_asset_bytes);

        for (index, record) in MODELS.iter().enumerate() {
            if index >= self.models.len() {
                break;
            }
            let Some(texture_asset) = record.texture_asset else {
                continue;
            };
            let Some(atlas_slot) = with_transient_gameplay_asset_bytes(
                texture_asset,
                AssetKind::Texture,
                |atlas_bytes| ensure_model_atlas_uploaded(texture_asset, atlas_bytes),
            )
            .flatten() else {
                continue;
            };
            let decoded = with_transient_gameplay_asset_bytes(
                record.mesh_asset,
                AssetKind::ModelMesh,
                |mesh_bytes| {
                    RuntimeModelAsset::from_record_bytes(
                        psx_level::ModelIndex::new(index as u16),
                        record,
                        mesh_bytes,
                        atlas_slot,
                        &mut self.model_faces,
                        &mut self.model_face_count,
                        &mut self.model_parts,
                        &mut self.model_part_count,
                        &mut self.model_vertices,
                        &mut self.model_vertex_count,
                    )
                },
            )
            .flatten();
            self.models[index] = decoded;
        }
    }

    /// Drop every parsed view into the scene-lifetime gameplay asset arena.
    /// Call before handing those bytes back to the front-end UI cache.
    pub(super) fn unload_runtime_models(&mut self) {
        for model in self.models.iter_mut() {
            *model = None;
        }
        for clip in self.clips.iter_mut() {
            *clip = None;
        }
        self.model_face_count = 0;
        self.model_part_count = 0;
        self.model_vertex_count = 0;
        self.runtime_models_loaded = false;
        self.clear_actor_pose_snapshots();
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

/// Read one transient gameplay asset through the shared loading scratch and
/// consume it before the next staged read overwrites that memory.
#[cfg(feature = "cd-stream-bench")]
fn with_transient_gameplay_asset_bytes<R>(
    asset_id: AssetId,
    kind: AssetKind,
    consume: impl FnOnce(&[u8]) -> R,
) -> Option<R> {
    let asset = find_asset_of_kind(ASSETS, asset_id, kind)?;
    if !asset.bytes.is_empty() {
        return Some(consume(asset.bytes));
    }
    if asset.flags & psx_level::asset_flags::STREAMED_GAMEPLAY_TRANSIENT == 0 {
        return None;
    }
    let byte_count = asset.ram_bytes as usize;
    let scratch = font_scratch_arena();
    let stage = scratch.stage_words_mut(byte_count.div_ceil(4))?;
    let result = psx_game_runtime::cd_stream::read_chunk_blocking(
        cd_arena(),
        UI_PACK_START_LBA,
        UI_PACK_TOC,
        asset.id.0 as u32,
        stage,
    );
    if result.status != psx_game_runtime::cd_stream::ROOM_CHUNK_STATUS_OK
        || result.bytes != byte_count
    {
        return None;
    }
    Some(consume(scratch.staged_bytes(result.bytes)?))
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
        self.previous_player_actor_pose = self.player_actor_pose;
        // `RuntimeCharacter` is 672 bytes and the R3000A has no data cache, so
        // every by-value hop through this path is a full-latency RAM copy. The
        // cooked record is borrowed from here down.
        self.player_actor_pose = match self.character.as_ref() {
            Some(character) => {
                // The vitality Attack Speed lane was the only thing that made
                // the presentation character differ from the cooked one, and
                // the resolver reads exactly two speeds because of it. Hand it
                // those two numbers instead of a retuned copy of the record.
                let blend = self.player_anim_blend(ctx.sim_tick);
                let modifiers = self.vitality_modifiers();
                let speeds = mr::PlayerActionSpeeds {
                    action_q8: live_action_speed_q8(modifiers, character, self.anim_state),
                    blend_q8: blend.map_or(0, |blend| {
                        live_action_speed_q8(modifiers, character, blend.anim)
                    }),
                };
                let clip_local = character.clip_for(self.anim_state);
                // The clip's first-frame root is constant for the clip, and
                // sampling it was a whole pose decode per tick. Last tick's
                // snapshot already carries it; reuse it while the same clip of
                // the same model is playing. A clip or model change simply
                // misses and the resolver samples once.
                let cached_clip_first_root_xz = self
                    .previous_player_actor_pose
                    .filter(|previous| {
                        previous.clip_local() == clip_local
                            && previous.model().index == character.model
                    })
                    .and_then(|previous| previous.clip_first_root_xz());
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
                    clip_local,
                    self.anim_start_tick,
                    blend,
                    speeds,
                    cached_clip_first_root_xz,
                    ctx.sim_tick,
                    ctx.video_hz,
                )
            }
            None => None,
        };

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
        let mut overrides =
            psx_engine::FixedScratch::<ModelInstancePoseOverride, MAX_GAME_ENTITIES>::new();
        self.game_entity_pose_overrides(&mut overrides);
        let overrides = overrides.as_slice();
        let elapsed_tick = self.gameplay_tick(ctx.sim_tick);
        let count = MODEL_INSTANCES.len().min(self.instance_actor_poses.len());
        let bsp_resident = self.bsp.is_some();
        let mut index = 0usize;
        while index < count {
            // The visibility mask discards the pose after resolving it, so
            // test it first. `resolve_instance_actor_pose` reads only shared
            // tables, which makes the skip output-identical.
            if bsp_resident && self.bsp_instance_visible_mask & (1u16 << index) == 0 {
                self.instance_actor_poses[index] = None;
                index += 1;
                continue;
            }
            self.instance_actor_poses[index] = mr::resolve_instance_actor_pose(
                model_tables(),
                &self.models,
                &self.clips,
                overrides,
                index,
                elapsed_tick,
                ctx.video_hz,
            );
            index += 1;
        }
    }

    pub(super) fn clear_actor_pose_snapshots(&mut self) {
        self.player_actor_pose = None;
        self.previous_player_actor_pose = None;
        for pose in self.instance_actor_poses.iter_mut() {
            *pose = None;
        }
    }
}

/// Draw the player's animated model through the crate policy.
pub(super) fn draw_player(
    current_room: RoomIndex,
    character: &RuntimeCharacter,
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
    let instance_equipment_skin = horizon_skin();
    for pose in instance_poses.iter().copied().flatten() {
        if remaining == 0 {
            break;
        }
        let mut knobs = MODEL_DRAW_KNOBS;
        knobs.max_equipment_draws = remaining;
        knobs.instance_equipment_skin = instance_equipment_skin;
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
    anim: PlayerAnim,
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
    triangles: &mut (impl PrimitiveSink<TriTextured>
              + PrimitiveSink<psx_gpu::prim::LineMono>
              + PrimitiveSink<psx_gpu::prim::QuadGouraudBlended>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut knobs = MODEL_DRAW_KNOBS;
    knobs.equipment_wire_q12 = wire_q12;
    knobs.equipment_materialization_skins = equipment_materialization_skins(anim);
    knobs.equipment_materialization = true;
    let mut out = mr::draw_player_equipment_from_pose::<
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
    );
    if let Some(controller) = PLAYER_CONTROLLER {
        let trail = mr::draw_player_weapon_trails_from_pose::<MAX_RUNTIME_MODELS, OT_DEPTH>(
            model_tables(),
            WEAPON_APPEARANCES,
            controller.character,
            anim.action(),
            wire_q12,
            player_pose,
            models,
            model_parts,
            model_vertices,
            camera,
            options,
            triangles,
            world,
        );
        out.stats.submitted_triangles = out
            .stats
            .submitted_triangles
            .saturating_add(trail.submitted_triangles);
        out.stats.culled_triangles = out
            .stats
            .culled_triangles
            .saturating_add(trail.culled_triangles);
        out.stats.dropped_triangles = out
            .stats
            .dropped_triangles
            .saturating_add(trail.dropped_triangles);
        out.stats.primitive_overflow |= trail.primitive_overflow;
        out.stats.command_overflow |= trail.command_overflow;
    }
    out
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

/// Draw the player's own geometry flattened onto the floor plane.
#[cfg(feature = "actor-shadows-projected")]
pub(super) fn draw_player_projected_shadow(
    player_pose: PlayerActorPoseSnapshot,
    floor_y: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let runtime_model = player_pose.model();
    mr::draw_actor_projected_shadow(
        PROJECTED_SHADOW_TUNING,
        model_scratch_arena(),
        runtime_model,
        player_pose.pose(),
        floor_y,
        runtime_model.material,
        camera,
        options,
        model_faces,
        model_parts,
        model_vertices,
        triangles,
        world,
    );
}

/// Draw the placed model instances of `current_room` flattened onto their
/// floor planes.
#[cfg(feature = "actor-shadows-projected")]
pub(super) fn draw_model_instance_projected_shadows(
    current_room: RoomIndex,
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    // psx-numeric-allow-next-line: one bit per model instance; the width IS the instance capacity
    visible_instance_mask: u64,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_model_instance_projected_shadows(
        model_tables(),
        MODEL_DRAW_KNOBS,
        PROJECTED_SHADOW_TUNING,
        model_scratch_arena(),
        current_room,
        instance_poses,
        camera,
        options,
        visible_instance_mask,
        model_faces,
        model_parts,
        model_vertices,
        triangles,
        world,
    );
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
