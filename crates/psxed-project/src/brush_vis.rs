//! Quake-style portal-flow visibility for compiled brush BSP leaves.
//!
//! This follows id Software's `qutils/VIS/FLOW.C`: build a conservative
//! directed-portal `mightsee` flood, process the least-complex portals first,
//! clip each recursive target through source/pass separator planes, then OR
//! outgoing portal results into leaf PVS rows. The editor's Draft cook keeps
//! the cheaper connected-component visibility path; Release uses this pass.

use crate::brush_compile::{normalized_plane, CompiledSurfaceBsp};
use crate::brush_portal::CompiledPortal;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Original Quake VIS winding/plane tolerance (`VIS.H::ON_EPSILON`).
const ON_EPSILON: f64 = 0.1;

#[derive(Clone, Copy, Debug)]
struct VisPlane {
    normal: [f64; 3],
    distance: f64,
}

impl VisPlane {
    fn inverse(self) -> Self {
        Self {
            normal: self.normal.map(|value| -value),
            distance: -self.distance,
        }
    }

    fn signed_distance(self, point: [f64; 3]) -> f64 {
        dot(self.normal, point) - self.distance
    }
}

#[derive(Clone, Debug)]
struct DirectedPortal {
    from_leaf: usize,
    to_leaf: usize,
    plane: VisPlane,
    winding: Vec<[f64; 3]>,
}

#[derive(Clone)]
struct FlowStack {
    source: Vec<[f64; 3]>,
    pass: Option<Vec<[f64; 3]>>,
    portal_plane: VisPlane,
    mightsee: Vec<u64>,
}

/// Build uncompressed PVS rows in runtime visible-leaf order.
///
/// The output is indexed by host BSP leaf. Solid leaves have `None`; every
/// visible leaf has a byte row whose bit zero represents runtime leaf one.
pub(crate) fn quake_portal_flow_rows(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    leaf_mapping: &[i16],
    visible_leaves: usize,
) -> Vec<Option<Vec<u8>>> {
    let directed = directed_open_portals(bsp, portals, leaf_mapping);
    let mut outgoing = vec![Vec::new(); visible_leaves];
    for (index, portal) in directed.iter().enumerate() {
        outgoing[portal.from_leaf].push(index);
    }

    let word_count = visible_leaves.div_ceil(64);
    let mightsee: Vec<Vec<u64>> = directed
        .iter()
        .map(|portal| base_portal_visibility(portal, &directed, &outgoing, word_count))
        .collect();
    let mut order: Vec<usize> = (0..directed.len()).collect();
    order.sort_by_key(|&portal| bit_count(&mightsee[portal]));

    let worker_count = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, 8)
        .min(directed.len().max(1));
    let flow_started = std::time::Instant::now();
    let report_progress = directed.len() >= 1024;
    if report_progress {
        eprintln!(
            "[brush-vis] Quake portal flow: {visible_leaves} visible leaves, {} directed portals, {worker_count} workers",
            directed.len()
        );
    }
    let next_portal = AtomicUsize::new(0);
    let completed_portals = AtomicUsize::new(0);
    let status: Vec<_> = (0..directed.len()).map(|_| AtomicU8::new(0)).collect();
    let portal_visibility: Vec<_> = (0..directed.len()).map(|_| OnceLock::new()).collect();
    // Quake's VIS worker queue marks in-flight portals as `stat_working`:
    // flows completed earlier may contribute their exact visbits, while other
    // jobs conservatively contribute `mightsee`. An atomic queue reproduces
    // that rule while allowing each worker to claim the next least-complex
    // portal as soon as it finishes, with no slow-portal batch barrier.
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let directed = &directed;
            let outgoing = &outgoing;
            let mightsee = &mightsee;
            let portal_visibility = &portal_visibility;
            let status = &status;
            let order = &order;
            let next_portal = &next_portal;
            let completed_portals = &completed_portals;
            scope.spawn(move || loop {
                let order_index = next_portal.fetch_add(1, Ordering::Relaxed);
                let Some(&portal_index) = order.get(order_index) else {
                    break;
                };
                status[portal_index].store(1, Ordering::Release);
                let visibility = flow_portal(
                    portal_index,
                    directed,
                    outgoing,
                    mightsee,
                    portal_visibility,
                    status,
                    visible_leaves,
                    word_count,
                );
                portal_visibility[portal_index]
                    .set(visibility)
                    .expect("portal VIS result must be published once");
                status[portal_index].store(2, Ordering::Release);
                let completed = completed_portals.fetch_add(1, Ordering::Relaxed) + 1;
                if report_progress && (completed.is_multiple_of(1024) || completed == order.len()) {
                    eprintln!(
                        "[brush-vis] portal flow {completed}/{} ({:.1}s)",
                        order.len(),
                        flow_started.elapsed().as_secs_f32()
                    );
                }
            });
        }
    });
    if report_progress {
        eprintln!(
            "[brush-vis] portal flow complete in {:.1}s",
            flow_started.elapsed().as_secs_f32()
        );
    }
    let portal_visibility: Vec<Vec<u64>> = portal_visibility
        .into_iter()
        .map(|visibility| {
            visibility
                .into_inner()
                .expect("every directed portal must be flowed")
        })
        .collect();

    let mut rows = vec![vec![0u64; word_count]; visible_leaves];
    for leaf in 0..visible_leaves {
        set_bit(&mut rows[leaf], leaf);
        for &portal in &outgoing[leaf] {
            union_bits(&mut rows[leaf], &portal_visibility[portal]);
        }
    }

    // Visibility is physically reciprocal. Quake's directed flows normally
    // arrive at that result independently; explicitly closing the relation
    // prevents floating-point tie direction from creating a false negative.
    for left in 0..visible_leaves {
        for right in left + 1..visible_leaves {
            if bit_is_set(&rows[left], right) || bit_is_set(&rows[right], left) {
                set_bit(&mut rows[left], right);
                set_bit(&mut rows[right], left);
            }
        }
    }

    let row_bytes = visible_leaves.div_ceil(8);
    leaf_mapping
        .iter()
        .map(|&runtime_leaf| {
            if runtime_leaf <= 0 {
                return None;
            }
            let words = &rows[runtime_leaf as usize - 1];
            let mut bytes = vec![0u8; row_bytes];
            for (byte_index, byte) in bytes.iter_mut().enumerate() {
                *byte = ((words[byte_index >> 3] >> ((byte_index & 7) * 8)) & 0xff) as u8;
            }
            Some(bytes)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn flow_portal(
    portal_index: usize,
    portals: &[DirectedPortal],
    outgoing: &[Vec<usize>],
    mightsee: &[Vec<u64>],
    portal_visibility: &[OnceLock<Vec<u64>>],
    status: &[AtomicU8],
    visible_leaves: usize,
    word_count: usize,
) -> Vec<u64> {
    let portal = &portals[portal_index];
    let mut leaf_visibility = vec![0; word_count];
    let mut path = vec![false; visible_leaves];
    let head = FlowStack {
        source: portal.winding.clone(),
        pass: None,
        portal_plane: portal.plane,
        mightsee: mightsee[portal_index].clone(),
    };
    recursive_leaf_flow(
        portal.to_leaf,
        portal.plane,
        &head,
        portals,
        outgoing,
        mightsee,
        portal_visibility,
        status,
        &mut leaf_visibility,
        &mut path,
    );
    leaf_visibility
}

fn directed_open_portals(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    leaf_mapping: &[i16],
) -> Vec<DirectedPortal> {
    let mut output = Vec::new();
    for portal in portals {
        if !bsp.leaves[portal.front_leaf].contents.is_visible()
            || !bsp.leaves[portal.back_leaf].contents.is_visible()
        {
            continue;
        }
        let back = leaf_mapping[portal.back_leaf] as usize - 1;
        let front = leaf_mapping[portal.front_leaf] as usize - 1;
        let (normal, distance) = normalized_plane(portal.plane);
        let plane = VisPlane { normal, distance };
        output.push(DirectedPortal {
            from_leaf: back,
            to_leaf: front,
            plane,
            winding: portal.vertices.clone(),
        });
        output.push(DirectedPortal {
            from_leaf: front,
            to_leaf: back,
            plane: plane.inverse(),
            winding: portal.vertices.iter().rev().copied().collect(),
        });
    }
    output
}

fn base_portal_visibility(
    source: &DirectedPortal,
    portals: &[DirectedPortal],
    outgoing: &[Vec<usize>],
    word_count: usize,
) -> Vec<u64> {
    let mut visible = vec![0u64; word_count];
    let mut pending = vec![source.to_leaf];
    set_bit(&mut visible, source.to_leaf);
    while let Some(leaf) = pending.pop() {
        for &candidate_index in &outgoing[leaf] {
            let candidate = &portals[candidate_index];
            if !base_portals_may_see(source, candidate) || bit_is_set(&visible, candidate.to_leaf) {
                continue;
            }
            set_bit(&mut visible, candidate.to_leaf);
            pending.push(candidate.to_leaf);
        }
    }
    visible
}

fn base_portals_may_see(source: &DirectedPortal, target: &DirectedPortal) -> bool {
    if !target
        .winding
        .iter()
        .any(|&point| source.plane.signed_distance(point) > ON_EPSILON)
    {
        return false;
    }
    source
        .winding
        .iter()
        .any(|&point| target.plane.signed_distance(point) < -ON_EPSILON)
}

#[allow(clippy::too_many_arguments)]
fn recursive_leaf_flow(
    leaf: usize,
    head_plane: VisPlane,
    previous: &FlowStack,
    portals: &[DirectedPortal],
    outgoing: &[Vec<usize>],
    mightsee: &[Vec<u64>],
    portal_visibility: &[OnceLock<Vec<u64>>],
    status: &[AtomicU8],
    leaf_visibility: &mut [u64],
    path: &mut [bool],
) {
    if path[leaf] {
        return;
    }
    path[leaf] = true;
    set_bit(leaf_visibility, leaf);

    for &portal_index in &outgoing[leaf] {
        let portal = &portals[portal_index];
        if path[portal.to_leaf] || !bit_is_set(&previous.mightsee, portal.to_leaf) {
            continue;
        }
        let test = if status[portal_index].load(Ordering::Acquire) == 2 {
            portal_visibility[portal_index]
                .get()
                .expect("done portal must have published visbits")
        } else {
            &mightsee[portal_index]
        };
        let next_mightsee = intersect_bits(&previous.mightsee, test);
        if !has_unseen_bits(&next_mightsee, leaf_visibility) {
            continue;
        }

        let back_plane = portal.plane.inverse();
        if same_normal(previous.portal_plane.normal, back_plane.normal) {
            continue;
        }
        let Some(mut target) = clip_winding(&portal.winding, head_plane) else {
            continue;
        };
        if previous.pass.is_none() {
            let stack = FlowStack {
                source: previous.source.clone(),
                pass: Some(target),
                portal_plane: portal.plane,
                mightsee: next_mightsee,
            };
            recursive_leaf_flow(
                portal.to_leaf,
                head_plane,
                &stack,
                portals,
                outgoing,
                mightsee,
                portal_visibility,
                status,
                leaf_visibility,
                path,
            );
            continue;
        }

        let Some(clipped_target) = clip_winding(&target, previous.portal_plane) else {
            continue;
        };
        target = clipped_target;
        let Some(source) = clip_winding(&previous.source, back_plane) else {
            continue;
        };
        let Some(target) = clip_to_separators(
            &source,
            previous.pass.as_deref().expect("checked pass"),
            target,
            false,
        ) else {
            continue;
        };
        let Some(target) = clip_to_separators(
            previous.pass.as_deref().expect("checked pass"),
            &source,
            target,
            true,
        ) else {
            continue;
        };
        let stack = FlowStack {
            source,
            pass: Some(target),
            portal_plane: portal.plane,
            mightsee: next_mightsee,
        };
        recursive_leaf_flow(
            portal.to_leaf,
            head_plane,
            &stack,
            portals,
            outgoing,
            mightsee,
            portal_visibility,
            status,
            leaf_visibility,
            path,
        );
    }

    path[leaf] = false;
}

fn clip_to_separators(
    source: &[[f64; 3]],
    pass: &[[f64; 3]],
    mut target: Vec<[f64; 3]>,
    flip_clip: bool,
) -> Option<Vec<[f64; 3]>> {
    for edge_start in 0..source.len() {
        let edge_end = (edge_start + 1) % source.len();
        let edge = subtract(source[edge_end], source[edge_start]);
        for pass_vertex in 0..pass.len() {
            let candidate = subtract(pass[pass_vertex], source[edge_start]);
            let mut normal = cross(edge, candidate);
            let length_squared = dot(normal, normal);
            if length_squared < ON_EPSILON {
                continue;
            }
            normal = scale(normal, length_squared.sqrt().recip());
            let mut plane = VisPlane {
                normal,
                distance: dot(pass[pass_vertex], normal),
            };

            let mut source_side = 0i8;
            for (index, &point) in source.iter().enumerate() {
                if index == edge_start || index == edge_end {
                    continue;
                }
                let distance = plane.signed_distance(point);
                if distance < -ON_EPSILON {
                    source_side = -1;
                    break;
                }
                if distance > ON_EPSILON {
                    source_side = 1;
                    break;
                }
            }
            if source_side == 0 {
                continue;
            }
            if source_side > 0 {
                plane = plane.inverse();
            }

            let mut pass_front = false;
            let mut separates = true;
            for (index, &point) in pass.iter().enumerate() {
                if index == pass_vertex {
                    continue;
                }
                let distance = plane.signed_distance(point);
                if distance < -ON_EPSILON {
                    separates = false;
                    break;
                }
                pass_front |= distance > ON_EPSILON;
            }
            if !separates || !pass_front {
                continue;
            }
            if flip_clip {
                plane = plane.inverse();
            }
            target = clip_winding(&target, plane)?;
        }
    }
    Some(target)
}

/// Clip a winding to the positive side, matching Quake VIS `ClipWinding`
/// with `keepon=false`.
fn clip_winding(input: &[[f64; 3]], plane: VisPlane) -> Option<Vec<[f64; 3]>> {
    if input.len() < 3 {
        return None;
    }
    let distances: Vec<f64> = input
        .iter()
        .map(|&point| plane.signed_distance(point))
        .collect();
    let has_front = distances.iter().any(|&distance| distance > ON_EPSILON);
    let has_back = distances.iter().any(|&distance| distance < -ON_EPSILON);
    if !has_front {
        return None;
    }
    if !has_back {
        return Some(input.to_vec());
    }

    let mut output = Vec::with_capacity(input.len() + 4);
    for index in 0..input.len() {
        let next = (index + 1) % input.len();
        let point = input[index];
        let distance = distances[index];
        let next_distance = distances[next];
        if distance >= -ON_EPSILON {
            output.push(point);
        }
        if (distance > ON_EPSILON && next_distance < -ON_EPSILON)
            || (distance < -ON_EPSILON && next_distance > ON_EPSILON)
        {
            let amount = distance / (distance - next_distance);
            output.push(add(point, scale(subtract(input[next], point), amount)));
        }
    }
    (output.len() >= 3).then_some(output)
}

fn same_normal(left: [f64; 3], right: [f64; 3]) -> bool {
    left == right
}

fn set_bit(bits: &mut [u64], index: usize) {
    bits[index >> 6] |= 1u64 << (index & 63);
}

fn bit_is_set(bits: &[u64], index: usize) -> bool {
    bits[index >> 6] & (1u64 << (index & 63)) != 0
}

fn union_bits(output: &mut [u64], input: &[u64]) {
    for (output, input) in output.iter_mut().zip(input) {
        *output |= *input;
    }
}

fn intersect_bits(left: &[u64], right: &[u64]) -> Vec<u64> {
    left.iter()
        .zip(right)
        .map(|(left, right)| left & right)
        .collect()
}

fn has_unseen_bits(mightsee: &[u64], visible: &[u64]) -> bool {
    mightsee
        .iter()
        .zip(visible)
        .any(|(mightsee, visible)| mightsee & !visible != 0)
}

fn bit_count(bits: &[u64]) -> u32 {
    bits.iter().map(|word| word.count_ones()).sum()
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(value: [f64; 3], amount: f64) -> [f64; 3] {
    [value[0] * amount, value[1] * amount, value[2] * amount]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Plane;
    use crate::brush_compile::{BspChild, BspLeafContents, CompiledBspLeaf};

    fn plane(normal: [i64; 3], distance: i64) -> Plane {
        Plane {
            normal,
            dist: distance,
        }
    }

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
            plane: plane([1, 0, 0], x),
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
            plane: plane([0, 1, 0], y),
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

    fn row_bit(rows: &[Option<Vec<u8>>], leaf: usize, visible: usize) -> bool {
        rows[leaf].as_ref().expect("visible leaf")[visible >> 3] & (1 << (visible & 7)) != 0
    }

    #[test]
    fn aligned_portal_chain_remains_visible_end_to_end() {
        let bsp = empty_bsp(4);
        let portals = vec![x_portal(0, 1, 0), x_portal(1, 2, 10), x_portal(2, 3, 20)];
        let rows = quake_portal_flow_rows(&bsp, &portals, &[1, 2, 3, 4], 4);
        assert!(row_bit(&rows, 0, 3));
        assert!(row_bit(&rows, 3, 0));
    }

    #[test]
    fn separator_flow_culls_a_portal_hidden_around_a_right_angle() {
        let bsp = empty_bsp(4);
        let portals = vec![
            x_portal(0, 1, 0),
            x_portal(1, 2, 10),
            y_portal(2, 3, 10, [9.0, 11.0]),
        ];
        let rows = quake_portal_flow_rows(&bsp, &portals, &[1, 2, 3, 4], 4);
        assert!(row_bit(&rows, 0, 0));
        assert!(row_bit(&rows, 0, 1));
        assert!(row_bit(&rows, 0, 2));
        assert!(!row_bit(&rows, 0, 3));
        assert!(!row_bit(&rows, 3, 0));
    }

    #[test]
    fn disconnected_visible_leaf_sees_only_itself() {
        let bsp = empty_bsp(2);
        let rows = quake_portal_flow_rows(&bsp, &[], &[1, 2], 2);
        assert!(row_bit(&rows, 0, 0));
        assert!(!row_bit(&rows, 0, 1));
        assert!(row_bit(&rows, 1, 1));
        assert!(!row_bit(&rows, 1, 0));
    }
}
