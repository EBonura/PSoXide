//! The PSVG wire format, version 1.
//!
//! All integers are little-endian. The asset must be 4-byte aligned in memory
//! (see [`include_vector!`](crate::include_vector)) so the record sections are read in
//! place with no copy. Sections follow the header in this order, each padded to
//! a multiple of four bytes:
//!
//! | Section  | Record            | Size | Count                |
//! |----------|-------------------|------|----------------------|
//! | header   | see [`HEADER_LEN`] | 56   | 1                    |
//! | groups   | [`GroupRecord`]   | 40   | `group_count`        |
//! | vertices | [`Vertex`]        | 8    | `vertex_count`       |
//! | indices  | `u16`             | 2    | `index_count`        |
//! | runs     | [`Run`]           | 4    | `run_count`          |
//! | names    | UTF-8 bytes       | 1    | `names_len`          |
//!
//! Header layout (byte offsets): `0` magic `"PSVG"`, `4` version (`u16`),
//! `6` group count (`u16`), `8` canvas width and `10` canvas height (`u16`, Q4
//! pixels), then `u32` fields at `12` vertex count, `16` index count, `20` run
//! count, `24` names length, `28..48` section offsets (groups, vertices,
//! indices, runs, names), `48` total length, `52` reserved (zero).
//!
//! A group owns a contiguous range of vertices, a contiguous range of indices
//! (local to the group's first vertex) and a contiguous range of runs.
//! Primitives are in paint order: a primitive is a triangle (three indices) or
//! a quad (four indices, drawn as the GPU draws a GP0 quad: triangles `0 1 2`
//! and `1 2 3`). A run is a consecutive stretch of primitives of one kind drawn
//! with the same blend mode, so painter's order survives mode and kind changes.

use psx_gpu::material::BlendMode;

/// File magic.
pub const MAGIC: [u8; 4] = *b"PSVG";
/// Format version written by the cooker and accepted by the reader.
pub const VERSION: u16 = 1;
/// Fractional bits of every cooked coordinate: positions are in 1/16 pixel.
pub const COORD_SHIFT: u32 = 4;
/// Size of the file header in bytes.
pub const HEADER_LEN: usize = 56;
/// Most vertices one group can hold (indices are `u16`).
pub const MAX_GROUP_VERTICES: usize = 1 << 16;
/// Most triangles one group can hold (`tri_count` is `u16`).
pub const MAX_GROUP_TRIANGLES: usize = u16::MAX as usize;

/// One group: a named, independently drawable set of triangles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct GroupRecord {
    /// Byte offset of the name in the names section.
    pub name_off: u16,
    /// Length of the name in bytes.
    pub name_len: u8,
    /// Reserved, zero.
    pub flags: u8,
    /// Index of the group's first vertex in the vertex section.
    pub vertex_start: u32,
    /// Number of vertices.
    pub vertex_count: u16,
    /// Number of triangle primitives.
    pub tri_count: u16,
    /// Index (in `u16` units) of the group's first index in the index section.
    pub index_start: u32,
    /// Index of the group's first run in the run section.
    pub run_start: u32,
    /// Number of runs.
    pub run_count: u16,
    /// Number of quad primitives.
    pub quad_count: u16,
    /// Bounding box `[min_x, min_y, max_x, max_y]` in Q4 pixels.
    pub bounds: [i16; 4],
    /// Pivot `[x, y]` in Q4 pixels, the default rotation and scale centre.
    pub pivot: [i16; 2],
    /// Reserved, zero.
    pub reserved2: u32,
}

/// One vertex: a Q4 position and a GP0 colour word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct Vertex {
    /// X in Q4 pixels.
    pub x: i16,
    /// Y in Q4 pixels.
    pub y: i16,
    /// `r | g << 8 | b << 16`, the colour word GP0 takes; the top byte is zero.
    pub color: u32,
}

/// A stretch of consecutive triangles that share one blend mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct Run {
    /// Blend code, see [`blend_from_code`].
    pub blend: u8,
    /// Primitive kind of the run, [`RUN_TRIANGLES`] or [`RUN_QUADS`].
    pub kind: u8,
    /// Number of primitives in the run.
    pub count: u16,
}

const _: () = assert!(core::mem::size_of::<GroupRecord>() == 40);
const _: () = assert!(core::mem::size_of::<Vertex>() == 8);
const _: () = assert!(core::mem::size_of::<Run>() == 4);

/// Blend code for an opaque run.
pub const BLEND_OPAQUE: u8 = 0;
/// [`Run::kind`] of a run of triangles (three indices each).
pub const RUN_TRIANGLES: u8 = 0;
/// [`Run::kind`] of a run of quads (four indices each).
pub const RUN_QUADS: u8 = 1;

/// Pack `r`, `g`, `b` into a [`Vertex::color`] word.
#[inline]
pub const fn pack_rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// The blend mode a run's code names, or `None` for an unknown code.
///
/// Codes: 0 opaque, 1 average, 2 add, 3 subtract, 4 add-quarter.
#[inline]
pub const fn blend_from_code(code: u8) -> Option<BlendMode> {
    match code {
        0 => Some(BlendMode::Opaque),
        1 => Some(BlendMode::Average),
        2 => Some(BlendMode::Add),
        3 => Some(BlendMode::Subtract),
        4 => Some(BlendMode::AddQuarter),
        _ => None,
    }
}

/// The run code for a blend mode, the inverse of [`blend_from_code`].
#[inline]
pub const fn blend_to_code(mode: BlendMode) -> u8 {
    match mode {
        BlendMode::Opaque => 0,
        BlendMode::Average => 1,
        BlendMode::Add => 2,
        BlendMode::Subtract => 3,
        BlendMode::AddQuarter => 4,
    }
}
