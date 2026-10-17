//! Wire records of the PGL grid level format.
//!
//! Two parts. The **level core** (`PGL1`) is a small resident blob: room
//! directory, portal graph, material table. One **room chunk** (`PGRM`) per
//! room holds the collision block first and the render block second.
//!
//! Every record is a fixed-size little-endian `#[repr(C)]` struct of plain
//! integers with no invalid bit patterns, so a table is a cast of the file
//! bytes, no parse and no copy. Every table starts on a 4-byte boundary
//! relative to its blob and every record size is a multiple of 4. Sizes and
//! the field offsets that matter are asserted at compile time and folded into
//! [`LAYOUT_HASH`], which both files carry, so a reader built against another
//! layout refuses the file instead of misreading it.
//!
//! Units: the **runtime unit** is the engine unit (editor unit / 16), the
//! same unit as `RoomPoint`. A sector is `sector_units` of them (S). Vertices,
//! heights and local offsets inside a room chunk are room-local `i16`; the
//! room's world position is [`RoomRecord::origin`].

const _: () = assert!(cfg!(target_endian = "little"));

/// Level core magic, `"PGL1"`.
pub const LEVEL_MAGIC: [u8; 4] = *b"PGL1";
/// Room chunk magic, `"PGRM"`.
pub const ROOM_MAGIC: [u8; 4] = *b"PGRM";
/// Layout version written by this crate.
pub const VERSION: u16 = 1;
/// Oldest reader that still understands what this crate writes.
pub const MIN_READER: u16 = 1;
/// Bytes per CD sector; chunks are padded to it.
pub const SECTOR_BYTES: usize = 2048;

/// Level flag: some room shows sky.
pub const LEVEL_HAS_SKY: u32 = 1;
/// Level flag: some room holds a liquid volume.
pub const LEVEL_HAS_LIQUIDS: u32 = 1 << 1;
/// Level flag: some room holds free planes.
pub const LEVEL_HAS_PLANES: u32 = 1 << 2;
/// Level flag: some room holds movers.
pub const LEVEL_HAS_MOVERS: u32 = 1 << 3;

/// Room flag: sky may show (an open sector exists).
pub const ROOM_OUTDOOR: u16 = 1;
/// Room flag: distance fog.
pub const ROOM_FOG: u16 = 1 << 1;
/// Room flag: always resident.
pub const ROOM_PINNED: u16 = 1 << 2;
/// Room flag: holds a liquid volume.
pub const ROOM_HAS_LIQUID: u16 = 1 << 3;

/// Core file header, 32 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LevelHeader {
    pub magic: [u8; 4],
    pub version: u16,
    pub min_reader: u16,
    pub flags: u32,
    pub room_count: u16,
    pub portal_count: u16,
    pub material_count: u16,
    /// Runtime units per sector edge (S).
    pub sector_units: u16,
    /// Worst-case resident 2 KiB pages for the declared radius.
    pub pool_pages: u16,
    /// Rooms within this many portal hops of the player room stay pinned.
    pub resident_hops: u8,
    /// Additional hops prefetched, evictable.
    pub prefetch_hops: u8,
    pub reserved: u32,
    pub layout_hash: u32,
}

/// One room in the directory, 40 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomRecord {
    /// First sector of the room chunk inside `WORLD.PAK`.
    pub pack_sector: u32,
    pub pack_sectors: u16,
    pub flags: u16,
    /// World runtime-unit position of local cell (0,0) at the room's vertical
    /// offset.
    pub origin: [i32; 3],
    /// Sectors along X.
    pub width: u8,
    /// Sectors along Z.
    pub depth: u8,
    pub portal_first: u16,
    pub portal_count: u16,
    /// Lowest geometry, room-local.
    pub min_y: i16,
    pub max_y: i16,
    pub fog_near: u16,
    pub fog_far: u16,
    pub fog_rgb: [u8; 3],
    pub reserved: [u8; 3],
}

/// Portal flag: floor or ceiling opening.
pub const PORTAL_VERTICAL: u16 = 1;
/// Portal flag: always open.
pub const PORTAL_ALWAYS_OPEN: u16 = 1 << 1;

/// A directed opening to a neighbouring room, 36 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Portal {
    pub dest: u16,
    pub flags: u16,
    /// Opening corners in ring order, source-room local.
    pub v: [[i16; 3]; 4],
    /// Q12 normal pointing into `dest`.
    pub normal: [i16; 3],
    pub reserved: u16,
}

/// Material flag mask: sidedness, 0 front, 1 back, 2 both.
pub const MATERIAL_SIDED_MASK: u16 = 3;
/// Material flag: palette index 0 is transparent.
pub const MATERIAL_CUTOUT: u16 = 1 << 2;
/// Material flag: liquid surface.
pub const MATERIAL_LIQUID: u16 = 1 << 3;

/// A level-global material, 8 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Material {
    /// Asset id of the texture in the manifest asset table.
    pub texture_asset: u16,
    pub flags: u16,
    /// 0 opaque, 1 average, 2 add, 3 subtract, 4 add-quarter.
    pub blend: u8,
    /// Index into the manifest's UV-motion table, 0 none.
    pub uv_motion: u8,
    pub reserved: u16,
}

/// Room chunk flag: carries distance-LOD quads.
pub const CHUNK_HAS_LOD: u16 = 1;
/// Room chunk flag: carries free planes.
pub const CHUNK_HAS_PLANES: u16 = 1 << 1;
/// Room chunk flag: carries liquids.
pub const CHUNK_HAS_LIQUIDS: u16 = 1 << 2;
/// Room chunk flag: carries movers.
pub const CHUNK_HAS_MOVERS: u16 = 1 << 3;

/// Number of sections in a room chunk.
pub const SECTION_COUNT: usize = 15;

/// Section ids, in file order.
pub mod section {
    /// Dense sector records (collision).
    pub const SECTORS: usize = 0;
    /// Sparse per-sector extension records.
    pub const SECTOR_EXTS: usize = 1;
    /// Solid wall pieces.
    pub const WALLS: usize = 2;
    /// Split-level second triangles.
    pub const SPLITBS: usize = 3;
    /// Collidable free planes, copied per overlapped cell.
    pub const COL_PLANES: usize = 4;
    /// Liquid volumes.
    pub const LIQUIDS: usize = 5;
    /// Rigid movers.
    pub const MOVERS: usize = 6;
    /// Vertex indices owned by movers.
    pub const MOVER_VERTS: usize = 7;
    /// Level-global material ids the room draws.
    pub const MATERIAL_USE: usize = 8;
    /// Lattice vertices.
    pub const VERTICES: usize = 9;
    /// Quads, cell lists first.
    pub const QUADS: usize = 10;
    /// Dense render cells.
    pub const CELLS: usize = 11;
    /// LOD blocks.
    pub const LOD_BLOCKS: usize = 12;
    /// Merged far quads.
    pub const LOD_QUADS: usize = 13;
    /// Free-plane render references.
    pub const PLANE_REFS: usize = 14;
}

/// Offset and element count of one section, chunk-relative.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionRef {
    pub offset: u32,
    pub count: u32,
}

/// Room chunk header, 152 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomHeader {
    pub magic: [u8; 4],
    pub version: u16,
    pub flags: u16,
    /// Unpadded chunk length; equals the pack entry's byte size.
    pub byte_len: u32,
    /// Byte offset of the render block (first byte after the collision block).
    pub render_offset: u32,
    pub room_id: u16,
    pub width: u8,
    pub depth: u8,
    pub min_y: i16,
    pub max_y: i16,
    /// Distance in sectors beyond which merged quads replace cell quads.
    pub lod_far_cells: u8,
    /// log2 of the LOD block edge in cells.
    pub lod_shift: u8,
    pub reserved: u16,
    pub sections: [SectionRef; SECTION_COUNT],
    pub reserved2: u32,
}

/// Sector flag: has a floor (clear is a pit).
pub const SECTOR_HAS_FLOOR: u16 = 1;
/// Sector flag: has a ceiling.
pub const SECTOR_HAS_CEIL: u16 = 1 << 1;
/// Sector flag: floor diagonal runs NE-SW, else NW-SE.
pub const SECTOR_FLOOR_NESW: u16 = 1 << 2;
/// Sector flag: ceiling diagonal runs NE-SW.
pub const SECTOR_CEIL_NESW: u16 = 1 << 3;
/// Sector flag: floor triangle A is walkable.
pub const SECTOR_FLOOR_WALK_A: u16 = 1 << 4;
/// Sector flag: floor triangle B is walkable.
pub const SECTOR_FLOOR_WALK_B: u16 = 1 << 5;
/// Sector flag: the ceiling blocks the head.
pub const SECTOR_CEIL_SOLID: u16 = 1 << 6;
/// Sector flag: no ceiling and the room shows sky here.
pub const SECTOR_OPEN_SKY: u16 = 1 << 7;
/// Sector flag: some liquid record covers the sector.
pub const SECTOR_LIQUID: u16 = 1 << 8;
/// Sector flag: the second triangle has its own heights ([`SplitB`]).
pub const SECTOR_SPLIT_LEVEL: u16 = 1 << 9;

// Summary bits (10 to 12) are derived by the writer
// (`collision::summarize_sectors`) so the hot collision paths can skip work.
// They were reserved before, readers that ignore them stay correct, and a
// file without them is still correct, only slower.

/// Summary flag: has a floor, its four corners are equal and the sector is
/// not split level, so the floor height is `floor[0]` everywhere.
pub const SECTOR_FLAT_FLOOR: u16 = 1 << 10;
/// Summary flag: has a ceiling, its four corners are equal and the sector is
/// not split level, so the ceiling height is `ceil[0]` everywhere.
pub const SECTOR_FLAT_CEIL: u16 = 1 << 11;
/// Summary flag: this sector and its eight plan neighbours (all inside the
/// room) have no wall pieces, planes or split level, one shared flat floor
/// height, and either no solid ceiling at all or one shared flat solid
/// ceiling at least `CLEAR_HEADROOM` units above the floor. A body no wider
/// than a sector centred here sees exactly one floor slab and at most one
/// ceiling slab, and no walls.
pub const SECTOR_CLEAR: u16 = 1 << 12;
/// Headroom, runtime units, a shared solid ceiling needs for [`SECTOR_CLEAR`].
pub const CLEAR_HEADROOM: i32 = 160;

/// Marker for "no extension".
pub const NO_EXT: u16 = 0xFFFF;

/// One collision sector, 20 bytes. Corners are `[NW, NE, SE, SW]`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sector {
    pub floor: [i16; 4],
    pub ceil: [i16; 4],
    pub flags: u16,
    /// Index into `SECTOR_EXTS` or [`NO_EXT`].
    pub ext: u16,
}

/// Rarely-needed per-sector data, 8 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SectorExt {
    pub wall_first: u16,
    pub wall_count: u8,
    pub plane_count: u8,
    pub plane_first: u16,
    /// Index into `SPLITBS` or [`NO_EXT`].
    pub splitb: u16,
}

/// Edge ids of [`ColWall::edge`], same order as the document's `WallEdge::ALL`.
pub mod edge {
    /// `-Z` side.
    pub const NORTH: u8 = 0;
    /// `+X` side.
    pub const EAST: u8 = 1;
    /// `+Z` side.
    pub const SOUTH: u8 = 2;
    /// `-X` side.
    pub const WEST: u8 = 3;
    /// NW to SE diagonal.
    pub const NWSE: u8 = 4;
    /// NE to SW diagonal.
    pub const NESW: u8 = 5;
}

/// Wall flag: blocks only bodies coming from the front side.
pub const WALL_FRONT_ONLY: u8 = 1;
/// Wall flag: owned by a mover.
pub const WALL_MOVER: u8 = 2;

/// A solid wall piece, 12 bytes. Geometry follows from the owning sector and
/// the edge id.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColWall {
    pub edge: u8,
    pub flags: u8,
    /// `[bottom at a, bottom at b, top at b, top at a]` walking `a -> b`.
    pub y: [i16; 4],
    pub reserved: u16,
}

/// Corner heights of the second triangle of a split-level sector, 16 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitB {
    pub floor_b: [i16; 4],
    pub ceil_b: [i16; 4],
}

/// Plane collision kinds.
pub mod plane_kind {
    /// Walkable platform.
    pub const PLATFORM: u8 = 1;
    /// Free-standing vertical wall.
    pub const WALL: u8 = 2;
    /// Axis-aligned blocker box.
    pub const BLOCKER: u8 = 3;
}

/// A collidable free plane, 28 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColPlane {
    pub kind: u8,
    /// Bit 0: camera passes through. Bit 1: one sided.
    pub flags: u8,
    /// Index of the plane in the room, so a trace counts it once.
    pub id: u8,
    pub reserved: u8,
    /// Footprint corners (platform); segment a, b in the first two (wall);
    /// min, max in the first two (blocker).
    pub xz: [[i16; 2]; 4],
    /// Height at `xz[0]` (platform), bottom (wall, blocker).
    pub y0: i16,
    /// Slope dy/dx, Q12 (platform).
    pub sx_q12: i16,
    /// Slope dy/dz, Q12 (platform).
    pub sz_q12: i16,
    /// Thickness below the surface (platform), height (wall, blocker).
    pub extent: i16,
}

/// Liquid kinds, ordered like the guest's `CONTENTS_*` constants.
pub mod liquid_kind {
    /// Water.
    pub const WATER: u8 = 1;
    /// Slime.
    pub const SLIME: u8 = 2;
    /// Lava.
    pub const LAVA: u8 = 3;
}

/// A liquid volume over an inclusive cell rectangle, 12 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Liquid {
    pub x0: u8,
    pub z0: u8,
    pub x1: u8,
    pub z1: u8,
    pub kind: u8,
    pub flags: u8,
    pub surface_y: i16,
    pub bottom_y: i16,
    pub reserved: u16,
}

/// Mover kinds.
pub mod mover_kind {
    /// Sliding door.
    pub const DOOR: u8 = 1;
    /// Lift.
    pub const LIFT: u8 = 2;
    /// Destructible.
    pub const DESTRUCTIBLE: u8 = 3;
}

/// A rigid translating mover, 32 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mover {
    pub kind: u8,
    /// Bit 0 start open, bit 1 enabled.
    pub flags: u8,
    pub id: u8,
    pub pad: u8,
    pub vert_first: u16,
    pub vert_count: u16,
    pub wall_first: u16,
    pub wall_count: u16,
    pub tail_cell: u16,
    pub tail_count: u8,
    pub target_open: u8,
    /// Open travel, runtime units.
    pub offset: [i16; 3],
    pub travel_ticks: u16,
    /// Mutable state, zero at load.
    pub progress_ticks: u16,
    pub reserved: [u8; 6],
}

/// A lattice vertex, 8 bytes. The pad keeps the GTE `VZ` word clean.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Vtx {
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub pad: i16,
}

/// Quad flag: three corners, `v[3] == v[2]`.
pub const QUAD_TRI: u8 = 1;
/// Quad flag: plane is axis aligned; `d` is the signed plane coordinate.
pub const QUAD_AXIS: u8 = 1 << 1;
/// Shift of the two axis-id bits (0 x, 1 y, 2 z).
pub const QUAD_AXIS_SHIFT: u32 = 2;
/// Quad flag: negative-facing axis plane.
pub const QUAD_AXIS_NEG: u8 = 1 << 4;
/// Quad flag: no back-face cull.
pub const QUAD_DOUBLE: u8 = 1 << 5;
/// Quad flag: free plane.
pub const QUAD_PLANE: u8 = 1 << 6;
/// Quad flag: a merged LOD quad stands in for this quad at far range.
pub const QUAD_LOD_COVERED: u8 = 1 << 7;

/// One drawn quad or triangle, 40 bytes.
///
/// Front-facing when `(n . camera) >> 7 > d` with `n` the Q7 normal and
/// `camera` room-local runtime units. Axis-aligned quads store the signed
/// plane coordinate in `d` and skip the dot product.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Quad {
    /// Room-local vertex indices, ring order.
    pub v: [u16; 4],
    /// Baked corner colours `r | g << 8 | b << 16`, ready to OR into a GP0
    /// command word.
    pub color: [u32; 4],
    /// Material-relative texels `u | v << 8` per corner.
    pub uv: [u16; 4],
    pub d: i16,
    pub n: [i8; 3],
    /// Level-global material index.
    pub material: u8,
    pub flags: u8,
    pub reserved: u8,
}

/// Render cell flag: open to the sky.
pub const CELL_OPEN_SKY: u8 = 1;
/// Render cell flag: part of a LOD block.
pub const CELL_HAS_LOD: u8 = 1 << 1;

/// One render cell, 8 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RCell {
    /// Index of the cell's first quad.
    pub first: u16,
    pub count: u8,
    pub flags: u8,
    pub min_y: i16,
    pub max_y: i16,
}

/// LOD block flag: some cell of the block is open to the sky.
pub const BLOCK_OPEN_SKY: u8 = 1;

/// A block of cells (`2^lod_shift` square), 8 bytes: its merged far quads,
/// its vertical extent and whether any cell is open to the sky. Every room
/// carries the table; `count` is zero where nothing merged.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LodBlock {
    pub first: u16,
    pub count: u8,
    pub flags: u8,
    pub min_y: i16,
    pub max_y: i16,
}

/// A free plane's render reference, 12 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaneRef {
    pub quad: u16,
    /// First `COL_PLANES` index or `0xFFFF`.
    pub col: u16,
    pub x0: u8,
    pub z0: u8,
    pub x1: u8,
    pub z1: u8,
    pub min_y: i16,
    pub max_y: i16,
}

macro_rules! sizes {
    ($($t:ty = $n:expr),* $(,)?) => {
        $(const _: () = assert!(core::mem::size_of::<$t>() == $n);)*
    };
}

sizes! {
    LevelHeader = 32,
    RoomRecord = 40,
    Portal = 36,
    Material = 8,
    SectionRef = 8,
    RoomHeader = 152,
    Sector = 20,
    SectorExt = 8,
    ColWall = 12,
    SplitB = 16,
    ColPlane = 28,
    Liquid = 12,
    Mover = 32,
    Vtx = 8,
    Quad = 40,
    RCell = 8,
    LodBlock = 8,
    PlaneRef = 12,
}

/// Size of one record of each section, in section order.
pub const SECTION_RECORD_BYTES: [usize; SECTION_COUNT] = [
    core::mem::size_of::<Sector>(),
    core::mem::size_of::<SectorExt>(),
    core::mem::size_of::<ColWall>(),
    core::mem::size_of::<SplitB>(),
    core::mem::size_of::<ColPlane>(),
    core::mem::size_of::<Liquid>(),
    core::mem::size_of::<Mover>(),
    2,
    1,
    core::mem::size_of::<Vtx>(),
    core::mem::size_of::<Quad>(),
    core::mem::size_of::<RCell>(),
    core::mem::size_of::<LodBlock>(),
    core::mem::size_of::<Quad>(),
    core::mem::size_of::<PlaneRef>(),
];

const fn mix(hash: u32, value: usize) -> u32 {
    let mut h = hash;
    let mut i = 0;
    while i < 4 {
        h ^= ((value >> (i * 8)) & 0xff) as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    h
}

const fn layout_hash() -> u32 {
    use core::mem::{offset_of, size_of};
    let mut h = 0x811c_9dc5u32;
    h = mix(h, size_of::<LevelHeader>());
    h = mix(h, size_of::<RoomRecord>());
    h = mix(h, offset_of!(RoomRecord, origin));
    h = mix(h, offset_of!(RoomRecord, min_y));
    h = mix(h, size_of::<Portal>());
    h = mix(h, offset_of!(Portal, normal));
    h = mix(h, size_of::<Material>());
    h = mix(h, size_of::<RoomHeader>());
    h = mix(h, offset_of!(RoomHeader, sections));
    h = mix(h, size_of::<Sector>());
    h = mix(h, offset_of!(Sector, flags));
    h = mix(h, size_of::<SectorExt>());
    h = mix(h, size_of::<ColWall>());
    h = mix(h, offset_of!(ColWall, y));
    h = mix(h, size_of::<SplitB>());
    h = mix(h, size_of::<ColPlane>());
    h = mix(h, offset_of!(ColPlane, y0));
    h = mix(h, size_of::<Liquid>());
    h = mix(h, size_of::<Mover>());
    h = mix(h, offset_of!(Mover, offset));
    h = mix(h, size_of::<Vtx>());
    h = mix(h, size_of::<Quad>());
    h = mix(h, offset_of!(Quad, color));
    h = mix(h, offset_of!(Quad, uv));
    h = mix(h, offset_of!(Quad, d));
    h = mix(h, offset_of!(Quad, flags));
    h = mix(h, size_of::<RCell>());
    h = mix(h, size_of::<LodBlock>());
    h = mix(h, size_of::<PlaneRef>());
    h
}

/// Hash of the record sizes and key field offsets, carried by the level core.
pub const LAYOUT_HASH: u32 = layout_hash();

/// Bytes of a table of plain records.
///
/// Safe in practice for every record in this module: they are `repr(C)`
/// integers with no padding bytes (sizes asserted above are the field sums).
pub fn bytes_of<T: Copy>(records: &[T]) -> &[u8] {
    // SAFETY: the records are plain integers without padding, so every byte
    // is initialised, and the length is exactly the slice's size in bytes.
    unsafe {
        core::slice::from_raw_parts(
            records.as_ptr().cast::<u8>(),
            core::mem::size_of_val(records),
        )
    }
}
