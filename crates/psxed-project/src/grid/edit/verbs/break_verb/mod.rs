//! Break: add natural detail to a region by breaking its walls, floors and
//! ceilings.
//!
//! The verb has the shape of every verb here: `begin` takes a snapshot of the
//! sectors it may touch and `apply(intensity, seed)` first restores it, then
//! plans and applies the operations from absolute values, so a drag can be
//! repeated, reversed and cancelled exactly and `apply` at intensity 0 gives
//! the document back byte for byte. Everything it writes is ordinary grid
//! data (corner heights, split-level triangles, void triangles, an edge
//! override's `top_delta`); the seed is not stored anywhere.
//!
//! What it does, by target (see `docs/grid/natural-terrain.md`, section 3):
//!
//! * **walls**: the lip of a plateau crumbles (its edge vertices drop, so the
//!   derived riser's top follows), a plateau edge gets bites (the half of a
//!   cell on the low side drops, a diagonal riser appears), and an open-topped
//!   rock wall gets a ragged top (`top_delta`) with the odd stub.
//! * **floor**: interior vertices tilt, tiles crack along a diagonal (the
//!   Cathedral's broken tile), and at high intensity a cell may turn into a
//!   gap.
//! * **ceiling**: the same for the roof, held above the body's headroom.
//!
//! With `keep_walkable` every operation is checked against the walkability
//! analysis (the motor's own numbers) around the cells it changes, and one
//! that would add a floor too steep to stand on, a step the character cannot
//! climb or too little headroom is retried at half the size, then dropped and
//! counted.

mod apply;
mod noise;
mod plan;

use crate::grid::edit::region::Region;
use crate::grid::walk::{Spawn, WalkLimits};
use crate::grid::world::GridWorld;

use super::Snapshot;

pub use noise::Op as Decision;

/// The vertical step operations are quantised to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BreakSnap {
    /// 64 grid units.
    #[default]
    Fine,
    /// 256 grid units, one click.
    Click,
    /// An exact step in grid units (what a lens makes of a fine step).
    Units(i32),
}

impl BreakSnap {
    /// The step in grid units.
    pub const fn step(self) -> i32 {
        match self {
            Self::Fine => crate::grid::units::FINE_UNITS,
            Self::Click => crate::grid::units::CLICK_UNITS,
            Self::Units(step) => {
                if step > 0 {
                    step
                } else {
                    crate::grid::units::FINE_UNITS
                }
            }
        }
    }
}

/// Which parts a break touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakTargets {
    /// Plateau lips, edge bites and open wall tops.
    pub walls: bool,
    /// Interior floor detail.
    pub floor: bool,
    /// The roof.
    pub ceiling: bool,
}

impl Default for BreakTargets {
    fn default() -> Self {
        Self {
            walls: true,
            floor: true,
            ceiling: false,
        }
    }
}

/// The intensity a break uses when none is given. The Cathedral's hand-made
/// look measures at about 10 to 20 (`natural_tests`); 40, the first default,
/// was too strong and read as rubble.
pub const DEFAULT_INTENSITY: u32 = 15;

/// A named look for a break: one click sets what the option row and the MCP
/// tool otherwise take one field at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakStyle {
    /// The Cathedral's sparse hand-made look: crumbled plateau lips and a
    /// few cracked tiles, at the intensity the Cathedral measures at.
    Cathedral,
}

impl BreakStyle {
    /// Parse the name an MCP caller gives.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "cathedral" => Some(Self::Cathedral),
            _ => None,
        }
    }

    /// The intensity the look is made at.
    pub const fn intensity(self) -> u32 {
        match self {
            Self::Cathedral => DEFAULT_INTENSITY,
        }
    }

    /// The parameters the look is made with (everything but the walking
    /// numbers and spawns, which the caller supplies).
    pub fn params(self) -> BreakParams {
        match self {
            Self::Cathedral => BreakParams {
                targets: BreakTargets {
                    walls: true,
                    floor: true,
                    ceiling: false,
                },
                amplitude: 4,
                snap: BreakSnap::Fine,
                keep_walkable: true,
                mix_materials: false,
                jitter: 0,
                ..BreakParams::default()
            },
        }
    }
}

/// Parameters of a break (the editor's option row and the MCP tool).
#[derive(Clone, Debug)]
pub struct BreakParams {
    /// What to break.
    pub targets: BreakTargets,
    /// The largest vertical change, in fine steps (64 grid units): the
    /// amplitude at intensity 100.
    pub amplitude: i32,
    /// The step changes are quantised to.
    pub snap: BreakSnap,
    /// Hold every change to the walkability mask.
    pub keep_walkable: bool,
    /// Where the player starts (reachability needs it).
    pub spawns: Vec<Spawn>,
    /// Give a cracked triangle the material of the floor it now borders.
    pub mix_materials: bool,
    /// The rough level (0 to 3) to give the broken sectors, 0 for none.
    pub jitter: u8,
    /// The character's walking numbers.
    pub limits: WalkLimits,
}

impl Default for BreakParams {
    fn default() -> Self {
        Self {
            targets: BreakTargets::default(),
            amplitude: 4,
            snap: BreakSnap::Fine,
            keep_walkable: true,
            spawns: Vec::new(),
            mix_materials: false,
            jitter: 0,
            limits: WalkLimits::DEFAULT,
        }
    }
}

/// What a break did, for the status line and the MCP reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BreakReport {
    /// The seed used.
    pub seed: u32,
    /// The intensity used.
    pub intensity: u32,
    /// Plateau lip vertices dropped.
    pub crumbled: usize,
    /// Interior floor vertices moved.
    pub tilted: usize,
    /// Tiles cracked along a diagonal (floor).
    pub cracked: usize,
    /// Plateau edge bites (a crack on a cell at a height boundary).
    pub bites: usize,
    /// Open wall segments given a ragged top.
    pub tops: usize,
    /// Of those, whole segments stubbed down by clicks.
    pub stubs: usize,
    /// Ceiling vertices moved and ceiling tiles cracked.
    pub ceiling: usize,
    /// Cells turned into gaps.
    pub gaps: usize,
    /// Sectors changed.
    pub touched: usize,
    /// Operations dropped by the walkability mask, with why (first few).
    pub rejects: Vec<String>,
    /// How many operations the mask dropped in all.
    pub rejected: usize,
    /// Operations that went through at a smaller size after a retry.
    pub shrunk: usize,
    /// Walkability before and after: standable sectors no spawn reaches,
    /// steps taller than the body climbs, steep floors. `None` without a
    /// player spawn on a floor.
    pub walk: Option<[WalkCount; 2]>,
}

/// The walkability totals a break compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalkCount {
    /// Walkable sectors.
    pub standable: usize,
    /// Of those, sectors no spawn reaches.
    pub unreachable: usize,
    /// Steps taller than the body climbs.
    pub steps: usize,
    /// Floors too steep to stand on.
    pub steep: usize,
}

/// A started Break gesture.
#[derive(Clone, Debug)]
pub struct BreakDrag {
    snapshot: Snapshot,
    cells: Vec<[i32; 2]>,
    params: BreakParams,
}

impl BreakDrag {
    /// Start on `region`: the sectors of it that have a floor or a ceiling.
    /// `None` when the room is missing or nothing is there.
    pub fn begin(world: &GridWorld, region: &Region, params: BreakParams) -> Option<Self> {
        let room = world.rooms.get(region.room)?;
        let cells: Vec<[i32; 2]> = region
            .cells()
            .filter(|&c| {
                room.local(c)
                    .and_then(|[x, z]| room.sector(x, z))
                    .is_some_and(|s| s.ground_floor().is_some() || s.ground_ceiling().is_some())
            })
            .collect();
        if cells.is_empty() {
            return None;
        }
        Some(Self {
            snapshot: Snapshot::capture(world, region.room, &cells)?,
            cells,
            params,
        })
    }

    /// The parameters.
    pub fn params(&self) -> &BreakParams {
        &self.params
    }

    /// Change the parameters between applies (the option row).
    pub fn set_params(&mut self, params: BreakParams) {
        self.params = params;
    }

    /// Sectors the break may change.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether nothing is there.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Restore the snapshot, then break it with `seed` at `intensity` (0 to
    /// 100). Safe to call any number of times, in any order.
    pub fn apply(&self, world: &mut GridWorld, intensity: u32, seed: u32) -> BreakReport {
        self.snapshot.restore(world);
        let intensity = intensity.min(100);
        let mut report = BreakReport {
            seed,
            intensity,
            ..BreakReport::default()
        };
        if intensity == 0 {
            return report;
        }
        apply::run(
            world,
            self.snapshot.room(),
            &self.cells,
            &self.params,
            intensity,
            seed,
            &mut report,
        );
        report
    }
}

/// Break `region` in one step (a typed intensity, the MCP, tests).
pub fn break_region(
    world: &mut GridWorld,
    region: &Region,
    params: BreakParams,
    intensity: u32,
    seed: u32,
) -> Option<BreakReport> {
    let drag = BreakDrag::begin(world, region, params)?;
    Some(drag.apply(world, intensity, seed))
}

#[cfg(test)]
mod tests;
