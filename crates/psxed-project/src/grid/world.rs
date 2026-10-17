//! The grid world document root and the scene's world-kind discriminator.

use serde::{Deserialize, Serialize};

use super::room::GridRoom;
use super::units::{GridScale, SECTOR_UNITS};

/// A grid world: rooms on one shared sector lattice and the scale that maps
/// it into the editor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridWorld {
    /// Grid units to editor units.
    #[serde(default)]
    pub scale: GridScale,
    /// Rooms, in authoring order.
    #[serde(default)]
    pub rooms: Vec<GridRoom>,
}

/// Axis-aligned box in editor units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorBounds {
    /// Smallest corner.
    pub min: [i32; 3],
    /// Largest corner.
    pub max: [i32; 3],
}

fn merge(bounds: Option<EditorBounds>, add: EditorBounds) -> EditorBounds {
    match bounds {
        None => add,
        Some(b) => EditorBounds {
            min: core::array::from_fn(|i| b.min[i].min(add.min[i])),
            max: core::array::from_fn(|i| b.max[i].max(add.max[i])),
        },
    }
}

impl GridWorld {
    /// An empty world at the default scale.
    pub fn new() -> Self {
        Self::default()
    }

    /// Editor units in one sector edge.
    pub fn sector_editor_units(&self) -> i32 {
        self.scale.sector_editor_units()
    }

    /// Editor-space position of a room's local corner `(0, 0)` at its
    /// vertical offset, `[x, y, z]`.
    pub fn room_origin_editor(&self, room: &GridRoom) -> [i32; 3] {
        [
            self.scale
                .to_editor(room.origin[0].saturating_mul(SECTOR_UNITS)),
            self.scale.to_editor(room.y_offset),
            self.scale
                .to_editor(room.origin[1].saturating_mul(SECTOR_UNITS)),
        ]
    }

    /// Every material the world's surfaces and walls use, once each, in
    /// first-use order.
    pub fn material_ids(&self) -> Vec<crate::ResourceId> {
        let mut ids = Vec::new();
        let mut note = |id: Option<crate::ResourceId>| {
            if let Some(id) = id {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        };
        for room in &self.rooms {
            for (_, _, sector) in room.iter() {
                for surface in [&sector.floor, &sector.ceiling].into_iter().flatten() {
                    note(surface.tri(0).material);
                    note(surface.tri(1).material);
                }
                for edge in super::sector::WallEdge::ALL {
                    for wall in sector.walls(edge) {
                        note(wall.material);
                    }
                }
            }
            for plane in &room.planes {
                note(plane.material);
            }
        }
        ids
    }

    /// Number of sectors over all rooms.
    pub fn sector_count(&self) -> usize {
        self.rooms.iter().map(GridRoom::sector_count).sum()
    }

    /// Height of the highest floor at editor position `(x, z)` that is not
    /// above `y_limit` (editor units), interpolated over the surface's
    /// triangles. `None` where no room has a floor there. The limit picks
    /// the right floor where rooms stack.
    ///
    /// The interpolation is [`psx_grid::height::height_at_i32`], the same
    /// function the runtime collision uses (there in runtime units), so the
    /// editor and the game stand on the same surface.
    pub fn floor_height_at(&self, x: i32, z: i32, y_limit: i32) -> Option<i32> {
        let (gx, gz) = (self.scale.to_grid(x), self.scale.to_grid(z));
        let cell = [gx.div_euclid(SECTOR_UNITS), gz.div_euclid(SECTOR_UNITS)];
        let edge = self.scale.sector_editor_units().max(1);
        let inv24 = psx_grid::height::weight_recip(edge as u32);
        let mut best: Option<i32> = None;
        for room in &self.rooms {
            let Some([lx, lz]) = room.local(cell) else {
                continue;
            };
            let Some(floor) = room.sector(lx, lz).and_then(|s| s.floor.as_ref()) else {
                continue;
            };
            // Offsets inside the sector, in editor units.
            let origin = [
                self.scale.to_editor(cell[0].saturating_mul(SECTOR_UNITS)),
                self.scale.to_editor(cell[1].saturating_mul(SECTOR_UNITS)),
            ];
            let off = |v: i32, o: i32| (v - o).clamp(0, edge) as u32;
            let (ox, oz) = (off(x, origin[0]), off(z, origin[1]));
            let nesw = matches!(floor.split, super::surface::GridSplit::NeSw);
            let second = !psx_grid::height::in_first_triangle(nesw, ox, oz, edge as u32);
            let corners = floor
                .heights_for(usize::from(second))
                .map(|h| self.scale.to_editor(room.y_offset + h));
            let y = psx_grid::height::height_at_i32(corners, nesw, ox, oz, edge as u32, inv24);
            if y <= y_limit && best.is_none_or(|b| y > b) {
                best = Some(y);
            }
        }
        best
    }

    /// Bounding box of every surface in editor units, or `None` for a world
    /// with no geometry. Used to frame the camera and size the depth range.
    pub fn editor_bounds(&self) -> Option<EditorBounds> {
        let mut bounds: Option<EditorBounds> = None;
        for room in &self.rooms {
            let origin = self.room_origin_editor(room);
            for (x, z, sector) in room.iter() {
                let Some((low, high)) = sector.height_range() else {
                    continue;
                };
                let x0 = self
                    .scale
                    .to_editor((room.origin[0] + x as i32) * SECTOR_UNITS);
                let z0 = self
                    .scale
                    .to_editor((room.origin[1] + z as i32) * SECTOR_UNITS);
                let x1 = self
                    .scale
                    .to_editor((room.origin[0] + x as i32 + 1) * SECTOR_UNITS);
                let z1 = self
                    .scale
                    .to_editor((room.origin[1] + z as i32 + 1) * SECTOR_UNITS);
                let y0 = origin[1] + self.scale.to_editor(low);
                let y1 = origin[1] + self.scale.to_editor(high);
                let cell = EditorBounds {
                    min: [x0, y0, z0],
                    max: [x1, y1, z1],
                };
                bounds = Some(merge(bounds, cell));
            }
            for plane in room.planes.iter().filter(|p| p.enabled) {
                let (lo, hi) = plane.bounds();
                let point = |v: [i32; 3]| {
                    [
                        self.scale.to_editor(v[0]),
                        origin[1] + self.scale.to_editor(v[1]),
                        self.scale.to_editor(v[2]),
                    ]
                };
                bounds = Some(merge(
                    bounds,
                    EditorBounds {
                        min: point(lo),
                        max: point(hi),
                    },
                ));
            }
        }
        bounds
    }
}

/// What a scene's world holds.
///
/// `Empty` is the default and the only value never written to disk, so a
/// scene with no sectors saves as it always has. `Grid` is the sector grid.
/// Files from the removed brush world carried `Brushes` here (it was never
/// written either), which the alias still reads as `Empty`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneWorld {
    /// No world authored yet.
    #[default]
    #[serde(alias = "Brushes")]
    Empty,
    /// Sector-grid world.
    Grid(GridWorld),
}

impl SceneWorld {
    /// Whether this is the default empty world.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// The grid world, if this is one.
    pub fn grid(&self) -> Option<&GridWorld> {
        match self {
            Self::Grid(grid) => Some(grid),
            Self::Empty => None,
        }
    }

    /// Mutable grid world, if this is one.
    pub fn grid_mut(&mut self) -> Option<&mut GridWorld> {
        match self {
            Self::Grid(grid) => Some(grid),
            Self::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::{GridSurface, GridWall};

    #[test]
    fn bounds_cover_surfaces_and_walls_in_editor_units() {
        let mut world = GridWorld::new();
        let mut room = GridRoom::new("a", 2, 1);
        room.origin = [1, -1];
        room.y_offset = 512;
        room.sector_or_insert(1, 0).unwrap().floor = Some(GridSurface::flat(256, None));
        room.sector_or_insert(1, 0)
            .unwrap()
            .north
            .push(GridWall::span(256, 2048, None));
        world.rooms.push(room);
        let b = world.editor_bounds().unwrap();
        // x: sector 2..3 at 1280 per sector; y: (512 + 256)..(512 + 2048)
        // scaled by 5/4; z: sector -1..0.
        assert_eq!(b.min, [2560, 960, -1280]);
        assert_eq!(b.max, [3840, 3200, 0]);
        assert_eq!(GridWorld::new().editor_bounds(), None);
    }

    #[test]
    fn floor_height_interpolates_both_triangles_and_respects_the_limit() {
        use crate::grid::surface::GridSplit;
        let mut room = GridRoom::new("a", 1, 1);
        let mut slope = GridSurface::flat(0, None);
        slope.heights = [0, 0, 1024, 1024]; // rises towards +z
        slope.split = GridSplit::NwSe;
        room.sector_or_insert(0, 0).unwrap().floor = Some(slope);
        let mut upper = GridRoom::new("b", 1, 1);
        upper.y_offset = 2048;
        upper.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room, upper],
        };
        // (512, 256) lies in the first triangle, (100, 900) in the second.
        assert_eq!(world.floor_height_at(512, 256, 10_000), Some(2048));
        assert_eq!(world.floor_height_at(512, 256, 1000), Some(256));
        assert_eq!(world.floor_height_at(100, 900, 1000), Some(900));
        assert_eq!(world.floor_height_at(5000, 0, 1000), None);
        assert_eq!(world.floor_height_at(512, 256, -5), None);
    }

    #[test]
    fn room_origin_applies_scale_and_offset() {
        let mut world = GridWorld::new();
        let mut room = GridRoom::new("a", 1, 1);
        room.origin = [-2, 3];
        room.y_offset = 1024;
        assert_eq!(world.room_origin_editor(&room), [-2560, 1280, 3840]);
        world.scale = GridScale { num: 9, den: 16 };
        assert_eq!(world.room_origin_editor(&room), [-1152, 576, 1728]);
    }

    #[test]
    fn only_the_empty_default_skips_serialisation() {
        assert!(SceneWorld::default().is_empty());
        assert!(!SceneWorld::Grid(GridWorld::new()).is_empty());
        assert!(SceneWorld::Empty.grid().is_none());
    }
}
