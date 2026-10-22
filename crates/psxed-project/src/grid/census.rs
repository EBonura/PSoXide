//! Cell census and flood sweeps over a cooked level.
//!
//! A cooked grid level is a set of cells (rooms) joined by portals. These
//! reads say how big the cells are, how the portals are spread, and what the
//! budgeted flood ([`psx_grid::flood`]) reaches from a spread of camera
//! positions, so a level can be sized (pages, pool, boundary distance) before
//! a disc is built. The same reads feed the cook gate ([`super::cook`]) and
//! the editor's `grid_cells` and `grid_flood` tools.
//!
//! Everything here runs on the host, over the cooked bytes the guest reads.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
use std::fmt::Write as _;

pub use psx_grid::flood::NO_PAGE_BUDGET;
use psx_grid::flood::{Flood, FloodQuery, FloodStats};
use psx_grid::format::{Portal, PORTAL_VERTICAL};
use psx_grid::stream::{plan_pool, StreamWindow, Want};
use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, RoomView};

/// Bytes in a pool page.
const PAGE_KIB: usize = 2;

/// Aligned copies of a cooked core and its room chunks, which the zero-copy
/// readers need.
pub struct LevelBlobs {
    core: Vec<u32>,
    core_len: usize,
    rooms: Vec<(Vec<u32>, usize)>,
}

impl LevelBlobs {
    /// Copy `core` and `chunks` (one per room) into aligned memory.
    pub fn new(core: &[u8], chunks: &[Vec<u8>]) -> Self {
        Self {
            core: aligned_copy(core),
            core_len: core.len(),
            rooms: chunks
                .iter()
                .map(|chunk| (aligned_copy(chunk), chunk.len()))
                .collect(),
        }
    }

    /// The level core.
    ///
    /// # Panics
    /// If the core does not parse; the cook only hands over bytes it wrote.
    pub fn view(&self) -> LevelView<'_> {
        LevelView::parse(words_as_bytes(&self.core, self.core_len))
            .expect("a cooked level core parses")
    }

    /// Room `room`'s chunk.
    ///
    /// # Panics
    /// If the chunk does not parse; the cook only hands over bytes it wrote.
    pub fn room(&self, room: usize) -> RoomView<'_> {
        let (words, len) = &self.rooms[room];
        RoomView::parse(words_as_bytes(words, *len)).expect("a cooked room chunk parses")
    }

    /// Pages (2 KiB) of every room chunk, in room order.
    pub fn pages(&self) -> Vec<usize> {
        self.rooms.iter().map(|(_, len)| len.div_ceil(2048)).collect()
    }

    /// Rooms in the level.
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }
}

/// How a portal opens, from its geometry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PortalKind {
    /// A floor or ceiling opening.
    Vertical,
    /// A side opening exactly one sector tall: open ground between cells.
    OpenAir,
    /// Any other side opening: a door, an arch, a gap in a wall.
    Opening,
}

impl PortalKind {
    /// The kind of `portal` in a level whose sector is `sector` units.
    pub fn of(portal: &Portal, sector: i32) -> Self {
        if portal.flags & PORTAL_VERTICAL != 0 {
            Self::Vertical
        } else if i32::from(portal.v[3][1]) - i32::from(portal.v[0][1]) == sector {
            Self::OpenAir
        } else {
            Self::Opening
        }
    }

    /// A lower case name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::OpenAir => "open-air",
            Self::Opening => "opening",
        }
    }
}

pub use psx_grid::walk::centre as portal_centre;

/// Minimum, mean, median and maximum of a sample.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Spread {
    pub min: f64,
    pub mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
}

impl Spread {
    /// The spread of `values` (all zero for none).
    pub fn of(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
        Self {
            min: sorted[0],
            mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p50: at(0.5),
            p90: at(0.9),
            max: sorted[sorted.len() - 1],
        }
    }
}

impl std::fmt::Display for Spread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "min {:.1} mean {:.1} p50 {:.1} p90 {:.1} max {:.1}",
            self.min, self.mean, self.p50, self.p90, self.max
        )
    }
}

/// Sectors a room owns, as world centres `[x, z]` in runtime units.
pub fn owned_sectors(level: &LevelView<'_>, room: usize) -> Vec<[i32; 2]> {
    let record = &level.rooms[room];
    let s = level.sector_units() as i32;
    let mut out = Vec::new();
    for z in 0..usize::from(record.depth) {
        for x in 0..usize::from(record.width) {
            if level.room_owns_cell(room, x, z) {
                out.push([
                    record.origin[0] + x as i32 * s + s / 2,
                    record.origin[2] + z as i32 * s + s / 2,
                ]);
            }
        }
    }
    out
}

/// The portal graph with the geometry a census needs.
pub struct Graph {
    pub rooms: usize,
    /// Per portal: the room it leaves, its centre and its kind.
    pub source: Vec<u16>,
    pub centre: Vec<[i32; 3]>,
    pub kind: Vec<PortalKind>,
    /// Which room owns each world sector `(floor(x / s), floor(z / s))`.
    pub owner: HashMap<(i32, i32), u16>,
    /// Quads drawn in each owned world sector.
    pub quads: HashMap<(i32, i32), u32>,
}

impl Graph {
    /// The graph of `blobs`.
    pub fn new(blobs: &LevelBlobs) -> Self {
        let level = blobs.view();
        let s = level.sector_units() as i32;
        let mut source = vec![0u16; level.portals.len()];
        let mut centre = vec![[0i32; 3]; level.portals.len()];
        for (room, record) in level.rooms.iter().enumerate() {
            let first = usize::from(record.portal_first);
            for index in first..first + usize::from(record.portal_count) {
                source[index] = room as u16;
                centre[index] = portal_centre(record, &level.portals[index]);
            }
        }
        let kind = level
            .portals
            .iter()
            .map(|portal| PortalKind::of(portal, s))
            .collect();
        let mut owner = HashMap::new();
        let mut quads = HashMap::new();
        for room in 0..level.rooms.len() {
            let record = &level.rooms[room];
            let view = blobs.room(room);
            let width = usize::from(record.width);
            for z in 0..usize::from(record.depth) {
                for x in 0..width {
                    if !level.room_owns_cell(room, x, z) {
                        continue;
                    }
                    let key = (
                        (record.origin[0] + x as i32 * s).div_euclid(s),
                        (record.origin[2] + z as i32 * s).div_euclid(s),
                    );
                    owner.insert(key, room as u16);
                    let count = view.render.cells.get(z * width + x).map_or(0, |c| c.count);
                    quads.insert(key, u32::from(count));
                }
            }
        }
        Self {
            rooms: level.rooms.len(),
            source,
            centre,
            kind,
            owner,
            quads,
        }
    }
}

/// What a level is made of, cell by cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellCensus {
    pub cells: usize,
    pub owned_sectors: usize,
    pub total_pages: usize,
    pub max_cell_pages: usize,
    pub pages: Spread,
    pub sectors: Spread,
    pub portals: usize,
    pub open_air: usize,
    pub opening: usize,
    pub vertical: usize,
    pub portals_per_cell: Spread,
    /// The level's own streaming rule: ring, walk radii or flood.
    pub rule: String,
    /// Pages of the pool the level's own rule needs, and the rooms and pages
    /// of its busiest wanted set.
    pub pool_pages: usize,
    pub peak_pages: usize,
    pub peak_rooms: usize,
}

impl CellCensus {
    /// The census of `blobs`.
    pub fn of(blobs: &LevelBlobs, graph: &Graph) -> Self {
        let level = blobs.view();
        let pages = blobs.pages();
        let cells = level.rooms.len();
        let sectors: Vec<f64> = (0..cells)
            .map(|room| owned_sectors(&level, room).len() as f64)
            .collect();
        let count = |kind| graph.kind.iter().filter(|&&k| k == kind).count();
        let per_cell: Vec<f64> = level
            .rooms
            .iter()
            .map(|r| f64::from(r.portal_count))
            .collect();
        let plan = plan_pool(&level, &pages);
        Self {
            cells,
            owned_sectors: graph.owner.len(),
            total_pages: pages.iter().sum(),
            max_cell_pages: pages.iter().copied().max().unwrap_or(0),
            pages: Spread::of(&pages.iter().map(|&p| p as f64).collect::<Vec<_>>()),
            sectors: Spread::of(&sectors),
            portals: level.portals.len(),
            open_air: count(PortalKind::OpenAir),
            opening: count(PortalKind::Opening),
            vertical: count(PortalKind::Vertical),
            portals_per_cell: Spread::of(&per_cell),
            rule: if level.budget_flood().is_some() {
                "flood".into()
            } else if level.ring_stream() {
                "ring".into()
            } else {
                "walk radii".into()
            },
            pool_pages: plan.pool_pages,
            peak_pages: plan.peak_pages,
            peak_rooms: plan.peak_rooms,
        }
    }

    /// The census as the tables of the design note.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let pct = |n: usize| {
            if self.portals == 0 {
                0.0
            } else {
                n as f64 * 100.0 / self.portals as f64
            }
        };
        let _ = writeln!(
            out,
            "cells {}, owned sectors {}, sectors per cell mean {:.1}",
            self.cells, self.owned_sectors, self.sectors.mean
        );
        let _ = writeln!(
            out,
            "pages: total {} ({} KiB), per cell mean {:.1} max {}",
            self.total_pages,
            self.total_pages * PAGE_KIB,
            self.pages.mean,
            self.max_cell_pages
        );
        let _ = writeln!(
            out,
            "portals {} ({:.0}% open-air: {} open-air, {} opening, {} vertical), per cell mean {:.1}",
            self.portals,
            pct(self.open_air),
            self.open_air,
            self.opening,
            self.vertical,
            self.portals_per_cell.mean
        );
        let _ = writeln!(
            out,
            "streaming rule: {}; its pool {} pages ({} KiB): busiest set {} cells, {} pages",
            self.rule,
            self.pool_pages,
            self.pool_pages * PAGE_KIB,
            self.peak_rooms,
            self.peak_pages
        );
        out
    }
}

/// One camera position to measure from.
#[derive(Copy, Clone, Debug)]
pub struct Sample {
    pub room: usize,
    pub at: [i32; 3],
    /// Heading, radians from +X towards +Z.
    pub yaw: f64,
}

/// Deterministic generator of the camera samples (a 64 bit LCG).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

/// Camera positions spread over the level: a few owned sectors per cell
/// (one per cell above 300 cells, three above 50, six below), at mid room
/// height, headings cycled over eight directions. Fixed seed, so two runs
/// over the same level measure the same cameras.
pub fn sample_cameras(level: &LevelView<'_>, seed: u64) -> Vec<Sample> {
    let cells = level.rooms.len();
    let per_cell = if cells > 300 {
        1
    } else if cells > 50 {
        3
    } else {
        6
    };
    let mut rng = Lcg(seed);
    let mut out: Vec<Sample> = Vec::new();
    for room in 0..cells {
        let owned = owned_sectors(level, room);
        if owned.is_empty() {
            continue;
        }
        let record = &level.rooms[room];
        let mid = i32::from(record.min_y) / 2 + i32::from(record.max_y) / 2 + record.origin[1];
        for i in 0..per_cell {
            let [x, z] = owned[(rng.next() as usize) % owned.len()];
            let yaw = ((out.len() + i) % 8) as f64 * FRAC_PI_4;
            out.push(Sample {
                room,
                at: [x, mid, z],
                yaw,
            });
        }
    }
    out
}

/// Distance in sectors along `rays` rays spread over `span` radians about
/// `yaw` to the first owned sector whose cell is not in `set`: the nearest
/// ray, and the mean over the rays that met one.
fn boundary_fan(
    graph: &Graph,
    sector: i32,
    set: &HashSet<u16>,
    at: [i32; 3],
    yaw: f64,
    span: f64,
    rays: i32,
) -> Option<(f64, f64)> {
    let s = f64::from(sector);
    let (mut nearest, mut sum, mut hits) = (f64::INFINITY, 0.0, 0.0);
    for i in 0..rays {
        let a = yaw + (f64::from(i) + 0.5 - f64::from(rays) / 2.0) * span / f64::from(rays);
        let (dx, dz) = (a.cos(), a.sin());
        let mut d = 0.5;
        while d < 90.0 {
            let x = (f64::from(at[0]) + dx * d * s).floor() as i32;
            let z = (f64::from(at[2]) + dz * d * s).floor() as i32;
            if let Some(room) = graph.owner.get(&(x.div_euclid(sector), z.div_euclid(sector))) {
                if !set.contains(room) {
                    nearest = nearest.min(d);
                    sum += d;
                    hits += 1.0;
                    break;
                }
            }
            d += 0.5;
        }
    }
    (hits > 0.0).then(|| (nearest, sum / hits))
}

/// Radius in sectors of a disc as big as the owned sectors of `set`.
fn equal_area_radius(level: &LevelView<'_>, set: &HashSet<u16>) -> f64 {
    let sectors: usize = set
        .iter()
        .map(|&room| owned_sectors(level, usize::from(room)).len())
        .sum();
    (sectors as f64 / PI).sqrt()
}

/// Cost of one flood in cycles, by the model of the design note: 80 per
/// portal relaxation, 160 per queue push or pop, 100 per cell added.
pub fn flood_cycles(stats: &FloodStats, cells: usize) -> f64 {
    f64::from(stats.relaxations) * 80.0
        + f64::from(stats.pushes + stats.pops) * 160.0
        + cells as f64 * 100.0
}

/// A flood from `sample` out to `radius` units and `page_budget` pages.
pub fn flood_from(
    flood: &mut Flood,
    level: &LevelView<'_>,
    sample: &Sample,
    radius: i32,
    page_budget: u32,
) -> HashSet<u16> {
    flood.solve(
        level,
        FloodQuery {
            room: sample.room,
            at: sample.at,
            radius,
            page_budget,
        },
    );
    flood.rooms().iter().map(|f| f.room).collect()
}

/// Pages a flood may hold under `ceiling_kib` of pool: the ceiling less the
/// largest cell (the room being installed), never more than the level.
pub fn flood_page_budget(blobs: &LevelBlobs, ceiling_kib: usize) -> usize {
    let pages = blobs.pages();
    let total: usize = pages.iter().sum();
    let largest = pages.iter().copied().max().unwrap_or(0);
    (ceiling_kib / PAGE_KIB).saturating_sub(largest).min(total)
}

/// What the flood to a pool ceiling reaches from a spread of cameras, next to
/// what the level's own ring rule reaches.
#[derive(Clone, Debug, Default)]
pub struct Sweep {
    pub samples: usize,
    pub ceiling_kib: usize,
    pub page_budget: usize,
    pub max_cell_pages: usize,
    /// Cells the ring rule needs, and the pages of the need set and of the
    /// need and lead sets together.
    pub ring_cells: Spread,
    pub ring_need_pages: Spread,
    pub ring_all_pages: Spread,
    pub ring_nearest_boundary: Spread,
    pub ring_equal_area: Spread,
    pub flood_cells: Spread,
    pub flood_pages: Spread,
    pub flood_fill: Spread,
    pub flood_exhausted: usize,
    pub forward_boundary_nearest: Spread,
    pub forward_boundary_mean: Spread,
    pub nearest_boundary: Spread,
    pub equal_area: Spread,
    pub relaxations: Spread,
    pub cycles: Spread,
    pub boundary_portals: Spread,
    pub boundary_portals_in_cone: Spread,
}

impl Sweep {
    /// Sweep `blobs` with `samples`.
    pub fn run(blobs: &LevelBlobs, graph: &Graph, samples: &[Sample], ceiling_kib: usize) -> Self {
        let level = blobs.view();
        let sector = level.sector_units() as i32;
        let page_budget = flood_page_budget(blobs, ceiling_kib);
        let mut window = StreamWindow::new(&level);
        let mut wanted = Vec::new();
        let mut flood = Flood::new(&level);
        let pages = blobs.pages();
        let page_sum = |set: &HashSet<u16>| -> f64 {
            set.iter().map(|&r| pages[usize::from(r)]).sum::<usize>() as f64
        };
        let (mut ring_cells, mut ring_need, mut ring_all, mut ring_near, mut ring_area) =
            (vec![], vec![], vec![], vec![], vec![]);
        let (mut cells, mut pages_v, mut fill, mut fwd_near, mut fwd_mean, mut near, mut area) =
            (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
        let (mut relax, mut cycles, mut bports, mut bcone) = (vec![], vec![], vec![], vec![]);
        let mut exhausted = 0;
        for sample in samples {
            window.invalidate();
            window.wanted(
                &level,
                Some((sample.room, sample.at)),
                Some((sample.room, sample.at)),
                &mut wanted,
            );
            let need: HashSet<u16> = wanted
                .iter()
                .filter(|w| w.want == Want::Need)
                .map(|w| w.room)
                .collect();
            let all: HashSet<u16> = wanted.iter().map(|w| w.room).collect();
            ring_cells.push(need.len() as f64);
            ring_need.push(page_sum(&need));
            ring_all.push(page_sum(&all));
            if let Some((n, _)) = boundary_fan(
                graph,
                sector,
                &need,
                sample.at,
                0.0,
                2.0 * PI,
                36,
            ) {
                ring_near.push(n);
            }
            ring_area.push(equal_area_radius(&level, &need));

            let set = flood_from(
                &mut flood,
                &level,
                sample,
                i32::MAX,
                page_budget as u32,
            );
            let stats = flood.stats();
            if stats.end == psx_grid::flood::FloodEnd::Exhausted {
                exhausted += 1;
            }
            cells.push(set.len() as f64);
            pages_v.push(f64::from(stats.pages));
            fill.push(f64::from(stats.pages) / page_budget.max(1) as f64 * 100.0);
            if let Some((n, m)) =
                boundary_fan(graph, sector, &set, sample.at, sample.yaw, FRAC_PI_2, 9)
            {
                fwd_near.push(n);
                fwd_mean.push(m);
            }
            if let Some((n, _)) = boundary_fan(graph, sector, &set, sample.at, 0.0, 2.0 * PI, 36) {
                near.push(n);
            }
            area.push(equal_area_radius(&level, &set));
            relax.push(f64::from(stats.relaxations));
            cycles.push(flood_cycles(&stats, set.len()));
            let (mut total, mut cone) = (0.0, 0.0);
            for &room in &set {
                let record = &level.rooms[usize::from(room)];
                let first = usize::from(record.portal_first);
                for index in first..first + usize::from(record.portal_count) {
                    if set.contains(&level.portals[index].dest) {
                        continue;
                    }
                    total += 1.0;
                    let c = graph.centre[index];
                    let angle = (f64::from(c[2] - sample.at[2]).atan2(f64::from(c[0] - sample.at[0]))
                        - sample.yaw
                        + 3.0 * PI)
                        % (2.0 * PI)
                        - PI;
                    if angle.abs() < 0.95 {
                        cone += 1.0;
                    }
                }
            }
            bports.push(total);
            bcone.push(cone);
        }
        Self {
            samples: samples.len(),
            ceiling_kib,
            page_budget,
            max_cell_pages: pages.iter().copied().max().unwrap_or(0),
            ring_cells: Spread::of(&ring_cells),
            ring_need_pages: Spread::of(&ring_need),
            ring_all_pages: Spread::of(&ring_all),
            ring_nearest_boundary: Spread::of(&ring_near),
            ring_equal_area: Spread::of(&ring_area),
            flood_cells: Spread::of(&cells),
            flood_pages: Spread::of(&pages_v),
            flood_fill: Spread::of(&fill),
            flood_exhausted: exhausted,
            forward_boundary_nearest: Spread::of(&fwd_near),
            forward_boundary_mean: Spread::of(&fwd_mean),
            nearest_boundary: Spread::of(&near),
            equal_area: Spread::of(&area),
            relaxations: Spread::of(&relax),
            cycles: Spread::of(&cycles),
            boundary_portals: Spread::of(&bports),
            boundary_portals_in_cone: Spread::of(&bcone),
        }
    }

    /// The sweep as text.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} cameras; flood to a {} KiB ceiling: budget {} pages (ceiling less the largest cell, {} pages), whole level reached in {} of them",
            self.samples, self.ceiling_kib, self.page_budget, self.max_cell_pages, self.flood_exhausted
        );
        let _ = writeln!(
            out,
            "ring rule: need cells {}; need pages {}; need+lead pages {}",
            self.ring_cells, self.ring_need_pages, self.ring_all_pages
        );
        let _ = writeln!(
            out,
            "  boundary of the need set, nearest in any direction (sectors) {}; equal-area radius {}",
            self.ring_nearest_boundary, self.ring_equal_area
        );
        let _ = writeln!(
            out,
            "flood: resident cells {}; pages {}; pool fill % {}",
            self.flood_cells, self.flood_pages, self.flood_fill
        );
        let _ = writeln!(
            out,
            "  boundary, forward 90 degree fan: nearest ray {}; mean ray {}",
            self.forward_boundary_nearest, self.forward_boundary_mean
        );
        let _ = writeln!(
            out,
            "  boundary, nearest in any direction {}; equal-area radius {}",
            self.nearest_boundary, self.equal_area
        );
        let _ = writeln!(
            out,
            "  relaxations {}; model cycles {}",
            self.relaxations, self.cycles
        );
        let _ = writeln!(
            out,
            "  boundary portals {}; in a 110 degree cone {}",
            self.boundary_portals, self.boundary_portals_in_cone
        );
        out
    }
}

/// What the flood stopping at `res_sectors` reaches, for sizing the pool:
/// the busiest set over the cameras plus the largest cell.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResidentRadius {
    pub res_sectors: f64,
    pub cells_mean: f64,
    pub pages_mean: f64,
    pub peak_pages: usize,
    pub pool_kib: usize,
    pub relaxations_mean: f64,
}

/// [`ResidentRadius`] for each radius in `radii`, over two random owned
/// sectors per cell (the camera positions of the design note; the random
/// stream runs on from one radius to the next).
pub fn resident_radii(blobs: &LevelBlobs, ceiling_kib: usize, radii: &[f64]) -> Vec<ResidentRadius> {
    let level = blobs.view();
    let sector = level.sector_units() as i32;
    let page_budget = flood_page_budget(blobs, ceiling_kib) as u32;
    let largest = blobs.pages().iter().copied().max().unwrap_or(0);
    let mut flood = Flood::new(&level);
    // One generator for all rows, so each row measures cameras of its own.
    let mut rng = Lcg(31337);
    radii
        .iter()
        .map(|&res| {
            let (mut cells, mut pages, mut relax, mut peak) = (vec![], vec![], vec![], 0usize);
            for room in 0..level.rooms.len() {
                let owned = owned_sectors(&level, room);
                if owned.is_empty() {
                    continue;
                }
                for _ in 0..2 {
                    let [x, z] = owned[(rng.next() as usize) % owned.len()];
                    let record = &level.rooms[room];
                    let mid = i32::from(record.min_y) / 2
                        + i32::from(record.max_y) / 2
                        + record.origin[1];
                    flood.solve(
                        &level,
                        FloodQuery {
                            room,
                            at: [x, mid, z],
                            radius: (res * f64::from(sector)) as i32,
                            page_budget,
                        },
                    );
                    let stats = flood.stats();
                    cells.push(flood.rooms().len() as f64);
                    pages.push(f64::from(stats.pages));
                    relax.push(f64::from(stats.relaxations));
                    peak = peak.max(stats.pages as usize);
                }
            }
            let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
            ResidentRadius {
                res_sectors: res,
                cells_mean: mean(&cells),
                pages_mean: mean(&pages),
                peak_pages: peak,
                pool_kib: (peak + largest) * PAGE_KIB,
                relaxations_mean: mean(&relax),
            }
        })
        .collect()
}

/// Quads drawn from `set` in the plan cone of a camera: the owned sectors
/// within `reach` sectors whose centre is within the half angle `half` of the
/// heading (widened by the angular size of the sector itself). A plan cone
/// ignores height and cover, so it is a bound, not a count of what is
/// drawn.
pub fn quads_in_cone(
    graph: &Graph,
    sector: i32,
    set: &HashSet<u16>,
    at: [i32; 3],
    yaw: f64,
    half: f64,
    reach: f64,
) -> u32 {
    let mut total = 0;
    for (key, room) in &graph.owner {
        if !set.contains(room) {
            continue;
        }
        let dx = f64::from(key.0 * sector + sector / 2 - at[0]);
        let dz = f64::from(key.1 * sector + sector / 2 - at[2]);
        let d = (dx * dx + dz * dz).sqrt() / f64::from(sector);
        if d >= reach {
            continue;
        }
        let angle = (dz.atan2(dx) - yaw + 3.0 * PI) % (2.0 * PI) - PI;
        let widen = if d < 1.0 { 9.0 } else { 0.7 / d };
        if angle.abs() - widen <= half {
            total += graph.quads.get(key).copied().unwrap_or(0);
        }
    }
    total
}

/// Half the horizontal field of view the cone counts use: the game's 53
/// degrees plus margin.
pub const CONE_HALF_ANGLE: f64 = 0.62;

/// Quads in the cone of the cell set within `radius_sectors` of each camera,
/// and what the cook gate reports from it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DrawLoad {
    pub quads_mean: f64,
    pub quads_p90: f64,
    pub quads_max: u32,
    pub cells_mean: f64,
    pub worst_room: usize,
}

/// Quads in the plan cone for the flood of `radius_sectors` from each of
/// `samples`, taking the worst of eight headings per camera.
pub fn draw_load(
    blobs: &LevelBlobs,
    graph: &Graph,
    samples: &[Sample],
    radius_sectors: u32,
    reach_sectors: f64,
) -> DrawLoad {
    let level = blobs.view();
    let sector = level.sector_units() as i32;
    let mut flood = Flood::new(&level);
    let (mut worst, mut cells) = (Vec::new(), Vec::new());
    let (mut max, mut worst_room) = (0u32, 0usize);
    for sample in samples {
        let set = flood_from(
            &mut flood,
            &level,
            sample,
            radius_sectors as i32 * sector,
            NO_PAGE_BUDGET,
        );
        let best = (0..8)
            .map(|h| {
                quads_in_cone(
                    graph,
                    sector,
                    &set,
                    sample.at,
                    f64::from(h) * FRAC_PI_4,
                    CONE_HALF_ANGLE,
                    reach_sectors,
                )
            })
            .max()
            .unwrap_or(0);
        if best > max {
            max = best;
            worst_room = sample.room;
        }
        worst.push(f64::from(best));
        cells.push(set.len() as f64);
    }
    let spread = Spread::of(&worst);
    DrawLoad {
        quads_mean: spread.mean,
        quads_p90: spread.p90,
        quads_max: max,
        cells_mean: Spread::of(&cells).mean,
        worst_room,
    }
}

/// The text `grid_flood` replies with for one camera.
pub fn flood_report(
    blobs: &LevelBlobs,
    graph: &Graph,
    sample: &Sample,
    near_sectors: u32,
    res_sectors: u32,
    page_budget: u32,
) -> String {
    let level = blobs.view();
    let sector = level.sector_units() as i32;
    let mut flood = Flood::new(&level);
    let res = flood_from(
        &mut flood,
        &level,
        sample,
        res_sectors as i32 * sector,
        page_budget,
    );
    let list: Vec<_> = flood.rooms().to_vec();
    let stats = flood.stats();
    let near: HashSet<u16> = list
        .iter()
        .filter(|f| f.cost <= near_sectors as i32 * sector)
        .map(|f| f.room)
        .collect();
    let pages = blobs.pages();
    let near_pages: usize = near.iter().map(|&r| pages[usize::from(r)]).sum();
    let boundary: usize = res
        .iter()
        .map(|&room| {
            let record = &level.rooms[usize::from(room)];
            let first = usize::from(record.portal_first);
            (first..first + usize::from(record.portal_count))
                .filter(|&i| !res.contains(&level.portals[i].dest))
                .count()
        })
        .sum();
    let cone = |set: &HashSet<u16>| {
        quads_in_cone(
            graph,
            sector,
            set,
            sample.at,
            sample.yaw,
            CONE_HALF_ANGLE,
            f64::INFINITY,
        )
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "flood from room {} at ({}, {}, {}) heading {:.0} degrees: near {} sectors, resident {} sectors, budget {}",
        sample.room,
        sample.at[0],
        sample.at[1],
        sample.at[2],
        sample.yaw.to_degrees(),
        near_sectors,
        res_sectors,
        if page_budget == NO_PAGE_BUDGET {
            "none".to_string()
        } else {
            format!("{page_budget} pages")
        }
    );
    let _ = writeln!(
        out,
        "resident: {} cells, {} pages ({} KiB); drawn (within near): {} cells, {} pages; ended {:?}",
        res.len(),
        stats.pages,
        stats.pages as usize * PAGE_KIB,
        near.len(),
        near_pages,
        stats.end
    );
    let _ = writeln!(
        out,
        "boundary portals {}; quads in the plan cone: drawn {} , resident {}",
        boundary,
        cone(&near),
        cone(&res)
    );
    let _ = writeln!(
        out,
        "work: {} relaxations, {} pushes, {} pops, about {:.0} cycles by the model",
        stats.relaxations,
        stats.pushes,
        stats.pops,
        flood_cycles(&stats, res.len())
    );
    let mut rows = String::new();
    for f in list.iter().take(40) {
        let _ = write!(
            rows,
            " {}:{:.1}{}",
            f.room,
            f64::from(f.cost) / f64::from(sector),
            if near.contains(&f.room) { "" } else { "r" }
        );
    }
    let _ = writeln!(
        out,
        "cells in flood order, room:cost in sectors (r = resident only):{rows}{}",
        if list.len() > 40 { " ..." } else { "" }
    );
    out
}
