use super::*;
use psx_engine::{JointWorldTransform, LoadedWorldCameraGte, TexturedViewVertex};
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
    for (index, equipment) in tables.equipment.iter().enumerate() {
        // What is held changes with the action; the cooked records do not.
        if index < 32 && knobs.equipment_mask & (1 << index) == 0 {
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
    let weapon_model = models.get(weapon.model?.to_usize()).copied().flatten()?;
    let (origin, weapon_rotation) = equipped_weapon_placement(
        socket_pose,
        weapon.grip_translation,
        weapon.grip_rotation_q12,
        weapon_model.local_to_world,
    );
    if knobs.equipment_assemble_q12 < SOLID_FROM_Q12 {
        let material = lighting.shade_model_material(origin, weapon_model.material);
        submit_weapon_assemble(
            weapon_model,
            origin,
            weapon_rotation,
            knobs,
            model_faces,
            model_parts,
            model_vertices,
            camera,
            options,
            material,
            triangles,
            world,
        );
        return Some(TexturedModelRenderStats::default());
    }
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

/// A weapon at this assembly level is solid, and the normal model path draws it.
pub const ASSEMBLED_Q12: u16 = 4096;

/// Past this the shards have almost landed, so the solid model stands in and
/// the expensive per-shard path stops. This is also what makes decimating the
/// cloud safe: the effect never has to show a complete weapon.
const SOLID_FROM_Q12: u16 = 3584;

/// Materialise a weapon out of its own triangles.
///
/// Each face seats at its own moment (a hash off the face index staggers the
/// timeline), and until it does it floats out along the ray from the weapon's
/// own origin, scattered and tumbling, over-brightened. So the blade converges
/// out of its own loose triangles, which is the Final Fantasy dissolve read
/// backwards. Dissolving is this function with `assemble_q12` running down.
///
/// The triangles stay rigid: no stretching into shards. The scatter and the
/// tumble are what keep it from reading as the model simply scaling up.
///
/// ponytail: no sparkle sprites, no transparency. Over-bright tint carries the
/// glow (additive submission is dropped by the world pass, see the
/// nanobot-assemble handoff), and the swords are 94 and 61 faces, so the whole
/// effect is a per-face loop with no scratch buffer.
#[allow(clippy::too_many_arguments)]
fn submit_weapon_assemble<const OT_DEPTH: usize>(
    weapon_model: RuntimeModelAsset,
    origin: WorldVertex,
    rotation: Mat3I16,
    knobs: ModelDrawKnobs,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize {
    // A face starts scattered in world units around where it will land, with
    // only a slight outward bias. Scatter has to dominate: pushing faces along
    // the ray from the weapon origin is a pure stretch on a model as long and
    // thin as a sword, and the cloud reads as a needle.
    const SPREAD_Q12: i32 = 1024;
    const SCATTER: i32 = 44;
    /// Full turns a face makes on its way in.
    const SPINS: i32 = 3;
    /// Distinct tumble angles and brightness steps across the flight. Both were
    /// computed per face at first, and both are expensive: an euler build is
    /// three matrices and two matrix multiplies, and `with_tint` rebuilds a
    /// packet material. That cost 690k cycles a frame against 22k for drawing
    /// the same 94 faces solid. Bucketing by how far along the face is costs 16
    /// of each instead of one per face, and is not visible at these steps.
    const STEPS: usize = 16;
    /// Most shards drawn at once. A face costs ~7200 cycles down this path
    /// against ~560 down the normal model path (measured: 680k against 52k for
    /// the same 94 faces), because nothing about a shard can be shared with its
    /// neighbours, so the count has to be capped rather than optimised away. A
    /// converging cloud reads the same at 40 pieces as at 94.
    const MAX_SHARDS: usize = 40;

    let Some(geometry) = runtime_model_geometry(weapon_model, model_parts, model_vertices) else {
        return 0;
    };
    let faces = runtime_model_faces(weapon_model, model_faces);
    let vertices = geometry.vertices;
    let stride = faces.len().div_ceil(MAX_SHARDS).max(1);
    let scale = weapon_model.local_to_world;
    let mut submitted = 0usize;

    // Subdivision is ON by default, and a zero max edge means subdivide
    // unconditionally: that alone was most of the effect's cost, because every
    // shard was being split every frame. A shard is small, short-lived and
    // untextured-looking anyway, so it does not need the affine correction.
    // No backface culling either, or half the tumbling triangles wink out.
    let shard_options = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_textured_triangle_splitting(false);

    // Every shard vertex is at its own place in the world, so nothing can be
    // shared between faces, but the CAMERA transform can: the plain world
    // submit redoes the world-to-view matrix in CPU fixed point for all 282
    // corners. Loading the camera into the GTE once and projecting through it
    // is what this projector exists for.
    let projector = LoadedWorldCameraGte::load(*camera);

    let mut tumbles = [Mat3I16::IDENTITY; STEPS];
    let mut materials = [material; STEPS];
    for step in 0..STEPS {
        let remaining = (step * 4096 / STEPS) as i32;
        let turn = SPINS * 4096 * remaining >> 12;
        tumbles[step] = euler_q12_rotation([
            (turn & 0xFFF) as i16,
            ((turn * 3 / 2) & 0xFFF) as i16,
            ((turn / 2) & 0xFFF) as i16,
        ]);
        if step > 0 {
            // 128 is 1.0 on PS1 modulation, so an airborne shard burns white
            // and cools to the weapon's own material as it lands.
            let level = (128 + ((remaining * 127) >> 12)) as u8;
            materials[step] = material.with_tint((level, level, level));
        }
    }

    for (face_index, face) in faces.iter().enumerate().step_by(stride) {
        let seed = (face_index as u32).wrapping_mul(0x9E37_79B9);
        // Doubling the progress leaves room for the per-face stagger while
        // still landing every face by the time the ramp completes.
        let seated = ((i32::from(knobs.equipment_assemble_q12)) * 2 - ((seed & 0x07FF) as i32))
            .clamp(0, 4096);
        if seated == 0 {
            continue;
        }
        let remaining = 4096 - seated;

        let mut verts = [WorldVertex::ZERO; 3];
        let mut uvs = [(0u8, 0u8); 3];
        let mut missing = false;
        for corner in 0..3 {
            let word = face.corner_words[corner];
            let Some(vertex) = vertices.get((word & 0xFFFF) as usize) else {
                missing = true;
                break;
            };
            let local = [
                scale.apply(i32::from(vertex.position.x)),
                scale.apply(i32::from(vertex.position.y)),
                scale.apply(i32::from(vertex.position.z)),
            ];
            let rotated = rotate_offset_q12(&rotation, local);
            verts[corner] = WorldVertex::new(
                origin.x.saturating_add(rotated[0]),
                origin.y.saturating_add(rotated[1]),
                origin.z.saturating_add(rotated[2]),
            );
            uvs[corner] = (((word >> 16) & 0xFF) as u8, ((word >> 24) & 0xFF) as u8);
        }
        if missing {
            continue;
        }

        let centre = [
            (verts[0].x + verts[1].x + verts[2].x) / 3,
            (verts[0].y + verts[1].y + verts[2].y) / 3,
            (verts[0].z + verts[1].z + verts[2].z) / 3,
        ];
        // The ray out of the weapon's own origin: no normalisation needed,
        // because a face's distance from the origin IS how far it travels.
        let spread = (SPREAD_Q12 * remaining) >> 12;
        let offset = [
            ((centre[0] - origin.x) * spread >> 12)
                + (((seed >> 3) & 0x3F) as i32 - 32) * SCATTER * remaining / 4096 / 32,
            ((centre[1] - origin.y) * spread >> 12)
                + (((seed >> 11) & 0x3F) as i32 - 32) * SCATTER * remaining / 4096 / 32,
            ((centre[2] - origin.z) * spread >> 12)
                + (((seed >> 19) & 0x3F) as i32 - 32) * SCATTER * remaining / 4096 / 32,
        ];
        // Tumble about the face's own centre, unwinding as it seats so the
        // triangle arrives on the pose rather than sliding in already aligned.
        // The turn COUNT is what reads as a spin: under one turn across the
        // whole flight looks like a settle. Faces stagger, so they sit at
        // different steps and do not turn in unison.
        let step = ((remaining as usize) * STEPS / 4096).min(STEPS - 1);
        let tumble = tumbles[step];
        for corner in 0..3 {
            let relative = [
                verts[corner].x - centre[0],
                verts[corner].y - centre[1],
                verts[corner].z - centre[2],
            ];
            let spun = rotate_offset_q12(&tumble, relative);
            verts[corner] = WorldVertex::new(
                centre[0] + spun[0] + offset[0],
                centre[1] + spun[1] + offset[1],
                centre[2] + spun[2] + offset[2],
            );
        }

        world.submit_textured_view_triangle(
            triangles,
            [
                TexturedViewVertex::new(
                    projector.view_vertex(verts[0]),
                    i32::from(uvs[0].0),
                    i32::from(uvs[0].1),
                ),
                TexturedViewVertex::new(
                    projector.view_vertex(verts[1]),
                    i32::from(uvs[1].0),
                    i32::from(uvs[1].1),
                ),
                TexturedViewVertex::new(
                    projector.view_vertex(verts[2]),
                    i32::from(uvs[2].0),
                    i32::from(uvs[2].1),
                ),
            ],
            camera.projection,
            materials[step],
            shard_options,
        );
        submitted += 1;
    }
    submitted
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

