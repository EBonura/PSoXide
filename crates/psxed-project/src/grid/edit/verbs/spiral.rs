//! Spiral stairs: a flight that winds round a ring of sectors, a storey per
//! turn or half turn (`docs/grid/multi-storey.md`, 5.7).
//!
//! The ring is a closed, four-connected walk of distinct sectors. The walk
//! applies the stairs rule: a sector the walk crosses straight is a **tread**
//! (one riser up), a sector where it turns is a **landing** (flat, at the
//! height of the tread before it). Walking round the ring again and again
//! until the climb is done, the stair winds up as many storeys as asked; on
//! an octagonal ring the chamfers are landings and the sides are flights.
//!
//! Each visit lays **a slab** at its height with the storey primitive
//! ([`insert_slab_column`]), so a sector the stair passes twice holds two
//! treads in its column, a storey apart, and the open volume under a tread
//! stays walkable where it is tall enough (the sliver rule fills it where it
//! is not). Nothing is guessed about the storeys: the heights are the climb,
//! counted from the floor at the start of the ring.
//!
//! Like every verb of the family, [`SpiralDrag`] captures what it may touch
//! when the gesture begins and each [`apply`](SpiralDrag::apply) restores that
//! capture first, so a drag can be repeated, reversed or cancelled exactly.

use psx_engine::WalkLimits;

use crate::grid::cook::RuntimeUnits;
use crate::grid::sector::GridSector;
use crate::grid::storeys::SLAB_THICKNESS;
use crate::grid::units::{snap_to, CLICK_UNITS, FINE_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::region::Region;
use super::super::spans::{insert_slab_column, SpanOutcome, SpanSkip};
use super::stairs::{max_riser, RiserOption};
use super::Snapshot;

/// Most treads one call lays: eight laps of a ring of a few dozen sectors.
const MAX_TREADS: usize = 1024;

/// Parameters of a spiral.
#[derive(Clone, Copy, Debug)]
pub struct SpiralParams {
    /// Storeys to climb, at least one.
    pub storeys: usize,
    /// Riser choice, as for a straight flight.
    pub riser: RiserOption,
    /// Material of the treads' walking surface.
    pub material: Option<ResourceId>,
}

impl Default for SpiralParams {
    fn default() -> Self {
        Self {
            storeys: 1,
            riser: RiserOption::Click,
            material: None,
        }
    }
}

/// Why a spiral was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpiralRefusal {
    /// The room index does not exist.
    NoSuchRoom,
    /// The room has leaves finer or coarser than a sector.
    MixedCells,
    /// The cells are not a closed ring of distinct, four-connected sectors
    /// (at least four of them).
    NotARing,
    /// Every sector of the ring is a turn: there is no tread to climb by.
    NoTreads,
    /// The climb per storey or the storey count is zero.
    NoClimb,
    /// The ring would need more laps than one call lays.
    TooLong,
}

impl SpiralRefusal {
    /// The refusal as a sentence.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "no such room".to_string(),
            Self::MixedCells => {
                "the room has cells of several sizes: the spiral works on whole sectors"
                    .to_string()
            }
            Self::NotARing => {
                "the cells are not a closed ring: they must be at least four distinct sectors, each next to the one before it, and the last next to the first"
                    .to_string()
            }
            Self::NoTreads => {
                "every sector of the ring is a turn, so there is no straight run to put treads on"
                    .to_string()
            }
            Self::NoClimb => "the climb must be above zero".to_string(),
            Self::TooLong => format!(
                "the climb needs more than {MAX_TREADS} treads: use fewer storeys or a taller riser"
            ),
        }
    }
}

/// One visit of the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpiralStep {
    /// The sector.
    pub cell: [i32; 2],
    /// Height of its walking surface above the floor at the start of the
    /// ring, grid units.
    pub rise: i32,
    /// A turn of the ring: flat, at the height of the tread before it.
    pub landing: bool,
}

/// What a spiral came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpiralReport {
    /// Why nothing was written.
    pub refused: Option<SpiralRefusal>,
    /// Treads laid (landings not counted).
    pub treads: usize,
    /// Landings visited.
    pub landings: usize,
    /// Laps of the ring the climb takes, counting a partial one.
    pub laps: usize,
    /// Riser of each tread, grid units.
    pub riser: i32,
    /// Climb per storey, grid units, as laid (the riser times the treads per
    /// storey).
    pub per_storey: i32,
    /// Total climb, grid units above the floor at the start.
    pub total: i32,
    /// Visits that already had a floor at their height.
    pub already: usize,
    /// The slabs: sectors changed, and sectors skipped by reason.
    pub slabs: SpanOutcome,
}

/// The closed ring a list of waypoints makes: consecutive waypoints joined
/// by the four-connected line walk ([`Region::path_cells`]) and the last
/// joined back to the first. `None` unless the result is at least four
/// distinct sectors.
pub fn ring_cells(waypoints: &[[i32; 2]]) -> Option<Vec<[i32; 2]>> {
    let first = *waypoints.first()?;
    let mut closed: Vec<[i32; 2]> = waypoints.to_vec();
    closed.push(first);
    let mut cells = Region::path_cells(&closed);
    // The closing leg ends on the first sector again.
    if cells.last() == Some(&first) {
        cells.pop();
    }
    is_ring(&cells).then_some(cells)
}

/// Whether `cells` is a closed, four-connected walk of distinct sectors.
pub fn is_ring(cells: &[[i32; 2]]) -> bool {
    if cells.len() < 4 {
        return false;
    }
    let adjacent = |a: [i32; 2], b: [i32; 2]| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() == 1;
    let closed = (0..cells.len()).all(|i| adjacent(cells[i], cells[(i + 1) % cells.len()]));
    let distinct = cells
        .iter()
        .enumerate()
        .all(|(i, c)| !cells[..i].contains(c));
    closed && distinct
}

/// Whether the walk turns at `index`.
fn is_turn(ring: &[[i32; 2]], index: usize) -> bool {
    let n = ring.len();
    let (prev, here, next) = (
        ring[(index + n - 1) % n],
        ring[index],
        ring[(index + 1) % n],
    );
    let dir = |a: [i32; 2], b: [i32; 2]| [b[0] - a[0], b[1] - a[1]];
    dir(prev, here) != dir(here, next)
}

/// The visits of a climb of `treads` treads at `riser` each round `ring`,
/// starting at its first sector: treads climb, turns are landings, and the
/// walk goes on through the landings after the last tread (the arrival).
///
/// Refused when the cells are no ring, the ring has no straight sector, or
/// the walk is longer than a call lays.
pub fn walk(
    ring: &[[i32; 2]],
    treads: usize,
    riser: i32,
) -> Result<Vec<SpiralStep>, SpiralRefusal> {
    if !is_ring(ring) {
        return Err(SpiralRefusal::NotARing);
    }
    let straight = (0..ring.len()).filter(|&i| !is_turn(ring, i)).count();
    if straight == 0 {
        return Err(SpiralRefusal::NoTreads);
    }
    if treads > MAX_TREADS {
        return Err(SpiralRefusal::TooLong);
    }
    let mut steps = Vec::new();
    let (mut laid, mut rise) = (0usize, 0i32);
    let mut index = 0usize;
    loop {
        let landing = is_turn(ring, index);
        if !landing {
            if laid == treads {
                // The first tread past the climb: the arrival is done.
                break;
            }
            laid += 1;
            rise += riser;
        }
        steps.push(SpiralStep {
            cell: ring[index],
            rise,
            landing,
        });
        index = (index + 1) % ring.len();
        if steps.len() > MAX_TREADS + ring.len() {
            return Err(SpiralRefusal::TooLong);
        }
    }
    Ok(steps)
}

/// A started spiral gesture.
#[derive(Clone, Debug)]
pub struct SpiralDrag {
    snapshot: Snapshot,
    params: SpiralParams,
    ring: Vec<[i32; 2]>,
    /// Floor height at the start of the ring, room-relative.
    base: i32,
    limit: i32,
    room: usize,
}

impl SpiralDrag {
    /// Start on `ring` (closed, in walking order) in `room`. `None` when the
    /// room is missing or has fine cells, or the cells are no ring.
    pub fn begin(
        world: &GridWorld,
        room: usize,
        ring: &[[i32; 2]],
        params: SpiralParams,
    ) -> Option<Self> {
        Self::begin_at(world, room, ring, params, base_height(world, room, ring))
    }

    /// [`begin`](Self::begin) with the height the climb starts from given
    /// (room-relative grid units) instead of read from the ring's first
    /// floor: for a ring whose sectors hold several floors, where the caller
    /// knows which one is meant.
    pub fn begin_at(
        world: &GridWorld,
        room: usize,
        ring: &[[i32; 2]],
        params: SpiralParams,
        base: Option<i32>,
    ) -> Option<Self> {
        let target = world.rooms.get(room)?;
        if !target.leaves.is_sectors_only() || !is_ring(ring) {
            return None;
        }
        let inside: Vec<[i32; 2]> = ring
            .iter()
            .copied()
            .filter(|c| target.local(*c).is_some())
            .collect();
        let units = RuntimeUnits::new(world.scale).ok();
        let limit = units
            .as_ref()
            .map_or(CLICK_UNITS * 2, |u| max_riser(u, &WalkLimits::DEFAULT));
        Some(Self {
            snapshot: Snapshot::capture(world, room, &inside)?,
            params,
            ring: ring.to_vec(),
            base: base.unwrap_or(0),
            limit,
            room,
        })
    }

    /// The parameters.
    pub fn params(&self) -> &SpiralParams {
        &self.params
    }

    /// Change the parameters between applies (storeys, riser).
    pub fn set_params(&mut self, params: SpiralParams) {
        self.params = params;
    }

    /// The ring, in walking order.
    pub fn ring(&self) -> &[[i32; 2]] {
        &self.ring
    }

    /// The tallest riser the character steps up, grid units.
    pub fn limit(&self) -> i32 {
        self.limit
    }

    /// The treads per storey and the riser for a climb of `per_storey` per
    /// storey: `round(per_storey / wanted riser)` treads, at least one, and
    /// the riser snapped to the fine step.
    fn shape(&self, per_storey: i32) -> (usize, i32) {
        let wanted = match self.params.riser {
            RiserOption::Click => CLICK_UNITS,
            RiserOption::Max => self.limit,
            RiserOption::Typed(v) => v.clamp(FINE_UNITS, self.limit.max(FINE_UNITS)),
        }
        .max(FINE_UNITS);
        let per = (((per_storey + wanted / 2) / wanted).max(1)) as usize;
        let riser = snap_to(per_storey / per as i32, FINE_UNITS).max(FINE_UNITS);
        (per, riser)
    }

    /// How a climb of `per_storey` grid units per storey would come out,
    /// without touching anything.
    pub fn plan(&self, per_storey: i32) -> SpiralReport {
        let mut report = SpiralReport::default();
        if per_storey <= 0 || self.params.storeys == 0 {
            report.refused = Some(SpiralRefusal::NoClimb);
            return report;
        }
        let (per, riser) = self.shape(per_storey);
        let treads = per * self.params.storeys;
        match walk(&self.ring, treads, riser) {
            Ok(steps) => {
                report.treads = treads;
                report.landings = steps.iter().filter(|s| s.landing).count();
                report.laps = steps.len().div_ceil(self.ring.len());
                report.riser = riser;
                report.per_storey = riser * per as i32;
                report.total = riser * treads as i32;
            }
            Err(refusal) => report.refused = Some(refusal),
        }
        report
    }

    /// The visits a climb of `per_storey` grid units per storey makes, in
    /// walking order, for a ghost; empty when the climb is refused.
    pub fn steps(&self, per_storey: i32) -> Vec<SpiralStep> {
        let plan = self.plan(per_storey);
        if plan.refused.is_some() {
            return Vec::new();
        }
        walk(&self.ring, plan.treads, plan.riser).unwrap_or_default()
    }

    /// Restore, then lay the spiral for a climb of `per_storey` grid units
    /// per storey. A refusal leaves the document as it began.
    pub fn apply(&self, world: &mut GridWorld, per_storey: i32) -> SpiralReport {
        self.snapshot.restore(world);
        let mut report = self.plan(per_storey);
        if report.refused.is_some() {
            return report;
        }
        let Ok(steps) = walk(&self.ring, report.treads, report.riser) else {
            return report;
        };
        let Some(room) = world.rooms.get_mut(self.room) else {
            return report;
        };
        for step in &steps {
            let y = self.base + step.rise;
            let Some(sector) = room
                .local(step.cell)
                .and_then(|[x, z]| room.sector_mut(x, z))
            else {
                report.slabs.skipped.add(SpanSkip::NoSpanAtHeight);
                continue;
            };
            if has_flat_floor_at(sector, y) {
                report.already += 1;
                continue;
            }
            match insert_slab_column(sector, y, SLAB_THICKNESS, self.params.material) {
                Ok(()) => report.slabs.changed += 1,
                Err(skip) => report.slabs.skipped.add(skip),
            }
        }
        report
    }
}

/// Whether some span of the column has a flat floor at exactly `y`.
fn has_flat_floor_at(sector: &GridSector, y: i32) -> bool {
    sector.spans().iter().any(|span| {
        span.floor
            .as_ref()
            .is_some_and(|f| f.height_range() == (y, y))
    })
}

/// The height a spiral on `ring` starts from: the ground floor of the first
/// sector of the ring that has one, room-relative grid units.
pub fn base_height(world: &GridWorld, room: usize, ring: &[[i32; 2]]) -> Option<i32> {
    let target = world.rooms.get(room)?;
    ring.iter().find_map(|&cell| {
        let [x, z] = target.local(cell)?;
        let floor = target.sector(x, z)?.ground_floor()?;
        Some(snap_to(floor.heights.iter().sum::<i32>() / 4, FINE_UNITS))
    })
}

/// Lay a spiral in one step (a typed amount, the MCP, tests).
pub fn lay_spiral(
    world: &mut GridWorld,
    room: usize,
    ring: &[[i32; 2]],
    params: SpiralParams,
    per_storey: i32,
) -> SpiralReport {
    let Some(target) = world.rooms.get(room) else {
        return refused(SpiralRefusal::NoSuchRoom);
    };
    if !target.leaves.is_sectors_only() {
        return refused(SpiralRefusal::MixedCells);
    }
    match SpiralDrag::begin(world, room, ring, params) {
        Some(drag) => drag.apply(world, per_storey),
        None => refused(SpiralRefusal::NotARing),
    }
}

fn refused(reason: SpiralRefusal) -> SpiralReport {
    SpiralReport {
        refused: Some(reason),
        ..SpiralReport::default()
    }
}
