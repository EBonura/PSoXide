//! Visibility closure V(R) and resident requirement Need(R) (design 3.4, 4.1).
//!
//! Estimation, not portal flow. For every ordered pair of regions within
//! `D_vis` of each other, rays run from standing viewpoints in the first to
//! front-facing surface points of the second; the pair is visible when any
//! ray clears every solid. Touching regions through an open aperture are
//! always included. The number of rays per pair is bounded by the sample
//! parameters, so a thin sightline between two samples can be missed. M6
//! replaces this with the leaf-level closure from the real portal flow; the
//! report labels every figure derived from it as an estimate.
//!
//! `Need(R)` is `V(R)` plus, for each hook point whose region is in `V(R)`
//! and within hook range, the closure of that hook's landing region.

use super::account::Region;
use super::geometry::{add, distance, V3};
use super::graph::RegionGraph;
use super::input::PartitionInput;
use super::PartitionParams;

/// A set of region ids as bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    pub fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
        }
    }

    pub fn insert(&mut self, i: u32) {
        self.words[i as usize / 64] |= 1 << (i % 64);
    }

    pub fn contains(&self, i: u32) -> bool {
        self.words[i as usize / 64] >> (i % 64) & 1 == 1
    }

    pub fn union_with(&mut self, other: &Self) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a |= b;
        }
    }

    /// Members in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words.iter().enumerate().flat_map(|(w, &word)| {
            (0..64).filter_map(move |b| (word >> b & 1 == 1).then_some((w * 64 + b) as u32))
        })
    }

    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Members of `self` not in `other`.
    pub fn difference(&self, other: &Self) -> Self {
        Self {
            words: self
                .words
                .iter()
                .zip(&other.words)
                .map(|(a, b)| a & !b)
                .collect(),
        }
    }

    pub fn from_slice(len: usize, members: &[u32]) -> Self {
        let mut set = Self::new(len);
        for &m in members {
            set.insert(m);
        }
        set
    }
}

#[derive(Clone, Debug)]
pub struct Closure {
    /// V(R): sorted region ids including R itself.
    pub visible: Vec<Vec<u32>>,
    /// Need(R) as sets (V plus hook landing closures).
    pub need: Vec<BitSet>,
    /// Standing viewpoints used per region.
    pub viewers: Vec<Vec<V3>>,
    /// Hook landing regions pulled into some Need set, per source region.
    pub hook_pulls: Vec<Vec<u32>>,
    pub rays_cast: u64,
    pub regions: usize,
}

/// Viewpoints in a region: standing points above its upward-facing
/// surfaces, spread evenly along a spatial sort.
fn viewers_of(input: &PartitionInput, params: &PartitionParams, region: &Region) -> Vec<V3> {
    let mut floors: Vec<V3> = region.walk_points.clone();
    floors.sort_by(|a, b| {
        a[0].total_cmp(&b[0])
            .then(a[2].total_cmp(&b[2]))
            .then(a[1].total_cmp(&b[1]))
    });
    let picks = stride(&floors, params.viewer_samples);
    picks
        .into_iter()
        .map(|p| {
            let eye = add(p, [0.0, params.eye_height, 0.0]);
            if input.solids.point_in_solid(eye) {
                p
            } else {
                eye
            }
        })
        .collect()
}

fn stride<T: Copy>(list: &[T], want: usize) -> Vec<T> {
    if list.len() <= want {
        return list.to_vec();
    }
    (0..want)
        .map(|i| list[i * list.len() / want + list.len() / (2 * want)])
        .collect()
}

pub(crate) fn compute_closure(
    input: &PartitionInput,
    params: &PartitionParams,
    regions: &[Region],
    graph: &RegionGraph,
) -> Closure {
    let n = regions.len();
    let viewers: Vec<Vec<V3>> = regions
        .iter()
        .map(|r| viewers_of(input, params, r))
        .collect();
    // Sight targets sorted spatially so the stride covers the whole region.
    let targets: Vec<Vec<(V3, V3)>> = regions
        .iter()
        .map(|r| {
            let mut t: Vec<(V3, V3)> = r.targets.iter().map(|t| (t.point, t.normal)).collect();
            t.sort_by(|a, b| {
                a.0[0]
                    .total_cmp(&b.0[0])
                    .then(a.0[2].total_cmp(&b.0[2]))
                    .then(a.0[1].total_cmp(&b.0[1]))
            });
            stride(&t, params.target_samples)
        })
        .collect();

    let ids: Vec<u32> = (0..n as u32).collect();
    let per_region = super::par_map(&ids, |&r| {
        let me = &regions[r as usize];
        let mut visible = vec![r];
        let mut rays = 0u64;
        for q in 0..n as u32 {
            if q == r {
                continue;
            }
            let other = &regions[q as usize];
            if me.bounds.distance_to_box(&other.bounds) > params.vis_distance {
                continue;
            }
            if graph.adjacency[r as usize].iter().any(|&(nb, _)| nb == q) {
                visible.push(q);
                continue;
            }
            'pair: for &viewer in &viewers[r as usize] {
                for &(point, normal) in &targets[q as usize] {
                    let to_viewer = [
                        viewer[0] - point[0],
                        viewer[1] - point[1],
                        viewer[2] - point[2],
                    ];
                    if to_viewer[0] * normal[0]
                        + to_viewer[1] * normal[1]
                        + to_viewer[2] * normal[2]
                        <= 0.0
                        || distance(viewer, point) > params.vis_distance
                    {
                        continue;
                    }
                    rays += 1;
                    if !input.solids.segment_blocked(viewer, point) {
                        visible.push(q);
                        break 'pair;
                    }
                }
            }
        }
        visible.sort_unstable();
        (visible, rays)
    });
    let rays_cast = per_region.iter().map(|(_, r)| r).sum();
    let visible: Vec<Vec<u32>> = per_region.into_iter().map(|(v, _)| v).collect();

    // Hook landing region of every hook point.
    let mut hook_region = vec![None; input.hooks.len()];
    for region in regions {
        for &h in &region.hook_ids {
            hook_region[h as usize] = Some(region.id);
        }
    }
    let visible_sets: Vec<BitSet> = visible.iter().map(|v| BitSet::from_slice(n, v)).collect();
    let mut need = visible_sets.clone();
    let mut hook_pulls = vec![Vec::new(); n];
    for r in 0..n {
        for (h, landing) in hook_region.iter().enumerate() {
            let Some(landing) = *landing else { continue };
            if landing as usize == r
                || !visible_sets[r].contains(landing)
                || regions[r].bounds.distance_to_point(input.hooks[h]) > params.hook_range
            {
                continue;
            }
            need[r].union_with(&visible_sets[landing as usize]);
            if !hook_pulls[r].contains(&landing) {
                hook_pulls[r].push(landing);
            }
        }
    }
    Closure {
        visible,
        need,
        viewers,
        hook_pulls,
        rays_cast,
        regions: n,
    }
}
