use super::*;
use psx_engine::{JointWorldTransform, LoadedWorldCameraGte, ProjectedVertex, WorldRenderLayer};
use psx_gpu::prim::LineMono;
use psx_level::equipment_flags;

#[derive(Copy, Clone)]
pub(super) struct AttachmentPose {
    pub(super) origin: WorldVertex,
    pub(super) rotation: Mat3I16,
}

pub(super) fn draw_player_equipment<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
    const PROFILE: bool,
>(
    tables: ModelTables,
    knobs: ModelDrawKnobs,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let Some(player_pose) = resolve_player_actor_pose(
        tables,
        character,
        models,
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
    ) else {
        return EquipmentDrawStats::default();
    };
    draw_player_equipment_from_pose::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        PROFILE,
    >(
        tables,
        knobs,
        scratch,
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

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_player_equipment_from_pose<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
    const PROFILE: bool,
>(
    tables: ModelTables,
    knobs: ModelDrawKnobs,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let character_model = player_pose.model();
    let character_pose = player_pose.pose();

    let mut drawn = 0usize;
    for (index, equipment) in tables.equipment.iter().enumerate() {
        // What is held changes with the action, and each weapon has its own
        // beat in the swing; the cooked records do not know either.
        let wire_q12 = knobs
            .equipment_wire_q12
            .get(index)
            .copied()
            .unwrap_or(ASSEMBLED_Q12);
        if knobs.equipment_wireframe && wire_q12 == 0 {
            continue;
        }
        // Player equipment follows the player across rooms, matching the
        // room-agnostic melee spec (combat::player_melee_spec): the
        // record's `room` field is only the spawn room, so filtering on
        // it made the weapon vanish outside that room while its damage
        // kept working.
        if equipment.flags & equipment_flags::PLAYER == 0 || drawn >= knobs.max_equipment_draws {
            continue;
        }
        let Some(weapon) = tables.weapons.get(equipment.weapon.to_usize()) else {
            continue;
        };
        let Some(socket) = find_model_socket(tables, character_model, equipment.character_socket)
            .or_else(|| {
                find_model_socket(tables, character_model, weapon.default_character_socket)
            })
        else {
            continue;
        };
        let Some(socket_pose) = attachment_socket_pose(character_pose, socket) else {
            continue;
        };
        if let Some(stats) = submit_equipped_weapon::<
            MAX_RUNTIME_MODELS,
            MAX_RUNTIME_MODEL_CLIPS,
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            PROFILE,
        >(
            weapon,
            socket_pose,
            wire_q12,
            knobs,
            scratch,
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
        ) {
            accumulate_model_stats(&mut out.stats, stats);
            if stats.primitive_overflow || stats.command_overflow {
                out.draws = drawn as u16;
                return out;
            }
            drawn += 1;
            out.draws = drawn as u16;
        }
    }
    out
}

/// Place and submit one weapon model on a composed socket pose,
/// shared by the player and instance equipment passes. `None` when
/// the weapon has no model, the model is not loaded, or it has no
/// clip (cooked static props always ship a bind_pose).
#[allow(clippy::too_many_arguments)]
fn submit_equipped_weapon<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
    const PROFILE: bool,
>(
    weapon: &LevelWeaponRecord,
    socket_pose: AttachmentPose,
    wire_q12: u16,
    knobs: ModelDrawKnobs,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> Option<TexturedModelRenderStats> {
    let weapon_model = models.get(weapon.model?.to_usize()).copied().flatten()?;
    let (origin, weapon_rotation) = equipped_weapon_placement(
        socket_pose,
        weapon.grip_translation,
        weapon.grip_rotation_q12,
        weapon_model.local_to_world,
    );
    let anim = weapon_model.clip(clips, weapon_model.default_clip)?;
    let phase = anim.phase_at_tick_q12(elapsed_tick.as_u32(), video_hz.as_u16());
    let material = lighting.shade_model_material(origin, weapon_model.material);
    let model_options = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::Back)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(knobs.texture_split_max_edge);
    let faces = runtime_model_faces(weapon_model, model_faces);
    // The weapon's faces are sorted hilt-first at load, so how much of it has
    // materialised is just how much of the face slice is drawn solid. The rest
    // is the wireframe it fills into.
    if knobs.equipment_wireframe {
        // A nanobot weapon is never solid: it is a wireframe construct that
        // grows up the blade and retreats the same way. The faces are sorted
        // hilt-first, so how much of it exists is how much of the face list
        // gets an outline.
        let wired = if wire_q12 >= ASSEMBLED_Q12 {
            faces.len()
        } else {
            faces.len() * usize::from(wire_q12) / 4096
        };
        if wired > 0 {
            if let Some(geometry) =
                runtime_model_geometry(weapon_model, model_parts, model_vertices)
            {
                submit_weapon_wireframe(
                    weapon_model,
                    origin,
                    weapon_rotation,
                    &faces[..wired],
                    geometry.vertices,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
        return Some(TexturedModelRenderStats::default());
    }
    Some(submit_runtime_model_predecoded(
        world,
        triangles,
        weapon_model,
        anim,
        phase,
        None,
        *camera,
        origin,
        weapon_rotation,
        weapon_model.local_to_world,
        ModelPoseTranslation::ZERO,
        material,
        None,
        model_options,
        faces,
        model_parts,
        model_vertices,
        PROFILE,
        scratch,
    ))
}

/// A weapon at this assembly level is solid, and the normal model path draws it.
pub const ASSEMBLED_Q12: u16 = 4096;

/// Draw the not-yet-solid part of a weapon as a green wireframe.
///
/// The whole point of this over a shard cloud: the weapon stays RIGID, so every
/// vertex shares one transform. Project the model's vertices once and an edge
/// is then two screen points and a three-word line packet, with no per-edge
/// transform at all. That is the case the engine is fast at, and the shard
/// cloud was the case it is slow at.
///
/// ponytail: shared edges are drawn twice (once per adjoining face). Dedupe if
/// the line count ever matters; on a 94-face sword it does not.
#[allow(clippy::too_many_arguments)]
fn submit_weapon_wireframe<const OT_DEPTH: usize>(
    weapon_model: RuntimeModelAsset,
    origin: WorldVertex,
    rotation: Mat3I16,
    faces: &[TexturedModelRenderFace],
    vertices: &[ModelVertex],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lines: &mut impl PrimitiveSink<LineMono>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize {
    /// Nanobot green.
    const WIRE_COLOR: (u8, u8, u8) = (32, 255, 128);

    /// Vertices a weapon may have for the wireframe path. The swords have 54
    /// and 45; anything larger falls back to no wireframe rather than growing
    /// the guest stack.
    const VERTEX_CAP: usize = 96;

    if vertices.len() > VERTEX_CAP {
        return 0;
    }
    let projector = LoadedWorldCameraGte::load(*camera);
    let scale = weapon_model.local_to_world;
    let mut projected = [ProjectedVertex::INVALID; VERTEX_CAP];
    for (index, vertex) in vertices.iter().enumerate() {
        let local = [
            scale.apply(i32::from(vertex.position.x)),
            scale.apply(i32::from(vertex.position.y)),
            scale.apply(i32::from(vertex.position.z)),
        ];
        let rotated = rotate_offset_q12(&rotation, local);
        if let Some(point) = projector.project_world(WorldVertex::new(
            origin.x.saturating_add(rotated[0]),
            origin.y.saturating_add(rotated[1]),
            origin.z.saturating_add(rotated[2]),
        )) {
            projected[index] = point;
        }
    }

    let wire_options = options.with_render_layer(WorldRenderLayer::Opaque);
    let mut drawn = 0usize;
    for face in faces {
        // One line per face, its longest edge. Drawing all three overlaps into
        // a solid green bar on a 40-pixel blade and hides the fill under it,
        // but culling by a fixed length is worse: a sword TAPERS, so every edge
        // near the tip falls under any threshold that thins out the hilt, and
        // the cage vanishes exactly where the effect is happening. Longest-edge
        // gives every face one line wherever it sits on the blade.
        let mut best = (0i32, 0usize, 0usize);
        for corner in 0..3 {
            let indices = face.vertex_indices();
            let a = indices[corner] as usize;
            let b = indices[(corner + 1) % 3] as usize;
            if a >= vertices.len() || b >= vertices.len() {
                continue;
            }
            let (pa, pb) = (vertices[a].position, vertices[b].position);
            let (dx, dy, dz) = (
                i32::from(pa.x) - i32::from(pb.x),
                i32::from(pa.y) - i32::from(pb.y),
                i32::from(pa.z) - i32::from(pb.z),
            );
            let length = dx * dx + dy * dy + dz * dz;
            if length > best.0 {
                best = (length, a, b);
            }
        }
        if best.0 == 0 {
            continue;
        }
        let (a, b) = (projected[best.1], projected[best.2]);
        if a == ProjectedVertex::INVALID || b == ProjectedVertex::INVALID {
            continue;
        }
        world.submit_projected_line(lines, [a, b], WIRE_COLOR, wire_options);
        drawn += 1;
    }
    drawn
}

/// Maximum authored ribbon subdivisions. The editor/cooker reject larger
/// values, keeping the guest stack and packet budget fixed.
const MAX_WEAPON_TRAIL_SEGMENTS: usize = 6;

/// Draw action-authored player weapon trails from retained animation poses.
///
/// Each edge is reconstructed by sampling the same hand socket at an older
/// Q12 animation phase. Adjacent edges form native semi-transparent Gouraud
/// quads, producing a curved blade ribbon without history buffers or floats.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_player_weapon_trails_from_pose<
    const MAX_RUNTIME_MODELS: usize,
    const OT_DEPTH: usize,
>(
    tables: ModelTables,
    appearances: &'static [WeaponAppearanceRecord],
    character: psx_level::CharacterIndex,
    action: CharacterAnimationAction,
    wire_q12: [u16; MAX_PLAYER_EQUIPMENT],
    player_pose: PlayerActorPoseSnapshot,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    quads: &mut impl PrimitiveSink<QuadGouraudBlended>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> psx_engine::WorldRenderStats {
    let mut total = psx_engine::WorldRenderStats::default();
    let character_model = player_pose.model();
    let pose = player_pose.pose();
    let current_phase = pose.phase_q12();
    let frame_count = pose.animation().frame_count();
    if frame_count == 0 {
        return total;
    }
    let projector = LoadedWorldCameraGte::load(*camera);

    for (equipment_index, equipment) in tables.equipment.iter().enumerate() {
        if equipment.flags & equipment_flags::PLAYER == 0
            || equipment_index >= MAX_PLAYER_EQUIPMENT
            || wire_q12[equipment_index] == 0
        {
            continue;
        }
        let Some(appearance) = appearances.iter().find(|appearance| {
            appearance.flags & psx_level::weapon_appearance_flags::TRAIL != 0
                && appearance.character == character
                && appearance.action == action
                && appearance.weapon == equipment.weapon
                && appearance.character_socket == equipment.character_socket
        }) else {
            continue;
        };
        let start_phase = u32::from(appearance.trail_start_frame) << 12;
        let trail_end = if appearance.trail_end_frame == psx_level::CHARACTER_ACTION_FRAME_END_FULL
        {
            frame_count.saturating_sub(1)
        } else {
            appearance
                .trail_end_frame
                .min(frame_count.saturating_sub(1))
        };
        let end_phase_exclusive = u32::from(trail_end.saturating_add(1)) << 12;
        if current_phase < start_phase || current_phase >= end_phase_exclusive {
            continue;
        }
        let segment_count =
            usize::from(appearance.trail_segments).clamp(1, MAX_WEAPON_TRAIL_SEGMENTS);
        let Some(weapon) = tables.weapons.get(equipment.weapon.to_usize()) else {
            continue;
        };
        let Some(weapon_model) = weapon
            .model
            .and_then(|index| models.get(index.to_usize()))
            .copied()
            .flatten()
        else {
            continue;
        };
        let Some(socket) = find_model_socket(tables, character_model, equipment.character_socket)
            .or_else(|| {
                find_model_socket(tables, character_model, weapon.default_character_socket)
            })
        else {
            continue;
        };
        let Some((blade_root, blade_tip)) =
            weapon_trail_local_segment(tables, weapon, weapon_model, model_parts, model_vertices)
        else {
            continue;
        };

        let mut roots = [ProjectedVertex::INVALID; MAX_WEAPON_TRAIL_SEGMENTS + 1];
        let mut tips = [ProjectedVertex::INVALID; MAX_WEAPON_TRAIL_SEGMENTS + 1];
        let history_q12 = u32::from(appearance.trail_history_frames.max(1)) << 12;
        for sample in 0..=segment_count {
            let back = history_q12.saturating_mul(sample as u32) / segment_count as u32;
            let sample_phase = current_phase.saturating_sub(back).max(start_phase);
            let sampled_pose = pose.with_phase_q12(sample_phase);
            let Some(socket_pose) = attachment_socket_pose(sampled_pose, socket) else {
                continue;
            };
            let (origin, rotation) = equipped_weapon_placement(
                socket_pose,
                weapon.grip_translation,
                weapon.grip_rotation_q12,
                weapon_model.local_to_world,
            );
            let root =
                weapon_local_point_world(origin, rotation, weapon_model.local_to_world, blade_root);
            let tip =
                weapon_local_point_world(origin, rotation, weapon_model.local_to_world, blade_tip);
            roots[sample] = projector
                .project_world(root)
                .unwrap_or(ProjectedVertex::INVALID);
            tips[sample] = projector
                .project_world(tip)
                .unwrap_or(ProjectedVertex::INVALID);
        }

        let trail_options = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::None)
            .with_render_layer(WorldRenderLayer::Transparent);
        let blend_mode = weapon_trail_blend_mode(appearance.trail_blend_mode);
        for segment in 0..segment_count {
            if roots[segment] == ProjectedVertex::INVALID
                || tips[segment] == ProjectedVertex::INVALID
                || roots[segment + 1] == ProjectedVertex::INVALID
                || tips[segment + 1] == ProjectedVertex::INVALID
            {
                continue;
            }
            let current_fade = segment_count.saturating_sub(segment) as u8;
            let previous_fade = segment_count.saturating_sub(segment + 1) as u8;
            let colors = [
                faded_trail_color(
                    appearance.trail_root_color,
                    current_fade,
                    segment_count as u8,
                ),
                faded_trail_color(
                    appearance.trail_tip_color,
                    current_fade,
                    segment_count as u8,
                ),
                faded_trail_color(
                    appearance.trail_root_color,
                    previous_fade,
                    segment_count as u8,
                ),
                faded_trail_color(
                    appearance.trail_tip_color,
                    previous_fade,
                    segment_count as u8,
                ),
            ];
            let next = world.submit_blended_gouraud_quad(
                quads,
                [
                    projected_lit(roots[segment], colors[0]),
                    projected_lit(tips[segment], colors[1]),
                    projected_lit(roots[segment + 1], colors[2]),
                    projected_lit(tips[segment + 1], colors[3]),
                ],
                blend_mode,
                trail_options,
            );
            accumulate_world_stats(&mut total, next);
            if total.primitive_overflow || total.command_overflow {
                return total;
            }
        }
    }
    total
}

fn weapon_trail_local_segment(
    tables: ModelTables,
    weapon: &LevelWeaponRecord,
    weapon_model: RuntimeModelAsset,
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
) -> Option<([i32; 3], [i32; 3])> {
    let hitbox_first = weapon.hitbox_first.to_usize();
    let hitbox_end = hitbox_first.saturating_add(weapon.hitbox_count as usize);
    if let Some((start, end)) = tables
        .weapon_hitboxes
        .get(hitbox_first..hitbox_end)
        .and_then(|hitboxes| {
            hitboxes.iter().find_map(|hitbox| match hitbox.shape {
                WeaponHitShapeRecord::Capsule { start, end, .. } => Some((start, end)),
                WeaponHitShapeRecord::Box { .. } => None,
            })
        })
    {
        return Some(
            if local_distance_score(start, weapon.grip_translation)
                <= local_distance_score(end, weapon.grip_translation)
            {
                (start, end)
            } else {
                (end, start)
            },
        );
    }

    let geometry = runtime_model_geometry(weapon_model, model_parts, model_vertices)?;
    let mut farthest = weapon.grip_translation;
    let mut farthest_score = 0i32;
    for vertex in geometry.vertices {
        let point = [
            i32::from(vertex.position.x),
            i32::from(vertex.position.y),
            i32::from(vertex.position.z),
        ];
        let score = local_distance_score(point, weapon.grip_translation);
        if score > farthest_score {
            farthest_score = score;
            farthest = point;
        }
    }
    (farthest_score > 0).then_some((weapon.grip_translation, farthest))
}

fn local_distance_score(point: [i32; 3], origin: [i32; 3]) -> i32 {
    point
        .iter()
        .zip(origin)
        .map(|(point, origin)| square_i32_saturating(point.saturating_sub(origin) >> 4))
        .fold(0, i32::saturating_add)
}

fn weapon_local_point_world(
    origin: WorldVertex,
    rotation: Mat3I16,
    scale: LocalToWorldScale,
    local: [i32; 3],
) -> WorldVertex {
    let rotated = rotate_offset_q12(&rotation, scaled_offset(scale, local));
    WorldVertex::new(
        origin.x.saturating_add(rotated[0]),
        origin.y.saturating_add(rotated[1]),
        origin.z.saturating_add(rotated[2]),
    )
}

fn faded_trail_color(color: [u8; 3], numerator: u8, denominator: u8) -> (u8, u8, u8) {
    let fade = |channel: u8| {
        (u16::from(channel).saturating_mul(u16::from(numerator)) / u16::from(denominator.max(1)))
            as u8
    };
    (fade(color[0]), fade(color[1]), fade(color[2]))
}

fn projected_lit(point: ProjectedVertex, color: (u8, u8, u8)) -> ProjectedLit {
    ProjectedLit {
        sx: point.sx,
        sy: point.sy,
        sz: point.sz.clamp(0, i32::from(u16::MAX)) as u16,
        r: color.0,
        g: color.1,
        b: color.2,
    }
}

fn weapon_trail_blend_mode(mode: u8) -> BlendMode {
    match mode {
        psx_level::weapon_trail_blend_mode::AVERAGE => BlendMode::Average,
        psx_level::weapon_trail_blend_mode::ADD => BlendMode::Add,
        psx_level::weapon_trail_blend_mode::SUBTRACT => BlendMode::Subtract,
        _ => BlendMode::AddQuarter,
    }
}

fn accumulate_world_stats(
    total: &mut psx_engine::WorldRenderStats,
    next: psx_engine::WorldRenderStats,
) {
    total.submitted_triangles = total
        .submitted_triangles
        .saturating_add(next.submitted_triangles);
    total.culled_triangles = total.culled_triangles.saturating_add(next.culled_triangles);
    total.clipped_triangles = total
        .clipped_triangles
        .saturating_add(next.clipped_triangles);
    total.split_triangles = total.split_triangles.saturating_add(next.split_triangles);
    total.dropped_triangles = total
        .dropped_triangles
        .saturating_add(next.dropped_triangles);
    total.primitive_overflow |= next.primitive_overflow;
    total.command_overflow |= next.command_overflow;
}

/// Draw weapons riding NON-player equipment records: each record bound
/// to a model instance composes its socket from the instance's LIVE
/// pose (position, yaw, state clip, phase, via
/// [`super::instances::resolve_instance_actor_pose`]), so a wandering
/// enemy's sword follows its hand. Instances are room-resident, so
/// unlike the player pass this one is room-gated and runs inside the
/// per-room draw.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_instance_equipment_from_pose<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
    const PROFILE: bool,
>(
    tables: ModelTables,
    knobs: ModelDrawKnobs,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
    current_room: RoomIndex,
    instance_pose: super::instances::InstanceActorPoseSnapshot,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let Some(inst) = tables.model_instances.get(instance_pose.instance_index()) else {
        return out;
    };
    if inst.room != current_room {
        return out;
    }
    let mut drawn = 0usize;
    for equipment in tables.equipment {
        if equipment.flags & equipment_flags::PLAYER != 0
            || equipment.model_instance == psx_level::EquipmentRecord::NO_INSTANCE
            || equipment.model_instance as usize != instance_pose.instance_index()
            || equipment.room != current_room
            || drawn >= knobs.max_equipment_draws
        {
            continue;
        }
        let Some(stats) = submit_instance_equipment_record_from_pose::<
            MAX_RUNTIME_MODELS,
            MAX_RUNTIME_MODEL_CLIPS,
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            PROFILE,
        >(
            tables,
            equipment,
            instance_pose,
            knobs,
            scratch,
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
        ) else {
            continue;
        };
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

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_instance_equipment<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
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
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    pose_overrides: &[super::instances::ModelInstancePoseOverride],
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let mut drawn = 0usize;
    for equipment in tables.equipment {
        if equipment.flags & equipment_flags::PLAYER != 0
            || equipment.model_instance == psx_level::EquipmentRecord::NO_INSTANCE
            || equipment.room != current_room
            || drawn >= knobs.max_equipment_draws
        {
            continue;
        }
        let instance_index = equipment.model_instance as usize;
        let Some(context) = super::instances::resolve_instance_actor_pose(
            tables,
            models,
            clips,
            pose_overrides,
            instance_index,
            elapsed_tick,
            video_hz,
        ) else {
            continue;
        };
        let Some(stats) = submit_instance_equipment_record_from_pose::<
            MAX_RUNTIME_MODELS,
            MAX_RUNTIME_MODEL_CLIPS,
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            PROFILE,
        >(
            tables,
            equipment,
            context,
            knobs,
            scratch,
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
        ) else {
            continue;
        };
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

#[allow(clippy::too_many_arguments)]
fn submit_instance_equipment_record_from_pose<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
    const PROFILE: bool,
>(
    tables: ModelTables,
    equipment: &psx_level::EquipmentRecord,
    instance_pose: super::instances::InstanceActorPoseSnapshot,
    knobs: ModelDrawKnobs,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
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
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> Option<TexturedModelRenderStats> {
    let weapon = tables.weapons.get(equipment.weapon.to_usize())?;
    let socket = find_model_socket(tables, instance_pose.model(), equipment.character_socket)
        .or_else(|| {
            find_model_socket(
                tables,
                instance_pose.model(),
                weapon.default_character_socket,
            )
        })?;
    let socket_pose = attachment_socket_pose(instance_pose.pose(), socket)?;
    submit_equipped_weapon::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        PROFILE,
    >(
        weapon,
        socket_pose,
        ASSEMBLED_Q12,
        knobs,
        scratch,
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

fn find_model_socket(
    tables: ModelTables,
    model: RuntimeModelAsset,
    name: &str,
) -> Option<&'static LevelModelSocketRecord> {
    let first = model.socket_first.to_usize();
    let count = model.socket_count as usize;
    let sockets = tables
        .model_sockets
        .get(first..first.saturating_add(count))?;
    sockets.iter().find(|socket| socket.name == name)
}

pub(super) fn attachment_socket_pose(
    pose: ActorPoseSnapshot,
    socket: &LevelModelSocketRecord,
) -> Option<AttachmentPose> {
    let (joint, basis) = pose.joint_world_transform_and_basis(socket.joint)?;
    Some(compose_socket_pose(
        joint,
        basis,
        socket.translation,
        socket.rotation_q12,
    ))
}

fn compose_socket_pose(
    joint: JointWorldTransform,
    basis: Mat3I16,
    translation: [i32; 3],
    rotation_q12: [i16; 3],
) -> AttachmentPose {
    // Socket offsets are model-local units: the scaled joint matrix
    // takes them to world units (same convention as combat capsules).
    let offset = rotate_offset_q12(&joint.rotation, translation);
    // Orientation uses the unscaled basis: the attached model applies
    // its own local-to-world, so a scaled basis would shrink it to
    // sub-pixel size (every face then culls as zero-area).
    let local_rotation = euler_q12_rotation(rotation_q12);
    AttachmentPose {
        origin: WorldVertex::new(
            joint.translation.x.saturating_add(offset[0]),
            joint.translation.y.saturating_add(offset[1]),
            joint.translation.z.saturating_add(offset[2]),
        ),
        rotation: basis.mul(&local_rotation),
    }
}

/// Pure equipped-weapon placement: the grip inverse composition and the
/// weapon-origin subtraction exactly as [`submit_equipped_weapon`] submits
/// them. Extracted so the spawn-transient regression exercises the shipped
/// calculation rather than a lookalike.
fn equipped_weapon_placement(
    socket_pose: AttachmentPose,
    grip_translation: [i32; 3],
    grip_rotation_q12: [i16; 3],
    weapon_scale: LocalToWorldScale,
) -> (WorldVertex, Mat3I16) {
    let weapon_rotation = socket_pose
        .rotation
        .mul(&euler_q12_rotation_inverse(grip_rotation_q12));
    let grip = scaled_offset(weapon_scale, grip_translation);
    let grip_world = rotate_offset_q12(&weapon_rotation, grip);
    let origin = WorldVertex::new(
        socket_pose.origin.x.saturating_sub(grip_world[0]),
        socket_pose.origin.y.saturating_sub(grip_world[1]),
        socket_pose.origin.z.saturating_sub(grip_world[2]),
    );
    (origin, weapon_rotation)
}

fn scaled_offset(scale: LocalToWorldScale, offset: [i32; 3]) -> [i32; 3] {
    [
        scale.apply(offset[0]),
        scale.apply(offset[1]),
        scale.apply(offset[2]),
    ]
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

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::actor_pose::ActorPoseSnapshot;
    use psx_engine::{LocalToWorldScale, ModelPoseTranslation, SimTick};
    use psx_level::{LevelModelSocketRecord, ModelIndex};
    use std::{boxed::Box, format, vec::Vec};

    fn one_joint_animation(translation: i16) -> Animation<'static> {
        const ANIMATION_HEADER_SIZE: usize = 8;
        const POSE_RECORD_SIZE: usize = 24;
        let payload_len = ANIMATION_HEADER_SIZE + 2 * POSE_RECORD_SIZE;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PSXA");
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&30u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        for _ in 0..2 {
            for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in [translation, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        Animation::from_bytes(Box::leak(bytes.into_boxed_slice())).expect("test animation")
    }

    /// Regression for the historical "spawn-adjacent tick produced an
    /// i32::MIN weapon-origin X" transient (handoff 7.1). It drives the
    /// SHIPPED weapon math: [`attachment_socket_pose`] (socket compose on
    /// the retained snapshot) into [`equipped_weapon_placement`] (grip
    /// inverse composition and the weapon-origin subtraction exactly as
    /// [`submit_equipped_weapon`] uses them), then the blade endpoints
    /// through the same scaled-rotation offset the vertex path applies.
    ///
    /// The swept envelope is the enforced envelope, not a guess:
    /// socket and grip translations to the compact i16 range the cook now
    /// rejects beyond; rotations across the full i16 Q12 turn wheel the
    /// record format admits; model scales across the full u16 Q12 header
    /// range; origins far beyond any cooked room. Within it, no component
    /// can reach the i32::MIN sentinel or leave a sane world bound.
    /// Pre-refresh sampling stays structurally impossible upstream
    /// (snapshots are Option and every consumer skips None).
    #[test]
    fn equipped_weapon_placement_cannot_reach_the_min_sentinel_in_the_enforced_envelope() {
        const SANE_WORLD_BOUND: i32 = 33_000_000;
        let assert_sane = |value: i32, what: &str| {
            assert_ne!(value, i32::MIN, "{what} saturated to the i32::MIN sentinel");
            assert!(
                value.abs() < SANE_WORLD_BOUND,
                "{what} left the sane world envelope: {value}"
            );
        };
        let spun = Mat3I16 {
            m: [[0, 0, 4096], [0, -4096, 0], [-4096, 0, 0]],
        };

        for joint_translation in [i16::MIN, i16::MAX, 0] {
            let animation = one_joint_animation(joint_translation);
            for scale_q12 in [0x1000u16, 0x2000, u16::MAX] {
                for origin_x in [-1_000_000i32, 1_000_000] {
                    let pose = ActorPoseSnapshot::new(
                        SimTick::from_u32(0),
                        animation,
                        0,
                        None,
                        WorldVertex::new(origin_x, 1_000_000, -1_000_000),
                        spun,
                        LocalToWorldScale::from_q12(scale_q12),
                        ModelPoseTranslation {
                            x: 32_767,
                            y: -32_768,
                            z: 32_767,
                        },
                    );
                    for extreme in [-32_768i32, 32_767] {
                        let socket = LevelModelSocketRecord {
                            model: ModelIndex(0),
                            name: "right_hand_grip",
                            joint: 0,
                            translation: [extreme, -extreme, extreme],
                            rotation_q12: [i16::MIN, i16::MAX, 0x2000],
                            flags: 0,
                        };
                        let socket_pose = attachment_socket_pose(pose, &socket)
                            .expect("extreme socket still samples");
                        assert_sane(socket_pose.origin.x, "socket origin x");
                        assert_sane(socket_pose.origin.y, "socket origin y");
                        assert_sane(socket_pose.origin.z, "socket origin z");

                        let (origin, rotation) = equipped_weapon_placement(
                            socket_pose,
                            [extreme, extreme, -extreme],
                            [i16::MAX, i16::MIN, -0x2000],
                            LocalToWorldScale::from_q12(scale_q12),
                        );
                        assert_sane(origin.x, "weapon origin x");
                        assert_sane(origin.y, "weapon origin y");
                        assert_sane(origin.z, "weapon origin z");

                        // Blade extremes: the farthest representable model
                        // vertices through the same scaled rotation the
                        // weapon vertex path applies.
                        for tip in [[32_767i32, 32_767, 32_767], [-32_768, -32_768, -32_768]] {
                            let scaled = scaled_offset(LocalToWorldScale::from_q12(scale_q12), tip);
                            let world = rotate_offset_q12(&rotation, scaled);
                            let origin_components = [origin.x, origin.y, origin.z];
                            for (axis, name) in ["x", "y", "z"].iter().enumerate() {
                                assert_sane(
                                    origin_components[axis].saturating_add(world[axis]),
                                    &format!("blade endpoint {name}"),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
