//! Stairs v1: a flight of whole-sector treads along a path.
//!
//! The path is an ordered, four-connected run of sectors (waypoints joined as
//! in [`Region::path_cells`]), widened sideways by a width. Each sector of
//! the path is one tread; a sector where the path turns is a **landing**, flat
//! at the height of the tread before it. The first tread stands one riser
//! above the floor at the start of the path, so the sector before the path is
//! the bottom step.
//!
//! The riser comes from the **riser option**: one click (the default, half the
//! climb limit), the largest riser the motor steps up ([`WalkLimits::step_up`]
//! read through the world's scale), or a typed value, never above that limit.
//! The number of steps is `round(rise / riser)`, at least one and at most the
//! number of treads the path holds; the riser is then `rise / steps` snapped to
//! the fine step, and the rest of the path is a flat landing at the top. If
//! even the shortest riser the path allows is above the limit the verb
//! **refuses** (too steep for treads: use a ramp or lengthen the path) and
//! writes nothing.
//!
//! The **ramp** option slopes every tread along its direction instead, from the
//! height of its entry edge to its exit edge: a flight of the same shape, with
//! no steps.
//!
//! Floors are made flat where the path crosses sectors that have none (inside
//! the room; the verb never grows it). Ceilings follow or are held a click
//! under, as for Raise; walls come from derivation.

use psx_engine::WalkLimits;

use crate::grid::auto_split;
use crate::grid::cook::RuntimeUnits;
use crate::grid::surface::GridSurface;
use crate::grid::units::{snap_to, CLICK_UNITS, FINE_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::region::Region;
use super::rules::{
    apply_floor_rule, ceiling_corners, clamp_under_ceiling, ensure_strata, ensure_trim,
    floor_class, floor_corners, move_ceiling_with_floor, set_floor_corners, stamp_wall_material,
};
use super::{Applied, Snapshot};

/// How the riser is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiserOption {
    /// One click (256 grid units).
    Click,
    /// The tallest riser the motor steps up.
    Max,
    /// A typed riser in grid units, held to the limit.
    Typed(i32),
}

/// Parameters of a flight.
#[derive(Clone, Copy, Debug)]
pub struct StairsParams {
    /// Riser choice.
    pub riser: RiserOption,
    /// Slope every tread instead of stepping.
    pub ramp: bool,
    /// Width of the flight in sectors.
    pub width: i32,
    /// Whether ceilings move with their floors.
    pub follow_ceiling: bool,
    /// The palette's material for the derived walls of the shaped sectors.
    pub wall_material: Option<ResourceId>,
}

impl Default for StairsParams {
    fn default() -> Self {
        Self {
            riser: RiserOption::Click,
            ramp: false,
            width: 1,
            follow_ceiling: true,
            wall_material: None,
        }
    }
}

/// Why a flight was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No sector of the path has a floor to start from.
    NoFloor,
    /// The run is too short for treads at this rise: the riser would pass
    /// the motor's step-up.
    TooSteep {
        /// The riser the shortest allowed count would need, grid units.
        riser: i32,
        /// The tallest riser, grid units.
        limit: i32,
    },
}

/// What a flight came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flight {
    /// Steps up (or down), landings not counted.
    pub steps: usize,
    /// Riser of each step, grid units (negative going down).
    pub riser: i32,
    /// Height reached at the top, relative to the start floor.
    pub total: i32,
    /// Sectors on the path (treads and landings).
    pub cells: usize,
    /// Why nothing was written.
    pub refusal: Option<Refusal>,
    /// What the verb touched.
    pub applied: Applied,
}

/// A path cell with its place in the flight.
#[derive(Clone, Copy, Debug)]
struct Tread {
    cell: [i32; 2],
    /// Step index it stands at (landings repeat the previous index).
    landing: bool,
    /// Unit direction of travel through it.
    dir: [i32; 2],
}

/// The ordered treads of a path.
fn treads(path: &[[i32; 2]]) -> Vec<Tread> {
    let dir_of = |a: [i32; 2], b: [i32; 2]| [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
    path.iter()
        .enumerate()
        .map(|(i, &cell)| {
            let incoming = i.checked_sub(1).map(|p| dir_of(path[p], cell));
            let outgoing = path.get(i + 1).map(|n| dir_of(cell, *n));
            let dir = outgoing.or(incoming).unwrap_or([1, 0]);
            // A turn is a landing; the first and last sector are treads.
            let landing = matches!((incoming, outgoing), (Some(a), Some(b)) if a != b);
            Tread { cell, landing, dir }
        })
        .collect()
}

/// The largest riser, grid units and a multiple of the fine step, whose
/// runtime height the motor still steps up.
pub fn max_riser(units: &RuntimeUnits, limits: &WalkLimits) -> i32 {
    let mut riser = 0;
    while units.rt(riser + FINE_UNITS) <= limits.step_up && riser < FINE_UNITS * 4096 {
        riser += FINE_UNITS;
    }
    riser
}

/// A started Stairs gesture.
#[derive(Clone, Debug)]
pub struct StairsDrag {
    snapshot: Snapshot,
    params: StairsParams,
    treads: Vec<Tread>,
    /// Which sectors each tread widens to, by tread index.
    cells: Vec<([i32; 2], usize)>,
    base: i32,
    has_floor: bool,
    skipped: usize,
    units: Option<RuntimeUnits>,
    limit: i32,
    y_offset: i32,
    room: usize,
}

impl StairsDrag {
    /// Start on `path` (waypoints, world sectors) in `room`. `None` when the
    /// room is missing or the path crosses no sector of it.
    pub fn begin(
        world: &GridWorld,
        room: usize,
        path: &[[i32; 2]],
        params: StairsParams,
    ) -> Option<Self> {
        let target = world.rooms.get(room)?;
        let ordered = Region::path_cells(path);
        let treads = treads(&ordered);
        if treads.is_empty() {
            return None;
        }
        // Widen sideways, remembering which tread each sector belongs to
        // (the first, on overlap).
        let width = params.width.max(1);
        let (before, after) = ((width - 1) / 2, width / 2);
        let mut cells: Vec<([i32; 2], usize)> = Vec::new();
        for (index, t) in treads.iter().enumerate() {
            let across = if t.dir[0] != 0 { [0, 1] } else { [1, 0] };
            for k in -before..=after {
                let cell = [t.cell[0] + across[0] * k, t.cell[1] + across[1] * k];
                if !cells.iter().any(|(c, _)| *c == cell) {
                    cells.push((cell, index));
                }
            }
        }
        let inside: Vec<[i32; 2]> = cells
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| target.local(*c).is_some())
            .collect();
        let skipped = cells.len() - inside.len();
        let units = RuntimeUnits::new(world.scale).ok();
        let limit = units
            .as_ref()
            .map_or(CLICK_UNITS * 2, |u| max_riser(u, &WalkLimits::DEFAULT));
        // The start height: the first path sector that has a floor.
        let base = treads.iter().find_map(|t| {
            let [x, z] = target.local(t.cell)?;
            let floor = target.sector(x, z)?.floor()?;
            Some(snap_to(floor.heights.iter().sum::<i32>() / 4, FINE_UNITS))
        });
        Some(Self {
            snapshot: Snapshot::capture(world, room, &inside)?,
            params,
            treads,
            cells,
            base: base.unwrap_or(0),
            has_floor: base.is_some(),
            skipped,
            units,
            limit,
            y_offset: target.y_offset,
            room,
        })
    }

    /// The parameters.
    pub fn params(&self) -> &StairsParams {
        &self.params
    }

    /// Change the parameters between applies (the riser option, ramp).
    pub fn set_params(&mut self, params: StairsParams) {
        self.params = params;
    }

    /// The tallest riser the motor steps up, grid units.
    pub fn limit(&self) -> i32 {
        self.limit
    }

    /// The sectors of the flight with the tread each belongs to and whether
    /// it is a landing, for the ghost.
    pub fn ghost(&self) -> Vec<([i32; 2], usize, bool)> {
        self.cells
            .iter()
            .map(|(cell, index)| (*cell, *index, self.treads[*index].landing))
            .collect()
    }

    /// How a flight of `rise` would come out, without touching anything.
    pub fn plan(&self, rise: i32) -> Flight {
        let n_treads = self.treads.iter().filter(|t| !t.landing).count().max(1);
        let sign = if rise < 0 { -1 } else { 1 };
        let magnitude = rise.abs();
        let mut flight = Flight {
            cells: self.treads.len(),
            ..Flight::default()
        };
        if magnitude == 0 {
            return flight;
        }
        if !self.has_floor && self.cells.len() == self.skipped {
            flight.refusal = Some(Refusal::NoFloor);
            return flight;
        }
        let wanted = match self.params.riser {
            RiserOption::Click => CLICK_UNITS,
            RiserOption::Max => self.limit,
            RiserOption::Typed(v) => v.clamp(FINE_UNITS, self.limit.max(FINE_UNITS)),
        }
        .max(FINE_UNITS);
        let steps = (((magnitude + wanted / 2) / wanted).max(1) as usize).min(n_treads);
        let riser = snap_to(magnitude / steps as i32, FINE_UNITS).max(FINE_UNITS);
        if riser > self.limit && !self.params.ramp {
            flight.refusal = Some(Refusal::TooSteep {
                riser,
                limit: self.limit,
            });
            return flight;
        }
        flight.steps = steps;
        flight.riser = riser * sign;
        flight.total = riser * sign * steps as i32;
        flight
    }

    /// Restore, then lay the flight for `rise` (negative goes down). A
    /// refusal leaves the document as it began.
    pub fn apply(&self, world: &mut GridWorld, rise: i32) -> Flight {
        self.snapshot.restore(world);
        let mut flight = self.plan(rise);
        flight.applied.skipped = self.skipped;
        if flight.refusal.is_some() || flight.steps == 0 {
            return flight;
        }
        let params = self.params;
        // Tread heights, relative to the start floor: landings repeat.
        let mut heights = Vec::with_capacity(self.treads.len());
        let mut step = 0usize;
        for t in &self.treads {
            if !t.landing && step < flight.steps {
                step += 1;
            }
            heights.push(self.base + flight.riser * step as i32);
        }
        let Some(room) = world.rooms.get_mut(self.room) else {
            return flight;
        };
        let rules = room.defaults.rules;
        for (cell, index) in &self.cells {
            let Some([x, z]) = room.local(*cell) else {
                continue;
            };
            let t = self.treads[*index];
            let top = heights[*index];
            let bottom = if *index == 0 {
                self.base
            } else {
                heights[*index - 1]
            };
            let corners = if params.ramp && !t.landing {
                ramp_corners(t.dir, bottom, top)
            } else {
                [top; 4]
            };
            let existed = room.sector(x, z).is_some_and(|s| s.floor().is_some());
            let Some(sector) = room.sector_or_insert(x, z) else {
                continue;
            };
            let before = floor_class(
                self.units.as_ref(),
                self.y_offset,
                &self.original(*cell).unwrap_or_default(),
            );
            let old = floor_corners(sector);
            if !existed {
                sector.set_floor(Some(GridSurface::flat_auto(corners[0], None)));
                flight.applied.created += 1;
            }
            let old = old.unwrap_or([corners; 2]);
            let wanted = [corners, corners];
            if let Some(floor) = sector.floor_mut() {
                set_floor_corners(floor, wanted);
                floor.heights_b = None;
                if wanted != old {
                    auto_split::resplit(floor);
                }
            }
            if params.follow_ceiling {
                let deltas = [0, 1].map(|k| core::array::from_fn(|i| wanted[k][i] - old[k][i]));
                move_ceiling_with_floor(sector, deltas);
            } else if let Some(ceiling) = ceiling_corners(sector) {
                let (clamped, held) = clamp_under_ceiling(ceiling, old, wanted);
                if held {
                    if let Some(floor) = sector.floor_mut() {
                        set_floor_corners(floor, clamped);
                    }
                    flight.applied.clamped += 1;
                }
            }
            apply_floor_rule(&rules, self.units.as_ref(), self.y_offset, before, sector);
            stamp_wall_material(sector, params.wall_material, &rules);
            flight.applied.moved += usize::from(wanted != old || !existed);
        }
        ensure_strata(room);
        ensure_trim(room);
        flight
    }

    /// The sector a cell held when the drag began.
    fn original(&self, cell: [i32; 2]) -> Option<crate::grid::sector::GridSector> {
        self.snapshot
            .cells()
            .iter()
            .find(|(c, _)| *c == cell)
            .and_then(|(_, s)| s.clone())
    }
}

/// Floor corners `[NW, NE, SE, SW]` of a tread sloping from `entry` to
/// `exit` in direction `dir`.
fn ramp_corners(dir: [i32; 2], entry: i32, exit: i32) -> [i32; 4] {
    match dir {
        [1, _] if dir[1] == 0 => [entry, exit, exit, entry],
        [-1, _] if dir[1] == 0 => [exit, entry, entry, exit],
        [_, 1] => [entry, entry, exit, exit],
        _ => [exit, exit, entry, entry],
    }
}

/// Lay a flight in one step (a typed amount, tests).
pub fn lay_stairs(
    world: &mut GridWorld,
    room: usize,
    path: &[[i32; 2]],
    params: StairsParams,
    rise: i32,
) -> Option<Flight> {
    let drag = StairsDrag::begin(world, room, path, params)?;
    Some(drag.apply(world, rise))
}
