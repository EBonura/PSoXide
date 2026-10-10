//! Cook gates (design 4.3, 3.4): rank-row width, bytes-per-unit-of-travel
//! (`rho`), and the RAM pool. Every threshold is a named parameter with a
//! label; a breach is a [`GateFailure`] with coordinates, never a clamp.
//!
//! `rho`: crossing from region A into B newly requires the bytes of
//! `Need(B)` not in `Need(A)`. Over any path, those bytes arrive faster than
//! the drive can deliver them when `bytes / distance` exceeds
//! `U x B_eff / v_max`. Because crossings that follow each other closely
//! arrive together, the check sums consecutive crossings inside a window of
//! the lead horizon and divides by the window length.

use std::collections::BTreeSet;

use psx_bsp::pxbsp::PXBSP_MAX_VISIBILITY_BYTES;

use super::account::Region;
use super::closure::{BitSet, Closure};
use super::geometry::V3;
use super::graph::RegionGraph;
use super::input::PartitionInput;
use super::{PartitionParams, SECTOR_BYTES};

#[derive(Clone, Debug, PartialEq)]
pub struct RhoEdge {
    pub from: u32,
    pub to: u32,
    pub bytes: u64,
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RhoWindow {
    pub path: Vec<u32>,
    pub bytes: u64,
    /// Window length used for the division, units.
    pub distance: f64,
    pub rho: f64,
}

/// Largest resident requirement found.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolPeak {
    pub horizon: f64,
    pub bytes: u64,
    pub region: u32,
    pub position: V3,
    pub need_regions: usize,
    pub lead_regions: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateFailure {
    /// Payload over a hard limit.
    RegionOverCap {
        region: u32,
        limits: Vec<&'static str>,
        bytes: u32,
        unsplittable: bool,
    },
    /// `|V(R)| x Lcap` bits do not fit one PVS row.
    RankRowTooWide {
        region: u32,
        closure: usize,
        width_bytes: u32,
        limit_bytes: u32,
    },
    /// The shipping collision compiler refused the cell.
    CollisionCompile { region: u32, error: String },
    /// More bytes per unit of travel than the drive delivers.
    RhoExceeded {
        rho: f64,
        limit: f64,
        path: Vec<u32>,
    },
    /// Need plus lead plus home pin exceed the page pool.
    PoolExceeded {
        bytes: u64,
        pool: u32,
        region: u32,
        position: V3,
    },
}

impl GateFailure {
    /// Short stable name of the failure class.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RegionOverCap { .. } => "region-over-cap",
            Self::RankRowTooWide { .. } => "rank-row-too-wide",
            Self::CollisionCompile { .. } => "collision-compile",
            Self::RhoExceeded { .. } => "rho",
            Self::PoolExceeded { .. } => "pool",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GateReport {
    pub v_max: f64,
    pub t_region_ms: f64,
    pub t_region_pessimistic_ms: f64,
    pub b_eff: f64,
    pub b_eff_batched: f64,
    pub rho_limit: f64,
    pub rho_limit_batched: f64,
    /// The limit the verdict uses (`rho_limit` or the batched one).
    pub rho_gate_limit: f64,
    pub h_lead: f64,
    pub h_lead_pessimistic: f64,
    pub max_closure: usize,
    pub max_row_bytes: u32,
    pub edges_checked: usize,
    pub worst_edge: Option<RhoEdge>,
    pub worst_windows: Vec<RhoWindow>,
    /// Crossing windows (one per directed edge) over the rho limit, of all.
    pub windows_over_limit: usize,
    pub windows_total: usize,
    pub pool: Option<PoolPeak>,
    pub pool_pessimistic: Option<PoolPeak>,
    /// Bytes the resident skeleton takes out of the pool, and what is left
    /// for region pages.
    pub skeleton_bytes: u64,
    pub skeleton_measured: bool,
    /// Leaf capacity the rank-row width was computed with.
    pub row_leaf_cap: u32,
    pub pool_available: u64,
    pub home_pin_bytes: u64,
    pub max_need_archetypes: usize,
    /// Regions with walkable floor or spawns that no chain of open apertures
    /// joins to the player start.
    pub unreachable: Vec<u32>,
    /// Regions with no walkable floor: sky layers, buried cells, roof tops.
    pub dead_regions: usize,
    pub failures: Vec<GateFailure>,
}

impl GateReport {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn rho_worst(&self) -> f64 {
        self.worst_windows.first().map_or(0.0, |w| w.rho)
    }
}

/// Bytes the pool holds for `set` once installed (inline textures are in
/// VRAM by then).
pub(crate) fn set_resident_bytes(regions: &[Region], set: &BitSet) -> u64 {
    set.iter()
        .map(|r| u64::from(regions[r as usize].counts.resident_bytes()))
        .sum()
}

/// Bytes read to bring `set` resident: every region payload plus each shared
/// texture once.
pub(crate) fn set_bytes(input: &PartitionInput, regions: &[Region], set: &BitSet) -> u64 {
    let mut bytes = 0u64;
    let mut shared: BTreeSet<usize> = BTreeSet::new();
    for r in set.iter() {
        bytes += u64::from(regions[r as usize].counts.bytes());
        shared.extend(&regions[r as usize].shared_textures);
    }
    bytes
        + shared
            .iter()
            .map(|&m| u64::from(input.materials[m].texture_bytes))
            .sum::<u64>()
}

fn shared_of(regions: &[Region], set: &BitSet) -> BTreeSet<usize> {
    let mut shared = BTreeSet::new();
    for r in set.iter() {
        shared.extend(&regions[r as usize].shared_textures);
    }
    shared
}

/// What a cook measured that the gates otherwise estimate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GateMeasured {
    /// The resident container's size.
    pub skeleton_bytes: u64,
    /// The per-region leaf capacity the rank rows were laid out with.
    pub leaf_cap: u32,
    /// Median encoded region payload. The drive model reads a region of this
    /// size, not of the size the cut search aimed at.
    pub median_region_bytes: u32,
}

/// `measured`: the cook's own container size and leaf capacity, when there is
/// one; otherwise the skeleton is the wire-size estimate from the region
/// count and the visibility lists, and the leaf capacity the slot cap.
pub(crate) fn evaluate(
    input: &PartitionInput,
    params: &PartitionParams,
    regions: &[Region],
    graph: &RegionGraph,
    closure: &Closure,
    run_speed: f64,
    measured: Option<GateMeasured>,
) -> GateReport {
    let n = regions.len();
    let v_max = params.v_max(run_speed);
    let typical_bytes = measured.map_or(params.region_target_bytes, |m| m.median_region_bytes);
    let typical_sectors = typical_bytes.div_ceil(SECTOR_BYTES).max(1);
    let read_ms = params.read_ms(typical_sectors);
    let t_region_ms = f64::from(params.seek_near_ms) + read_ms;
    let t_region_pessimistic_ms = 2.0 * f64::from(params.seek_far_ms) + read_ms;
    let region_bytes = f64::from(typical_sectors * SECTOR_BYTES);
    let b_eff = region_bytes / (t_region_ms / 1000.0);
    let k = f64::from(params.batch_k.max(1));
    let b_eff_batched =
        k * region_bytes / ((f64::from(params.seek_near_ms) + k * read_ms) / 1000.0);
    let util = f64::from(params.utilisation_pct) / 100.0;
    let rho_limit = util * b_eff / v_max;
    let rho_limit_batched = util * b_eff_batched / v_max;
    let rho_gate_limit = if params.rho_batched {
        rho_limit_batched
    } else {
        rho_limit
    };
    let lead_seconds =
        |t_ms: f64| (f64::from(params.lead_depth_q) * t_ms + f64::from(params.install_ms)) / 1000.0;
    let h_lead = v_max * lead_seconds(t_region_ms);
    let h_lead_pessimistic = v_max * lead_seconds(t_region_pessimistic_ms);

    let mut failures = Vec::new();

    // Hard per-region limits and collision refusals.
    for region in regions {
        let limits = region.violations(params);
        if !limits.is_empty() {
            failures.push(GateFailure::RegionOverCap {
                region: region.id,
                limits,
                bytes: region.counts.bytes(),
                unsplittable: region.unsplittable,
            });
        }
        if let Some(error) = &region.collision_error {
            failures.push(GateFailure::CollisionCompile {
                region: region.id,
                error: error.clone(),
            });
        }
    }

    // Rank-row width: bit = rank * Lcap + local leaf.
    let row_leaf_cap = measured.map_or(params.caps.leaves, |m| m.leaf_cap);
    let mut max_closure = 0;
    let mut max_row_bytes = 0;
    for (r, visible) in closure.visible.iter().enumerate() {
        let bits = visible.len() as u32 * row_leaf_cap;
        let bytes = bits.div_ceil(8);
        max_closure = max_closure.max(visible.len());
        max_row_bytes = max_row_bytes.max(bytes);
        if bytes as usize > PXBSP_MAX_VISIBILITY_BYTES {
            failures.push(GateFailure::RankRowTooWide {
                region: r as u32,
                closure: visible.len(),
                width_bytes: bytes,
                limit_bytes: PXBSP_MAX_VISIBILITY_BYTES as u32,
            });
        }
    }

    // rho over directed edges, then windows.
    let shared: Vec<BTreeSet<usize>> = closure.need.iter().map(|s| shared_of(regions, s)).collect();
    let enter = |a: u32, b: u32| -> u64 {
        let new = closure.need[b as usize].difference(&closure.need[a as usize]);
        let mut bytes: u64 = new
            .iter()
            .map(|r| u64::from(regions[r as usize].counts.bytes()))
            .sum();
        for &m in shared[b as usize].difference(&shared[a as usize]) {
            bytes += u64::from(input.materials[m].texture_bytes);
        }
        bytes
    };
    let mut edges: Vec<(u32, u32, u32, f64, u64)> = Vec::new(); // from, to, aperture, dist, bytes
    for a in 0..n as u32 {
        for &(b, ap) in &graph.adjacency[a as usize] {
            edges.push((a, b, ap, graph.edge_distance(a, b, ap), enter(a, b)));
        }
    }
    let worst_edge = edges
        .iter()
        .max_by(|x, y| {
            (x.4 as f64 / x.3.max(1.0))
                .total_cmp(&(y.4 as f64 / y.3.max(1.0)))
                .then(y.0.cmp(&x.0))
                .then(y.1.cmp(&x.1))
        })
        .map(|e| RhoEdge {
            from: e.0,
            to: e.1,
            bytes: e.4,
            distance: e.3,
        });
    let window = h_lead.max(1.0);
    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, e) in edges.iter().enumerate() {
        out_edges[e.0 as usize].push(i);
    }
    let seeds: Vec<usize> = (0..edges.len()).collect();
    let per_seed = super::par_map(&seeds, |&seed| {
        let mut best: Option<RhoWindow> = None;
        let mut stack = vec![(
            seed,
            edges[seed].4,
            edges[seed].3,
            vec![edges[seed].0, edges[seed].1],
        )];
        while let Some((last, bytes, dist, path)) = stack.pop() {
            let mut extended = false;
            if dist < window && path.len() < 8 {
                let tail = edges[last].1;
                let prev = path[path.len() - 2];
                for &next in &out_edges[tail as usize] {
                    let e = &edges[next];
                    if e.1 == prev {
                        continue;
                    }
                    extended = true;
                    let mut p = path.clone();
                    p.push(e.1);
                    stack.push((next, bytes + e.4, dist + e.3, p));
                }
            }
            if !extended {
                let length = dist.max(window);
                let rho = bytes as f64 / length;
                if best.as_ref().is_none_or(|b| rho > b.rho) {
                    best = Some(RhoWindow {
                        path,
                        bytes,
                        distance: length,
                        rho,
                    });
                }
            }
        }
        best
    });
    let mut windows: Vec<RhoWindow> = per_seed.into_iter().flatten().collect();
    let windows_over_limit = windows.iter().filter(|w| w.rho > rho_gate_limit).count();
    let windows_total = windows.len();
    windows.sort_by(|a, b| b.rho.total_cmp(&a.rho).then(a.path.cmp(&b.path)));
    windows.dedup_by(|a, b| a.path == b.path);
    windows.truncate(5);
    if let Some(worst) = windows.first() {
        if worst.rho > rho_gate_limit {
            failures.push(GateFailure::RhoExceeded {
                rho: worst.rho,
                limit: rho_gate_limit,
                path: worst.path.clone(),
            });
        }
    }

    // Pool: viewer ball + lead ring + home pin, at every sample position.
    let mut home = BitSet::new(n);
    let mut home_regions: Vec<u32> = Vec::new();
    for p in input.checkpoints.iter().chain(input.player_start.iter()) {
        if let Some(r) = regions.iter().find(|r| r.bounds.contains_half_open(*p)) {
            home_regions.push(r.id);
        }
    }
    home_regions.sort_unstable();
    home_regions.dedup();
    for &r in &home_regions {
        home.union_with(&closure.need[r as usize]);
    }
    let home_pin_bytes = set_resident_bytes(regions, &home);
    let peak = |horizon: f64| -> Option<PoolPeak> {
        let ids: Vec<u32> = (0..n as u32).collect();
        let per_region = super::par_map(&ids, |&r| {
            let mut points: Vec<V3> = closure.viewers[r as usize].clone();
            for &(_, ap) in &graph.adjacency[r as usize] {
                points.push(graph.apertures[ap as usize].center);
            }
            if points.is_empty() {
                // Nobody can stand here; it only matters as someone's
                // neighbour.
                return None;
            }
            let mut best: Option<PoolPeak> = None;
            for p in points {
                let mut set = home.clone();
                for q in 0..n as u32 {
                    if regions[q as usize].bounds.distance_to_point(p) <= params.viewer_radius {
                        set.union_with(&closure.need[q as usize]);
                    }
                }
                let need_regions = set.count();
                for q in graph.reachable_within(r, p, horizon) {
                    set.union_with(&closure.need[q as usize]);
                }
                let bytes = set_resident_bytes(regions, &set);
                if best.as_ref().is_none_or(|b| bytes > b.bytes) {
                    best = Some(PoolPeak {
                        horizon,
                        bytes,
                        region: r,
                        position: p,
                        need_regions,
                        lead_regions: set.count(),
                    });
                }
            }
            best
        });
        per_region
            .into_iter()
            .flatten()
            .fold(None, |acc: Option<PoolPeak>, p| match acc {
                Some(a) if a.bytes >= p.bytes => Some(a),
                _ => Some(p),
            })
    };
    let pool = peak(h_lead);
    let pool_pessimistic = peak(h_lead_pessimistic);
    let skeleton_bytes = measured
        .map(|m| m.skeleton_bytes)
        .unwrap_or_else(|| super::skeleton_bytes(n, closure.visible.iter().map(Vec::len).sum()));
    // Baked into `.data` and copied onto the heap: paid twice.
    let pool_available = u64::from(params.pool_bytes).saturating_sub(2 * skeleton_bytes);
    if let Some(p) = &pool {
        if p.bytes > pool_available {
            failures.push(GateFailure::PoolExceeded {
                bytes: p.bytes,
                pool: pool_available.min(u64::from(u32::MAX)) as u32,
                region: p.region,
                position: p.position,
            });
        }
    }

    let max_need_archetypes = closure
        .need
        .iter()
        .map(|set| {
            set.iter()
                .flat_map(|r| regions[r as usize].archetypes.iter().copied())
                .collect::<BTreeSet<_>>()
                .len()
        })
        .max()
        .unwrap_or(0);

    let unreachable = match input.player_start {
        Some(start) => regions
            .iter()
            .find(|r| r.bounds.contains_half_open(start))
            .map(|r| {
                let seen = graph.connected_from(r.id);
                (0..n as u32)
                    .filter(|&i| {
                        let region = &regions[i as usize];
                        !seen[i as usize]
                            && (!region.walk_points.is_empty() || !region.spawn_ids.is_empty())
                    })
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };

    GateReport {
        v_max,
        t_region_ms,
        t_region_pessimistic_ms,
        b_eff,
        b_eff_batched,
        rho_limit,
        rho_limit_batched,
        rho_gate_limit,
        h_lead,
        h_lead_pessimistic,
        max_closure,
        max_row_bytes,
        edges_checked: edges.len(),
        worst_edge,
        worst_windows: windows,
        windows_over_limit,
        windows_total,
        pool,
        pool_pessimistic,
        skeleton_bytes,
        skeleton_measured: measured.is_some(),
        row_leaf_cap,
        pool_available,
        home_pin_bytes,
        max_need_archetypes,
        unreachable,
        dead_regions: regions.iter().filter(|r| r.walk_points.is_empty()).count(),
        failures,
    }
}
