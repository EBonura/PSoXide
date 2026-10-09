//! Region graph (design 3.3): which cells touch through open space, how big
//! the opening is, and walk distances through those openings.
//!
//! An aperture is the open part of the face two cells share. Open means both
//! sides of the plane at a sample point are outside every solid, sampled on a
//! grid of at most `aperture_samples` points, so a passage narrower than the
//! sample spacing can be missed (spacing is printed in the report notes).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::account::Region;
use super::cuts::{grid_dims, other_axes, sample_open, CutTree};
use super::geometry::{distance, Aabb, V3};
use super::input::PartitionInput;
use super::PartitionParams;

/// The open face between two cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Aperture {
    pub a: u32,
    pub b: u32,
    pub axis: u8,
    pub position: f64,
    /// Open area, units squared.
    pub area: f64,
    pub center: V3,
}

#[derive(Clone, Debug, Default)]
pub struct RegionGraph {
    pub apertures: Vec<Aperture>,
    /// Per region: (neighbour, aperture index), sorted by neighbour.
    pub adjacency: Vec<Vec<(u32, u32)>>,
    /// Walk anchor of each region (its cell centre).
    pub centers: Vec<V3>,
}

const CONTACT_EPS: f64 = 1.0e-6;

pub(crate) fn build_graph(
    input: &PartitionInput,
    params: &PartitionParams,
    _tree: &CutTree,
    regions: &[Region],
) -> RegionGraph {
    // Candidate contacts first (cheap, exact), then sample them in parallel.
    let mut contacts: Vec<(u32, u32, usize, f64, Aabb)> = Vec::new();
    for i in 0..regions.len() {
        for j in i + 1..regions.len() {
            if let Some(contact) = contact(&regions[i].bounds, &regions[j].bounds) {
                contacts.push((i as u32, j as u32, contact.0, contact.1, contact.2));
            }
        }
    }
    let sampled = super::par_map(&contacts, |&(a, b, axis, position, rect)| {
        sample_aperture(input, params, a, b, axis, position, &rect)
    });
    let mut graph = RegionGraph {
        apertures: sampled.into_iter().flatten().collect(),
        adjacency: vec![Vec::new(); regions.len()],
        centers: regions.iter().map(|r| r.bounds.center()).collect(),
    };
    for (index, ap) in graph.apertures.iter().enumerate() {
        graph.adjacency[ap.a as usize].push((ap.b, index as u32));
        graph.adjacency[ap.b as usize].push((ap.a, index as u32));
    }
    for list in &mut graph.adjacency {
        list.sort_unstable();
    }
    graph
}

/// Shared face of two cells: (axis, position, overlap rectangle).
fn contact(a: &Aabb, b: &Aabb) -> Option<(usize, f64, Aabb)> {
    for axis in 0..3 {
        let touching = if (a.max[axis] - b.min[axis]).abs() <= CONTACT_EPS {
            Some(a.max[axis])
        } else if (b.max[axis] - a.min[axis]).abs() <= CONTACT_EPS {
            Some(b.max[axis])
        } else {
            None
        };
        let Some(position) = touching else { continue };
        let (u, v) = other_axes(axis);
        let mut rect = Aabb {
            min: [0.0; 3],
            max: [0.0; 3],
        };
        rect.min[axis] = position;
        rect.max[axis] = position;
        let mut ok = true;
        for k in [u, v] {
            rect.min[k] = a.min[k].max(b.min[k]);
            rect.max[k] = a.max[k].min(b.max[k]);
            ok &= rect.max[k] - rect.min[k] > CONTACT_EPS;
        }
        if ok {
            return Some((axis, position, rect));
        }
    }
    None
}

fn sample_aperture(
    input: &PartitionInput,
    params: &PartitionParams,
    a: u32,
    b: u32,
    axis: usize,
    position: f64,
    rect: &Aabb,
) -> Option<Aperture> {
    let (u, v) = other_axes(axis);
    let (eu, ev) = (rect.extent(u), rect.extent(v));
    let (nu, nv) = grid_dims(eu, ev, params.aperture_samples);
    let mut open = 0usize;
    let mut sum = [0.0; 3];
    for i in 0..nu {
        for j in 0..nv {
            let mut point = [0.0; 3];
            point[axis] = position;
            point[u] = rect.min[u] + eu * (i as f64 + 0.5) / nu as f64;
            point[v] = rect.min[v] + ev * (j as f64 + 0.5) / nv as f64;
            if sample_open(input, point, axis) && walkable_near(input, point) {
                open += 1;
                for k in 0..3 {
                    sum[k] += point[k];
                }
            }
        }
    }
    if open == 0 {
        return None;
    }
    let total = (nu * nv) as f64;
    Some(Aperture {
        a,
        b,
        axis: axis as u8,
        position,
        area: eu * ev * open as f64 / total,
        center: sum.map(|s| s / open as f64),
    })
}

impl RegionGraph {
    /// Walk distance from region `from`'s anchor to region `to`'s anchor
    /// through one aperture between them.
    pub fn edge_distance(&self, from: u32, to: u32, aperture: u32) -> f64 {
        let ap = &self.apertures[aperture as usize];
        distance(self.centers[from as usize], ap.center)
            + distance(ap.center, self.centers[to as usize])
    }

    /// Regions whose boundary a walker standing at `point` in `start` can
    /// reach within `horizon` units, walking through apertures. Includes
    /// `start`. Sorted.
    pub fn reachable_within(&self, start: u32, point: V3, horizon: f64) -> Vec<u32> {
        let mut reached = vec![start];
        let mut best = vec![f64::INFINITY; self.apertures.len()];
        // (distance bits, aperture); distances are non-negative so the bit
        // order is the numeric order.
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
        for &(_, ap) in &self.adjacency[start as usize] {
            let d = distance(point, self.apertures[ap as usize].center);
            if d <= horizon && d < best[ap as usize] {
                best[ap as usize] = d;
                heap.push(Reverse((d.to_bits(), ap)));
            }
        }
        while let Some(Reverse((bits, ap))) = heap.pop() {
            let d = f64::from_bits(bits);
            if d > best[ap as usize] {
                continue;
            }
            let aperture = &self.apertures[ap as usize];
            for region in [aperture.a, aperture.b] {
                if !reached.contains(&region) {
                    reached.push(region);
                }
                for &(_, next) in &self.adjacency[region as usize] {
                    if next == ap {
                        continue;
                    }
                    let nd = d + distance(aperture.center, self.apertures[next as usize].center);
                    if nd <= horizon && nd < best[next as usize] {
                        best[next as usize] = nd;
                        heap.push(Reverse((nd.to_bits(), next)));
                    }
                }
            }
        }
        reached.sort_unstable();
        reached
    }

    /// Regions reachable from `start` over any number of apertures.
    pub fn connected_from(&self, start: u32) -> Vec<bool> {
        let mut seen = vec![false; self.adjacency.len()];
        let mut stack = vec![start];
        seen[start as usize] = true;
        while let Some(r) = stack.pop() {
            for &(n, _) in &self.adjacency[r as usize] {
                if !seen[n as usize] {
                    seen[n as usize] = true;
                    stack.push(n);
                }
            }
        }
        seen
    }
}

/// A passage counts only where a player could stand under it: with a walked
/// floor map, a sample must have a reachable floor within 200 units across and
/// 200 units below (door height plus a head). Without one every open sample
/// counts.
fn walkable_near(input: &PartitionInput, point: V3) -> bool {
    input
        .walk
        .as_ref()
        .is_none_or(|walk| walk.near(point, 200.0, 200.0, 8.0))
}
