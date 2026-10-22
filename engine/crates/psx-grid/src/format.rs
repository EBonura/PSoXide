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

/// Most rooms a level may hold: room ids are `u16` and `u16::MAX` marks "no
/// room" (a save with no pose, an empty resident slot). The core directory
/// is the only table that grows with the whole world, 40 bytes a room plus
/// 36 per directed portal (`docs/grid/streaming-design.md` section 3.6 puts
/// the point where it needs paging itself at about 2000 rooms).
pub const MAX_ROOMS: usize = u16::MAX as usize - 1;

/// Level core magic, `"PGL1"`.
pub const LEVEL_MAGIC: [u8; 4] = *b"PGL1";
/// Room chunk magic, `"PGRM"`.
pub const ROOM_MAGIC: [u8; 4] = *b"PGRM";
/// Layout version written by this crate. Version 2 gave [`PlaneRef`] a quad
/// count (a plane is a run of quads, tiled one per sector) in place of the
/// collision index nothing read. Version 3 gave [`Material`] its texture
/// shape and [`Quad`] a second flag byte whose `REPEAT` bit makes a primitive
/// repeat its texture through the GPU texture window (up to four tiles per
/// axis). Version 4 is the streaming-ready layout (`docs/grid/streaming-design.md`
/// section 4): the core holds radii in sectors in place of portal hop counts,
/// the renderer scratch maxima, a world lattice index over the room
/// footprints and no whole-level page total; [`Quad::material`] indexes the
/// room's own `MATERIAL_USE` table, which maps it to a level material; a
/// [`Mover`] carries a stable id. Older files are refused: cook and guest
/// ship together.
pub const VERSION: u16 = 4;
/// Oldest reader that still understands what this crate writes.
pub const MIN_READER: u16 = 4;
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
/// Level flag: the core carries per-room sector occupancy after the lattice,
/// because room rectangles overlap in plan (rooms shaped as sector sets, such
/// as hexagonal chunks). Without it a room owns its whole rectangle.
pub const LEVEL_HAS_OCCUPANCY: u32 = 1 << 4;
/// Level flag: rooms stream and draw as a ring, the observer's room and every
/// room one portal away (a level cut into even chunks, such as hexagons).
/// The draw distance and lead radius do not apply; activation still walks.
pub const LEVEL_RING_STREAM: u32 = 1 << 5;
/// Level flag: the core ends with a [`FloodHeader`] and one weight byte per
/// portal (after the occupancy table, each part on a word boundary). The
/// streamer then keeps resident the rooms a budgeted portal flood from the
/// observer reaches ([`crate::flood`]) in place of the ring or walk radii,
/// draws the ones within `near_radius` and keeps the ones within
/// `res_radius` and `pool_cap_pages` in memory. Levels without the flag are
/// byte for byte what they were.
pub const LEVEL_BUDGET_FLOOD: u32 = 1 << 6;

/// Fixed part of the flood block ([`LEVEL_BUDGET_FLOOD`]), 8 bytes.
///
/// The radii are whole sectors, like the other radii of the core.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct FloodHeader {
    /// Most pages the flood may hold resident; 0 means the whole level is
    /// resident and no flood ever runs (a level that fits its pool).
    pub pool_cap_pages: u16,
    /// Rooms whose flood cost is within this many sectors are drawn and are
    /// needed in memory.
    pub near_radius: u8,
    /// Rooms within this many sectors are kept in memory ahead of need
    /// (at least `near_radius`).
    pub res_radius: u8,
    /// View-cone bias of the flood: 0 is off, the only value this reader
    /// understands.
    pub bias: u8,
    pub reserved: [u8; 3],
}
const _: () = assert!(core::mem::size_of::<FloodHeader>() == 8);

/// Flood weight of a portal that costs its plain length: weights are Q4, so
/// 16 is 1.0.
pub const FLOOD_WEIGHT_ONE: u8 = 16;

/// Level flag: the core ends with a skin block (after the flood block, each
/// part on a word boundary): a [`SkinHeader`], one `u16` first-leaf index per
/// room and level of detail and one more, and the [`SkinLeaf`] records. The skin is the whole
/// level as untextured, flat coloured top surfaces (a quadtree of squares
/// over the owned sectors), drawn for the rooms that are not drawn whole.
pub const LEVEL_HAS_SKIN: u32 = 1 << 7;

/// Fixed part of the skin block ([`LEVEL_HAS_SKIN`]), 12 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SkinHeader {
    pub leaf_count: u16,
    /// Runtime-unit height of height step 0.
    pub base_y: i16,
    /// World sector `[x, z]` of grid cell `(0, 0)`.
    pub origin: [i16; 2],
    /// Runtime units per height step.
    pub height_step: u8,
    /// Levels of detail each room's leaves come in (1 to 3): the finest
    /// first. A room's lists are `cell_first[room * lod_count + lod]` up to
    /// the next entry.
    pub lod_count: u8,
    pub reserved: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<SkinHeader>() == 12);

/// Largest leaf, as the log2 of its edge in sectors (8 by 8).
pub const SKIN_MAX_SIZE_LOG2: u8 = 3;
/// Height steps a [`SkinLeaf`] can hold (9 bits).
pub const SKIN_HEIGHT_STEPS: u16 = 512;

/// Height steps a skirt nibble stands for: the drop from a leaf's top to the
/// lowest neighbour across the edge, rounded up to the next entry.
pub const SKIRT_STEPS: [u16; 16] = [0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192];

/// One skin leaf, 8 bytes: a square of `1 << size_log2` sectors with one top
/// height and one colour.
///
/// `place`: bits 0 to 7 `x`, 8 to 15 `z` (sectors from the header's origin),
/// 16 to 17 `size_log2`, 18 to 26 height in steps above `base_y`.
/// `look`: bits 0 to 14 colour RGB555, 15 to 30 four skirt nibbles (edges
/// towards -Z, +X, +Z, -X, each an index into [`SKIRT_STEPS`]).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SkinLeaf {
    pub place: u32,
    pub look: u32,
}
const _: () = assert!(core::mem::size_of::<SkinLeaf>() == 8);

impl SkinLeaf {
    /// Pack a leaf. `height` is below [`SKIN_HEIGHT_STEPS`], `size_log2` at
    /// most [`SKIN_MAX_SIZE_LOG2`], `rgb555` below 32768 and every skirt
    /// nibble below 16; larger values are masked.
    pub const fn pack(
        x: u8,
        z: u8,
        size_log2: u8,
        height: u16,
        rgb555: u16,
        skirts: [u8; 4],
    ) -> Self {
        Self {
            place: x as u32
                | (z as u32) << 8
                | ((size_log2 & 3) as u32) << 16
                | ((height & 0x1ff) as u32) << 18,
            look: (rgb555 & 0x7fff) as u32
                | ((skirts[0] & 15) as u32) << 15
                | ((skirts[1] & 15) as u32) << 19
                | ((skirts[2] & 15) as u32) << 23
                | ((skirts[3] & 15) as u32) << 27,
        }
    }

    /// Sector column from the header's origin.
    #[inline]
    pub const fn x(&self) -> u8 {
        self.place as u8
    }

    /// Sector row from the header's origin.
    #[inline]
    pub const fn z(&self) -> u8 {
        (self.place >> 8) as u8
    }

    /// Log2 of the edge in sectors.
    #[inline]
    pub const fn size_log2(&self) -> u8 {
        ((self.place >> 16) & 3) as u8
    }

    /// Height in steps above the header's `base_y`.
    #[inline]
    pub const fn height(&self) -> u16 {
        ((self.place >> 18) & 0x1ff) as u16
    }

    /// Colour, RGB555 (bits 0 to 4 red).
    #[inline]
    pub const fn rgb555(&self) -> u16 {
        (self.look & 0x7fff) as u16
    }

    /// Skirt nibble of edge `side` (0 -Z, 1 +X, 2 +Z, 3 -X).
    #[inline]
    pub const fn skirt(&self, side: u8) -> u8 {
        ((self.look >> (15 + 4 * (side as u32 & 3))) & 15) as u8
    }
}

/// Room flag: sky may show (an open sector exists).
pub const ROOM_OUTDOOR: u16 = 1;
/// Room flag: distance fog.
pub const ROOM_FOG: u16 = 1 << 1;
/// Room flag: always resident.
pub const ROOM_PINNED: u16 = 1 << 2;
/// Room flag: holds a liquid volume.
pub const ROOM_HAS_LIQUID: u16 = 1 << 3;
/// Room flag: the collision block is a leaf tree ([`CHUNK_TREE`]), so the
/// collision world resolves a sector through its index. Set by the cook in
/// step with the chunk flag; it lets the hot paths choose without loading the
/// chunk header.
pub const ROOM_TREE: u16 = 1 << 4;

/// Room flag: some column holds extra spans ([`SpanExt`]). `tree_ext` then
/// locates a [`TreeExt`] (dense rooms have empty tree tables) followed by the
/// `SpanExt`, 4 bytes after it. Rooms without extra spans never set it, so
/// their bytes are unchanged.
pub const ROOM_SPANS: u16 = 1 << 5;

/// Room flag: the render block opens with a [`ProbeExt`], the room's light
/// probes (one per sector, a few more in a column of several floors). Rooms
/// cooked before probes existed, and rooms with nothing to light, never set
/// it, so their bytes are unchanged.
pub const ROOM_PROBES: u16 = 1 << 6;

/// Core file header, 52 bytes.
///
/// Every field is sized to the range it can hold (fewest-bits rule, see
/// `docs/grid/compact-format.md` section 2): sector size and the radii are
/// bytes, the scratch maxima are the `u16` ranges of the chunk tables they
/// bound.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LevelHeader {
    pub magic: [u8; 4],
    pub version: u16,
    pub min_reader: u16,
    pub flags: u32,
    pub room_count: u16,
    pub portal_count: u16,
    /// Runtime units per sector edge (S), 1 to 255.
    pub sector_units: u8,
    /// Level materials, at most 255 (the `u8` ids of `MATERIAL_USE`).
    pub material_count: u8,
    /// Pages (2 KiB) of the largest room chunk: the biggest pool run one
    /// unit can need. Replaces the whole-level page total of version 3.
    pub max_room_pages: u8,
    /// Draw distance in sectors: rooms whose walk distance from the camera
    /// exceeds it are not drawn and need not be resident. 0 is unbounded.
    pub draw_far: u8,
    /// Entity activation radius in sectors, walked over the portal graph.
    pub active_radius: u8,
    /// Extra sectors beyond `draw_far` a streamer prefetches (evictable).
    pub lead_radius: u8,
    /// log2 of the world lattice cell edge, runtime units.
    pub lattice_shift: u8,
    pub reserved: u8,
    /// World X and Z of lattice cell (0, 0)'s corner.
    pub lattice_origin: [i32; 2],
    /// Lattice cells along X and Z.
    pub lattice_cells: [u16; 2],
    /// Entries in the lattice room list.
    pub lattice_refs: u16,
    /// Renderer scratch maxima over every room, so the scratch is sized
    /// before any room chunk is resident.
    pub max_vertices: u16,
    pub max_cells: u16,
    pub max_quads: u16,
    pub max_lod_blocks: u16,
    pub max_lod_quads: u16,
    pub layout_hash: u32,
}

/// Renderer scratch maxima: the largest room's table lengths, written into the
/// core so the scratch can be sized before any room chunk is resident.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderMaxima {
    pub vertices: u16,
    pub cells: u16,
    pub quads: u16,
    pub lod_blocks: u16,
    pub lod_quads: u16,
}

impl RenderMaxima {
    /// Fold one room's table lengths in.
    pub fn include(
        &mut self,
        vertices: usize,
        cells: usize,
        quads: usize,
        blocks: usize,
        lod: usize,
    ) {
        let cap = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
        self.vertices = self.vertices.max(cap(vertices));
        self.cells = self.cells.max(cap(cells));
        self.quads = self.quads.max(cap(quads));
        self.lod_blocks = self.lod_blocks.max(cap(blocks));
        self.lod_quads = self.lod_quads.max(cap(lod));
    }
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

/// A level material, 8 bytes. A room reaches it through its `MATERIAL_USE`
/// table.
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
    /// Texture size, see [`tex_shape`]. 0 is unknown (an untextured stand-in):
    /// such a material never carries `REPEAT` quads.
    pub tex_shape: u16,
}

/// Pack a texture size into [`Material::tex_shape`]: bits 0 to 3 hold
/// `log2(width)` and bits 4 to 7 `log2(height)`. Only powers of two from 8
/// to 128 texels (the limits of a GP0(E2) texture window) pack; anything
/// else gives 0, "unknown".
pub const fn tex_shape(width: u16, height: u16) -> u16 {
    if !window_size_ok(width) || !window_size_ok(height) {
        return 0;
    }
    (width.trailing_zeros() as u16) | ((height.trailing_zeros() as u16) << 4)
}

const fn window_size_ok(size: u16) -> bool {
    size >= 8 && size <= 128 && size.is_power_of_two()
}

/// Width and height in texels a [`tex_shape`] stands for, or `None` for 0
/// and for a shape outside the window range.
pub const fn tex_size(shape: u16) -> Option<[u16; 2]> {
    let (lw, lh) = ((shape & 15) as u32, ((shape >> 4) & 15) as u32);
    if shape >> 8 != 0 || lw < 3 || lw > 7 || lh < 3 || lh > 7 {
        return None;
    }
    Some([1 << lw, 1 << lh])
}

/// Room chunk flag: carries distance-LOD quads.
pub const CHUNK_HAS_LOD: u16 = 1;
/// Room chunk flag: carries free planes.
pub const CHUNK_HAS_PLANES: u16 = 1 << 1;
/// Room chunk flag: carries liquids.
pub const CHUNK_HAS_LIQUIDS: u16 = 1 << 2;
/// Room chunk flag: carries movers.
pub const CHUNK_HAS_MOVERS: u16 = 1 << 3;
/// Room chunk flag: the collision block is a **leaf tree**. `SECTORS` holds
/// one record per leaf (a leaf is a cell of 4, 2, 1, 1/2 or 1/4 sector
/// edge) in place of one per sector, and a [`TreeExt`] at
/// [`RoomHeader::tree_ext`] places the per-sector `SECTOR_INDEX` and the
/// `SUBBLOCKS` table. A room whose leaves are all whole sectors never sets
/// it: it keeps the dense layout byte for byte.
pub const CHUNK_TREE: u16 = 1 << 4;

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
    /// Level material ids the room draws; [`Quad::material`] indexes this
    /// table.
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
    /// With [`CHUNK_TREE`]: the room's world origin modulo four sectors,
    /// `x | z << 2` (four bits), so a leaf of 2 or 4 sectors, which is
    /// aligned in the world, finds its own origin from a room-local sector
    /// (see [`leaf_origin_sector`]). Zero otherwise.
    pub origin_mod: u8,
    pub reserved: u8,
    pub sections: [SectionRef; SECTION_COUNT],
    /// With [`CHUNK_TREE`]: chunk-relative byte offset of the [`TreeExt`].
    /// Zero otherwise.
    pub tree_ext: u32,
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

/// Shift of the leaf size code in [`Sector::flags`] (bits 13 to 15). Only a
/// [`CHUNK_TREE`] room writes it; in a dense room the bits are zero, which is
/// the code of a whole sector.
pub const SECTOR_LEAF_SHIFT: u32 = 13;

/// Leaf size codes, the value of [`Sector::flags`] `>> 13`. Code 0 is a whole
/// sector so a dense record reads as one without any change.
pub mod leaf_code {
    /// One sector (document level 2).
    pub const SECTOR: u16 = 0;
    /// Two sectors along an edge (level 1).
    pub const X2: u16 = 1;
    /// Four sectors along an edge (level 0).
    pub const X4: u16 = 2;
    /// Half a sector (level 3).
    pub const HALF: u16 = 3;
    /// A quarter of a sector (level 4).
    pub const QUARTER: u16 = 4;
}

/// Edge of a leaf of size code `code` in a level whose sector edge is `s`,
/// same unit as `s`. A code above [`leaf_code::QUARTER`] reads as a sector.
pub const fn leaf_edge(s: i32, code: u16) -> i32 {
    match code {
        leaf_code::X2 => s * 2,
        leaf_code::X4 => s * 4,
        leaf_code::HALF => s / 2,
        leaf_code::QUARTER => s / 4,
        _ => s,
    }
}

/// Sector (room-local) holding the minimum corner of the leaf of size code
/// `code` (X2 or X4) that covers sector `c` along one axis, given the room's
/// `origin_mod` for that axis. Leaves are aligned in the world, not in the
/// room: this is one add, one and, one subtract.
pub const fn leaf_origin_sector(c: i32, code: u16, origin_mod: i32) -> i32 {
    let n = 1 << code;
    ((c + origin_mod) & !(n - 1)) - origin_mod
}

/// [`SECTOR_INDEX`](TreeExt::sector_index) entry bit: the sector is divided
/// into sub-sector leaves and the id names a 16 entry block of `SUBBLOCKS`.
pub const INDEX_SUB: u16 = 1 << 15;
/// Entry bit reserved for a per-sector clear-neighbourhood summary (the dense
/// layout's [`SECTOR_CLEAR`], which a tree room does not compute yet; the
/// cook writes zero).
pub const INDEX_CLEAR: u16 = 1 << 14;
/// Mask of the leaf or block id of an index entry (14 bits).
pub const INDEX_ID: u16 = 0x3FFF;
/// Id meaning "no leaf here" (an empty sector, or an empty quarter of a
/// subdivided one). Real ids are `0..NO_LEAF`.
pub const NO_LEAF: u16 = 0x3FFF;
/// Entries of one `SUBBLOCKS` block: the 4 by 4 quarter sectors of a sector,
/// row major (`z * 4 + x`).
pub const SUBBLOCK_ENTRIES: usize = 16;
/// Most leaves a room may have (14 bit ids, one reserved).
pub const MAX_LEAVES: usize = NO_LEAF as usize;
/// Most sub-block tables a room may have: one per subdivided sector, and the
/// index holds the id in the same 14 bits.
pub const MAX_SUBBLOCKS: usize = NO_LEAF as usize;

/// The leaf tree's own tables, located by [`RoomHeader::tree_ext`], 20 bytes.
///
/// `layout` is [`TREE_LAYOUT_HASH`], checked by the reader, so the tree part
/// carries its own version without changing the 15 entry section table every
/// dense room shares. A section whose count is zero has no data.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeExt {
    pub layout: u32,
    /// `u16` per sector, `cz * width + cx`: [`INDEX_SUB`], [`INDEX_CLEAR`]
    /// and a 14 bit leaf or block id, [`NO_LEAF`] for none. Count `width *
    /// depth`.
    pub sector_index: SectionRef,
    /// 16 `u16` per block ([`SUBBLOCK_ENTRIES`]): the leaf covering each
    /// quarter sector, [`NO_LEAF`] for none. A half-sector leaf appears in
    /// its four entries. Count is the number of blocks.
    pub subblocks: SectionRef,
}

/// Marker for "no extension".
pub const NO_EXT: u16 = 0xFFFF;

/// Marker for "no span run" in [`SpanExt::index`].
pub const NO_SPAN: u16 = 0xFFFF;

/// The extra spans of a room, located right after its [`TreeExt`] (see
/// [`ROOM_SPANS`]), 28 bytes.
///
/// A column of several spans keeps its ground span in the room's
/// `SECTORS` record (or leaf record) as always. Its spans above the ground
/// one are `Sector` records in [`Self::spans`], bottom to top, which the
/// collision reads as more records of the same cell or leaf: each offers its
/// floor and its ceiling slabs. The ground record of such a column never has
/// [`SECTOR_CLEAR`] or the flat-slab flags, so the fast paths give way to
/// the per-span walk. The reader does not rely on that: it reads the span
/// index, and a room that has any extra span ignores [`SECTOR_CLEAR`]
/// everywhere (a neighbour's extra span would break its promise).
///
/// An extra span has no [`Sector::ext`] (no walls or planes of its own: they
/// belong to the ground record), and in a tree room carries the size code of
/// its leaf in [`Sector::flags`] like the ground record, since its heights
/// are interpolated over the leaf's edge.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanExt {
    /// [`SPAN_LAYOUT_HASH`], checked by the reader.
    pub layout: u32,
    /// `u16` per `SECTORS` record (cell order for a dense room, leaf id for a
    /// tree): an index into [`Self::runs`], or [`NO_SPAN`].
    pub index: SectionRef,
    /// [`SpanRun`] per record that has extra spans.
    pub runs: SectionRef,
    /// [`Sector`] per extra span.
    pub spans: SectionRef,
}

/// The extra spans of one column: `count` records of [`SpanExt::spans`] from
/// `first`, bottom to top. 4 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanRun {
    pub first: u16,
    pub count: u16,
}

/// One light probe, 4 bytes: the light a character standing at `y` in this
/// cell is lit by, baked by the same rig that colours the cell's quads.
///
/// `rgb` packs the colour as RGB555 (bits 0 to 4 red, 5 to 9 green, 10 to 14
/// blue), each channel the light value divided by eight, so the neutral value
/// 128 packs exactly; see [`probe_pack`]. In a [`ProbeExt::cells`] entry bit
/// 15 ([`PROBE_MULTI`]) turns the entry into a pointer instead.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// Height of the probe, room-local runtime units: the floor plus the
    /// height of a character's chest. In a pointer entry, the index of the
    /// first probe in [`ProbeExt::multi`].
    pub y: i16,
    /// RGB555 light colour. In a pointer entry, bit 15 set and the number of
    /// probes in the low bits; zero probes means the cell holds none (a pit).
    pub rgb: u16,
}

/// [`Probe::rgb`] bit 15 in a [`ProbeExt::cells`] entry: the cell is a column
/// of several floors, or empty. `y` is then the first of `rgb & 0x7FFF`
/// consecutive entries of [`ProbeExt::multi`], bottom to top (none for a cell
/// with no floor).
pub const PROBE_MULTI: u16 = 1 << 15;

/// Most probes of one multi-floor column.
pub const PROBE_MULTI_MAX: usize = 0x7FFF;

/// The light probes of a room, at the start of its render block (see
/// [`ROOM_PROBES`]), 20 bytes. The tables follow in the chunk, located by the
/// two section references.
///
/// One probe per sector: at the middle of the sector, a chest height above
/// the floor. A column of several floors (a storey stack, see [`SpanExt`]) has
/// one per floor. A sector with no floor has none.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeExt {
    /// [`PROBE_LAYOUT_HASH`], checked by the reader.
    pub layout: u32,
    /// [`Probe`] per sector, `cz * width + cx`, as the entry kinds above.
    pub cells: SectionRef,
    /// [`Probe`] per floor of the columns of several floors.
    pub multi: SectionRef,
}

/// Pack a light colour (128 neutral, 255 twice neutral) to RGB555: each
/// channel to the nearest multiple of eight, so the neutral 128 stays exact
/// and anything from 252 up packs as 248.
pub const fn probe_pack(rgb: [u8; 3]) -> u16 {
    const fn c(v: u8) -> u16 {
        let q = (v as u16 + 4) >> 3;
        if q > 31 {
            31
        } else {
            q
        }
    }
    c(rgb[0]) | (c(rgb[1]) << 5) | (c(rgb[2]) << 10)
}

/// The light colour of a packed probe colour (bit 15 is ignored).
pub const fn probe_unpack(packed: u16) -> [u8; 3] {
    [
        ((packed & 31) << 3) as u8,
        (((packed >> 5) & 31) << 3) as u8,
        (((packed >> 10) & 31) << 3) as u8,
    ]
}

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

/// Wall flag: blocks only bodies and rays on the front side of the edge (the
/// side `wall_code` calls positive for north, west and the NW-SE diagonal,
/// negative for south, east and NE-SW: the sector interior for a cardinal
/// edge). The cliff face of a tall derived riser, so a ledge can be walked
/// off and a body below cannot walk into the rock.
pub const WALL_FRONT_ONLY: u8 = 1;
/// Wall flag: owned by a mover.
pub const WALL_MOVER: u8 = 2;
/// Wall flag: blocks only bodies and rays on the back side of the edge. Only
/// the diagonals use it, for a derived riser seen from the back.
pub const WALL_BACK_ONLY: u8 = 4;
/// Wall flag: the wall covers only the first half of the owning leaf's edge
/// (the half at the start of the edge, `a` to the middle, in the edge's
/// walking direction), with its four heights given at that half's ends. A
/// derived wall hosted by a leaf next to two smaller ones, or beside a gap.
/// Cardinal edges only. Neither half flag: the whole edge.
pub const WALL_HALF_FIRST: u8 = 8;
/// Wall flag: the second half of the edge, see [`WALL_HALF_FIRST`].
pub const WALL_HALF_SECOND: u8 = 16;

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

/// [`ColPlane::flags`] bit: the first copy of its plane in the table. Every
/// plane is copied into each sector it touches; the per-sector scans want
/// all copies, point rays want each plane once and skip the rest.
pub const COLPLANE_PRIMARY: u8 = 1;

/// [`ColPlane::flags`] bit: the camera arm passes through this plane
/// (a translucent or alpha-cut banner, grate or foliage). Bodies and every
/// other trace still collide with it per its kind; only a provider made with
/// `GridTraceProvider::for_camera` ignores it.
pub const COLPLANE_CAMERA_PASS: u8 = 2;

/// A collidable free plane, 28 bytes.
///
/// Per kind, all coordinates room-local runtime units:
///
/// * [`plane_kind::PLATFORM`]: `xz` holds the four footprint corners in ring
///   order, `y0` the surface height at `xz[0]`, `sx_q12` and `sz_q12` the
///   surface slope (dy/dx and dy/dz) and `extent` the slab thickness below
///   the surface. The surface is `y0 + (sx * dx + sz * dz) >> 12` for the
///   offset `(dx, dz)` from `xz[0]`.
/// * [`plane_kind::WALL`]: `xz[0]` and `xz[1]` are the segment ends, `y0` the
///   bottom, `extent` the height and `sx_q12`, `sz_q12` the unit direction
///   from end 0 to end 1 in Q12. Zero thickness, blocks from both sides.
/// * [`plane_kind::BLOCKER`]: `xz[0]` is the minimum and `xz[1]` the maximum
///   corner of an axis-aligned box from `y0` up by `extent`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColPlane {
    pub kind: u8,
    /// [`COLPLANE_PRIMARY`] and [`COLPLANE_CAMERA_PASS`], other bits reserved
    /// zero.
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
    /// Stable id as two halves, low then high ([`Mover::stable_id`]).
    pub stable: [u16; 2],
    pub reserved: u16,
}

impl Mover {
    /// The mover's stable id: an authored identity that survives recooks and
    /// does not depend on the room index or the order of the tables. State
    /// that must outlive a chunk (an open door, a broken destructible) is
    /// keyed by it. `id` stays the index inside the room.
    pub const fn stable_id(&self) -> MoverId {
        MoverId(self.stable[0] as u32 | (self.stable[1] as u32) << 16)
    }

    /// Set the stable id.
    pub fn set_stable_id(&mut self, id: MoverId) {
        self.stable = [id.0 as u16, (id.0 >> 16) as u16];
    }
}

/// Stable global id of a mover: 32 bits, the FNV-1a hash of its authored
/// name ([`MoverId::from_name`]). Not `room << 8 | local`: that packing
/// breaks at 256 rooms and renumbers when rooms are cut or reordered.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MoverId(pub u32);

impl MoverId {
    /// Hash an authored name.
    pub const fn from_name(name: &str) -> Self {
        let bytes = name.as_bytes();
        let mut h = 0x811c_9dc5u32;
        let mut i = 0;
        while i < bytes.len() {
            h ^= bytes[i] as u32;
            h = h.wrapping_mul(0x0100_0193);
            i += 1;
        }
        Self(h)
    }
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

/// [`Quad::flags2`] bit: the primitive repeats its texture through the
/// material's GP0(E2) window. Its UVs are raw texture coordinates (the
/// material's page-local origin is not added), up to four tiles per axis.
pub const QUAD2_REPEAT: u8 = 1;
/// [`Quad::flags2`] bit: listed in several cells (reserved for the cook
/// merge; no writer sets it yet).
pub const QUAD2_MULTI: u8 = 1 << 1;
/// Every [`Quad::flags2`] bit this reader knows.
pub const QUAD2_KNOWN: u8 = QUAD2_REPEAT | QUAD2_MULTI;

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
    /// Index into the room's `MATERIAL_USE` table, which names the level
    /// material.
    pub material: u8,
    pub flags: u8,
    /// [`QUAD2_REPEAT`] and [`QUAD2_MULTI`]; other bits are zero.
    pub flags2: u8,
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

/// A free plane's render reference, 12 bytes: the run of quads it draws and
/// a box around them for the per-frame frustum test.
///
/// The box is the cell rectangle `x0..=x1`, `z0..=z1` (room-local cells)
/// between `min_y` and `max_y` (room-local runtime units).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaneRef {
    /// First quad of the run in `QUADS`.
    pub quad: u16,
    /// Quads in the run.
    pub count: u8,
    pub flags: u8,
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
    LevelHeader = 52,
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
    TreeExt = 20,
    Probe = 4,
    ProbeExt = 20,
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
    h = mix(h, offset_of!(LevelHeader, lattice_origin));
    h = mix(h, offset_of!(LevelHeader, max_vertices));
    h = mix(h, size_of::<RoomRecord>());
    h = mix(h, offset_of!(RoomRecord, origin));
    h = mix(h, offset_of!(RoomRecord, min_y));
    h = mix(h, size_of::<Portal>());
    h = mix(h, offset_of!(Portal, normal));
    h = mix(h, size_of::<Material>());
    h = mix(h, offset_of!(Material, tex_shape));
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
    h = mix(h, offset_of!(Mover, stable));
    h = mix(h, size_of::<Vtx>());
    h = mix(h, size_of::<Quad>());
    h = mix(h, offset_of!(Quad, color));
    h = mix(h, offset_of!(Quad, uv));
    h = mix(h, offset_of!(Quad, d));
    h = mix(h, offset_of!(Quad, flags));
    h = mix(h, offset_of!(Quad, flags2));
    h = mix(h, size_of::<RCell>());
    h = mix(h, size_of::<LodBlock>());
    h = mix(h, size_of::<PlaneRef>());
    h = mix(h, offset_of!(PlaneRef, count));
    h
}

/// Hash of the record sizes and key field offsets, carried by the level core.
pub const LAYOUT_HASH: u32 = layout_hash();

const fn tree_layout_hash() -> u32 {
    use core::mem::{offset_of, size_of};
    let mut h = 0x811c_9dc5u32;
    h = mix(h, size_of::<TreeExt>());
    h = mix(h, offset_of!(TreeExt, sector_index));
    h = mix(h, offset_of!(TreeExt, subblocks));
    h = mix(h, SECTOR_LEAF_SHIFT as usize);
    h = mix(h, leaf_code::QUARTER as usize);
    h = mix(h, INDEX_SUB as usize);
    h = mix(h, NO_LEAF as usize);
    h = mix(h, SUBBLOCK_ENTRIES);
    h = mix(h, WALL_HALF_FIRST as usize);
    h = mix(h, WALL_HALF_SECOND as usize);
    h
}

/// Hash of the leaf tree's layout, carried by [`TreeExt`] (the core's
/// [`LAYOUT_HASH`] stays what it was, so dense levels are byte for byte
/// unchanged).
pub const TREE_LAYOUT_HASH: u32 = tree_layout_hash();

const fn span_layout_hash() -> u32 {
    use core::mem::{offset_of, size_of};
    let mut h = 0x811c_9dc5u32;
    h = mix(h, size_of::<SpanExt>());
    h = mix(h, offset_of!(SpanExt, index));
    h = mix(h, offset_of!(SpanExt, runs));
    h = mix(h, offset_of!(SpanExt, spans));
    h = mix(h, size_of::<SpanRun>());
    h = mix(h, size_of::<Sector>());
    h = mix(h, NO_SPAN as usize);
    h
}

/// Hash of the span tables' layout, carried by [`SpanExt`].
pub const SPAN_LAYOUT_HASH: u32 = span_layout_hash();

const fn probe_layout_hash() -> u32 {
    use core::mem::{offset_of, size_of};
    let mut h = 0x811c_9dc5u32;
    h = mix(h, size_of::<ProbeExt>());
    h = mix(h, offset_of!(ProbeExt, cells));
    h = mix(h, offset_of!(ProbeExt, multi));
    h = mix(h, size_of::<Probe>());
    h = mix(h, offset_of!(Probe, rgb));
    h = mix(h, PROBE_MULTI as usize);
    h = mix(h, PROBE_MULTI_MAX);
    h
}

/// Hash of the light probe tables' layout, carried by [`ProbeExt`].
pub const PROBE_LAYOUT_HASH: u32 = probe_layout_hash();

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
