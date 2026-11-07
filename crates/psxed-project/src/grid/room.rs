//! One room: a rectangle holding a quadtree of leaves, with its own light
//! and fog settings.

use serde::{Deserialize, Serialize};

use super::bands::BandSet;
use super::leaf::LeafKey;
use super::leaf_map::{LeafError, LeafMap};
use super::overrides::{EdgeOverrides, RoomDefaults, WallMode};
use super::plane::GridPlane;
use super::sector::GridSector;

/// Distance fog for a room, in grid units.
///
/// Imported verbatim from bonnie-32: fog starts at `start` and reaches full
/// strength `falloff` units further out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridFog {
    /// Whether the fog is applied.
    pub enabled: bool,
    /// Fog colour.
    pub color: [u8; 3],
    /// Distance where fog begins, grid units.
    pub start: i32,
    /// Distance from `start` to full fog, grid units.
    pub falloff: i32,
}

impl Default for GridFog {
    fn default() -> Self {
        Self {
            enabled: false,
            color: [128, 128, 128],
            start: 8192,
            falloff: 8192,
        }
    }
}

impl GridFog {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Default ambient light of a new room, per channel. The bake treats 128 as
/// neutral (the texture shown unmodified).
pub const DEFAULT_AMBIENT: [u8; 3] = [96; 3];

fn default_ambient() -> [u8; 3] {
    DEFAULT_AMBIENT
}

/// A room.
///
/// The cells are a quadtree of [leaves](LeafMap) keyed in **world** clicks,
/// so a leaf never moves when the room grows or trims; the rectangle
/// (`origin`, `width`, `depth`, in whole sectors) is what the leaves must
/// lie inside. Level 2 is one sector, so every room made before the
/// quadtree is "all leaves at level 2" and the sector accessors
/// ([`Self::sector`] and friends, addressed by local sector) see exactly
/// those. Where the room sits in the world is `origin` (in whole sectors, so
/// rooms share one lattice) plus `y_offset` (grid units) for a vertical
/// shift. Rooms may overlap in plan; that is how floors stack today.
///
/// On disk a room whose leaves are all whole sectors inside the rectangle is
/// written as the dense `sectors` list, `width * depth` entries, `z` major,
/// exactly as before the quadtree; any other room is written as a `leaves`
/// list in Morton order. Either form loads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridRoom {
    /// Display name.
    pub name: String,
    /// World position of local sector `(0, 0)`, in sectors `[x, z]`.
    pub origin: [i32; 2],
    /// Vertical offset of the whole room, grid units.
    pub y_offset: i32,
    /// Sectors along X.
    pub width: usize,
    /// Sectors along Z.
    pub depth: usize,
    /// Ambient light, per channel.
    pub ambient: [u8; 3],
    /// Distance fog.
    pub fog: GridFog,
    /// The cells, keyed by level and world click. Use the accessors; they
    /// keep leaves disjoint and aligned.
    pub leaves: LeafMap,
    /// Free planes the room owns. They draw and collide with the room and
    /// must lie inside its footprint to collide (editing grows the room).
    pub planes: Vec<GridPlane>,
    /// Static meshes placed in the room (instances of the world's mesh
    /// assets). They draw and collide with the room; see [`super::mesh`].
    pub meshes: Vec<super::mesh::GridMesh>,
    /// How walls come to exist: only as records (legacy) or derived from
    /// height differences.
    pub wall_mode: WallMode,
    /// Fallbacks for derived faces.
    pub defaults: RoomDefaults,
    /// What the user changed on individual derived wall segments.
    pub edge_overrides: EdgeOverrides,
    /// Horizontal wall band sets, referenced by index from the room
    /// default, derived overrides and authored walls.
    pub band_sets: Vec<BandSet>,
}

impl GridRoom {
    /// An empty room of `width` by `depth` sectors.
    pub fn new(name: impl Into<String>, width: usize, depth: usize) -> Self {
        Self {
            name: name.into(),
            origin: [0, 0],
            y_offset: 0,
            width,
            depth,
            ambient: DEFAULT_AMBIENT,
            fog: GridFog::default(),
            leaves: LeafMap::new(),
            planes: Vec::new(),
            meshes: Vec::new(),
            wall_mode: WallMode::Authored,
            defaults: RoomDefaults::default(),
            edge_overrides: EdgeOverrides::default(),
            band_sets: Vec::new(),
        }
    }

    /// An empty room whose walls are derived from height differences, the
    /// way every new room is made.
    pub fn new_derived(name: impl Into<String>, width: usize, depth: usize) -> Self {
        Self {
            wall_mode: WallMode::Derived,
            ..Self::new(name, width, depth)
        }
    }

    // ---- sectors: the level-2 leaves, by local coordinate ---------------

    /// The leaf key of local sector `(x, z)`, or `None` outside the
    /// rectangle.
    pub fn sector_key(&self, x: usize, z: usize) -> Option<LeafKey> {
        (x < self.width && z < self.depth).then(|| LeafKey::sector(self.world_cell(x, z)))
    }

    /// The sector at local `(x, z)`: the level-2 leaf there. A cell held by
    /// a coarser or finer leaf has no sector; use [`Self::leaf_at`] for any
    /// size.
    pub fn sector(&self, x: usize, z: usize) -> Option<&GridSector> {
        self.leaves.get(self.sector_key(x, z)?)
    }

    /// Mutable sector at local `(x, z)`.
    pub fn sector_mut(&mut self, x: usize, z: usize) -> Option<&mut GridSector> {
        let key = self.sector_key(x, z)?;
        self.leaves.get_mut(key)
    }

    /// Sector at local `(x, z)`, created empty when absent. `None` outside
    /// the room (use [`Self::grow_to_include`] first to extend it) and where
    /// a leaf of another level already holds the cell.
    pub fn sector_or_insert(&mut self, x: usize, z: usize) -> Option<&mut GridSector> {
        let key = self.sector_key(x, z)?;
        self.leaves
            .get_or_insert_with(key, GridSector::default)
            .ok()
    }

    /// Store `sector` (or remove the sector, for `None`) at local `(x, z)`,
    /// returning what was there. Does nothing and returns `None` outside
    /// the room or where a leaf of another level holds the cell.
    pub fn set_sector(
        &mut self,
        x: usize,
        z: usize,
        sector: Option<GridSector>,
    ) -> Option<GridSector> {
        let key = self.sector_key(x, z)?;
        match sector {
            Some(sector) => self.leaves.insert(key, sector).ok().flatten(),
            None => self.leaves.remove(key),
        }
    }

    /// Remove and return the sector at local `(x, z)`.
    pub fn take_sector(&mut self, x: usize, z: usize) -> Option<GridSector> {
        self.set_sector(x, z, None)
    }

    /// Local coordinates of a world sector, or `None` outside the room.
    pub fn local(&self, world: [i32; 2]) -> Option<[usize; 2]> {
        let x = usize::try_from(world[0].checked_sub(self.origin[0])?).ok()?;
        let z = usize::try_from(world[1].checked_sub(self.origin[1])?).ok()?;
        (x < self.width && z < self.depth).then_some([x, z])
    }

    /// World sector of local `(x, z)`.
    pub fn world_cell(&self, x: usize, z: usize) -> [i32; 2] {
        [self.origin[0] + x as i32, self.origin[1] + z as i32]
    }

    /// Number of sectors that exist (level-2 leaves).
    pub fn sector_count(&self) -> usize {
        self.leaves.keys().filter(|k| k.is_sector()).count()
    }

    /// Every existing sector as `(x, z, sector)` with local coordinates, in
    /// the order the storage had before the quadtree: `z` major, then `x`.
    /// The cook, the face lists and every golden hash depend on this order,
    /// which is why the compatibility accessor is not in Morton order.
    /// Leaves of other levels, and level-2 leaves outside the rectangle, are
    /// not listed; see [`Self::leaves`].
    pub fn iter(&self) -> impl Iterator<Item = (usize, usize, &GridSector)> + '_ {
        let mut cells: Vec<(usize, usize, &GridSector)> = self
            .leaves
            .iter()
            .filter(|(key, _)| key.is_sector())
            .filter_map(|(key, sector)| {
                let [x, z] = self.local(key.cell())?;
                Some((x, z, sector))
            })
            .collect();
        cells.sort_unstable_by_key(|&(x, z, _)| (z, x));
        cells.into_iter()
    }

    // ---- leaves of any size -------------------------------------------

    /// Every sector of the rectangle that any leaf covers, as local
    /// `[x, z]`, `z` major then `x`: the cells the faces are listed under.
    /// For a room of whole sectors this is [`Self::iter`]'s cells. A leaf
    /// larger than a sector covers several, a smaller one shares its sector
    /// with others.
    pub fn covered_cells(&self) -> Vec<[usize; 2]> {
        let mut cells = std::collections::BTreeSet::new();
        for key in self.leaves.keys() {
            let (lo, hi) = key.sector_range();
            for z in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    if let Some([lx, lz]) = self.local([x, z]) {
                        cells.insert((lz, lx));
                    }
                }
            }
        }
        cells.into_iter().map(|(z, x)| [x, z]).collect()
    }

    /// The cells the faces are listed under, local `[x, z]`, `z` major: the
    /// existing sectors of a room of whole sectors (the order the cook and
    /// every golden hash depend on), the covered cells of a room with
    /// leaves of other sizes.
    pub fn face_cells(&self) -> Vec<[usize; 2]> {
        if self.leaves.is_sectors_only() {
            self.iter().map(|(x, z, _)| [x, z]).collect()
        } else {
            self.covered_cells()
        }
    }

    /// Whether any leaf of local cell `[x, z]` has a floor and no ceiling:
    /// the open sky the renderer flags.
    pub fn cell_is_open(&self, x: usize, z: usize) -> bool {
        self.leaves_in_cell(x, z)
            .iter()
            .any(|(_, s)| s.ground_floor().is_some() && s.ground_ceiling().is_none())
    }

    /// The sectors-and-leaves view of one cell for the faces: every leaf
    /// that touches local cell `[x, z]`.
    pub fn leaves_in_cell(&self, x: usize, z: usize) -> Vec<(LeafKey, &GridSector)> {
        self.leaves.in_sector(self.world_cell(x, z))
    }

    /// Every leaf in Morton order, of any level.
    pub fn leaves(&self) -> impl DoubleEndedIterator<Item = (LeafKey, &GridSector)> + '_ {
        self.leaves.iter()
    }

    /// The leaf stored under exactly `key`.
    pub fn leaf(&self, key: LeafKey) -> Option<&GridSector> {
        self.leaves.get(key)
    }

    /// Mutable leaf stored under exactly `key`.
    pub fn leaf_mut(&mut self, key: LeafKey) -> Option<&mut GridSector> {
        self.leaves.get_mut(key)
    }

    /// The leaf covering the world click `click`, of any level.
    pub fn leaf_at(&self, click: [i32; 2]) -> Option<(LeafKey, &GridSector)> {
        self.leaves.leaf_at(click)
    }

    /// Number of leaves of every level.
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    /// Whether `key` lies wholly inside the room rectangle.
    pub fn contains_leaf(&self, key: LeafKey) -> bool {
        let (lo, hi) = key.sector_range();
        self.local(lo).is_some() && self.local(hi).is_some()
    }

    /// Store a leaf of any level. The key must be aligned, disjoint from
    /// the other leaves and inside the rectangle (grow the room first with
    /// [`Self::grow_to_include`]); a leaf is never stored outside it.
    pub fn insert_leaf(
        &mut self,
        key: LeafKey,
        sector: GridSector,
    ) -> Result<Option<GridSector>, LeafStoreError> {
        if !self.contains_leaf(key) {
            return Err(LeafStoreError::OutsideRoom(key));
        }
        self.leaves
            .insert(key, sector)
            .map_err(LeafStoreError::Leaf)
    }

    /// Remove and return the leaf stored under exactly `key`.
    pub fn remove_leaf(&mut self, key: LeafKey) -> Option<GridSector> {
        self.leaves.remove(key)
    }

    /// Whether every leaf is a whole sector inside the rectangle, which is
    /// what the dense disk form holds.
    pub fn is_uniform(&self) -> bool {
        self.leaves
            .keys()
            .all(|key| key.is_sector() && self.local(key.cell()).is_some())
    }

    /// Move the whole room by `delta` sectors in the world: the origin and
    /// every leaf move together, so nothing changes in the room's own
    /// terms. (Assigning `origin` alone would leave the leaves where they
    /// are, because they are keyed in the world.) Returns `false` and moves
    /// nothing when a leaf larger than a sector would stop being aligned to
    /// its own lattice: level-0 leaves need a multiple of 4 sectors, level-1
    /// leaves a multiple of 2.
    pub fn translate(&mut self, delta: [i32; 2]) -> bool {
        if delta == [0, 0] {
            return true;
        }
        let aligned = self.leaves.keys().all(|key| {
            let clicks = delta.map(|d| d.saturating_mul(super::leaf::CLICKS_PER_SECTOR));
            clicks.iter().all(|c| c & (key.edge_clicks() - 1) == 0)
        });
        if !aligned {
            return false;
        }
        let moved: Vec<(LeafKey, GridSector)> = {
            let keys: Vec<LeafKey> = self.leaves.keys().collect();
            keys.into_iter()
                .filter_map(|k| Some((k, self.leaves.remove(k)?)))
                .collect()
        };
        for (key, sector) in moved {
            let key = LeafKey {
                level: key.level,
                x: key
                    .x
                    .saturating_add(delta[0] * super::leaf::CLICKS_PER_SECTOR),
                z: key
                    .z
                    .saturating_add(delta[1] * super::leaf::CLICKS_PER_SECTOR),
            };
            self.leaves
                .insert(key, sector)
                .expect("a rigid move keeps the leaves disjoint and aligned");
        }
        self.origin = [self.origin[0] + delta[0], self.origin[1] + delta[1]];
        true
    }

    /// Grow the room so that local `(x, z)` (possibly negative or past the
    /// edge) is inside it. Growing towards negative coordinates shifts
    /// `origin` so existing sectors keep their world position; the leaves
    /// are keyed in the world, so none of them moves.
    ///
    /// Returns how far existing local coordinates moved, `[dx, dz]`: add it
    /// to any local index you hold from before the call.
    pub fn grow_to_include(&mut self, x: i64, z: i64) -> [usize; 2] {
        let before_x = (-x).max(0) as usize;
        let before_z = (-z).max(0) as usize;
        let new_width = (x + 1).max(self.width as i64) as usize + before_x;
        let new_depth = (z + 1).max(self.depth as i64) as usize + before_z;
        if before_x == 0 && before_z == 0 && new_width == self.width && new_depth == self.depth {
            return [0, 0];
        }
        self.width = new_width;
        self.depth = new_depth;
        self.origin[0] -= before_x as i32;
        self.origin[1] -= before_z as i32;
        [before_x, before_z]
    }

    /// Drop empty leaves, then shrink the room to the bounding box of what
    /// is left (keeping at least one cell), moving `origin` so nothing
    /// shifts in the world. A leaf larger than a sector keeps its whole
    /// extent; the footprint of every plane and mesh counts as content.
    pub fn trim_empty(&mut self) {
        self.leaves.retain(|_, sector| !sector.is_empty());
        // Plane and mesh footprints are clamped back into the old rectangle
        // (the room never grows here); the leaves are inside it by
        // construction, and a stray one outside pulls the rectangle out to
        // hold it.
        let old_lo = self.origin;
        let old_hi = [
            self.origin[0] + self.width as i32 - 1,
            self.origin[1] + self.depth as i32 - 1,
        ];
        let clamp = |v: [i32; 2]| -> [i32; 2] {
            [
                v[0].clamp(old_lo[0], old_hi[0].max(old_lo[0])),
                v[1].clamp(old_lo[1], old_hi[1].max(old_lo[1])),
            ]
        };
        let mut bounds = self.leaves.sector_bounds();
        let mut take = |lo: [i32; 2], hi: [i32; 2]| {
            bounds = Some(match bounds {
                None => (lo, hi),
                Some((a, b)) => (
                    [a[0].min(lo[0]), a[1].min(lo[1])],
                    [b[0].max(hi[0]), b[1].max(hi[1])],
                ),
            });
        };
        for plane in &self.planes {
            let (lo, hi) = plane.cell_span();
            take(clamp(lo), clamp(hi));
        }
        for mesh in &self.meshes {
            let cell = clamp(mesh.center_cell());
            take(cell, cell);
        }
        let Some((min, max)) = bounds else {
            self.width = 1.min(self.width);
            self.depth = 1.min(self.depth);
            return;
        };
        self.width = (max[0] - min[0] + 1) as usize;
        self.depth = (max[1] - min[1] + 1) as usize;
        self.origin = min;
    }
}

/// Why a leaf could not be stored in a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafStoreError {
    /// The leaf is not wholly inside the room rectangle.
    OutsideRoom(LeafKey),
    /// The leaf map refused it (misaligned, or overlapping another leaf).
    Leaf(LeafError),
}

impl core::fmt::Display for LeafStoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutsideRoom(k) => write!(
                f,
                "leaf level {} at clicks ({}, {}) is not inside the room",
                k.level, k.x, k.z
            ),
            Self::Leaf(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for LeafStoreError {}

// ---- disk form ------------------------------------------------------------

/// One entry of the `leaves` list: the key spelled out, then the cell.
#[derive(Serialize, Deserialize)]
struct LeafEntry {
    level: u8,
    x: i32,
    z: i32,
    sector: GridSector,
}

/// Writes the dense sector list straight from the leaf map: `width * depth`
/// entries, `z` major.
struct DenseSectors<'a>(&'a GridRoom);

impl Serialize for DenseSectors<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let room = self.0;
        let mut seq = serializer.serialize_seq(Some(room.width * room.depth))?;
        for z in 0..room.depth {
            for x in 0..room.width {
                seq.serialize_element(&room.sector(x, z))?;
            }
        }
        seq.end()
    }
}

/// The leaves in Morton order as entries.
struct LeafList<'a>(&'a LeafMap);

impl Serialize for LeafList<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (key, sector) in self.0.iter() {
            seq.serialize_element(&LeafEntryRef {
                level: key.level,
                x: key.x,
                z: key.z,
                sector,
            })?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
#[serde(rename = "LeafEntry")]
struct LeafEntryRef<'a> {
    level: u8,
    x: i32,
    z: i32,
    sector: &'a GridSector,
}

#[derive(Deserialize)]
struct RoomDoc {
    #[serde(default)]
    name: String,
    #[serde(default)]
    origin: [i32; 2],
    #[serde(default)]
    y_offset: i32,
    width: usize,
    depth: usize,
    #[serde(default = "default_ambient")]
    ambient: [u8; 3],
    #[serde(default)]
    fog: GridFog,
    #[serde(default)]
    sectors: Vec<Option<GridSector>>,
    #[serde(default)]
    leaves: Vec<LeafEntry>,
    #[serde(default)]
    planes: Vec<GridPlane>,
    #[serde(default)]
    meshes: Vec<super::mesh::GridMesh>,
    #[serde(default)]
    wall_mode: WallMode,
    #[serde(default)]
    defaults: RoomDefaults,
    #[serde(default)]
    edge_overrides: EdgeOverrides,
    #[serde(default)]
    band_sets: Vec<BandSet>,
}

impl Serialize for GridRoom {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let dense = self.is_uniform();
        let fields = 6
            + usize::from(!self.fog.is_default())
            + 1 // sectors or leaves
            + usize::from(!self.planes.is_empty())
            + usize::from(!self.meshes.is_empty())
            + usize::from(!self.wall_mode.is_authored())
            + usize::from(!self.defaults.is_default())
            + usize::from(!self.edge_overrides.is_empty())
            + usize::from(!self.band_sets.is_empty());
        let mut out = serializer.serialize_struct("GridRoom", fields)?;
        out.serialize_field("name", &self.name)?;
        out.serialize_field("origin", &self.origin)?;
        out.serialize_field("y_offset", &self.y_offset)?;
        out.serialize_field("width", &self.width)?;
        out.serialize_field("depth", &self.depth)?;
        out.serialize_field("ambient", &self.ambient)?;
        if !self.fog.is_default() {
            out.serialize_field("fog", &self.fog)?;
        }
        if dense {
            out.serialize_field("sectors", &DenseSectors(self))?;
        } else {
            out.serialize_field("leaves", &LeafList(&self.leaves))?;
        }
        if !self.planes.is_empty() {
            out.serialize_field("planes", &self.planes)?;
        }
        if !self.meshes.is_empty() {
            out.serialize_field("meshes", &self.meshes)?;
        }
        if !self.wall_mode.is_authored() {
            out.serialize_field("wall_mode", &self.wall_mode)?;
        }
        if !self.defaults.is_default() {
            out.serialize_field("defaults", &self.defaults)?;
        }
        if !self.edge_overrides.is_empty() {
            out.serialize_field("edge_overrides", &self.edge_overrides)?;
        }
        if !self.band_sets.is_empty() {
            out.serialize_field("band_sets", &self.band_sets)?;
        }
        out.end()
    }
}

impl<'de> Deserialize<'de> for GridRoom {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let doc = RoomDoc::deserialize(deserializer)?;
        let mut room = Self {
            name: doc.name,
            origin: doc.origin,
            y_offset: doc.y_offset,
            width: doc.width,
            depth: doc.depth,
            ambient: doc.ambient,
            fog: doc.fog,
            leaves: LeafMap::new(),
            planes: doc.planes,
            meshes: doc.meshes,
            wall_mode: doc.wall_mode,
            defaults: doc.defaults,
            edge_overrides: doc.edge_overrides,
            band_sets: doc.band_sets,
        };
        // The dense list is `z` major over the declared size. A list of a
        // different length is read as far as it goes: an entry past the
        // declared depth lands outside the rectangle, where validation
        // reports it, and a short list is just empty sectors.
        let width = room.width.max(1);
        for (index, sector) in doc.sectors.into_iter().enumerate() {
            let Some(sector) = sector else { continue };
            let cell = [
                room.origin[0].saturating_add((index % width) as i32),
                room.origin[1].saturating_add((index / width) as i32),
            ];
            room.leaves
                .insert(LeafKey::sector(cell), sector)
                .map_err(D::Error::custom)?;
        }
        for entry in doc.leaves {
            let key = LeafKey {
                level: entry.level,
                x: entry.x,
                z: entry.z,
            };
            room.leaves
                .insert(key, entry.sector)
                .map_err(D::Error::custom)?;
        }
        Ok(room)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::GridSurface;

    fn floor_room() -> GridRoom {
        let mut room = GridRoom::new("hall", 2, 2);
        room.origin = [10, 20];
        for (x, z) in [(0, 0), (1, 1)] {
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(256 * (x as i32 + 1), None)));
        }
        room
    }

    #[test]
    fn sectors_are_addressed_by_local_coordinate_and_keyed_in_the_world() {
        let room = floor_room();
        assert_eq!(room.sector_key(1, 0), Some(LeafKey::sector([11, 20])));
        assert_eq!(room.sector_key(0, 1), Some(LeafKey::sector([10, 21])));
        assert_eq!(room.sector_key(2, 0), None);
        assert!(room.sector(1, 1).is_some());
        assert!(room.sector(1, 0).is_none());
        assert_eq!(room.sector_count(), 2);
    }

    #[test]
    fn world_and_local_cells_round_trip() {
        let room = floor_room();
        assert_eq!(room.world_cell(1, 1), [11, 21]);
        assert_eq!(room.local([11, 21]), Some([1, 1]));
        assert_eq!(room.local([9, 21]), None);
        assert_eq!(room.local([12, 20]), None);
    }

    #[test]
    fn growing_negative_keeps_sectors_in_place_in_the_world() {
        let mut room = floor_room();
        let shift = room.grow_to_include(-2, -1);
        assert_eq!(shift, [2, 1]);
        assert_eq!((room.width, room.depth), (4, 3));
        assert_eq!(room.origin, [8, 19]);
        // The sector that was at world [11, 21] is still there.
        let [x, z] = room.local([11, 21]).unwrap();
        assert_eq!(
            room.sector(x, z).unwrap().ground_floor().unwrap().heights[0],
            512
        );
        assert_eq!(room.sector_count(), 2);
    }

    #[test]
    fn growing_positive_leaves_origin_alone() {
        let mut room = floor_room();
        assert_eq!(room.grow_to_include(3, 4), [0, 0]);
        assert_eq!((room.width, room.depth, room.origin), (4, 5, [10, 20]));
        assert_eq!(room.leaf_count(), 2, "growing stores nothing");
        assert_eq!(room.grow_to_include(1, 1), [0, 0]);
    }

    #[test]
    fn trim_removes_empty_margins_without_moving_geometry() {
        let mut room = floor_room();
        room.grow_to_include(-2, -2);
        room.grow_to_include(6, 6);
        room.trim_empty();
        assert_eq!((room.width, room.depth), (2, 2));
        assert_eq!(room.origin, [10, 20]);
        assert_eq!(
            room.sector(1, 1).unwrap().ground_floor().unwrap().heights[0],
            512
        );
    }

    #[test]
    fn trim_treats_hollow_sectors_as_empty() {
        let mut room = GridRoom::new("r", 3, 3);
        room.sector_or_insert(0, 0);
        room.sector_or_insert(2, 2)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(0, None)));
        room.trim_empty();
        assert_eq!((room.width, room.depth), (1, 1));
        assert_eq!(room.origin, [2, 2]);
    }

    /// Rock roughness was removed from the editor. A project saved while it
    /// existed carries `rough` on a room and on its sectors: it must still
    /// load, and saving it again drops the data.
    #[test]
    fn an_old_room_with_rough_data_loads_and_saves_without_it() {
        let room = floor_room();
        let plain = ron::to_string(&room).unwrap();
        assert!(!plain.contains("rough"), "{plain}");
        let old = plain
            .replacen("(name:", "(rough:(seed:7,amp:(10,20,40),tint:30),name:", 1)
            .replacen("(floor:", "(rough:2,floor:", 1);
        assert_ne!(old, plain, "the fixture must carry rough data");
        assert!(
            old.contains("rough:(seed") && old.contains("rough:2"),
            "{old}"
        );
        let loaded: GridRoom = ron::from_str(&old).unwrap_or_else(|e| panic!("{old}: {e}"));
        assert_eq!(loaded, room);
        let saved = ron::to_string(&loaded).unwrap();
        assert!(!saved.contains("rough"), "{saved}");
        assert_eq!(saved, plain);
    }
}
