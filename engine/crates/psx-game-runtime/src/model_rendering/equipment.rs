use super::*;
use psx_engine::JointWorldTransform;
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let character_model = player_pose.model();
    let character_pose = player_pose.pose();

    let mut drawn = 0usize;
    for equipment in tables.equipment {
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> Option<TexturedModelRenderStats> {
    let weapon_rotation = socket_pose
        .rotation
        .mul(&euler_q12_rotation_inverse(weapon.grip_rotation_q12));
    let weapon_model = models.get(weapon.model?.to_usize()).copied().flatten()?;
    let grip = scaled_offset(weapon_model.local_to_world, weapon.grip_translation);
    let grip_world = rotate_offset_q12(&weapon_rotation, grip);
    let origin = WorldVertex::new(
        socket_pose.origin.x.saturating_sub(grip_world[0]),
        socket_pose.origin.y.saturating_sub(grip_world[1]),
        socket_pose.origin.z.saturating_sub(grip_world[2]),
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
