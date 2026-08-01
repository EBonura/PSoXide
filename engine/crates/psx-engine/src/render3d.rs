//! Fixed-budget 3D render passes built on ordering tables.
//!
//! This module owns the part every PS1-scale 3D scene otherwise ends
//! up rewriting: project vertices through the currently loaded GTE
//! transform, cull back-facing faces, build GPU packets into fixed
//! arenas, sort the resulting commands, and finally insert them into
//! the frame ordering table.
//!
//! It deliberately stays renderer-shaped rather than editor-shaped.
//! Games and future editor exports still choose scene layout,
//! materials, animation, and streaming. The engine guarantees that a
//! frame's opaque mesh triangles share one depth policy and one
//! deterministic OT insertion order.

use crate::render::{
    CameraDepth, DepthBand, DepthRange, DepthSlot, OtFrame, PrimitiveArena, PrimitiveSink,
};
use crate::{Angle, WorldVertex, Q12};
use core::mem::MaybeUninit;
use psx_asset::{Animation, GteJointPose, JointPose, Mesh, Model, ModelPart, ModelPoseBlend, ModelVertex};
use psx_gpu::{
    material::{TextureMaterial, TexturedGouraudPacketMaterial, TexturedPacketMaterial},
    prim::{QuadTexturedGouraud, TriGouraud, TriTextured, TriTexturedGouraud},
};
use psx_gte::{
    lighting::{project_lit, project_lit_triangle, ProjectedLit},
    math::{Mat3I16, Vec3I16, Vec3I32},
    scene,
};

mod world_pass;
mod world_pass_gouraud;
mod world_pass_model;

const PSX_VERTEX_MIN: i16 = -1024;
const PSX_VERTEX_MAX: i16 = 1023;
const PSX_TRI_MAX_DX: i32 = 1023;
const PSX_TRI_MAX_DY: i32 = 511;
const MAX_TEXTURED_HW_SPLIT_DEPTH: u8 = 5;
const WORLD_COMMAND_NONE: u16 = u16::MAX;
const GOURAUD_COMMAND_NONE: u16 = u16::MAX;
/// Compose animated joint matrices on the GTE by default. The CPU path is
/// retained as a build-time escape hatch for regression testing.
const MODEL_GTE_JOINT_COMPOSE: bool = option_env!("PSXO_DISABLE_GTE_JOINT_COMPOSE").is_none();
/// Rotate joint translations on the GTE by default. This keeps the hot
/// animated-model path on the same coprocessor used for vertex projection.
const MODEL_GTE_JOINT_TRANSLATION: bool =
    MODEL_GTE_JOINT_COMPOSE && option_env!("PSXO_DISABLE_GTE_JOINT_TRANSLATION").is_none();
const MODEL_GTE_JOINT_PACKED_TRANSLATION: bool =
    option_env!("PSXO_GTE_JOINT_PACKED_TRANSLATION").is_some();

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum WorldCommandOrdering {
    LinkedSorted,
    DeferredSorted,
    DeferredSlotSorted,
    Bucketed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ModelTrianglePacketResult {
    Submitted,
    CommandOverflow,
    PrimitiveOverflow,
}

impl WorldCommandOrdering {
    #[inline(always)]
    const fn uses_slot_heads(self) -> bool {
        matches!(self, Self::LinkedSorted | Self::DeferredSlotSorted)
    }

    #[inline(always)]
    const fn uses_slot_tails(self) -> bool {
        matches!(self, Self::DeferredSlotSorted)
    }
}

/// Aggregated micro-profile for cached textured-Gouraud triangle submission.
#[cfg(feature = "room-surface-profile")]
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct TexturedGouraudSubmitMicroProfile {
    pub(crate) hw_safe_test_cycles: u32,
    pub(crate) packet_fill_cycles: u32,
    pub(crate) primitive_push_cycles: u32,
    pub(crate) depth_cycles: u32,
    pub(crate) command_cycles: u32,
    pub(crate) fallback_cycles: u32,
    pub(crate) hw_safe_calls: u32,
    pub(crate) fallback_calls: u32,
    pub(crate) command_overflows: u32,
    pub(crate) primitive_overflows: u32,
}

#[cfg(feature = "room-surface-profile")]
impl TexturedGouraudSubmitMicroProfile {
    #[inline(always)]
    fn cycle() -> u32 {
        crate::telemetry::cycle_counter()
    }

    #[inline(always)]
    fn elapsed(start: u32) -> u32 {
        Self::cycle().wrapping_sub(start)
    }

    #[inline(always)]
    fn add_hw_safe_test(&mut self, cycles: u32) {
        self.hw_safe_test_cycles = self.hw_safe_test_cycles.saturating_add(cycles);
    }

    #[inline(always)]
    fn add_packet_fill(&mut self, cycles: u32) {
        self.packet_fill_cycles = self.packet_fill_cycles.saturating_add(cycles);
    }

    #[inline(always)]
    fn add_primitive_push(&mut self, cycles: u32) {
        self.primitive_push_cycles = self.primitive_push_cycles.saturating_add(cycles);
    }

    #[inline(always)]
    fn add_depth(&mut self, cycles: u32) {
        self.depth_cycles = self.depth_cycles.saturating_add(cycles);
    }

    #[inline(always)]
    fn add_command(&mut self, cycles: u32) {
        self.command_cycles = self.command_cycles.saturating_add(cycles);
    }

    #[inline(always)]
    fn add_fallback(&mut self, cycles: u32) {
        self.fallback_cycles = self.fallback_cycles.saturating_add(cycles);
    }

    #[inline(always)]
    fn count_hw_safe(&mut self) {
        self.hw_safe_calls = self.hw_safe_calls.saturating_add(1);
    }

    #[inline(always)]
    fn count_fallback(&mut self) {
        self.fallback_calls = self.fallback_calls.saturating_add(1);
    }

    #[inline(always)]
    fn count_command_overflow(&mut self) {
        self.command_overflows = self.command_overflows.saturating_add(1);
    }

    #[inline(always)]
    fn count_primitive_overflow(&mut self) {
        self.primitive_overflows = self.primitive_overflows.saturating_add(1);
    }
}

/// Canonical quad → triangle split.
///
/// Quad corners arrive in perimeter order `[0, 1, 2, 3]`:
///
/// ```text
///   0 ─────── 1
///   │         │
///   3 ─────── 2
/// ```
///
/// Both triangles share the `0`–`2` diagonal so the union covers
/// the whole quad with no overlap. A pre-history version split
/// the second triangle as `(2, 1, 3)`, which uses the OTHER
/// diagonal -- the two halves overlap at the `1`–`2` edge and
/// leave a triangular hole near corner `3`. That manifested as
/// the "black triangular gaps" floor-rendering bug. Centralised
/// here so every quad-submitting path uses the same split.
const TEXTURED_QUAD_TRIANGLES: [[usize; 3]; 2] = [[0, 1, 2], [0, 2, 3]];

/// Scalar depth policy used to bucket a triangle into the ordering table.
///
/// The PS1 has no z-buffer, so every triangle eventually becomes one
/// scalar sort key. `Average` mirrors the common GTE `AVSZ3` style.
/// `Nearest` and `Farthest` are useful escape hatches for authored
/// meshes where a long triangle should bias toward one side of the
/// painter's algorithm.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DepthPolicy {
    /// Use the average of the three projected vertex depths.
    Average,
    /// Use the nearest projected vertex depth.
    Nearest,
    /// Use the farthest projected vertex depth.
    Farthest,
    /// Use a caller-supplied camera-space depth for every triangle.
    ///
    /// Grid-visible room drawing uses this to make the quantised tile cell,
    /// not a single sloped/oversized triangle corner, the painter's
    /// algorithm ordering unit.
    Fixed(i32),
}

impl DepthPolicy {
    fn depth_values(self, z0: i32, z1: i32, z2: i32) -> i32 {
        match self {
            DepthPolicy::Average => (z0 + z1 + z2) / 3,
            DepthPolicy::Nearest => z0.min(z1).min(z2),
            DepthPolicy::Farthest => z0.max(z1).max(z2),
            DepthPolicy::Fixed(depth) => depth,
        }
    }

    fn depth(self, verts: [ProjectedLit; 3]) -> i32 {
        self.depth_values(verts[0].sz as i32, verts[1].sz as i32, verts[2].sz as i32)
    }
}

/// Triangle culling policy.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CullMode {
    /// Submit triangles regardless of screen-space winding.
    None,
    /// Cull clockwise screen-space triangles.
    Back,
    /// Cull counter-clockwise screen-space triangles.
    Front,
}

/// Projected vertex used by CPU-projected world surfaces.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectedVertex {
    /// Screen-space X.
    pub sx: i16,
    /// Screen-space Y.
    pub sy: i16,
    /// Camera-space depth.
    pub sz: i32,
}

impl ProjectedVertex {
    /// Sentinel used by indexed room projection caches when a vertex
    /// falls behind the near plane.
    pub const INVALID: Self = Self {
        sx: 0,
        sy: 0,
        sz: i32::MIN,
    };

    /// Build a projected vertex.
    pub const fn new(sx: i16, sy: i16, sz: i32) -> Self {
        Self { sx, sy, sz }
    }

    /// Return whether this projection can be consumed by a textured
    /// primitive.
    pub const fn is_valid(self) -> bool {
        // Projection outputs always have positive camera-space depth. Treat
        // the zero-filled scratch default as invalid too: otherwise a missed
        // cache projection silently becomes a real corner at screen (0, 0),
        // stretching an off-screen room surface across the whole display.
        self.sz > 0
    }
}

/// Camera-space vertex used by clipped world surfaces.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewVertex {
    /// Camera-space X.
    pub x: i32,
    /// Camera-space Y.
    pub y: i32,
    /// Camera-space depth.
    pub z: i32,
}

impl ViewVertex {
    /// Zero camera-space vertex.
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };

    /// Build a camera-space vertex.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// Uniform Q12 scale from dense model-local units to engine world units.
///
/// Imported characters can spend the full signed-16-bit vertex range on
/// their own local detail while rooms keep using stable 1024-unit grid
/// sectors. This helper applies the scale without requiring 64-bit
/// multiplication on the PS1 runtime path.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LocalToWorldScale {
    scale: Q12,
}

impl LocalToWorldScale {
    /// Identity scale.
    pub const IDENTITY: Self = Self { scale: Q12::ONE };

    /// Build from a Q12 header value. Zero means unspecified and maps to
    /// identity for compatibility with older cooked blobs.
    pub const fn from_q12(q12: u16) -> Self {
        if q12 == 0 {
            Self::IDENTITY
        } else {
            Self {
                scale: Q12::from_raw(q12 as i32),
            }
        }
    }

    /// Raw Q12 scale value.
    pub const fn q12(self) -> u16 {
        self.scale.raw() as u16
    }

    /// Typed Q12 scale value.
    pub const fn scale(self) -> Q12 {
        self.scale
    }

    /// Apply the scale to one signed coordinate.
    pub fn apply(self, value: i32) -> i32 {
        let whole = value >> 12;
        let frac = value - (whole << 12);
        whole.saturating_mul(self.scale.raw()) + self.scale.mul_i32(frac)
    }
}

/// Textured camera-space vertex used by near-plane clipping.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TexturedViewVertex {
    /// Camera-space position.
    pub position: ViewVertex,
    /// Texture U coordinate.
    pub u: i32,
    /// Texture V coordinate.
    pub v: i32,
}

impl TexturedViewVertex {
    /// Zero textured camera-space vertex.
    pub const ZERO: Self = Self {
        position: ViewVertex::ZERO,
        u: 0,
        v: 0,
    };

    /// Build a textured camera-space vertex.
    pub const fn new(position: ViewVertex, u: i32, v: i32) -> Self {
        Self { position, u, v }
    }
}

/// Projected textured vertex used as scratch by GTE-backed textured model paths.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectedTexturedVertex {
    /// Screen-space position and depth.
    pub projected: ProjectedVertex,
    /// Texture U coordinate.
    pub u: i32,
    /// Texture V coordinate.
    pub v: i32,
}

impl ProjectedTexturedVertex {
    /// Build a projected textured vertex.
    pub const fn new(projected: ProjectedVertex, u: i32, v: i32) -> Self {
        Self { projected, u, v }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct ProjectedTexturedGouraudVertex {
    projected: ProjectedVertex,
    u: i32,
    v: i32,
    color: (u8, u8, u8),
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct TexturedGouraudViewVertex {
    position: ViewVertex,
    u: i32,
    v: i32,
    color: (u8, u8, u8),
}

impl TexturedGouraudViewVertex {
    const ZERO: Self = Self {
        position: ViewVertex::ZERO,
        u: 0,
        v: 0,
        color: (0, 0, 0),
    };

    const fn new(position: ViewVertex, uv_word: u16, color: (u8, u8, u8)) -> Self {
        Self {
            position,
            u: (uv_word & 0xff) as i32,
            v: (uv_word >> 8) as i32,
            color,
        }
    }
}

impl ProjectedTexturedGouraudVertex {
    const fn new(projected: ProjectedVertex, u: i32, v: i32, color: (u8, u8, u8)) -> Self {
        Self {
            projected,
            u,
            v,
            color,
        }
    }

    const fn textured(self) -> ProjectedTexturedVertex {
        ProjectedTexturedVertex::new(self.projected, self.u, self.v)
    }
}

/// Predecoded textured model face for runtime-hot model rendering.
///
/// Cooked `.psxmdl` face records are compact byte records. The generic
/// parser keeps those records zero-copy and fallible, which is ideal for
/// loaders and validation but wasteful in the per-frame face loop. Runtime
/// code can decode them once into this compact POD record and pass the slice
/// to the predecoded geometry submit methods.
#[repr(C, align(4))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TexturedModelRenderFace {
    /// Three cooked corner words: vertex index in the low half and packet UV
    /// (`u | v << 8`) in the high half. This mirrors the `.psxmdl` record and
    /// lets the runtime face loop load one aligned word per corner.
    pub corner_words: [u32; 3],
}

/// Wrapped offset applied to model UVs for an independently moving texture
/// layer. PS1 polygon UVs are bytes, so addition naturally wraps at 256.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelUvOffset {
    /// Horizontal texel offset.
    pub u: u8,
    /// Vertical texel offset.
    pub v: u8,
}

/// UV source used by textured model packets.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ModelUvMapping {
    /// Preserve UVs authored in the cooked model atlas.
    #[default]
    Authored,
    /// Preserve authored UVs and apply a wrapping byte-space displacement.
    AuthoredOffset(ModelUvOffset),
    /// Project the model through the screen into the active room's compact
    /// environment map. Roughness is a 0..=3 UV-quantisation level.
    ScreenSpaceReflection {
        /// Resident probe width in texels.
        texture_width: u8,
        /// Resident probe height in texels.
        texture_height: u8,
        /// Quantised roughness (`0 = sharp`, `3 = rough`).
        roughness: u8,
        /// Optional wrapped movement applied after probe projection.
        uv_offset: ModelUvOffset,
    },
}

impl ModelUvMapping {
    /// Whether packet UVs can use the model's prepacked authored words.
    pub const fn is_authored(self) -> bool {
        matches!(self, Self::Authored)
    }
}

impl ModelUvOffset {
    /// No UV motion.
    pub const ZERO: Self = Self::new(0, 0);

    /// Build a wrapped UV offset.
    pub const fn new(u: u8, v: u8) -> Self {
        Self { u, v }
    }

    /// True when applying this offset would leave UVs unchanged.
    pub const fn is_zero(self) -> bool {
        self.u == 0 && self.v == 0
    }
}

/// A second textured model pass and its optional UV displacement.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TexturedModelLayer {
    /// Texture, tint, blend mode, and texture-window state.
    pub material: TextureMaterial,
    /// Wrapped UV displacement applied only to this layer.
    pub uv_offset: ModelUvOffset,
    /// Authored or probe-projected UV source for this layer.
    pub uv_mapping: ModelUvMapping,
}

impl TexturedModelLayer {
    /// Build a static second layer.
    pub const fn new(material: TextureMaterial) -> Self {
        Self {
            material,
            uv_offset: ModelUvOffset::ZERO,
            uv_mapping: ModelUvMapping::Authored,
        }
    }

    /// Return this layer with a wrapped UV displacement.
    pub const fn with_uv_offset(mut self, uv_offset: ModelUvOffset) -> Self {
        self.uv_offset = uv_offset;
        self
    }

    /// Return this layer with an independent UV source.
    pub const fn with_uv_mapping(mut self, uv_mapping: ModelUvMapping) -> Self {
        self.uv_mapping = uv_mapping;
        self
    }
}

impl TexturedModelRenderFace {
    /// Empty face record used for fixed-size static arrays.
    pub const ZERO: Self = Self {
        corner_words: [0; 3],
    };

    /// Build a predecoded face from canonical `(u, v)` pairs.
    pub const fn new(vertex_indices: [u16; 3], uvs: [(u8, u8); 3]) -> Self {
        Self {
            corner_words: [
                (vertex_indices[0] as u32) | ((model_uv_word(uvs[0]) as u32) << 16),
                (vertex_indices[1] as u32) | ((model_uv_word(uvs[1]) as u32) << 16),
                (vertex_indices[2] as u32) | ((model_uv_word(uvs[2]) as u32) << 16),
            ],
        }
    }

    /// Projected-vertex indices for the triangle corners.
    pub const fn vertex_indices(self) -> [u16; 3] {
        [
            self.corner_words[0] as u16,
            self.corner_words[1] as u16,
            self.corner_words[2] as u16,
        ]
    }

    /// Packed packet UV words for the triangle corners.
    pub const fn uv_words(self) -> [u16; 3] {
        [
            (self.corner_words[0] >> 16) as u16,
            (self.corner_words[1] >> 16) as u16,
            (self.corner_words[2] >> 16) as u16,
        ]
    }

    /// Replace one packed packet UV word while preserving its vertex index.
    pub const fn with_corner_uv_word(mut self, index: usize, uv_word: u16) -> Self {
        self.corner_words[index] = (self.corner_words[index] & 0xffff) | ((uv_word as u32) << 16);
        self
    }

    /// Return canonical `(u, v)` pairs for fallback paths and tests.
    pub const fn uvs(self) -> [(u8, u8); 3] {
        let uv_words = self.uv_words();
        [
            model_uv_pair(uv_words[0]),
            model_uv_pair(uv_words[1]),
            model_uv_pair(uv_words[2]),
        ]
    }

    /// Return this face with all UVs displaced in wrapping byte space.
    pub const fn with_uv_offset(mut self, offset: ModelUvOffset) -> Self {
        let mut index = 0;
        while index < self.corner_words.len() {
            let word = (self.corner_words[index] >> 16) as u16;
            let u = (word as u8).wrapping_add(offset.u);
            let v = ((word >> 8) as u8).wrapping_add(offset.v);
            let uv_word = (u as u16) | ((v as u16) << 8);
            self.corner_words[index] =
                (self.corner_words[index] & 0xffff) | ((uv_word as u32) << 16);
            index += 1;
        }
        self
    }
}

/// Predecoded model records for the runtime model path.
///
/// This keeps the original `.psxmdl` part and vertex order intact; it only
/// moves byte decoding out of the per-frame projection loop.
#[derive(Copy, Clone)]
pub struct TexturedModelGeometry<'a> {
    /// One part record per `.psxmdl` part.
    pub parts: &'a [ModelPart],
    /// One vertex record per `.psxmdl` vertex.
    pub vertices: &'a [ModelVertex],
}

impl<'a> TexturedModelGeometry<'a> {
    /// Build a predecoded geometry view.
    pub const fn new(parts: &'a [ModelPart], vertices: &'a [ModelVertex]) -> Self {
        Self { parts, vertices }
    }

    fn usable_for(self, model: Model<'_>) -> bool {
        self.parts.len() >= model.part_count() as usize
            && self.vertices.len() >= model.vertex_count() as usize
    }
}

const fn model_uv_word(uv: (u8, u8)) -> u16 {
    (uv.0 as u16) | ((uv.1 as u16) << 8)
}

const fn model_uv_pair(word: u16) -> (u8, u8) {
    (word as u8, (word >> 8) as u8)
}

const fn packet_uv_words_to_pairs(uv_words: [u16; 3]) -> [(u8, u8); 3] {
    [
        model_uv_pair(uv_words[0]),
        model_uv_pair(uv_words[1]),
        model_uv_pair(uv_words[2]),
    ]
}

#[cfg(test)]
fn model_uv_max(size: u16) -> i32 {
    size.saturating_sub(1).min(u16::from(u8::MAX)) as i32
}

#[cfg(test)]
fn clamp_model_uv(u: i32, v: i32, max_u: i32, max_v: i32) -> (i32, i32) {
    (
        clamp_model_uv_i32_component(u, max_u),
        clamp_model_uv_i32_component(v, max_v),
    )
}

#[cfg(test)]
fn clamp_model_uv_i32_component(value: i32, max: i32) -> i32 {
    let max = max.clamp(0, u8::MAX as i32);
    if value <= 0 {
        0
    } else if value >= max {
        max
    } else {
        value
    }
}

/// Per-joint world-to-view transform for one render frame.
///
/// Model submission fills one entry per skin joint up-front so blend-skin
/// vertices can read both their primary and secondary joint matrices without
/// re-deriving them mid-frame.
#[derive(Copy, Clone, Debug, Default)]
pub struct JointViewTransform {
    /// Combined view × model rotation, Q12.
    pub rotation: Mat3I16,
    /// View-space translation, Q0.
    pub translation: Vec3I32,
}

impl JointViewTransform {
    /// All-zero transform suitable for `static mut` scratch storage.
    pub const ZERO: Self = Self {
        rotation: Mat3I16::ZERO,
        translation: Vec3I32::ZERO,
    };
}

/// Per-joint room/world transform for gameplay attachment points.
///
/// Unlike [`JointViewTransform`], this is not camera-relative. It is
/// the model instance's oriented joint pose in room-local world units,
/// suitable for composing sockets, weapon grips, and hit volumes.
#[derive(Copy, Clone, Debug, Default)]
pub struct JointWorldTransform {
    /// Instance × joint rotation, Q12.
    pub rotation: Mat3I16,
    /// Joint origin in room-local world units.
    pub translation: WorldVertex,
}

impl JointWorldTransform {
    /// All-zero transform suitable for static scratch.
    pub const ZERO: Self = Self {
        rotation: Mat3I16::ZERO,
        translation: WorldVertex::ZERO,
    };
}

/// Model-local translation applied to every sampled joint pose before
/// model-to-world scaling. Gameplay uses this to render locomotion
/// clips in-place while movement remains owned by controller code.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelPoseTranslation {
    /// Model-local X offset.
    pub x: i32,
    /// Model-local Y offset.
    pub y: i32,
    /// Model-local Z offset.
    pub z: i32,
}

impl ModelPoseTranslation {
    /// No pose offset.
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };
}

/// Perspective projection settings for world-space render passes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldProjection {
    /// Screen centre X.
    pub screen_x: i16,
    /// Screen centre Y.
    pub screen_y: i16,
    /// Perspective focal length.
    pub focal_length: i32,
    /// Near clipping plane in camera-space depth.
    pub near_z: i32,
}

impl WorldProjection {
    /// Build projection settings for world-space render passes.
    pub const fn new(screen_x: i16, screen_y: i16, focal_length: i32, near_z: i32) -> Self {
        Self {
            screen_x,
            screen_y,
            focal_length,
            near_z,
        }
    }

    /// Project a camera-space vertex into screen space.
    pub fn project_view(self, vertex: ViewVertex) -> Option<ProjectedVertex> {
        if vertex.z <= 0 || vertex.z < self.near_z {
            return None;
        }

        let sx = (self.screen_x as i32) + (vertex.x * self.focal_length) / vertex.z;
        let sy = (self.screen_y as i32) - (vertex.y * self.focal_length) / vertex.z;
        Some(ProjectedVertex::new(clamp_i16(sx), clamp_i16(sy), vertex.z))
    }
}

impl From<ProjectedLit> for ProjectedVertex {
    fn from(value: ProjectedLit) -> Self {
        Self {
            sx: value.sx,
            sy: value.sy,
            sz: value.sz as i32,
        }
    }
}

/// Perspective camera for authored world surfaces and GTE model passes.
///
/// The camera stores a simple orbit-style basis: yaw rotates around
/// the world's Y axis, then pitch tilts the view around the camera's
/// local X axis. It is deliberately small and fixed-point friendly:
/// the trigonometric basis is Q0.12, matching `psx-math`'s sine table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldCamera {
    /// Projection used after world-to-view transformation.
    pub projection: WorldProjection,
    /// Camera position in the same world units as submitted surfaces.
    pub position: WorldVertex,
    /// Sine of yaw.
    pub sin_yaw: Q12,
    /// Cosine of yaw.
    pub cos_yaw: Q12,
    /// Sine of pitch.
    pub sin_pitch: Q12,
    /// Cosine of pitch.
    pub cos_pitch: Q12,
}

impl WorldCamera {
    /// Build a camera from an explicit fixed-point basis.
    pub const fn from_basis(
        projection: WorldProjection,
        position: WorldVertex,
        sin_yaw: Q12,
        cos_yaw: Q12,
        sin_pitch: Q12,
        cos_pitch: Q12,
    ) -> Self {
        Self {
            projection,
            position,
            sin_yaw,
            cos_yaw,
            sin_pitch,
            cos_pitch,
        }
    }

    /// Build a camera on a horizontal orbit that looks at `target`.
    ///
    /// `yaw` is the orbit direction from target to camera. `camera_y`
    /// is the camera's absolute world-space height. Pitch is derived
    /// from `target.y - camera_y`, so dollying the radius keeps the
    /// target centred without per-frame call-site math.
    pub fn orbit_yaw(
        projection: WorldProjection,
        target: WorldVertex,
        camera_y: i32,
        radius: i32,
        yaw: Angle,
    ) -> Self {
        let sin_yaw = yaw.sin();
        let cos_yaw = yaw.cos();
        let target_dy = target.y - camera_y;
        let pitch_len =
            isqrt_i32(radius.saturating_mul(radius) + target_dy.saturating_mul(target_dy)).max(1);
        Self {
            projection,
            position: WorldVertex::new(
                target.x + sin_yaw.mul_i32(radius),
                camera_y,
                target.z + cos_yaw.mul_i32(radius),
            ),
            sin_yaw,
            cos_yaw,
            sin_pitch: Q12::from_ratio(target_dy, pitch_len),
            cos_pitch: Q12::from_ratio(radius, pitch_len),
        }
    }

    /// Build a camera on a full spherical orbit around `target`.
    ///
    /// `yaw` and `pitch` are canonical engine angles. The camera sits
    /// at constant `radius` from `target` and looks at it directly:
    /// positive `pitch` raises the camera above the target so the view
    /// tilts down. Pitch wraps freely, so the orbit can pass through
    /// the poles and view the model upside-down.
    pub fn orbit(
        projection: WorldProjection,
        target: WorldVertex,
        radius: i32,
        yaw: Angle,
        pitch: Angle,
    ) -> Self {
        let sin_yaw = yaw.sin();
        let cos_yaw = yaw.cos();
        let sin_pitch = pitch.sin();
        let cos_pitch = pitch.cos();
        let horiz = cos_pitch.mul_i32(radius);
        Self {
            projection,
            position: WorldVertex::new(
                target.x + sin_yaw.mul_i32(horiz),
                target.y + sin_pitch.mul_i32(radius),
                target.z + cos_yaw.mul_i32(horiz),
            ),
            sin_yaw,
            cos_yaw,
            // The view basis tilts opposite the camera's vertical offset so
            // the target stays centred (world Y is up, view pitch is the
            // angle the camera looks down from).
            sin_pitch: Q12::from_raw(-sin_pitch.raw()),
            cos_pitch,
        }
    }

    /// Transform a world-space vertex into camera-space.
    pub fn view_vertex(self, vertex: WorldVertex) -> ViewVertex {
        let dx = vertex.x - self.position.x;
        let dy = vertex.y - self.position.y;
        let dz = vertex.z - self.position.z;

        let sin_yaw = self.sin_yaw.raw();
        let cos_yaw = self.cos_yaw.raw();
        let sin_pitch = self.sin_pitch.raw();
        let cos_pitch = self.cos_pitch.raw();
        let x1 = ((dx * cos_yaw) - (dz * sin_yaw)) >> 12;
        let z1 = ((-dx * sin_yaw) - (dz * cos_yaw)) >> 12;
        let y2 = ((dy * cos_pitch) - (z1 * sin_pitch)) >> 12;
        let z2 = ((dy * sin_pitch) + (z1 * cos_pitch)) >> 12;

        ViewVertex::new(x1, y2, z2)
    }

    /// Transform and project a world-space vertex.
    pub fn project_world(self, vertex: WorldVertex) -> Option<ProjectedVertex> {
        self.projection.project_view(self.view_vertex(vertex))
    }

    /// Transform and project a world-space quad. Returns `None` if
    /// any corner falls behind the projection near plane.
    pub fn project_world_quad(self, verts: [WorldVertex; 4]) -> Option<[ProjectedVertex; 4]> {
        Some([
            self.project_world(verts[0])?,
            self.project_world(verts[1])?,
            self.project_world(verts[2])?,
            self.project_world(verts[3])?,
        ])
    }

    /// Transform a textured world-space vertex to camera-space.
    pub fn textured_view_vertex(self, vertex: WorldVertex, uv: (u8, u8)) -> TexturedViewVertex {
        TexturedViewVertex::new(self.view_vertex(vertex), uv.0 as i32, uv.1 as i32)
    }
}

/// GTE-backed projector for repeated world-space projections from one camera.
///
/// Constructing this loads the camera's world-to-view transform and projection
/// registers. Callers that project many independent points or quads can then
/// reuse that loaded state instead of doing the same fixed-point matrix work on
/// the CPU for every vertex.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LoadedWorldCameraGte {
    camera: WorldCamera,
}

impl LoadedWorldCameraGte {
    /// Load `camera` into the GTE and return a projector bound to that state.
    #[inline]
    pub fn load(camera: WorldCamera) -> Self {
        load_world_camera_gte(camera);
        Self { camera }
    }

    /// Return the camera this projector was loaded from.
    #[inline(always)]
    pub const fn camera(self) -> WorldCamera {
        self.camera
    }

    /// Transform and project one world-space vertex through the loaded GTE.
    ///
    /// Vertices outside the GTE's signed 16-bit input range fall back to the
    /// CPU path, preserving behaviour for very large authored worlds.
    #[inline]
    pub fn project_world(self, vertex: WorldVertex) -> Option<ProjectedVertex> {
        let Some(input) = world_vertex_gte_input(vertex) else {
            return self.camera.project_world(vertex);
        };
        projected_option_from_gte(
            scene::project_vertex_scheduled(input),
            self.camera.projection.near_z,
        )
    }

    /// Transform one world-space vertex into *view* space through the loaded
    /// GTE (MVMVA, no perspective divide), mirroring [`WorldCamera::view_vertex`]
    /// but on the otherwise-idle GTE. Used for depth/cull queries (cell select,
    /// bounds tests) so they share the same transform as `project_world` instead
    /// of redoing the camera rotation in CPU fixed-point. Out-of-range vertices
    /// fall back to the CPU path, matching `project_world`.
    #[inline]
    pub fn view_vertex(self, vertex: WorldVertex) -> ViewVertex {
        let Some(input) = world_vertex_gte_input(vertex) else {
            return self.camera.view_vertex(vertex);
        };
        let t = scene::transform_vertex_scheduled(input);
        ViewVertex::new(t.x, t.y, t.z)
    }

    /// Transform and project a world-space quad through the loaded GTE.
    ///
    /// The first three vertices are projected with `RTPT`; the fourth uses
    /// `RTPS`. If any vertex is behind the near plane this returns `None`,
    /// matching [`WorldCamera::project_world_quad`].
    #[inline]
    pub fn project_world_quad(self, verts: [WorldVertex; 4]) -> Option<[ProjectedVertex; 4]> {
        let a = world_vertex_gte_input(verts[0]);
        let b = world_vertex_gte_input(verts[1]);
        let c = world_vertex_gte_input(verts[2]);
        let d = world_vertex_gte_input(verts[3]);
        let near_z = self.camera.projection.near_z;
        if let (Some(a), Some(b), Some(c), Some(d)) = (a, b, c, d) {
            let tri = scene::project_triangle_scheduled(a, b, c);
            let fourth = scene::project_vertex_scheduled(d);
            return Some([
                projected_option_from_gte(tri[0], near_z)?,
                projected_option_from_gte(tri[1], near_z)?,
                projected_option_from_gte(tri[2], near_z)?,
                projected_option_from_gte(fourth, near_z)?,
            ]);
        }
        self.camera.project_world_quad(verts)
    }
}

pub(crate) fn project_world_vertex_indices_gte(
    camera: WorldCamera,
    vertices: &[WorldVertex],
    indices: &[u16],
    projected_vertices: &mut [ProjectedVertex],
) {
    load_world_camera_gte(camera);
    let near_z = camera.projection.near_z;
    let limit = vertices.len().min(projected_vertices.len());
    let mut group = [0usize; 3];
    let mut group_count = 0usize;
    for raw_index in indices {
        let index = *raw_index as usize;
        if index >= limit {
            continue;
        }
        group[group_count] = index;
        group_count += 1;
        if group_count == 3 {
            project_world_index_group_gte(camera, vertices, projected_vertices, near_z, group);
            group_count = 0;
        }
    }
    let mut i = 0usize;
    while i < group_count {
        project_world_vertex_cpu(camera, vertices, projected_vertices, group[i]);
        i += 1;
    }
}

/// Project one contiguous cached world-vertex slice through the GTE.
///
/// Dense room views already reference most of their cache. Walking a second
/// index stream and maintaining a per-frame dedup bitset costs more CPU than
/// projecting the small remainder, while the GTE itself has substantial
/// headroom. Keep this separate from the indexed path so sparse portal views
/// can continue projecting only the vertices they actually use.
pub(crate) fn project_world_vertices_gte(
    camera: WorldCamera,
    vertices: &[WorldVertex],
    projected_vertices: &mut [ProjectedVertex],
) {
    load_world_camera_gte(camera);
    let near_z = camera.projection.near_z;
    let limit = vertices.len().min(projected_vertices.len());
    let mut index = 0usize;
    while index.saturating_add(3) <= limit {
        project_world_index_group_gte(
            camera,
            vertices,
            projected_vertices,
            near_z,
            [index, index + 1, index + 2],
        );
        index += 3;
    }
    while index < limit {
        project_world_vertex_cpu(camera, vertices, projected_vertices, index);
        index += 1;
    }
}

fn project_world_index_group_gte(
    camera: WorldCamera,
    vertices: &[WorldVertex],
    projected_vertices: &mut [ProjectedVertex],
    near_z: i32,
    indices: [usize; 3],
) {
    let a_index = indices[0];
    let b_index = indices[1];
    let c_index = indices[2];
    let a = world_vertex_gte_input(vertices[a_index]);
    let b = world_vertex_gte_input(vertices[b_index]);
    let c = world_vertex_gte_input(vertices[c_index]);
    if let (Some(a), Some(b), Some(c)) = (a, b, c) {
        let projected = scene::project_triangle_scheduled(a, b, c);
        projected_vertices[a_index] = valid_projected_from_gte(projected[0], near_z);
        projected_vertices[b_index] = valid_projected_from_gte(projected[1], near_z);
        projected_vertices[c_index] = valid_projected_from_gte(projected[2], near_z);
    } else {
        project_world_vertex_cpu(camera, vertices, projected_vertices, a_index);
        project_world_vertex_cpu(camera, vertices, projected_vertices, b_index);
        project_world_vertex_cpu(camera, vertices, projected_vertices, c_index);
    }
}

/// Coarse render layer for world surfaces inside one ordering table.
///
/// PS1 ordering tables are still depth-first. This layer only resolves
/// exact slot/depth ties so translucent packets blend over opaque packets
/// submitted into the same OT cell.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorldRenderLayer {
    /// Opaque surfaces that overwrite destination pixels.
    Opaque,
    /// Semi-transparent surfaces that should blend with prior pixels.
    Transparent,
}

impl WorldRenderLayer {
    /// Pick the world render layer implied by a texture material.
    pub const fn for_material(material: TextureMaterial) -> Self {
        if material.is_translucent() {
            Self::Transparent
        } else {
            Self::Opaque
        }
    }
}

const fn world_render_layer_code(layer: WorldRenderLayer) -> u8 {
    match layer {
        WorldRenderLayer::Opaque => 0,
        WorldRenderLayer::Transparent => 1,
    }
}

/// First camera-space depth band used by the later PS1 adaptive room
/// renderer. `SPEC_PSX/ROOMLETB.MIP` compares `max_z >> 3` with `0x280`,
/// so room polygons closer than five 1024-unit sectors split 4-way.
pub const ADAPTIVE_SUBDIVIDE_FAR_DEPTH: i32 = 5 * 1024;

/// Second camera-space depth band used by the later PS1 adaptive room
/// renderer. First-level children closer than three sectors split 4-way once
/// more, yielding at most sixteen leaves from one authored polygon.
pub const ADAPTIVE_SUBDIVIDE_NEAR_DEPTH: i32 = 3 * 1024;

/// The original renderer keeps the authored polygon behind its children from
/// this depth onward. It is invisible in the ordinary case and only covers
/// sub-pixel cracks caused by fixed-point midpoint projection.
pub const ADAPTIVE_UNDERDRAW_DEPTH: i32 = 5 * 512;

/// Camera-depth offset corresponding to the original renderer's 32-slot
/// ordering-table underdraw offset.
pub const ADAPTIVE_UNDERDRAW_DEPTH_BIAS: i32 = 256;

/// Camera-space depth bands for adaptive-style room subdivision.
///
/// The original REFERENCE values are expressed in terms of its 1024-unit sectors.
/// Keeping the profile beside each surface submission lets projects with a
/// different sector scale preserve the same five-sector/three-sector visual
/// schedule instead of inheriting thresholds that are too close to the camera.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AdaptiveSubdivisionProfile {
    /// Maximum number of four-way subdivision passes (one or two).
    pub max_levels: u8,
    /// Farthest root depth that receives the first four-way split.
    pub far_depth: i32,
    /// Farthest child depth that receives the second four-way split.
    pub near_depth: i32,
    /// Root depth from which the authored polygon is retained as crack cover.
    pub underdraw_depth: i32,
    /// Ordering-table depth bias applied to the crack-cover polygon.
    pub underdraw_depth_bias: i32,
}

impl AdaptiveSubdivisionProfile {
    /// Exact depth profile used by REFERENCE's 1024-unit room sectors.
    pub const REFERENCE: Self = Self {
        max_levels: 2,
        far_depth: ADAPTIVE_SUBDIVIDE_FAR_DEPTH,
        near_depth: ADAPTIVE_SUBDIVIDE_NEAR_DEPTH,
        underdraw_depth: ADAPTIVE_UNDERDRAW_DEPTH,
        underdraw_depth_bias: ADAPTIVE_UNDERDRAW_DEPTH_BIAS,
    };

    /// Preserve REFERENCE's subdivision distances for an arbitrary room sector size.
    pub const fn for_sector_size(sector_size: i32) -> Self {
        let sector_size = if sector_size > 0 { sector_size } else { 1024 };
        Self {
            max_levels: 2,
            far_depth: sector_size.saturating_mul(5),
            near_depth: sector_size.saturating_mul(3),
            underdraw_depth: sector_size.saturating_mul(5) / 2,
            underdraw_depth_bias: sector_size / 4,
        }
    }
}

/// Cached room-surface kinds eligible for adaptive-style subdivision.
///
/// This policy is evaluated by the indexed room renderer before it enters the
/// generated-vertex path. Keeping it as a compact mask lets projects spend
/// affine-correction work only on surface orientations that materially need
/// it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AdaptiveSubdivisionKindMask(u8);

impl AdaptiveSubdivisionKindMask {
    /// No cached room surfaces.
    pub const NONE: Self = Self(0);
    /// Authored floor surfaces.
    pub const FLOOR: Self = Self(1 << 0);
    /// Authored ceiling surfaces.
    pub const CEILING: Self = Self(1 << 1);
    /// Authored wall surfaces.
    pub const WALL: Self = Self(1 << 2);
    /// Floors and walls, excluding ceilings.
    pub const FLOOR_WALL: Self = Self(Self::FLOOR.0 | Self::WALL.0);
    /// Every cached room-surface kind.
    pub const ALL: Self = Self(Self::FLOOR.0 | Self::CEILING.0 | Self::WALL.0);

    /// True when every bit in `kind` is enabled.
    pub const fn contains(self, kind: Self) -> bool {
        self.0 & kind.0 == kind.0
    }
}

/// Shared options for projected world surfaces.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldSurfaceOptions {
    /// OT slot band reserved for this surface layer.
    pub depth_band: DepthBand,
    /// Camera-space depth range mapped into `depth_band`.
    pub depth_range: DepthRange,
    /// Triangle scalar depth policy.
    pub depth_policy: DepthPolicy,
    /// Signed offset added to scalar depth before slot mapping.
    pub depth_bias: i32,
    /// Triangle culling mode.
    pub cull_mode: CullMode,
    /// Coarse tie-break layer for opaque/translucent surfaces.
    pub render_layer: WorldRenderLayer,
    /// Split oversized projected textured triangles before packet emission.
    ///
    /// Room-scale quads keep this enabled because a floor/wall can span most
    /// of the screen. Compact character meshes can disable it to stay on the
    /// direct packet path and spend their budget on GTE transforms instead of
    /// conservative CPU-side subdivision checks.
    pub split_textured_triangles: bool,
    /// Optional projected-edge limit for visual subdivision.
    ///
    /// Zero keeps the splitter focused on PS1 hardware extent limits. A
    /// positive value lets close textured meshes subdivide before packet
    /// emission, reducing affine/painter artifacts without forcing world
    /// geometry to spend the same budget.
    pub textured_split_max_edge: u16,
    /// Enable the bounded room subdivision used by the later
    /// PlayStation adaptive renderers.
    ///
    /// Unlike projected-edge splitting, this path averages camera-space
    /// positions and then reprojects every generated midpoint. That turns the
    /// PS1 GPU's affine texture mapping into a bounded piecewise-perspective
    /// approximation while keeping the work capped by the active profile.
    pub adaptive_subdivision: bool,
    /// Depth bands used when `adaptive_subdivision` is enabled.
    pub adaptive_subdivision_profile: AdaptiveSubdivisionProfile,
    /// Cached room-surface orientations eligible for TR subdivision.
    pub adaptive_subdivision_kinds: AdaptiveSubdivisionKindMask,
    /// Replace generated leaf lighting with diagnostic subdivision colors.
    ///
    /// This is intended for emulator wireframe captures: first-level children
    /// are cyan, second-level children are magenta, and the optional authored
    /// crack-cover polygon is yellow. It remains disabled in normal builds.
    pub adaptive_debug_subdivision_levels: bool,
    /// Simulation tick used by animated room materials.
    pub material_animation_tick: u32,
    /// Simulation rate used to convert UV-scroll speeds from texels/second.
    pub material_animation_hz: u16,
    /// Model-only UV source. Room geometry ignores this field.
    pub model_uv_mapping: ModelUvMapping,
}

impl WorldSurfaceOptions {
    /// Build options for a world-geometry depth band and range.
    pub const fn new(depth_band: DepthBand, depth_range: DepthRange) -> Self {
        Self {
            depth_band,
            depth_range,
            depth_policy: DepthPolicy::Average,
            depth_bias: 0,
            cull_mode: CullMode::Back,
            render_layer: WorldRenderLayer::Opaque,
            split_textured_triangles: true,
            textured_split_max_edge: 0,
            adaptive_subdivision: false,
            adaptive_subdivision_profile: AdaptiveSubdivisionProfile::REFERENCE,
            adaptive_subdivision_kinds: AdaptiveSubdivisionKindMask::ALL,
            adaptive_debug_subdivision_levels: false,
            material_animation_tick: 0,
            material_animation_hz: 60,
            model_uv_mapping: ModelUvMapping::Authored,
        }
    }

    /// Return options with a different scalar depth policy.
    pub const fn with_depth_policy(mut self, depth_policy: DepthPolicy) -> Self {
        self.depth_policy = depth_policy;
        self
    }

    /// Return options with a signed depth bias.
    pub const fn with_depth_bias(mut self, depth_bias: i32) -> Self {
        self.depth_bias = depth_bias;
        self
    }

    /// Return options with a different culling mode.
    pub const fn with_cull_mode(mut self, cull_mode: CullMode) -> Self {
        self.cull_mode = cull_mode;
        self
    }

    /// Return options with a model-specific UV source.
    pub const fn with_model_uv_mapping(mut self, mapping: ModelUvMapping) -> Self {
        self.model_uv_mapping = mapping;
        self
    }

    /// Return options with a different render layer.
    pub const fn with_render_layer(mut self, render_layer: WorldRenderLayer) -> Self {
        self.render_layer = render_layer;
        self
    }

    /// Return options using the render layer implied by `material`.
    pub const fn with_material_layer(mut self, material: TextureMaterial) -> Self {
        self.render_layer = WorldRenderLayer::for_material(material);
        self
    }

    /// Return options with textured triangle splitting enabled/disabled.
    pub const fn with_textured_triangle_splitting(mut self, enabled: bool) -> Self {
        self.split_textured_triangles = enabled;
        self
    }

    /// Return options with an optional projected-edge split threshold.
    pub const fn with_textured_triangle_max_edge(mut self, max_edge: u16) -> Self {
        self.textured_split_max_edge = max_edge;
        self
    }

    /// Enable or disable adaptive-style pre-projection subdivision.
    pub const fn with_adaptive_subdivision(mut self, enabled: bool) -> Self {
        self.adaptive_subdivision = enabled;
        self
    }

    /// Enable adaptive subdivision using an explicit depth profile.
    pub const fn with_adaptive_subdivision_profile(
        mut self,
        profile: AdaptiveSubdivisionProfile,
    ) -> Self {
        self.adaptive_subdivision = true;
        self.adaptive_subdivision_profile = profile;
        self
    }

    /// Enable adaptive subdivision scaled to the room's cooked sector size.
    pub const fn with_adaptive_subdivision_sector_size(self, sector_size: i32) -> Self {
        self.with_adaptive_subdivision_profile(AdaptiveSubdivisionProfile::for_sector_size(
            sector_size,
        ))
    }

    /// Limit adaptive subdivision to one or two four-way passes.
    pub const fn with_adaptive_subdivision_max_levels(mut self, max_levels: u8) -> Self {
        self.adaptive_subdivision_profile.max_levels = if max_levels < 1 {
            1
        } else if max_levels > 2 {
            2
        } else {
            max_levels
        };
        self
    }

    /// Restrict cached room subdivision to selected surface orientations.
    pub const fn with_adaptive_subdivision_kinds(
        mut self,
        kinds: AdaptiveSubdivisionKindMask,
    ) -> Self {
        self.adaptive_subdivision_kinds = kinds;
        self
    }

    /// Enable or disable diagnostic colors on generated subdivision leaves.
    pub const fn with_adaptive_subdivision_debug_levels(mut self, enabled: bool) -> Self {
        self.adaptive_debug_subdivision_levels = enabled;
        self
    }

    /// Return options evaluated at the supplied gameplay tick and video rate.
    pub const fn with_material_animation(mut self, tick: u32, hz: u16) -> Self {
        self.material_animation_tick = tick;
        self.material_animation_hz = if hz == 0 { 1 } else { hz };
        self
    }
}

/// Prepared ordering-table depth for cached room triangles.
///
/// Grid-visible room rendering already sorts by quantised tile cell, so every
/// triangle submitted for that cell shares the same fixed scalar depth. Keeping
/// the mapped OT slot beside the raw depth lets the hot cached path avoid
/// recomputing the same depth key for every triangle packet.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedTriangleDepth {
    slot: DepthSlot,
    depth: i32,
}

impl PreparedTriangleDepth {
    /// Build a prepared depth from fixed-depth world surface options.
    #[inline(always)]
    pub(crate) fn from_fixed_options<const OT_DEPTH: usize>(
        options: WorldSurfaceOptions,
    ) -> Option<Self> {
        let DepthPolicy::Fixed(depth) = options.depth_policy else {
            return None;
        };
        let depth = CameraDepth::new(depth.saturating_add(options.depth_bias));
        Some(Self {
            slot: options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth: depth.raw(),
        })
    }

    /// Build a prepared depth from a whole-quad surface's own averaged
    /// projected depth -- the same key its two split leaves would each
    /// approximate. Lets risky (triangle-depth) whole-quad surfaces
    /// stay on the single-packet quad path with a per-surface sort key
    /// instead of falling back to two triangle leaves.
    #[inline(always)]
    pub(crate) fn from_quad_average<const OT_DEPTH: usize>(
        options: WorldSurfaceOptions,
        projected: [ProjectedVertex; 4],
    ) -> Self {
        let average = (projected[0].sz + projected[1].sz + projected[2].sz + projected[3].sz) / 4;
        let depth = CameraDepth::new(average.saturating_add(options.depth_bias));
        Self {
            slot: options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth: depth.raw(),
        }
    }
}

/// Scratch command for a mixed world render pass.
///
/// Commands hold raw packet pointers so one pass can sort and submit
/// several packet kinds. The pointed-to packets must live until
/// [`WorldRenderPass::flush`] has inserted them into the OT.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldTriCommand {
    packet_ptr: *mut u32,
    depth: i32,
    slot: u16,
    order: u16,
    next: u16,
    render_layer: u8,
    words: u8,
}

/// Compact storage used by the comparison-free bucketed path.
///
/// On PS1 this is eight bytes versus sixteen for [`WorldTriCommand`]. The
/// bucketed pass aliases the beginning of its caller-provided command scratch
/// as this type because it needs only a packet pointer, slot and word count.
#[repr(C)]
#[derive(Copy, Clone)]
struct BucketedWorldCommand {
    packet_ptr: *mut u32,
    // Machine-word storage keeps this compact at eight bytes on PS1 while
    // leaving the host representation fully initialized for semantic tests.
    slot_words: usize,
}

impl BucketedWorldCommand {
    #[inline(always)]
    fn new(packet_ptr: *mut u32, slot: usize, words: u8) -> Self {
        Self {
            packet_ptr,
            slot_words: slot.min(u16::MAX as usize) | ((words as usize) << 24),
        }
    }

    #[inline(always)]
    #[cfg(test)]
    const fn slot(self) -> usize {
        self.slot_words & u16::MAX as usize
    }

    #[inline(always)]
    #[cfg(test)]
    const fn words(self) -> u8 {
        (self.slot_words >> 24) as u8
    }
}

impl WorldTriCommand {
    /// Empty command value for static scratch-buffer initialisation.
    pub const EMPTY: Self = Self {
        packet_ptr: core::ptr::null_mut(),
        depth: 0,
        slot: 0,
        order: 0,
        next: 0,
        render_layer: 0,
        words: 0,
    };

    #[cfg(test)]
    pub(crate) const fn depth_raw(self) -> i32 {
        self.depth
    }
}

/// Per-submit counters and overflow flags for world surfaces.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorldRenderStats {
    /// Triangles accepted into the pass after culling.
    pub submitted_triangles: u16,
    /// Triangles rejected by back-face culling.
    pub culled_triangles: u16,
    /// Triangles that crossed the near plane and were clipped before
    /// projection.
    pub clipped_triangles: u16,
    /// Oversized projected triangles split to satisfy PS1 hardware
    /// extent limits.
    pub split_triangles: u16,
    /// Triangles dropped before packet emission because they were
    /// fully clipped or could not be made hardware-legal.
    pub dropped_triangles: u16,
    /// True if the primitive packet arena filled up.
    pub primitive_overflow: bool,
    /// True if the command scratch buffer filled up.
    pub command_overflow: bool,
}

/// Per-submit counters and overflow flags for textured model rendering.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TexturedModelRenderStats {
    /// Vertices projected into the caller-provided scratch buffer.
    pub projected_vertices: u16,
    /// Triangles accepted into the pass after culling.
    pub submitted_triangles: u16,
    /// Triangles rejected by back-face culling.
    pub culled_triangles: u16,
    /// Oversized projected triangles split to satisfy PS1 hardware
    /// extent limits.
    pub split_triangles: u16,
    /// Triangles skipped because a face referenced vertices outside
    /// the part's projected vertex range.
    pub skipped_triangles: u16,
    /// Triangles dropped before packet emission because they were
    /// behind the near plane or could not be made hardware-legal.
    pub dropped_triangles: u16,
    /// Vertices projected through the CPU blend path.
    pub cpu_blended_vertices: u16,
    /// Faces routed through any packed fast path.
    pub packed_face_calls: u16,
    /// Faces routed through the packed unclamped helper.
    pub packed_unclamped_face_calls: u16,
    /// Faces routed through packed helpers that still clamp screen coordinates.
    pub packed_clamped_face_calls: u16,
    /// Faces routed through the generic packed helper.
    pub packed_general_face_calls: u16,
    /// Faces routed through the fully general helper.
    pub fallback_face_calls: u16,
    /// Packed faces that fell back to split/general submission for hardware extents.
    pub hw_extent_fallbacks: u16,
    /// Faces dropped by near-plane checks.
    pub near_plane_dropped_faces: u16,
    /// Faces dropped by final hardware-safety checks.
    pub hw_unsafe_dropped_faces: u16,
    /// Triangles submitted through packed fast paths.
    pub fast_submitted_triangles: u16,
    /// True if the vertex scratch buffer was too small for any part.
    pub vertex_overflow: bool,
    /// True if the primitive packet arena filled up.
    pub primitive_overflow: bool,
    /// True if the command scratch buffer filled up.
    pub command_overflow: bool,
}

/// Mixed world render pass.
///
/// Authoring code can submit surfaces as quads or triangles; the pass
/// stores sorted triangle packets internally so culling, depth bucketing,
/// and same-slot ordering are deterministic across packet kinds.
#[must_use = "call flush() to insert submitted triangles into the ordering table"]
pub struct WorldRenderPass<'a, 'ot, const OT_DEPTH: usize> {
    ot: &'a mut OtFrame<'ot, OT_DEPTH>,
    commands: &'a mut [WorldTriCommand],
    command_len: usize,
    next_order: u16,
    ordering: WorldCommandOrdering,
    slot_heads: MaybeUninit<[u16; OT_DEPTH]>,
    slot_tails: MaybeUninit<[u16; OT_DEPTH]>,
}

/// Scratch command for a Gouraud triangle render pass.
///
/// Caller-owned arrays of this type let the engine collect triangles
/// from several meshes before it mutates the ordering table. The
/// fields are private so call sites cannot accidentally depend on the
/// insertion-order details.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GouraudTriCommand {
    slot: DepthSlot,
    depth: i32,
    primitive_index: usize,
    next: u16,
}

impl GouraudTriCommand {
    /// Empty command value for static scratch-buffer initialisation.
    pub const EMPTY: Self = Self {
        slot: DepthSlot::new(0),
        depth: 0,
        primitive_index: 0,
        next: GOURAUD_COMMAND_NONE,
    };
}

/// Options for submitting a lit Gouraud mesh.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GouraudMeshOptions {
    /// OT slot band reserved for this mesh layer.
    pub depth_band: DepthBand,
    /// Camera-space depth range mapped into `depth_band`.
    pub depth_range: DepthRange,
    /// Triangle scalar depth policy.
    pub depth_policy: DepthPolicy,
    /// Signed offset added to the scalar depth before slot mapping.
    pub depth_bias: i32,
    /// Pixel offset added to projected vertices before packet build.
    pub screen_offset: (i16, i16),
    /// Normal used when the mesh blob lacks a normal for a vertex.
    pub default_normal: Vec3I16,
    /// Material RGB used when the mesh blob lacks face colours.
    pub default_material: (u8, u8, u8),
    /// Whether clockwise screen-space triangles should be culled.
    pub cull_backfaces: bool,
}

impl GouraudMeshOptions {
    /// Build mesh options for a world-geometry depth band and range.
    pub const fn new(depth_band: DepthBand, depth_range: DepthRange) -> Self {
        Self {
            depth_band,
            depth_range,
            depth_policy: DepthPolicy::Average,
            depth_bias: 0,
            screen_offset: (0, 0),
            default_normal: Vec3I16::ZERO,
            default_material: (128, 128, 128),
            cull_backfaces: true,
        }
    }

    /// Return options with a different scalar depth policy.
    pub const fn with_depth_policy(mut self, depth_policy: DepthPolicy) -> Self {
        self.depth_policy = depth_policy;
        self
    }

    /// Return options with a signed depth bias.
    pub const fn with_depth_bias(mut self, depth_bias: i32) -> Self {
        self.depth_bias = depth_bias;
        self
    }

    /// Return options with a projected-screen-space offset.
    pub const fn with_screen_offset(mut self, screen_offset: (i16, i16)) -> Self {
        self.screen_offset = screen_offset;
        self
    }

    /// Return options with a different fallback normal.
    pub const fn with_default_normal(mut self, default_normal: Vec3I16) -> Self {
        self.default_normal = default_normal;
        self
    }

    /// Return options with a different fallback material colour.
    pub const fn with_default_material(mut self, default_material: (u8, u8, u8)) -> Self {
        self.default_material = default_material;
        self
    }

    /// Return options with back-face culling enabled or disabled.
    pub const fn with_backface_culling(mut self, cull_backfaces: bool) -> Self {
        self.cull_backfaces = cull_backfaces;
        self
    }
}

/// Per-submit counters and overflow flags.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MeshRenderStats {
    /// Vertices projected into the caller-provided scratch buffer.
    pub projected_vertices: u16,
    /// Triangles accepted into the pass after culling.
    pub submitted_triangles: u16,
    /// Triangles rejected by back-face culling.
    pub culled_triangles: u16,
    /// Triangles skipped because their vertex indices were not projected.
    pub skipped_triangles: u16,
    /// True if the vertex scratch buffer was too small for the mesh.
    pub vertex_overflow: bool,
    /// True if the triangle packet arena filled up.
    pub primitive_overflow: bool,
    /// True if the command scratch buffer filled up.
    pub command_overflow: bool,
}

/// Opaque Gouraud triangle pass.
///
/// Create one pass for a world layer, submit one or more meshes after
/// loading their GTE transforms and lights, then call [`flush`](Self::flush).
/// The pass sorts all submitted triangles together so triangles from
/// different meshes can share the same OT depth policy.
#[must_use = "call flush() to insert submitted triangles into the ordering table"]
pub struct GouraudRenderPass<'a, 'ot, 'arena, const OT_DEPTH: usize> {
    ot: &'a mut OtFrame<'ot, OT_DEPTH>,
    triangles: &'a mut PrimitiveArena<'arena, TriGouraud>,
    commands: &'a mut [GouraudTriCommand],
    slot_heads: [u16; OT_DEPTH],
    command_len: usize,
}

impl<'a, 'ot, 'arena, const OT_DEPTH: usize> GouraudRenderPass<'a, 'ot, 'arena, OT_DEPTH> {
    /// Start an opaque Gouraud triangle pass.
    pub fn new(
        ot: &'a mut OtFrame<'ot, OT_DEPTH>,
        triangles: &'a mut PrimitiveArena<'arena, TriGouraud>,
        commands: &'a mut [GouraudTriCommand],
    ) -> Self {
        Self {
            ot,
            triangles,
            commands,
            slot_heads: [GOURAUD_COMMAND_NONE; OT_DEPTH],
            command_len: 0,
        }
    }

    /// Submit a lit mesh using the currently loaded GTE transform and light rig.
    ///
    /// The caller must load rotation, translation, projection setup,
    /// and lighting before calling this method. `projected_vertices`
    /// is temporary scratch and may be reused for the next mesh after
    /// the call returns.
    pub fn submit_lit_mesh(
        &mut self,
        mesh: &Mesh<'_>,
        projected_vertices: &mut [ProjectedLit],
        options: GouraudMeshOptions,
    ) -> MeshRenderStats {
        let mut stats = MeshRenderStats::default();
        let mesh_verts = mesh.vert_count() as usize;
        let project_count = mesh_verts
            .min(projected_vertices.len())
            .min(u16::MAX as usize);
        stats.vertex_overflow = mesh_verts > project_count;
        stats.projected_vertices = project_count as u16;

        let mut vi = 0usize;
        while vi + 2 < project_count {
            let vert = vi as u16;
            let projected = project_lit_triangle(
                [
                    mesh.vertex(vert),
                    mesh.vertex(vert + 1),
                    mesh.vertex(vert + 2),
                ],
                [
                    mesh.vertex_normal(vert).unwrap_or(options.default_normal),
                    mesh.vertex_normal(vert + 1)
                        .unwrap_or(options.default_normal),
                    mesh.vertex_normal(vert + 2)
                        .unwrap_or(options.default_normal),
                ],
                [
                    vertex_material(mesh, vert, options.default_material),
                    vertex_material(mesh, vert + 1, options.default_material),
                    vertex_material(mesh, vert + 2, options.default_material),
                ],
            );
            projected_vertices[vi] = offset_projected_lit(projected[0], options.screen_offset);
            projected_vertices[vi + 1] = offset_projected_lit(projected[1], options.screen_offset);
            projected_vertices[vi + 2] = offset_projected_lit(projected[2], options.screen_offset);
            vi += 3;
        }
        while vi < project_count {
            let vert = vi as u16;
            let p = project_lit(
                mesh.vertex(vert),
                mesh.vertex_normal(vert).unwrap_or(options.default_normal),
                vertex_material(mesh, vert, options.default_material),
            );
            projected_vertices[vi] = offset_projected_lit(p, options.screen_offset);
            vi += 1;
        }

        let face_stats =
            self.submit_projected_mesh(mesh, &projected_vertices[..project_count], options);
        stats.submitted_triangles = face_stats.submitted_triangles;
        stats.culled_triangles = face_stats.culled_triangles;
        stats.skipped_triangles = face_stats.skipped_triangles;
        stats.primitive_overflow = face_stats.primitive_overflow;
        stats.command_overflow = face_stats.command_overflow;
        stats
    }

    /// Submit a mesh whose vertices are already projected and lit.
    ///
    /// This covers CPU-lighting paths: the caller can compute
    /// per-vertex colours however it wants, while the engine still
    /// owns culling, depth policy, command sorting, and OT insertion.
    /// `projected_vertices` must be indexed by the mesh's face
    /// indices.
    pub fn submit_projected_mesh(
        &mut self,
        mesh: &Mesh<'_>,
        projected_vertices: &[ProjectedLit],
        options: GouraudMeshOptions,
    ) -> MeshRenderStats {
        let mut stats = MeshRenderStats {
            projected_vertices: projected_vertices.len().min(u16::MAX as usize) as u16,
            ..MeshRenderStats::default()
        };
        let project_count = projected_vertices.len();

        let mut face_idx = 0;
        while face_idx < mesh.face_count() {
            let (ia, ib, ic) = mesh.face(face_idx);
            if (ia as usize) >= project_count
                || (ib as usize) >= project_count
                || (ic as usize) >= project_count
            {
                stats.skipped_triangles = stats.skipped_triangles.wrapping_add(1);
                face_idx += 1;
                continue;
            }

            let verts = [
                projected_vertices[ia as usize],
                projected_vertices[ib as usize],
                projected_vertices[ic as usize],
            ];
            if options.cull_backfaces && back_facing(verts) {
                stats.culled_triangles = stats.culled_triangles.wrapping_add(1);
                face_idx += 1;
                continue;
            }

            if self.command_len >= self.commands.len() {
                stats.command_overflow = true;
                break;
            }

            let Some(primitive_index) = self.triangles.push_index(TriGouraud::new(
                [
                    (verts[0].sx, verts[0].sy),
                    (verts[1].sx, verts[1].sy),
                    (verts[2].sx, verts[2].sy),
                ],
                [
                    (verts[0].r, verts[0].g, verts[0].b),
                    (verts[1].r, verts[1].g, verts[1].b),
                    (verts[2].r, verts[2].g, verts[2].b),
                ],
            )) else {
                stats.primitive_overflow = true;
                break;
            };

            let depth = CameraDepth::new(options.depth_policy.depth(verts))
                .saturating_add(options.depth_bias);
            let command_index = self.command_len;
            self.commands[command_index] = GouraudTriCommand {
                slot: options
                    .depth_band
                    .slot_depth::<OT_DEPTH>(options.depth_range, depth),
                depth: depth.raw(),
                primitive_index,
                next: GOURAUD_COMMAND_NONE,
            };
            self.command_len += 1;
            self.insert_command_in_slot(command_index);
            stats.submitted_triangles = stats.submitted_triangles.wrapping_add(1);
            face_idx += 1;
        }

        stats
    }

    fn insert_command_in_slot(&mut self, command_index: usize) {
        if OT_DEPTH == 0 || command_index >= GOURAUD_COMMAND_NONE as usize {
            return;
        }

        let slot = self.commands[command_index].slot.index();
        debug_assert!(slot < OT_DEPTH);
        let command_link = command_index as u16;
        let head = self.slot_heads[slot];
        if head == GOURAUD_COMMAND_NONE
            || should_insert_gouraud_before(
                self.commands[command_index],
                self.commands[head as usize],
            )
        {
            self.commands[command_index].next = head;
            self.slot_heads[slot] = command_link;
            return;
        }

        let mut prev = head as usize;
        loop {
            let next = self.commands[prev].next;
            if next == GOURAUD_COMMAND_NONE
                || should_insert_gouraud_before(
                    self.commands[command_index],
                    self.commands[next as usize],
                )
            {
                self.commands[command_index].next = next;
                self.commands[prev].next = command_link;
                return;
            }
            prev = next as usize;
        }
    }

    /// Sort and insert all submitted triangles into the ordering table.
    pub fn flush(self) {
        let mut slot = 0;
        while slot < OT_DEPTH {
            let mut command_index = self.slot_heads[slot];
            while command_index != GOURAUD_COMMAND_NONE {
                let command = self.commands[command_index as usize];
                if let Some(tri) = self.triangles.get_mut(command.primitive_index) {
                    // SAFETY: Commands are created only after the primitive
                    // arena push succeeds, and their slots come from
                    // OT-depth-aware depth-band mapping.
                    unsafe {
                        self.ot.add_raw_unchecked(
                            command.slot.index(),
                            tri as *mut TriGouraud as *mut u32,
                            TriGouraud::WORDS,
                        )
                    };
                }
                command_index = command.next;
            }
            slot += 1;
        }
    }
}

fn offset_projected_lit(p: ProjectedLit, screen_offset: (i16, i16)) -> ProjectedLit {
    ProjectedLit {
        sx: p.sx.saturating_add(screen_offset.0),
        sy: p.sy.saturating_add(screen_offset.1),
        sz: p.sz,
        r: p.r,
        g: p.g,
        b: p.b,
    }
}

fn vertex_material(mesh: &Mesh<'_>, vert: u16, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    let mut face_idx = 0;
    while face_idx < mesh.face_count() {
        let (a, b, c) = mesh.face(face_idx);
        if a == vert || b == vert || c == vert {
            return mesh.face_color(face_idx).unwrap_or(fallback);
        }
        face_idx += 1;
    }
    fallback
}

fn load_world_projection_gte(projection: WorldProjection) {
    scene::set_screen_offset(
        (projection.screen_x as i32) << 16,
        (projection.screen_y as i32) << 16,
    );
    scene::set_projection_plane(clamp_u16_i32(projection.focal_length));
}

fn load_world_camera_gte(camera: WorldCamera) {
    load_world_projection_gte(camera.projection);
    let view = camera_gte_view_matrix(camera);
    let delta = WorldVertex::new(
        0i32.saturating_sub(camera.position.x),
        0i32.saturating_sub(camera.position.y),
        0i32.saturating_sub(camera.position.z),
    );
    let translation = Vec3I32::new(
        dot_world_q12(view.m[0], delta),
        dot_world_q12(view.m[1], delta),
        dot_world_q12(view.m[2], delta),
    );
    scene::load_rotation(&view);
    scene::load_translation(translation);
}

/// Install an identity camera-space transform for adaptive-style generated
/// vertices. The authored room vertices have already been transformed into
/// view space; RTPS is used here only for the perspective divide, exactly as
/// in the original subdivision path.
fn load_adaptive_view_projection_gte(projection: WorldProjection) {
    load_world_projection_gte(projection);
    scene::load_rotation(&Mat3I16::IDENTITY);
    scene::load_translation(Vec3I32::ZERO);
}

/// Project one ordinary (Y-up) camera-space point with the identity RTPS state
/// installed by [`load_adaptive_view_projection_gte`].
fn project_adaptive_view_vertex_gte(
    vertex: ViewVertex,
    projection: WorldProjection,
) -> Option<ProjectedVertex> {
    if vertex.z <= 0 || vertex.z < projection.near_z {
        return None;
    }
    let Some(input) = adaptive_view_gte_input(vertex) else {
        return projection.project_view(vertex);
    };
    adaptive_projected_option_from_gte(scene::project_vertex_scheduled(input), projection.near_z)
}

/// Project three generated camera-space vertices with one RTPT, matching the
/// batched GTE path in adaptive's room subdivision loop.
fn project_adaptive_view_triangle_gte(
    vertices: [ViewVertex; 3],
    projection: WorldProjection,
) -> Option<[ProjectedVertex; 3]> {
    if vertices
        .iter()
        .any(|vertex| vertex.z <= 0 || vertex.z < projection.near_z)
    {
        return None;
    }
    let inputs = [
        adaptive_view_gte_input(vertices[0]),
        adaptive_view_gte_input(vertices[1]),
        adaptive_view_gte_input(vertices[2]),
    ];
    let [Some(a), Some(b), Some(c)] = inputs else {
        return Some([
            projection.project_view(vertices[0])?,
            projection.project_view(vertices[1])?,
            projection.project_view(vertices[2])?,
        ]);
    };
    let projected = scene::project_triangle_scheduled(a, b, c);
    Some([
        adaptive_projected_option_from_gte(projected[0], projection.near_z)?,
        adaptive_projected_option_from_gte(projected[1], projection.near_z)?,
        adaptive_projected_option_from_gte(projected[2], projection.near_z)?,
    ])
}

/// Project a generated camera-space quad as RTPT + RTPS rather than four
/// independent RTPS operations.
fn project_adaptive_view_quad_gte(
    vertices: [ViewVertex; 4],
    projection: WorldProjection,
) -> Option<[ProjectedVertex; 4]> {
    let triangle =
        project_adaptive_view_triangle_gte([vertices[0], vertices[1], vertices[2]], projection)?;
    Some([
        triangle[0],
        triangle[1],
        triangle[2],
        project_adaptive_view_vertex_gte(vertices[3], projection)?,
    ])
}

/// Project the fixed 3x3 point lattice of a one-level subdivided quad.
///
/// The recursive path projects four overlapping child quads (16 inputs).
/// This schedule emits three RTPT operations and reuses the shared edge and
/// centre results, matching the table-driven topology used by later PS1 Tomb
/// Raider room renderers.
#[cfg(feature = "tr-subdivision-lattice")]
fn project_adaptive_view_lattice_gte(
    vertices: [ViewVertex; 9],
    projection: WorldProjection,
    root_projected: Option<[ProjectedVertex; 4]>,
) -> Option<[ProjectedVertex; 9]> {
    if vertices
        .iter()
        .any(|vertex| vertex.z <= 0 || vertex.z < projection.near_z)
    {
        return None;
    }
    if let Some(root) = root_projected {
        let generated = [
            adaptive_view_gte_input(vertices[1]),
            adaptive_view_gte_input(vertices[3]),
            adaptive_view_gte_input(vertices[4]),
            adaptive_view_gte_input(vertices[5]),
            adaptive_view_gte_input(vertices[7]),
        ];
        let [Some(top), Some(left), Some(center), Some(right), Some(bottom)] = generated else {
            return Some([
                root[0],
                projection.project_view(vertices[1])?,
                root[1],
                projection.project_view(vertices[3])?,
                projection.project_view(vertices[4])?,
                projection.project_view(vertices[5])?,
                root[2],
                projection.project_view(vertices[7])?,
                root[3],
            ]);
        };
        let first = scene::project_triangle_scheduled(top, left, center);
        let second = scene::project_triangle_scheduled(right, bottom, center);
        return Some([
            root[0],
            adaptive_projected_option_from_gte(first[0], projection.near_z)?,
            root[1],
            adaptive_projected_option_from_gte(first[1], projection.near_z)?,
            adaptive_projected_option_from_gte(first[2], projection.near_z)?,
            adaptive_projected_option_from_gte(second[0], projection.near_z)?,
            root[2],
            adaptive_projected_option_from_gte(second[1], projection.near_z)?,
            root[3],
        ]);
    }
    let inputs = [
        adaptive_view_gte_input(vertices[0]),
        adaptive_view_gte_input(vertices[1]),
        adaptive_view_gte_input(vertices[2]),
        adaptive_view_gte_input(vertices[3]),
        adaptive_view_gte_input(vertices[4]),
        adaptive_view_gte_input(vertices[5]),
        adaptive_view_gte_input(vertices[6]),
        adaptive_view_gte_input(vertices[7]),
        adaptive_view_gte_input(vertices[8]),
    ];
    let [
        Some(a),
        Some(b),
        Some(c),
        Some(d),
        Some(e),
        Some(f),
        Some(g),
        Some(h),
        Some(i),
    ] = inputs
    else {
        return Some([
            projection.project_view(vertices[0])?,
            projection.project_view(vertices[1])?,
            projection.project_view(vertices[2])?,
            projection.project_view(vertices[3])?,
            projection.project_view(vertices[4])?,
            projection.project_view(vertices[5])?,
            projection.project_view(vertices[6])?,
            projection.project_view(vertices[7])?,
            projection.project_view(vertices[8])?,
        ]);
    };
    let top = scene::project_triangle_scheduled(a, b, c);
    let middle = scene::project_triangle_scheduled(d, e, f);
    let bottom = scene::project_triangle_scheduled(g, h, i);
    Some([
        adaptive_projected_option_from_gte(top[0], projection.near_z)?,
        adaptive_projected_option_from_gte(top[1], projection.near_z)?,
        adaptive_projected_option_from_gte(top[2], projection.near_z)?,
        adaptive_projected_option_from_gte(middle[0], projection.near_z)?,
        adaptive_projected_option_from_gte(middle[1], projection.near_z)?,
        adaptive_projected_option_from_gte(middle[2], projection.near_z)?,
        adaptive_projected_option_from_gte(bottom[0], projection.near_z)?,
        adaptive_projected_option_from_gte(bottom[1], projection.near_z)?,
        adaptive_projected_option_from_gte(bottom[2], projection.near_z)?,
    ])
}

#[inline(always)]
fn adaptive_view_gte_input(vertex: ViewVertex) -> Option<Vec3I16> {
    Some(Vec3I16::new(
        i16::try_from(vertex.x).ok()?,
        i16::try_from(vertex.y.checked_neg()?).ok()?,
        i16::try_from(vertex.z).ok()?,
    ))
}

#[inline(always)]
fn adaptive_projected_option_from_gte(
    projected: psx_gte::scene::Projected,
    near_z: i32,
) -> Option<ProjectedVertex> {
    if (projected.sz as i32) < near_z {
        None
    } else {
        Some(ProjectedVertex::new(
            projected.sx,
            projected.sy,
            projected.sz as i32,
        ))
    }
}

#[inline(always)]
fn world_vertex_gte_input(vertex: WorldVertex) -> Option<Vec3I16> {
    // One combined i16 range test: `v + 0x8000` fits u16 exactly when
    // `v` is in i16 range, so OR-ing the three biased axes and
    // comparing once replaces three try_from/Option chains. This runs
    // per cell / per vertex in the scan loops, and the in-emulator
    // cost model is a flat 2 cycles per instruction, so the branch
    // count IS the cost. Wrapping overflow only happens for values far
    // outside i16 range and leaves a high bit set, so those still
    // reject.
    let bx = vertex.x.wrapping_add(0x8000);
    let by = vertex.y.wrapping_add(0x8000);
    let bz = vertex.z.wrapping_add(0x8000);
    if (bx | by | bz) as u32 > 0xFFFF {
        return None;
    }
    Some(Vec3I16::new(
        vertex.x as i16,
        vertex.y as i16,
        vertex.z as i16,
    ))
}

fn project_world_vertex_cpu(
    camera: WorldCamera,
    vertices: &[WorldVertex],
    projected_vertices: &mut [ProjectedVertex],
    index: usize,
) {
    if let Some(projected) = camera.project_world(vertices[index]) {
        projected_vertices[index] = projected;
    } else {
        projected_vertices[index] = ProjectedVertex::INVALID;
    }
}

/// Compose the GTE transform for one joint of a placed model
/// instance: `view × instance × pose_model_to_world`. The
/// returned matrix loads into GTE rotation; the returned vector
/// is the camera-space translation (already pre-rotated by the
/// view matrix). Public so the host editor preview can drive
/// the same math without re-implementing it.
pub fn compute_joint_view_transform(
    camera: WorldCamera,
    pose: JointPose,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    origin: WorldVertex,
) -> (Mat3I16, Vec3I32) {
    textured_model_part_gte_transform(camera, pose, instance_rotation, local_to_world, origin)
}

/// Compose the world-space transform for one animated model joint.
///
/// This shares the same `instance × pose_model_to_world` math used by model
/// rendering, but stops before camera view composition so gameplay systems can
/// attach child objects to animated joints.
pub fn compute_joint_world_transform(
    pose: JointPose,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    origin: WorldVertex,
) -> JointWorldTransform {
    let model = scaled_pose_matrix(pose, local_to_world);
    let rotation = mat3_mul_q12(&instance_rotation, &model);
    let scaled_pose_translation = Vec3I32::new(
        local_to_world.apply(pose.translation.x),
        local_to_world.apply(pose.translation.y),
        local_to_world.apply(pose.translation.z),
    );
    let rotated_pose_translation =
        rotate_translation_q12(&instance_rotation, scaled_pose_translation);
    JointWorldTransform {
        rotation,
        translation: WorldVertex::new(
            origin.x.saturating_add(rotated_pose_translation.x),
            origin.y.saturating_add(rotated_pose_translation.y),
            origin.z.saturating_add(rotated_pose_translation.z),
        ),
    }
}

/// Apply a model-local translation to a sampled joint pose.
pub fn apply_model_pose_translation(
    mut pose: JointPose,
    translation: ModelPoseTranslation,
) -> JointPose {
    pose.translation.x = pose.translation.x.saturating_add(translation.x);
    pose.translation.y = pose.translation.y.saturating_add(translation.y);
    pose.translation.z = pose.translation.z.saturating_add(translation.z);
    pose
}

/// Project one model vertex using the same GTE/CPU-blend split as model
/// rendering.
///
/// The caller must have already loaded the primary joint transform
/// into the GTE. Vertices without a valid secondary blend joint use
/// the GTE path; blend vertices use the CPU view/projection path so
/// host previews and runtime rendering keep identical deformation.
pub fn project_model_vertex_with_joint_transforms(
    vertex: ModelVertex,
    primary: JointViewTransform,
    joint_view_transforms: &[JointViewTransform],
    joint_count: usize,
    projection: WorldProjection,
) -> ProjectedVertex {
    project_textured_model_vertex(
        vertex,
        primary,
        joint_view_transforms,
        joint_count,
        projection,
    )
}

fn textured_model_part_gte_transform(
    camera: WorldCamera,
    pose: JointPose,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    origin: WorldVertex,
) -> (Mat3I16, Vec3I32) {
    let view = camera_gte_view_matrix(camera);
    textured_model_part_gte_transform_with_view(
        view,
        camera.position,
        pose,
        instance_rotation,
        local_to_world,
        origin,
    )
}

fn textured_model_part_gte_transform_with_view(
    view: Mat3I16,
    camera_position: WorldVertex,
    pose: JointPose,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    origin: WorldVertex,
) -> (Mat3I16, Vec3I32) {
    let model = scaled_pose_matrix(pose, local_to_world);
    player_vert_debug::record_compose_input(&model, Vec3I16::ZERO);
    // Composition order: view × instance × model. `instance` is
    // pre-multiplied through to rotate the joint pose around
    // the model origin in world space; `view` then rotates the
    // already-oriented model into camera space.
    let oriented = mat3_mul_q12(&instance_rotation, &model);
    let rotation = mat3_mul_q12(&view, &oriented);

    // Pose translation needs the same instance rotation before
    // it lands at world space -- otherwise a yawed model's joints
    // would translate along model-local axes rather than world
    // axes.
    let scaled_pose_translation = Vec3I32::new(
        local_to_world.apply(pose.translation.x),
        local_to_world.apply(pose.translation.y),
        local_to_world.apply(pose.translation.z),
    );
    let rotated_pose_translation =
        rotate_translation_q12(&instance_rotation, scaled_pose_translation);

    let world_translation = WorldVertex::new(
        origin.x.saturating_add(rotated_pose_translation.x),
        origin.y.saturating_add(rotated_pose_translation.y),
        origin.z.saturating_add(rotated_pose_translation.z),
    );
    let delta = WorldVertex::new(
        world_translation.x.saturating_sub(camera_position.x),
        world_translation.y.saturating_sub(camera_position.y),
        world_translation.z.saturating_sub(camera_position.z),
    );
    let translation = Vec3I32::new(
        dot_world_q12(view.m[0], delta),
        dot_world_q12(view.m[1], delta),
        dot_world_q12(view.m[2], delta),
    );

    (rotation, translation)
}

fn textured_model_part_gte_transform_with_view_gte_compose(
    view: Mat3I16,
    view_instance: Mat3I16,
    camera_position: WorldVertex,
    pose: JointPose,
    instance_rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    origin: WorldVertex,
) -> (Mat3I16, Vec3I32) {
    let model = scaled_pose_matrix(pose, local_to_world);
    let rotation = gte_compose_joint_rotation(view_instance, model);

    // Keep translation on the CPU for now. Cooked pose translations are
    // i32 model-local values, while MVMVA takes i16 vector inputs.
    let scaled_pose_translation = Vec3I32::new(
        local_to_world.apply(pose.translation.x),
        local_to_world.apply(pose.translation.y),
        local_to_world.apply(pose.translation.z),
    );
    let rotated_pose_translation =
        rotate_translation_q12(&instance_rotation, scaled_pose_translation);

    let world_translation = WorldVertex::new(
        origin.x.saturating_add(rotated_pose_translation.x),
        origin.y.saturating_add(rotated_pose_translation.y),
        origin.z.saturating_add(rotated_pose_translation.z),
    );
    let delta = WorldVertex::new(
        world_translation.x.saturating_sub(camera_position.x),
        world_translation.y.saturating_sub(camera_position.y),
        world_translation.z.saturating_sub(camera_position.z),
    );
    let translation = Vec3I32::new(
        dot_world_q12(view.m[0], delta),
        dot_world_q12(view.m[1], delta),
        dot_world_q12(view.m[2], delta),
    );

    (rotation, translation)
}

fn textured_model_part_gte_transform_with_view_gte_translation(
    view_instance: Mat3I16,
    view_origin_translation: Vec3I32,
    pose: JointPose,
    local_to_world: LocalToWorldScale,
) -> (Mat3I16, Vec3I32) {
    let model = scaled_pose_matrix(pose, local_to_world);
    let scaled_pose_translation = Vec3I32::new(
        local_to_world.apply(pose.translation.x),
        local_to_world.apply(pose.translation.y),
        local_to_world.apply(pose.translation.z),
    );
    let (pose_translation, pose_translation_shift) =
        quantize_pose_translation_for_gte(scaled_pose_translation);
    let (rotation, view_pose_translation) =
        gte_compose_joint_rotation_and_translation(view_instance, model, pose_translation);
    let translation = Vec3I32::new(
        view_origin_translation
            .x
            .saturating_add(rescale_gte_pose_translation(
                view_pose_translation.x,
                pose_translation_shift,
            )),
        view_origin_translation
            .y
            .saturating_add(rescale_gte_pose_translation(
                view_pose_translation.y,
                pose_translation_shift,
            )),
        view_origin_translation
            .z
            .saturating_add(rescale_gte_pose_translation(
                view_pose_translation.z,
                pose_translation_shift,
            )),
    );

    (rotation, translation)
}

fn textured_model_part_gte_transform_with_view_gte_packed_translation(
    view_instance: Mat3I16,
    view_origin_translation: Vec3I32,
    pose: GteJointPose,
    pose_translation: ModelPoseTranslation,
    local_to_world: LocalToWorldScale,
) -> Option<(Mat3I16, Vec3I32)> {
    if local_to_world != LocalToWorldScale::IDENTITY {
        return None;
    }
    let pose_translation = add_model_pose_translation_to_packed(
        pose.translation,
        pose_translation,
        pose.translation_shift,
    )?;
    let (rotation, view_pose_translation) = gte_compose_joint_rotation_and_translation(
        view_instance,
        Mat3I16 { m: pose.matrix },
        pose_translation,
    );
    let translation = Vec3I32::new(
        view_origin_translation
            .x
            .saturating_add(rescale_gte_pose_translation(
                view_pose_translation.x,
                pose.translation_shift,
            )),
        view_origin_translation
            .y
            .saturating_add(rescale_gte_pose_translation(
                view_pose_translation.y,
                pose.translation_shift,
            )),
        view_origin_translation
            .z
            .saturating_add(rescale_gte_pose_translation(
                view_pose_translation.z,
                pose.translation_shift,
            )),
    );

    Some((rotation, translation))
}

fn gte_compose_joint_rotation(view_instance: Mat3I16, model: Mat3I16) -> Mat3I16 {
    player_vert_debug::record_compose_input(&model, Vec3I16::ZERO);
    scene::load_rotation(&view_instance);
    scene::load_translation(Vec3I32::ZERO);

    // The default transform schedule carries the HWB-011 console-
    // confirmed MTC2-commit hazard gap (see scene::transform_vertex_mips).
    let c0 = scene::transform_vertex_scheduled(Vec3I16::new(
        model.m[0][0],
        model.m[1][0],
        model.m[2][0],
    ));
    let c1 = scene::transform_vertex_scheduled(Vec3I16::new(
        model.m[0][1],
        model.m[1][1],
        model.m[2][1],
    ));
    let c2 = scene::transform_vertex_scheduled(Vec3I16::new(
        model.m[0][2],
        model.m[1][2],
        model.m[2][2],
    ));

    Mat3I16 {
        m: [
            [clamp_i16(c0.x), clamp_i16(c1.x), clamp_i16(c2.x)],
            [clamp_i16(c0.y), clamp_i16(c1.y), clamp_i16(c2.y)],
            [clamp_i16(c0.z), clamp_i16(c1.z), clamp_i16(c2.z)],
        ],
    }
}

fn gte_compose_joint_rotation_and_translation(
    view_instance: Mat3I16,
    model: Mat3I16,
    pose_translation: Vec3I16,
) -> (Mat3I16, Vec3I32) {
    player_vert_debug::record_compose_input(&model, pose_translation);
    scene::load_rotation(&view_instance);
    scene::load_translation(Vec3I32::ZERO);

    // The default transform schedule carries the HWB-011 console-
    // confirmed MTC2-commit hazard gap (see scene::transform_vertex_mips).
    let c0 = scene::transform_vertex_scheduled(Vec3I16::new(
        model.m[0][0],
        model.m[1][0],
        model.m[2][0],
    ));
    let c1 = scene::transform_vertex_scheduled(Vec3I16::new(
        model.m[0][1],
        model.m[1][1],
        model.m[2][1],
    ));
    let c2 = scene::transform_vertex_scheduled(Vec3I16::new(
        model.m[0][2],
        model.m[1][2],
        model.m[2][2],
    ));
    let translation = scene::transform_vertex_scheduled(pose_translation);

    (
        Mat3I16 {
            m: [
                [clamp_i16(c0.x), clamp_i16(c1.x), clamp_i16(c2.x)],
                [clamp_i16(c0.y), clamp_i16(c1.y), clamp_i16(c2.y)],
                [clamp_i16(c0.z), clamp_i16(c1.z), clamp_i16(c2.z)],
            ],
        },
        translation,
    )
}

fn compute_view_origin_translation(
    view: Mat3I16,
    origin: WorldVertex,
    camera_position: WorldVertex,
) -> Vec3I32 {
    let delta = WorldVertex::new(
        origin.x.saturating_sub(camera_position.x),
        origin.y.saturating_sub(camera_position.y),
        origin.z.saturating_sub(camera_position.z),
    );
    Vec3I32::new(
        dot_world_q12(view.m[0], delta),
        dot_world_q12(view.m[1], delta),
        dot_world_q12(view.m[2], delta),
    )
}

fn quantize_pose_translation_for_gte(translation: Vec3I32) -> (Vec3I16, u8) {
    let mut max_abs = abs_i32_saturating(translation.x)
        .max(abs_i32_saturating(translation.y))
        .max(abs_i32_saturating(translation.z));
    let mut shift = 0u8;
    while max_abs > i16::MAX as i32 && shift < 15 {
        max_abs = (max_abs + 1) >> 1;
        shift += 1;
    }

    (
        Vec3I16::new(
            clamp_i16(round_shift_i32(translation.x, shift)),
            clamp_i16(round_shift_i32(translation.y, shift)),
            clamp_i16(round_shift_i32(translation.z, shift)),
        ),
        shift,
    )
}

fn add_model_pose_translation_to_packed(
    base: Vec3I16,
    offset: ModelPoseTranslation,
    shift: u8,
) -> Option<Vec3I16> {
    Some(Vec3I16::new(
        checked_add_packed_translation(base.x, offset.x, shift)?,
        checked_add_packed_translation(base.y, offset.y, shift)?,
        checked_add_packed_translation(base.z, offset.z, shift)?,
    ))
}

fn checked_add_packed_translation(base: i16, offset: i32, shift: u8) -> Option<i16> {
    let packed_offset = exact_shift_i32(offset, shift)?;
    let value = base as i32 + packed_offset;
    if value < i16::MIN as i32 || value > i16::MAX as i32 {
        None
    } else {
        Some(value as i16)
    }
}

fn exact_shift_i32(value: i32, shift: u8) -> Option<i32> {
    if shift == 0 {
        return Some(value);
    }
    let mask = (1i32 << shift) - 1;
    if value & mask == 0 {
        Some(value >> shift)
    } else {
        None
    }
}

fn abs_i32_saturating(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

fn round_shift_i32(value: i32, shift: u8) -> i32 {
    if shift == 0 {
        return value;
    }
    // psx-numeric-allow-next-line: depth-key widening; single 64-bit product from one mult
    let value = value as i64;
    let half = 1i64 << (shift - 1);
    if value >= 0 {
        ((value + half) >> shift) as i32
    } else {
        -(((-value + half) >> shift) as i32)
    }
}

fn rescale_gte_pose_translation(value: i32, shift: u8) -> i32 {
    value.saturating_mul(1i32 << shift)
}

/// Apply a Q12 rotation matrix to an i32 translation vector.
/// Runtime pose translations are model-local and bounded by cooked
/// asset scale, so keep this on the PS1's native 32-bit fast path.
fn rotate_translation_q12(rot: &Mat3I16, t: Vec3I32) -> Vec3I32 {
    let row = |r: [i16; 3]| -> i32 {
        let x = (r[0] as i32).saturating_mul(t.x);
        let y = (r[1] as i32).saturating_mul(t.y);
        let z = (r[2] as i32).saturating_mul(t.z);
        x.saturating_add(y).saturating_add(z) >> 12
    };
    Vec3I32::new(row(rot.m[0]), row(rot.m[1]), row(rot.m[2]))
}

fn camera_gte_view_matrix(camera: WorldCamera) -> Mat3I16 {
    let sy_sp = camera.sin_yaw.mul_q12(camera.sin_pitch).raw();
    let cy_sp = camera.cos_yaw.mul_q12(camera.sin_pitch).raw();
    let sy_cp = camera.sin_yaw.mul_q12(camera.cos_pitch).raw();
    let cy_cp = camera.cos_yaw.mul_q12(camera.cos_pitch).raw();

    Mat3I16 {
        m: [
            [
                clamp_i16(camera.cos_yaw.raw()),
                0,
                clamp_i16(-camera.sin_yaw.raw()),
            ],
            [
                clamp_i16(-sy_sp),
                clamp_i16(-camera.cos_pitch.raw()),
                clamp_i16(-cy_sp),
            ],
            [
                clamp_i16(-sy_cp),
                clamp_i16(camera.sin_pitch.raw()),
                clamp_i16(-cy_cp),
            ],
        ],
    }
}

/// Projects sky directions (points at infinity) through the GTE. It installs
/// the camera rotation with zero translation and the projection plane, so RTPS
/// performs the yaw/pitch rotate and perspective divide in hardware, replacing
/// the per-direction CPU rotate (eight Q12 muls) and perspective divide (two
/// divides). Load once, project the whole sky grid, then load the world camera
/// before world geometry -- this leaves the GTE holding the sky rotation/TR.
pub struct SkyDirectionProjector {
    near_z: i32,
}

impl SkyDirectionProjector {
    /// Install the camera rotation, zero translation, and projection plane.
    pub fn load(camera: WorldCamera) -> Self {
        load_world_projection_gte(camera.projection);
        scene::load_rotation(&camera_gte_view_matrix(camera));
        scene::load_translation(Vec3I32::ZERO);
        Self {
            near_z: camera.projection.near_z,
        }
    }

    /// Project a Q12 direction to screen space, or `None` when it is at or
    /// behind the near plane (the GTE's clamped SZ reproduces the CPU
    /// `z2 <= near_z` cull). Matches `camera_gte_view_matrix` so the result
    /// equals the former CPU sky projection.
    pub fn project(&self, dir: [i16; 3]) -> Option<(i16, i16)> {
        let p = scene::project_vertex(Vec3I16::new(dir[0], dir[1], dir[2]));
        if (p.sz as i32) > self.near_z {
            Some((p.sx, p.sy))
        } else {
            None
        }
    }
}

fn scaled_pose_matrix(pose: JointPose, local_to_world: LocalToWorldScale) -> Mat3I16 {
    let scale = local_to_world.scale().raw();
    let mut out = [[0i16; 3]; 3];
    let mut row = 0;
    while row < 3 {
        let mut col = 0;
        while col < 3 {
            // i16 x u16 always fits in i32, so the generic Q12 saturating
            // multiply's overflow checks are unnecessary for pose matrices.
            out[row][col] = clamp_i16(((pose.matrix[col][row] as i32) * scale) >> 12);
            col += 1;
        }
        row += 1;
    }
    Mat3I16 { m: out }
}

fn mat3_mul_q12(a: &Mat3I16, b: &Mat3I16) -> Mat3I16 {
    let mut out = [[0i16; 3]; 3];
    let mut row = 0;
    while row < 3 {
        let mut col = 0;
        while col < 3 {
            let mut sum = 0i32;
            let mut k = 0;
            while k < 3 {
                sum = sum.saturating_add((a.m[row][k] as i32) * (b.m[k][col] as i32));
                k += 1;
            }
            out[row][col] = clamp_i16(sum >> 12);
            col += 1;
        }
        row += 1;
    }
    Mat3I16 { m: out }
}

fn dot_world_q12(row: [i16; 3], v: WorldVertex) -> i32 {
    let x = (row[0] as i32).saturating_mul(v.x);
    let y = (row[1] as i32).saturating_mul(v.y);
    let z = (row[2] as i32).saturating_mul(v.z);
    x.saturating_add(y).saturating_add(z) >> 12
}

/// Software-side equivalent of one GTE RTPS transform stage.
///
/// Used by the blend-skin slow path: a vertex with weight on a second
/// joint cannot stay on the GTE because the rotation/translation
/// registers are loaded for the part's primary joint. We compute its
/// view-space position twice on the CPU, lerp, and project on the CPU
/// using `WorldProjection::project_view`.
#[inline]
fn project_textured_model_vertex(
    vertex: ModelVertex,
    primary: JointViewTransform,
    joint_view_transforms: &[JointViewTransform],
    joint_count: usize,
    projection: WorldProjection,
) -> ProjectedVertex {
    if model_vertex_uses_cpu_blend(vertex, joint_count) {
        project_blended_textured_model_vertex(vertex, primary, joint_view_transforms, projection)
    } else {
        project_gte_model_vertex(vertex)
    }
}

#[inline(always)]
fn project_blended_textured_model_vertex(
    vertex: ModelVertex,
    primary: JointViewTransform,
    joint_view_transforms: &[JointViewTransform],
    projection: WorldProjection,
) -> ProjectedVertex {
    let secondary = joint_view_transforms[vertex.joint1 as usize];
    // The part's primary joint is already loaded in the GTE by the caller's
    // project loop, so transform view_a there instead of by hand on the CPU.
    // The default schedule carries the HWB-011 hazard gap.
    let a = scene::transform_vertex_scheduled(vertex.position);
    let view_a = ViewVertex::new(a.x, a.y, a.z);
    // view_b needs the secondary joint: load it and transform.
    scene::load_rotation(&secondary.rotation);
    scene::load_translation(secondary.translation);
    let b = scene::transform_vertex_scheduled(vertex.position);
    let view_b = ViewVertex::new(b.x, b.y, b.z);
    let view_blend = lerp_view_vertex(view_a, view_b, vertex.blend);
    // Project the blended view-space vertex on the GTE (perspective divide in
    // hardware) instead of two CPU divides, then restore the primary joint so
    // the caller's GTE batch state is preserved on return.
    let projected = project_gte_view_vertex(view_blend, projection);
    player_vert_debug::observe(
        vertex, &primary, &secondary, view_a, view_b, view_blend, projected,
    );
    scene::load_rotation(&primary.rotation);
    scene::load_translation(primary.translation);
    projected
}

/// DIAGNOSTIC (vertex-explosion probe, not for release): per-frame bounds of
/// the player's skinned model vertices, captured in
/// `project_blended_textured_model_vertex` to isolate where the on-hardware
/// vertex stretch is born. Three stages, so one burn separates the suspects:
///   - `joint_abs_max`: largest |coord| of a per-joint view-space transform
///     (`view_a`/`view_b`) -> a bad JOINT MATRIX (the GTE rotation/translation).
///   - `blend_abs_max`: largest |coord| of the blended view-space position
///     -> the CPU lerp/SKINNING blend overflowing.
///   - `scr_*` / `oob`: projected screen coords -> the RTPS PROJECTION flinging
///     a vertex off-screen even from a sane view position.
/// On the silicon-faithful emulator these stay small/in-bounds; if hardware
/// blows one stage up while the others stay sane, that stage is the bug.
#[cfg(feature = "vert-debug")]
pub mod player_vert_debug {
    use super::{
        projected_model_vertex_inside_hw_bounds, JointViewTransform, Mat3I16, ModelVertex,
        ProjectedVertex, Vec3I16, ViewVertex,
    };
    use psx_gte::scene;

    /// Per-frame player skinned-vertex bounds for the explosion probe.
    /// Hardware vs the silicon-faithful emulator (controlled fixed-pose diff,
    /// IMG_6161-6165) showed only the projected X widens -- the view-space X
    /// is computed wider on hardware. So we track the view-space X EXTENT
    /// (min..max), split per skinning stage, to find which step widens it:
    /// `view_a` (primary joint), `view_b` (secondary joint AFTER the GTE
    /// matrix reload -- the CTC2->MVMVA hazard suspect), and the lerp blend.
    #[derive(Clone, Copy)]
    pub struct Bounds {
        /// Min/max view-space X of the PRIMARY joint transform (`view_a`).
        pub ax_min: i32,
        /// See [`Bounds::ax_min`].
        pub ax_max: i32,
        /// Min/max view-space X of the SECONDARY joint transform (`view_b`).
        pub bx_min: i32,
        /// See [`Bounds::bx_min`].
        pub bx_max: i32,
        /// Min/max view-space X of the blended (lerp) position.
        pub lx_min: i32,
        /// See [`Bounds::lx_min`].
        pub lx_max: i32,
        /// Min/max projected screen X across ALL player vertex paths
        /// (blended + single-bone batch + remainder), merged from the model
        /// pass's `projected_min_x/max_x` accumulator. The `scr_*` fields
        /// below cover only the blended verts; comparing the two on hardware
        /// tells whether the widening is blended-only or mesh-wide.
        pub px_min: i16,
        /// See [`Bounds::px_min`].
        pub px_max: i16,
        /// Min projected screen X.
        pub scr_min_x: i16,
        /// Max projected screen X.
        pub scr_max_x: i16,
        /// Min projected screen Y.
        pub scr_min_y: i16,
        /// Max projected screen Y.
        pub scr_max_y: i16,
        /// Vertices projected outside the hardware screen bounds.
        pub oob: u16,
        /// Total blended vertices observed this frame.
        pub total: u16,
    }

    impl Bounds {
        const EMPTY: Self = Self {
            ax_min: i32::MAX,
            ax_max: i32::MIN,
            bx_min: i32::MAX,
            bx_max: i32::MIN,
            lx_min: i32::MAX,
            lx_max: i32::MIN,
            px_min: i16::MAX,
            px_max: i16::MIN,
            scr_min_x: i16::MAX,
            scr_max_x: i16::MIN,
            scr_min_y: i16::MAX,
            scr_max_y: i16::MIN,
            oob: 0,
            total: 0,
        };
    }

    static mut B: Bounds = Bounds::EMPTY;

    /// Clear at the start of each frame's model pass. The worst-vertex
    /// snapshot is NOT cleared: it latches the all-time worst event so the
    /// auto-cycling overlay pages stay mutually consistent across the
    /// seconds it takes to photograph them.
    pub fn reset() {
        unsafe {
            *core::ptr::addr_of_mut!(B) = Bounds::EMPTY;
            CAPTURED = false;
        }
    }

    /// Snapshot for the HUD overlay.
    pub fn get() -> Bounds {
        unsafe { *core::ptr::addr_of!(B) }
    }

    /// Merge the model pass's ALL-path projected X bounds (covers blended +
    /// single-bone batch + remainder vertices). Called by the world model
    /// pass for models that had at least one blended vertex (the player).
    pub fn merge_all_x(min_x: i16, max_x: i16) {
        unsafe {
            let b = &mut *core::ptr::addr_of_mut!(B);
            b.px_min = b.px_min.min(min_x);
            b.px_max = b.px_max.max(max_x);
        }
    }

    pub(super) fn observe(
        vertex: ModelVertex,
        primary: &JointViewTransform,
        secondary: &JointViewTransform,
        view_a: ViewVertex,
        view_b: ViewVertex,
        view_blend: ViewVertex,
        p: ProjectedVertex,
    ) {
        unsafe {
            let b = &mut *core::ptr::addr_of_mut!(B);
            b.ax_min = b.ax_min.min(view_a.x);
            b.ax_max = b.ax_max.max(view_a.x);
            b.bx_min = b.bx_min.min(view_b.x);
            b.bx_max = b.bx_max.max(view_b.x);
            b.lx_min = b.lx_min.min(view_blend.x);
            b.lx_max = b.lx_max.max(view_blend.x);
            b.scr_min_x = b.scr_min_x.min(p.sx);
            b.scr_max_x = b.scr_max_x.max(p.sx);
            b.scr_min_y = b.scr_min_y.min(p.sy);
            b.scr_max_y = b.scr_max_y.max(p.sy);
            b.total = b.total.wrapping_add(1);
            if !projected_model_vertex_inside_hw_bounds(p) {
                b.oob = b.oob.wrapping_add(1);
            }
            // Freeze the joint compose table on the first blended vertex:
            // this model IS the player, later models must not overwrite it.
            CAPTURED = true;
            // Worst-vertex snapshot, LATCHED ALL-TIME (not per frame): the
            // idle animation drifts values frame to frame, and the overlay
            // pages are photographed seconds apart -- a per-frame snapshot
            // would mix frames and break same-frame offline replay. The
            // latch holds the single worst event since boot (a hardware
            // stretch event, |X| ~10x normal, dominates immediately), and
            // the compose inputs for its two joints are copied in HERE so
            // all four pages describe one frozen event.
            let score = abs_i32(view_a.x)
                .max(abs_i32(view_b.x))
                .max(abs_i32(view_blend.x));
            if score > SNAP_SCORE {
                SNAP_SCORE = score;
                let joints = &*core::ptr::addr_of!(JOINTS);
                let m0 = if (PRIMARY_JOINT as usize) < JOINT_CAP {
                    joints[PRIMARY_JOINT as usize]
                } else {
                    JointComposeIn::EMPTY
                };
                let m1 = if (vertex.joint1 as usize) < JOINT_CAP {
                    joints[vertex.joint1 as usize]
                } else {
                    JointComposeIn::EMPTY
                };
                *core::ptr::addr_of_mut!(SNAP) = Snapshot {
                    valid: true,
                    score,
                    vi: *core::ptr::addr_of!(VIEW_INSTANCE),
                    m0,
                    m1,
                    pos: vertex.position,
                    j0: PRIMARY_JOINT,
                    j1: vertex.joint1,
                    blend: vertex.blend,
                    rot0: primary.rotation,
                    tr0: [
                        primary.translation.x,
                        primary.translation.y,
                        primary.translation.z,
                    ],
                    rot1: secondary.rotation,
                    tr1: [
                        secondary.translation.x,
                        secondary.translation.y,
                        secondary.translation.z,
                    ],
                    va: [view_a.x, view_a.y, view_a.z],
                    vb: [view_b.x, view_b.y, view_b.z],
                    vl: [view_blend.x, view_blend.y, view_blend.z],
                    sx: p.sx,
                    sy: p.sy,
                    flag: scene::read_flag(),
                };
            }
        }
    }

    fn abs_i32(v: i32) -> i32 {
        if v == i32::MIN {
            i32::MAX
        } else {
            v.abs()
        }
    }

    // ------------------------------------------------------------------
    // ALL-STAGES live capture (single-burn bisection). For the worst
    // blended vertex of the frame the overlay can show the FULL skinning
    // chain in hex; each stage is then recomputable offline through the
    // console-exact GTE core, and the FIRST stage where the photographed
    // hardware value diverges from the recomputation is the bug locus:
    //   compose inputs (view*instance + per-joint model matrix, page 4)
    //     -> composed matrices as CTC2'd (rot0/rot1 + translations, page 3)
    //     -> MVMVA outputs (view_a / view_b), lerp, projection, FLAG (page 2)
    // ------------------------------------------------------------------

    /// Worst-vertex full-chain snapshot for the overlay.
    #[derive(Clone, Copy)]
    pub struct Snapshot {
        /// False until the first blended vertex of the frame lands.
        pub valid: bool,
        /// Model-local vertex position (MVMVA input vector).
        pub pos: Vec3I16,
        /// Primary joint index (the part's joint).
        pub j0: u8,
        /// Secondary joint index.
        pub j1: u8,
        /// Secondary blend weight (0..=255).
        pub blend: u8,
        /// Primary joint matrix exactly as CTC2'd for `view_a`.
        pub rot0: Mat3I16,
        /// Primary joint view-space translation.
        pub tr0: [i32; 3],
        /// Secondary joint matrix exactly as CTC2'd for `view_b`.
        pub rot1: Mat3I16,
        /// Secondary joint view-space translation.
        pub tr1: [i32; 3],
        /// Primary-joint MVMVA output (view-space A).
        pub va: [i32; 3],
        /// Secondary-joint MVMVA output (view-space B).
        pub vb: [i32; 3],
        /// CPU lerp of A/B by `blend`.
        pub vl: [i32; 3],
        /// Projected screen X (RTPS of `vl`).
        pub sx: i16,
        /// Projected screen Y.
        pub sy: i16,
        /// GTE FLAG read right after the projection.
        pub flag: u32,
        /// The latch score (max |view X| across the three stages).
        pub score: i32,
        /// View x instance compose input, from the LATCHED frame.
        pub vi: Mat3I16,
        /// Primary joint compose inputs from the latched frame.
        pub m0: JointComposeIn,
        /// Secondary joint compose inputs from the latched frame.
        pub m1: JointComposeIn,
    }

    impl Snapshot {
        const EMPTY: Self = Self {
            valid: false,
            pos: Vec3I16::ZERO,
            j0: 0xFF,
            j1: 0xFF,
            blend: 0,
            rot0: Mat3I16::IDENTITY,
            tr0: [0; 3],
            rot1: Mat3I16::IDENTITY,
            tr1: [0; 3],
            va: [0; 3],
            vb: [0; 3],
            vl: [0; 3],
            sx: 0,
            sy: 0,
            flag: 0,
            score: -1,
            vi: Mat3I16::IDENTITY,
            m0: JointComposeIn::EMPTY,
            m1: JointComposeIn::EMPTY,
        };
    }

    /// Per-joint GTE compose INPUTS (recorded inside the compose calls, so
    /// they are exactly what the GTE saw, not a re-derivation).
    #[derive(Clone, Copy)]
    pub struct JointComposeIn {
        /// True once this joint's compose ran this frame.
        pub valid: bool,
        /// Model/pose matrix fed column-by-column through MVMVA.
        pub model: Mat3I16,
        /// Pose translation fed to the translation MVMVA (zero for the
        /// rotation-only compose path).
        pub ptrans: Vec3I16,
    }

    impl JointComposeIn {
        const EMPTY: Self = Self {
            valid: false,
            model: Mat3I16::IDENTITY,
            ptrans: Vec3I16::ZERO,
        };
    }

    /// Joint slots captured per frame (player joint counts are far below this).
    pub const JOINT_CAP: usize = 24;

    static mut SNAP: Snapshot = Snapshot::EMPTY;
    static mut SNAP_SCORE: i32 = -1;
    static mut CAPTURED: bool = false;
    static mut PRIMARY_JOINT: u8 = 0xFF;
    static mut CUR_SLOT: u8 = 0xFF;
    static mut VIEW_INSTANCE: Mat3I16 = Mat3I16::IDENTITY;
    static mut JOINTS: [JointComposeIn; JOINT_CAP] = [JointComposeIn::EMPTY; JOINT_CAP];

    /// Start of a model's joint pass: reset the compose table for THIS model
    /// unless the player (a model with blended verts) was already captured
    /// this frame.
    pub fn record_joints_begin(view_instance: &Mat3I16) {
        unsafe {
            if CAPTURED {
                return;
            }
            *core::ptr::addr_of_mut!(VIEW_INSTANCE) = *view_instance;
            let joints = &mut *core::ptr::addr_of_mut!(JOINTS);
            let mut i = 0;
            while i < JOINT_CAP {
                joints[i].valid = false;
                i += 1;
            }
        }
    }

    /// Sync the compose-record slot to the joint index about to be built
    /// (compose functions don't know the index; jointless poses skip).
    pub fn set_joint_slot(idx: u8) {
        unsafe {
            CUR_SLOT = idx;
        }
    }

    /// Record the GTE compose inputs for the current joint slot. Called from
    /// inside the compose functions with the exact values handed to the GTE.
    pub(super) fn record_compose_input(model: &Mat3I16, ptrans: Vec3I16) {
        unsafe {
            if CAPTURED || CUR_SLOT as usize >= JOINT_CAP {
                return;
            }
            let joints = &mut *core::ptr::addr_of_mut!(JOINTS);
            joints[CUR_SLOT as usize] = JointComposeIn {
                valid: true,
                model: *model,
                ptrans,
            };
        }
    }

    /// The part loop's current primary joint (for the snapshot's `j0`).
    pub fn set_primary_joint(idx: u8) {
        unsafe {
            PRIMARY_JOINT = idx;
        }
    }

    /// Worst-vertex snapshot for the HUD overlay (all-time latch).
    pub fn snapshot() -> Snapshot {
        unsafe { *core::ptr::addr_of!(SNAP) }
    }
}

/// No-op twin of [`player_vert_debug`] for builds without the
/// `vert-debug` feature: identical signatures, empty inlined bodies, so
/// the hot paths keep their unconditional calls at zero cost and no
/// capture state exists in shipping or perf-measurement builds.
#[cfg(not(feature = "vert-debug"))]
pub mod player_vert_debug {
    use super::{JointViewTransform, Mat3I16, ModelVertex, ProjectedVertex, Vec3I16, ViewVertex};

    /// See the `vert-debug` build for the real implementation.
    #[inline(always)]
    pub fn reset() {}
    /// See the `vert-debug` build for the real implementation.
    #[inline(always)]
    pub fn record_joints_begin(_view_instance: &Mat3I16) {}
    /// See the `vert-debug` build for the real implementation.
    #[inline(always)]
    pub fn set_joint_slot(_idx: u8) {}
    /// See the `vert-debug` build for the real implementation.
    #[inline(always)]
    pub fn set_primary_joint(_idx: u8) {}
    /// See the `vert-debug` build for the real implementation.
    #[inline(always)]
    pub fn merge_all_x(_min_x: i16, _max_x: i16) {}

    #[inline(always)]
    pub(super) fn record_compose_input(_model: &Mat3I16, _ptrans: Vec3I16) {}

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(super) fn observe(
        _vertex: ModelVertex,
        _primary: &JointViewTransform,
        _secondary: &JointViewTransform,
        _view_a: ViewVertex,
        _view_b: ViewVertex,
        _view_blend: ViewVertex,
        _p: ProjectedVertex,
    ) {
    }
}

/// Minimum `joint1` weight (out of 255) for a vertex to take the two-bone
/// blend path. Below it the secondary influence is under ~6%, which reads as
/// single-bone: the vertex rides the primary-joint GTE batch instead of the
/// per-vertex blend, dropping it off the expensive path with no visible change.
const MODEL_BLEND_MIN_WEIGHT: u8 = 16;

#[inline]
fn model_vertex_uses_cpu_blend(vertex: ModelVertex, joint_count: usize) -> bool {
    vertex.is_blend()
        && vertex.blend >= MODEL_BLEND_MIN_WEIGHT
        && (vertex.joint1 as usize) < joint_count
}

#[inline]
fn project_gte_model_vertex(vertex: ModelVertex) -> ProjectedVertex {
    projected_from_gte(scene::project_vertex_scheduled(vertex.position))
}

#[inline]
fn projected_from_gte(projected: scene::Projected) -> ProjectedVertex {
    ProjectedVertex::new(projected.sx, projected.sy, projected.sz as i32)
}

/// Store one RTPT triple and fold its bookkeeping (near-plane verdict,
/// screen-extent bounds) in a single pass.
///
/// The GTE saturates SX/SY to the hardware vertex range, so the
/// inside-hw-bounds verdict cannot fail for GTE-projected vertices and
/// is intentionally not part of this path (the CPU-blend path keeps
/// its own check). Called between an RTPT kick and its read, this is
/// the scalar work that hides the GTE op latency.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn commit_projected_triple(
    base: usize,
    triple: [scene::Projected; 3],
    near_z: i32,
    out: &mut [ProjectedVertex],
    all_in_front: &mut bool,
    min_x: &mut i16,
    max_x: &mut i16,
    min_y: &mut i16,
    max_y: &mut i16,
) {
    let mut k = 0usize;
    while k < 3 {
        let p = triple[k];
        *all_in_front &= (p.sz as i32) >= near_z;
        *min_x = (*min_x).min(p.sx);
        *max_x = (*max_x).max(p.sx);
        *min_y = (*min_y).min(p.sy);
        *max_y = (*max_y).max(p.sy);
        out[base + k] = ProjectedVertex::new(p.sx, p.sy, p.sz as i32);
        k += 1;
    }
}

#[inline]
fn projected_option_from_gte(projected: scene::Projected, near_z: i32) -> Option<ProjectedVertex> {
    if (projected.sz as i32) >= near_z {
        Some(projected_from_gte(projected))
    } else {
        None
    }
}

#[inline]
fn valid_projected_from_gte(projected: scene::Projected, near_z: i32) -> ProjectedVertex {
    if (projected.sz as i32) >= near_z {
        projected_from_gte(projected)
    } else {
        ProjectedVertex::INVALID
    }
}

#[inline]
fn projected_model_vertex_in_front(vertex: ProjectedVertex, near_z: i32) -> bool {
    vertex.sz >= near_z
}

#[inline]
fn projected_model_vertex_inside_hw_bounds(vertex: ProjectedVertex) -> bool {
    vertex.sx >= PSX_VERTEX_MIN
        && vertex.sx <= PSX_VERTEX_MAX
        && vertex.sy >= PSX_VERTEX_MIN
        && vertex.sy <= PSX_VERTEX_MAX
}

#[inline]
fn track_projected_model_bounds(
    vertex: ProjectedVertex,
    min_x: &mut i16,
    max_x: &mut i16,
    min_y: &mut i16,
    max_y: &mut i16,
) {
    *min_x = (*min_x).min(vertex.sx);
    *max_x = (*max_x).max(vertex.sx);
    *min_y = (*min_y).min(vertex.sy);
    *max_y = (*max_y).max(vertex.sy);
}

#[inline]
pub(crate) fn projected_model_bounds_hw_extent_safe(
    min_x: i16,
    max_x: i16,
    min_y: i16,
    max_y: i16,
) -> bool {
    min_x <= max_x
        && min_y <= max_y
        && ((max_x as i32) - (min_x as i32)) <= PSX_TRI_MAX_DX
        && ((max_y as i32) - (min_y as i32)) <= PSX_TRI_MAX_DY
}

#[inline]
fn projected_model_face_crosses_near(verts: [ProjectedVertex; 3], near_z: i32) -> bool {
    verts[0].sz < near_z || verts[1].sz < near_z || verts[2].sz < near_z
}

/// CPU projection that matches the GTE RTPS convention used by the
/// rest of the model render path.
///
/// `WorldProjection::project_view` is for *unflipped* camera-space
/// vertices and applies its own `screen_y -= y*H/z` flip. The
/// view-space output of [`cpu_view_transform`] is already pre-flipped
/// in Y by the GTE-style camera matrix in
/// [`camera_gte_view_matrix`], so we project with `screen_y += y*H/z`
/// to avoid the double-flip that put blend verts on the wrong half
/// of the screen.
#[inline]
fn cpu_project_gte_view(view: ViewVertex, projection: WorldProjection) -> Option<ProjectedVertex> {
    if view.z <= 0 || view.z < projection.near_z {
        return None;
    }
    let sx = (projection.screen_x as i32) + (view.x * projection.focal_length) / view.z;
    let sy = (projection.screen_y as i32) + (view.y * projection.focal_length) / view.z;
    Some(ProjectedVertex::new(clamp_i16(sx), clamp_i16(sy), view.z))
}

/// Project an already-view-space vertex through the GTE. Loading the identity
/// rotation with zero translation makes RTPS perform only the perspective
/// divide and screen mapping -- in hardware -- replacing the two CPU divides
/// in [`cpu_project_gte_view`]. The screen offset and projection plane are
/// already loaded frame-wide for the model batch. The GTE vertex input is
/// i16, so a rare oversized view vertex (far off-screen) falls back to the CPU
/// path. Leaves the GTE rotation clobbered; the caller restores its own state.
fn project_gte_view_vertex(view: ViewVertex, projection: WorldProjection) -> ProjectedVertex {
    scene::load_rotation(&Mat3I16::IDENTITY);
    scene::load_translation(Vec3I32::ZERO);
    project_gte_view_vertex_identity_loaded(view, projection)
}

/// [`project_gte_view_vertex`] minus the identity/zero load: the caller
/// has already put the identity rotation and zero translation in the GTE,
/// so a batch of view-space vertices pays for that load once (see the
/// blended-chunk flush in `world_pass_model.rs`).
fn project_gte_view_vertex_identity_loaded(
    view: ViewVertex,
    projection: WorldProjection,
) -> ProjectedVertex {
    let (Ok(x), Ok(y), Ok(z)) = (
        i16::try_from(view.x),
        i16::try_from(view.y),
        i16::try_from(view.z),
    ) else {
        return cpu_project_gte_view(view, projection)
            .unwrap_or_else(|| ProjectedVertex::new(0, 0, projection.near_z - 1));
    };
    // Scheduled RTPS wrapper: same op, shared load-delay slots. The
    // compact macro wrapper measured substantially slower on the
    // 161-blended-vertex player path (see docs/perf-30fps.md).
    let p = scene::project_vertex_scheduled(Vec3I16::new(x, y, z));
    if (p.sz as i32) >= projection.near_z {
        ProjectedVertex::new(p.sx, p.sy, p.sz as i32)
    } else {
        ProjectedVertex::new(0, 0, projection.near_z - 1)
    }
}

/// Number of deferred CPU-blend vertices flushed per batch by
/// [`flush_blended_model_vertex_chunk`]. Sized so the stack scratch stays
/// small while still amortizing GTE matrix reloads across a seam cluster.
#[cfg(not(feature = "vert-debug"))]
const BLENDED_VERTEX_CHUNK: usize = 32;

/// Project a chunk of deferred CPU-blend model vertices in joint-grouped
/// phases.
///
/// The per-vertex slow path ([`project_blended_textured_model_vertex`])
/// swaps the GTE rotation/translation three times per vertex: secondary
/// joint, identity for the RTPS wrapper, primary restore. Blended
/// vertices cluster at joint seams, so deferring them into a chunk
/// amortizes those control-register loads: phase 1 transforms every
/// chunk vertex while the caller's primary joint is still loaded, phase
/// 2 reloads a secondary joint only when the seam's `joint1` changes and
/// lerps in place, phase 3 projects the whole chunk under one
/// identity/zero load, and phase 4 restores the caller's primary state.
/// The per-vertex arithmetic is identical to the slow path, so frames
/// stay bit-exact.
///
/// Caller guarantees `chunk_ptr` addresses at least `chunk_len` initialized
/// `u16`s, every referenced index is in range for `vertices` and
/// `projected_vertices`, belongs to the part whose joint is `primary`, and
/// passed [`model_vertex_uses_cpu_blend`].
#[cfg(not(feature = "vert-debug"))]
#[allow(clippy::too_many_arguments)]
unsafe fn flush_blended_model_vertex_chunk(
    chunk_ptr: *const u16,
    chunk_len: usize,
    vertices: &[ModelVertex],
    primary: JointViewTransform,
    joint_view_transforms: &[JointViewTransform],
    projection: WorldProjection,
    near_z: i32,
    projected_vertices: &mut [ProjectedVertex],
    all_in_front: &mut bool,
    all_inside_hw_bounds: &mut bool,
    min_x: &mut i16,
    max_x: &mut i16,
    min_y: &mut i16,
    max_y: &mut i16,
) {
    let chunk_len = chunk_len.min(BLENDED_VERTEX_CHUNK);
    let mut view_blend = [ViewVertex::ZERO; BLENDED_VERTEX_CHUNK];
    // Phase 1: primary-joint transforms while the caller's state is live.
    let mut slot = 0usize;
    while slot < chunk_len {
        // SAFETY: the caller guarantees every chunk index is valid for the
        // model vertex and projected-vertex slices; `slot < chunk_len` and
        // the local array is capped to `BLENDED_VERTEX_CHUNK` above.
        let vertex_index = unsafe { *chunk_ptr.add(slot) as usize };
        let vertex = unsafe { *vertices.get_unchecked(vertex_index) };
        let a = scene::transform_vertex_scheduled(vertex.position);
        unsafe {
            *view_blend.get_unchecked_mut(slot) = ViewVertex::new(a.x, a.y, a.z);
        }
        slot += 1;
    }
    // Phase 2: secondary transforms, reloading only on joint1 change.
    let mut loaded_joint1 = u16::MAX;
    slot = 0;
    while slot < chunk_len {
        let vertex_index = unsafe { *chunk_ptr.add(slot) as usize };
        let vertex = unsafe { *vertices.get_unchecked(vertex_index) };
        if u16::from(vertex.joint1) != loaded_joint1 {
            let secondary = unsafe { *joint_view_transforms.get_unchecked(vertex.joint1 as usize) };
            scene::load_rotation(&secondary.rotation);
            scene::load_translation(secondary.translation);
            loaded_joint1 = u16::from(vertex.joint1);
        }
        let b = scene::transform_vertex_scheduled(vertex.position);
        unsafe {
            let blend = view_blend.get_unchecked_mut(slot);
            *blend = lerp_view_vertex(*blend, ViewVertex::new(b.x, b.y, b.z), vertex.blend);
        }
        slot += 1;
    }
    // Phase 3: one identity/zero load projects the whole chunk.
    scene::load_rotation(&Mat3I16::IDENTITY);
    scene::load_translation(Vec3I32::ZERO);
    slot = 0;
    while slot < chunk_len {
        let vertex_index = unsafe { *chunk_ptr.add(slot) as usize };
        let projected = project_gte_view_vertex_identity_loaded(
            unsafe { *view_blend.get_unchecked(slot) },
            projection,
        );
        *all_in_front &= projected_model_vertex_in_front(projected, near_z);
        *all_inside_hw_bounds &= projected_model_vertex_inside_hw_bounds(projected);
        track_projected_model_bounds(projected, min_x, max_x, min_y, max_y);
        unsafe {
            *projected_vertices.get_unchecked_mut(vertex_index) = projected;
        }
        slot += 1;
    }
    // Phase 4: restore the caller's primary joint state.
    scene::load_rotation(&primary.rotation);
    scene::load_translation(primary.translation);
}

/// 256-step linear blend between two view-space positions.
///
/// `t` is the cooked blend byte: `0` returns `a` exactly, `255` returns
/// (255 a + 1 b) / 256 -- close enough to `b` for skin-deform purposes
/// and avoids the expensive divide-by-255 a true unit lerp would cost.
#[inline]
fn lerp_view_vertex(a: ViewVertex, b: ViewVertex, t: u8) -> ViewVertex {
    let t = t as i32;
    ViewVertex::new(
        a.x + (((b.x - a.x) * t) >> 8),
        a.y + (((b.y - a.y) * t) >> 8),
        a.z + (((b.z - a.z) * t) >> 8),
    )
}

fn back_facing(verts: [ProjectedLit; 3]) -> bool {
    projected_back_facing([
        ProjectedVertex::from(verts[0]),
        ProjectedVertex::from(verts[1]),
        ProjectedVertex::from(verts[2]),
    ])
}

fn projected_back_facing(verts: [ProjectedVertex; 3]) -> bool {
    let ax = (verts[1].sx as i32) - (verts[0].sx as i32);
    let ay = (verts[1].sy as i32) - (verts[0].sy as i32);
    let bx = (verts[2].sx as i32) - (verts[0].sx as i32);
    let by = (verts[2].sy as i32) - (verts[0].sy as i32);
    (ax * by - ay * bx) <= 0
}

fn projected_culled(verts: [ProjectedVertex; 3], cull_mode: CullMode) -> bool {
    match cull_mode {
        CullMode::None => false,
        CullMode::Back => projected_back_facing(verts),
        CullMode::Front => !projected_back_facing(verts),
    }
}

fn clamp_projected_textured_vertex(vertex: ProjectedTexturedVertex) -> ProjectedTexturedVertex {
    ProjectedTexturedVertex::new(
        ProjectedVertex::new(
            clamp_i16_range(vertex.projected.sx, PSX_VERTEX_MIN, PSX_VERTEX_MAX),
            clamp_i16_range(vertex.projected.sy, PSX_VERTEX_MIN, PSX_VERTEX_MAX),
            vertex.projected.sz,
        ),
        vertex.u,
        vertex.v,
    )
}

fn clamp_projected_textured_gouraud_vertex(
    vertex: ProjectedTexturedGouraudVertex,
) -> ProjectedTexturedGouraudVertex {
    ProjectedTexturedGouraudVertex::new(
        ProjectedVertex::new(
            clamp_i16_range(vertex.projected.sx, PSX_VERTEX_MIN, PSX_VERTEX_MAX),
            clamp_i16_range(vertex.projected.sy, PSX_VERTEX_MIN, PSX_VERTEX_MAX),
            vertex.projected.sz,
        ),
        vertex.u,
        vertex.v,
        vertex.color,
    )
}

fn projected_textured_exceeds_hw_extent(verts: [ProjectedTexturedVertex; 3]) -> bool {
    projected_edge_exceeds_hw_extent(verts[0], verts[1])
        || projected_edge_exceeds_hw_extent(verts[1], verts[2])
        || projected_edge_exceeds_hw_extent(verts[2], verts[0])
}

fn projected_textured_needs_split(
    verts: [ProjectedTexturedVertex; 3],
    options: WorldSurfaceOptions,
) -> bool {
    projected_textured_exceeds_hw_extent(verts)
        || projected_textured_exceeds_quality_extent(verts, options.textured_split_max_edge)
}

fn projected_textured_exceeds_quality_extent(
    verts: [ProjectedTexturedVertex; 3],
    max_edge: u16,
) -> bool {
    if max_edge == 0 {
        return false;
    }
    projected_edge_split_score(verts[0], verts[1]) > max_edge as i32
        || projected_edge_split_score(verts[1], verts[2]) > max_edge as i32
        || projected_edge_split_score(verts[2], verts[0]) > max_edge as i32
}

fn projected_textured_gouraud_exceeds_hw_extent(
    verts: [ProjectedTexturedGouraudVertex; 3],
) -> bool {
    projected_edge_exceeds_hw_extent(verts[0].textured(), verts[1].textured())
        || projected_edge_exceeds_hw_extent(verts[1].textured(), verts[2].textured())
        || projected_edge_exceeds_hw_extent(verts[2].textured(), verts[0].textured())
}

fn projected_textured_gouraud_needs_split(
    verts: [ProjectedTexturedGouraudVertex; 3],
    options: WorldSurfaceOptions,
) -> bool {
    projected_textured_gouraud_exceeds_hw_extent(verts)
        || projected_textured_exceeds_quality_extent(
            [
                verts[0].textured(),
                verts[1].textured(),
                verts[2].textured(),
            ],
            options.textured_split_max_edge,
        )
}

fn projected_triangle_hw_safe(verts: [ProjectedVertex; 3]) -> bool {
    let min_x = verts[0].sx.min(verts[1].sx).min(verts[2].sx);
    let max_x = verts[0].sx.max(verts[1].sx).max(verts[2].sx);
    let min_y = verts[0].sy.min(verts[1].sy).min(verts[2].sy);
    let max_y = verts[0].sy.max(verts[1].sy).max(verts[2].sy);
    min_x >= PSX_VERTEX_MIN
        && max_x <= PSX_VERTEX_MAX
        && min_y >= PSX_VERTEX_MIN
        && max_y <= PSX_VERTEX_MAX
        && ((max_x as i32) - (min_x as i32)) <= PSX_TRI_MAX_DX
        && ((max_y as i32) - (min_y as i32)) <= PSX_TRI_MAX_DY
}

fn projected_triangle_can_skip_split(
    verts: [ProjectedVertex; 3],
    options: WorldSurfaceOptions,
) -> bool {
    options.split_textured_triangles
        && projected_triangle_hw_safe(verts)
        && !projected_vertices_exceed_quality_extent(verts, options.textured_split_max_edge)
}

#[inline(always)]
fn projected_quad_bounds_hw_extent_safe(verts: &[ProjectedVertex; 4]) -> bool {
    let min_x = verts[0]
        .sx
        .min(verts[1].sx)
        .min(verts[2].sx)
        .min(verts[3].sx);
    let max_x = verts[0]
        .sx
        .max(verts[1].sx)
        .max(verts[2].sx)
        .max(verts[3].sx);
    let min_y = verts[0]
        .sy
        .min(verts[1].sy)
        .min(verts[2].sy)
        .min(verts[3].sy);
    let max_y = verts[0]
        .sy
        .max(verts[1].sy)
        .max(verts[2].sy)
        .max(verts[3].sy);
    min_x >= PSX_VERTEX_MIN
        && max_x <= PSX_VERTEX_MAX
        && min_y >= PSX_VERTEX_MIN
        && max_y <= PSX_VERTEX_MAX
        && projected_model_bounds_hw_extent_safe(min_x, max_x, min_y, max_y)
}

fn projected_vertices_exceed_quality_extent(verts: [ProjectedVertex; 3], max_edge: u16) -> bool {
    if max_edge == 0 {
        return false;
    }
    projected_vertex_edge_split_score(verts[0], verts[1]) > max_edge as i32
        || projected_vertex_edge_split_score(verts[1], verts[2]) > max_edge as i32
        || projected_vertex_edge_split_score(verts[2], verts[0]) > max_edge as i32
}

fn projected_vertex_edge_split_score(a: ProjectedVertex, b: ProjectedVertex) -> i32 {
    let dx = ((a.sx as i32) - (b.sx as i32)).abs();
    let dy = ((a.sy as i32) - (b.sy as i32)).abs();
    dx.max(dy.saturating_mul(2))
}

#[inline(always)]
fn projected_triangle_preclamped_hw_extent_safe(verts: [ProjectedVertex; 3]) -> bool {
    let min_x = verts[0].sx.min(verts[1].sx).min(verts[2].sx);
    let max_x = verts[0].sx.max(verts[1].sx).max(verts[2].sx);
    let min_y = verts[0].sy.min(verts[1].sy).min(verts[2].sy);
    let max_y = verts[0].sy.max(verts[1].sy).max(verts[2].sy);
    ((max_x as i32) - (min_x as i32)) <= PSX_TRI_MAX_DX
        && ((max_y as i32) - (min_y as i32)) <= PSX_TRI_MAX_DY
}

fn clamp_projected_vertex(vertex: ProjectedVertex) -> ProjectedVertex {
    ProjectedVertex::new(
        clamp_i16_range(vertex.sx, PSX_VERTEX_MIN, PSX_VERTEX_MAX),
        clamp_i16_range(vertex.sy, PSX_VERTEX_MIN, PSX_VERTEX_MAX),
        vertex.sz,
    )
}

fn projected_edge_exceeds_hw_extent(
    a: ProjectedTexturedVertex,
    b: ProjectedTexturedVertex,
) -> bool {
    let dx = ((a.projected.sx as i32) - (b.projected.sx as i32)).abs();
    let dy = ((a.projected.sy as i32) - (b.projected.sy as i32)).abs();
    dx > PSX_TRI_MAX_DX || dy > PSX_TRI_MAX_DY
}

fn largest_projected_edge(verts: [ProjectedTexturedVertex; 3]) -> usize {
    let mut edge = 0;
    let mut score = projected_edge_split_score(verts[0], verts[1]);
    let score_1 = projected_edge_split_score(verts[1], verts[2]);
    if score_1 > score {
        edge = 1;
        score = score_1;
    }
    let score_2 = projected_edge_split_score(verts[2], verts[0]);
    if score_2 > score {
        edge = 2;
    }
    edge
}

fn largest_projected_gouraud_edge(verts: [ProjectedTexturedGouraudVertex; 3]) -> usize {
    let textured = [
        verts[0].textured(),
        verts[1].textured(),
        verts[2].textured(),
    ];
    largest_projected_edge(textured)
}

#[inline(always)]
fn projected_edge_split_score(a: ProjectedTexturedVertex, b: ProjectedTexturedVertex) -> i32 {
    let dx = ((a.projected.sx as i32) - (b.projected.sx as i32)).abs();
    let dy = ((a.projected.sy as i32) - (b.projected.sy as i32)).abs();
    dx.max(dy.saturating_mul(2))
}

fn midpoint_projected_textured(
    a: ProjectedTexturedVertex,
    b: ProjectedTexturedVertex,
) -> ProjectedTexturedVertex {
    ProjectedTexturedVertex::new(
        ProjectedVertex::new(
            midpoint_i16(a.projected.sx, b.projected.sx),
            midpoint_i16(a.projected.sy, b.projected.sy),
            midpoint_i32(a.projected.sz, b.projected.sz),
        ),
        midpoint_i32(a.u, b.u),
        midpoint_i32(a.v, b.v),
    )
}

fn midpoint_projected_textured_gouraud(
    a: ProjectedTexturedGouraudVertex,
    b: ProjectedTexturedGouraudVertex,
) -> ProjectedTexturedGouraudVertex {
    ProjectedTexturedGouraudVertex::new(
        midpoint_projected_textured(a.textured(), b.textured()).projected,
        midpoint_i32(a.u, b.u),
        midpoint_i32(a.v, b.v),
        (
            midpoint_u8(a.color.0, b.color.0),
            midpoint_u8(a.color.1, b.color.1),
            midpoint_u8(a.color.2, b.color.2),
        ),
    )
}

fn midpoint_textured_gouraud_view(
    a: TexturedGouraudViewVertex,
    b: TexturedGouraudViewVertex,
) -> TexturedGouraudViewVertex {
    TexturedGouraudViewVertex {
        position: ViewVertex::new(
            midpoint_i32(a.position.x, b.position.x),
            midpoint_i32(a.position.y, b.position.y),
            midpoint_i32(a.position.z, b.position.z),
        ),
        u: midpoint_i32(a.u, b.u),
        v: midpoint_i32(a.v, b.v),
        color: (
            midpoint_u8(a.color.0, b.color.0),
            midpoint_u8(a.color.1, b.color.1),
            midpoint_u8(a.color.2, b.color.2),
        ),
    }
}

fn textured_gouraud_view_uv_word(vertex: TexturedGouraudViewVertex) -> u16 {
    (vertex.u.clamp(0, 255) as u16) | ((vertex.v.clamp(0, 255) as u16) << 8)
}

fn adaptive_quad_farthest_depth(vertices: &[TexturedGouraudViewVertex; 4]) -> i32 {
    vertices[0]
        .position
        .z
        .max(vertices[1].position.z)
        .max(vertices[2].position.z)
        .max(vertices[3].position.z)
}

fn adaptive_triangle_farthest_depth(vertices: &[TexturedGouraudViewVertex; 3]) -> i32 {
    vertices[0]
        .position
        .z
        .max(vertices[1].position.z)
        .max(vertices[2].position.z)
}

fn midpoint_i16(a: i16, b: i16) -> i16 {
    midpoint_i32(a as i32, b as i32) as i16
}

fn midpoint_i32(a: i32, b: i32) -> i32 {
    a + (b - a) / 2
}

fn midpoint_u8(a: u8, b: u8) -> u8 {
    (((a as u16) + (b as u16)) / 2) as u8
}

fn clip_textured_triangle_to_near(
    verts: [TexturedViewVertex; 3],
    near_z: i32,
    out: &mut [TexturedViewVertex; 4],
) -> usize {
    let mut count = 0;
    let mut prev = verts[2];
    let mut prev_inside = prev.position.z >= near_z;
    let mut i = 0;
    while i < verts.len() {
        let current = verts[i];
        let current_inside = current.position.z >= near_z;
        if current_inside != prev_inside {
            out[count] = intersect_textured_near(prev, current, near_z);
            count += 1;
        }
        if current_inside {
            out[count] = current;
            count += 1;
        }
        prev = current;
        prev_inside = current_inside;
        i += 1;
    }
    count
}

fn clip_textured_gouraud_triangle_to_near(
    verts: [TexturedGouraudViewVertex; 3],
    near_z: i32,
    out: &mut [TexturedGouraudViewVertex; 4],
) -> usize {
    let mut count = 0;
    let mut prev = verts[2];
    let mut prev_inside = prev.position.z >= near_z;
    let mut i = 0;
    while i < verts.len() {
        let current = verts[i];
        let current_inside = current.position.z >= near_z;
        if current_inside != prev_inside {
            out[count] = intersect_textured_gouraud_near(prev, current, near_z);
            count += 1;
        }
        if current_inside {
            out[count] = current;
            count += 1;
        }
        prev = current;
        prev_inside = current_inside;
        i += 1;
    }
    count
}

fn intersect_textured_gouraud_near(
    a: TexturedGouraudViewVertex,
    b: TexturedGouraudViewVertex,
    near_z: i32,
) -> TexturedGouraudViewVertex {
    let dz = b.position.z - a.position.z;
    if dz == 0 {
        return TexturedGouraudViewVertex {
            position: ViewVertex::new(a.position.x, a.position.y, near_z),
            ..a
        };
    }
    let num = near_z - a.position.z;
    TexturedGouraudViewVertex {
        position: ViewVertex::new(
            lerp_i32(a.position.x, b.position.x, num, dz),
            lerp_i32(a.position.y, b.position.y, num, dz),
            near_z,
        ),
        u: lerp_i32(a.u, b.u, num, dz),
        v: lerp_i32(a.v, b.v, num, dz),
        color: (
            lerp_u8(a.color.0, b.color.0, num, dz),
            lerp_u8(a.color.1, b.color.1, num, dz),
            lerp_u8(a.color.2, b.color.2, num, dz),
        ),
    }
}

fn intersect_textured_near(
    a: TexturedViewVertex,
    b: TexturedViewVertex,
    near_z: i32,
) -> TexturedViewVertex {
    let dz = b.position.z - a.position.z;
    if dz == 0 {
        return TexturedViewVertex::new(
            ViewVertex::new(a.position.x, a.position.y, near_z),
            a.u,
            a.v,
        );
    }

    let num = near_z - a.position.z;
    TexturedViewVertex::new(
        ViewVertex::new(
            lerp_i32(a.position.x, b.position.x, num, dz),
            lerp_i32(a.position.y, b.position.y, num, dz),
            near_z,
        ),
        lerp_i32(a.u, b.u, num, dz),
        lerp_i32(a.v, b.v, num, dz),
    )
}

fn lerp_i32(a: i32, b: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        return a;
    }
    a.saturating_add(b.saturating_sub(a).saturating_mul(numerator) / denominator)
}

fn lerp_u8(a: u8, b: u8, numerator: i32, denominator: i32) -> u8 {
    lerp_i32(a as i32, b as i32, numerator, denominator).clamp(0, 255) as u8
}

fn merge_world_stats(stats: &mut WorldRenderStats, next: WorldRenderStats) {
    stats.submitted_triangles = stats
        .submitted_triangles
        .wrapping_add(next.submitted_triangles);
    stats.culled_triangles = stats.culled_triangles.wrapping_add(next.culled_triangles);
    stats.clipped_triangles = stats.clipped_triangles.wrapping_add(next.clipped_triangles);
    stats.split_triangles = stats.split_triangles.wrapping_add(next.split_triangles);
    stats.dropped_triangles = stats.dropped_triangles.wrapping_add(next.dropped_triangles);
    stats.primitive_overflow |= next.primitive_overflow;
    stats.command_overflow |= next.command_overflow;
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn flush_packed_unclamped_model_batch_stats(
    stats: &mut TexturedModelRenderStats,
    skipped_triangles: u16,
    packed_face_calls: u16,
    packed_unclamped_face_calls: u16,
    culled_triangles: u16,
    submitted_triangles: u16,
    fast_submitted_triangles: u16,
    hw_extent_fallbacks: u16,
) {
    stats.skipped_triangles = stats.skipped_triangles.wrapping_add(skipped_triangles);
    stats.packed_face_calls = stats.packed_face_calls.wrapping_add(packed_face_calls);
    stats.packed_unclamped_face_calls = stats
        .packed_unclamped_face_calls
        .wrapping_add(packed_unclamped_face_calls);
    stats.culled_triangles = stats.culled_triangles.wrapping_add(culled_triangles);
    stats.submitted_triangles = stats.submitted_triangles.wrapping_add(submitted_triangles);
    stats.fast_submitted_triangles = stats
        .fast_submitted_triangles
        .wrapping_add(fast_submitted_triangles);
    stats.hw_extent_fallbacks = stats.hw_extent_fallbacks.wrapping_add(hw_extent_fallbacks);
}

fn merge_textured_model_stats(stats: &mut TexturedModelRenderStats, next: WorldRenderStats) {
    stats.submitted_triangles = stats
        .submitted_triangles
        .wrapping_add(next.submitted_triangles);
    stats.culled_triangles = stats.culled_triangles.wrapping_add(next.culled_triangles);
    stats.split_triangles = stats.split_triangles.wrapping_add(next.split_triangles);
    stats.dropped_triangles = stats.dropped_triangles.wrapping_add(next.dropped_triangles);
    stats.primitive_overflow |= next.primitive_overflow;
    stats.command_overflow |= next.command_overflow;
}

fn emit_textured_model_detail_counters(
    joint_count: usize,
    part_count: u16,
    project_count: usize,
    faces_considered: u32,
    blend_vertices: bool,
    all_projected_vertices_in_front: bool,
    all_projected_vertices_inside_hw_bounds: bool,
    packed_average_unclamped_faces: bool,
    packed_back_in_front_faces: bool,
    packed_fast_faces: bool,
    stats: &TexturedModelRenderStats,
) {
    crate::telemetry::counter(
        crate::telemetry::counter::TEXTURED_MODEL_JOINTS,
        joint_count as u32,
    );
    crate::telemetry::counter(
        crate::telemetry::counter::TEXTURED_MODEL_PARTS,
        part_count as u32,
    );
    crate::telemetry::counter(
        crate::telemetry::counter::TEXTURED_MODEL_VERTICES,
        project_count as u32,
    );
    crate::telemetry::counter(
        crate::telemetry::counter::TEXTURED_MODEL_FACES,
        faces_considered,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_CPU_BLEND_VERTICES,
        stats.cpu_blended_vertices as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_FACE_CALLS,
        stats.packed_face_calls as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_UNCLAMPED_CALLS,
        stats.packed_unclamped_face_calls as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_CLAMPED_CALLS,
        stats.packed_clamped_face_calls as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_GENERAL_CALLS,
        stats.packed_general_face_calls as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_FALLBACK_FACE_CALLS,
        stats.fallback_face_calls as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_HW_EXTENT_FALLBACKS,
        stats.hw_extent_fallbacks as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_NEAR_DROPS,
        stats.near_plane_dropped_faces as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_HW_UNSAFE_DROPS,
        stats.hw_unsafe_dropped_faces as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_FAST_SUBMITTED_TRIS,
        stats.fast_submitted_triangles as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_SPLIT_TRIS,
        stats.split_triangles as u32,
    );
    emit_nonzero_counter(
        crate::telemetry::counter::TEXTURED_MODEL_SKIPPED_TRIS,
        stats.skipped_triangles as u32,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_CPU_BLEND_SUBMITS,
        blend_vertices,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PRIMARY_JOINT_SUBMITS,
        !blend_vertices,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_ALL_FRONT_SUBMITS,
        all_projected_vertices_in_front,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_ALL_HW_BOUNDS_SUBMITS,
        all_projected_vertices_inside_hw_bounds,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_UNCLAMPED_ELIGIBLE_SUBMITS,
        packed_average_unclamped_faces,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_CLAMPED_ELIGIBLE_SUBMITS,
        packed_back_in_front_faces && !packed_average_unclamped_faces,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PACKED_GENERAL_ELIGIBLE_SUBMITS,
        packed_fast_faces && !packed_back_in_front_faces && !packed_average_unclamped_faces,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_VERTEX_OVERFLOW_SUBMITS,
        stats.vertex_overflow,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_PRIMITIVE_OVERFLOW_SUBMITS,
        stats.primitive_overflow,
    );
    emit_bool_counter(
        crate::telemetry::counter::TEXTURED_MODEL_COMMAND_OVERFLOW_SUBMITS,
        stats.command_overflow,
    );
}

fn emit_nonzero_counter(counter_id: u16, value: u32) {
    if value != 0 {
        crate::telemetry::counter(counter_id, value);
    }
}

fn emit_bool_counter(counter_id: u16, value: bool) {
    if value {
        crate::telemetry::counter(counter_id, 1);
    }
}

fn clamp_i16(value: i32) -> i16 {
    if value < i16::MIN as i32 {
        i16::MIN
    } else if value > i16::MAX as i32 {
        i16::MAX
    } else {
        value as i16
    }
}

fn clamp_i16_range(value: i16, min: i16, max: i16) -> i16 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

fn clamp_u8(value: i32) -> u8 {
    if value < 0 {
        0
    } else if value > u8::MAX as i32 {
        u8::MAX
    } else {
        value as u8
    }
}

fn clamp_u16_i32(value: i32) -> u16 {
    if value < 0 {
        0
    } else if value > u16::MAX as i32 {
        u16::MAX
    } else {
        value as u16
    }
}

fn isqrt_i32(value: i32) -> i32 {
    if value <= 0 {
        return 0;
    }

    let mut bit = 1 << 30;
    let mut n = value;
    let mut root = 0;
    while bit > n {
        bit >>= 2;
    }
    while bit != 0 {
        if n >= root + bit {
            n -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

#[cfg(test)]
fn sort_for_ot_insert(commands: &mut [GouraudTriCommand]) {
    let mut gap = commands.len() / 2;
    while gap > 0 {
        let mut i = gap;
        while i < commands.len() {
            let command = commands[i];
            let mut j = i;
            while j >= gap && should_insert_after(commands[j - gap], command) {
                commands[j] = commands[j - gap];
                j -= gap;
            }
            commands[j] = command;
            i += 1;
        }
        gap /= 2;
    }
}

#[cfg(test)]
fn should_insert_after(a: GouraudTriCommand, b: GouraudTriCommand) -> bool {
    if a.slot.index() != b.slot.index() {
        return a.slot.index() > b.slot.index();
    }
    if a.depth != b.depth {
        return a.depth > b.depth;
    }
    // OT insertion prepends packets. For exact ties, insert later
    // primitive indices first so the eventual DMA walk preserves the
    // source order for those equal-depth packets.
    a.primitive_index < b.primitive_index
}

fn should_insert_gouraud_before(a: GouraudTriCommand, b: GouraudTriCommand) -> bool {
    if a.depth != b.depth {
        return a.depth < b.depth;
    }
    a.primitive_index > b.primitive_index
}

fn sort_world_for_ot_insert(commands: &mut [WorldTriCommand]) {
    let mut gap = commands.len() / 2;
    while gap > 0 {
        let mut i = gap;
        while i < commands.len() {
            let command = commands[i];
            let mut j = i;
            while j >= gap && should_insert_world_after(commands[j - gap], command) {
                commands[j] = commands[j - gap];
                j -= gap;
            }
            commands[j] = command;
            i += 1;
        }
        gap /= 2;
    }
}

fn should_insert_world_after(a: WorldTriCommand, b: WorldTriCommand) -> bool {
    if a.slot != b.slot {
        return a.slot > b.slot;
    }
    if a.depth != b.depth {
        return a.depth > b.depth;
    }
    if a.render_layer != b.render_layer {
        return a.render_layer == world_render_layer_code(WorldRenderLayer::Opaque)
            && b.render_layer == world_render_layer_code(WorldRenderLayer::Transparent);
    }
    a.order < b.order
}

fn should_insert_world_before(a: WorldTriCommand, b: WorldTriCommand) -> bool {
    if a.depth != b.depth {
        return a.depth < b.depth;
    }
    if a.render_layer != b.render_layer {
        return a.render_layer == world_render_layer_code(WorldRenderLayer::Transparent)
            && b.render_layer == world_render_layer_code(WorldRenderLayer::Opaque);
    }
    a.order > b.order
}

#[cfg(test)]
mod tests;
