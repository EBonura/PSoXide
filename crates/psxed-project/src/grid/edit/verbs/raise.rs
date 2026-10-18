//! Raise and Lower: move the floor of a region up or down by an amount.
//!
//! Two modes. **Offset** moves every corner by the amount and keeps the
//! shape (a slope stays a slope). **Level** sets every corner to a reference
//! height plus the amount, so a lumpy region becomes one flat plateau. On a
//! flat region the two agree; that is the default there, Offset is the
//! default anywhere else. Corners on the region's border move with the region
//! and the sectors outside keep their heights, so a cliff appears and the
//! derived walls close it; nothing is welded outward (the one place this
//! differs from the Height tool's link behaviour).
//!
//! With `follow_ceiling` each ceiling moves by the same delta as the floor
//! corner under it (headroom stays constant). Without it a floor is held one
//! click under its ceiling and the held count is reported.

use crate::grid::auto_split;
use crate::grid::cook::RuntimeUnits;
use crate::grid::units::{CLICK_UNITS, FINE_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::region::Region;
use super::rules::{
    apply_floor_rule, ceiling_corners, clamp_under_ceiling, ensure_strata, ensure_trim,
    floor_class, floor_corners, move_ceiling_with_floor, set_floor_corners, stamp_wall_material,
};
use super::{Applied, Snapshot};

/// How the amount is applied to the region's corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiseMode {
    /// Every corner moves by the amount; the shape is kept.
    Offset,
    /// Every corner is set to the reference height plus the amount.
    Level,
}

impl RaiseMode {
    /// The other mode.
    pub const fn toggled(self) -> Self {
        match self {
            Self::Offset => Self::Level,
            Self::Level => Self::Offset,
        }
    }

    /// Lower-case label for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Offset => "Offset",
            Self::Level => "Level",
        }
    }
}

/// Parameters of a raise.
#[derive(Clone, Copy, Debug)]
pub struct RaiseParams {
    /// Offset or Level.
    pub mode: RaiseMode,
    /// Whether ceilings move with their floors.
    pub follow_ceiling: bool,
    /// The palette's material for the derived walls of the shaped sectors.
    pub wall_material: Option<ResourceId>,
    /// Room-relative height Level mode measures from (the height under the
    /// pointer at press).
    pub reference: i32,
}

/// The mode a region starts in: Level when every floor in it is flat at one
/// height (the two modes agree there), else Offset.
pub fn default_mode(world: &GridWorld, region: &Region) -> RaiseMode {
    let (cells, _) = region.with_floors(world);
    let Some(room) = world.rooms.get(region.room) else {
        return RaiseMode::Offset;
    };
    let mut level: Option<i32> = None;
    for cell in cells {
        let Some(floor) = room
            .local(cell)
            .and_then(|[x, z]| room.sector(x, z))
            .and_then(|s| s.floor())
        else {
            continue;
        };
        let h = floor.heights[0];
        let flat = floor.heights.iter().all(|v| *v == h)
            && floor.heights_b.is_none_or(|b| b.iter().all(|v| *v == h));
        if !flat || level.is_some_and(|l| l != h) {
            return RaiseMode::Offset;
        }
        level = Some(h);
    }
    if level.is_some() {
        RaiseMode::Level
    } else {
        RaiseMode::Offset
    }
}

/// The room-relative height of the floor of `cell` at its middle, snapped to
/// the fine step: what "the height under the pointer" means for Level mode.
pub fn height_under(world: &GridWorld, room: usize, cell: [i32; 2]) -> Option<i32> {
    let room = world.rooms.get(room)?;
    let [x, z] = room.local(cell)?;
    let floor = room.sector(x, z)?.floor()?;
    let sum: i32 = floor.heights.iter().sum();
    Some(crate::grid::units::snap_to(sum / 4, FINE_UNITS))
}

/// A started Raise/Lower gesture.
#[derive(Clone, Debug)]
pub struct RaiseDrag {
    snapshot: Snapshot,
    params: RaiseParams,
    skipped: usize,
    units: Option<RuntimeUnits>,
}

impl RaiseDrag {
    /// Start on `region`. `None` when the room is missing or no sector of the
    /// region has a floor.
    pub fn begin(world: &GridWorld, region: &Region, params: RaiseParams) -> Option<Self> {
        let (cells, skipped) = region.with_floors(world);
        if cells.is_empty() {
            return None;
        }
        Some(Self {
            snapshot: Snapshot::capture(world, region.room, &cells)?,
            params,
            skipped,
            units: RuntimeUnits::new(world.scale).ok(),
        })
    }

    /// The parameters, to read the mode back.
    pub fn params(&self) -> &RaiseParams {
        &self.params
    }

    /// Change the mode between applies (Tab).
    pub fn set_mode(&mut self, mode: RaiseMode) {
        self.params.mode = mode;
    }

    /// Number of sectors that will move.
    pub fn len(&self) -> usize {
        self.snapshot.cells().len()
    }

    /// Whether nothing would move.
    pub fn is_empty(&self) -> bool {
        self.snapshot.cells().is_empty()
    }

    /// Restore the captured sectors, then raise them by `amount` grid units
    /// (negative lowers). Safe to call any number of times, in any order.
    pub fn apply(&self, world: &mut GridWorld, amount: i32) -> Applied {
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
            let mut wanted = old;
            for tri in &mut wanted {
                for h in tri.iter_mut() {
                    *h = match params.mode {
                        RaiseMode::Offset => h.saturating_add(amount),
                        RaiseMode::Level => params.reference.saturating_add(amount),
                    };
                }
            }
            let mut held = false;
            if !params.follow_ceiling {
                if let Some(ceiling) = ceiling_corners(sector) {
                    let (clamped, was_held) = clamp_under_ceiling(ceiling, old, wanted);
                    wanted = clamped;
                    held = was_held;
                }
            }
            if let Some(floor) = sector.floor_mut() {
                set_floor_corners(floor, wanted);
                // Only a floor that moved chooses its diagonal again, so an
                // amount of zero leaves the document exactly as it was.
                if wanted != old {
                    auto_split::resplit(floor);
                }
            }
            if params.follow_ceiling {
                let deltas = [0, 1].map(|t| core::array::from_fn(|i| wanted[t][i] - old[t][i]));
                move_ceiling_with_floor(sector, deltas);
                if let Some(ceiling) = sector.ceiling_mut() {
                    if deltas != [[0; 4]; 2] {
                        auto_split::resplit(ceiling);
                    }
                }
            }
            apply_floor_rule(&rules, self.units.as_ref(), y_offset, before, sector);
            stamp_wall_material(sector, params.wall_material, &rules);
            applied.moved += usize::from(wanted != old);
            applied.clamped += usize::from(held);
        }
        if amount != 0 {
            ensure_strata(room);
            ensure_trim(room);
        }
        applied
    }
}

/// Raise `region` by `amount` in one step (a typed number, tests).
pub fn raise_region(
    world: &mut GridWorld,
    region: &Region,
    params: RaiseParams,
    amount: i32,
) -> Option<Applied> {
    let drag = RaiseDrag::begin(world, region, params)?;
    Some(drag.apply(world, amount))
}

/// The amount, in clicks, as the status line writes it.
pub fn clicks_text(amount: i32) -> String {
    format!("{:+.2}", amount as f32 / CLICK_UNITS as f32)
}
