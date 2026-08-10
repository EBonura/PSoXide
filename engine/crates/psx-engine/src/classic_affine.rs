//! Compact classic-affine world submission for retained C/Rust renderers.
//!
//! This path keeps the historical PS1 packet topology: camera-space
//! midpoints are reprojected, near triangles use a fixed two-level lattice,
//! compatible leaves are paired into GP0(3Ch) quads, and optional root-edge
//! underdraw seals rasterisation cracks. It deliberately emits the compact
//! no-texture-window packets from `psx-gpu`; callers set one texture-window
//! state for the pass and finish the staged tags with PSoXide's tagged-stream
//! OT linker.

use core::{mem::size_of, ptr};

use psx_gpu::prim::{
    ClassicQuadTexturedGouraud, ClassicTriTextured, ClassicTriTexturedGouraud, QuadTexturedGouraud,
    TriTexturedGouraud,
};
use psx_gte::{
    math::{Mat3I16, Vec3I16, Vec3I32},
    scene::{self, Projected},
};

const EXTRA_VERTICES: usize = 12;

/// Mutable vertex layout shared with retained renderers.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineVertex {
    /// Camera-space position consumed by the GTE.
    pub position: [i16; 3],
    /// Packet-space UV bytes.
    pub uv: [u8; 2],
    /// RGB in the low 24 bits.
    pub color: u32,
    /// Projected screen coordinate.
    pub screen: [i16; 2],
    /// Cached GTE SZ value.
    pub depth: i32,
}

/// Packed source vertex used by retained BSP/world formats.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineSourceVertex {
    /// Model- or world-space position.
    pub position: [i16; 3],
    /// Material-relative UV.
    pub uv: [u8; 2],
    /// Two baked light-style contributions.
    pub light: [u8; 2],
}

/// Word-strided retained-world vertex with either two light contributions or
/// a baked RGB word in its final field.
///
/// This layout lets validated asset loaders retain compact twelve-byte source
/// records while the renderer expands only the visible fans into projection
/// scratch.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineWordSourceVertex {
    /// Model- or world-space position.
    pub position: [i16; 3],
    /// Material-relative or already baked atlas UV.
    pub uv: [u8; 2],
    /// Two light contributions in the low bytes or baked RGB in the low 24 bits.
    pub light: u32,
}

/// Compact projected vertex used by the packed world-fan path.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineProjectedVertex {
    /// Material-atlas UV.
    pub uv: [u8; 2],
    /// Preserves four-byte alignment for packet attributes.
    pub _pad: u16,
    /// RGB in the low 24 bits.
    pub color: u32,
    /// Projected screen coordinate.
    pub screen: [i16; 2],
    /// Cached GTE depth.
    pub depth: i32,
}

/// Expand word-strided retained-world vertices into classic-affine scratch.
///
/// The first three words of each destination are materialized directly. The
/// projection fields are intentionally left unchanged because every classic
/// affine submit path overwrites them before reading them.
///
/// # Safety
/// `source` must contain `vertex_count` aligned source records, `destination`
/// must contain the same number of aligned writable records, and the ranges
/// must not overlap.
pub unsafe fn materialize_classic_affine_word_vertices(
    source: *const ClassicAffineWordSourceVertex,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
    uv_offset: [u8; 2],
    light_weights: [u16; 2],
    baked_uv: bool,
    baked_light: bool,
) {
    let mut index = 0usize;
    while index < vertex_count {
        let source_words = unsafe { source.add(index).cast::<u32>() };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy = unsafe { ptr::read(source_words) };
        let mut position_z_uv = unsafe { ptr::read(source_words.add(1)) };
        let source_light = unsafe { ptr::read(source_words.add(2)) };

        if !baked_uv {
            let u = ((position_z_uv >> 16) as u8).wrapping_add(uv_offset[0]);
            let v = ((position_z_uv >> 24) as u8).wrapping_add(uv_offset[1]);
            position_z_uv = (position_z_uv & 0x0000_ffff) | ((u as u32) << 16) | ((v as u32) << 24);
        }
        let color = if baked_light {
            source_light
        } else {
            light_color(
                [source_light as u8, (source_light >> 8) as u8],
                light_weights,
            )
        };

        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
}

/// Deduplicated signed-integer position consumed by the retained indexed
/// world projection batch.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffinePosition {
    /// Position in the 3D space expected by the currently loaded GTE matrix.
    pub position: [i16; 3],
}

/// One fan inside a contiguous classic-affine vertex batch.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineBatchSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan.
    pub vertex_count: u16,
    /// Texture-page word used by this surface.
    pub tpage: u16,
    /// CLUT word used by this surface.
    pub clut: u16,
}

/// One independently windowed fan inside a contiguous classic-affine batch.
///
/// Unlike [`ClassicAffineBatchSurface`], this descriptor selects the
/// self-contained packet shape that prefixes every polygon with GP0(E2).
/// That is necessary when tiled materials with different windows can
/// interleave at the same ordering-table depths.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineWindowedBatchSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan.
    pub vertex_count: u16,
    /// Texture-page word used by this surface.
    pub tpage: u16,
    /// CLUT word used by this surface.
    pub clut: u16,
    /// Wrapping U/V offset applied while materializing this surface's packets.
    ///
    /// Multiple material layers can therefore share projected positions while
    /// selecting independently scrolling regions of one texture page.
    pub uv_offset: [u8; 2],
    /// Fully encoded GP0(E2) texture-window command.
    pub texture_window_word: u32,
}

/// Fixed topology and packet bounds for classic affine submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffineProfile {
    /// Viewport width.
    pub screen_width: i16,
    /// Viewport height.
    pub screen_height: i16,
    /// Number of ordering-table slots.
    pub ot_depth: u16,
    /// OTZ below which one subdivision level is used.
    pub subdivide_once_at: u16,
    /// OTZ below which two subdivision levels are used.
    pub subdivide_twice_at: u16,
    /// OT slot bias applied to crack-sealing underdraw triangles.
    pub underdraw_slot_bias: u16,
}

impl ClassicAffineProfile {
    /// Historical 320x240 Quake-style profile with 2,048 OT slots.
    pub const QUAKE_REFERENCE: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        subdivide_once_at: 136,
        subdivide_twice_at: 60,
        underdraw_slot_bias: 8,
    };
}

/// Result of one fan submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffineSubmit {
    /// First packet word after the emitted stream.
    pub next_packet: *mut u32,
    /// Number of compact GPU packets emitted.
    pub packets: u32,
    /// Number of hardware triangles represented by those packets.
    pub hardware_triangles: u32,
}

/// Indexed face used by classic alias-model batches.
///
/// Each word keeps its packet UV in the low half and projected-cache byte
/// offset in the high half, matching the retained Quake model cooker.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAliasFace {
    /// Three packed UV16|projected-offset16 corner words.
    pub corners: [u32; 3],
}

/// Compact alias-model position in the original Quake byte representation.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAliasVertex {
    /// Original unsigned X, Y, and Z coordinate bytes.
    pub position: [u8; 3],
}

/// Projected scratch record used by classic alias-model batches.
#[repr(C, align(4))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAliasProjectedVertex {
    /// Packed screen coordinate.
    pub screen: [i16; 2],
    /// Cached unsigned GTE depth. The aligned record retains an eight-byte
    /// stride, with two trailing padding bytes left unused.
    pub depth: u16,
}

/// Compose the exact retained-alias model-to-view transform while keeping
/// repeated matrix work on one loaded GTE schedule.
///
/// `model_offset` and `world_origin` are integer world/model units, matching
/// the classic renderer's matrix translation lanes. `scale` is a per-axis Q12
/// value. The result is bit-identical to rotating the offset, scaling the
/// model matrix, and then composing it with the view matrix as separate SDK
/// operations, but avoids reloading the same model rotation between steps.
pub fn compose_classic_alias_transform(
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

/// Project selected deduplicated positions through the currently loaded GTE
/// scene state.
///
/// This applies the same dense-versus-indexed cache pattern used by PSoXide's
/// native room renderer to retained polygon streams. Indices are consumed in
/// groups of three through scheduled RTPT, with a scheduled RTPS tail.
///
/// # Safety
/// `positions` must contain `position_count` records, every one of the
/// `index_count` indices must be below `position_count`, and `projected` must
/// contain `position_count` writable records. The GTE rotation, translation,
/// projection plane, screen offset, and depth state must already be loaded.
pub unsafe fn project_classic_affine_indexed_vertices(
    positions: *const ClassicAffinePosition,
    position_count: usize,
    indices: *const u16,
    index_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
) {
    if positions.is_null()
        || indices.is_null()
        || projected.is_null()
        || position_count == 0
        || index_count == 0
    {
        return;
    }

    let mut index = 0usize;
    while index + 2 < index_count {
        let a_index = unsafe { *indices.add(index) } as usize;
        let b_index = unsafe { *indices.add(index + 1) } as usize;
        let c_index = unsafe { *indices.add(index + 2) } as usize;
        debug_assert!(a_index < position_count);
        debug_assert!(b_index < position_count);
        debug_assert!(c_index < position_count);
        let output = scene::project_triangle_scheduled(
            classic_position_vec3(unsafe { *positions.add(a_index) }),
            classic_position_vec3(unsafe { *positions.add(b_index) }),
            classic_position_vec3(unsafe { *positions.add(c_index) }),
        );
        unsafe {
            ptr::write(
                projected.add(a_index),
                ClassicAliasProjectedVertex {
                    screen: [output[0].sx, output[0].sy],
                    depth: output[0].sz,
                },
            );
            ptr::write(
                projected.add(b_index),
                ClassicAliasProjectedVertex {
                    screen: [output[1].sx, output[1].sy],
                    depth: output[1].sz,
                },
            );
            ptr::write(
                projected.add(c_index),
                ClassicAliasProjectedVertex {
                    screen: [output[2].sx, output[2].sy],
                    depth: output[2].sz,
                },
            );
        }
        index += 3;
    }
    while index < index_count {
        let position_index = unsafe { *indices.add(index) } as usize;
        debug_assert!(position_index < position_count);
        let output = scene::project_vertex_scheduled(classic_position_vec3(unsafe {
            *positions.add(position_index)
        }));
        unsafe {
            ptr::write(
                projected.add(position_index),
                ClassicAliasProjectedVertex {
                    screen: [output.sx, output.sy],
                    depth: output.sz,
                },
            );
        }
        index += 1;
    }
}

#[inline(always)]
fn classic_position_vec3(position: ClassicAffinePosition) -> Vec3I16 {
    Vec3I16::new(
        position.position[0],
        position.position[1],
        position.position[2],
    )
}

struct PacketWriter {
    next: *mut u32,
    packets: u32,
    clut_high_word: u32,
    tpage_high_word: u32,
    profile: ClassicAffineProfile,
}

impl PacketWriter {
    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = unsafe {
            ClassicTriTexturedGouraud::with_staged_slot_prepacked_unchecked(
                [
                    (projected[0].screen[0], projected[0].screen[1]),
                    (projected[1].screen[0], projected[1].screen[1]),
                    (projected[2].screen[0], projected[2].screen[1]),
                ],
                [
                    uv_word(attributes[0]),
                    uv_word(attributes[1]),
                    uv_word(attributes[2]),
                ],
                [
                    attributes[0].color,
                    attributes[1].color,
                    attributes[2].color,
                ],
                self.clut_high_word,
                self.tpage_high_word,
                otz,
            )
        };
        unsafe { ptr::write(self.next.cast::<ClassicTriTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = unsafe {
            ClassicQuadTexturedGouraud::with_staged_slot_prepacked_unchecked(
                [
                    (projected[0].screen[0], projected[0].screen[1]),
                    (projected[1].screen[0], projected[1].screen[1]),
                    (projected[2].screen[0], projected[2].screen[1]),
                    (projected[3].screen[0], projected[3].screen[1]),
                ],
                [
                    uv_word(attributes[0]),
                    uv_word(attributes[1]),
                    uv_word(attributes[2]),
                    uv_word(attributes[3]),
                ],
                [
                    attributes[0].color,
                    attributes[1].color,
                    attributes[2].color,
                    attributes[3].color,
                ],
                self.clut_high_word,
                self.tpage_high_word,
                otz,
            )
        };
        unsafe { ptr::write(self.next.cast::<ClassicQuadTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn emit_compact_tri(&mut self, vertices: [&ClassicAffineProjectedVertex; 3], otz: u16) {
        if classic_tri_screen_clipped(
            [vertices[0].screen, vertices[1].screen, vertices[2].screen],
            self.profile,
        ) {
            return;
        }
        debug_assert!(vertices
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = unsafe {
            ClassicTriTexturedGouraud::with_staged_slot_prepacked_unchecked(
                [
                    (vertices[0].screen[0], vertices[0].screen[1]),
                    (vertices[1].screen[0], vertices[1].screen[1]),
                    (vertices[2].screen[0], vertices[2].screen[1]),
                ],
                [
                    uv_compact_word(vertices[0]),
                    uv_compact_word(vertices[1]),
                    uv_compact_word(vertices[2]),
                ],
                [vertices[0].color, vertices[1].color, vertices[2].color],
                self.clut_high_word,
                self.tpage_high_word,
                otz,
            )
        };
        unsafe { ptr::write(self.next.cast::<ClassicTriTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn finish(self, output: *mut u32) -> ClassicAffineSubmit {
        let words = unsafe { self.next.offset_from(output) as u32 };
        let tri_words = (size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>()) as u32;
        let quad_words = (size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>()) as u32;
        // Every packet is either one hardware triangle or one quad. Recover
        // the quad count from the final stream length so emitters only update
        // the packet count in the hot path.
        let quads =
            words.wrapping_sub(self.packets.wrapping_mul(tri_words)) / (quad_words - tri_words);
        ClassicAffineSubmit {
            next_packet: self.next,
            packets: self.packets,
            hardware_triangles: self.packets.wrapping_add(quads),
        }
    }
}

/// Packet sink shared by the compact and self-contained-window variants.
/// Generic subdivision code monomorphises this trait, so the normal compact
/// path keeps its existing packet shape and does not gain a per-polygon
/// material branch.
trait AffinePacketWriter {
    fn profile(&self) -> ClassicAffineProfile;

    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    );

    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    );
}

impl AffinePacketWriter for PacketWriter {
    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.profile
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { PacketWriter::emit_tri(self, projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { PacketWriter::emit_quad(self, projected, attributes, otz) };
    }
}

struct WindowedPacketWriter {
    next: *mut u32,
    packets: u32,
    clut_high_word: u32,
    tpage_high_word: u32,
    uv_offset: [u8; 2],
    texture_window_word: u32,
    profile: ClassicAffineProfile,
}

impl AffinePacketWriter for WindowedPacketWriter {
    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.profile
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = unsafe {
            TriTexturedGouraud::with_staged_slot_prepacked_unchecked(
                [
                    (projected[0].screen[0], projected[0].screen[1]),
                    (projected[1].screen[0], projected[1].screen[1]),
                    (projected[2].screen[0], projected[2].screen[1]),
                ],
                [
                    offset_uv_word(attributes[0], self.uv_offset),
                    offset_uv_word(attributes[1], self.uv_offset),
                    offset_uv_word(attributes[2], self.uv_offset),
                ],
                [
                    attributes[0].color,
                    attributes[1].color,
                    attributes[2].color,
                ],
                self.clut_high_word,
                self.tpage_high_word,
                self.texture_window_word,
                otz,
            )
        };
        unsafe { ptr::write(self.next.cast::<TriTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<TriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let packet = unsafe {
            QuadTexturedGouraud::with_staged_slot_prepacked_unchecked(
                [
                    (projected[0].screen[0], projected[0].screen[1]),
                    (projected[1].screen[0], projected[1].screen[1]),
                    (projected[2].screen[0], projected[2].screen[1]),
                    (projected[3].screen[0], projected[3].screen[1]),
                ],
                [
                    offset_uv_word(attributes[0], self.uv_offset),
                    offset_uv_word(attributes[1], self.uv_offset),
                    offset_uv_word(attributes[2], self.uv_offset),
                    offset_uv_word(attributes[3], self.uv_offset),
                ],
                [
                    attributes[0].color,
                    attributes[1].color,
                    attributes[2].color,
                    attributes[3].color,
                ],
                self.clut_high_word,
                self.tpage_high_word,
                self.texture_window_word,
                otz,
            )
        };
        unsafe { ptr::write(self.next.cast::<QuadTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<QuadTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
    }
}

impl WindowedPacketWriter {
    #[inline(always)]
    unsafe fn finish(self, output: *mut u32) -> ClassicAffineSubmit {
        let words = unsafe { self.next.offset_from(output) as u32 };
        let tri_words = (size_of::<TriTexturedGouraud>() / size_of::<u32>()) as u32;
        let quad_words = (size_of::<QuadTexturedGouraud>() / size_of::<u32>()) as u32;
        let quads =
            words.wrapping_sub(self.packets.wrapping_mul(tri_words)) / (quad_words - tri_words);
        ClassicAffineSubmit {
            next_packet: self.next,
            packets: self.packets,
            hardware_triangles: self.packets.wrapping_add(quads),
        }
    }
}

#[inline(always)]
const fn uv_word(vertex: &ClassicAffineVertex) -> u16 {
    vertex.uv[0] as u16 | ((vertex.uv[1] as u16) << 8)
}

#[inline(always)]
const fn offset_uv_word(vertex: &ClassicAffineVertex, offset: [u8; 2]) -> u16 {
    vertex.uv[0].wrapping_add(offset[0]) as u16
        | ((vertex.uv[1].wrapping_add(offset[1]) as u16) << 8)
}

#[inline(always)]
const fn uv_compact_word(vertex: &ClassicAffineProjectedVertex) -> u16 {
    vertex.uv[0] as u16 | ((vertex.uv[1] as u16) << 8)
}

#[inline(always)]
fn midpoint(a: &ClassicAffineVertex, b: &ClassicAffineVertex) -> ClassicAffineVertex {
    let light = ((a.color as u8 as u16 + b.color as u8 as u16) >> 1) as u32;
    let a_uv = u16::from_le_bytes(a.uv);
    let b_uv = u16::from_le_bytes(b.uv);
    // Average both packed bytes independently. Clearing each byte's low bit
    // before the shift prevents a carry from U into V.
    let uv = (a_uv & b_uv).wrapping_add(((a_uv ^ b_uv) & 0xfefe) >> 1);
    ClassicAffineVertex {
        position: [
            ((a.position[0] as i32 + b.position[0] as i32) >> 1) as i16,
            ((a.position[1] as i32 + b.position[1] as i32) >> 1) as i16,
            ((a.position[2] as i32 + b.position[2] as i32) >> 1) as i16,
        ],
        uv: uv.to_le_bytes(),
        color: light | (light << 8) | (light << 16),
        screen: [0; 2],
        depth: 0,
    }
}

#[inline(always)]
unsafe fn project_three_consecutive(vertices: *mut ClassicAffineVertex) {
    let a = unsafe { classic_vertex_position(vertices) };
    let b = unsafe { classic_vertex_position(vertices.add(1)) };
    let c = unsafe { classic_vertex_position(vertices.add(2)) };
    let out = scene::project_triangle_scheduled(a, b, c);
    unsafe {
        store_classic_projection(vertices, out[0]);
        store_classic_projection(vertices.add(1), out[1]);
        store_classic_projection(vertices.add(2), out[2]);
    }
}

#[inline(always)]
unsafe fn project_one(vertex: *mut ClassicAffineVertex) {
    let source = unsafe { classic_vertex_position(vertex) };
    let out = scene::project_vertex_scheduled(source);
    unsafe { store_classic_projection(vertex, out) };
}

#[inline(always)]
unsafe fn classic_vertex_position(vertex: *const ClassicAffineVertex) -> Vec3I16 {
    let position = unsafe { (*vertex).position };
    Vec3I16::new(position[0], position[1], position[2])
}

#[inline(always)]
unsafe fn store_classic_projection(vertex: *mut ClassicAffineVertex, out: Projected) {
    unsafe {
        ptr::write(ptr::addr_of_mut!((*vertex).screen), [out.sx, out.sy]);
        ptr::write(ptr::addr_of_mut!((*vertex).depth), out.sz as i32);
    }
}

#[inline(always)]
fn average3(vertices: [&ClassicAffineVertex; 3]) -> u16 {
    let sum = vertices[0].depth as u16 as u32
        + vertices[1].depth as u16 as u32
        + vertices[2].depth as u16 as u32;
    scene::classic_otz3_from_sum(sum)
}

#[inline(always)]
unsafe fn sorted_tri<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 3],
    attributes: [&ClassicAffineVertex; 3],
) {
    let otz = average3(projected);
    if otz > 0 {
        unsafe { writer.emit_tri(projected, attributes, otz) };
    }
}

#[inline(always)]
unsafe fn sorted_quad<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 4],
    attributes: [&ClassicAffineVertex; 4],
) {
    let depth_sum = projected[0].depth as u16 as u32
        + projected[1].depth as u16 as u32
        + projected[2].depth as u16 as u32
        + projected[3].depth as u16 as u32;
    let otz = (depth_sum >> 4) as u16;
    if otz > 0 {
        unsafe { writer.emit_quad(projected, attributes, otz) };
    }
}

unsafe fn subdivide_once<W: AffinePacketWriter>(
    writer: &mut W,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
) {
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root2, root0));
        project_three_consecutive(scratch);
    }
    let h01 = unsafe { &*scratch };
    let h12 = unsafe { &*scratch.add(1) };
    let h20 = unsafe { &*scratch.add(2) };
    unsafe {
        sorted_quad(writer, [root0, h01, h20, h12], [root0, h01, h20, h12]);
        sorted_tri(writer, [h01, root1, h12], [h01, root1, h12]);
        sorted_tri(writer, [h12, root2, h20], [h12, root2, h20]);
    }
    let profile = writer.profile();
    let underdraw_at = i32::from(profile.subdivide_once_at);
    if root0.depth >= underdraw_at || root1.depth >= underdraw_at || root2.depth >= underdraw_at {
        let underdraw = root_otz.saturating_add(profile.underdraw_slot_bias);
        unsafe {
            writer.emit_tri([root0, root1, h01], [root0, root1, h01], underdraw);
            writer.emit_tri([root1, root2, h12], [root0, root1, h12], underdraw);
            writer.emit_tri([root2, root0, h20], [root2, root0, h20], underdraw);
        }
    }
}

unsafe fn subdivide_twice<W: AffinePacketWriter>(
    writer: &mut W,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
) {
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root0, root2));
        ptr::write(scratch.add(3), midpoint(root0, &*scratch));
        ptr::write(scratch.add(4), midpoint(root1, &*scratch));
        ptr::write(scratch.add(5), midpoint(root1, &*scratch.add(1)));
        ptr::write(scratch.add(6), midpoint(&*scratch.add(1), root2));
        ptr::write(scratch.add(7), midpoint(&*scratch.add(2), root2));
        ptr::write(scratch.add(8), midpoint(&*scratch.add(2), root0));
        ptr::write(scratch.add(9), midpoint(&*scratch.add(2), &*scratch));
        ptr::write(
            scratch.add(10),
            midpoint(&*scratch.add(9), &*scratch.add(5)),
        );
        ptr::write(
            scratch.add(11),
            midpoint(&*scratch.add(2), &*scratch.add(1)),
        );
        project_three_consecutive(scratch);
        project_three_consecutive(scratch.add(3));
        project_three_consecutive(scratch.add(6));
        project_three_consecutive(scratch.add(9));
    }
    let v = unsafe { core::slice::from_raw_parts(scratch, EXTRA_VERTICES) };
    unsafe {
        sorted_tri(writer, [root0, &v[3], &v[8]], [root0, &v[3], &v[8]]);
        sorted_tri(writer, [&v[8], &v[9], &v[2]], [&v[8], &v[9], &v[2]]);
        sorted_tri(writer, [&v[2], &v[11], &v[7]], [&v[2], &v[11], &v[7]]);
        sorted_tri(writer, [&v[7], &v[6], root2], [&v[7], &v[6], root2]);
        sorted_quad(
            writer,
            [&v[3], &v[0], &v[8], &v[9]],
            [&v[3], &v[0], &v[8], &v[9]],
        );
        sorted_quad(
            writer,
            [&v[0], &v[4], &v[9], &v[10]],
            [&v[0], &v[4], &v[9], &v[10]],
        );
        sorted_quad(
            writer,
            [&v[4], root1, &v[10], &v[5]],
            [&v[4], root1, &v[10], &v[5]],
        );
        sorted_quad(
            writer,
            [&v[9], &v[10], &v[2], &v[11]],
            [&v[9], &v[10], &v[2], &v[11]],
        );
        sorted_quad(
            writer,
            [&v[10], &v[5], &v[11], &v[1]],
            [&v[10], &v[5], &v[11], &v[1]],
        );
        sorted_quad(
            writer,
            [&v[11], &v[1], &v[7], &v[6]],
            [&v[11], &v[1], &v[7], &v[6]],
        );
    }
    let profile = writer.profile();
    let underdraw_at = i32::from(profile.subdivide_twice_at);
    if root0.depth >= underdraw_at || root1.depth >= underdraw_at || root2.depth >= underdraw_at {
        let underdraw = root_otz.saturating_add(profile.underdraw_slot_bias);
        unsafe {
            // GP0(3Ch) splits [a,b,c,d] into [b,d,c] then [a,b,c].
            // These orders are cyclic rotations of the two old triangles in
            // their exact OT draw order, so the crack-sealing raster stays
            // bit-identical while each edge pair needs one packet setup.
            writer.emit_quad(
                [root1, &v[0], root0, &v[3]],
                [root1, &v[0], root0, &v[3]],
                underdraw,
            );
            writer.emit_tri([&v[0], root1, &v[4]], [&v[0], root1, &v[4]], underdraw);
            writer.emit_quad(
                [root2, &v[1], root1, &v[5]],
                [root2, &v[1], root1, &v[5]],
                underdraw,
            );
            writer.emit_tri([&v[1], root2, &v[6]], [&v[1], root2, &v[6]], underdraw);
            writer.emit_quad(
                [root2, &v[2], root0, &v[8]],
                [root2, &v[2], root0, &v[8]],
                underdraw,
            );
            writer.emit_tri([&v[2], root2, &v[7]], [&v[2], root2, &v[7]], underdraw);
        }
    }
}

/// Project and submit a convex triangle fan through the compact classic
/// affine path.
///
/// # Safety
/// `vertices` must point to `vertex_count + 12` writable
/// [`ClassicAffineVertex`] records; the extra records are scratch for the
/// fixed two-level lattice. `output` must point to enough writable,
/// four-byte-aligned packet memory for the worst-case fan output, and remain
/// live until the staged stream is linked and submitted.
pub unsafe fn submit_classic_affine_fan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null() || output.is_null() || vertex_count < 3 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    let mut index = 0usize;
    while index + 2 < vertex_count {
        unsafe { project_three_consecutive(vertices.add(index)) };
        index += 3;
    }
    while index < vertex_count {
        unsafe { project_one(vertices.add(index)) };
        index += 1;
    }

    unsafe {
        submit_classic_affine_projected_fan_with_scratch(
            vertices,
            vertex_count,
            vertices.add(vertex_count),
            output,
            tpage,
            clut,
            profile,
        )
    }
}

/// Submit a convex triangle fan whose source vertices already contain screen
/// coordinates and cached GTE depths.
///
/// This is the indexed-cache counterpart to [`submit_classic_affine_fan`]. A
/// retained renderer can project shared positions once, copy those cached
/// results into its surface scratch, and keep the exact same subdivision and
/// packet topology without repeating RTPT/RTPS for every referencing surface.
///
/// # Safety
/// The pointer, scratch-tail, output-capacity, and lifetime contract is the
/// same as [`submit_classic_affine_fan`]. Every source record's `screen` and
/// `depth` fields must have been produced by the currently intended camera.
pub unsafe fn submit_classic_affine_projected_fan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null() || output.is_null() || vertex_count < 3 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    unsafe {
        submit_classic_affine_projected_fan_with_scratch(
            vertices,
            vertex_count,
            vertices.add(vertex_count),
            output,
            tpage,
            clut,
            profile,
        )
    }
}

#[inline(always)]
unsafe fn submit_classic_affine_projected_fan_with_scratch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    let mut writer = PacketWriter {
        next: output,
        packets: 0,
        clut_high_word: (clut as u32) << 16,
        tpage_high_word: (tpage as u32) << 16,
        profile,
    };
    unsafe {
        submit_classic_affine_projected_fan_into_writer(
            vertices,
            vertex_count,
            generated,
            &mut writer,
        );
    }
    unsafe { writer.finish(output) }
}

#[inline(always)]
unsafe fn submit_classic_affine_projected_fan_into_writer<W: AffinePacketWriter>(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut W,
) {
    let profile = writer.profile();
    let mut surface_clip = 0x0fu8;
    let mut clip_index = 0usize;
    while clip_index < vertex_count && surface_clip != 0 {
        surface_clip &= classic_clip_code(unsafe { (*vertices.add(clip_index)).screen }, profile);
        clip_index += 1;
    }
    if surface_clip != 0 {
        return;
    }

    let root = unsafe { &*vertices };
    let root_depth = root.depth as u16 as u32;
    let end = unsafe { vertices.add(vertex_count) };
    let mut previous = unsafe { vertices.add(1) };
    let mut current = unsafe { vertices.add(2) };
    while current != end {
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = scene::classic_otz3_from_sum(
            root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
        );
        if otz > 0 && otz < profile.ot_depth {
            let next = unsafe { current.add(1) };
            if otz >= profile.subdivide_once_at && next != end {
                let next_ref = unsafe { &*next };
                if scene::classic_otz3_from_sum(
                    root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
                ) == otz
                {
                    // GP0 quads split on q1-q2. Reorder two adjacent fan
                    // triangles so that edge lands on the fan's shared 0-2
                    // diagonal and its two internal triangles match the
                    // staged OT stream's reverse link order. This keeps the
                    // affine interpolation anchors bit-exact at the seam.
                    let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                    unsafe { writer.emit_quad(quad_refs, quad_refs, otz) };
                    previous = next;
                    current = unsafe { next.add(1) };
                    continue;
                }
            }
            if otz < profile.subdivide_twice_at {
                unsafe { subdivide_twice(writer, root, previous_ref, current_ref, generated, otz) };
            } else if otz < profile.subdivide_once_at {
                unsafe { subdivide_once(writer, root, previous_ref, current_ref, generated, otz) };
            } else {
                let root_refs = [root, previous_ref, current_ref];
                unsafe { writer.emit_tri(root_refs, root_refs, otz) };
            }
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
}

/// Project and submit several contiguous convex fans as one scheduled batch.
///
/// Root vertices from adjacent surfaces share RTPT groups, avoiding the RTPS
/// tails paid when every small fan is projected independently. Surfaces are
/// still submitted in descriptor order with their own material state and the
/// exact same subdivision, quad-pairing, clipping, and underdraw rules as
/// [`submit_classic_affine_fan`].
///
/// # Safety
/// `vertices` must point to `vertex_count + 12` writable records, with the
/// final records reserved for shared subdivision scratch. `surfaces` must
/// contain `surface_count` descriptors whose vertex ranges fit entirely in
/// the first `vertex_count` records. `output` must have room for every fan's
/// worst-case packet expansion and remain live until submission completes.
pub unsafe fn submit_classic_affine_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || output.is_null()
        || vertex_count == 0
        || surface_count == 0
    {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }

    let mut vertex = 0usize;
    while vertex + 2 < vertex_count {
        unsafe { project_three_consecutive(vertices.add(vertex)) };
        vertex += 3;
    }
    while vertex < vertex_count {
        unsafe { project_one(vertices.add(vertex)) };
        vertex += 1;
    }

    let generated = unsafe { vertices.add(vertex_count) };
    let mut writer = PacketWriter {
        next: output,
        packets: 0,
        clut_high_word: 0,
        tpage_high_word: 0,
        profile,
    };
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        writer.tpage_high_word = (surface.tpage as u32) << 16;
        writer.clut_high_word = (surface.clut as u32) << 16;
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut writer,
            );
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
    }

    unsafe { writer.finish(output) }
}

/// Project and submit one convex fan with a self-contained GP0(E2) texture
/// window in every emitted polygon packet.
///
/// Use this for repeating sub-rectangles that can interleave with other
/// materials in the ordering table. The geometry, camera-space subdivision,
/// quad pairing, and crack sealing are identical to
/// [`submit_classic_affine_fan`]; only the packet shape gains the inline
/// texture-window command.
///
/// # Safety
/// The vertex, scratch-tail, output-capacity, and lifetime contract is the
/// same as [`submit_classic_affine_fan`]. `texture_window_word` must be a
/// valid GP0(E2) command, normally produced by
/// [`psx_gpu::material::TextureWindow::word`].
pub unsafe fn submit_classic_affine_windowed_fan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    texture_window_word: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null() || output.is_null() || vertex_count < 3 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    let mut index = 0usize;
    while index + 2 < vertex_count {
        unsafe { project_three_consecutive(vertices.add(index)) };
        index += 3;
    }
    while index < vertex_count {
        unsafe { project_one(vertices.add(index)) };
        index += 1;
    }

    let mut writer = WindowedPacketWriter {
        next: output,
        packets: 0,
        clut_high_word: (clut as u32) << 16,
        tpage_high_word: (tpage as u32) << 16,
        uv_offset: [0; 2],
        texture_window_word,
        profile,
    };
    unsafe {
        submit_classic_affine_projected_fan_into_writer(
            vertices,
            vertex_count,
            vertices.add(vertex_count),
            &mut writer,
        );
        writer.finish(output)
    }
}

/// Project and submit several independently windowed convex fans in one GTE
/// schedule.
///
/// Each descriptor selects its own tpage, CLUT, and GP0(E2) word. Every
/// emitted polygon therefore restores the correct window even when packets
/// from different surfaces meet at the same OT depth.
///
/// # Safety
/// The vertex, descriptor, scratch-tail, output-capacity, and lifetime
/// contract matches [`submit_classic_affine_batch`]. Every descriptor's
/// `texture_window_word` must be a valid GP0(E2) command.
pub unsafe fn submit_classic_affine_windowed_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineWindowedBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || output.is_null()
        || vertex_count == 0
        || surface_count == 0
    {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }

    let mut vertex = 0usize;
    while vertex + 2 < vertex_count {
        unsafe { project_three_consecutive(vertices.add(vertex)) };
        vertex += 3;
    }
    while vertex < vertex_count {
        unsafe { project_one(vertices.add(vertex)) };
        vertex += 1;
    }

    let generated = unsafe { vertices.add(vertex_count) };
    let mut writer = WindowedPacketWriter {
        next: output,
        packets: 0,
        clut_high_word: 0,
        tpage_high_word: 0,
        uv_offset: [0; 2],
        texture_window_word: 0,
        profile,
    };
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        writer.tpage_high_word = (surface.tpage as u32) << 16;
        writer.clut_high_word = (surface.clut as u32) << 16;
        writer.uv_offset = surface.uv_offset;
        writer.texture_window_word = surface.texture_window_word;
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut writer,
            );
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
    }

    unsafe { writer.finish(output) }
}

/// Project and submit a convex fan directly from a packed retained-world
/// vertex stream.
///
/// This fuses UV/light preparation with shared-vertex projection and keeps
/// only the compact packet attributes in scratch. Full camera-space records
/// are materialized only for triangles that enter the historical near
/// subdivision bands.
///
/// # Safety
/// `vertices` must contain `vertex_count` valid source records. `scratch`
/// must provide `vertex_count * size_of::<ClassicAffineProjectedVertex>() +
/// 12 * size_of::<ClassicAffineVertex>()` writable bytes. `output` must have
/// enough packet storage for the worst-case fan expansion.
pub unsafe fn submit_classic_affine_packed_fan(
    vertices: *const ClassicAffineSourceVertex,
    vertex_count: usize,
    scratch: *mut ClassicAffineProjectedVertex,
    output: *mut u32,
    uv_offset: [u8; 2],
    light_weights: [u16; 2],
    tpage: u16,
    clut: u16,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null() || scratch.is_null() || output.is_null() || vertex_count < 3 {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }

    let projected = unsafe { core::slice::from_raw_parts_mut(scratch, vertex_count) };
    let mut index = 0usize;
    while index + 2 < vertex_count {
        let a = unsafe { ptr::read_unaligned(vertices.add(index)) };
        let b = unsafe { ptr::read_unaligned(vertices.add(index + 1)) };
        let c = unsafe { ptr::read_unaligned(vertices.add(index + 2)) };
        let out = scene::project_triangle_scheduled(source_vec3(a), source_vec3(b), source_vec3(c));
        projected[index] = prepare_projected(a, out[0], uv_offset, light_weights);
        projected[index + 1] = prepare_projected(b, out[1], uv_offset, light_weights);
        projected[index + 2] = prepare_projected(c, out[2], uv_offset, light_weights);
        index += 3;
    }
    while index < vertex_count {
        let source = unsafe { ptr::read_unaligned(vertices.add(index)) };
        let out = scene::project_vertex_scheduled(source_vec3(source));
        projected[index] = prepare_projected(source, out, uv_offset, light_weights);
        index += 1;
    }

    let generated_ptr = unsafe { scratch.add(vertex_count).cast::<ClassicAffineVertex>() };
    let mut writer = PacketWriter {
        next: output,
        packets: 0,
        clut_high_word: (clut as u32) << 16,
        tpage_high_word: (tpage as u32) << 16,
        profile,
    };
    let mut fan = 2usize;
    while fan < vertex_count {
        let root = [&projected[0], &projected[fan - 1], &projected[fan]];
        let otz = scene::average_cached_z3([
            root[0].depth as u16,
            root[1].depth as u16,
            root[2].depth as u16,
        ]);
        if otz > 0 && otz < profile.ot_depth {
            if otz < profile.subdivide_once_at {
                let source = [
                    unsafe { ptr::read_unaligned(vertices) },
                    unsafe { ptr::read_unaligned(vertices.add(fan - 1)) },
                    unsafe { ptr::read_unaligned(vertices.add(fan)) },
                ];
                let expanded = [
                    expand_source(source[0], root[0]),
                    expand_source(source[1], root[1]),
                    expand_source(source[2], root[2]),
                ];
                if otz < profile.subdivide_twice_at {
                    unsafe {
                        subdivide_twice(
                            &mut writer,
                            &expanded[0],
                            &expanded[1],
                            &expanded[2],
                            generated_ptr,
                            otz,
                        )
                    };
                } else {
                    unsafe {
                        subdivide_once(
                            &mut writer,
                            &expanded[0],
                            &expanded[1],
                            &expanded[2],
                            generated_ptr,
                            otz,
                        )
                    };
                }
            } else {
                unsafe { writer.emit_compact_tri(root, otz) };
            }
        }
        fan += 1;
    }

    unsafe { writer.finish(output) }
}

#[inline(always)]
fn source_vec3(vertex: ClassicAffineSourceVertex) -> Vec3I16 {
    Vec3I16::new(vertex.position[0], vertex.position[1], vertex.position[2])
}

#[inline(always)]
fn prepare_projected(
    source: ClassicAffineSourceVertex,
    projected: Projected,
    uv_offset: [u8; 2],
    light_weights: [u16; 2],
) -> ClassicAffineProjectedVertex {
    ClassicAffineProjectedVertex {
        uv: [
            source.uv[0].wrapping_add(uv_offset[0]),
            source.uv[1].wrapping_add(uv_offset[1]),
        ],
        _pad: 0,
        color: light_color(source.light, light_weights),
        screen: [projected.sx, projected.sy],
        depth: projected.sz as i32,
    }
}

#[inline(always)]
fn light_color(light: [u8; 2], weights: [u16; 2]) -> u32 {
    let mut lit = light[0] as u32 * weights[0] as u32;
    if weights[1] != 0 {
        lit = lit.wrapping_add(light[1] as u32 * weights[1] as u32);
    }
    lit >>= 8;
    lit | (lit << 8) | (lit << 16)
}

#[inline(always)]
fn expand_source(
    source: ClassicAffineSourceVertex,
    projected: &ClassicAffineProjectedVertex,
) -> ClassicAffineVertex {
    ClassicAffineVertex {
        position: source.position,
        uv: projected.uv,
        color: projected.color,
        screen: projected.screen,
        depth: projected.depth,
    }
}

/// Project and submit an indexed alias model through compact flat textured
/// packets.
///
/// This is the retained-model counterpart to [`submit_classic_affine_fan`]:
/// all shared vertices are projected once, then the face loop performs the
/// scheduled GTE area/depth tests and stages packets for the tagged-stream OT
/// linker.
///
/// # Safety
/// `vertices` must contain `vertex_count` records. `faces` must be four-byte
/// aligned and contain `face_count` valid faces whose projected byte offsets
/// are eight-byte aligned and below `vertex_count * 8`, `projected` must
/// contain `vertex_count` writable records, and `output` must have room for
/// one [`ClassicTriTextured`] per face.
unsafe fn submit_classic_alias_model_inner<const SCREEN_SPACE: bool>(
    vertices: *const ClassicAliasVertex,
    vertex_count: usize,
    faces: *const ClassicAliasFace,
    face_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    tint: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    if vertices.is_null() || faces.is_null() || projected.is_null() || output.is_null() {
        return ClassicAffineSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }

    let mut vertex = 0usize;
    while vertex + 2 < vertex_count {
        let out = scene::project_triangle_scheduled(
            unsafe { alias_vec3(vertices, vertex) },
            unsafe { alias_vec3(vertices, vertex + 1) },
            unsafe { alias_vec3(vertices, vertex + 2) },
        );
        let projected_a = ClassicAliasProjectedVertex {
            screen: [out[0].sx, out[0].sy],
            depth: out[0].sz,
        };
        let projected_b = ClassicAliasProjectedVertex {
            screen: [out[1].sx, out[1].sy],
            depth: out[1].sz,
        };
        let projected_c = ClassicAliasProjectedVertex {
            screen: [out[2].sx, out[2].sy],
            depth: out[2].sz,
        };
        unsafe {
            ptr::write(projected.add(vertex), projected_a);
            ptr::write(projected.add(vertex + 1), projected_b);
            ptr::write(projected.add(vertex + 2), projected_c);
        }
        vertex += 3;
    }
    while vertex < vertex_count {
        let out = scene::project_vertex_scheduled(unsafe { alias_vec3(vertices, vertex) });
        let projected_vertex = ClassicAliasProjectedVertex {
            screen: [out.sx, out.sy],
            depth: out.sz,
        };
        unsafe { ptr::write(projected.add(vertex), projected_vertex) };
        vertex += 1;
    }

    let mut next = output;
    let mut face_index = 0usize;
    while face_index < face_count {
        let corners = unsafe { ptr::read(faces.add(face_index)) }.corners;
        let projected_bytes = projected.cast::<u8>();
        let a = unsafe {
            &*projected_bytes
                .add((corners[0] >> 16) as usize)
                .cast::<ClassicAliasProjectedVertex>()
        };
        let b = unsafe {
            &*projected_bytes
                .add((corners[1] >> 16) as usize)
                .cast::<ClassicAliasProjectedVertex>()
        };
        let c = unsafe {
            &*projected_bytes
                .add((corners[2] >> 16) as usize)
                .cast::<ClassicAliasProjectedVertex>()
        };
        let screens = [a.screen, b.screen, c.screen];
        let screen_points = [
            (screens[0][0], screens[0][1]),
            (screens[1][0], screens[1][1]),
            (screens[2][0], screens[2][1]),
        ];
        let (area, cached_otz) = if SCREEN_SPACE {
            (scene::screen_area_mac0(screen_points), u16::MAX)
        } else {
            scene::screen_area_and_classic_otz3_scheduled(
                screen_points,
                [a.depth, b.depth, c.depth],
            )
        };
        if area >= 0 {
            let otz = if SCREEN_SPACE {
                Some(cached_otz)
            } else {
                let depth = cached_otz;
                (depth > 0 && depth < profile.ot_depth).then_some(depth)
            };
            if let Some(otz) = otz {
                let packet = ClassicTriTextured::with_staged_slot(
                    [
                        (screens[0][0], screens[0][1]),
                        (screens[1][0], screens[1][1]),
                        (screens[2][0], screens[2][1]),
                    ],
                    [corners[0] as u16, corners[1] as u16, corners[2] as u16],
                    tint,
                    clut,
                    tpage,
                    otz,
                );
                unsafe { ptr::write(next.cast::<ClassicTriTextured>(), packet) };
                next = unsafe { next.add(size_of::<ClassicTriTextured>() / size_of::<u32>()) };
            }
        }
        face_index += 1;
    }

    let packets = unsafe { next.offset_from(output) as u32 }
        / (size_of::<ClassicTriTextured>() / size_of::<u32>()) as u32;
    ClassicAffineSubmit {
        next_packet: next,
        packets,
        hardware_triangles: packets,
    }
}

#[inline(always)]
unsafe fn alias_vec3(vertices: *const ClassicAliasVertex, index: usize) -> Vec3I16 {
    let vertex = unsafe { (*vertices.add(index)).position };
    Vec3I16::new(vertex[0] as i16, vertex[1] as i16, vertex[2] as i16)
}

/// Project and submit an indexed alias model into staged ordering-table
/// packets.
///
/// # Safety
/// The pointer and capacity contract is the same as
/// [`submit_classic_affine_fan`]; every projected face offset must be aligned
/// to eight bytes and below `vertex_count * 8`.
pub unsafe fn submit_classic_alias_model(
    vertices: *const ClassicAliasVertex,
    vertex_count: usize,
    faces: *const ClassicAliasFace,
    face_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    tint: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    unsafe {
        submit_classic_alias_model_inner::<false>(
            vertices,
            vertex_count,
            faces,
            face_count,
            projected,
            output,
            tpage,
            clut,
            tint,
            profile,
        )
    }
}

/// Project and submit an indexed first-person alias model into a
/// contiguous screen-space packet stream.
///
/// The returned packets carry the `0xffff` tagged-stream sentinel and must be
/// registered with the caller's screen/HUD submission list.
///
/// # Safety
/// The pointer and capacity contract is the same as
/// [`submit_classic_alias_model`].
pub unsafe fn submit_classic_alias_view_model(
    vertices: *const ClassicAliasVertex,
    vertex_count: usize,
    faces: *const ClassicAliasFace,
    face_count: usize,
    projected: *mut ClassicAliasProjectedVertex,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    tint: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    unsafe {
        submit_classic_alias_model_inner::<true>(
            vertices,
            vertex_count,
            faces,
            face_count,
            projected,
            output,
            tpage,
            clut,
            tint,
            profile,
        )
    }
}

#[inline(always)]
fn classic_clip_code(screen: [i16; 2], profile: ClassicAffineProfile) -> u8 {
    let x = screen[0] as i32;
    let y = screen[1] as i32;
    let right = profile.screen_width as i32 - 2;
    let bottom = profile.screen_height as i32 - 2;

    if (x as u32) <= right as u32 && (y as u32) <= bottom as u32 {
        return 0;
    }

    // Projected coordinates and viewport extents fit comfortably in i32.
    ((x as u32 >> 31) as u8)
        | ((((right - x) as u32 >> 31) as u8) << 1)
        | (((y as u32 >> 31) as u8) << 2)
        | ((((bottom - y) as u32 >> 31) as u8) << 3)
}

#[inline(always)]
fn classic_tri_screen_clipped(screens: [[i16; 2]; 3], profile: ClassicAffineProfile) -> bool {
    let code = |screen: [i16; 2]| {
        let mut result = 0u8;
        if screen[0] < 0 {
            result |= 1;
        }
        if screen[0] >= profile.screen_width - 1 {
            result |= 2;
        }
        if screen[1] < 0 {
            result |= 4;
        }
        if screen[1] >= profile.screen_height - 1 {
            result |= 8;
        }
        result
    };
    let c0 = code(screens[0]);
    let c1 = code(screens[1]);
    let c2 = code(screens[2]);
    (c0 & c1) != 0 && (c1 & c2) != 0 && (c2 & c0) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vertex_layout_matches_retained_c_record() {
        assert_eq!(size_of::<ClassicAffineVertex>(), 20);
        assert_eq!(core::mem::align_of::<ClassicAffineVertex>(), 4);
        assert_eq!(size_of::<ClassicAffineSourceVertex>(), 10);
        assert_eq!(core::mem::align_of::<ClassicAffineSourceVertex>(), 1);
        assert_eq!(size_of::<ClassicAffineWordSourceVertex>(), 12);
        assert_eq!(core::mem::align_of::<ClassicAffineWordSourceVertex>(), 4);
        assert_eq!(size_of::<ClassicAffineProjectedVertex>(), 16);
        assert_eq!(core::mem::align_of::<ClassicAffineProjectedVertex>(), 4);
        assert_eq!(size_of::<ClassicAffinePosition>(), 6);
        assert_eq!(core::mem::align_of::<ClassicAffinePosition>(), 2);
        assert_eq!(size_of::<ClassicAffineBatchSurface>(), 8);
        assert_eq!(core::mem::align_of::<ClassicAffineBatchSurface>(), 2);
        assert_eq!(size_of::<ClassicAffineWindowedBatchSurface>(), 16);
        assert_eq!(
            core::mem::align_of::<ClassicAffineWindowedBatchSurface>(),
            4
        );
        assert_eq!(size_of::<ClassicAliasFace>(), 12);
        assert_eq!(core::mem::align_of::<ClassicAliasFace>(), 4);
        assert_eq!(size_of::<ClassicAliasVertex>(), 3);
        assert_eq!(core::mem::align_of::<ClassicAliasVertex>(), 1);
        assert_eq!(size_of::<ClassicAliasProjectedVertex>(), 8);
        assert_eq!(core::mem::align_of::<ClassicAliasProjectedVertex>(), 4);
    }

    #[test]
    fn windowed_surface_uv_offsets_wrap_each_component() {
        let vertex = ClassicAffineVertex {
            uv: [250, 255],
            ..ClassicAffineVertex::default()
        };
        assert_eq!(offset_uv_word(&vertex, [10, 2]), 0x0104);
    }

    #[test]
    fn word_source_materialization_preserves_baked_and_dynamic_attributes() {
        let source = [
            ClassicAffineWordSourceVertex {
                position: [-7, 11, 23],
                uv: [250, 4],
                light: 0x0000_2010,
            },
            ClassicAffineWordSourceVertex {
                position: [31, -19, 5],
                uv: [9, 17],
                light: 0x00ab_cdef,
            },
        ];
        let mut output = [ClassicAffineVertex {
            screen: [123, -456],
            depth: 789,
            ..ClassicAffineVertex::default()
        }; 2];
        unsafe {
            materialize_classic_affine_word_vertices(
                source.as_ptr(),
                1,
                output.as_mut_ptr(),
                [10, 252],
                [256, 128],
                false,
                false,
            );
            materialize_classic_affine_word_vertices(
                source.as_ptr().add(1),
                1,
                output.as_mut_ptr().add(1),
                [99, 99],
                [0, 0],
                true,
                true,
            );
        }
        assert_eq!(output[0].position, [-7, 11, 23]);
        assert_eq!(output[0].uv, [4, 0]);
        assert_eq!(output[0].color, 0x0020_2020);
        assert_eq!(output[0].screen, [123, -456]);
        assert_eq!(output[0].depth, 789);
        assert_eq!(output[1].position, [31, -19, 5]);
        assert_eq!(output[1].uv, [9, 17]);
        assert_eq!(output[1].color, 0x00ab_cdef);
    }

    #[test]
    fn midpoint_matches_byte_attribute_and_signed_position_averages() {
        let a = ClassicAffineVertex {
            position: [-3, 10, 21],
            uv: [1, 250],
            color: 0x0010_1010,
            ..ClassicAffineVertex::default()
        };
        let b = ClassicAffineVertex {
            position: [2, 15, 20],
            uv: [4, 10],
            color: 0x0020_2020,
            ..ClassicAffineVertex::default()
        };
        let mid = midpoint(&a, &b);
        assert_eq!(mid.position, [-1, 12, 20]);
        assert_eq!(mid.uv, [2, 130]);
        assert_eq!(mid.color, 0x0018_1818);
    }

    #[test]
    fn midpoint_packed_uv_matches_independent_byte_averages() {
        for a in u8::MIN..=u8::MAX {
            for b in u8::MIN..=u8::MAX {
                let left = ClassicAffineVertex {
                    uv: [a, b],
                    ..ClassicAffineVertex::default()
                };
                let right = ClassicAffineVertex {
                    uv: [b, a],
                    ..ClassicAffineVertex::default()
                };
                let expected = ((a as u16 + b as u16) >> 1) as u8;
                assert_eq!(midpoint(&left, &right).uv, [expected; 2]);
            }
        }
    }

    #[test]
    fn packet_writer_derives_hardware_triangles_from_stream_length() {
        let tri_words = size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>();
        let quad_words = size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>();
        let mut storage = [0u32; 128];
        let output = storage.as_mut_ptr();

        for (triangles, quads) in [(0usize, 0usize), (1, 0), (0, 1), (2, 3)] {
            let packets = triangles + quads;
            let words = triangles * tri_words + quads * quad_words;
            let writer = PacketWriter {
                next: unsafe { output.add(words) },
                packets: packets as u32,
                clut_high_word: 0,
                tpage_high_word: 0,
                profile: ClassicAffineProfile::QUAKE_REFERENCE,
            };
            let submit = unsafe { writer.finish(output) };
            assert_eq!(submit.next_packet, unsafe { output.add(words) });
            assert_eq!(submit.packets, packets as u32);
            assert_eq!(submit.hardware_triangles, (triangles + quads * 2) as u32);
        }
    }

    #[test]
    fn alias_vertex_reader_preserves_compact_coordinates() {
        let compact = [
            ClassicAliasVertex {
                position: [1, 2, 3],
            },
            ClassicAliasVertex {
                position: [4, 5, 6],
            },
        ];
        assert_eq!(
            unsafe { alias_vec3(compact.as_ptr(), 1) },
            Vec3I16::new(4, 5, 6)
        );
    }

    #[test]
    fn fused_alias_transform_matches_separate_sdk_operations() {
        let view_rotation = Mat3I16::rotate_xyz(19, 37, 5);
        let view_translation = Vec3I32::new(120, -48, 320);
        let model_rotation = Mat3I16::rotate_z(71).mul(&Mat3I16::rotate_y(43));
        let model_offset = Vec3I16::new(-7, 13, 4);
        let world_origin = Vec3I32::new(96, -112, 28);
        let scale = Vec3I16::new(3072, 4096, 5120);

        scene::load_rotation(&model_rotation);
        scene::load_translation(Vec3I32::ZERO);
        let rotated_offset = scene::transform_vertex_scheduled(model_offset);
        let diagonal = Mat3I16 {
            m: [[scale.x, 0, 0], [0, scale.y, 0], [0, 0, scale.z]],
        };
        let scaled = scene::compose_rotation_scheduled(&model_rotation, &diagonal);
        let rotation = scene::compose_rotation_scheduled(&view_rotation, &scaled);
        scene::load_rotation(&view_rotation);
        scene::load_translation(Vec3I32::ZERO);
        let rotated_translation = scene::transform_vertex_scheduled(Vec3I16::new(
            rotated_offset.x.wrapping_add(world_origin.x) as i16,
            rotated_offset.y.wrapping_add(world_origin.y) as i16,
            rotated_offset.z.wrapping_add(world_origin.z) as i16,
        ));
        let translation = Vec3I32::new(
            rotated_translation.x.wrapping_add(view_translation.x),
            rotated_translation.y.wrapping_add(view_translation.y),
            rotated_translation.z.wrapping_add(view_translation.z),
        );

        assert_eq!(
            compose_classic_alias_transform(
                view_rotation,
                view_translation,
                model_rotation,
                model_offset,
                world_origin,
                scale,
            ),
            (rotation, translation)
        );
    }

    #[test]
    fn branchless_clip_code_keeps_quake_viewport_boundaries() {
        let profile = ClassicAffineProfile::QUAKE_REFERENCE;
        assert_eq!(classic_clip_code([0, 0], profile), 0);
        assert_eq!(classic_clip_code([318, 238], profile), 0);
        assert_eq!(classic_clip_code([-1, 120], profile), 1);
        assert_eq!(classic_clip_code([319, 120], profile), 2);
        assert_eq!(classic_clip_code([160, -1], profile), 4);
        assert_eq!(classic_clip_code([160, 239], profile), 8);
        assert_eq!(classic_clip_code([-1024, -1024], profile), 5);
        assert_eq!(classic_clip_code([1023, 1023], profile), 10);
    }
}
