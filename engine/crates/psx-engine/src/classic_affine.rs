//! Compact classic-affine world submission for retained C/Rust renderers.
//!
//! This path keeps the historical PS1 packet topology: camera-space
//! midpoints are reprojected, near or visibly warped triangles use a fixed
//! two-level lattice, compatible leaves are paired into GP0(3Ch) quads, and
//! optional root-edge underdraw seals rasterisation cracks. It deliberately
//! emits the compact no-texture-window packets from `psx-gpu`; callers set one
//! texture-window state for the pass and finish the staged tags with PSoXide's
//! tagged-stream OT linker.

use core::{mem::size_of, ptr};

use psx_gpu::material::TextureWindow;
use psx_gpu::ot::TAG_SCOPED_TEXTURE_WINDOW;
use psx_gpu::prim::{
    ClassicQuadTexturedGouraud, ClassicTriTextured, ClassicTriTexturedGouraud, QuadTexturedGouraud,
    TriTexturedGouraud,
};
use psx_gte::{
    math::{Mat3I16, Vec3I16, Vec3I32},
    scene::{self, Projected},
};

use crate::projection::{
    classic_quad_screen_rejected, classic_triangle_screen_rejected, project_triangle_scheduled,
    project_vertex_scheduled,
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

/// Indexed retained-world corner with material attributes separated from its
/// shared position.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineIndexedCorner {
    /// Index into the caller-owned shared-position array.
    pub position_index: u16,
    /// Material-relative or baked atlas UV.
    pub uv: [u8; 2],
    /// Two light contributions in the low bytes or baked RGB.
    pub light: u32,
}

const _: [(); 8] = [(); size_of::<ClassicAffineIndexedCorner>()];

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

/// Expand the common cooked-brush vertex form into projection scratch.
///
/// Cooked brush vertices commonly carry material-relative UVs and baked RGB.
/// Selecting that contract once per face avoids testing both format flags for
/// every vertex in the generic compatibility path.
///
/// # Safety
/// The source, destination, alignment, length, and non-overlap requirements
/// match [`materialize_classic_affine_word_vertices`].
pub unsafe fn materialize_classic_affine_baked_light_vertices(
    source: *const ClassicAffineWordSourceVertex,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
    uv_offset: [u8; 2],
) {
    let mut index = 0usize;
    while index < vertex_count {
        let source_words = unsafe { source.add(index).cast::<u32>() };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy = unsafe { ptr::read(source_words) };
        let mut position_z_uv = unsafe { ptr::read(source_words.add(1)) };
        let u = ((position_z_uv >> 16) as u8).wrapping_add(uv_offset[0]);
        let v = ((position_z_uv >> 24) as u8).wrapping_add(uv_offset[1]);
        position_z_uv = (position_z_uv & 0x0000_ffff) | ((u as u32) << 16) | ((v as u32) << 24);
        let color = unsafe { ptr::read(source_words.add(2)) };

        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
}

/// Expand indexed corners and shared positions into projection scratch.
///
/// # Safety
/// `corners` and `destination` must contain `vertex_count` records, every
/// corner index must address `positions`, and all ranges must be aligned and
/// non-overlapping.
pub unsafe fn materialize_classic_affine_indexed_vertices(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
    uv_offset: [u8; 2],
    light_weights: [u16; 2],
    baked_uv: bool,
    baked_light: bool,
) {
    let mut index = 0usize;
    while index < vertex_count {
        let corner = unsafe { ptr::read(corners.add(index)) };
        let position_index = corner.position_index as usize;
        debug_assert!(position_index < position_count);
        let position = unsafe { ptr::read(positions.add(position_index)) };
        let uv = if baked_uv {
            corner.uv
        } else {
            [
                corner.uv[0].wrapping_add(uv_offset[0]),
                corner.uv[1].wrapping_add(uv_offset[1]),
            ]
        };
        let color = if baked_light {
            corner.light
        } else {
            light_color(
                [corner.light as u8, (corner.light >> 8) as u8],
                light_weights,
            )
        };
        // Projection overwrites screen/depth before either field is read. As
        // with the retained word-source path, initialize only the first three
        // words and avoid eight bytes of dead stores per visible corner.
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy =
            u32::from(position.position[0] as u16) | (u32::from(position.position[1] as u16) << 16);
        let position_z_uv = u32::from(position.position[2] as u16)
            | (u32::from(uv[0]) << 16)
            | (u32::from(uv[1]) << 24);
        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
}

/// Expand indexed corners whose UV and RGB attributes were both baked by the
/// cooker.
///
/// This is the dominant static-world contract in Quake-derived maps. Selecting
/// it once per face removes the two per-corner format branches, the light-style
/// arithmetic, and the dead UV-offset inputs from the hot materialisation loop.
/// The corner's two aligned words can also be copied directly around the shared
/// position lookup.
///
/// # Safety
/// `corners` and `destination` must contain `vertex_count` records, every
/// corner index must address `positions`, and all ranges must be aligned and
/// non-overlapping.
pub unsafe fn materialize_classic_affine_indexed_baked_vertices(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
) {
    let mut index = 0usize;
    while index < vertex_count {
        let corner_words = unsafe { corners.add(index).cast::<u32>() };
        let position_and_uv = unsafe { ptr::read(corner_words) };
        let position_index = position_and_uv as u16 as usize;
        debug_assert!(position_index < position_count);
        let position = unsafe { ptr::read(positions.add(position_index)) };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy =
            u32::from(position.position[0] as u16) | (u32::from(position.position[1] as u16) << 16);
        let position_z_uv =
            u32::from(position.position[2] as u16) | (position_and_uv & 0xffff_0000);
        let color = unsafe { ptr::read(corner_words.add(1)) };
        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
}

/// Expand and project one baked indexed fan in a single source pass.
///
/// The ordinary retained path otherwise writes the position/attribute prefix
/// for every corner, then reloads those positions in a later batch projection
/// pass.  This form issues RTPT as soon as three gathered positions are ready,
/// uses the GTE execution window to commit the corresponding UV/colour
/// records, and writes SXY/SZ into the same destination records before moving
/// on.  It deliberately preserves the complete [`ClassicAffineVertex`]
/// layout because adaptive subdivision still consumes the original positions.
///
/// # Safety
/// The source, destination, alignment, length, and index requirements match
/// [`materialize_classic_affine_indexed_baked_vertices`]. The GTE must contain
/// the camera transform and projection state for the submitted fan.
pub unsafe fn materialize_project_classic_affine_indexed_baked_vertices(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
) {
    let mut index = 0usize;
    while index + 2 < vertex_count {
        let mut position_vectors = [Vec3I16::ZERO; 3];
        let mut position_xy = [0u32; 3];
        let mut position_z_uv = [0u32; 3];
        let mut colors = [0u32; 3];
        let mut lane = 0usize;
        while lane < 3 {
            let corner_words = unsafe { corners.add(index + lane).cast::<u32>() };
            let position_and_uv = unsafe { ptr::read(corner_words) };
            let position_index = position_and_uv as u16 as usize;
            debug_assert!(position_index < position_count);
            let position = unsafe { ptr::read(positions.add(position_index)) };
            position_vectors[lane] = classic_position_vec3(position);
            position_xy[lane] = u32::from(position.position[0] as u16)
                | (u32::from(position.position[1] as u16) << 16);
            position_z_uv[lane] =
                u32::from(position.position[2] as u16) | (position_and_uv & 0xffff_0000);
            colors[lane] = unsafe { ptr::read(corner_words.add(1)) };
            lane += 1;
        }

        let projected = scene::rtpt_kick(
            position_vectors[0],
            position_vectors[1],
            position_vectors[2],
        );
        lane = 0;
        while lane < 3 {
            let destination_words = unsafe { destination.add(index + lane).cast::<u32>() };
            unsafe {
                ptr::write(destination_words, position_xy[lane]);
                ptr::write(destination_words.add(1), position_z_uv[lane]);
                ptr::write(destination_words.add(2), colors[lane]);
            }
            lane += 1;
        }
        let projected = projected.read();
        lane = 0;
        while lane < 3 {
            unsafe { store_classic_projection(destination.add(index + lane), projected[lane]) };
            lane += 1;
        }
        index += 3;
    }

    while index < vertex_count {
        let corner_words = unsafe { corners.add(index).cast::<u32>() };
        let position_and_uv = unsafe { ptr::read(corner_words) };
        let position_index = position_and_uv as u16 as usize;
        debug_assert!(position_index < position_count);
        let position = unsafe { ptr::read(positions.add(position_index)) };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy =
            u32::from(position.position[0] as u16) | (u32::from(position.position[1] as u16) << 16);
        let position_z_uv =
            u32::from(position.position[2] as u16) | (position_and_uv & 0xffff_0000);
        let color = unsafe { ptr::read(corner_words.add(1)) };
        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        let projected = project_vertex_scheduled(classic_position_vec3(position));
        unsafe { store_classic_projection(destination.add(index), projected) };
        index += 1;
    }
}

/// Materialize and project a contiguous indexed fan batch in one source pass.
///
/// Unlike the single-fan helper, RTPT groups are formed across descriptor
/// boundaries. This preserves the original batch submitter's GTE schedule for
/// the common four- and five-corner faces while still eliminating the second
/// position pass through materialized scratch.
///
/// # Safety
/// `surfaces` and `sources` must contain `surface_count` matching descriptors.
/// Surface vertex ranges must densely cover `vertex_count` destination
/// records in ascending order. Every source corner range and position index
/// must be valid, and the GTE must contain the active camera state.
#[allow(clippy::too_many_arguments)]
pub unsafe fn materialize_project_classic_affine_indexed_batch(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    sources: *const ClassicAffineIndexedBatchSource,
    surface_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
) {
    if corners.is_null()
        || positions.is_null()
        || surfaces.is_null()
        || sources.is_null()
        || destination.is_null()
        || surface_count == 0
        || vertex_count == 0
    {
        return;
    }

    let mut surface_index = 0usize;
    let mut surface = unsafe { ptr::read(surfaces) };
    let mut source = unsafe { ptr::read(sources) };
    let mut index = 0usize;
    while index + 2 < vertex_count {
        let mut position_vectors = [Vec3I16::ZERO; 3];
        let mut position_xy = [0u32; 3];
        let mut position_z_uv = [0u32; 3];
        let mut colors = [0u32; 3];
        let mut lane = 0usize;
        while lane < 3 {
            let flat = index + lane;
            while flat >= surface.first_vertex as usize + surface.vertex_count as usize {
                surface_index += 1;
                debug_assert!(surface_index < surface_count);
                surface = unsafe { ptr::read(surfaces.add(surface_index)) };
                source = unsafe { ptr::read(sources.add(surface_index)) };
            }
            debug_assert!(flat >= surface.first_vertex as usize);
            let local = flat - surface.first_vertex as usize;
            let corner_words = unsafe {
                corners
                    .add(source.first_corner as usize + local)
                    .cast::<u32>()
            };
            let position_and_uv = unsafe { ptr::read(corner_words) };
            let position_index = position_and_uv as u16 as usize;
            debug_assert!(position_index < position_count);
            let position = unsafe { ptr::read(positions.add(position_index)) };
            position_vectors[lane] = classic_position_vec3(position);
            position_xy[lane] = u32::from(position.position[0] as u16)
                | (u32::from(position.position[1] as u16) << 16);
            let source_uv = [(position_and_uv >> 16) as u8, (position_and_uv >> 24) as u8];
            let uv = if source.format & 1 != 0 {
                source_uv
            } else {
                [
                    source_uv[0].wrapping_add(source.uv_offset[0]),
                    source_uv[1].wrapping_add(source.uv_offset[1]),
                ]
            };
            position_z_uv[lane] = u32::from(position.position[2] as u16)
                | (u32::from(uv[0]) << 16)
                | (u32::from(uv[1]) << 24);
            let corner_light = unsafe { ptr::read(corner_words.add(1)) };
            colors[lane] = if source.format & 2 != 0 {
                corner_light
            } else {
                light_color(
                    [corner_light as u8, (corner_light >> 8) as u8],
                    source.light_weights,
                )
            };
            lane += 1;
        }

        let projected = scene::rtpt_kick(
            position_vectors[0],
            position_vectors[1],
            position_vectors[2],
        );
        lane = 0;
        while lane < 3 {
            let destination_words = unsafe { destination.add(index + lane).cast::<u32>() };
            unsafe {
                ptr::write(destination_words, position_xy[lane]);
                ptr::write(destination_words.add(1), position_z_uv[lane]);
                ptr::write(destination_words.add(2), colors[lane]);
            }
            lane += 1;
        }
        let projected = projected.read();
        lane = 0;
        while lane < 3 {
            unsafe { store_classic_projection(destination.add(index + lane), projected[lane]) };
            lane += 1;
        }
        index += 3;
    }

    while index < vertex_count {
        while index >= surface.first_vertex as usize + surface.vertex_count as usize {
            surface_index += 1;
            debug_assert!(surface_index < surface_count);
            surface = unsafe { ptr::read(surfaces.add(surface_index)) };
            source = unsafe { ptr::read(sources.add(surface_index)) };
        }
        let local = index - surface.first_vertex as usize;
        let corner_words = unsafe {
            corners
                .add(source.first_corner as usize + local)
                .cast::<u32>()
        };
        let position_and_uv = unsafe { ptr::read(corner_words) };
        let position_index = position_and_uv as u16 as usize;
        debug_assert!(position_index < position_count);
        let position = unsafe { ptr::read(positions.add(position_index)) };
        let source_uv = [(position_and_uv >> 16) as u8, (position_and_uv >> 24) as u8];
        let uv = if source.format & 1 != 0 {
            source_uv
        } else {
            [
                source_uv[0].wrapping_add(source.uv_offset[0]),
                source_uv[1].wrapping_add(source.uv_offset[1]),
            ]
        };
        let corner_light = unsafe { ptr::read(corner_words.add(1)) };
        let color = if source.format & 2 != 0 {
            corner_light
        } else {
            light_color(
                [corner_light as u8, (corner_light >> 8) as u8],
                source.light_weights,
            )
        };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        unsafe {
            ptr::write(
                destination_words,
                u32::from(position.position[0] as u16)
                    | (u32::from(position.position[1] as u16) << 16),
            );
            ptr::write(
                destination_words.add(1),
                u32::from(position.position[2] as u16)
                    | (u32::from(uv[0]) << 16)
                    | (u32::from(uv[1]) << 24),
            );
            ptr::write(destination_words.add(2), color);
        }
        let projected = project_vertex_scheduled(classic_position_vec3(position));
        unsafe { store_classic_projection(destination.add(index), projected) };
        index += 1;
    }
}

/// Project a materialized classic-affine vertex range in place.
///
/// This is the compatibility tail for faces whose UV or light fields cannot
/// use the baked fused gather above. It exposes the same RTPT/RTPS schedule as
/// [`submit_classic_affine_batch`] without also traversing packet topology.
///
/// # Safety
/// `vertices` must contain `vertex_count` writable records and the GTE must
/// contain the active camera transform and projection state.
pub unsafe fn project_classic_affine_vertices(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
) {
    let mut vertex = 0usize;
    while vertex + 2 < vertex_count {
        unsafe { project_three_consecutive(vertices.add(vertex)) };
        vertex += 3;
    }
    while vertex < vertex_count {
        unsafe { project_one(vertices.add(vertex)) };
        vertex += 1;
    }
}

/// Expand baked indexed corners while assigning a dense projection slot to
/// every distinct shared position in the caller's current batch.
///
/// This fuses the dominant materialisation loop with the only bookkeeping
/// needed for Quake II-style project-once polygon assembly. `position_slots`
/// uses `u8::MAX` for positions not yet admitted. Newly admitted global
/// indices are appended to `unique_positions`; `destination_slots` records
/// the dense projected entry each expanded corner must later consume.
///
/// # Safety
/// The source/destination requirements match
/// [`materialize_classic_affine_indexed_baked_vertices`]. `position_slots`
/// must contain `position_count` writable bytes, `unique_positions` must have
/// `unique_capacity` entries, `unique_count` must be valid and no greater than
/// that capacity, and `destination_slots` must contain `vertex_count` bytes.
pub unsafe fn materialize_classic_affine_indexed_baked_vertices_with_projection_slots(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
    position_slots: *mut u8,
    unique_positions: *mut u16,
    unique_count: *mut usize,
    unique_capacity: usize,
    destination_slots: *mut u8,
) {
    let mut count = unsafe { *unique_count };
    let mut index = 0usize;
    while index < vertex_count {
        let corner_words = unsafe { corners.add(index).cast::<u32>() };
        let position_and_uv = unsafe { ptr::read(corner_words) };
        let position_index = position_and_uv as u16 as usize;
        debug_assert!(position_index < position_count);
        let slot_ptr = unsafe { position_slots.add(position_index) };
        let mut slot = unsafe { *slot_ptr };
        if slot == u8::MAX {
            debug_assert!(count < unique_capacity && count < u8::MAX as usize);
            slot = count as u8;
            unsafe {
                *slot_ptr = slot;
                ptr::write(unique_positions.add(count), position_index as u16);
            }
            count += 1;
        }
        unsafe { ptr::write(destination_slots.add(index), slot) };

        let position = unsafe { ptr::read(positions.add(position_index)) };
        let destination_words = unsafe { destination.add(index).cast::<u32>() };
        let position_xy =
            u32::from(position.position[0] as u16) | (u32::from(position.position[1] as u16) << 16);
        let position_z_uv =
            u32::from(position.position[2] as u16) | (position_and_uv & 0xffff_0000);
        let color = unsafe { ptr::read(corner_words.add(1)) };
        unsafe {
            ptr::write(destination_words, position_xy);
            ptr::write(destination_words.add(1), position_z_uv);
            ptr::write(destination_words.add(2), color);
        }
        index += 1;
    }
    unsafe { *unique_count = count };
}

/// Assign dense projection slots for indexed corners already materialized by
/// the generic UV/light path.
///
/// # Safety
/// The index and slot-buffer requirements match
/// [`materialize_classic_affine_indexed_baked_vertices_with_projection_slots`].
pub unsafe fn collect_classic_affine_indexed_projection_slots(
    corners: *const ClassicAffineIndexedCorner,
    position_count: usize,
    vertex_count: usize,
    position_slots: *mut u8,
    unique_positions: *mut u16,
    unique_count: *mut usize,
    unique_capacity: usize,
    destination_slots: *mut u8,
) {
    let mut count = unsafe { *unique_count };
    let mut index = 0usize;
    while index < vertex_count {
        let position_index = unsafe { (*corners.add(index)).position_index } as usize;
        debug_assert!(position_index < position_count);
        let slot_ptr = unsafe { position_slots.add(position_index) };
        let mut slot = unsafe { *slot_ptr };
        if slot == u8::MAX {
            debug_assert!(count < unique_capacity && count < u8::MAX as usize);
            slot = count as u8;
            unsafe {
                *slot_ptr = slot;
                ptr::write(unique_positions.add(count), position_index as u16);
            }
            count += 1;
        }
        unsafe { ptr::write(destination_slots.add(index), slot) };
        index += 1;
    }
    unsafe { *unique_count = count };
}

/// Expand indexed corners while reusing already projected shared positions.
///
/// # Safety
/// The requirements of [`materialize_classic_affine_indexed_vertices`] apply,
/// and `projected` must contain `position_count` records produced for the
/// active camera.
pub unsafe fn materialize_classic_affine_indexed_projected_vertices(
    corners: *const ClassicAffineIndexedCorner,
    positions: *const ClassicAffinePosition,
    projected: *const ClassicAliasProjectedVertex,
    position_count: usize,
    vertex_count: usize,
    destination: *mut ClassicAffineVertex,
    uv_offset: [u8; 2],
    light_weights: [u16; 2],
    baked_uv: bool,
    baked_light: bool,
) {
    unsafe {
        materialize_classic_affine_indexed_vertices(
            corners,
            positions,
            position_count,
            vertex_count,
            destination,
            uv_offset,
            light_weights,
            baked_uv,
            baked_light,
        );
    }
    let mut index = 0usize;
    while index < vertex_count {
        let corner = unsafe { ptr::read(corners.add(index)) };
        let cached = unsafe { ptr::read(projected.add(corner.position_index as usize)) };
        unsafe {
            (*destination.add(index)).screen = cached.screen;
            (*destination.add(index)).depth = cached.depth as i32;
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

/// Indexed source attributes for one fan in a fused materialize/project batch.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineIndexedBatchSource {
    /// First corner in the retained indexed-corner array.
    pub first_corner: u32,
    /// Texture-atlas offset used when UVs were not baked by the cooker.
    pub uv_offset: [u8; 2],
    /// Bit zero marks baked UVs; bit one marks baked RGB.
    pub format: u16,
    /// Current Q8 contributions of the face's two light styles.
    pub light_weights: [u16; 2],
}

const _: [(); 12] = [(); size_of::<ClassicAffineIndexedBatchSource>()];

/// One compact fan submitted through a persistent theoretical packet layout.
///
/// The caller may mark invariant UV, colour, CLUT, TPAGE, and command fields
/// reusable only when the source surface and its material attributes exactly
/// match the packet slots already resident at the destination address.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineResidentBatchSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan.
    pub vertex_count: u16,
    /// Texture-page word used by this surface.
    pub tpage: u16,
    /// CLUT word used by this surface.
    pub clut: u16,
    /// Non-zero allows invariant packet fields to survive a topology hit.
    pub reuse_invariants: u8,
    /// Explicit deterministic alignment padding.
    pub _padding: [u8; 3],
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
    /// GP0 textured-Gouraud triangle command in the high byte.
    pub color_command_word: u32,
}

/// One PXBSP fan whose packet shape is selected per surface.
///
/// Cooker-proven page-local UVs use compact GP0(34h/3Ch) packets. Tiled,
/// animated, translucent, and other exceptional materials retain the
/// self-contained GP0(E2) selector and reset used by the windowed path.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineMixedBatchSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan.
    pub vertex_count: u16,
    /// Texture-page word used by this surface.
    pub tpage: u16,
    /// CLUT word used by this surface.
    pub clut: u16,
    /// Wrapping U/V offset used only by the windowed packet shape.
    pub uv_offset: [u8; 2],
    /// Non-zero selects compact packets without GP0(E2).
    pub compact: u8,
    /// Non-zero keys this surface's packets at their farthest vertex
    /// ([`ClassicAffineProfile::farthest_depth_key`]); floors and ceilings.
    pub depth_law: u8,
    /// Fully encoded GP0(E2) command used by windowed packets.
    pub texture_window_word: u32,
    /// GP0 textured-Gouraud triangle command in the high byte.
    pub color_command_word: u32,
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
    /// Predicted affine error in texels above which one level is used.
    ///
    /// Zero disables the error trigger and retains the historical depth-only
    /// schedule.
    pub subdivide_once_error_texels: u8,
    /// Predicted affine error in texels above which two levels are used.
    ///
    /// Zero disables the error trigger. This must be no smaller than
    /// [`Self::subdivide_once_error_texels`] when both are enabled.
    pub subdivide_twice_error_texels: u8,
    /// OT slot bias applied to crack-sealing underdraw triangles.
    pub underdraw_slot_bias: u16,
    /// Key each emitted packet at its farthest vertex instead of the vertex
    /// average. Floors and ceilings take this: a large floor triangle keyed at
    /// its average sorts in front of anything standing on its far half and
    /// paints it out (beacons down to a sliver, actors' feet). Keyed at its
    /// far edge the floor draws first and what stands on it draws over it.
    /// Subdivision and quad pairing still use the average, so tessellation
    /// is unchanged.
    pub farthest_depth_key: bool,
}

impl ClassicAffineProfile {
    /// Historical 320x240 Quake-style profile with 2,048 OT slots.
    pub const QUAKE_REFERENCE: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        subdivide_once_at: 136,
        subdivide_twice_at: 60,
        subdivide_once_error_texels: 0,
        subdivide_twice_error_texels: 0,
        underdraw_slot_bias: 8,
        farthest_depth_key: false,
    };

    /// Experimental bounded-lattice affine-error profile.
    ///
    /// The depth bands preserve the historical close-surface workload. The
    /// additional error trigger uses the measured p90 affine-error bound and
    /// spends one split above four predicted texels, then the existing second
    /// split above eight. A bisection can reduce the worst near-side edge
    /// error by as little as half, so the eight-texel threshold keeps each
    /// resulting edge near the four-texel budget without introducing a third
    /// lattice level or changing packet-capacity bounds. This profile is not
    /// selected by a shipping world renderer until that renderer also owns a
    /// hard per-frame extra-packet budget. A fixed-camera Quake measurement
    /// showed that selecting it globally could increase modeled GPU cost by
    /// 82 percent and reach the emulator's 4,096-draw census envelope.
    pub const RUNTIME_ADAPTIVE: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        subdivide_once_at: 136,
        subdivide_twice_at: 60,
        subdivide_once_error_texels: 4,
        subdivide_twice_error_texels: 8,
        underdraw_slot_bias: 8,
        farthest_depth_key: false,
    };

    /// PSoXide brush-world profile: the same topology as
    /// [`Self::QUAKE_REFERENCE`] but each subdivision band reaches twice as
    /// far. Quake's first-person camera looks at walls square-on; a
    /// third-person camera looks down at the floor from a few dozen units
    /// up, and 128-unit patches split only within ~80 units left ~100 px
    /// affine triangles underfoot that swim whenever the view pitches.
    pub const PXBSP_THIRD_PERSON: Self = Self {
        subdivide_once_at: 272,
        subdivide_twice_at: 136,
        ..Self::QUAKE_REFERENCE
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

/// Diagnostic description of the camera-dependent topology selected for a
/// sequence of already projected classic-affine fans.
///
/// The packet and byte counts are the exact pre-screen-rejection topology.
/// Comparing them with the submitter's actual output isolates packets removed
/// by polygon-level screen rejection without adding counters to the shipping
/// packet writer.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineTopologyCensus {
    /// Convex fans examined.
    pub surfaces: u32,
    /// Source fan triangles, including roots rejected before subdivision.
    pub root_triangles: u32,
    /// Fans rejected by the shared four-way screen clip code.
    pub surface_clip_rejects: u32,
    /// Root triangles rejected by the OTZ range.
    pub depth_rejects: u32,
    /// Root triangles emitted without subdivision.
    pub level0_root_triangles: u32,
    /// Root triangles expanded through the one-level lattice.
    pub level1_root_triangles: u32,
    /// Root triangles expanded through the two-level lattice.
    pub level2_root_triangles: u32,
    /// Level-zero GT4 packets formed by pairing adjacent fan triangles.
    pub paired_level0_packets: u32,
    /// One-level roots that require crack-sealing underdraw.
    pub level1_underdraw_roots: u32,
    /// Two-level roots that require crack-sealing underdraw.
    pub level2_underdraw_roots: u32,
    /// Packets selected before individual polygon screen rejection.
    pub theoretical_packets: u32,
    /// Hardware triangles represented by the theoretical packet stream.
    pub theoretical_hardware_triangles: u32,
    /// Bytes occupied by the theoretical compact GT3/GT4 packet stream.
    pub theoretical_packet_bytes: u32,
    /// First deterministic topology fingerprint lane.
    pub topology_hash_a: u32,
    /// Second deterministic topology fingerprint lane.
    pub topology_hash_b: u32,
}

/// One camera-selected adaptive-subdivision root requested by an already
/// projected compact batch.
///
/// This deliberately identifies the root only within its batch surface. The
/// caller owns the stable source-face identity needed for a persistent cache.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineSubdivisionRequest {
    /// Index of the owning surface descriptor in the submitted batch.
    pub batch_surface: u8,
    /// Fan-root index (`0` is corners 0/1/2, `1` is corners 0/2/3).
    pub root: u8,
    /// Selected subdivision lattice level: one or two.
    pub level: u8,
    /// Non-zero when the root also requires crack-sealing underdraw packets.
    pub underdraw: u8,
    /// Ordering-table depth selected for the source root.
    pub otz: u16,
    /// Complete compact packet footprint for this root.
    pub packet_bytes: u16,
    /// Bytes whose UV, colour, CLUT, TPAGE and command fields are invariant.
    pub invariant_bytes: u16,
    /// Explicit padding kept deterministic across targets.
    pub _padding: u16,
    /// Packed CLUT/TPAGE identity from the source surface.
    pub material: u32,
}

/// Dual-display-pool destination selected for one persistent subdivision root.
#[cfg(feature = "classic-affine-subdivision-cache")]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffineSubdivisionRootSlot {
    /// Fixed packet block in the display pool being built now.
    pub active: *mut u32,
    /// True when invariant packet fields already exist in this pool.
    ///
    /// A cold logical allocation initializes each display pool only when that
    /// pool next becomes active, never while the GPU may own the other copy.
    pub resident: bool,
}

/// Destination-owned policy used by the classic-affine subdivision cache.
///
/// The engine owns exact lattice construction and screen rejection; the game
/// owns stable source identities, capacity, retirement and ordering-table
/// insertion because those lifetimes span renderer frames.
#[cfg(feature = "classic-affine-subdivision-cache")]
pub trait ClassicAffineSubdivisionCacheSink {
    /// Resolve or allocate one fixed packet for an unsplit source root. The
    /// default keeps existing subdivision-only sinks source-compatible.
    #[inline(always)]
    fn acquire_base_packet(
        &mut self,
        _source_face: u16,
        _root: u8,
        _quad: bool,
        _material: u32,
        _screen_z: u16,
    ) -> Option<ClassicAffineSubdivisionRootSlot> {
        None
    }

    /// Resolve or allocate the fixed dual-pool block for one source root.
    /// Returning `None` keeps that root on the authoritative dynamic writer.
    fn acquire_root(
        &mut self,
        source_face: u16,
        root: u8,
        level: u8,
        underdraw: bool,
        material: u32,
        screen_z: u16,
    ) -> Option<ClassicAffineSubdivisionRootSlot>;

    /// Insert every still-unlinked dynamic packet before a resident root.
    ///
    /// # Safety
    ///
    /// `end` must terminate the current live staged dynamic stream.
    unsafe fn flush_dynamic_until(&mut self, end: *mut u32);

    /// Insert one admitted packet from a fixed resident root block.
    ///
    /// # Safety
    ///
    /// `packet` must identify a live writable packet in the active display
    /// pool, `otz` must be in range, and `words` must match its packet type.
    unsafe fn insert_resident_packet(&mut self, packet: *mut u32, otz: u16, words: u8);

    /// Insert one contiguous resident root whose packet tags carry their
    /// staged OT slots.
    ///
    /// This is the Quake-II-shaped fast path used when the GPU draw area owns
    /// polygon clipping: every theoretical child packet is present, so the
    /// fixed root block can use the same tight tagged-stream linker as the
    /// dynamic arena instead of returning to Rust once per child.
    ///
    /// # Safety
    ///
    /// `first..end` must be one live contiguous packet stream in the active
    /// display pool. Every tag must contain the exact packet length and a
    /// bounded staged OT slot.
    unsafe fn insert_resident_stream(&mut self, first: *mut u32, end: *mut u32);
}

/// Camera-dependent visible packet layout retained by one compact batch.
///
/// The fingerprints cover every fan/root subdivision decision and the packed
/// polygon-level clip mask. Combined with the caller's exact source/material
/// identity, a matching key proves byte-for-byte slot alignment without
/// rereading and hashing every interpolated UV/colour word.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassicAffineTopologyKey {
    /// Visible compact GT3/GT4 packet slots represented by the key.
    pub packet_slots: u32,
    /// Bytes occupied by the visible compact packet stream.
    pub packet_bytes: u32,
    /// First deterministic subdivision-and-clip layout fingerprint lane.
    pub layout_hash_a: u32,
    /// Second deterministic subdivision-and-clip layout fingerprint lane.
    pub layout_hash_b: u32,
}

/// Result of one persistent-layout compact batch submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffineResidentSubmit {
    /// Actual emitted packet/triangle counts and end of the compact range.
    pub submit: ClassicAffineSubmit,
    /// Shape selected for the current projected batch.
    pub topology_key: ClassicAffineTopologyKey,
    /// True when the supplied prior key matched the current shape exactly.
    pub topology_hit: bool,
    /// Visible compact GT3/GT4 slots resident in the stream.
    pub resident_packet_slots: u32,
    /// Slots whose invariant fields were retained and only tag/XY were patched.
    pub invariant_hit_slots: u32,
    /// Slots fully materialized because topology or surface attributes missed.
    pub invariant_miss_slots: u32,
}

/// Maximum compact topology decisions retained by the bounded planned-batch
/// path. A 39-vertex convex-fan batch has at most 38 events: one admission
/// event per surface plus one event per root triangle.
pub const CLASSIC_AFFINE_PLAN_DECISION_CAPACITY: usize = 48;

/// Maximum polygon admission bits retained by the bounded planned-batch path.
/// The current two-level lattice emits at most sixteen candidate packets per
/// root triangle, so 768 bits safely cover the 39-vertex world-batch contract.
pub const CLASSIC_AFFINE_PLAN_CLIP_CAPACITY: usize = 768;

/// Exact camera-dependent topology proof for one bounded compact batch.
///
/// Decisions use four bits apiece and polygon screen admissions use one bit
/// apiece. This deliberately stores the proof instead of a rolling hash: the
/// hot path compares each decision while it is already being made, retains a
/// small writer state, and cannot accept a collision. Plans which exceed the
/// fixed capacities are marked invalid and remain on the authoritative path.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffinePacketPlan {
    decision_bits: [u8; CLASSIC_AFFINE_PLAN_DECISION_CAPACITY / 2],
    clip_bits: [u8; CLASSIC_AFFINE_PLAN_CLIP_CAPACITY / 8],
    decision_events: u16,
    clip_events: u16,
    packet_slots: u32,
    packet_bytes: u32,
    valid: bool,
}

impl Default for ClassicAffinePacketPlan {
    fn default() -> Self {
        Self {
            decision_bits: [0; CLASSIC_AFFINE_PLAN_DECISION_CAPACITY / 2],
            clip_bits: [0; CLASSIC_AFFINE_PLAN_CLIP_CAPACITY / 8],
            decision_events: 0,
            clip_events: 0,
            packet_slots: 0,
            packet_bytes: 0,
            valid: false,
        }
    }
}

impl ClassicAffinePacketPlan {
    /// Whether the bounded recorder completed without exhausting either bit
    /// stream.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.valid
    }

    #[inline(always)]
    fn decision(&self, index: u16) -> u8 {
        let byte = self.decision_bits[usize::from(index >> 1)];
        (byte >> ((index & 1) * 4)) & 0x0f
    }

    #[inline(always)]
    fn clip_admitted(&self, index: u16) -> bool {
        self.clip_bits[usize::from(index >> 3)] & (1 << (index & 7)) != 0
    }
}

/// Result of one proof-guided resident packet submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ClassicAffinePlannedSubmit {
    /// Actual emitted packet/triangle counts and end of the compact range.
    pub submit: ClassicAffineSubmit,
    /// Exact plan recorded for the current projected batch.
    pub plan: ClassicAffinePacketPlan,
    /// True when the supplied plan matched without a fallback replay.
    pub topology_hit: bool,
    /// Slots whose invariant fields were retained and only tag/XY were patched.
    pub invariant_hit_slots: u32,
    /// Slots fully materialized on a miss or for non-stable surface attributes.
    pub invariant_miss_slots: u32,
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

/// Project the indexed subset of deduplicated positions through the currently
/// loaded GTE scene state, storing results by position index.
///
/// # Safety
/// `positions` must contain `position_count` records, every one of the
/// `index_count` indices must be below `position_count`, and `projected` must
/// contain `position_count` writable records. The GTE scene state must already
/// be loaded for the active camera.
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
        let position_indices = [
            unsafe { *indices.add(index) } as usize,
            unsafe { *indices.add(index + 1) } as usize,
            unsafe { *indices.add(index + 2) } as usize,
        ];
        debug_assert!(position_indices
            .iter()
            .all(|&position_index| position_index < position_count));
        let output = project_triangle_scheduled(
            classic_position_vec3(unsafe { *positions.add(position_indices[0]) }),
            classic_position_vec3(unsafe { *positions.add(position_indices[1]) }),
            classic_position_vec3(unsafe { *positions.add(position_indices[2]) }),
        );
        let mut lane = 0usize;
        while lane < 3 {
            unsafe {
                ptr::write(
                    projected.add(position_indices[lane]),
                    ClassicAliasProjectedVertex {
                        screen: [output[lane].sx, output[lane].sy],
                        depth: output[lane].sz,
                    },
                );
            }
            lane += 1;
        }
        index += 3;
    }
    while index < index_count {
        let position_index = unsafe { *indices.add(index) } as usize;
        debug_assert!(position_index < position_count);
        let output = project_vertex_scheduled(classic_position_vec3(unsafe {
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

/// Project an indexed position subset densely in index-list order.
///
/// Unlike [`project_classic_affine_indexed_vertices`], the destination needs
/// only `index_count` entries. This is the bounded batch form used when a
/// large retained map has only a few dozen selected shared positions.
///
/// # Safety
/// `positions` must contain `position_count` records, every index must be in
/// range, and `projected` must contain `index_count` writable records. The GTE
/// scene state must already describe the active camera.
pub unsafe fn project_classic_affine_indexed_vertices_dense(
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
        let position_indices = [
            unsafe { *indices.add(index) } as usize,
            unsafe { *indices.add(index + 1) } as usize,
            unsafe { *indices.add(index + 2) } as usize,
        ];
        debug_assert!(position_indices
            .iter()
            .all(|&position_index| position_index < position_count));
        let output = project_triangle_scheduled(
            classic_position_vec3(unsafe { *positions.add(position_indices[0]) }),
            classic_position_vec3(unsafe { *positions.add(position_indices[1]) }),
            classic_position_vec3(unsafe { *positions.add(position_indices[2]) }),
        );
        let mut lane = 0usize;
        while lane < 3 {
            unsafe {
                ptr::write(
                    projected.add(index + lane),
                    ClassicAliasProjectedVertex {
                        screen: [output[lane].sx, output[lane].sy],
                        depth: output[lane].sz,
                    },
                );
            }
            lane += 1;
        }
        index += 3;
    }
    while index < index_count {
        let position_index = unsafe { *indices.add(index) } as usize;
        debug_assert!(position_index < position_count);
        let output = project_vertex_scheduled(classic_position_vec3(unsafe {
            *positions.add(position_index)
        }));
        unsafe {
            ptr::write(
                projected.add(index),
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
    unsafe fn emit_tri_unclipped(
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
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        if classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.profile,
        ) {
            return;
        }
        unsafe { self.emit_tri_unclipped(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_quad_unclipped(
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
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        if classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.profile,
        ) {
            return;
        }
        unsafe { self.emit_quad_unclipped(projected, attributes, otz) };
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

struct ResidentPacketWriter {
    next: *mut u32,
    packets: u32,
    hardware_triangles: u32,
    invariant_hit_slots: u32,
    invariant_miss_slots: u32,
    layout_hash_a: u32,
    layout_hash_b: u32,
    decision_bits: u32,
    decision_nibbles: u8,
    clip_bits: u32,
    clip_count: u8,
    clut_high_word: u32,
    tpage_high_word: u32,
    reuse_invariants: bool,
    profile: ClassicAffineProfile,
}

#[inline(always)]
const fn packed_screen(screen: [i16; 2]) -> u32 {
    screen[0] as u16 as u32 | ((screen[1] as u16 as u32) << 16)
}

#[inline(always)]
fn mix_resident_layout_hashes(hash_a: &mut u32, hash_b: &mut u32, value: u32) {
    *hash_a = (*hash_a ^ value).wrapping_mul(0x0100_0193);
    *hash_b = hash_b
        .rotate_left(7)
        .wrapping_add(value.wrapping_mul(0x85eb_ca6b));
}

impl ResidentPacketWriter {
    #[inline(always)]
    fn mix_layout_chunk(&mut self, value: u32) {
        mix_resident_layout_hashes(&mut self.layout_hash_a, &mut self.layout_hash_b, value);
    }

    #[inline(always)]
    fn push_decision(&mut self, value: u8) {
        debug_assert!(value < 16);
        self.decision_bits |= u32::from(value) << (u32::from(self.decision_nibbles) * 4);
        self.decision_nibbles += 1;
        if self.decision_nibbles == 8 {
            self.mix_layout_chunk(self.decision_bits ^ 0xd000_0000);
            self.decision_bits = 0;
            self.decision_nibbles = 0;
        }
    }

    #[inline(always)]
    fn push_clip(&mut self, clipped: bool) {
        self.clip_bits |= u32::from(!clipped) << u32::from(self.clip_count);
        self.clip_count += 1;
        if self.clip_count == 32 {
            self.mix_layout_chunk(self.clip_bits ^ 0xc000_0000);
            self.clip_bits = 0;
            self.clip_count = 0;
        }
    }

    fn finalized_layout_hashes(&self) -> (u32, u32) {
        let mut hash_a = self.layout_hash_a;
        let mut hash_b = self.layout_hash_b;
        if self.decision_nibbles != 0 {
            mix_resident_layout_hashes(
                &mut hash_a,
                &mut hash_b,
                self.decision_bits ^ 0xd100_0000 ^ (u32::from(self.decision_nibbles) << 24),
            );
        }
        if self.clip_count != 0 {
            mix_resident_layout_hashes(
                &mut hash_a,
                &mut hash_b,
                self.clip_bits ^ 0xc100_0000 ^ (u32::from(self.clip_count) << 24),
            );
        }
        (hash_a, hash_b)
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        let clipped = classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.profile,
        );
        self.push_clip(clipped);
        if clipped {
            return;
        }
        let packet_ptr = self.next.cast::<ClassicTriTexturedGouraud>();
        if self.reuse_invariants {
            let packet = unsafe { &mut *packet_ptr };
            packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
            packet.v0 = packed_screen(projected[0].screen);
            packet.v1 = packed_screen(projected[1].screen);
            packet.v2 = packed_screen(projected[2].screen);
            self.invariant_hit_slots = self.invariant_hit_slots.wrapping_add(1);
        } else {
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
            unsafe { ptr::write(packet_ptr, packet) };
            self.invariant_miss_slots = self.invariant_miss_slots.wrapping_add(1);
        }
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
        self.hardware_triangles = self.hardware_triangles.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        let clipped = classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.profile,
        );
        self.push_clip(clipped);
        if clipped {
            return;
        }
        let packet_ptr = self.next.cast::<ClassicQuadTexturedGouraud>();
        if self.reuse_invariants {
            let packet = unsafe { &mut *packet_ptr };
            packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
            packet.v0 = packed_screen(projected[0].screen);
            packet.v1 = packed_screen(projected[1].screen);
            packet.v2 = packed_screen(projected[2].screen);
            packet.v3 = packed_screen(projected[3].screen);
            self.invariant_hit_slots = self.invariant_hit_slots.wrapping_add(1);
        } else {
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
            unsafe { ptr::write(packet_ptr, packet) };
            self.invariant_miss_slots = self.invariant_miss_slots.wrapping_add(1);
        }
        self.next = unsafe {
            self.next
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
        self.hardware_triangles = self.hardware_triangles.wrapping_add(2);
    }

    unsafe fn finish(self, output: *mut u32, topology_hit: bool) -> ClassicAffineResidentSubmit {
        let packet_bytes = unsafe { self.next.offset_from(output) as u32 }.wrapping_mul(4);
        let (layout_hash_a, layout_hash_b) = self.finalized_layout_hashes();
        let topology_key = ClassicAffineTopologyKey {
            packet_slots: self.packets,
            packet_bytes,
            layout_hash_a,
            layout_hash_b,
        };
        ClassicAffineResidentSubmit {
            submit: ClassicAffineSubmit {
                next_packet: self.next,
                packets: self.packets,
                hardware_triangles: self.hardware_triangles,
            },
            topology_key,
            topology_hit,
            resident_packet_slots: self.packets,
            invariant_hit_slots: self.invariant_hit_slots,
            invariant_miss_slots: self.invariant_miss_slots,
        }
    }
}

/// Cold packet writer which records an exact compact topology plan while
/// materializing every packet field. It is intentionally separate from the
/// resident hot writer so plan construction does not increase that writer's
/// live register set.
struct PlannedPacketRecorder {
    writer: PacketWriter,
    plan: ClassicAffinePacketPlan,
    decision_cursor: u16,
    clip_cursor: u16,
    overflow: bool,
}

impl PlannedPacketRecorder {
    #[inline(always)]
    fn record_decision(&mut self, value: u8) {
        debug_assert!(value < 16);
        let index = usize::from(self.decision_cursor);
        if index >= CLASSIC_AFFINE_PLAN_DECISION_CAPACITY {
            self.overflow = true;
        } else {
            let byte = &mut self.plan.decision_bits[index >> 1];
            let shift = (index & 1) * 4;
            *byte = (*byte & !(0x0f << shift)) | (value << shift);
        }
        self.decision_cursor = self.decision_cursor.wrapping_add(1);
    }

    #[inline(always)]
    fn record_clip(&mut self, admitted: bool) {
        let index = usize::from(self.clip_cursor);
        if index >= CLASSIC_AFFINE_PLAN_CLIP_CAPACITY {
            self.overflow = true;
        } else if admitted {
            self.plan.clip_bits[index >> 3] |= 1 << (index & 7);
        }
        self.clip_cursor = self.clip_cursor.wrapping_add(1);
    }

    unsafe fn finish(mut self, output: *mut u32) -> ClassicAffinePlannedSubmit {
        let submit = unsafe { self.writer.finish(output) };
        self.plan.decision_events = self.decision_cursor;
        self.plan.clip_events = self.clip_cursor;
        self.plan.packet_slots = submit.packets;
        self.plan.packet_bytes =
            unsafe { submit.next_packet.offset_from(output) as u32 }.wrapping_mul(4);
        self.plan.valid = !self.overflow;
        ClassicAffinePlannedSubmit {
            submit,
            plan: self.plan,
            topology_hit: false,
            invariant_hit_slots: 0,
            invariant_miss_slots: submit.packets,
        }
    }
}

/// Hot packet writer for an already resident destination layout. Every
/// topology decision is compared directly with the prior exact plan; stable
/// packets retain command/material words and patch only tag/XY. Any mismatch
/// is safe because the caller replays the full recorder before submission.
struct PlannedPacketPatcher<'a> {
    writer: PacketWriter,
    expected: &'a ClassicAffinePacketPlan,
    decision_cursor: u16,
    clip_cursor: u16,
    mismatch: bool,
    reuse_invariants: bool,
    invariant_hit_slots: u32,
    invariant_miss_slots: u32,
}

impl PlannedPacketPatcher<'_> {
    #[inline(always)]
    fn compare_decision(&mut self, value: u8) {
        if self.decision_cursor >= self.expected.decision_events
            || self.expected.decision(self.decision_cursor) != value
        {
            self.mismatch = true;
        }
        self.decision_cursor = self.decision_cursor.wrapping_add(1);
    }

    #[inline(always)]
    fn compare_clip(&mut self, admitted: bool) {
        if self.clip_cursor >= self.expected.clip_events
            || self.expected.clip_admitted(self.clip_cursor) != admitted
        {
            self.mismatch = true;
        }
        self.clip_cursor = self.clip_cursor.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn patch_tri(&mut self, projected: [&ClassicAffineVertex; 3], otz: u16) {
        let packet = unsafe { &mut *self.writer.next.cast::<ClassicTriTexturedGouraud>() };
        packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = packed_screen(projected[0].screen);
        packet.v1 = packed_screen(projected[1].screen);
        packet.v2 = packed_screen(projected[2].screen);
        self.writer.next = unsafe {
            self.writer
                .next
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.writer.packets = self.writer.packets.wrapping_add(1);
        self.invariant_hit_slots = self.invariant_hit_slots.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn patch_quad(&mut self, projected: [&ClassicAffineVertex; 4], otz: u16) {
        let packet = unsafe { &mut *self.writer.next.cast::<ClassicQuadTexturedGouraud>() };
        packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = packed_screen(projected[0].screen);
        packet.v1 = packed_screen(projected[1].screen);
        packet.v2 = packed_screen(projected[2].screen);
        packet.v3 = packed_screen(projected[3].screen);
        self.writer.next = unsafe {
            self.writer
                .next
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
        self.writer.packets = self.writer.packets.wrapping_add(1);
        self.invariant_hit_slots = self.invariant_hit_slots.wrapping_add(1);
    }

    unsafe fn finish(self, output: *mut u32) -> (ClassicAffineSubmit, bool) {
        let decision_cursor = self.decision_cursor;
        let clip_cursor = self.clip_cursor;
        let expected = self.expected;
        let mismatch = self.mismatch;
        let submit = unsafe { self.writer.finish(output) };
        let packet_bytes = unsafe { submit.next_packet.offset_from(output) as u32 }.wrapping_mul(4);
        let matched = !mismatch
            && decision_cursor == expected.decision_events
            && clip_cursor == expected.clip_events
            && submit.packets == expected.packet_slots
            && packet_bytes == expected.packet_bytes;
        (submit, matched)
    }
}

/// Packet sink shared by the compact and self-contained-window variants.
/// Generic subdivision code monomorphises this trait, so the normal compact
/// path keeps its existing packet shape and does not gain a per-polygon
/// material branch.
trait AffinePacketWriter {
    /// True when this writer deliberately delegates per-polygon lattice
    /// rejection to the GPU draw area after the whole surface was admitted.
    const LATTICE_USES_GPU_CLIP: bool = false;

    /// True when the caller's conservative 3D admission permits this writer
    /// to delegate the projected whole-surface reject to the GPU draw area.
    const SURFACE_USES_GPU_CLIP: bool = false;

    fn profile(&self) -> ClassicAffineProfile;

    #[inline(always)]
    fn topology_event(&mut self, _value: u8) {}

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

    #[inline(always)]
    unsafe fn emit_visible_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { self.emit_tri(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_visible_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { self.emit_quad(projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_subdivide_twice(
        &mut self,
        root0: &ClassicAffineVertex,
        root1: &ClassicAffineVertex,
        root2: &ClassicAffineVertex,
        scratch: *mut ClassicAffineVertex,
        root_otz: u16,
        underdraw_edges: u8,
    ) where
        Self: Sized,
    {
        unsafe {
            subdivide_twice(
                self,
                root0,
                root1,
                root2,
                scratch,
                root_otz,
                underdraw_edges,
            )
        };
    }
}

#[cfg(feature = "classic-affine-resident-level2-scatter")]
struct ResidentLevel2Scatter {
    active: *mut u32,
    packets: u32,
    hardware_triangles: u32,
}

#[cfg(feature = "classic-affine-subdivision-cache")]
struct CachedSubdivisionPacketWriter<'a, S: ClassicAffineSubdivisionCacheSink> {
    active: *mut u32,
    initialize: bool,
    packets: u32,
    hardware_triangles: u32,
    clut_high_word: u32,
    tpage_high_word: u32,
    profile: ClassicAffineProfile,
    #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
    sink: &'a mut S,
    #[cfg(feature = "classic-affine-gpu-polygon-clip")]
    _sink: core::marker::PhantomData<&'a mut S>,
}

#[cfg(feature = "classic-affine-subdivision-cache")]
impl<S: ClassicAffineSubdivisionCacheSink> CachedSubdivisionPacketWriter<'_, S> {
    #[inline(always)]
    unsafe fn write_or_patch_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
        clipped: bool,
    ) {
        let active = self.active.cast::<ClassicTriTexturedGouraud>();
        if self.initialize {
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
            unsafe { ptr::write(active, packet) };
        } else if !clipped {
            let packet = unsafe { &mut *active };
            #[cfg(feature = "classic-affine-gpu-polygon-clip")]
            {
                packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
            }
            packet.v0 = packed_screen(projected[0].screen);
            packet.v1 = packed_screen(projected[1].screen);
            packet.v2 = packed_screen(projected[2].screen);
        }
        if !clipped {
            #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
            unsafe {
                self.sink
                    .insert_resident_packet(self.active, otz, ClassicTriTexturedGouraud::WORDS)
            };
            self.packets = self.packets.wrapping_add(1);
            self.hardware_triangles = self.hardware_triangles.wrapping_add(1);
        }
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
    }

    #[inline(always)]
    unsafe fn write_or_patch_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
        clipped: bool,
    ) {
        let active = self.active.cast::<ClassicQuadTexturedGouraud>();
        if self.initialize {
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
            unsafe { ptr::write(active, packet) };
        } else if !clipped {
            let packet = unsafe { &mut *active };
            #[cfg(feature = "classic-affine-gpu-polygon-clip")]
            {
                packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
            }
            packet.v0 = packed_screen(projected[0].screen);
            packet.v1 = packed_screen(projected[1].screen);
            packet.v2 = packed_screen(projected[2].screen);
            packet.v3 = packed_screen(projected[3].screen);
        }
        if !clipped {
            #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
            unsafe {
                self.sink.insert_resident_packet(
                    self.active,
                    otz,
                    ClassicQuadTexturedGouraud::WORDS,
                )
            };
            self.packets = self.packets.wrapping_add(1);
            self.hardware_triangles = self.hardware_triangles.wrapping_add(2);
        }
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
    }
}

#[cfg(feature = "classic-affine-subdivision-cache")]
impl<S: ClassicAffineSubdivisionCacheSink> AffinePacketWriter
    for CachedSubdivisionPacketWriter<'_, S>
{
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
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        let clipped = classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.profile,
        );
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        let clipped = false;
        unsafe { self.write_or_patch_tri(projected, attributes, otz, clipped) };
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        let clipped = classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.profile,
        );
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        let clipped = false;
        unsafe { self.write_or_patch_quad(projected, attributes, otz, clipped) };
    }
}

#[cfg(feature = "classic-affine-resident-base-cache")]
#[inline(always)]
unsafe fn prepare_resident_base_tri(
    slot: ClassicAffineSubdivisionRootSlot,
    projected: [&ClassicAffineVertex; 3],
    attributes: [&ClassicAffineVertex; 3],
    otz: u16,
    clut_high_word: u32,
    tpage_high_word: u32,
) {
    let packet = unsafe { &mut *slot.active.cast::<ClassicTriTexturedGouraud>() };
    if slot.resident {
        packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = packed_screen(projected[0].screen);
        packet.v1 = packed_screen(projected[1].screen);
        packet.v2 = packed_screen(projected[2].screen);
    } else {
        *packet = unsafe {
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
                clut_high_word,
                tpage_high_word,
                otz,
            )
        };
    }
}

#[cfg(feature = "classic-affine-resident-base-cache")]
#[inline(always)]
unsafe fn prepare_resident_base_quad(
    slot: ClassicAffineSubdivisionRootSlot,
    projected: [&ClassicAffineVertex; 4],
    attributes: [&ClassicAffineVertex; 4],
    otz: u16,
    clut_high_word: u32,
    tpage_high_word: u32,
) {
    let packet = unsafe { &mut *slot.active.cast::<ClassicQuadTexturedGouraud>() };
    if slot.resident {
        packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = packed_screen(projected[0].screen);
        packet.v1 = packed_screen(projected[1].screen);
        packet.v2 = packed_screen(projected[2].screen);
        packet.v3 = packed_screen(projected[3].screen);
    } else {
        *packet = unsafe {
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
                clut_high_word,
                tpage_high_word,
                otz,
            )
        };
    }
}

/// Cold dual-pool initializer for one resident L2 root. Cache hits never need
/// UV, colour, CLUT, TPAGE or opcode stores, so keeping this complete writer
/// behind one rare call stops it inflating the fixed scatter caller.
#[cfg(feature = "classic-affine-resident-level2-cold-init")]
#[inline(never)]
unsafe fn initialize_resident_level2_root<S: ClassicAffineSubdivisionCacheSink>(
    active: *mut u32,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
    clut_high_word: u32,
    tpage_high_word: u32,
    profile: ClassicAffineProfile,
) -> ResidentLevel2Scatter {
    let mut cached: CachedSubdivisionPacketWriter<'_, S> = CachedSubdivisionPacketWriter {
        active,
        initialize: true,
        packets: 0,
        hardware_triangles: 0,
        clut_high_word,
        tpage_high_word,
        profile,
        _sink: core::marker::PhantomData,
    };
    unsafe {
        cached.emit_subdivide_twice(root0, root1, root2, scratch, root_otz, 7);
    }
    ResidentLevel2Scatter {
        active: cached.active,
        packets: cached.packets,
        hardware_triangles: cached.hardware_triangles,
    }
}

/// Hot cache-hit writer. Its distinct type lets the compiler erase every
/// invariant UV/colour/material store from the resident subdivision kernels
/// instead of carrying the cold initializer's runtime branch through the
/// 4 KiB PS1 instruction cache.
#[cfg(all(
    feature = "classic-affine-subdivision-cache",
    not(feature = "classic-affine-resident-level2-scatter")
))]
struct CachedSubdivisionPacketPatcher<'a, S: ClassicAffineSubdivisionCacheSink> {
    active: *mut u32,
    packets: u32,
    hardware_triangles: u32,
    profile: ClassicAffineProfile,
    #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
    sink: &'a mut S,
    #[cfg(feature = "classic-affine-gpu-polygon-clip")]
    _sink: core::marker::PhantomData<&'a mut S>,
}

#[cfg(all(
    feature = "classic-affine-subdivision-cache",
    not(feature = "classic-affine-resident-level2-scatter")
))]
impl<S: ClassicAffineSubdivisionCacheSink> CachedSubdivisionPacketPatcher<'_, S> {
    // Each lattice references these emitters many times. One shared copy is
    // substantially cheaper than duplicating the clip, XY patch and OT link
    // sequence at every call site in the PS1's 4 KiB I-cache.
    #[inline(never)]
    unsafe fn patch_tri(&mut self, projected: [&ClassicAffineVertex; 3], otz: u16) {
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        let clipped = classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.profile,
        );
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        let clipped = false;
        if !clipped {
            let packet = unsafe { &mut *self.active.cast::<ClassicTriTexturedGouraud>() };
            #[cfg(feature = "classic-affine-gpu-polygon-clip")]
            {
                packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
            }
            packet.v0 = packed_screen(projected[0].screen);
            packet.v1 = packed_screen(projected[1].screen);
            packet.v2 = packed_screen(projected[2].screen);
            #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
            unsafe {
                self.sink
                    .insert_resident_packet(self.active, otz, ClassicTriTexturedGouraud::WORDS)
            };
            self.packets = self.packets.wrapping_add(1);
            self.hardware_triangles = self.hardware_triangles.wrapping_add(1);
        }
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
    }

    #[inline(never)]
    unsafe fn patch_quad(&mut self, projected: [&ClassicAffineVertex; 4], otz: u16) {
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        let clipped = classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.profile,
        );
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        let clipped = false;
        if !clipped {
            let packet = unsafe { &mut *self.active.cast::<ClassicQuadTexturedGouraud>() };
            #[cfg(feature = "classic-affine-gpu-polygon-clip")]
            {
                packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
            }
            packet.v0 = packed_screen(projected[0].screen);
            packet.v1 = packed_screen(projected[1].screen);
            packet.v2 = packed_screen(projected[2].screen);
            packet.v3 = packed_screen(projected[3].screen);
            #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
            unsafe {
                self.sink.insert_resident_packet(
                    self.active,
                    otz,
                    ClassicQuadTexturedGouraud::WORDS,
                )
            };
            self.packets = self.packets.wrapping_add(1);
            self.hardware_triangles = self.hardware_triangles.wrapping_add(2);
        }
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
    }
}

#[cfg(all(
    feature = "classic-affine-subdivision-cache",
    not(feature = "classic-affine-resident-level2-scatter")
))]
impl<S: ClassicAffineSubdivisionCacheSink> AffinePacketWriter
    for CachedSubdivisionPacketPatcher<'_, S>
{
    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.profile
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        _attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { self.patch_tri(projected, otz) };
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        _attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { self.patch_quad(projected, otz) };
    }
}

/// Fixed packet scatter used only after an L2 resident root has been
/// initialized in the active display pool. It owns no material state and has
/// no clip branches: the admitted source face and PS1 draw area already own
/// those policies for this feature combination.
#[cfg(feature = "classic-affine-resident-level2-scatter")]
impl ResidentLevel2Scatter {
    #[inline(always)]
    unsafe fn patch_tri(&mut self, a: &Projected, b: &Projected, c: &Projected) {
        let otz = scene::classic_otz3_from_sum(u32::from(a.sz) + u32::from(b.sz) + u32::from(c.sz));
        if otz == 0 {
            return;
        }
        let packet = unsafe { &mut *self.active.cast::<ClassicTriTexturedGouraud>() };
        packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = resident_packed_screen(a);
        packet.v1 = resident_packed_screen(b);
        packet.v2 = resident_packed_screen(c);
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
        self.hardware_triangles = self.hardware_triangles.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn patch_quad(&mut self, a: &Projected, b: &Projected, c: &Projected, d: &Projected) {
        let otz =
            ((u32::from(a.sz) + u32::from(b.sz) + u32::from(c.sz) + u32::from(d.sz)) >> 4) as u16;
        if otz == 0 {
            return;
        }
        let packet = unsafe { &mut *self.active.cast::<ClassicQuadTexturedGouraud>() };
        packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = resident_packed_screen(a);
        packet.v1 = resident_packed_screen(b);
        packet.v2 = resident_packed_screen(c);
        packet.v3 = resident_packed_screen(d);
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
        self.hardware_triangles = self.hardware_triangles.wrapping_add(2);
    }

    #[inline(always)]
    unsafe fn patch_tri_at(&mut self, a: &Projected, b: &Projected, c: &Projected, otz: u16) {
        if otz == 0 {
            return;
        }
        let packet = unsafe { &mut *self.active.cast::<ClassicTriTexturedGouraud>() };
        packet.tag = ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = resident_packed_screen(a);
        packet.v1 = resident_packed_screen(b);
        packet.v2 = resident_packed_screen(c);
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
        self.hardware_triangles = self.hardware_triangles.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn patch_quad_at(
        &mut self,
        a: &Projected,
        b: &Projected,
        c: &Projected,
        d: &Projected,
        otz: u16,
    ) {
        if otz == 0 {
            return;
        }
        let packet = unsafe { &mut *self.active.cast::<ClassicQuadTexturedGouraud>() };
        packet.tag = ((ClassicQuadTexturedGouraud::WORDS as u32) << 24) | u32::from(otz);
        packet.v0 = resident_packed_screen(a);
        packet.v1 = resident_packed_screen(b);
        packet.v2 = resident_packed_screen(c);
        packet.v3 = resident_packed_screen(d);
        self.active = unsafe {
            self.active
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>())
        };
        self.packets = self.packets.wrapping_add(1);
        self.hardware_triangles = self.hardware_triangles.wrapping_add(2);
    }
}

#[cfg(feature = "classic-affine-resident-level2-scatter")]
#[inline(always)]
const fn resident_packed_screen(vertex: &Projected) -> u32 {
    vertex.sx as u16 as u32 | ((vertex.sy as u16 as u32) << 16)
}

/// Project and scatter one already-resident Quake-style L2 root.
///
/// This deliberately forms one normal ABI island per root rather than one
/// call per packet. Its scratch traffic is 72 bytes of positions overwritten
/// by 72 bytes of projection results; no UV or RGB midpoint is materialized.
#[cfg(feature = "classic-affine-resident-level2-scatter")]
#[inline(never)]
unsafe fn scatter_resident_level2_root(
    active: *mut u32,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    underdraw_otz: u16,
) -> ResidentLevel2Scatter {
    debug_assert_eq!(size_of::<Vec3I16>(), size_of::<Projected>());
    let positions = scratch.cast::<Vec3I16>();
    let r0 = unsafe { classic_vertex_position(root0) };
    let r1 = unsafe { classic_vertex_position(root1) };
    let r2 = unsafe { classic_vertex_position(root2) };
    unsafe {
        ptr::write(positions, resident_midpoint_position(r0, r1));
        ptr::write(positions.add(1), resident_midpoint_position(r1, r2));
        ptr::write(positions.add(2), resident_midpoint_position(r0, r2));
        ptr::write(positions.add(3), resident_midpoint_position(r0, *positions));
        ptr::write(positions.add(4), resident_midpoint_position(r1, *positions));
        ptr::write(
            positions.add(5),
            resident_midpoint_position(r1, *positions.add(1)),
        );
        ptr::write(
            positions.add(6),
            resident_midpoint_position(*positions.add(1), r2),
        );
        ptr::write(
            positions.add(7),
            resident_midpoint_position(*positions.add(2), r2),
        );
        ptr::write(
            positions.add(8),
            resident_midpoint_position(*positions.add(2), r0),
        );
        ptr::write(
            positions.add(9),
            resident_midpoint_position(*positions.add(2), *positions),
        );
        ptr::write(
            positions.add(10),
            resident_midpoint_position(*positions.add(9), *positions.add(5)),
        );
        ptr::write(
            positions.add(11),
            resident_midpoint_position(*positions.add(2), *positions.add(1)),
        );
    }

    let projected = positions.cast::<Projected>();
    unsafe {
        project_resident_position_triplet(positions, projected, 0);
        project_resident_position_triplet(positions, projected, 3);
        project_resident_position_triplet(positions, projected, 6);
        project_resident_position_triplet(positions, projected, 9);
    }
    let v = unsafe { core::slice::from_raw_parts(projected, EXTRA_VERTICES) };
    let r0 = Projected {
        sx: root0.screen[0],
        sy: root0.screen[1],
        sz: root0.depth as u16,
    };
    let r1 = Projected {
        sx: root1.screen[0],
        sy: root1.screen[1],
        sz: root1.depth as u16,
    };
    let r2 = Projected {
        sx: root2.screen[0],
        sy: root2.screen[1],
        sz: root2.depth as u16,
    };
    let mut writer = ResidentLevel2Scatter {
        active,
        packets: 0,
        hardware_triangles: 0,
    };
    unsafe {
        writer.patch_tri(&r0, &v[3], &v[8]);
        writer.patch_tri(&v[8], &v[9], &v[2]);
        writer.patch_tri(&v[2], &v[11], &v[7]);
        writer.patch_tri(&v[7], &v[6], &r2);
        writer.patch_quad(&v[3], &v[0], &v[8], &v[9]);
        writer.patch_quad(&v[0], &v[4], &v[9], &v[10]);
        writer.patch_quad(&v[4], &r1, &v[10], &v[5]);
        writer.patch_quad(&v[9], &v[10], &v[2], &v[11]);
        writer.patch_quad(&v[10], &v[5], &v[11], &v[1]);
        writer.patch_quad(&v[11], &v[1], &v[7], &v[6]);
        if underdraw_otz != 0 {
            writer.patch_quad_at(&r1, &v[0], &r0, &v[3], underdraw_otz);
            writer.patch_tri_at(&v[0], &r1, &v[4], underdraw_otz);
            writer.patch_quad_at(&r2, &v[1], &r1, &v[5], underdraw_otz);
            writer.patch_tri_at(&v[1], &r2, &v[6], underdraw_otz);
            writer.patch_quad_at(&r2, &v[2], &r0, &v[8], underdraw_otz);
            writer.patch_tri_at(&v[2], &r2, &v[7], underdraw_otz);
        }
    }
    writer
}

impl AffinePacketWriter for PacketWriter {
    #[cfg(feature = "classic-affine-gpu-lattice-clip")]
    const LATTICE_USES_GPU_CLIP: bool = true;

    #[cfg(feature = "classic-affine-gpu-surface-clip")]
    const SURFACE_USES_GPU_CLIP: bool = true;

    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.profile
    }

    #[cfg_attr(feature = "classic-affine-compact-subdivision-emitters", inline(never))]
    #[cfg_attr(
        not(feature = "classic-affine-compact-subdivision-emitters"),
        inline(always)
    )]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        unsafe {
            self.emit_tri_unclipped(projected, attributes, otz)
        };
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        unsafe {
            PacketWriter::emit_tri(self, projected, attributes, otz)
        };
    }

    #[cfg_attr(feature = "classic-affine-compact-subdivision-emitters", inline(never))]
    #[cfg_attr(
        not(feature = "classic-affine-compact-subdivision-emitters"),
        inline(always)
    )]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        #[cfg(feature = "classic-affine-gpu-polygon-clip")]
        unsafe {
            self.emit_quad_unclipped(projected, attributes, otz)
        };
        #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
        unsafe {
            PacketWriter::emit_quad(self, projected, attributes, otz)
        };
    }

    #[cfg(feature = "classic-affine-gpu-lattice-clip")]
    #[inline(always)]
    unsafe fn emit_visible_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { self.emit_tri_unclipped(projected, attributes, otz) };
    }

    #[cfg(feature = "classic-affine-gpu-lattice-clip")]
    #[inline(always)]
    unsafe fn emit_visible_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { self.emit_quad_unclipped(projected, attributes, otz) };
    }

    #[cfg(feature = "classic-affine-compact-world-level2-kernel")]
    #[inline(always)]
    unsafe fn emit_subdivide_twice(
        &mut self,
        root0: &ClassicAffineVertex,
        root1: &ClassicAffineVertex,
        root2: &ClassicAffineVertex,
        scratch: *mut ClassicAffineVertex,
        root_otz: u16,
        underdraw_edges: u8,
    ) {
        unsafe {
            subdivide_twice_packet_writer(
                self,
                root0,
                root1,
                root2,
                scratch,
                root_otz,
                underdraw_edges,
            )
        };
    }
}

impl AffinePacketWriter for ResidentPacketWriter {
    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.profile
    }

    #[inline(always)]
    fn topology_event(&mut self, value: u8) {
        self.push_decision(value);
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        unsafe { ResidentPacketWriter::emit_tri(self, projected, attributes, otz) };
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        unsafe { ResidentPacketWriter::emit_quad(self, projected, attributes, otz) };
    }
}

impl AffinePacketWriter for PlannedPacketRecorder {
    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.writer.profile
    }

    #[inline(always)]
    fn topology_event(&mut self, value: u8) {
        self.record_decision(value);
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        let clipped = classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.writer.profile,
        );
        self.record_clip(!clipped);
        if !clipped {
            unsafe { self.writer.emit_tri_unclipped(projected, attributes, otz) };
        }
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        let clipped = classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.writer.profile,
        );
        self.record_clip(!clipped);
        if !clipped {
            unsafe { self.writer.emit_quad_unclipped(projected, attributes, otz) };
        }
    }
}

impl AffinePacketWriter for PlannedPacketPatcher<'_> {
    #[inline(always)]
    fn profile(&self) -> ClassicAffineProfile {
        self.writer.profile
    }

    #[inline(always)]
    fn topology_event(&mut self, value: u8) {
        self.compare_decision(value);
    }

    #[inline(always)]
    unsafe fn emit_tri(
        &mut self,
        projected: [&ClassicAffineVertex; 3],
        attributes: [&ClassicAffineVertex; 3],
        otz: u16,
    ) {
        let clipped = classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.writer.profile,
        );
        self.compare_clip(!clipped);
        if clipped {
            return;
        }
        if self.reuse_invariants {
            unsafe { self.patch_tri(projected, otz) };
        } else {
            unsafe { self.writer.emit_tri_unclipped(projected, attributes, otz) };
            self.invariant_miss_slots = self.invariant_miss_slots.wrapping_add(1);
        }
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        let clipped = classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.writer.profile,
        );
        self.compare_clip(!clipped);
        if clipped {
            return;
        }
        if self.reuse_invariants {
            unsafe { self.patch_quad(projected, otz) };
        } else {
            unsafe { self.writer.emit_quad_unclipped(projected, attributes, otz) };
            self.invariant_miss_slots = self.invariant_miss_slots.wrapping_add(1);
        }
    }
}

struct WindowedPacketWriter<const RESTORE_WINDOW: bool> {
    next: *mut u32,
    packets: u32,
    clut_high_word: u32,
    tpage_high_word: u32,
    uv_offset: [u8; 2],
    texture_window_word: u32,
    color_command_word: u32,
    profile: ClassicAffineProfile,
}

impl<const RESTORE_WINDOW: bool> AffinePacketWriter for WindowedPacketWriter<RESTORE_WINDOW> {
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
        if classic_tri_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
            ],
            self.profile,
        ) {
            return;
        }
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let mut packet = unsafe {
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
        if RESTORE_WINDOW {
            packet.tag = packet.tag.wrapping_add(1 << 24) | TAG_SCOPED_TEXTURE_WINDOW;
        }
        packet.color0_cmd = self.color_command_word | attributes[0].color;
        unsafe { ptr::write(self.next.cast::<TriTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<TriTexturedGouraud>() / size_of::<u32>())
        };
        if RESTORE_WINDOW {
            unsafe { ptr::write(self.next, TextureWindow::NONE.word()) };
            self.next = unsafe { self.next.add(1) };
        }
        self.packets = self.packets.wrapping_add(1);
    }

    #[inline(always)]
    unsafe fn emit_quad(
        &mut self,
        projected: [&ClassicAffineVertex; 4],
        attributes: [&ClassicAffineVertex; 4],
        otz: u16,
    ) {
        if classic_quad_screen_clipped(
            [
                projected[0].screen,
                projected[1].screen,
                projected[2].screen,
                projected[3].screen,
            ],
            self.profile,
        ) {
            return;
        }
        debug_assert!(attributes
            .iter()
            .all(|vertex| vertex.color & 0xff00_0000 == 0));
        let mut packet = unsafe {
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
        if RESTORE_WINDOW {
            packet.tag = packet.tag.wrapping_add(1 << 24) | TAG_SCOPED_TEXTURE_WINDOW;
        }
        packet.color0_cmd = (self.color_command_word | 0x0800_0000) | attributes[0].color;
        unsafe { ptr::write(self.next.cast::<QuadTexturedGouraud>(), packet) };
        self.next = unsafe {
            self.next
                .add(size_of::<QuadTexturedGouraud>() / size_of::<u32>())
        };
        if RESTORE_WINDOW {
            unsafe { ptr::write(self.next, TextureWindow::NONE.word()) };
            self.next = unsafe { self.next.add(1) };
        }
        self.packets = self.packets.wrapping_add(1);
    }
}

impl<const RESTORE_WINDOW: bool> WindowedPacketWriter<RESTORE_WINDOW> {
    #[inline(always)]
    unsafe fn finish(self, output: *mut u32) -> ClassicAffineSubmit {
        let words = unsafe { self.next.offset_from(output) as u32 };
        let restore_words = u32::from(RESTORE_WINDOW);
        let tri_words = (size_of::<TriTexturedGouraud>() / size_of::<u32>()) as u32 + restore_words;
        let quad_words =
            (size_of::<QuadTexturedGouraud>() / size_of::<u32>()) as u32 + restore_words;
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
    // Average every colour channel. The colour word carries baked RGB light
    // on PXBSP faces; averaging one byte and replicating it turned every
    // generated near-band vertex grey, so the floor and rails darkened in a
    // band that followed the camera.
    let color = ((a.color & 0x00ff_00ff) + (b.color & 0x00ff_00ff)) >> 1 & 0x00ff_00ff
        | ((a.color & 0x0000_ff00) + (b.color & 0x0000_ff00)) >> 1 & 0x0000_ff00;
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
        color,
        screen: [0; 2],
        depth: 0,
    }
}

#[inline(always)]
unsafe fn project_three_consecutive(vertices: *mut ClassicAffineVertex) {
    let a = unsafe { classic_vertex_position(vertices) };
    let b = unsafe { classic_vertex_position(vertices.add(1)) };
    let c = unsafe { classic_vertex_position(vertices.add(2)) };
    let out = project_triangle_scheduled(a, b, c);
    unsafe {
        store_classic_projection(vertices, out[0]);
        store_classic_projection(vertices.add(1), out[1]);
        store_classic_projection(vertices.add(2), out[2]);
    }
}

/// Position-only midpoint used by the resident L2 cache-hit kernel.
///
/// Resident packets already contain their UV, colour, CLUT, TPAGE and GP0
/// opcode words. Reconstructing those attributes in twelve full
/// [`ClassicAffineVertex`] records on every hit is therefore dead traffic.
#[cfg(feature = "classic-affine-resident-level2-scatter")]
#[inline(always)]
fn resident_midpoint_position(a: Vec3I16, b: Vec3I16) -> Vec3I16 {
    Vec3I16::new(
        ((i32::from(a.x) + i32::from(b.x)) >> 1) as i16,
        ((i32::from(a.y) + i32::from(b.y)) >> 1) as i16,
        ((i32::from(a.z) + i32::from(b.z)) >> 1) as i16,
    )
}

/// Project one compact position triplet in place. `Vec3I16` and `Projected`
/// are both six-byte POD records, so the caller can build the complete
/// dependent lattice first and then overwrite the same 72-byte scratch span
/// with the twelve camera-dependent results.
#[cfg(feature = "classic-affine-resident-level2-scatter")]
#[inline(always)]
unsafe fn project_resident_position_triplet(
    positions: *mut Vec3I16,
    projected: *mut Projected,
    first: usize,
) {
    let out = project_triangle_scheduled(
        unsafe { ptr::read(positions.add(first)) },
        unsafe { ptr::read(positions.add(first + 1)) },
        unsafe { ptr::read(positions.add(first + 2)) },
    );
    unsafe {
        ptr::write(projected.add(first), out[0]);
        ptr::write(projected.add(first + 1), out[1]);
        ptr::write(projected.add(first + 2), out[2]);
    }
}

#[inline(always)]
unsafe fn project_one(vertex: *mut ClassicAffineVertex) {
    let source = unsafe { classic_vertex_position(vertex) };
    let out = project_vertex_scheduled(source);
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
    average3_depths(
        vertices[0].depth as u16,
        vertices[1].depth as u16,
        vertices[2].depth as u16,
    )
}

/// Convert three cached GTE depths to the classic OT key.
///
/// The host form intentionally retains the arithmetic oracle so all existing
/// packet-parity tests stay independent of global emulated GTE state. On PS1,
/// the feature A/B reloads SZ1..SZ3 and executes AVSZ3 with the installed
/// ZSF3=0x155; this is mathematically identical to the software path.
#[inline(always)]
fn average3_depths(a: u16, b: u16, c: u16) -> u16 {
    #[cfg(all(feature = "classic-affine-gte-otz", target_arch = "mips"))]
    {
        scene::average_cached_z3([a, b, c])
    }
    #[cfg(not(all(feature = "classic-affine-gte-otz", target_arch = "mips")))]
    {
        scene::classic_otz3_from_sum(u32::from(a) + u32::from(b) + u32::from(c))
    }
}

#[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
trait ClassicAffineSample {
    fn affine_uv(&self) -> [u8; 2];
    fn affine_depth(&self) -> i32;
}

#[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
impl ClassicAffineSample for ClassicAffineVertex {
    #[inline(always)]
    fn affine_uv(&self) -> [u8; 2] {
        self.uv
    }

    #[inline(always)]
    fn affine_depth(&self) -> i32 {
        self.depth
    }
}

#[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
impl ClassicAffineSample for ClassicAffineProjectedVertex {
    #[inline(always)]
    fn affine_uv(&self) -> [u8; 2] {
        self.uv
    }

    #[inline(always)]
    fn affine_depth(&self) -> i32 {
        self.depth
    }
}

/// Select the existing zero-, one-, or two-level lattice from depth and a
/// calibrated affine texture-error bound.
///
/// For an edge spanning `du` texels between positive depths `za` and `zb`,
/// its exact midpoint error is `du * |zb-za| / (2 * (za+zb))`. The measured
/// p90 worst-polygon multiplier is 2.4, so the bound exceeds `target` exactly
/// when `6 * du * |zb-za| > 5 * target * (za+zb)`. This comparison avoids a
/// guest division and stays within `u32` for PS1 UV and GTE depth ranges.
#[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
#[inline(always)]
fn classic_affine_subdivision_level<T: ClassicAffineSample>(
    vertices: [&T; 3],
    otz: u16,
    profile: ClassicAffineProfile,
) -> u8 {
    let mut level = if otz < profile.subdivide_twice_at {
        2
    } else if otz < profile.subdivide_once_at {
        1
    } else {
        0
    };
    if level == 2 || profile.subdivide_once_error_texels == 0 {
        return level;
    }

    debug_assert!(
        profile.subdivide_twice_error_texels == 0
            || profile.subdivide_twice_error_texels >= profile.subdivide_once_error_texels
    );
    for (a, b) in [(0usize, 1usize), (1, 2), (2, 0)] {
        let za = vertices[a].affine_depth();
        let zb = vertices[b].affine_depth();
        if za <= 0 || zb <= 0 {
            continue;
        }
        // Projected GTE depths are u16. Clamp caller-supplied projected
        // records as well so the public preprojected path cannot overflow the
        // fixed-width comparison even if its safety contract is violated.
        let za = (za as u32).min(u16::MAX as u32);
        let zb = (zb as u32).min(u16::MAX as u32);
        let uv_a = vertices[a].affine_uv();
        let uv_b = vertices[b].affine_uv();
        let du = u32::from(uv_a[0].abs_diff(uv_b[0]).max(uv_a[1].abs_diff(uv_b[1])));
        let scaled_error = du * za.abs_diff(zb) * 6;
        let scaled_depth = (za + zb) * 5;
        if profile.subdivide_twice_error_texels != 0
            && scaled_error > scaled_depth * u32::from(profile.subdivide_twice_error_texels)
        {
            return 2;
        }
        if scaled_error > scaled_depth * u32::from(profile.subdivide_once_error_texels) {
            level = 1;
        }
    }
    level
}

/// Quake's shipping profile selects subdivision from OTZ alone. Compiling
/// that contract as a distinct kernel keeps the unused affine-error loop out
/// of every inlined root and adjacent-root decision while preserving the
/// exact `QUAKE_REFERENCE` topology.
#[cfg(feature = "classic-affine-depth-only-subdivision")]
#[inline(always)]
fn classic_affine_subdivision_level<T>(
    _vertices: [&T; 3],
    otz: u16,
    profile: ClassicAffineProfile,
) -> u8 {
    debug_assert_eq!(profile.subdivide_once_error_texels, 0);
    debug_assert_eq!(profile.subdivide_twice_error_texels, 0);
    if otz < profile.subdivide_twice_at {
        2
    } else if otz < profile.subdivide_once_at {
        1
    } else {
        0
    }
}

#[inline(always)]
unsafe fn sorted_tri<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 3],
    attributes: [&ClassicAffineVertex; 3],
    lattice_visible: bool,
) {
    let otz = average3(projected);
    if otz > 0 {
        if lattice_visible {
            unsafe { writer.emit_visible_tri(projected, attributes, otz) };
        } else {
            unsafe { writer.emit_tri(projected, attributes, otz) };
        }
    }
}

#[inline(always)]
unsafe fn sorted_quad<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 4],
    attributes: [&ClassicAffineVertex; 4],
    lattice_visible: bool,
) {
    let depth_sum = projected[0].depth as u16 as u32
        + projected[1].depth as u16 as u32
        + projected[2].depth as u16 as u32
        + projected[3].depth as u16 as u32;
    let otz = (depth_sum >> 4) as u16;
    if otz > 0 {
        if lattice_visible {
            unsafe { writer.emit_visible_quad(projected, attributes, otz) };
        } else {
            unsafe { writer.emit_quad(projected, attributes, otz) };
        }
    }
}

#[inline(always)]
unsafe fn projected_lattice_inside<W: AffinePacketWriter>(
    _writer: &W,
    _roots: [&ClassicAffineVertex; 3],
    _generated: *const ClassicAffineVertex,
    _generated_count: usize,
) -> bool {
    W::LATTICE_USES_GPU_CLIP
}

#[inline(always)]
unsafe fn emit_classified_tri<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 3],
    attributes: [&ClassicAffineVertex; 3],
    otz: u16,
    lattice_visible: bool,
) {
    if lattice_visible {
        unsafe { writer.emit_visible_tri(projected, attributes, otz) };
    } else {
        unsafe { writer.emit_tri(projected, attributes, otz) };
    }
}

#[inline(always)]
unsafe fn emit_classified_quad<W: AffinePacketWriter>(
    writer: &mut W,
    projected: [&ClassicAffineVertex; 4],
    attributes: [&ClassicAffineVertex; 4],
    otz: u16,
    lattice_visible: bool,
) {
    if lattice_visible {
        unsafe { writer.emit_visible_quad(projected, attributes, otz) };
    } else {
        unsafe { writer.emit_quad(projected, attributes, otz) };
    }
}

#[cfg_attr(feature = "classic-affine-compact-subdivision-kernels", inline(never))]
unsafe fn subdivide_once<W: AffinePacketWriter>(
    writer: &mut W,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
    underdraw_edges: u8,
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
    let lattice_visible =
        unsafe { projected_lattice_inside(writer, [root0, root1, root2], scratch, 3) };
    unsafe {
        sorted_quad(
            writer,
            [root0, h01, h20, h12],
            [root0, h01, h20, h12],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [h01, root1, h12],
            [h01, root1, h12],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [h12, root2, h20],
            [h12, root2, h20],
            lattice_visible,
        );
    }
    let profile = writer.profile();
    let underdraw_at = i32::from(profile.subdivide_once_at);
    if root0.depth >= underdraw_at || root1.depth >= underdraw_at || root2.depth >= underdraw_at {
        let underdraw = root_otz.saturating_add(profile.underdraw_slot_bias);
        unsafe {
            if underdraw_edges & 1 != 0 {
                emit_classified_tri(
                    writer,
                    [root0, root1, h01],
                    [root0, root1, h01],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 2 != 0 {
                emit_classified_tri(
                    writer,
                    [root1, root2, h12],
                    [root0, root1, h12],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 4 != 0 {
                emit_classified_tri(
                    writer,
                    [root2, root0, h20],
                    [root2, root0, h20],
                    underdraw,
                    lattice_visible,
                );
            }
        }
    }
}

#[cfg_attr(
    any(
        feature = "classic-affine-compact-subdivision-kernels",
        feature = "classic-affine-compact-level2-kernel"
    ),
    inline(never)
)]
unsafe fn subdivide_twice<W: AffinePacketWriter>(
    writer: &mut W,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
    underdraw_edges: u8,
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
    let lattice_visible =
        unsafe { projected_lattice_inside(writer, [root0, root1, root2], scratch, EXTRA_VERTICES) };
    unsafe {
        sorted_tri(
            writer,
            [root0, &v[3], &v[8]],
            [root0, &v[3], &v[8]],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [&v[8], &v[9], &v[2]],
            [&v[8], &v[9], &v[2]],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [&v[2], &v[11], &v[7]],
            [&v[2], &v[11], &v[7]],
            lattice_visible,
        );
        sorted_tri(
            writer,
            [&v[7], &v[6], root2],
            [&v[7], &v[6], root2],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[3], &v[0], &v[8], &v[9]],
            [&v[3], &v[0], &v[8], &v[9]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[0], &v[4], &v[9], &v[10]],
            [&v[0], &v[4], &v[9], &v[10]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[4], root1, &v[10], &v[5]],
            [&v[4], root1, &v[10], &v[5]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[9], &v[10], &v[2], &v[11]],
            [&v[9], &v[10], &v[2], &v[11]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[10], &v[5], &v[11], &v[1]],
            [&v[10], &v[5], &v[11], &v[1]],
            lattice_visible,
        );
        sorted_quad(
            writer,
            [&v[11], &v[1], &v[7], &v[6]],
            [&v[11], &v[1], &v[7], &v[6]],
            lattice_visible,
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
            if underdraw_edges & 1 != 0 {
                emit_classified_quad(
                    writer,
                    [root1, &v[0], root0, &v[3]],
                    [root1, &v[0], root0, &v[3]],
                    underdraw,
                    lattice_visible,
                );
                emit_classified_tri(
                    writer,
                    [&v[0], root1, &v[4]],
                    [&v[0], root1, &v[4]],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 2 != 0 {
                emit_classified_quad(
                    writer,
                    [root2, &v[1], root1, &v[5]],
                    [root2, &v[1], root1, &v[5]],
                    underdraw,
                    lattice_visible,
                );
                emit_classified_tri(
                    writer,
                    [&v[1], root2, &v[6]],
                    [&v[1], root2, &v[6]],
                    underdraw,
                    lattice_visible,
                );
            }
            if underdraw_edges & 4 != 0 {
                emit_classified_quad(
                    writer,
                    [root2, &v[2], root0, &v[8]],
                    [root2, &v[2], root0, &v[8]],
                    underdraw,
                    lattice_visible,
                );
                emit_classified_tri(
                    writer,
                    [&v[2], root2, &v[7]],
                    [&v[2], root2, &v[7]],
                    underdraw,
                    lattice_visible,
                );
            }
        }
    }
}

#[cfg(feature = "classic-affine-compact-world-level2-kernel")]
#[inline(never)]
unsafe fn subdivide_twice_packet_writer(
    writer: &mut PacketWriter,
    root0: &ClassicAffineVertex,
    root1: &ClassicAffineVertex,
    root2: &ClassicAffineVertex,
    scratch: *mut ClassicAffineVertex,
    root_otz: u16,
    underdraw_edges: u8,
) {
    unsafe {
        subdivide_twice(
            writer,
            root0,
            root1,
            root2,
            scratch,
            root_otz,
            underdraw_edges,
        )
    };
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
    if !W::SURFACE_USES_GPU_CLIP {
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < vertex_count && surface_clip != 0 {
            surface_clip &=
                classic_clip_code(unsafe { (*vertices.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip != 0 {
            writer.topology_event(15);
            return;
        }
    }
    writer.topology_event(14);

    let root = unsafe { &*vertices };
    let root_depth = root.depth as u16;
    let end = unsafe { vertices.add(vertex_count) };
    let mut previous = unsafe { vertices.add(1) };
    let mut current = unsafe { vertices.add(2) };
    #[cfg(feature = "classic-affine-shared-subdivision-edges")]
    let mut previous_subdivision_level = None;
    while current != end {
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root_depth,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        // The packet key: the average, or the farthest vertex on the same
        // law (three equal depths give the same slot either way).
        let key_otz = if profile.farthest_depth_key {
            let far = root_depth
                .max(previous_ref.depth as u16)
                .max(current_ref.depth as u16);
            scene::classic_otz3_from_sum(u32::from(far) * 3)
        } else {
            otz
        };
        if otz > 0 && otz < profile.ot_depth {
            let subdivision_level =
                classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile);
            let next = unsafe { current.add(1) };
            if subdivision_level == 0 && next != end {
                let next_ref = unsafe { &*next };
                let next_otz =
                    average3_depths(root_depth, current_ref.depth as u16, next_ref.depth as u16);
                let next_level = classic_affine_subdivision_level(
                    [root, current_ref, next_ref],
                    next_otz,
                    profile,
                );
                let compatible_depth = next_otz == otz
                    || (cfg!(feature = "classic-affine-relaxed-quad-pairing")
                        && next_otz.abs_diff(otz) <= 1);
                if next_otz > 0
                    && next_otz < profile.ot_depth
                    && compatible_depth
                    && next_level == 0
                {
                    // GP0 quads split on q1-q2. Reorder two adjacent fan
                    // triangles so that edge lands on the fan's shared 0-2
                    // diagonal and its two internal triangles match the
                    // staged OT stream's reverse link order. This keeps the
                    // affine interpolation anchors bit-exact at the seam.
                    writer.topology_event(2);
                    let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                    #[cfg(feature = "classic-affine-relaxed-quad-pairing")]
                    let quad_otz = ((u32::from(root_depth)
                        + previous_ref.depth as u16 as u32
                        + current_ref.depth as u16 as u32
                        + next_ref.depth as u16 as u32)
                        >> 4) as u16;
                    #[cfg(not(feature = "classic-affine-relaxed-quad-pairing"))]
                    let quad_otz = otz;
                    let quad_otz = if profile.farthest_depth_key {
                        (u32::from(
                            root_depth
                                .max(previous_ref.depth as u16)
                                .max(current_ref.depth as u16)
                                .max(next_ref.depth as u16),
                        ) >> 2) as u16
                    } else {
                        quad_otz
                    };
                    unsafe { writer.emit_quad(quad_refs, quad_refs, quad_otz) };
                    #[cfg(feature = "classic-affine-shared-subdivision-edges")]
                    {
                        previous_subdivision_level = Some(0);
                    }
                    previous = next;
                    current = unsafe { next.add(1) };
                    continue;
                }
            }
            #[cfg(feature = "classic-affine-shared-subdivision-edges")]
            let underdraw_edges = if subdivision_level == 0 {
                7
            } else {
                let previous_shared = previous_subdivision_level == Some(subdivision_level);
                let next_shared = if next == end {
                    false
                } else {
                    let next_ref = unsafe { &*next };
                    let next_otz = average3_depths(
                        root_depth,
                        current_ref.depth as u16,
                        next_ref.depth as u16,
                    );
                    next_otz > 0
                        && next_otz < profile.ot_depth
                        && classic_affine_subdivision_level(
                            [root, current_ref, next_ref],
                            next_otz,
                            profile,
                        ) == subdivision_level
                };
                2 | u8::from(!previous_shared) | (u8::from(!next_shared) << 2)
            };
            #[cfg(not(feature = "classic-affine-shared-subdivision-edges"))]
            let underdraw_edges = 7;
            if subdivision_level == 2 {
                let underdraw_at = i32::from(profile.subdivide_twice_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                writer.topology_event(if underdraw { 6 } else { 5 });
                unsafe {
                    writer.emit_subdivide_twice(
                        root,
                        previous_ref,
                        current_ref,
                        generated,
                        key_otz,
                        underdraw_edges,
                    )
                };
            } else if subdivision_level == 1 {
                let underdraw_at = i32::from(profile.subdivide_once_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                writer.topology_event(if underdraw { 4 } else { 3 });
                unsafe {
                    subdivide_once(
                        writer,
                        root,
                        previous_ref,
                        current_ref,
                        generated,
                        key_otz,
                        underdraw_edges,
                    )
                };
            } else {
                writer.topology_event(1);
                let root_refs = [root, previous_ref, current_ref];
                unsafe { writer.emit_tri(root_refs, root_refs, key_otz) };
            }
            #[cfg(feature = "classic-affine-shared-subdivision-edges")]
            {
                previous_subdivision_level = Some(subdivision_level);
            }
        } else {
            writer.topology_event(0);
            #[cfg(feature = "classic-affine-shared-subdivision-edges")]
            {
                previous_subdivision_level = None;
            }
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
}

/// Submit a projected fan through the compact level-zero-only path.
///
/// The first pass proves that every depth-admitted root uses the level-zero
/// topology before the writer is touched. Returning `false` therefore leaves
/// the output cursor unchanged and lets the complete lattice path replay the
/// fan without rollback. Whole-surface and per-root rejection remain valid
/// level-zero outcomes.
#[cfg(feature = "classic-affine-level0-fast-path")]
#[inline(never)]
unsafe fn submit_classic_affine_level0_fan_if_supported(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    writer: &mut PacketWriter,
) -> bool {
    let profile = writer.profile;
    let mut surface_clip = 0x0fu8;
    let mut clip_index = 0usize;
    while clip_index < vertex_count && surface_clip != 0 {
        surface_clip &= classic_clip_code(unsafe { (*vertices.add(clip_index)).screen }, profile);
        clip_index += 1;
    }
    if surface_clip != 0 {
        return true;
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
        if otz > 0
            && otz < profile.ot_depth
            && classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile)
                != 0
        {
            return false;
        }
        previous = current;
        current = unsafe { current.add(1) };
    }

    previous = unsafe { vertices.add(1) };
    current = unsafe { vertices.add(2) };
    while current != end {
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = scene::classic_otz3_from_sum(
            root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
        );
        if otz > 0 && otz < profile.ot_depth {
            let next = unsafe { current.add(1) };
            if next != end {
                let next_ref = unsafe { &*next };
                let next_otz = scene::classic_otz3_from_sum(
                    root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
                );
                let compatible_depth = next_otz == otz
                    || (cfg!(feature = "classic-affine-relaxed-quad-pairing")
                        && next_otz.abs_diff(otz) <= 1);
                if next_otz > 0 && next_otz < profile.ot_depth && compatible_depth {
                    let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                    #[cfg(feature = "classic-affine-relaxed-quad-pairing")]
                    let quad_otz = ((root_depth
                        + previous_ref.depth as u16 as u32
                        + current_ref.depth as u16 as u32
                        + next_ref.depth as u16 as u32)
                        >> 4) as u16;
                    #[cfg(not(feature = "classic-affine-relaxed-quad-pairing"))]
                    let quad_otz = otz;
                    let quad_otz = if profile.farthest_depth_key {
                        (u32::from(
                            root_depth
                                .max(previous_ref.depth as u16)
                                .max(current_ref.depth as u16)
                                .max(next_ref.depth as u16),
                        ) >> 2) as u16
                    } else {
                        quad_otz
                    };
                    unsafe { writer.emit_quad(quad_refs, quad_refs, quad_otz) };
                    previous = next;
                    current = unsafe { next.add(1) };
                    continue;
                }
            }
            let root_refs = [root, previous_ref, current_ref];
            unsafe { writer.emit_tri(root_refs, root_refs, otz) };
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
    true
}

#[cfg(any(
    feature = "classic-affine-level0-fast-path",
    feature = "classic-affine-speculative-level0"
))]
#[inline(never)]
unsafe fn submit_classic_affine_projected_fan_slow(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    unsafe {
        submit_classic_affine_projected_fan_into_writer(vertices, vertex_count, generated, writer);
    }
}

/// Emit the common level-zero fan in one pass, rolling the still-unlinked
/// packet cursor back if any admitted root requests the adaptive lattice.
///
/// Unlike the older level-zero preflight this does not traverse successful
/// fans twice. A failed speculation only dirties words beyond the restored
/// cursor; the complete writer immediately overwrites the live prefix and no
/// DMA tag has been linked yet.
#[cfg(feature = "classic-affine-speculative-level0")]
#[inline(always)]
unsafe fn submit_classic_affine_speculative_level0_fan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    let profile = writer.profile;
    let saved_next = writer.next;
    let saved_packets = writer.packets;

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
            if classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile)
                != 0
            {
                writer.next = saved_next;
                writer.packets = saved_packets;
                unsafe {
                    submit_classic_affine_projected_fan_slow(
                        vertices,
                        vertex_count,
                        generated,
                        writer,
                    )
                };
                return;
            }

            let next = unsafe { current.add(1) };
            if next != end {
                let next_ref = unsafe { &*next };
                let next_otz = scene::classic_otz3_from_sum(
                    root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
                );
                if next_otz > 0 && next_otz < profile.ot_depth {
                    if classic_affine_subdivision_level(
                        [root, current_ref, next_ref],
                        next_otz,
                        profile,
                    ) != 0
                    {
                        writer.next = saved_next;
                        writer.packets = saved_packets;
                        unsafe {
                            submit_classic_affine_projected_fan_slow(
                                vertices,
                                vertex_count,
                                generated,
                                writer,
                            )
                        };
                        return;
                    }
                    let compatible_depth = next_otz == otz
                        || (cfg!(feature = "classic-affine-relaxed-quad-pairing")
                            && next_otz.abs_diff(otz) <= 1);
                    if compatible_depth {
                        let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                        #[cfg(feature = "classic-affine-relaxed-quad-pairing")]
                        let quad_otz = ((root_depth
                            + previous_ref.depth as u16 as u32
                            + current_ref.depth as u16 as u32
                            + next_ref.depth as u16 as u32)
                            >> 4) as u16;
                        #[cfg(not(feature = "classic-affine-relaxed-quad-pairing"))]
                        let quad_otz = otz;
                        unsafe { writer.emit_quad(quad_refs, quad_refs, quad_otz) };
                        previous = next;
                        current = unsafe { next.add(1) };
                        continue;
                    }
                }
            }
            let tri_refs = [root, previous_ref, current_ref];
            unsafe { writer.emit_tri(tri_refs, tri_refs, otz) };
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
}

/// Submit one projected convex fan as fixed adjacent GT4 pairs and an
/// optional GT3 remainder.
///
/// This is an architectural ceiling for cooker-authored tessellation, not a
/// visual-parity mode: it deliberately removes the runtime affine lattice and
/// uses each quad's natural four-corner depth key, like the recovered Quake II
/// PSX brush packets.
#[cfg(feature = "classic-affine-fixed-fan-quads")]
#[inline(always)]
unsafe fn submit_classic_affine_fixed_fan_quads(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    writer: &mut PacketWriter,
) {
    let profile = writer.profile;
    let mut surface_clip = 0x0fu8;
    let mut clip_index = 0usize;
    while clip_index < vertex_count && surface_clip != 0 {
        surface_clip &= classic_clip_code(unsafe { (*vertices.add(clip_index)).screen }, profile);
        clip_index += 1;
    }
    if surface_clip != 0 {
        return;
    }

    unsafe { submit_classic_affine_fixed_fan_quads_visible(vertices, vertex_count, writer) };
}

/// Emit a fixed fan which the caller has already proven not to lie wholly on
/// one side of the viewport. Keeping the clip walk out of this body lets the
/// guarded path classify and submit a face in one pass.
#[cfg(any(
    feature = "classic-affine-fixed-fan-quads",
    feature = "classic-affine-fixed-fan-guarded"
))]
#[inline(always)]
unsafe fn submit_classic_affine_fixed_fan_quads_visible(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    writer: &mut PacketWriter,
) {
    let profile = writer.profile;

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
        let next = unsafe { current.add(1) };
        if otz > 0 && otz < profile.ot_depth && next != end {
            let next_ref = unsafe { &*next };
            let next_otz = scene::classic_otz3_from_sum(
                root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
            );
            if next_otz > 0 && next_otz < profile.ot_depth {
                let depth_sum = root_depth
                    + previous_ref.depth as u16 as u32
                    + current_ref.depth as u16 as u32
                    + next_ref.depth as u16 as u32;
                let quad_otz = (depth_sum >> 4) as u16;
                let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                unsafe { writer.emit_quad(quad_refs, quad_refs, quad_otz) };
                previous = next;
                current = unsafe { next.add(1) };
                continue;
            }
        }
        if otz > 0 && otz < profile.ot_depth {
            let tri_refs = [root, previous_ref, current_ref];
            unsafe { writer.emit_tri(tri_refs, tri_refs, otz) };
        }
        previous = current;
        current = next;
    }
}

/// Use the fixed Quake II-style fan for ordinary faces, but retain the full
/// reference lattice when every source corner is outside on different sides
/// or the GTE has saturated a projected coordinate. Those exceptional faces
/// are where generated interior vertices recover coverage or avoid the worst
/// near-camera affine texture distortion. This intentionally avoids a broad
/// per-face screen bounding box: the GTE saturation boundary is both cheaper
/// to classify and more directly tied to the projection failure mode.
#[cfg(feature = "classic-affine-fixed-fan-guarded")]
#[inline(always)]
unsafe fn submit_classic_affine_fixed_fan_guarded(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    let profile = writer.profile;
    let mut surface_clip = 0x0fu8;
    let mut any_inside = false;
    let mut gte_saturated = false;
    let mut index = 0usize;
    while index < vertex_count {
        let screen = unsafe { (*vertices.add(index)).screen };
        let clip = classic_clip_code(screen, profile);
        surface_clip &= clip;
        any_inside |= clip == 0;
        // SXY is clamped to this signed 11-bit range by the GTE. Exact
        // endpoints therefore identify projection saturation without another
        // span calculation or a camera-dependent viewport heuristic.
        gte_saturated |= classic_gte_screen_saturated(screen);
        index += 1;
    }
    if surface_clip != 0 {
        return;
    }
    if !any_inside || gte_saturated {
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices,
                vertex_count,
                generated,
                writer,
            )
        };
    } else {
        unsafe { submit_classic_affine_fixed_fan_quads_visible(vertices, vertex_count, writer) };
    }
}

/// Submit ordinary fans through a fixed-packet path while retaining the
/// complete two-level lattice for the closest depth band.
///
/// This is a quality/performance point between the reference renderer and
/// [`submit_classic_affine_fixed_fan_quads`]. Level-one roots are deliberately
/// flattened and may pair with adjacent level-zero or level-one roots. A
/// level-two root retains the reference sixteen-triangle lattice and crack
/// underdraw, preventing the most conspicuous near-camera affine distortion.
#[cfg(feature = "classic-affine-fixed-fan-level2")]
#[inline(always)]
unsafe fn submit_classic_affine_fixed_fan_level2(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    let profile = writer.profile;
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
        if otz == 0 || otz >= profile.ot_depth {
            previous = current;
            current = unsafe { current.add(1) };
            continue;
        }

        let level =
            classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile);
        if level == 2 {
            unsafe {
                writer.emit_subdivide_twice(root, previous_ref, current_ref, generated, otz, 7);
            }
            previous = current;
            current = unsafe { current.add(1) };
            continue;
        }

        let next = unsafe { current.add(1) };
        if next != end {
            let next_ref = unsafe { &*next };
            let next_otz = scene::classic_otz3_from_sum(
                root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
            );
            if next_otz > 0
                && next_otz < profile.ot_depth
                && classic_affine_subdivision_level(
                    [root, current_ref, next_ref],
                    next_otz,
                    profile,
                ) != 2
            {
                let depth_sum = root_depth
                    + previous_ref.depth as u16 as u32
                    + current_ref.depth as u16 as u32
                    + next_ref.depth as u16 as u32;
                let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                unsafe { writer.emit_quad(quad_refs, quad_refs, (depth_sum >> 4) as u16) };
                previous = next;
                current = unsafe { next.add(1) };
                continue;
            }
        }

        let tri_refs = [root, previous_ref, current_ref];
        unsafe { writer.emit_tri(tri_refs, tri_refs, otz) };
        previous = current;
        current = next;
    }
}

#[inline(always)]
fn topology_census_mix(census: &mut ClassicAffineTopologyCensus, value: u32) {
    if census.topology_hash_a == 0 && census.topology_hash_b == 0 {
        census.topology_hash_a = 0x811c_9dc5;
        census.topology_hash_b = 0x9e37_79b9;
    }
    census.topology_hash_a = (census.topology_hash_a ^ value).wrapping_mul(0x0100_0193);
    census.topology_hash_b = census
        .topology_hash_b
        .rotate_left(7)
        .wrapping_add(value.wrapping_mul(0x85eb_ca6b));
}

#[inline(always)]
fn topology_census_packets(
    census: &mut ClassicAffineTopologyCensus,
    packets: u32,
    hardware_triangles: u32,
    bytes: u32,
) {
    census.theoretical_packets = census.theoretical_packets.wrapping_add(packets);
    census.theoretical_hardware_triangles = census
        .theoretical_hardware_triangles
        .wrapping_add(hardware_triangles);
    census.theoretical_packet_bytes = census.theoretical_packet_bytes.wrapping_add(bytes);
}

/// Accumulate the exact adaptive topology decisions for already projected
/// compact fans without emitting another packet stream.
///
/// This is a diagnostic counterpart to [`submit_classic_affine_batch`]. It
/// shares the submitter's subdivision selector, fan pairing, OTZ rejection,
/// whole-surface clipping, and underdraw predicates. Individual polygon screen
/// rejection is deliberately left to the real submit result, so the difference
/// between theoretical and actual bytes measures that final camera-dependent
/// stage.
///
/// # Safety
///
/// `vertices` and `surfaces` must obey the source-range contract of
/// [`submit_classic_affine_projected_batch`], and every source vertex must
/// already contain the current camera's projected screen coordinate and depth.
pub unsafe fn census_classic_affine_projected_batch_topology(
    vertices: *const ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    profile: ClassicAffineProfile,
    census: &mut ClassicAffineTopologyCensus,
) {
    if vertices.is_null() || surfaces.is_null() || vertex_count == 0 || surface_count == 0 {
        return;
    }

    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        census.surfaces = census.surfaces.wrapping_add(1);
        census.root_triangles = census
            .root_triangles
            .wrapping_add(surface.vertex_count.saturating_sub(2) as u32);
        topology_census_mix(census, 0x1000_0000 | u32::from(surface.vertex_count));

        let fan = unsafe { vertices.add(first_vertex) };
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < surface_vertices && surface_clip != 0 {
            surface_clip &= classic_clip_code(unsafe { (*fan.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip != 0 {
            census.surface_clip_rejects = census.surface_clip_rejects.wrapping_add(1);
            topology_census_mix(census, 0xf000_0000);
            surface_ptr = unsafe { surface_ptr.add(1) };
            continue;
        }

        let root = unsafe { &*fan };
        let root_depth = root.depth as u16 as u32;
        let end = unsafe { fan.add(surface_vertices) };
        let mut previous = unsafe { fan.add(1) };
        let mut current = unsafe { fan.add(2) };
        while current != end {
            let previous_ref = unsafe { &*previous };
            let current_ref = unsafe { &*current };
            let otz = scene::classic_otz3_from_sum(
                root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
            );
            if otz == 0 || otz >= profile.ot_depth {
                census.depth_rejects = census.depth_rejects.wrapping_add(1);
                topology_census_mix(census, 0xe000_0000);
                previous = current;
                current = unsafe { current.add(1) };
                continue;
            }

            let level =
                classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile);
            let next = unsafe { current.add(1) };
            if level == 0 && next != end {
                let next_ref = unsafe { &*next };
                let next_otz = scene::classic_otz3_from_sum(
                    root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
                );
                if next_otz == otz
                    && classic_affine_subdivision_level(
                        [root, current_ref, next_ref],
                        next_otz,
                        profile,
                    ) == 0
                {
                    census.level0_root_triangles = census.level0_root_triangles.wrapping_add(2);
                    census.paired_level0_packets = census.paired_level0_packets.wrapping_add(1);
                    topology_census_packets(census, 1, 2, 52);
                    topology_census_mix(census, 0x4000_0000);
                    previous = next;
                    current = unsafe { next.add(1) };
                    continue;
                }
            }

            if level == 2 {
                census.level2_root_triangles = census.level2_root_triangles.wrapping_add(1);
                topology_census_packets(census, 10, 16, 472);
                let underdraw_at = i32::from(profile.subdivide_twice_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                if underdraw {
                    census.level2_underdraw_roots = census.level2_underdraw_roots.wrapping_add(1);
                    topology_census_packets(census, 6, 9, 276);
                }
                topology_census_mix(census, 0x3000_0000 | (u32::from(underdraw) << 27));
            } else if level == 1 {
                census.level1_root_triangles = census.level1_root_triangles.wrapping_add(1);
                topology_census_packets(census, 3, 4, 132);
                let underdraw_at = i32::from(profile.subdivide_once_at);
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                if underdraw {
                    census.level1_underdraw_roots = census.level1_underdraw_roots.wrapping_add(1);
                    topology_census_packets(census, 3, 3, 120);
                }
                topology_census_mix(census, 0x2000_0000 | (u32::from(underdraw) << 27));
            } else {
                census.level0_root_triangles = census.level0_root_triangles.wrapping_add(1);
                topology_census_packets(census, 1, 1, 40);
                topology_census_mix(census, 0);
            }
            previous = current;
            current = unsafe { current.add(1) };
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
    }
}

/// Collect adaptive-subdivision roots from already projected compact fans.
///
/// The returned count is the number of requests found, which may exceed
/// `output_capacity`; only the prefix that fits is written. Level-zero roots,
/// whole-surface screen rejects and invalid OT depths do not request cache
/// residency. The packet and invariant-byte shapes exactly match the compact
/// GT3/GT4 lattices used by the authoritative submitter.
///
/// # Safety
///
/// `vertices` and `surfaces` must obey the source-range contract of
/// [`submit_classic_affine_projected_batch`]. When `output_capacity` is not
/// zero, `output` must point to that many writable request records.
pub unsafe fn collect_classic_affine_projected_subdivision_requests(
    vertices: *const ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    profile: ClassicAffineProfile,
    output: *mut ClassicAffineSubdivisionRequest,
    output_capacity: usize,
) -> usize {
    if vertices.is_null()
        || surfaces.is_null()
        || vertex_count == 0
        || surface_count == 0
        || (output.is_null() && output_capacity != 0)
    {
        return 0;
    }

    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    let mut surface_index = 0usize;
    let mut request_count = 0usize;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);

        let fan = unsafe { vertices.add(first_vertex) };
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < surface_vertices && surface_clip != 0 {
            surface_clip &= classic_clip_code(unsafe { (*fan.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip == 0 {
            let root = unsafe { &*fan };
            let root_depth = root.depth as u16 as u32;
            let end = unsafe { fan.add(surface_vertices) };
            let mut previous = unsafe { fan.add(1) };
            let mut current = unsafe { fan.add(2) };
            let mut root_index = 0usize;
            while current != end {
                let previous_ref = unsafe { &*previous };
                let current_ref = unsafe { &*current };
                let otz = scene::classic_otz3_from_sum(
                    root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
                );
                if otz != 0 && otz < profile.ot_depth {
                    let level = classic_affine_subdivision_level(
                        [root, previous_ref, current_ref],
                        otz,
                        profile,
                    );
                    if level != 0 {
                        let underdraw_at = i32::from(if level == 2 {
                            profile.subdivide_twice_at
                        } else {
                            profile.subdivide_once_at
                        });
                        let underdraw = root.depth >= underdraw_at
                            || previous_ref.depth >= underdraw_at
                            || current_ref.depth >= underdraw_at;
                        let (base_bytes, underdraw_bytes, invariant_base, invariant_underdraw) =
                            if level == 2 {
                                (472u16, 276u16, 288u16, 168u16)
                            } else {
                                (132u16, 120u16, 80u16, 72u16)
                            };
                        if request_count < output_capacity {
                            unsafe {
                                ptr::write(
                                    output.add(request_count),
                                    ClassicAffineSubdivisionRequest {
                                        batch_surface: surface_index as u8,
                                        root: root_index as u8,
                                        level,
                                        underdraw: u8::from(underdraw),
                                        otz,
                                        packet_bytes: base_bytes
                                            + if underdraw { underdraw_bytes } else { 0 },
                                        invariant_bytes: invariant_base
                                            + if underdraw { invariant_underdraw } else { 0 },
                                        _padding: 0,
                                        material: u32::from(surface.tpage)
                                            | (u32::from(surface.clut) << 16),
                                    },
                                );
                            }
                        }
                        request_count += 1;
                    }
                }
                previous = current;
                current = unsafe { current.add(1) };
                root_index += 1;
            }
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
        surface_index += 1;
    }
    request_count
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
#[cfg_attr(feature = "classic-affine-quake-specialized-kernel", inline(always))]
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
        #[cfg(all(
            not(feature = "classic-affine-level0-fast-path"),
            not(feature = "classic-affine-speculative-level0"),
            not(feature = "classic-affine-fixed-fan-quads"),
            not(feature = "classic-affine-fixed-fan-guarded"),
            not(feature = "classic-affine-fixed-fan-level2")
        ))]
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut writer,
            );
        }
        #[cfg(feature = "classic-affine-level0-fast-path")]
        unsafe {
            let fan = vertices.add(first_vertex);
            if !submit_classic_affine_level0_fan_if_supported(fan, surface_vertices, &mut writer) {
                submit_classic_affine_projected_fan_slow(
                    fan,
                    surface_vertices,
                    generated,
                    &mut writer,
                );
            }
        }
        #[cfg(all(
            feature = "classic-affine-speculative-level0",
            not(feature = "classic-affine-level0-fast-path"),
            not(feature = "classic-affine-fixed-fan-quads"),
            not(feature = "classic-affine-fixed-fan-guarded"),
            not(feature = "classic-affine-fixed-fan-level2")
        ))]
        unsafe {
            let fan = vertices.add(first_vertex);
            submit_classic_affine_speculative_level0_fan(
                fan,
                surface_vertices,
                generated,
                &mut writer,
            );
        }
        #[cfg(all(
            feature = "classic-affine-fixed-fan-quads",
            not(feature = "classic-affine-level0-fast-path"),
            not(feature = "classic-affine-speculative-level0"),
            not(feature = "classic-affine-fixed-fan-guarded"),
            not(feature = "classic-affine-fixed-fan-level2")
        ))]
        unsafe {
            submit_classic_affine_fixed_fan_quads(
                vertices.add(first_vertex),
                surface_vertices,
                &mut writer,
            );
        }
        #[cfg(all(
            feature = "classic-affine-fixed-fan-guarded",
            not(feature = "classic-affine-level0-fast-path"),
            not(feature = "classic-affine-speculative-level0"),
            not(feature = "classic-affine-fixed-fan-quads"),
            not(feature = "classic-affine-fixed-fan-level2")
        ))]
        unsafe {
            submit_classic_affine_fixed_fan_guarded(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut writer,
            );
        }
        #[cfg(all(
            feature = "classic-affine-fixed-fan-level2",
            not(feature = "classic-affine-level0-fast-path"),
            not(feature = "classic-affine-speculative-level0"),
            not(feature = "classic-affine-fixed-fan-quads"),
            not(feature = "classic-affine-fixed-fan-guarded")
        ))]
        unsafe {
            submit_classic_affine_fixed_fan_level2(
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

/// Consume the maximal consecutive run of Quake level-zero roots beginning
/// at `current`. Invalid-depth roots are consumed without output. The first
/// L1/L2 root is returned untouched so the established adaptive lattice can
/// handle it without rollback or a whole-fan proof traversal.
#[cfg(feature = "classic-affine-quake-level0-run")]
#[inline(never)]
unsafe fn submit_quake_level0_run(
    root: *const ClassicAffineVertex,
    mut current: *mut ClassicAffineVertex,
    end: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) -> *mut ClassicAffineVertex {
    let root_ref = unsafe { &*root };
    let root_depth = root_ref.depth as u16;
    while current != end {
        let previous = unsafe { current.sub(1) };
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root_depth,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        if otz > 0 && otz < ClassicAffineProfile::QUAKE_REFERENCE.subdivide_once_at {
            break;
        }
        let next = unsafe { current.add(1) };
        if otz > 0 && otz < ClassicAffineProfile::QUAKE_REFERENCE.ot_depth {
            if next != end {
                let next_ref = unsafe { &*next };
                let next_otz =
                    average3_depths(root_depth, current_ref.depth as u16, next_ref.depth as u16);
                if next_otz >= ClassicAffineProfile::QUAKE_REFERENCE.subdivide_once_at
                    && next_otz < ClassicAffineProfile::QUAKE_REFERENCE.ot_depth
                    && next_otz == otz
                {
                    let quad = [previous_ref, current_ref, root_ref, next_ref];
                    unsafe { writer.emit_quad_unclipped(quad, quad, otz) };
                    current = unsafe { next.add(1) };
                    continue;
                }
            }
            let tri = [root_ref, previous_ref, current_ref];
            unsafe { writer.emit_tri_unclipped(tri, tri, otz) };
        }
        current = next;
    }
    current
}

#[cfg(feature = "classic-affine-quake-level0-run")]
#[inline(always)]
unsafe fn submit_quake_projected_fan_level0_runs(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    let profile = ClassicAffineProfile::QUAKE_REFERENCE;
    let mut surface_clip = 0x0fu8;
    let mut clip_index = 0usize;
    while clip_index < vertex_count && surface_clip != 0 {
        surface_clip &= classic_clip_code(unsafe { (*vertices.add(clip_index)).screen }, profile);
        clip_index += 1;
    }
    if surface_clip != 0 {
        return;
    }

    let root = vertices.cast_const();
    let end = unsafe { vertices.add(vertex_count) };
    let mut current = unsafe { vertices.add(2) };
    while current != end {
        current = unsafe { submit_quake_level0_run(root, current, end, writer) };
        if current == end {
            break;
        }
        let previous = unsafe { current.sub(1) };
        let root_ref = unsafe { &*root };
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root_ref.depth as u16,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        debug_assert!(otz > 0 && otz < profile.subdivide_once_at);
        if otz < profile.subdivide_twice_at {
            unsafe {
                subdivide_twice(
                    writer,
                    root_ref,
                    previous_ref,
                    current_ref,
                    generated,
                    otz,
                    7,
                )
            };
        } else {
            unsafe {
                subdivide_once(
                    writer,
                    root_ref,
                    previous_ref,
                    current_ref,
                    generated,
                    otz,
                    7,
                )
            };
        }
        current = unsafe { current.add(1) };
    }
}

#[cfg(feature = "classic-affine-quake-cold-adaptive")]
#[inline(never)]
unsafe fn submit_quake_adaptive_root(
    writer: &mut PacketWriter,
    root: &ClassicAffineVertex,
    previous: &ClassicAffineVertex,
    current: &ClassicAffineVertex,
    generated: *mut ClassicAffineVertex,
    otz: u16,
) {
    if otz < ClassicAffineProfile::QUAKE_REFERENCE.subdivide_twice_at {
        unsafe { subdivide_twice(writer, root, previous, current, generated, otz, 7) };
    } else {
        unsafe { subdivide_once(writer, root, previous, current, generated, otz, 7) };
    }
}

/// Quake-specific fan loop with its level-zero GT3/GT4 path kept local and
/// only adaptive roots crossing the cold call boundary.
#[cfg(feature = "classic-affine-quake-cold-adaptive")]
#[inline(always)]
unsafe fn submit_quake_projected_fan_cold_adaptive(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    let profile = ClassicAffineProfile::QUAKE_REFERENCE;
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
    let root_depth = root.depth as u16;
    let end = unsafe { vertices.add(vertex_count) };
    let mut previous = unsafe { vertices.add(1) };
    let mut current = unsafe { vertices.add(2) };
    while current != end {
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root_depth,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        // The packet key: the average, or the farthest vertex on the same
        // law (three equal depths give the same slot either way).
        let key_otz = if profile.farthest_depth_key {
            let far = root_depth
                .max(previous_ref.depth as u16)
                .max(current_ref.depth as u16);
            scene::classic_otz3_from_sum(u32::from(far) * 3)
        } else {
            otz
        };
        if otz > 0 && otz < profile.ot_depth {
            if otz >= profile.subdivide_once_at {
                let next = unsafe { current.add(1) };
                if next != end {
                    let next_ref = unsafe { &*next };
                    let next_otz = average3_depths(
                        root_depth,
                        current_ref.depth as u16,
                        next_ref.depth as u16,
                    );
                    if next_otz >= profile.subdivide_once_at
                        && next_otz < profile.ot_depth
                        && next_otz == otz
                    {
                        let quad = [previous_ref, current_ref, root, next_ref];
                        unsafe { writer.emit_quad_unclipped(quad, quad, otz) };
                        previous = next;
                        current = unsafe { next.add(1) };
                        continue;
                    }
                }
                let tri = [root, previous_ref, current_ref];
                unsafe { writer.emit_tri_unclipped(tri, tri, otz) };
            } else {
                unsafe {
                    submit_quake_adaptive_root(
                        writer,
                        root,
                        previous_ref,
                        current_ref,
                        generated,
                        otz,
                    )
                };
            }
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
}

#[cfg(feature = "classic-affine-quake-cold-level2")]
#[inline(never)]
unsafe fn submit_quake_level2_root(
    writer: &mut PacketWriter,
    root: &ClassicAffineVertex,
    previous: &ClassicAffineVertex,
    current: &ClassicAffineVertex,
    generated: *mut ClassicAffineVertex,
    otz: u16,
) {
    unsafe { subdivide_twice(writer, root, previous, current, generated, otz, 7) };
}

/// Quake fan loop retaining the frequent L0 path and compact L1 lattice in
/// one hot function while isolating only the large L2 schedule.
#[cfg(feature = "classic-affine-quake-cold-level2")]
#[inline(always)]
unsafe fn submit_quake_projected_fan_cold_level2(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    generated: *mut ClassicAffineVertex,
    writer: &mut PacketWriter,
) {
    let profile = ClassicAffineProfile::QUAKE_REFERENCE;
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
    let root_depth = root.depth as u16;
    let end = unsafe { vertices.add(vertex_count) };
    let mut previous = unsafe { vertices.add(1) };
    let mut current = unsafe { vertices.add(2) };
    while current != end {
        let previous_ref = unsafe { &*previous };
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root_depth,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        // The packet key: the average, or the farthest vertex on the same
        // law (three equal depths give the same slot either way).
        let key_otz = if profile.farthest_depth_key {
            let far = root_depth
                .max(previous_ref.depth as u16)
                .max(current_ref.depth as u16);
            scene::classic_otz3_from_sum(u32::from(far) * 3)
        } else {
            otz
        };
        if otz > 0 && otz < profile.ot_depth {
            if otz >= profile.subdivide_once_at {
                let next = unsafe { current.add(1) };
                if next != end {
                    let next_ref = unsafe { &*next };
                    let next_otz = average3_depths(
                        root_depth,
                        current_ref.depth as u16,
                        next_ref.depth as u16,
                    );
                    if next_otz >= profile.subdivide_once_at
                        && next_otz < profile.ot_depth
                        && next_otz == otz
                    {
                        let quad = [previous_ref, current_ref, root, next_ref];
                        unsafe { writer.emit_quad_unclipped(quad, quad, otz) };
                        previous = next;
                        current = unsafe { next.add(1) };
                        continue;
                    }
                }
                let tri = [root, previous_ref, current_ref];
                unsafe { writer.emit_tri_unclipped(tri, tri, otz) };
            } else if otz >= profile.subdivide_twice_at {
                unsafe {
                    subdivide_once(writer, root, previous_ref, current_ref, generated, otz, 7)
                };
            } else {
                unsafe {
                    submit_quake_level2_root(
                        writer,
                        root,
                        previous_ref,
                        current_ref,
                        generated,
                        otz,
                    )
                };
            }
        }
        previous = current;
        current = unsafe { current.add(1) };
    }
}

/// Quake-specific ordinary batch entry point with a compile-time renderer
/// profile. This is intentionally a separate non-calling ownership boundary:
/// the inlined generic body can discard runtime tests for affine-error policy,
/// viewport dimensions, OT depth and subdivision bands while the public
/// general entry point remains available to other PSoXide users.
///
/// # Safety
/// The pointer, capacity and scratch contracts are identical to
/// [`submit_classic_affine_batch`].
#[cfg(feature = "classic-affine-quake-specialized-kernel")]
#[inline(never)]
pub unsafe fn submit_quake_classic_affine_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    surface_count: usize,
    output: *mut u32,
) -> ClassicAffineSubmit {
    #[cfg(not(any(
        feature = "classic-affine-quake-level0-run",
        feature = "classic-affine-quake-cold-adaptive",
        feature = "classic-affine-quake-cold-level2"
    )))]
    unsafe {
        submit_classic_affine_batch(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            ClassicAffineProfile::QUAKE_REFERENCE,
        )
    }
    #[cfg(any(
        feature = "classic-affine-quake-level0-run",
        feature = "classic-affine-quake-cold-adaptive",
        feature = "classic-affine-quake-cold-level2"
    ))]
    {
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
            profile: ClassicAffineProfile::QUAKE_REFERENCE,
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
            #[cfg(feature = "classic-affine-quake-level0-run")]
            unsafe {
                submit_quake_projected_fan_level0_runs(
                    vertices.add(first_vertex),
                    surface_vertices,
                    generated,
                    &mut writer,
                )
            };
            #[cfg(feature = "classic-affine-quake-cold-adaptive")]
            unsafe {
                submit_quake_projected_fan_cold_adaptive(
                    vertices.add(first_vertex),
                    surface_vertices,
                    generated,
                    &mut writer,
                )
            };
            #[cfg(feature = "classic-affine-quake-cold-level2")]
            unsafe {
                submit_quake_projected_fan_cold_level2(
                    vertices.add(first_vertex),
                    surface_vertices,
                    generated,
                    &mut writer,
                )
            };
            surface_ptr = unsafe { surface_ptr.add(1) };
        }
        unsafe { writer.finish(output) }
    }
}

/// Project and submit compact fans while retaining adaptive subdivision roots
/// in destination-owned dual-pool packet slabs.
///
/// Level-zero packets and cache fallbacks remain in the ordinary contiguous
/// output stream. Before a resident root is linked, the sink flushes that
/// dynamic prefix, preserving the exact `addPrim` encounter order. Resident
/// blocks keep a fixed theoretical packet layout: clipped child polygons are
/// not linked but still retain their invariant slot, so camera-dependent clip
/// masks never invalidate the cache identity.
///
/// # Safety
///
/// The vertex, surface, output and scratch contracts match
/// [`submit_classic_affine_batch`]. `source_faces` must contain
/// `surface_count` entries; `u16::MAX` forces the corresponding surface onto
/// the dynamic writer. Every slot returned by `sink` must be large enough for
/// the selected 252-byte level-one or 748-byte level-two maximum root shape.
#[cfg(feature = "classic-affine-subdivision-cache")]
pub unsafe fn submit_classic_affine_cached_subdivision_batch<
    S: ClassicAffineSubdivisionCacheSink,
>(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineBatchSurface,
    source_faces: *const u16,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
    sink: &mut S,
) -> ClassicAffineSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || source_faces.is_null()
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
    let mut resident_packets = 0u32;
    let mut resident_triangles = 0u32;
    let mut surface_index = 0usize;
    while surface_index < surface_count {
        let surface = unsafe { ptr::read(surfaces.add(surface_index)) };
        let source_face = unsafe { ptr::read(source_faces.add(surface_index)) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        writer.tpage_high_word = u32::from(surface.tpage) << 16;
        writer.clut_high_word = u32::from(surface.clut) << 16;

        let fan = unsafe { vertices.add(first_vertex) };
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < surface_vertices && surface_clip != 0 {
            surface_clip &= classic_clip_code(unsafe { (*fan.add(clip_index)).screen }, profile);
            clip_index += 1;
        }
        if surface_clip != 0 {
            surface_index += 1;
            continue;
        }

        let root = unsafe { &*fan };
        let root_depth = root.depth as u16 as u32;
        let end = unsafe { fan.add(surface_vertices) };
        let mut previous = unsafe { fan.add(1) };
        let mut current = unsafe { fan.add(2) };
        let mut root_index = 0u8;
        while current != end {
            let previous_ref = unsafe { &*previous };
            let current_ref = unsafe { &*current };
            let otz = scene::classic_otz3_from_sum(
                root_depth + previous_ref.depth as u16 as u32 + current_ref.depth as u16 as u32,
            );
            if otz == 0 || otz >= profile.ot_depth {
                previous = current;
                current = unsafe { current.add(1) };
                root_index = root_index.wrapping_add(1);
                continue;
            }

            let level =
                classic_affine_subdivision_level([root, previous_ref, current_ref], otz, profile);
            let next = unsafe { current.add(1) };
            if level == 0 && next != end {
                let next_ref = unsafe { &*next };
                let next_otz = scene::classic_otz3_from_sum(
                    root_depth + current_ref.depth as u16 as u32 + next_ref.depth as u16 as u32,
                );
                if next_otz == otz
                    && classic_affine_subdivision_level(
                        [root, current_ref, next_ref],
                        next_otz,
                        profile,
                    ) == 0
                {
                    let quad_refs = unsafe { [&*previous, &*current, root, &*next] };
                    #[cfg(feature = "classic-affine-resident-base-cache")]
                    if source_face != u16::MAX {
                        let material = u32::from(surface.tpage) | (u32::from(surface.clut) << 16);
                        if let Some(slot) =
                            sink.acquire_base_packet(source_face, root_index, true, material, otz)
                        {
                            unsafe {
                                sink.flush_dynamic_until(writer.next);
                                prepare_resident_base_quad(
                                    slot,
                                    quad_refs,
                                    quad_refs,
                                    otz,
                                    writer.clut_high_word,
                                    writer.tpage_high_word,
                                );
                                sink.insert_resident_packet(
                                    slot.active,
                                    otz,
                                    ClassicQuadTexturedGouraud::WORDS,
                                );
                            }
                            resident_packets = resident_packets.wrapping_add(1);
                            resident_triangles = resident_triangles.wrapping_add(2);
                            previous = next;
                            current = unsafe { next.add(1) };
                            root_index = root_index.wrapping_add(2);
                            continue;
                        }
                    }
                    unsafe { writer.emit_quad(quad_refs, quad_refs, otz) };
                    previous = next;
                    current = unsafe { next.add(1) };
                    root_index = root_index.wrapping_add(2);
                    continue;
                }
            }

            if level == 0 {
                let root_refs = [root, previous_ref, current_ref];
                #[cfg(feature = "classic-affine-resident-base-cache")]
                if source_face != u16::MAX {
                    let material = u32::from(surface.tpage) | (u32::from(surface.clut) << 16);
                    if let Some(slot) =
                        sink.acquire_base_packet(source_face, root_index, false, material, otz)
                    {
                        unsafe {
                            sink.flush_dynamic_until(writer.next);
                            prepare_resident_base_tri(
                                slot,
                                root_refs,
                                root_refs,
                                otz,
                                writer.clut_high_word,
                                writer.tpage_high_word,
                            );
                            sink.insert_resident_packet(
                                slot.active,
                                otz,
                                ClassicTriTexturedGouraud::WORDS,
                            );
                        }
                        resident_packets = resident_packets.wrapping_add(1);
                        resident_triangles = resident_triangles.wrapping_add(1);
                        previous = current;
                        current = next;
                        root_index = root_index.wrapping_add(1);
                        continue;
                    }
                }
                unsafe { writer.emit_tri(root_refs, root_refs, otz) };
            } else {
                let underdraw_at = i32::from(if level == 2 {
                    profile.subdivide_twice_at
                } else {
                    profile.subdivide_once_at
                });
                let underdraw = root.depth >= underdraw_at
                    || previous_ref.depth >= underdraw_at
                    || current_ref.depth >= underdraw_at;
                let material = u32::from(surface.tpage) | (u32::from(surface.clut) << 16);
                let slot = if source_face == u16::MAX
                    || (cfg!(feature = "classic-affine-subdivision-cache-level2-only")
                        && level != 2)
                {
                    None
                } else {
                    sink.acquire_root(source_face, root_index, level, underdraw, material, otz)
                };
                if let Some(slot) = slot {
                    unsafe { sink.flush_dynamic_until(writer.next) };
                    let active_start = slot.active;
                    let (active_end, packets, triangles) = if slot.resident {
                        #[cfg(feature = "classic-affine-resident-level2-scatter")]
                        {
                            debug_assert_eq!(level, 2);
                            let underdraw_otz = if underdraw {
                                otz.saturating_add(profile.underdraw_slot_bias)
                            } else {
                                0
                            };
                            let scattered = unsafe {
                                scatter_resident_level2_root(
                                    slot.active,
                                    root,
                                    previous_ref,
                                    current_ref,
                                    generated,
                                    underdraw_otz,
                                )
                            };
                            (
                                scattered.active,
                                scattered.packets,
                                scattered.hardware_triangles,
                            )
                        }
                        #[cfg(not(feature = "classic-affine-resident-level2-scatter"))]
                        {
                            let mut cached: CachedSubdivisionPacketPatcher<'_, S> =
                                CachedSubdivisionPacketPatcher {
                                    active: slot.active,
                                    packets: 0,
                                    hardware_triangles: 0,
                                    profile,
                                    #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
                                    sink,
                                    #[cfg(feature = "classic-affine-gpu-polygon-clip")]
                                    _sink: core::marker::PhantomData,
                                };
                            if level == 2 {
                                unsafe {
                                    cached.emit_subdivide_twice(
                                        root,
                                        previous_ref,
                                        current_ref,
                                        generated,
                                        otz,
                                        7,
                                    )
                                };
                            } else {
                                #[cfg(not(
                                    feature = "classic-affine-subdivision-cache-level2-only"
                                ))]
                                unsafe {
                                    subdivide_once(
                                        &mut cached,
                                        root,
                                        previous_ref,
                                        current_ref,
                                        generated,
                                        otz,
                                        7,
                                    )
                                };
                            }
                            (cached.active, cached.packets, cached.hardware_triangles)
                        }
                    } else {
                        #[cfg(feature = "classic-affine-resident-level2-cold-init")]
                        {
                            debug_assert_eq!(level, 2);
                            let initialized = unsafe {
                                initialize_resident_level2_root::<S>(
                                    slot.active,
                                    root,
                                    previous_ref,
                                    current_ref,
                                    generated,
                                    otz,
                                    writer.clut_high_word,
                                    writer.tpage_high_word,
                                    profile,
                                )
                            };
                            (
                                initialized.active,
                                initialized.packets,
                                initialized.hardware_triangles,
                            )
                        }
                        #[cfg(not(feature = "classic-affine-resident-level2-cold-init"))]
                        {
                            let mut cached: CachedSubdivisionPacketWriter<'_, S> =
                                CachedSubdivisionPacketWriter {
                                    active: slot.active,
                                    initialize: true,
                                    packets: 0,
                                    hardware_triangles: 0,
                                    clut_high_word: writer.clut_high_word,
                                    tpage_high_word: writer.tpage_high_word,
                                    profile,
                                    #[cfg(not(feature = "classic-affine-gpu-polygon-clip"))]
                                    sink,
                                    #[cfg(feature = "classic-affine-gpu-polygon-clip")]
                                    _sink: core::marker::PhantomData,
                                };
                            if level == 2 {
                                unsafe {
                                    cached.emit_subdivide_twice(
                                        root,
                                        previous_ref,
                                        current_ref,
                                        generated,
                                        otz,
                                        7,
                                    )
                                };
                            } else {
                                #[cfg(not(
                                    feature = "classic-affine-subdivision-cache-level2-only"
                                ))]
                                unsafe {
                                    subdivide_once(
                                        &mut cached,
                                        root,
                                        previous_ref,
                                        current_ref,
                                        generated,
                                        otz,
                                        7,
                                    )
                                };
                            }
                            (cached.active, cached.packets, cached.hardware_triangles)
                        }
                    };
                    let root_bytes = unsafe { active_end.offset_from(active_start) as usize } * 4;
                    debug_assert!(root_bytes <= if level == 2 { 748 } else { 252 });
                    #[cfg(feature = "classic-affine-gpu-polygon-clip")]
                    unsafe {
                        sink.insert_resident_stream(active_start, active_end);
                    }
                    resident_packets = resident_packets.wrapping_add(packets);
                    resident_triangles = resident_triangles.wrapping_add(triangles);
                } else if level == 2 {
                    unsafe {
                        writer.emit_subdivide_twice(
                            root,
                            previous_ref,
                            current_ref,
                            generated,
                            otz,
                            7,
                        )
                    };
                } else {
                    unsafe {
                        subdivide_once(
                            &mut writer,
                            root,
                            previous_ref,
                            current_ref,
                            generated,
                            otz,
                            7,
                        )
                    };
                }
            }
            previous = current;
            current = next;
            root_index = root_index.wrapping_add(1);
        }
        surface_index += 1;
    }

    let mut submitted = unsafe { writer.finish(output) };
    submitted.packets = submitted.packets.wrapping_add(resident_packets);
    submitted.hardware_triangles = submitted
        .hardware_triangles
        .wrapping_add(resident_triangles);
    submitted
}

unsafe fn submit_classic_affine_projected_resident_pass(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineResidentBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
    allow_invariant_reuse: bool,
) -> ResidentPacketWriter {
    let generated = unsafe { vertices.add(vertex_count) };
    let mut writer = ResidentPacketWriter {
        next: output,
        packets: 0,
        hardware_triangles: 0,
        invariant_hit_slots: 0,
        invariant_miss_slots: 0,
        layout_hash_a: 0x811c_9dc5,
        layout_hash_b: 0x9e37_79b9,
        decision_bits: 0,
        decision_nibbles: 0,
        clip_bits: 0,
        clip_count: 0,
        clut_high_word: 0,
        tpage_high_word: 0,
        reuse_invariants: false,
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
        writer.tpage_high_word = u32::from(surface.tpage) << 16;
        writer.clut_high_word = u32::from(surface.clut) << 16;
        writer.reuse_invariants = allow_invariant_reuse && surface.reuse_invariants != 0;
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
    writer
}

unsafe fn record_classic_affine_projected_plan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineResidentBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
) -> ClassicAffinePlannedSubmit {
    let generated = unsafe { vertices.add(vertex_count) };
    let mut recorder = PlannedPacketRecorder {
        writer: PacketWriter {
            next: output,
            packets: 0,
            clut_high_word: 0,
            tpage_high_word: 0,
            profile,
        },
        plan: ClassicAffinePacketPlan::default(),
        decision_cursor: 0,
        clip_cursor: 0,
        overflow: false,
    };
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        recorder.writer.tpage_high_word = u32::from(surface.tpage) << 16;
        recorder.writer.clut_high_word = u32::from(surface.clut) << 16;
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut recorder,
            );
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
    }
    unsafe { recorder.finish(output) }
}

unsafe fn patch_classic_affine_projected_plan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineResidentBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
    expected: &ClassicAffinePacketPlan,
) -> (ClassicAffineSubmit, bool, u32, u32) {
    let generated = unsafe { vertices.add(vertex_count) };
    let mut patcher = PlannedPacketPatcher {
        writer: PacketWriter {
            next: output,
            packets: 0,
            clut_high_word: 0,
            tpage_high_word: 0,
            profile,
        },
        expected,
        decision_cursor: 0,
        clip_cursor: 0,
        mismatch: false,
        reuse_invariants: false,
        invariant_hit_slots: 0,
        invariant_miss_slots: 0,
    };
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let first_vertex = surface.first_vertex as usize;
        let surface_vertices = surface.vertex_count as usize;
        debug_assert!(surface_vertices >= 3);
        debug_assert!(first_vertex + surface_vertices <= vertex_count);
        patcher.writer.tpage_high_word = u32::from(surface.tpage) << 16;
        patcher.writer.clut_high_word = u32::from(surface.clut) << 16;
        patcher.reuse_invariants = surface.reuse_invariants != 0;
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.add(first_vertex),
                surface_vertices,
                generated,
                &mut patcher,
            );
        }
        surface_ptr = unsafe { surface_ptr.add(1) };
    }
    let invariant_hit_slots = patcher.invariant_hit_slots;
    let invariant_miss_slots = patcher.invariant_miss_slots;
    let (submit, matched) = unsafe { patcher.finish(output) };
    (submit, matched, invariant_hit_slots, invariant_miss_slots)
}

/// Project and submit compact fans using an exact resident topology plan.
///
/// A cold call writes complete packets and records a bounded bit-exact plan.
/// A later call for the same source/material identity and destination address
/// may supply that plan. The hot writer compares subdivision and screen-clip
/// decisions directly while patching resident tag/XY words. A mismatch
/// immediately falls back to a complete replay from the already projected
/// vertices; no speculative packet reaches the ordering table.
///
/// The caller remains responsible for proving that a supplied plan belongs to
/// the same source surfaces, material attributes, and output address. Per
/// surface `reuse_invariants` may be nonzero only when UV, colour, CLUT, TPAGE,
/// and command words are unchanged.
///
/// # Safety
/// The requirements match [`submit_classic_affine_resident_batch`]. The
/// bounded plan is intended for at most 39 source vertices; larger batches
/// remain correct but produce an invalid, non-reusable plan.
pub unsafe fn submit_classic_affine_planned_resident_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineResidentBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
    expected_plan: Option<&ClassicAffinePacketPlan>,
) -> ClassicAffinePlannedSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || output.is_null()
        || vertex_count == 0
        || surface_count == 0
    {
        return ClassicAffinePlannedSubmit {
            submit: ClassicAffineSubmit {
                next_packet: output,
                packets: 0,
                hardware_triangles: 0,
            },
            plan: ClassicAffinePacketPlan::default(),
            topology_hit: false,
            invariant_hit_slots: 0,
            invariant_miss_slots: 0,
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

    if let Some(expected) = expected_plan.filter(|plan| plan.is_valid()) {
        let (submit, matched, invariant_hit_slots, invariant_miss_slots) = unsafe {
            patch_classic_affine_projected_plan(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                profile,
                expected,
            )
        };
        if matched {
            return ClassicAffinePlannedSubmit {
                submit,
                plan: *expected,
                topology_hit: true,
                invariant_hit_slots,
                invariant_miss_slots,
            };
        }
    }

    unsafe {
        record_classic_affine_projected_plan(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            profile,
        )
    }
}

/// Project and submit compact fans into a persistent visible packet layout.
///
/// The first pass speculatively patches only tag/XY for reusable surfaces and
/// packs the exact subdivision decisions and polygon clip bits while the
/// normal traversal is already producing them. A matching key completes
/// in that one pass. A mismatch replays the already projected batch once with
/// full packet writes, so no incorrect speculative invariant reaches the GPU.
/// Screen-rejected polygons occupy no slots, keeping the stream as compact as
/// [`submit_classic_affine_batch`] and avoiding a second linker scan over
/// sentinel holes.
///
/// `expected_topology` proves shape only. The caller must also prove that the
/// same source surfaces and material attributes occupy the same destination
/// addresses before setting `reuse_invariants` in any descriptor.
///
/// # Safety
///
/// The vertex, descriptor, scratch-tail, output-capacity, and packet-lifetime
/// requirements match [`submit_classic_affine_batch`]. `output` must have room
/// for the normal worst-case packet expansion.
pub unsafe fn submit_classic_affine_resident_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineResidentBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
    expected_topology: Option<ClassicAffineTopologyKey>,
) -> ClassicAffineResidentSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || output.is_null()
        || vertex_count == 0
        || surface_count == 0
    {
        return ClassicAffineResidentSubmit {
            submit: ClassicAffineSubmit {
                next_packet: output,
                packets: 0,
                hardware_triangles: 0,
            },
            topology_key: ClassicAffineTopologyKey::default(),
            topology_hit: false,
            resident_packet_slots: 0,
            invariant_hit_slots: 0,
            invariant_miss_slots: 0,
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

    let speculative = unsafe {
        submit_classic_affine_projected_resident_pass(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            profile,
            expected_topology.is_some(),
        )
    };
    let packet_bytes = unsafe { speculative.next.offset_from(output) as u32 }.wrapping_mul(4);
    let (layout_hash_a, layout_hash_b) = speculative.finalized_layout_hashes();
    let current_key = ClassicAffineTopologyKey {
        packet_slots: speculative.packets,
        packet_bytes,
        layout_hash_a,
        layout_hash_b,
    };
    let topology_hit = expected_topology == Some(current_key);
    if !topology_hit && speculative.invariant_hit_slots != 0 {
        // The speculative pass may have patched slots whose topology changed.
        // Rebuild from the already projected vertices before the OT sees them.
        let rebuilt = unsafe {
            submit_classic_affine_projected_resident_pass(
                vertices,
                vertex_count,
                surfaces,
                surface_count,
                output,
                profile,
                false,
            )
        };
        debug_assert_eq!(rebuilt.packets, current_key.packet_slots);
        debug_assert_eq!(
            unsafe { rebuilt.next.offset_from(output) as u32 }.wrapping_mul(4),
            current_key.packet_bytes
        );
        let (rebuilt_hash_a, rebuilt_hash_b) = rebuilt.finalized_layout_hashes();
        debug_assert_eq!(rebuilt_hash_a, current_key.layout_hash_a);
        debug_assert_eq!(rebuilt_hash_b, current_key.layout_hash_b);
        return unsafe { rebuilt.finish(output, false) };
    }
    unsafe { speculative.finish(output, topology_hit) }
}

/// Submit several contiguous convex fans whose source vertices already carry
/// screen coordinates and cached GTE depths.
///
/// This is the indexed-cache counterpart to [`submit_classic_affine_batch`].
/// A retained renderer can project shared positions once, scatter those
/// results into its per-corner attribute stream, and preserve the exact same
/// clipping, subdivision, packet topology, and ordering-table behaviour.
///
/// # Safety
/// `vertices` must point to `vertex_count + 12` writable records, with the
/// final records reserved for shared subdivision scratch. `surfaces` must
/// contain `surface_count` descriptors whose vertex ranges fit entirely in
/// the first `vertex_count` records. Every source record's `screen` and
/// `depth` fields must have been produced for the currently intended camera.
/// `output` must have room for every fan's worst-case packet expansion and
/// remain live until submission completes.
pub unsafe fn submit_classic_affine_projected_batch(
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
    unsafe {
        submit_classic_affine_windowed_fan_impl::<false>(
            vertices,
            vertex_count,
            output,
            tpage,
            clut,
            texture_window_word,
            profile,
        )
    }
}

/// Project and submit one convex fan whose packets restore an unwindowed
/// GP0(E2) state immediately after drawing.
///
/// This scoped variant is intended for a windowed material mixed with compact
/// non-windowed packets in one ordering table. Since depth sorting may place
/// either packet next, restoring the state inside every special polygon avoids
/// paying for a redundant GP0(E2) command on every ordinary polygon.
///
/// # Safety
/// The contract matches [`submit_classic_affine_windowed_fan`]. The caller's
/// worst-case output capacity must include one additional data word per
/// emitted polygon for the trailing reset command.
pub unsafe fn submit_classic_affine_scoped_windowed_fan(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    output: *mut u32,
    tpage: u16,
    clut: u16,
    texture_window_word: u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    unsafe {
        submit_classic_affine_windowed_fan_impl::<true>(
            vertices,
            vertex_count,
            output,
            tpage,
            clut,
            texture_window_word,
            profile,
        )
    }
}

unsafe fn submit_classic_affine_windowed_fan_impl<const RESTORE_WINDOW: bool>(
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

    let mut writer = WindowedPacketWriter::<RESTORE_WINDOW> {
        next: output,
        packets: 0,
        clut_high_word: (clut as u32) << 16,
        tpage_high_word: (tpage as u32) << 16,
        uv_offset: [0; 2],
        texture_window_word,
        color_command_word: 0x3400_0000,
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
    unsafe {
        submit_classic_affine_windowed_batch_impl::<false>(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            profile,
        )
    }
}

/// Project and submit several independently windowed convex fans whose OT
/// packets restore an unwindowed GP0(E2) state after every polygon.
///
/// Use this when windowed surfaces share an ordering table with compact world,
/// brush-model, or alias packets. OT linking can interleave those consumers at
/// any depth, so a reset at the end of a CPU-side batch is not sufficient.
/// The caller's worst-case output capacity must include one additional data
/// word per emitted polygon for the trailing reset command.
///
/// # Safety
/// The contract matches [`submit_classic_affine_windowed_batch`].
pub unsafe fn submit_classic_affine_scoped_windowed_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineWindowedBatchSurface,
    surface_count: usize,
    output: *mut u32,
    profile: ClassicAffineProfile,
) -> ClassicAffineSubmit {
    unsafe {
        submit_classic_affine_windowed_batch_impl::<true>(
            vertices,
            vertex_count,
            surfaces,
            surface_count,
            output,
            profile,
        )
    }
}

/// Project and submit a batch that mixes compact page-local and scoped
/// windowed PXBSP surfaces without flushing between packet shapes.
///
/// # Safety
/// The vertex, descriptor, scratch-tail, output-capacity, and lifetime
/// contract matches [`submit_classic_affine_batch`]. Windowed descriptors
/// must carry a valid GP0(E2) command; compact descriptors ignore it.
pub unsafe fn submit_classic_affine_mixed_batch(
    vertices: *mut ClassicAffineVertex,
    vertex_count: usize,
    surfaces: *const ClassicAffineMixedBatchSurface,
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
    let mut next = output;
    let mut packets = 0u32;
    let mut hardware_triangles = 0u32;
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        let surface = unsafe { ptr::read(surface_ptr) };
        let submitted = if surface.compact != 0 {
            let run_output = next;
            let mut writer = PacketWriter {
                next,
                packets: 0,
                clut_high_word: 0,
                tpage_high_word: 0,
                profile,
            };
            while surface_ptr != surface_end {
                let surface = unsafe { ptr::read(surface_ptr) };
                if surface.compact == 0 {
                    break;
                }
                let first_vertex = surface.first_vertex as usize;
                let surface_vertices = surface.vertex_count as usize;
                debug_assert!(surface_vertices >= 3);
                debug_assert!(first_vertex + surface_vertices <= vertex_count);
                writer.tpage_high_word = (surface.tpage as u32) << 16;
                writer.clut_high_word = (surface.clut as u32) << 16;
                writer.profile.farthest_depth_key = surface.depth_law != 0;
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
            unsafe { writer.finish(run_output) }
        } else {
            let run_output = next;
            let mut writer = WindowedPacketWriter::<true> {
                next,
                packets: 0,
                clut_high_word: 0,
                tpage_high_word: 0,
                uv_offset: [0; 2],
                texture_window_word: 0,
                color_command_word: 0x3400_0000,
                profile,
            };
            while surface_ptr != surface_end {
                let surface = unsafe { ptr::read(surface_ptr) };
                if surface.compact != 0 {
                    break;
                }
                let first_vertex = surface.first_vertex as usize;
                let surface_vertices = surface.vertex_count as usize;
                debug_assert!(surface_vertices >= 3);
                debug_assert!(first_vertex + surface_vertices <= vertex_count);
                writer.tpage_high_word = (surface.tpage as u32) << 16;
                writer.clut_high_word = (surface.clut as u32) << 16;
                writer.profile.farthest_depth_key = surface.depth_law != 0;
                writer.uv_offset = surface.uv_offset;
                writer.texture_window_word = surface.texture_window_word;
                writer.color_command_word = surface.color_command_word;
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
            unsafe { writer.finish(run_output) }
        };
        next = submitted.next_packet;
        packets = packets.wrapping_add(submitted.packets);
        hardware_triangles = hardware_triangles.wrapping_add(submitted.hardware_triangles);
    }

    ClassicAffineSubmit {
        next_packet: next,
        packets,
        hardware_triangles,
    }
}

unsafe fn submit_classic_affine_windowed_batch_impl<const RESTORE_WINDOW: bool>(
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
    let mut writer = WindowedPacketWriter::<RESTORE_WINDOW> {
        next: output,
        packets: 0,
        clut_high_word: 0,
        tpage_high_word: 0,
        uv_offset: [0; 2],
        texture_window_word: 0,
        color_command_word: 0x3400_0000,
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
        writer.color_command_word = surface.color_command_word;
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
        let out = project_triangle_scheduled(source_vec3(a), source_vec3(b), source_vec3(c));
        projected[index] = prepare_projected(a, out[0], uv_offset, light_weights);
        projected[index + 1] = prepare_projected(b, out[1], uv_offset, light_weights);
        projected[index + 2] = prepare_projected(c, out[2], uv_offset, light_weights);
        index += 3;
    }
    while index < vertex_count {
        let source = unsafe { ptr::read_unaligned(vertices.add(index)) };
        let out = project_vertex_scheduled(source_vec3(source));
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
            let subdivision_level = classic_affine_subdivision_level(root, otz, profile);
            if subdivision_level != 0 {
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
                if subdivision_level == 2 {
                    unsafe {
                        writer.emit_subdivide_twice(
                            &expanded[0],
                            &expanded[1],
                            &expanded[2],
                            generated_ptr,
                            otz,
                            7,
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
                            7,
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
        let out = project_triangle_scheduled(
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
        let out = project_vertex_scheduled(unsafe { alias_vec3(vertices, vertex) });
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
    crate::projection::zero_origin_screen_outcode(
        screen,
        profile.screen_width as i32 - 1,
        profile.screen_height as i32 - 1,
    )
}

#[cfg(feature = "classic-affine-fixed-fan-guarded")]
#[inline(always)]
fn classic_gte_screen_saturated(screen: [i16; 2]) -> bool {
    screen[0] <= -1024 || screen[0] >= 1023 || screen[1] <= -1024 || screen[1] >= 1023
}

#[inline(always)]
fn classic_tri_screen_clipped(screens: [[i16; 2]; 3], profile: ClassicAffineProfile) -> bool {
    classic_triangle_screen_rejected(
        screens,
        profile.screen_width as i32 - 1,
        profile.screen_height as i32 - 1,
    )
}

#[inline(always)]
fn classic_quad_screen_clipped(screens: [[i16; 2]; 4], profile: ClassicAffineProfile) -> bool {
    classic_quad_screen_rejected(
        screens,
        profile.screen_width as i32 - 1,
        profile.screen_height as i32 - 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "classic-affine-fixed-fan-guarded")]
    #[test]
    fn guarded_fixed_fan_detects_only_gte_sxy_saturation_bounds() {
        assert!(!classic_gte_screen_saturated([-1023, 1022]));
        assert!(classic_gte_screen_saturated([-1024, 0]));
        assert!(classic_gte_screen_saturated([1023, 0]));
        assert!(classic_gte_screen_saturated([0, -1024]));
        assert!(classic_gte_screen_saturated([0, 1023]));
    }

    #[test]
    fn shared_vertex_layout_matches_retained_c_record() {
        assert_eq!(size_of::<ClassicAffineVertex>(), 20);
        assert_eq!(core::mem::align_of::<ClassicAffineVertex>(), 4);
        assert_eq!(size_of::<ClassicAffineSourceVertex>(), 10);
        assert_eq!(core::mem::align_of::<ClassicAffineSourceVertex>(), 1);
        assert_eq!(size_of::<ClassicAffineWordSourceVertex>(), 12);
        assert_eq!(core::mem::align_of::<ClassicAffineWordSourceVertex>(), 4);
        assert_eq!(size_of::<ClassicAffineIndexedCorner>(), 8);
        assert_eq!(core::mem::align_of::<ClassicAffineIndexedCorner>(), 4);
        assert_eq!(size_of::<ClassicAffineProjectedVertex>(), 16);
        assert_eq!(core::mem::align_of::<ClassicAffineProjectedVertex>(), 4);
        assert_eq!(size_of::<ClassicAffinePosition>(), 6);
        assert_eq!(core::mem::align_of::<ClassicAffinePosition>(), 2);
        assert_eq!(size_of::<ClassicAffineBatchSurface>(), 8);
        assert_eq!(core::mem::align_of::<ClassicAffineBatchSurface>(), 2);
        assert_eq!(size_of::<ClassicAffineWindowedBatchSurface>(), 20);
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

    fn census_vertices(count: usize) -> [ClassicAffineVertex; 4] {
        let mut vertices = [ClassicAffineVertex::default(); 4];
        for (index, vertex) in vertices[..count].iter_mut().enumerate() {
            vertex.screen = [40 + index as i16 * 8, 60 + index as i16 * 6];
            vertex.depth = 100;
            vertex.uv = [index as u8 * 8, index as u8 * 4];
            vertex.color = 0x0080_8080;
        }
        vertices
    }

    #[test]
    fn topology_census_recovers_level_zero_fan_pairing() {
        let vertices = census_vertices(4);
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 4,
            tpage: 0,
            clut: 0,
        };
        let profile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut census = ClassicAffineTopologyCensus::default();
        unsafe {
            census_classic_affine_projected_batch_topology(
                vertices.as_ptr(),
                4,
                &surface,
                1,
                profile,
                &mut census,
            );
        }
        assert_eq!(census.root_triangles, 2);
        assert_eq!(census.level0_root_triangles, 2);
        assert_eq!(census.paired_level0_packets, 1);
        assert_eq!(census.theoretical_packets, 1);
        assert_eq!(census.theoretical_hardware_triangles, 2);
        assert_eq!(census.theoretical_packet_bytes, 52);
    }

    #[cfg(any(
        feature = "classic-affine-quake-level0-run",
        feature = "classic-affine-quake-cold-adaptive",
        feature = "classic-affine-quake-cold-level2"
    ))]
    #[test]
    fn quake_hybrid_matches_reference_across_adaptive_boundaries() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        const COUNT: usize = 7;
        let depths = [400, 800, 800, 100, 100, 800, 800];
        let mut vertices = [ClassicAffineVertex::default(); COUNT + EXTRA_VERTICES];
        for index in 0..COUNT {
            vertices[index] = ClassicAffineVertex {
                position: [
                    index as i16 * 10 - 30,
                    (index as i16 & 1) * 20 - 10,
                    depths[index] as i16,
                ],
                uv: [index as u8 * 17, index as u8 * 23],
                color: 0x0010_2030 + index as u32,
                screen: [40 + index as i16 * 12, 80 + (index as i16 & 1) * 16],
                depth: depths[index],
            };
        }
        let mut reference_vertices = vertices;
        let mut hybrid_vertices = vertices;
        let mut reference_words = [0u32; 1024];
        let mut hybrid_words = [0u32; 1024];
        let mut reference_writer = PacketWriter {
            next: reference_words.as_mut_ptr(),
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0105_0000,
            profile: ClassicAffineProfile::QUAKE_REFERENCE,
        };
        let mut hybrid_writer = PacketWriter {
            next: hybrid_words.as_mut_ptr(),
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0105_0000,
            profile: ClassicAffineProfile::QUAKE_REFERENCE,
        };
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                reference_vertices.as_mut_ptr(),
                COUNT,
                reference_vertices.as_mut_ptr().add(COUNT),
                &mut reference_writer,
            );
            #[cfg(feature = "classic-affine-quake-level0-run")]
            submit_quake_projected_fan_level0_runs(
                hybrid_vertices.as_mut_ptr(),
                COUNT,
                hybrid_vertices.as_mut_ptr().add(COUNT),
                &mut hybrid_writer,
            );
            #[cfg(all(
                feature = "classic-affine-quake-cold-adaptive",
                not(feature = "classic-affine-quake-level0-run")
            ))]
            submit_quake_projected_fan_cold_adaptive(
                hybrid_vertices.as_mut_ptr(),
                COUNT,
                hybrid_vertices.as_mut_ptr().add(COUNT),
                &mut hybrid_writer,
            );
            #[cfg(all(
                feature = "classic-affine-quake-cold-level2",
                not(feature = "classic-affine-quake-level0-run"),
                not(feature = "classic-affine-quake-cold-adaptive")
            ))]
            submit_quake_projected_fan_cold_level2(
                hybrid_vertices.as_mut_ptr(),
                COUNT,
                hybrid_vertices.as_mut_ptr().add(COUNT),
                &mut hybrid_writer,
            );
        }
        let reference = unsafe { reference_writer.finish(reference_words.as_mut_ptr()) };
        let hybrid = unsafe { hybrid_writer.finish(hybrid_words.as_mut_ptr()) };
        assert_eq!(hybrid.packets, reference.packets);
        assert_eq!(hybrid.hardware_triangles, reference.hardware_triangles);
        let reference_len =
            unsafe { reference.next_packet.offset_from(reference_words.as_ptr()) as usize };
        let hybrid_len = unsafe { hybrid.next_packet.offset_from(hybrid_words.as_ptr()) as usize };
        assert_eq!(hybrid_len, reference_len);
        assert_eq!(
            &hybrid_words[..hybrid_len],
            &reference_words[..reference_len]
        );
    }

    #[cfg(feature = "classic-affine-relaxed-quad-pairing")]
    #[test]
    fn relaxed_pairing_combines_neighbouring_level_zero_ot_slots() {
        let mut vertices = [ClassicAffineVertex::default(); 4 + EXTRA_VERTICES];
        for (index, (screen, depth)) in [
            ([100, 100], 100),
            ([100, 140], 100),
            ([140, 140], 100),
            ([140, 100], 108),
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index].screen = screen;
            vertices[index].depth = depth;
            vertices[index].uv = [index as u8 * 8, index as u8 * 4];
            vertices[index].color = 0x0080_8080;
        }
        let first_otz = scene::classic_otz3_from_sum(300);
        let second_otz = scene::classic_otz3_from_sum(308);
        assert_ne!(first_otz, second_otz);
        assert_eq!(first_otz.abs_diff(second_otz), 1);

        let profile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut packets = [0u32; 32];
        let submitted = unsafe {
            submit_classic_affine_projected_fan(
                vertices.as_mut_ptr(),
                4,
                packets.as_mut_ptr(),
                0x0105,
                0x1234,
                profile,
            )
        };
        assert_eq!(submitted.packets, 1);
        assert_eq!(submitted.hardware_triangles, 2);
        assert_eq!(packets[0] & 0xffff, 408 >> 4);
    }

    #[test]
    fn topology_census_bounds_both_subdivision_lattices() {
        let vertices = census_vertices(3);
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 3,
            tpage: 0,
            clut: 0,
        };
        let mut once = ClassicAffineTopologyCensus::default();
        unsafe {
            census_classic_affine_projected_batch_topology(
                vertices.as_ptr(),
                3,
                &surface,
                1,
                ClassicAffineProfile {
                    subdivide_once_at: 1000,
                    subdivide_twice_at: 0,
                    ..ClassicAffineProfile::QUAKE_REFERENCE
                },
                &mut once,
            );
        }
        assert_eq!(once.level1_root_triangles, 1);
        assert_eq!(once.level1_underdraw_roots, 0);
        assert_eq!(once.theoretical_packets, 3);
        assert_eq!(once.theoretical_hardware_triangles, 4);
        assert_eq!(once.theoretical_packet_bytes, 132);

        let mut twice = ClassicAffineTopologyCensus::default();
        unsafe {
            census_classic_affine_projected_batch_topology(
                vertices.as_ptr(),
                3,
                &surface,
                1,
                ClassicAffineProfile {
                    subdivide_once_at: 2000,
                    subdivide_twice_at: 1000,
                    ..ClassicAffineProfile::QUAKE_REFERENCE
                },
                &mut twice,
            );
        }
        assert_eq!(twice.level2_root_triangles, 1);
        assert_eq!(twice.level2_underdraw_roots, 0);
        assert_eq!(twice.theoretical_packets, 10);
        assert_eq!(twice.theoretical_hardware_triangles, 16);
        assert_eq!(twice.theoretical_packet_bytes, 472);
    }

    #[test]
    fn subdivision_request_collector_reports_exact_cache_shapes() {
        let mut vertices = census_vertices(4);
        vertices[0].depth = 100;
        vertices[1].depth = 100;
        vertices[2].depth = 100;
        vertices[3].depth = 1200;
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 4,
            tpage: 0x1234,
            clut: 0xabcd,
        };
        let profile = ClassicAffineProfile {
            subdivide_once_at: 1000,
            subdivide_twice_at: 500,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut requests = [ClassicAffineSubdivisionRequest::default(); 2];
        let count = unsafe {
            collect_classic_affine_projected_subdivision_requests(
                vertices.as_ptr(),
                4,
                &surface,
                1,
                profile,
                requests.as_mut_ptr(),
                requests.len(),
            )
        };
        assert_eq!(count, 2);
        assert_eq!(requests[0].batch_surface, 0);
        assert_eq!(requests[0].root, 0);
        assert_eq!(requests[0].level, 2);
        assert_eq!(requests[0].underdraw, 0);
        assert_eq!(requests[0].packet_bytes, 472);
        assert_eq!(requests[0].invariant_bytes, 288);
        assert_eq!(requests[0].material, 0xabcd_1234);
        assert_eq!(requests[1].root, 1);
        assert_eq!(requests[1].level, 2);
        assert_eq!(requests[1].underdraw, 1);
        assert_eq!(requests[1].packet_bytes, 748);
        assert_eq!(requests[1].invariant_bytes, 456);

        let required = unsafe {
            collect_classic_affine_projected_subdivision_requests(
                vertices.as_ptr(),
                4,
                &surface,
                1,
                profile,
                requests.as_mut_ptr(),
                1,
            )
        };
        assert_eq!(required, 2);
    }

    #[test]
    fn resident_batch_hit_patches_stable_slots_and_rebuilds_dynamic_slots() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 6 + EXTRA_VERTICES];
        for (index, position) in [
            [-80, -40, 1000],
            [0, 40, 1000],
            [80, -40, 1000],
            [-60, -20, 900],
            [0, 60, 900],
            [60, -20, 900],
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                uv: [index as u8, index as u8],
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            };
        }
        let surfaces = [
            ClassicAffineResidentBatchSurface {
                first_vertex: 0,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                reuse_invariants: 1,
                _padding: [0; 3],
            },
            ClassicAffineResidentBatchSurface {
                first_vertex: 3,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                reuse_invariants: 0,
                _padding: [0; 3],
            },
        ];
        let profile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut packets = [0u32; 512];
        let miss = unsafe {
            submit_classic_affine_resident_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                profile,
                None,
            )
        };
        assert!(!miss.topology_hit);
        assert_eq!(miss.resident_packet_slots, 2);
        assert_eq!(miss.invariant_hit_slots, 0);
        assert_eq!(miss.invariant_miss_slots, 2);
        assert_eq!(miss.submit.packets, 2);
        assert_eq!(miss.topology_key.packet_bytes, 80);

        packets[1] = 0xdead_beef;
        packets[11] = 0xfeed_face;
        let hit = unsafe {
            submit_classic_affine_resident_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                profile,
                Some(miss.topology_key),
            )
        };
        assert!(hit.topology_hit);
        assert_eq!(hit.invariant_hit_slots, 1);
        assert_eq!(hit.invariant_miss_slots, 1);
        assert_eq!(packets[1], 0xdead_beef);
        assert_ne!(packets[11], 0xfeed_face);
        assert_eq!(hit.submit.next_packet, unsafe {
            packets.as_mut_ptr().add(20)
        });

        packets[1] = 0xa5a5_a5a5;
        let changed_profile = ClassicAffineProfile {
            subdivide_once_at: u16::MAX,
            subdivide_twice_at: 0,
            ..profile
        };
        let replayed = unsafe {
            submit_classic_affine_resident_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                changed_profile,
                Some(hit.topology_key),
            )
        };
        assert!(!replayed.topology_hit);
        assert_eq!(replayed.invariant_hit_slots, 0);
        assert!(replayed.resident_packet_slots > 2);
        assert_eq!(
            replayed.invariant_miss_slots,
            replayed.resident_packet_slots
        );
        assert_ne!(packets[1], 0xa5a5_a5a5);
    }

    #[test]
    fn planned_resident_batch_compares_exact_plan_and_replays_mismatch() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 6 + EXTRA_VERTICES];
        for (index, position) in [
            [-80, -40, 1000],
            [0, 40, 1000],
            [80, -40, 1000],
            [-60, -20, 900],
            [0, 60, 900],
            [60, -20, 900],
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                uv: [index as u8, index as u8],
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            };
        }
        let surfaces = [
            ClassicAffineResidentBatchSurface {
                first_vertex: 0,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                reuse_invariants: 1,
                _padding: [0; 3],
            },
            ClassicAffineResidentBatchSurface {
                first_vertex: 3,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                reuse_invariants: 0,
                _padding: [0; 3],
            },
        ];
        let profile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut packets = [0u32; 512];
        let cold = unsafe {
            submit_classic_affine_planned_resident_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                profile,
                None,
            )
        };
        assert!(!cold.topology_hit);
        assert!(cold.plan.is_valid());
        assert_eq!(cold.submit.packets, 2);
        assert_eq!(cold.invariant_miss_slots, 2);

        packets[1] = 0xdead_beef;
        packets[11] = 0xfeed_face;
        let hot = unsafe {
            submit_classic_affine_planned_resident_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                profile,
                Some(&cold.plan),
            )
        };
        assert!(hot.topology_hit);
        assert_eq!(hot.plan, cold.plan);
        assert_eq!(hot.invariant_hit_slots, 1);
        assert_eq!(hot.invariant_miss_slots, 1);
        assert_eq!(packets[1], 0xdead_beef);
        assert_ne!(packets[11], 0xfeed_face);

        packets[1] = 0xa5a5_a5a5;
        let changed_profile = ClassicAffineProfile {
            subdivide_once_at: u16::MAX,
            subdivide_twice_at: 0,
            ..profile
        };
        let replayed = unsafe {
            submit_classic_affine_planned_resident_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                changed_profile,
                Some(&hot.plan),
            )
        };
        assert!(!replayed.topology_hit);
        assert!(replayed.submit.packets > 2);
        assert_eq!(replayed.invariant_hit_slots, 0);
        assert_eq!(replayed.invariant_miss_slots, replayed.submit.packets);
        assert_ne!(packets[1], 0xa5a5_a5a5);
    }

    #[cfg(feature = "classic-affine-shared-subdivision-edges")]
    #[test]
    fn equal_level_quad_fan_omits_only_the_shared_radial_underdraw() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 4 + EXTRA_VERTICES];
        for (index, position) in [
            [-80, -40, 1000],
            [-80, 40, 1000],
            [80, 40, 1000],
            [80, -40, 1000],
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                uv: [index as u8 * 16, index as u8 * 8],
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            };
        }
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 4,
            tpage: 0x0105,
            clut: 0x1234,
        };
        let profile = ClassicAffineProfile {
            subdivide_once_at: 500,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let mut packets = [0u32; 256];
        let submitted = unsafe {
            submit_classic_affine_batch(
                vertices.as_mut_ptr(),
                4,
                &surface,
                1,
                packets.as_mut_ptr(),
                profile,
            )
        };

        // Each root keeps its three visible lattice packets and the two outer
        // underdraw edges. The duplicated radial underdraw is absent on both
        // sides: 5 + 5 packets instead of the legacy 6 + 6.
        assert_eq!(submitted.packets, 10);
        assert_eq!(submitted.hardware_triangles, 12);
    }

    #[cfg(feature = "classic-affine-level0-fast-path")]
    #[test]
    fn level0_fast_path_matches_complete_writer_word_for_word() {
        let mut vertices = [ClassicAffineVertex::default(); 5 + EXTRA_VERTICES];
        for (index, (screen, depth)) in [
            ([120, 100], 700),
            ([140, 80], 704),
            ([180, 90], 700),
            ([190, 130], 704),
            ([150, 150], 700),
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                uv: [index as u8 * 11, index as u8 * 7],
                color: 0x0020_2020 + index as u32 * 0x0008_0808,
                screen,
                depth,
                ..ClassicAffineVertex::default()
            };
        }
        let profile = ClassicAffineProfile::QUAKE_REFERENCE;
        let mut fast_packets = [0u32; 128];
        let mut reference_packets = [0u32; 128];
        let mut fast_writer = PacketWriter {
            next: fast_packets.as_mut_ptr(),
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0105_0000,
            profile,
        };
        let mut reference_writer = PacketWriter {
            next: reference_packets.as_mut_ptr(),
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0105_0000,
            profile,
        };
        assert!(unsafe {
            submit_classic_affine_level0_fan_if_supported(
                vertices.as_mut_ptr(),
                5,
                &mut fast_writer,
            )
        });
        unsafe {
            submit_classic_affine_projected_fan_into_writer(
                vertices.as_mut_ptr(),
                5,
                vertices.as_mut_ptr().add(5),
                &mut reference_writer,
            );
        }
        let fast = unsafe { fast_writer.finish(fast_packets.as_mut_ptr()) };
        let reference = unsafe { reference_writer.finish(reference_packets.as_mut_ptr()) };
        assert_eq!(fast.packets, reference.packets);
        assert_eq!(fast.hardware_triangles, reference.hardware_triangles);
        let words = unsafe { fast.next_packet.offset_from(fast_packets.as_ptr()) as usize };
        assert_eq!(&fast_packets[..words], &reference_packets[..words]);
    }

    #[cfg(feature = "classic-affine-speculative-level0")]
    #[test]
    fn speculative_level0_matches_complete_writer_on_hit_and_rollback() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        for profile in [
            ClassicAffineProfile::QUAKE_REFERENCE,
            ClassicAffineProfile {
                subdivide_once_at: 900,
                subdivide_twice_at: 500,
                ..ClassicAffineProfile::QUAKE_REFERENCE
            },
        ] {
            let mut speculative_vertices = [ClassicAffineVertex::default(); 5 + EXTRA_VERTICES];
            for (index, (position, screen, depth)) in [
                ([-80, -40, 700], [142, 111], 700),
                ([-80, 40, 700], [142, 129], 700),
                ([0, 60, 700], [160, 134], 700),
                ([80, 40, 700], [178, 129], 700),
                ([80, -40, 700], [178, 111], 700),
            ]
            .into_iter()
            .enumerate()
            {
                speculative_vertices[index] = ClassicAffineVertex {
                    position,
                    uv: [index as u8 * 19, index as u8 * 13],
                    color: 0x0020_2020 + index as u32 * 0x0008_0808,
                    screen,
                    depth,
                };
            }
            let mut reference_vertices = speculative_vertices;
            let mut speculative_packets = [0u32; 1024];
            let mut reference_packets = [0u32; 1024];
            let mut speculative_writer = PacketWriter {
                next: speculative_packets.as_mut_ptr(),
                packets: 0,
                clut_high_word: 0x1234_0000,
                tpage_high_word: 0x0105_0000,
                profile,
            };
            let mut reference_writer = PacketWriter {
                next: reference_packets.as_mut_ptr(),
                packets: 0,
                clut_high_word: 0x1234_0000,
                tpage_high_word: 0x0105_0000,
                profile,
            };
            unsafe {
                submit_classic_affine_speculative_level0_fan(
                    speculative_vertices.as_mut_ptr(),
                    5,
                    speculative_vertices.as_mut_ptr().add(5),
                    &mut speculative_writer,
                );
                submit_classic_affine_projected_fan_into_writer(
                    reference_vertices.as_mut_ptr(),
                    5,
                    reference_vertices.as_mut_ptr().add(5),
                    &mut reference_writer,
                );
            }
            let speculative =
                unsafe { speculative_writer.finish(speculative_packets.as_mut_ptr()) };
            let reference = unsafe { reference_writer.finish(reference_packets.as_mut_ptr()) };
            assert_eq!(speculative.packets, reference.packets);
            assert_eq!(speculative.hardware_triangles, reference.hardware_triangles);
            let words = unsafe {
                speculative
                    .next_packet
                    .offset_from(speculative_packets.as_ptr()) as usize
            };
            assert_eq!(
                &speculative_packets[..words],
                &reference_packets[..words],
                "profile {profile:?}"
            );
        }
    }

    #[cfg(feature = "classic-affine-fixed-fan-quads")]
    #[test]
    fn fixed_fan_path_emits_gt4_pairs_without_subdivision() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 5 + EXTRA_VERTICES];
        for (index, position) in [
            [-80, -40, 80],
            [-80, 40, 80],
            [0, 60, 80],
            [80, 40, 80],
            [80, -40, 80],
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                uv: [index as u8 * 16, index as u8 * 8],
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            };
        }
        let surface = ClassicAffineBatchSurface {
            first_vertex: 0,
            vertex_count: 5,
            tpage: 0x0105,
            clut: 0x1234,
        };
        let mut packets = [0u32; 256];
        let submitted = unsafe {
            submit_classic_affine_batch(
                vertices.as_mut_ptr(),
                5,
                &surface,
                1,
                packets.as_mut_ptr(),
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        };
        assert_eq!(submitted.packets, 2);
        assert_eq!(submitted.hardware_triangles, 3);
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

        let mut generic = [ClassicAffineVertex::default(); 2];
        let mut specialized = [ClassicAffineVertex::default(); 2];
        unsafe {
            materialize_classic_affine_word_vertices(
                source.as_ptr(),
                source.len(),
                generic.as_mut_ptr(),
                [10, 252],
                [123, 456],
                false,
                true,
            );
            materialize_classic_affine_baked_light_vertices(
                source.as_ptr(),
                source.len(),
                specialized.as_mut_ptr(),
                [10, 252],
            );
        }
        assert_eq!(specialized, generic);
    }

    #[test]
    fn indexed_baked_materialization_matches_generic_path() {
        let positions = [
            ClassicAffinePosition {
                position: [-7, 11, 23],
            },
            ClassicAffinePosition {
                position: [31, -19, 5],
            },
        ];
        let corners = [
            ClassicAffineIndexedCorner {
                position_index: 1,
                uv: [250, 4],
                light: 0x00ab_cdef,
            },
            ClassicAffineIndexedCorner {
                position_index: 0,
                uv: [9, 17],
                light: 0x0012_3456,
            },
        ];
        let mut generic = [ClassicAffineVertex::default(); 2];
        let mut specialized = [ClassicAffineVertex::default(); 2];
        let mut fused = [ClassicAffineVertex::default(); 2];
        let mut position_slots = [u8::MAX; 2];
        let mut unique_positions = [0u16; 2];
        let mut unique_count = 0usize;
        let mut corner_slots = [u8::MAX; 2];
        unsafe {
            materialize_classic_affine_indexed_vertices(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                corners.len(),
                generic.as_mut_ptr(),
                [99, 99],
                [123, 456],
                true,
                true,
            );
            materialize_classic_affine_indexed_baked_vertices(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                corners.len(),
                specialized.as_mut_ptr(),
            );
            materialize_classic_affine_indexed_baked_vertices_with_projection_slots(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                corners.len(),
                fused.as_mut_ptr(),
                position_slots.as_mut_ptr(),
                unique_positions.as_mut_ptr(),
                &mut unique_count,
                unique_positions.len(),
                corner_slots.as_mut_ptr(),
            );
        }
        assert_eq!(specialized, generic);
        assert_eq!(fused, generic);
        assert_eq!(unique_count, 2);
        assert_eq!(unique_positions, [1, 0]);
        assert_eq!(position_slots, [1, 0]);
        assert_eq!(corner_slots, [0, 1]);
    }

    #[test]
    fn fused_indexed_baked_projection_matches_two_pass_path() {
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::new(0, 0, 256));
        let positions = [
            ClassicAffinePosition {
                position: [-80, -32, 48],
            },
            ClassicAffinePosition {
                position: [72, -24, 64],
            },
            ClassicAffinePosition {
                position: [96, 56, 80],
            },
            ClassicAffinePosition {
                position: [0, 88, 52],
            },
            ClassicAffinePosition {
                position: [-104, 40, 72],
            },
        ];
        let corners = [
            ClassicAffineIndexedCorner {
                position_index: 2,
                uv: [7, 11],
                light: 0x0011_2233,
            },
            ClassicAffineIndexedCorner {
                position_index: 0,
                uv: [29, 31],
                light: 0x0044_5566,
            },
            ClassicAffineIndexedCorner {
                position_index: 4,
                uv: [47, 53],
                light: 0x0077_8899,
            },
            ClassicAffineIndexedCorner {
                position_index: 1,
                uv: [61, 67],
                light: 0x00aa_bbcc,
            },
            ClassicAffineIndexedCorner {
                position_index: 3,
                uv: [71, 73],
                light: 0x00dd_eeff,
            },
        ];
        let mut reference = [ClassicAffineVertex::default(); 5];
        let mut fused = [ClassicAffineVertex::default(); 5];
        unsafe {
            materialize_classic_affine_indexed_baked_vertices(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                corners.len(),
                reference.as_mut_ptr(),
            );
            project_classic_affine_vertices(reference.as_mut_ptr(), reference.len());
            materialize_project_classic_affine_indexed_baked_vertices(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                corners.len(),
                fused.as_mut_ptr(),
            );
        }
        assert_eq!(fused, reference);
    }

    #[test]
    fn fused_indexed_batch_preserves_cross_surface_rtpt_grouping() {
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::new(0, 0, 320));
        let positions = [
            [-90, -50, 24],
            [-20, -70, 40],
            [60, -48, 56],
            [92, 12, 72],
            [48, 76, 48],
            [-32, 88, 64],
            [-100, 28, 80],
        ]
        .map(|position| ClassicAffinePosition { position });
        let corners = core::array::from_fn::<_, 7, _>(|index| ClassicAffineIndexedCorner {
            position_index: index as u16,
            uv: [index as u8 * 13, index as u8 * 17],
            light: if index < 4 {
                0x0020_3040 + index as u32
            } else {
                0x0000_4020 + index as u32
            },
        });
        let surfaces = [
            ClassicAffineBatchSurface {
                first_vertex: 0,
                vertex_count: 4,
                tpage: 1,
                clut: 2,
            },
            ClassicAffineBatchSurface {
                first_vertex: 4,
                vertex_count: 3,
                tpage: 3,
                clut: 4,
            },
        ];
        let sources = [
            ClassicAffineIndexedBatchSource {
                first_corner: 0,
                uv_offset: [99, 99],
                format: 3,
                light_weights: [0, 0],
            },
            ClassicAffineIndexedBatchSource {
                first_corner: 4,
                uv_offset: [7, 11],
                format: 0,
                light_weights: [192, 64],
            },
        ];
        let mut reference = [ClassicAffineVertex::default(); 7];
        let mut fused = [ClassicAffineVertex::default(); 7];
        unsafe {
            materialize_classic_affine_indexed_baked_vertices(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                4,
                reference.as_mut_ptr(),
            );
            materialize_classic_affine_indexed_vertices(
                corners.as_ptr().add(4),
                positions.as_ptr(),
                positions.len(),
                3,
                reference.as_mut_ptr().add(4),
                [7, 11],
                [192, 64],
                false,
                false,
            );
            project_classic_affine_vertices(reference.as_mut_ptr(), reference.len());
            materialize_project_classic_affine_indexed_batch(
                corners.as_ptr(),
                positions.as_ptr(),
                positions.len(),
                surfaces.as_ptr(),
                sources.as_ptr(),
                surfaces.len(),
                fused.len(),
                fused.as_mut_ptr(),
            );
        }
        assert_eq!(fused, reference);
    }

    #[test]
    fn projected_batch_rejects_empty_inputs_without_touching_output() {
        let mut output = [0xdead_beefu32; 4];
        let submitted = unsafe {
            submit_classic_affine_projected_batch(
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
                0,
                output.as_mut_ptr(),
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        };
        assert_eq!(submitted.next_packet, output.as_mut_ptr());
        assert_eq!(submitted.packets, 0);
        assert_eq!(submitted.hardware_triangles, 0);
        assert_eq!(output, [0xdead_beef; 4]);
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
    fn midpoint_averages_every_colour_channel() {
        // Baked RGB light on a PXBSP floor: teal corners must stay teal at
        // the generated vertex, not collapse to the red channel.
        let a = ClassicAffineVertex {
            color: 0x00ec_c62b,
            ..ClassicAffineVertex::default()
        };
        let b = ClassicAffineVertex {
            color: 0x00ff_db31,
            ..ClassicAffineVertex::default()
        };
        assert_eq!(midpoint(&a, &b).color, 0x00f5_d02e);
        let dark = ClassicAffineVertex {
            color: 0x0000_00ff,
            ..ClassicAffineVertex::default()
        };
        assert_eq!(midpoint(&dark, &dark).color, 0x0000_00ff);
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

    #[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
    fn affine_sample(uv: [u8; 2], depth: i32) -> ClassicAffineVertex {
        ClassicAffineVertex {
            uv,
            depth,
            ..ClassicAffineVertex::default()
        }
    }

    fn affine_refs(vertices: &[ClassicAffineVertex; 3]) -> [&ClassicAffineVertex; 3] {
        [&vertices[0], &vertices[1], &vertices[2]]
    }

    #[cfg(feature = "classic-affine-depth-only-subdivision")]
    #[test]
    fn depth_only_selector_matches_every_quake_reference_otz() {
        let vertices = [ClassicAffineVertex::default(); 3];
        for otz in 0..=u16::MAX {
            let expected = if otz < ClassicAffineProfile::QUAKE_REFERENCE.subdivide_twice_at {
                2
            } else if otz < ClassicAffineProfile::QUAKE_REFERENCE.subdivide_once_at {
                1
            } else {
                0
            };
            assert_eq!(
                classic_affine_subdivision_level(
                    affine_refs(&vertices),
                    otz,
                    ClassicAffineProfile::QUAKE_REFERENCE,
                ),
                expected
            );
        }
    }

    #[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
    #[test]
    fn adaptive_profile_splits_far_oblique_texture_edges_by_predicted_error() {
        let moderate = [
            affine_sample([0, 0], 900),
            affine_sample([63, 0], 1100),
            affine_sample([0, 0], 900),
        ];
        let severe = [
            affine_sample([0, 0], 900),
            affine_sample([80, 0], 1100),
            affine_sample([0, 0], 900),
        ];
        assert_eq!(
            classic_affine_subdivision_level(
                affine_refs(&moderate),
                500,
                ClassicAffineProfile::QUAKE_REFERENCE,
            ),
            0,
            "the historical profile must remain byte-compatible"
        );
        assert_eq!(
            classic_affine_subdivision_level(
                affine_refs(&moderate),
                500,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            1,
            "7.56 predicted texels fit after one bounded bisection"
        );
        assert_eq!(
            classic_affine_subdivision_level(
                affine_refs(&severe),
                500,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            2,
            "9.6 predicted texels require the existing second lattice level"
        );
    }

    #[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
    #[test]
    fn adaptive_error_rule_ignores_constant_or_invalid_depth_edges() {
        let constant = [
            affine_sample([0, 0], 1000),
            affine_sample([255, 255], 1000),
            affine_sample([0, 255], 1000),
        ];
        let behind = [
            affine_sample([0, 0], 0),
            affine_sample([255, 255], 1000),
            affine_sample([0, 255], -1),
        ];
        for vertices in [&constant, &behind] {
            assert_eq!(
                classic_affine_subdivision_level(
                    [&vertices[0], &vertices[1], &vertices[2]],
                    500,
                    ClassicAffineProfile::RUNTIME_ADAPTIVE,
                ),
                0
            );
        }
    }

    #[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
    #[test]
    fn adaptive_error_rule_never_exceeds_the_existing_two_level_packet_bound() {
        let extreme = [
            affine_sample([0, 0], 1),
            affine_sample([255, 255], u16::MAX as i32),
            affine_sample([0, 255], 1),
        ];
        assert_eq!(
            classic_affine_subdivision_level(
                [&extreme[0], &extreme[1], &extreme[2]],
                500,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            2
        );

        let flat_near = [
            affine_sample([0, 0], 1000),
            affine_sample([0, 0], 1000),
            affine_sample([0, 0], 1000),
        ];
        assert_eq!(
            classic_affine_subdivision_level(
                [&flat_near[0], &flat_near[1], &flat_near[2]],
                50,
                ClassicAffineProfile::RUNTIME_ADAPTIVE,
            ),
            2,
            "the original close-surface depth schedule remains authoritative"
        );
    }

    #[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
    fn adaptive_packet_count(uv_span: u8, profile: ClassicAffineProfile) -> ClassicAffineSubmit {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 3 + EXTRA_VERTICES];
        for (index, (position, uv, screen, depth)) in [
            ([-100, 0, 900], [0, 0], [142, 120], 900),
            ([100, 0, 1100], [uv_span, 0], [175, 120], 1100),
            ([0, 100, 900], [0, 0], [160, 102], 900),
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                uv,
                color: 0x0080_8080,
                screen,
                depth,
            };
        }
        let mut packets = [0u32; 19 * 14];
        unsafe {
            submit_classic_affine_projected_fan(
                vertices.as_mut_ptr(),
                3,
                packets.as_mut_ptr(),
                0,
                0,
                profile,
            )
        }
    }

    #[cfg(not(feature = "classic-affine-depth-only-subdivision"))]
    #[test]
    fn adaptive_packet_goldens_use_only_the_existing_bounded_lattices() {
        let historical = adaptive_packet_count(63, ClassicAffineProfile::QUAKE_REFERENCE);
        let once = adaptive_packet_count(63, ClassicAffineProfile::RUNTIME_ADAPTIVE);
        let twice = adaptive_packet_count(80, ClassicAffineProfile::RUNTIME_ADAPTIVE);

        assert_eq!((historical.packets, historical.hardware_triangles), (1, 1));
        assert_eq!((once.packets, once.hardware_triangles), (6, 7));
        assert_eq!((twice.packets, twice.hardware_triangles), (16, 25));
        assert!(twice.packets <= 19, "packet-capacity contract changed");
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
    fn scoped_window_writer_appends_reset_inside_each_packet() {
        let vertices = [
            ClassicAffineVertex {
                screen: [10, 10],
                depth: 512,
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            },
            ClassicAffineVertex {
                screen: [20, 10],
                depth: 512,
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            },
            ClassicAffineVertex {
                screen: [10, 20],
                depth: 512,
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            },
            ClassicAffineVertex {
                screen: [20, 20],
                depth: 512,
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            },
        ];
        let mut storage = [0u32; 64];
        let output = storage.as_mut_ptr();
        let window = TextureWindow::power_of_two_tile(64, 64, 64, 64).word();
        let mut writer = WindowedPacketWriter::<true> {
            next: output,
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0160_0000,
            uv_offset: [0; 2],
            texture_window_word: window,
            color_command_word: 0x3400_0000,
            profile: ClassicAffineProfile::QUAKE_REFERENCE,
        };
        unsafe {
            writer.emit_tri(
                [&vertices[0], &vertices[1], &vertices[2]],
                [&vertices[0], &vertices[1], &vertices[2]],
                17,
            );
            writer.emit_quad(
                [&vertices[0], &vertices[1], &vertices[2], &vertices[3]],
                [&vertices[0], &vertices[1], &vertices[2], &vertices[3]],
                23,
            );
        }
        let tri_words = size_of::<TriTexturedGouraud>() / size_of::<u32>() + 1;
        let quad_words = size_of::<QuadTexturedGouraud>() / size_of::<u32>() + 1;
        assert_eq!(storage[0] >> 24, TriTexturedGouraud::WORDS as u32 + 1);
        assert_eq!(storage[tri_words - 1], TextureWindow::NONE.word());
        assert_eq!(
            storage[tri_words] >> 24,
            QuadTexturedGouraud::WORDS as u32 + 1
        );
        assert_eq!(
            storage[tri_words + quad_words - 1],
            TextureWindow::NONE.word()
        );
        let submit = unsafe { writer.finish(output) };
        assert_eq!(submit.next_packet, unsafe {
            output.add(tri_words + quad_words)
        });
        assert_eq!(submit.packets, 2);
        assert_eq!(submit.hardware_triangles, 3);
    }

    #[test]
    fn scoped_windowed_batch_restores_full_window_inside_its_ot_packet() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 3 + EXTRA_VERTICES];
        for (index, position) in [[-64, -64, 1024], [64, -64, 1024], [0, 64, 1024]]
            .into_iter()
            .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            };
        }
        let window = TextureWindow::power_of_two_tile(64, 64, 64, 64).word();
        let surface = ClassicAffineWindowedBatchSurface {
            first_vertex: 0,
            vertex_count: 3,
            tpage: 0x0160,
            clut: 0x1234,
            uv_offset: [0; 2],
            texture_window_word: window,
            color_command_word: 0x3400_0000,
        };
        let mut storage = [0u32; 19 * 15];
        let submitted = unsafe {
            submit_classic_affine_scoped_windowed_batch(
                vertices.as_mut_ptr(),
                3,
                &surface,
                1,
                storage.as_mut_ptr(),
                ClassicAffineProfile::QUAKE_REFERENCE,
            )
        };

        assert_eq!(submitted.packets, 1);
        let data_words = (storage[0] >> 24) as usize;
        assert_eq!(storage[1], window);
        assert_eq!(storage[data_words], TextureWindow::NONE.word());
        assert_eq!(submitted.next_packet, unsafe {
            storage.as_mut_ptr().add(data_words + 1)
        });
    }

    #[test]
    fn mixed_batch_keeps_compact_and_windowed_packets_in_one_projection_run() {
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);

        let mut vertices = [ClassicAffineVertex::default(); 6 + EXTRA_VERTICES];
        for (index, position) in [
            [-80, -40, 1000],
            [0, 40, 1000],
            [80, -40, 1000],
            [-60, -20, 900],
            [0, 60, 900],
            [60, -20, 900],
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index] = ClassicAffineVertex {
                position,
                uv: [index as u8, index as u8],
                color: 0x0080_8080,
                ..ClassicAffineVertex::default()
            };
        }
        let window = 0xe200_1234;
        let surfaces = [
            ClassicAffineMixedBatchSurface {
                first_vertex: 0,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                compact: 1,
                ..ClassicAffineMixedBatchSurface::default()
            },
            ClassicAffineMixedBatchSurface {
                first_vertex: 3,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                texture_window_word: window,
                color_command_word: 0x3400_0000,
                ..ClassicAffineMixedBatchSurface::default()
            },
        ];
        let mut packets = [0u32; 64];
        let profile = ClassicAffineProfile {
            subdivide_once_at: 0,
            subdivide_twice_at: 0,
            ..ClassicAffineProfile::QUAKE_REFERENCE
        };
        let submit = unsafe {
            submit_classic_affine_mixed_batch(
                vertices.as_mut_ptr(),
                6,
                surfaces.as_ptr(),
                surfaces.len(),
                packets.as_mut_ptr(),
                profile,
            )
        };

        assert_eq!((submit.packets, submit.hardware_triangles), (2, 2));
        assert_eq!(
            unsafe { submit.next_packet.offset_from(packets.as_ptr()) },
            22
        );
        assert_eq!(packets[0] >> 24, 9);
        assert_eq!(packets[1] >> 24, 0x34);
        assert_eq!(packets[10] >> 24, 11);
        assert_eq!(packets[11], window);
        assert_eq!(packets[12] >> 24, 0x34);
        assert_eq!(packets[21], TextureWindow::NONE.word());
    }

    #[test]
    fn far_keyed_surface_sorts_at_its_farthest_vertex() {
        // One sloped floor triangle spanning depths 400..1000. Average-keyed it
        // lands near the middle; far-keyed it lands where its far vertex is,
        // so anything standing on its near half draws after it.
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_avsz_weights(0x155, 0x100);
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::ZERO);
        let otz_of = |depth_law: u8| -> u16 {
            let mut vertices = [ClassicAffineVertex::default(); 3 + EXTRA_VERTICES];
            for (index, position) in [[-80, -40, 1000], [0, 40, 400], [80, -40, 700]]
                .into_iter()
                .enumerate()
            {
                vertices[index] = ClassicAffineVertex {
                    position,
                    color: 0x0080_8080,
                    ..ClassicAffineVertex::default()
                };
            }
            let surfaces = [ClassicAffineMixedBatchSurface {
                first_vertex: 0,
                vertex_count: 3,
                tpage: 0x0105,
                clut: 0x1234,
                compact: 1,
                depth_law,
                ..ClassicAffineMixedBatchSurface::default()
            }];
            let mut packets = [0u32; 32];
            let profile = ClassicAffineProfile {
                subdivide_once_at: 0,
                subdivide_twice_at: 0,
                ..ClassicAffineProfile::QUAKE_REFERENCE
            };
            let submit = unsafe {
                submit_classic_affine_mixed_batch(
                    vertices.as_mut_ptr(),
                    3,
                    surfaces.as_ptr(),
                    surfaces.len(),
                    packets.as_mut_ptr(),
                    profile,
                )
            };
            assert_eq!(submit.packets, 1);
            (packets[0] & 0xffff) as u16
        };
        let average = otz_of(0);
        let farthest = otz_of(1);
        // Identity view: SZ is the vertex z. Average of 1000, 400, 700 over 4
        // versus the far vertex 1000 over 4, on the same ZSF3 law.
        assert_eq!(average, scene::classic_otz3_from_sum(1000 + 400 + 700));
        assert_eq!(farthest, scene::classic_otz3_from_sum(3000));
        assert!(farthest > average);
    }

    #[test]
    fn windowed_writer_preserves_translucent_material_command() {
        let vertices = [
            ClassicAffineVertex {
                color: 0x0011_2233,
                ..ClassicAffineVertex::default()
            },
            ClassicAffineVertex {
                color: 0x0044_5566,
                ..ClassicAffineVertex::default()
            },
            ClassicAffineVertex {
                color: 0x0077_8899,
                ..ClassicAffineVertex::default()
            },
            ClassicAffineVertex {
                color: 0x0001_0203,
                ..ClassicAffineVertex::default()
            },
        ];
        let mut storage = [0u32; 64];
        let output = storage.as_mut_ptr();
        let mut writer = WindowedPacketWriter::<false> {
            next: output,
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0567_0000,
            uv_offset: [0; 2],
            texture_window_word: 0xe200_0000,
            color_command_word: 0x3600_0000,
            profile: ClassicAffineProfile::QUAKE_REFERENCE,
        };
        unsafe {
            writer.emit_tri(
                [&vertices[0], &vertices[1], &vertices[2]],
                [&vertices[0], &vertices[1], &vertices[2]],
                7,
            );
            writer.emit_quad(
                [&vertices[0], &vertices[1], &vertices[2], &vertices[3]],
                [&vertices[0], &vertices[1], &vertices[2], &vertices[3]],
                9,
            );
        }
        let tri = unsafe { &*output.cast::<TriTexturedGouraud>() };
        assert_eq!(tri.color0_cmd, 0x3611_2233);
        let quad = unsafe {
            &*output
                .add(size_of::<TriTexturedGouraud>() / size_of::<u32>())
                .cast::<QuadTexturedGouraud>()
        };
        assert_eq!(quad.color0_cmd, 0x3e11_2233);
    }

    #[test]
    fn projected_packet_writers_reject_wholly_offscreen_primitives() {
        let mut vertices = [ClassicAffineVertex::default(); 4];
        for (index, screen) in [[-20, 20], [-10, 80], [-1, 140], [-30, 200]]
            .into_iter()
            .enumerate()
        {
            vertices[index].screen = screen;
            vertices[index].color = 0x0080_8080;
        }
        let mut storage = [0u32; 64];
        let output = storage.as_mut_ptr();
        let mut writer = WindowedPacketWriter::<true> {
            next: output,
            packets: 0,
            clut_high_word: 0x1234_0000,
            tpage_high_word: 0x0567_0000,
            uv_offset: [0; 2],
            texture_window_word: 0xe200_0000,
            color_command_word: 0x3400_0000,
            profile: ClassicAffineProfile::QUAKE_REFERENCE,
        };
        unsafe {
            writer.emit_tri(
                [&vertices[0], &vertices[1], &vertices[2]],
                [&vertices[0], &vertices[1], &vertices[2]],
                7,
            );
            writer.emit_quad(
                [&vertices[0], &vertices[1], &vertices[2], &vertices[3]],
                [&vertices[0], &vertices[1], &vertices[2], &vertices[3]],
                9,
            );
        }
        assert_eq!(writer.packets, 0);
        assert_eq!(writer.next, output);
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
    fn branchless_clip_code_keeps_inclusive_viewport_boundaries() {
        let profile = ClassicAffineProfile::QUAKE_REFERENCE;
        assert_eq!(classic_clip_code([0, 0], profile), 0);
        assert_eq!(classic_clip_code([318, 238], profile), 0);
        assert_eq!(classic_clip_code([319, 239], profile), 0);
        assert_eq!(classic_clip_code([-1, 120], profile), 1);
        assert_eq!(classic_clip_code([320, 120], profile), 2);
        assert_eq!(classic_clip_code([160, -1], profile), 4);
        assert_eq!(classic_clip_code([160, 240], profile), 8);
        assert_eq!(classic_clip_code([-1024, -1024], profile), 5);
        assert_eq!(classic_clip_code([1023, 1023], profile), 10);
    }
}
