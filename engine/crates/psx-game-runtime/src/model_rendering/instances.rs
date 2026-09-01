use super::*;
use crate::vram::SHADOW_TEXEL_U;
#[cfg(feature = "collision-debug-overlay")]
use psx_gpu::draw_line_mono;

/// Shadow decals share the shadow/particle 4bpp page allocated by the unified
/// VRAM allocator. UVs are page-relative, so only the page base moves; the
/// texel origin is the crate vram module's placement contract.
const SHADOW_UV_MAX: u8 = SHADOW_TEXEL_U + 63;
#[cfg(feature = "collision-debug-overlay")]
const COLLISION_DEBUG_SEGMENTS: usize = 8;
#[cfg(feature = "collision-debug-overlay")]
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
    /// Q8 playback speed carried by the live state selection.
    pub speed_q8: u16,
    /// Inclusive source-frame window carried by the live state selection.
    pub frame_range: psx_level::CharacterActionFrameRange,
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
    // psx-numeric-allow-next-line: one bit per model instance; the width IS the instance capacity
    visible_instance_mask: u64,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let mut drawn = 0usize;
    for (index, inst) in tables.model_instances.iter().enumerate() {
        if index >= 64
            || visible_instance_mask & (1u64 << index) == 0
            || inst.room != current_room
            || drawn >= knobs.max_model_instances
        {
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
    // Keep the actor clearance the caller already applied and add the decal's
    // own nudge on top. The hardcoded `-6` replaced the clearance outright and
    // ignored `ShadowTuning::depth_bias`, which no call site could therefore
    // affect. Measured on the benchmark tape the visible change is small (the
    // decal moves by about one OT slot, ~140 pixels of a 320x240 frame) and
    // the cost is zero, but a tuning field that does nothing is worse than
    // either value.
    let shadow_options = options
        .with_depth_policy(DepthPolicy::Average)
        .with_depth_bias(options.depth_bias.saturating_add(shadow.depth_bias))
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
#[cfg(feature = "collision-debug-overlay")]
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

#[cfg(feature = "collision-debug-overlay")]
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

#[cfg(feature = "collision-debug-overlay")]
fn screen_xy(vertex: ProjectedVertex) -> (i16, i16) {
    (vertex.sx, vertex.sy)
}

#[cfg(feature = "collision-debug-overlay")]
fn draw_optional_debug_line(a: Option<(i16, i16)>, b: Option<(i16, i16)>, color: (u8, u8, u8)) {
    let (Some(a), Some(b)) = (a, b) else {
        return;
    };
    draw_line_mono(a.0, a.1, b.0, b.1, color.0, color.1, color.2);
}

/// One placed model instance's render-independent per-tick pose authority.
///
/// Resolve this once with [`resolve_instance_actor_pose`], retain it in the
/// gameplay loop's fixed-capacity live-entity storage, and pass the same value
/// to body, equipment, and combat consumers. The snapshot contains no render
/// scratch, camera, material-residency, or heap-owned state.
#[derive(Copy, Clone)]
pub struct InstanceActorPoseSnapshot {
    instance_index: u16,
    model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
    pose: ActorPoseSnapshot,
}

impl InstanceActorPoseSnapshot {
    /// Index into the cooked [`ModelTables::model_instances`] table.
    pub const fn instance_index(self) -> usize {
        self.instance_index as usize
    }

    /// Runtime model whose skeleton this snapshot samples.
    pub const fn model(self) -> RuntimeModelAsset {
        self.model
    }

    /// Model-local clip selected after applying the live override policy.
    pub const fn clip_local(self) -> ModelClipIndex {
        self.clip_local
    }

    /// Shared actor pose consumed by body, sockets, and combat volumes.
    pub const fn pose(self) -> ActorPoseSnapshot {
        self.pose
    }
}

/// Resolve one cooked/live model instance into its authoritative pose for one
/// simulation tick.
///
/// Live position, yaw, clip, phase, and one-shot state come from the matching
/// [`ModelInstancePoseOverride`]. Missing or invalid live clips retain the
/// cooked clip policy. The returned snapshot freezes root correction,
/// pitch/yaw/roll, visual offset, and scale so later consumers cannot resample
/// a different presentation pose.
pub fn resolve_instance_actor_pose<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
>(
    tables: ModelTables,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    pose_overrides: &[ModelInstancePoseOverride],
    instance_index: usize,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
) -> Option<InstanceActorPoseSnapshot> {
    let inst = tables.model_instances.get(instance_index)?;
    resolve_instance_actor_pose_record(
        tables,
        models,
        clips,
        pose_overrides,
        instance_index,
        inst,
        elapsed_tick,
        video_hz,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_instance_actor_pose_record<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
>(
    tables: ModelTables,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    pose_overrides: &[ModelInstancePoseOverride],
    instance_index: usize,
    inst: &psx_level::LevelModelInstanceRecord,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
) -> Option<InstanceActorPoseSnapshot> {
    let instance_index = u16::try_from(instance_index).ok()?;
    let runtime_model = models.get(inst.model.to_usize()).copied().flatten()?;
    let live = pose_override_for(pose_overrides, usize::from(instance_index));
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
    let anim = runtime_model.clip(clips, clip_local)?;
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
            live.speed_q8,
            live.frame_range,
        ),
        _ => {
            if inst.pose_frame == psx_level::MODEL_INSTANCE_POSE_ANIMATE {
                anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16())
            } else {
                (inst.pose_frame.min(anim.frame_count().saturating_sub(1)) as u32) << 12
            }
        }
    };
    Some(InstanceActorPoseSnapshot {
        instance_index,
        model: runtime_model,
        clip_local,
        pose: instance_actor_pose_from_components(
            tables,
            elapsed_tick,
            runtime_model,
            inst,
            anim,
            phase,
            clip_local,
            inst_x,
            inst_y,
            inst_z,
            inst_yaw,
        ),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn instance_actor_pose_from_components(
    tables: ModelTables,
    tick: SimTick,
    runtime_model: RuntimeModelAsset,
    inst: &psx_level::LevelModelInstanceRecord,
    animation: Animation<'static>,
    phase_q12: u32,
    clip_local: ModelClipIndex,
    x: i32,
    y: i32,
    z: i32,
    yaw: i16,
) -> ActorPoseSnapshot {
    let clip_anchor = model_clip_anchor(tables, runtime_model, clip_local);
    let reference_anchor = model_clip_anchor(tables, runtime_model, runtime_model.default_clip);
    let pose_translation = model_pose_anchor_translation(
        animation,
        phase_q12,
        clip_anchor,
        reference_anchor,
        None,
        None,
    );

    // Instance rotation from the authored transform (or the live
    // entity pose). The entity yaw and the renderer's visual yaw
    // share the Y axis; pitch and roll come from the entity
    // transform and compose as `Rz(roll) * Ry(yaw) * Rx(pitch)`
    // (the socket convention). The yaw-only case keeps the
    // cheaper single-axis build.
    let root_yaw = Angle::from_q12(yaw as u16);
    let combined_yaw = root_yaw.add_signed_q12(inst.visual_yaw);
    let rotation = if inst.pitch == 0 && inst.roll == 0 {
        yaw_rotation_matrix(combined_yaw)
    } else {
        euler_q12_rotation([inst.pitch, combined_yaw.as_q12() as i16, inst.roll])
    };
    // Authored instance positions are floor anchors. Ground the reference
    // (default) clip from its cooked posed floor, then reconcile every live
    // clip to that reference through `pose_translation`. Using only the mesh
    // bind-pose lift here leaves animated entities a few world units below
    // the BSP floor; painter ordering then clips their feet and makes them
    // appear to hover even though their gameplay anchor is correct.
    let origin = visual_model_origin(
        x,
        y,
        z,
        clip_floor_lift(reference_anchor, runtime_model),
        visual_model_local_to_world(runtime_model, inst.visual_scale_q8),
        inst.visual_offset,
        &rotation,
    );
    let local_to_world = visual_model_local_to_world(runtime_model, inst.visual_scale_q8);
    ActorPoseSnapshot::new(
        tick,
        animation,
        phase_q12,
        None,
        origin,
        rotation,
        local_to_world,
        pose_translation,
    )
}

/// Draw one placed model instance from a pose already resolved by the
/// simulation tick.
///
/// This function never reads animation clips or live pose overrides. Pair it
/// with [`resolve_instance_actor_pose`] and
/// [`super::draw_instance_equipment_from_pose`] so visible body geometry,
/// weapon sockets, and authored combat capsules all consume the exact same
/// [`InstanceActorPoseSnapshot`]. A snapshot for another room, an invalid
/// cooked instance index, or a filtered depth pass produces zero draws.
#[allow(clippy::too_many_arguments)]
pub fn draw_model_instance_from_pose<
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
    instance_pose: InstanceActorPoseSnapshot,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    room_reflection_probe: Option<VramSlot>,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    resolve_override_texture: &mut impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    let mut out = ModelInstanceDrawStats::default();
    if knobs.max_model_instances == 0 {
        return out;
    }
    let Some(inst) = tables.model_instances.get(instance_pose.instance_index()) else {
        return out;
    };
    if inst.room != current_room {
        return out;
    }
    let runtime_model = instance_pose.model();
    let clip_local = instance_pose.clip_local();
    let pose = instance_pose.pose();
    let anim = pose.animation();
    let phase = pose.phase_q12();
    let pose_translation = pose.pose_translation();
    let model_rotation = pose.rotation();
    let origin = pose.origin();
    let local_to_world = pose.local_to_world();
    let bounds = model_frame_bounds(tables, runtime_model, clip_local, phase);
    let bounds_origin =
        model_pose_translated_origin(origin, model_rotation, local_to_world, pose_translation);
    telemetry::stage_begin(telemetry::stage::MODEL_BOUNDS);
    out.bounds_tests = 1;
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
        out.bounds_culled = 1;
        return out;
    }

    let Some((base_material, cull_mode, uv_mapping)) = model_material_and_cull(
        runtime_model,
        inst.material_override,
        room_reflection_probe,
        elapsed_tick,
        video_hz,
        resolve_override_texture,
    ) else {
        return out;
    };
    let material = shade_model_material_at_bounds(
        lighting,
        origin,
        bounds_origin,
        model_rotation,
        bounds,
        inst.visual_scale_q8,
        base_material,
    );
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
            layer.material = shade_model_material_at_bounds(
                lighting,
                origin,
                bounds_origin,
                model_rotation,
                bounds,
                inst.visual_scale_q8,
                layer.material,
            );
            layer
        });
    let stats = submit_runtime_model_predecoded(
        world,
        triangles,
        runtime_model,
        anim,
        phase,
        pose.blend_from(),
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
    if !stats.primitive_overflow && !stats.command_overflow {
        out.draws = 1;
    }
    out
}

/// Animate + draw the placed model instances of `current_room`.
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
        let Some(instance_pose) = resolve_instance_actor_pose_record(
            tables,
            models,
            clips,
            pose_overrides,
            instance_index,
            inst,
            elapsed_tick,
            video_hz,
        ) else {
            continue;
        };
        let next = draw_model_instance_from_pose::<
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            BOUNDS_CULL,
            PROFILE,
        >(
            tables,
            knobs,
            scratch,
            current_room,
            instance_pose,
            elapsed_tick,
            video_hz,
            camera,
            options,
            lighting,
            room_reflection_probe,
            model_faces,
            model_parts,
            model_vertices,
            resolve_override_texture,
            triangles,
            world,
        );
        let overflow = next.stats.primitive_overflow || next.stats.command_overflow;
        drawn = drawn.saturating_add(next.draws as usize);
        accumulate_model_instance_draw_stats(&mut out, next);
        out.draws = drawn as u16;
        if overflow {
            return out;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::combat::transform_actor_combat_capsule;
    use psx_asset::Model;
    use psx_gpu::material::TextureMaterial;
    use psx_level::{
        CombatCapsuleRecord, LevelModelClipBoundsRecord, LevelModelInstanceRecord,
        LevelModelSocketRecord, ModelClipTableIndex, ModelFrameBoundsIndex, ModelIndex,
        ModelSocketIndex, OptionalModelClipIndex,
    };
    use std::{boxed::Box, vec::Vec};

    fn one_joint_model() -> Model<'static> {
        const ASSET_HEADER_SIZE: usize = 12;
        const MODEL_HEADER_SIZE: usize = 16;
        const JOINT_RECORD_SIZE: usize = 4;
        let payload_len = MODEL_HEADER_SIZE + JOINT_RECORD_SIZE;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PSMD");
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
        debug_assert_eq!(bytes.len(), ASSET_HEADER_SIZE);
        for value in [1u16, 0, 0, 0, 0, 1, 1, 4096] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&u16::MAX.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        Model::from_bytes(Box::leak(bytes.into_boxed_slice())).expect("one-joint model")
    }

    fn one_joint_animation(translations: &[i16]) -> Animation<'static> {
        const ANIMATION_HEADER_SIZE: usize = 8;
        const POSE_RECORD_SIZE: usize = 24;
        let payload_len = ANIMATION_HEADER_SIZE + translations.len() * POSE_RECORD_SIZE;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PSXA");
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&(translations.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&30u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        for &translation in translations {
            for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in [translation, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        Animation::from_bytes(Box::leak(bytes.into_boxed_slice())).expect("test animation")
    }

    fn runtime_model() -> RuntimeModelAsset {
        RuntimeModelAsset {
            index: ModelIndex::new(0),
            model_info: one_joint_model().into(),
            material: TextureMaterial::opaque(0, 0, (128, 128, 128)),
            clip_first: ModelClipTableIndex::new(0),
            clip_count: 2,
            default_clip: ModelClipIndex::new(0),
            socket_first: ModelSocketIndex::new(0),
            socket_count: 1,
            face_first: 0,
            face_count: 0,
            part_first: 0,
            part_count: 0,
            vertex_first: 0,
            vertex_count: 0,
            requires_cpu_blend: false,
            double_sided: false,
            world_height: 100,
            collision_radius: 24,
            local_to_world: LocalToWorldScale::IDENTITY,
            floor_lift: 0,
        }
    }

    fn instance(pose_frame: u16) -> LevelModelInstanceRecord {
        LevelModelInstanceRecord {
            room: RoomIndex::new(0),
            model: ModelIndex::new(0),
            clip: OptionalModelClipIndex::INHERIT,
            pose_frame,
            x: 10,
            y: 20,
            z: 30,
            yaw: 0,
            visual_yaw: 0,
            pitch: 0,
            roll: 0,
            visual_offset: [8, 4, -2],
            visual_scale_q8: 512,
            material_override: None,
            flags: 0,
        }
    }

    fn tables(inst: LevelModelInstanceRecord) -> ModelTables {
        let clip_bounds = Box::leak(Box::new([
            LevelModelClipBoundsRecord {
                model: ModelIndex::new(0),
                clip: ModelClipTableIndex::new(0),
                first_frame: ModelFrameBoundsIndex::new(0),
                frame_count: 3,
                floor_y: 10,
                pose_offset: [0, 0, 0],
                flags: 0,
            },
            LevelModelClipBoundsRecord {
                model: ModelIndex::new(0),
                clip: ModelClipTableIndex::new(1),
                first_frame: ModelFrameBoundsIndex::new(0),
                frame_count: 3,
                floor_y: 30,
                pose_offset: [4, 5, 6],
                flags: psx_level::model_clip_flags::IN_PLACE,
            },
        ]));
        ModelTables {
            model_clip_bounds: clip_bounds,
            model_frame_bounds: &[],
            model_sockets: &[],
            model_instances: Box::leak(Box::new([inst])),
            equipment: &[],
            weapons: &[],
            weapon_hitboxes: &[],
            entities: &[],
        }
    }

    fn live_override(one_shot: bool, phase_ticks: u16) -> ModelInstancePoseOverride {
        ModelInstancePoseOverride {
            instance: 0,
            x: 1_000,
            y: 2_000,
            z: 3_000,
            yaw: 1_024,
            clip: OptionalModelClipIndex::some(ModelClipIndex::new(1)),
            phase_ticks,
            one_shot,
            speed_q8: psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: psx_level::CharacterActionFrameRange::FULL,
        }
    }

    #[test]
    fn one_instance_snapshot_is_the_body_socket_and_combat_pose() {
        let tables = tables(instance(psx_level::MODEL_INSTANCE_POSE_ANIMATE));
        let runtime_model = runtime_model();
        let animations = [
            Some(one_joint_animation(&[10, 20, 30])),
            Some(one_joint_animation(&[100, 200, 300])),
        ];
        let live = [live_override(true, 999)];
        let snapshot = resolve_instance_actor_pose(
            tables,
            &[Some(runtime_model)],
            &animations,
            &live,
            0,
            SimTick::from_u32(77),
            VideoHz::NTSC,
        )
        .expect("instance pose");

        assert_eq!(snapshot.instance_index(), 0);
        assert_eq!(snapshot.model().index, ModelIndex::new(0));
        assert_eq!(snapshot.clip_local(), ModelClipIndex::new(1));
        assert_eq!(snapshot.pose().tick(), SimTick::from_u32(77));
        assert_eq!(snapshot.pose().phase_q12(), 1 << 12);
        assert!(snapshot.pose().blend_from().is_none());
        assert_eq!(
            snapshot.pose().pose_translation(),
            ModelPoseTranslation {
                x: -96,
                y: -15,
                z: 6
            }
        );
        assert_eq!(snapshot.pose().local_to_world().q12(), 8_192);
        assert_eq!(
            snapshot.pose().origin(),
            WorldVertex::new(998, 2_004, 2_992)
        );

        let body_joint = snapshot
            .pose()
            .joint_world_transform(0)
            .expect("body joint");
        assert_eq!(
            body_joint.translation,
            WorldVertex::new(1_010, 1_974, 2_784)
        );
        let legacy_joint = super::super::model_instance_joint_world_transform(
            tables,
            runtime_model,
            &tables.model_instances[0],
            animations[1].expect("live clip"),
            1 << 12,
            ModelClipIndex::new(1),
            live[0].x,
            live[0].y,
            live[0].z,
            live[0].yaw,
            0,
        )
        .expect("legacy body joint facade");
        assert_eq!(legacy_joint.translation, body_joint.translation);
        assert_eq!(legacy_joint.rotation, body_joint.rotation);

        let socket = LevelModelSocketRecord {
            model: ModelIndex::new(0),
            name: "RightHand",
            joint: 0,
            translation: [0, 0, 10],
            rotation_q12: [0; 3],
            flags: 0,
        };
        let socket_pose = super::super::equipment::attachment_socket_pose(snapshot.pose(), &socket)
            .expect("socket pose");
        assert_eq!(socket_pose.origin, WorldVertex::new(1_030, 1_974, 2_784));
        assert_eq!(
            socket_pose.rotation,
            snapshot.pose().joint_world_basis(0).unwrap()
        );

        let combat = transform_actor_combat_capsule(
            &CombatCapsuleRecord {
                joint: 0,
                flags: 0,
                action: 0,
                reserved: 0,
                start: [10, 0, 0],
                end: [30, 0, 0],
                radius: 8,
                active_start_frame: 0,
                active_end_frame: 2,
                damage: 5,
                poise_damage: 2,
                projectile_speed: 0,
                projectile_lifetime_ticks: 0,
                projectile_min_range: 0,
                projectile_max_range: 0,
                projectile_tint_rgb: [0; 3],
                projectile_damage_channel: psx_level::projectile_damage_channel::ZENITH,
                projectile_core_rgb: [0; 3],
                projectile_trail_segments: 0,
                projectile_glow_rgb: [0; 3],
                projectile_length_ticks: 0,
                projectile_impact_rgb: [0; 3],
                projectile_trail_spacing_ticks: 0,
                projectile_charge_start_frame: 0,
                projectile_glow_scale_q8: 0,
                projectile_impact_lifetime_ticks: 0,
                projectile_reserved: 0,
            },
            snapshot.pose(),
        )
        .expect("combat capsule");
        assert_eq!(combat.start, [1_010, 1_974, 2_764]);
        assert_eq!(combat.end, [1_010, 1_974, 2_724]);
    }

    #[test]
    fn live_override_loop_wraps_while_one_shot_clamps() {
        let tables = tables(instance(psx_level::MODEL_INSTANCE_POSE_ANIMATE));
        let model = runtime_model();
        let animations = [
            Some(one_joint_animation(&[10, 20, 30])),
            Some(one_joint_animation(&[100, 200, 300])),
        ];
        let looping = [live_override(false, 4)];
        let one_shot = [live_override(true, 4)];
        let resolve = |overrides: &[ModelInstancePoseOverride]| {
            resolve_instance_actor_pose(
                tables,
                &[Some(model)],
                &animations,
                overrides,
                0,
                SimTick::from_u32(91),
                VideoHz::NTSC,
            )
            .expect("instance pose")
        };
        assert_eq!(resolve(&looping).pose().phase_q12(), 0);
        assert_eq!(resolve(&one_shot).pose().phase_q12(), 1 << 12);
    }

    #[test]
    fn instance_origin_uses_the_default_clips_posed_floor() {
        let mut inst = instance(psx_level::MODEL_INSTANCE_POSE_ANIMATE);
        inst.visual_offset = [0; 3];
        inst.visual_scale_q8 = 256;
        let tables = ModelTables {
            model_clip_bounds: Box::leak(Box::new([LevelModelClipBoundsRecord {
                model: ModelIndex::new(0),
                clip: ModelClipTableIndex::new(0),
                first_frame: ModelFrameBoundsIndex::new(0),
                frame_count: 3,
                floor_y: -100,
                pose_offset: [0; 3],
                flags: 0,
            }])),
            model_frame_bounds: &[],
            model_sockets: &[],
            model_instances: Box::leak(Box::new([inst])),
            equipment: &[],
            weapons: &[],
            weapon_hitboxes: &[],
            entities: &[],
        };
        let mut model = runtime_model();
        // The bind floor intentionally disagrees with the posed default clip.
        // Regressing to the old bind-only path would produce Y=40, clipping
        // the animated feet through the authored floor.
        model.floor_lift = 20;
        let snapshot = resolve_instance_actor_pose(
            tables,
            &[Some(model)],
            &[Some(one_joint_animation(&[10, 20, 30])), None],
            &[],
            0,
            SimTick::ZERO,
            VideoHz::NTSC,
        )
        .expect("instance pose");

        assert_eq!(snapshot.pose().origin(), WorldVertex::new(10, 120, 30));
    }

    #[test]
    fn invalid_live_clip_keeps_cooked_frozen_phase() {
        let tables = tables(instance(1));
        let model = runtime_model();
        let animations = [
            Some(one_joint_animation(&[10, 20, 30])),
            Some(one_joint_animation(&[100, 200, 300])),
        ];
        let mut live = live_override(true, 999);
        live.clip = OptionalModelClipIndex::some(ModelClipIndex::new(7));
        let snapshot = resolve_instance_actor_pose(
            tables,
            &[Some(model)],
            &animations,
            &[live],
            0,
            SimTick::from_u32(123),
            VideoHz::NTSC,
        )
        .expect("fallback pose");
        assert_eq!(snapshot.clip_local(), ModelClipIndex::new(0));
        assert_eq!(snapshot.pose().phase_q12(), 1 << 12);
        assert_eq!(
            snapshot.pose().origin(),
            WorldVertex::new(998, 2_004, 2_992)
        );
    }
}
