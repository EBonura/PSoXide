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

    /// Number of sectors over all rooms.
    pub fn sector_count(&self) -> usize {
        self.rooms.iter().map(GridRoom::sector_count).sum()
    }

    /// Height of the highest floor at editor position `(x, z)` that is not
    /// above `y_limit` (editor units), interpolated over the surface's
    /// triangles. `None` where no room has a floor there. The limit picks
    /// the right floor where rooms stack.
    pub fn floor_height_at(&self, x: i32, z: i32, y_limit: i32) -> Option<i32> {
        let (gx, gz) = (self.scale.to_grid(x), self.scale.to_grid(z));
        let mut best: Option<i32> = None;
        for room in &self.rooms {
            let cell = [gx.div_euclid(SECTOR_UNITS), gz.div_euclid(SECTOR_UNITS)];
            let Some([lx, lz]) = room.local(cell) else {
                continue;
            };
            let Some(floor) = room.sector(lx, lz).and_then(|s| s.floor.as_ref()) else {
                continue;
            };
            let p = [
                i64::from(gx.rem_euclid(SECTOR_UNITS)),
                i64::from(gz.rem_euclid(SECTOR_UNITS)),
            ];
            let s = i64::from(SECTOR_UNITS);
            let corners = [[0, 0], [s, 0], [s, s], [0, s]];
            // The triangle containing `p`: the split diagonal decides.
            let on_first = match floor.split {
                super::surface::GridSplit::NwSe => p[0] >= p[1],
                super::surface::GridSplit::NeSw => p[0] + p[1] <= s,
            };
            let index = usize::from(!on_first);
            let heights = floor.heights_for(index);
            let [a, b, c] = floor.split.triangle(index);
            let (pa, pb, pc) = (corners[a], corners[b], corners[c]);
            let det = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (pb[1] - pa[1]);
            if det == 0 {
                continue;
            }
            let wb = (p[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (p[1] - pa[1]);
            let wc = (pb[0] - pa[0]) * (p[1] - pa[1]) - (p[0] - pa[0]) * (pb[1] - pa[1]);
            let h = i64::from(heights[a])
                + ((i64::from(heights[b]) - i64::from(heights[a])) * wb
                    + (i64::from(heights[c]) - i64::from(heights[a])) * wc)
                    / det;
            let y = self.scale.to_editor(room.y_offset + h as i32);
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
                bounds = Some(match bounds {
                    None => cell,
                    Some(b) => EditorBounds {
                        min: core::array::from_fn(|i| b.min[i].min(cell.min[i])),
                        max: core::array::from_fn(|i| b.max[i].max(cell.max[i])),
                    },
                });
            }
        }
        bounds
    }
}

/// What kind of world a scene holds.
///
/// `Brushes` is the BSP brush world every existing project has, stored in
/// [`Scene::brushes`](crate::Scene::brushes). `Grid` is the sector-grid
/// world. The default, and the only value that is never written to disk, is
/// `Brushes`, so a project saved before this field existed loads and saves
/// exactly as it did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneWorld {
    /// Convex brush (BSP) world.
    #[default]
    Brushes,
    /// Sector-grid world.
    Grid(GridWorld),
}

impl SceneWorld {
    /// Whether this is the default brush world.
    pub fn is_brushes(&self) -> bool {
        matches!(self, Self::Brushes)
    }

    /// The grid world, if this is one.
    pub fn grid(&self) -> Option<&GridWorld> {
        match self {
            Self::Grid(grid) => Some(grid),
            Self::Brushes => None,
        }
    }

    /// Mutable grid world, if this is one.
    pub fn grid_mut(&mut self) -> Option<&mut GridWorld> {
        match self {
            Self::Grid(grid) => Some(grid),
            Self::Brushes => None,
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
        // x: sector 2..3 at 1344 per sector; y: (512 + 256)..(512 + 2048)
        // scaled by 21/16; z: sector -1..0.
        assert_eq!(b.min, [2688, 1008, -1344]);
        assert_eq!(b.max, [4032, 3360, 0]);
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
        assert_eq!(world.room_origin_editor(&room), [-2688, 1344, 4032]);
        world.scale = GridScale { num: 9, den: 16 };
        assert_eq!(world.room_origin_editor(&room), [-1152, 576, 1728]);
    }

    #[test]
    fn only_the_brush_default_skips_serialisation() {
        assert!(SceneWorld::default().is_brushes());
        assert!(!SceneWorld::Grid(GridWorld::new()).is_brushes());
        assert!(SceneWorld::Brushes.grid().is_none());
    }
}
