//! Clustered portal flow for streamed worlds (design 2026-10-08, section 3.4).
//!
//! The whole-map flow ([`super::quake_portal_flow_rows`]) keeps one bit per
//! visible leaf in every working set, so its memory and time grow with the
//! square of the leaf count, and its leaf index is a `u16`. A streamed world
//! can be far bigger than anything one resident map holds, so this flow works
//! per *cluster* (a streaming region) instead:
//!
//! * A cluster's flow only ever sees the portals inside its **neighbourhood**:
//!   the clusters reachable from it through open portals without leaving the
//!   far-reject distance. A sight line that can be drawn is shorter than the
//!   far-reject distance (`reach`), so it never leaves the neighbourhood, and
//!   every portal chain it crosses is inside the sub-problem. The flow is
//!   therefore exact for everything that can matter, and bounded by the size
//!   of a neighbourhood instead of the size of the world.
//! * Working sets are bitsets over the neighbourhood's leaves only (a dense
//!   local index), so the cost of a cluster does not depend on the world size.
//! * Results are sparse sorted lists of leaf ids (`u32`), clipped to
//!   leaf-to-leaf distances within `reach`, and made reciprocal at the end.
//!
//! Output equals the whole-map flow for every leaf pair whose sight line is
//! within `reach` (see the equivalence tests), and is a subset of it overall.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use super::{
    base_portal_visibility, bit_count, directed_open_portals_by, flow_portal, set_bit, union_bits,
    CompiledPortal, CompiledSurfaceBsp, DirectedPortal, Mightsee,
};
use crate::brush_region::geometry::Aabb;

/// Everything the clustered flow reads.
pub(crate) struct ClusterFlow<'a> {
    pub bsp: &'a CompiledSurfaceBsp,
    pub portals: &'a [CompiledPortal],
    /// Host leaf to visible-leaf id plus one; zero for a leaf that is not
    /// visible.
    pub dense_of_leaf: &'a [u32],
    pub visible: usize,
    /// Cluster of every visible leaf.
    pub cluster_of: &'a [u32],
    pub clusters: usize,
    /// Far-reject distance: nothing farther than this from a viewpoint is drawn.
    pub reach: f64,
    /// Run the separator flow (`true`) or stop at the conservative `mightsee`
    /// rows (`false`, the draft cook).
    pub exact: bool,
}

/// What the flow cost, for the cook log and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClusterFlowStats {
    pub directed_portals: usize,
    /// Largest neighbourhood, in clusters, leaves and directed portals.
    pub max_clusters: usize,
    pub max_leaves: usize,
    pub max_portals: usize,
    /// Total rows entries before and after the reciprocal closure.
    pub entries: usize,
    pub added_by_closure: usize,
}

struct Prepared {
    directed: Vec<DirectedPortal>,
    outgoing: Vec<Vec<usize>>,
    leaf_box: Vec<Aabb>,
    cluster_box: Vec<Aabb>,
    cluster_leaves: Vec<Vec<u32>>,
    cluster_adjacent: Vec<Vec<u32>>,
}

fn prepare(world: &ClusterFlow<'_>) -> Prepared {
    let dense = |leaf: usize| {
        let id = world.dense_of_leaf[leaf];
        (id > 0).then(|| id as usize - 1)
    };
    let directed = directed_open_portals_by(world.bsp, world.portals, dense);
    let mut outgoing = vec![Vec::new(); world.visible];
    for (index, portal) in directed.iter().enumerate() {
        outgoing[portal.from_leaf].push(index);
    }

    // Every portal of a leaf, solid neighbours included, bounds it: the BSP
    // is closed by the six headnode portals.
    let mut leaf_box = vec![Aabb::EMPTY; world.visible];
    for portal in world.portals {
        for host in [portal.front_leaf, portal.back_leaf] {
            if let Some(leaf) = dense(host) {
                for vertex in &portal.vertices {
                    leaf_box[leaf].grow(*vertex);
                }
            }
        }
    }
    let mut cluster_box = vec![Aabb::EMPTY; world.clusters];
    let mut cluster_leaves = vec![Vec::new(); world.clusters];
    for (leaf, bounds) in leaf_box.iter().enumerate() {
        let cluster = world.cluster_of[leaf] as usize;
        cluster_leaves[cluster].push(leaf as u32);
        if !bounds.is_empty() {
            cluster_box[cluster] = cluster_box[cluster].union(bounds);
        }
    }
    let mut cluster_adjacent = vec![Vec::new(); world.clusters];
    for portal in &directed {
        let (a, b) = (
            world.cluster_of[portal.from_leaf],
            world.cluster_of[portal.to_leaf],
        );
        if a != b {
            cluster_adjacent[a as usize].push(b);
        }
    }
    for list in &mut cluster_adjacent {
        list.sort_unstable();
        list.dedup();
    }
    Prepared {
        directed,
        outgoing,
        leaf_box,
        cluster_box,
        cluster_leaves,
        cluster_adjacent,
    }
}

/// `mightsee` of a portal, computed the first time a flow asks for it.
struct LazyMightsee<'a> {
    cells: Vec<OnceCell<Vec<u64>>>,
    portals: &'a [DirectedPortal],
    outgoing: &'a [Vec<usize>],
    words: usize,
}

impl Mightsee for LazyMightsee<'_> {
    fn get(&self, portal: usize) -> &[u64] {
        self.cells[portal].get_or_init(|| {
            base_portal_visibility(
                &self.portals[portal],
                self.portals,
                self.outgoing,
                self.words,
            )
        })
    }
}

/// The rows of one cluster's leaves, `(leaf, row)`, and the size of its
/// neighbourhood in clusters, leaves and directed portals.
type ClusterResult = (Vec<(u32, Vec<u32>)>, [usize; 3]);

/// Leaf-to-leaf rows of every leaf of cluster `cluster`.
fn cluster_rows(world: &ClusterFlow<'_>, prep: &Prepared, cluster: usize) -> ClusterResult {
    // Neighbourhood: clusters joined to this one by open portals, each within
    // reach of this cluster's bounds.
    let home = prep.cluster_box[cluster];
    let mut member = vec![false; world.clusters];
    member[cluster] = true;
    let mut stack = vec![cluster as u32];
    while let Some(c) = stack.pop() {
        for &n in &prep.cluster_adjacent[c as usize] {
            let n_box = &prep.cluster_box[n as usize];
            if !member[n as usize] && home.distance_to_box(n_box) <= world.reach {
                member[n as usize] = true;
                stack.push(n);
            }
        }
    }
    let members: Vec<usize> = (0..world.clusters).filter(|&c| member[c]).collect();

    // Local leaf numbering: the neighbourhood's leaves.
    let mut local_leaves: Vec<u32> = Vec::new();
    for &c in &members {
        local_leaves.extend(&prep.cluster_leaves[c]);
    }
    let mut local_of = vec![u32::MAX; world.visible];
    for (local, &leaf) in local_leaves.iter().enumerate() {
        local_of[leaf as usize] = local as u32;
    }
    let leaves = local_leaves.len();
    let words = leaves.div_ceil(64);

    // Local directed portals, and which of them start in this cluster.
    let mut portals: Vec<DirectedPortal> = Vec::new();
    let mut outgoing = vec![Vec::new(); leaves];
    let mut sources: Vec<usize> = Vec::new();
    for (local, &leaf) in local_leaves.iter().enumerate() {
        for &global in &prep.outgoing[leaf as usize] {
            let portal = &prep.directed[global];
            let to = local_of[portal.to_leaf];
            if to == u32::MAX {
                continue;
            }
            let index = portals.len();
            portals.push(DirectedPortal {
                from_leaf: local,
                to_leaf: to as usize,
                plane: portal.plane,
                winding: portal.winding.clone(),
            });
            outgoing[local].push(index);
            if world.cluster_of[leaf as usize] as usize == cluster {
                sources.push(index);
            }
        }
    }

    let mightsee = LazyMightsee {
        cells: (0..portals.len()).map(|_| OnceCell::new()).collect(),
        portals: &portals,
        outgoing: &outgoing,
        words,
    };
    let mut visible_by_portal: Vec<Option<Vec<u64>>> = vec![None; portals.len()];
    if world.exact {
        // Least complex first, as Quake does: a flow can reuse the exact
        // result of every source ranked before it.
        sources.sort_by_key(|&p| (bit_count(mightsee.get(p)), p));
        let mut order_rank = vec![usize::MAX; portals.len()];
        for (rank, &p) in sources.iter().enumerate() {
            order_rank[p] = rank;
        }
        let status: Vec<AtomicU8> = (0..portals.len()).map(|_| AtomicU8::new(0)).collect();
        let published: Vec<OnceLock<Vec<u64>>> =
            (0..portals.len()).map(|_| OnceLock::new()).collect();
        for &p in &sources {
            let visibility = flow_portal(
                p,
                &portals,
                &outgoing,
                &mightsee,
                &published,
                &status,
                &order_rank,
                leaves,
                words,
            );
            published[p]
                .set(visibility)
                .expect("a source portal is flowed once");
            status[p].store(2, Ordering::Release);
        }
        for &p in &sources {
            visible_by_portal[p] = published[p].get().cloned();
        }
    } else {
        for &p in &sources {
            visible_by_portal[p] = Some(mightsee.get(p).to_vec());
        }
    }

    // Rows of this cluster's leaves, clipped to the far-reject distance.
    let mut rows = Vec::with_capacity(prep.cluster_leaves[cluster].len());
    for &leaf in &prep.cluster_leaves[cluster] {
        let local = local_of[leaf as usize] as usize;
        let mut bits = vec![0u64; words];
        set_bit(&mut bits, local);
        for &p in &outgoing[local] {
            if let Some(visible) = &visible_by_portal[p] {
                union_bits(&mut bits, visible);
            }
        }
        let from = &prep.leaf_box[leaf as usize];
        let mut row = Vec::new();
        for (word_index, &word) in bits.iter().enumerate() {
            let mut remaining = word;
            while remaining != 0 {
                let bit = remaining.trailing_zeros() as usize;
                remaining &= remaining - 1;
                let target = local_leaves[word_index * 64 + bit];
                if target == leaf
                    || from.distance_to_box(&prep.leaf_box[target as usize]) <= world.reach
                {
                    row.push(target);
                }
            }
        }
        row.sort_unstable();
        rows.push((leaf, row));
    }
    (rows, [members.len(), leaves, portals.len()])
}

/// Sparse visibility rows, one per visible leaf: the sorted visible-leaf ids
/// each leaf sees (itself included), reciprocal.
pub(crate) fn clustered_portal_rows(world: &ClusterFlow<'_>) -> (Vec<Vec<u32>>, ClusterFlowStats) {
    let prep = prepare(world);
    let mut stats = ClusterFlowStats {
        directed_portals: prep.directed.len(),
        ..ClusterFlowStats::default()
    };
    let started = std::time::Instant::now();
    let report = prep.directed.len() >= 1024;
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, 8)
        .min(world.clusters.max(1));
    if report {
        crate::playtest::emit_cook_output(format_args!(
            "[brush-vis] clustered portal flow: {} visible leaves in {} clusters, {} directed portals, reach {}, {workers} workers",
            world.visible,
            world.clusters,
            prep.directed.len(),
            world.reach
        ));
    }

    // Dynamic queue: clusters differ a lot in cost, and results land in their
    // own slot so the output never depends on scheduling.
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<ClusterResult>>> =
        (0..world.clusters).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let cluster = next.fetch_add(1, Ordering::Relaxed);
                if cluster >= world.clusters {
                    break;
                }
                let result = cluster_rows(world, &prep, cluster);
                *slots[cluster].lock().expect("cluster slot") = Some(result);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                if report && (finished.is_multiple_of(64) || finished == world.clusters) {
                    crate::playtest::emit_cook_output(format_args!(
                        "[brush-vis] clustered flow {finished}/{} ({:.1}s)",
                        world.clusters,
                        started.elapsed().as_secs_f32()
                    ));
                }
            });
        }
    });

    let mut rows: Vec<Vec<u32>> = vec![Vec::new(); world.visible];
    for slot in slots {
        let (cluster_rows, sizes) = slot
            .into_inner()
            .expect("cluster slot")
            .expect("every cluster is flowed");
        stats.max_clusters = stats.max_clusters.max(sizes[0]);
        stats.max_leaves = stats.max_leaves.max(sizes[1]);
        stats.max_portals = stats.max_portals.max(sizes[2]);
        for (leaf, row) in cluster_rows {
            rows[leaf as usize] = row;
        }
    }
    stats.entries = rows.iter().map(Vec::len).sum();

    // Visibility is reciprocal. Directed flows reach that on their own except
    // where floating point breaks a tie, and a pair that is only found from
    // one side lies on the edge of the neighbourhoods anyway.
    let mut extra: Vec<Vec<u32>> = vec![Vec::new(); world.visible];
    for (from, row) in rows.iter().enumerate() {
        for &to in row {
            if rows[to as usize].binary_search(&(from as u32)).is_err() {
                extra[to as usize].push(from as u32);
            }
        }
    }
    for (row, extra) in rows.iter_mut().zip(extra) {
        if extra.is_empty() {
            continue;
        }
        stats.added_by_closure += extra.len();
        row.extend(extra);
        row.sort_unstable();
        row.dedup();
    }
    if report {
        crate::playtest::emit_cook_output(format_args!(
            "[brush-vis] clustered flow complete in {:.1}s: largest neighbourhood {} clusters / {} leaves / {} portals, {} row entries (+{} by closure)",
            started.elapsed().as_secs_f32(),
            stats.max_clusters,
            stats.max_leaves,
            stats.max_portals,
            stats.entries,
            stats.added_by_closure
        ));
    }
    (rows, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Plane;
    use crate::brush_compile::{BspChild, BspLeafContents, CompiledBspLeaf};
    use crate::brush_vis::quake_portal_flow_rows;

    fn empty_bsp(leaves: usize) -> CompiledSurfaceBsp {
        CompiledSurfaceBsp {
            root: BspChild::Leaf(0),
            nodes: Vec::new(),
            leaves: vec![
                CompiledBspLeaf {
                    contents: BspLeafContents::Empty,
                    mark_surfaces: Vec::new(),
                };
                leaves
            ],
            surfaces: Vec::new(),
        }
    }

    fn x_portal(back: usize, front: usize, x: i64) -> CompiledPortal {
        CompiledPortal {
            plane: Plane {
                normal: [1, 0, 0],
                dist: x,
            },
            front_leaf: front,
            back_leaf: back,
            vertices: vec![
                [x as f64, -1.0, -1.0],
                [x as f64, 1.0, -1.0],
                [x as f64, 1.0, 1.0],
                [x as f64, -1.0, 1.0],
            ],
        }
    }

    fn y_portal(back: usize, front: usize, y: i64, x: [f64; 2]) -> CompiledPortal {
        CompiledPortal {
            plane: Plane {
                normal: [0, 1, 0],
                dist: y,
            },
            front_leaf: front,
            back_leaf: back,
            vertices: vec![
                [x[0], y as f64, -1.0],
                [x[0], y as f64, 1.0],
                [x[1], y as f64, 1.0],
                [x[1], y as f64, -1.0],
            ],
        }
    }

    fn run(
        leaves: usize,
        portals: &[CompiledPortal],
        cluster_of: &[u32],
        reach: f64,
        exact: bool,
    ) -> Vec<Vec<u32>> {
        let bsp = empty_bsp(leaves);
        let dense: Vec<u32> = (1..=leaves as u32).collect();
        let clusters = cluster_of.iter().max().map_or(0, |&c| c as usize + 1);
        clustered_portal_rows(&ClusterFlow {
            bsp: &bsp,
            portals,
            dense_of_leaf: &dense,
            visible: leaves,
            cluster_of,
            clusters,
            reach,
            exact,
        })
        .0
    }

    /// The right-angle world of the whole-map tests: leaf 3 hides around a
    /// corner from leaf 0.
    fn corner() -> Vec<CompiledPortal> {
        vec![
            x_portal(0, 1, 0),
            x_portal(1, 2, 10),
            y_portal(2, 3, 10, [9.0, 11.0]),
        ]
    }

    #[test]
    fn clustered_rows_equal_the_whole_map_flow_when_reach_is_unbounded() {
        let portals = corner();
        let whole = quake_portal_flow_rows(&empty_bsp(4), &portals, &[1, 2, 3, 4], 4);
        for clusters in [[0, 0, 0, 0], [0, 0, 1, 1], [0, 1, 2, 3], [0, 1, 1, 0]] {
            let rows = run(4, &portals, &clusters, 1.0e9, true);
            for from in 0..4usize {
                for to in 0..4usize {
                    let bit =
                        whole[from].as_ref().expect("visible")[to >> 3] & (1 << (to & 7)) != 0;
                    assert_eq!(
                        rows[from].contains(&(to as u32)),
                        bit,
                        "clusters {clusters:?}: {from} sees {to}"
                    );
                }
            }
        }
    }

    #[test]
    fn mightsee_rows_are_a_superset_of_the_separator_flow() {
        let portals = corner();
        let exact = run(4, &portals, &[0, 0, 1, 1], 1.0e9, true);
        let fast = run(4, &portals, &[0, 0, 1, 1], 1.0e9, false);
        for (e, f) in exact.iter().zip(&fast) {
            assert!(e.iter().all(|id| f.contains(id)), "{e:?} vs {f:?}");
        }
    }

    #[test]
    fn rows_are_clipped_to_the_far_reject_distance_and_stay_reciprocal() {
        // Ten leaves in a row, ten units each.
        let portals: Vec<_> = (0..9)
            .map(|i| x_portal(i, i + 1, 10 * (i as i64 + 1)))
            .collect();
        let cluster_of: Vec<u32> = (0..10).map(|i| i / 2).collect();
        let rows = run(10, &portals, &cluster_of, 25.0, true);
        for from in 0..10usize {
            // The gap between leaf i and j is 10 * (|i - j| - 1).
            let expected: Vec<u32> = (0..10u32)
                .filter(|&to| {
                    10.0 * (from as f64 - f64::from(to)).abs() - 10.0 <= 25.0 || to as usize == from
                })
                .collect();
            assert_eq!(rows[from], expected, "leaf {from}");
            for &to in &rows[from] {
                assert!(rows[to as usize].contains(&(from as u32)));
            }
        }
    }

    #[test]
    fn a_world_past_the_old_sixteen_bit_leaf_index_flows_in_bounded_clusters() {
        let leaves = 40_000usize;
        let portals: Vec<_> = (0..leaves - 1)
            .map(|i| x_portal(i, i + 1, 10 * (i as i64 + 1)))
            .collect();
        let cluster_of: Vec<u32> = (0..leaves as u32).map(|i| i / 8).collect();
        let (rows, stats) = {
            let bsp = empty_bsp(leaves);
            let dense: Vec<u32> = (1..=leaves as u32).collect();
            clustered_portal_rows(&ClusterFlow {
                bsp: &bsp,
                portals: &portals,
                dense_of_leaf: &dense,
                visible: leaves,
                cluster_of: &cluster_of,
                clusters: leaves / 8,
                reach: 100.0,
                exact: true,
            })
        };
        assert!(leaves > i16::MAX as usize);
        // |i - j| - 1 gaps of ten units: leaves up to eleven away.
        for from in [0usize, 7, 8, 32_767, 32_768, 39_999] {
            let lo = from.saturating_sub(11);
            let hi = (from + 11).min(leaves - 1);
            let expected: Vec<u32> = (lo..=hi).map(|i| i as u32).collect();
            assert_eq!(rows[from], expected, "leaf {from}");
        }
        // The work per cluster is the neighbourhood, not the world.
        assert!(stats.max_leaves <= 8 * 5, "{stats:?}");
        assert!(stats.max_portals <= 2 * 8 * 5, "{stats:?}");
    }
}
