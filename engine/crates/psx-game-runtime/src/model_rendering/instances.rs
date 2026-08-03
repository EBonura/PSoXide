use super::*;
use crate::vram::SHADOW_TEXEL_U;
use psx_gpu::draw_line_mono;

/// Shadow decals share the shadow/particle 4bpp page allocated by the unified
/// VRAM allocator. UVs are page-relative, so only the page base moves; the
/// texel origin is the crate vram module's placement contract.
const SHADOW_UV_MAX: u8 = SHADOW_TEXEL_U + 63;
const COLLISION_DEBUG_SEGMENTS: usize = 8;
const COLLISION_DEBUG_FLOOR_LIFT: i32 = 8;

/// Animate + render placed model instances whose owning room matches
/// `current_room`. Meshes, clips, and atlas materials are resolved by
/// `load_runtime_models` once at init; the frame path only chooses
/// phase + transform and submits packets.
///
/// Errors (parse failure, missing asset) skip the instance
/// rather than crashing.
#[derive(Copy, Clone, Debug, Default)]
pub struct ModelInstanceDrawStats {
    /// Instances drawn.
    pub draws: u16,
    /// Bounds tests run.
    pub bounds_tests: u16,
    /// Bounds tests that culled the instance.
    pub bounds_culled: u16,
    /// Model submit stats.
    pub stats: TexturedModelRenderStats,
}

/// Depth-pass selector for the two-pass instance draw around the
/// player.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ModelInstanceDepthPass {
    /// Draw every instance.
    All,
    /// Draw instances at or beyond the player's view depth.
    BehindPlayer(i32),
    /// Draw instances nearer than the player's view depth.
    InFrontOfPlayer(i32),
}

/// Live pose override for one cooked model instance: a game entity
/// bound to an instance renders at the entity runtime's position and
/// facing instead of the cooked spawn transform (phase 3 of
/// docs/game-runtime-plan.md), and plays the entity state's clip
/// instead of the cooked instance clip when one is carried. The
/// owning game rebuilds the (tiny) list each frame from its
/// `GameEntities` state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelInstancePoseOverride {
    /// Index into the cooked `MODEL_INSTANCES` table.
    pub instance: u16,
    /// Live position, room-local engine units (floor anchor).
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Z.
    pub z: i32,
    /// Live facing yaw, PSX angle units.
    pub yaw: i16,
    /// Model-local clip driving the instance, or
    /// [`psx_level::OptionalModelClipIndex::NONE`] to keep the cooked
    /// clip on the cooked clock.
    pub clip: psx_level::OptionalModelClipIndex,
    /// 60 Hz ticks into the override clip's playback (state-entry
    /// relative; only read when `clip` is some).
    pub phase_ticks: u16,
    /// One-shot playback: clamp at the clip's final frame instead of
    /// looping.
    pub one_shot: bool,
}

/// Look up the pose override for instance `index`, if any (linear
/// scan; the list is at most the awake-entity count).
fn pose_override_for(
    overrides: &[ModelInstancePoseOverride],
    index: usize,
) -> Option<ModelInstancePoseOverride> {
    let index = u16::try_from(index).ok()?;
    overrides.iter().copied().find(|o| o.instance == index)
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

/// Sum instance draw stats across rooms/passes.
pub fn accumulate_model_instance_draw_stats(
    total: &mut ModelInstanceDrawStats,
    stats: ModelInstanceDrawStats,
) {
    total.draws = total.draws.saturating_add(stats.draws);
    total.bounds_tests = total.bounds_tests.saturating_add(stats.bounds_tests);
    total.bounds_culled = total.bounds_culled.saturating_add(stats.bounds_culled);
    accumulate_model_stats(&mut total.stats, stats.stats);
}

/// Draw the floor shadow decal under every placed model instance of
/// `current_room`.
#[inline]
pub fn draw_model_instance_shadows<const MAX_RUNTIME_MODELS: usize, const OT_DEPTH: usize>(
    tables: ModelTables,
    knobs: ModelDrawKnobs,
    shadow: ShadowTuning,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    pose_overrides: &[ModelInstancePoseOverride],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let mut drawn = 0usize;
    for (index, inst) in tables.model_instances.iter().enumerate() {
        if inst.room != current_room || drawn >= knobs.max_model_instances {
            continue;
        }
        let Some(runtime_model) = models.get(inst.model.to_usize()).copied().flatten() else {
            continue;
        };
        let (x, y, z) = match pose_override_for(pose_overrides, index) {
            Some(live) => (live.x, live.y, live.z),
            None => (inst.x, inst.y, inst.z),
        };

        draw_actor_shadow(
            shadow,
            x,
            y,
            z,
            actor_shadow_radius(shadow, i32::from(runtime_model.collision_radius)),
            camera,
            options,
            material,
            triangles,
            world,
        );
        drawn += 1;
    }
}

/// Draw one actor's circular floor shadow decal.
#[inline]
pub fn draw_actor_shadow<const OT_DEPTH: usize>(
    shadow: ShadowTuning,
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
    let y = floor_y.saturating_add(shadow.floor_lift);
    let h = radius;
    let verts = [
        WorldVertex::new(x.saturating_sub(h), y, z.saturating_sub(h)),
        WorldVertex::new(x.saturating_add(h), y, z.saturating_sub(h)),
        WorldVertex::new(x.saturating_add(h), y, z.saturating_add(h)),
        WorldVertex::new(x.saturating_sub(h), y, z.saturating_add(h)),
    ];
    let shadow_options = options
        .with_depth_policy(DepthPolicy::Nearest)
        .with_depth_bias(shadow.depth_bias.saturating_neg())
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

/// Shadow decal radius for an actor's collision radius.
#[inline]
pub fn actor_shadow_radius(shadow: ShadowTuning, base_radius: i32) -> i32 {
    base_radius
        .saturating_mul(shadow.radius_scale_num)
        .checked_div(shadow.radius_scale_den)
        .unwrap_or(base_radius)
        .clamp(shadow.radius_min, shadow.radius_max)
}

/// Immediate-mode wireframe cylinder for tuning actor blockers.
pub fn draw_collision_cylinder_debug(
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
        if i.is_multiple_of(2) {
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

/// Animate + draw the placed model instances of `current_room` that
/// fall in `depth_pass`.
#[inline]
pub fn draw_model_instances<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
    const BOUNDS_CULL: bool,
    const PROFILE: bool,
>(
    tables: ModelTables,
    knobs: ModelDrawKnobs,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
    current_room: RoomIndex,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    room_reflection_probe: Option<VramSlot>,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    pose_overrides: &[ModelInstancePoseOverride],
    depth_pass: ModelInstanceDepthPass,
    resolve_override_texture: &mut impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    let mut drawn = 0usize;
    let mut out = ModelInstanceDrawStats::default();
    for (instance_index, inst) in tables.model_instances.iter().enumerate() {
        if inst.room != current_room || drawn >= knobs.max_model_instances {
            continue;
        }
        let Some(runtime_model) = models.get(inst.model.to_usize()).copied().flatten() else {
            continue;
        };
        let live = pose_override_for(pose_overrides, instance_index);
        let (inst_x, inst_y, inst_z, inst_yaw) = match live {
            Some(live) => (live.x, live.y, live.z, live.yaw),
            None => (inst.x, inst.y, inst.z, inst.yaw),
        };

        // Clip resolution: live entity state → per-instance override
        // → model default. The cooker validates the cooked paths end
        // up `< clip_count`; a live clip that does not (a mis-cooked
        // record) falls back to the cooked resolution rather than
        // vanishing the instance.
        let live_clip = live
            .and_then(|live| live.clip.to_option())
            .filter(|clip| clip.raw() < runtime_model.clip_count);
        let clip_local = live_clip.unwrap_or(inst.clip.unwrap_or(runtime_model.default_clip));
        let Some(anim) = runtime_model.clip(clips, clip_local) else {
            continue;
        };
        // A frozen instance (pose_frame != ANIMATE) holds one sampled
        // frame: phase = frame << 12 lands exactly on it, with no
        // fractional interpolation. Lets posed props (e.g. corpses) sit
        // on a chosen frame instead of advancing the clip.
        let phase = match (live, live_clip) {
            // A live clip plays on the entity's state clock: looping
            // states wrap, one-shots clamp at the final frame (the
            // same frame math the player's action playback uses).
            (Some(live), Some(_)) => animation_phase_at_tick_q12(
                anim,
                u32::from(live.phase_ticks),
                video_hz,
                !live.one_shot,
                psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8,
                psx_level::CharacterActionFrameRange::FULL,
            ),
            _ => {
                if inst.pose_frame == psx_level::MODEL_INSTANCE_POSE_ANIMATE {
                    anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16())
                } else {
                    (inst.pose_frame.min(anim.frame_count().saturating_sub(1)) as u32) << 12
                }
            }
        };
        let bounds = model_frame_bounds(tables, runtime_model, clip_local, phase);
        let clip_anchor = model_clip_anchor(tables, runtime_model, clip_local);
        let reference_anchor = model_clip_anchor(tables, runtime_model, runtime_model.default_clip);
        let pose_translation =
            model_pose_anchor_translation(anim, phase, clip_anchor, reference_anchor, None);

        // Instance rotation from the authored transform (or the live
        // entity pose). The entity yaw and the renderer's visual yaw
        // share the Y axis; pitch and roll come from the entity
        // transform and compose as `Rz(roll) * Ry(yaw) * Rx(pitch)`
        // (the socket convention). The yaw-only case keeps the
        // cheaper single-axis build.
        let root_yaw = Angle::from_q12(inst_yaw as u16);
        let combined_yaw = root_yaw.add_signed_q12(inst.visual_yaw);
        let model_rotation = if inst.pitch == 0 && inst.roll == 0 {
            yaw_rotation_matrix(combined_yaw)
        } else {
            euler_q12_rotation([inst.pitch, combined_yaw.as_q12() as i16, inst.roll])
        };
        // Authored instance positions are floor anchors; cooked
        // model vertices are centred around their bounds.
        let origin = visual_model_origin(
            inst_x,
            inst_y,
            inst_z,
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
            Some(bounds) if BOUNDS_CULL => model_bounds_visible(
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

        let Some((base_material, cull_mode, uv_mapping)) = model_material_and_cull(
            runtime_model,
            inst.material_override,
            room_reflection_probe,
            elapsed_tick,
            video_hz,
            resolve_override_texture,
        ) else {
            continue;
        };
        let material = lighting.shade_model_material(origin, base_material);
        let model_options = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(cull_mode)
            .with_model_uv_mapping(uv_mapping)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(knobs.texture_split_max_edge);

        telemetry::stage_begin(telemetry::stage::MODEL_DRAW);
        let faces = runtime_model_faces(runtime_model, model_faces);
        let secondary_material = inst
            .material_override
            .and_then(|material| material.secondary_layer)
            .and_then(|layer| {
                model_secondary_layer(
                    layer,
                    elapsed_tick,
                    video_hz,
                    room_reflection_probe,
                    resolve_override_texture,
                )
            })
            .map(|mut layer| {
                layer.material = lighting.shade_model_material(origin, layer.material);
                layer
            });
        let stats = submit_runtime_model_predecoded(
            world,
            triangles,
            runtime_model,
            anim,
            phase,
            None,
            *camera,
            origin,
            model_rotation,
            local_to_world,
            pose_translation,
            material,
            secondary_material,
            model_options,
            faces,
            model_parts,
            model_vertices,
            PROFILE,
            scratch,
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
