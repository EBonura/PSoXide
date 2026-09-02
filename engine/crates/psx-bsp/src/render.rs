//! XBSP world rendering through PSoXide's classic-affine path.
//!
//! Lifted from quake-psx `game/src/renderer.rs` commit 83a6349, same GPL-2
//! authorship. Frame lifecycle, packet storage and entity ownership are
//! caller-supplied so this module can serve both runtimes.

use alloc::vec;
use alloc::vec::Vec;

use psx_engine::{
    attributed_clip::{
        clip_convex_plane, crossing_fraction_q16_i32, lerp_q16_i32_exact, AttributedClipPlane,
        ClipTraversal,
    },
    compose_classic_alias_transform, materialize_classic_affine_baked_light_vertices,
    materialize_classic_affine_word_vertices, submit_classic_affine_batch,
    submit_classic_affine_mixed_batch, submit_classic_affine_scoped_windowed_fan,
    submit_classic_alias_model, ClassicAffineBatchSurface, ClassicAffineMixedBatchSurface,
    ClassicAffineProfile, ClassicAffineSubmit, ClassicAffineVertex, ClassicAffineWordSourceVertex,
    ClassicAliasFace, ClassicAliasProjectedVertex, ClassicAliasVertex,
};
use psx_gpu::material::TextureWindow;
use psx_gpu::prim::ClassicTriTextured;
use psx_gte::math::{Mat3I16, Vec3I16 as GteVec3I16, Vec3I32 as GteVec3I32};
use psx_gte::scene::{self, AabbClipPlane};
use psx_math::int32::{isqrt_i32, mul_q12_i32};
use psx_math::{cos_q12, sin_q12};

use crate::collision::BrushTransform;
use crate::pxbsp::{
    decompress_visibility, material_blend, material_flags, PxbspMaterial, PxbspMaterialAnimation,
    PXBSP_MAX_VISIBILITY_BYTES,
};
use crate::pxbsp_resident::{FaceRef, PxbspResidentMap};
use crate::resident::ResidentMap;
use crate::{
    CompactPlane, Face, Plane, TextureInfo, Vec3I16, Vec3I32, FACE_BACKSIDE, FACE_BAKED_LIGHT,
    FACE_BAKED_UV, FACE_PAGE_LOCAL_UV, FACE_TWO_SIDED, TEXTURE_INVISIBLE, TEXTURE_LIQUID,
    TEXTURE_NULL, TEXTURE_SKY,
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
/// Traversal stack entry layout: node index in the low bits, the residual
/// clip-plane mask (Quake's clipflags) above it. Node indices are `i16`, so
/// sixteen bits is ample.
const PXBSP_NODE_STACK_INDEX: u32 = 0xffff;
const PXBSP_NODE_STACK_MASK_SHIFT: u32 = 16;
/// All five clip planes still need testing.
const PXBSP_CLIP_ALL_PLANES: u8 = 0x1f;
/// Index of the near plane in [`FrustumPlanes::planes`]. It is the only plane
/// whose per-face answer is "does any vertex fail it", rather than "do all".
const PXBSP_CLIP_NEAR_PLANE: usize = 0;
const PXBSP_FRAME_NODE_BACK: u8 = 1;
const PXBSP_FRAME_NODE_FRONT: u8 = 2;
const PXBSP_FRAME_FALLBACK: u8 = 3;

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

/// Append every face whose packed two-bit state is non-zero to `output`, in
/// ascending face order. Returns false when `output`'s fixed capacity is too
/// small, leaving the caller to fail the frame closed.
///
/// Reading the packed table back out is what makes the chain sorted and
/// unique: no comparison sort and no per-entry deduplication is needed, and
/// the whole-byte skip means an empty region of the face table costs one load
/// per four faces.
fn collect_marked_faces_ascending(states: &[u8], face_count: usize, output: &mut Vec<u16>) -> bool {
    output.clear();
    let capacity = output.capacity();
    for (byte_index, &packed) in states.iter().enumerate() {
        if packed == 0 {
            continue;
        }
        let base = byte_index << 2;
        for slot in 0..4 {
            if packed >> (slot << 1) & 3 == 0 {
                continue;
            }
            let face = base + slot;
            if face >= face_count {
                break;
            }
            if output.len() == capacity {
                return false;
            }
            output.push(face as u16);
        }
    }
    true
}

/// True when `mins`/`maxs` lies inside every box in `ancestors`.
fn box_fits(ancestors: &[(Vec3I16, Vec3I16)], mins: Vec3I16, maxs: Vec3I16) -> bool {
    !ancestors.iter().any(|(amins, amaxs)| {
        mins.x < amins.x
            || mins.y < amins.y
            || mins.z < amins.z
            || maxs.x > amaxs.x
            || maxs.y > amaxs.y
            || maxs.z > amaxs.z
    })
}
const BATCH_MAX_VERTICES: usize = 39;
const BATCH_MAX_SURFACES: usize = 13;
const SUBDIVISION_SCRATCH_VERTICES: usize = 12;
const AFFINE_BATCH_VERTEX_CAPACITY: usize = BATCH_MAX_VERTICES + SUBDIVISION_SCRATCH_VERTICES;
#[cfg(target_arch = "mips")]
const AFFINE_BATCH_WORKSPACE_BYTES: usize =
    AFFINE_BATCH_VERTEX_CAPACITY * core::mem::size_of::<ClassicAffineVertex>();
#[cfg(target_arch = "mips")]
const _: () = assert!(AFFINE_BATCH_WORKSPACE_BYTES <= psx_engine::scratchpad::SIZE);

/// PXBSP scratchpad layout: the five clip-plane records, then the batch
/// vertex workspace.
///
/// The clip planes are the most re-read bytes in the face loop. Each live
/// plane costs five loads per face, and with the hierarchical clipflags
/// leaving under two planes live per face that is about two thousand loads a
/// frame from main RAM at six stall cycles each. In the scratchpad they are
/// one-cycle reads at an absolute address, so the base-pointer reload goes
/// too. The batch gives up six vertex slots to pay for it; this project's
/// cook has no face wider than six vertices, so the batch still groups eight
/// faces per flush instead of nine.
const PXBSP_CLIP_PLANE_BYTES: usize = core::mem::size_of::<[([i32; 3], i32); 5]>();
const PXBSP_BATCH_MAX_VERTICES: usize = 33;
const PXBSP_BATCH_MAX_SURFACES: usize = 13;
const PXBSP_AFFINE_BATCH_VERTEX_CAPACITY: usize =
    PXBSP_BATCH_MAX_VERTICES + SUBDIVISION_SCRATCH_VERTICES;
#[cfg(target_arch = "mips")]
const _: () = assert!(
    PXBSP_CLIP_PLANE_BYTES
        + PXBSP_AFFINE_BATCH_VERTEX_CAPACITY * core::mem::size_of::<ClassicAffineVertex>()
        <= psx_engine::scratchpad::SIZE
);
// A `ClassicAffineVertex` is four-aligned and the plane block is a multiple
// of both its own eight-byte alignment and four.
const _: () = assert!(PXBSP_CLIP_PLANE_BYTES.is_multiple_of(8));
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
/// The third-person camera exposes large, oblique floor polygons much farther
/// from the near plane than Quake's first-person view. Keep one profile
/// authority for every world, special-surface and alias submission so their
/// shared edges cross subdivision bands together.
const PXBSP_RENDER_PROFILE: ClassicAffineProfile = ClassicAffineProfile::PXBSP_THIRD_PERSON;

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
    /// Direct page-local origin of the texture allocation. Cooker-proven
    /// faces add this to their UVs and use packets without GP0(E2).
    pub page_uv_origin: [u8; 2],
    pub texture_size: [u8; 2],
}

/// Counts emitted by one world frame.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderStats {
    pub visible_faces: u16,
    /// Visible brush faces that reveal the caller-owned scene sky.
    pub visible_sky_apertures: u16,
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

/// Uniform scale the XBSP view remaps bake into the rotation (3.0 in Q12).
///
/// Projection divides it back out, so screen positions are unchanged, but
/// every GTE `SZ` the classic affine path reads is this many times the true
/// view depth, and so is every OTZ it stages (`SZ / 4` with the installed
/// `ZSF3 = 0x155`, `ZSF4 = 0x100`). Anything else that sorts into the same
/// ordering table must key its depth through the same law; see
/// [`pxbsp_classic_far_depth`].
pub const XBSP_VIEW_SCALE_Q12: i32 = 0x3000;

/// True view depth at which a flat PXBSP surface reaches OT slot `ot_depth`,
/// the first slot the classic path rejects: `ot_depth * 4 / scale`.
///
/// Mapping `0..=this` linearly onto the world band reproduces the classic
/// triangle key for equal-depth vertices to within a slot, so the runtime
/// uses it as the far end of the depth range every non-world draw maps
/// through. Surfaces beyond it are not drawn by the world renderer at all.
pub const fn pxbsp_classic_far_depth(ot_depth: u16) -> i32 {
    (ot_depth as i32 * 4 * 4096 + XBSP_VIEW_SCALE_Q12 / 2) / XBSP_VIEW_SCALE_Q12
}

const XBSP_VIEW_SCALE: i16 = XBSP_VIEW_SCALE_Q12 as i16;

/// Build and load the classic XBSP camera transform.
pub fn load_view(camera: Camera) -> ViewTransform {
    load_view_with_coordinates(
        camera,
        Mat3I16 {
            m: [
                [0, -XBSP_VIEW_SCALE, 0],
                [0, 0, -XBSP_VIEW_SCALE],
                [XBSP_VIEW_SCALE, 0, 0],
            ],
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
    load_view_with_coordinates(camera, PXBSP_COORDINATES)
}

/// PXBSP's axis remap: the map's `+X` forward, `+Y` up frame into the GTE's
/// `+Z` forward, `-Y` up frame, carrying the view scale.
const PXBSP_COORDINATES: Mat3I16 = Mat3I16 {
    m: [
        [0, 0, XBSP_VIEW_SCALE],
        [0, -XBSP_VIEW_SCALE, 0],
        [XBSP_VIEW_SCALE, 0, 0],
    ],
};

/// The exact PXBSP view rotation for a camera whose pitch and yaw are known
/// as Q12 sine and cosine pairs: `Rx(pitch) * Ry(yaw)`, the same product
/// [`load_pxbsp_view`] forms from 256-step table angles.
///
/// A third-person engine camera carries full-precision trig and the model
/// pass rotates with it. Quantising that camera to the 256-step table for
/// the world pass rotated the world by up to 0.7 degrees against the
/// actors standing in it, which at 250 units of depth is three units of
/// floor: every actor looked a little above or below the ground depending
/// on where the camera sat between two table steps.
pub fn pxbsp_view_rotation(sin_pitch: i16, cos_pitch: i16, sin_yaw: i16, cos_yaw: i16) -> Mat3I16 {
    let pitch = Mat3I16 {
        m: [
            [0x1000, 0, 0],
            [0, cos_pitch, 0i16.wrapping_sub(sin_pitch)],
            [0, sin_pitch, cos_pitch],
        ],
    };
    let yaw = Mat3I16 {
        m: [
            [cos_yaw, 0, sin_yaw],
            [0, 0x1000, 0],
            [0i16.wrapping_sub(sin_yaw), 0, cos_yaw],
        ],
    };
    pitch.mul(&yaw)
}

/// [`load_pxbsp_view`] with a caller-built view rotation such as
/// [`pxbsp_view_rotation`]; `origin` is Q12 world units as in [`Camera`].
pub fn load_pxbsp_view_rotation(origin: Vec3I32, view: Mat3I16) -> ViewTransform {
    load_view_rotation_with_coordinates(origin, view, PXBSP_COORDINATES)
}

fn load_view_with_coordinates(camera: Camera, coordinates: Mat3I16) -> ViewTransform {
    let view = Mat3I16::rotate_xyz(
        (camera.angles[0] as u16) >> 4,
        (camera.angles[1] as u16) >> 4,
        (camera.angles[2] as u16) >> 4,
    );
    load_view_rotation_with_coordinates(camera.origin, view, coordinates)
}

fn load_view_rotation_with_coordinates(
    origin: Vec3I32,
    view: Mat3I16,
    coordinates: Mat3I16,
) -> ViewTransform {
    let rotation = scene::compose_rotation_scheduled(&view, &coordinates);
    scene::load_rotation(&rotation);
    scene::load_translation(GteVec3I32::ZERO);
    let translation = scene::transform_vertex_scheduled(GteVec3I16::new(
        (origin.x.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (origin.y.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (origin.z.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
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

/// Result of [`FrustumPlanes::classify_aabb`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg(test)]
enum AabbClass {
    /// Every corner fails one plane. No vertex can be inside it.
    Outside,
    /// Every corner satisfies every plane, near plane included.
    Inside,
    /// Neither proof holds; the exact per-vertex scan decides.
    Straddling,
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
    /// Plane normal and constant term, `distance(p) = n . p + c`, all `i32`.
    ///
    /// The near plane is exact: a Q12 view row against `i16` positions and
    /// an `i16`-range camera origin fits `i32`. The side planes carry the
    /// focal length and half-extent factors, so their normals are rounded
    /// down by `side_shift` bits to fit; [`Self::side_error`] bounds the
    /// rounding error of any side distance and every classification below
    /// treats that band as "undecided" so a rounded plane can neither reject
    /// a visible polygon nor vouch for a box it does not contain.
    planes: [([i32; 3], i32); 5],
    /// Worst-case rounding error of a side-plane distance (zero for near).
    side_error: i32,
    view_rows: [[i16; 3]; 3],
    view_translation: [i32; 3],
    side_scale: [i32; 2],
    focal_length: i32,
    near_view: i32,
}

impl FrustumPlanes {
    /// Build from the view rotation/translation currently loaded in the GTE.
    /// The view may carry a uniform scale (PXBSP loads x3); the near distance
    /// is scaled by the row length so `near_world` stays in world units.
    pub fn from_view(
        rotation: &Mat3I16,
        translation: GteVec3I32,
        camera_origin_units: [i32; 3],
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
        // Squared Q12 row length: three products of at most 3 * 4096 each,
        // under 4.6e8, so the 32-bit square root answers it.
        let scale_q12 = isqrt_i32(
            forward[0] * forward[0] + forward[1] * forward[1] + forward[2] * forward[2],
        );
        let near_view = projection.near_world.saturating_mul(scale_q12) >> 12;
        // The view translation is the rotated, negated camera origin in world
        // units, so `T.z << 12` is `-forward . camera` and every distance is
        // camera-relative: a Q12 row against an `i16` position stays under
        // 7e8, and the near constant under 2^30, so the near plane is exact
        // in `i32` and bit-identical to the former i64 evaluation.
        let _ = camera_origin_units;
        let near = (
            forward,
            (translation.z << 12).wrapping_sub(near_view << 12),
        );
        // side planes: |view_x| <= (half_w + margin)/H * view_z, same for y.
        let kx = projection.half_width + projection.edge_margin;
        let ky = projection.half_height + projection.edge_margin;
        let h = projection.focal_length.max(1);
        // Largest normal L1 that keeps `n . q` inside i32 for |q| < 65536 with
        // room for the constant term.
        const SIDE_L1_LIMIT: i32 = 30_000;
        let mut side_shift = 0u32;
        let raw_side = |axis: [i32; 3], k: i32, sign: i32| -> [i32; 3] {
            [
                k * forward[0] - sign * h * axis[0],
                k * forward[1] - sign * h * axis[1],
                k * forward[2] - sign * h * axis[2],
            ]
        };
        let raw = [
            raw_side(right, kx, 1),
            raw_side(right, kx, -1),
            raw_side(down, ky, 1),
            raw_side(down, ky, -1),
        ];
        for n in &raw {
            let l1 = n[0].unsigned_abs() + n[1].unsigned_abs() + n[2].unsigned_abs();
            while (l1 >> side_shift) > SIDE_L1_LIMIT as u32 {
                side_shift += 1;
            }
        }
        let round = |value: i32| -> i32 {
            if side_shift == 0 {
                value
            } else {
                (value + (1 << (side_shift - 1))) >> side_shift
            }
        };
        // The side constant is `(k * T.z - sign * h * T.axis) << 12` reduced
        // by the same shift, exact because the shift never exceeds twelve.
        let side = |n: [i32; 3], axis_t: i32, k: i32, sign: i32| -> ([i32; 3], i32) {
            let n = [round(n[0]), round(n[1]), round(n[2])];
            let constant = k.wrapping_mul(translation.z).wrapping_sub(sign.wrapping_mul(h).wrapping_mul(axis_t));
            let constant = if side_shift <= 12 {
                constant << (12 - side_shift)
            } else {
                constant >> (side_shift - 12)
            };
            (n, constant)
        };
        // Each rounded component is off by at most one half; the distance of a
        // point up to 65535 units away on every axis is off by at most that
        // times three halves. Zero when nothing was rounded.
        let side_error = if side_shift == 0 { 0 } else { 3 * 65_536 / 2 };
        Self {
            planes: [
                near,
                side(raw[0], translation.x, kx, 1),
                side(raw[1], translation.x, kx, -1),
                side(raw[2], translation.y, ky, 1),
                side(raw[3], translation.y, ky, -1),
            ],
            side_error,
            view_rows: [rotation.m[0], rotation.m[1], rotation.m[2]],
            view_translation: [translation.x, translation.y, translation.z],
            side_scale: [kx, ky],
            focal_length: h,
            near_view,
        }
    }
    /// Rounding error band of plane `index`: zero for the exact near plane.
    /// Distance of `position` from plane `index`: the dot on the GTE on the
    /// guest (the planes must be loaded by [`load_gte_clip_planes`]), the
    /// same wrapping `i32` products on the host.
    #[inline(always)]
    fn distance_at(&self, index: usize, position: [i16; 3]) -> i32 {
        #[cfg(target_arch = "mips")]
        {
            let xy = (position[0] as u16 as u32) | ((position[1] as u16 as u32) << 16);
            let z = position[2] as i32 as u32;
            // One MVMVA yields three plane dots but a box corner is chosen
            // for one plane, so only that MAC is read back.
            let dot = unsafe {
                match index {
                    0 => gte_dot_one::<MVMVA_LLM_V0_SF0, MFC2_MAC1>(xy, z),
                    1 => gte_dot_one::<MVMVA_LLM_V0_SF0, MFC2_MAC2>(xy, z),
                    2 => gte_dot_one::<MVMVA_LLM_V0_SF0, MFC2_MAC3>(xy, z),
                    3 => gte_dot_one::<MVMVA_LCM_V0_SF0, MFC2_MAC1>(xy, z),
                    _ => gte_dot_one::<MVMVA_LCM_V0_SF0, MFC2_MAC2>(xy, z),
                }
            };
            dot.wrapping_add(self.planes[index].1)
        }
        #[cfg(not(target_arch = "mips"))]
        {
            Self::distance(&self.planes[index], position)
        }
    }
    #[inline(always)]
    fn error_of(&self, index: usize) -> i32 {
        if index == PXBSP_CLIP_NEAR_PLANE {
            0
        } else {
            self.side_error
        }
    }
    #[inline(always)]
    fn distance(plane: &([i32; 3], i32), position: [i16; 3]) -> i32 {
        plane.0[0]
            .wrapping_mul(position[0] as i32)
            .wrapping_add(plane.0[1].wrapping_mul(position[1] as i32))
            .wrapping_add(plane.0[2].wrapping_mul(position[2] as i32))
            .wrapping_add(plane.1)
    }
    /// The point is outside beyond any rounding doubt.
    #[inline(always)]
    fn surely_outside(plane: &([i32; 3], i32), error: i32, position: [i16; 3]) -> bool {
        Self::distance(plane, position) < -error
    }
    /// The point is inside beyond any rounding doubt.
    #[inline(always)]
    fn surely_inside(plane: &([i32; 3], i32), error: i32, position: [i16; 3]) -> bool {
        Self::distance(plane, position) >= error
    }
    /// Clip one convex polygon against the planes named by `clip_mask`.
    ///
    /// `None` rejects the polygon: some live plane has every vertex outside
    /// it. `Some(near)` keeps it, where `near` says the near plane is live and
    /// at least one vertex fails it, so the caller must clip the polygon.
    ///
    /// Plane-major, and that is the point. The hierarchical clipflags leave
    /// under two planes live per face on real routes, so the historical
    /// vertex-major form (one pass over the vertices, all five planes each,
    /// sharing the three view-space dot products) spent most of its work on
    /// planes an ancestor node box had already proven. Here each live plane
    /// loads its record once and stops at the first vertex that satisfies it,
    /// which is the whole answer for every plane except the near one, whose
    /// answer also needs one failing vertex.
    ///
    /// Same predicate, so no polygon bounding box is built: the box was a
    /// pruning device for a five-plane-per-vertex scan that no longer happens,
    /// and building it cost a full pass over the vertices on its own.
    ///
    /// Planes outside `clip_mask` were proven satisfied by an enclosing box,
    /// so no vertex can fail them; skipping them keeps the answer exact,
    /// near-plane reporting included.
    ///
    /// `planes` is the caller's own copy of [`Self::planes`], so the guest can
    /// hold it somewhere cheaper to read than main RAM.
    #[inline(always)]
    fn cull_polygon(
        planes: &[([i32; 3], i32); 5],
        side_error: i32,
        clip_mask: u8,
        count: usize,
        mut vertex: impl FnMut(usize) -> [i16; 3],
    ) -> Option<bool> {
        let mut live = clip_mask;
        let mut needs_near_clip = false;
        while live != 0 {
            let plane_index = live.trailing_zeros() as usize;
            live &= live - 1;
            let plane = unsafe { planes.get_unchecked(plane_index) };
            let error = if plane_index == PXBSP_CLIP_NEAR_PLANE { 0 } else { side_error };
            let mut wholly_outside = true;
            let mut any_outside = false;
            let mut index = 0usize;
            while index < count {
                // A rounded side plane has an error band: a vertex inside it
                // neither proves the polygon visible nor lets it be rejected.
                // Rejection needs every vertex surely outside; the near clip
                // decision fires for any vertex not surely inside. The near
                // band is zero, so that plane keeps its exact answers.
                let distance = Self::distance(plane, vertex(index));
                if distance >= error {
                    wholly_outside = false;
                    // Every other plane only decides whether the polygon is
                    // wholly outside, which this vertex just answered; the
                    // near plane still wants to know whether any vertex is
                    // outside.
                    if plane_index != PXBSP_CLIP_NEAR_PLANE || any_outside {
                        break;
                    }
                } else {
                    any_outside = true;
                    if distance >= -error {
                        wholly_outside = false;
                    }
                    if !wholly_outside {
                        break;
                    }
                }
                index += 1;
            }
            if wholly_outside {
                return None;
            }
            if plane_index == PXBSP_CLIP_NEAR_PLANE {
                needs_near_clip = any_outside;
            }
        }
        Some(needs_near_clip)
    }

    /// Bit mask of frustum planes for which `position` is outside.
    ///
    /// This is algebraically identical to evaluating [`Self::distance`] for
    /// each plane, but reuses the three view-space dot products. A face can
    /// intersect the frustum only when the AND of its vertex masks is zero.
    /// Retained as the reference the plane-major [`Self::cull_polygon`] is
    /// tested against.
    #[cfg(test)]
    #[inline(always)]
    fn vertex_outside_mask(&self, position: [i16; 3]) -> u8 {
        let view_q12 = |row: [i16; 3], translation: i32| -> i64 {
            i64::from(
                i32::from(row[0]) * i32::from(position[0])
                    + i32::from(row[1]) * i32::from(position[1])
                    + i32::from(row[2]) * i32::from(position[2]),
            ) + (i64::from(translation) << 12)
        };
        let view_x = view_q12(self.view_rows[0], self.view_translation[0]);
        let view_y = view_q12(self.view_rows[1], self.view_translation[1]);
        let view_z = view_q12(self.view_rows[2], self.view_translation[2]);
        let near = view_z - (i64::from(self.near_view) << 12);
        let x_depth = i64::from(self.side_scale[0]) * view_z;
        let y_depth = i64::from(self.side_scale[1]) * view_z;
        let screen_x = i64::from(self.focal_length) * view_x;
        let screen_y = i64::from(self.focal_length) * view_y;

        u8::from(near < 0)
            | (u8::from(x_depth - screen_x < 0) << 1)
            | (u8::from(x_depth + screen_x < 0) << 2)
            | (u8::from(y_depth - screen_y < 0) << 3)
            | (u8::from(y_depth + screen_y < 0) << 4)
    }

    /// Classify one axis-aligned box against the clip planes named by `mask`.
    ///
    /// Planes outside `mask` were proven satisfied by an enclosing box, so the
    /// answer stays exact while testing fewer planes.
    ///
    /// Both decisive answers are exact with respect to the per-vertex scan in
    /// [`Self::vertex_outside_mask`]. A box wholly outside one plane contains
    /// no vertex inside that plane, so the scan's `wholly_outside` mask would
    /// keep that bit set and reject the face. A box wholly inside every plane
    /// contains no vertex outside any plane, near plane included, so the scan
    /// would clear every mask bit and report no near clip. Only a straddling
    /// box needs the exact scan.
    ///
    /// The support-point form costs at most six 32x32 multiplies per plane
    /// with an early reject, against twenty per vertex in the scan.
    #[cfg(test)]
    #[inline]
    fn classify_aabb(&self, mins: [i16; 3], maxs: [i16; 3], mask: u8) -> AabbClass {
        let mut wholly_inside = true;
        let mut index = 0usize;
        while index < 5 {
            if mask & (1 << index) == 0 {
                index += 1;
                continue;
            }
            let plane = &self.planes[index];
            let normal = plane.0;
            // The box corner farthest along the normal. If that one is
            // outside, every corner is.
            let outer = [
                if normal[0] >= 0 { maxs[0] } else { mins[0] },
                if normal[1] >= 0 { maxs[1] } else { mins[1] },
                if normal[2] >= 0 { maxs[2] } else { mins[2] },
            ];
            let error = self.error_of(index);
            if self.distance_at(index, outer) < -error {
                return AabbClass::Outside;
            }
            if wholly_inside {
                // The opposite corner. If that one is inside, every corner is.
                let inner = [
                    if normal[0] >= 0 { mins[0] } else { maxs[0] },
                    if normal[1] >= 0 { mins[1] } else { maxs[1] },
                    if normal[2] >= 0 { mins[2] } else { maxs[2] },
                ];
                wholly_inside = self.distance_at(index, inner) >= error;
            }
            index += 1;
        }
        if wholly_inside {
            AabbClass::Inside
        } else {
            AabbClass::Straddling
        }
    }

    /// Quake's `R_RecursiveWorldNode` clipflags, applied to one node box.
    ///
    /// `mask` names the planes a caller still has to care about; the rest were
    /// already proven satisfied by an ancestor box. Returns `None` when the box
    /// is wholly outside one of the masked planes, otherwise the residual mask
    /// for the subtree, with every plane this box proves cleared.
    fn cull_aabb(&self, mins: Vec3I16, maxs: Vec3I16, mask: u8) -> Option<u8> {
        let mins = [mins.x, mins.y, mins.z];
        let maxs = [maxs.x, maxs.y, maxs.z];
        let mut residual = mask;
        let mut index = 0usize;
        while index < 5 {
            if mask & (1 << index) == 0 {
                index += 1;
                continue;
            }
            let plane = &self.planes[index];
            let normal = plane.0;
            let outer = [
                if normal[0] >= 0 { maxs[0] } else { mins[0] },
                if normal[1] >= 0 { maxs[1] } else { mins[1] },
                if normal[2] >= 0 { maxs[2] } else { mins[2] },
            ];
            let error = self.error_of(index);
            if self.distance_at(index, outer) < -error {
                return None;
            }
            let inner = [
                if normal[0] >= 0 { mins[0] } else { maxs[0] },
                if normal[1] >= 0 { mins[1] } else { maxs[1] },
                if normal[2] >= 0 { mins[2] } else { maxs[2] },
            ];
            if self.distance_at(index, inner) >= error {
                residual &= !(1 << index);
            }
            index += 1;
        }
        Some(residual)
    }

    /// True when the whole axis-aligned box lies outside any clip plane.
    /// Testing the vertex farthest along each plane normal makes this a
    /// conservative hierarchical reject; intersecting nodes still reach the
    /// exact polygon clip below.
    fn aabb_outside(&self, mins: Vec3I16, maxs: Vec3I16) -> bool {
        let mins = [mins.x, mins.y, mins.z];
        let maxs = [maxs.x, maxs.y, maxs.z];
        self.planes.iter().enumerate().any(|(index, plane)| {
            let positive = [
                if plane.0[0] >= 0 { maxs[0] } else { mins[0] },
                if plane.0[1] >= 0 { maxs[1] } else { mins[1] },
                if plane.0[2] >= 0 { maxs[2] } else { mins[2] },
            ];
            self.distance_at(index, positive) < -self.error_of(index)
        })
    }

    /// Cheaply prove that a point is inside every plane using only 32-bit
    /// arithmetic. View coordinates are rounded down from Q12; the extra
    /// screen-axis unit makes the side-plane proof conservative. Points near
    /// a boundary fall back to the exact i64 clip below.
    #[inline]
    fn point_definitely_inside(&self, position: [i16; 3]) -> bool {
        let view_coord = |row: [i16; 3], translation: i32| {
            let dot = row[0] as i32 * position[0] as i32
                + row[1] as i32 * position[1] as i32
                + row[2] as i32 * position[2] as i32;
            (dot >> 12).saturating_add(translation)
        };
        let view_z = view_coord(self.view_rows[2], self.view_translation[2]);
        if view_z < self.near_view {
            return false;
        }
        let view_x = view_coord(self.view_rows[0], self.view_translation[0]);
        let view_y = view_coord(self.view_rows[1], self.view_translation[1]);
        let Some(x_extent) = view_x.checked_abs().and_then(|v| v.checked_add(1)) else {
            return false;
        };
        let Some(y_extent) = view_y.checked_abs().and_then(|v| v.checked_add(1)) else {
            return false;
        };
        let Some(x_depth) = self.side_scale[0].checked_mul(view_z) else {
            return false;
        };
        let Some(y_depth) = self.side_scale[1].checked_mul(view_z) else {
            return false;
        };
        let Some(x_edge) = self.focal_length.checked_mul(x_extent) else {
            return false;
        };
        let Some(y_edge) = self.focal_length.checked_mul(y_extent) else {
            return false;
        };
        x_depth >= x_edge && y_depth >= y_edge
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
        let (count, in_scratch) = self.clip_polygon_buffers(vertices, count, scratch);
        if in_scratch {
            vertices[..count].copy_from_slice(&scratch[..count]);
        }
        count
    }

    /// Clip while alternating the input/output buffers. Returning which
    /// buffer owns the result lets the renderer consume it directly instead
    /// of copying the polygon back after every one of the five planes.
    fn clip_polygon_buffers(
        &self,
        vertices: &mut [ClassicAffineVertex],
        count: usize,
        scratch: &mut [ClassicAffineVertex],
    ) -> (usize, bool) {
        self.clip_polygon_buffers_inner(vertices, count, scratch, false)
    }

    fn clip_polygon_buffers_inner(
        &self,
        vertices: &mut [ClassicAffineVertex],
        count: usize,
        scratch: &mut [ClassicAffineVertex],
        outside_rejected: bool,
    ) -> (usize, bool) {
        let mut count = count;
        // Most visible world faces are well inside the frustum. Prove that
        // common case with 32-bit math before paying for fifteen i64 products
        // per vertex in the exact five-plane test.
        if vertices[..count]
            .iter()
            .all(|vertex| self.point_definitely_inside(vertex.position))
        {
            return (count, false);
        }
        // Classify once before clipping. Quake-PSX performs the equivalent
        // screen outcode AND before emitting a brush triangle: if every point
        // is outside the same boundary, no edge can cross the view. Keeping
        // this test in exact plane space preserves the polygon clip contract
        // while avoiding five complete copy/interpolation passes for the many
        // PVS faces that sit wholly beyond one side of the camera frustum.
        let mut all_inside = true;
        let mut outside_every_vertex = if outside_rejected {
            0
        } else {
            (1u8 << self.planes.len()) - 1
        };
        for vertex in &vertices[..count] {
            let mut vertex_outside = 0u8;
            for (plane_index, plane) in self.planes.iter().enumerate() {
                let error = self.error_of(plane_index);
                if !Self::surely_inside(plane, error, vertex.position) {
                    all_inside = false;
                }
                if Self::surely_outside(plane, error, vertex.position) {
                    vertex_outside |= 1 << plane_index;
                }
            }
            if !outside_rejected {
                outside_every_vertex &= vertex_outside;
            }
        }
        if all_inside {
            return (count, false);
        }
        if outside_every_vertex != 0 {
            return (0, false);
        }
        let mut in_scratch = false;
        for plane in &self.planes {
            if count < 3 {
                return (0, false);
            }
            count = if in_scratch {
                clip_polygon_plane(plane, &scratch[..count], vertices)
            } else {
                clip_polygon_plane(plane, &vertices[..count], scratch)
            };
            in_scratch = !in_scratch;
        }
        if count < 3 {
            (0, false)
        } else {
            (count, in_scratch)
        }
    }
}

struct PxbspClipPlane<'a>(&'a ([i32; 3], i32));

impl AttributedClipPlane<ClassicAffineVertex> for PxbspClipPlane<'_> {
    type Distance = i32;

    #[inline(always)]
    fn distance(&self, _: usize, vertex: &ClassicAffineVertex) -> Self::Distance {
        FrustumPlanes::distance(self.0, vertex.position)
    }

    #[inline(always)]
    fn inside(&self, distance: Self::Distance) -> bool {
        distance >= 0
    }

    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &ClassicAffineVertex,
        first_distance: Self::Distance,
        _: usize,
        second: &ClassicAffineVertex,
        second_distance: Self::Distance,
    ) -> ClassicAffineVertex {
        let fraction = crossing_fraction_q16_i32(first_distance, second_distance);
        lerp_vertex(first, second, fraction)
    }
}

#[inline(never)]
fn clip_polygon_plane(
    plane: &([i32; 3], i32),
    source: &[ClassicAffineVertex],
    destination: &mut [ClassicAffineVertex],
) -> usize {
    unsafe {
        clip_convex_plane::<_, _, false>(
            source,
            destination,
            &PxbspClipPlane(plane),
            ClipTraversal::PreviousToCurrent,
        )
    }
}

fn lerp_vertex(
    a: &ClassicAffineVertex,
    b: &ClassicAffineVertex,
    fraction_q16: u32,
) -> ClassicAffineVertex {
    let lerp_i = |x: i32, y: i32| -> i32 { lerp_q16_i32_exact(x, y, fraction_q16) };
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
    /// `draw_pxbsp_world` always retires this chain before returning, because
    /// external runtimes may overlay its backing bytes with model scratch.
    frame_pxbsp_faces: Vec<u16>,
    /// Two-bit per-face frame selection: 0 hidden, 1/2 node-owned back/front,
    /// 3 leaf/staticized fallback. The node states retain the camera-side
    /// result Quake computes once for every coplanar node surface.
    frame_pxbsp_face_state: Vec<u8>,
    /// Per-face residual clip-plane mask for this frame: the planes the
    /// marking node could not prove. Zero means the face is wholly inside the
    /// frustum and needs no per-face clip at all. Only narrowed below
    /// [`PXBSP_CLIP_ALL_PLANES`] while
    /// [`Self::pxbsp_node_bounds_enclose_faces`] holds.
    frame_pxbsp_face_clip_mask: Vec<u8>,
    /// Cached PVS reachability for the compact PXBSP render tree.
    pxbsp_node_visible: Vec<u8>,
    pxbsp_node_discovered: Vec<u8>,
    pxbsp_node_count: usize,
    pxbsp_node_stack: Vec<u32>,
    pxbsp_node_metadata_valid: bool,
    /// Node ranges are empty, so visible faces are gathered from the marked
    /// leaves reached by the bounds traversal instead of node-owned ranges.
    pxbsp_node_leaf_marks: bool,
    /// Proven once per loaded map: every face a node's subtree can mark lies
    /// inside that node's stored bounds. Only then does a node-level "wholly
    /// inside the frustum" result also prove it for the node's faces.
    pxbsp_node_bounds_enclose_faces: bool,
    /// Map generation `pxbsp_node_bounds_enclose_faces` was proven for.
    pxbsp_node_bounds_generation: Option<u32>,
    /// Caller-owned frame-chain backing. A Vec facade is attached only for
    /// the duration of `draw_pxbsp_world`, then detached before model scratch
    /// reuses these bytes. Null means both face chains are ordinary Vecs.
    external_frame_pxbsp_faces: *mut u16,
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
    /// Per-material resolve cache, direct-mapped on the material index.
    pxbsp_material_cache: [PxbspResolvedMaterial; PXBSP_MATERIAL_CACHE_SLOTS],
    /// Bumped on entry to every face pass, so a cached entry from an earlier
    /// pass (different animation tick, different binding table) never hits.
    pxbsp_material_epoch: u32,
    /// Whether sky faces must survive sidedness testing to report visible
    /// apertures. Always-visible skies disable this so their non-rendering BSP
    /// faces leave the hot path before plane-side and clip work.
    track_sky_apertures: bool,
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

    /// Configure whether PXBSP sky surfaces act as visibility apertures.
    ///
    /// Leave this enabled for `ThroughSkySurfaces` skies. An always-visible
    /// sky does not consume the aperture result, so disabling it is exact and
    /// avoids evaluating every sky-marked BSP face each frame.
    pub fn set_track_sky_apertures(&mut self, track: bool) {
        self.track_sky_apertures = track;
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
        Self::new_pxbsp_with_capacities(face_count, node_count, face_count)
    }

    /// Construct PXBSP scratch from cooker-proven world capacities.
    ///
    /// `visible_face_capacity` bounds both the persistent PVS chain and its
    /// per-frame frustum-filtered subset. Runtime never grows either vector:
    /// an invalid undersized cook fails the frame closed instead of leaking a
    /// second allocation from the PS1 bump heap.
    pub fn new_pxbsp_with_capacities(
        face_count: usize,
        node_count: usize,
        visible_face_capacity: usize,
    ) -> Self {
        let chain_capacity = visible_face_capacity.min(face_count);
        Self::new_pxbsp_with_face_chains(
            face_count,
            node_count,
            Vec::with_capacity(chain_capacity),
            Vec::with_capacity(chain_capacity),
        )
    }

    /// Construct PXBSP scratch with caller-owned, session-lifetime face
    /// chains. This is the no-heap path used by project runtimes whose grid
    /// streaming and model-projection arenas are inactive while the BSP world
    /// chain is live.
    ///
    /// The visible-chain reference is consumed for the renderer's complete
    /// lifetime. The frame-chain reference is reduced to raw backing storage:
    /// a bounded `Vec` facade exists only during
    /// [`draw_pxbsp_world`](Self::draw_pxbsp_world), and is retired and
    /// detached before the caller may reuse those bytes. [`Drop`] never sends
    /// either caller-owned allocation to the global allocator.
    pub fn new_pxbsp_with_external_face_chains(
        face_count: usize,
        node_count: usize,
        visible_faces: &'static mut [u16],
        frame_faces: &'static mut [u16],
    ) -> Self {
        let capacity = visible_faces.len().min(frame_faces.len()).min(face_count);
        let visible_faces = external_face_chain(&mut visible_faces[..capacity]);
        let frame_faces = &mut frame_faces[..capacity];
        let mut renderer =
            Self::new_pxbsp_with_face_chains(face_count, node_count, visible_faces, Vec::new());
        renderer.external_frame_pxbsp_faces = frame_faces.as_mut_ptr();
        renderer
    }

    fn new_pxbsp_with_face_chains(
        face_count: usize,
        node_count: usize,
        visible_pxbsp_faces: Vec<u16>,
        frame_pxbsp_faces: Vec<u16>,
    ) -> Self {
        let mut renderer = Self::with_capacities(0, 0, 0);
        // These chains are persistent world scratch. Allocate their exact
        // upper bound once: a first full-level PVS can exceed a small starter
        // capacity, and repeated Vec growth leaks peak space from the PS1's
        // bump-style boot heap before the first frame is presented.
        renderer.pxbsp_face_state = vec![0; face_count.div_ceil(4)];
        renderer.pxbsp_face_count = face_count;
        renderer.visible_pxbsp_faces = visible_pxbsp_faces;
        renderer.frame_pxbsp_faces = frame_pxbsp_faces;
        renderer.frame_pxbsp_face_state = vec![0; face_count.div_ceil(4)];
        renderer.frame_pxbsp_face_clip_mask = vec![PXBSP_CLIP_ALL_PLANES; face_count];
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
            frame_pxbsp_face_state: Vec::new(),
            frame_pxbsp_face_clip_mask: Vec::new(),
            pxbsp_node_visible: Vec::new(),
            pxbsp_node_discovered: Vec::new(),
            pxbsp_node_count: 0,
            pxbsp_node_stack: Vec::new(),
            pxbsp_node_metadata_valid: false,
            pxbsp_node_leaf_marks: false,
            pxbsp_node_bounds_enclose_faces: false,
            pxbsp_node_bounds_generation: None,
            external_frame_pxbsp_faces: core::ptr::null_mut(),
            visibility: [0; PXBSP_MAX_VISIBILITY_BYTES],
            visible_leaf_count: 0,
            cached_visibility: None,
            cached_pxbsp_visibility: None,
            alias_projected: vec![ClassicAliasProjectedVertex::default(); alias_vertex_count],
            visible_entity_indices: Vec::with_capacity(render_entity_count),
            cached_frustum: None,
            view_projection: ViewProjection::DEFAULT,
            pxbsp_material_cache: [PxbspResolvedMaterial::default(); PXBSP_MATERIAL_CACHE_SLOTS],
            pxbsp_material_epoch: 0,
            track_sky_apertures: true,
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
            // This hot CPU-only workspace lives in the PS1's 1 KiB
            // scratchpad while GPU DMA reads packet data from main RAM. Host
            // builds retain an ordinary local array for parallel-safe tests.
            #[cfg(target_arch = "mips")]
            let batch_vertices = unsafe {
                core::slice::from_raw_parts_mut(
                    psx_engine::scratchpad::ptr_at::<ClassicAffineVertex>(0),
                    AFFINE_BATCH_VERTEX_CAPACITY,
                )
            };
            #[cfg(not(target_arch = "mips"))]
            let mut batch_vertex_storage =
                [ClassicAffineVertex::default(); AFFINE_BATCH_VERTEX_CAPACITY];
            #[cfg(not(target_arch = "mips"))]
            let batch_vertices = &mut batch_vertex_storage[..];
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
                            batch_vertices,
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
                            PXBSP_RENDER_PROFILE,
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
                            batch_vertices,
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
                    batch_vertices,
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
        self.attach_external_frame_pxbsp_faces();
        scene::load_rotation(&view.rotation);
        scene::load_translation(view.translation);

        let frustum =
            FrustumPlanes::from_view(
                &view.rotation,
                view.translation,
                [camera.origin.x >> 12, camera.origin.y >> 12, camera.origin.z >> 12],
                self.view_projection,
            );
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
                packet_storage,
            )
        } else {
            RenderFrame::default()
        };
        // The runtime overlays the frame chain with model projection scratch
        // immediately after this pass. Retire both its logical length and the
        // corresponding face marks while the selected indices are still
        // intact, so the next frame never reads overwritten model data.
        self.retire_frame_pxbsp_selection();
        self.detach_external_frame_pxbsp_faces();
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
        let frustum = FrustumPlanes::from_view(
            &rotation,
            translation,
            [local_camera.x >> 12, local_camera.y >> 12, local_camera.z >> 12],
            self.view_projection,
        );

        // Quake-PSX R_DrawBrushModel also scans the brush model's bounded
        // surface slice and performs its plane-side test per face. World-node
        // plane reuse does not apply to this transformed mover path.
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
            packet_storage,
        );
        self.frame = self.frame.wrapping_add(1);
        Some(frame)
    }

    fn draw_pxbsp_faces(
        &mut self,
        map: &PxbspResidentMap,
        camera_origin: Vec3I32,
        frustum: &FrustumPlanes,
        materials: &[Option<PxbspTextureBinding>],
        material_tick: u32,
        selection: PxbspFaceSelection,
        packet_storage: &mut [u32],
    ) -> RenderFrame {
        let start = packet_storage.as_mut_ptr();
        let end = unsafe { start.add(packet_storage.len()) };
        let mut next = start;
        let mut stats = RenderStats::default();

        // The two world paths are never active concurrently, so they can
        // share the same complete scratchpad reservation.
        #[cfg(target_arch = "mips")]
        let batch_vertices = unsafe {
            core::slice::from_raw_parts_mut(
                psx_engine::scratchpad::ptr_at::<ClassicAffineVertex>(PXBSP_CLIP_PLANE_BYTES),
                PXBSP_AFFINE_BATCH_VERTEX_CAPACITY,
            )
        };
        #[cfg(not(target_arch = "mips"))]
        let mut batch_vertex_storage =
            [ClassicAffineVertex::default(); PXBSP_AFFINE_BATCH_VERTEX_CAPACITY];
        #[cfg(not(target_arch = "mips"))]
        let batch_vertices = &mut batch_vertex_storage[..];
        let mut batch_surfaces =
            [ClassicAffineMixedBatchSurface::default(); PXBSP_BATCH_MAX_SURFACES];
        // One face at a time is materialized here, frustum-clipped, then
        // copied into the batch (a clip adds at most one vertex per plane).
        let mut face_vertices = [ClassicAffineVertex::default(); PXBSP_BATCH_MAX_VERTICES + 8];
        let mut clip_scratch = [ClassicAffineVertex::default(); PXBSP_BATCH_MAX_VERTICES + 8];
        let mut batch_vertex_count = 0usize;
        let mut batch_surface_count = 0usize;
        let mut batch_worst_words = 0usize;

        let faces = map.faces();
        // The vertex lump base, resolved once. `materialize_pxbsp_face` used
        // to re-derive it per drawn face, which is a lump-table lookup plus a
        // checked byte offset and a slice bounds compare.
        let source_base = map
            .vertex_data()
            .as_ptr()
            .cast::<ClassicAffineWordSourceVertex>();
        // Own the clip planes for the duration of the loop. Reaching them
        // through the caller's reference made every plane test reload a
        // spilled pointer from this frame before it could load the plane.
        // On the guest they go to the head of the scratchpad, which is a
        // constant address and one-cycle reads; on the host a local copy
        // addressed off `sp`.
        let frustum_local = *frustum;
        let side_error = frustum_local.side_error;
        #[cfg(target_arch = "mips")]
        let clip_planes: &[([i32; 3], i32); 5] = unsafe {
            let block = psx_engine::scratchpad::ptr_at::<([i32; 3], i32)>(0);
            core::ptr::copy_nonoverlapping(frustum_local.planes.as_ptr(), block, 5);
            &*block.cast::<[([i32; 3], i32); 5]>()
        };
        #[cfg(not(target_arch = "mips"))]
        let clip_planes: &[([i32; 3], i32); 5] = &frustum_local.planes;
        #[cfg(target_arch = "mips")]
        load_gte_clip_planes(clip_planes);
        // Every face pass gets its own epoch, so an entry cached under a
        // different animation tick or a different binding table cannot hit.
        self.pxbsp_material_epoch = self.pxbsp_material_epoch.wrapping_add(1);
        let epoch = self.pxbsp_material_epoch;
        let (first_face, face_end) = selection.range(faces.len(), self.frame_pxbsp_faces.len());
        for selection_index in first_face..face_end {
            let face_index = selection.face_index(selection_index, &self.frame_pxbsp_faces);
            let face = unsafe { map.face_ref_unchecked(face_index) };
            let material_index = face.texture();
            let slot = PxbspResolvedMaterial::slot(material_index);
            if !self.pxbsp_material_cache[slot].answers(material_index, epoch) {
                self.fill_pxbsp_material_cache(
                    map,
                    materials,
                    material_index,
                    material_tick,
                    epoch,
                );
            }
            // Only the policy byte decides the three rejections below, so the
            // rest of the record is not loaded for a face that never draws.
            let policy = unsafe { self.pxbsp_material_cache.get_unchecked(slot).policy };
            if policy & pxbsp_material_policy::SKY != 0 && !self.track_sky_apertures {
                continue;
            }
            let authored_front = match selection {
                PxbspFaceSelection::VisibleWorld => {
                    match packed_face_state(&self.frame_pxbsp_face_state, face_index) {
                        PXBSP_FRAME_NODE_BACK => false,
                        PXBSP_FRAME_NODE_FRONT => true,
                        _ => front_facing_pxbsp(map, face.plane(), face.flags(), camera_origin),
                    }
                }
                PxbspFaceSelection::ModelRange { .. } => {
                    front_facing_pxbsp(map, face.plane(), face.flags(), camera_origin)
                }
            };
            let face_flags = face.flags();
            if !pxbsp_policy_face_draws(policy, face_flags, authored_front) {
                continue;
            }

            if policy & pxbsp_material_policy::SKY != 0 {
                stats.visible_faces = stats.visible_faces.saturating_add(1);
                stats.visible_sky_apertures = stats.visible_sky_apertures.saturating_add(1);
                continue;
            }

            if policy & pxbsp_material_policy::BOUND == 0 {
                stats.unresolved_material_faces = stats.unresolved_material_faces.saturating_add(1);
                continue;
            }
            let resolved = unsafe { *self.pxbsp_material_cache.get_unchecked(slot) };

            let source_count = face.vertex_count();
            if source_count > PXBSP_BATCH_MAX_VERTICES {
                stats.packet_overflow_avoided = true;
                break;
            }
            // Classify all five planes in one vertex pass. The historical
            // path rescanned the polygon once per plane and then a sixth time
            // for near clipping; reusing each vertex's view-space dot products
            // preserves the exact clip result without that repeated work.
            // Planes the marking node already proved for this face are dropped
            // from the test; an empty mask means the face is wholly inside the
            // frustum, so the bounding pass, the box classification and the
            // per-vertex scan are all skipped.
            let clip_mask = match selection {
                PxbspFaceSelection::VisibleWorld => unsafe {
                    *self.frame_pxbsp_face_clip_mask.get_unchecked(face_index)
                },
                PxbspFaceSelection::ModelRange { .. } => PXBSP_CLIP_ALL_PLANES,
            };
            let needs_near_clip = if clip_mask == 0 {
                false
            } else {
                #[cfg(target_arch = "mips")]
                let classified = unsafe {
                    Self::pxbsp_face_clip_gte(source_base, face, clip_planes, side_error, clip_mask)
                };
                #[cfg(not(target_arch = "mips"))]
                let classified = unsafe {
                    Self::pxbsp_face_clip(source_base, face, clip_planes, side_error, clip_mask)
                };
                let Some(needs_near_clip) = classified
                else {
                    continue;
                };
                needs_near_clip
            };
            let state = resolved.state;
            let compact_surface = face_flags & FACE_PAGE_LOCAL_UV != 0
                && policy & pxbsp_material_policy::COMPACT != 0;
            let uv_offset = if compact_surface {
                state.page_uv_offset
            } else {
                state.uv_offset
            };
            let vertex_count = if needs_near_clip {
                unsafe {
                    self.materialize_pxbsp_face(
                        source_base,
                        face,
                        uv_offset,
                        state.color_scale_q7,
                        &mut face_vertices[..source_count],
                    );
                }
                let count = clip_polygon_plane(
                    &clip_planes[0],
                    &face_vertices[..source_count],
                    &mut clip_scratch,
                );
                if !(3..=PXBSP_BATCH_MAX_VERTICES).contains(&count) {
                    continue;
                }
                count
            } else {
                source_count
            };
            let face_worst_words = (vertex_count - 2)
                * if compact_surface {
                    WORST_PACKET_WORDS_PER_TRIANGLE
                } else {
                    WORST_WINDOWED_PACKET_WORDS_PER_TRIANGLE
                };
            if batch_vertex_count + vertex_count > PXBSP_BATCH_MAX_VERTICES
                || batch_surface_count == PXBSP_BATCH_MAX_SURFACES
                || !packet_capacity(next, end, batch_worst_words + face_worst_words)
            {
                if batch_surface_count != 0 {
                    stats.surface_batches = stats.surface_batches.saturating_add(1);
                }
                let submitted = unsafe {
                    flush_pxbsp_batch(
                        batch_vertices,
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
            batch_surfaces[batch_surface_count] = ClassicAffineMixedBatchSurface {
                first_vertex: batch_vertex_count as u16,
                vertex_count: vertex_count as u16,
                tpage: state.texture_page,
                clut: resolved.clut,
                // PXBSP vertices are materialized with the resolved layer
                // offset above, so the shared writer applies no second offset.
                uv_offset: [0; 2],
                compact: u8::from(compact_surface),
                _padding: 0,
                texture_window_word: resolved.texture_window_word,
                color_command_word: state.color_command_word,
            };
            if needs_near_clip {
                batch_vertices[batch_vertex_count..batch_vertex_count + vertex_count]
                    .copy_from_slice(&clip_scratch[..vertex_count]);
            } else {
                unsafe {
                    self.materialize_pxbsp_face(
                        source_base,
                        face,
                        uv_offset,
                        state.color_scale_q7,
                        &mut batch_vertices[batch_vertex_count..batch_vertex_count + vertex_count],
                    );
                }
            }
            batch_vertex_count += vertex_count;
            batch_surface_count += 1;
            batch_worst_words += face_worst_words;
            stats.visible_faces = stats.visible_faces.saturating_add(1);
        }

        if batch_surface_count != 0 {
            stats.surface_batches = stats.surface_batches.saturating_add(1);
        }
        let submitted = unsafe {
            flush_pxbsp_batch(
                batch_vertices,
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
        if baked_light && !baked_uv {
            unsafe {
                materialize_classic_affine_baked_light_vertices(
                    source_ptr.cast::<ClassicAffineWordSourceVertex>(),
                    output.len(),
                    output.as_mut_ptr(),
                    [texture.atlas.x, texture.atlas.y],
                );
            }
        } else {
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

    /// `source_base` is the map's vertex lump, resolved once per face pass.
    ///
    /// # Safety
    /// `source_base` must be the base of `map.vertex_data()` for the map the
    /// face came from, so `face.first_vertex` indexes it in range. Deriving it
    /// here instead cost a lump-table lookup, a checked offset multiply and a
    /// slice bounds compare on every drawn face.
    unsafe fn materialize_pxbsp_face(
        &self,
        source_base: *const ClassicAffineWordSourceVertex,
        face: FaceRef,
        uv_offset: [u8; 2],
        color_scale_q7: u8,
        output: &mut [ClassicAffineVertex],
    ) {
        let first = face.first_vertex();
        let flags = face.flags();
        let baked_uv = flags & FACE_BAKED_UV != 0;
        let baked_light = flags & FACE_BAKED_LIGHT != 0;
        let light_styles = face.light_styles();
        let style0 = self.light_styles[light_styles[0] as usize];
        let style1 = self.light_styles[light_styles[1] as usize];
        let source_ptr = unsafe { source_base.add(first) };
        debug_assert_eq!(source_ptr as usize & 3, 0);
        if baked_light && !baked_uv {
            unsafe {
                materialize_classic_affine_baked_light_vertices(
                    source_ptr,
                    output.len(),
                    output.as_mut_ptr(),
                    uv_offset,
                );
            }
        } else {
            unsafe {
                materialize_classic_affine_word_vertices(
                    source_ptr,
                    output.len(),
                    output.as_mut_ptr(),
                    uv_offset,
                    [style0, style1],
                    baked_uv,
                    baked_light,
                );
            }
        }
        for vertex in output {
            let color = if baked_light {
                normalize_baked_color(vertex.color)
            } else {
                vertex.color
            };
            vertex.color = scale_baked_color_q7(color, color_scale_q7);
        }
    }

    /// Fill one direct-mapped slot of the per-material resolve cache.
    ///
    /// Deliberately outlined: this runs at most once per distinct material per
    /// face pass (twelve times a frame on this project's world), against a
    /// face loop that consults the cache a few hundred times. Inlining it
    /// would put the sixteen-byte record decode and the whole animation
    /// derivation back in the loop body's instruction footprint.
    #[inline(never)]
    fn fill_pxbsp_material_cache(
        &mut self,
        map: &PxbspResidentMap,
        materials: &[Option<PxbspTextureBinding>],
        material_index: usize,
        tick: u32,
        epoch: u32,
    ) {
        let material = unsafe { map.materials().get_unchecked(material_index) };
        let mut policy = (material.flags as u8) & pxbsp_material_policy::FACE_MASK;
        if material.flags
            & (crate::pxbsp::material_flags::SKY_APERTURE
                | crate::pxbsp::material_flags::DIRECTIONAL_SKY)
            != 0
        {
            policy |= pxbsp_material_policy::SKY;
        }
        if material.blend_mode == material_blend::OPAQUE
            && material.animation_kind == crate::pxbsp::material_animation::STATIC
            && material.flags & crate::pxbsp::material_flags::SKY_APERTURE == 0
        {
            policy |= pxbsp_material_policy::COMPACT;
        }
        let binding = materials.get(material_index).copied().flatten();
        if binding.is_some() {
            policy |= pxbsp_material_policy::BOUND;
        }
        // A face whose material has no binding, or which is a sky aperture,
        // never reads the state; resolving against a default binding keeps
        // this branch-free without changing any drawn packet.
        let binding = binding.unwrap_or(PxbspTextureBinding {
            texture_page: 0,
            clut: 0,
            texture_window_word: 0,
            uv_origin: [0; 2],
            page_uv_origin: [0; 2],
            texture_size: [1; 2],
        });
        self.pxbsp_material_cache[PxbspResolvedMaterial::slot(material_index)] =
            PxbspResolvedMaterial {
                state: pxbsp_material_state(material, binding, tick),
                texture_window_word: binding.texture_window_word,
                epoch,
                clut: binding.clut,
                material_index: material_index as u16,
                policy,
            };
    }

    /// Return `None` when every face vertex is outside any one frustum plane;
    /// otherwise return whether the face crosses the near plane.
    ///
    /// # Safety
    /// `source_base` must be the base of the map's vertex lump; see
    /// [`Self::materialize_pxbsp_face`].
    /// [`Self::pxbsp_face_clip`] with the five plane dot products on the GTE.
    ///
    /// The CPU form is plane-major with three `mult`s per vertex per plane;
    /// on the R3000 each `mult` interlocks the following `mflo`, and the
    /// per-vertex loop of the world pass was the single hottest code in
    /// the Cortex whole-level profile. Here one `MVMVA` against the light
    /// matrix yields three plane distances for a vertex at once, a second
    /// against the colour matrix the remaining two, and the scan is
    /// vertex-major with the same early exits. Plane constants are added
    /// on the CPU because MVMVA scales its translation vector by 4096
    /// before the add, which is not exact without the fractional shift.
    /// Every distance is the same wrapping `i32` dot product, so the answer
    /// is bit-identical to the plane-major scan; the whole-level tape
    /// hashes pin that.
    ///
    /// # Safety
    ///
    /// As [`Self::pxbsp_face_clip`], and [`load_gte_clip_planes`] must have
    /// loaded `planes` since the last light or colour matrix write.
    #[cfg(target_arch = "mips")]
    #[inline(always)]
    unsafe fn pxbsp_face_clip_gte(
        source_base: *const ClassicAffineWordSourceVertex,
        face: FaceRef,
        planes: &[([i32; 3], i32); 5],
        side_error: i32,
        clip_mask: u8,
    ) -> Option<bool> {
        const NEAR: u8 = 1 << PXBSP_CLIP_NEAR_PLANE;
        let count = face.vertex_count();
        if count == 0 {
            return None;
        }
        let source = unsafe { source_base.add(face.first_vertex()) };
        // Bit p stays set while every vertex so far was surely outside p.
        let mut wholly_outside = clip_mask;
        let near_tested = clip_mask & NEAR != 0;
        let mut near_any_outside = false;
        let mut index = 0usize;
        while index < count {
            let base = unsafe { source.add(index).cast::<u32>() };
            let xy = unsafe { core::ptr::read(base) };
            let z = unsafe { core::ptr::read(base.add(1).cast::<i16>()) } as i32 as u32;
            let (d0, d1, d2) = unsafe { gte_dot3::<MVMVA_LLM_V0_SF0>(xy, z) };
            // The near band is zero, so that plane keeps its exact answers.
            if d0.wrapping_add(planes[0].1) >= 0 {
                wholly_outside &= !NEAR;
            } else {
                near_any_outside = true;
            }
            if d1.wrapping_add(planes[1].1) >= -side_error {
                wholly_outside &= !(1 << 1);
            }
            if d2.wrapping_add(planes[2].1) >= -side_error {
                wholly_outside &= !(1 << 2);
            }
            if wholly_outside & 0x18 != 0 {
                let (d3, d4, _) = unsafe { gte_dot3::<MVMVA_LCM_V0_SF0>(xy, z) };
                if d3.wrapping_add(planes[3].1) >= -side_error {
                    wholly_outside &= !(1 << 3);
                }
                if d4.wrapping_add(planes[4].1) >= -side_error {
                    wholly_outside &= !(1 << 4);
                }
            }
            if wholly_outside == 0 && (!near_tested || near_any_outside) {
                break;
            }
            index += 1;
        }
        if wholly_outside != 0 {
            return None;
        }
        Some(near_tested && near_any_outside)
    }

    unsafe fn pxbsp_face_clip(
        source_base: *const ClassicAffineWordSourceVertex,
        face: FaceRef,
        planes: &[([i32; 3], i32); 5],
        side_error: i32,
        clip_mask: u8,
    ) -> Option<bool> {
        let first = face.first_vertex();
        let count = face.vertex_count();
        let source = unsafe { source_base.add(first) };
        if count == 0 {
            return None;
        }
        // `ClassicAffineWordSourceVertex` is `repr(C)`, four-aligned and
        // twelve-byte strided, with `position` first. Reading x and y as the
        // one word they already share costs two shifts and saves a load, and
        // a load is six cycles of RAM stall against one cycle for a shift.
        FrustumPlanes::cull_polygon(planes, side_error, clip_mask, count, |index| unsafe {
            let base = source.add(index).cast::<u32>();
            let xy = core::ptr::read(base);
            let z = core::ptr::read(base.add(1).cast::<i16>());
            [xy as i16, (xy >> 16) as i16, z]
        })
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
                    PXBSP_RENDER_PROFILE,
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
        let marks = map.mark_surfaces_native();
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
                let face = *marks.get(mark_index).expect("validated mark surface") as usize;
                set_packed_face_state(&mut self.pxbsp_face_state, face, 1);
            }
        }
        // The former full-table scan visited faces in ascending source order.
        // Preserve that exact order: reading the packed marks back out in
        // index order yields the same sorted, unique chain the comparison sort
        // produced, from a byte-at-a-time pass over 2 bits per face instead of
        // an O(n log n) quicksort over the chain.
        if !collect_marked_faces_ascending(
            &self.pxbsp_face_state,
            self.pxbsp_face_count,
            &mut self.visible_pxbsp_faces,
        ) {
            self.pxbsp_face_state.fill(0);
            self.cached_pxbsp_visibility = None;
            self.visible_pxbsp_faces.clear();
            self.visible_leaf_count = 0;
            return false;
        }
        self.visible_leaf_count = visible_leaves;
        self.rebuild_pxbsp_node_visibility(map);
        self.cached_pxbsp_visibility = Some((map.generation(), leaf_index));
        true
    }

    /// Rebuild the Quake-style PVS stamp for render nodes. This runs only
    /// when the camera enters a different leaf; per-frame traversal can then
    /// skip every branch that cannot lead to a PVS-visible leaf.
    fn rebuild_pxbsp_node_visibility(&mut self, map: &PxbspResidentMap) {
        let nodes = map.compact_nodes();
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
        if self.pxbsp_node_bounds_generation != Some(map.generation()) {
            self.pxbsp_node_bounds_enclose_faces = self.prove_pxbsp_node_bounds(map);
            self.pxbsp_node_bounds_generation = Some(map.generation());
        }
    }

    /// Prove, once per loaded map, that every face reachable under a node lies
    /// inside that node's stored bounds.
    ///
    /// Quake's `qbsp` maintains this invariant and `R_RecursiveWorldNode`'s
    /// clipflags rely on it, but a PXBSP cook is free to store looser bounds.
    /// Without the proof, inheriting "wholly inside the frustum" down the tree
    /// could drop a face that pokes out of its node box, so the render path
    /// falls back to the exact per-face clip whenever this returns false.
    ///
    /// Node boxes are quantized outward per node, so a child's box is NOT
    /// always contained in its parent's (89 of this project's 743 nodes break
    /// that). Containment is therefore proven directly against every ancestor
    /// on the path, one depth-first walk from the world root, and only for the
    /// nodes the render traversal can actually reach.
    fn prove_pxbsp_node_bounds(&mut self, map: &PxbspResidentMap) -> bool {
        let nodes = map.nodes();
        let leaves = map.leaves();
        let marks = map.mark_surfaces();
        let faces = map.faces();
        let vertices = map.vertex_data();
        let stride = core::mem::size_of::<ClassicAffineWordSourceVertex>();
        let Some(root) = map.brush_models().get(0).map(|world| world.head_nodes[0]) else {
            return false;
        };

        // Union box of a face range, or None when any record is out of range.
        let face_range_box = |first_face: usize, count: usize| -> Option<(Vec3I16, Vec3I16)> {
            let mut mins = Vec3I16 {
                x: i16::MAX,
                y: i16::MAX,
                z: i16::MAX,
            };
            let mut maxs = Vec3I16 {
                x: i16::MIN,
                y: i16::MIN,
                z: i16::MIN,
            };
            let mut seen = false;
            for index in first_face..first_face.checked_add(count)? {
                let face = faces.get(index)?;
                let vertex_count = face.vertex_count as usize;
                let first = face.first_vertex as usize;
                if first.checked_add(vertex_count)?.checked_mul(stride)? > vertices.len() {
                    return None;
                }
                let base = unsafe { vertices.as_ptr().add(first * stride) };
                for vertex in 0..vertex_count {
                    let position = unsafe {
                        core::ptr::read_unaligned(base.add(vertex * stride).cast::<[i16; 3]>())
                    };
                    seen = true;
                    mins.x = mins.x.min(position[0]);
                    mins.y = mins.y.min(position[1]);
                    mins.z = mins.z.min(position[2]);
                    maxs.x = maxs.x.max(position[0]);
                    maxs.y = maxs.y.max(position[1]);
                    maxs.z = maxs.z.max(position[2]);
                }
            }
            if seen {
                Some((mins, maxs))
            } else {
                None
            }
        };

        // Depth-first with an explicit ancestor stack. A world deeper than this
        // simply fails the proof and keeps the exact per-face clip.
        const MAX_DEPTH: usize = 64;
        let mut ancestors = [(Vec3I16::default(), Vec3I16::default()); MAX_DEPTH];
        let mut walk = [(0i16, 0usize); MAX_DEPTH * 2];
        let mut top = 1usize;
        walk[0] = (root, 0);
        while top != 0 {
            top -= 1;
            let (child, depth) = walk[top];
            if depth >= MAX_DEPTH {
                return false;
            }
            if child < 0 {
                let Some(leaf) = leaves.get((-1i32 - child as i32) as usize) else {
                    return false;
                };
                let first = leaf.first_mark_surface as usize;
                for mark_index in first..first + leaf.mark_surface_count as usize {
                    let Some(face) = marks.get(mark_index) else {
                        return false;
                    };
                    let Some((mins, maxs)) = face_range_box(face as usize, 1) else {
                        return false;
                    };
                    if !box_fits(&ancestors[..depth], mins, maxs) {
                        return false;
                    }
                }
                continue;
            }
            let Some(node) = nodes.get(child as usize) else {
                return false;
            };
            ancestors[depth] = (node.mins, node.maxs);
            if node.face_count != 0 {
                let Some((mins, maxs)) =
                    face_range_box(node.first_face as usize, node.face_count as usize)
                else {
                    return false;
                };
                if !box_fits(&ancestors[..depth + 1], mins, maxs) {
                    return false;
                }
            }
            if top + 2 > walk.len() {
                return false;
            }
            walk[top] = (node.children[0], depth + 1);
            walk[top + 1] = (node.children[1], depth + 1);
            top += 2;
        }
        true
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
        self.retire_frame_pxbsp_selection();
        if !self.pxbsp_node_metadata_valid {
            for &face in &self.visible_pxbsp_faces {
                set_packed_face_state(
                    &mut self.frame_pxbsp_face_state,
                    face as usize,
                    PXBSP_FRAME_FALLBACK,
                );
                self.frame_pxbsp_face_clip_mask[face as usize] = PXBSP_CLIP_ALL_PLANES;
            }
            self.frame_pxbsp_faces
                .extend_from_slice(&self.visible_pxbsp_faces);
            return true;
        }

        let nodes = map.compact_nodes();
        let planes = map.planes();
        let root = map
            .brush_models()
            .get(0)
            .expect("validated world model")
            .head_nodes[0];
        if root < 0 || root as usize >= nodes.len() {
            return false;
        }

        // Quake's R_RecursiveWorldNode carries clip state down the tree: once a
        // node's box is wholly inside every plane, no descendant box or face
        // can leave the frustum. Inheriting that proof (valid only while the
        // cook's node bounds are proven to enclose their faces) lets the draw
        // loop skip the exact per-face clip entirely.
        let inherit_clip = self.pxbsp_node_bounds_enclose_faces;
        // The node boxes below test their support corners on the GTE.
        #[cfg(target_arch = "mips")]
        load_gte_clip_planes(&frustum.planes);
        self.pxbsp_node_stack.clear();
        self.pxbsp_node_stack
            .push(root as u32 | (PXBSP_CLIP_ALL_PLANES as u32) << PXBSP_NODE_STACK_MASK_SHIFT);
        while let Some(entry) = self.pxbsp_node_stack.pop() {
            let index = (entry & PXBSP_NODE_STACK_INDEX) as usize;
            if !packed_bit(&self.pxbsp_node_visible, index) {
                continue;
            }
            let node = nodes.get(index).expect("validated node traversal");
            let mut mask = (entry >> PXBSP_NODE_STACK_MASK_SHIFT) as u8;
            if mask != 0 {
                // The wire record stores the box as signed eight-unit grid
                // codes; only a node that still has a plane to test pays for
                // expanding them.
                let mins = Vec3I16 {
                    x: crate::decode_node_bound_min(node.mins[0]),
                    y: crate::decode_node_bound_min(node.mins[1]),
                    z: crate::decode_node_bound_min(node.mins[2]),
                };
                let maxs = Vec3I16 {
                    x: crate::decode_node_bound_max(node.maxs[0]),
                    y: crate::decode_node_bound_max(node.maxs[1]),
                    z: crate::decode_node_bound_max(node.maxs[2]),
                };
                if !inherit_clip {
                    if frustum.aabb_outside(mins, maxs) {
                        continue;
                    }
                } else {
                    let Some(residual) = frustum.cull_aabb(mins, maxs, mask) else {
                        continue;
                    };
                    mask = residual;
                }
            }
            let child_tag = (mask as u32) << PXBSP_NODE_STACK_MASK_SHIFT;

            let plane = planes
                .get(node.plane as usize)
                .expect("validated node plane");
            let behind = compact_plane_distance(*plane, camera_origin) < 0;
            let near = node.children[behind as usize];
            let far = node.children[usize::from(!behind)];
            for child in [far, near] {
                if child >= 0 {
                    self.pxbsp_node_stack.push(child as u32 | child_tag);
                } else if self.pxbsp_node_leaf_marks {
                    let leaf_index = (-1i32 - child as i32) as usize;
                    if !self.pxbsp_leaf_visible(leaf_index) {
                        continue;
                    }
                    let leaf = map.leaves().get(leaf_index).expect("validated node leaf");
                    let start = leaf.first_mark_surface as usize;
                    let end = start + leaf.mark_surface_count as usize;
                    for mark_index in start..end {
                        // `validate_references` checked every mark index
                        // against the face count at load.
                        let face =
                            unsafe { *map.mark_surfaces_native().get_unchecked(mark_index) }
                                as usize;
                        if packed_face_state(&self.pxbsp_face_state, face) != 0 {
                            set_packed_face_state(
                                &mut self.frame_pxbsp_face_state,
                                face,
                                PXBSP_FRAME_FALLBACK,
                            );
                            self.frame_pxbsp_face_clip_mask[face] = mask;
                        }
                    }
                }
            }

            let start = node.first_face as usize;
            let end = start + node.face_count as usize;
            for face in start..end {
                if packed_face_state(&self.pxbsp_face_state, face) != 0 {
                    let authored_front = behind
                        == (unsafe { map.face_ref_unchecked(face) }.flags() & FACE_BACKSIDE != 0);
                    set_packed_face_state(
                        &mut self.frame_pxbsp_face_state,
                        face,
                        if authored_front {
                            PXBSP_FRAME_NODE_FRONT
                        } else {
                            PXBSP_FRAME_NODE_BACK
                        },
                    );
                    self.frame_pxbsp_face_clip_mask[face] = mask;
                }
            }
        }

        if !self.pxbsp_node_leaf_marks {
            // Imported func_* brushes are deliberately staticized into leaf
            // mark lists and do not belong to world-node face ranges.
            for &face in &self.visible_pxbsp_faces {
                if packed_face_state(&self.pxbsp_face_state, face as usize) == 1 {
                    set_packed_face_state(
                        &mut self.frame_pxbsp_face_state,
                        face as usize,
                        PXBSP_FRAME_FALLBACK,
                    );
                    self.frame_pxbsp_face_clip_mask[face as usize] = PXBSP_CLIP_ALL_PLANES;
                }
            }
        }
        // Reading the frame marks back out in index order yields exactly the
        // chain the old filter over the sorted PVS list produced, but costs one
        // load per four faces instead of one per PVS face: the traversal marks
        // only a subset, and whole empty bytes are skipped.
        if !collect_marked_faces_ascending(
            &self.frame_pxbsp_face_state,
            self.pxbsp_face_count,
            &mut self.frame_pxbsp_faces,
        ) {
            // Traversal may already have marked faces which did not fit in the
            // bounded output chain. This invalid-cook path is rare, so clear
            // the complete packed table rather than leaving untracked marks
            // for the next frame.
            self.frame_pxbsp_face_state.fill(0);
            self.frame_pxbsp_faces.clear();
            return false;
        }
        true
    }

    fn retire_frame_pxbsp_selection(&mut self) {
        for &face in &self.frame_pxbsp_faces {
            set_packed_face_state(&mut self.frame_pxbsp_face_state, face as usize, 0);
        }
        self.frame_pxbsp_faces.clear();
    }

    fn attach_external_frame_pxbsp_faces(&mut self) {
        if self.external_frame_pxbsp_faces.is_null() {
            return;
        }
        debug_assert!(self.frame_pxbsp_faces.is_empty());
        debug_assert_eq!(self.frame_pxbsp_faces.capacity(), 0);
        self.frame_pxbsp_faces = unsafe {
            Vec::from_raw_parts(
                self.external_frame_pxbsp_faces,
                0,
                self.visible_pxbsp_faces.capacity(),
            )
        };
    }

    fn detach_external_frame_pxbsp_faces(&mut self) {
        if self.external_frame_pxbsp_faces.is_null() || self.frame_pxbsp_faces.capacity() == 0 {
            return;
        }
        debug_assert!(self.frame_pxbsp_faces.is_empty());
        debug_assert_eq!(
            self.frame_pxbsp_faces.as_mut_ptr(),
            self.external_frame_pxbsp_faces
        );
        let frame = core::mem::take(&mut self.frame_pxbsp_faces);
        core::mem::forget(frame);
    }
}

fn external_face_chain(storage: &'static mut [u16]) -> Vec<u16> {
    // SAFETY: the caller gives this storage exclusively to the renderer for
    // the session. Length starts at zero, capacity is the complete slice, all
    // writes stay capacity-checked, and Renderer::drop forgets the facade so
    // the global allocator never sees caller-owned memory.
    unsafe { Vec::from_raw_parts(storage.as_mut_ptr(), 0, storage.len()) }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if self.external_frame_pxbsp_faces.is_null() {
            return;
        }
        self.retire_frame_pxbsp_selection();
        self.detach_external_frame_pxbsp_faces();
        let visible = core::mem::take(&mut self.visible_pxbsp_faces);
        core::mem::forget(visible);
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

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct PxbspMaterialState {
    texture_page: u16,
    color_command_word: u32,
    uv_offset: [u8; 2],
    page_uv_offset: [u8; 2],
    color_scale_q7: u8,
}

/// Everything the face loop needs from one material and its texture binding,
/// resolved once per material per frame instead of once per face.
///
/// This project's world is 1699 faces drawn through 12 materials, and every
/// term below depends only on the material index, its binding and the
/// animation tick. Recomputing it per face meant decoding the sixteen-byte
/// material record and re-deriving the animation state roughly 270 times a
/// frame to produce at most a dozen distinct answers.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct PxbspResolvedMaterial {
    state: PxbspMaterialState,
    texture_window_word: u32,
    /// Epoch this entry was filled for; see [`Renderer::pxbsp_material_epoch`].
    epoch: u32,
    clut: u16,
    /// Material index this slot holds, so a direct-mapped collision refills
    /// rather than answering for the wrong material.
    material_index: u16,
    policy: u8,
}

impl PxbspResolvedMaterial {
    /// Slot `material_index` maps to. Direct-mapped, so two materials whose
    /// indices are congruent modulo the slot count share one entry.
    #[inline(always)]
    fn slot(material_index: usize) -> usize {
        material_index & (PXBSP_MATERIAL_CACHE_SLOTS - 1)
    }

    /// Whether this entry already answers for `material_index` in `epoch`.
    /// Both terms are load-bearing: the epoch rejects a value resolved under
    /// a different animation tick or binding table, the index rejects a
    /// direct-mapped collision.
    #[inline(always)]
    fn answers(&self, material_index: usize, epoch: u32) -> bool {
        self.epoch == epoch && usize::from(self.material_index) == material_index
    }
}

/// Direct-mapped slots for [`PxbspResolvedMaterial`]. A map with more
/// materials than this still renders identically; colliding indices simply
/// refill their shared slot instead of hitting.
const PXBSP_MATERIAL_CACHE_SLOTS: usize = 32;

mod pxbsp_material_policy {
    /// Sidedness, stored as `material_flags::FACE_MASK` verbatim so the
    /// policy form of the draw test can reuse the same match arms.
    pub const FACE_MASK: u8 = crate::pxbsp::material_flags::FACE_MASK as u8;
    const _: () = assert!(FACE_MASK as u16 == crate::pxbsp::material_flags::FACE_MASK);
    /// The face reveals the scene sky instead of submitting its polygon.
    pub const SKY: u8 = 0x04;
    /// Opaque and unanimated, so page-local UVs may skip GP0(E2).
    pub const COMPACT: u8 = 0x08;
    /// A texture binding resolved for this material.
    pub const BOUND: u8 = 0x10;
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
    let color_scale_q7 = pxbsp_animation_color_scale_q7(animation, tick);
    let uv_offset = [
        binding.uv_origin[0].wrapping_add(animated[0]),
        binding.uv_origin[1].wrapping_add(animated[1]),
    ];
    PxbspMaterialState {
        texture_page,
        color_command_word,
        uv_offset,
        page_uv_offset: [
            uv_offset[0].wrapping_add(binding.page_uv_origin[0]),
            uv_offset[1].wrapping_add(binding.page_uv_origin[1]),
        ],
        color_scale_q7,
    }
}

fn pxbsp_animation_color_scale_q7(animation: PxbspMaterialAnimation, tick: u32) -> u8 {
    let PxbspMaterialAnimation::LightPulse {
        minimum_q7,
        maximum_q7,
        ticks_per_cycle,
        phase,
    } = animation
    else {
        return 128;
    };
    let period = u32::from(ticks_per_cycle.max(1));
    let phase_tick = tick.wrapping_add(u32::from(phase)) % period;
    let angle_q12 = (phase_tick.saturating_mul(4096) / period) as u16;
    let wave_q12 = (sin_q12(angle_q12).saturating_add(4096) / 2).clamp(0, 4096);
    let range = i32::from(maximum_q7.saturating_sub(minimum_q7));
    i32::from(minimum_q7)
        .saturating_add(range.saturating_mul(wave_q12) >> 12)
        .clamp(0, 255) as u8
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
        PxbspMaterialAnimation::LightPulse { .. } => [0; 2],
    }
}

fn pxbsp_scroll_axis(speed_q8: i16, phase: u8, period: u8, tick: u32) -> u8 {
    let travelled_q8 =
        i64::from(speed_q8).saturating_mul(i64::from(tick)) / PXBSP_MATERIAL_TICKS_PER_SECOND;
    (travelled_q8 / 256 + i64::from(phase)).rem_euclid(i64::from(period.max(1))) as u8
}

#[cfg(test)]
fn pxbsp_face_draws(material: PxbspMaterial, face_flags: u16, authored_front: bool) -> bool {
    pxbsp_policy_face_draws(
        (material.flags & material_flags::FACE_MASK) as u8,
        face_flags,
        authored_front,
    )
}

/// [`pxbsp_face_draws`] against a resolved material's cached policy bits. The
/// sidedness bits are `material_flags::FACE_MASK` verbatim, so the two agree
/// by construction.
#[inline(always)]
fn pxbsp_policy_face_draws(policy: u8, face_flags: u16, authored_front: bool) -> bool {
    if face_flags & FACE_TWO_SIDED != 0 {
        return true;
    }
    match u16::from(policy & pxbsp_material_policy::FACE_MASK) {
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
            PXBSP_RENDER_PROFILE,
        )
    }
}

unsafe fn flush_pxbsp_batch(
    vertices: &mut [ClassicAffineVertex],
    vertex_count: usize,
    surfaces: &[ClassicAffineMixedBatchSurface],
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
        submit_classic_affine_mixed_batch(
            vertices.as_mut_ptr(),
            vertex_count,
            surfaces.as_ptr(),
            surface_count,
            output,
            PXBSP_RENDER_PROFILE,
        )
    }
}

fn front_facing(map: &ResidentMap, face: Face, point: Vec3I32) -> bool {
    let plane = unsafe { map.planes().get_unchecked(face.plane as usize) };
    let behind = plane_distance(plane, point) < 0;
    behind == (face.flags & FACE_BACKSIDE != 0)
}

/// `MVMVA(mx=LLM, vx=V0, cv=none, sf=0, lm=0)`: three plane dots at once.
#[cfg(target_arch = "mips")]
const MVMVA_LLM_V0_SF0: u32 = 0x4A02_6012;
/// `MVMVA(mx=LCM, vx=V0, cv=none, sf=0, lm=0)`.
#[cfg(target_arch = "mips")]
const MVMVA_LCM_V0_SF0: u32 = 0x4A04_6012;

/// Load the five frustum planes for [`Renderer::pxbsp_face_clip_gte`]:
/// planes 0..3 fill the light matrix rows, planes 3..5 the first two colour
/// matrix rows. Both matrices are free during the world pass (the batch
/// writer only uses the rotation and translation), and every later lit
/// submission reloads them.
#[cfg(target_arch = "mips")]
#[inline(never)]
fn load_gte_clip_planes(planes: &[([i32; 3], i32); 5]) {
    #[inline(always)]
    fn row(plane: &([i32; 3], i32)) -> [i16; 3] {
        // Near rows are Q12 view rows, side rows are rounded to an L1 norm
        // of at most SIDE_L1_LIMIT, so every component fits.
        debug_assert!(plane
            .0
            .iter()
            .all(|c| (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(c)));
        [plane.0[0] as i16, plane.0[1] as i16, plane.0[2] as i16]
    }
    scene::load_light_matrix(&Mat3I16 {
        m: [row(&planes[0]), row(&planes[1]), row(&planes[2])],
    });
    scene::load_light_colour_matrix(&Mat3I16 {
        m: [row(&planes[3]), row(&planes[4]), [0; 3]],
    });
}

/// `mfc2 $8, MAC1..MAC3` encodings for [`gte_dot_one`].
#[cfg(target_arch = "mips")]
const MFC2_MAC1: u32 = 0x4808_c800;
#[cfg(target_arch = "mips")]
const MFC2_MAC2: u32 = 0x4808_d000;
#[cfg(target_arch = "mips")]
const MFC2_MAC3: u32 = 0x4808_d800;

/// One vertex through `OP` and a single MAC result selected by `READ`.
#[cfg(target_arch = "mips")]
#[inline(always)]
unsafe fn gte_dot_one<const OP: u32, const READ: u32>(xy: u32, z: u32) -> i32 {
    let a: u32;
    unsafe {
        core::arch::asm!(
            ".word 0x48880000", // mtc2 $8, VXY0
            ".word 0x48890800", // mtc2 $9, VZ0
            ".word 0",
            ".word 0",
            ".word {op}",
            ".word {read}",
            ".word 0",
            op = const OP,
            read = const READ,
            inlateout("$8") xy => a,
            in("$9") z,
            options(nostack, nomem, preserves_flags),
        );
    }
    a as i32
}

/// One vertex through `OP` (an MVMVA against V0) and its three MAC results,
/// with the console-confirmed V0 commit distance used by the SDK's AABB
/// classifier.
#[cfg(target_arch = "mips")]
#[inline(always)]
unsafe fn gte_dot3<const OP: u32>(xy: u32, z: u32) -> (i32, i32, i32) {
    let a: u32;
    let b: u32;
    let c: u32;
    unsafe {
        core::arch::asm!(
            ".word 0x48880000", // mtc2 $8, VXY0
            ".word 0x48890800", // mtc2 $9, VZ0
            ".word 0",
            ".word 0",
            ".word {op}",
            ".word 0x4808c800", // mfc2 $8, MAC1
            ".word 0x4809d000", // mfc2 $9, MAC2
            ".word 0x480ad800", // mfc2 $10, MAC3
            ".word 0",
            op = const OP,
            inlateout("$8") xy => a,
            inlateout("$9") z => b,
            out("$10") c,
            options(nostack, nomem, preserves_flags),
        );
    }
    (a as i32, b as i32, c as i32)
}

fn front_facing_pxbsp(map: &PxbspResidentMap, plane: usize, flags: u16, point: Vec3I32) -> bool {
    let plane = unsafe { map.planes().get_unchecked(plane) };
    let behind = compact_plane_distance(*plane, point) < 0;
    behind == (flags & FACE_BACKSIDE != 0)
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

#[inline(always)]
fn compact_plane_distance(plane: CompactPlane, point: Vec3I32) -> i32 {
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

fn scale_baked_color_q7(color: u32, scale_q7: u8) -> u32 {
    if scale_q7 == 128 {
        return color;
    }
    let scale = u32::from(scale_q7);
    let channel = |shift: u32| (((color >> shift) & 0xff) * scale / 128).min(255);
    channel(0) | (channel(8) << 8) | (channel(16) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxbsp::PxbspLumpKind;

    #[test]
    fn exact_view_rotation_matches_the_table_form_at_table_angles() {
        // At angles the 256-step table represents exactly, the trig pairs
        // are +-4096 / 0 and both constructions must agree cell for cell.
        for (pitch_q12, yaw_q12) in [(0u16, 0u16), (0, 1024), (1024, 0), (3072, 2048), (2048, 3072)] {
            let table = Mat3I16::rotate_xyz(pitch_q12 >> 4, yaw_q12 >> 4, 0);
            let trig = |angle: u16| -> (i16, i16) {
                (sin_q12(angle) as i16, cos_q12(angle) as i16)
            };
            let (sp, cp) = trig(pitch_q12);
            let (sy, cy) = trig(yaw_q12);
            assert_eq!(pxbsp_view_rotation(sp, cp, sy, cy).m, table.m, "pitch {pitch_q12} yaw {yaw_q12}");
        }
    }
    use crate::pxbsp_resident::tests::{valid_lumps, write_file};
    use crate::SliceReader;
    use alloc::boxed::Box;
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
            page_uv_origin: [32, 64],
            texture_size: [64, 32],
        };
        let state = pxbsp_material_state(material, binding, 60);
        assert_eq!((state.texture_page >> 5) & 3, 2);
        assert_eq!(state.color_command_word, 0x3600_0000);
        assert_eq!(state.uv_offset, [16, 24]);
        assert_eq!(state.page_uv_offset, [48, 88]);
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
    fn resolves_pxbsp_light_pulse_and_scales_each_rgb_channel() {
        let animation = PxbspMaterialAnimation::LightPulse {
            minimum_q7: 96,
            maximum_q7: 192,
            ticks_per_cycle: 96,
            phase: 0,
        };
        assert_eq!(pxbsp_animation_color_scale_q7(animation, 0), 144);
        assert_eq!(pxbsp_animation_color_scale_q7(animation, 24), 192);
        assert_eq!(pxbsp_animation_color_scale_q7(animation, 72), 96);
        assert_eq!(scale_baked_color_q7(0x0010_2040, 192), 0x0018_3060);
    }

    #[test]
    fn a_material_cache_entry_answers_only_for_its_own_material_and_epoch() {
        // No cooked map here reaches the slot count, so the collision and
        // staleness rules are pinned directly rather than through a fixture.
        let fresh = PxbspResolvedMaterial::default();
        assert!(
            !fresh.answers(0, 1),
            "a never-filled slot must miss; epochs start at one"
        );
        let filled = PxbspResolvedMaterial {
            material_index: 7,
            epoch: 42,
            ..PxbspResolvedMaterial::default()
        };
        assert!(filled.answers(7, 42));
        assert!(!filled.answers(7, 43), "a stale epoch must miss");
        assert!(
            !filled.answers(7 + PXBSP_MATERIAL_CACHE_SLOTS, 42),
            "a direct-mapped collision must miss"
        );
        assert_eq!(
            PxbspResolvedMaterial::slot(7),
            PxbspResolvedMaterial::slot(7 + PXBSP_MATERIAL_CACHE_SLOTS),
            "the collision above has to be a real one"
        );
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
            page_uv_origin: [0; 2],
            texture_size: [64; 2],
        };
        let mut packets = [0u32; 512];
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        assert_eq!(map.point_leaf_index(camera.origin), Some(1));
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        assert_eq!(packed_face_state(&renderer.pxbsp_face_state, 0), 2);
        assert_eq!(renderer.visible_pxbsp_faces, [0]);
        let face = map.faces().get(0).expect("face");
        assert!(front_facing_pxbsp(
            &map,
            face.plane as usize,
            face.flags,
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
        assert_eq!(
            packed_face_state(&renderer.frame_pxbsp_face_state, 0),
            0,
            "world draw must retire per-frame marks before model scratch reuse"
        );
        assert!(renderer.frame_pxbsp_faces.is_empty());
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
    fn draws_cooker_proven_page_local_face_without_texture_window_packets() {
        configure_projection();
        let mut lumps = valid_lumps();
        let mut vertices = Vec::new();
        for (position, uv) in [
            ([64i16, -16, -16], [1u8, 2]),
            ([64, 16, -16], [9, 2]),
            ([64, 0, 16], [1, 10]),
        ] {
            for component in position {
                vertices.extend_from_slice(&component.to_le_bytes());
            }
            vertices.extend_from_slice(&[uv[0], uv[1], 128, 0, 0, 0]);
        }
        lumps[PxbspLumpKind::Vertices as usize] = vertices;
        lumps[PxbspLumpKind::Faces as usize][6] = FACE_PAGE_LOCAL_UV as u8;
        let mins = [64i16, -16, -16].map(crate::encode_node_bound_min);
        let maxs = [64i16, 16, 16].map(crate::encode_node_bound_max);
        lumps[PxbspLumpKind::Nodes as usize][6..9].copy_from_slice(&mins.map(|value| value as u8));
        lumps[PxbspLumpKind::Nodes as usize][9..12].copy_from_slice(&maxs.map(|value| value as u8));
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(11, &mut SliceReader::new(&bytes))
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
            page_uv_origin: [32, 64],
            texture_size: [64; 2],
        };
        let mut packets = [0u32; 512];
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
        assert!(frame.stats.packets > 0);
        let mut offset = 0usize;
        let mut packet_count = 0u32;
        while offset < frame.packet_words {
            let data_words = (packets[offset] >> 24) as usize;
            let uv_offsets: &[usize] = match data_words {
                9 => {
                    assert_eq!(packets[offset + 1] >> 24, 0x34);
                    &[3, 6, 9]
                }
                12 => {
                    assert_eq!(packets[offset + 1] >> 24, 0x3c);
                    &[3, 6, 9, 12]
                }
                _ => panic!("unexpected compact packet length {data_words}"),
            };
            assert_ne!(packets[offset + 1], binding.texture_window_word);
            for &uv_offset in uv_offsets {
                let uv_word = packets[offset + uv_offset];
                assert!((32..96).contains(&((uv_word & 0xff) as u8)));
                assert!((64..128).contains(&(((uv_word >> 8) & 0xff) as u8)));
            }
            offset += data_words + 1;
            packet_count += 1;
        }
        assert_eq!(offset, frame.packet_words);
        assert_eq!(packet_count, frame.stats.packets);
    }

    #[test]
    fn sky_aperture_is_reported_without_a_material_binding() {
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
        let mut packets = [0u32; 16];
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        let frame = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[None],
            0,
            &mut packets,
        );

        assert_eq!(frame.stats.visible_faces, 1);
        assert_eq!(frame.stats.visible_sky_apertures, 1);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert!(!frame.stats.packet_overflow_avoided);
        assert_eq!(frame.stats.packets, 0);
        assert_eq!(frame.stats.hardware_triangles, 0);
        assert_eq!(frame.packet_words, 0);
    }

    #[test]
    fn always_visible_sky_skips_aperture_faces() {
        configure_projection();
        let mut lumps = valid_lumps();
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
        let mut packets = [0u32; 16];
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        renderer.set_track_sky_apertures(false);
        let frame = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[None],
            0,
            &mut packets,
        );

        assert_eq!(frame.stats.visible_faces, 0);
        assert_eq!(frame.stats.visible_sky_apertures, 0);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert_eq!(frame.stats.packets, 0);
        assert_eq!(frame.packet_words, 0);
    }

    #[test]
    fn legacy_directional_sky_bit_migrates_to_the_aperture_contract() {
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
            .copy_from_slice(&material_flags::DIRECTIONAL_SKY.to_le_bytes());
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
        let mut packets = [0u32; 16];
        let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
        assert!(renderer.mark_visible_pxbsp_faces(&map, camera.origin));
        let frame = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[None],
            0,
            &mut packets,
        );

        assert_eq!(frame.stats.visible_faces, 1);
        assert_eq!(frame.stats.visible_sky_apertures, 1);
        assert_eq!(frame.stats.unresolved_material_faces, 0);
        assert!(!frame.stats.packet_overflow_avoided);
        assert_eq!(frame.stats.packets, 0);
        assert_eq!(frame.stats.hardware_triangles, 0);
        assert_eq!(frame.packet_words, 0);
    }

    #[test]
    fn pxbsp_renderer_allocates_exact_world_scratch() {
        let renderer = Renderer::new_pxbsp_with_capacities(37, 11, 9);
        assert!(renderer.face_visible.is_empty());
        assert_eq!(renderer.pxbsp_face_count, 37);
        assert_eq!(renderer.pxbsp_face_state.len(), 10);
        assert_eq!(renderer.visible_pxbsp_faces.len(), 0);
        assert_eq!(renderer.visible_pxbsp_faces.capacity(), 9);
        assert_eq!(renderer.frame_pxbsp_faces.len(), 0);
        assert_eq!(renderer.frame_pxbsp_faces.capacity(), 9);
        assert_eq!(renderer.frame_pxbsp_face_state.len(), 10);
        assert_eq!(renderer.pxbsp_node_visible.len(), 2);
        assert_eq!(renderer.pxbsp_node_discovered.len(), 2);
        assert!(renderer.alias_projected.is_empty());
        assert_eq!(renderer.visible_entity_indices.capacity(), 0);
    }

    #[test]
    fn undersized_cooked_face_chain_fails_closed_without_heap_growth() {
        let bytes = write_file(&valid_lumps());
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(3, &mut SliceReader::new(&bytes))
            .expect("resident map");
        let mut renderer =
            Renderer::new_pxbsp_with_capacities(map.faces().len(), map.nodes().len(), 0);
        let origin = Vec3I32 {
            x: 1 << 12,
            y: 0,
            z: 0,
        };

        assert!(!renderer.mark_visible_pxbsp_faces(&map, origin));
        assert_eq!(renderer.visible_pxbsp_faces.capacity(), 0);
        assert!(renderer.visible_pxbsp_faces.is_empty());
        assert_eq!(renderer.cached_pxbsp_visibility, None);
    }

    #[test]
    fn external_face_chains_use_exact_caller_owned_storage() {
        let visible = Box::leak(vec![0u16; 17].into_boxed_slice());
        let frame = Box::leak(vec![0u16; 17].into_boxed_slice());
        let visible_ptr = visible.as_mut_ptr();
        let frame_ptr = frame.as_mut_ptr();

        let renderer = Renderer::new_pxbsp_with_external_face_chains(23, 5, visible, frame);

        assert_eq!(renderer.visible_pxbsp_faces.capacity(), 17);
        assert_eq!(renderer.frame_pxbsp_faces.capacity(), 0);
        assert_eq!(renderer.visible_pxbsp_faces.as_ptr(), visible_ptr);
        assert_eq!(renderer.external_frame_pxbsp_faces, frame_ptr);
        drop(renderer);
    }

    #[test]
    fn external_frame_chain_can_be_overwritten_between_identical_world_draws() {
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
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(19, &mut SliceReader::new(&bytes))
            .expect("resident map");
        let face_capacity = map.faces().len();
        let visible = Box::leak(vec![0u16; face_capacity].into_boxed_slice());
        let frame = Box::leak(vec![0u16; face_capacity].into_boxed_slice());
        let frame_ptr = frame.as_mut_ptr();
        let mut renderer = Renderer::new_pxbsp_with_external_face_chains(
            face_capacity,
            map.nodes().len(),
            visible,
            frame,
        );
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
            page_uv_origin: [0; 2],
            texture_size: [64; 2],
        };
        let mut first_packets = [0u32; 512];
        let first = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[Some(binding)],
            0,
            &mut first_packets,
        );
        assert!(first.packet_words != 0);
        assert!(renderer.frame_pxbsp_faces.is_empty());
        assert_eq!(renderer.frame_pxbsp_faces.capacity(), 0);
        assert!(renderer
            .frame_pxbsp_face_state
            .iter()
            .all(|&state| state == 0));

        // Model projection owns the same bytes after the BSP pass. Hostile
        // values reproduce the former failure deterministically: a retained
        // Vec length made the next frame interpret these as face indices.
        for index in 0..face_capacity {
            unsafe { frame_ptr.add(index).write(u16::MAX) };
        }

        let mut second_packets = [0u32; 512];
        let second = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[Some(binding)],
            0,
            &mut second_packets,
        );
        assert_eq!(second.packet_words, first.packet_words);
        assert_eq!(
            &second_packets[..second.packet_words],
            &first_packets[..first.packet_words]
        );
        assert!(renderer.frame_pxbsp_faces.is_empty());
        assert_eq!(renderer.frame_pxbsp_faces.capacity(), 0);
        assert!(renderer
            .frame_pxbsp_face_state
            .iter()
            .all(|&state| state == 0));
    }

    #[test]
    fn undersized_frame_chain_failure_clears_untracked_marks() {
        configure_projection();
        let bytes = write_file(&valid_lumps());
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(23, &mut SliceReader::new(&bytes))
            .expect("resident map");
        let mut renderer = Renderer::new_pxbsp_with_face_chains(
            map.faces().len(),
            map.nodes().len(),
            Vec::with_capacity(map.faces().len()),
            Vec::new(),
        );
        let camera = Camera {
            origin: Vec3I32 {
                x: 1 << 12,
                y: 0,
                z: 0,
            },
            angles: [0; 3],
        };
        let mut packets = [0u32; 32];

        let frame = renderer.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &[None],
            0,
            &mut packets,
        );

        assert_eq!(frame.packet_words, 0);
        assert!(renderer.frame_pxbsp_faces.is_empty());
        assert!(renderer
            .frame_pxbsp_face_state
            .iter()
            .all(|&state| state == 0));
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
            FrustumPlanes::from_view(
                &view.rotation,
                view.translation,
                [camera.origin.x >> 12, camera.origin.y >> 12, camera.origin.z >> 12],
                renderer.view_projection,
            );
        assert!(renderer.select_frame_pxbsp_faces(&map, camera.origin, &frustum));
        assert_eq!(renderer.frame_pxbsp_faces, [0]);
        assert_eq!(
            packed_face_state(&renderer.frame_pxbsp_face_state, 0),
            PXBSP_FRAME_FALLBACK
        );
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
            page_uv_origin: [0; 2],
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

    #[test]
    fn pxbsp_renderer_uses_the_third_person_affine_profile() {
        assert_eq!(
            PXBSP_RENDER_PROFILE,
            ClassicAffineProfile::PXBSP_THIRD_PERSON
        );
        assert_eq!(PXBSP_RENDER_PROFILE.subdivide_once_at, 272);
        assert_eq!(PXBSP_RENDER_PROFILE.subdivide_twice_at, 136);
        assert_eq!(PXBSP_RENDER_PROFILE.ot_depth, 2048);
    }

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
            FrustumPlanes::from_view(
                &view.rotation,
                view.translation,
                [camera.origin.x >> 12, camera.origin.y >> 12, camera.origin.z >> 12],
                projection,
            ),
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

    /// Deterministic small PRNG so the sweeps below cover many boxes without
    /// pulling in a dependency.
    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *state
    }

    /// The predicate `cull_polygon` replaced: one pass over the vertices,
    /// every plane evaluated for each, exactly as the code shipped before the
    /// plane-major rewrite.
    fn vertex_major_clip(
        planes: &FrustumPlanes,
        clip_mask: u8,
        polygon: &[[i16; 3]],
    ) -> Option<bool> {
        if polygon.is_empty() {
            return None;
        }
        let mut wholly_outside = clip_mask;
        let mut needs_near_clip = false;
        for position in polygon {
            let outside = planes.vertex_outside_mask(*position);
            wholly_outside &= outside;
            needs_near_clip |= outside & 0x01 != 0;
        }
        (wholly_outside == 0).then_some(needs_near_clip)
    }

    #[test]
    fn plane_major_clip_matches_the_vertex_major_scan() {
        // Same answer, every mask, for polygons spread across the frustum
        // boundary: rejection, acceptance, and the near-clip flag.
        let (planes, _) = planes_for([37, 70, 273], 1024, 100);
        let mut state = 0x51ed_2c07u32;
        let mut rejected = 0usize;
        let mut near = 0usize;
        let mut kept = 0usize;
        // Anchored on the camera so the sweep straddles the frustum rather
        // than sitting far outside it; a polygon spread comparable to the
        // anchor spread is what produces near-plane crossings.
        let mut kept_by_band = 0usize;
        let mut compared = 0usize;
        for _ in 0..6000 {
            let mut axis = || (lcg(&mut state) % 700) as i16 - 350;
            let anchor = [37 + axis(), 70 + axis(), 273 + axis()];
            let count = 3 + (lcg(&mut state) % 6) as usize;
            let mut polygon = [[0i16; 3]; 8];
            for slot in polygon.iter_mut().take(count) {
                let mut spread = || (lcg(&mut state) % 500) as i16 - 250;
                *slot = [
                    anchor[0] + spread(),
                    anchor[1] + spread(),
                    anchor[2] + spread(),
                ];
            }
            let polygon = &polygon[..count];
            // Only masks a node walk could actually hand down: every plane the
            // mask drops must really be satisfied by every vertex, which is
            // what an ancestor box proves.
            let satisfied = (0..5u8)
                .filter(|plane| {
                    polygon
                        .iter()
                        .all(|p| planes.vertex_outside_mask(*p) & (1 << plane) == 0)
                })
                .fold(0u8, |acc, plane| acc | 1 << plane);
            for drop in 0..32u8 {
                let clip_mask = 0x1f & !(drop & satisfied);
                let expected = vertex_major_clip(&planes, clip_mask, polygon);
                let actual =
                    FrustumPlanes::cull_polygon(&planes.planes, planes.side_error, clip_mask, polygon.len(), |i| {
                        polygon[i]
                    });
                // The exact scan is the oracle. The plane-major test may keep
                // a polygon the oracle rejects when every vertex sits in a
                // rounded side plane's error band (conservative), but it may
                // never reject one the oracle keeps, and the near-clip answer
                // is exact.
                match (expected, actual) {
                    (None, None) => {}
                    (None, Some(_)) => kept_by_band += 1,
                    (Some(_), None) => panic!(
                        "cull_polygon rejected a kept polygon for mask {clip_mask:#x} on {polygon:?}"
                    ),
                    (Some(expected_near), Some(actual_near)) => assert_eq!(
                        actual_near, expected_near,
                        "near clip answer differs for mask {clip_mask:#x} on {polygon:?}"
                    ),
                }
                compared += 1;
            }
            match vertex_major_clip(&planes, 0x1f, polygon) {
                None => {
                    // An empty mask means an ancestor box proved all five
                    // planes for this subtree. It must then keep even a
                    // polygon the full mask rejects, unclipped: that is a
                    // claim about the mask, not about the geometry.
                    assert_eq!(
                        FrustumPlanes::cull_polygon(&planes.planes, planes.side_error, 0, polygon.len(), |i| polygon
                            [i]),
                        Some(false)
                    );
                    rejected += 1;
                }
                Some(true) => near += 1,
                Some(false) => kept += 1,
            }
        }
        assert!(
            rejected > 100 && near > 20 && kept > 100,
            "sweep degenerated: {rejected} rejected, {near} near-clipped, {kept} kept"
        );
        assert!(compared > 100_000, "sweep collapsed to {compared} comparisons");
        assert!(
            kept_by_band * 50 < compared,
            "{kept_by_band} of {compared} rejections were withheld by the error band"
        );
    }

    #[test]
    fn masked_classification_matches_the_full_five_plane_answer() {
        // A residual mask may only omit planes the caller already proved. For
        // every box, drop exactly the planes whose inner corner is inside (the
        // proof `cull_aabb` performs) and require the same verdict.
        let (planes, _) = planes_for([37, 70, 273], 1024, 100);
        let mut state = 0x1234_5678u32;
        let mut checked = 0usize;
        for _ in 0..4000 {
            let mut axis = || (lcg(&mut state) % 1200) as i16 - 600;
            let a = [axis(), axis(), axis()];
            let mut extent = || (lcg(&mut state) % 160) as i16;
            let b = [extent(), extent(), extent()];
            let mins = Vec3I16 {
                x: a[0],
                y: a[1],
                z: a[2],
            };
            let maxs = Vec3I16 {
                x: a[0] + b[0],
                y: a[1] + b[1],
                z: a[2] + b[2],
            };
            let full =
                planes.classify_aabb([mins.x, mins.y, mins.z], [maxs.x, maxs.y, maxs.z], 0x1f);
            let Some(residual) = planes.cull_aabb(mins, maxs, 0x1f) else {
                assert_eq!(full, AabbClass::Outside);
                continue;
            };
            assert_ne!(full, AabbClass::Outside);
            assert_eq!(
                full == AabbClass::Inside,
                residual == 0,
                "cull_aabb residual {residual:#x} disagrees with {full:?} for {mins:?}..{maxs:?}"
            );
            let masked =
                planes.classify_aabb([mins.x, mins.y, mins.z], [maxs.x, maxs.y, maxs.z], residual);
            assert_eq!(
                masked, full,
                "masked classification changed the verdict for {mins:?}..{maxs:?}"
            );
            checked += 1;
        }
        assert!(checked > 100, "sweep degenerated to {checked} live boxes");
    }

    #[test]
    fn a_proven_plane_cannot_reject_a_contained_box() {
        // The inheritance contract: whatever `cull_aabb` clears for an outer
        // box stays satisfied for every box inside it, so a descendant never
        // needs to retest those planes.
        let (planes, _) = planes_for([37, 70, 273], 1024, 100);
        let mut state = 0x0bad_c0deu32;
        for _ in 0..2000 {
            let mut axis = || (lcg(&mut state) % 900) as i16 - 450;
            let a = [axis(), axis(), axis()];
            let outer_mins = Vec3I16 {
                x: a[0],
                y: a[1],
                z: a[2],
            };
            let outer_maxs = Vec3I16 {
                x: a[0] + 300,
                y: a[1] + 300,
                z: a[2] + 300,
            };
            let Some(residual) = planes.cull_aabb(outer_mins, outer_maxs, 0x1f) else {
                continue;
            };
            // An arbitrary sub-box of the outer box.
            let mut inset = || (lcg(&mut state) % 150) as i16;
            let inner_mins = Vec3I16 {
                x: outer_mins.x + inset(),
                y: outer_mins.y + inset(),
                z: outer_mins.z + inset(),
            };
            let inner_maxs = Vec3I16 {
                x: inner_mins.x + inset(),
                y: inner_mins.y + inset(),
                z: inner_mins.z + inset(),
            };
            for plane in 0..5u8 {
                if residual & (1 << plane) != 0 {
                    continue;
                }
                let only = 1u8 << plane;
                assert_eq!(
                    planes.classify_aabb(
                        [inner_mins.x, inner_mins.y, inner_mins.z],
                        [inner_maxs.x, inner_maxs.y, inner_maxs.z],
                        only,
                    ),
                    AabbClass::Inside,
                    "plane {plane} was proven for the outer box but rejects a box inside it"
                );
            }
        }
    }

    #[test]
    fn marked_face_collection_matches_a_sorted_unique_chain() {
        // The ascending readback replaced `sort_unstable` on the face chain;
        // it has to produce exactly the same sequence.
        let face_count = 1699usize;
        let mut states = vec![0u8; face_count.div_ceil(4)];
        let mut state = 0xfeed_face_u32;
        let mut expected: Vec<u16> = Vec::new();
        for _ in 0..900 {
            let face = (lcg(&mut state) as usize) % face_count;
            if packed_face_state(&states, face) == 0 {
                expected.push(face as u16);
            }
            set_packed_face_state(&mut states, face, 1);
        }
        expected.sort_unstable();
        let mut output: Vec<u16> = Vec::with_capacity(face_count);
        assert!(collect_marked_faces_ascending(
            &states,
            face_count,
            &mut output
        ));
        assert_eq!(output, expected);

        // A chain smaller than the marked set fails closed rather than growing.
        let mut small: Vec<u16> = Vec::with_capacity(expected.len() - 1);
        assert!(!collect_marked_faces_ascending(
            &states, face_count, &mut small
        ));
    }

    #[test]
    fn face_collection_ignores_marks_past_the_face_count() {
        // The packed table rounds up to whole bytes; the tail slots of the
        // last byte are not faces and must never be emitted.
        let face_count = 6usize;
        let mut states = vec![0u8; face_count.div_ceil(4)];
        for face in 0..8 {
            set_packed_face_state(&mut states, face, 1);
        }
        let mut output: Vec<u16> = Vec::with_capacity(16);
        assert!(collect_marked_faces_ascending(
            &states,
            face_count,
            &mut output
        ));
        assert_eq!(output, vec![0, 1, 2, 3, 4, 5]);
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
                d >= -(1i32 << 14),
                "clipped vertex behind the near plane: {:?} d={d}",
                v.position
            );
        }
    }

    #[test]
    fn fast_accept_only_proves_points_inside_every_exact_plane() {
        let mut accepted = 0usize;
        for origin in [[0, 0, 0], [37, 70, 273], [-96, 24, 128]] {
            for yaw in [0, 257, 1024, 2051, 3072] {
                for pitch in [-320, 0, 100, 512] {
                    let (planes, _) = planes_for(origin, yaw, pitch);
                    for x in (-256i16..=256).step_by(64) {
                        for y in (-128i16..=256).step_by(64) {
                            for z in (-256i16..=256).step_by(64) {
                                let position = [x, y, z];
                                if !planes.point_definitely_inside(position) {
                                    continue;
                                }
                                accepted += 1;
                                assert!(
                                    planes
                                        .planes
                                        .iter()
                                        .all(|plane| FrustumPlanes::distance(plane, position) >= 0),
                                    "fast accept escaped the exact frustum: origin={origin:?} yaw={yaw} pitch={pitch} point={position:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(accepted > 100, "sample did not exercise the fast path");
    }

    #[test]
    fn one_pass_vertex_masks_never_contradict_the_wide_oracle() {
        // `vertex_outside_mask` keeps the exact i64 planes. The i32 planes
        // round the side normals, so they may only be undecided inside
        // their error band: they must never call a point surely outside
        // when the oracle has it inside, nor surely inside when the oracle
        // has it outside. The near plane is exact and must agree bit for bit.
        let mut undecided = 0usize;
        let mut checked = 0usize;
        for origin in [[0, 0, 0], [37, 70, 273], [-96, 24, 128]] {
            for yaw in [0, 257, 1024, 2051, 3072] {
                for pitch in [-320, 0, 100, 512] {
                    let (planes, _) = planes_for(origin, yaw, pitch);
                    for x in (-320i16..=320).step_by(64) {
                        for y in (-192i16..=256).step_by(64) {
                            for z in (-320i16..=320).step_by(64) {
                                let position = [x, y, z];
                                let oracle = planes.vertex_outside_mask(position);
                                for (index, plane) in planes.planes.iter().enumerate() {
                                    let error = planes.error_of(index);
                                    let outside = FrustumPlanes::surely_outside(plane, error, position);
                                    let inside = FrustumPlanes::surely_inside(plane, error, position);
                                    let oracle_outside = oracle & (1 << index) != 0;
                                    checked += 1;
                                    if index == PXBSP_CLIP_NEAR_PLANE {
                                        assert_eq!(outside, oracle_outside, "near {position:?}");
                                        assert_eq!(inside, !oracle_outside, "near {position:?}");
                                        continue;
                                    }
                                    assert!(
                                        !(outside && !oracle_outside),
                                        "plane {index} rejected an inside point {position:?}"
                                    );
                                    assert!(
                                        !(inside && oracle_outside),
                                        "plane {index} vouched for an outside point {position:?}"
                                    );
                                    if !outside && !inside {
                                        undecided += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // The band must be narrow. This grid sits within 320 units of the
        // camera, where the fixed error bound (sized for 65535-unit
        // positions) is at its widest relative to the distances; even there
        // rounding must decide the vast majority of points.
        assert!(checked > 50_000, "sweep collapsed to {checked} checks");
        assert!(
            undecided * 20 < checked,
            "{undecided} of {checked} plane tests were undecided"
        );
    }

    #[test]
    fn polygon_wholly_outside_one_plane_is_rejected_before_clipping() {
        let (planes, _) = planes_for([0, 0, 0], 0, 0);
        let mut vertices = [vertex([0; 3]); 16];
        vertices[..4].copy_from_slice(&[
            vertex([-320, -32, 96]),
            vertex([-288, -32, 96]),
            vertex([-288, 32, 96]),
            vertex([-320, 32, 96]),
        ]);
        let mut scratch = [vertex([0; 3]); 16];

        let (count, in_scratch) = planes.clip_polygon_buffers(&mut vertices, 4, &mut scratch);

        assert_eq!(count, 0);
        assert!(!in_scratch);
        assert!(
            planes.planes.iter().any(|plane| {
                vertices[..4]
                    .iter()
                    .all(|vertex| FrustumPlanes::distance(plane, vertex.position) < 0)
            }),
            "fixture must sit wholly outside one exact plane"
        );
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
