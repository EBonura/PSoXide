//! XBSP world rendering through PSoXide's classic-affine path.
//!
//! Lifted from quake-psx `game/src/renderer.rs` commit 83a6349, same GPL-2
//! authorship. Frame lifecycle, packet storage and entity ownership are
//! caller-supplied so this module can serve both runtimes.

use alloc::vec;
use alloc::vec::Vec;

use psx_engine::{
    compose_classic_alias_transform, materialize_classic_affine_word_vertices,
    submit_classic_affine_batch, submit_classic_affine_windowed_batch,
    submit_classic_affine_windowed_fan, submit_classic_alias_model, ClassicAffineBatchSurface,
    ClassicAffineProfile, ClassicAffineSubmit, ClassicAffineVertex,
    ClassicAffineWindowedBatchSurface, ClassicAffineWordSourceVertex, ClassicAliasFace,
    ClassicAliasProjectedVertex, ClassicAliasVertex,
};
use psx_gpu::material::TextureWindow;
use psx_gpu::prim::ClassicTriTextured;
use psx_gte::math::{Mat3I16, Vec3I16 as GteVec3I16, Vec3I32 as GteVec3I32};
use psx_gte::scene::{self, AabbClipPlane};
use psx_math::int32::mul_q12_i32;
use psx_math::{cos_q12, sin_q12};

use crate::collision::BrushTransform;
use crate::pxbsp::{
    decompress_visibility, material_blend, material_flags, PxbspMaterial, PxbspMaterialAnimation,
    PXBSP_MAX_VISIBILITY_BYTES,
};
use crate::pxbsp_resident::PxbspResidentMap;
use crate::resident::ResidentMap;
use crate::{
    Face, Plane, TextureInfo, Vec3I16, Vec3I32, FACE_BACKSIDE, FACE_BAKED_LIGHT, FACE_BAKED_UV,
    FACE_TWO_SIDED, TEXTURE_INVISIBLE, TEXTURE_LIQUID, TEXTURE_NULL, TEXTURE_SKY,
};

/// Packet storage used by the original renderer's double-buffered arenas.
pub const DEFAULT_PACKET_WORDS: usize = 0x30000 / core::mem::size_of::<u32>();

// ponytail: these fixed arrays match the first XBSP format and PS1 budget;
// the PXBSP cook reports them and region paging removes the global ceilings.
const MAX_FACE_COUNT: usize = 32_767;
const BATCH_MAX_VERTICES: usize = 39;
const BATCH_MAX_SURFACES: usize = 13;
const SUBDIVISION_SCRATCH_VERTICES: usize = 12;
const MAX_ALIAS_VERTICES: usize = 512;
const MAX_RENDER_ENTITIES: usize = 512;
const CLUT_DEFAULT: u16 = 240 << 6;
const DUMMY_LIGHT_STYLE: usize = 64;
// Two-level subdivision emits at most 19 packets for one source triangle;
// 13 words covers the larger textured-Gouraud quad packet.
const WORST_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 13;
// A windowed polygon adds one self-contained GP0(E2) word per packet.
const WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 14;
const ALIAS_PACKET_WORDS: usize =
    core::mem::size_of::<ClassicTriTextured>() / core::mem::size_of::<u32>();
const ANIMATION_FRAMES_PER_SECOND: u32 = 30;
const SKY_SCROLL_TEXELS_PER_SECOND: u32 = 4;
const WATER_PHASE_PER_TEXEL_Q12: u32 = 326;
const WATER_PHASE_PER_FRAME_Q12: u32 = 22;
const WATER_AMPLITUDE_TEXELS: i32 = 2;
const ALIAS_MODEL_ROTATES: u8 = 8;
const PXBSP_MATERIAL_TICKS_PER_SECOND: i64 = 60;
const TEXTURED_GOURAUD_COMMAND: u32 = 0x3400_0000;
const SEMI_TRANSPARENT_COMMAND_BIT: u32 = 0x0200_0000;

/// Q20.12 world camera and Q0.12 turn angles.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Camera {
    pub origin: Vec3I32,
    pub angles: [i16; 3],
}

/// Camera transform retained for composing model-local alias transforms.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewTransform {
    pub rotation: Mat3I16,
    pub translation: GteVec3I32,
}

/// Runtime-neutral input for one retained alias-style model instance.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AliasEntity {
    pub origin: Vec3I32,
    pub angles: Vec3I16,
    pub model_id: i16,
    pub model_index: u16,
    pub frame: u16,
    pub skin: u8,
    pub clip_mins: [i16; 3],
    pub clip_maxs: [i16; 3],
    pub leaf_index: u16,
    pub light: u8,
}

/// VRAM binding resolved by the PSoXide runtime for one PXBSP material slot.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PxbspTextureBinding {
    pub texture_page: u16,
    pub clut: u16,
    pub texture_window_word: u32,
    pub uv_origin: [u8; 2],
    pub texture_size: [u8; 2],
}

/// Counts emitted by one world frame.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderStats {
    pub visible_faces: u16,
    pub surface_batches: u16,
    pub visible_entities: u16,
    pub alias_packets: u32,
    pub packets: u32,
    pub hardware_triangles: u32,
    pub unresolved_material_faces: u16,
    pub packet_overflow_avoided: bool,
}

/// Render result and the initialized prefix of caller-owned packet storage.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderFrame {
    pub stats: RenderStats,
    pub packet_words: usize,
}

#[derive(Copy, Clone)]
enum PxbspFaceSelection {
    VisibleWorld,
    ModelRange { first: usize, end: usize },
}

impl PxbspFaceSelection {
    fn range(self, face_count: usize) -> (usize, usize) {
        match self {
            Self::VisibleWorld => (0, face_count),
            Self::ModelRange { first, end } => (first.min(face_count), end.min(face_count)),
        }
    }

    fn includes(self, face: usize, visible_faces: &[u8]) -> bool {
        match self {
            Self::VisibleWorld => visible_faces.get(face).copied() == Some(1),
            Self::ModelRange { .. } => true,
        }
    }
}

/// Configure the 320x240 projection used by the lifted XBSP renderer.
pub fn configure_projection() {
    scene::set_screen_offset(160 << 16, 120 << 16);
    scene::set_projection_plane(160);
    scene::set_avsz_weights(0x155, 0x100);
}

/// Build and load the classic XBSP camera transform.
pub fn load_view(camera: Camera) -> ViewTransform {
    load_view_with_coordinates(
        camera,
        Mat3I16 {
            m: [[0, -0x3000, 0], [0, 0, -0x3000], [0x3000, 0, 0]],
        },
    )
}

/// Build and load the Y-up camera transform used by PSoXide brush worlds.
///
/// Zero angles look along world +X with +Y up; the remap must be a proper
/// rotation (determinant > 0) so the world keeps the editor's handedness:
/// view right = world +Z, view down = world -Y, view depth = world +X. The
/// previous `-Z` right axis was a reflection and drew every brush world as
/// its mirror image (models, which use the engine camera, were not
/// mirrored, so characters faced the wrong way and the analog stick read
/// mirrored).
pub fn load_pxbsp_view(camera: Camera) -> ViewTransform {
    load_view_with_coordinates(
        camera,
        Mat3I16 {
            m: [[0, 0, 0x3000], [0, -0x3000, 0], [0x3000, 0, 0]],
        },
    )
}

fn load_view_with_coordinates(camera: Camera, coordinates: Mat3I16) -> ViewTransform {
    let view = Mat3I16::rotate_xyz(
        (camera.angles[0] as u16) >> 4,
        (camera.angles[1] as u16) >> 4,
        (camera.angles[2] as u16) >> 4,
    );
    let rotation = scene::compose_rotation_scheduled(&view, &coordinates);
    scene::load_rotation(&rotation);
    scene::load_translation(GteVec3I32::ZERO);
    let translation = scene::transform_vertex_scheduled(GteVec3I16::new(
        (camera.origin.x.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (camera.origin.y.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (camera.origin.z.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
    ));
    scene::load_translation(translation);
    ViewTransform {
        rotation,
        translation,
    }
}

/// Cached PVS and projection scratch for the XBSP render path.
pub struct Renderer {
    frame: u32,
    face_visible: Vec<u8>,
    visibility: [u8; PXBSP_MAX_VISIBILITY_BYTES],
    visible_leaf_count: usize,
    cached_visibility: Option<(u32, usize)>,
    cached_pxbsp_visibility: Option<(u32, usize)>,
    alias_projected: Vec<ClassicAliasProjectedVertex>,
    visible_entity_indices: Vec<u16>,
    cached_frustum: Option<(Camera, [AabbClipPlane; 4])>,
    light_styles: [u16; DUMMY_LIGHT_STYLE + 1],
}

impl Renderer {
    pub fn new() -> Self {
        Self::with_capacities(MAX_FACE_COUNT, MAX_ALIAS_VERTICES, MAX_RENDER_ENTITIES)
    }

    /// Construct the render scratch needed by a validated PXBSP world.
    ///
    /// PXBSP skeletal entities are submitted by the game runtime, outside
    /// this world renderer. Sizing the face marks to the cooked map avoids
    /// reserving the legacy 32K-face XBSP ceiling on the PS1 heap.
    pub fn new_pxbsp(face_count: usize) -> Self {
        Self::with_capacities(face_count, 0, 0)
    }

    fn with_capacities(
        face_count: usize,
        alias_vertex_count: usize,
        render_entity_count: usize,
    ) -> Self {
        let mut light_styles = [256; DUMMY_LIGHT_STYLE + 1];
        light_styles[DUMMY_LIGHT_STYLE] = 0;
        Self {
            frame: 0,
            face_visible: vec![0; face_count],
            visibility: [0; PXBSP_MAX_VISIBILITY_BYTES],
            visible_leaf_count: 0,
            cached_visibility: None,
            cached_pxbsp_visibility: None,
            alias_projected: vec![ClassicAliasProjectedVertex::default(); alias_vertex_count],
            visible_entity_indices: Vec::with_capacity(render_entity_count),
            cached_frustum: None,
            light_styles,
        }
    }

    /// Materialize one world and alias-entity frame into caller-owned packets.
    pub fn draw_frame(
        &mut self,
        map: &ResidentMap,
        camera: Camera,
        view: ViewTransform,
        entities: &[AliasEntity],
        rotating_yaw: i16,
        packet_storage: &mut [u32],
    ) -> RenderFrame {
        scene::load_rotation(&view.rotation);
        scene::load_translation(view.translation);

        let start = packet_storage.as_mut_ptr();
        let end = unsafe { start.add(packet_storage.len()) };
        let mut next = start;
        let mut stats = RenderStats::default();

        let visibility_valid = self.mark_visible_faces(map, camera.origin);
        if visibility_valid {
            let mut batch_vertices =
                [ClassicAffineVertex::default(); BATCH_MAX_VERTICES + SUBDIVISION_SCRATCH_VERTICES];
            let mut batch_surfaces = [ClassicAffineBatchSurface::default(); BATCH_MAX_SURFACES];
            let mut batch_vertex_count = 0usize;
            let mut batch_surface_count = 0usize;
            let mut batch_worst_words = 0usize;

            let faces = map.faces();
            for face_index in 0..faces.len() {
                if self.face_visible[face_index] == 0 {
                    continue;
                }
                let face = unsafe { faces.get_unchecked(face_index) };
                let texture = unsafe { map.textures().get_unchecked(face.texture as usize) };
                if texture.flags & (TEXTURE_INVISIBLE | TEXTURE_NULL) != 0
                    || !front_facing(map, face, camera.origin)
                {
                    continue;
                }

                let vertex_count = face.vertex_count as usize;
                if vertex_count > BATCH_MAX_VERTICES {
                    stats.packet_overflow_avoided = true;
                    break;
                }
                if texture.flags & (TEXTURE_LIQUID | TEXTURE_SKY) != 0 {
                    if batch_surface_count != 0 {
                        stats.surface_batches = stats.surface_batches.saturating_add(1);
                    }
                    let submitted = unsafe {
                        flush_batch(
                            &mut batch_vertices,
                            batch_vertex_count,
                            &batch_surfaces,
                            batch_surface_count,
                            next,
                        )
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;

                    let face_worst_words =
                        (vertex_count - 2) * WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE;
                    if !packet_capacity(next, end, face_worst_words) {
                        stats.packet_overflow_avoided = true;
                        break;
                    }
                    self.materialize_face(map, face, texture, &mut batch_vertices[..vertex_count]);
                    animate_special_surface(
                        &mut batch_vertices[..vertex_count],
                        texture,
                        self.frame,
                    );
                    let submitted = unsafe {
                        submit_classic_affine_windowed_fan(
                            batch_vertices.as_mut_ptr(),
                            vertex_count,
                            next,
                            texture.texture_page,
                            CLUT_DEFAULT,
                            special_texture_window(texture).word(),
                            ClassicAffineProfile::QUAKE_REFERENCE,
                        )
                    };
                    next = submitted.next_packet;
                    stats.surface_batches = stats.surface_batches.saturating_add(1);
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    stats.visible_faces = stats.visible_faces.saturating_add(1);
                    continue;
                }

                let face_worst_words = (vertex_count - 2) * WORST_PACKET_WORDS_PER_TRIANGLE;
                if batch_vertex_count + vertex_count > BATCH_MAX_VERTICES
                    || batch_surface_count == BATCH_MAX_SURFACES
                    || !packet_capacity(next, end, batch_worst_words + face_worst_words)
                {
                    if batch_surface_count != 0 {
                        stats.surface_batches = stats.surface_batches.saturating_add(1);
                    }
                    let submitted = unsafe {
                        flush_batch(
                            &mut batch_vertices,
                            batch_vertex_count,
                            &batch_surfaces,
                            batch_surface_count,
                            next,
                        )
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;
                }
                if !packet_capacity(next, end, face_worst_words) {
                    stats.packet_overflow_avoided = true;
                    break;
                }

                batch_surfaces[batch_surface_count] = ClassicAffineBatchSurface {
                    first_vertex: batch_vertex_count as u16,
                    vertex_count: vertex_count as u16,
                    tpage: texture.texture_page,
                    clut: CLUT_DEFAULT,
                };
                self.materialize_face(
                    map,
                    face,
                    texture,
                    &mut batch_vertices[batch_vertex_count..batch_vertex_count + vertex_count],
                );
                batch_vertex_count += vertex_count;
                batch_surface_count += 1;
                batch_worst_words += face_worst_words;
                stats.visible_faces = stats.visible_faces.saturating_add(1);
            }

            if batch_surface_count != 0 {
                stats.surface_batches = stats.surface_batches.saturating_add(1);
            }
            let submitted = unsafe {
                flush_batch(
                    &mut batch_vertices,
                    batch_vertex_count,
                    &batch_surfaces,
                    batch_surface_count,
                    next,
                )
            };
            next = submitted.next_packet;
            stats.packets = stats.packets.wrapping_add(submitted.packets);
            stats.hardware_triangles = stats
                .hardware_triangles
                .wrapping_add(submitted.hardware_triangles);
        }

        if visibility_valid && !stats.packet_overflow_avoided {
            next = self.draw_entities(
                map,
                entities,
                rotating_yaw,
                camera,
                view,
                next,
                end,
                &mut stats,
            );
        }

        let packet_words = unsafe { next.offset_from(start) as usize };
        self.frame = self.frame.wrapping_add(1);
        RenderFrame {
            stats,
            packet_words,
        }
    }

    /// Materialize one PXBSP world frame into caller-owned packets.
    ///
    /// `materials` is indexed exactly like [`PxbspResidentMap::materials`].
    /// An unresolved entry skips its faces and increments
    /// [`RenderStats::unresolved_material_faces`]. Skeletal entities remain
    /// caller-owned and are submitted into the same ordering table after this
    /// staged packet stream.
    pub fn draw_pxbsp_world(
        &mut self,
        map: &PxbspResidentMap,
        camera: Camera,
        view: ViewTransform,
        materials: &[Option<PxbspTextureBinding>],
        material_tick: u32,
        packet_storage: &mut [u32],
    ) -> RenderFrame {
        scene::load_rotation(&view.rotation);
        scene::load_translation(view.translation);

        let frame = if self.mark_visible_pxbsp_faces(map, camera.origin) {
            self.draw_pxbsp_faces(
                map,
                camera.origin,
                materials,
                material_tick,
                PxbspFaceSelection::VisibleWorld,
                packet_storage,
            )
        } else {
            RenderFrame::default()
        };
        self.frame = self.frame.wrapping_add(1);
        frame
    }

    /// Materialize one transformed brush submodel into caller-owned packets.
    ///
    /// The model's vertices and planes remain model-local. `transform` is
    /// applied to the GTE render path and inverted for plane-side culling, so
    /// render and [`crate::collision::TransformedCollisionHull`] share one
    /// rigid-transform contract.
    pub fn draw_pxbsp_model(
        &mut self,
        map: &PxbspResidentMap,
        model_index: usize,
        transform: BrushTransform,
        camera: Camera,
        view: ViewTransform,
        materials: &[Option<PxbspTextureBinding>],
        material_tick: u32,
        packet_storage: &mut [u32],
    ) -> Option<RenderFrame> {
        let model = map.brush_models().get(model_index)?;
        let first_face = model.first_face as usize;
        let face_end = first_face.checked_add(model.face_count as usize)?;
        let local_camera = transform.point_to_local(camera.origin);
        let (rotation, translation) = compose_classic_alias_transform(
            view.rotation,
            view.translation,
            transform.rotation,
            GteVec3I16::ZERO,
            GteVec3I32::new(
                transform.origin.x >> 12,
                transform.origin.y >> 12,
                transform.origin.z >> 12,
            ),
            GteVec3I16::new(4096, 4096, 4096),
        );
        scene::load_rotation(&rotation);
        scene::load_translation(translation);

        // ponytail: the first mover slice scans its bounded face range. The
        // recursive near/far node walker with bounds culling replaces this
        // together with the world's current PVS-mark scan.
        let frame = self.draw_pxbsp_faces(
            map,
            local_camera,
            materials,
            material_tick,
            PxbspFaceSelection::ModelRange {
                first: first_face,
                end: face_end,
            },
            packet_storage,
        );
        self.frame = self.frame.wrapping_add(1);
        Some(frame)
    }

    fn draw_pxbsp_faces(
        &self,
        map: &PxbspResidentMap,
        camera_origin: Vec3I32,
        materials: &[Option<PxbspTextureBinding>],
        material_tick: u32,
        selection: PxbspFaceSelection,
        packet_storage: &mut [u32],
    ) -> RenderFrame {
        let start = packet_storage.as_mut_ptr();
        let end = unsafe { start.add(packet_storage.len()) };
        let mut next = start;
        let mut stats = RenderStats::default();

        let mut batch_vertices =
            [ClassicAffineVertex::default(); BATCH_MAX_VERTICES + SUBDIVISION_SCRATCH_VERTICES];
        let mut batch_surfaces = [ClassicAffineWindowedBatchSurface::default(); BATCH_MAX_SURFACES];
        let mut batch_vertex_count = 0usize;
        let mut batch_surface_count = 0usize;
        let mut batch_worst_words = 0usize;

        let faces = map.faces();
        let map_materials = map.materials();
        let (first_face, face_end) = selection.range(faces.len());
        for face_index in first_face..face_end {
            if !selection.includes(face_index, &self.face_visible) {
                continue;
            }
            let face = unsafe { faces.get_unchecked(face_index) };
            let material_index = face.texture as usize;
            let material = unsafe { map_materials.get_unchecked(material_index) };
            let Some(binding) = materials.get(material_index).copied().flatten() else {
                stats.unresolved_material_faces = stats.unresolved_material_faces.saturating_add(1);
                continue;
            };
            if !pxbsp_face_draws(
                material,
                face.flags,
                front_facing_pxbsp(map, face, camera_origin),
            ) {
                continue;
            }

            let vertex_count = face.vertex_count as usize;
            if vertex_count > BATCH_MAX_VERTICES {
                stats.packet_overflow_avoided = true;
                break;
            }
            let face_worst_words = (vertex_count - 2) * WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE;
            if batch_vertex_count + vertex_count > BATCH_MAX_VERTICES
                || batch_surface_count == BATCH_MAX_SURFACES
                || !packet_capacity(next, end, batch_worst_words + face_worst_words)
            {
                if batch_surface_count != 0 {
                    stats.surface_batches = stats.surface_batches.saturating_add(1);
                }
                let submitted = unsafe {
                    flush_windowed_batch(
                        &mut batch_vertices,
                        batch_vertex_count,
                        &batch_surfaces,
                        batch_surface_count,
                        next,
                    )
                };
                next = submitted.next_packet;
                stats.packets = stats.packets.wrapping_add(submitted.packets);
                stats.hardware_triangles = stats
                    .hardware_triangles
                    .wrapping_add(submitted.hardware_triangles);
                batch_vertex_count = 0;
                batch_surface_count = 0;
                batch_worst_words = 0;
            }
            if !packet_capacity(next, end, face_worst_words) {
                stats.packet_overflow_avoided = true;
                break;
            }

            let state = pxbsp_material_state(material, binding, material_tick);
            if material.flags & crate::pxbsp::material_flags::LAYERED_SKY != 0 {
                // quake-psx layered sky: the atlas holds a masked
                // foreground tile (left) and a solid background tile
                // (right); the same fan is emitted twice with per-layer
                // texture windows and scroll speeds. Background is
                // staged second so the tagged-stream prepend draws it
                // first; foreground black texels stay transparent.
                if batch_surface_count + 2 > BATCH_MAX_SURFACES
                    || !packet_capacity(next, end, batch_worst_words + face_worst_words * 2)
                {
                    if batch_surface_count != 0 {
                        stats.surface_batches = stats.surface_batches.saturating_add(1);
                    }
                    let submitted = unsafe {
                        flush_windowed_batch(
                            &mut batch_vertices,
                            batch_vertex_count,
                            &batch_surfaces,
                            batch_surface_count,
                            next,
                        )
                    };
                    next = submitted.next_packet;
                    stats.packets = stats.packets.wrapping_add(submitted.packets);
                    stats.hardware_triangles = stats
                        .hardware_triangles
                        .wrapping_add(submitted.hardware_triangles);
                    batch_vertex_count = 0;
                    batch_surface_count = 0;
                    batch_worst_words = 0;
                }
                let layers = layered_sky_batch_surfaces(
                    binding,
                    state,
                    batch_vertex_count as u16,
                    vertex_count as u16,
                    material_tick,
                );
                batch_surfaces[batch_surface_count] = layers[0];
                batch_surfaces[batch_surface_count + 1] = layers[1];
                self.materialize_pxbsp_face(
                    map,
                    face,
                    state.uv_offset,
                    &mut batch_vertices[batch_vertex_count..batch_vertex_count + vertex_count],
                );
                batch_vertex_count += vertex_count;
                batch_surface_count += 2;
                batch_worst_words += face_worst_words * 2;
                stats.visible_faces = stats.visible_faces.saturating_add(1);
                previous_advance_marker(&mut stats);
                continue;
            }
            batch_surfaces[batch_surface_count] = ClassicAffineWindowedBatchSurface {
                first_vertex: batch_vertex_count as u16,
                vertex_count: vertex_count as u16,
                tpage: state.texture_page,
                clut: binding.clut,
                // PXBSP vertices are materialized with the resolved layer
                // offset below, so the shared packet writer must not apply it
                // a second time.
                uv_offset: [0; 2],
                texture_window_word: binding.texture_window_word,
                color_command_word: state.color_command_word,
            };
            self.materialize_pxbsp_face(
                map,
                face,
                state.uv_offset,
                &mut batch_vertices[batch_vertex_count..batch_vertex_count + vertex_count],
            );
            batch_vertex_count += vertex_count;
            batch_surface_count += 1;
            batch_worst_words += face_worst_words;
            stats.visible_faces = stats.visible_faces.saturating_add(1);
        }

        if batch_surface_count != 0 {
            stats.surface_batches = stats.surface_batches.saturating_add(1);
        }
        let submitted = unsafe {
            flush_windowed_batch(
                &mut batch_vertices,
                batch_vertex_count,
                &batch_surfaces,
                batch_surface_count,
                next,
            )
        };
        next = submitted.next_packet;
        stats.packets = stats.packets.wrapping_add(submitted.packets);
        stats.hardware_triangles = stats
            .hardware_triangles
            .wrapping_add(submitted.hardware_triangles);

        let packet_words = unsafe { next.offset_from(start) as usize };
        RenderFrame {
            stats,
            packet_words,
        }
    }

    fn materialize_face(
        &self,
        map: &ResidentMap,
        face: Face,
        texture: TextureInfo,
        output: &mut [ClassicAffineVertex],
    ) {
        let first = face.first_vertex as usize;
        let baked_uv = face.flags & FACE_BAKED_UV != 0;
        let baked_light = face.flags & FACE_BAKED_LIGHT != 0;
        let style0 = self.light_styles[face.light_styles[0] as usize];
        let style1 = self.light_styles[face.light_styles[1] as usize];
        let source = map.vertex_data();
        let source_offset = first * core::mem::size_of::<ClassicAffineWordSourceVertex>();
        let source_ptr = unsafe { source.as_ptr().add(source_offset) };
        debug_assert_eq!(source_ptr as usize & 3, 0);
        unsafe {
            materialize_classic_affine_word_vertices(
                source_ptr.cast::<ClassicAffineWordSourceVertex>(),
                output.len(),
                output.as_mut_ptr(),
                [texture.atlas.x, texture.atlas.y],
                [style0, style1],
                baked_uv,
                baked_light,
            );
        }
        if baked_light {
            // ponytail: commit 83a6349 maps can carry grayscale bake overflow
            // in the GP0 command byte; saturate until the cooker clamps and
            // regenerated assets make every baked color a clean RGB24 word.
            for vertex in output {
                vertex.color = normalize_baked_color(vertex.color);
            }
        }
    }

    fn materialize_pxbsp_face(
        &self,
        map: &PxbspResidentMap,
        face: Face,
        uv_offset: [u8; 2],
        output: &mut [ClassicAffineVertex],
    ) {
        let first = face.first_vertex as usize;
        let baked_uv = face.flags & FACE_BAKED_UV != 0;
        let baked_light = face.flags & FACE_BAKED_LIGHT != 0;
        let style0 = self.light_styles[face.light_styles[0] as usize];
        let style1 = self.light_styles[face.light_styles[1] as usize];
        let source = map.vertex_data();
        let source_offset = first * core::mem::size_of::<ClassicAffineWordSourceVertex>();
        let source_ptr = unsafe { source.as_ptr().add(source_offset) };
        debug_assert_eq!(source_ptr as usize & 3, 0);
        unsafe {
            materialize_classic_affine_word_vertices(
                source_ptr.cast::<ClassicAffineWordSourceVertex>(),
                output.len(),
                output.as_mut_ptr(),
                uv_offset,
                [style0, style1],
                baked_uv,
                baked_light,
            );
        }
        if baked_light {
            for vertex in output {
                vertex.color = normalize_baked_color(vertex.color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_entities(
        &mut self,
        map: &ResidentMap,
        entities: &[AliasEntity],
        rotating_yaw: i16,
        camera: Camera,
        view: ViewTransform,
        mut next: *mut u32,
        end: *mut u32,
        stats: &mut RenderStats,
    ) -> *mut u32 {
        self.visible_entity_indices.clear();
        let frustum = if let Some((cached_camera, cached_frustum)) = self.cached_frustum {
            if cached_camera == camera {
                cached_frustum
            } else {
                let frustum = view_frustum(camera);
                self.cached_frustum = Some((camera, frustum));
                frustum
            }
        } else {
            let frustum = view_frustum(camera);
            self.cached_frustum = Some((camera, frustum));
            frustum
        };
        scene::load_aabb_clip4(&frustum);
        for (index, entity) in entities.iter().enumerate() {
            if !self.point_visible(entity.leaf_index as usize) {
                continue;
            }
            if !scene::aabb_outside_clip4(entity.clip_mins, entity.clip_maxs, &frustum, 0x0f) {
                if self.visible_entity_indices.len() == self.visible_entity_indices.capacity() {
                    stats.packet_overflow_avoided = true;
                    break;
                }
                self.visible_entity_indices.push(index as u16);
            }
        }

        let models = map.alias_models();
        for visible in 0..self.visible_entity_indices.len() {
            let entity = &entities[self.visible_entity_indices[visible] as usize];
            let Some(model) = models.model_at(entity.model_index as usize) else {
                continue;
            };
            debug_assert_eq!(model.header().id, entity.model_id);
            let header = model.header();
            let face_count = header.triangle_count as usize;
            let Some(worst_words) = face_count.checked_mul(ALIAS_PACKET_WORDS) else {
                stats.packet_overflow_avoided = true;
                break;
            };
            if !packet_capacity(next, end, worst_words) {
                stats.packet_overflow_avoided = true;
                break;
            }

            let frame = (entity.frame as usize).min(header.frame_count as usize - 1);
            let skin = (entity.skin as usize).min(header.skin_count as usize - 1);
            let vertices = model
                .frame_bytes(frame)
                .expect("validated alias-model frame");
            let faces = model
                .triangle_bytes(skin)
                .expect("validated alias-model skin");
            debug_assert_eq!(vertices.len(), header.vertex_count as usize * 3);
            debug_assert_eq!(
                faces.len(),
                face_count * core::mem::size_of::<ClassicAliasFace>()
            );
            debug_assert_eq!(faces.as_ptr() as usize & 3, 0);

            let yaw = if header.flags & ALIAS_MODEL_ROTATES != 0 {
                rotating_yaw
            } else {
                entity.angles.y
            };
            let model_rotation = Mat3I16::rotate_z((yaw as u16) >> 4)
                .mul(&Mat3I16::rotate_y((entity.angles.x as u16) >> 4));
            let (rotation, translation) = compose_classic_alias_transform(
                view.rotation,
                view.translation,
                model_rotation,
                GteVec3I16::new(header.offset.x, header.offset.y, header.offset.z),
                GteVec3I32::new(
                    entity.origin.x >> 12,
                    entity.origin.y >> 12,
                    entity.origin.z >> 12,
                ),
                GteVec3I16::new(header.scale.x, header.scale.y, header.scale.z),
            );
            scene::load_rotation(&rotation);
            scene::load_translation(translation);
            let light = entity.light as u32;
            let tint = light | (light << 8) | (light << 16);
            let submitted = unsafe {
                submit_classic_alias_model(
                    vertices.as_ptr().cast::<ClassicAliasVertex>(),
                    header.vertex_count as usize,
                    faces.as_ptr().cast::<ClassicAliasFace>(),
                    face_count,
                    self.alias_projected.as_mut_ptr(),
                    next,
                    header.skins[skin].texture_page,
                    CLUT_DEFAULT,
                    tint,
                    ClassicAffineProfile::QUAKE_REFERENCE,
                )
            };
            next = submitted.next_packet;
            stats.visible_entities = stats.visible_entities.saturating_add(1);
            stats.alias_packets = stats.alias_packets.wrapping_add(submitted.packets);
            stats.packets = stats.packets.wrapping_add(submitted.packets);
            stats.hardware_triangles = stats
                .hardware_triangles
                .wrapping_add(submitted.hardware_triangles);
        }
        next
    }

    fn point_visible(&self, leaf_index: usize) -> bool {
        if leaf_index == 0 {
            return false;
        }
        let visible_index = leaf_index - 1;
        visible_index < self.visible_leaf_count
            && self.visibility[visible_index >> 3] & (1 << (visible_index & 7)) != 0
    }

    fn mark_visible_faces(&mut self, map: &ResidentMap, point: Vec3I32) -> bool {
        self.cached_pxbsp_visibility = None;
        let faces = map.faces();
        if faces.len() > self.face_visible.len() {
            return false;
        }
        let Some(leaf_index) = map.point_leaf_index(point) else {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        };
        if leaf_index == 0 {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }
        if self.cached_visibility == Some((map.generation(), leaf_index)) {
            return true;
        }
        self.face_visible[..faces.len()].fill(0);
        let leaf = map.leaves().get(leaf_index).expect("validated leaf");
        if leaf.visibility_offset < 0 {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }

        let world = map.brush_models().get(0).expect("validated world model");
        let visible_leaves = world.visible_leaves.max(0) as usize;
        let row_bytes = (visible_leaves + 7) >> 3;
        if row_bytes > self.visibility.len() {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }
        self.visibility.fill(0);
        if !decompress_visibility(
            map.visibility(),
            leaf.visibility_offset as usize,
            &mut self.visibility[..row_bytes],
        ) {
            self.cached_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }

        let leaves = map.leaves();
        let marks = map.mark_surfaces();
        for visible_index in 0..visible_leaves {
            if self.visibility[visible_index >> 3] & (1 << (visible_index & 7)) == 0 {
                continue;
            }
            let Some(leaf) = leaves.get(visible_index + 1) else {
                return false;
            };
            let start = leaf.first_mark_surface as usize;
            let end = start + leaf.mark_surface_count as usize;
            for mark_index in start..end {
                let face = marks.get(mark_index).expect("validated mark surface") as usize;
                self.face_visible[face] = 1;
            }
        }
        self.visible_leaf_count = visible_leaves;
        self.cached_visibility = Some((map.generation(), leaf_index));
        true
    }

    fn mark_visible_pxbsp_faces(&mut self, map: &PxbspResidentMap, point: Vec3I32) -> bool {
        self.cached_visibility = None;
        let faces = map.faces();
        if faces.len() > self.face_visible.len() {
            self.cached_pxbsp_visibility = None;
            return false;
        }
        let Some(leaf_index) = map.point_leaf_index(point) else {
            self.cached_pxbsp_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        };
        if leaf_index == 0 {
            self.cached_pxbsp_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }
        if self.cached_pxbsp_visibility == Some((map.generation(), leaf_index)) {
            return true;
        }
        self.face_visible[..faces.len()].fill(0);
        let leaf = map.leaves().get(leaf_index).expect("validated leaf");
        if leaf.visibility_offset < 0 {
            self.cached_pxbsp_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }

        let world = map.brush_models().get(0).expect("validated world model");
        let visible_leaves = world.visible_leaves.max(0) as usize;
        let row_bytes = (visible_leaves + 7) >> 3;
        if row_bytes > self.visibility.len() {
            self.cached_pxbsp_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }
        self.visibility.fill(0);
        if !decompress_visibility(
            map.visibility(),
            leaf.visibility_offset as usize,
            &mut self.visibility[..row_bytes],
        ) {
            self.cached_pxbsp_visibility = None;
            self.visible_leaf_count = 0;
            return false;
        }

        let leaves = map.leaves();
        let marks = map.mark_surfaces();
        for visible_index in 0..visible_leaves {
            if self.visibility[visible_index >> 3] & (1 << (visible_index & 7)) == 0 {
                continue;
            }
            let Some(leaf) = leaves.get(visible_index + 1) else {
                return false;
            };
            let start = leaf.first_mark_surface as usize;
            let end = start + leaf.mark_surface_count as usize;
            for mark_index in start..end {
                let face = marks.get(mark_index).expect("validated mark surface") as usize;
                self.face_visible[face] = 1;
            }
        }
        self.visible_leaf_count = visible_leaves;
        self.cached_pxbsp_visibility = Some((map.generation(), leaf_index));
        true
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

fn view_frustum(camera: Camera) -> [AabbClipPlane; 4] {
    let yaw = camera.angles[1] as u16 & 0x0fff;
    let pitch = camera.angles[0] as u16 & 0x0fff;
    let roll = camera.angles[2] as u16 & 0x0fff;
    let sy = sin_q12(yaw);
    let cy = cos_q12(yaw);
    let sp = sin_q12(pitch);
    let cp = cos_q12(pitch);
    let sr = sin_q12(roll);
    let cr = cos_q12(roll);
    let multiply = |left: i32, right: i32| mul_q12_i32(left, right);
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

    let forward = [clamp(multiply(cp, cy)), clamp(multiply(cp, sy)), clamp(-sp)];
    let right = [
        clamp(multiply(multiply(-sr, sp), cy) + multiply(-cr, -sy)),
        clamp(multiply(multiply(-sr, sp), sy) + multiply(-cr, cy)),
        clamp(multiply(-sr, cp)),
    ];
    let up = [
        clamp(multiply(multiply(cr, sp), cy) + multiply(-sr, -sy)),
        clamp(multiply(multiply(cr, sp), sy) + multiply(-sr, cy)),
        clamp(multiply(cr, cp)),
    ];
    let normals = [
        add_normal(forward, right),
        subtract_normal(forward, right),
        add_normal(forward, up),
        subtract_normal(forward, up),
    ];
    normals.map(|normal| {
        let distance = mul_q12_i32(camera.origin.x, normal[0] as i32)
            .saturating_add(mul_q12_i32(camera.origin.y, normal[1] as i32))
            .saturating_add(mul_q12_i32(camera.origin.z, normal[2] as i32));
        let signbits = u8::from(normal[0] < 0)
            | (u8::from(normal[1] < 0) << 1)
            | (u8::from(normal[2] < 0) << 2);
        AabbClipPlane {
            normal,
            kind: 3,
            signbits,
            distance,
        }
    })
}

fn add_normal(left: [i16; 3], right: [i16; 3]) -> [i16; 3] {
    [
        left[0].saturating_add(right[0]),
        left[1].saturating_add(right[1]),
        left[2].saturating_add(right[2]),
    ]
}

fn subtract_normal(left: [i16; 3], right: [i16; 3]) -> [i16; 3] {
    [
        left[0].saturating_sub(right[0]),
        left[1].saturating_sub(right[1]),
        left[2].saturating_sub(right[2]),
    ]
}

fn animate_special_surface(vertices: &mut [ClassicAffineVertex], texture: TextureInfo, frame: u32) {
    if texture.flags & TEXTURE_LIQUID != 0 {
        let time_phase = frame.wrapping_mul(WATER_PHASE_PER_FRAME_Q12);
        for vertex in vertices {
            let local_u = vertex.uv[0].wrapping_sub(texture.atlas.x) as u32;
            let local_v = vertex.uv[1].wrapping_sub(texture.atlas.y) as u32;
            let u_phase = ((local_v
                .wrapping_mul(WATER_PHASE_PER_TEXEL_Q12)
                .wrapping_add(time_phase))
                & 0x0fff) as u16;
            let v_phase = ((local_u
                .wrapping_mul(WATER_PHASE_PER_TEXEL_Q12)
                .wrapping_add(time_phase))
                & 0x0fff) as u16;
            let u_offset = (sin_q12(u_phase) * WATER_AMPLITUDE_TEXELS) >> 12;
            let v_offset = (sin_q12(v_phase) * WATER_AMPLITUDE_TEXELS) >> 12;
            vertex.uv[0] = vertex.uv[0].wrapping_add(u_offset as u8);
            vertex.uv[1] = vertex.uv[1].wrapping_add(v_offset as u8);
        }
    } else if texture.flags & TEXTURE_SKY != 0 {
        let scroll = frame.wrapping_mul(SKY_SCROLL_TEXELS_PER_SECOND) / ANIMATION_FRAMES_PER_SECOND;
        for vertex in vertices {
            vertex.uv[0] = vertex.uv[0].wrapping_add(scroll as u8);
        }
    }
}

fn special_texture_window(texture: TextureInfo) -> TextureWindow {
    let width = (texture.size.x.max(4) as u16 * 2).min(128) as u8;
    let mask_x = texture_window_mask(width);
    let offset_x = texture.atlas.x / 8;
    if texture.flags & TEXTURE_LIQUID != 0 {
        let height = (texture.size.y.max(8) as u16).min(128) as u8;
        TextureWindow::new(
            mask_x,
            texture_window_mask(height),
            offset_x,
            texture.atlas.y / 8,
        )
    } else {
        // The legacy atlas may place sky rows at a non-window-aligned Y.
        // Only U scrolls, so leave V unmasked and preserve its exact address.
        TextureWindow::new(mask_x, 0, offset_x, 0)
    }
}

fn texture_window_mask(size: u8) -> u8 {
    (((!(size - 1)) as u16 & 0x00ff) as u8) / 8
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PxbspMaterialState {
    texture_page: u16,
    color_command_word: u32,
    uv_offset: [u8; 2],
}
/// Placeholder for loop-position bookkeeping in the sky branch; the
/// per-face loop advances `face_index` in the `for` header, so nothing
/// extra is needed. Kept as a named no-op for readability.
#[inline(always)]
fn previous_advance_marker(_stats: &mut RenderStats) {}

/// quake-psx layered sky layers for one fan: masked foreground (left
/// half of the atlas) and solid background (right half), each a
/// power-of-two texture window with its own scroll offset. Scroll
/// rates mirror quake-psx (foreground 8s, background 16s full cycles
/// at 60 material ticks per second).
fn layered_sky_batch_surfaces(
    binding: PxbspTextureBinding,
    state: PxbspMaterialState,
    first_vertex: u16,
    vertex_count: u16,
    material_tick: u32,
) -> [ClassicAffineWindowedBatchSurface; 2] {
    const SKY_FOREGROUND_CYCLE_SECONDS: u32 = 8;
    const SKY_BACKGROUND_CYCLE_SECONDS: u32 = 16;
    const MATERIAL_TICKS_PER_SECOND: u32 = 60;
    let width = (binding.texture_size[0] / 2).clamp(8, 128);
    let height = binding.texture_size[1].clamp(8, 128);
    let foreground_window =
        TextureWindow::power_of_two_tile(binding.uv_origin[0], binding.uv_origin[1], width, height)
            .word();
    let background_window = TextureWindow::power_of_two_tile(
        binding.uv_origin[0].wrapping_add(width),
        binding.uv_origin[1],
        width,
        height,
    )
    .word();
    let scroll = |cycle_seconds: u32| {
        ((u64::from(material_tick) * u64::from(width)
            / u64::from(MATERIAL_TICKS_PER_SECOND * cycle_seconds))
            & 0xff) as u8
    };
    let layer = |window: u32, scroll: u8| ClassicAffineWindowedBatchSurface {
        first_vertex,
        vertex_count,
        tpage: state.texture_page,
        clut: binding.clut,
        uv_offset: [scroll, scroll],
        texture_window_word: window,
        color_command_word: state.color_command_word,
    };
    // Foreground staged first: tagged packets prepend at equal OT
    // depth, so the background executes first and the masked
    // foreground draws over it.
    [
        layer(foreground_window, scroll(SKY_FOREGROUND_CYCLE_SECONDS)),
        layer(background_window, scroll(SKY_BACKGROUND_CYCLE_SECONDS)),
    ]
}

fn pxbsp_material_state(
    material: PxbspMaterial,
    binding: PxbspTextureBinding,
    tick: u32,
) -> PxbspMaterialState {
    let blend_bits = match material.blend_mode {
        material_blend::ADD => 1,
        material_blend::SUBTRACT => 2,
        material_blend::ADD_QUARTER => 3,
        _ => 0,
    };
    let texture_page = (binding.texture_page & !0x0060) | (blend_bits << 5);
    let color_command_word = TEXTURED_GOURAUD_COMMAND
        | if material.blend_mode == material_blend::OPAQUE {
            0
        } else {
            SEMI_TRANSPARENT_COMMAND_BIT
        };
    let animation = material
        .animation()
        .expect("resident PXBSP material was validated");
    let animated = pxbsp_animation_offset(animation, binding.texture_size, tick);
    PxbspMaterialState {
        texture_page,
        color_command_word,
        uv_offset: [
            binding.uv_origin[0].wrapping_add(animated[0]),
            binding.uv_origin[1].wrapping_add(animated[1]),
        ],
    }
}

fn pxbsp_animation_offset(
    animation: PxbspMaterialAnimation,
    texture_size: [u8; 2],
    tick: u32,
) -> [u8; 2] {
    match animation {
        PxbspMaterialAnimation::Static => [0; 2],
        PxbspMaterialAnimation::UvScroll {
            speed_u_q8,
            speed_v_q8,
            phase_u,
            phase_v,
        } => [
            pxbsp_scroll_axis(speed_u_q8, phase_u, texture_size[0], tick),
            pxbsp_scroll_axis(speed_v_q8, phase_v, texture_size[1], tick),
        ],
        PxbspMaterialAnimation::Flipbook {
            columns,
            rows,
            frame_count,
            ticks_per_frame,
            phase,
        } => {
            let frame =
                ((tick / u32::from(ticks_per_frame)) + u32::from(phase)) % u32::from(frame_count);
            let frame_width = (texture_size[0] / columns).max(1);
            let frame_height = (texture_size[1] / rows).max(1);
            [
                (frame as u8 % columns).wrapping_mul(frame_width),
                (frame as u8 / columns).wrapping_mul(frame_height),
            ]
        }
    }
}

fn pxbsp_scroll_axis(speed_q8: i16, phase: u8, period: u8, tick: u32) -> u8 {
    let travelled_q8 =
        i64::from(speed_q8).saturating_mul(i64::from(tick)) / PXBSP_MATERIAL_TICKS_PER_SECOND;
    (travelled_q8 / 256 + i64::from(phase)).rem_euclid(i64::from(period.max(1))) as u8
}

fn pxbsp_face_draws(material: PxbspMaterial, face_flags: u16, authored_front: bool) -> bool {
    if face_flags & FACE_TWO_SIDED != 0 {
        return true;
    }
    match material.flags & material_flags::FACE_MASK {
        material_flags::FACE_BACK => !authored_front,
        material_flags::FACE_BOTH => true,
        _ => authored_front,
    }
}

#[inline]
fn packet_capacity(next: *mut u32, end: *mut u32, needed_words: usize) -> bool {
    // `ptr.add(needed_words)` would itself be undefined if the speculative
    // result crossed the arena. Both pointers are members of one slice.
    let remaining = unsafe { end.offset_from(next) };
    remaining >= 0 && needed_words <= remaining as usize
}

unsafe fn flush_batch(
    vertices: &mut [ClassicAffineVertex],
    vertex_count: usize,
    surfaces: &[ClassicAffineBatchSurface],
    surface_count: usize,
    output: *mut u32,
) -> ClassicAffineSubmit {
    if vertex_count == 0 || surface_count == 0 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    unsafe {
        submit_classic_affine_batch(
            vertices.as_mut_ptr(),
            vertex_count,
            surfaces.as_ptr(),
            surface_count,
            output,
            ClassicAffineProfile::QUAKE_REFERENCE,
        )
    }
}

unsafe fn flush_windowed_batch(
    vertices: &mut [ClassicAffineVertex],
    vertex_count: usize,
    surfaces: &[ClassicAffineWindowedBatchSurface],
    surface_count: usize,
    output: *mut u32,
) -> ClassicAffineSubmit {
    if vertex_count == 0 || surface_count == 0 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    unsafe {
        submit_classic_affine_windowed_batch(
            vertices.as_mut_ptr(),
            vertex_count,
            surfaces.as_ptr(),
            surface_count,
            output,
            ClassicAffineProfile::QUAKE_REFERENCE,
        )
    }
}

fn front_facing(map: &ResidentMap, face: Face, point: Vec3I32) -> bool {
    let plane = unsafe { map.planes().get_unchecked(face.plane as usize) };
    let behind = plane_distance(plane, point) < 0;
    behind == (face.flags & FACE_BACKSIDE != 0)
}

fn front_facing_pxbsp(map: &PxbspResidentMap, face: Face, point: Vec3I32) -> bool {
    let plane = unsafe { map.planes().get_unchecked(face.plane as usize) };
    let behind = plane_distance(plane, point) < 0;
    behind == (face.flags & FACE_BACKSIDE != 0)
}

fn plane_distance(plane: Plane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32(point.x, plane.normal.x as i32)
            .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
            .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
    };
    dot.saturating_sub(plane.distance)
}

fn normalize_baked_color(color: u32) -> u32 {
    if color & 0xff00_0000 == 0 {
        color
    } else {
        0x00ff_ffff
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxbsp::PxbspLumpKind;
    use crate::pxbsp_resident::tests::{valid_lumps, write_file};
    use crate::SliceReader;
    use psx_engine::OtFrame;
    use psx_gpu::ot::OrderingTable;

    #[test]
    fn panorama_inserted_after_pxbsp_executes_before_a_same_slot_world_packet() {
        let mut ot_storage = OrderingTable::<8>::new();
        let mut ot = OtFrame::begin(&mut ot_storage);
        let mut pxbsp_packet = [0u32, 0x5058_4253];
        let mut panorama_packet = [0u32, 0x534b_5920];

        unsafe {
            // Mirrors the PSoXide scene contract: link the PXBSP tagged
            // stream first, then the panorama into the same farthest slot.
            ot.add_raw(7, pxbsp_packet.as_mut_ptr(), 1);
            ot.add_raw(7, panorama_packet.as_mut_ptr(), 1);
        }
        drop(ot);

        let mut packets = unsafe { ot_storage.iter_packets() };
        assert_eq!(
            packets.next().expect("panorama packet").0,
            panorama_packet.as_ptr()
        );
        assert_eq!(
            packets.next().expect("PXBSP packet").0,
            pxbsp_packet.as_ptr()
        );
        assert!(packets.next().is_none());
    }

    #[test]
    fn pxbsp_view_projects_y_up_with_positive_z_on_the_right() {
        // Zero angles look along +X with +Y up. In a right-handed Y-up world
        // (the editor's and the model pipeline's convention) the view's right
        // is +Z, so +Z must land right of centre; the old remap put it on the
        // left and mirrored every brush world.
        psx_gte::host::reset();
        configure_projection();
        load_pxbsp_view(Camera {
            origin: Vec3I32::default(),
            angles: [0; 3],
        });
        let centered = scene::project_vertex(GteVec3I16::new(128, 0, 0));
        let above = scene::project_vertex(GteVec3I16::new(128, 64, 0));
        let positive_z = scene::project_vertex(GteVec3I16::new(128, 0, 64));
        assert_eq!((centered.sx, centered.sy), (160, 120));
        assert!(above.sy < centered.sy);
        assert!(positive_z.sx > centered.sx);

        // A quarter turn of yaw looks along -Z (the engine feeds
        // `orbit_yaw + 1024`, so a camera north of its target at engine
        // yaw 0 looks south).
        load_pxbsp_view(Camera {
            origin: Vec3I32::default(),
            angles: [0, 1024, 0],
        });
        let turned = scene::project_vertex(GteVec3I16::new(0, 0, -128));
        assert_eq!((turned.sx, turned.sy), (160, 120));
        assert!(turned.sz > 0);
    }

    #[test]
    fn rejects_zero_length_visibility_runs() {
        let mut output = [0xff; 2];
        assert!(!decompress_visibility(&[0, 0], 0, &mut output));
    }

    #[test]
    fn expands_visibility_runs_to_the_exact_row() {
        let mut output = [0xff; 4];
        assert!(decompress_visibility(&[0x11, 0, 2, 0x80], 0, &mut output));
        assert_eq!(output, [0x11, 0, 0, 0x80]);
    }

    #[test]
    fn saturates_legacy_baked_light_carry_before_packet_submission() {
        assert_eq!(normalize_baked_color(0x0105_0505), 0x00ff_ffff);
        assert_eq!(normalize_baked_color(0x0012_3456), 0x0012_3456);
    }

    #[test]
    fn resolves_pxbsp_blend_and_scroll_state() {
        let material = PxbspMaterial {
            blend_mode: material_blend::SUBTRACT,
            animation_kind: crate::pxbsp::material_animation::UV_SCROLL,
            animation_data: [0x00, 0x01, 0x00, 0xff, 7, 9, 0],
            ..PxbspMaterial::default()
        };
        let binding = PxbspTextureBinding {
            texture_page: 0x1234,
            clut: 0x4567,
            texture_window_word: 0xe200_0000,
            uv_origin: [8, 16],
            texture_size: [64, 32],
        };
        let state = pxbsp_material_state(material, binding, 60);
        assert_eq!((state.texture_page >> 5) & 3, 2);
        assert_eq!(state.color_command_word, 0x3600_0000);
        assert_eq!(state.uv_offset, [16, 24]);
    }

    #[test]
    fn resolves_pxbsp_flipbook_cells_at_fixed_ticks() {
        let animation = PxbspMaterialAnimation::Flipbook {
            columns: 4,
            rows: 2,
            frame_count: 7,
            ticks_per_frame: 3,
            phase: 1,
        };
        assert_eq!(pxbsp_animation_offset(animation, [128, 64], 9), [0, 32]);
    }

    #[test]
    fn pxbsp_material_sidedness_follows_authored_face() {
        let front = PxbspMaterial::default();
        let back = PxbspMaterial {
            flags: material_flags::FACE_BACK,
            ..front
        };
        let both = PxbspMaterial {
            flags: material_flags::FACE_BOTH,
            ..front
        };
        assert!(pxbsp_face_draws(front, 0, true));
        assert!(!pxbsp_face_draws(front, 0, false));
        assert!(!pxbsp_face_draws(back, 0, true));
        assert!(pxbsp_face_draws(back, 0, false));
        assert!(pxbsp_face_draws(both, 0, true));
        assert!(pxbsp_face_draws(both, 0, false));
        assert!(pxbsp_face_draws(front, FACE_TWO_SIDED, true));
        assert!(pxbsp_face_draws(front, FACE_TWO_SIDED, false));
    }

    #[test]
    fn draws_checked_pxbsp_material_into_windowed_packets() {
        configure_projection();
        let mut lumps = valid_lumps();
        let mut vertices = Vec::new();
        for position in [[64i16, -16, -16], [64, 16, -16], [64, 0, 16]] {
            for component in position {
                vertices.extend_from_slice(&component.to_le_bytes());
            }
            vertices.extend_from_slice(&[0, 0, 128, 0, 0, 0]);
        }
        lumps[PxbspLumpKind::Vertices as usize] = vertices;
        lumps[PxbspLumpKind::Materials as usize][7] = material_blend::SUBTRACT;
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(7, &mut SliceReader::new(&bytes))
            .expect("resident map");

        let camera = Camera {
            origin: Vec3I32 {
                x: 1 << 12,
                y: 0,
                z: 0,
            },
            angles: [0; 3],
        };
        let binding = PxbspTextureBinding {
            texture_page: 0x0105,
            clut: 0x1234,
            texture_window_word: 0xe200_0000,
            uv_origin: [0; 2],
            texture_size: [64; 2],
        };
        let mut packets = [0u32; 512];
        let mut renderer = Renderer::new_pxbsp(map.faces().len());
        assert_eq!(map.point_leaf_index(camera.origin), Some(1));
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        assert_eq!(renderer.face_visible[0], 1);
        assert!(front_facing_pxbsp(
            &map,
            map.faces().get(0).expect("face"),
            camera.origin
        ));
        assert!(pxbsp_face_draws(
            map.materials().get(0).expect("material"),
            map.faces().get(0).expect("face").flags,
            true
        ));
        assert_eq!(map.faces().get(0).expect("face").texture, 0);
        let frame = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[Some(binding)],
            0,
            &mut packets,
        );

        assert_eq!(renderer.face_visible[0], 1);
        assert_eq!(frame.stats.visible_faces, 1);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert!(frame.stats.packets > 0);
        let mut offset = 0usize;
        let mut packet_count = 0u32;
        while offset < frame.packet_words {
            let data_words = (packets[offset] >> 24) as usize;
            assert!(matches!(packets[offset + 2] >> 24, 0x36 | 0x3e));
            assert_eq!(packets[offset + 1], binding.texture_window_word);
            assert_eq!(packets[offset + 4] >> 16, u32::from(binding.clut));
            assert_eq!((packets[offset + 7] >> 21) & 3, 2);
            offset += data_words + 1;
            packet_count += 1;
        }
        assert_eq!(offset, frame.packet_words);
        assert_eq!(packet_count, frame.stats.packets);
    }

    #[test]
    fn pxbsp_renderer_allocates_only_world_face_marks() {
        let renderer = Renderer::new_pxbsp(37);
        assert_eq!(renderer.face_visible.len(), 37);
        assert!(renderer.alias_projected.is_empty());
        assert_eq!(renderer.visible_entity_indices.capacity(), 0);
    }

    #[test]
    fn draws_selected_brush_model_under_world_transform() {
        configure_projection();
        let mut lumps = valid_lumps();
        let mut vertices = Vec::new();
        for position in [[64i16, -16, -16], [64, 16, -16], [64, 0, 16]] {
            for component in position {
                vertices.extend_from_slice(&component.to_le_bytes());
            }
            vertices.extend_from_slice(&[0, 0, 128, 0, 0, 0]);
        }
        lumps[PxbspLumpKind::Vertices as usize] = vertices;
        let model_bytes = lumps[PxbspLumpKind::Models as usize].clone();
        lumps[PxbspLumpKind::Models as usize].extend_from_slice(&model_bytes);
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(8, &mut SliceReader::new(&bytes))
            .expect("resident map");

        let camera = Camera {
            origin: Vec3I32 {
                x: 129 << 12,
                y: 0,
                z: 0,
            },
            angles: [0; 3],
        };
        let transform = BrushTransform::translated(Vec3I32 {
            x: 128 << 12,
            y: 0,
            z: 0,
        });
        let binding = PxbspTextureBinding {
            texture_page: 0x0105,
            clut: 0x1234,
            texture_window_word: 0xe200_0000,
            uv_origin: [0; 2],
            texture_size: [64; 2],
        };
        let mut packets = [0u32; 512];
        let mut renderer = Renderer::new();
        let frame = renderer
            .draw_pxbsp_model(
                &map,
                1,
                transform,
                camera,
                load_pxbsp_view(camera),
                &[Some(binding)],
                0,
                &mut packets,
            )
            .expect("brush model");

        assert_eq!(frame.stats.visible_faces, 1);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert!(frame.stats.packets > 0);
        assert!(renderer
            .draw_pxbsp_model(
                &map,
                2,
                transform,
                camera,
                load_pxbsp_view(camera),
                &[Some(binding)],
                0,
                &mut packets,
            )
            .is_none());
    }
}
