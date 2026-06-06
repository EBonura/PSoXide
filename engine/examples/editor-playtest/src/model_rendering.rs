use super::*;

/// Parsed, VRAM-bound model payload ready for the hot render path.
#[derive(Copy, Clone)]
pub(super) struct RuntimeModelAsset {
    pub(super) index: ModelIndex,
    pub(super) model: Model<'static>,
    pub(super) material: TextureMaterial,
    pub(super) clip_first: ModelClipTableIndex,
    pub(super) clip_count: u16,
    pub(super) default_clip: ModelClipIndex,
    pub(super) socket_first: ModelSocketIndex,
    pub(super) socket_count: u16,
    pub(super) face_first: u16,
    pub(super) face_count: u16,
    pub(super) part_first: u16,
    pub(super) part_count: u16,
    pub(super) vertex_first: u16,
    pub(super) vertex_count: u16,
    pub(super) requires_cpu_blend: bool,
    pub(super) world_height: u16,
    pub(super) collision_radius: u16,
    pub(super) local_to_world: LocalToWorldScale,
}

impl RuntimeModelAsset {
    fn from_record(
        index: ModelIndex,
        record: &LevelModelRecord,
        face_pool: &mut [TexturedModelRenderFace],
        face_cursor: &mut usize,
        part_pool: &mut [ModelPart],
        part_cursor: &mut usize,
        vertex_pool: &mut [ModelVertex],
        vertex_cursor: &mut usize,
    ) -> Option<Self> {
        let mesh_asset = find_asset_of_kind(ASSETS, record.mesh_asset, AssetKind::ModelMesh)?;
        let model = Model::from_bytes(mesh_asset.bytes).ok()?;
        let texture_asset = record.texture_asset?;
        let atlas_asset = find_asset_of_kind(ASSETS, texture_asset, AssetKind::Texture)?;
        let atlas_slot = ensure_model_atlas_uploaded(atlas_asset.id, atlas_asset.bytes)?;
        let mut next_face_cursor = *face_cursor;
        let face_first = next_face_cursor;
        let face_count = decode_model_render_faces(
            model,
            atlas_slot.texture_width,
            atlas_slot.texture_height,
            face_pool,
            &mut next_face_cursor,
        )?;
        let mut next_part_cursor = *part_cursor;
        let mut next_vertex_cursor = *vertex_cursor;
        let (part_first, part_count, vertex_first, vertex_count) = decode_model_render_geometry(
            model,
            part_pool,
            &mut next_part_cursor,
            vertex_pool,
            &mut next_vertex_cursor,
        )?;
        *face_cursor = next_face_cursor;
        *part_cursor = next_part_cursor;
        *vertex_cursor = next_vertex_cursor;
        Some(Self {
            index,
            model,
            material: TextureMaterial::opaque(
                atlas_slot.clut_word,
                atlas_slot.tpage_word,
                (0x80, 0x80, 0x80),
            ),
            clip_first: record.clip_first,
            clip_count: record.clip_count,
            default_clip: record.default_clip,
            socket_first: record.socket_first,
            socket_count: record.socket_count,
            face_first: face_first as u16,
            face_count: face_count as u16,
            part_first: part_first as u16,
            part_count: part_count as u16,
            vertex_first: vertex_first as u16,
            vertex_count: vertex_count as u16,
            requires_cpu_blend: model_requires_cpu_blend(model),
            world_height: record.world_height,
            collision_radius: record.collision_radius,
            local_to_world: LocalToWorldScale::from_q12(model.local_to_world_q12()),
        })
    }

    fn clip(
        self,
        clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
        local_clip: ModelClipIndex,
    ) -> Option<Animation<'static>> {
        let index = self.clip_table_index(local_clip)?.to_usize();
        clips.get(index).copied().flatten()
    }

    fn clip_table_index(self, local_clip: ModelClipIndex) -> Option<ModelClipTableIndex> {
        if local_clip.raw() >= self.clip_count {
            return None;
        }
        Some(ModelClipTableIndex(
            self.clip_first.raw().saturating_add(local_clip.raw()),
        ))
    }
}

fn model_requires_cpu_blend(model: Model<'_>) -> bool {
    let joint_count = model.joint_count() as usize;
    let mut i = 0u16;
    while i < model.vertex_count() {
        if let Some(vertex) = model.vertex(i) {
            if vertex.is_blend() && (vertex.joint1 as usize) < joint_count {
                return true;
            }
        }
        i = i.saturating_add(1);
    }
    false
}

fn decode_model_render_faces(
    model: Model<'_>,
    texture_width: u16,
    texture_height: u16,
    face_pool: &mut [TexturedModelRenderFace],
    face_cursor: &mut usize,
) -> Option<usize> {
    let face_count = model.face_count() as usize;
    if face_count > u16::MAX as usize || face_pool.len().saturating_sub(*face_cursor) < face_count {
        return None;
    }

    let (max_u, max_v) = model_render_uv_limits(texture_width, texture_height);
    let mut i = 0usize;
    while i < face_count {
        let face = model.face(i as u16)?;
        face_pool[*face_cursor + i] = TexturedModelRenderFace::new(
            [
                face.corners[0].vertex_index,
                face.corners[1].vertex_index,
                face.corners[2].vertex_index,
            ],
            [
                clamp_model_render_uv(face.corners[0].uv, max_u, max_v),
                clamp_model_render_uv(face.corners[1].uv, max_u, max_v),
                clamp_model_render_uv(face.corners[2].uv, max_u, max_v),
            ],
        );
        i += 1;
    }
    *face_cursor += face_count;
    Some(face_count)
}

fn decode_model_render_geometry(
    model: Model<'_>,
    part_pool: &mut [ModelPart],
    part_cursor: &mut usize,
    vertex_pool: &mut [ModelVertex],
    vertex_cursor: &mut usize,
) -> Option<(usize, usize, usize, usize)> {
    let part_count = model.part_count() as usize;
    let vertex_count = model.vertex_count() as usize;
    if part_count > u16::MAX as usize
        || vertex_count > u16::MAX as usize
        || part_pool.len().saturating_sub(*part_cursor) < part_count
        || vertex_pool.len().saturating_sub(*vertex_cursor) < vertex_count
    {
        return None;
    }

    let part_first = *part_cursor;
    let vertex_first = *vertex_cursor;
    let mut i = 0usize;
    while i < part_count {
        part_pool[part_first + i] = model.part(i as u16)?;
        i += 1;
    }
    i = 0;
    while i < vertex_count {
        vertex_pool[vertex_first + i] = model.vertex(i as u16)?;
        i += 1;
    }
    *part_cursor += part_count;
    *vertex_cursor += vertex_count;
    Some((part_first, part_count, vertex_first, vertex_count))
}

fn model_render_uv_limits(texture_width: u16, texture_height: u16) -> (u8, u8) {
    (
        model_render_uv_max(texture_width),
        model_render_uv_max(texture_height),
    )
}

pub(super) fn model_render_uv_max(size: u16) -> u8 {
    size.saturating_sub(1).min(u16::from(u8::MAX)) as u8
}

fn clamp_model_render_uv(uv: (u8, u8), max_u: u8, max_v: u8) -> (u8, u8) {
    (uv.0.min(max_u), uv.1.min(max_v))
}

fn runtime_model_faces<'a>(
    model: RuntimeModelAsset,
    face_pool: &'a [TexturedModelRenderFace],
) -> &'a [TexturedModelRenderFace] {
    let first = model.face_first as usize;
    let count = model.face_count as usize;
    let end = first.saturating_add(count).min(face_pool.len());
    if first >= end || first >= face_pool.len() {
        &[]
    } else {
        &face_pool[first..end]
    }
}

fn runtime_model_geometry<'a>(
    model: RuntimeModelAsset,
    part_pool: &'a [ModelPart],
    vertex_pool: &'a [ModelVertex],
) -> Option<TexturedModelGeometry<'a>> {
    let part_first = model.part_first as usize;
    let part_count = model.part_count as usize;
    let vertex_first = model.vertex_first as usize;
    let vertex_count = model.vertex_count as usize;
    if part_count == 0 || vertex_count == 0 {
        return None;
    }
    let part_end = part_first.checked_add(part_count)?;
    let vertex_end = vertex_first.checked_add(vertex_count)?;
    let parts = part_pool.get(part_first..part_end)?;
    let vertices = vertex_pool.get(vertex_first..vertex_end)?;
    Some(TexturedModelGeometry::new(parts, vertices))
}

impl Playtest {
    pub(super) fn player_clip_duration_vblanks(
        &self,
        character: RuntimeCharacter,
        clip: ModelClipIndex,
        video_hz: VideoHz,
    ) -> Option<u32> {
        let runtime_model = self
            .models
            .get(character.model.to_usize())
            .copied()
            .flatten()?;
        let animation = runtime_model.clip(&self.clips, clip)?;
        let sample_rate = animation.sample_rate_hz().max(1) as u32;
        let frames = animation.frame_count().max(1) as u32;
        Some(
            frames
                .saturating_mul(video_hz.as_nonzero_u32())
                .div_ceil(sample_rate),
        )
    }

    pub(super) fn load_runtime_models(&mut self) {
        let mut i = 0;
        while i < MAX_RUNTIME_MODELS {
            self.models[i] = None;
            i += 1;
        }
        i = 0;
        while i < MAX_RUNTIME_MODEL_CLIPS {
            self.clips[i] = None;
            i += 1;
        }
        self.model_face_count = 0;
        self.model_part_count = 0;
        self.model_vertex_count = 0;

        for (index, clip) in MODEL_CLIPS.iter().enumerate() {
            if index >= MAX_RUNTIME_MODEL_CLIPS {
                break;
            }
            let Some(asset) =
                find_asset_of_kind(ASSETS, clip.animation_asset, AssetKind::ModelAnimation)
            else {
                continue;
            };
            self.clips[index] = Animation::from_bytes(asset.bytes).ok();
        }

        for (index, record) in MODELS.iter().enumerate() {
            if index >= MAX_RUNTIME_MODELS {
                break;
            }
            self.models[index] = RuntimeModelAsset::from_record(
                ModelIndex(index as u16),
                record,
                &mut self.model_faces,
                &mut self.model_face_count,
                &mut self.model_parts,
                &mut self.model_part_count,
                &mut self.model_vertices,
                &mut self.model_vertex_count,
            );
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub(super) struct PlayerModelDrawStats {
    pub(super) stats: TexturedModelRenderStats,
    pub(super) bounds_tests: u16,
    pub(super) bounds_culled: u16,
}

pub(super) fn draw_player(
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
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> PlayerModelDrawStats {
    let Some(runtime_model) = models.get(character.model.to_usize()).copied().flatten() else {
        return PlayerModelDrawStats::default();
    };

    let Some(anim) = runtime_model.clip(clips, clip_local) else {
        return PlayerModelDrawStats::default();
    };
    // Phase the animation relative to the clip-start tick so
    // state changes don't pop into the middle of a new clip.
    let local_tick = elapsed_tick.saturating_sub(anim_start_tick);
    let phase = animation_phase_at_tick_q12(
        anim,
        local_tick,
        video_hz,
        character.action_loops(anim_action),
    );
    let bounds = model_frame_bounds(runtime_model, clip_local, phase);
    let clip_anchor = model_clip_anchor(runtime_model, clip_local);
    let reference_anchor = model_clip_anchor(runtime_model, character.clip_for(PlayerAnim::Idle));
    let pose_translation = model_pose_anchor_translation(
        anim,
        phase,
        clip_anchor,
        reference_anchor,
        character.action_in_place_override(anim_action),
    );

    let model_rotation = yaw_rotation_matrix(yaw.add_signed_q12(character.visual_yaw));
    let origin = visual_model_origin(
        x,
        y,
        z,
        runtime_model.world_height,
        character.visual_offset,
        character.visual_scale_q8,
        &model_rotation,
    );
    let local_to_world = visual_model_local_to_world(runtime_model, character.visual_scale_q8);
    let bounds_origin =
        model_pose_translated_origin(origin, model_rotation, local_to_world, pose_translation);
    telemetry::stage_begin(telemetry::stage::PLAYER_BOUNDS);
    let visible = match bounds {
        Some(bounds) if MODEL_BOUNDS_CULLING_ENABLED => model_bounds_visible(
            camera,
            options,
            bounds_origin,
            model_rotation,
            bounds,
            character.visual_scale_q8,
        ),
        _ => true,
    };
    telemetry::stage_end(telemetry::stage::PLAYER_BOUNDS);
    if !visible {
        return PlayerModelDrawStats {
            stats: TexturedModelRenderStats::default(),
            bounds_tests: 1,
            bounds_culled: 1,
        };
    }

    let material = lighting.shade_model_material(origin, runtime_model.material);
    let model_options = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::Back)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(MODEL_TEXTURE_SPLIT_MAX_EDGE);

    telemetry::stage_begin(telemetry::stage::PLAYER_DRAW);
    let faces = runtime_model_faces(runtime_model, model_faces);
    let stats = submit_runtime_model_predecoded(
        world,
        triangles,
        runtime_model,
        anim,
        phase,
        *camera,
        origin,
        model_rotation,
        local_to_world,
        pose_translation,
        material,
        model_options,
        faces,
        model_parts,
        model_vertices,
    );
    telemetry::stage_end(telemetry::stage::PLAYER_DRAW);
    PlayerModelDrawStats {
        stats,
        bounds_tests: 1,
        bounds_culled: 0,
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn submit_runtime_model_predecoded(
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    runtime_model: RuntimeModelAsset,
    anim: Animation<'static>,
    phase: u32,
    camera: WorldCamera,
    origin: WorldVertex,
    rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
    material: TextureMaterial,
    options: WorldSurfaceOptions,
    faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
) -> TexturedModelRenderStats {
    let start_cycles = if MODEL_PROFILE_ENABLED {
        telemetry::cycle_counter()
    } else {
        0
    };
    let Some(geometry) = runtime_model_geometry(runtime_model, model_parts, model_vertices) else {
        let mut stats = TexturedModelRenderStats::default();
        stats.vertex_overflow = true;
        return stats;
    };
    let stats = if runtime_model.requires_cpu_blend {
        world.submit_textured_model_predecoded_geometry_faces(
            triangles,
            runtime_model.model,
            anim,
            phase,
            camera,
            origin,
            rotation,
            local_to_world,
            pose_translation,
            unsafe { &mut MODEL_VERTICES },
            unsafe { &mut JOINT_VIEW_TRANSFORMS },
            material,
            options,
            faces,
            geometry,
        )
    } else {
        world.submit_textured_model_primary_joints_predecoded_geometry_faces(
            triangles,
            runtime_model.model,
            anim,
            phase,
            camera,
            origin,
            rotation,
            local_to_world,
            pose_translation,
            unsafe { &mut MODEL_VERTICES },
            unsafe { &mut JOINT_VIEW_TRANSFORMS },
            material,
            options,
            faces,
            geometry,
        )
    };
    if MODEL_PROFILE_ENABLED {
        emit_runtime_model_profile(runtime_model.index, start_cycles);
    }
    stats
}

fn emit_runtime_model_profile(index: ModelIndex, start_cycles: u32) {
    let Some(cycle_counter) = runtime_model_profile_cycle_counter(index) else {
        return;
    };
    let draw_counter = telemetry::counter::MODEL_PROFILE_DRAWS_0.saturating_add(index.raw().min(7));
    telemetry::counter(draw_counter, 1);
    telemetry::counter(
        cycle_counter,
        telemetry::cycle_counter().wrapping_sub(start_cycles),
    );
}

fn runtime_model_profile_cycle_counter(index: ModelIndex) -> Option<u16> {
    match index.raw() {
        0 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_0),
        1 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_1),
        2 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_2),
        3 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_3),
        4 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_4),
        5 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_5),
        6 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_6),
        7 => Some(telemetry::counter::MODEL_PROFILE_CYCLES_7),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub(super) struct EquipmentDrawStats {
    pub(super) draws: u16,
    pub(super) active_hitboxes: u16,
    pub(super) target_hits: u16,
    pub(super) stats: TexturedModelRenderStats,
}

#[derive(Copy, Clone)]
struct AttachmentPose {
    origin: WorldVertex,
    rotation: Mat3I16,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_player_equipment(
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
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let Some(character_model) = models.get(character.model.to_usize()).copied().flatten() else {
        return out;
    };
    let Some(character_anim) = character_model.clip(clips, clip_local) else {
        return out;
    };
    let local_tick = elapsed_tick.saturating_sub(anim_start_tick);
    let character_phase = animation_phase_at_tick_q12(
        character_anim,
        local_tick,
        video_hz,
        character.action_loops(anim_action),
    );
    let character_anchor = model_clip_anchor(character_model, clip_local);
    let reference_anchor = model_clip_anchor(character_model, character.clip_for(PlayerAnim::Idle));
    let character_pose_translation = model_pose_anchor_translation(
        character_anim,
        character_phase,
        character_anchor,
        reference_anchor,
        character.action_in_place_override(anim_action),
    );
    let character_frame = (character_phase >> 12) as u16;
    let character_model_rotation = yaw_rotation_matrix(yaw.add_signed_q12(character.visual_yaw));
    let character_origin = visual_model_origin(
        x,
        y,
        z,
        character_model.world_height,
        character.visual_offset,
        character.visual_scale_q8,
        &character_model_rotation,
    );
    let character_local_to_world =
        visual_model_local_to_world(character_model, character.visual_scale_q8);

    let mut drawn = 0usize;
    for equipment in EQUIPMENT {
        if equipment.room != current_room
            || equipment.flags & equipment_flags::PLAYER == 0
            || drawn >= MAX_EQUIPMENT_DRAWS
        {
            continue;
        }
        let Some(weapon) = WEAPONS.get(equipment.weapon.to_usize()) else {
            continue;
        };
        let Some(socket) = find_model_socket(character_model, equipment.character_socket)
            .or_else(|| find_model_socket(character_model, weapon.default_character_socket))
        else {
            continue;
        };
        let Some(socket_pose) = attachment_socket_pose(
            character_model,
            character_anim,
            character_phase,
            character_origin,
            character_model_rotation,
            character_local_to_world,
            character_pose_translation,
            socket,
        ) else {
            continue;
        };
        let weapon_rotation = socket_pose
            .rotation
            .mul(&euler_q12_rotation_inverse(weapon.grip_rotation_q12));

        match weapon.model {
            Some(model_index) => {
                let Some(weapon_model) = models.get(model_index.to_usize()).copied().flatten()
                else {
                    continue;
                };
                let grip = scaled_offset(weapon_model.local_to_world, weapon.grip_translation);
                let grip_world = rotate_offset_q12(&weapon_rotation, grip);
                let origin = WorldVertex::new(
                    socket_pose.origin.x.saturating_sub(grip_world[0]),
                    socket_pose.origin.y.saturating_sub(grip_world[1]),
                    socket_pose.origin.z.saturating_sub(grip_world[2]),
                );
                if let Some(anim) = weapon_model.clip(clips, weapon_model.default_clip) {
                    let phase = anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16());
                    let material = lighting.shade_model_material(origin, weapon_model.material);
                    let model_options = options
                        .with_depth_policy(DepthPolicy::Average)
                        .with_cull_mode(CullMode::Back)
                        .with_material_layer(material)
                        .with_textured_triangle_splitting(true)
                        .with_textured_triangle_max_edge(MODEL_TEXTURE_SPLIT_MAX_EDGE);
                    let faces = runtime_model_faces(weapon_model, model_faces);
                    let stats = submit_runtime_model_predecoded(
                        world,
                        triangles,
                        weapon_model,
                        anim,
                        phase,
                        *camera,
                        origin,
                        weapon_rotation,
                        weapon_model.local_to_world,
                        ModelPoseTranslation::ZERO,
                        material,
                        model_options,
                        faces,
                        model_parts,
                        model_vertices,
                    );
                    accumulate_model_stats(&mut out.stats, stats);
                    if stats.primitive_overflow || stats.command_overflow {
                        out.draws = drawn as u16;
                        return out;
                    }
                    drawn += 1;
                    out.draws = drawn as u16;
                }
            }
            None => {}
        };

        let (active, hits) = evaluate_weapon_hitboxes(
            current_room,
            weapon.hitbox_first.to_usize(),
            weapon.hitbox_count,
            character_frame,
            socket_pose.origin,
            socket_pose.rotation,
        );
        out.active_hitboxes = out.active_hitboxes.saturating_add(active);
        out.target_hits = out.target_hits.saturating_add(hits);
    }
    out
}

fn find_model_socket(
    model: RuntimeModelAsset,
    name: &str,
) -> Option<&'static LevelModelSocketRecord> {
    let first = model.socket_first.to_usize();
    let count = model.socket_count as usize;
    let sockets = MODEL_SOCKETS.get(first..first.saturating_add(count))?;
    sockets.iter().find(|socket| socket.name == name)
}

fn attachment_socket_pose(
    _model: RuntimeModelAsset,
    animation: Animation<'static>,
    phase_q12: u32,
    origin: WorldVertex,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
    socket: &LevelModelSocketRecord,
) -> Option<AttachmentPose> {
    let pose = apply_model_pose_translation(
        animation.pose_looped_q12(phase_q12, socket.joint)?,
        pose_translation,
    );
    let joint = compute_joint_world_transform(pose, instance_rotation, local_to_world, origin);
    Some(compose_socket_pose(
        joint,
        socket.translation,
        socket.rotation_q12,
    ))
}

fn compose_socket_pose(
    joint: JointWorldTransform,
    translation: [i32; 3],
    rotation_q12: [i16; 3],
) -> AttachmentPose {
    let offset = rotate_offset_q12(&joint.rotation, translation);
    let local_rotation = euler_q12_rotation(rotation_q12);
    AttachmentPose {
        origin: WorldVertex::new(
            joint.translation.x.saturating_add(offset[0]),
            joint.translation.y.saturating_add(offset[1]),
            joint.translation.z.saturating_add(offset[2]),
        ),
        rotation: joint.rotation.mul(&local_rotation),
    }
}

fn evaluate_weapon_hitboxes(
    current_room: RoomIndex,
    first: usize,
    count: u16,
    frame: u16,
    origin: WorldVertex,
    rotation: Mat3I16,
) -> (u16, u16) {
    let mut active = 0u16;
    let mut hits = 0u16;
    let Some(hitboxes) = WEAPON_HITBOXES.get(first..first.saturating_add(count as usize)) else {
        return (0, 0);
    };
    for hitbox in hitboxes {
        if frame < hitbox.active_start_frame || frame > hitbox.active_end_frame {
            continue;
        }
        active = active.saturating_add(1);
        for entity in ENTITIES {
            if entity.room != current_room {
                continue;
            }
            if weapon_hit_shape_hits_point(hitbox.shape, origin, rotation, entity.x, entity.z) {
                hits = hits.saturating_add(1);
            }
        }
    }
    (active, hits)
}

fn weapon_hit_shape_hits_point(
    shape: WeaponHitShapeRecord,
    origin: WorldVertex,
    rotation: Mat3I16,
    px: i32,
    pz: i32,
) -> bool {
    match shape {
        WeaponHitShapeRecord::Box {
            center,
            half_extents,
        } => {
            let c = transform_local_point(origin, rotation, center);
            let radius = half_extents[0].max(half_extents[2]) as i32;
            distance_xz_sq(RoomPoint::new(px, 0, pz), RoomPoint::new(c.x, 0, c.z))
                <= square_i32_saturating(radius)
        }
        WeaponHitShapeRecord::Capsule { start, end, radius } => {
            let a = transform_local_point(origin, rotation, start);
            let b = transform_local_point(origin, rotation, end);
            point_segment_xz_distance_sq(px, pz, a.x, a.z, b.x, b.z)
                <= square_i32_saturating(radius as i32)
        }
    }
}

fn transform_local_point(origin: WorldVertex, rotation: Mat3I16, point: [i32; 3]) -> WorldVertex {
    let offset = rotate_offset_q12(&rotation, point);
    WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn scaled_offset(scale: LocalToWorldScale, offset: [i32; 3]) -> [i32; 3] {
    [
        scale.apply(offset[0]),
        scale.apply(offset[1]),
        scale.apply(offset[2]),
    ]
}

fn rotate_offset_q12(rotation: &Mat3I16, offset: [i32; 3]) -> [i32; 3] {
    let row = |r: [i16; 3]| -> i32 {
        let x = (r[0] as i32).saturating_mul(offset[0]);
        let y = (r[1] as i32).saturating_mul(offset[1]);
        let z = (r[2] as i32).saturating_mul(offset[2]);
        x.saturating_add(y).saturating_add(z) >> 12
    };
    [row(rotation.m[0]), row(rotation.m[1]), row(rotation.m[2])]
}

fn euler_q12_rotation(rotation_q12: [i16; 3]) -> Mat3I16 {
    let rx = Mat3I16::rotate_x(Angle::from_q12(rotation_q12[0] as u16).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(rotation_q12[1] as u16).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(rotation_q12[2] as u16).rotate_y_arg());
    rz.mul(&ry).mul(&rx)
}

fn euler_q12_rotation_inverse(rotation_q12: [i16; 3]) -> Mat3I16 {
    let inv_x = (-(rotation_q12[0] as i32)) as u16;
    let inv_y = (-(rotation_q12[1] as i32)) as u16;
    let inv_z = (-(rotation_q12[2] as i32)) as u16;
    let rx = Mat3I16::rotate_x(Angle::from_q12(inv_x).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(inv_y).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(inv_z).rotate_y_arg());
    rx.mul(&ry).mul(&rz)
}

fn point_segment_xz_distance_sq(px: i32, pz: i32, ax: i32, az: i32, bx: i32, bz: i32) -> i32 {
    let abx = bx.saturating_sub(ax);
    let abz = bz.saturating_sub(az);
    let apx = px.saturating_sub(ax);
    let apz = pz.saturating_sub(az);
    let denom = square_i32_saturating(abx).saturating_add(square_i32_saturating(abz));
    if denom <= 0 {
        return square_i32_saturating(apx).saturating_add(square_i32_saturating(apz));
    }
    let dot = apx
        .saturating_mul(abx)
        .saturating_add(apz.saturating_mul(abz));
    let t_q8 = ratio_q8_i32(dot.clamp(0, denom), denom);
    let cx = ax.saturating_add((abx.saturating_mul(t_q8)) >> 8);
    let cz = az.saturating_add((abz.saturating_mul(t_q8)) >> 8);
    square_i32_saturating(px.saturating_sub(cx))
        .saturating_add(square_i32_saturating(pz.saturating_sub(cz)))
}

pub(super) fn emit_model_counters(
    stats: TexturedModelRenderStats,
    projected_counter: u16,
    submitted_counter: u16,
    culled_counter: u16,
    dropped_counter: u16,
) {
    telemetry::counter(projected_counter, stats.projected_vertices as u32);
    telemetry::counter(submitted_counter, stats.submitted_triangles as u32);
    telemetry::counter(culled_counter, stats.culled_triangles as u32);
    telemetry::counter(dropped_counter, stats.dropped_triangles as u32);

    let mut overflow = 0u32;
    if stats.vertex_overflow {
        overflow |= 1;
    }
    if stats.primitive_overflow {
        overflow |= 1 << 1;
    }
    if stats.command_overflow {
        overflow |= 1 << 2;
    }
    if overflow != 0 {
        telemetry::counter(telemetry::counter::MODEL_OVERFLOW_FLAGS, overflow);
    }
}

pub(super) fn player_actor_depth_for_room(
    active: ActiveRuntimeRoom,
    character: Option<RuntimeCharacter>,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    player: RoomPoint,
    camera: &WorldCamera,
) -> Option<i32> {
    let character = character?;
    let runtime_model = models.get(character.model.to_usize()).copied().flatten()?;
    let origin = floor_anchored_model_origin(
        player.x.saturating_sub(active.offset_x),
        player.y,
        player.z.saturating_sub(active.offset_z),
        runtime_model.world_height,
    );
    Some(camera.view_vertex(origin).z)
}

/// Animate + render placed model instances whose owning room matches
/// `current_room`. Meshes, clips, and atlas materials are resolved by
/// `load_runtime_models` once at init; the frame path only chooses
/// phase + transform and submits packets.
///
/// Errors (parse failure, missing asset) skip the instance
/// rather than crashing.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct ModelInstanceDrawStats {
    pub(super) draws: u16,
    pub(super) bounds_tests: u16,
    pub(super) bounds_culled: u16,
    pub(super) stats: TexturedModelRenderStats,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ModelInstanceDepthPass {
    All,
    BehindPlayer(i32),
    InFrontOfPlayer(i32),
}

impl ModelInstanceDepthPass {
    fn includes(self, depth: i32) -> bool {
        match self {
            Self::All => true,
            Self::BehindPlayer(player_depth) => depth >= player_depth,
            Self::InFrontOfPlayer(player_depth) => depth < player_depth,
        }
    }
}

pub(super) fn accumulate_model_instance_draw_stats(
    total: &mut ModelInstanceDrawStats,
    stats: ModelInstanceDrawStats,
) {
    total.draws = total.draws.saturating_add(stats.draws);
    total.bounds_tests = total.bounds_tests.saturating_add(stats.bounds_tests);
    total.bounds_culled = total.bounds_culled.saturating_add(stats.bounds_culled);
    accumulate_model_stats(&mut total.stats, stats.stats);
}

pub(super) fn draw_model_instance_shadows(
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let mut drawn = 0usize;
    for inst in MODEL_INSTANCES {
        if inst.room != current_room || drawn >= MAX_MODEL_INSTANCES {
            continue;
        }
        let Some(runtime_model) = models.get(inst.model.to_usize()).copied().flatten() else {
            continue;
        };

        draw_actor_shadow(
            inst.x,
            inst.y,
            inst.z,
            actor_shadow_radius(i32::from(runtime_model.collision_radius)),
            camera,
            options,
            material,
            triangles,
            world,
        );
        drawn += 1;
    }
}

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
    if radius <= 0 {
        return;
    }
    let y = floor_y.saturating_add(SHADOW_FLOOR_LIFT);
    let h = radius;
    let verts = [
        WorldVertex::new(x.saturating_sub(h), y, z.saturating_sub(h)),
        WorldVertex::new(x.saturating_add(h), y, z.saturating_sub(h)),
        WorldVertex::new(x.saturating_add(h), y, z.saturating_add(h)),
        WorldVertex::new(x.saturating_sub(h), y, z.saturating_add(h)),
    ];
    let shadow_options = options
        .with_depth_policy(DepthPolicy::Nearest)
        .with_depth_bias(SHADOW_DEPTH_BIAS.saturating_neg())
        .with_cull_mode(CullMode::None)
        .with_material_layer(material);
    const UVS: [(u8, u8); 4] = [
        (SHADOW_TEXEL_U, 0),
        (SHADOW_UV_MAX, 0),
        (SHADOW_UV_MAX, 63),
        (SHADOW_TEXEL_U, 63),
    ];
    let _ =
        world.submit_textured_world_quad(triangles, *camera, verts, UVS, material, shadow_options);
}

pub(super) fn actor_shadow_radius(base_radius: i32) -> i32 {
    base_radius
        .saturating_mul(SHADOW_RADIUS_SCALE_NUM)
        .checked_div(SHADOW_RADIUS_SCALE_DEN)
        .unwrap_or(base_radius)
        .clamp(SHADOW_RADIUS_MIN, SHADOW_RADIUS_MAX)
}

pub(super) fn draw_collision_cylinder_debug(
    position: RoomPoint,
    radius: i32,
    height: i32,
    camera: WorldCamera,
    color: (u8, u8, u8),
) {
    if radius <= 0 || height <= 0 {
        return;
    }

    let bottom_y = position.y.saturating_add(COLLISION_DEBUG_FLOOR_LIFT);
    let top_y = position
        .y
        .saturating_add(height.max(COLLISION_DEBUG_FLOOR_LIFT));
    let mut bottom = [None; COLLISION_DEBUG_SEGMENTS];
    let mut top = [None; COLLISION_DEBUG_SEGMENTS];
    let mut i = 0usize;
    while i < COLLISION_DEBUG_SEGMENTS {
        let (dx, dz) = collision_debug_ring_offset(radius, i);
        let x = position.x.saturating_add(dx);
        let z = position.z.saturating_add(dz);
        bottom[i] = camera
            .project_world(WorldVertex::new(x, bottom_y, z))
            .map(screen_xy);
        top[i] = camera
            .project_world(WorldVertex::new(x, top_y, z))
            .map(screen_xy);
        i += 1;
    }

    i = 0;
    while i < COLLISION_DEBUG_SEGMENTS {
        let next = (i + 1) % COLLISION_DEBUG_SEGMENTS;
        draw_optional_debug_line(bottom[i], bottom[next], color);
        draw_optional_debug_line(top[i], top[next], color);
        if i % 2 == 0 {
            draw_optional_debug_line(bottom[i], top[i], color);
        }
        i += 1;
    }
}

fn collision_debug_ring_offset(radius: i32, index: usize) -> (i32, i32) {
    let diagonal = radius.saturating_mul(181) >> 8;
    match index & 7 {
        0 => (radius, 0),
        1 => (diagonal, diagonal),
        2 => (0, radius),
        3 => (diagonal.saturating_neg(), diagonal),
        4 => (radius.saturating_neg(), 0),
        5 => (diagonal.saturating_neg(), diagonal.saturating_neg()),
        6 => (0, radius.saturating_neg()),
        _ => (diagonal, diagonal.saturating_neg()),
    }
}

fn screen_xy(vertex: ProjectedVertex) -> (i16, i16) {
    (vertex.sx, vertex.sy)
}

fn draw_optional_debug_line(a: Option<(i16, i16)>, b: Option<(i16, i16)>, color: (u8, u8, u8)) {
    let (Some(a), Some(b)) = (a, b) else {
        return;
    };
    draw_line_mono(a.0, a.1, b.0, b.1, color.0, color.1, color.2);
}

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
    depth_pass: ModelInstanceDepthPass,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    let mut drawn = 0usize;
    let mut out = ModelInstanceDrawStats::default();
    for inst in MODEL_INSTANCES {
        if inst.room != current_room || drawn >= MAX_MODEL_INSTANCES {
            continue;
        }
        let Some(runtime_model) = models.get(inst.model.to_usize()).copied().flatten() else {
            continue;
        };

        // Clip resolution: per-instance override → model default.
        // The cooker validates that both end up `< clip_count`,
        // so by the time we get here `clip_local` is in-range.
        let clip_local = inst.clip.unwrap_or(runtime_model.default_clip);
        let Some(anim) = runtime_model.clip(clips, clip_local) else {
            continue;
        };
        // A frozen instance (pose_frame != ANIMATE) holds one sampled
        // frame: phase = frame << 12 lands exactly on it, with no
        // fractional interpolation. Lets posed props (e.g. corpses) sit
        // on a chosen frame instead of advancing the clip.
        let phase = if inst.pose_frame == psx_level::MODEL_INSTANCE_POSE_ANIMATE {
            anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16())
        } else {
            (inst.pose_frame.min(anim.frame_count().saturating_sub(1)) as u32) << 12
        };
        let bounds = model_frame_bounds(runtime_model, clip_local, phase);
        let clip_anchor = model_clip_anchor(runtime_model, clip_local);
        let reference_anchor = model_clip_anchor(runtime_model, runtime_model.default_clip);
        let pose_translation =
            model_pose_anchor_translation(anim, phase, clip_anchor, reference_anchor, None);

        // Instance Y-axis rotation from authored yaw. PSX angle
        // units (4096 per turn) → Q12 sin/cos via the existing
        // GTE shim, then composed into a rotation matrix.
        let root_yaw = Angle::from_q12(inst.yaw as u16);
        let model_rotation = yaw_rotation_matrix(root_yaw.add_signed_q12(inst.visual_yaw));
        // Authored instance positions are floor anchors; cooked
        // model vertices are centred around their bounds.
        let origin = visual_model_origin(
            inst.x,
            inst.y,
            inst.z,
            runtime_model.world_height,
            inst.visual_offset,
            inst.visual_scale_q8,
            &model_rotation,
        );
        let local_to_world = visual_model_local_to_world(runtime_model, inst.visual_scale_q8);
        let bounds_origin =
            model_pose_translated_origin(origin, model_rotation, local_to_world, pose_translation);
        if !depth_pass.includes(camera.view_vertex(origin).z) {
            continue;
        }
        telemetry::stage_begin(telemetry::stage::MODEL_BOUNDS);
        out.bounds_tests = out.bounds_tests.saturating_add(1);
        let visible = match bounds {
            Some(bounds) if MODEL_BOUNDS_CULLING_ENABLED => model_bounds_visible(
                camera,
                options,
                bounds_origin,
                model_rotation,
                bounds,
                inst.visual_scale_q8,
            ),
            None => true,
            _ => true,
        };
        telemetry::stage_end(telemetry::stage::MODEL_BOUNDS);
        if !visible {
            out.bounds_culled = out.bounds_culled.saturating_add(1);
            continue;
        }

        let material = lighting.shade_model_material(origin, runtime_model.material);
        let model_options = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::Back)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(MODEL_TEXTURE_SPLIT_MAX_EDGE);

        telemetry::stage_begin(telemetry::stage::MODEL_DRAW);
        let faces = runtime_model_faces(runtime_model, model_faces);
        let stats = submit_runtime_model_predecoded(
            world,
            triangles,
            runtime_model,
            anim,
            phase,
            *camera,
            origin,
            model_rotation,
            local_to_world,
            pose_translation,
            material,
            model_options,
            faces,
            model_parts,
            model_vertices,
        );
        telemetry::stage_end(telemetry::stage::MODEL_DRAW);
        accumulate_model_stats(&mut out.stats, stats);
        if stats.primitive_overflow || stats.command_overflow {
            out.draws = drawn as u16;
            return out;
        }
        drawn += 1;
        out.draws = drawn as u16;
    }
    out
}

fn accumulate_model_stats(total: &mut TexturedModelRenderStats, next: TexturedModelRenderStats) {
    total.projected_vertices = total
        .projected_vertices
        .saturating_add(next.projected_vertices);
    total.submitted_triangles = total
        .submitted_triangles
        .saturating_add(next.submitted_triangles);
    total.culled_triangles = total.culled_triangles.saturating_add(next.culled_triangles);
    total.split_triangles = total.split_triangles.saturating_add(next.split_triangles);
    total.skipped_triangles = total
        .skipped_triangles
        .saturating_add(next.skipped_triangles);
    total.dropped_triangles = total
        .dropped_triangles
        .saturating_add(next.dropped_triangles);
    total.cpu_blended_vertices = total
        .cpu_blended_vertices
        .saturating_add(next.cpu_blended_vertices);
    total.packed_face_calls = total
        .packed_face_calls
        .saturating_add(next.packed_face_calls);
    total.packed_unclamped_face_calls = total
        .packed_unclamped_face_calls
        .saturating_add(next.packed_unclamped_face_calls);
    total.packed_clamped_face_calls = total
        .packed_clamped_face_calls
        .saturating_add(next.packed_clamped_face_calls);
    total.packed_general_face_calls = total
        .packed_general_face_calls
        .saturating_add(next.packed_general_face_calls);
    total.fallback_face_calls = total
        .fallback_face_calls
        .saturating_add(next.fallback_face_calls);
    total.hw_extent_fallbacks = total
        .hw_extent_fallbacks
        .saturating_add(next.hw_extent_fallbacks);
    total.near_plane_dropped_faces = total
        .near_plane_dropped_faces
        .saturating_add(next.near_plane_dropped_faces);
    total.hw_unsafe_dropped_faces = total
        .hw_unsafe_dropped_faces
        .saturating_add(next.hw_unsafe_dropped_faces);
    total.fast_submitted_triangles = total
        .fast_submitted_triangles
        .saturating_add(next.fast_submitted_triangles);
    total.vertex_overflow |= next.vertex_overflow;
    total.primitive_overflow |= next.primitive_overflow;
    total.command_overflow |= next.command_overflow;
}

/// Rotation matrix around the world Y axis.
fn yaw_rotation_matrix(yaw: Angle) -> Mat3I16 {
    let s = clamp_i16(yaw.sin().raw());
    let c = clamp_i16(yaw.cos().raw());
    Mat3I16 {
        m: [[c, 0, s], [0, 0x1000, 0], [-s, 0, c]],
    }
}

fn visual_model_local_to_world(
    runtime_model: RuntimeModelAsset,
    visual_scale_q8: u16,
) -> LocalToWorldScale {
    let scale_q8 = visual_scale_q8.max(1) as u32;
    let q12 = ((runtime_model.local_to_world.q12() as u32)
        .saturating_mul(scale_q8)
        .saturating_add((MODEL_VISUAL_SCALE_ONE_Q8 / 2) as u32))
        / MODEL_VISUAL_SCALE_ONE_Q8 as u32;
    LocalToWorldScale::from_q12(q12.clamp(1, u16::MAX as u32) as u16)
}

fn visual_model_origin(
    x: i32,
    y: i32,
    z: i32,
    world_height: u16,
    visual_offset: [i16; 3],
    _visual_scale_q8: u16,
    rotation: &Mat3I16,
) -> WorldVertex {
    let origin = floor_anchored_model_origin(x, y, z, world_height);
    let offset = rotate_offset_q12(
        rotation,
        [
            visual_offset[0] as i32,
            visual_offset[1] as i32,
            visual_offset[2] as i32,
        ],
    );
    WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn animation_phase_at_tick_q12(
    animation: Animation<'static>,
    local_tick: u32,
    video_hz: VideoHz,
    looping: bool,
) -> u32 {
    let phase = animation.phase_at_tick_q12(local_tick, video_hz.as_u16());
    if looping {
        return phase;
    }
    let final_unique_frame = animation.frame_count().saturating_sub(2) as u32;
    phase.min(final_unique_frame << 12)
}

fn model_pose_anchor_translation(
    animation: Animation<'static>,
    phase_q12: u32,
    clip_anchor: Option<ModelClipAnchor>,
    reference_anchor: Option<ModelClipAnchor>,
    in_place_override: Option<bool>,
) -> ModelPoseTranslation {
    let Some(clip_anchor) = clip_anchor else {
        return ModelPoseTranslation::ZERO;
    };
    let reference_floor_y = reference_anchor.map(|anchor| anchor.floor_y);
    let in_place = in_place_override.unwrap_or(clip_anchor.in_place);
    let root_translation = if in_place {
        match (
            animation.pose(0, 0),
            animation.pose_looped_q12(phase_q12, 0),
        ) {
            (Some(first_root), Some(current_root)) => [
                first_root
                    .translation
                    .x
                    .saturating_sub(current_root.translation.x),
                0,
                first_root
                    .translation
                    .z
                    .saturating_sub(current_root.translation.z),
            ],
            _ => [0, 0, 0],
        }
    } else {
        [0, 0, 0]
    };
    let floor_y = match reference_floor_y {
        Some(reference_floor_y) => reference_floor_y.saturating_sub(clip_anchor.floor_y),
        None => 0,
    };
    ModelPoseTranslation {
        x: root_translation[0].saturating_add(clip_anchor.pose_offset[0]),
        y: root_translation[1]
            .saturating_add(floor_y)
            .saturating_add(clip_anchor.pose_offset[1]),
        z: root_translation[2].saturating_add(clip_anchor.pose_offset[2]),
    }
}

fn model_pose_translated_origin(
    origin: WorldVertex,
    rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
) -> WorldVertex {
    let scaled = [
        local_to_world.apply(pose_translation.x),
        local_to_world.apply(pose_translation.y),
        local_to_world.apply(pose_translation.z),
    ];
    let offset = rotate_offset_q12(&rotation, scaled);
    WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn floor_anchored_model_origin(x: i32, y: i32, z: i32, world_height: u16) -> WorldVertex {
    WorldVertex::new(
        x,
        y.saturating_add(model_origin_floor_lift(world_height)),
        z,
    )
}

fn model_origin_floor_lift(world_height: u16) -> i32 {
    (world_height as i32) >> 1
}

const MODEL_BOUNDS_SCREEN_MARGIN: i32 = 192;
const MODEL_BOUNDS_RUNTIME_RADIUS_PAD: i32 = 128;

#[derive(Clone, Copy, Default)]
struct ModelClipAnchor {
    floor_y: i32,
    pose_offset: [i32; 3],
    in_place: bool,
}

fn model_frame_bounds(
    runtime_model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
    phase_q12: u32,
) -> Option<LevelModelFrameBoundsRecord> {
    let clip = runtime_model.clip_table_index(clip_local)?;
    let record = MODEL_CLIP_BOUNDS.get(clip.to_usize()).copied()?;
    if record.model != runtime_model.index || record.clip != clip || record.frame_count == 0 {
        return None;
    }
    let frame = ((phase_q12 >> 12) % record.frame_count as u32) as usize;
    MODEL_FRAME_BOUNDS
        .get(record.first_frame.to_usize().saturating_add(frame))
        .copied()
}

fn model_clip_anchor(
    runtime_model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
) -> Option<ModelClipAnchor> {
    let clip = runtime_model.clip_table_index(clip_local)?;
    let record = MODEL_CLIP_BOUNDS.get(clip.to_usize()).copied()?;
    (record.model == runtime_model.index && record.clip == clip).then_some(ModelClipAnchor {
        floor_y: record.floor_y,
        pose_offset: record.pose_offset,
        in_place: (record.flags & model_clip_flags::IN_PLACE) != 0,
    })
}

fn model_bounds_visible(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    origin: WorldVertex,
    rotation: Mat3I16,
    bounds: LevelModelFrameBoundsRecord,
    visual_scale_q8: u16,
) -> bool {
    let center = rotate_bounds_center(
        rotation,
        scaled_bounds_center(bounds.center, visual_scale_q8),
    );
    let radius = scale_model_bounds_radius(bounds.radius, visual_scale_q8);
    sphere_visible_to_camera(
        camera,
        options,
        WorldVertex::new(
            origin.x.saturating_add(center[0]),
            origin.y.saturating_add(center[1]),
            origin.z.saturating_add(center[2]),
        ),
        radius
            .max(0)
            .saturating_add(MODEL_BOUNDS_RUNTIME_RADIUS_PAD),
        MODEL_BOUNDS_SCREEN_MARGIN,
    )
}

fn scaled_bounds_center(center: [i32; 3], visual_scale_q8: u16) -> [i32; 3] {
    [
        scale_q8_i32(center[0], visual_scale_q8),
        scale_q8_i32(center[1], visual_scale_q8),
        scale_q8_i32(center[2], visual_scale_q8),
    ]
}

fn scale_model_bounds_radius(radius: i32, visual_scale_q8: u16) -> i32 {
    scale_q8_i32(radius, visual_scale_q8)
}

fn scale_q8_i32(value: i32, scale_q8: u16) -> i32 {
    let scale = scale_q8.max(1) as i32;
    value.saturating_mul(scale) / MODEL_VISUAL_SCALE_ONE_Q8 as i32
}

fn rotate_bounds_center(rotation: Mat3I16, center: [i32; 3]) -> [i32; 3] {
    [
        dot_bounds_row_q12(rotation.m[0], center),
        dot_bounds_row_q12(rotation.m[1], center),
        dot_bounds_row_q12(rotation.m[2], center),
    ]
}

fn dot_bounds_row_q12(row: [i16; 3], center: [i32; 3]) -> i32 {
    let x = (row[0] as i32).saturating_mul(center[0]);
    let y = (row[1] as i32).saturating_mul(center[1]);
    let z = (row[2] as i32).saturating_mul(center[2]);
    x.saturating_add(y).saturating_add(z) >> 12
}

pub(super) fn sphere_visible_to_camera(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    center: WorldVertex,
    radius: i32,
    screen_margin: i32,
) -> bool {
    let view = camera.view_vertex(center);
    let near = camera.projection.near_z.max(1);
    let far = options.depth_range.far().max(near);
    if view.z < near.saturating_sub(radius) || view.z > far.saturating_add(radius) {
        return false;
    }

    let z = view.z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(screen_margin)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(screen_margin)
        .max(1);
    let projected_x = view.x.abs().saturating_sub(radius).saturating_mul(focal);
    let projected_y = view.y.abs().saturating_sub(radius).saturating_mul(focal);
    projected_x <= half_w.saturating_mul(z) && projected_y <= half_h.saturating_mul(z)
}
