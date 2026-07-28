use super::geometry::*;
use super::*;
use crate::image_props::{average4_i32, average_vertex_rgb};
use crate::model_rendering::{
    model_override_blend_mode, model_render_uv_max, sphere_visible_to_camera,
};
use crate::room_lighting::RuntimeRoomLighting;
use crate::vram::VramSlot;
use psx_engine::{
    CullMode, DepthPolicy, LoadedWorldCameraGte, PrimitiveSink, ProjectedVertex, WorldCamera,
    WorldRenderPass, WorldSurfaceOptions,
};
use psx_gpu::{
    material::TextureMaterial,
    prim::{QuadTexturedGouraud, TriTextured, TriTexturedGouraud},
};
use psx_level::AssetId;

#[derive(Copy, Clone)]
struct BoxPropFaceTextureRuntime {
    material: TextureMaterial,
    u_max: u8,
    v_max: u8,
}

/// Break-time debris cache: a broken box's floor chips re-derived the
/// same world quads, UVs and bilinear base colours every frame (~36
/// multiplies per chip for the colours alone), all fixed the moment
/// the box breaks. The most recently drawn broken boxes keep those
/// values here; per frame only projection, fog and the submit remain.
/// A slot is filled EAGERLY for all chips when claimed, so there is no
/// partial-validity hazard; boxes beyond the pool simply refill on
/// their next draw (worst case equals the old recompute cost).
const DEBRIS_CACHE_SLOTS: usize = 16;

#[derive(Copy, Clone)]
struct DebrisChipCache {
    quad: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    material: TextureMaterial,
}

/// Owned break-time floor-debris cache (formerly the example's
/// `DEBRIS_CACHE*` statics). The game keeps one instance in its
/// runtime arenas; [`DebrisCache::init`] stamps the unclaimed-slot
/// sentinels onto the zeroed storage at boot. Chip validity rides in
/// a parallel flag array (not `Option`) so the zeroed image stays
/// all-zero bytes and the arena static stays in `.bss`.
pub struct DebrisCache {
    entries: [[DebrisChipCache; BOX_PROP_FLOOR_DEBRIS_CHIPS.len()]; DEBRIS_CACHE_SLOTS],
    valid: [[bool; BOX_PROP_FLOOR_DEBRIS_CHIPS.len()]; DEBRIS_CACHE_SLOTS],
    props: [usize; DEBRIS_CACHE_SLOTS],
    next: u8,
}

impl DebrisCache {
    /// All-zero state (link-time `.bss`-safe). NOT ready for use until
    /// [`Self::init`] stamps the unclaimed-slot sentinels.
    pub const fn zeroed() -> Self {
        // SAFETY: the cache is plain old data plus fieldless enums whose
        // discriminant 0 is a valid variant (`BlendMode::Opaque` inside
        // `TextureMaterial`); every chip read is gated by its `valid`
        // flag, zero (false) until a fill writes it.
        unsafe { core::mem::zeroed() }
    }

    /// Stamp the unclaimed-slot sentinels (what the old static
    /// initializer stored) onto the zeroed storage.
    pub fn init(&mut self) {
        self.props = [usize::MAX; DEBRIS_CACHE_SLOTS];
    }

    fn entries_for(
        &mut self,
        prop_index: usize,
        prop: &LevelBoxPropRecord,
        face_textures: &[Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT],
        bounds: BoxPropDebrisBounds,
        floor_y: i32,
    ) -> (
        &[DebrisChipCache; BOX_PROP_FLOOR_DEBRIS_CHIPS.len()],
        &[bool; BOX_PROP_FLOOR_DEBRIS_CHIPS.len()],
    ) {
        let mut i = 0usize;
        while i < DEBRIS_CACHE_SLOTS {
            if self.props[i] == prop_index {
                return (&self.entries[i], &self.valid[i]);
            }
            i += 1;
        }
        let slot = (self.next as usize) % DEBRIS_CACHE_SLOTS;
        self.next = self.next.wrapping_add(1);
        self.props[slot] = prop_index;
        let entries = &mut self.entries[slot];
        let valid = &mut self.valid[slot];
        let mut c = 0usize;
        while c < BOX_PROP_FLOOR_DEBRIS_CHIPS.len() {
            let chip = BOX_PROP_FLOOR_DEBRIS_CHIPS[c];
            let face = chip.face as usize;
            let texture = if face < psx_level::BOX_PROP_FACE_COUNT {
                face_textures[face]
            } else {
                None
            };
            match texture {
                Some(texture) => {
                    entries[c] = DebrisChipCache {
                        quad: box_prop_floor_debris_quad(bounds, floor_y, chip),
                        uvs: box_prop_floor_debris_uvs(texture.u_max, texture.v_max, chip),
                        colors: [
                            box_prop_face_color_at(prop, face, chip.u0_q8, chip.v0_q8),
                            box_prop_face_color_at(prop, face, chip.u1_q8, chip.v0_q8),
                            box_prop_face_color_at(prop, face, chip.u1_q8, chip.v1_q8),
                            box_prop_face_color_at(prop, face, chip.u0_q8, chip.v1_q8),
                        ],
                        material: texture.material,
                    };
                    valid[c] = true;
                }
                None => {
                    valid[c] = false;
                }
            }
            c += 1;
        }
        (&self.entries[slot], &self.valid[slot])
    }
}

fn box_prop_face_textures(
    prop: &LevelBoxPropRecord,
    prop_texture_slot: &mut impl FnMut(AssetId) -> Option<VramSlot>,
) -> [Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT] {
    let mut textures = [None; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        if let Some(texture_asset) = prop.texture_assets[face] {
            if let Some(slot) = prop_texture_slot(texture_asset) {
                textures[face] = Some(BoxPropFaceTextureRuntime {
                    material: TextureMaterial::opaque(
                        slot.clut_word,
                        slot.tpage_word,
                        (0x80, 0x80, 0x80),
                    )
                    .with_blend_mode(model_override_blend_mode(prop.blend_modes[face]))
                    .with_texture_window(slot.texture_window),
                    u_max: model_render_uv_max(slot.texture_width),
                    v_max: model_render_uv_max(slot.texture_height),
                });
            }
        }
        face += 1;
    }
    textures
}

/// Draw the unbroken box props of `current_room` (a falling box draws
/// shifted by its current fall offset).
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn draw_box_props<
    T,
    const MAX_BOX_PROP_STATE: usize,
    const BOX_PROP_BROKEN_WORDS: usize,
    const MAX_BOX_PROP_BREAK_EVENTS: usize,
    const OT_DEPTH: usize,
>(
    props: &[LevelBoxPropRecord],
    generated_surfaces: &[LevelBoxPropSurfaceRecord],
    state: &BoxProps<MAX_BOX_PROP_STATE, BOX_PROP_BROKEN_WORDS, MAX_BOX_PROP_BREAK_EVENTS>,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    mut prop_texture_slot: impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    // Box faces and nearby props commonly reuse one texture. Keep the last
    // resolved VRAM binding across the whole draw so six faces do not each
    // linearly scan the resident-slot table.
    let mut face_texture_cache: Option<(AssetId, BoxPropFaceTextureRuntime)> = None;
    let mut projector = None;
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room
            || box_prop_broken_in_words::<MAX_BOX_PROP_STATE>(&state.broken, index)
            || box_prop_broken_in_words::<MAX_BOX_PROP_STATE>(&state.door_open, index)
        {
            continue;
        }
        let Some(box_runtime) = state.runtime.get(index) else {
            continue;
        };
        // A box mid-fall is drawn shifted down by its current fall offset.
        let fall_y = state.fall[index].fall_y;
        let cull_center = WorldVertex::new(
            box_runtime.cull_center.x,
            box_runtime.cull_center.y.saturating_add(fall_y),
            box_runtime.cull_center.z,
        );
        if !sphere_visible_to_camera(camera, options, cull_center, box_runtime.cull_radius, 96) {
            continue;
        }
        let loaded_projector = match projector {
            Some(projector) => projector,
            None => {
                let loaded = LoadedWorldCameraGte::load(*camera);
                projector = Some(loaded);
                loaded
            }
        };
        if prop.surface_count == 0 {
            draw_box_prop_faces(
                prop,
                &box_runtime.faces,
                fall_y,
                loaded_projector,
                camera,
                options,
                lighting,
                &mut prop_texture_slot,
                &mut face_texture_cache,
                triangles,
                world,
            );
        } else {
            draw_generated_box_prop_surfaces(
                prop,
                generated_surfaces,
                fall_y,
                loaded_projector,
                camera,
                options,
                lighting,
                &mut prop_texture_slot,
                &mut face_texture_cache,
                triangles,
                world,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_generated_box_prop_surfaces<T, const OT_DEPTH: usize>(
    prop: &LevelBoxPropRecord,
    generated_surfaces: &[LevelBoxPropSurfaceRecord],
    fall_y: i32,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    prop_texture_slot: &mut impl FnMut(AssetId) -> Option<VramSlot>,
    face_texture_cache: &mut Option<(AssetId, BoxPropFaceTextureRuntime)>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    let first = usize::from(prop.surface_first);
    let end = first
        .saturating_add(usize::from(prop.surface_count))
        .min(generated_surfaces.len());
    for surface in generated_surfaces.get(first..end).unwrap_or(&[]) {
        let face = usize::from(surface.source_face);
        if face >= psx_level::BOX_PROP_FACE_COUNT {
            continue;
        }
        let face_runtime = BoxPropFaceRuntime {
            vertices: surface
                .vertices
                .map(|vertex| WorldVertex::new(vertex[0], vertex[1], vertex[2])),
            center: WorldVertex::new(surface.center[0], surface.center[1], surface.center[2]),
            normal: surface.normal,
        };
        if !box_prop_face_front_facing(camera, face_runtime) {
            continue;
        }
        let face_vertices = box_prop_offset_quad_y(face_runtime.vertices, fall_y);
        let Some(texture_asset) = prop.texture_assets[face] else {
            continue;
        };
        let face_texture = match *face_texture_cache {
            Some((cached_asset, cached)) if cached_asset == texture_asset => cached,
            _ => {
                let Some(slot) = prop_texture_slot(texture_asset) else {
                    continue;
                };
                let resolved = BoxPropFaceTextureRuntime {
                    material: TextureMaterial::opaque(
                        slot.clut_word,
                        slot.tpage_word,
                        (0x80, 0x80, 0x80),
                    )
                    .with_blend_mode(model_override_blend_mode(prop.blend_modes[face]))
                    .with_texture_window(slot.texture_window),
                    u_max: model_render_uv_max(slot.texture_width),
                    v_max: model_render_uv_max(slot.texture_height),
                };
                *face_texture_cache = Some((texture_asset, resolved));
                resolved
            }
        };
        let material = face_texture.material;
        let uvs = if surface.flags & psx_level::box_prop_surface_flags::UV_BAKED != 0 {
            surface.uv_q8.map(|uv| (uv[0], uv[1]))
        } else {
            surface
                .uv_q8
                .map(|uv| box_prop_face_uv_at(prop.uvs[face], uv))
        };
        let opts = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::None)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(0);
        if let Some(projected) = projector.project_world_quad(face_vertices) {
            let colors = [
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[0], projected[0].sz),
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[1], projected[1].sz),
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[2], projected[2].sz),
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[3], projected[3].sz),
            ];
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, &projected, &uvs, &colors, material, opts,
            );
        } else {
            let colors = [
                lighting.apply_vertex_fog(surface.baked_vertex_rgb[0], face_vertices[0]),
                lighting.apply_vertex_fog(surface.baked_vertex_rgb[1], face_vertices[1]),
                lighting.apply_vertex_fog(surface.baked_vertex_rgb[2], face_vertices[2]),
                lighting.apply_vertex_fog(surface.baked_vertex_rgb[3], face_vertices[3]),
            ];
            let tint = average_vertex_rgb(colors);
            let material = material.with_tint(tint);
            let opts = opts.with_material_layer(material);
            let _ = world.submit_textured_world_quad(
                triangles,
                *camera,
                face_vertices,
                uvs,
                material,
                opts,
            );
        }
    }
}

/// Draw the settled floor-debris chips of the broken box props of
/// `current_room` through the owned debris cache.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn draw_box_prop_floor_debris<
    T,
    const MAX_BOX_PROP_STATE: usize,
    const BOX_PROP_BROKEN_WORDS: usize,
    const MAX_BOX_PROP_BREAK_EVENTS: usize,
    const GTE_PROJECT: bool,
    const OT_DEPTH: usize,
>(
    props: &[LevelBoxPropRecord],
    state: &BoxProps<MAX_BOX_PROP_STATE, BOX_PROP_BROKEN_WORDS, MAX_BOX_PROP_BREAK_EVENTS>,
    debris: &mut DebrisCache,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    mut prop_texture_slot: impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let mut projector = None;
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room
            || !box_prop_broken_in_words::<MAX_BOX_PROP_STATE>(&state.broken, index)
        {
            continue;
        }
        let Some(box_runtime) = state.runtime.get(index) else {
            continue;
        };
        let debris_center = WorldVertex::new(
            box_runtime.cull_center.x,
            box_runtime.ground_y.saturating_add(16),
            box_runtime.cull_center.z,
        );
        if !sphere_visible_to_camera(
            camera,
            options,
            debris_center,
            box_runtime.cull_radius.saturating_mul(2),
            128,
        ) {
            continue;
        }
        let loaded_projector = match projector {
            Some(projector) => projector,
            None => {
                let loaded = LoadedWorldCameraGte::load(*camera);
                projector = Some(loaded);
                loaded
            }
        };
        let face_textures = box_prop_face_textures(prop, &mut prop_texture_slot);
        draw_box_prop_floor_debris_chips::<T, GTE_PROJECT, OT_DEPTH>(
            debris,
            index,
            prop,
            &face_textures,
            box_runtime.debris_bounds,
            box_runtime.ground_y,
            loaded_projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_floor_debris_chips<T, const GTE_PROJECT: bool, const OT_DEPTH: usize>(
    debris: &mut DebrisCache,
    prop_index: usize,
    prop: &LevelBoxPropRecord,
    face_textures: &[Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT],
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let (chips, valid) = debris.entries_for(prop_index, prop, face_textures, bounds, floor_y);
    for (chip, valid) in chips.iter().zip(valid.iter()) {
        if !valid {
            continue;
        }
        draw_box_prop_floor_debris_chip::<T, GTE_PROJECT, OT_DEPTH>(
            chip, projector, camera, options, lighting, triangles, world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_floor_debris_chip<T, const GTE_PROJECT: bool, const OT_DEPTH: usize>(
    chip: &DebrisChipCache,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let material = chip.material;
    let uvs = chip.uvs;
    let quad = chip.quad;
    let opts = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);
    if GTE_PROJECT {
        if let Some(projected) = projector.project_world_quad(quad) {
            let colors = [
                lighting.apply_vertex_fog_weight(
                    chip.colors[0],
                    lighting.fog_weight_at_depth(projected[0].sz),
                ),
                lighting.apply_vertex_fog_weight(
                    chip.colors[1],
                    lighting.fog_weight_at_depth(projected[1].sz),
                ),
                lighting.apply_vertex_fog_weight(
                    chip.colors[2],
                    lighting.fog_weight_at_depth(projected[2].sz),
                ),
                lighting.apply_vertex_fog_weight(
                    chip.colors[3],
                    lighting.fog_weight_at_depth(projected[3].sz),
                ),
            ];
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, &projected, &uvs, &colors, material, opts,
            );
            return;
        }
    }
    let colors = [
        lighting.apply_vertex_fog(chip.colors[0], quad[0]),
        lighting.apply_vertex_fog(chip.colors[1], quad[1]),
        lighting.apply_vertex_fog(chip.colors[2], quad[2]),
        lighting.apply_vertex_fog(chip.colors[3], quad[3]),
    ];
    if let Some(projected) = camera.project_world_quad(quad) {
        submit_projected_textured_gouraud_quad_u8(
            world, triangles, &projected, &uvs, &colors, material, opts,
        );
    } else {
        let tint = average_vertex_rgb(colors);
        let material = material.with_tint(tint);
        let opts = opts.with_material_layer(material);
        let _ = world.submit_textured_world_quad(triangles, *camera, quad, uvs, material, opts);
    }
}

/// Draw the live break bursts (flying shards) of `current_room`.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn draw_box_prop_break_events<
    T,
    const MAX_BOX_PROP_STATE: usize,
    const BOX_PROP_BROKEN_WORDS: usize,
    const MAX_BOX_PROP_BREAK_EVENTS: usize,
    const GTE_PROJECT: bool,
    const OT_DEPTH: usize,
>(
    props: &[LevelBoxPropRecord],
    state: &BoxProps<MAX_BOX_PROP_STATE, BOX_PROP_BROKEN_WORDS, MAX_BOX_PROP_BREAK_EVENTS>,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    mut prop_texture_slot: impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let mut projector = None;
    for event in &state.break_events {
        if !event.is_active() || event.age >= BOX_PROP_BREAK_FRAMES {
            continue;
        }
        let Some(prop) = props.get(event.prop_index as usize) else {
            continue;
        };
        if prop.room != current_room {
            continue;
        }
        let Some(box_runtime) = state.runtime.get(event.prop_index as usize) else {
            continue;
        };
        if !sphere_visible_to_camera(
            camera,
            options,
            box_runtime.cull_center,
            box_runtime.cull_radius.saturating_mul(3),
            128,
        ) {
            continue;
        }
        let loaded_projector = match projector {
            Some(projector) => projector,
            None => {
                let loaded = LoadedWorldCameraGte::load(*camera);
                projector = Some(loaded);
                loaded
            }
        };
        let face_textures = box_prop_face_textures(prop, &mut prop_texture_slot);
        draw_box_prop_break_shards::<T, GTE_PROJECT, OT_DEPTH>(
            &face_textures,
            &box_runtime.break_shards,
            *event,
            loaded_projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_break_shards<T, const GTE_PROJECT: bool, const OT_DEPTH: usize>(
    face_textures: &[Option<BoxPropFaceTextureRuntime>; psx_level::BOX_PROP_FACE_COUNT],
    shard_runtimes: &[BoxPropBreakShardRuntime; BOX_PROP_BREAK_SHARD_COUNT],
    event: BoxPropBreakEvent,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    for (shard_index, shard) in BOX_PROP_BREAK_SHARDS.iter().copied().enumerate() {
        if event.age < shard.delay {
            continue;
        }
        let shard_runtime = shard_runtimes[shard_index];
        let face = shard_runtime.face as usize;
        if face >= psx_level::BOX_PROP_FACE_COUNT {
            continue;
        }
        draw_box_prop_break_shard::<T, GTE_PROJECT, OT_DEPTH>(
            face_textures[face],
            shard_runtime,
            event,
            shard,
            shard_index,
            projector,
            camera,
            options,
            lighting,
            triangles,
            world,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_break_shard<T, const GTE_PROJECT: bool, const OT_DEPTH: usize>(
    face_texture: Option<BoxPropFaceTextureRuntime>,
    shard_runtime: BoxPropBreakShardRuntime,
    event: BoxPropBreakEvent,
    shard: BoxPropBreakShard,
    shard_index: usize,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    let Some(face_texture) = face_texture else {
        return;
    };

    let material = face_texture.material;
    let uvs = box_prop_shard_uvs(face_texture.u_max, face_texture.v_max, shard);
    let quad = box_prop_break_shard_quad(shard_runtime, event, shard, shard_index);
    let opts = options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);
    if GTE_PROJECT {
        if let Some(projected) = projector.project_world_quad(quad) {
            let fog_weight = lighting.fog_weight_at_depth(average4_i32(
                projected[0].sz,
                projected[1].sz,
                projected[2].sz,
                projected[3].sz,
            ));
            let colors = box_prop_apply_fog_weight(lighting, shard_runtime.colors, fog_weight);
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, &projected, &uvs, &colors, material, opts,
            );
            return;
        }
    }
    let center = box_prop_quad_center(quad);
    let fog_weight = lighting.fog_weight_at_depth(camera.view_vertex(center).z);
    let colors = box_prop_apply_fog_weight(lighting, shard_runtime.colors, fog_weight);
    if let Some(projected) = camera.project_world_quad(quad) {
        submit_projected_textured_gouraud_quad_u8(
            world, triangles, &projected, &uvs, &colors, material, opts,
        );
    } else {
        let tint = average_vertex_rgb(colors);
        let material = material.with_tint(tint);
        let opts = opts.with_material_layer(material);
        let _ = world.submit_textured_world_quad(triangles, *camera, quad, uvs, material, opts);
    }
}

#[inline]
fn submit_projected_textured_gouraud_quad_u8<T, const OT_DEPTH: usize>(
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut T,
    projected: &[ProjectedVertex; 4],
    uvs: &[(u8, u8); 4],
    colors: &[(u8, u8, u8); 4],
    material: TextureMaterial,
    options: WorldSurfaceOptions,
) where
    T: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
{
    let _ = world.submit_textured_gouraud_quad_prescreened_u8(
        triangles, projected, uvs, colors, material, options,
    );
}

fn box_prop_apply_fog_weight(
    lighting: &RuntimeRoomLighting,
    colors: [(u8, u8, u8); 4],
    fog_weight: i32,
) -> [(u8, u8, u8); 4] {
    [
        lighting.apply_vertex_fog_weight(colors[0], fog_weight),
        lighting.apply_vertex_fog_weight(colors[1], fog_weight),
        lighting.apply_vertex_fog_weight(colors[2], fog_weight),
        lighting.apply_vertex_fog_weight(colors[3], fog_weight),
    ]
}

#[allow(clippy::too_many_arguments)]
fn draw_box_prop_faces<T, const OT_DEPTH: usize>(
    prop: &LevelBoxPropRecord,
    faces: &[BoxPropFaceRuntime; psx_level::BOX_PROP_FACE_COUNT],
    fall_y: i32,
    projector: LoadedWorldCameraGte,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    prop_texture_slot: &mut impl FnMut(AssetId) -> Option<VramSlot>,
    face_texture_cache: &mut Option<(AssetId, BoxPropFaceTextureRuntime)>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        let face_runtime = faces[face];
        if !box_prop_face_front_facing(camera, face_runtime) {
            continue;
        }
        // A uniform Y shift while the box falls; facing is unchanged so the
        // front-facing test above still uses the resting normal/center.
        let face_vertices = box_prop_offset_quad_y(face_runtime.vertices, fall_y);
        let Some(texture_asset) = prop.texture_assets[face] else {
            continue;
        };
        let face_texture = match *face_texture_cache {
            Some((cached_asset, cached)) if cached_asset == texture_asset => cached,
            _ => {
                let Some(slot) = prop_texture_slot(texture_asset) else {
                    continue;
                };
                let resolved = BoxPropFaceTextureRuntime {
                    material: TextureMaterial::opaque(
                        slot.clut_word,
                        slot.tpage_word,
                        (0x80, 0x80, 0x80),
                    )
                    .with_blend_mode(model_override_blend_mode(prop.blend_modes[face]))
                    .with_texture_window(slot.texture_window),
                    u_max: model_render_uv_max(slot.texture_width),
                    v_max: model_render_uv_max(slot.texture_height),
                };
                *face_texture_cache = Some((texture_asset, resolved));
                resolved
            }
        };
        let material = face_texture.material;
        let uvs = prop.uvs[face];
        let opts = options
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::None)
            .with_material_layer(material)
            .with_textured_triangle_splitting(true)
            .with_textured_triangle_max_edge(0);
        if let Some(projected) = projector.project_world_quad(face_vertices) {
            // Projection already produced the four view depths. Reusing them
            // for fog avoids transforming every visible box-face vertex a
            // second time on the CPU (the normal path projects through GTE).
            let colors = [
                lighting.apply_fog_at_depth(prop.baked_vertex_rgb[face][0], projected[0].sz),
                lighting.apply_fog_at_depth(prop.baked_vertex_rgb[face][1], projected[1].sz),
                lighting.apply_fog_at_depth(prop.baked_vertex_rgb[face][2], projected[2].sz),
                lighting.apply_fog_at_depth(prop.baked_vertex_rgb[face][3], projected[3].sz),
            ];
            submit_projected_textured_gouraud_quad_u8(
                world, triangles, &projected, &uvs, &colors, material, opts,
            );
        } else {
            let colors = [
                lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][0], face_vertices[0]),
                lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][1], face_vertices[1]),
                lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][2], face_vertices[2]),
                lighting.apply_vertex_fog(prop.baked_vertex_rgb[face][3], face_vertices[3]),
            ];
            let tint = average_vertex_rgb(colors);
            let material = material.with_tint(tint);
            let opts = opts.with_material_layer(material);
            let _ = world.submit_textured_world_quad(
                triangles,
                *camera,
                face_vertices,
                uvs,
                material,
                opts,
            );
        }
    }
}

fn box_prop_face_uv_at(corners: [(u8, u8); 4], uv_q8: [u8; 2]) -> (u8, u8) {
    let u = u32::from(uv_q8[0]);
    let v = u32::from(uv_q8[1]);
    let inv_u = 255 - u;
    let inv_v = 255 - v;
    let interpolate = |axis: usize| {
        let values = if axis == 0 {
            [
                u32::from(corners[0].0),
                u32::from(corners[1].0),
                u32::from(corners[2].0),
                u32::from(corners[3].0),
            ]
        } else {
            [
                u32::from(corners[0].1),
                u32::from(corners[1].1),
                u32::from(corners[2].1),
                u32::from(corners[3].1),
            ]
        };
        let top = values[0] * inv_u + values[1] * u;
        let bottom = values[3] * inv_u + values[2] * u;
        ((top * inv_v + bottom * v + 32_512) / 65_025).min(255) as u8
    };
    (interpolate(0), interpolate(1))
}

fn box_prop_face_front_facing(camera: &WorldCamera, face: BoxPropFaceRuntime) -> bool {
    let [nx, ny, nz] = face.normal;
    let center = face.center;
    let vx = camera.position.x.saturating_sub(center.x);
    let vy = camera.position.y.saturating_sub(center.y);
    let vz = camera.position.z.saturating_sub(center.z);
    nx.saturating_mul(vx)
        .saturating_add(ny.saturating_mul(vy))
        .saturating_add(nz.saturating_mul(vz))
        > 0
}

fn box_prop_break_shard_quad(
    shard_runtime: BoxPropBreakShardRuntime,
    event: BoxPropBreakEvent,
    shard: BoxPropBreakShard,
    shard_index: usize,
) -> [WorldVertex; 4] {
    let age = event
        .age
        .saturating_sub(shard.delay)
        .min(BOX_PROP_BREAK_MOTION_FRAMES) as i32;
    let mut quad = shard_runtime.base_quad;
    let shard_center = shard_runtime.center;
    let edge_u = shard_runtime.edge_u;
    let edge_v = shard_runtime.edge_v;
    let spin_q12 = box_prop_break_shard_spin_q12(event.prop_index, shard_index, age);
    let outward_q8 = age.saturating_mul(age);
    let drift_q8 = (shard.drift_q8_per_frame as i32)
        .saturating_mul(age)
        .clamp(-96, 96);
    let twist_q8 = (shard.twist_q8_per_frame as i32)
        .saturating_mul(age)
        .clamp(-96, 96);
    let shrink_q8 = (252 - age.saturating_mul(3)).max(176);
    let impulse_units = age.saturating_mul(shard.impulse_per_frame as i32);
    let fall = age.saturating_mul(age).saturating_mul(4);
    let drift = scale_world_delta_q8(edge_u, drift_q8);
    let offset = [
        scale_q8_i32_signed(shard_runtime.face_delta[0], outward_q8)
            .saturating_add((event.impulse_x_q8 as i32).saturating_mul(impulse_units) / 256)
            .saturating_add(drift[0]),
        scale_q8_i32_signed(shard_runtime.face_delta[1], outward_q8)
            .saturating_add((shard.lift_per_frame as i32).saturating_mul(age))
            .saturating_sub(fall)
            .saturating_add(drift[1]),
        scale_q8_i32_signed(shard_runtime.face_delta[2], outward_q8)
            .saturating_add((event.impulse_z_q8 as i32).saturating_mul(impulse_units) / 256)
            .saturating_add(drift[2]),
    ];

    for (corner, vertex) in quad.iter_mut().enumerate() {
        let mut p = shrink_world_vertex_around(*vertex, shard_center, shrink_q8);
        let sign_u = if corner == 0 || corner == 3 { -1 } else { 1 };
        let sign_v = if corner == 0 || corner == 1 { -1 } else { 1 };
        let tumble_u = scale_world_delta_q8(edge_u, sign_v * twist_q8 / 2);
        let tumble_v = scale_world_delta_q8(edge_v, -sign_u * twist_q8);
        p = add_world_vertex_offset(p, tumble_u);
        p = add_world_vertex_offset(p, tumble_v);
        p = rotate_world_vertex_y_around_q12(p, shard_center, spin_q12);
        let landed = add_world_vertex_offset(p, offset);
        // Shift by the box's landed fall offset (0 for an in-place break),
        // then keep the fragment from sinking below the room floor: for an
        // elevated or fallen box this settles it on the ground rather than
        // the box's own (elevated) bottom.
        let y = landed.y.saturating_add(event.y_offset).max(event.ground_y);
        *vertex = WorldVertex::new(landed.x, y, landed.z);
    }
    quad
}

fn box_prop_break_shard_spin_q12(prop_index: u16, shard_index: usize, age: i32) -> u16 {
    let seed = (prop_index as u32)
        .wrapping_mul(73)
        .wrapping_add((shard_index as u32).wrapping_mul(151))
        .wrapping_add(0x4d3);
    let speed = 4 + (seed & 0x0f) as i32;
    let wobble = (((seed >> 5) & 0x07) as i32).saturating_sub(3);
    let signed = age.saturating_mul(speed.saturating_add(wobble).max(2));
    let spin = if seed & 0x10 == 0 { signed } else { -signed };
    spin.rem_euclid(4096) as u16
}

fn rotate_world_vertex_y_around_q12(
    vertex: WorldVertex,
    center: WorldVertex,
    angle_q12: u16,
) -> WorldVertex {
    if angle_q12 == 0 {
        return vertex;
    }
    let relative = [
        vertex.x.saturating_sub(center.x),
        vertex.y.saturating_sub(center.y),
        vertex.z.saturating_sub(center.z),
    ];
    let rotated = crate::image_props::rotate_y_q12(relative, angle_q12);
    WorldVertex::new(
        center.x.saturating_add(rotated[0]),
        center.y.saturating_add(rotated[1]),
        center.z.saturating_add(rotated[2]),
    )
}

fn box_prop_floor_debris_quad(
    bounds: BoxPropDebrisBounds,
    floor_y: i32,
    chip: BoxPropFloorDebrisChip,
) -> [WorldVertex; 4] {
    let base = bounds.span_x.max(bounds.span_z).max(128);
    let half_length = (base.saturating_mul(chip.half_length_q8 as i32) >> 8).clamp(32, base);
    let half_width = (base.saturating_mul(chip.half_width_q8 as i32) >> 8).clamp(16, base);
    let center_x = bounds
        .center_x
        .saturating_add(bounds.span_x.saturating_mul(chip.offset_x_q8 as i32) / 256);
    let center_z = bounds
        .center_z
        .saturating_add(bounds.span_z.saturating_mul(chip.offset_z_q8 as i32) / 256);
    let long = crate::image_props::rotate_y_q12([half_length, 0, 0], chip.yaw_q12);
    let short = crate::image_props::rotate_y_q12([0, 0, half_width], chip.yaw_q12);
    let y = floor_y.saturating_add(chip.lift as i32);
    [
        WorldVertex::new(
            center_x - long[0] - short[0],
            y,
            center_z - long[2] - short[2],
        ),
        WorldVertex::new(
            center_x + long[0] - short[0],
            y,
            center_z + long[2] - short[2],
        ),
        WorldVertex::new(
            center_x + long[0] + short[0],
            y,
            center_z + long[2] + short[2],
        ),
        WorldVertex::new(
            center_x - long[0] + short[0],
            y,
            center_z - long[2] + short[2],
        ),
    ]
}

fn box_prop_floor_debris_uvs(u_max: u8, v_max: u8, chip: BoxPropFloorDebrisChip) -> [(u8, u8); 4] {
    let u0 = uv_from_q8(u_max, chip.u0_q8);
    let u1 = uv_from_q8(u_max, chip.u1_q8);
    let v0 = uv_from_q8(v_max, chip.v0_q8);
    let v1 = uv_from_q8(v_max, chip.v1_q8);
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}

fn box_prop_shard_uvs(u_max: u8, v_max: u8, shard: BoxPropBreakShard) -> [(u8, u8); 4] {
    let u0 = uv_from_q8(u_max, shard.u0_q8);
    let u1 = uv_from_q8(u_max, shard.u1_q8);
    let v0 = uv_from_q8(v_max, shard.v0_q8);
    let v1 = uv_from_q8(v_max, shard.v1_q8);
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}
