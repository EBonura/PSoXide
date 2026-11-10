//! Terrain patch: seeded, re-rollable natural ground over a region.
//!
//! The ground is a height field on the corner lattice (shared corners, so
//! the surface is continuous inside the patch): octaves of tileable value
//! noise, an optional constant slope, and an edge rule that blends into the
//! neighbours so the border does not form a cliff unless asked. It writes
//! ordinary floor corner heights, the way Break and Rough do, and every
//! number is an integer: the noise is a hash of `(seed, octave, lattice
//! point)` with an integer smoothstep, so a seed means the same ground on
//! every host and two patches with one seed tile at their shared border.
//!
//! `keep_walkable` relaxes the field until no two neighbouring corners
//! differ by more than a slope the motor stands on (both triangles of every
//! cell stay under [`psx_engine::WalkLimits`] `min_floor_normal_y_q12`),
//! never moving the corners the edge blend anchors to their neighbours.

use std::collections::{BTreeMap, BTreeSet};

use crate::grid::auto_split;
use crate::grid::leaf::LeafKey;
use crate::grid::sector::GridSector;
use crate::grid::surface::GridSurface;
use crate::grid::tree::normalize;
use crate::grid::units::{snap_to, FINE_UNITS, SECTOR_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::focus::FocusRefusal;
use super::super::rects::{grow_room_to, RectRefusal};
use super::common::{aimed, bounds_of, sector_ready};
use super::rules::{floor_corners, move_ceiling_with_floor, set_floor_corners};

/// What happens at the patch border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Fade the relief to nothing over this many sectors next to cells
    /// outside the patch that have a floor.
    Blend(i32),
    /// Full relief up to the border (a cliff where the neighbour differs).
    Cliff,
}

/// Parameters of a terrain patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerrainParams {
    /// The room.
    pub room: usize,
    /// The sectors, world coordinates.
    pub cells: BTreeSet<[i32; 2]>,
    /// The seed; another seed is a re-roll.
    pub seed: u32,
    /// Peak amplitude of the first octave, in clicks (256 grid units).
    pub amplitude_clicks: i32,
    /// Octaves, 1 to 4; each halves the wavelength and the amplitude.
    pub octaves: u32,
    /// Wavelength of the first octave, in sectors.
    pub wavelength: i32,
    /// Constant slope, grid units of rise per sector along `[x, z]`.
    pub slope: [i32; 2],
    /// Floor height of the patch, grid units; `None` adds the relief to the
    /// floors that exist.
    pub base: Option<i32>,
    /// Make a floor where a cell has none (needs `base`).
    pub create: bool,
    /// Clamp neighbouring corners to the walkable slope.
    pub keep_walkable: bool,
    /// The border rule.
    pub edge: Edge,
    /// Move ceilings with their floors.
    pub follow_ceiling: bool,
    /// Material of floors this makes.
    pub material: Option<ResourceId>,
    /// The storey the edit is aimed at (defaults to `base` or 0).
    pub aim: Option<i32>,
}

impl TerrainParams {
    /// A patch with the defaults: two octaves of a four sector wavelength,
    /// one click of relief, walkable, blended over two sectors.
    pub fn new(room: usize, cells: BTreeSet<[i32; 2]>, seed: u32) -> Self {
        Self {
            room,
            cells,
            seed,
            amplitude_clicks: 1,
            octaves: 2,
            wavelength: 4,
            slope: [0, 0],
            base: None,
            create: false,
            keep_walkable: true,
            edge: Edge::Blend(2),
            follow_ceiling: true,
            material: None,
            aim: None,
        }
    }
}

/// Why a patch was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerrainRefusal {
    /// The room does not exist.
    NoSuchRoom,
    /// No sector.
    EmptyRegion,
    /// A number is out of range (the message names it).
    BadParameter(&'static str),
    /// A sector has no floor and there is no base to make one at.
    NoFloor {
        /// The sector.
        cell: [i32; 2],
    },
    /// The room would pass its size cap.
    RoomTooLarge,
    /// Finer cells hold part of this sector.
    FineCells {
        /// The sector.
        cell: [i32; 2],
    },
    /// The storey aim failed.
    Aim(FocusRefusal),
}

impl TerrainRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::EmptyRegion => "Pick the cells to cover".to_string(),
            Self::BadParameter(what) => format!("{what} is out of range"),
            Self::NoFloor { cell } => format!(
                "Sector ({}, {}) has no floor: give a base height or leave it out",
                cell[0], cell[1]
            ),
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
            Self::FineCells { cell } => format!(
                "Finer cells stand in sector ({}, {}): merge them first",
                cell[0], cell[1]
            ),
            Self::Aim(f) => f.describe(),
        }
    }
}

/// What a patch did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainReport {
    /// Sectors written.
    pub cells: usize,
    /// Floors made.
    pub created: usize,
    /// Corners the walkable clamp moved.
    pub clamped: usize,
    /// Lowest and highest corner written, grid units.
    pub range: (i32, i32),
    /// The steepest edge between neighbouring corners, grid units a sector.
    pub steepest: i32,
}

/// The steepest rise between two neighbouring corners the clamp allows,
/// grid units over one sector (a multiple of the fine step). Each triangle's
/// gradient has two such components, so its normal stays above the motor's
/// limit: `sqrt(2) * 576 = 815 < 1024 * 1.02`.
pub const MAX_EDGE_RISE: i32 = 576;

fn mix(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// Value in `-1024..=1024` at a noise lattice point.
fn lattice(seed: u32, octave: u32, p: [i32; 2]) -> i64 {
    let mut h = mix(seed ^ 0x9e37_79b9);
    h = mix(h ^ octave.wrapping_mul(0x85eb_ca6b));
    h = mix(h ^ (p[0] as u32).wrapping_mul(0xc2b2_ae35));
    h = mix(h ^ (p[1] as u32).wrapping_mul(0x27d4_eb2f));
    i64::from(h % 2049) - 1024
}

/// Integer smoothstep of `t` in `0..=1024`.
fn smooth(t: i64) -> i64 {
    (t * t * (3 * 1024 - 2 * t)) >> 20
}

/// Relief at lattice point `p` (sectors), grid units.
pub fn relief(p: &TerrainParams, at: [i32; 2]) -> i32 {
    let mut total = 0i64;
    for o in 0..p.octaves {
        let wl = (p.wavelength >> o).max(1);
        let amp = i64::from(p.amplitude_clicks) * i64::from(SECTOR_UNITS / 4) >> o;
        let (gx, gz) = (at[0].div_euclid(wl), at[1].div_euclid(wl));
        let tx = i64::from(at[0].rem_euclid(wl)) * 1024 / i64::from(wl);
        let tz = i64::from(at[1].rem_euclid(wl)) * 1024 / i64::from(wl);
        let (sx, sz) = (smooth(tx), smooth(tz));
        let v = |dx, dz| lattice(p.seed, o, [gx + dx, gz + dz]);
        let top = v(0, 0) * (1024 - sx) + v(1, 0) * sx;
        let bottom = v(0, 1) * (1024 - sx) + v(1, 1) * sx;
        let field = (top * (1024 - sz) + bottom * sz) >> 20;
        total += field * amp / 1024;
    }
    total as i32
}

fn round_div(num: i64, den: i64) -> i64 {
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

/// The corner heights the patch would write, by lattice point (sectors).
/// Pure: reads the document, changes nothing. Returns the field and the set
/// of points the edge blend anchors.
#[allow(clippy::type_complexity)]
pub fn field(
    world: &GridWorld,
    p: &TerrainParams,
) -> Result<(BTreeMap<[i32; 2], i32>, BTreeSet<[i32; 2]>), TerrainRefusal> {
    let room = world.rooms.get(p.room).ok_or(TerrainRefusal::NoSuchRoom)?;
    if p.cells.is_empty() {
        return Err(TerrainRefusal::EmptyRegion);
    }
    if !(1..=4).contains(&p.octaves) {
        return Err(TerrainRefusal::BadParameter("octaves"));
    }
    if !(1..=64).contains(&p.wavelength) {
        return Err(TerrainRefusal::BadParameter("wavelength"));
    }
    if !(0..=64).contains(&p.amplitude_clicks) {
        return Err(TerrainRefusal::BadParameter("amplitude"));
    }
    // Existing corner heights, first cell in order wins a shared point.
    let mut existing: BTreeMap<[i32; 2], i32> = BTreeMap::new();
    let mut points: BTreeSet<[i32; 2]> = BTreeSet::new();
    for &c in &p.cells {
        let floor = room
            .local(c)
            .and_then(|[x, z]| room.sector(x, z))
            .and_then(|s| s.ground_floor());
        if floor.is_none() && !(p.create && p.base.is_some()) {
            return Err(TerrainRefusal::NoFloor { cell: c });
        }
        for (i, (dx, dz)) in [(0, 0), (1, 0), (1, 1), (0, 1)].into_iter().enumerate() {
            let pt = [c[0] + dx, c[1] + dz];
            points.insert(pt);
            if let Some(f) = floor {
                existing.entry(pt).or_insert(f.heights[i]);
            }
        }
    }
    // Lattice points next to a floor outside the patch.
    let mut outside: BTreeSet<[i32; 2]> = BTreeSet::new();
    if matches!(p.edge, Edge::Blend(_)) {
        for &c in &p.cells {
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let n = [c[0] + dx, c[1] + dz];
                let has = !p.cells.contains(&n)
                    && room
                        .leaf_at([n[0] * 4, n[1] * 4])
                        .is_some_and(|(_, s)| s.ground_floor().is_some());
                if has {
                    for (ox, oz) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                        outside.insert([n[0] + ox, n[1] + oz]);
                    }
                }
            }
        }
    }
    let origin = bounds_of(&p.cells).map_or([0, 0], |r| r.min);
    let mut out = BTreeMap::new();
    let mut anchored = BTreeSet::new();
    for &pt in &points {
        let e = existing.get(&pt).copied().or(p.base).unwrap_or(0);
        let slope = p.slope[0] * (pt[0] - origin[0]) + p.slope[1] * (pt[1] - origin[1]);
        let target = p.base.unwrap_or(e) + relief(p, pt) + slope;
        let w = match p.edge {
            Edge::Cliff => 1024,
            Edge::Blend(b) => {
                let d = outside
                    .iter()
                    .map(|q| (pt[0] - q[0]).abs().max((pt[1] - q[1]).abs()))
                    .min();
                match d {
                    None => 1024,
                    Some(d) => (i64::from(d) * 1024 / i64::from(b.max(1))).min(1024) as i32,
                }
            }
        };
        if w == 0 {
            anchored.insert(pt);
        }
        let h = i64::from(e) + round_div(i64::from(target - e) * i64::from(w), 1024);
        out.insert(pt, snap_to(h as i32, FINE_UNITS));
    }
    Ok((out, anchored))
}

/// Relax `field` until neighbouring corners differ by at most `limit`.
/// Returns how many corners moved. Deterministic: points are visited in key
/// order and a pass changes a corner by at most the excess.
pub fn clamp_slopes(
    field: &mut BTreeMap<[i32; 2], i32>,
    anchored: &BTreeSet<[i32; 2]>,
    limit: i32,
) -> usize {
    let mut moved: BTreeSet<[i32; 2]> = BTreeSet::new();
    for _ in 0..256 {
        let mut changed = false;
        let keys: Vec<[i32; 2]> = field.keys().copied().collect();
        for k in keys {
            for (dx, dz) in [(1, 0), (0, 1)] {
                let n = [k[0] + dx, k[1] + dz];
                let (Some(&a), Some(&b)) = (field.get(&k), field.get(&n)) else {
                    continue;
                };
                let excess = (a - b).abs() - limit;
                if excess <= 0 {
                    continue;
                }
                let step = snap_to(excess, FINE_UNITS).max(FINE_UNITS);
                let sign = (b - a).signum();
                let (fa, fb) = (anchored.contains(&k), anchored.contains(&n));
                let (da, db) = match (fa, fb) {
                    (true, true) => continue,
                    (true, false) => (0, -sign * step),
                    (false, true) => (sign * step, 0),
                    (false, false) => {
                        let half = snap_to(step / 2, FINE_UNITS).max(FINE_UNITS);
                        (sign * half, -sign * half)
                    }
                };
                if da != 0 {
                    *field.get_mut(&k).expect("key") += da;
                    moved.insert(k);
                }
                if db != 0 {
                    *field.get_mut(&n).expect("key") += db;
                    moved.insert(n);
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    moved.len()
}

/// Lay a terrain patch. One logical edit: the caller snapshots once.
pub fn lay_terrain(
    world: &mut GridWorld,
    p: &TerrainParams,
) -> Result<TerrainReport, TerrainRefusal> {
    // Work on a copy; cells held by a bigger leaf are split to sectors first
    // so their corners can be read and written.
    let mut work = world.clone();
    if let Some(room) = work.rooms.get_mut(p.room) {
        for &c in &p.cells {
            if !sector_ready(room, c) {
                return Err(TerrainRefusal::FineCells { cell: c });
            }
        }
    }
    let (mut f, anchored) = field(&work, p)?;
    let mut report = TerrainReport::default();
    if p.keep_walkable {
        report.clamped = clamp_slopes(&mut f, &anchored, MAX_EDGE_RISE);
    }
    let rect = bounds_of(&p.cells).ok_or(TerrainRefusal::EmptyRegion)?;
    grow_room_to(&mut work.rooms[p.room], rect).map_err(|e| match e {
        RectRefusal::NoSuchRoom => TerrainRefusal::NoSuchRoom,
        RectRefusal::RoomTooLarge { .. } => TerrainRefusal::RoomTooLarge,
    })?;
    let aim = Some(p.aim.or(p.base).unwrap_or(0));
    let written = aimed(&mut work, p.room, &p.cells, aim, |w| write(w, p, &f))
        .map_err(TerrainRefusal::Aim)??;
    normalize(&mut work.rooms[p.room]);
    *world = work;
    report.cells = written.0;
    report.created = written.1;
    report.range = (
        f.values().copied().min().unwrap_or(0),
        f.values().copied().max().unwrap_or(0),
    );
    report.steepest = steepest(&f);
    Ok(report)
}

/// The steepest rise between neighbouring corners of `f`.
pub fn steepest(f: &BTreeMap<[i32; 2], i32>) -> i32 {
    let mut worst = 0;
    for (k, a) in f {
        for (dx, dz) in [(1, 0), (0, 1)] {
            if let Some(b) = f.get(&[k[0] + dx, k[1] + dz]) {
                worst = worst.max((a - b).abs());
            }
        }
    }
    worst
}

fn write(
    world: &mut GridWorld,
    p: &TerrainParams,
    f: &BTreeMap<[i32; 2], i32>,
) -> Result<(usize, usize), TerrainRefusal> {
    let room = &mut world.rooms[p.room];
    let (mut cells, mut created) = (0, 0);
    for &c in &p.cells {
        if !sector_ready(room, c) {
            return Err(TerrainRefusal::FineCells { cell: c });
        }
        let [x, z] = room.local(c).ok_or(TerrainRefusal::RoomTooLarge)?;
        let corners = [(0, 0), (1, 0), (1, 1), (0, 1)].map(|(dx, dz)| f[&[c[0] + dx, c[1] + dz]]);
        if room.sector(x, z).is_none() {
            room.insert_leaf(LeafKey::sector(c), GridSector::default())
                .map_err(|_| TerrainRefusal::RoomTooLarge)?;
        }
        let sector = room.sector_mut(x, z).ok_or(TerrainRefusal::RoomTooLarge)?;
        let old = floor_corners(sector);
        if old.is_none() {
            sector.set_ground_floor(Some(GridSurface::flat_auto(corners[0], p.material)));
            created += 1;
        }
        let old = old.unwrap_or([corners; 2]);
        let wanted = [corners, corners];
        if let Some(floor) = sector.ground_floor_mut() {
            set_floor_corners(floor, wanted);
            floor.heights_b = None;
            if wanted != old {
                auto_split::resplit(floor);
            }
        }
        if p.follow_ceiling {
            let deltas = [0, 1].map(|k| core::array::from_fn(|i| wanted[k][i] - old[k][i]));
            move_ceiling_with_floor(sector, deltas);
        }
        cells += 1;
    }
    Ok((cells, created))
}
