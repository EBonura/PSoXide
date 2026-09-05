//! Shared contract for cooked grid-world assets.
//!
//! Each `.psxw` blob is a fixed-size grid room: a world header, one
//! sector record per grid cell, then a compact wall table referenced
//! by sectors. Only `VERSION = 5` is emitted by the cooker; v1-v4
//! are legacy parser compatibility only.
//!
//! # Active format (VERSION = 5)
//!
//! - `[i32; 4]` heights per face -- 16 B per height set
//! - `QuadUvRecord` per floor / ceiling / wall -- 8 B per face
//! - 60 B sector record, 32 B wall record
//! - Wall records may store a non-quad shape in their 16-bit
//!   shape field, allowing one corner to be dropped for triangular
//!   vertical faces without changing the record size.
//! - Optional `HorizontalOverrideRecord` side table for floor /
//!   ceiling faces whose two split triangles have different material,
//!   UV, walkability, visibility, or triangle-local heights.
//! - Optional appended `SurfaceLightRecord` table when
//!   [`world_flags::STATIC_VERTEX_LIGHTING`] is set. The table is
//!   direct-indexed as `sector_count * 2` floor/ceiling slots,
//!   followed by one slot per wall record.
//! - No embedded material table; slot ids resolve via an external
//!   bank that the caller (engine / playtest manifest) supplies
//! - No sector logic stream, no portal records
//!
//! # Legacy format (VERSION = 4)
//!
//! - Active v3 header plus horizontal override side table for
//!   material, UV, walkability, and visibility, but no split
//!   triangle-local heights.
//!
//! # Legacy format (VERSION = 3)
//!
//! - 60 B sector record, 32 B wall record
//! - Per-face UVs and optional embedded vertex lighting.
//!
//! # Legacy format (VERSION = 2)
//!
//! - 60 B sector record, 32 B wall record
//! - Per-face UVs, but no embedded vertex lighting.
//!
//! # Legacy format (VERSION = 1)
//!
//! - 44 B sector record, 24 B wall record
//! - No UV records; readers synthesize the engine's default 64×64
//!   tile UVs.
//!
//! # Future compact format
//!
//! A more compact runtime format is sketched in
//! `docs/world-format-roadmap.md`. It does not live in this crate
//! as Rust types: `psxed-format` is the producer/consumer contract,
//! and a record only belongs here once both the cooker emits it
//! and the runtime parser accepts it. Until then the design stays
//! in docs.

/// ASCII magic for the `.psxw` grid-world format.
pub const MAGIC: [u8; 4] = *b"PSXW";

/// Legacy world format revision without per-face UV records.
pub const VERSION_V1: u16 = 1;

/// Legacy world format revision with per-face UV records but no
/// embedded static vertex lighting.
pub const VERSION_V2: u16 = 2;

/// Legacy world format revision with per-face UV records and optional
/// embedded static vertex lighting.
pub const VERSION_V3: u16 = 3;

/// Legacy world format revision with horizontal override records
/// for material, UV, walkability, and visibility.
pub const VERSION_V4: u16 = 4;

/// Current world format revision.
pub const VERSION: u16 = 5;

/// Canonical/default engine units per grid sector.
pub const SECTOR_SIZE: i32 = 1024;

/// Material sentinel used by missing optional floor/ceiling records.
pub const NO_MATERIAL: u16 = u16::MAX;

/// North-west horizontal face corner index.
pub const CORNER_NW: usize = 0;

/// North-east horizontal face corner index.
pub const CORNER_NE: usize = 1;

/// South-east horizontal face corner index.
pub const CORNER_SE: usize = 2;

/// South-west horizontal face corner index.
pub const CORNER_SW: usize = 3;

/// Bottom-left vertical wall corner index.
pub const WALL_BOTTOM_LEFT: usize = 0;

/// Bottom-right vertical wall corner index.
pub const WALL_BOTTOM_RIGHT: usize = 1;

/// Top-right vertical wall corner index.
pub const WALL_TOP_RIGHT: usize = 2;

/// Top-left vertical wall corner index.
pub const WALL_TOP_LEFT: usize = 3;

/// Stored values for diagonal split directions.
pub mod split {
    /// Split from north-west to south-east.
    pub const NORTH_WEST_SOUTH_EAST: u8 = 0;

    /// Split from north-east to south-west.
    pub const NORTH_EAST_SOUTH_WEST: u8 = 1;
}

/// Stored values for sector wall directions.
pub mod direction {
    /// North edge, negative Z.
    pub const NORTH: u8 = 0;

    /// East edge, positive X.
    pub const EAST: u8 = 1;

    /// South edge, positive Z.
    pub const SOUTH: u8 = 2;

    /// West edge, negative X.
    pub const WEST: u8 = 3;

    /// Diagonal edge from north-west to south-east.
    pub const NORTH_WEST_SOUTH_EAST: u8 = 4;

    /// Diagonal edge from north-east to south-west.
    pub const NORTH_EAST_SOUTH_WEST: u8 = 5;
}

/// World-level feature flags stored in `AssetHeader::flags`.
pub mod flags {
    /// Reserved for future multi-room payloads.
    pub const RESERVED: u16 = 0;
}

/// World payload flags stored in [`WorldHeader::flags`].
pub mod world_flags {
    /// PS1 depth cue/fog is enabled for this grid.
    pub const FOG_ENABLED: u8 = 1 << 0;

    /// Face records carry baked static per-vertex room lighting.
    pub const STATIC_VERTEX_LIGHTING: u8 = 1 << 1;
}

/// Sector flags stored in [`SectorRecord::flags`].
pub mod sector_flags {
    /// Sector has a floor face.
    pub const HAS_FLOOR: u8 = 1 << 0;

    /// Sector has a ceiling face.
    pub const HAS_CEILING: u8 = 1 << 1;

    /// Floor face is walkable.
    pub const FLOOR_WALKABLE: u8 = 1 << 2;

    /// Ceiling face is walkable.
    pub const CEILING_WALKABLE: u8 = 1 << 3;
}

/// Horizontal split-triangle flags stored in
/// [`HorizontalOverrideRecord::flags`].
pub mod horizontal_flags {
    /// Triangle A is present for the referenced floor/ceiling face.
    pub const TRI_A_PRESENT: u8 = 1 << 0;

    /// Triangle B is present for the referenced floor/ceiling face.
    pub const TRI_B_PRESENT: u8 = 1 << 1;

    /// Triangle A is walkable for collision sampling.
    pub const TRI_A_WALKABLE: u8 = 1 << 2;

    /// Triangle B is walkable for collision sampling.
    pub const TRI_B_WALKABLE: u8 = 1 << 3;
}

/// Horizontal surface ids stored in
/// [`HorizontalOverrideRecord::surface`].
pub mod horizontal_surface {
    /// Sector floor.
    pub const FLOOR: u8 = 0;

    /// Sector ceiling.
    pub const CEILING: u8 = 1;
}

/// Wall flags stored in [`WallRecord::flags`].
pub mod wall_flags {
    /// Wall blocks collision.
    pub const SOLID: u8 = 1 << 0;
}

/// Stored values for [`WallRecord::shape`].
pub mod wall_shape {
    /// Full four-corner quad wall.
    pub const QUAD: u16 = 0;

    /// Triangle wall with the bottom-left corner removed.
    pub const DROP_BOTTOM_LEFT: u16 = 1;

    /// Triangle wall with the bottom-right corner removed.
    pub const DROP_BOTTOM_RIGHT: u16 = 2;

    /// Triangle wall with the top-right corner removed.
    pub const DROP_TOP_RIGHT: u16 = 3;

    /// Triangle wall with the top-left corner removed.
    pub const DROP_TOP_LEFT: u16 = 4;
}

/// Shared quad tessellation helpers for horizontal faces and
/// dropped-corner wall faces.
pub mod topology {
    use super::{
        split, wall_shape, CORNER_NE, CORNER_NW, CORNER_SE, CORNER_SW, WALL_BOTTOM_LEFT,
        WALL_BOTTOM_RIGHT, WALL_TOP_LEFT, WALL_TOP_RIGHT,
    };

    /// Three corner indices making up one triangle.
    pub type TriangleCorners = [usize; 3];

    /// The two triangles that compose a quad.
    pub type SplitTriangles = [TriangleCorners; 2];

    /// Triangle index used by runtime caches for unsplit whole quads.
    pub const WHOLE_QUAD_TRIANGLE_INDEX: u8 = 2;

    /// Triangles for a split from corner 0 to corner 2.
    pub const SPLIT_ZERO_TWO_TRIANGLES: SplitTriangles = [
        [CORNER_NW, CORNER_NE, CORNER_SE],
        [CORNER_NW, CORNER_SE, CORNER_SW],
    ];

    /// Triangles for a split from corner 1 to corner 3.
    pub const SPLIT_ONE_THREE_TRIANGLES: SplitTriangles = [
        [CORNER_NW, CORNER_NE, CORNER_SW],
        [CORNER_NE, CORNER_SE, CORNER_SW],
    ];

    /// Triangle indices for a horizontal NW-SE split.
    pub const HORIZONTAL_NW_SE_TRIANGLES: SplitTriangles = SPLIT_ZERO_TWO_TRIANGLES;

    /// Triangle indices for a horizontal NE-SW split.
    pub const HORIZONTAL_NE_SW_TRIANGLES: SplitTriangles = SPLIT_ONE_THREE_TRIANGLES;

    /// Resolve a split id to the two triangle corner sets. Unknown
    /// split ids fall back to NW-SE so malformed data still draws a
    /// coherent full surface.
    pub const fn split_triangles(split_id: u8) -> SplitTriangles {
        if split_id == split::NORTH_EAST_SOUTH_WEST {
            SPLIT_ONE_THREE_TRIANGLES
        } else {
            SPLIT_ZERO_TWO_TRIANGLES
        }
    }

    /// Resolve one triangle for `split_id`. Indices other than zero
    /// select the second triangle.
    pub const fn split_triangle(split_id: u8, triangle_index: usize) -> TriangleCorners {
        let triangles = split_triangles(split_id);
        if triangle_index == 0 {
            triangles[0]
        } else {
            triangles[1]
        }
    }

    /// Return `true` when `triangle` contains `corner`.
    pub const fn triangle_contains_corner(triangle: TriangleCorners, corner: usize) -> bool {
        triangle[0] == corner || triangle[1] == corner || triangle[2] == corner
    }

    /// Runtime horizontal triangle for a local X/Z point. Coordinates
    /// use the `.psxw` convention: local Z=0 is the north edge.
    pub const fn horizontal_triangle_at_local(
        split_id: u8,
        local_x: i32,
        local_z: i32,
        sector_size: i32,
    ) -> usize {
        let sector = if sector_size > 0 { sector_size } else { 1 };
        let u = clamp_i32(local_x, 0, sector);
        let v = clamp_i32(local_z, 0, sector);
        if split_id == split::NORTH_EAST_SOUTH_WEST {
            if u + v <= sector {
                0
            } else {
                1
            }
        } else if v <= u {
            0
        } else {
            1
        }
    }

    /// Surviving triangle for a dropped-corner wall shape. The
    /// returned split id uses the generic quad-corner numbering:
    /// `[bottom-left, bottom-right, top-right, top-left]`.
    pub const fn wall_shape_triangle(shape: u16) -> Option<(u8, u8)> {
        match shape {
            wall_shape::DROP_BOTTOM_LEFT => Some((split::NORTH_EAST_SOUTH_WEST, 1)),
            wall_shape::DROP_BOTTOM_RIGHT => Some((split::NORTH_WEST_SOUTH_EAST, 1)),
            wall_shape::DROP_TOP_RIGHT => Some((split::NORTH_EAST_SOUTH_WEST, 0)),
            wall_shape::DROP_TOP_LEFT => Some((split::NORTH_WEST_SOUTH_EAST, 0)),
            _ => None,
        }
    }

    /// Wall-corner members for the triangle surviving `shape`.
    pub const fn wall_shape_triangle_corners(shape: u16) -> Option<TriangleCorners> {
        match wall_shape_triangle(shape) {
            Some((split_id, triangle_index)) => {
                Some(split_triangle(split_id, triangle_index as usize))
            }
            None => None,
        }
    }

    /// Shape id for removing one wall corner.
    pub const fn wall_shape_for_dropped_corner(corner: usize) -> u16 {
        match corner {
            WALL_BOTTOM_LEFT => wall_shape::DROP_BOTTOM_LEFT,
            WALL_BOTTOM_RIGHT => wall_shape::DROP_BOTTOM_RIGHT,
            WALL_TOP_RIGHT => wall_shape::DROP_TOP_RIGHT,
            WALL_TOP_LEFT => wall_shape::DROP_TOP_LEFT,
            _ => wall_shape::QUAD,
        }
    }

    const fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
        if value < min {
            min
        } else if value > max {
            max
        } else {
            value
        }
    }
}

/// Default texture-page UV span for grid tile faces.
pub const TILE_UV: u8 = 64;

/// Default floor / ceiling UVs in `[NW, NE, SE, SW]` order.
pub const FLOOR_UVS: [(u8, u8); 4] = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];

/// Default wall UVs in `[bottom-left, bottom-right, top-right, top-left]` order.
pub const WALL_UVS: [(u8, u8); 4] = [(0, TILE_UV), (TILE_UV, TILE_UV), (TILE_UV, 0), (0, 0)];

/// Four packed PS1 texture coordinates for one quad face.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct QuadUvRecord {
    /// Per-corner `[u, v]` pairs in the same order as the face's
    /// height / vertex corners.
    pub corners: [[u8; 2]; 4],
}

impl QuadUvRecord {
    /// Size of one UV record in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Four RGB vertex colours for one world quad face.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct SurfaceLightRecord {
    /// Per-corner RGB values in the same order as the face's
    /// height / UV corners.
    pub vertex_rgb: [[u8; 3]; 4],
}

impl SurfaceLightRecord {
    /// Size of one surface-light record in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Payload header that follows the common `AssetHeader`.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct WorldHeader {
    /// Width in sectors.
    pub width: u16,
    /// Depth in sectors.
    pub depth: u16,
    /// Engine units per sector.
    pub sector_size: i32,
    /// Number of sector records following this header.
    pub sector_count: u16,
    /// Number of material slots referenced by faces.
    pub material_count: u16,
    /// Number of wall records after the sector table.
    pub wall_count: u16,
    /// Room ambient RGB color.
    pub ambient_color: [u8; 3],
    /// World payload flags, see [`world_flags`].
    pub flags: u8,
    /// Number of appended [`SurfaceLightRecord`]s. Zero when
    /// [`world_flags::STATIC_VERTEX_LIGHTING`] is not set.
    pub surface_light_count: u16,
    /// Number of [`HorizontalOverrideRecord`]s following the wall table.
    pub horizontal_override_count: u16,
    /// Reserved. Writers store zero; readers ignore.
    pub _reserved: u16,
}

impl WorldHeader {
    /// Size of the world header in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Optional per-triangle floor / ceiling data for one sector surface.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct HorizontalOverrideRecord {
    /// Flat sector index in `[x * depth + z]` order.
    pub sector_index: u16,
    /// Surface kind, see [`horizontal_surface`].
    pub surface: u8,
    /// Triangle presence and walkability bits, see [`horizontal_flags`].
    pub flags: u8,
    /// Material slot for triangle A or [`NO_MATERIAL`].
    pub material_a: u16,
    /// Material slot for triangle B or [`NO_MATERIAL`].
    pub material_b: u16,
    /// Triangle A UVs in `[NW, NE, SE, SW]` corner order.
    pub uvs_a: QuadUvRecord,
    /// Triangle B UVs in `[NW, NE, SE, SW]` corner order.
    pub uvs_b: QuadUvRecord,
    /// Triangle A heights in its split-corner order.
    pub heights_a: [i32; 3],
    /// Triangle B heights in its split-corner order.
    pub heights_b: [i32; 3],
}

impl HorizontalOverrideRecord {
    /// Size of one horizontal override record in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Fixed sector record in flat `[x * depth + z]` order.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct SectorRecord {
    /// Sector flags, see [`sector_flags`].
    pub flags: u8,
    /// Floor split id, see [`split`].
    pub floor_split: u8,
    /// Ceiling split id, see [`split`].
    pub ceiling_split: u8,
    /// Reserved padding. Writers store zero; readers ignore.
    pub _pad: u8,
    /// Floor material slot or [`NO_MATERIAL`].
    pub floor_material: u16,
    /// Ceiling material slot or [`NO_MATERIAL`].
    pub ceiling_material: u16,
    /// First wall record index for this sector.
    pub first_wall: u16,
    /// Number of wall records belonging to this sector.
    pub wall_count: u16,
    /// Floor heights `[NW, NE, SE, SW]`.
    pub floor_heights: [i32; 4],
    /// Ceiling heights `[NW, NE, SE, SW]`.
    pub ceiling_heights: [i32; 4],
    /// Floor UVs `[NW, NE, SE, SW]`.
    pub floor_uvs: QuadUvRecord,
    /// Ceiling UVs `[NW, NE, SE, SW]`.
    pub ceiling_uvs: QuadUvRecord,
}

impl SectorRecord {
    /// Size of one sector record in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Variable wall record referenced by a sector.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct WallRecord {
    /// Wall direction id, see [`direction`].
    pub direction: u8,
    /// Wall flags, see [`wall_flags`].
    pub flags: u8,
    /// Reserved padding. Writers store zero; readers ignore.
    pub _pad: u16,
    /// Material slot.
    pub material: u16,
    /// Wall shape id, see [`wall_shape`]. Legacy writers store zero.
    pub shape: u16,
    /// Wall heights `[bottom-left, bottom-right, top-right, top-left]`.
    pub heights: [i32; 4],
    /// Wall UVs `[bottom-left, bottom-right, top-right, top-left]`.
    pub uvs: QuadUvRecord,
}

impl WallRecord {
    /// Size of one wall record in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}
