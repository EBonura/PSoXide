use super::*;

/// Animate + render placed model instances whose owning room matches
/// `current_room`. Meshes, clips, and atlas materials are resolved by
/// `load_runtime_models` once at init; the frame path only chooses
/// phase + transform and submits packets.
///
/// Errors (parse failure, missing asset) skip the instance
/// rather than crashing.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct ModelInstanceDrawStats {
    pub(crate) draws: u16,
    pub(crate) bounds_tests: u16,
    pub(crate) bounds_culled: u16,
    pub(crate) stats: TexturedModelRenderStats,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModelInstanceDepthPass {
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

pub(crate) fn accumulate_model_instance_draw_stats(
    total: &mut ModelInstanceDrawStats,
    stats: ModelInstanceDrawStats,
) {
    total.draws = total.draws.saturating_add(stats.draws);
    total.bounds_tests = total.bounds_tests.saturating_add(stats.bounds_tests);
    total.bounds_culled = total.bounds_culled.saturating_add(stats.bounds_culled);
    accumulate_model_stats(&mut total.stats, stats.stats);
}

pub(crate) fn draw_model_instance_shadows(
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

pub(crate) fn draw_actor_shadow(
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

pub(crate) fn actor_shadow_radius(base_radius: i32) -> i32 {
    base_radius
        .saturating_mul(SHADOW_RADIUS_SCALE_NUM)
        .checked_div(SHADOW_RADIUS_SCALE_DEN)
        .unwrap_or(base_radius)
        .clamp(SHADOW_RADIUS_MIN, SHADOW_RADIUS_MAX)
}

pub(crate) fn draw_collision_cylinder_debug(
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

pub(crate) fn draw_model_instances(
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

        // Instance rotation from the authored transform. The entity
        // yaw and the renderer's visual yaw share the Y axis; pitch
        // and roll come from the entity transform and compose as
        // `Rz(roll) * Ry(yaw) * Rx(pitch)` (the socket convention).
        // The yaw-only case keeps the cheaper single-axis build.
        let root_yaw = Angle::from_q12(inst.yaw as u16);
        let combined_yaw = root_yaw.add_signed_q12(inst.visual_yaw);
        let model_rotation = if inst.pitch == 0 && inst.roll == 0 {
            yaw_rotation_matrix(combined_yaw)
        } else {
            euler_q12_rotation([inst.pitch, combined_yaw.as_q12() as i16, inst.roll])
        };
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
        let cull_mode = if runtime_model.double_sided {
            CullMode::None
        } else {
            CullMode::Back
        };
        let model_options = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(cull_mode)
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
