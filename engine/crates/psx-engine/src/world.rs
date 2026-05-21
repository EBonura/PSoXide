//! Grid-world runtime data.
//!
//! The editor-facing model is free to be pleasant and dynamic. The engine
//! model is intentionally static: rooms are grids of sectors, sectors own
//! optional floor/ceiling faces plus edge walls, and every collection is a
//! borrowed slice suitable for cooked, ROM-backed data.

use crate::WorldVertex;
use psx_level::{
    compact_collision_header, compact_collision_sector_flags, compact_collision_surface,
    compact_collision_triangle_flags, compact_collision_wall_flags, COMPACT_COLLISION_HEADER_BYTES,
    COMPACT_COLLISION_HEIGHT_OVERRIDE_BYTES, COMPACT_COLLISION_MAGIC,
    COMPACT_COLLISION_SECTOR_BYTES, COMPACT_COLLISION_VERSION, COMPACT_COLLISION_WALL_BYTES,
};

/// World units per grid sector.
///
/// Bonnie-32 used TR-style 1024-unit sectors. Keeping that unit at engine
/// level gives the editor, collision, and render cooker a shared scale.
pub const GRID_SECTOR_SIZE: i32 = 1024;

/// Runtime material slot used by cooked world geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldMaterialId(pub u16);

/// Cardinal or diagonal sector edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridDirection {
    /// North edge, -Z.
    North,
    /// East edge, +X.
    East,
    /// South edge, +Z.
    South,
    /// West edge, -X.
    West,
    /// Diagonal from north-west to south-east.
    NorthWestSouthEast,
    /// Diagonal from north-east to south-west.
    NorthEastSouthWest,
}

/// Diagonal split used for a quad face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridSplit {
    /// Split from north-west to south-east.
    #[default]
    NorthWestSouthEast,
    /// Split from north-east to south-west.
    NorthEastSouthWest,
}

impl GridSplit {
    /// First triangle corner indices in `[NW, NE, SE, SW]` order.
    pub const fn triangle_a(self) -> [usize; 3] {
        match self {
            Self::NorthWestSouthEast => [0, 1, 2],
            Self::NorthEastSouthWest => [0, 1, 3],
        }
    }

    /// Second triangle corner indices in `[NW, NE, SE, SW]` order.
    pub const fn triangle_b(self) -> [usize; 3] {
        match self {
            Self::NorthWestSouthEast => [0, 2, 3],
            Self::NorthEastSouthWest => [1, 2, 3],
        }
    }
}

/// Horizontal sector face, used for floors and ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridHorizontalFace {
    /// Corner heights `[NW, NE, SE, SW]` in world units.
    pub heights: [i32; 4],
    /// Diagonal split.
    pub split: GridSplit,
    /// Runtime material slot.
    pub material: WorldMaterialId,
    /// Whether character collision treats this face as walkable.
    pub walkable: bool,
}

impl GridHorizontalFace {
    /// Create a flat face at `height`.
    pub const fn flat(height: i32, material: WorldMaterialId) -> Self {
        Self {
            heights: [height, height, height, height],
            split: GridSplit::NorthWestSouthEast,
            material,
            walkable: true,
        }
    }

    /// Average face height.
    pub const fn average_height(self) -> i32 {
        (self.heights[0] + self.heights[1] + self.heights[2] + self.heights[3]) / 4
    }

    /// True when every corner has the same height.
    pub const fn is_flat(self) -> bool {
        self.heights[0] == self.heights[1]
            && self.heights[0] == self.heights[2]
            && self.heights[0] == self.heights[3]
    }

    /// Heights along one edge, ordered left-to-right as seen from inside.
    pub const fn edge_heights(self, direction: GridDirection) -> (i32, i32) {
        match direction {
            GridDirection::North => (self.heights[0], self.heights[1]),
            GridDirection::East => (self.heights[1], self.heights[2]),
            GridDirection::South => (self.heights[3], self.heights[2]),
            GridDirection::West => (self.heights[0], self.heights[3]),
            GridDirection::NorthWestSouthEast => (self.heights[0], self.heights[2]),
            GridDirection::NorthEastSouthWest => (self.heights[1], self.heights[3]),
        }
    }
}

/// Vertical face on a sector edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridVerticalFace {
    /// Corner heights `[bottom-left, bottom-right, top-right, top-left]`.
    pub heights: [i32; 4],
    /// Runtime material slot.
    pub material: WorldMaterialId,
    /// Whether collision treats this face as blocking.
    pub solid: bool,
}

impl GridVerticalFace {
    /// Create a flat vertical wall.
    pub const fn flat(bottom: i32, top: i32, material: WorldMaterialId) -> Self {
        Self {
            heights: [bottom, bottom, top, top],
            material,
            solid: true,
        }
    }

    /// Average bottom height.
    pub const fn bottom(self) -> i32 {
        (self.heights[0] + self.heights[1]) / 2
    }

    /// Average top height.
    pub const fn top(self) -> i32 {
        (self.heights[2] + self.heights[3]) / 2
    }
}

/// Wall lists for every sector edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridWalls<'a> {
    /// Walls on the north edge.
    pub north: &'a [GridVerticalFace],
    /// Walls on the east edge.
    pub east: &'a [GridVerticalFace],
    /// Walls on the south edge.
    pub south: &'a [GridVerticalFace],
    /// Walls on the west edge.
    pub west: &'a [GridVerticalFace],
    /// Diagonal NW-SE walls.
    pub north_west_south_east: &'a [GridVerticalFace],
    /// Diagonal NE-SW walls.
    pub north_east_south_west: &'a [GridVerticalFace],
}

impl<'a> GridWalls<'a> {
    /// Empty edge wall lists.
    pub const EMPTY: Self = Self {
        north: &[],
        east: &[],
        south: &[],
        west: &[],
        north_west_south_east: &[],
        north_east_south_west: &[],
    };

    /// Walls for one direction.
    pub const fn get(self, direction: GridDirection) -> &'a [GridVerticalFace] {
        match direction {
            GridDirection::North => self.north,
            GridDirection::East => self.east,
            GridDirection::South => self.south,
            GridDirection::West => self.west,
            GridDirection::NorthWestSouthEast => self.north_west_south_east,
            GridDirection::NorthEastSouthWest => self.north_east_south_west,
        }
    }
}

impl Default for GridWalls<'_> {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// One authored sector in a room grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSector<'a> {
    /// Optional floor face.
    pub floor: Option<GridHorizontalFace>,
    /// Optional ceiling face.
    pub ceiling: Option<GridHorizontalFace>,
    /// Walls on sector edges.
    pub walls: GridWalls<'a>,
}

impl<'a> GridSector<'a> {
    /// Empty sector.
    pub const EMPTY: Self = Self {
        floor: None,
        ceiling: None,
        walls: GridWalls::EMPTY,
    };

    /// Sector with a single floor.
    pub const fn with_floor(height: i32, material: WorldMaterialId) -> Self {
        Self {
            floor: Some(GridHorizontalFace::flat(height, material)),
            ceiling: None,
            walls: GridWalls::EMPTY,
        }
    }

    /// True when this sector emits any world geometry.
    pub const fn has_geometry(self) -> bool {
        self.floor.is_some()
            || self.ceiling.is_some()
            || !self.walls.north.is_empty()
            || !self.walls.east.is_empty()
            || !self.walls.south.is_empty()
            || !self.walls.west.is_empty()
            || !self.walls.north_west_south_east.is_empty()
            || !self.walls.north_east_south_west.is_empty()
    }
}

impl Default for GridSector<'_> {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Integer grid coordinate inside a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCoord {
    /// X sector coordinate.
    pub x: u16,
    /// Z sector coordinate.
    pub z: u16,
}

/// Floor sample at a world-space X/Z point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridFloorSample {
    /// Sector containing the point.
    pub coord: GridCoord,
    /// Floor face used for the sample.
    pub face: GridHorizontalFace,
    /// Interpolated floor height in world units.
    pub height: i32,
}

/// One cooked grid room -- **authoring / test helper**, not the
/// resident PSX runtime format.
///
/// The room body holds `&[Option<GridSector<'a>>]` where each
/// `GridSector<'a>` further holds six borrowed slices. Convenient
/// for tests that build a world in static const data and for
/// engine-side code that wants direct access; **but six-pointer
/// pre-decoded sectors are not what we want resident in PSX
/// memory at scale**. The PSX target shape is `psx_asset::World<'a>`
/// -- flat byte tables decoded by-value on demand. Don't grow new
/// runtime systems on top of `GridRoom`; build them on
/// `psx_asset::World`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridRoom<'a> {
    /// World-space room origin.
    pub origin: WorldVertex,
    /// Width in sectors.
    pub width: u16,
    /// Depth in sectors.
    pub depth: u16,
    /// Flat `[x * depth + z]` sector storage. `None` means no sector.
    pub sectors: &'a [Option<GridSector<'a>>],
}

impl<'a> GridRoom<'a> {
    /// Create a room over cooked sector storage.
    pub const fn new(
        origin: WorldVertex,
        width: u16,
        depth: u16,
        sectors: &'a [Option<GridSector<'a>>],
    ) -> Self {
        Self {
            origin,
            width,
            depth,
            sectors,
        }
    }

    /// Flat sector index for a coordinate.
    pub const fn sector_index(self, coord: GridCoord) -> Option<usize> {
        if coord.x < self.width && coord.z < self.depth {
            Some(coord.x as usize * self.depth as usize + coord.z as usize)
        } else {
            None
        }
    }

    /// Sector at a coordinate.
    pub fn sector(self, coord: GridCoord) -> Option<GridSector<'a>> {
        self.sector_index(coord)
            .and_then(|index| self.sectors.get(index).copied().flatten())
    }

    /// Floor under a world-space X/Z point.
    pub fn floor_at(self, x: i32, z: i32) -> Option<GridFloorSample> {
        let coord = self.world_to_grid(x, z)?;
        let sector = self.sector(coord)?;
        let face = sector.floor?;
        let local_x =
            (x - (self.origin.x + coord.x as i32 * GRID_SECTOR_SIZE)).clamp(0, GRID_SECTOR_SIZE);
        let local_z =
            (z - (self.origin.z + coord.z as i32 * GRID_SECTOR_SIZE)).clamp(0, GRID_SECTOR_SIZE);
        Some(GridFloorSample {
            coord,
            face,
            height: face.height_at_local(local_x, local_z),
        })
    }

    /// Walls on a sector edge, or an empty slice for absent sectors.
    pub fn walls(self, coord: GridCoord, direction: GridDirection) -> &'a [GridVerticalFace] {
        self.sector(coord)
            .map(|sector| sector.walls.get(direction))
            .unwrap_or(&[])
    }

    /// Convert a sector coordinate to the world-space north-west corner.
    pub const fn grid_to_world(self, coord: GridCoord) -> WorldVertex {
        WorldVertex::new(
            self.origin.x + coord.x as i32 * GRID_SECTOR_SIZE,
            self.origin.y,
            self.origin.z + coord.z as i32 * GRID_SECTOR_SIZE,
        )
    }

    /// Convert a world X/Z position to a sector coordinate.
    pub const fn world_to_grid(self, x: i32, z: i32) -> Option<GridCoord> {
        let local_x = x - self.origin.x;
        let local_z = z - self.origin.z;
        if local_x < 0 || local_z < 0 {
            return None;
        }
        let grid_x = (local_x / GRID_SECTOR_SIZE) as u16;
        let grid_z = (local_z / GRID_SECTOR_SIZE) as u16;
        if grid_x < self.width && grid_z < self.depth {
            Some(GridCoord {
                x: grid_x,
                z: grid_z,
            })
        } else {
            None
        }
    }
}

impl GridHorizontalFace {
    /// Interpolated height at local sector coordinates.
    ///
    /// `local_x` and `local_z` are clamped to `0..=GRID_SECTOR_SIZE`.
    pub fn height_at_local(self, local_x: i32, local_z: i32) -> i32 {
        let u = local_x.clamp(0, GRID_SECTOR_SIZE);
        let v = local_z.clamp(0, GRID_SECTOR_SIZE);
        let [nw, ne, se, sw] = self.heights;
        match self.split {
            GridSplit::NorthWestSouthEast => {
                if v <= u {
                    nw.saturating_add(mul_sector(height_delta(ne, nw), u - v))
                        .saturating_add(mul_sector(height_delta(se, nw), v))
                } else {
                    nw.saturating_add(mul_sector(height_delta(se, sw), u))
                        .saturating_add(mul_sector(height_delta(sw, nw), v))
                }
            }
            GridSplit::NorthEastSouthWest => {
                if u + v <= GRID_SECTOR_SIZE {
                    nw.saturating_add(mul_sector(height_delta(ne, nw), u))
                        .saturating_add(mul_sector(height_delta(sw, nw), v))
                } else {
                    sw.saturating_add(mul_sector(height_delta(se, sw), u))
                        .saturating_add(mul_sector(height_delta(ne, se), GRID_SECTOR_SIZE - v))
                }
            }
        }
    }
}

fn height_delta(to: i32, from: i32) -> i32 {
    to.saturating_sub(from)
}

fn mul_sector(delta: i32, amount: i32) -> i32 {
    delta.saturating_mul(amount) / GRID_SECTOR_SIZE
}

/// Complete cooked grid-world.
///
/// Same authoring / test caveat as [`GridRoom`]: this is the
/// engine-side helper, not the PSX-resident shape. PSX-resident
/// world data is `psx_asset::World<'a>` (one room) plus a thin
/// runtime wrapper -- see [`RuntimeRoom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridWorld<'a> {
    /// Rooms in the world.
    pub rooms: &'a [GridRoom<'a>],
}

impl<'a> GridWorld<'a> {
    /// Empty grid-world.
    pub const EMPTY: Self = Self { rooms: &[] };

    /// Create a world from rooms.
    pub const fn new(rooms: &'a [GridRoom<'a>]) -> Self {
        Self { rooms }
    }

    /// Room by index.
    pub fn room(self, index: usize) -> Option<GridRoom<'a>> {
        self.rooms.get(index).copied()
    }
}

/// PSX-resident wrapper over a parsed `.psxw` blob.
///
/// Compared with [`GridRoom`], this type holds **only** the
/// zero-copy `psx_asset::World<'a>` view -- no pre-decoded sector
/// array, no `Option<GridSector>` slice, no per-sector borrows.
/// Sectors and walls decode by value on demand:
///
/// ```ignore
/// let blob: &[u8] = include_bytes!("level1.psxw");
/// let room = RuntimeRoom::from_bytes(blob)?;
/// for x in 0..room.width() {
///     for z in 0..room.depth() {
///         if let Some(sector) = room.sector(x, z) {
///             // …decode walls by value:
///             for i in 0..sector.wall_count() {
///                 if let Some(wall) = room.sector_wall(sector, i) {
///                     // …
///                 }
///             }
///         }
///     }
/// }
/// ```
///
/// New runtime systems (collision, rendering, AI floor sampling)
/// should grow on this type rather than `GridRoom` -- `GridRoom`
/// stays for tests and authoring helpers only.
#[derive(Copy, Clone, Debug)]
pub struct RuntimeRoom<'a> {
    inner: psx_asset::World<'a>,
}

impl<'a> RuntimeRoom<'a> {
    /// Parse a cooked `.psxw` blob into a runtime view.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, psx_asset::ParseError> {
        Ok(Self {
            inner: psx_asset::World::from_bytes(bytes)?,
        })
    }

    /// Wrap an already-parsed `World`. Used when the parse and
    /// the engine-side wrap happen in different layers.
    pub const fn from_world(world: psx_asset::World<'a>) -> Self {
        Self { inner: world }
    }

    /// Underlying byte-level view. Borrow it when you need the
    /// raw slice access (validation, debug dump, …).
    pub const fn world(&self) -> &psx_asset::World<'a> {
        &self.inner
    }

    /// Width in grid sectors.
    pub fn width(&self) -> u16 {
        self.inner.width()
    }

    /// Depth in grid sectors.
    pub fn depth(&self) -> u16 {
        self.inner.depth()
    }

    /// Engine units per sector.
    pub fn sector_size(&self) -> i32 {
        self.inner.sector_size()
    }

    /// Number of wall records in the room.
    pub fn wall_count(&self) -> u16 {
        self.inner.wall_count()
    }

    /// Sector by `(x, z)` cell index, or `None` for empty cells
    /// or out-of-range coords.
    pub fn sector(&self, x: u16, z: u16) -> Option<psx_asset::WorldSector> {
        self.inner.sector(x, z)
    }

    /// Wall record by sector-local wall index. Skip the array-
    /// decode dance the caller would otherwise do over
    /// `sector.first_wall + i`.
    pub fn sector_wall(
        &self,
        sector: psx_asset::WorldSector,
        local_index: u16,
    ) -> Option<psx_asset::WorldWall> {
        self.inner.sector_wall(sector, local_index)
    }

    /// Static surface-light record by direct table index.
    pub fn surface_light(&self, index: u16) -> Option<[[u8; 3]; 4]> {
        self.inner
            .surface_light(index)
            .map(|light| light.vertex_rgb())
    }

    /// Render-side facade, see [`RoomRender`].
    pub const fn render(&self) -> RoomRender<'a, '_> {
        RoomRender { room: self }
    }

    /// Collision-side facade, see [`RoomCollision`].
    pub const fn collision(&self) -> RoomCollision<'a, '_> {
        RoomCollision::Runtime(self)
    }
}

/// Parse error for compact collision-only room payloads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompactCollisionParseError {
    /// Payload ended before a required header/table field.
    Truncated,
    /// Payload magic did not match the compact collision format.
    WrongMagic,
    /// Payload version is unknown to this runtime.
    UnsupportedVersion(u32),
    /// Header counts or table ranges are inconsistent.
    InvalidLayout,
}

/// Compact collision-only room view.
///
/// This is the streamed-room collision payload: no materials, no UVs,
/// no lighting. It keeps only the data consumed by character/camera
/// collision.
#[derive(Copy, Clone, Debug)]
pub struct CompactCollisionRoom<'a> {
    sectors: &'a [u8],
    walls: &'a [u8],
    height_overrides: &'a [u8],
    width: u16,
    depth: u16,
    sector_size: i32,
    wall_count: u16,
    height_override_count: u16,
    ambient_rgb: [u8; 3],
}

impl<'a> CompactCollisionRoom<'a> {
    /// Parse a compact collision-only room payload.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, CompactCollisionParseError> {
        if bytes.len() < COMPACT_COLLISION_HEADER_BYTES {
            return Err(CompactCollisionParseError::Truncated);
        }
        if bytes.get(0..8) != Some(COMPACT_COLLISION_MAGIC.as_slice()) {
            return Err(CompactCollisionParseError::WrongMagic);
        }
        let version = read_u32(bytes, compact_collision_header::VERSION)
            .ok_or(CompactCollisionParseError::Truncated)?;
        if version != COMPACT_COLLISION_VERSION {
            return Err(CompactCollisionParseError::UnsupportedVersion(version));
        }
        let width = read_u16(bytes, compact_collision_header::WIDTH)
            .ok_or(CompactCollisionParseError::Truncated)?;
        let depth = read_u16(bytes, compact_collision_header::DEPTH)
            .ok_or(CompactCollisionParseError::Truncated)?;
        let sector_size = read_i32(bytes, compact_collision_header::SECTOR_SIZE)
            .ok_or(CompactCollisionParseError::Truncated)?;
        let sector_count = read_u16(bytes, compact_collision_header::SECTOR_COUNT)
            .ok_or(CompactCollisionParseError::Truncated)?;
        let wall_count = read_u16(bytes, compact_collision_header::WALL_COUNT)
            .ok_or(CompactCollisionParseError::Truncated)?;
        let height_override_count =
            read_u16(bytes, compact_collision_header::HEIGHT_OVERRIDE_COUNT)
                .ok_or(CompactCollisionParseError::Truncated)?;
        let ambient = bytes
            .get(compact_collision_header::AMBIENT_RGB..compact_collision_header::AMBIENT_RGB + 3)
            .ok_or(CompactCollisionParseError::Truncated)?;
        let expected_sectors = (width as usize)
            .checked_mul(depth as usize)
            .ok_or(CompactCollisionParseError::InvalidLayout)?;
        if expected_sectors != sector_count as usize {
            return Err(CompactCollisionParseError::InvalidLayout);
        }

        let mut offset = COMPACT_COLLISION_HEADER_BYTES;
        let sector_bytes = checked_table_len(sector_count, COMPACT_COLLISION_SECTOR_BYTES)?;
        let sectors = take_bytes(bytes, &mut offset, sector_bytes)?;
        let wall_bytes = checked_table_len(wall_count, COMPACT_COLLISION_WALL_BYTES)?;
        let walls = take_bytes(bytes, &mut offset, wall_bytes)?;
        let override_bytes = checked_table_len(
            height_override_count,
            COMPACT_COLLISION_HEIGHT_OVERRIDE_BYTES,
        )?;
        let height_overrides = take_bytes(bytes, &mut offset, override_bytes)?;
        if offset != bytes.len() {
            return Err(CompactCollisionParseError::InvalidLayout);
        }

        let room = Self {
            sectors,
            walls,
            height_overrides,
            width,
            depth,
            sector_size,
            wall_count,
            height_override_count,
            ambient_rgb: [ambient[0], ambient[1], ambient[2]],
        };
        if !room.validate_sector_wall_ranges() {
            return Err(CompactCollisionParseError::InvalidLayout);
        }
        Ok(room)
    }

    /// Collision-side facade.
    pub const fn collision(&self) -> RoomCollision<'a, '_> {
        RoomCollision::Compact(self)
    }

    /// Width in grid sectors.
    pub const fn width(self) -> u16 {
        self.width
    }

    /// Depth in grid sectors.
    pub const fn depth(self) -> u16 {
        self.depth
    }

    /// Engine units per sector.
    pub const fn sector_size(self) -> i32 {
        self.sector_size
    }

    /// Number of wall records in the compact collision payload.
    pub const fn wall_count(self) -> u16 {
        self.wall_count
    }

    /// Ambient room RGB used for actor lighting while this chunk is active.
    pub const fn ambient_color(self) -> [u8; 3] {
        self.ambient_rgb
    }

    fn sector(self, x: u16, z: u16) -> Option<CompactSectorCollision> {
        let sector = self.sector_record(x, z)?;
        sector.has_geometry().then_some(sector)
    }

    fn sector_probe(self, x: u16, z: u16) -> Option<CompactSectorCollisionProbe> {
        let sector = self.sector_record(x, z)?;
        sector
            .has_geometry()
            .then_some(CompactSectorCollisionProbe {
                flags: sector.flags,
                first_wall: sector.first_wall,
                wall_count: sector.wall_count,
            })
    }

    fn sector_wall(
        self,
        sector: CompactSectorCollision,
        local_index: u16,
    ) -> Option<CompactWallCollision> {
        if local_index >= sector.wall_count {
            return None;
        }
        self.wall(sector.first_wall.checked_add(local_index)?)
    }

    fn sector_probe_wall(
        self,
        sector: CompactSectorCollisionProbe,
        local_index: u16,
    ) -> Option<CompactWallCollision> {
        if local_index >= sector.wall_count {
            return None;
        }
        self.wall(sector.first_wall.checked_add(local_index)?)
    }

    fn wall(self, index: u16) -> Option<CompactWallCollision> {
        if index >= self.wall_count {
            return None;
        }
        let base = (index as usize).checked_mul(COMPACT_COLLISION_WALL_BYTES)?;
        let bytes = self
            .walls
            .get(base..base.checked_add(COMPACT_COLLISION_WALL_BYTES)?)?;
        Some(CompactWallCollision {
            direction: *bytes.first()?,
            flags: *bytes.get(1)?,
            shape: read_u16(bytes, 2)?,
            heights: read_i32x4(bytes, 4)?,
        })
    }

    fn sector_record(self, x: u16, z: u16) -> Option<CompactSectorCollision> {
        if x >= self.width || z >= self.depth {
            return None;
        }
        let index = (x as usize)
            .checked_mul(self.depth as usize)?
            .checked_add(z as usize)?;
        let base = index.checked_mul(COMPACT_COLLISION_SECTOR_BYTES)?;
        let bytes = self
            .sectors
            .get(base..base.checked_add(COMPACT_COLLISION_SECTOR_BYTES)?)?;
        let sector_index = u16::try_from(index).ok()?;
        let floor_heights = read_i32x4(bytes, 12)?;
        let ceiling_heights = read_i32x4(bytes, 28)?;
        Some(CompactSectorCollision {
            flags: *bytes.first()?,
            floor_split: *bytes.get(1)?,
            ceiling_split: *bytes.get(2)?,
            floor_triangle_flags: *bytes.get(3)?,
            ceiling_triangle_flags: *bytes.get(4)?,
            first_wall: read_u16(bytes, 6)?,
            wall_count: read_u16(bytes, 8)?,
            floor_heights,
            ceiling_heights,
            floor_triangle_heights: self
                .height_override(sector_index, compact_collision_surface::FLOOR)
                .unwrap_or_else(|| {
                    [
                        horizontal_triangle_heights(floor_heights, *bytes.get(1).unwrap_or(&0), 0),
                        horizontal_triangle_heights(floor_heights, *bytes.get(1).unwrap_or(&0), 1),
                    ]
                }),
            ceiling_triangle_heights: self
                .height_override(sector_index, compact_collision_surface::CEILING)
                .unwrap_or_else(|| {
                    [
                        horizontal_triangle_heights(
                            ceiling_heights,
                            *bytes.get(2).unwrap_or(&0),
                            0,
                        ),
                        horizontal_triangle_heights(
                            ceiling_heights,
                            *bytes.get(2).unwrap_or(&0),
                            1,
                        ),
                    ]
                }),
        })
    }

    fn height_override(self, sector_index: u16, surface: u8) -> Option<[[i32; 3]; 2]> {
        let mut index = 0usize;
        while index < self.height_override_count as usize {
            let base = index.checked_mul(COMPACT_COLLISION_HEIGHT_OVERRIDE_BYTES)?;
            let bytes = self
                .height_overrides
                .get(base..base.checked_add(COMPACT_COLLISION_HEIGHT_OVERRIDE_BYTES)?)?;
            if read_u16(bytes, 0)? == sector_index && *bytes.get(2)? == surface {
                return Some([
                    [
                        read_i32(bytes, 4)?,
                        read_i32(bytes, 8)?,
                        read_i32(bytes, 12)?,
                    ],
                    [
                        read_i32(bytes, 16)?,
                        read_i32(bytes, 20)?,
                        read_i32(bytes, 24)?,
                    ],
                ]);
            }
            index += 1;
        }
        None
    }

    fn validate_sector_wall_ranges(self) -> bool {
        let mut index = 0usize;
        let sector_count = self.width as usize * self.depth as usize;
        while index < sector_count {
            let base = match index.checked_mul(COMPACT_COLLISION_SECTOR_BYTES) {
                Some(base) => base,
                None => return false,
            };
            let Some(bytes) = self
                .sectors
                .get(base..base.saturating_add(COMPACT_COLLISION_SECTOR_BYTES))
            else {
                return false;
            };
            let Some(first) = read_u16(bytes, 6) else {
                return false;
            };
            let Some(count) = read_u16(bytes, 8) else {
                return false;
            };
            if first
                .checked_add(count)
                .is_none_or(|end| end > self.wall_count)
            {
                return false;
            }
            index += 1;
        }
        true
    }
}

#[derive(Copy, Clone, Debug)]
/// Collision-side projection of one compact decoded sector.
pub struct CompactSectorCollision {
    flags: u8,
    floor_split: u8,
    ceiling_split: u8,
    floor_triangle_flags: u8,
    ceiling_triangle_flags: u8,
    first_wall: u16,
    wall_count: u16,
    floor_heights: [i32; 4],
    ceiling_heights: [i32; 4],
    floor_triangle_heights: [[i32; 3]; 2],
    ceiling_triangle_heights: [[i32; 3]; 2],
}

impl CompactSectorCollision {
    fn has_geometry(self) -> bool {
        self.has_floor() || self.has_ceiling() || self.wall_count != 0
    }

    fn has_floor(self) -> bool {
        self.flags & compact_collision_sector_flags::HAS_FLOOR != 0
            && (horizontal_triangle_present(self.floor_triangle_flags, 0)
                || horizontal_triangle_present(self.floor_triangle_flags, 1))
    }

    fn has_ceiling(self) -> bool {
        self.flags & compact_collision_sector_flags::HAS_CEILING != 0
            && (horizontal_triangle_present(self.ceiling_triangle_flags, 0)
                || horizontal_triangle_present(self.ceiling_triangle_flags, 1))
    }
}

#[derive(Copy, Clone, Debug)]
/// Collision probe projection of one compact decoded sector.
pub struct CompactSectorCollisionProbe {
    flags: u8,
    first_wall: u16,
    wall_count: u16,
}

impl CompactSectorCollisionProbe {
    fn has_floor(self) -> bool {
        self.flags & compact_collision_sector_flags::HAS_FLOOR != 0
    }
}

#[derive(Copy, Clone, Debug)]
/// Collision-side projection of one compact decoded wall.
pub struct CompactWallCollision {
    direction: u8,
    flags: u8,
    shape: u16,
    heights: [i32; 4],
}

/// Runtime collision room source.
#[derive(Copy, Clone, Debug)]
pub enum RuntimeCollisionRoom<'a> {
    /// Full `.psxw` runtime room.
    Runtime(RuntimeRoom<'a>),
    /// Compact collision-only room.
    Compact(CompactCollisionRoom<'a>),
}

impl<'a> RuntimeCollisionRoom<'a> {
    /// Collision-side facade.
    pub fn collision(&self) -> RoomCollision<'a, '_> {
        match self {
            Self::Runtime(room) => room.collision(),
            Self::Compact(room) => room.collision(),
        }
    }

    /// Width in grid sectors.
    pub fn width(self) -> u16 {
        match self {
            Self::Runtime(room) => room.width(),
            Self::Compact(room) => room.width(),
        }
    }

    /// Depth in grid sectors.
    pub fn depth(self) -> u16 {
        match self {
            Self::Runtime(room) => room.depth(),
            Self::Compact(room) => room.depth(),
        }
    }

    /// Engine units per sector.
    pub fn sector_size(self) -> i32 {
        match self {
            Self::Runtime(room) => room.sector_size(),
            Self::Compact(room) => room.sector_size(),
        }
    }

    /// Number of wall records in the collision room.
    pub fn wall_count(self) -> u16 {
        match self {
            Self::Runtime(room) => room.wall_count(),
            Self::Compact(room) => room.wall_count(),
        }
    }

    /// Ambient room RGB used for actor lighting while this room is active.
    pub fn ambient_color(self) -> [u8; 3] {
        match self {
            Self::Runtime(room) => room.render().ambient_color(),
            Self::Compact(room) => room.ambient_color(),
        }
    }
}

// ============================================================
// Render-vs-collision facades
// ============================================================
//
// Tomb Raider runs render and collision off the same on-disk
// room data, but with two distinct read paths: the renderer
// walks tr_face4 / tr_face3 lists with materials and lighting;
// collision walks tr_room_sector heights and traversal portals
// with no concept of texture pages. The two systems literally
// cannot fetch each other's fields at the API level.
//
// `RoomRender` / `RoomCollision` give us the same discipline
// over a single `RuntimeRoom`. Both views are zero-cost
// `Copy` borrows. Streamed chunks now expose separate render and
// collision payload ranges, but a caller that says
// `room.render().sector(...)` cannot accidentally branch on
// `floor_walkable`, and a caller that says
// `room.collision().sector(...)` cannot accidentally read a
// material slot.

/// Render-side view over a [`RuntimeRoom`].
///
/// Exposes only the fields a draw pass cares about: heights and
/// splits for vertex emission, materials for tpage / clut lookup,
/// world-level lighting state. Collision-only state
/// (`walkable`, `solid`, traversal portals) is intentionally
/// **not** reachable through this view.
#[derive(Copy, Clone, Debug)]
pub struct RoomRender<'a, 'b> {
    room: &'b RuntimeRoom<'a>,
}

impl<'a, 'b> RoomRender<'a, 'b> {
    /// Width in grid sectors.
    pub fn width(self) -> u16 {
        self.room.width()
    }

    /// Depth in grid sectors.
    pub fn depth(self) -> u16 {
        self.room.depth()
    }

    /// Engine units per sector.
    pub fn sector_size(self) -> i32 {
        self.room.sector_size()
    }

    /// Room ambient RGB color.
    pub fn ambient_color(self) -> [u8; 3] {
        self.room.world().ambient_color()
    }

    /// Whether fog / depth cue is enabled for this world.
    pub fn fog_enabled(self) -> bool {
        self.room.world().fog_enabled()
    }

    /// Whether this room carries baked static vertex lighting in
    /// its `.psxw` face records.
    pub fn static_vertex_lighting(self) -> bool {
        self.room.world().static_vertex_lighting()
    }

    /// Sector at `(x, z)` for render purposes, or `None` for
    /// empty cells.
    pub fn sector(self, x: u16, z: u16) -> Option<SectorRender> {
        self.room.sector(x, z).map(SectorRender)
    }

    /// Wall record by sector-local index, render view.
    pub fn sector_wall(self, sector: SectorRender, local_index: u16) -> Option<WallRender> {
        self.room.sector_wall(sector.0, local_index).map(WallRender)
    }

    /// Baked floor vertex lighting for a sector, if this room
    /// carries static lighting.
    pub fn floor_light(self, sx: u16, sz: u16) -> Option<[[u8; 3]; 4]> {
        let index = surface_light_sector_index(self.depth(), sx, sz)?;
        self.room.surface_light(index.checked_mul(2)?)
    }

    /// Baked ceiling vertex lighting for a sector, if this room
    /// carries static lighting.
    pub fn ceiling_light(self, sx: u16, sz: u16) -> Option<[[u8; 3]; 4]> {
        let index = surface_light_sector_index(self.depth(), sx, sz)?;
        self.room
            .surface_light(index.checked_mul(2)?.checked_add(1)?)
    }

    /// Baked wall vertex lighting for a sector-local wall, if
    /// this room carries static lighting.
    pub fn wall_light(self, sector: SectorRender, local_index: u16) -> Option<[[u8; 3]; 4]> {
        let wall_index = sector.first_wall().checked_add(local_index)?;
        let first_wall_light = self.width().checked_mul(self.depth())?.checked_mul(2)?;
        self.room
            .surface_light(first_wall_light.checked_add(wall_index)?)
    }
}

/// Collision-side view over a [`RuntimeRoom`].
///
/// Exposes only the fields a movement / floor-sample query
/// cares about: heights for surface sampling, splits for
/// triangulation of the height grid, walkable / solid bits for
/// stop-or-pass decisions. Render-only state (materials,
/// lighting, fog) is intentionally **not** reachable through
/// this view.
#[derive(Copy, Clone, Debug)]
pub enum RoomCollision<'a, 'b> {
    /// Collision view over a full runtime room.
    Runtime(&'b RuntimeRoom<'a>),
    /// Collision view over a compact collision-only room.
    Compact(&'b CompactCollisionRoom<'a>),
}

impl<'a, 'b> RoomCollision<'a, 'b> {
    /// Width in grid sectors.
    pub fn width(self) -> u16 {
        match self {
            Self::Runtime(room) => room.width(),
            Self::Compact(room) => room.width(),
        }
    }

    /// Depth in grid sectors.
    pub fn depth(self) -> u16 {
        match self {
            Self::Runtime(room) => room.depth(),
            Self::Compact(room) => room.depth(),
        }
    }

    /// Engine units per sector.
    pub fn sector_size(self) -> i32 {
        match self {
            Self::Runtime(room) => room.sector_size(),
            Self::Compact(room) => room.sector_size(),
        }
    }

    /// Number of wall records in the collision room.
    pub fn wall_count(self) -> u16 {
        match self {
            Self::Runtime(room) => room.wall_count(),
            Self::Compact(room) => room.wall_count(),
        }
    }

    /// Sector at `(x, z)` for collision purposes, or `None` for
    /// empty cells.
    pub fn sector(self, x: u16, z: u16) -> Option<SectorCollision> {
        match self {
            Self::Runtime(room) => room.sector(x, z).map(SectorCollision::Runtime),
            Self::Compact(room) => room.sector(x, z).map(SectorCollision::Compact),
        }
    }

    /// Sector at `(x, z)` without applying render-side horizontal
    /// override records. Camera wall probes use this cheaper path
    /// because they only need floor presence and wall ranges.
    pub fn sector_without_horizontal_overrides(self, x: u16, z: u16) -> Option<SectorCollision> {
        match self {
            Self::Runtime(room) => room
                .world()
                .sector_without_horizontal_overrides(x, z)
                .map(SectorCollision::Runtime),
            Self::Compact(room) => room.sector(x, z).map(SectorCollision::Compact),
        }
    }

    /// Minimal sector header for camera wall probes.
    pub fn sector_probe(self, x: u16, z: u16) -> Option<SectorCollisionProbe> {
        match self {
            Self::Runtime(room) => room
                .world()
                .sector_collision_probe(x, z)
                .map(SectorCollisionProbe::Runtime),
            Self::Compact(room) => room.sector_probe(x, z).map(SectorCollisionProbe::Compact),
        }
    }

    /// Wall record by sector-local index, collision view.
    pub fn sector_wall(self, sector: SectorCollision, local_index: u16) -> Option<WallCollision> {
        match (self, sector) {
            (Self::Runtime(room), SectorCollision::Runtime(sector)) => room
                .sector_wall(sector, local_index)
                .map(WallCollision::Runtime),
            (Self::Compact(room), SectorCollision::Compact(sector)) => room
                .sector_wall(sector, local_index)
                .map(WallCollision::Compact),
            _ => None,
        }
    }

    /// Wall record by sector-local index for a minimal probe sector.
    pub fn sector_probe_wall(
        self,
        sector: SectorCollisionProbe,
        local_index: u16,
    ) -> Option<WallCollision> {
        match (self, sector) {
            (Self::Runtime(room), SectorCollisionProbe::Runtime(sector)) => {
                if local_index >= sector.wall_count() {
                    return None;
                }
                room.world()
                    .wall(sector.first_wall().checked_add(local_index)?)
                    .map(WallCollision::Runtime)
            }
            (Self::Compact(room), SectorCollisionProbe::Compact(sector)) => room
                .sector_probe_wall(sector, local_index)
                .map(WallCollision::Compact),
            _ => None,
        }
    }
}

/// Render-side projection of one decoded sector.
#[derive(Copy, Clone, Debug)]
pub struct SectorRender(psx_asset::WorldSector);

impl SectorRender {
    /// `true` if this sector emits a floor face.
    pub fn has_floor(self) -> bool {
        self.0.has_floor()
    }

    /// `true` if this sector emits a ceiling face.
    pub fn has_ceiling(self) -> bool {
        self.0.has_ceiling()
    }

    /// Floor diagonal split id.
    pub fn floor_split(self) -> u8 {
        self.0.floor_split()
    }

    /// Ceiling diagonal split id.
    pub fn ceiling_split(self) -> u8 {
        self.0.ceiling_split()
    }

    /// Floor material slot, if any.
    pub fn floor_material(self) -> Option<u16> {
        self.0.floor_material()
    }

    /// Floor split-triangle material slot, if present.
    pub fn floor_triangle_material(self, index: usize) -> Option<u16> {
        self.0.floor_triangle_material(index)
    }

    /// `true` if the floor split triangle is present.
    pub fn floor_triangle_present(self, index: usize) -> bool {
        self.0.floor_triangle_present(index)
    }

    /// Ceiling material slot, if any.
    pub fn ceiling_material(self) -> Option<u16> {
        self.0.ceiling_material()
    }

    /// Ceiling split-triangle material slot, if present.
    pub fn ceiling_triangle_material(self, index: usize) -> Option<u16> {
        self.0.ceiling_triangle_material(index)
    }

    /// `true` if the ceiling split triangle is present.
    pub fn ceiling_triangle_present(self, index: usize) -> bool {
        self.0.ceiling_triangle_present(index)
    }

    /// Floor corner heights `[NW, NE, SE, SW]` for vertex emission.
    pub fn floor_heights(self) -> [i32; 4] {
        self.0.floor_heights()
    }

    /// Floor UVs `[NW, NE, SE, SW]` for textured vertex emission.
    pub fn floor_uvs(self) -> [(u8, u8); 4] {
        self.0.floor_uvs().corners()
    }

    /// Floor split-triangle UVs `[NW, NE, SE, SW]`.
    pub fn floor_triangle_uvs(self, index: usize) -> [(u8, u8); 4] {
        self.0.floor_triangle_uvs(index).corners()
    }

    /// Floor split-triangle heights in that triangle's corner order.
    pub fn floor_triangle_heights(self, index: usize) -> [i32; 3] {
        self.0.floor_triangle_heights(index)
    }

    /// Ceiling corner heights `[NW, NE, SE, SW]` for vertex emission.
    pub fn ceiling_heights(self) -> [i32; 4] {
        self.0.ceiling_heights()
    }

    /// Ceiling UVs `[NW, NE, SE, SW]` for textured vertex emission.
    pub fn ceiling_uvs(self) -> [(u8, u8); 4] {
        self.0.ceiling_uvs().corners()
    }

    /// Ceiling split-triangle UVs `[NW, NE, SE, SW]`.
    pub fn ceiling_triangle_uvs(self, index: usize) -> [(u8, u8); 4] {
        self.0.ceiling_triangle_uvs(index).corners()
    }

    /// Ceiling split-triangle heights in that triangle's corner order.
    pub fn ceiling_triangle_heights(self, index: usize) -> [i32; 3] {
        self.0.ceiling_triangle_heights(index)
    }

    /// First global wall index for this sector.
    pub fn first_wall(self) -> u16 {
        self.0.first_wall()
    }

    /// Number of walls belonging to this sector.
    pub fn wall_count(self) -> u16 {
        self.0.wall_count()
    }
}

/// Collision-side projection of one decoded sector.
#[derive(Copy, Clone, Debug)]
pub enum SectorCollision {
    /// Sector decoded from a full runtime room.
    Runtime(psx_asset::WorldSector),
    /// Sector decoded from a compact collision-only room.
    Compact(CompactSectorCollision),
}

impl SectorCollision {
    /// `true` if this sector has a floor surface to sample.
    pub fn has_floor(self) -> bool {
        match self {
            Self::Runtime(sector) => sector.has_floor(),
            Self::Compact(sector) => sector.has_floor(),
        }
    }

    /// `true` if this sector has a ceiling surface for clearance.
    pub fn has_ceiling(self) -> bool {
        match self {
            Self::Runtime(sector) => sector.has_ceiling(),
            Self::Compact(sector) => sector.has_ceiling(),
        }
    }

    /// `true` if the floor face is walkable.
    pub fn floor_walkable(self) -> bool {
        match self {
            Self::Runtime(sector) => sector.floor_walkable(),
            Self::Compact(sector) => {
                sector.flags & compact_collision_sector_flags::FLOOR_WALKABLE != 0
            }
        }
    }

    /// `true` if the floor split triangle is present and walkable.
    pub fn floor_triangle_walkable(self, index: usize) -> bool {
        match self {
            Self::Runtime(sector) => sector.floor_triangle_walkable(index),
            Self::Compact(sector) => {
                horizontal_triangle_present(sector.floor_triangle_flags, index)
                    && horizontal_triangle_walkable(sector.floor_triangle_flags, index)
            }
        }
    }

    /// `true` if the floor split triangle is present.
    pub fn floor_triangle_present(self, index: usize) -> bool {
        match self {
            Self::Runtime(sector) => sector.floor_triangle_present(index),
            Self::Compact(sector) => {
                horizontal_triangle_present(sector.floor_triangle_flags, index)
            }
        }
    }

    /// Floor diagonal split id (decides the triangulation used
    /// to interpolate height samples).
    pub fn floor_split(self) -> u8 {
        match self {
            Self::Runtime(sector) => sector.floor_split(),
            Self::Compact(sector) => sector.floor_split,
        }
    }

    /// Ceiling diagonal split id.
    pub fn ceiling_split(self) -> u8 {
        match self {
            Self::Runtime(sector) => sector.ceiling_split(),
            Self::Compact(sector) => sector.ceiling_split,
        }
    }

    /// Floor corner heights `[NW, NE, SE, SW]`.
    pub fn floor_heights(self) -> [i32; 4] {
        match self {
            Self::Runtime(sector) => sector.floor_heights(),
            Self::Compact(sector) => sector.floor_heights,
        }
    }

    /// Floor split-triangle heights in that triangle's corner order.
    pub fn floor_triangle_heights(self, index: usize) -> [i32; 3] {
        match self {
            Self::Runtime(sector) => sector.floor_triangle_heights(index),
            Self::Compact(sector) => sector.floor_triangle_heights[index.min(1)],
        }
    }

    /// Ceiling corner heights `[NW, NE, SE, SW]`.
    pub fn ceiling_heights(self) -> [i32; 4] {
        match self {
            Self::Runtime(sector) => sector.ceiling_heights(),
            Self::Compact(sector) => sector.ceiling_heights,
        }
    }

    /// Ceiling split-triangle heights in that triangle's corner order.
    pub fn ceiling_triangle_heights(self, index: usize) -> [i32; 3] {
        match self {
            Self::Runtime(sector) => sector.ceiling_triangle_heights(index),
            Self::Compact(sector) => sector.ceiling_triangle_heights[index.min(1)],
        }
    }

    /// First global wall index for this sector.
    pub fn first_wall(self) -> u16 {
        match self {
            Self::Runtime(sector) => sector.first_wall(),
            Self::Compact(sector) => sector.first_wall,
        }
    }

    /// Number of walls belonging to this sector.
    pub fn wall_count(self) -> u16 {
        match self {
            Self::Runtime(sector) => sector.wall_count(),
            Self::Compact(sector) => sector.wall_count,
        }
    }
}

/// Collision probe projection of one decoded sector header.
#[derive(Copy, Clone, Debug)]
pub enum SectorCollisionProbe {
    /// Probe decoded from a full runtime room.
    Runtime(psx_asset::WorldSectorCollisionProbe),
    /// Probe decoded from a compact collision-only room.
    Compact(CompactSectorCollisionProbe),
}

impl SectorCollisionProbe {
    /// `true` if this sector has a floor surface to sample.
    pub fn has_floor(self) -> bool {
        match self {
            Self::Runtime(sector) => sector.has_floor(),
            Self::Compact(sector) => sector.has_floor(),
        }
    }

    /// First global wall index for this sector.
    pub fn first_wall(self) -> u16 {
        match self {
            Self::Runtime(sector) => sector.first_wall(),
            Self::Compact(sector) => sector.first_wall,
        }
    }

    /// Number of walls belonging to this sector.
    pub fn wall_count(self) -> u16 {
        match self {
            Self::Runtime(sector) => sector.wall_count(),
            Self::Compact(sector) => sector.wall_count,
        }
    }
}

/// Render-side projection of one decoded wall.
#[derive(Copy, Clone, Debug)]
pub struct WallRender(psx_asset::WorldWall);

impl WallRender {
    /// Direction id.
    pub fn direction(self) -> u8 {
        self.0.direction()
    }

    /// Material slot.
    pub fn material(self) -> u16 {
        self.0.material()
    }

    /// Wall shape id, see `psxed_format::world::wall_shape`.
    pub fn shape(self) -> u16 {
        self.0.shape()
    }

    /// Wall heights `[bottom-left, bottom-right, top-right, top-left]`.
    pub fn heights(self) -> [i32; 4] {
        self.0.heights()
    }

    /// Wall UVs `[bottom-left, bottom-right, top-right, top-left]`.
    pub fn uvs(self) -> [(u8, u8); 4] {
        self.0.uvs().corners()
    }
}

fn surface_light_sector_index(depth: u16, sx: u16, sz: u16) -> Option<u16> {
    sx.checked_mul(depth)?.checked_add(sz)
}

/// Collision-side projection of one decoded wall.
#[derive(Copy, Clone, Debug)]
pub enum WallCollision {
    /// Wall decoded from a full runtime room.
    Runtime(psx_asset::WorldWall),
    /// Wall decoded from a compact collision-only room.
    Compact(CompactWallCollision),
}

impl WallCollision {
    /// Direction id.
    pub fn direction(self) -> u8 {
        match self {
            Self::Runtime(wall) => wall.direction(),
            Self::Compact(wall) => wall.direction,
        }
    }

    /// `true` when this wall blocks character movement.
    pub fn solid(self) -> bool {
        match self {
            Self::Runtime(wall) => wall.solid(),
            Self::Compact(wall) => wall.flags & compact_collision_wall_flags::SOLID != 0,
        }
    }

    /// Wall shape id, see `psxed_format::world::wall_shape`.
    pub fn shape(self) -> u16 {
        match self {
            Self::Runtime(wall) => wall.shape(),
            Self::Compact(wall) => wall.shape,
        }
    }

    /// Wall heights `[bottom-left, bottom-right, top-right, top-left]`
    /// for slab-vs-character clearance checks.
    pub fn heights(self) -> [i32; 4] {
        match self {
            Self::Runtime(wall) => wall.heights(),
            Self::Compact(wall) => wall.heights,
        }
    }
}

fn checked_table_len(count: u16, stride: usize) -> Result<usize, CompactCollisionParseError> {
    (count as usize)
        .checked_mul(stride)
        .ok_or(CompactCollisionParseError::InvalidLayout)
}

fn take_bytes<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    len: usize,
) -> Result<&'a [u8], CompactCollisionParseError> {
    let end = offset
        .checked_add(len)
        .ok_or(CompactCollisionParseError::InvalidLayout)?;
    let slice = bytes
        .get(*offset..end)
        .ok_or(CompactCollisionParseError::Truncated)?;
    *offset = end;
    Ok(slice)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    let raw = bytes.get(offset..offset + 4)?;
    Some(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_i32x4(bytes: &[u8], offset: usize) -> Option<[i32; 4]> {
    Some([
        read_i32(bytes, offset)?,
        read_i32(bytes, offset + 4)?,
        read_i32(bytes, offset + 8)?,
        read_i32(bytes, offset + 12)?,
    ])
}

const fn horizontal_triangle_present(flags: u8, index: usize) -> bool {
    let bit = if index == 0 {
        compact_collision_triangle_flags::TRI_A_PRESENT
    } else {
        compact_collision_triangle_flags::TRI_B_PRESENT
    };
    flags & bit != 0
}

const fn horizontal_triangle_walkable(flags: u8, index: usize) -> bool {
    let bit = if index == 0 {
        compact_collision_triangle_flags::TRI_A_WALKABLE
    } else {
        compact_collision_triangle_flags::TRI_B_WALKABLE
    };
    flags & bit != 0
}

fn horizontal_triangle_heights(heights: [i32; 4], split: u8, index: usize) -> [i32; 3] {
    let corners = psx_asset::world_topology::split_triangle(split, index);
    [
        heights[corners[0]],
        heights[corners[1]],
        heights[corners[2]],
    ]
}

// Compile-time guarantee that `RuntimeRoom` and its render /
// collision facades stay zero-allocation `Copy` types. Any
// future change that adds an owned field (Vec, String, …) will
// break the build here, which is the whole point.
const _: () = {
    const fn _assert_copy<T: Copy>() {}
    _assert_copy::<RuntimeRoom<'static>>();
    _assert_copy::<CompactCollisionRoom<'static>>();
    _assert_copy::<RuntimeCollisionRoom<'static>>();
    _assert_copy::<RoomRender<'static, 'static>>();
    _assert_copy::<RoomCollision<'static, 'static>>();
    _assert_copy::<SectorRender>();
    _assert_copy::<SectorCollision>();
    _assert_copy::<WallRender>();
    _assert_copy::<WallCollision>();
};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    const MAT_FLOOR: WorldMaterialId = WorldMaterialId(1);
    const MAT_WALL: WorldMaterialId = WorldMaterialId(2);
    const NORTH_WALL: [GridVerticalFace; 1] = [GridVerticalFace::flat(0, 1024, MAT_WALL)];
    const SECTORS: [Option<GridSector<'static>>; 2] = [
        Some(GridSector {
            floor: Some(GridHorizontalFace::flat(0, MAT_FLOOR)),
            ceiling: None,
            walls: GridWalls {
                north: &NORTH_WALL,
                ..GridWalls::EMPTY
            },
        }),
        None,
    ];
    const ROOM: GridRoom<'static> = GridRoom::new(WorldVertex::ZERO, 1, 2, &SECTORS);

    #[test]
    fn room_maps_world_positions_to_sectors() {
        assert_eq!(ROOM.world_to_grid(12, 1030), Some(GridCoord { x: 0, z: 1 }));
        assert_eq!(ROOM.world_to_grid(-1, 0), None);
        assert_eq!(ROOM.world_to_grid(0, 2048), None);
    }

    #[test]
    fn sector_preserves_floor_and_wall_data() {
        let sector = ROOM.sector(GridCoord { x: 0, z: 0 }).unwrap();
        assert!(sector.has_geometry());
        assert_eq!(sector.floor.unwrap().average_height(), 0);
        assert_eq!(sector.walls.get(GridDirection::North)[0].top(), 1024);
        assert!(ROOM.sector(GridCoord { x: 0, z: 1 }).is_none());
    }

    #[test]
    fn floor_at_samples_flat_floor_and_empty_cells() {
        let sample = ROOM.floor_at(12, 12).unwrap();
        assert_eq!(sample.coord, GridCoord { x: 0, z: 0 });
        assert_eq!(sample.height, 0);
        assert!(ROOM.floor_at(12, 1030).is_none());
    }

    #[test]
    fn walls_returns_sector_edge_or_empty_slice() {
        assert_eq!(
            ROOM.walls(GridCoord { x: 0, z: 0 }, GridDirection::North)
                .len(),
            1
        );
        assert!(ROOM
            .walls(GridCoord { x: 0, z: 1 }, GridDirection::North)
            .is_empty());
    }

    #[test]
    fn height_at_local_respects_nw_se_split() {
        let face = GridHorizontalFace {
            heights: [0, 1024, 2048, 1024],
            split: GridSplit::NorthWestSouthEast,
            material: MAT_FLOOR,
            walkable: true,
        };
        assert_eq!(face.height_at_local(0, 0), 0);
        assert_eq!(face.height_at_local(1024, 0), 1024);
        assert_eq!(face.height_at_local(1024, 1024), 2048);
        assert_eq!(face.height_at_local(0, 1024), 1024);
        assert_eq!(face.height_at_local(512, 512), 1024);
    }

    #[test]
    fn height_at_local_respects_ne_sw_split() {
        let face = GridHorizontalFace {
            heights: [0, 1024, 2048, 1024],
            split: GridSplit::NorthEastSouthWest,
            material: MAT_FLOOR,
            walkable: true,
        };
        assert_eq!(face.height_at_local(0, 0), 0);
        assert_eq!(face.height_at_local(1024, 0), 1024);
        assert_eq!(face.height_at_local(1024, 1024), 2048);
        assert_eq!(face.height_at_local(0, 1024), 1024);
        assert_eq!(face.height_at_local(512, 512), 1024);
    }
}
