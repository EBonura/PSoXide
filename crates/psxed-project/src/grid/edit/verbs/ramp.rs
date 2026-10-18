//! Ramp: a slope from one side of the region to the opposite side.
//!
//! The region's bounding box has four sides. The **A edge** keeps its height
//! (the mean of the floor corners on it, snapped to the fine step); the
//! opposite **B edge** rises by the amount; every corner in between gets
//! `hA + rise * t` with `t` its distance from A over the box depth, rounded
//! to the fine step. A region that is not a rectangle ramps by position, so an
//! L shape slopes as one plane. Planar over whole sectors, so the diagonal
//! never matters; rounding to the fine step can bend a long ramp a little,
//! and an auto-split floor then picks its level diagonal.
//!
//! The steepest walkable slope is read from the same rule the cook and the
//! character motor use ([`psx_engine::WalkLimits`]); [`RampDrag::max_walkable_rise`]
//! finds the largest rise that keeps every floor a body can stand on.

use psx_engine::WalkLimits;

use crate::grid::auto_split;
use crate::grid::cook::RuntimeUnits;
use crate::grid::slope::{surface_class, SlopeClass};
use crate::grid::surface::GridSurface;
use crate::grid::units::{snap_to, FINE_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::region::Region;
use super::rules::{
    apply_floor_rule, ceiling_corners, ensure_strata, ensure_trim, floor_class, floor_corners,
    move_ceiling_with_floor, set_floor_corners, stamp_wall_material,
};
use super::{Applied, Snapshot};

/// A side of the region's bounding box, used for the A edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RampDir {
    /// The `-Z` side.
    North,
    /// The `+X` side.
    East,
    /// The `+Z` side.
    South,
    /// The `-X` side.
    West,
}

impl RampDir {
    /// Every side, clockwise from north.
    pub const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// The opposite side (the B edge).
    pub const fn opposite(self) -> Self {
        match self {
            Self::North => Self::South,
            Self::East => Self::West,
            Self::South => Self::North,
            Self::West => Self::East,
        }
    }

    /// A quarter turn clockwise (R).
    pub const fn turned(self) -> Self {
        match self {
            Self::North => Self::East,
            Self::East => Self::South,
            Self::South => Self::West,
            Self::West => Self::North,
        }
    }

    /// Lower-case label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::North => "north",
            Self::East => "east",
            Self::South => "south",
            Self::West => "west",
        }
    }

    /// Whether the slope runs along X (A is east or west).
    pub const fn along_x(self) -> bool {
        matches!(self, Self::East | Self::West)
    }
}

/// Parameters of a ramp.
#[derive(Clone, Copy, Debug)]
pub struct RampParams {
    /// The side that keeps its height.
    pub from: RampDir,
    /// Whether ceilings move with their floors.
    pub follow_ceiling: bool,
    /// The palette's material for the derived walls of the shaped sectors.
    pub wall_material: Option<ResourceId>,
}

/// Lattice distance, in sectors, of the corner `(cx, cz)` from the A edge of
/// a box `[x0..=x1] x [z0..=z1]`, and the box depth along A to B.
fn distance_from_a(from: RampDir, bounds: ([i32; 2], [i32; 2]), corner: [i32; 2]) -> (i32, i32) {
    let ([x0, z0], [x1, z1]) = bounds;
    match from {
        RampDir::North => (corner[1] - z0, z1 - z0 + 1),
        RampDir::South => (z1 + 1 - corner[1], z1 - z0 + 1),
        RampDir::West => (corner[0] - x0, x1 - x0 + 1),
        RampDir::East => (x1 + 1 - corner[0], x1 - x0 + 1),
    }
}

/// `rise * k / depth`, rounded half away from zero, then to the fine step.
fn rise_at(rise: i32, k: i32, depth: i32) -> i32 {
    let num = i64::from(rise) * i64::from(k);
    let den = i64::from(depth.max(1));
    let half = den / 2;
    let rounded = if num >= 0 {
        (num + half) / den
    } else {
        -((-num + half) / den)
    };
    snap_to(rounded as i32, FINE_UNITS)
}

/// Corner heights of the cell at `cell` for the ramp: `[NW, NE, SE, SW]`.
fn ramp_heights(
    from: RampDir,
    bounds: ([i32; 2], [i32; 2]),
    base: i32,
    rise: i32,
    cell: [i32; 2],
) -> [i32; 4] {
    let corner = |dx: i32, dz: i32| {
        let (k, depth) = distance_from_a(from, bounds, [cell[0] + dx, cell[1] + dz]);
        base + rise_at(rise, k, depth)
    };
    [corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1)]
}

/// A started Ramp gesture.
#[derive(Clone, Debug)]
pub struct RampDrag {
    snapshot: Snapshot,
    params: RampParams,
    bounds: ([i32; 2], [i32; 2]),
    base: i32,
    skipped: usize,
    units: Option<RuntimeUnits>,
    y_offset: i32,
}

/// The mean height of the floor corners lying on the A edge of `bounds`,
/// snapped to the fine step: the height the ramp starts from.
fn edge_height(
    world: &GridWorld,
    room: usize,
    cells: &[[i32; 2]],
    from: RampDir,
    bounds: ([i32; 2], [i32; 2]),
) -> i32 {
    let Some(r) = world.rooms.get(room) else {
        return 0;
    };
    let (mut sum, mut n) = (0i64, 0i64);
    for &cell in cells {
        let Some(floor) = r
            .local(cell)
            .and_then(|[x, z]| r.sector(x, z))
            .and_then(|s| s.ground_floor())
        else {
            continue;
        };
        for (i, (dx, dz)) in [(0, 0), (1, 0), (1, 1), (0, 1)].into_iter().enumerate() {
            let (k, _) = distance_from_a(from, bounds, [cell[0] + dx, cell[1] + dz]);
            if k == 0 {
                sum += i64::from(floor.heights[i]);
                n += 1;
            }
        }
    }
    if n == 0 {
        return 0;
    }
    snap_to((sum / n) as i32, FINE_UNITS)
}

impl RampDrag {
    /// Start on `region` with the A edge on side `params.from`. `None` when
    /// no sector of the region has a floor.
    pub fn begin(world: &GridWorld, region: &Region, params: RampParams) -> Option<Self> {
        let (cells, skipped) = region.with_floors(world);
        let b = region.bounds()?;
        if cells.is_empty() {
            return None;
        }
        let bounds = (b.min, b.max);
        Some(Self {
            snapshot: Snapshot::capture(world, region.room, &cells)?,
            params,
            bounds,
            base: edge_height(world, region.room, &cells, params.from, bounds),
            skipped,
            units: RuntimeUnits::new(world.scale).ok(),
            y_offset: world.rooms.get(region.room).map_or(0, |r| r.y_offset),
        })
    }

    /// The parameters.
    pub fn params(&self) -> &RampParams {
        &self.params
    }

    /// Height of the A edge, room-relative grid units.
    pub fn base(&self) -> i32 {
        self.base
    }

    /// Depth of the region from the A edge to the B edge, in sectors.
    pub fn depth(&self) -> i32 {
        let ([x0, z0], [x1, z1]) = self.bounds;
        if self.params.from.along_x() {
            x1 - x0 + 1
        } else {
            z1 - z0 + 1
        }
    }

    /// The bounding box of the region, `(min, max)` world sectors.
    pub fn bounds(&self) -> ([i32; 2], [i32; 2]) {
        self.bounds
    }

    /// Number of sectors that will take the slope.
    pub fn len(&self) -> usize {
        self.snapshot.cells().len()
    }

    /// Whether nothing would change.
    pub fn is_empty(&self) -> bool {
        self.snapshot.cells().is_empty()
    }

    /// Whether `rise` leaves every floor of the ramp one a body can stand
    /// on (no steep triangle).
    fn walkable(&self, rise: i32) -> bool {
        let Some(units) = self.units.as_ref() else {
            return true;
        };
        let limits = WalkLimits::DEFAULT;
        self.snapshot.cells().iter().all(|(cell, original)| {
            let Some(original) = original else {
                return true;
            };
            let mut surface: GridSurface = match original.ground_floor().cloned() {
                Some(f) => f,
                None => return true,
            };
            surface.heights = ramp_heights(self.params.from, self.bounds, self.base, rise, *cell);
            surface.heights_b = None;
            surface.split = auto_split::best_split(surface.heights, surface.split);
            surface_class(units, self.y_offset, &surface, &limits) != SlopeClass::Steep
        })
    }

    /// The largest rise (grid units, a multiple of the fine step, positive)
    /// that keeps every floor of the ramp walkable. Read from the same normal
    /// the cook and the motor use; the room's height offset does not matter
    /// for a slope.
    pub fn max_walkable_rise(&self) -> i32 {
        // Walkability only gets worse as the rise grows, so scan up from 0.
        let mut rise = 0;
        let cap = FINE_UNITS * 4096;
        while rise + FINE_UNITS <= cap && self.walkable(rise + FINE_UNITS) {
            rise += FINE_UNITS;
        }
        rise
    }

    /// Restore, then slope the region by `rise` (negative slopes down).
    pub fn apply(&self, world: &mut GridWorld, rise: i32) -> Applied {
        self.snapshot.restore(world);
        let mut applied = Applied {
            skipped: self.skipped,
            ..Applied::default()
        };
        let params = self.params;
        let Some(room) = world.rooms.get_mut(self.snapshot.room()) else {
            return applied;
        };
        let rules = room.defaults.rules;
        let y_offset = room.y_offset;
        for (cell, original) in self.snapshot.cells() {
            let Some(original) = original else { continue };
            let Some(old) = floor_corners(original) else {
                continue;
            };
            let Some([x, z]) = room.local(*cell) else {
                continue;
            };
            let Some(sector) = room.sector_mut(x, z) else {
                continue;
            };
            let before = floor_class(self.units.as_ref(), y_offset, original);
            let h = ramp_heights(params.from, self.bounds, self.base, rise, *cell);
            let wanted = [h, h];
            if let Some(floor) = sector.ground_floor_mut() {
                set_floor_corners(floor, wanted);
                // A split-level floor becomes one plane under a ramp.
                floor.heights_b = None;
                if wanted != old {
                    auto_split::resplit(floor);
                }
            }
            if params.follow_ceiling {
                let deltas = [0, 1].map(|t| core::array::from_fn(|i| wanted[t][i] - old[t][i]));
                move_ceiling_with_floor(sector, deltas);
                if let Some(ceiling) = sector.ground_ceiling_mut() {
                    if deltas != [[0; 4]; 2] {
                        auto_split::resplit(ceiling);
                    }
                }
            } else if let Some(ceiling) = ceiling_corners(sector) {
                // Hold a floor a click under the ceiling it keeps.
                let (clamped, held) = super::rules::clamp_under_ceiling(ceiling, old, wanted);
                if held {
                    if let Some(floor) = sector.ground_floor_mut() {
                        set_floor_corners(floor, clamped);
                        auto_split::resplit(floor);
                    }
                    applied.clamped += 1;
                }
            }
            apply_floor_rule(&rules, self.units.as_ref(), y_offset, before, sector);
            stamp_wall_material(sector, params.wall_material, &rules);
            applied.moved += usize::from(wanted != old);
        }
        if rise != 0 {
            ensure_strata(room);
            ensure_trim(room);
        }
        applied
    }
}

/// Slope `region` in one step (a typed amount, tests).
pub fn ramp_region(
    world: &mut GridWorld,
    region: &Region,
    params: RampParams,
    rise: i32,
) -> Option<Applied> {
    let drag = RampDrag::begin(world, region, params)?;
    Some(drag.apply(world, rise))
}
