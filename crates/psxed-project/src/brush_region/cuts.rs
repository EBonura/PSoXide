//! Cut search (design 3.2): recursive axis-aligned bisection of the world.
//!
//! Candidate planes are the lattice multiples and the planes of existing
//! axial faces (walls and floors). A candidate is scored lexicographically by
//! the open area it crosses (a door is nearly free, an open field is not),
//! then by the surfaces it splits, then by payload imbalance, then by
//! coordinate so equal scores resolve the same way on every run.
//!
//! The search runs on a cheap byte model of each item (the real clip and BSP
//! accounting happens later, per final cell). The accounting pass can ask for
//! further splits when a cell turns out heavier than the model said.

use super::geometry::{polygon_centroid, Aabb, V3};
use super::input::PartitionInput;
use super::{record, PartitionParams};

/// One node of the cut tree. Index 0 is the root.
#[derive(Clone, Debug, PartialEq)]
pub enum CutNode {
    Split {
        axis: u8,
        position: f64,
        /// Child holding `p[axis] < position`.
        low: u32,
        high: u32,
        bounds: Aabb,
        /// Open area the plane crosses, units squared (sampled).
        open_area: f64,
    },
    Leaf {
        bounds: Aabb,
        region: u32,
        /// A re-split was asked for but no admissible plane exists.
        unsplittable: bool,
    },
}

impl CutNode {
    pub fn bounds(&self) -> Aabb {
        match self {
            Self::Split { bounds, .. } | Self::Leaf { bounds, .. } => *bounds,
        }
    }
}

/// The cut tree. Region ids are assigned in depth-first order, low child
/// first, so they depend only on the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct CutTree {
    pub nodes: Vec<CutNode>,
    /// Node index of each region's leaf.
    pub leaves: Vec<u32>,
}

impl CutTree {
    pub fn root_bounds(&self) -> Aabb {
        self.nodes[0].bounds()
    }

    pub fn region_count(&self) -> usize {
        self.leaves.len()
    }

    pub fn leaf_bounds(&self, region: u32) -> Aabb {
        self.nodes[self.leaves[region as usize] as usize].bounds()
    }

    /// Region whose half-open cell holds `point` (clamped to the world).
    pub fn locate(&self, point: V3) -> u32 {
        let mut node = 0usize;
        loop {
            match &self.nodes[node] {
                CutNode::Leaf { region, .. } => return *region,
                CutNode::Split {
                    axis,
                    position,
                    low,
                    high,
                    ..
                } => {
                    node = if point[*axis as usize] < *position {
                        *low as usize
                    } else {
                        *high as usize
                    };
                }
            }
        }
    }

    /// Reassign region ids depth first.
    pub(crate) fn renumber(&mut self) {
        self.leaves.clear();
        let mut stack = vec![0u32];
        while let Some(index) = stack.pop() {
            match self.nodes[index as usize].clone() {
                CutNode::Split { low, high, .. } => {
                    stack.push(high);
                    stack.push(low);
                }
                CutNode::Leaf {
                    bounds,
                    unsplittable,
                    ..
                } => {
                    let region = self.leaves.len() as u32;
                    self.nodes[index as usize] = CutNode::Leaf {
                        bounds,
                        region,
                        unsplittable,
                    };
                    self.leaves.push(index);
                }
            }
        }
    }
}

/// Byte-model item for the search.
#[derive(Clone, Copy)]
struct Item {
    aabb: Aabb,
    centroid: V3,
    bytes: f64,
    face: bool,
    vertices: u32,
}

/// Model bytes of one render surface: vertices, face record, mark, shared
/// plane. Calibrated on the three real projects (render lumps are ~82% of a
/// cooked map). [E]
pub(crate) fn surface_bytes(vertices: usize) -> f64 {
    (vertices as u32 * record::VERTEX + record::FACE + 3 + 5) as f64
}

/// Model bytes of one topology surface: one BSP node and one leaf. [E]
pub(crate) const TOPOLOGY_BYTES: f64 = (record::NODE + record::LEAF) as f64;
/// Model bytes of collision per brush; cooked clipnode lumps run 75 to 109
/// bytes per brush on graybox-valley, graybox-reach and cortex-ignition-0.5. [E]
pub(crate) const BRUSH_BYTES: f64 = 100.0;

fn items(input: &PartitionInput) -> Vec<Item> {
    let mut items = Vec::with_capacity(input.render.len() + input.topology.len());
    for surface in &input.render {
        let aabb = Aabb::from_points(&surface.vertices);
        items.push(Item {
            aabb,
            centroid: polygon_centroid(&surface.vertices),
            bytes: surface_bytes(surface.vertices.len()),
            face: true,
            vertices: surface.vertices.len() as u32,
        });
    }
    for surface in &input.topology {
        let aabb = Aabb::from_points(&surface.vertices);
        items.push(Item {
            aabb,
            centroid: polygon_centroid(&surface.vertices),
            bytes: TOPOLOGY_BYTES,
            face: false,
            vertices: 0,
        });
    }
    for brush in &input.brushes {
        let solved = brush.solve();
        let aabb = Aabb {
            min: solved.min,
            max: solved.max,
        };
        items.push(Item {
            aabb,
            centroid: aabb.center(),
            bytes: BRUSH_BYTES,
            face: false,
            vertices: 0,
        });
    }
    for spawn in &input.spawns {
        items.push(Item {
            aabb: Aabb {
                min: spawn.position,
                max: spawn.position,
            },
            centroid: spawn.position,
            bytes: f64::from(record::ENTITY),
            face: false,
            vertices: 0,
        });
    }
    items
}

/// Bytes the cut search's model gives the items whose centroid lies in
/// `bounds`: what decides whether a cell is split. A generator that keeps a
/// module under the cut target never has its interior cut.
#[cfg(test)]
pub(crate) fn model_bytes_in(input: &PartitionInput, bounds: &Aabb) -> f64 {
    items(input)
        .iter()
        .filter(|item| bounds.contains_half_open(item.centroid))
        .map(|item| item.bytes)
        .sum()
}

#[derive(Clone, Copy, Default)]
struct Totals {
    bytes: f64,
    faces: u32,
    vertices: u32,
}

fn totals(items: &[Item], members: &[u32]) -> Totals {
    let mut t = Totals::default();
    for &m in members {
        let item = &items[m as usize];
        t.bytes += item.bytes;
        t.faces += u32::from(item.face);
        t.vertices += item.vertices;
    }
    t
}

fn fits(params: &PartitionParams, t: Totals, limit: u32) -> bool {
    t.bytes <= f64::from(limit)
        && t.faces <= params.caps.faces
        && t.vertices <= params.caps.vertices
}

/// Build the cut tree from the byte model.
pub(crate) fn build_tree(input: &PartitionInput, params: &PartitionParams) -> CutTree {
    let items = items(input);
    let all: Vec<u32> = (0..items.len() as u32).collect();
    let mut tree = CutTree {
        nodes: vec![CutNode::Leaf {
            bounds: input.bounds,
            region: 0,
            unsplittable: false,
        }],
        leaves: Vec::new(),
    };
    // A world that fits one region cooks with zero cuts.
    if !fits(params, totals(&items, &all), params.region_hard_cap_bytes) {
        let mut stack = vec![(0u32, all)];
        while let Some((node, members)) = stack.pop() {
            let t = totals(&items, &members);
            if fits(params, t, params.region_target_bytes) {
                continue;
            }
            let bounds = tree.nodes[node as usize].bounds();
            let Some(cut) = best_cut(input, params, &items, &members, &bounds, t) else {
                if let CutNode::Leaf { unsplittable, .. } = &mut tree.nodes[node as usize] {
                    *unsplittable = true;
                }
                continue;
            };
            let (low_members, high_members) = partition_members(&items, &members, &cut);
            let (low_bounds, high_bounds) = bounds.split(cut.axis, cut.position);
            let low = tree.nodes.len() as u32;
            tree.nodes.push(CutNode::Leaf {
                bounds: low_bounds,
                region: 0,
                unsplittable: false,
            });
            let high = tree.nodes.len() as u32;
            tree.nodes.push(CutNode::Leaf {
                bounds: high_bounds,
                region: 0,
                unsplittable: false,
            });
            tree.nodes[node as usize] = CutNode::Split {
                axis: cut.axis as u8,
                position: cut.position,
                low,
                high,
                bounds,
                open_area: cut.open_area,
            };
            stack.push((low, low_members));
            stack.push((high, high_members));
        }
    }
    tree.renumber();
    tree
}

/// Split the leaves of `over` (region ids) once more, ignoring the byte
/// limit. Returns whether the tree changed.
pub(crate) fn resplit(
    input: &PartitionInput,
    params: &PartitionParams,
    tree: &mut CutTree,
    over: &[u32],
) -> bool {
    let items = items(input);
    let mut changed = false;
    for &region in over {
        let node = tree.leaves[region as usize];
        if matches!(
            &tree.nodes[node as usize],
            CutNode::Leaf {
                unsplittable: true,
                ..
            }
        ) {
            continue;
        }
        let bounds = tree.nodes[node as usize].bounds();
        let members: Vec<u32> = (0..items.len() as u32)
            .filter(|&i| bounds.contains_half_open(items[i as usize].centroid))
            .collect();
        let t = totals(&items, &members);
        match best_cut(input, params, &items, &members, &bounds, t) {
            None => {
                if let CutNode::Leaf { unsplittable, .. } = &mut tree.nodes[node as usize] {
                    *unsplittable = true;
                }
            }
            Some(cut) => {
                let (low_bounds, high_bounds) = bounds.split(cut.axis, cut.position);
                let low = tree.nodes.len() as u32;
                tree.nodes.push(CutNode::Leaf {
                    bounds: low_bounds,
                    region: 0,
                    unsplittable: false,
                });
                let high = tree.nodes.len() as u32;
                tree.nodes.push(CutNode::Leaf {
                    bounds: high_bounds,
                    region: 0,
                    unsplittable: false,
                });
                tree.nodes[node as usize] = CutNode::Split {
                    axis: cut.axis as u8,
                    position: cut.position,
                    low,
                    high,
                    bounds,
                    open_area: cut.open_area,
                };
                changed = true;
            }
        }
    }
    tree.renumber();
    changed
}

#[derive(Clone, Copy, Debug)]
struct Cut {
    axis: usize,
    position: f64,
    open_area: f64,
    /// Score tuple, lower is better: sliver, open-area class, splits, imbalance.
    key: (u32, u32, u32, u64),
}

fn partition_members(items: &[Item], members: &[u32], cut: &Cut) -> (Vec<u32>, Vec<u32>) {
    let mut low = Vec::new();
    let mut high = Vec::new();
    for &m in members {
        if items[m as usize].centroid[cut.axis] < cut.position {
            low.push(m);
        } else {
            high.push(m);
        }
    }
    (low, high)
}

/// Relaxation ladder for the minimum share either side must keep, as a fraction.
const BALANCE_LADDER: [f64; 3] = [0.25, 0.15, 0.08];
/// Open area is bucketed on a log2 scale from this unit before it ranks
/// candidates, so near-equal doors tie and the next criterion decides. [E]
const AREA_UNIT: f64 = 1024.0;

/// Faces of `members` a plane at `position` on `axis` passes through.
fn faces_split_by(items: &[Item], members: &[u32], axis: usize, position: f64) -> u32 {
    members
        .iter()
        .filter(|&&m| {
            let item = &items[m as usize];
            item.face
                && item.aabb.min[axis] < position - 1.0e-6
                && item.aabb.max[axis] > position + 1.0e-6
        })
        .count() as u32
}

/// Thin `candidates` (ascending) to `max_cut_candidates`. An even spread
/// drops the planes that run between two walls, the ones that split nothing,
/// as soon as there are many, and the cut then lands inside a room. So the
/// candidates that split the fewest faces stay, and the last tier that does
/// not fit whole is spread evenly.
fn thin_candidates(
    items: &[Item],
    members: &[u32],
    axis: usize,
    candidates: &[f64],
    params: &PartitionParams,
) -> Vec<f64> {
    let keep = params.max_cut_candidates;
    if keep == 0 {
        return Vec::new();
    }
    let mut ranked: Vec<(u32, f64)> = candidates
        .iter()
        .map(|&p| (faces_split_by(items, members, axis, p), p))
        .collect();
    let mut splits: Vec<u32> = ranked.iter().map(|r| r.0).collect();
    splits.sort_unstable();
    let tier = splits[keep.min(splits.len()) - 1];
    ranked.retain(|r| r.0 <= tier);
    let sure = ranked.iter().filter(|r| r.0 < tier).count();
    let edge: Vec<f64> = ranked.iter().filter(|r| r.0 == tier).map(|r| r.1).collect();
    let room = keep.saturating_sub(sure).max(1);
    let mut out: Vec<f64> = ranked.iter().filter(|r| r.0 < tier).map(|r| r.1).collect();
    if edge.len() <= room {
        out.extend(edge);
    } else {
        let stride = edge.len() as f64 / room as f64;
        out.extend((0..room).map(|i| edge[(i as f64 * stride) as usize]));
    }
    out.sort_by(f64::total_cmp);
    out
}

fn best_cut(
    input: &PartitionInput,
    params: &PartitionParams,
    items: &[Item],
    members: &[u32],
    bounds: &Aabb,
    total: Totals,
) -> Option<Cut> {
    if members.is_empty() || total.bytes <= 0.0 {
        return None;
    }
    for balance in BALANCE_LADDER {
        let mut best: Option<Cut> = None;
        for axis in 0..3 {
            let lo = bounds.min[axis] + params.min_cell_extent;
            let hi = bounds.max[axis] - params.min_cell_extent;
            if lo >= hi {
                continue;
            }
            // Members sorted by centroid along the axis, with prefix bytes.
            let mut order: Vec<u32> = members.to_vec();
            order.sort_by(|&a, &b| {
                items[a as usize].centroid[axis]
                    .total_cmp(&items[b as usize].centroid[axis])
                    .then(a.cmp(&b))
            });
            let mut prefix = Vec::with_capacity(order.len() + 1);
            prefix.push(0.0f64);
            for &m in &order {
                let last = *prefix.last().unwrap();
                prefix.push(last + items[m as usize].bytes);
            }
            let low_bytes_at = |position: f64| -> f64 {
                let k = order.partition_point(|&m| items[m as usize].centroid[axis] < position);
                prefix[k]
            };

            let mut candidates: Vec<f64> = Vec::new();
            let mut position = (lo / params.cut_lattice).ceil() * params.cut_lattice;
            while position <= hi {
                candidates.push(position);
                position += params.cut_lattice;
            }
            for &m in members {
                let item = &items[m as usize];
                if item.face && item.aabb.extent(axis) <= 1.0e-6 {
                    let p = item.aabb.min[axis];
                    if p >= lo && p <= hi {
                        candidates.push((p * 1024.0).round() / 1024.0);
                    }
                }
            }
            candidates.sort_by(f64::total_cmp);
            candidates.dedup();
            candidates.retain(|&p| {
                let share = low_bytes_at(p) / total.bytes;
                share >= balance && share <= 1.0 - balance
            });
            if candidates.len() > params.max_cut_candidates {
                candidates = thin_candidates(items, members, axis, &candidates, params);
            }
            for &position in &candidates {
                let splits = members
                    .iter()
                    .filter(|&&m| {
                        let item = &items[m as usize];
                        item.face
                            && item.aabb.min[axis] < position - 1.0e-6
                            && item.aabb.max[axis] > position + 1.0e-6
                    })
                    .count() as u32;
                let open_area = open_cross_section(input, params, bounds, axis, position);
                let class = (1.0 + open_area / AREA_UNIT).log2().ceil() as u32;
                let imbalance = (low_bytes_at(position) - total.bytes * 0.5).abs() as u64;
                // A cut that leaves a child thinner than `min_sliver_extent` cuts
                // off a sliver: a door wall and its vestibule sliced from a
                // module, say. Such a cut is cheap (the door is nearly free) but
                // a region that thin sees through both its sides, so the crossing
                // into it newly needs most of what the module beyond needs and
                // the drive gate trips. Any cut without a sliver ranks ahead of
                // every cut with one. [E]
                let sliver = u32::from(
                    position - bounds.min[axis] < params.min_sliver_extent
                        || bounds.max[axis] - position < params.min_sliver_extent,
                );
                let candidate = Cut {
                    axis,
                    position,
                    open_area,
                    key: (sliver, class, splits, imbalance),
                };
                let better = match &best {
                    None => true,
                    Some(current) => {
                        (candidate.key, candidate.axis, candidate.position.to_bits())
                            < (current.key, current.axis, current.position.to_bits())
                    }
                };
                if better {
                    best = Some(candidate);
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

/// Open area of the cell's cross-section at `axis = position`, sampled on a
/// grid of at most `aperture_samples` points. A sample is open when both
/// sides of the plane at that point are outside every solid.
pub(crate) fn open_cross_section(
    input: &PartitionInput,
    params: &PartitionParams,
    bounds: &Aabb,
    axis: usize,
    position: f64,
) -> f64 {
    let (a, b) = other_axes(axis);
    let (ea, eb) = (bounds.extent(a), bounds.extent(b));
    if ea <= 0.0 || eb <= 0.0 {
        return 0.0;
    }
    let (na, nb) = grid_dims(ea, eb, params.aperture_samples);
    let mut open = 0usize;
    for i in 0..na {
        for j in 0..nb {
            let mut point = [0.0; 3];
            point[axis] = position;
            point[a] = bounds.min[a] + ea * (i as f64 + 0.5) / na as f64;
            point[b] = bounds.min[b] + eb * (j as f64 + 0.5) / nb as f64;
            if sample_open(input, point, axis) {
                open += 1;
            }
        }
    }
    ea * eb * open as f64 / (na * nb) as f64
}

pub(crate) fn sample_open(input: &PartitionInput, point: V3, axis: usize) -> bool {
    let mut front = point;
    let mut back = point;
    front[axis] += 0.5;
    back[axis] -= 0.5;
    !input.solids.point_in_solid(front) && !input.solids.point_in_solid(back)
}

pub(crate) fn other_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// Grid of about `samples` points matching the rectangle's aspect.
pub(crate) fn grid_dims(ea: f64, eb: f64, samples: usize) -> (usize, usize) {
    let samples = samples.max(1) as f64;
    let na = ((samples * ea / eb).sqrt().round() as usize).clamp(1, samples as usize);
    let nb = ((samples / na as f64).round() as usize).max(1);
    (na, nb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(x0: f64, x1: f64) -> Item {
        Item {
            aabb: Aabb {
                min: [x0, 0.0, 0.0],
                max: [x1, 1.0, 1.0],
            },
            centroid: [(x0 + x1) / 2.0, 0.5, 0.5],
            bytes: 100.0,
            face: true,
            vertices: 4,
        }
    }

    #[test]
    fn thinning_keeps_the_planes_that_split_no_face() {
        // Ten walls 8 wide every 10 units: 99 candidate planes, about a third
        // of them between walls. An even spread to 32 would drop some of those.
        let items: Vec<Item> = (0..10)
            .map(|k| wall(f64::from(k) * 10.0, f64::from(k) * 10.0 + 8.0))
            .collect();
        let members: Vec<u32> = (0..10).collect();
        let candidates: Vec<f64> = (1..100).map(f64::from).collect();
        let params = PartitionParams::default();
        let clear: Vec<f64> = candidates
            .iter()
            .copied()
            .filter(|&p| faces_split_by(&items, &members, 0, p) == 0)
            .collect();
        assert!(clear.len() > 20 && clear.len() <= params.max_cut_candidates);
        let kept = thin_candidates(&items, &members, 0, &candidates, &params);
        assert!(kept.len() <= params.max_cut_candidates);
        for plane in clear {
            assert!(kept.contains(&plane), "plane {plane} was dropped");
        }
        // With more clear planes than room, the spread covers the whole range.
        let clear_only: Vec<f64> = (0..200).map(|i| 8.0 + f64::from(i) * 0.01).collect();
        let kept = thin_candidates(&items, &members, 0, &clear_only, &params);
        assert_eq!(kept.len(), params.max_cut_candidates);
        assert!(kept[0] < 8.1 && *kept.last().unwrap() > 9.8);
    }
}
