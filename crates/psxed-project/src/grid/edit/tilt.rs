//! Tilt: add a linear ramp to the existing heights of a selection.
//!
//! Unlike Ramp, which writes absolute heights (and so flattens a platform),
//! Tilt is an *offset*: every corner of every selected floor and/or ceiling
//! gains `rise * t`, where `t` runs from 0 on the hinge to 1 on the far
//! side. A platform stays a platform, a pit stays a pit, a stair stays a
//! stair; the whole shape just leans. The hinge is an edge of the
//! selection's bounding box (the offset is planar along one axis) or a
//! corner (planar along the diagonal).
//!
//! Offsets are rounded to the 64 lattice per corner, and the same corner
//! always rounds the same way, so neighbours agree. The rounding is
//! returned per cell so a preview can show it. A tilt that would leave a
//! floor too steep to stand on is refused unless forced.

use std::collections::BTreeSet;

use crate::grid::auto_split;
use crate::grid::cook::RuntimeUnits;
use crate::grid::leaf::{LeafKey, CLICKS_PER_SECTOR};
use crate::grid::slope::{surface_class, SlopeClass};
use crate::grid::tree::normalize;
use crate::grid::units::{snap_to, FINE_UNITS};
use crate::grid::world::GridWorld;
use psx_engine::WalkLimits;

use super::rects::CellRect;
use super::verbs::ramp::RampDir;

/// What tilts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiltTarget {
    /// Floors only.
    Floor,
    /// Ceilings only.
    Ceiling,
    /// Both, by the same offsets.
    Both,
}

/// A corner of the selection's bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxCorner {
    /// North-west.
    NorthWest,
    /// North-east.
    NorthEast,
    /// South-east.
    SouthEast,
    /// South-west.
    SouthWest,
}

/// What stays put.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hinge {
    /// An edge of the bounding box: the far side gets the whole rise.
    Edge(RampDir),
    /// A corner: the opposite corner gets the whole rise.
    Corner(BoxCorner),
}

/// Parameters of a tilt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiltParams {
    /// The room that owns the cells.
    pub room: usize,
    /// World sectors to tilt.
    pub cells: BTreeSet<[i32; 2]>,
    /// Floor, ceiling or both.
    pub target: TiltTarget,
    /// The hinge.
    pub hinge: Hinge,
    /// Offset at the far side, grid units (negative leans down).
    pub rise: i32,
    /// Apply even where a floor ends up steeper than a body can stand on.
    pub force: bool,
}

/// Why a tilt was refused. Nothing changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TiltRefusal {
    /// No such room.
    NoSuchRoom,
    /// The selection holds no cell with a floor or ceiling.
    Empty,
    /// A leaf is partly inside the selection.
    LeafCrossesEdge(LeafKey),
    /// This many floors would become too steep to stand on.
    TooSteep(usize),
    /// A floor would meet or pass its ceiling in this cell.
    MeetsCeiling([i32; 2]),
}

impl TiltRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::Empty => "Nothing to tilt in the selection".to_string(),
            Self::LeafCrossesEdge(_) => {
                "A cell crosses the edge of the selection: split it to fit first".to_string()
            }
            Self::TooSteep(n) => {
                format!("{n} floors would be too steep to stand on (force to tilt anyway)")
            }
            Self::MeetsCeiling(c) => {
                format!("The floor would meet the ceiling at {},{}", c[0], c[1])
            }
        }
    }
}

/// The rounding of one leaf, for a preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TiltCell {
    /// The leaf's first sector.
    pub cell: [i32; 2],
    /// The exact offset at each corner `[NW, NE, SE, SW]`, in sixteenths of
    /// a grid unit.
    pub exact_16ths: [i32; 4],
    /// The offset applied at each corner (a multiple of 64).
    pub applied: [i32; 4],
}

/// What a tilt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiltOutcome {
    /// Per leaf rounding, in leaf order.
    pub cells: Vec<TiltCell>,
    /// Leaves whose heights changed.
    pub tilted: usize,
    /// Floors that ended up steep (only non zero when forced).
    pub steep: usize,
}

/// `rise * num / den` in sixteenths, rounded half away from zero, and the
/// same snapped to the lattice.
fn offset(rise: i32, num: i32, den: i32) -> (i32, i32) {
    let n = i64::from(rise) * 16 * i64::from(num);
    let d = i64::from(den.max(1));
    let half = d / 2;
    let exact = if n >= 0 {
        (n + half) / d
    } else {
        -((-n + half) / d)
    } as i32;
    let whole = if exact >= 0 {
        (exact + 8) / 16
    } else {
        -((-exact + 8) / 16)
    };
    (exact, snap_to(whole, FINE_UNITS))
}

fn corner_offset(hinge: Hinge, b: ([i32; 2], [i32; 2]), c: [i32; 2], rise: i32) -> (i32, i32) {
    let ([x0, z0], [x1, z1]) = b;
    let (w, d) = ((x1 - x0).max(1), (z1 - z0).max(1));
    let (num, den) = match hinge {
        Hinge::Edge(RampDir::North) => (c[1] - z0, d),
        Hinge::Edge(RampDir::South) => (z1 - c[1], d),
        Hinge::Edge(RampDir::West) => (c[0] - x0, w),
        Hinge::Edge(RampDir::East) => (x1 - c[0], w),
        Hinge::Corner(BoxCorner::NorthWest) => ((c[0] - x0) + (c[1] - z0), w + d),
        Hinge::Corner(BoxCorner::NorthEast) => ((x1 - c[0]) + (c[1] - z0), w + d),
        Hinge::Corner(BoxCorner::SouthEast) => ((x1 - c[0]) + (z1 - c[1]), w + d),
        Hinge::Corner(BoxCorner::SouthWest) => ((c[0] - x0) + (z1 - c[1]), w + d),
    };
    offset(rise, num, den)
}

/// Tilt the selection of `p` in `world`.
pub fn tilt_cells(world: &mut GridWorld, p: &TiltParams) -> Result<TiltOutcome, TiltRefusal> {
    let room = world.rooms.get(p.room).ok_or(TiltRefusal::NoSuchRoom)?;
    let mut leaves: Vec<LeafKey> = Vec::new();
    for (key, _) in room.leaves() {
        let (lo, hi) = key.sector_range();
        let r = CellRect { min: lo, max: hi };
        if r.cells().all(|c| p.cells.contains(&c)) {
            leaves.push(key);
        } else if r.cells().any(|c| p.cells.contains(&c)) {
            return Err(TiltRefusal::LeafCrossesEdge(key));
        }
    }
    let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
    for c in &p.cells {
        lo = [lo[0].min(c[0]), lo[1].min(c[1])];
        hi = [hi[0].max(c[0]), hi[1].max(c[1])];
    }
    let bounds = (
        [lo[0] * CLICKS_PER_SECTOR, lo[1] * CLICKS_PER_SECTOR],
        [
            (hi[0] + 1) * CLICKS_PER_SECTOR,
            (hi[1] + 1) * CLICKS_PER_SECTOR,
        ],
    );
    let floors = matches!(p.target, TiltTarget::Floor | TiltTarget::Both);
    let ceilings = matches!(p.target, TiltTarget::Ceiling | TiltTarget::Both);
    let units = RuntimeUnits::new(world.scale).ok();
    let limits = WalkLimits::DEFAULT;
    let y_offset = room.y_offset;

    let mut work = world.clone();
    let r = &mut work.rooms[p.room];
    let mut out = TiltOutcome {
        cells: Vec::new(),
        tilted: 0,
        steep: 0,
    };
    for key in leaves {
        let n = key.edge_clicks();
        let corners = [
            [key.x, key.z],
            [key.x + n, key.z],
            [key.x + n, key.z + n],
            [key.x, key.z + n],
        ];
        let mut exact = [0; 4];
        let mut applied = [0; 4];
        for (i, c) in corners.iter().enumerate() {
            let (e, a) = corner_offset(p.hinge, bounds, *c, p.rise);
            exact[i] = e;
            applied[i] = a;
        }
        let cell = key.cell();
        let Some(sector) = r.leaf_mut(key) else {
            continue;
        };
        let mut changed = false;
        if floors {
            if let Some(f) = sector.ground_floor_mut() {
                let was_steep = units
                    .as_ref()
                    .is_some_and(|u| surface_class(u, y_offset, f, &limits) == SlopeClass::Steep);
                for i in 0..4 {
                    f.heights[i] += applied[i];
                    if let Some(b) = f.heights_b.as_mut() {
                        b[i] += applied[i];
                    }
                }
                if applied != [0; 4] {
                    auto_split::resplit(f);
                    changed = true;
                }
                if !was_steep
                    && units.as_ref().is_some_and(|u| {
                        surface_class(u, y_offset, f, &limits) == SlopeClass::Steep
                    })
                {
                    out.steep += 1;
                }
            }
        }
        if ceilings {
            if let Some(c) = sector.ground_ceiling_mut() {
                for i in 0..4 {
                    c.heights[i] += applied[i];
                    if let Some(b) = c.heights_b.as_mut() {
                        b[i] += applied[i];
                    }
                }
                if applied != [0; 4] {
                    auto_split::resplit(c);
                    changed = true;
                }
            }
        }
        if let (Some(f), Some(c)) = (sector.ground_floor(), sector.ground_ceiling()) {
            if f.heights.iter().zip(c.heights.iter()).any(|(f, c)| f >= c) {
                return Err(TiltRefusal::MeetsCeiling(cell));
            }
        }
        out.tilted += usize::from(changed);
        out.cells.push(TiltCell {
            cell,
            exact_16ths: exact,
            applied,
        });
    }
    if out.cells.is_empty() {
        return Err(TiltRefusal::Empty);
    }
    if out.steep > 0 && !p.force {
        return Err(TiltRefusal::TooSteep(out.steep));
    }
    normalize(r);
    *world = work;
    Ok(out)
}
