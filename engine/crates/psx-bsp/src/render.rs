//! XBSP world rendering through PSoXide's classic-affine path.
//!
//! Lifted from quake-psx `game/src/renderer.rs` commit 83a6349, same GPL-2
//! authorship. Frame lifecycle, packet storage and entity ownership are
//! caller-supplied so this module can serve both runtimes.

use alloc::vec;
use alloc::vec::Vec;

use psx_engine::{
    compose_classic_alias_transform, materialize_classic_affine_word_vertices,
    submit_classic_affine_batch, submit_classic_affine_scoped_windowed_batch,
    submit_classic_affine_scoped_windowed_fan, submit_classic_alias_model,
    ClassicAffineBatchSurface, ClassicAffineProfile, ClassicAffineSubmit, ClassicAffineVertex,
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
use crate::sky::{submit_view_ray_layered_sky, VIEW_RAY_SKY_PACKET_WORDS};
use crate::{
    Face, Plane, TextureInfo, Vec3I16, Vec3I32, FACE_BACKSIDE, FACE_BAKED_LIGHT, FACE_BAKED_UV,
    FACE_TWO_SIDED, TEXTURE_INVISIBLE, TEXTURE_LIQUID, TEXTURE_NULL, TEXTURE_SKY,
};

/// Packet storage used by the original renderer's double-buffered arenas.
pub const DEFAULT_PACKET_WORDS: usize = 0x30000 / core::mem::size_of::<u32>();

// ponytail: these fixed arrays match the first XBSP format and PS1 budget;
// the PXBSP cook reports them and region paging removes the global ceilings.
const MAX_FACE_COUNT: usize = 32_767;
/// Initial PXBSP face-chain storage. Typical leaf PVS sets are far smaller
/// than the whole map (E1M1 is 484/5,724); vectors may still grow for denser
/// maps without permanently reserving two complete face tables on PS1.
const PXBSP_NODE_POSTORDER: u32 = 1 << 31;

#[inline]
fn packed_face_state(states: &[u8], index: usize) -> u8 {
    (states[index >> 2] >> ((index & 3) << 1)) & 3
}

#[inline]
fn set_packed_face_state(states: &mut [u8], index: usize, state: u8) {
    let shift = (index & 3) << 1;
    let byte = &mut states[index >> 2];
    *byte = (*byte & !(3 << shift)) | ((state & 3) << shift);
}

#[inline]
fn packed_bit(bits: &[u8], index: usize) -> bool {
    bits[index >> 3] & (1 << (index & 7)) != 0
}

#[inline]
fn set_packed_bit(bits: &mut [u8], index: usize) {
    bits[index >> 3] |= 1 << (index & 7);
}
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
// A scoped windowed polygon adds its GP0(E2) selector and full-window reset.
const WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 15;
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
    fn range(self, face_count: usize, visible_face_count: usize) -> (usize, usize) {
        match self {
            Self::VisibleWorld => (0, visible_face_count),
            Self::ModelRange { first, end } => (first.min(face_count), end.min(face_count)),
        }
    }

    fn face_index(self, index: usize, visible_faces: &[u16]) -> usize {
        match self {
            Self::VisibleWorld => visible_faces[index] as usize,
            Self::ModelRange { .. } => index,
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
/// Projection parameters the brush-face frustum clip is built from: the GTE
/// H register and the screen half-extents the caller projects into, plus a
/// pixel margin kept outside the visible edge and the near distance in world
/// units.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewProjection {
    pub focal_length: i32,
    pub half_width: i32,
    pub half_height: i32,
    pub edge_margin: i32,
    pub near_world: i32,
}

impl ViewProjection {
    /// The widest projection any consumer uses (H=160, 90 degree horizontal
    /// FOV): a consumer that never calls `set_view_projection` clips less
    /// than it could, never more.
    pub const DEFAULT: Self = Self {
        focal_length: 160,
        half_width: 160,
        half_height: 120,
        edge_margin: 16,
        near_world: 8,
    };
}

/// Frustum planes in the space the loaded GTE view transforms from (world
/// for the world model, model-local for movers): `dot(normal, p) + offset >= 0`
/// keeps `p`. Normals are the view rows combined per plane (Q12), offsets are
/// in the same Q12 domain; dot products run in i64.
///
/// Brush faces are not clipped by the GPU: a face with a vertex behind the
/// camera projects to garbage or to a primitive beyond the 1023x511 GPU limit
/// and is dropped whole, which blacks out entire rooms when the third-person
/// camera is cornered. Clipping the polygon here, before projection, keeps
/// every emitted vertex inside the frustum, so nothing the GPU receives can
/// be oversize.
#[derive(Copy, Clone, Debug)]
pub struct FrustumPlanes {
    planes: [([i32; 3], i64); 5],
}

impl FrustumPlanes {
    /// Build from the view rotation/translation currently loaded in the GTE.
    /// The view may carry a uniform scale (PXBSP loads x3); the near distance
    /// is scaled by the row length so `near_world` stays in world units.
    pub fn from_view(
        rotation: &Mat3I16,
        translation: GteVec3I32,
        projection: ViewProjection,
    ) -> Self {
        let row = |i: usize| -> [i32; 3] {
            [
                rotation.m[i][0] as i32,
                rotation.m[i][1] as i32,
                rotation.m[i][2] as i32,
            ]
        };
        let (right, down, forward) = (row(0), row(1), row(2));
        let scale_q12 = isqrt_i64(
            forward[0] as i64 * forward[0] as i64
                + forward[1] as i64 * forward[1] as i64
                + forward[2] as i64 * forward[2] as i64,
        ) as i32;
        let near_view = projection.near_world.saturating_mul(scale_q12) >> 12;
        // near: forward.p + T.z >= near_view
        let near = (forward, ((translation.z - near_view) as i64) << 12);
        // side planes: |view_x| <= (half_w + margin)/H * view_z, same for y.
        let kx = projection.half_width + projection.edge_margin;
        let ky = projection.half_height + projection.edge_margin;
        let h = projection.focal_length.max(1);
        let combine = |axis: [i32; 3], axis_t: i32, k: i32, sign: i32| -> ([i32; 3], i64) {
            let n = [
                k * forward[0] - sign * h * axis[0],
                k * forward[1] - sign * h * axis[1],
                k * forward[2] - sign * h * axis[2],
            ];
            let c =
                (k as i64 * translation.z as i64 - sign as i64 * h as i64 * axis_t as i64) << 12;
            (n, c)
        };
        Self {
            planes: [
                near,
                combine(right, translation.x, kx, 1),
                combine(right, translation.x, kx, -1),
                combine(down, translation.y, ky, 1),
                combine(down, translation.y, ky, -1),
            ],
        }
    }

    #[inline]
    fn distance(plane: &([i32; 3], i64), position: [i16; 3]) -> i64 {
        plane.0[0] as i64 * position[0] as i64
            + plane.0[1] as i64 * position[1] as i64
            + plane.0[2] as i64 * position[2] as i64
            + plane.1
    }

    /// True when the whole axis-aligned box lies outside any clip plane.
    /// Testing the vertex farthest along each plane normal makes this a
    /// conservative hierarchical reject; intersecting nodes still reach the
    /// exact polygon clip below.
    fn aabb_outside(&self, mins: Vec3I16, maxs: Vec3I16) -> bool {
        let mins = [mins.x, mins.y, mins.z];
        let maxs = [maxs.x, maxs.y, maxs.z];
        self.planes.iter().any(|plane| {
            let positive = [
                if plane.0[0] >= 0 { maxs[0] } else { mins[0] },
                if plane.0[1] >= 0 { maxs[1] } else { mins[1] },
                if plane.0[2] >= 0 { maxs[2] } else { mins[2] },
            ];
            Self::distance(plane, positive) < 0
        })
    }

    /// Clip a convex polygon in place. Returns the vertex count after
    /// clipping (0 when nothing remains). `vertices` must have room for
    /// `count + 5` records; `scratch` the same.
    pub fn clip_polygon(
        &self,
        vertices: &mut [ClassicAffineVertex],
        count: usize,
        scratch: &mut [ClassicAffineVertex],
    ) -> usize {
        let mut count = count;
        // Trivial accept: every vertex inside every plane (the common case).
        let mut all_inside = true;
        'planes: for plane in &self.planes {
            for v in &vertices[..count] {
                if Self::distance(plane, v.position) < 0 {
                    all_inside = false;
                    break 'planes;
                }
            }
        }
        if all_inside {
            return count;
        }
        for plane in &self.planes {
            if count < 3 {
                return 0;
            }
            let mut out = 0usize;
            let mut prev = vertices[count - 1];
            let mut prev_d = Self::distance(plane, prev.position);
            for i in 0..count {
                let cur = vertices[i];
                let cur_d = Self::distance(plane, cur.position);
                if (prev_d >= 0) != (cur_d >= 0) {
                    // Crossing: interpolate at t = prev_d / (prev_d - cur_d), Q16.
                    let t = ((prev_d << 16) / (prev_d - cur_d)).clamp(0, 1 << 16);
                    scratch[out] = lerp_vertex(&prev, &cur, t);
                    out += 1;
                }
                if cur_d >= 0 {
                    scratch[out] = cur;
                    out += 1;
                }
                prev = cur;
                prev_d = cur_d;
            }
            vertices[..out].copy_from_slice(&scratch[..out]);
            count = out;
        }
        if count < 3 {
            0
        } else {
            count
        }
    }
}

fn lerp_vertex(
    a: &ClassicAffineVertex,
    b: &ClassicAffineVertex,
    t_q16: i64,
) -> ClassicAffineVertex {
    let lerp_i = |x: i32, y: i32| -> i32 { (x as i64 + (((y - x) as i64 * t_q16) >> 16)) as i32 };
    let lerp_u8 = |x: u8, y: u8| -> u8 { lerp_i(x as i32, y as i32).clamp(0, 255) as u8 };
    let ca = a.color;
    let cb = b.color;
    let color = (lerp_u8(ca as u8, cb as u8) as u32)
        | ((lerp_u8((ca >> 8) as u8, (cb >> 8) as u8) as u32) << 8)
        | ((lerp_u8((ca >> 16) as u8, (cb >> 16) as u8) as u32) << 16)
        | (ca & 0xff00_0000);
    ClassicAffineVertex {
        position: [
            lerp_i(a.position[0] as i32, b.position[0] as i32).clamp(-32768, 32767) as i16,
            lerp_i(a.position[1] as i32, b.position[1] as i32).clamp(-32768, 32767) as i16,
            lerp_i(a.position[2] as i32, b.position[2] as i32).clamp(-32768, 32767) as i16,
        ],
        uv: [lerp_u8(a.uv[0], b.uv[0]), lerp_u8(a.uv[1], b.uv[1])],
        color,
        screen: [0, 0],
        depth: 0,
    }
}

fn isqrt_i64(value: i64) -> i64 {
    if value <= 0 {
        return 0;
    }
    let mut x = value;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + value / x) / 2;
    }
    x
}

pub struct Renderer {
    frame: u32,
    face_visible: Vec<u8>,
    /// Two-bit PXBSP face state: 0 hidden, 1 PVS fallback, 2 node-owned PVS.
    pxbsp_face_state: Vec<u8>,
    pxbsp_face_count: usize,
    /// Sorted, unique PVS face chain for PXBSP worlds. Quake-PSX builds the
    /// same kind of surface chain during BSP traversal; retaining it across
    /// frames avoids scanning the complete face table while the view leaf is
    /// unchanged. `pxbsp_face_state` remains the packed deduplication table.
    visible_pxbsp_faces: Vec<u16>,
    /// Per-frame subset left after hierarchical node-frustum rejection.
    frame_pxbsp_faces: Vec<u16>,
    /// Cached PVS reachability for the compact PXBSP render tree.
    pxbsp_node_visible: Vec<u8>,
    pxbsp_node_discovered: Vec<u8>,
    pxbsp_node_count: usize,
    pxbsp_node_stack: Vec<u32>,
    pxbsp_node_metadata_valid: bool,
    /// Node ranges are empty, so visible faces are gathered from the marked
    /// leaves reached by the bounds traversal instead of node-owned ranges.
    pxbsp_node_leaf_marks: bool,
    visibility: [u8; PXBSP_MAX_VISIBILITY_BYTES],
    visible_leaf_count: usize,
    cached_visibility: Option<(u32, usize)>,
    cached_pxbsp_visibility: Option<(u32, usize)>,
    alias_projected: Vec<ClassicAliasProjectedVertex>,
    visible_entity_indices: Vec<u16>,
    cached_frustum: Option<(Camera, [AabbClipPlane; 4])>,
    light_styles: [u16; DUMMY_LIGHT_STYLE + 1],
    /// Projection the brush-face frustum clip assumes (see
    /// [`Renderer::set_view_projection`]).
    view_projection: ViewProjection,
}

impl Renderer {
    pub fn new() -> Self {
        Self::with_capacities(MAX_FACE_COUNT, MAX_ALIAS_VERTICES, MAX_RENDER_ENTITIES)
    }

    /// Declare the projection the caller renders with (GTE H and screen
    /// half-extents), so the brush-face frustum clip matches it.
    pub fn set_view_projection(&mut self, projection: ViewProjection) {
        self.view_projection = projection;
    }

    /// Construct the render scratch needed by a validated PXBSP world.
    ///
    /// PXBSP skeletal entities are submitted by the game runtime, outside
    /// this world renderer. Sizing the face marks to the cooked map avoids
    /// reserving the legacy 32K-face XBSP ceiling on the PS1 heap.
    pub fn new_pxbsp(face_count: usize) -> Self {
        Self::new_pxbsp_with_nodes(face_count, 0)
    }

    /// Construct PXBSP scratch with the render-tree capacity known up front,
    /// avoiding heap growth on the first PVS rebuild during gameplay.
    pub fn new_pxbsp_with_nodes(face_count: usize, node_count: usize) -> Self {
        let mut renderer = Self::with_capacities(0, 0, 0);
        // These chains are persistent world scratch. Allocate their exact
        // upper bound once: a first full-level PVS can exceed a small starter
        // capacity, and repeated Vec growth leaks peak space from the PS1's
        // bump-style boot heap before the first frame is presented.
        let chain_capacity = face_count;
        renderer.pxbsp_face_state = vec![0; face_count.div_ceil(4)];
        renderer.pxbsp_face_count = face_count;
        renderer.visible_pxbsp_faces = Vec::with_capacity(chain_capacity);
        renderer.frame_pxbsp_faces = Vec::with_capacity(chain_capacity);
        renderer.pxbsp_node_visible = vec![0; node_count.div_ceil(8)];
        renderer.pxbsp_node_discovered = vec![0; node_count.div_ceil(8)];
        renderer.pxbsp_node_count = node_count;
        renderer.pxbsp_node_stack = Vec::with_capacity(node_count.min(128));
        renderer
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
            pxbsp_face_state: Vec::new(),
            pxbsp_face_count: 0,
            visible_pxbsp_faces: Vec::new(),
            frame_pxbsp_faces: Vec::new(),
            pxbsp_node_visible: Vec::new(),
            pxbsp_node_discovered: Vec::new(),
            pxbsp_node_count: 0,
            pxbsp_node_stack: Vec::new(),
            pxbsp_node_metadata_valid: false,
            pxbsp_node_leaf_marks: false,
            visibility: [0; PXBSP_MAX_VISIBILITY_BYTES],
            visible_leaf_count: 0,
            cached_visibility: None,
            cached_pxbsp_visibility: None,
            alias_projected: vec![ClassicAliasProjectedVertex::default(); alias_vertex_count],
            visible_entity_indices: Vec::with_capacity(render_entity_count),
            cached_frustum: None,
            view_projection: ViewProjection::DEFAULT,
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
                        submit_classic_affine_scoped_windowed_fan(
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

        let frustum =
            FrustumPlanes::from_view(&view.rotation, view.translation, self.view_projection);
        let frame = if self.mark_visible_pxbsp_faces(map, camera.origin)
            && self.select_frame_pxbsp_faces(map, camera.origin, &frustum)
        {
            self.draw_pxbsp_faces(
                map,
                camera.origin,
                &frustum,
                materials,
                material_tick,
                PxbspFaceSelection::VisibleWorld,
                Some(view),
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
        let frustum = FrustumPlanes::from_view(&rotation, translation, self.view_projection);

        // ponytail: the first mover slice scans its bounded face range. The
        // recursive near/far node walker with bounds culling replaces this
        // together with the world's current PVS-mark scan.
        let frame = self.draw_pxbsp_faces(
            map,
            local_camera,
            &frustum,
            materials,
            material_tick,
            PxbspFaceSelection::ModelRange {
                first: first_face,
                end: face_end,
            },
            None,
            packet_storage,
        );
        self.frame = self.frame.wrapping_add(1);
        Some(frame)
    }

    fn draw_pxbsp_faces(
        &self,
        map: &PxbspResidentMap,
        camera_origin: Vec3I32,
        frustum: &FrustumPlanes,
        materials: &[Option<PxbspTextureBinding>],
        material_tick: u32,
        selection: PxbspFaceSelection,
        sky_view: Option<ViewTransform>,
        packet_storage: &mut [u32],
    ) -> RenderFrame {
        let start = packet_storage.as_mut_ptr();
        let end = unsafe { start.add(packet_storage.len()) };
        let mut next = start;
        let mut stats = RenderStats::default();

        let mut batch_vertices =
            [ClassicAffineVertex::default(); BATCH_MAX_VERTICES + SUBDIVISION_SCRATCH_VERTICES];
        let mut batch_surfaces = [ClassicAffineWindowedBatchSurface::default(); BATCH_MAX_SURFACES];
        // One face at a time is materialized here, frustum-clipped, then
        // copied into the batch (a clip adds at most one vertex per plane).
        let mut face_vertices = [ClassicAffineVertex::default(); BATCH_MAX_VERTICES + 8];
        let mut clip_scratch = [ClassicAffineVertex::default(); BATCH_MAX_VERTICES + 8];
        let mut batch_vertex_count = 0usize;
        let mut batch_surface_count = 0usize;
        let mut batch_worst_words = 0usize;
        let mut layered_sky_binding = None;

        let faces = map.faces();
        let map_materials = map.materials();
        let (first_face, face_end) = selection.range(faces.len(), self.frame_pxbsp_faces.len());
        for selection_index in first_face..face_end {
            let face_index = selection.face_index(selection_index, &self.frame_pxbsp_faces);
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

            let source_count = face.vertex_count as usize;
            if source_count > BATCH_MAX_VERTICES {
                stats.packet_overflow_avoided = true;
                break;
            }
            let state = pxbsp_material_state(material, binding, material_tick);
            self.materialize_pxbsp_face(
                map,
                face,
                state.uv_offset,
                &mut face_vertices[..source_count],
            );
            let vertex_count =
                frustum.clip_polygon(&mut face_vertices, source_count, &mut clip_scratch);
            if vertex_count == 0 || vertex_count > BATCH_MAX_VERTICES {
                // Fully outside the frustum (or too many vertices after the
                // clip for one batch, which a face this size never is).
                continue;
            }
            if material.flags & crate::pxbsp::material_flags::LAYERED_SKY != 0 {
                if sky_view.is_some() {
                    if let Some(selected) = layered_sky_binding {
                        debug_assert_eq!(selected, binding);
                    } else {
                        layered_sky_binding = Some(binding);
                    }
                }
                stats.visible_faces = stats.visible_faces.saturating_add(1);
                continue;
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
            batch_vertices[batch_vertex_count..batch_vertex_count + vertex_count]
                .copy_from_slice(&face_vertices[..vertex_count]);
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

        // Sky faces are apertures, not textured polygons. Append one bounded
        // view-ray lattice after the world stream; equal-slot OT prepending
        // then executes it first, behind every opaque world packet.
        if let (Some(binding), Some(view)) = (layered_sky_binding, sky_view) {
            if packet_capacity(next, end, VIEW_RAY_SKY_PACKET_WORDS) {
                let half_width = self.view_projection.half_width.max(1);
                let half_height = self.view_projection.half_height.max(1);
                let screen_size = [
                    half_width.saturating_mul(2).min(i32::from(i16::MAX)) as i16,
                    half_height.saturating_mul(2).min(i32::from(i16::MAX)) as i16,
                ];
                let screen_center = [
                    half_width.min(i32::from(i16::MAX)) as i16,
                    half_height.min(i32::from(i16::MAX)) as i16,
                ];
                let projection = self
                    .view_projection
                    .focal_length
                    .clamp(1, i32::from(i16::MAX)) as i16;
                let submitted = unsafe {
                    submit_view_ray_layered_sky(
                        binding.texture_page,
                        binding.clut,
                        binding.uv_origin,
                        binding.texture_size,
                        view.rotation,
                        screen_size,
                        screen_center,
                        projection,
                        material_tick,
                        next,
                    )
                };
                next = submitted.next_packet;
                stats.packets = stats.packets.wrapping_add(submitted.packets);
                stats.hardware_triangles = stats
                    .hardware_triangles
                    .wrapping_add(submitted.hardware_triangles);
            } else {
                stats.packet_overflow_avoided = true;
            }
        }

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
        if faces.len() != self.pxbsp_face_count {
            self.cached_pxbsp_visibility = None;
            self.visible_pxbsp_faces.clear();
            return false;
        }
        let Some(leaf_index) = map.point_leaf_index(point) else {
            self.cached_pxbsp_visibility = None;
            self.visible_pxbsp_faces.clear();
            self.visible_leaf_count = 0;
            return false;
        };
        if leaf_index == 0 {
            self.cached_pxbsp_visibility = None;
            self.visible_pxbsp_faces.clear();
            self.visible_leaf_count = 0;
            return false;
        }
        if self.cached_pxbsp_visibility == Some((map.generation(), leaf_index)) {
            return true;
        }
        self.pxbsp_face_state.fill(0);
        self.visible_pxbsp_faces.clear();
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
                if packed_face_state(&self.pxbsp_face_state, face) == 0 {
                    set_packed_face_state(&mut self.pxbsp_face_state, face, 1);
                    self.visible_pxbsp_faces.push(face as u16);
                }
            }
        }
        // The former full-table scan visited faces in ascending source order.
        // Preserve that exact order so batching and equal-depth packet ties do
        // not change while replacing the scan with the compact chain.
        self.visible_pxbsp_faces.sort_unstable();
        self.visible_leaf_count = visible_leaves;
        self.rebuild_pxbsp_node_visibility(map);
        self.cached_pxbsp_visibility = Some((map.generation(), leaf_index));
        true
    }

    /// Rebuild the Quake-style PVS stamp for render nodes. This runs only
    /// when the camera enters a different leaf; per-frame traversal can then
    /// skip every branch that cannot lead to a PVS-visible leaf.
    fn rebuild_pxbsp_node_visibility(&mut self, map: &PxbspResidentMap) {
        let nodes = map.nodes();
        if nodes.len() != self.pxbsp_node_count {
            self.pxbsp_node_metadata_valid = false;
            self.pxbsp_node_leaf_marks = false;
            return;
        }
        self.pxbsp_node_visible.fill(0);
        self.pxbsp_node_discovered.fill(0);
        self.pxbsp_node_stack.clear();

        let root = map
            .brush_models()
            .get(0)
            .expect("validated world model")
            .head_nodes[0];
        if root < 0 || root as usize >= nodes.len() {
            self.pxbsp_node_metadata_valid = false;
            self.pxbsp_node_leaf_marks = false;
            return;
        }

        // Explicit tagged postorder avoids guest recursion and needs only a
        // depth-sized stack. The same walk upgrades state 1 PVS faces to state
        // 2 when a world node owns them, leaving staticized func_* faces at 1.
        let mut owns_faces = false;
        self.pxbsp_node_stack.push(root as u32);
        while let Some(entry) = self.pxbsp_node_stack.pop() {
            let index = (entry & !PXBSP_NODE_POSTORDER) as usize;
            let node = nodes.get(index).expect("validated node traversal");
            if entry & PXBSP_NODE_POSTORDER != 0 {
                let visible = node.children.into_iter().any(|child| {
                    if child >= 0 {
                        packed_bit(&self.pxbsp_node_visible, child as usize)
                    } else {
                        let leaf = (-1i32 - child as i32) as usize;
                        self.pxbsp_leaf_visible(leaf)
                    }
                });
                if visible {
                    set_packed_bit(&mut self.pxbsp_node_visible, index);
                }
                continue;
            }

            if packed_bit(&self.pxbsp_node_discovered, index) {
                continue;
            }
            set_packed_bit(&mut self.pxbsp_node_discovered, index);
            self.pxbsp_node_stack.push(entry | PXBSP_NODE_POSTORDER);
            for child in node.children {
                if child >= 0 {
                    self.pxbsp_node_stack.push(child as u32);
                }
            }

            let start = node.first_face as usize;
            let end = start + node.face_count as usize;
            owns_faces |= node.face_count != 0;
            for face in start..end {
                if packed_face_state(&self.pxbsp_face_state, face) == 1 {
                    set_packed_face_state(&mut self.pxbsp_face_state, face, 2);
                }
            }
        }
        self.pxbsp_node_metadata_valid = true;
        self.pxbsp_node_leaf_marks = !owns_faces;
    }

    fn pxbsp_leaf_visible(&self, leaf_index: usize) -> bool {
        if leaf_index == 0 || leaf_index > self.visible_leaf_count {
            return false;
        }
        let visible_index = leaf_index - 1;
        self.visibility[visible_index >> 3] & (1 << (visible_index & 7)) != 0
    }

    /// Select only PVS-stamped node faces whose subtree AABB intersects the
    /// current frustum. Exact per-polygon clipping remains in the draw path.
    fn select_frame_pxbsp_faces(
        &mut self,
        map: &PxbspResidentMap,
        camera_origin: Vec3I32,
        frustum: &FrustumPlanes,
    ) -> bool {
        self.frame_pxbsp_faces.clear();
        if !self.pxbsp_node_metadata_valid {
            self.frame_pxbsp_faces
                .extend_from_slice(&self.visible_pxbsp_faces);
            return true;
        }

        let nodes = map.nodes();
        let planes = map.planes();
        let root = map
            .brush_models()
            .get(0)
            .expect("validated world model")
            .head_nodes[0];
        if root < 0 || root as usize >= nodes.len() {
            return false;
        }

        self.pxbsp_node_stack.clear();
        self.pxbsp_node_stack.push(root as u32);
        while let Some(node_index) = self.pxbsp_node_stack.pop() {
            let index = node_index as usize;
            if !packed_bit(&self.pxbsp_node_visible, index) {
                continue;
            }
            let node = nodes.get(index).expect("validated node traversal");
            if frustum.aabb_outside(node.mins, node.maxs) {
                continue;
            }

            let plane = planes
                .get(node.plane as usize)
                .expect("validated node plane");
            let behind = plane_distance(plane, camera_origin) < 0;
            let near = node.children[behind as usize];
            let far = node.children[usize::from(!behind)];
            for child in [far, near] {
                if child >= 0 {
                    self.pxbsp_node_stack.push(child as u32);
                } else if self.pxbsp_node_leaf_marks {
                    let leaf_index = (-1i32 - child as i32) as usize;
                    if !self.pxbsp_leaf_visible(leaf_index) {
                        continue;
                    }
                    let leaf = map.leaves().get(leaf_index).expect("validated node leaf");
                    let start = leaf.first_mark_surface as usize;
                    let end = start + leaf.mark_surface_count as usize;
                    for mark_index in start..end {
                        let face =
                            map.mark_surfaces()
                                .get(mark_index)
                                .expect("validated leaf mark") as usize;
                        if packed_face_state(&self.pxbsp_face_state, face) != 0 {
                            self.frame_pxbsp_faces.push(face as u16);
                        }
                    }
                }
            }

            let start = node.first_face as usize;
            let end = start + node.face_count as usize;
            for face in start..end {
                if packed_face_state(&self.pxbsp_face_state, face) != 0 {
                    self.frame_pxbsp_faces.push(face as u16);
                }
            }
        }

        if !self.pxbsp_node_leaf_marks {
            // Imported func_* brushes are deliberately staticized into leaf
            // mark lists and do not belong to world-node face ranges.
            for &face in &self.visible_pxbsp_faces {
                if packed_face_state(&self.pxbsp_face_state, face as usize) == 1 {
                    self.frame_pxbsp_faces.push(face);
                }
            }
        }
        self.frame_pxbsp_faces.sort_unstable();
        self.frame_pxbsp_faces.dedup();
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
        submit_classic_affine_scoped_windowed_batch(
            vertices.as_mut_ptr(),
            vertex_count,
            surfaces.as_ptr(),
            surface_count,
            output,
            ClassicAffineProfile::PXBSP_THIRD_PERSON,
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
        let mins = [64i16, -16, -16].map(crate::encode_node_bound_min);
        let maxs = [64i16, 16, 16].map(crate::encode_node_bound_max);
        lumps[PxbspLumpKind::Nodes as usize][6..9].copy_from_slice(&mins.map(|value| value as u8));
        lumps[PxbspLumpKind::Nodes as usize][9..12].copy_from_slice(&maxs.map(|value| value as u8));
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
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        assert_eq!(map.point_leaf_index(camera.origin), Some(1));
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        assert_eq!(packed_face_state(&renderer.pxbsp_face_state, 0), 2);
        assert_eq!(renderer.visible_pxbsp_faces, [0]);
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

        assert_eq!(packed_face_state(&renderer.pxbsp_face_state, 0), 2);
        assert_eq!(frame.stats.visible_faces, 1);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert!(frame.stats.packets > 0);
        let mut offset = 0usize;
        let mut packet_count = 0u32;
        while offset < frame.packet_words {
            let data_words = (packets[offset] >> 24) as usize;
            assert!(matches!(packets[offset + 2] >> 24, 0x36 | 0x3e));
            assert_eq!(packets[offset + 1], binding.texture_window_word);
            assert_eq!(
                packets[offset + data_words],
                TextureWindow::NONE.word(),
                "every depth-sorted material packet must restore the full window"
            );
            assert_eq!(packets[offset + 4] >> 16, u32::from(binding.clut));
            assert_eq!((packets[offset + 7] >> 21) & 3, 2);
            offset += data_words + 1;
            packet_count += 1;
        }
        assert_eq!(offset, frame.packet_words);
        assert_eq!(packet_count, frame.stats.packets);
    }

    #[test]
    fn layered_sky_face_selects_one_constant_cost_view_background() {
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
        let mins = [64i16, -16, -16].map(crate::encode_node_bound_min);
        let maxs = [64i16, 16, 16].map(crate::encode_node_bound_max);
        lumps[PxbspLumpKind::Nodes as usize][6..9].copy_from_slice(&mins.map(|value| value as u8));
        lumps[PxbspLumpKind::Nodes as usize][9..12].copy_from_slice(&maxs.map(|value| value as u8));
        lumps[PxbspLumpKind::Materials as usize][2..4]
            .copy_from_slice(&material_flags::LAYERED_SKY.to_le_bytes());
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(10, &mut SliceReader::new(&bytes))
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
            // Runtime bindings describe one square half of the 256x128 pair.
            texture_size: [128; 2],
        };
        let mut packets = [0u32; VIEW_RAY_SKY_PACKET_WORDS];
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        let frame = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[Some(binding)],
            0,
            &mut packets,
        );

        assert_eq!(frame.stats.visible_faces, 1);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert!(!frame.stats.packet_overflow_avoided);
        assert_eq!(frame.stats.packets, 243);
        assert_eq!(frame.stats.hardware_triangles, 480);
        assert_eq!(frame.packet_words, VIEW_RAY_SKY_PACKET_WORDS);
        assert_eq!(packets[0] >> 24, 1, "first packet is the window reset");
        assert_eq!(packets[2] >> 24, 9, "sky follows with a textured quad");
    }

    #[test]
    fn pxbsp_renderer_allocates_exact_world_scratch() {
        let renderer = Renderer::new_pxbsp_with_nodes(37, 11);
        assert!(renderer.face_visible.is_empty());
        assert_eq!(renderer.pxbsp_face_count, 37);
        assert_eq!(renderer.pxbsp_face_state.len(), 10);
        assert_eq!(renderer.visible_pxbsp_faces.len(), 0);
        assert!(renderer.visible_pxbsp_faces.capacity() >= 37);
        assert_eq!(renderer.frame_pxbsp_faces.len(), 0);
        assert!(renderer.frame_pxbsp_faces.capacity() >= 37);
        assert_eq!(renderer.pxbsp_node_visible.len(), 2);
        assert_eq!(renderer.pxbsp_node_discovered.len(), 2);
        assert!(renderer.alias_projected.is_empty());
        assert_eq!(renderer.visible_entity_indices.capacity(), 0);
    }

    #[test]
    fn pxbsp_leaf_visibility_keeps_sparse_high_index_pvs_bits() {
        let mut renderer = Renderer::new_pxbsp_with_nodes(0, 0);
        renderer.visible_leaf_count = 100;
        renderer.visibility.fill(0);
        set_packed_bit(&mut renderer.visibility, 99);

        assert!(!renderer.pxbsp_leaf_visible(1));
        assert!(renderer.pxbsp_leaf_visible(100));
        assert!(!renderer.pxbsp_leaf_visible(101));
    }

    #[test]
    fn zero_face_nodes_collect_visible_leaf_marks_through_bounds() {
        configure_projection();
        let mut lumps = valid_lumps();
        // Standard editable-brush maps keep render faces in leaf marks; BSP
        // nodes remain spatial partitions and deliberately own no face range.
        lumps[PxbspLumpKind::Nodes as usize][12..16].fill(0);
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(9, &mut SliceReader::new(&bytes))
            .expect("resident map");
        let camera = Camera {
            origin: Vec3I32 {
                x: 1 << 12,
                y: 0,
                z: 0,
            },
            angles: [0; 3],
        };
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        assert_eq!(packed_face_state(&renderer.pxbsp_face_state, 0), 1);
        assert!(renderer.pxbsp_node_metadata_valid);
        assert!(renderer.pxbsp_node_leaf_marks);

        let view = load_pxbsp_view(camera);
        let frustum =
            FrustumPlanes::from_view(&view.rotation, view.translation, renderer.view_projection);
        assert!(renderer.select_frame_pxbsp_faces(&map, camera.origin, &frustum));
        assert_eq!(renderer.frame_pxbsp_faces, [0]);
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

#[cfg(test)]
mod frustum_tests {
    use super::*;

    fn planes_for(origin: [i32; 3], yaw: i16, pitch: i16) -> (FrustumPlanes, ViewTransform) {
        psx_gte::host::reset();
        let camera = Camera {
            origin: Vec3I32 {
                x: origin[0] << 12,
                y: origin[1] << 12,
                z: origin[2] << 12,
            },
            angles: [pitch, yaw, 0],
        };
        let view = load_pxbsp_view(camera);
        let projection = ViewProjection {
            focal_length: 320,
            half_width: 160,
            half_height: 120,
            ..ViewProjection::DEFAULT
        };
        (
            FrustumPlanes::from_view(&view.rotation, view.translation, projection),
            view,
        )
    }

    fn vertex(p: [i16; 3]) -> ClassicAffineVertex {
        ClassicAffineVertex {
            position: p,
            uv: [0, 0],
            color: 0x808080,
            screen: [0, 0],
            depth: 0,
        }
    }

    #[test]
    fn point_in_front_of_the_cornered_camera_survives_the_clip() {
        // The lap-tape pose that blacked out: camera at (37,70,273) looking
        // roughly -Z (engine orbit yaw 0 -> PXBSP yaw 1024) and a little down.
        let (planes, _) = planes_for([37, 70, 273], 1024, 100);
        // The far wall ahead of the camera (x across, full height, z=130).
        let mut quad = [
            vertex([16, 0, 130]),
            vertex([200, 0, 130]),
            vertex([200, 192, 130]),
            vertex([16, 192, 130]),
        ];
        let mut scratch = [vertex([0; 3]); 16];
        let mut verts = [vertex([0; 3]); 16];
        verts[..4].copy_from_slice(&quad);
        let n = planes.clip_polygon(&mut verts, 4, &mut scratch);
        assert!(
            n >= 3,
            "far wall in front of the camera was clipped away (n={n})"
        );
        // A wall face that runs from behind the camera to far ahead keeps its
        // far part.
        quad = [
            vertex([16, 0, 340]),
            vertex([16, 192, 340]),
            vertex([16, 192, 100]),
            vertex([16, 0, 100]),
        ];
        verts[..4].copy_from_slice(&quad);
        let n = planes.clip_polygon(&mut verts, 4, &mut scratch);
        assert!(
            n >= 3,
            "side wall spanning the near plane was clipped away (n={n})"
        );
        for v in &verts[..n] {
            let d = FrustumPlanes::distance(&planes.planes[0], v.position);
            assert!(
                d >= -(1i64 << 14),
                "clipped vertex behind the near plane: {:?} d={d}",
                v.position
            );
        }
    }

    #[test]
    fn node_aabb_rejection_is_conservative_for_the_cornered_camera() {
        let (planes, _) = planes_for([37, 70, 273], 1024, 100);
        assert!(!planes.aabb_outside(
            Vec3I16 {
                x: 16,
                y: 0,
                z: 100,
            },
            Vec3I16 {
                x: 200,
                y: 192,
                z: 130,
            },
        ));
        assert!(planes.aabb_outside(
            Vec3I16 {
                x: 16,
                y: 0,
                z: 320,
            },
            Vec3I16 {
                x: 200,
                y: 192,
                z: 340,
            },
        ));
    }
}
