//! Sculpt: terraforming with a brush.
//!
//! A stroke is a list of dabs (circles of the brush radius laid along the
//! pointer's path). The stroke keeps the room as it was when it began and
//! every update replays all the dabs from that copy, so a stroke can grow,
//! be cancelled and be redone exactly, and the document at any moment is the
//! base plus the dabs so far.
//!
//! What a dab changes is a **delta field** on the click lattice (a quarter
//! sector, 256 grid units): one number per lattice vertex, quantised to the
//! fine step of 64 before it is written. Every leaf corner that sits on a
//! vertex moves by that vertex's delta, so neighbouring cells stay welded, a
//! cliff keeps its height (both sides move together) and the derived walls
//! and risers follow. Because the delta is an offset, not an absolute
//! height, the 64 unit lattice of corner heights is kept: an offset of a
//! multiple of 64 on a multiple of 64 is a multiple of 64. (A hanging corner
//! between a coarse and two fine cells still takes the exact midpoint the
//! tree constrains it to, as every tree edit does.)
//!
//! * **Raise** adds to the field under the brush (**invert** lowers).
//! * **Smooth** moves each vertex toward the mean of its four neighbours.
//! * **Flatten** moves vertices toward the height the stroke started at.
//! * **Ramp** makes the ground along the drag from A to B a straight slope
//!   from the height at A to the height at B (plus a chosen rise).
//! * **Dunes** lays ripples across the drag: the crests run perpendicular to
//!   the drag direction, with the chosen wavelength and height.
//!
//! **Detail** says how fine the cells under the brush may become: whole
//! sectors, halves or quarters (the cells are subdivided first, which never
//! changes the surface), or **Auto**, which starts at sectors and divides
//! only the cells whose new surface a plane cannot follow within one fine
//! step. A column that holds several storeys is never divided (the cook
//! writes its extra spans per whole sector); it is sculpted as a whole
//! sector and the report says how many.
//!
//! The tool edits the ground floor (or the ground ceiling, `Target`) of the
//! cells it reaches. A floor is held a body's height under the ceiling of
//! its own cell and a ceiling the same over the floor; a corner that was
//! already closer is never pushed further.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::super::auto_split;
use super::super::leaf::{LeafKey, CLICKS_PER_SECTOR, FINEST_LEVEL, SECTOR_LEVEL};
use super::super::room::GridRoom;
use super::super::sector::GridSector;
use super::super::surface::{GridSplit, GridSurface};
use super::super::tree;
use super::super::units::{CLICK_UNITS, FINE_UNITS};
use super::super::world::GridWorld;

#[cfg(test)]
mod tests;

/// What the brush does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SculptMode {
    /// Raise the ground under the brush (inverted: lower it).
    Raise,
    /// Blend the ground toward the mean of its neighbours.
    Smooth,
    /// Level the ground to the height the stroke started at.
    Flatten,
    /// A straight slope from the start of the drag to its end.
    Ramp,
    /// Ripples across the drag.
    Dunes,
}

impl SculptMode {
    /// Every mode, in the order the option row lists them.
    pub const ALL: [Self; 5] = [
        Self::Raise,
        Self::Smooth,
        Self::Flatten,
        Self::Ramp,
        Self::Dunes,
    ];

    /// Short name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Raise => "Raise/Lower",
            Self::Smooth => "Smooth",
            Self::Flatten => "Flatten",
            Self::Ramp => "Ramp",
            Self::Dunes => "Dunes",
        }
    }

    /// Whether the mode accumulates dab by dab (the others are a function of
    /// the whole stroke).
    pub const fn accumulates(self) -> bool {
        matches!(self, Self::Raise | Self::Smooth | Self::Flatten)
    }
}

/// How the brush's strength falls off from its centre to its rim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Falloff {
    /// A cosine: full at the centre, nothing and no slope at the rim.
    Smooth,
    /// A straight line from the centre to the rim.
    Linear,
    /// A tight peak: most of the effect is near the centre.
    Sharp,
}

impl Falloff {
    /// Every falloff, in the order the option row lists them.
    pub const ALL: [Self; 3] = [Self::Smooth, Self::Linear, Self::Sharp];

    /// Short name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Smooth => "Smooth",
            Self::Linear => "Linear",
            Self::Sharp => "Sharp",
        }
    }

    /// The weight at `u` of the way from the centre (0) to the rim (1).
    pub fn weight(self, u: f64) -> f64 {
        if !(0.0..1.0).contains(&u) {
            return 0.0;
        }
        match self {
            Self::Smooth => 0.5 * (1.0 + (core::f64::consts::PI * u).cos()),
            Self::Linear => 1.0 - u,
            Self::Sharp => (1.0 - u).powi(3),
        }
    }
}

/// How fine the cells under the brush may become.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// Whole sectors (cells coarser than a sector are divided to it).
    Sector,
    /// Half sectors.
    Half,
    /// Quarter sectors.
    Quarter,
    /// Sectors, divided to halves and quarters only where the new surface
    /// needs it.
    Auto,
}

impl Detail {
    /// Every detail, in the order the option row lists them.
    pub const ALL: [Self; 4] = [Self::Sector, Self::Half, Self::Quarter, Self::Auto];

    /// Short name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sector => "Sector",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
            Self::Auto => "Auto",
        }
    }

    /// The leaf level every cell under the brush is taken to first.
    const fn level(self) -> u8 {
        match self {
            Self::Sector | Self::Auto => SECTOR_LEVEL,
            Self::Half => 3,
            Self::Quarter => FINEST_LEVEL,
        }
    }
}

/// Which surface is sculpted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The ground floor.
    Floor,
    /// The ground ceiling.
    Ceiling,
}

/// Everything the option row sets.
#[derive(Clone, Debug, PartialEq)]
pub struct SculptParams {
    /// What the brush does.
    pub mode: SculptMode,
    /// Brush radius in clicks (quarter sectors).
    pub radius: f64,
    /// 0 to 1: how much one dab does (Raise, Smooth, Flatten).
    pub strength: f64,
    /// How the strength falls off to the rim.
    pub falloff: Falloff,
    /// How fine the cells may become.
    pub detail: Detail,
    /// Floor or ceiling.
    pub target: Target,
    /// Shift: Raise lowers, Ramp's rise goes down, Dunes start with a trough.
    pub invert: bool,
    /// Dunes: crest to crest, in clicks (at least four).
    pub wavelength: f64,
    /// Dunes: height of a crest above the mean, grid units.
    pub dune_height: i32,
    /// Ramp: extra rise at B over the ground height there, grid units.
    pub rise: i32,
}

impl Default for SculptParams {
    fn default() -> Self {
        Self {
            mode: SculptMode::Raise,
            radius: 6.0,
            strength: 0.5,
            falloff: Falloff::Smooth,
            detail: Detail::Sector,
            target: Target::Floor,
            invert: false,
            wavelength: 12.0,
            dune_height: CLICK_UNITS,
            rise: 0,
        }
    }
}

/// Smallest and largest brush radius, clicks.
pub const MIN_RADIUS: f64 = 1.0;
/// Largest brush radius, clicks.
pub const MAX_RADIUS: f64 = 24.0;
/// Shortest dune wavelength, clicks: four samples of the finest lattice.
pub const MIN_WAVELENGTH: f64 = 4.0;
/// Longest dune wavelength, clicks.
pub const MAX_WAVELENGTH: f64 = 48.0;
/// Grid units one dab of full strength raises the ground at the brush
/// centre.
pub const RAISE_PER_DAB: f64 = 32.0;
/// Auto divides a cell when the field leaves its plane by more than this:
/// one fine step, which is also the most that rounding the field to the
/// lattice can account for.
pub const AUTO_TOLERANCE: i32 = FINE_UNITS;
/// How far a floor stays under its own ceiling, and a ceiling over its
/// floor: a body (1024 editor units tall) fits.
pub const HEADROOM: i32 = 4 * CLICK_UNITS;

/// What a stroke did, for the status line and the Inspector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SculptReport {
    /// Leaves in the room before the stroke and now.
    pub leaves: [usize; 2],
    /// Leaf corners that moved.
    pub moved: usize,
    /// Lattice vertices the brush reached with a non-zero change.
    pub vertices: usize,
    /// Largest change of any corner, grid units (absolute).
    pub peak: i32,
    /// Cells that hold several storeys, left whole.
    pub storeys: usize,
    /// Corners held back by the headroom rule.
    pub clamped: usize,
}

/// A sculpt stroke in flight.
#[derive(Clone, Debug)]
pub struct Sculpt {
    room: usize,
    /// The room as it was when the stroke began.
    base: GridRoom,
    params: SculptParams,
    /// Dab centres, click coordinates.
    dabs: Vec<[f64; 2]>,
    /// The ground height at the first dab, grid units (Flatten's level,
    /// Ramp's A end).
    anchor: Option<f64>,
}

impl Sculpt {
    /// Begin a stroke in room `room` at `at` (click coordinates; a click is
    /// a quarter sector). `None` when the room does not exist.
    pub fn begin(
        world: &GridWorld,
        room: usize,
        at: [f64; 2],
        params: SculptParams,
    ) -> Option<Self> {
        let base = world.rooms.get(room)?.clone();
        let mut stroke = Self {
            room,
            base,
            params,
            dabs: Vec::new(),
            anchor: None,
        };
        stroke.anchor = stroke.surface_at(at);
        stroke.dabs.push(at);
        Some(stroke)
    }

    /// The room being sculpted.
    pub fn room(&self) -> usize {
        self.room
    }

    /// The parameters.
    pub fn params(&self) -> &SculptParams {
        &self.params
    }

    /// Change the parameters between updates (the option row); the next
    /// [`Self::apply`] replays the whole stroke with them.
    pub fn set_params(&mut self, params: SculptParams) {
        let target_changed = params.target != self.params.target;
        self.params = params;
        if target_changed {
            let first = self.dabs.first().copied();
            self.anchor = first.and_then(|at| self.surface_at(at));
        }
    }

    /// The dab centres so far.
    pub fn dabs(&self) -> &[[f64; 2]] {
        &self.dabs
    }

    /// The room as it was before the stroke.
    pub fn base(&self) -> &GridRoom {
        &self.base
    }

    /// Clicks between dabs along a stroke.
    pub fn spacing(&self) -> f64 {
        (self.params.radius / 4.0).max(0.25)
    }

    /// Add a dab at `at` when it is a spacing away from the last one (or
    /// `force`). Returns whether one was added.
    pub fn add_dab(&mut self, at: [f64; 2], force: bool) -> bool {
        let last = self.dabs.last().copied().unwrap_or(at);
        let far = (at[0] - last[0]).hypot(at[1] - last[1]) >= self.spacing();
        if force || far {
            self.dabs.push(at);
            true
        } else {
            false
        }
    }

    /// Whether the stroke needs a direction it does not have yet (Dunes and
    /// Ramp read it from the drag).
    pub fn has_direction(&self) -> bool {
        self.direction().is_some()
    }

    /// The drag from the first dab to the last, click coordinates.
    pub fn drag(&self) -> Option<([f64; 2], [f64; 2])> {
        Some((*self.dabs.first()?, *self.dabs.last()?))
    }

    fn direction(&self) -> Option<[f64; 2]> {
        let (a, b) = self.drag()?;
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        (len >= 0.5).then(|| [(b[0] - a[0]) / len, (b[1] - a[1]) / len])
    }

    /// The ground height at a point of the base room, grid units.
    fn surface_at(&self, at: [f64; 2]) -> Option<f64> {
        surface_height(&self.base, self.params.target, at)
    }

    /// Put the room back to the base and replay every dab. Safe to call any
    /// number of times, in any order.
    pub fn apply(&self, world: &mut GridWorld) -> SculptReport {
        let Some(room) = world.rooms.get_mut(self.room) else {
            return SculptReport::default();
        };
        *room = self.base.clone();
        let before = room.leaf_count();
        let mut report = SculptReport {
            leaves: [before, before],
            ..SculptReport::default()
        };
        let delta = self.deltas();
        report.vertices = delta.len();
        if delta.is_empty() {
            return report;
        }
        self.divide(room, &delta, &mut report);
        self.write(room, &delta, &mut report);
        tree::normalize(room);
        report.leaves[1] = room.leaf_count();
        report
    }

    // ---- the delta field -------------------------------------------------

    /// Lattice vertices a brush at `at` can reach.
    fn disc(&self, at: [f64; 2]) -> impl Iterator<Item = [i32; 2]> {
        let r = self.params.radius.clamp(MIN_RADIUS, MAX_RADIUS);
        let (x0, x1) = ((at[0] - r).floor() as i32, (at[0] + r).ceil() as i32);
        let (z0, z1) = ((at[1] - r).floor() as i32, (at[1] + r).ceil() as i32);
        (z0..=z1).flat_map(move |z| {
            (x0..=x1)
                .filter(move |&x| (f64::from(x) - at[0]).hypot(f64::from(z) - at[1]) < r)
                .map(move |x| [x, z])
        })
    }

    fn radius(&self) -> f64 {
        self.params.radius.clamp(MIN_RADIUS, MAX_RADIUS)
    }

    /// The brush weight of lattice vertex `v` for a dab at `at`.
    fn weight(&self, v: [i32; 2], at: [f64; 2]) -> f64 {
        let d = (f64::from(v[0]) - at[0]).hypot(f64::from(v[1]) - at[1]);
        self.params.falloff.weight(d / self.radius())
    }

    /// Quantised changes, grid units, of the lattice vertices the stroke
    /// moves.
    fn deltas(&self) -> BTreeMap<[i32; 2], i32> {
        let p = &self.params;
        let sign = if p.invert { -1.0 } else { 1.0 };
        let target = p.target;
        let mut base: HashMap<[i32; 2], Option<f64>> = HashMap::new();
        let ground = |v: [i32; 2], base: &mut HashMap<[i32; 2], Option<f64>>| {
            *base
                .entry(v)
                .or_insert_with(|| vertex_height(&self.base, target, v))
        };
        let mut d: HashMap<[i32; 2], f64> = HashMap::new();
        match p.mode {
            SculptMode::Raise => {
                for &at in &self.dabs {
                    for v in self.disc(at) {
                        if ground(v, &mut base).is_none() {
                            continue;
                        }
                        let w = self.weight(v, at);
                        *d.entry(v).or_insert(0.0) += sign * RAISE_PER_DAB * p.strength * w;
                    }
                }
            }
            SculptMode::Smooth | SculptMode::Flatten => {
                let level = self.anchor.unwrap_or(0.0);
                let k0 = p.strength.clamp(0.0, 1.0);
                for &at in &self.dabs {
                    let vs: Vec<[i32; 2]> = self.disc(at).collect();
                    let mut step: Vec<([i32; 2], f64)> = Vec::new();
                    for v in vs {
                        let Some(here) = ground(v, &mut base) else {
                            continue;
                        };
                        let cur = here + d.get(&v).copied().unwrap_or(0.0);
                        let goal = if p.mode == SculptMode::Flatten {
                            level
                        } else {
                            let mut sum = 0.0;
                            let mut n = 0.0;
                            for o in [[1, 0], [-1, 0], [0, 1], [0, -1]] {
                                let u = [v[0] + o[0], v[1] + o[1]];
                                if let Some(h) = ground(u, &mut base) {
                                    sum += h + d.get(&u).copied().unwrap_or(0.0);
                                    n += 1.0;
                                }
                            }
                            if n == 0.0 {
                                continue;
                            }
                            sum / n
                        };
                        step.push((v, (goal - cur) * k0 * self.weight(v, at)));
                    }
                    for (v, change) in step {
                        *d.entry(v).or_insert(0.0) += change;
                    }
                }
            }
            SculptMode::Ramp => {
                if let (Some((a, b)), Some(h_a)) = (self.drag(), self.anchor) {
                    let h_b = self.surface_at(b).unwrap_or(h_a) + sign * f64::from(p.rise);
                    let ab = [b[0] - a[0], b[1] - a[1]];
                    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
                    let mut verts: BTreeSet<[i32; 2]> = BTreeSet::new();
                    let steps = (len2.sqrt() / self.spacing()).ceil().max(1.0) as usize;
                    for i in 0..=steps {
                        let t = i as f64 / steps as f64;
                        let at = [a[0] + ab[0] * t, a[1] + ab[1] * t];
                        verts.extend(self.disc(at));
                    }
                    for v in verts {
                        let Some(here) = ground(v, &mut base) else {
                            continue;
                        };
                        let rel = [f64::from(v[0]) - a[0], f64::from(v[1]) - a[1]];
                        let t = if len2 > 1e-9 {
                            ((rel[0] * ab[0] + rel[1] * ab[1]) / len2).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let nearest = [a[0] + ab[0] * t, a[1] + ab[1] * t];
                        let dist =
                            (f64::from(v[0]) - nearest[0]).hypot(f64::from(v[1]) - nearest[1]);
                        let w = p.falloff.weight(dist / self.radius());
                        if w > 0.0 {
                            let goal = h_a + t * (h_b - h_a);
                            d.insert(v, (goal - here) * w);
                        }
                    }
                }
            }
            SculptMode::Dunes => {
                let a = self.dabs.first().copied().unwrap_or([0.0, 0.0]);
                let dir = self.direction().unwrap_or([1.0, 0.0]);
                let lambda = p.wavelength.max(MIN_WAVELENGTH);
                let mut verts: BTreeSet<[i32; 2]> = BTreeSet::new();
                for &at in &self.dabs {
                    verts.extend(self.disc(at));
                }
                for v in verts {
                    if ground(v, &mut base).is_none() {
                        continue;
                    }
                    let w = self
                        .dabs
                        .iter()
                        .map(|&at| self.weight(v, at))
                        .fold(0.0, f64::max);
                    let along =
                        (f64::from(v[0]) - a[0]) * dir[0] + (f64::from(v[1]) - a[1]) * dir[1];
                    let wave = (core::f64::consts::TAU * along / lambda).sin();
                    d.insert(v, sign * w * f64::from(p.dune_height) * wave);
                }
            }
        }
        let step = f64::from(FINE_UNITS);
        d.into_iter()
            .filter_map(|(v, x)| {
                let q = (x / step).round() as i32 * FINE_UNITS;
                (q != 0).then_some((v, q))
            })
            .collect()
    }

    // ---- the leaves ------------------------------------------------------

    /// The leaves whose square the brush reaches.
    fn reached(&self, room: &GridRoom) -> BTreeSet<LeafKey> {
        let r = self.radius();
        let mut out = BTreeSet::new();
        let mut seen: BTreeSet<[i32; 2]> = BTreeSet::new();
        for &at in &self.dabs {
            let (x0, x1) = ((at[0] - r).floor() as i32, (at[0] + r).ceil() as i32);
            let (z0, z1) = ((at[1] - r).floor() as i32, (at[1] + r).ceil() as i32);
            for z in z0..=z1 {
                for x in x0..=x1 {
                    if !seen.insert([x, z]) {
                        continue;
                    }
                    let dx = (f64::from(x) - at[0])
                        .max(0.0)
                        .max(at[0] - f64::from(x + 1));
                    let dz = (f64::from(z) - at[1])
                        .max(0.0)
                        .max(at[1] - f64::from(z + 1));
                    if dx.hypot(dz) < r {
                        if let Some((key, _)) = room.leaf_at([x, z]) {
                            out.insert(key);
                        }
                    }
                }
            }
        }
        out
    }

    /// Take the cells under the brush to the detail's level, then (Auto)
    /// divide the ones whose new surface needs it.
    fn divide(
        &self,
        room: &mut GridRoom,
        delta: &BTreeMap<[i32; 2], i32>,
        report: &mut SculptReport,
    ) {
        let level = self.params.detail.level();
        let edge = super::super::leaf::edge_clicks(level);
        let r = self.radius();
        let mut cells: BTreeSet<[i32; 2]> = BTreeSet::new();
        for &at in &self.dabs {
            let c0 = [
                ((at[0] - r) / f64::from(edge)).floor() as i32,
                ((at[1] - r) / f64::from(edge)).floor() as i32,
            ];
            let c1 = [
                ((at[0] + r) / f64::from(edge)).floor() as i32,
                ((at[1] + r) / f64::from(edge)).floor() as i32,
            ];
            for cz in c0[1]..=c1[1] {
                for cx in c0[0]..=c1[0] {
                    let (x0, z0) = (f64::from(cx * edge), f64::from(cz * edge));
                    let dx = (x0 - at[0]).max(0.0).max(at[0] - (x0 + f64::from(edge)));
                    let dz = (z0 - at[1]).max(0.0).max(at[1] - (z0 + f64::from(edge)));
                    if dx.hypot(dz) < r {
                        cells.insert([cx, cz]);
                    }
                }
            }
        }
        let stacked = storey_sectors(room);
        let mut storeys: BTreeSet<[i32; 2]> = BTreeSet::new();
        for [cx, cz] in cells {
            let click = [cx * edge, cz * edge];
            let sector = [
                click[0].div_euclid(CLICKS_PER_SECTOR),
                click[1].div_euclid(CLICKS_PER_SECTOR),
            ];
            if level > SECTOR_LEVEL && stacked.contains(&sector) {
                storeys.insert(sector);
                continue;
            }
            // Balance would divide a storey column that a quarter cell
            // touches: keep the cells that touch it to halves.
            let level = if level == FINEST_LEVEL
                && touches_storey(&stacked, parent_rect(LeafKey::containing(level, click)))
            {
                level - 1
            } else {
                level
            };
            let _ = tree::subdivide_to(room, click, level);
        }
        if level == SECTOR_LEVEL {
            // Sector detail never divides a column of storeys either, and a
            // cell of several sectors is brought to sectors (which is a
            // plain subdivision). Count the columns the brush reached.
            for key in self.reached(room) {
                let sector = key.cell();
                if stacked.contains(&sector) {
                    storeys.insert(sector);
                }
            }
        }
        if self.params.detail == Detail::Auto {
            for _ in 0..(FINEST_LEVEL - SECTOR_LEVEL) {
                let split: Vec<LeafKey> = self
                    .reached(room)
                    .into_iter()
                    .filter(|k| k.level < FINEST_LEVEL)
                    .filter(|k| room.leaf(*k).is_some_and(GridSector::is_single_span))
                    .filter(|k| {
                        k.level + 1 < FINEST_LEVEL || !touches_storey(&stacked, leaf_rect(*k))
                    })
                    .filter(|k| needs_detail(*k, delta))
                    .collect();
                if split.is_empty() {
                    break;
                }
                for key in split {
                    let _ = tree::subdivide(room, key);
                }
            }
        }
        report.storeys = storeys.len();
    }

    /// Write the deltas into the corners of every leaf that has one.
    fn write(
        &self,
        room: &mut GridRoom,
        delta: &BTreeMap<[i32; 2], i32>,
        report: &mut SculptReport,
    ) {
        let mut keys: BTreeSet<LeafKey> = BTreeSet::new();
        for v in delta.keys() {
            for dz in [-1, 0] {
                for dx in [-1, 0] {
                    if let Some((key, _)) = room.leaf_at([v[0] + dx, v[1] + dz]) {
                        keys.insert(key);
                    }
                }
            }
        }
        let target = self.params.target;
        for key in keys {
            let corners = leaf_corners(key);
            let change = corners.map(|v| delta.get(&v).copied().unwrap_or(0));
            if change == [0; 4] {
                continue;
            }
            let Some(sector) = room.leaf_mut(key) else {
                continue;
            };
            let (floor, ceiling) = sector.ground_floor_and_ceiling_mut();
            let (surface, other) = match target {
                Target::Floor => (floor, ceiling.map(|c| c.clone())),
                Target::Ceiling => (ceiling, floor.map(|f| f.clone())),
            };
            let Some(surface) = surface else { continue };
            let limit = other.map(|o| {
                let both = |pick: fn(i32, i32) -> i32| {
                    let b = o.heights_b.unwrap_or(o.heights);
                    core::array::from_fn::<i32, 4, _>(|i| pick(o.heights[i], b[i]))
                };
                match target {
                    Target::Floor => both(i32::min).map(|h| h - HEADROOM),
                    Target::Ceiling => both(i32::max).map(|h| h + HEADROOM),
                }
            });
            for i in 0..4 {
                if change[i] == 0 {
                    continue;
                }
                let mut moved = false;
                for heights in [Some(&mut surface.heights), surface.heights_b.as_mut()]
                    .into_iter()
                    .flatten()
                {
                    let old = heights[i];
                    let mut new = old + change[i];
                    if let Some(limit) = limit {
                        new = match target {
                            Target::Floor => new.min(limit[i].max(old)),
                            Target::Ceiling => new.max(limit[i].min(old)),
                        };
                        report.clamped += usize::from(new != old + change[i]);
                    }
                    if new != old {
                        heights[i] = new;
                        moved = true;
                        report.peak = report.peak.max((new - old).abs());
                    }
                }
                report.moved += usize::from(moved);
            }
            auto_split::resplit(surface);
        }
    }
}

/// The sectors that hold several storeys.
fn storey_sectors(room: &GridRoom) -> BTreeSet<[i32; 2]> {
    room.leaves()
        .filter(|(_, s)| !s.is_single_span())
        .map(|(k, _)| k.cell())
        .collect()
}

/// A leaf's square in clicks, `[x0, z0, x1, z1]` (end exclusive).
fn leaf_rect(key: LeafKey) -> [i32; 4] {
    let [x1, z1] = key.end_click();
    [key.x, key.z, x1, z1]
}

/// The square of the leaf one level up that holds `key`.
fn parent_rect(key: LeafKey) -> [i32; 4] {
    key.parent().map_or_else(|| leaf_rect(key), leaf_rect)
}

/// Whether a column of storeys touches (or lies in) the square `rect`
/// grown by one click.
fn touches_storey(stacked: &BTreeSet<[i32; 2]>, rect: [i32; 4]) -> bool {
    let (x0, z0) = (
        (rect[0] - 1).div_euclid(CLICKS_PER_SECTOR),
        (rect[1] - 1).div_euclid(CLICKS_PER_SECTOR),
    );
    let (x1, z1) = (
        rect[2].div_euclid(CLICKS_PER_SECTOR),
        rect[3].div_euclid(CLICKS_PER_SECTOR),
    );
    (z0..=z1).any(|z| (x0..=x1).any(|x| stacked.contains(&[x, z])))
}

/// The four lattice vertices of a leaf, `[NW, NE, SE, SW]`.
fn leaf_corners(key: LeafKey) -> [[i32; 2]; 4] {
    let n = key.edge_clicks();
    [
        [key.x, key.z],
        [key.x + n, key.z],
        [key.x + n, key.z + n],
        [key.x, key.z + n],
    ]
}

/// Whether the field leaves the plane of leaf `key`'s corners by more than
/// [`AUTO_TOLERANCE`] at some lattice point inside it.
fn needs_detail(key: LeafKey, delta: &BTreeMap<[i32; 2], i32>) -> bool {
    let n = key.edge_clicks();
    let at = |v: [i32; 2]| f64::from(delta.get(&v).copied().unwrap_or(0));
    let c = leaf_corners(key).map(at);
    for dz in 0..=n {
        for dx in 0..=n {
            let (u, w) = (f64::from(dx) / f64::from(n), f64::from(dz) / f64::from(n));
            let plane = (1.0 - u) * (1.0 - w) * c[0]
                + u * (1.0 - w) * c[1]
                + u * w * c[2]
                + (1.0 - u) * w * c[3];
            if (at([key.x + dx, key.z + dz]) - plane).abs() > f64::from(AUTO_TOLERANCE) {
                return true;
            }
        }
    }
    false
}

/// The surface of `target` in a leaf's ground span.
fn leaf_surface(sector: &GridSector, target: Target) -> Option<&GridSurface> {
    match target {
        Target::Floor => sector.ground_floor(),
        Target::Ceiling => sector.ground_ceiling(),
    }
}

/// The height of the surface of leaf `key` at the grid-unit point
/// `(gx, gz)`, with the same triangle interpolation the collision uses.
fn leaf_height_at(
    sector: &GridSector,
    key: LeafKey,
    target: Target,
    gx: i32,
    gz: i32,
) -> Option<i32> {
    let surface = leaf_surface(sector, target)?;
    let edge = key.edge_clicks() * CLICK_UNITS;
    let clamp = |v: i32, origin: i32| (v - origin).clamp(0, edge) as u32;
    let (ox, oz) = (
        clamp(gx, key.x * CLICK_UNITS),
        clamp(gz, key.z * CLICK_UNITS),
    );
    let nesw = matches!(surface.split, GridSplit::NeSw);
    let second = !psx_grid::height::in_first_triangle(nesw, ox, oz, edge as u32);
    let inv24 = psx_grid::height::weight_recip(edge as u32);
    Some(psx_grid::height::height_at_i32(
        surface.heights_for(usize::from(second)),
        nesw,
        ox,
        oz,
        edge as u32,
        inv24,
    ))
}

/// The height of the surface at lattice vertex `v`: the mean over the
/// distinct leaves that meet there (they agree unless there is a cliff).
fn vertex_height(room: &GridRoom, target: Target, v: [i32; 2]) -> Option<f64> {
    let mut seen: BTreeSet<LeafKey> = BTreeSet::new();
    let (mut sum, mut n) = (0.0, 0.0);
    for dz in [-1, 0] {
        for dx in [-1, 0] {
            let Some((key, sector)) = room.leaf_at([v[0] + dx, v[1] + dz]) else {
                continue;
            };
            if !seen.insert(key) {
                continue;
            }
            if let Some(h) =
                leaf_height_at(sector, key, target, v[0] * CLICK_UNITS, v[1] * CLICK_UNITS)
            {
                sum += f64::from(h);
                n += 1.0;
            }
        }
    }
    (n > 0.0).then(|| sum / n)
}

/// The height of the surface at click coordinates `at`, grid units.
fn surface_height(room: &GridRoom, target: Target, at: [f64; 2]) -> Option<f64> {
    let click = [at[0].floor() as i32, at[1].floor() as i32];
    let (key, sector) = room.leaf_at(click)?;
    leaf_height_at(
        sector,
        key,
        target,
        (at[0] * f64::from(CLICK_UNITS)).round() as i32,
        (at[1] * f64::from(CLICK_UNITS)).round() as i32,
    )
    .map(f64::from)
}

/// The ground height under click coordinates `at` in room `room`, for the
/// brush ghost to rest on. Grid units, relative to the room's offset.
pub fn height_under(world: &GridWorld, room: usize, target: Target, at: [f64; 2]) -> Option<f64> {
    surface_height(world.rooms.get(room)?, target, at)
}

/// A whole stroke at once along `path` (click coordinates), the way the MCP
/// and the tests drive it. Dabs are laid a spacing apart.
pub fn sculpt_path(
    world: &mut GridWorld,
    room: usize,
    path: &[[f64; 2]],
    params: SculptParams,
) -> Option<SculptReport> {
    let first = *path.first()?;
    let mut stroke = Sculpt::begin(world, room, first, params)?;
    for pair in path.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        let steps = (len / stroke.spacing()).ceil().max(1.0) as usize;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            stroke.add_dab([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], false);
        }
    }
    Some(stroke.apply(world))
}
