//! Depth-banded affine surface submission for brush worlds.
//!
//! The PS1 GPU interpolates texture coordinates linearly in screen space, so a
//! large polygon close to the camera visibly swims as the view turns. This
//! module splits such polygons in camera space, before projection, so every
//! piece that reaches the GPU spans little enough depth for the affine error
//! to stay small.
//!
//! # How a surface is split
//!
//! A surface is a convex fan of [`AffineVertex`] records in a caller-owned
//! batch. Its vertices are projected once; the fan's triangles are then
//! refined edge by edge. An edge is halved when the mean depth of its two
//! end points, in ordering-table units, is nearer than the profile's band
//! for the current level ([`SurfaceProfile::split_once_below`], then
//! [`SurfaceProfile::split_twice_below`]). The decision depends only on the
//! edge's own end points and level, never on the triangle it belongs to, so
//! two triangles sharing an edge always split it identically and place the
//! same midpoint. The refinement is therefore conforming: no vertex of one
//! piece lies on the open edge of another, which is what lets the GPU
//! rasterise the pieces without pinholes and without any sealing geometry.
//!
//! Depending on which of its three edges split, a triangle becomes two,
//! three or four children (one split edge: a bisection; two: a corner plus a
//! quadrilateral cut on a diagonal; three: three corners and a centre). At
//! most two levels are applied, so one source triangle yields at most twelve
//! GPU packets and needs at most six extra vertices at a time. Neighbouring
//! leaf triangles that form a parallelogram are sent as one GP0 quad, which
//! the GPU rasterises as the triangles (v0, v1, v2) and (v1, v2, v3).
//!
//! Midpoints average position, UV and each colour channel of their end
//! points and are projected through the loaded GTE view like any other
//! vertex, so they land on the true perspective image of the 3D edge.
//!
//! # Packets
//!
//! Every piece is keyed into the ordering table by the GTE's average depth
//! (AVSZ3 or AVSZ4 with the caller's ZSF weights) and dropped when that key
//! is zero or past the table. A piece wholly beyond one screen edge is
//! dropped before it is written. Compact (page-local) surfaces emit a
//! full-window GP0(E2) selector before each GP0(34h/3Ch) textured Gouraud
//! polygon, because other ordering-table packets (character models) can leave
//! a nonzero window active between world polygons; windowed surfaces wrap each
//! polygon in their own GP0(E2) texture-window selector and a full-window
//! reset, and mark the tag with `WINDOWED_POLYGON_TAG`. Tags carry the packet's data-word
//! count in bits 24..31 and its ordering-table slot in bits 0..15, ready for
//! the tagged-stream linker.

use psx_gpu::material::TextureWindow;
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene;

/// Staged-tag bit marking a polygon wrapped in its own texture-window
/// selector. It is this module's marker alone: tagged-stream insertion keeps
/// only the word count and the slot from a staged tag and ignores it.
const WINDOWED_POLYGON_TAG: u32 = 1 << 16;

/// Extra vertex slots the caller reserves after a batch for midpoints.
pub const AFFINE_SPLIT_SCRATCH_VERTICES: usize = 6;

/// Most GPU packets one source triangle can produce.
pub const AFFINE_PACKETS_PER_TRIANGLE: usize = 12;

/// Data words of a textured Gouraud triangle and quad, without any
/// texture-window state words.
const TRI_WORDS: u32 = 9;
const QUAD_WORDS: u32 = 12;

/// GP0 quad bit: GP0(34h) becomes GP0(3Ch).
const QUAD_COMMAND_BIT: u32 = 0x0800_0000;

/// Working vertex: a model- or world-space position for the loaded GTE view,
/// packet attributes, and the projection the submitter fills in.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AffineVertex {
    /// Position in the space the loaded GTE rotation and translation expect.
    pub position: [i16; 3],
    /// Packet UV bytes.
    pub uv: [u8; 2],
    /// RGB in the low 24 bits. The high byte is ignored by packet writes.
    pub color: u32,
    /// Projected screen coordinate, written by submission.
    pub screen: [i16; 2],
    /// Projected GTE depth (SZ), written by submission.
    pub depth: u32,
}

/// Cooked brush-world vertex: position, UV, and either two light-style
/// levels in its low bytes or a baked RGB word.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceSourceVertex {
    /// Model- or world-space position.
    pub position: [i16; 3],
    /// Material-relative or already baked atlas UV.
    pub uv: [u8; 2],
    /// Light-style levels in bytes 0 and 1, or baked RGB.
    pub light: u32,
}

/// One convex fan in a batch, and how its packets are shaped.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AffineSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan.
    pub vertex_count: u16,
    /// Texture-page word.
    pub tpage: u16,
    /// CLUT word.
    pub clut: u16,
    /// Wrapping UV offset applied to windowed packets only.
    pub uv_offset: [u8; 2],
    /// Non-zero selects page-local UVs with a GP0(E2) full-window prefix.
    pub compact: u8,
    /// Fully encoded GP0(E2) command for windowed packets.
    pub texture_window_word: u32,
    /// GP0 textured Gouraud triangle command in the high byte.
    pub color_command_word: u32,
}

/// Viewport, ordering-table size and split bands for one renderer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SurfaceProfile {
    /// Viewport width in pixels.
    pub screen_width: i16,
    /// Viewport height in pixels.
    pub screen_height: i16,
    /// Number of ordering-table slots.
    pub ot_depth: u16,
    /// Edges whose mean ordering depth is nearer than this split once.
    pub split_once_below: u16,
    /// Edges of the first-level pieces nearer than this split again. Must
    /// not exceed [`Self::split_once_below`].
    pub split_twice_below: u16,
}

impl SurfaceProfile {
    /// Brush-world profile for the third-person camera, 320x240 with a
    /// 2048-slot table. The camera looks down at large floor polygons from a
    /// few dozen units up, so the bands reach well out: measured on the
    /// Cortex whole-level tape, these keep textured warp low for a few
    /// percent of frame rate.
    pub const PXBSP_THIRD_PERSON: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        split_once_below: 340,
        split_twice_below: 170,
    };
}

/// Result of one batch submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SurfaceSubmit {
    /// First packet word after the emitted stream.
    pub next_packet: *mut u32,
    /// Number of GPU packets emitted.
    pub packets: u32,
    /// Number of hardware triangles those packets draw.
    pub hardware_triangles: u32,
}

/// Brightness of a vertex lit by two light styles: each style's level
/// (0..=255) scaled by that style's current weight (256 = full), summed,
/// rounded to the nearest step and limited to 255.
#[inline(always)]
fn style_level(light: u32, weights: [u16; 2]) -> u32 {
    let first = (light & 0xff) * u32::from(weights[0]);
    let second = ((light >> 8) & 0xff) * u32::from(weights[1]);
    ((first + second + 128) >> 8).min(255)
}

/// Expand cooked vertices into working vertices.
///
/// UVs get `uv_offset` added (wrapping) unless `baked_uv`. The colour is the
/// baked word when `baked_light`, otherwise the light-style brightness from
/// [`style_level`] on all three channels. Projection fields are left as they
/// are; submission overwrites them.
///
/// # Safety
/// `source` must hold `count` readable records and `destination` `count`
/// writable ones, both aligned, not overlapping.
pub unsafe fn materialize_surface_vertices(
    source: *const SurfaceSourceVertex,
    count: usize,
    destination: *mut AffineVertex,
    uv_offset: [u8; 2],
    style_weights: [u16; 2],
    baked_uv: bool,
    baked_light: bool,
) {
    for index in 0..count {
        // SAFETY: both ranges hold `count` records per the contract.
        let input = unsafe { &*source.add(index) };
        let output = unsafe { &mut *destination.add(index) };
        output.position = input.position;
        output.uv = if baked_uv {
            input.uv
        } else {
            [
                input.uv[0].wrapping_add(uv_offset[0]),
                input.uv[1].wrapping_add(uv_offset[1]),
            ]
        };
        output.color = if baked_light {
            input.light
        } else {
            let level = style_level(input.light, style_weights);
            level | (level << 8) | (level << 16)
        };
    }
}

/// [`materialize_surface_vertices`] for the common cooked form: offset UVs
/// and a baked colour word.
///
/// # Safety
/// As [`materialize_surface_vertices`].
pub unsafe fn materialize_baked_surface_vertices(
    source: *const SurfaceSourceVertex,
    count: usize,
    destination: *mut AffineVertex,
    uv_offset: [u8; 2],
) {
    for index in 0..count {
        // SAFETY: as above.
        let input = unsafe { &*source.add(index) };
        let output = unsafe { &mut *destination.add(index) };
        output.position = input.position;
        output.uv = [
            input.uv[0].wrapping_add(uv_offset[0]),
            input.uv[1].wrapping_add(uv_offset[1]),
        ];
        output.color = input.light;
    }
}

/// Compose a model-to-view transform from the view, a model rotation, a
/// model-space offset, a world origin and a per-axis Q12 scale.
///
/// `model_offset` and `world_origin` are integer world/model units. The
/// result equals rotating the offset, scaling the model matrix and composing
/// it with the view as separate operations, done on one GTE schedule.
pub fn compose_model_view_transform(
    view_rotation: Mat3I16,
    view_translation: Vec3I32,
    model_rotation: Mat3I16,
    model_offset: Vec3I16,
    world_origin: Vec3I32,
    scale: Vec3I16,
) -> (Mat3I16, Vec3I32) {
    scene::load_rotation(&model_rotation);
    scene::load_translation(Vec3I32::ZERO);
    let rotated_offset = scene::transform_vertex_scheduled(model_offset);
    let scaled = transform_matrix_columns([
        Vec3I16::new(scale.x, 0, 0),
        Vec3I16::new(0, scale.y, 0),
        Vec3I16::new(0, 0, scale.z),
    ]);

    scene::load_rotation(&view_rotation);
    scene::load_translation(Vec3I32::ZERO);
    let composed = transform_matrix_columns([
        Vec3I16::new(scaled.m[0][0], scaled.m[1][0], scaled.m[2][0]),
        Vec3I16::new(scaled.m[0][1], scaled.m[1][1], scaled.m[2][1]),
        Vec3I16::new(scaled.m[0][2], scaled.m[1][2], scaled.m[2][2]),
    ]);
    let model_translation = Vec3I16::new(
        rotated_offset.x.wrapping_add(world_origin.x) as i16,
        rotated_offset.y.wrapping_add(world_origin.y) as i16,
        rotated_offset.z.wrapping_add(world_origin.z) as i16,
    );
    let rotated_translation = scene::transform_vertex_scheduled(model_translation);
    let translation = Vec3I32::new(
        rotated_translation.x.wrapping_add(view_translation.x),
        rotated_translation.y.wrapping_add(view_translation.y),
        rotated_translation.z.wrapping_add(view_translation.z),
    );
    (composed, translation)
}

#[inline(always)]
fn transform_matrix_columns(columns: [Vec3I16; 3]) -> Mat3I16 {
    let c0 = scene::transform_vertex_scheduled(columns[0]);
    let c1 = scene::transform_vertex_scheduled(columns[1]);
    let c2 = scene::transform_vertex_scheduled(columns[2]);
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    Mat3I16 {
        m: [
            [clamp(c0.x), clamp(c1.x), clamp(c2.x)],
            [clamp(c0.y), clamp(c1.y), clamp(c2.y)],
            [clamp(c0.z), clamp(c1.z), clamp(c2.z)],
        ],
    }
}

#[inline(always)]
fn vec3(position: [i16; 3]) -> Vec3I16 {
    Vec3I16::new(position[0], position[1], position[2])
}

#[inline(always)]
fn store_projection(vertex: &mut AffineVertex, projected: scene::Projected) {
    vertex.screen = [projected.sx, projected.sy];
    vertex.depth = u32::from(projected.sz);
}

/// Project `count` consecutive vertices, three at a time where possible.
///
/// # Safety
/// `vertices` must hold `count` writable records.
unsafe fn project_vertices(vertices: *mut AffineVertex, count: usize) {
    let mut index = 0;
    while index + 3 <= count {
        // SAFETY: `index + 2 < count`.
        let (a, b, c) = unsafe {
            (
                &mut *vertices.add(index),
                &mut *vertices.add(index + 1),
                &mut *vertices.add(index + 2),
            )
        };
        let projected =
            scene::project_triangle_scheduled(vec3(a.position), vec3(b.position), vec3(c.position));
        store_projection(a, projected[0]);
        store_projection(b, projected[1]);
        store_projection(c, projected[2]);
        index += 3;
    }
    while index < count {
        // SAFETY: `index < count`.
        let vertex = unsafe { &mut *vertices.add(index) };
        store_projection(
            vertex,
            scene::project_vertex_scheduled(vec3(vertex.position)),
        );
        index += 1;
    }
}

/// [`project_vertices`], reporting which vertices the GTE could not place.
///
/// The GTE sets FLAG bit 17 when its perspective divide saturates
/// (`H >= 2 * SZ`) for any vertex of a command, so one `CFC2` per triple names
/// the exact condition. Bit `i` of the result is set for every vertex of a
/// triple (or the single vertex) that tripped it.
///
/// # Safety
/// As [`project_vertices`]; `count` is at most 32.
unsafe fn project_vertices_flagged(vertices: *mut AffineVertex, count: usize) -> u32 {
    const DIVIDE_OVERFLOW: u32 = 1 << 17;
    let mut flagged = 0u32;
    let mut index = 0;
    while index + 3 <= count {
        // SAFETY: `index + 2 < count`.
        let (a, b, c) = unsafe {
            (
                &mut *vertices.add(index),
                &mut *vertices.add(index + 1),
                &mut *vertices.add(index + 2),
            )
        };
        let projected =
            scene::project_triangle_scheduled(vec3(a.position), vec3(b.position), vec3(c.position));
        if scene::error_flags() & DIVIDE_OVERFLOW != 0 {
            flagged |= 0b111 << index;
        }
        store_projection(a, projected[0]);
        store_projection(b, projected[1]);
        store_projection(c, projected[2]);
        index += 3;
    }
    while index < count {
        // SAFETY: `index < count`.
        let vertex = unsafe { &mut *vertices.add(index) };
        let projected = scene::project_vertex_scheduled(vec3(vertex.position));
        if scene::error_flags() & DIVIDE_OVERFLOW != 0 {
            flagged |= 1 << index;
        }
        store_projection(vertex, projected);
        index += 1;
    }
    flagged
}

/// Midpoint of two working vertices, symmetric in its arguments: positions,
/// UVs and each colour channel are averaged (rounding down).
#[inline(always)]
fn midpoint(a: &AffineVertex, b: &AffineVertex) -> AffineVertex {
    let half = |x: i32, y: i32| ((x + y) >> 1) as i16;
    let half_u8 = |x: u8, y: u8| ((u16::from(x) + u16::from(y)) >> 1) as u8;
    let channel = |shift: u32| (((a.color >> shift) & 0xff) + ((b.color >> shift) & 0xff)) >> 1;
    AffineVertex {
        position: [
            half(i32::from(a.position[0]), i32::from(b.position[0])),
            half(i32::from(a.position[1]), i32::from(b.position[1])),
            half(i32::from(a.position[2]), i32::from(b.position[2])),
        ],
        uv: [half_u8(a.uv[0], b.uv[0]), half_u8(a.uv[1], b.uv[1])],
        color: channel(0) | (channel(8) << 8) | (channel(16) << 16),
        screen: [0, 0],
        depth: 0,
    }
}

#[inline(always)]
const fn screen_word(vertex: &AffineVertex) -> u32 {
    (vertex.screen[0] as u16 as u32) | ((vertex.screen[1] as u16 as u32) << 16)
}

/// The loaded GTE view, for projecting the vertices the GTE cannot.
///
/// The GTE's perspective divide saturates once `H >= 2 * SZ`: every vertex
/// that close to the camera plane lands at a screen position pulled toward
/// the screen centre, not where its perspective image is. A caller that
/// clips a surface to the screen (so every vertex projects inside it) passes
/// the loaded view to [`submit_surface_batch_near`], which reprojects such
/// vertices exactly.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NearView {
    /// Q12 rows of the loaded rotation: `view = (row . position >> 12) + translation`.
    pub rows: [[i16; 3]; 3],
    /// Loaded GTE translation.
    pub translation: [i32; 3],
    /// GTE `H` register.
    pub focal_length: i32,
    /// Screen offset (`OFX`, `OFY`) in whole pixels.
    pub center: [i32; 2],
}

/// Largest distance from the screen centre a reprojected vertex keeps
/// (`AffineVertex::screen` holds 16 bits, and the clip below keeps its products
/// inside 31).
const LIMIT: i32 = 15_000;

impl NearView {
    /// Whether the GTE divide saturates for a vertex of this projected depth.
    #[inline(always)]
    pub const fn saturates(&self, depth: u32) -> bool {
        self.focal_length >= 2 * depth as i32
    }

    /// Exact projection of `position`, or `None` when it has no positive
    /// depth to divide by. Matches the GTE (`sx = (OFX + IR1 * H / SZ) >> 16`
    /// with `IR1` saturated to 16 bits) wherever the GTE divide does not
    /// saturate and the result fits its 11-bit screen range.
    pub fn project(&self, position: [i16; 3]) -> Option<[i16; 2]> {
        // A Q12 row against `i16` positions stays inside `i32`, and so does
        // the product with the divisor once it is split at 16 bits: the PS1
        // has no 64-bit arithmetic to spare.
        let (x, y, z) = (
            i32::from(position[0]),
            i32::from(position[1]),
            i32::from(position[2]),
        );
        let dot = |row: &[i16; 3]| {
            (i32::from(row[0]) * x + i32::from(row[1]) * y + i32::from(row[2]) * z) >> 12
        };
        let depth = dot(&self.rows[2]) + self.translation[2];
        if depth <= 0 {
            return None;
        }
        let depth = depth.min(i32::from(u16::MAX)) as u32;
        // `H << 16 / depth`, rounded: `H` is a 16-bit register, so it fits.
        let divisor = (((self.focal_length as u32) << 16) + depth / 2) / depth;
        // `IR * divisor >> 16` as `IR * high + (IR * low >> 16)`, exact for
        // the floor shift, with no product past 31 bits (`high` is at most
        // `H` and `IR` fits 16 bits).
        let (high, low) = ((divisor >> 16) as i32, (divisor & 0xffff) as i32);
        let offset = |axis: usize| -> i32 {
            let ir = (dot(&self.rows[axis]) + self.translation[axis])
                .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
            ir * high + ((ir * low) >> 16)
        };
        // The GTE saturates a screen coordinate at 11 bits, which bends the
        // edges of a polygon that leaves the screen. Keep the exact offset
        // from the screen centre, and only scale a point beyond `LIMIT` back
        // along its ray from the centre, which keeps the direction of an edge
        // toward it.
        let (dx, dy) = (offset(0), offset(1));
        let far = dx.abs().max(dy.abs());
        let (dx, dy) = if far <= LIMIT {
            (dx, dy)
        } else {
            // Drop low bits until `component * LIMIT` fits 31 bits.
            let mut shift = 0;
            while (far >> shift) > 60_000 {
                shift += 1;
            }
            let far = far >> shift;
            ((dx >> shift) * LIMIT / far, (dy >> shift) * LIMIT / far)
        };
        Some([(self.center[0] + dx) as i16, (self.center[1] + dy) as i16])
    }
}

/// What a batch does to freshly projected vertices. `()` leaves the GTE's
/// answer alone and is zero-sized, so the legacy submission carries nothing.
trait Reproject: Copy {
    /// Whether polygons are clipped to the GPU's window before they are written.
    const CLIPS: bool;

    /// Whether surfaces with a vertex the GTE divide saturates on are left
    /// unwritten and reported to the caller.
    const SCREENS: bool = false;

    /// The end of the packet storage a clipped polygon may write up to.
    fn limit(self) -> *mut u32;

    /// # Safety
    /// `vertices` must hold `count` initialised records.
    unsafe fn fix(self, vertices: *mut AffineVertex, count: usize);
}

impl Reproject for () {
    const CLIPS: bool = false;

    #[inline(always)]
    fn limit(self) -> *mut u32 {
        core::ptr::null_mut()
    }

    #[inline(always)]
    unsafe fn fix(self, _vertices: *mut AffineVertex, _count: usize) {}
}

/// A near-surface batch: the view to reproject with, and where the packet
/// storage ends.
type NearBatch<'a> = (&'a NearView, *mut u32);

impl Reproject for NearBatch<'_> {
    const CLIPS: bool = true;

    #[inline(always)]
    fn limit(self) -> *mut u32 {
        self.1
    }

    #[inline(always)]
    unsafe fn fix(self, vertices: *mut AffineVertex, count: usize) {
        for index in 0..count {
            // SAFETY: `index < count`.
            let vertex = unsafe { &mut *vertices.add(index) };
            if self.0.saturates(vertex.depth) {
                if let Some(screen) = self.0.project(vertex.position) {
                    vertex.screen = screen;
                }
            }
        }
    }
}

/// The shared batch with surfaces the GTE cannot place left to the caller.
#[derive(Copy, Clone)]
struct Screened;

impl Reproject for Screened {
    const CLIPS: bool = false;
    const SCREENS: bool = true;

    #[inline(always)]
    fn limit(self) -> *mut u32 {
        core::ptr::null_mut()
    }

    #[inline(always)]
    unsafe fn fix(self, _vertices: *mut AffineVertex, _count: usize) {}
}

/// Packet writer for one batch.
struct PacketSink {
    next: *mut u32,
    packets: u32,
    triangles: u32,
    right: i16,
    bottom: i16,
    ot_depth: u16,
}

impl PacketSink {
    /// True when every point lies beyond the same screen edge.
    #[inline(always)]
    fn off_screen(&self, points: &[&AffineVertex]) -> bool {
        let mut left = true;
        let mut right = true;
        let mut above = true;
        let mut below = true;
        for point in points {
            left &= point.screen[0] < 0;
            right &= point.screen[0] > self.right;
            above &= point.screen[1] < 0;
            below &= point.screen[1] > self.bottom;
        }
        left || right || above || below
    }

    #[inline(always)]
    fn usable_slot(&self, otz: u16) -> bool {
        otz != 0 && otz < self.ot_depth
    }

    /// Write one polygon: `corners` are three or four vertices in GPU order.
    ///
    /// # Safety
    /// `self.next` must have room for the polygon's packet.
    #[inline(always)]
    unsafe fn polygon(&mut self, surface: &AffineSurface, corners: &[&AffineVertex]) {
        if self.off_screen(corners) {
            return;
        }
        let otz = Self::average_depth(corners);
        if !self.usable_slot(otz) {
            return;
        }
        // SAFETY: the caller reserved room for the polygon's packet.
        unsafe { self.write(surface, corners, otz) };
    }

    /// Ordering-table key of a polygon: the GTE's average of its depths.
    #[inline(always)]
    fn average_depth(corners: &[&AffineVertex]) -> u16 {
        if corners.len() == 4 {
            scene::average_cached_z4([
                corners[0].depth as u16,
                corners[1].depth as u16,
                corners[2].depth as u16,
                corners[3].depth as u16,
            ])
        } else {
            scene::average_cached_z3([
                corners[0].depth as u16,
                corners[1].depth as u16,
                corners[2].depth as u16,
            ])
        }
    }

    /// Write the packet of a polygon keyed at `otz`.
    ///
    /// # Safety
    /// `self.next` must have room for the polygon's packet.
    #[inline(always)]
    unsafe fn write(&mut self, surface: &AffineSurface, corners: &[&AffineVertex], otz: u16) {
        let quad = corners.len() == 4;
        let windowed = surface.compact == 0;
        let polygon_words = if quad { QUAD_WORDS } else { TRI_WORDS };
        let mut command = surface.color_command_word & 0xff00_0000;
        if quad {
            command |= QUAD_COMMAND_BIT;
        }
        let uv_offset = if windowed { surface.uv_offset } else { [0, 0] };
        let uv = |vertex: &AffineVertex| {
            u32::from(vertex.uv[0].wrapping_add(uv_offset[0]))
                | (u32::from(vertex.uv[1].wrapping_add(uv_offset[1])) << 8)
        };
        let mut word = self.next;
        let mut put = |value: u32| {
            // SAFETY: the caller reserved room for the whole packet.
            unsafe {
                word.write(value);
                word = word.add(1);
            }
        };
        if windowed {
            put(((polygon_words + 2) << 24) | WINDOWED_POLYGON_TAG | u32::from(otz));
            put(surface.texture_window_word);
        } else {
            // Reset a model's window in this packet's own DMA node: an
            // earlier reset in the world pass is not enough, because world
            // and model packets interleave by depth.
            put(((polygon_words + 1) << 24) | u32::from(otz));
            put(TextureWindow::NONE.word());
        }
        for (index, corner) in corners.iter().enumerate() {
            let color = corner.color & 0x00ff_ffff;
            put(if index == 0 { command | color } else { color });
            put(screen_word(corner));
            let attribute = match index {
                0 => u32::from(surface.clut) << 16,
                1 => u32::from(surface.tpage) << 16,
                _ => 0,
            };
            put(uv(corner) | attribute);
        }
        if windowed {
            put(TextureWindow::NONE.word());
        }
        self.next = word;
        self.packets += 1;
        self.triangles += if quad { 2 } else { 1 };
    }
}

/// The GPU drops a triangle spanning 1024 columns or 512 rows, and reads
/// vertices as 11-bit signed numbers. A clipped polygon stays inside this
/// window, which is as large as the GPU draws around the 320x240 screen less
/// the pixel the outward rounding of a cut may add on each side.
const WINDOW_X: (i32, i32) = (-350, 670);
const WINDOW_Y: (i32, i32) = (-134, 374);

/// A polygon corner with its attributes widened for clipping.
#[derive(Copy, Clone, Default)]
struct ClipCorner {
    x: i32,
    y: i32,
    u: i32,
    v: i32,
    color: [i32; 3],
}

impl ClipCorner {
    fn of(vertex: &AffineVertex) -> Self {
        Self {
            x: i32::from(vertex.screen[0]),
            y: i32::from(vertex.screen[1]),
            u: i32::from(vertex.uv[0]),
            v: i32::from(vertex.uv[1]),
            color: [
                (vertex.color & 0xff) as i32,
                ((vertex.color >> 8) & 0xff) as i32,
                ((vertex.color >> 16) & 0xff) as i32,
            ],
        }
    }

    fn vertex(&self) -> AffineVertex {
        AffineVertex {
            position: [0; 3],
            uv: [self.u as u8, self.v as u8],
            color: (self.color[0] as u32)
                | ((self.color[1] as u32) << 8)
                | ((self.color[2] as u32) << 16),
            screen: [self.x as i16, self.y as i16],
            depth: 0,
        }
    }
}

/// Twice the signed area of a triangle.
fn cross(a: &ClipCorner, b: &ClipCorner, c: &ClipCorner) -> i32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Whether the GPU draws a triangle with these screen corners as given.
fn gpu_draws(corners: [&AffineVertex; 3]) -> bool {
    let [a, b, c] = corners;
    let pairs = [(a, b), (b, c), (c, a)];
    pairs.iter().all(|(p, q)| {
        i32::from(p.screen[0]).abs_diff(i32::from(q.screen[0])) <= 1023
            && i32::from(p.screen[1]).abs_diff(i32::from(q.screen[1])) <= 511
    }) && corners.iter().all(|corner| {
        (-1024..=1023).contains(&i32::from(corner.screen[0]))
            && (-1024..=1023).contains(&i32::from(corner.screen[1]))
    })
}

/// Whether the GPU draws every triangle of a polygon with these corners: a
/// bounding box inside its 11-bit coordinates and under the 1024 by 512 span.
/// Sufficient for both halves of a quad; a failing quad is judged per triangle.
#[inline(always)]
fn within_gpu_limits(corners: &[&AffineVertex]) -> bool {
    let (mut left, mut right) = (i32::MAX, i32::MIN);
    let (mut top, mut bottom) = (i32::MAX, i32::MIN);
    for corner in corners {
        let (x, y) = (i32::from(corner.screen[0]), i32::from(corner.screen[1]));
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    left >= -1024
        && right <= 1023
        && top >= -1024
        && bottom <= 1023
        && right - left <= 1023
        && bottom - top <= 511
}

/// The corner where segment `from -> to` crosses the line `axis = bound`
/// (axis 0 is x, 1 is y). Attributes interpolate linearly in screen space,
/// as the GPU does along an edge. The crossing point is rounded away from the
/// polygon interior (`winding` is the sign of the triangle's area), so a
/// polygon cut here overlaps a neighbour that continues past the cut rather
/// than leaving a hairline between them.
fn cut(from: &ClipCorner, to: &ClipCorner, axis: usize, bound: i32, winding: i32) -> ClipCorner {
    // Coordinates along the cut axis, then along the other one.
    let (from_a, from_b, to_a, to_b) = if axis == 0 {
        (from.x, from.y, to.x, to.y)
    } else {
        (from.y, from.x, to.y, to.x)
    };
    let mut numerator = bound - from_a;
    let mut denominator = to_a - from_a;
    if denominator < 0 {
        numerator = -numerator;
        denominator = -denominator;
    }
    // `from + (to - from) * numerator / denominator`, rounded to nearest.
    let nearest = |a: i32, b: i32| -> i32 {
        (a * denominator + (b - a) * numerator + denominator / 2).div_euclid(denominator)
    };
    let exact = from_b * denominator + (to_b - from_b) * numerator;
    let low = exact.div_euclid(denominator);
    let point = |other: i32| -> (i32, i32) {
        if axis == 0 {
            (bound, other)
        } else {
            (other, bound)
        }
    };
    // Which side of the edge a candidate lies on, positive toward the interior.
    let interior = |other: i32| -> i32 {
        let (x, y) = point(other);
        let side = (to.x - from.x) * (y - from.y) - (to.y - from.y) * (x - from.x);
        side * winding.signum()
    };
    let other = if exact.rem_euclid(denominator) != 0 && interior(low + 1) < interior(low) {
        low + 1
    } else {
        low
    };
    let (x, y) = point(other);
    ClipCorner {
        x,
        y,
        u: nearest(from.u, to.u),
        v: nearest(from.v, to.v),
        color: [
            nearest(from.color[0], to.color[0]),
            nearest(from.color[1], to.color[1]),
            nearest(from.color[2], to.color[2]),
        ],
    }
}

/// Clip a triangle to the window. Returns the convex polygon (at most seven
/// corners) in the triangle's winding.
fn clip_to_window(triangle: [ClipCorner; 3]) -> ([ClipCorner; 8], usize) {
    let winding = cross(&triangle[0], &triangle[1], &triangle[2]);
    let mut current = [ClipCorner::default(); 8];
    current[..3].copy_from_slice(&triangle);
    let mut count = 3;
    // (axis, bound, keeps coordinates at least / at most the bound)
    let bounds = [
        (0usize, WINDOW_X.0, true),
        (0, WINDOW_X.1, false),
        (1, WINDOW_Y.0, true),
        (1, WINDOW_Y.1, false),
    ];
    for (axis, bound, keep_above) in bounds {
        let coordinate = |c: &ClipCorner| if axis == 0 { c.x } else { c.y };
        let inside = |c: &ClipCorner| {
            if keep_above {
                coordinate(c) >= bound
            } else {
                coordinate(c) <= bound
            }
        };
        let mut next = [ClipCorner::default(); 8];
        let mut kept = 0;
        for index in 0..count {
            let from = current[index];
            let to = current[(index + 1) % count];
            match (inside(&from), inside(&to)) {
                (true, true) => {
                    next[kept] = to;
                    kept += 1;
                }
                (true, false) => {
                    next[kept] = cut(&from, &to, axis, bound, winding);
                    kept += 1;
                }
                (false, true) => {
                    next[kept] = cut(&from, &to, axis, bound, winding);
                    next[kept + 1] = to;
                    kept += 2;
                }
                (false, false) => {}
            }
        }
        current = next;
        count = kept;
        if count < 3 {
            return (current, 0);
        }
    }
    (current, count)
}

impl PacketSink {
    /// [`Self::polygon`] for surfaces projected without the GTE's divide
    /// limits: a polygon the GPU draws is written as it stands, and only one
    /// it would drop goes to [`Self::polygon_clipped`].
    ///
    /// # Safety
    /// As [`Self::polygon_clipped`].
    #[inline(always)]
    unsafe fn polygon_near(
        &mut self,
        surface: &AffineSurface,
        corners: &[&AffineVertex],
        limit: *mut u32,
    ) {
        if self.off_screen(corners) {
            return;
        }
        let otz = Self::average_depth(corners);
        if !self.usable_slot(otz) {
            return;
        }
        if within_gpu_limits(corners) {
            // SAFETY: the caller reserved room for the polygon's packet.
            unsafe { self.write(surface, corners, otz) };
        } else {
            // SAFETY: as above.
            unsafe { self.polygon_clipped(surface, corners, otz, limit) };
        }
    }

    /// Clip a polygon the GPU would drop to the window, for surfaces
    /// projected without the GTE's divide limits: a polygon the GPU would drop whole (a span of 1024 columns or
    /// 512 rows, or a corner outside its 11-bit coordinates) is clipped to the
    /// window instead of lost. Polygons the GPU draws are written untouched.
    ///
    /// A clipped polygon can take several packets where the batch reserved
    /// one, so every write first checks the room left before `limit`; a
    /// polygon that no longer fits is lost, as it would be if the whole face
    /// had overflowed the storage.
    ///
    /// # Safety
    /// `self.next` must have room for the polygon's packet, and `limit` must
    /// be the end of the storage it lies in.
    #[inline(never)]
    unsafe fn polygon_clipped(
        &mut self,
        surface: &AffineSurface,
        corners: &[&AffineVertex],
        otz: u16,
        limit: *mut u32,
    ) {
        // The longest packet: a windowed quad with its tag and both windows.
        const MOST_WORDS: isize = 15;
        // SAFETY: both pointers lie in the storage the caller reserved.
        let room = |next: *mut u32| unsafe { limit.offset_from(next) >= MOST_WORDS };
        let quad = corners.len() == 4;
        let first = [corners[0], corners[1], corners[2]];
        let first_ok = gpu_draws(first);
        let second = if quad {
            [corners[1], corners[2], corners[3]]
        } else {
            first
        };
        let second_ok = !quad || gpu_draws(second);
        if first_ok && second_ok {
            // SAFETY: the caller reserved room for this packet.
            unsafe { self.write(surface, corners, otz) };
            return;
        }
        let triangles = [(first, first_ok), (second, second_ok)];
        for (triangle, ok) in triangles.iter().take(if quad { 2 } else { 1 }) {
            if !room(self.next) {
                return;
            }
            if *ok {
                // SAFETY: room checked above.
                unsafe { self.write(surface, triangle, otz) };
                continue;
            }
            let (polygon, count) = clip_to_window([
                ClipCorner::of(triangle[0]),
                ClipCorner::of(triangle[1]),
                ClipCorner::of(triangle[2]),
            ]);
            for index in 1..count.saturating_sub(1) {
                let (a, b, c) = (polygon[0], polygon[index], polygon[index + 1]);
                if cross(&a, &b, &c) == 0 {
                    continue;
                }
                if !room(self.next) {
                    return;
                }
                let (a, b, c) = (a.vertex(), b.vertex(), c.vertex());
                // SAFETY: room checked above.
                unsafe { self.write(surface, &[&a, &b, &c], otz) };
            }
        }
    }
}

/// A batch vertex or scratch slot by index.
///
/// # Safety
/// `index` must name an initialised record of `vertices`.
#[inline(always)]
unsafe fn at<'v>(vertices: *const AffineVertex, index: usize) -> &'v AffineVertex {
    unsafe { &*vertices.add(index) }
}

/// Edges of a triangle, by the bit that marks them split.
const EDGE_AB: u8 = 1;
const EDGE_BC: u8 = 2;
const EDGE_CA: u8 = 4;

/// Splits one surface's fan triangles into pieces and writes their packets.
struct Splitter<'a, R: Reproject> {
    vertices: *mut AffineVertex,
    /// Index of the first scratch slot after the batch.
    scratch: usize,
    sink: &'a mut PacketSink,
    surface: AffineSurface,
    bands: [u32; 2],
    view: R,
}

impl<R: Reproject> Splitter<'_, R> {
    #[inline(always)]
    fn vertex(&self, index: usize) -> &AffineVertex {
        // SAFETY: every index handed out is a batch vertex or a scratch slot.
        unsafe { &*self.vertices.add(index) }
    }

    /// Whether the edge between two projected vertices splits at `level`:
    /// the mean of their depths, in ordering-table units (SZ / 4), is nearer
    /// than that level's band.
    #[inline(always)]
    fn splits(&self, a: usize, b: usize, level: usize) -> bool {
        (self.vertex(a).depth + self.vertex(b).depth) >> 3 < self.bands[level]
    }

    #[inline(always)]
    fn split_mask(&self, triangle: [usize; 3], level: usize) -> u8 {
        let [a, b, c] = triangle;
        (if self.splits(a, b, level) { EDGE_AB } else { 0 })
            | (if self.splits(b, c, level) { EDGE_BC } else { 0 })
            | (if self.splits(c, a, level) { EDGE_CA } else { 0 })
    }

    /// Place the midpoints of the split edges in three scratch slots from
    /// `first` and project them. Returns the slots for AB, BC and CA.
    ///
    /// # Safety
    /// `first..first + 3` must be free scratch slots.
    #[inline(always)]
    unsafe fn midpoints(&mut self, triangle: [usize; 3], mask: u8, first: usize) -> [usize; 3] {
        let [a, b, c] = triangle;
        let slots = [first, first + 1, first + 2];
        let edges = [(a, b, EDGE_AB), (b, c, EDGE_BC), (c, a, EDGE_CA)];
        for (slot, (from, to, bit)) in slots.iter().zip(edges) {
            if mask & bit != 0 {
                let point = midpoint(self.vertex(from), self.vertex(to));
                // SAFETY: `slot` is a free scratch slot per the contract.
                unsafe { self.vertices.add(*slot).write(point) };
            }
        }
        // SAFETY: the slots were just written.
        unsafe {
            if mask == EDGE_AB | EDGE_BC | EDGE_CA {
                project_vertices(self.vertices.add(first), 3);
                self.view.fix(self.vertices.add(first), 3);
            } else {
                for (slot, (_, _, bit)) in slots.iter().zip(edges) {
                    if mask & bit != 0 {
                        project_vertices(self.vertices.add(*slot), 1);
                        self.view.fix(self.vertices.add(*slot), 1);
                    }
                }
            }
        }
        slots
    }

    /// The children of a triangle whose `mask` edges split at `mid`.
    /// Children keep the parent's winding.
    #[inline(always)]
    fn children(triangle: [usize; 3], mask: u8, mid: [usize; 3]) -> ([[usize; 3]; 4], usize) {
        let [a, b, c] = triangle;
        let [ab, bc, ca] = mid;
        let none = [0; 3];
        match mask {
            EDGE_AB => ([[a, ab, c], [ab, b, c], none, none], 2),
            EDGE_BC => ([[a, b, bc], [a, bc, c], none, none], 2),
            EDGE_CA => ([[a, b, ca], [ca, b, c], none, none], 2),
            m if m == EDGE_AB | EDGE_BC => ([[ab, b, bc], [a, ab, bc], [a, bc, c], none], 3),
            m if m == EDGE_BC | EDGE_CA => ([[bc, c, ca], [a, b, bc], [a, bc, ca], none], 3),
            m if m == EDGE_AB | EDGE_CA => ([[a, ab, ca], [ab, b, c], [ab, c, ca], none], 3),
            _ => ([[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]], 4),
        }
    }

    /// # Safety
    /// The sink must have room for every packet of this triangle.
    #[inline(always)]
    unsafe fn emit_triangle(&mut self, triangle: [usize; 3]) {
        let vertices = self.vertices;
        // SAFETY: the indices are batch vertices or written scratch slots.
        let corners = unsafe {
            [
                at(vertices, triangle[0]),
                at(vertices, triangle[1]),
                at(vertices, triangle[2]),
            ]
        };
        // SAFETY: capacity per the contract.
        unsafe {
            if R::CLIPS {
                self.sink
                    .polygon_near(&self.surface, &corners, self.view.limit())
            } else {
                self.sink.polygon(&self.surface, &corners)
            }
        };
    }

    /// Emit triangles (p, q, r) and (q, r, s), which share the edge q-r, as
    /// one quad.
    ///
    /// # Safety
    /// As [`Self::emit_triangle`].
    #[inline(always)]
    unsafe fn emit_pair(&mut self, quad: [usize; 4]) {
        let vertices = self.vertices;
        // SAFETY: as in `emit_triangle`.
        let corners = unsafe {
            [
                at(vertices, quad[0]),
                at(vertices, quad[1]),
                at(vertices, quad[2]),
                at(vertices, quad[3]),
            ]
        };
        // SAFETY: capacity per the contract.
        unsafe {
            if R::CLIPS {
                self.sink
                    .polygon_near(&self.surface, &corners, self.view.limit())
            } else {
                self.sink.polygon(&self.surface, &corners)
            }
        };
    }

    /// Second-level triangle: split once more where its edges ask for it and
    /// emit the leaves, pairing two of them into a quad where they form one.
    ///
    /// # Safety
    /// As [`Self::emit_triangle`]; scratch slots `3..6` must be free.
    #[inline(never)]
    unsafe fn refine_second(&mut self, triangle: [usize; 3]) {
        let mask = self.split_mask(triangle, 1);
        if mask == 0 {
            // SAFETY: as above.
            unsafe { self.emit_triangle(triangle) };
            return;
        }
        let first = self.scratch + 3;
        // SAFETY: slots `first..first + 3` are free per the contract.
        let mid = unsafe { self.midpoints(triangle, mask, first) };
        let (children, count) = Self::children(triangle, mask, mid);
        // SAFETY: as above.
        unsafe {
            match count {
                // Corner a with the centre: [a, ab, ca] and [ab, bc, ca]
                // share ab-ca.
                4 => {
                    self.emit_pair([children[0][0], children[0][1], children[0][2], mid[1]]);
                    self.emit_triangle(children[1]);
                    self.emit_triangle(children[2]);
                }
                // The two pieces besides the corner share their middle edge.
                3 => {
                    self.emit_triangle(children[0]);
                    let (p, q) = (children[1], children[2]);
                    self.emit_pair([p[1], p[0], p[2], q[2]]);
                }
                _ => {
                    self.emit_triangle(children[0]);
                    self.emit_triangle(children[1]);
                }
            }
        }
    }

    /// Fan triangle at the first level.
    ///
    /// # Safety
    /// As [`Self::emit_triangle`]; all six scratch slots must be free.
    #[inline(never)]
    unsafe fn refine_first(&mut self, triangle: [usize; 3], mask: u8) {
        let first = self.scratch;
        // SAFETY: slots `0..3` are free; the children use `3..6`.
        unsafe {
            let mid = self.midpoints(triangle, mask, first);
            let (children, count) = Self::children(triangle, mask, mid);
            for child in &children[..count] {
                self.refine_second(*child);
            }
        }
    }

    /// Submit one convex fan of `count` vertices starting at `first`.
    ///
    /// # Safety
    /// As [`Self::emit_triangle`] for every triangle of the fan.
    unsafe fn surface(&mut self, first: usize, count: usize) {
        let fan = |index: usize| first + index;
        let mut corner = 1;
        while corner + 1 < count {
            let triangle = [fan(0), fan(corner), fan(corner + 1)];
            let mask = self.split_mask(triangle, 0);
            if mask != 0 {
                // SAFETY: as above.
                unsafe { self.refine_first(triangle, mask) };
                corner += 1;
                continue;
            }
            // Two unsplit neighbours in the fan form one quad.
            if corner + 2 < count {
                let next = [fan(0), fan(corner + 1), fan(corner + 2)];
                if self.split_mask(next, 0) == 0 {
                    // SAFETY: as above.
                    unsafe {
                        self.emit_pair([fan(corner), fan(0), fan(corner + 1), fan(corner + 2)])
                    };
                    corner += 2;
                    continue;
                }
            }
            // SAFETY: as above.
            unsafe { self.emit_triangle(triangle) };
            corner += 1;
        }
    }
}

/// Project, split and write packets for a batch of convex fans.
///
/// The GTE view (rotation, translation, projection and ZSF weights) must be
/// loaded. Every surface is projected in place, so the vertices' screen and
/// depth fields are overwritten.
///
/// # Safety
/// - `vertices` must hold `vertex_count` initialised records followed by
///   [`AFFINE_SPLIT_SCRATCH_VERTICES`] writable scratch records.
/// - Every surface's vertex range must lie within `vertex_count`.
/// - `output` must have room for [`AFFINE_PACKETS_PER_TRIANGLE`] packets per
///   fan triangle, each at most 15 words (a windowed quad with its tag).
pub unsafe fn submit_surface_batch(
    vertices: *mut AffineVertex,
    vertex_count: usize,
    surfaces: *const AffineSurface,
    surface_count: usize,
    output: *mut u32,
    profile: SurfaceProfile,
) -> SurfaceSubmit {
    // SAFETY: the caller's contract is the inner function's.
    unsafe {
        submit_surface_batch_with(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            &profile,
            (),
        )
        .0
    }
}

/// [`submit_surface_batch`] for a batch some of whose surfaces may sit inside
/// the GTE's divide-saturation zone: such a surface is left unwritten, and its
/// index is bit `i` of the returned mask. Its vertices stay in the batch for
/// the caller to hand to [`submit_surface_batch_near`].
///
/// # Safety
/// As [`submit_surface_batch`], and `surface_count` is at most 32.
pub unsafe fn submit_surface_batch_screened(
    vertices: *mut AffineVertex,
    vertex_count: usize,
    surfaces: *const AffineSurface,
    surface_count: usize,
    output: *mut u32,
    profile: &SurfaceProfile,
) -> (SurfaceSubmit, u32) {
    // SAFETY: the caller's contract is the inner function's.
    unsafe {
        submit_surface_batch_with(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            profile,
            Screened,
        )
    }
}

/// [`submit_surface_batch`] for surfaces already clipped to the screen:
/// vertices the GTE divide saturates on are reprojected exactly from `view`.
///
/// # Safety
/// As [`submit_surface_batch`], with `output..end` the packet storage the
/// writer may fill: a polygon clipped to the GPU's window takes more packets
/// than the batch reserved, and is dropped when `end` leaves no room. `view`
/// must describe the view loaded in the GTE.
pub unsafe fn submit_surface_batch_near(
    vertices: *mut AffineVertex,
    vertex_count: usize,
    surfaces: *const AffineSurface,
    surface_count: usize,
    output: *mut u32,
    end: *mut u32,
    profile: SurfaceProfile,
    view: &NearView,
) -> SurfaceSubmit {
    // SAFETY: the caller's contract is the inner function's.
    unsafe {
        submit_surface_batch_with(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            &profile,
            (view, end),
        )
        .0
    }
}

#[inline(always)]
unsafe fn submit_surface_batch_with<R: Reproject>(
    vertices: *mut AffineVertex,
    vertex_count: usize,
    surfaces: *const AffineSurface,
    surface_count: usize,
    output: *mut u32,
    profile: &SurfaceProfile,
    view: R,
) -> (SurfaceSubmit, u32) {
    let mut sink = PacketSink {
        next: output,
        packets: 0,
        triangles: 0,
        right: profile.screen_width - 1,
        bottom: profile.screen_height - 1,
        ot_depth: profile.ot_depth,
    };
    if vertices.is_null() || surfaces.is_null() || output.is_null() || vertex_count == 0 {
        let none = SurfaceSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
        return (none, 0);
    }
    // SAFETY: `vertex_count` initialised records per the contract.
    let flagged = unsafe {
        let flagged = if R::SCREENS {
            project_vertices_flagged(vertices, vertex_count)
        } else {
            project_vertices(vertices, vertex_count);
            0
        };
        view.fix(vertices, vertex_count);
        flagged
    };
    let mut deferred = 0u32;
    let bands = [
        u32::from(profile.split_once_below),
        u32::from(profile.split_twice_below.min(profile.split_once_below)),
    ];
    for index in 0..surface_count {
        // SAFETY: `surface_count` descriptors per the contract.
        let surface = unsafe { *surfaces.add(index) };
        let first = usize::from(surface.first_vertex);
        let count = usize::from(surface.vertex_count);
        if count < 3 || first + count > vertex_count {
            continue;
        }
        if R::SCREENS && flagged != 0 {
            // A flagged triple may belong to a neighbour: keep the surface
            // unless one of its own vertices is the one the divide clamped.
            let span = ((1u32 << count) - 1) << first;
            if flagged & span != 0 && unsafe { saturates_any(vertices.add(first), count) } {
                deferred |= 1 << index;
                continue;
            }
        }
        let mut splitter = Splitter {
            vertices,
            scratch: vertex_count,
            sink: &mut sink,
            surface,
            bands,
            view,
        };
        // SAFETY: range checked above; capacity and scratch per the contract.
        unsafe { splitter.surface(first, count) };
    }
    let submitted = SurfaceSubmit {
        next_packet: sink.next,
        packets: sink.packets,
        hardware_triangles: sink.triangles,
    };
    (submitted, deferred)
}

/// Whether any of `count` projected vertices has the depth at which the GTE
/// divide saturates (`H >= 2 * SZ`, with `H` the projection plane loaded).
///
/// # Safety
/// `vertices` must hold `count` initialised records.
#[inline(never)]
unsafe fn saturates_any(vertices: *const AffineVertex, count: usize) -> bool {
    let h = psx_gte::read_control!(26) as i32;
    (0..count).any(|index| {
        // SAFETY: `index < count`.
        let depth = unsafe { (*vertices.add(index)).depth };
        h >= 2 * depth as i32
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// Identity view: positions are camera space, +Z forward, 320x240.
    fn configure() {
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_average_z_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);
    }

    fn vertex(position: [i16; 3], uv: [u8; 2]) -> AffineVertex {
        AffineVertex {
            position,
            uv,
            color: 0x0080_8080,
            ..AffineVertex::default()
        }
    }

    fn surface(first: u16, count: u16, compact: bool) -> AffineSurface {
        AffineSurface {
            first_vertex: first,
            vertex_count: count,
            tpage: 0x0105,
            clut: 0x1234,
            uv_offset: [0; 2],
            compact: u8::from(compact),
            texture_window_word: 0xe200_0000,
            color_command_word: 0x3400_0000,
        }
    }

    /// One decoded packet: data words, ordering slot, screen corners.
    type Packet = (u32, u32, Vec<[i16; 2]>);

    /// Submit `vertices` as the given surfaces and decode the packets.
    fn submit(
        vertices: &[AffineVertex],
        surfaces: &[AffineSurface],
    ) -> (SurfaceSubmit, Vec<Packet>) {
        configure();
        let mut batch = vertices.to_vec();
        batch.extend([AffineVertex::default(); AFFINE_SPLIT_SCRATCH_VERTICES]);
        let mut words = std::vec![0u32; 4096];
        let result = unsafe {
            submit_surface_batch(
                batch.as_mut_ptr(),
                vertices.len(),
                surfaces.as_ptr(),
                surfaces.len(),
                words.as_mut_ptr(),
                SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        let used = unsafe { result.next_packet.offset_from(words.as_ptr()) } as usize;
        let mut packets = Vec::new();
        let mut offset = 0;
        while offset < used {
            let tag = words[offset];
            let data = tag >> 24;
            // Both shapes lead with one GP0(E2) window word.
            let polygon = offset + 2;
            let corners = if data == QUAD_WORDS + 1 || data == QUAD_WORDS + 2 {
                4
            } else {
                3
            };
            let screens = (0..corners)
                .map(|corner| {
                    let word = words[polygon + corner * 3 + 1];
                    [word as u16 as i16, (word >> 16) as u16 as i16]
                })
                .collect();
            packets.push((data, tag & 0xffff, screens));
            offset += data as usize + 1;
        }
        assert_eq!(offset, used);
        (result, packets)
    }

    #[test]
    fn a_screened_batch_defers_exactly_the_surfaces_with_a_saturating_vertex() {
        // H = 160 saturates the divide at SZ <= 80. Triples run (0,1,2),
        // (3,4,5), (6,7), so the far surface A shares one with the near B.
        let far = |x: i16, y: i16| vertex([x, y, 400], [0, 0]);
        let near = |x: i16, y: i16| vertex([x, y, 40], [0, 0]);
        let vertices = [
            far(-50, -50),
            far(50, -50),
            far(50, 50),
            far(-50, 50),
            near(-5, -5),
            near(5, -5),
            near(5, 5),
            near(-5, 5),
        ];
        let surfaces = [surface(0, 4, false), surface(4, 4, false)];
        configure();
        let mut batch = vertices.to_vec();
        batch.extend([AffineVertex::default(); AFFINE_SPLIT_SCRATCH_VERTICES]);
        let mut words = std::vec![0u32; 4096];
        let (screened, deferred) = unsafe {
            submit_surface_batch_screened(
                batch.as_mut_ptr(),
                vertices.len(),
                surfaces.as_ptr(),
                surfaces.len(),
                words.as_mut_ptr(),
                &SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        assert_eq!(deferred, 0b10, "only the near surface is left");
        // The far surface is written exactly as the plain writer writes it.
        let (plain, _) = submit(&vertices[..4], &surfaces[..1]);
        assert_eq!(screened.packets, plain.packets);
        assert!(screened.packets > 0);
        // A batch with nothing near defers nothing.
        let (_, none) = unsafe {
            submit_surface_batch_screened(
                batch.as_mut_ptr(),
                4,
                surfaces.as_ptr(),
                1,
                words.as_mut_ptr(),
                &SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        assert_eq!(none, 0);
    }

    #[test]
    fn midpoint_averages_signed_positions_bytes_and_each_channel() {
        let a = AffineVertex {
            position: [-7, 100, -32768],
            uv: [255, 0],
            color: 0x00ff_0010,
            ..AffineVertex::default()
        };
        let b = AffineVertex {
            position: [8, -101, 32767],
            uv: [254, 3],
            color: 0x0001_ff31,
            ..AffineVertex::default()
        };
        let m = midpoint(&a, &b);
        assert_eq!(m.position, [0, -1, -1]);
        assert_eq!(m.uv, [254, 1]);
        assert_eq!(m.color, 0x0080_7f20);
        assert_eq!(midpoint(&b, &a), m, "midpoints are symmetric");
    }

    #[test]
    fn far_quad_is_one_unsplit_gpu_quad() {
        // Depth 3000: ordering depth 750, past both bands.
        let quad = [
            vertex([-400, -300, 3000], [0, 0]),
            vertex([400, -300, 3000], [63, 0]),
            vertex([400, 300, 3000], [63, 63]),
            vertex([-400, 300, 3000], [0, 63]),
        ];
        let (result, packets) = submit(&quad, &[surface(0, 4, true)]);
        assert_eq!(result.packets, 1);
        assert_eq!(result.hardware_triangles, 2);
        assert_eq!(packets[0].0, QUAD_WORDS + 1);
        assert_eq!(packets[0].1, 750);
    }

    #[test]
    fn a_near_triangle_splits_twice_within_the_packet_bound() {
        // Depth 200: ordering depth 50, inside both bands.
        let triangle = [
            vertex([-100, -60, 200], [0, 0]),
            vertex([100, -60, 200], [64, 0]),
            vertex([0, 60, 200], [32, 64]),
        ];
        let (result, packets) = submit(&triangle, &[surface(0, 3, false)]);
        assert_eq!(result.hardware_triangles, 16);
        assert!(result.packets as usize <= AFFINE_PACKETS_PER_TRIANGLE);
        // AVSZ3 keys a flat triangle at 49, AVSZ4 a flat quad at 50.
        assert!(packets.iter().all(|packet| (49..=50).contains(&packet.1)));
    }

    #[test]
    fn neighbours_split_their_shared_edge_identically() {
        // A strip that runs from near to far: every interior fan edge is
        // shared, and its subdivision points must appear on both sides.
        let strip = [
            vertex([-60, 40, 120], [0, 0]),
            vertex([60, 40, 120], [64, 0]),
            vertex([60, 40, 900], [64, 255]),
            vertex([-60, 40, 900], [0, 255]),
        ];
        let surfaces = [surface(0, 4, true)];
        let (_, packets) = submit(&strip, &surfaces);
        configure();
        // Collect every emitted corner; each must be either a source vertex
        // or a corner of at least two pieces (a shared split point) or lie on
        // the polygon's own boundary.
        let mut counts: std::collections::BTreeMap<[i16; 2], usize> = Default::default();
        for (_, _, corners) in &packets {
            for corner in corners {
                *counts.entry(*corner).or_default() += 1;
            }
        }
        let projected: Vec<[i16; 2]> = strip
            .iter()
            .map(|v| {
                let p = scene::project_vertex_scheduled(vec3(v.position));
                [p.sx, p.sy]
            })
            .collect();
        // The diagonal from corner 0 to corner 2 is interior: any split point
        // on it is used by pieces on both of its sides.
        let on_diagonal = |point: [i16; 2]| {
            let (a, c) = (projected[0], projected[2]);
            let cross = i32::from(c[0] - a[0]) * i32::from(point[1] - a[1])
                - i32::from(c[1] - a[1]) * i32::from(point[0] - a[0]);
            cross.abs() <= 2 * i32::from((c[0] - a[0]).abs().max((c[1] - a[1]).abs()))
                && !projected.contains(&point)
        };
        let mut diagonal_points = 0;
        for (point, count) in &counts {
            if on_diagonal(*point) {
                diagonal_points += 1;
                assert!(*count >= 2, "split point {point:?} used by one side only");
            }
        }
        assert!(diagonal_points > 0, "the near end of the diagonal splits");
    }

    #[test]
    fn pieces_beyond_one_screen_edge_or_the_table_are_dropped() {
        let left = [
            vertex([-2000, -10, 1000], [0, 0]),
            vertex([-1900, -10, 1000], [8, 0]),
            vertex([-1950, 10, 1000], [4, 8]),
        ];
        let (result, _) = submit(&left, &[surface(0, 3, true)]);
        assert_eq!(result.packets, 0);
        // Depth 9000: ordering depth 2250, past the 2048-slot table.
        let far = [
            vertex([-100, -10, 9000], [0, 0]),
            vertex([100, -10, 9000], [8, 0]),
            vertex([0, 10, 9000], [4, 8]),
        ];
        let (result, _) = submit(&far, &[surface(0, 3, true)]);
        assert_eq!(result.packets, 0);
    }

    #[test]
    fn windowed_surfaces_wrap_each_packet_in_a_scoped_window() {
        let triangle = [
            vertex([-100, -60, 3000], [0, 0]),
            vertex([100, -60, 3000], [64, 0]),
            vertex([0, 60, 3000], [32, 64]),
        ];
        let mut windowed = surface(0, 3, false);
        windowed.color_command_word = 0x3600_0000;
        windowed.uv_offset = [5, 7];
        configure();
        let mut batch = triangle.to_vec();
        batch.extend([AffineVertex::default(); AFFINE_SPLIT_SCRATCH_VERTICES]);
        let mut words = [0u32; 64];
        let result = unsafe {
            submit_surface_batch(
                batch.as_mut_ptr(),
                3,
                &windowed,
                1,
                words.as_mut_ptr(),
                SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        assert_eq!(result.packets, 1);
        assert_eq!(words[0] >> 24, TRI_WORDS + 2);
        assert_ne!(words[0] & WINDOWED_POLYGON_TAG, 0);
        assert_eq!(words[1], 0xe200_0000);
        assert_eq!(words[2] >> 24, 0x36);
        assert_eq!(words[4], 5 | (7 << 8) | (0x1234 << 16));
        assert_eq!(words[11], TextureWindow::NONE.word());
    }

    #[test]
    fn compact_surfaces_reset_the_texture_window_before_each_polygon() {
        let triangle = [
            vertex([-100, -60, 3000], [0, 0]),
            vertex([100, -60, 3000], [64, 0]),
            vertex([0, 60, 3000], [32, 64]),
        ];
        configure();
        let mut batch = triangle.to_vec();
        batch.extend([AffineVertex::default(); AFFINE_SPLIT_SCRATCH_VERTICES]);
        let mut words = [0u32; 64];
        let compact = surface(0, 3, true);
        let result = unsafe {
            submit_surface_batch(
                batch.as_mut_ptr(),
                3,
                &compact,
                1,
                words.as_mut_ptr(),
                SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        assert_eq!(result.packets, 1);
        // A model packet can leave a window selected between world draws, so
        // the polygon's own DMA node resets it first.
        assert_eq!(words[0] >> 24, TRI_WORDS + 1);
        assert_eq!(words[0] & WINDOWED_POLYGON_TAG, 0);
        assert_eq!(words[1], TextureWindow::NONE.word());
        assert_eq!(words[2] >> 24, 0x34);
    }

    fn corner(x: i32, y: i32, u: i32, v: i32) -> ClipCorner {
        ClipCorner {
            x,
            y,
            u,
            v,
            color: [u, v, 255 - u],
        }
    }

    fn inside(polygon: &[ClipCorner], x: i32, y: i32, slack: i32) -> bool {
        let point = ClipCorner {
            x,
            y,
            ..ClipCorner::default()
        };
        let winding = cross(&polygon[0], &polygon[1], &polygon[2]).signum();
        (0..polygon.len()).all(|index| {
            let (a, b) = (&polygon[index], &polygon[(index + 1) % polygon.len()]);
            cross(a, b, &point) * winding >= -slack
        })
    }

    #[test]
    fn a_triangle_beyond_the_gpu_limits_is_clipped_to_the_window_and_keeps_its_pixels() {
        let mut state = 0x1234_5678u32;
        let mut next = |span: i32| -> i32 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((state >> 8) % (2 * span as u32)) as i32 - span
        };
        let mut clipped_count = 0;
        for _ in 0..300 {
            let triangle = [
                corner(next(6000), next(6000), 10, 20),
                corner(next(6000), next(6000), 200, 40),
                corner(next(6000), next(6000), 90, 250),
            ];
            if cross(&triangle[0], &triangle[1], &triangle[2]) == 0 {
                continue;
            }
            let (polygon, count) = clip_to_window(triangle);
            if count == 0 {
                continue;
            }
            clipped_count += 1;
            for corner in &polygon[..count] {
                assert!(
                    (WINDOW_X.0 - 1..=WINDOW_X.1 + 1).contains(&corner.x)
                        && (WINDOW_Y.0 - 1..=WINDOW_Y.1 + 1).contains(&corner.y),
                    "corner ({}, {}) outside the window",
                    corner.x,
                    corner.y
                );
            }
            // Every screen pixel the triangle covers, with a pixel to spare
            // from its edges, is covered by the clipped polygon.
            let winding = cross(&triangle[0], &triangle[1], &triangle[2]).signum();
            for y in (0..240).step_by(7) {
                for x in (0..320).step_by(7) {
                    let strictly_inside = {
                        let point = ClipCorner {
                            x,
                            y,
                            ..ClipCorner::default()
                        };
                        (0..3).all(|index| {
                            let (a, b) = (&triangle[index], &triangle[(index + 1) % 3]);
                            let length = (((b.x - a.x) as f64).powi(2)
                                + ((b.y - a.y) as f64).powi(2))
                            .sqrt();
                            (cross(a, b, &point) * winding) as f64 > 2.0 * length
                        })
                    };
                    if strictly_inside {
                        assert!(
                            inside(&polygon[..count], x, y, 0),
                            "pixel ({x}, {y}) lost by the clip"
                        );
                    }
                }
            }
        }
        assert!(clipped_count > 100, "only {clipped_count} clips exercised");
    }

    #[test]
    fn exact_projection_agrees_with_the_gte_where_it_does_not_saturate() {
        // The PXBSP view: rotation scaled by 3, H = 320.
        let scale = 0x3000;
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(320);
        scene::load_rotation(&Mat3I16 {
            m: [[scale, 0, 0], [0, scale, 0], [0, 0, scale]],
        });
        scene::load_translation(Vec3I32::ZERO);
        let view = NearView {
            rows: [[scale, 0, 0], [0, scale, 0], [0, 0, scale]],
            translation: [0, 0, 0],
            focal_length: 320,
            center: [160, 120],
        };
        let mut state = 0x7e57_0001u32;
        let mut axis = |span: i32| -> i16 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (((state >> 8) % (2 * span as u32)) as i32 - span) as i16
        };
        let mut compared = 0;
        for _ in 0..20_000 {
            let position = [axis(400), axis(400), axis(600).abs() + 1];
            let projected = scene::project_vertex(vec3(position));
            let depth = u32::from(projected.sz);
            if view.saturates(depth) || depth == u32::from(u16::MAX) {
                continue;
            }
            // Skip vertices the GTE clamps to its screen range.
            if projected.sx.unsigned_abs() >= 0x3ff || projected.sy.unsigned_abs() >= 0x3ff {
                continue;
            }
            let exact = view.project(position).expect("positive depth");
            assert!(
                (i32::from(exact[0]) - i32::from(projected.sx)).abs() <= 1
                    && (i32::from(exact[1]) - i32::from(projected.sy)).abs() <= 1,
                "{position:?} exact {exact:?} gte ({}, {})",
                projected.sx,
                projected.sy
            );
            compared += 1;
        }
        assert!(compared > 1000, "only {compared} compared");
    }

    #[test]
    fn a_vertex_far_off_screen_keeps_its_direction_from_the_centre() {
        let view = NearView {
            rows: [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]],
            translation: [0, 0, 0],
            focal_length: 320,
            center: [160, 120],
        };
        // 1 unit deep at 3000 units right and 1500 down: far beyond 16 bits.
        let [x, y] = view.project([3000, 1500, 1]).expect("positive depth");
        let (dx, dy) = (i32::from(x) - 160, i32::from(y) - 120);
        assert!(dx.abs().max(dy.abs()) <= LIMIT);
        assert!(dx.abs().max(dy.abs()) > LIMIT - 2, "scaled to the limit");
        assert!((dx * 1500 - dy * 3000).abs() <= 3000 * 2, "direction kept");
        // A vertex 10 units deep lands where the divide puts it.
        assert_eq!(view.project([10, 5, 10]), Some([160 + 320, 120 + 160]));
        assert_eq!(view.project([10, 5, 0]), None);
    }

    #[test]
    fn a_triangle_within_the_gpu_limits_is_not_touched() {
        let make = |x: i16, y: i16| AffineVertex {
            screen: [x, y],
            ..AffineVertex::default()
        };
        let (a, b, c) = (make(-300, -100), make(600, 300), make(100, 380));
        assert!(gpu_draws([&a, &b, &c]));
        let (a, b, c) = (make(-300, -300), make(600, 300), make(100, 380));
        assert!(!gpu_draws([&a, &b, &c]), "a 600-row span is dropped");
        let (a, b, c) = (make(-1100, 0), make(100, 10), make(100, 80));
        assert!(
            !gpu_draws([&a, &b, &c]),
            "a corner outside 11 bits is dropped"
        );
    }

    #[test]
    fn cut_rounds_away_from_the_interior_and_interpolates_attributes() {
        // The edge crosses x = 670 at y = 2.01; the third corner is below it.
        let from = corner(0, 0, 0, 0);
        let to = corner(1000, 3, 100, 200);
        let below = corner(0, 100, 0, 0);
        let winding = cross(&from, &to, &below);
        let at = cut(&from, &to, 0, 670, winding);
        assert_eq!((at.x, at.y), (670, 2), "away from the interior below");
        assert_eq!((at.u, at.v), (67, 134));
        // The same edge with the interior above rounds the other way.
        let above = corner(0, -100, 0, 0);
        let winding = cross(&from, &to, &above);
        let at = cut(&from, &to, 0, 670, winding);
        assert_eq!((at.x, at.y), (670, 3), "away from the interior above");
    }
}
