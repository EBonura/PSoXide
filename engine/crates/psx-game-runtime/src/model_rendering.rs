//! Model-asset decode and the player/instance/equipment render policy,
//! carved out of `editor-playtest`'s `model_rendering` module family
//! (phase 2 of docs/game-runtime-plan.md). Cooked tables arrive
//! bundled as `&'static` psx-level records in [`ModelTables`],
//! capacities as `const N` generic parameters, knobs as
//! [`ModelDrawKnobs`]/[`ShadowTuning`] values, the projected-vertex
//! and joint scratch as an owned [`ModelDrawScratch`], and the
//! example's VRAM atlas uploader as a closure until its glue fully
//! migrates.

use psx_asset::{Animation, Model, ModelPart, ModelPoseBlend, ModelVertex};
use psx_engine::{
    apply_model_pose_translation, compute_joint_world_transform, telemetry, Angle, CullMode,
    DepthPolicy, JointViewTransform, JointWorldTransform, LocalToWorldScale, Mat3I16,
    ModelPoseTranslation, ModelUvMapping, ModelUvOffset, PrimitiveSink, ProjectedVertex, RoomPoint,
    SimTick, TexturedModelGeometry, TexturedModelLayer, TexturedModelRenderFace,
    TexturedModelRenderStats, VideoHz, WorldCamera, WorldRenderPass, WorldSurfaceOptions,
    WorldVertex,
};
use psx_gpu::{
    material::{BlendMode, TextureMaterial},
    prim::TriTextured,
};
use psx_level::{
    model_clip_flags, model_override_blend, AssetId, AssetKind, CharacterAnimationAction,
    EntityRecord, EquipmentRecord, LevelMaterialSidedness, LevelModelClipBoundsRecord,
    LevelModelClipRecord, LevelModelFrameBoundsRecord, LevelModelInstanceRecord,
    LevelModelMaterialOverride, LevelModelRecord, LevelModelSecondaryLayer, LevelModelSocketRecord,
    LevelWeaponRecord, ModelClipIndex, ModelClipTableIndex, ModelIndex, ModelSocketIndex,
    RoomIndex, WeaponHitboxRecord,
};
use psx_math::int32::{clamp_i16, square_i32_saturating};

use crate::character::{PlayerAnim, PlayerAnimBlend, RuntimeCharacter};
use crate::room_cache::ActiveRuntimeRoom;
use crate::room_lighting::RuntimeRoomLighting;
use crate::vram::{vram_slot_texture_size_u8, VramSlot};

mod equipment;
mod instances;

pub use self::instances::{
    accumulate_model_instance_draw_stats, actor_shadow_radius, draw_actor_shadow,
    draw_collision_cylinder_debug, draw_model_instance_shadows, draw_model_instances,
    ModelInstanceDepthPass, ModelInstanceDrawStats, ModelInstancePoseOverride,
};

/// The cooked model-family tables the render policy walks, bundled so
/// call sites thread one value.
#[derive(Copy, Clone)]
pub struct ModelTables {
    /// Per-clip pose anchors + frame-bounds directory.
    pub model_clip_bounds: &'static [LevelModelClipBoundsRecord],
    /// Per-frame bounding spheres the directory indexes.
    pub model_frame_bounds: &'static [LevelModelFrameBoundsRecord],
    /// Authored attachment sockets.
    pub model_sockets: &'static [LevelModelSocketRecord],
    /// Placed model instances.
    pub model_instances: &'static [LevelModelInstanceRecord],
    /// Authored equipment placements.
    pub equipment: &'static [EquipmentRecord],
    /// Weapon definitions.
    pub weapons: &'static [LevelWeaponRecord],
    /// Weapon hitbox windows.
    pub weapon_hitboxes: &'static [WeaponHitboxRecord],
    /// Cooked entities (the first-pass hitbox targets).
    pub entities: &'static [EntityRecord],
}

/// Model draw policy knobs, as one value: the split threshold and
/// per-frame draw caps the game selects. The culling/profiling
/// TOGGLES ride as `const` parameters on the draw functions instead
/// (`BOUNDS_CULL`/`PROFILE`) so a disabled path costs nothing, exactly
/// like the compile-time consts they were before the carve.
#[derive(Copy, Clone)]
pub struct ModelDrawKnobs {
    /// Projected edge threshold used to subdivide close model triangles.
    pub texture_split_max_edge: u16,
    /// Cap on placed model instances rendered per frame.
    pub max_model_instances: usize,
    /// Cap on attached weapon/equipment visuals rendered per frame.
    pub max_equipment_draws: usize,
}

/// Actor floor-shadow tuning, as one value.
#[derive(Copy, Clone)]
pub struct ShadowTuning {
    /// Lift above the floor so the decal never z-fights it.
    pub floor_lift: i32,
    /// Depth bias pushing the decal behind co-planar geometry.
    pub depth_bias: i32,
    /// Shadow radius scale numerator over the actor radius.
    pub radius_scale_num: i32,
    /// Shadow radius scale denominator.
    pub radius_scale_den: i32,
    /// Smallest shadow radius.
    pub radius_min: i32,
    /// Largest shadow radius.
    pub radius_max: i32,
}

/// Q8 fixed-point identity for per-instance visual model scale.
pub const MODEL_VISUAL_SCALE_ONE_Q8: u16 = 256;

/// Owned projected-vertex + joint-transform scratch for the model
/// submit paths (formerly the example's `MODEL_VERTICES` and
/// `JOINT_VIEW_TRANSFORMS` statics).
pub struct ModelDrawScratch<const MODEL_VERTEX_CAP: usize, const JOINT_CAP: usize> {
    vertices: [ProjectedVertex; MODEL_VERTEX_CAP],
    joints: [JointViewTransform; JOINT_CAP],
}

impl<const MODEL_VERTEX_CAP: usize, const JOINT_CAP: usize>
    ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>
{
    /// All-zero scratch (link-time `.bss`-safe); every use overwrites
    /// before reading.
    pub const fn zeroed() -> Self {
        Self {
            vertices: [ProjectedVertex::new(0, 0, 0); MODEL_VERTEX_CAP],
            joints: [JointViewTransform::ZERO; JOINT_CAP],
        }
    }
}

/// Parsed, VRAM-bound model payload ready for the hot render path.
#[derive(Copy, Clone)]
#[allow(missing_docs)]
pub struct RuntimeModelAsset {
    /// Index into the cooked `MODELS` table.
    pub index: ModelIndex,
    pub model: Model<'static>,
    pub material: TextureMaterial,
    pub clip_first: ModelClipTableIndex,
    pub clip_count: u16,
    pub default_clip: ModelClipIndex,
    pub socket_first: ModelSocketIndex,
    pub socket_count: u16,
    pub face_first: u16,
    pub face_count: u16,
    pub part_first: u16,
    pub part_count: u16,
    pub vertex_first: u16,
    pub vertex_count: u16,
    pub requires_cpu_blend: bool,
    pub double_sided: bool,
    pub world_height: u16,
    pub collision_radius: u16,
    pub local_to_world: LocalToWorldScale,
}

impl RuntimeModelAsset {
    #[allow(clippy::too_many_arguments)]
    fn from_record(
        index: ModelIndex,
        record: &LevelModelRecord,
        face_pool: &mut [TexturedModelRenderFace],
        face_cursor: &mut usize,
        part_pool: &mut [ModelPart],
        part_cursor: &mut usize,
        vertex_pool: &mut [ModelVertex],
        vertex_cursor: &mut usize,
        resolve_asset_bytes: &mut impl FnMut(AssetId, AssetKind) -> Option<&'static [u8]>,
        ensure_model_atlas_uploaded: &mut impl FnMut(AssetId, &[u8]) -> Option<VramSlot>,
    ) -> Option<Self> {
        let mesh_bytes = resolve_asset_bytes(record.mesh_asset, AssetKind::ModelMesh)?;
        let model = Model::from_bytes(mesh_bytes).ok()?;
        let texture_asset = record.texture_asset?;
        let atlas_bytes = resolve_asset_bytes(texture_asset, AssetKind::Texture)?;
        let atlas_slot = ensure_model_atlas_uploaded(texture_asset, atlas_bytes)?;
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
            double_sided: model.double_sided(),
            world_height: record.world_height,
            collision_radius: record.collision_radius,
            local_to_world: LocalToWorldScale::from_q12(model.local_to_world_q12()),
        })
    }

    /// The parsed animation for a model-local clip index, if loaded.
    pub fn clip<const MAX_RUNTIME_MODEL_CLIPS: usize>(
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

/// Decode a cooked [`model_override_blend`] code into the SDK blend
/// mode. Unknown codes render opaque.
pub const fn model_override_blend_mode(code: u8) -> BlendMode {
    match code {
        model_override_blend::AVERAGE => BlendMode::Average,
        model_override_blend::ADD => BlendMode::Add,
        model_override_blend::SUBTRACT => BlendMode::Subtract,
        model_override_blend::ADD_QUARTER => BlendMode::AddQuarter,
        _ => BlendMode::Opaque,
    }
}

/// Covering material for a model override resolved to a VRAM slot:
/// the slot's tpage/CLUT/texture-window with the authored blend mode
/// and tint. Model UVs address the override texture directly; the
/// slot texture window wraps them for packed power-of-two textures.
pub fn model_override_material(
    slot: VramSlot,
    material_override: LevelModelMaterialOverride,
) -> TextureMaterial {
    let [r, g, b] = material_override.tint_rgb;
    TextureMaterial::blended(
        slot.clut_word,
        slot.tpage_word,
        (r, g, b),
        model_override_blend_mode(material_override.blend_mode),
    )
    .with_texture_window(slot.texture_window)
}

/// Resolve one cooked secondary model layer to its independently tinted and
/// blended VRAM material. A non-resident layer is skipped rather than
/// disturbing the base model draw.
fn model_secondary_material(
    layer: LevelModelSecondaryLayer,
    slot: VramSlot,
) -> (TextureMaterial, u8, u8) {
    let [r, g, b] = layer.tint_rgb;
    (
        TextureMaterial::blended(
            slot.clut_word,
            slot.tpage_word,
            (r, g, b),
            model_override_blend_mode(layer.blend_mode),
        )
        .with_texture_window(slot.texture_window),
        vram_slot_texture_size_u8(slot.texture_width),
        vram_slot_texture_size_u8(slot.texture_height),
    )
}

/// Resolve one moving secondary pass without touching texture residency. The
/// fixed simulation tick drives only byte-sized UV displacement.
fn model_secondary_layer(
    layer: LevelModelSecondaryLayer,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    room_reflection_probe: Option<VramSlot>,
    resolve_override_texture: &mut impl FnMut(AssetId) -> Option<VramSlot>,
) -> Option<TexturedModelLayer> {
    let slot = if layer.uses_room_reflection_probe() {
        room_reflection_probe?
    } else {
        resolve_override_texture(layer.texture_asset?)?
    };
    let (mut material, texture_width, texture_height) = model_secondary_material(layer, slot);
    if layer.uses_room_reflection_probe() {
        let strength = u16::from(layer.reflection_strength());
        let [r, g, b] = layer.tint_rgb;
        material = material.with_tint((
            ((u16::from(r) * strength + 127) / 255) as u8,
            ((u16::from(g) * strength + 127) / 255) as u8,
            ((u16::from(b) * strength + 127) / 255) as u8,
        ));
    }
    let [mut u, mut v] = layer
        .motion
        .offset_at_tick(elapsed_tick.as_u32(), video_hz.as_u16());
    u %= texture_width.max(1);
    v %= texture_height.max(1);
    let offset = ModelUvOffset::new(u, v);
    let mapping = if layer.uses_room_reflection_probe() {
        ModelUvMapping::ScreenSpaceReflection {
            texture_width,
            texture_height,
            roughness: layer.reflection_roughness_level(),
            uv_offset: offset,
        }
    } else {
        ModelUvMapping::Authored
    };
    Some(
        TexturedModelLayer::new(material)
            .with_uv_mapping(mapping)
            .with_uv_offset(if mapping.is_authored() {
                offset
            } else {
                ModelUvOffset::ZERO
            }),
    )
}

/// Apply an override's blend and tint to the model's own baked
/// atlas. CLUT, tpage address/depth, and texture window stay intact.
pub const fn model_override_atlas_material(
    atlas: TextureMaterial,
    material_override: LevelModelMaterialOverride,
) -> TextureMaterial {
    let [r, g, b] = material_override.tint_rgb;
    atlas
        .with_blend_mode(model_override_blend_mode(material_override.blend_mode))
        .with_tint((r, g, b))
}

/// Winding cull for a model override's authored face sidedness.
pub const fn model_override_cull_mode(material_override: LevelModelMaterialOverride) -> CullMode {
    match material_override.sidedness() {
        LevelMaterialSidedness::Front => CullMode::Back,
        LevelMaterialSidedness::Back => CullMode::Front,
        LevelMaterialSidedness::Both => CullMode::None,
    }
}

/// Resolve the material + cull mode one model draw uses. A model with an
/// authored covering texture waits until that texture is resident instead of
/// exposing its native atlas as a visible one-frame placeholder.
#[inline]
fn model_material_and_cull(
    runtime_model: RuntimeModelAsset,
    material_override: Option<LevelModelMaterialOverride>,
    room_reflection_probe: Option<VramSlot>,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    resolve_override_texture: &mut impl FnMut(AssetId) -> Option<VramSlot>,
) -> Option<(TextureMaterial, CullMode, ModelUvMapping)> {
    let base_cull = if runtime_model.double_sided {
        CullMode::None
    } else {
        CullMode::Back
    };
    match material_override {
        Some(material_override) => {
            let [u, v] = material_override
                .motion
                .offset_at_tick(elapsed_tick.as_u32(), video_hz.as_u16());
            let uv_offset = ModelUvOffset::new(u, v);
            let atlas = model_override_atlas_material(runtime_model.material, material_override);
            if material_override.uses_room_reflection_probe() {
                if let Some(slot) = room_reflection_probe {
                    let strength = u16::from(material_override.reflection_strength());
                    let mut reflection_override = material_override;
                    for channel in &mut reflection_override.tint_rgb {
                        *channel = ((u16::from(*channel) * strength + 127) / 255) as u8;
                    }
                    return Some((
                        model_override_material(slot, reflection_override),
                        model_override_cull_mode(material_override),
                        ModelUvMapping::ScreenSpaceReflection {
                            texture_width: vram_slot_texture_size_u8(slot.texture_width),
                            texture_height: vram_slot_texture_size_u8(slot.texture_height),
                            roughness: material_override.reflection_roughness_level(),
                            uv_offset,
                        },
                    ));
                }
            }
            let material = match material_override.texture_asset {
                Some(texture_asset) => resolve_override_texture(texture_asset)
                    .map(|slot| model_override_material(slot, material_override))?,
                None => atlas,
            };
            Some((
                material,
                model_override_cull_mode(material_override),
                if uv_offset.is_zero() {
                    ModelUvMapping::Authored
                } else {
                    ModelUvMapping::AuthoredOffset(uv_offset)
                },
            ))
        }
        None => Some((runtime_model.material, base_cull, ModelUvMapping::Authored)),
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
        let vertex_count = model.vertex_count();
        if face
            .corners
            .iter()
            .any(|corner| corner.vertex_index >= vertex_count)
        {
            return None;
        }
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

/// Largest u8 UV coordinate addressing a `size`-texel texture.
#[inline(always)]
pub fn model_render_uv_max(size: u16) -> u8 {
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

/// The clip's full duration in vblanks for one playthrough of the
/// action's frame range at its authored speed.
#[allow(clippy::too_many_arguments)]
pub fn player_clip_duration_vblanks<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
>(
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    character: RuntimeCharacter,
    clip: ModelClipIndex,
    video_hz: VideoHz,
    speed_q8: u16,
    frame_range: psx_level::CharacterActionFrameRange,
) -> Option<u32> {
    let runtime_model = models.get(character.model.to_usize()).copied().flatten()?;
    let animation = runtime_model.clip(clips, clip)?;
    let sample_rate = animation.sample_rate_hz().max(1) as u32;
    let frames = action_frame_range_frame_count(animation, frame_range);
    let unscaled = frames
        .saturating_mul(video_hz.as_nonzero_u32())
        .div_ceil(sample_rate);
    Some(scale_duration_for_action_speed(unscaled, speed_q8))
}

/// Forward push speed for the current attack frame, if the action
/// pushes and the animation is inside the push window.
pub fn player_action_push_speed<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
>(
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    character: RuntimeCharacter,
    anim: PlayerAnim,
    local_tick: u32,
    video_hz: VideoHz,
) -> Option<i32> {
    let action = anim.action();
    let push = character.action_push(action);
    if push.distance <= 0 {
        return None;
    }
    let runtime_model = models.get(character.model.to_usize()).copied().flatten()?;
    let animation = runtime_model.clip(clips, character.clip_for(anim))?;
    let action_range = character.action_frame_range(action);
    let (push_start, push_end) =
        normalized_action_push_frame_range(animation, action_range, push.frame_range);
    let phase = animation_phase_at_tick_q12(
        animation,
        local_tick,
        video_hz,
        character.action_loops(action),
        character.action_speed(action),
        action_range,
    );
    let current_frame = phase >> 12;
    if current_frame < push_start || current_frame > push_end {
        return None;
    }
    let sample_rate = animation.sample_rate_hz().max(1) as u32;
    let push_frames = push_end.saturating_sub(push_start).saturating_add(1);
    let unscaled = push_frames
        .saturating_mul(video_hz.as_nonzero_u32())
        .div_ceil(sample_rate);
    let duration = scale_duration_for_action_speed(unscaled, character.action_speed(action));
    Some((push.distance as u32).div_ceil(duration).max(1) as i32)
}

/// Parse every cooked model + animation into the runtime tables:
/// clears the caches, decodes clips, then decodes each model's faces,
/// parts, and vertices into the shared pools.
#[allow(clippy::too_many_arguments)]
pub fn load_runtime_models<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
>(
    model_records: &'static [LevelModelRecord],
    model_clip_records: &'static [LevelModelClipRecord],
    models: &mut [Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    clips: &mut [Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    model_faces: &mut [TexturedModelRenderFace],
    model_face_count: &mut usize,
    model_parts: &mut [ModelPart],
    model_part_count: &mut usize,
    model_vertices: &mut [ModelVertex],
    model_vertex_count: &mut usize,
    mut resolve_asset_bytes: impl FnMut(AssetId, AssetKind) -> Option<&'static [u8]>,
    mut ensure_model_atlas_uploaded: impl FnMut(AssetId, &[u8]) -> Option<VramSlot>,
) {
    let mut i = 0;
    while i < MAX_RUNTIME_MODELS {
        models[i] = None;
        i += 1;
    }

    i = 0;
    while i < MAX_RUNTIME_MODEL_CLIPS {
        clips[i] = None;
        i += 1;
    }
    *model_face_count = 0;
    *model_part_count = 0;
    *model_vertex_count = 0;

    for (index, clip) in model_clip_records.iter().enumerate() {
        if index >= MAX_RUNTIME_MODEL_CLIPS {
            break;
        }
        let Some(bytes) = resolve_asset_bytes(clip.animation_asset, AssetKind::ModelAnimation)
        else {
            continue;
        };
        clips[index] = Animation::from_bytes(bytes).ok();
    }

    for (index, record) in model_records.iter().enumerate() {
        if index >= MAX_RUNTIME_MODELS {
            break;
        }
        models[index] = RuntimeModelAsset::from_record(
            ModelIndex(index as u16),
            record,
            model_faces,
            model_face_count,
            model_parts,
            model_part_count,
            model_vertices,
            model_vertex_count,
            &mut resolve_asset_bytes,
            &mut ensure_model_atlas_uploaded,
        );
    }
}

fn scale_duration_for_action_speed(duration_vblanks: u32, speed_q8: u16) -> u32 {
    duration_vblanks
        .saturating_mul(psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8 as u32)
        .div_ceil(speed_q8.max(1) as u32)
        .max(1)
}

fn normalized_action_frame_range(
    animation: Animation<'static>,
    frame_range: psx_level::CharacterActionFrameRange,
) -> (u32, u32) {
    let final_unique_frame = animation.frame_count().saturating_sub(2) as u32;
    let start = (frame_range.start as u32).min(final_unique_frame);
    let end = if frame_range.end == psx_level::CHARACTER_ACTION_FRAME_END_FULL {
        final_unique_frame
    } else {
        (frame_range.end as u32).min(final_unique_frame)
    };
    (start, end.max(start))
}

fn normalized_action_push_frame_range(
    animation: Animation<'static>,
    action_range: psx_level::CharacterActionFrameRange,
    push_range: psx_level::CharacterActionFrameRange,
) -> (u32, u32) {
    let (action_start, action_end) = normalized_action_frame_range(animation, action_range);
    if push_range == psx_level::CharacterActionFrameRange::FULL {
        return (action_start, action_end);
    }
    let (push_start, push_end) = normalized_action_frame_range(animation, push_range);
    let start = push_start.max(action_start);
    let end = push_end.min(action_end).max(start);
    (start, end)
}

fn action_frame_range_frame_count(
    animation: Animation<'static>,
    frame_range: psx_level::CharacterActionFrameRange,
) -> u32 {
    let (start, end) = normalized_action_frame_range(animation, frame_range);
    end.saturating_sub(start).saturating_add(1)
}

/// Player draw outcome: submit stats plus the bounds-cull verdict.
#[derive(Copy, Clone, Debug, Default)]
pub struct PlayerModelDrawStats {
    /// Model submit stats.
    pub stats: TexturedModelRenderStats,
    /// Bounds tests run (0 or 1).
    pub bounds_tests: u16,
    /// Bounds tests that culled the draw (0 or 1).
    pub bounds_culled: u16,
}

/// Draw the player's animated model through the compact predecoded
/// model path.
#[allow(clippy::too_many_arguments)]
#[inline]
/// Resolve a crossfade spec into a pose-blend source against this
/// model's clip table.
///
/// `None` when there is no blend, the blend has saturated, or the
/// outgoing clip is missing from the loaded set (a missing clip
/// degrades to a hard cut, never a corrupt pose).
pub(crate) fn player_pose_blend<const MAX_RUNTIME_MODEL_CLIPS: usize>(
    character: RuntimeCharacter,
    runtime_model: RuntimeModelAsset,
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    blend: Option<PlayerAnimBlend>,
    video_hz: VideoHz,
) -> Option<ModelPoseBlend<'static>> {
    let blend = blend?;
    if blend.alpha_q12 >= 1 << 12 {
        return None;
    }
    let action = blend.anim.action();
    let clip_local = character.clip_for(blend.anim);
    let anim = runtime_model.clip(clips, clip_local)?;
    let phase = animation_phase_at_tick_q12(
        anim,
        blend.local_tick,
        video_hz,
        character.action_loops(action),
        character.action_speed(action),
        character.action_frame_range(action),
    );
    anim.looped_pose_sample_q12(phase)
        .map(|sample| ModelPoseBlend {
            sample,
            alpha_q12: blend.alpha_q12,
        })
}

/// Nanobot assemble effect: draw the player as free triangles falling
/// into the live pose. `progress_q12` runs 0 (fully scattered) to 4096
/// (fully assembled). Per-face stagger, tumble, and an additive tint
/// ramp come from a deterministic hash of the face index, so the effect
/// replays identically every run. Replaces `draw_player` for the
/// effect's duration; primary-joint skinning only (blend verts follow
/// their primary joint), which is invisible at tumble speeds.
#[allow(clippy::too_many_arguments)]
pub fn draw_player_assemble<
    const MAX_RUNTIME_MODELS: usize,
    const MAX_RUNTIME_MODEL_CLIPS: usize,
    const OT_DEPTH: usize,
>(
    tables: ModelTables,
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
    progress_q12: u16,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let Some(runtime_model) = models.get(character.model.to_usize()).copied().flatten() else {
        return;
    };
    let Some(anim) = runtime_model.clip(clips, clip_local) else {
        return;
    };
    let local_tick = elapsed_tick.saturating_sub(anim_start_tick);
    let phase = animation_phase_at_tick_q12(
        anim,
        local_tick,
        video_hz,
        character.action_loops(anim_action),
        character.action_speed(anim_action),
        character.action_frame_range(anim_action),
    );
    let clip_anchor = model_clip_anchor(tables, runtime_model, clip_local);
    let reference_anchor =
        model_clip_anchor(tables, runtime_model, character.clip_for(PlayerAnim::Idle));
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

    // Per-joint world transforms for the live pose.
    let model = runtime_model.model;
    let joint_count = (model.joint_count() as usize).min(64);
    let mut joints = [JointWorldTransform {
        rotation: Mat3I16::IDENTITY,
        translation: WorldVertex::ZERO,
    }; 64];
    let mut joint = 0usize;
    while joint < joint_count {
        if let Some(pose) = anim.pose_looped_q12(phase, joint as u16) {
            let pose = apply_model_pose_translation(pose, pose_translation);
            joints[joint] =
                compute_joint_world_transform(pose, model_rotation, local_to_world, origin);
        }
        joint += 1;
    }

    let Some(geometry) = runtime_model_geometry(runtime_model, model_parts, model_vertices) else {
        return;
    };
    let faces = runtime_model_faces(runtime_model, model_faces);
    let base_material = lighting.shade_model_material(origin, runtime_model.material);
    let parts = geometry.parts;
    let vertices = geometry.vertices;

    let joint_of_vertex = |vi: usize| -> usize {
        let mut p = 0usize;
        while p < parts.len() {
            let part = parts[p];
            let first = part.first_vertex() as usize;
            if vi >= first && vi < first + part.vertex_count() as usize {
                return (part.joint_index() as usize).min(joint_count.saturating_sub(1));
            }
            p += 1;
        }
        0
    };

    let mut face_index = 0usize;
    while face_index < faces.len() {
        let face = faces[face_index];
        let seed = (face_index as u32).wrapping_mul(0x9E37_79B9);
        // Staggered per-face timeline: each face settles at its own moment.
        let ft = ((progress_q12 as i32) * 2 - (seed & 0x07FF) as i32).clamp(0, 4096);
        face_index += 1;
        if ft == 0 {
            continue;
        }
        let mut verts = [WorldVertex::ZERO; 3];
        let mut uvs = [(0u8, 0u8); 3];
        let mut corner = 0usize;
        let mut skip = false;
        while corner < 3 {
            let word = face.corner_words[corner];
            let vi = (word & 0xFFFF) as usize;
            if vi >= vertices.len() {
                skip = true;
                break;
            }
            let v = vertices[vi];
            let jwt = joints[joint_of_vertex(vi)];
            let scaled = [
                local_to_world.apply(v.position.x as i32),
                local_to_world.apply(v.position.y as i32),
                local_to_world.apply(v.position.z as i32),
            ];
            let r = rotate_offset_q12(&jwt.rotation, scaled);
            verts[corner] = WorldVertex::new(
                jwt.translation.x.saturating_add(r[0]),
                jwt.translation.y.saturating_add(r[1]),
                jwt.translation.z.saturating_add(r[2]),
            );
            uvs[corner] = (((word >> 16) & 0xFF) as u8, ((word >> 24) & 0xFF) as u8);
            corner += 1;
        }
        if skip {
            continue;
        }
        let cx = (verts[0].x + verts[1].x + verts[2].x) / 3;
        let cy = (verts[0].y + verts[1].y + verts[2].y) / 3;
        let cz = (verts[0].z + verts[1].z + verts[2].z) / 3;
        let remaining = (4096 - ft) as i32;
        // Fall from above with lateral scatter, easing linearly onto the pose.
        let off_x = ((((seed >> 8) & 0x3FF) as i32 - 512) * 3) * remaining >> 12;
        let off_y = (1000 + ((seed >> 2) & 0x3FF) as i32) * remaining >> 12;
        let off_z = ((((seed >> 16) & 0x3FF) as i32 - 512) * 3) * remaining >> 12;
        // Tumble that winds down as the face settles.
        let spin_total = (((seed >> 20) & 0x7FF) + 1024) as i32;
        let ang = ((spin_total * remaining) >> 12) as i16;
        let spin = euler_q12_rotation([ang, ang / 2, 0]);
        let mut corner = 0usize;
        while corner < 3 {
            let rel = [
                verts[corner].x - cx,
                verts[corner].y - cy,
                verts[corner].z - cz,
            ];
            let rot = rotate_offset_q12(&spin, rel);
            verts[corner] = WorldVertex::new(
                cx + rot[0] + off_x,
                cy + rot[1] + off_y,
                cz + rot[2] + off_z,
            );
            corner += 1;
        }
        // Additive ghost ramp while unsettled; full material once seated.
        let material = if ft < 3072 {
            // Over-bright additive (128 = 1.0 modulation) so airborne
            // shards glow against dark scenes instead of vanishing.
            let level = (128 + (ft * 127) / 3072) as u8;
            base_material
                .with_tint((level, level, level))
                .with_blend_mode(BlendMode::Add)
        } else if ft < 3584 {
            // Seat flash: modulation above 128 over-brightens on PS1.
            base_material.with_tint((220, 220, 220))
        } else {
            base_material
        };
        world.submit_textured_world_triangle(triangles, *camera, verts, uvs, material, options);
    }
}

pub fn draw_player<
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
    room_reflection_probe: Option<VramSlot>,
    resolve_override_texture: &mut impl FnMut(AssetId) -> Option<VramSlot>,
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
        character.action_speed(anim_action),
        character.action_frame_range(anim_action),
    );
    let blend_from = player_pose_blend(character, runtime_model, clips, blend, video_hz);
    let bounds = model_frame_bounds(tables, runtime_model, clip_local, phase);
    let clip_anchor = model_clip_anchor(tables, runtime_model, clip_local);
    let reference_anchor =
        model_clip_anchor(tables, runtime_model, character.clip_for(PlayerAnim::Idle));
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
        Some(bounds) if BOUNDS_CULL => model_bounds_visible(
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

    let Some((base_material, cull_mode, uv_mapping)) = model_material_and_cull(
        runtime_model,
        character.material_override,
        room_reflection_probe,
        elapsed_tick,
        video_hz,
        resolve_override_texture,
    ) else {
        return PlayerModelDrawStats {
            stats: TexturedModelRenderStats::default(),
            bounds_tests: 1,
            bounds_culled: 0,
        };
    };
    let material = lighting.shade_model_material(origin, base_material);
    let model_options = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(cull_mode)
        .with_model_uv_mapping(uv_mapping)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(knobs.texture_split_max_edge);

    telemetry::stage_begin(telemetry::stage::PLAYER_DRAW);
    let faces = runtime_model_faces(runtime_model, model_faces);
    let secondary_material = character
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
        blend_from,
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
    telemetry::stage_end(telemetry::stage::PLAYER_DRAW);
    PlayerModelDrawStats {
        stats,
        bounds_tests: 1,
        bounds_culled: 0,
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn submit_runtime_model_predecoded<
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
>(
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    runtime_model: RuntimeModelAsset,
    anim: Animation<'static>,
    phase: u32,
    blend_from: Option<ModelPoseBlend<'static>>,
    camera: WorldCamera,
    origin: WorldVertex,
    rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
    material: TextureMaterial,
    secondary_material: Option<TexturedModelLayer>,
    options: WorldSurfaceOptions,
    faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    profile_enabled: bool,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
) -> TexturedModelRenderStats {
    let start_cycles = if profile_enabled {
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
        world.submit_textured_model_predecoded_geometry_faces_layered(
            triangles,
            runtime_model.model,
            anim,
            phase,
            camera,
            origin,
            rotation,
            local_to_world,
            pose_translation,
            &mut scratch.vertices,
            &mut scratch.joints,
            material,
            secondary_material,
            options,
            faces,
            geometry,
            blend_from,
        )
    } else {
        world.submit_textured_model_primary_joints_predecoded_geometry_faces_layered(
            triangles,
            runtime_model.model,
            anim,
            phase,
            camera,
            origin,
            rotation,
            local_to_world,
            pose_translation,
            &mut scratch.vertices,
            &mut scratch.joints,
            material,
            secondary_material,
            options,
            faces,
            geometry,
            blend_from,
        )
    };
    if profile_enabled {
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

/// Equipment draw outcome: draw count, hitbox activity, and submit
/// stats.
#[derive(Copy, Clone, Debug, Default)]
pub struct EquipmentDrawStats {
    /// Weapon models drawn.
    pub draws: u16,
    /// Hitboxes active on the current animation frame.
    pub active_hitboxes: u16,
    /// Entities the active hitboxes overlapped.
    pub target_hits: u16,
    /// Model submit stats.
    pub stats: TexturedModelRenderStats,
}

/// Draw the player's attached equipment (weapon models) and evaluate
/// its active weapon hitboxes against cooked entities.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn draw_player_equipment<
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
    equipment::draw_player_equipment::<
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
        current_room,
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

fn rotate_offset_q12(rotation: &Mat3I16, offset: [i32; 3]) -> [i32; 3] {
    let row = |r: [i16; 3]| -> i32 {
        let x = (r[0] as i32).saturating_mul(offset[0]);
        let y = (r[1] as i32).saturating_mul(offset[1]);
        let z = (r[2] as i32).saturating_mul(offset[2]);
        x.saturating_add(y).saturating_add(z) >> 12
    };
    [row(rotation.m[0]), row(rotation.m[1]), row(rotation.m[2])]
}

/// Emit one model family's submit stats onto the given counters.
pub fn emit_model_counters(
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

/// The player's camera-view depth in `active`'s room frame (for the
/// behind/in-front model-instance depth passes).
#[inline]
pub fn player_actor_depth_for_room<const MAX_RUNTIME_MODELS: usize>(
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

/// Euler rotation matrix in Q12 turns, composed as `Rz * Ry * Rx`.
/// Shared by attachment sockets and full-rotation model instances.
fn euler_q12_rotation(rotation_q12: [i16; 3]) -> Mat3I16 {
    let rx = Mat3I16::rotate_x(Angle::from_q12(rotation_q12[0] as u16).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(rotation_q12[1] as u16).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(rotation_q12[2] as u16).rotate_y_arg());
    rz.mul(&ry).mul(&rx)
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

/// Animation phase (frame << 12 | fraction) at `local_tick` for the
/// action's authored frame range, speed, and looping mode.
pub fn animation_phase_at_tick_q12(
    animation: Animation<'static>,
    local_tick: u32,
    video_hz: VideoHz,
    looping: bool,
    speed_q8: u16,
    frame_range: psx_level::CharacterActionFrameRange,
) -> u32 {
    let (start_frame, end_frame) = normalized_action_frame_range(animation, frame_range);
    let range_phase = animation.phase_at_tick_scaled_q12(local_tick, video_hz.as_u16(), speed_q8);
    let start_phase = start_frame << 12;
    let end_phase = end_frame << 12;
    if looping {
        let frame_count = end_frame.saturating_sub(start_frame).saturating_add(1);
        let cycle_phase = frame_count << 12;
        return start_phase + range_phase % cycle_phase.max(1);
    }
    start_phase + range_phase.min(end_phase.saturating_sub(start_phase))
}

/// Sample one player joint through the exact presentation transform used by
/// [`draw_player`]. Gameplay hit volumes therefore follow the visible pose,
/// including authored visual yaw/scale/offset and in-place root correction.
#[allow(clippy::too_many_arguments)]
pub fn player_joint_world_transform(
    tables: ModelTables,
    runtime_model: RuntimeModelAsset,
    character: RuntimeCharacter,
    animation: Animation<'static>,
    phase_q12: u32,
    x: i32,
    y: i32,
    z: i32,
    yaw: Angle,
    action: CharacterAnimationAction,
    clip_local: ModelClipIndex,
    joint: u8,
) -> Option<JointWorldTransform> {
    let clip_anchor = model_clip_anchor(tables, runtime_model, clip_local);
    let reference_anchor =
        model_clip_anchor(tables, runtime_model, character.clip_for(PlayerAnim::Idle));
    let pose_translation = model_pose_anchor_translation(
        animation,
        phase_q12,
        clip_anchor,
        reference_anchor,
        character.action_in_place_override(action),
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
    let pose = apply_model_pose_translation(
        animation.pose_looped_q12(phase_q12, u16::from(joint))?,
        pose_translation,
    );
    Some(compute_joint_world_transform(
        pose,
        model_rotation,
        local_to_world,
        origin,
    ))
}

/// Sample one live model-instance joint through the same transform used by
/// [`draw_model_instances`]. The caller supplies the entity's live transform,
/// clip, and phase so authored hurtboxes stay attached to the rendered rig.
#[allow(clippy::too_many_arguments)]
pub fn model_instance_joint_world_transform(
    tables: ModelTables,
    runtime_model: RuntimeModelAsset,
    instance: &LevelModelInstanceRecord,
    animation: Animation<'static>,
    phase_q12: u32,
    clip_local: ModelClipIndex,
    x: i32,
    y: i32,
    z: i32,
    yaw: i16,
    joint: u8,
) -> Option<JointWorldTransform> {
    let clip_anchor = model_clip_anchor(tables, runtime_model, clip_local);
    let reference_anchor = model_clip_anchor(tables, runtime_model, runtime_model.default_clip);
    let pose_translation =
        model_pose_anchor_translation(animation, phase_q12, clip_anchor, reference_anchor, None);
    let combined_yaw = Angle::from_q12(yaw as u16).add_signed_q12(instance.visual_yaw);
    let model_rotation = if instance.pitch == 0 && instance.roll == 0 {
        yaw_rotation_matrix(combined_yaw)
    } else {
        euler_q12_rotation([instance.pitch, combined_yaw.as_q12() as i16, instance.roll])
    };
    let origin = visual_model_origin(
        x,
        y,
        z,
        runtime_model.world_height,
        instance.visual_offset,
        instance.visual_scale_q8,
        &model_rotation,
    );
    let local_to_world = visual_model_local_to_world(runtime_model, instance.visual_scale_q8);
    let pose = apply_model_pose_translation(
        animation.pose_looped_q12(phase_q12, u16::from(joint))?,
        pose_translation,
    );
    Some(compute_joint_world_transform(
        pose,
        model_rotation,
        local_to_world,
        origin,
    ))
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

#[inline]
fn model_frame_bounds(
    tables: ModelTables,
    runtime_model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
    phase_q12: u32,
) -> Option<LevelModelFrameBoundsRecord> {
    let clip = runtime_model.clip_table_index(clip_local)?;
    let record = tables.model_clip_bounds.get(clip.to_usize()).copied()?;
    if record.model != runtime_model.index || record.clip != clip || record.frame_count == 0 {
        return None;
    }
    let frame = ((phase_q12 >> 12) % record.frame_count as u32) as usize;
    tables
        .model_frame_bounds
        .get(record.first_frame.to_usize().saturating_add(frame))
        .copied()
}

#[inline]
fn model_clip_anchor(
    tables: ModelTables,
    runtime_model: RuntimeModelAsset,
    clip_local: ModelClipIndex,
) -> Option<ModelClipAnchor> {
    let clip = runtime_model.clip_table_index(clip_local)?;
    let record = tables.model_clip_bounds.get(clip.to_usize()).copied()?;
    (record.model == runtime_model.index && record.clip == clip).then_some(ModelClipAnchor {
        floor_y: record.floor_y,
        pose_offset: record.pose_offset,
        in_place: (record.flags & model_clip_flags::IN_PLACE) != 0,
    })
}

#[inline]
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

/// Conservative sphere-vs-view-frustum test: whether a sphere at
/// `center` with `radius` can touch the screen (padded by
/// `screen_margin`) between the near plane and the options' far depth.
#[inline]
pub fn sphere_visible_to_camera(
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

/// Saturating Q8 ratio `numerator / denominator` (0 when either is
/// non-positive).
pub fn ratio_q8_i32(numerator: i32, denominator: i32) -> i32 {
    if numerator <= 0 || denominator <= 0 {
        return 0;
    }
    let numerator = numerator as u32;
    let denominator = denominator as u32;
    let whole = numerator / denominator;
    let remainder = numerator % denominator;
    let scaled_whole = if whole > (i32::MAX as u32 / 256) {
        return i32::MAX;
    } else {
        whole * 256
    };
    let scaled_remainder = remainder.saturating_mul(256) / denominator;
    scaled_whole
        .saturating_add(scaled_remainder)
        .min(i32::MAX as u32) as i32
}

/// Squared X/Z (floor-plane) distance between two room points.
pub fn distance_xz_sq(a: RoomPoint, b: RoomPoint) -> i32 {
    let dx = a.x.saturating_sub(b.x);
    let dz = a.z.saturating_sub(b.z);
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vram::VramSlotClutMode;
    use psx_gpu::material::TextureWindow;
    use psx_vram::VramHandle;

    fn slot(texture_window: TextureWindow) -> VramSlot {
        VramSlot {
            asset: AssetId(7),
            clut_mode: VramSlotClutMode::TransparentZero,
            ready: true,
            clut_word: 0x1234,
            tpage_word: 0x018F,
            texture_window,
            texture_width: 64,
            texture_height: 64,
            region: VramHandle::Empty,
            clut_region: VramHandle::Empty,
        }
    }

    fn override_record(blend_mode: u8, flags: u16) -> LevelModelMaterialOverride {
        LevelModelMaterialOverride {
            texture_asset: Some(AssetId(7)),
            blend_mode,
            tint_rgb: [96, 128, 160],
            motion: psx_level::LevelMaterialUvMotion::default(),
            secondary_layer: None,
            flags,
        }
    }

    #[test]
    fn override_blend_codes_decode_to_sdk_modes() {
        assert_eq!(
            model_override_blend_mode(model_override_blend::OPAQUE),
            BlendMode::Opaque
        );
        assert_eq!(
            model_override_blend_mode(model_override_blend::AVERAGE),
            BlendMode::Average
        );
        assert_eq!(
            model_override_blend_mode(model_override_blend::ADD),
            BlendMode::Add
        );
        assert_eq!(
            model_override_blend_mode(model_override_blend::SUBTRACT),
            BlendMode::Subtract
        );
        assert_eq!(
            model_override_blend_mode(model_override_blend::ADD_QUARTER),
            BlendMode::AddQuarter
        );
        assert_eq!(model_override_blend_mode(0xFF), BlendMode::Opaque);
    }

    #[test]
    fn override_material_encodes_slot_blend_tint_and_window() {
        let window = TextureWindow::power_of_two_tile(64, 64, 64, 64);
        let material = model_override_material(
            slot(window),
            override_record(model_override_blend::ADD_QUARTER, 0),
        );
        assert_eq!(material.clut_word(), 0x1234);
        // Slot tpage address/depth bits preserved, blend bits rewritten.
        assert_eq!(material.tpage_word() & !(0x0060 | 0x0200), 0x018F & !0x0060);
        assert_eq!((material.tpage_word() >> 5) & 0x3, 3);
        assert_eq!(material.tint(), (96, 128, 160));
        assert_eq!(material.texture_window_word(), window.word());
        // Semi-transparent command bit set for translucent modes...
        assert!(material.textured_packet_material().is_translucent());
        // ...and clear for an opaque covering material.
        let opaque = model_override_material(
            slot(TextureWindow::NONE),
            override_record(model_override_blend::OPAQUE, 0),
        );
        assert!(!opaque.textured_packet_material().is_translucent());
    }

    #[test]
    fn atlas_override_preserves_texture_binding_and_changes_optics() {
        let atlas = TextureMaterial::opaque(0x4321, 0x018F, (128, 128, 128))
            .with_texture_window(TextureWindow::power_of_two_tile(64, 32, 128, 128));
        let mut override_record = override_record(model_override_blend::AVERAGE, 0);
        override_record.texture_asset = None;
        let material = model_override_atlas_material(atlas, override_record);
        assert_eq!(material.clut_word(), atlas.clut_word());
        assert_eq!(material.texture_window_word(), atlas.texture_window_word());
        assert_eq!(material.tint(), (96, 128, 160));
        assert_eq!(material.blend_mode(), BlendMode::Average);
        assert!(material.textured_packet_material().is_translucent());
    }

    #[test]
    fn secondary_layer_scroll_wraps_inside_its_texture_window() {
        let mut overlay_slot = slot(TextureWindow::power_of_two_tile(64, 64, 32, 16));
        overlay_slot.texture_width = 32;
        overlay_slot.texture_height = 16;
        let layer = LevelModelSecondaryLayer {
            texture_asset: Some(overlay_slot.asset),
            blend_mode: model_override_blend::ADD_QUARTER,
            tint_rgb: [128; 3],
            motion: psx_level::LevelMaterialUvMotion {
                enabled: true,
                speed_u_q8: 2 * 256,
                speed_v_q8: -2 * 256,
                phase_u: 250,
                phase_v: 250,
            },
            flags: 0,
        };
        let resolved = model_secondary_layer(
            layer,
            SimTick::from_u32(30),
            VideoHz::NTSC,
            None,
            &mut |_| Some(overlay_slot),
        )
        .expect("resident overlay");
        assert_eq!(resolved.uv_offset, ModelUvOffset::new(27, 9));
    }

    #[test]
    fn secondary_layer_can_use_the_room_probe_with_its_own_motion() {
        let probe_slot = slot(TextureWindow::power_of_two_tile(64, 64, 64, 64));
        let layer = LevelModelSecondaryLayer {
            texture_asset: None,
            blend_mode: model_override_blend::ADD_QUARTER,
            tint_rgb: [128; 3],
            motion: psx_level::LevelMaterialUvMotion {
                enabled: true,
                speed_u_q8: 2 * 256,
                speed_v_q8: 0,
                phase_u: 3,
                phase_v: 5,
            },
            flags: psx_level::material_flags::MODEL_REFLECTION_PROBE
                | (2 << psx_level::material_flags::MODEL_REFLECTION_ROUGHNESS_SHIFT)
                | (255 << psx_level::material_flags::MODEL_REFLECTION_STRENGTH_SHIFT),
        };
        let resolved = model_secondary_layer(
            layer,
            SimTick::from_u32(30),
            VideoHz::NTSC,
            Some(probe_slot),
            &mut |_| None,
        )
        .expect("resident room probe");
        assert_eq!(resolved.uv_offset, ModelUvOffset::ZERO);
        assert_eq!(
            resolved.uv_mapping,
            ModelUvMapping::ScreenSpaceReflection {
                texture_width: 64,
                texture_height: 64,
                roughness: 2,
                uv_offset: ModelUvOffset::new(4, 5),
            }
        );
    }

    #[test]
    fn override_sidedness_selects_cull_mode() {
        use psx_level::material_flags;
        assert_eq!(
            model_override_cull_mode(override_record(0, material_flags::FACE_FRONT)),
            CullMode::Back
        );
        assert_eq!(
            model_override_cull_mode(override_record(0, material_flags::FACE_BACK)),
            CullMode::Front
        );
        assert_eq!(
            model_override_cull_mode(override_record(0, material_flags::FACE_BOTH)),
            CullMode::None
        );
    }
}
