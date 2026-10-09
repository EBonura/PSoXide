//! Disc layout (design 3.7): order the region payloads on disc so that
//! regions the player moves between sit close together.
//!
//! Objective: minimise the sum over region-graph edges of
//! `weight x |start sector difference|`, with the weight the open aperture
//! area. That is a linear arrangement problem (NP-hard), so the order is the
//! Fiedler vector of the weighted graph Laplacian per connected component,
//! refined by bounded segment reversals. Everything is deterministic: fixed
//! start vector, fixed iteration counts, ties broken by region id.
//!
//! The seek model is the measured table (11 / 79 / 137 / 310 ms for 1 / 16 /
//! 128 / 512 sectors) but silicon swings about 2x run to run, so the report
//! gives the share of transitions per sector-distance class rather than
//! pretending to know milliseconds.

use super::account::Region;
use super::graph::RegionGraph;
use super::PartitionParams;

/// Upper bounds, in sectors, of the reported seek-distance classes.
pub const SEEK_CLASSES: [u32; 3] = [16, 128, 512];

/// Weighted cost and per-class edge counts of one order.
type OrderScore = (u64, [u32; 4]);

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Region ids in disc order.
    pub order: Vec<u32>,
    /// Start sector of each region (indexed by region id), relative to the
    /// start of the region pack.
    pub start_sector: Vec<u32>,
    pub sectors: Vec<u32>,
    pub total_sectors: u32,
    /// Weighted sector distance of the chosen order.
    pub cost: u64,
    /// The same for the cut-tree order.
    pub baseline_cost: u64,
    /// Region-graph edges per class: <=16, <=128, <=512, >512 sectors apart.
    pub class_counts: [u32; 4],
    pub baseline_class_counts: [u32; 4],
}

pub(crate) fn compute_layout(
    _params: &PartitionParams,
    regions: &[Region],
    graph: &RegionGraph,
) -> Layout {
    let n = regions.len();
    let sectors: Vec<u32> = regions.iter().map(|r| r.counts.sectors()).collect();
    // Undirected weighted edges.
    let mut edges: Vec<(u32, u32, f64)> = Vec::new();
    for ap in &graph.apertures {
        edges.push((ap.a, ap.b, ap.area.max(1.0)));
    }
    let eval = |order: &[u32]| -> OrderScore {
        let starts = starts_of(order, &sectors);
        let mut cost = 0f64;
        let mut classes = [0u32; 4];
        for &(a, b, w) in &edges {
            let d = starts[a as usize].abs_diff(starts[b as usize]);
            cost += w * f64::from(d);
            let class = SEEK_CLASSES.iter().position(|&c| d <= c).unwrap_or(3);
            classes[class] += 1;
        }
        (cost.round() as u64, classes)
    };
    let baseline: Vec<u32> = (0..n as u32).collect();
    let (baseline_cost, baseline_class_counts) = eval(&baseline);

    let mut order = spectral_order(n, &edges);
    refine(&mut order, &eval);
    let (mut cost, mut class_counts) = eval(&order);
    if baseline_cost <= cost {
        order = baseline;
        cost = baseline_cost;
        class_counts = baseline_class_counts;
    }
    let start_sector = starts_of(&order, &sectors);
    let total_sectors = sectors.iter().sum();
    Layout {
        order,
        start_sector,
        sectors,
        total_sectors,
        cost,
        baseline_cost,
        class_counts,
        baseline_class_counts,
    }
}

fn starts_of(order: &[u32], sectors: &[u32]) -> Vec<u32> {
    let mut starts = vec![0u32; order.len()];
    let mut at = 0;
    for &r in order {
        starts[r as usize] = at;
        at += sectors[r as usize];
    }
    starts
}

fn refine(order: &mut [u32], eval: &dyn Fn(&[u32]) -> OrderScore) {
    if order.len() < 3 {
        return;
    }
    let mut best = eval(order).0;
    for _sweep in 0..3 {
        let mut improved = false;
        for i in 0..order.len() {
            for len in 2..=6usize {
                let j = i + len;
                if j > order.len() {
                    break;
                }
                order[i..j].reverse();
                let cost = eval(order).0;
                if cost < best {
                    best = cost;
                    improved = true;
                } else {
                    order[i..j].reverse();
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// Components in order of their smallest region id, each sorted by its
/// Fiedler vector.
fn spectral_order(n: usize, edges: &[(u32, u32, f64)]) -> Vec<u32> {
    let mut neighbours: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for &(a, b, w) in edges {
        neighbours[a as usize].push((b as usize, w));
        neighbours[b as usize].push((a as usize, w));
    }
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for start in 0..n {
        if seen[start] {
            continue;
        }
        let mut component = vec![start];
        seen[start] = true;
        let mut cursor = 0;
        while cursor < component.len() {
            let v = component[cursor];
            cursor += 1;
            for &(u, _) in &neighbours[v] {
                if !seen[u] {
                    seen[u] = true;
                    component.push(u);
                }
            }
        }
        component.sort_unstable();
        order.extend(fiedler_sorted(&component, &neighbours));
    }
    order
}

fn fiedler_sorted(component: &[usize], neighbours: &[Vec<(usize, f64)>]) -> Vec<u32> {
    let m = component.len();
    if m <= 2 {
        return component.iter().map(|&v| v as u32).collect();
    }
    let local = |v: usize| component.binary_search(&v).unwrap();
    let degree: Vec<f64> = component
        .iter()
        .map(|&v| neighbours[v].iter().map(|&(_, w)| w).sum())
        .collect();
    let shift = 2.0 * degree.iter().cloned().fold(0.0, f64::max).max(1.0);
    // Power iteration on (shift*I - L) orthogonal to the constant vector.
    let mut x: Vec<f64> = (0..m)
        .map(|i| ((i as f64 * 0.618_033_988_75) % 1.0) - 0.5)
        .collect();
    for _ in 0..400 {
        let mean = x.iter().sum::<f64>() / m as f64;
        x.iter_mut().for_each(|v| *v -= mean);
        let mut y = vec![0.0; m];
        for (i, &v) in component.iter().enumerate() {
            let mut lx = degree[i] * x[i];
            for &(u, w) in &neighbours[v] {
                lx -= w * x[local(u)];
            }
            y[i] = shift * x[i] - lx;
        }
        let norm = y.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm < 1.0e-18 {
            break;
        }
        x = y.iter().map(|v| v / norm).collect();
    }
    let mut ids: Vec<usize> = (0..m).collect();
    ids.sort_by(|&a, &b| x[a].total_cmp(&x[b]).then(a.cmp(&b)));
    ids.into_iter().map(|i| component[i] as u32).collect()
}
