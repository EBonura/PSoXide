//! One room: a rectangle of sectors with its own light and fog settings.

use serde::{Deserialize, Serialize};

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
/// Sectors are stored in a dense `width * depth` list, `z` major. Where the
/// room sits in the world is `origin` (in whole sectors, so rooms share one
/// lattice) plus `y_offset` (grid units) for a vertical shift. Rooms may
/// overlap in plan; that is how floors stack.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridRoom {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// World position of local sector `(0, 0)`, in sectors `[x, z]`.
    #[serde(default)]
    pub origin: [i32; 2],
    /// Vertical offset of the whole room, grid units.
    #[serde(default)]
    pub y_offset: i32,
    /// Sectors along X.
    pub width: usize,
    /// Sectors along Z.
    pub depth: usize,
    /// Ambient light, per channel.
    #[serde(default = "default_ambient")]
    pub ambient: [u8; 3],
    /// Distance fog.
    #[serde(default, skip_serializing_if = "GridFog::is_default")]
    pub fog: GridFog,
    /// Sector storage, `sectors[z * width + x]`. Length is `width * depth`;
    /// [`super::limits::validate`] reports a mismatch.
    pub sectors: Vec<Option<GridSector>>,
    /// Free planes the room owns. They draw and collide with the room and
    /// must lie inside its footprint to collide (editing grows the room).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planes: Vec<GridPlane>,
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
            sectors: vec![None; width * depth],
            planes: Vec::new(),
        }
    }

    /// Storage index of local `(x, z)`, or `None` outside the room or when
    /// the storage is shorter than the declared size.
    pub fn index(&self, x: usize, z: usize) -> Option<usize> {
        (x < self.width && z < self.depth)
            .then(|| z * self.width + x)
            .filter(|index| *index < self.sectors.len())
    }

    /// Sector at local `(x, z)`.
    pub fn sector(&self, x: usize, z: usize) -> Option<&GridSector> {
        self.sectors.get(self.index(x, z)?)?.as_ref()
    }

    /// Mutable sector at local `(x, z)`.
    pub fn sector_mut(&mut self, x: usize, z: usize) -> Option<&mut GridSector> {
        let index = self.index(x, z)?;
        self.sectors.get_mut(index)?.as_mut()
    }

    /// Sector at local `(x, z)`, created empty when absent. `None` only
    /// outside the room; use [`Self::grow_to_include`] first to extend it.
    pub fn sector_or_insert(&mut self, x: usize, z: usize) -> Option<&mut GridSector> {
        let index = self.index(x, z)?;
        Some(
            self.sectors
                .get_mut(index)?
                .get_or_insert_with(GridSector::default),
        )
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

    /// Number of sectors that exist.
    pub fn sector_count(&self) -> usize {
        self.sectors.iter().filter(|s| s.is_some()).count()
    }

    /// Every existing sector as `(x, z, sector)`.
    pub fn iter(&self) -> impl Iterator<Item = (usize, usize, &GridSector)> + '_ {
        let width = self.width.max(1);
        self.sectors
            .iter()
            .enumerate()
            .filter_map(move |(i, s)| Some((i % width, i / width, s.as_ref()?)))
    }

    /// Grow the room so that local `(x, z)` (possibly negative or past the
    /// edge) is inside it. Growing towards negative coordinates shifts
    /// `origin` so existing sectors keep their world position.
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
        let mut sectors = vec![None; new_width * new_depth];
        let occupied: Vec<(usize, usize)> = self.iter().map(|(x, z, _)| (x, z)).collect();
        for (ox, oz) in occupied {
            let from = oz * self.width + ox;
            sectors[(oz + before_z) * new_width + ox + before_x] = self.sectors[from].take();
        }
        self.sectors = sectors;
        self.width = new_width;
        self.depth = new_depth;
        self.origin[0] -= before_x as i32;
        self.origin[1] -= before_z as i32;
        [before_x, before_z]
    }

    /// Drop empty sectors, then shrink the room to the bounding box of what
    /// is left (keeping at least one cell), moving `origin` so nothing
    /// shifts in the world. The footprint of every plane counts as content.
    pub fn trim_empty(&mut self) {
        for slot in &mut self.sectors {
            if slot.as_ref().is_some_and(GridSector::is_empty) {
                *slot = None;
            }
        }
        let mut min = [usize::MAX; 2];
        let mut max = [0usize; 2];
        let mut any = false;
        for (x, z, _) in self.iter() {
            any = true;
            min = [min[0].min(x), min[1].min(z)];
            max = [max[0].max(x), max[1].max(z)];
        }
        for plane in &self.planes {
            let (lo, hi) = plane.cell_span();
            let clamp = |world: i32, axis: usize, limit: usize| {
                usize::try_from(world - self.origin[axis])
                    .unwrap_or(0)
                    .min(limit.saturating_sub(1))
            };
            any = true;
            min = [
                min[0].min(clamp(lo[0], 0, self.width)),
                min[1].min(clamp(lo[1], 1, self.depth)),
            ];
            max = [
                max[0].max(clamp(hi[0], 0, self.width)),
                max[1].max(clamp(hi[1], 1, self.depth)),
            ];
        }
        if !any {
            self.width = 1.min(self.width);
            self.depth = 1.min(self.depth);
            self.sectors.truncate(self.width * self.depth);
            return;
        }
        let (new_width, new_depth) = (max[0] - min[0] + 1, max[1] - min[1] + 1);
        let mut sectors = vec![None; new_width * new_depth];
        for z in min[1]..=max[1] {
            for x in min[0]..=max[0] {
                sectors[(z - min[1]) * new_width + (x - min[0])] =
                    self.sectors[z * self.width + x].take();
            }
        }
        self.sectors = sectors;
        self.width = new_width;
        self.depth = new_depth;
        self.origin[0] += min[0] as i32;
        self.origin[1] += min[1] as i32;
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
            room.sector_or_insert(x, z).unwrap().floor =
                Some(GridSurface::flat(256 * (x as i32 + 1), None));
        }
        room
    }

    #[test]
    fn indexing_is_z_major() {
        let room = floor_room();
        assert_eq!(room.index(1, 0), Some(1));
        assert_eq!(room.index(0, 1), Some(2));
        assert_eq!(room.index(2, 0), None);
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
            room.sector(x, z).unwrap().floor.as_ref().unwrap().heights[0],
            512
        );
        assert_eq!(room.sector_count(), 2);
    }

    #[test]
    fn growing_positive_leaves_origin_alone() {
        let mut room = floor_room();
        assert_eq!(room.grow_to_include(3, 4), [0, 0]);
        assert_eq!((room.width, room.depth, room.origin), (4, 5, [10, 20]));
        assert_eq!(room.sectors.len(), 20);
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
            room.sector(1, 1).unwrap().floor.as_ref().unwrap().heights[0],
            512
        );
    }

    #[test]
    fn trim_treats_hollow_sectors_as_empty() {
        let mut room = GridRoom::new("r", 3, 3);
        room.sector_or_insert(0, 0);
        room.sector_or_insert(2, 2).unwrap().floor = Some(GridSurface::flat(0, None));
        room.trim_empty();
        assert_eq!((room.width, room.depth), (1, 1));
        assert_eq!(room.origin, [2, 2]);
    }
}
