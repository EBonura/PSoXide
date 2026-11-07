//! Plan components and the rectangles a floating solid is built from.

use std::collections::HashMap;

use super::columns::PlanCell;

/// A connected patch of plan cells that hold a floating solid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    /// Cells in the patch.
    pub cells: usize,
    /// Smallest and largest cell `[x, z]`.
    pub bounds: ([i32; 2], [i32; 2]),
    /// Mean open space under the solid, grid units.
    pub mean_clearance: i32,
    /// Mean thickness, grid units.
    pub mean_thickness: i32,
}

/// Running totals of one component.
#[derive(Default)]
struct Tally {
    cells: usize,
    lo: [i32; 2],
    hi: [i32; 2],
    clearance: i64,
    thickness: i64,
}

/// Label every cell with its component (cells touching along an edge are
/// one) and describe the components, indexed by label.
pub fn components(cells: &[PlanCell]) -> (Vec<usize>, Vec<Component>) {
    let mut nodes: HashMap<(i32, i32), usize> = HashMap::new();
    for cell in cells {
        let next = nodes.len();
        nodes.entry((cell.x, cell.z)).or_insert(next);
    }
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (&(x, z), &a) in &nodes {
        for next in [(x + 1, z), (x, z + 1)] {
            if let Some(&b) = nodes.get(&next) {
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut labels: HashMap<usize, usize> = HashMap::new();
    let mut sums: Vec<Tally> = Vec::new();
    let mut out = Vec::with_capacity(cells.len());
    for cell in cells {
        let node = nodes[&(cell.x, cell.z)];
        let r = root(&mut parent, node);
        let next = labels.len();
        let label = *labels.entry(r).or_insert(next);
        if label == sums.len() {
            sums.push(Tally {
                lo: [cell.x, cell.z],
                hi: [cell.x, cell.z],
                ..Tally::default()
            });
        }
        let s = &mut sums[label];
        s.cells += 1;
        s.lo = [s.lo[0].min(cell.x), s.lo[1].min(cell.z)];
        s.hi = [s.hi[0].max(cell.x), s.hi[1].max(cell.z)];
        s.clearance += i64::from(cell.clearance);
        s.thickness += i64::from(cell.top - cell.bottom);
        out.push(label);
    }
    let described = sums
        .into_iter()
        .map(|t| Component {
            cells: t.cells,
            bounds: (t.lo, t.hi),
            mean_clearance: (t.clearance / t.cells as i64) as i32,
            mean_thickness: (t.thickness / t.cells as i64) as i32,
        })
        .collect();
    (out, described)
}

/// A rectangle of plan cells with one underside and one top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Smallest cell along x.
    pub x: i32,
    /// Smallest cell along z.
    pub z: i32,
    /// Cells along x.
    pub width: i32,
    /// Cells along z.
    pub depth: i32,
    /// Underside, grid units.
    pub bottom: i32,
    /// Top, grid units.
    pub top: i32,
}

impl Rect {
    /// Cells covered.
    pub fn area(&self) -> i32 {
        self.width * self.depth
    }
}

/// Which axis a greedy pass grows first.
#[derive(Clone, Copy)]
enum Grow {
    AlongX,
    AlongZ,
}

/// Cover `cells` with rectangles whose cells agree on underside and top
/// within `tolerance`, taking whichever growth order needs fewer.
pub fn pack(cells: &[PlanCell], tolerance: i32) -> Vec<Rect> {
    let a = greedy(cells, tolerance, Grow::AlongX);
    let b = greedy(cells, tolerance, Grow::AlongZ);
    if b.len() < a.len() {
        b
    } else {
        a
    }
}

/// Widen `rects` over the `rim` cells that run along their sides.
///
/// A rectangle takes the strip of cells beside one of its sides when at least
/// `percent` of the strip is rim cells with ends within `tolerance` of the
/// rectangle's. The strip is taken whole, so a deck gets its edge back as one
/// wider slab instead of a scatter of single-cell ones. Each rectangle grows
/// by at most `reach` cells on a side.
pub fn grow_rects(
    rects: &mut [Rect],
    rim: &[PlanCell],
    tolerance: i32,
    percent: usize,
    reach: i32,
) {
    let mut at: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, cell) in rim.iter().enumerate() {
        at.entry((cell.x, cell.z)).or_default().push(i);
    }
    let mut claimed = vec![false; rim.len()];
    for rect in rects.iter_mut() {
        for _ in 0..reach.max(0) {
            let mut grew = false;
            for side in 0..4 {
                // The strip beside the side: first cell, step, length.
                let (start, step, len) = match side {
                    0 => ((rect.x - 1, rect.z), (0, 1), rect.depth),
                    1 => ((rect.x + rect.width, rect.z), (0, 1), rect.depth),
                    2 => ((rect.x, rect.z - 1), (1, 0), rect.width),
                    _ => ((rect.x, rect.z + rect.depth), (1, 0), rect.width),
                };
                let cell_at = |k: i32| (start.0 + step.0 * k, start.1 + step.1 * k);
                let mut hits: Vec<usize> = Vec::new();
                for k in 0..len {
                    let Some(list) = at.get(&cell_at(k)) else {
                        continue;
                    };
                    if let Some(&i) = list.iter().find(|&&i| {
                        !claimed[i]
                            && (rim[i].bottom - rect.bottom).abs() <= tolerance
                            && (rim[i].top - rect.top).abs() <= tolerance
                    }) {
                        hits.push(i);
                    }
                }
                if hits.is_empty() || hits.len() * 100 < len as usize * percent {
                    continue;
                }
                for i in hits {
                    claimed[i] = true;
                }
                match side {
                    0 => {
                        rect.x -= 1;
                        rect.width += 1;
                    }
                    1 => rect.width += 1,
                    2 => {
                        rect.z -= 1;
                        rect.depth += 1;
                    }
                    _ => rect.depth += 1,
                }
                grew = true;
            }
            if !grew {
                break;
            }
        }
    }
}

fn median(values: &mut [i32]) -> i32 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn greedy(cells: &[PlanCell], tolerance: i32, grow: Grow) -> Vec<Rect> {
    // Cell lookup by position; a position may hold several solids.
    let mut at: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, cell) in cells.iter().enumerate() {
        at.entry((cell.x, cell.z)).or_default().push(i);
    }
    let mut order: Vec<usize> = (0..cells.len()).collect();
    order.sort_unstable_by_key(|&i| match grow {
        Grow::AlongX => (cells[i].z, cells[i].x, cells[i].bottom),
        Grow::AlongZ => (cells[i].x, cells[i].z, cells[i].bottom),
    });
    let mut used = vec![false; cells.len()];
    // `along` steps in the growth direction, `across` the other way.
    let step = |p: (i32, i32), along: i32, across: i32| match grow {
        Grow::AlongX => (p.0 + along, p.1 + across),
        Grow::AlongZ => (p.0 + across, p.1 + along),
    };
    let mut out = Vec::new();
    for seed in order {
        if used[seed] {
            continue;
        }
        let s = cells[seed];
        let find = |p: (i32, i32), used: &[bool]| -> Option<usize> {
            at.get(&p)?.iter().copied().find(|&i| {
                !used[i]
                    && (cells[i].bottom - s.bottom).abs() <= tolerance
                    && (cells[i].top - s.top).abs() <= tolerance
            })
        };
        let origin = (s.x, s.z);
        let mut length = 1;
        while find(step(origin, length, 0), &used).is_some() {
            length += 1;
        }
        let mut rows = 1;
        while (0..length).all(|k| find(step(origin, k, rows), &used).is_some()) {
            rows += 1;
        }
        let mut bottoms = Vec::new();
        let mut tops = Vec::new();
        for row in 0..rows {
            for k in 0..length {
                if let Some(i) = find(step(origin, k, row), &used) {
                    used[i] = true;
                    bottoms.push(cells[i].bottom);
                    tops.push(cells[i].top);
                }
            }
        }
        let (width, depth) = match grow {
            Grow::AlongX => (length, rows),
            Grow::AlongZ => (rows, length),
        };
        out.push(Rect {
            x: s.x,
            z: s.z,
            width,
            depth,
            bottom: median(&mut bottoms),
            top: median(&mut tops),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: i32, z: i32, bottom: i32, top: i32) -> PlanCell {
        PlanCell {
            x,
            z,
            bottom,
            top,
            clearance: 3000,
        }
    }

    fn block(x0: i32, z0: i32, w: i32, d: i32, bottom: i32, top: i32) -> Vec<PlanCell> {
        (z0..z0 + d)
            .flat_map(|z| (x0..x0 + w).map(move |x| cell(x, z, bottom, top)))
            .collect()
    }

    #[test]
    fn touching_cells_are_one_component_and_apart_are_two() {
        let mut cells = block(0, 0, 4, 2, 4096, 5120);
        cells.extend(block(10, 10, 2, 2, 2048, 3072));
        let (labels, found) = components(&cells);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].cells, 8);
        assert_eq!(found[0].bounds, ([0, 0], [3, 1]));
        assert_eq!(found[0].mean_thickness, 1024);
        assert_eq!(found[1].cells, 4);
        assert_eq!(labels[0], 0);
        assert_eq!(labels[8], 1);
    }

    #[test]
    fn a_plain_deck_is_one_rectangle() {
        let cells = block(3, 5, 12, 3, 4096, 5120);
        let rects = pack(&cells, 256);
        assert_eq!(
            rects,
            vec![Rect {
                x: 3,
                z: 5,
                width: 12,
                depth: 3,
                bottom: 4096,
                top: 5120
            }]
        );
    }

    #[test]
    fn an_l_shape_is_two_and_a_step_in_height_splits() {
        let mut cells = block(0, 0, 6, 2, 4096, 5120);
        cells.extend(block(0, 2, 2, 4, 4096, 5120));
        assert_eq!(pack(&cells, 256).len(), 2);
        // The same deck with its far half 512 higher is two slabs.
        let mut stepped = block(0, 0, 4, 2, 4096, 5120);
        stepped.extend(block(4, 0, 4, 2, 4608, 5632));
        let rects = pack(&stepped, 256);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects.iter().map(Rect::area).sum::<i32>(), 16);
        // Within the tolerance the wobble is absorbed.
        let mut wobble = block(0, 0, 4, 2, 4096, 5120);
        wobble.extend(block(4, 0, 4, 2, 4160, 5184));
        assert_eq!(pack(&wobble, 256).len(), 1);
    }

    #[test]
    fn two_levels_over_one_cell_are_both_covered() {
        let mut cells = block(0, 0, 3, 3, 4096, 5120);
        cells.extend(block(0, 0, 3, 3, 8192, 9216));
        let rects = pack(&cells, 256);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects.iter().map(Rect::area).sum::<i32>(), 18);
    }
}
