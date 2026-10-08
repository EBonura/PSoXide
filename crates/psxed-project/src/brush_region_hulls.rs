//! Solid-leaf collision hull BSPs built from expanded brush polytopes.
//!
//! The chain compiler in `brush_collision_hulls` ends every spatial leaf in
//! per-brush plane chains: a point query pays one node per rejecting plane for
//! each brush of the leaf, and a brush that straddles a split repeats its
//! whole chain on both sides. This module builds the real thing instead. The
//! input is the same set of convex polytopes (the brushes Minkowski-expanded
//! by the body box, bevel planes included), each already reduced to packed
//! plane records, and the output is the same 6-byte clipnode records over the
//! same 14-byte plane records, so the runtime tracer is untouched.
//!
//! Construction is a brush-based BSP. A node owns a convex cell and, for every
//! brush that overlaps it, the fragment `brush ∩ cell`. A cell is a leaf as
//! soon as the highest-precedence overlapping brush covers all of it (that
//! brush's contents) or no brush overlaps it (empty). Otherwise it is split on
//! a plane taken from a fragment's own supporting planes: axial planes first
//! (they keep heightfields logarithmic), then the brushes' sloped and bevel
//! planes when no axial plane makes enough progress. Fragments that straddle
//! the plane are clipped, not duplicated whole.
//!
//! Geometry is done in `f64` over the *decoded packed records*, not over the
//! authored faces, so the solid the BSP describes is the solid the chain
//! compiler's records describe, quantisation included.

use crate::brush_collision_hulls::{
    intern_plane, limit, CollisionHullCompileError, PreparedHullBrush,
};
use crate::brush_compile::pack_normalized_plane;
use psx_bsp::collision::CONTENTS_EMPTY;
use std::rc::Rc;

type V3 = [f64; 3];

/// Plane-side tolerance, world units. Matches `HULL_EPSILON` in the chain
/// compiler and is far below the tracer's own contact margin.
const EPS: f64 = 1.0 / 1024.0;
/// Cells with an inradius estimate (3 V / S) under this are numerical slivers
/// from quantised planes: they become leaves without further splitting.
const THIN_CELL: f64 = 1.0 / 128.0;
/// Fragments are kept down to numerical noise. Dropping a merely thin one
/// would punch a hairline hole in a solid, which a trace then reports as a
/// contact.
const THIN_FRAGMENT: f64 = 1.0e-6;
/// Deeper than this the build gives up (the tracer defers at most 64 pieces).
const MAX_DEPTH: usize = 56;
/// Tunable scoring knobs. Fixed in production; the tests override them from
/// `HULL_PARAMS="name=value,..."` to sweep without recompiling.
#[derive(Clone, Copy)]
struct Params {
    split_charge: f64,
    balance: f64,
    general_factor: f64,
    axial_candidates: usize,
    /// 1.0 weights a fragment by its facet count, 0.0 counts it once.
    facet_weight: f64,
    /// Only planes carrying a real facet of a live fragment are general
    /// candidates.
    facet_only: bool,
    /// 1: axial splits only at the brushes' own axial planes (existing
    /// records, so planes are shared and the table stays small); 0: at
    /// every fragment extent, which mints a fresh plane per cut.
    axial_mode: u8,
}

fn params() -> Params {
    #[allow(unused_mut)]
    let mut p = Params {
        split_charge: SPLIT_CHARGE,
        balance: BALANCE_WEIGHT,
        general_factor: GENERAL_FACTOR,
        axial_candidates: AXIAL_CANDIDATES,
        facet_weight: 1.0,
        facet_only: true,
        axial_mode: 1,
    };
    #[cfg(test)]
    if let Ok(spec) = std::env::var("HULL_PARAMS") {
        for item in spec.split(',') {
            let Some((key, value)) = item.split_once('=') else {
                continue;
            };
            let Ok(v) = value.parse::<f64>() else {
                continue;
            };
            match key {
                "split" => p.split_charge = v,
                "balance" => p.balance = v,
                "general" => p.general_factor = v,
                "axial" => p.axial_candidates = v as usize,
                "facets" => p.facet_weight = v,
                "facet_only" => p.facet_only = v != 0.0,
                "axial_mode" => p.axial_mode = v as u8,
                _ => {}
            }
        }
    }
    p
}

/// Axial split positions scored per axis and node.
const AXIAL_CANDIDATES: usize = 24;
/// General (sloped or bevel) split planes scored per node.
const GENERAL_CANDIDATES: usize = 192;
/// Score multiplier of a non-axial splitter (axial planes keep trees shallow).
const GENERAL_FACTOR: f64 = 1.0;
/// Weight of the larger child's fragment count in a split's score.
const BALANCE_WEIGHT: f64 = 0.5;
/// Score charge for every fragment a split clips in two.
const SPLIT_CHARGE: f64 = 1.0;
/// Margin added around the brush bounds for the root cell.
const ROOT_MARGIN: f64 = 16.0;

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn lex_cmp(a: &V3, b: &V3) -> std::cmp::Ordering {
    a[0].total_cmp(&b[0])
        .then(a[1].total_cmp(&b[1]))
        .then(a[2].total_cmp(&b[2]))
}

/// Decode a packed record to its stored unit-ish normal and Q12 distance.
fn decode(record: &[u8; 14]) -> (V3, f64) {
    let q = |at: usize| f64::from(i16::from_le_bytes([record[at], record[at + 1]])) / 4096.0;
    let distance = f64::from(i32::from_le_bytes([
        record[6], record[7], record[8], record[9],
    ])) / 4096.0;
    ([q(0), q(2), q(4)], distance)
}

/// A convex polyhedron as closed faces plus the deduplicated vertex set.
#[derive(Clone)]
struct Poly {
    faces: Vec<Vec<V3>>,
    verts: Vec<V3>,
}

impl Poly {
    fn new(faces: Vec<Vec<V3>>) -> Self {
        let mut verts: Vec<V3> = faces.iter().flatten().copied().collect();
        verts.sort_by(lex_cmp);
        verts.dedup();
        Self { faces, verts }
    }

    fn cuboid(min: V3, max: V3) -> Self {
        let c = |x: usize, y: usize, z: usize| {
            [
                if x == 0 { min[0] } else { max[0] },
                if y == 0 { min[1] } else { max[1] },
                if z == 0 { min[2] } else { max[2] },
            ]
        };
        Self::new(vec![
            vec![c(0, 0, 0), c(0, 1, 0), c(0, 1, 1), c(0, 0, 1)],
            vec![c(1, 0, 0), c(1, 0, 1), c(1, 1, 1), c(1, 1, 0)],
            vec![c(0, 0, 0), c(0, 0, 1), c(1, 0, 1), c(1, 0, 0)],
            vec![c(0, 1, 0), c(1, 1, 0), c(1, 1, 1), c(0, 1, 1)],
            vec![c(0, 0, 0), c(1, 0, 0), c(1, 1, 0), c(0, 1, 0)],
            vec![c(0, 0, 1), c(0, 1, 1), c(1, 1, 1), c(1, 0, 1)],
        ])
    }

    /// Smallest and largest of `n . p - d` over the vertices.
    fn range(&self, n: V3, d: f64) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for &p in &self.verts {
            let value = dot(n, p) - d;
            lo = lo.min(value);
            hi = hi.max(value);
        }
        (lo, hi)
    }

    fn extent(&self, axis: usize) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for p in &self.verts {
            lo = lo.min(p[axis]);
            hi = hi.max(p[axis]);
        }
        (lo, hi)
    }

    fn centroid(&self) -> V3 {
        let mut sum = [0.0; 3];
        for p in &self.verts {
            for axis in 0..3 {
                sum[axis] += p[axis];
            }
        }
        let count = self.verts.len().max(1) as f64;
        sum.map(|value| value / count)
    }

    /// Whether the polyhedron has real volume: the inradius estimate
    /// `3 V / S` clears [`THIN`].
    fn has_volume(&self) -> bool {
        self.inradius() >= THIN_FRAGMENT
    }

    /// Whether the polyhedron is too thin to be worth splitting.
    fn is_sliver(&self) -> bool {
        self.inradius() < THIN_CELL
    }

    /// Inradius estimate `3 V / S` (zero for an open or flat shape).
    fn inradius(&self) -> f64 {
        if self.verts.len() < 4 || self.faces.len() < 4 {
            return 0.0;
        }
        let reference = self.centroid();
        let mut volume = 0.0;
        let mut area = 0.0;
        for face in &self.faces {
            let mut vector = [0.0; 3];
            for index in 1..face.len().saturating_sub(1) {
                let triangle = cross(sub(face[index], face[0]), sub(face[index + 1], face[0]));
                for axis in 0..3 {
                    vector[axis] += triangle[axis] * 0.5;
                }
            }
            let magnitude = dot(vector, vector).sqrt();
            area += magnitude;
            volume += dot(vector, sub(face[0], reference)).abs() / 3.0;
        }
        if area > 0.0 {
            3.0 * volume / area
        } else {
            0.0
        }
    }

    /// Cut by the plane `n . p = d`. Front is `n . p - d >= 0`. A side with
    /// no usable closed shape is `None`.
    fn split(&self, n: V3, d: f64) -> (Option<Self>, Option<Self>) {
        let (lo, hi) = self.range(n, d);
        if hi <= EPS {
            return (None, Some(self.clone()));
        }
        if lo >= -EPS {
            return (Some(self.clone()), None);
        }
        let side = |value: f64| -> i8 {
            if value > EPS {
                1
            } else if value < -EPS {
                -1
            } else {
                0
            }
        };
        let mut front_faces = Vec::new();
        let mut back_faces = Vec::new();
        let mut cap: Vec<V3> = Vec::new();
        for face in &self.faces {
            let distances: Vec<f64> = face.iter().map(|&p| dot(n, p) - d).collect();
            let mut front = Vec::new();
            let mut back = Vec::new();
            let mut has_front = false;
            let mut has_back = false;
            for index in 0..face.len() {
                let next = (index + 1) % face.len();
                let (here, there) = (side(distances[index]), side(distances[next]));
                if here >= 0 {
                    front.push(face[index]);
                }
                if here <= 0 {
                    back.push(face[index]);
                }
                has_front |= here > 0;
                has_back |= here < 0;
                if here == 0 {
                    cap.push(face[index]);
                }
                if here * there < 0 {
                    // Canonical endpoint order so both faces sharing an edge
                    // compute bit-identical crossing points.
                    let (a, da, b, db) =
                        if lex_cmp(&face[index], &face[next]) == std::cmp::Ordering::Less {
                            (face[index], distances[index], face[next], distances[next])
                        } else {
                            (face[next], distances[next], face[index], distances[index])
                        };
                    let t = da / (da - db);
                    let crossing = [
                        a[0] + t * (b[0] - a[0]),
                        a[1] + t * (b[1] - a[1]),
                        a[2] + t * (b[2] - a[2]),
                    ];
                    front.push(crossing);
                    back.push(crossing);
                    cap.push(crossing);
                }
            }
            if has_front && front.len() >= 3 {
                front_faces.push(front);
            }
            if has_back && back.len() >= 3 {
                back_faces.push(back);
            }
        }
        cap.sort_by(lex_cmp);
        cap.dedup();
        let cap_face = order_cap(&cap, n);
        let finish = |mut faces: Vec<Vec<V3>>| -> Option<Self> {
            let cap_face = cap_face.clone()?;
            faces.push(cap_face);
            (faces.len() >= 4).then(|| Self::new(faces))
        };
        (finish(front_faces), finish(back_faces))
    }
}

/// Order cap points around their centroid as a polygon in the plane `n`.
fn order_cap(points: &[V3], n: V3) -> Option<Vec<V3>> {
    if points.len() < 3 {
        return None;
    }
    let mut centroid = [0.0; 3];
    for p in points {
        for axis in 0..3 {
            centroid[axis] += p[axis];
        }
    }
    centroid = centroid.map(|value| value / points.len() as f64);
    let seed = if n[0].abs() <= n[1].abs() && n[0].abs() <= n[2].abs() {
        [1.0, 0.0, 0.0]
    } else if n[1].abs() <= n[2].abs() {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let u = cross(n, seed);
    let length = dot(u, u).sqrt();
    if length <= 0.0 {
        return None;
    }
    let u = u.map(|value| value / length);
    let w = cross(n, u);
    let mut keyed: Vec<(f64, V3)> = points
        .iter()
        .map(|&p| {
            let offset = sub(p, centroid);
            (dot(offset, w).atan2(dot(offset, u)), p)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(lex_cmp(&a.1, &b.1)));
    Some(keyed.into_iter().map(|(_, p)| p).collect())
}

/// Outward half-space: inside is `n . p <= d`.
#[derive(Clone, Copy)]
struct Space {
    n: V3,
    d: f64,
}

struct Brush {
    spaces: Vec<Space>,
    records: Vec<[u8; 14]>,
    contents: i16,
}

impl Brush {
    /// Whether `point` is inside every plane, within the tolerance.
    fn contains(&self, point: V3) -> bool {
        self.spaces.iter().all(|s| dot(s.n, point) - s.d <= EPS)
    }

    fn covers(&self, cell: &Poly) -> bool {
        cell.verts.iter().all(|&v| self.contains(v))
    }
}

#[derive(Clone)]
struct Frag {
    brush: usize,
    poly: Rc<Poly>,
    /// Remaining work a query pays to test this fragment: its facet count.
    weight: f64,
}

impl Frag {
    fn new(brush: usize, poly: Poly) -> Self {
        let weight = poly.faces.len() as f64;
        Self {
            brush,
            poly: Rc::new(poly),
            weight,
        }
    }
}

struct Splitter {
    record: [u8; 14],
    n: V3,
    d: f64,
    axial: bool,
}

struct Context<'a> {
    brushes: Vec<Brush>,
    planes: &'a mut Vec<[u8; 14]>,
    nodes: &'a mut Vec<[i16; 3]>,
}

/// Build a solid-leaf BSP for one hull and return its head clipnode.
///
/// `brushes` is in precedence order, highest first, as the chain compiler
/// receives it. Planes are interned lazily: only splitters the tree uses reach
/// `planes`.
pub(crate) fn build_hull_bsp(
    prepared: &[PreparedHullBrush],
    planes: &mut Vec<[u8; 14]>,
    nodes: &mut Vec<[i16; 3]>,
) -> Result<i16, CollisionHullCompileError> {
    let mut brushes = Vec::with_capacity(prepared.len());
    for brush in prepared {
        let spaces = brush
            .records
            .iter()
            .map(|&(record, flipped)| {
                let (n, d) = decode(&record);
                if flipped {
                    Space {
                        n: n.map(|v| -v),
                        d: -d,
                    }
                } else {
                    Space { n, d }
                }
            })
            .collect();
        brushes.push(Brush {
            spaces,
            records: brush.records.iter().map(|&(record, _)| record).collect(),
            contents: brush.contents,
        });
    }

    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for brush in prepared {
        for axis in 0..3 {
            min[axis] = min[axis].min(brush.mins[axis]);
            max[axis] = max[axis].max(brush.maxs[axis]);
        }
    }
    let mut frags = Vec::new();
    if min[0].is_finite() {
        let root_box = Poly::cuboid(min.map(|v| v - ROOT_MARGIN), max.map(|v| v + ROOT_MARGIN));
        for (index, brush) in brushes.iter().enumerate() {
            let mut poly = Some(root_box.clone());
            for space in &brush.spaces {
                poly = poly.and_then(|p| p.split(space.n, space.d).1);
                if poly.is_none() {
                    break;
                }
            }
            if let Some(poly) = poly.filter(Poly::has_volume) {
                frags.push(Frag::new(index, poly));
            }
        }
        let cell = Poly::cuboid(min.map(|v| v - ROOT_MARGIN), max.map(|v| v + ROOT_MARGIN));
        let mut context = Context {
            brushes,
            planes,
            nodes,
        };
        let head = context.build(cell, frags, 0)?;
        if head >= 0 {
            return Ok(head);
        }
        return context.dummy_head(head);
    }
    let mut context = Context {
        brushes,
        planes,
        nodes,
    };
    context.dummy_head(CONTENTS_EMPTY)
}

impl Context<'_> {
    /// A head must be a real clipnode: wrap a lone leaf in a node whose two
    /// sides agree.
    fn dummy_head(&mut self, contents: i16) -> Result<i16, CollisionHullCompileError> {
        let (record, _) = pack_normalized_plane([1.0, 0.0, 0.0], 0.0)
            .ok_or(CollisionHullCompileError::InvalidPlane(None))?;
        let plane = intern_plane(self.planes, record)?;
        limit("clipnodes", self.nodes.len() + 1, i16::MAX as usize + 1)?;
        self.nodes.push([plane, contents, contents]);
        Ok((self.nodes.len() - 1) as i16)
    }

    /// Contents of a cell too thin to split: whatever the highest-precedence
    /// brush at its centre says.
    fn contents_at(&self, frags: &[Frag], point: V3) -> i16 {
        frags
            .iter()
            .find(|frag| self.brushes[frag.brush].contains(point))
            .map_or(CONTENTS_EMPTY, |frag| self.brushes[frag.brush].contents)
    }

    fn build(
        &mut self,
        cell: Poly,
        mut frags: Vec<Frag>,
        depth: usize,
    ) -> Result<i16, CollisionHullCompileError> {
        // Everything below the first brush that covers the whole cell is
        // shadowed by it.
        if let Some(first) = frags
            .iter()
            .position(|frag| self.brushes[frag.brush].covers(&cell))
        {
            frags.truncate(first + 1);
            if first == 0 {
                return Ok(self.brushes[frags[0].brush].contents);
            }
        }
        if frags.is_empty() {
            return Ok(CONTENTS_EMPTY);
        }
        if depth >= MAX_DEPTH {
            return Err(CollisionHullCompileError::LimitExceeded {
                kind: "hull depth",
                count: depth,
                max: MAX_DEPTH,
            });
        }
        let Some(splitter) = self.choose_splitter(&cell, &frags) else {
            // No plane makes progress: the cell is uniform to within the
            // tolerance. Take its centre's contents.
            return Ok(self.contents_at(&frags, cell.centroid()));
        };

        let (front_cell, back_cell) = cell.split(splitter.n, splitter.d);
        let mut front_frags = Vec::new();
        let mut back_frags = Vec::new();
        for frag in &frags {
            let (lo, hi) = frag.poly.range(splitter.n, splitter.d);
            if hi <= EPS {
                back_frags.push(frag.clone());
            } else if lo >= -EPS {
                front_frags.push(frag.clone());
            } else {
                let (front, back) = frag.poly.split(splitter.n, splitter.d);
                for (part, list) in [(front, &mut front_frags), (back, &mut back_frags)] {
                    if let Some(poly) = part.filter(Poly::has_volume) {
                        list.push(Frag::new(frag.brush, poly));
                    }
                }
            }
        }

        let plane = intern_plane(self.planes, splitter.record)?;
        limit("clipnodes", self.nodes.len() + 1, i16::MAX as usize + 1)?;
        let slot = self.nodes.len();
        self.nodes.push([plane, CONTENTS_EMPTY, CONTENTS_EMPTY]);
        let mut children = [CONTENTS_EMPTY; 2];
        for (child, (part, list)) in children
            .iter_mut()
            .zip([(front_cell, front_frags), (back_cell, back_frags)])
        {
            *child = match part {
                Some(part) if !part.is_sliver() => self.build(part, list, depth + 1)?,
                // A sliver cell takes the contents at its centre from every
                // brush that reached the parent, not just the fragments that
                // survived clipping.
                Some(part) => self.contents_at(&frags, part.centroid()),
                None => CONTENTS_EMPTY,
            };
        }
        if children[0] < 0 && children[0] == children[1] && self.nodes.len() == slot + 1 {
            self.nodes.pop();
            return Ok(children[0]);
        }
        self.nodes[slot] = [plane, children[0], children[1]];
        Ok(slot as i16)
    }

    fn choose_splitter(&self, cell: &Poly, frags: &[Frag]) -> Option<Splitter> {
        let params = params();
        let weight = |frag: &Frag| 1.0 + params.facet_weight * (frag.weight - 1.0);
        let mut best: Option<(f64, Splitter)> = None;
        let consider = |splitter: Splitter, best: &mut Option<(f64, Splitter)>| {
            let (cell_lo, cell_hi) = cell.range(splitter.n, splitter.d);
            if cell_lo >= -EPS || cell_hi <= EPS {
                return;
            }
            let (mut front, mut back, mut both, mut touches) = (0.0f64, 0.0f64, 0.0f64, false);
            for frag in frags {
                let (lo, hi) = frag.poly.range(splitter.n, splitter.d);
                let w = weight(frag);
                if hi <= EPS {
                    back += w;
                } else if lo >= -EPS {
                    front += w;
                } else {
                    both += w;
                }
                touches |= lo <= EPS && hi >= -EPS;
            }
            if !touches {
                return;
            }
            // Expected remaining work of a query that lands uniformly in the
            // cell: each side's fragment weight times the share of the cell's
            // volume it holds (exact for a box cut by an axial plane, a
            // linear estimate otherwise), plus a charge for the weight of
            // every fragment the cut would clip in two. Empty sides cost
            // nothing, so large empty regions are cut away first; a balance
            // term keeps the worst case shallow where volume says nothing.
            let front_share = cell_hi / (cell_hi - cell_lo);
            let (front_total, back_total) = (front + both, back + both);
            let volume_cost = front_share * front_total + (1.0 - front_share) * back_total;
            let balance = front_total.max(back_total);
            let mut cost = volume_cost + params.balance * balance + params.split_charge * both;
            if !splitter.axial {
                cost *= params.general_factor;
            }
            if best.as_ref().is_none_or(|(current, _)| cost < *current) {
                *best = Some((cost, splitter));
            }
        };

        // Axial candidates: the fragments' own extents inside the cell.
        for axis in 0..3 {
            let (cell_lo, cell_hi) = cell.extent(axis);
            let mut values: Vec<f64> = Vec::new();
            for frag in frags {
                if params.axial_mode == 1 {
                    for record in &self.brushes[frag.brush].records {
                        if i32::from_le_bytes([record[10], record[11], record[12], record[13]])
                            == axis as i32
                        {
                            let value = decode(record).1;
                            if value > cell_lo + EPS && value < cell_hi - EPS {
                                values.push(value);
                            }
                        }
                    }
                    continue;
                }
                let (lo, hi) = frag.poly.extent(axis);
                for value in [lo, hi] {
                    if value > cell_lo + EPS && value < cell_hi - EPS {
                        values.push(value);
                    }
                }
            }
            values.sort_by(f64::total_cmp);
            values.dedup_by(|a, b| (*a - *b).abs() <= EPS);
            let stride = values.len().div_ceil(params.axial_candidates).max(1);
            let mut normal = [0.0; 3];
            normal[axis] = 1.0;
            for &value in values.iter().step_by(stride) {
                let Some((record, _)) = pack_normalized_plane(normal, value) else {
                    continue;
                };
                let (n, d) = decode(&record);
                consider(
                    Splitter {
                        record,
                        n,
                        d,
                        axial: true,
                    },
                    &mut best,
                );
            }
        }

        // General candidates: brush planes that carry a real facet of a live
        // fragment (three or more of its vertices on the plane), so a cut
        // follows the surface instead of slicing through solids.
        {
            let mut records: Vec<[u8; 14]> = Vec::new();
            for frag in frags {
                for &record in &self.brushes[frag.brush].records {
                    if record[10..14] != 3i32.to_le_bytes() {
                        continue;
                    }
                    if params.facet_only {
                        let (n, d) = decode(&record);
                        let on_plane = frag
                            .poly
                            .verts
                            .iter()
                            .filter(|&&v| (dot(n, v) - d).abs() <= EPS)
                            .count();
                        if on_plane < 3 {
                            continue;
                        }
                    }
                    records.push(record);
                }
            }
            records.sort_unstable();
            records.dedup();
            let stride = records.len().div_ceil(GENERAL_CANDIDATES).max(1);
            for record in records.into_iter().step_by(stride) {
                let (n, d) = decode(&record);
                consider(
                    Splitter {
                        record,
                        n,
                        d,
                        axial: false,
                    },
                    &mut best,
                );
            }
        }
        best.map(|(_, splitter)| splitter)
    }
}

#[cfg(test)]
#[allow(clippy::print_stdout)]
mod tests {
    use super::*;
    use crate::brush::Brush;
    use crate::brush_collision_hulls::{
        compile_collision_hulls_with, CollisionHullBounds, CollisionHullStrategy,
        CompiledCollisionHulls,
    };
    use psx_bsp::collision::{CollisionHull, Trace, TraceScratch, Q12_ONE};
    use psx_bsp::{ClipNode, Plane as BspPlane, RecordSlice, Vec3I32};

    pub(crate) const PLAYER: CollisionHullBounds = CollisionHullBounds {
        mins: [-16, 0, -16],
        maxs: [16, 56, 16],
    };

    pub(crate) fn hull(compiled: &CompiledCollisionHulls, index: usize) -> CollisionHull<'_> {
        CollisionHull::new(
            RecordSlice::<BspPlane>::new(&compiled.planes).expect("planes"),
            RecordSlice::<ClipNode>::new(&compiled.clipnodes).expect("nodes"),
            compiled.head_nodes[index],
        )
        .expect("aligned collision records")
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound.max(1)
        }

        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
        }
    }

    fn q12(units: f64) -> i32 {
        (units * f64::from(Q12_ONE)).round() as i32
    }

    fn point(p: V3) -> Vec3I32 {
        Vec3I32 {
            x: q12(p[0]),
            y: q12(p[1]),
            z: q12(p[2]),
        }
    }

    fn trace(hull: &CollisionHull<'_>, start: Vec3I32, end: Vec3I32) -> Trace {
        let mut output = Trace::default();
        assert!(hull.trace_into(&start, &end, &mut TraceScratch::new(), &mut output));
        output
    }

    /// Tally of one old-vs-new comparison.
    #[derive(Default, Debug)]
    pub(crate) struct Report {
        pub traces: usize,
        pub points: usize,
        pub identical: usize,
        pub fraction_within_epsilon: usize,
        pub fraction_within_unit: usize,
        pub flag_mismatch: Vec<String>,
        pub hit_mismatch: Vec<String>,
        pub fraction_mismatch: Vec<String>,
        pub normal_mismatch: Vec<String>,
        pub contents_mismatch: Vec<String>,
        pub normal_at_edge: usize,
    }

    impl Report {
        pub fn failures(&self) -> usize {
            self.flag_mismatch.len()
                + self.hit_mismatch.len()
                + self.fraction_mismatch.len()
                + self.normal_mismatch.len()
                + self.contents_mismatch.len()
        }

        pub fn summary(&self) -> String {
            format!(
                "traces {} points {} identical {} within-eps {} within-unit {} edge-normal {} | flag {} hit {} fraction {} normal {} contents {}",
                self.traces,
                self.points,
                self.identical,
                self.fraction_within_epsilon,
                self.fraction_within_unit,
                self.normal_at_edge,
                self.flag_mismatch.len(),
                self.hit_mismatch.len(),
                self.fraction_mismatch.len(),
                self.normal_mismatch.len(),
                self.contents_mismatch.len()
            )
        }
    }

    fn describe(start: Vec3I32, end: Vec3I32) -> String {
        let u = |v: i32| f64::from(v) / 4096.0;
        format!(
            "({:.4},{:.4},{:.4})->({:.4},{:.4},{:.4})",
            u(start.x),
            u(start.y),
            u(start.z),
            u(end.x),
            u(end.y),
            u(end.z)
        )
    }

    /// Sample region: the brushes' bounds with a margin.
    fn bounds_of(brushes: &[Brush]) -> (V3, V3, Vec<V3>) {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut vertices = Vec::new();
        for brush in brushes {
            let solved = brush.solve();
            for polygon in solved.polygons.iter().flatten() {
                vertices.extend(polygon.verts.iter().copied());
            }
            for axis in 0..3 {
                min[axis] = min[axis].min(solved.min[axis]);
                max[axis] = max[axis].max(solved.max[axis]);
            }
        }
        (min, max, vertices)
    }

    /// Compare the chain and BSP hulls over random point and segment queries.
    pub(crate) fn differential(
        brushes: &[Brush],
        bounds: &[CollisionHullBounds],
        samples_per_hull: usize,
        seed: u64,
    ) -> (Report, CompiledCollisionHulls, CompiledCollisionHulls) {
        let old =
            compile_collision_hulls_with(brushes, bounds, CollisionHullStrategy::SpatialChains)
                .expect("chain cook");
        let new = compile_collision_hulls_with(brushes, bounds, CollisionHullStrategy::HullBsp)
            .expect("bsp cook");
        let (min, max, vertices) = bounds_of(brushes);
        let mut rng = Rng(seed | 1);
        let mut report = Report::default();
        for hull_index in 0..bounds.len() {
            let old_hull = hull(&old, hull_index);
            let new_hull = hull(&new, hull_index);
            for sample in 0..samples_per_hull {
                let pick = |rng: &mut Rng| -> V3 {
                    [
                        rng.range(min[0] - 48.0, max[0] + 48.0),
                        rng.range(min[1] - 48.0, max[1] + 48.0),
                        rng.range(min[2] - 48.0, max[2] + 48.0),
                    ]
                };
                let near_vertex = |rng: &mut Rng, spread: f64| -> V3 {
                    let v = vertices[rng.below(vertices.len() as u64) as usize];
                    [
                        v[0] + rng.range(-spread, spread),
                        v[1] + rng.range(-spread, spread),
                        v[2] + rng.range(-spread, spread),
                    ]
                };
                let delta = |rng: &mut Rng, length: f64| -> V3 {
                    [
                        rng.range(-length, length),
                        rng.range(-length, length),
                        rng.range(-length, length),
                    ]
                };
                let (start, end): (V3, V3) = match sample % 6 {
                    0 => (pick(&mut rng), pick(&mut rng)),
                    1 => {
                        let s = pick(&mut rng);
                        let d = delta(&mut rng, 128.0);
                        (s, [s[0] + d[0], s[1] + d[1], s[2] + d[2]])
                    }
                    2 => {
                        let s = near_vertex(&mut rng, 48.0);
                        let d = delta(&mut rng, 96.0);
                        (s, [s[0] + d[0], s[1] + d[1], s[2] + d[2]])
                    }
                    3 => {
                        let s = near_vertex(&mut rng, 40.0);
                        let axis = rng.below(3) as usize;
                        let mut e = s;
                        e[axis] += rng.range(-220.0, 220.0);
                        (s, e)
                    }
                    4 => {
                        let s = near_vertex(&mut rng, 12.0);
                        let d = delta(&mut rng, 40.0);
                        (s, [s[0] + d[0], s[1] + d[1], s[2] + d[2]])
                    }
                    _ => {
                        // Whole-unit coordinates: exercises plane-tie rules.
                        let s = near_vertex(&mut rng, 40.0).map(f64::round);
                        let d = delta(&mut rng, 80.0).map(f64::round);
                        (s, [s[0] + d[0], s[1] + d[1], s[2] + d[2]])
                    }
                };
                let (start, end) = (point(start), point(end));
                report.points += 1;
                if old_hull.point_contents(start) != new_hull.point_contents(start) {
                    report.contents_mismatch.push(format!(
                        "hull {hull_index} contents at {} old {:?} new {:?}",
                        describe(start, start),
                        old_hull.point_contents(start),
                        new_hull.point_contents(start)
                    ));
                }
                let a = trace(&old_hull, start, end);
                let b = trace(&new_hull, start, end);
                report.traces += 1;
                if a == b {
                    report.identical += 1;
                    continue;
                }
                let tag = format!("hull {hull_index} {}", describe(start, end));
                if (a.all_solid, a.start_solid, a.in_open, a.in_water)
                    != (b.all_solid, b.start_solid, b.in_open, b.in_water)
                {
                    report.flag_mismatch.push(format!(
                        "{tag} old {:?} new {:?}",
                        (a.all_solid, a.start_solid, a.in_open, a.in_water),
                        (b.all_solid, b.start_solid, b.in_open, b.in_water)
                    ));
                    continue;
                }
                if (a.fraction == Q12_ONE) != (b.fraction == Q12_ONE) {
                    report.hit_mismatch.push(format!(
                        "{tag} old fraction {} new fraction {}",
                        a.fraction, b.fraction
                    ));
                    continue;
                }
                let length = {
                    let d = [
                        f64::from(end.x - start.x),
                        f64::from(end.y - start.y),
                        f64::from(end.z - start.z),
                    ];
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() / f64::from(Q12_ONE)
                };
                let gap = f64::from((a.fraction - b.fraction).abs()) / f64::from(Q12_ONE) * length;
                if a.normal != b.normal && a.fraction != Q12_ONE {
                    // Two contact planes meeting at an edge or corner are
                    // both right, and each backs the contact off by its own
                    // margin. Accept when the segment crosses both planes at
                    // the same point.
                    let crossing = |trace: &Trace| -> Option<f64> {
                        let n = [
                            f64::from(trace.normal.x) / 4096.0,
                            f64::from(trace.normal.y) / 4096.0,
                            f64::from(trace.normal.z) / 4096.0,
                        ];
                        let s = [
                            f64::from(start.x) / 4096.0,
                            f64::from(start.y) / 4096.0,
                            f64::from(start.z) / 4096.0,
                        ];
                        let e = [
                            f64::from(end.x) / 4096.0,
                            f64::from(end.y) / 4096.0,
                            f64::from(end.z) / 4096.0,
                        ];
                        let ds = dot(n, s) - f64::from(trace.plane_distance) / 4096.0;
                        let de = dot(n, e) - f64::from(trace.plane_distance) / 4096.0;
                        (ds != de).then(|| ds / (ds - de))
                    };
                    if let (Some(ta), Some(tb)) = (crossing(&a), crossing(&b)) {
                        if (ta - tb).abs() * length <= 1.0 / 16.0 {
                            report.normal_at_edge += 1;
                            continue;
                        }
                    }
                }
                if gap <= 1.0 / 32.0 {
                    report.fraction_within_epsilon += 1;
                } else if gap <= 1.0 {
                    report.fraction_within_unit += 1;
                } else {
                    report.fraction_mismatch.push(format!(
                        "{tag} old fraction {} new fraction {} gap {gap:.3} units, normals old {:?} new {:?}",
                        a.fraction, b.fraction, a.normal, b.normal
                    ));
                    continue;
                }
                if a.normal != b.normal && a.fraction != Q12_ONE {
                    report.normal_mismatch.push(format!(
                        "{tag} fraction {} old normal {:?} new normal {:?}",
                        a.fraction, a.normal, b.normal
                    ));
                }
            }
        }
        (report, old, new)
    }

    fn assert_clean(report: &Report) {
        println!("{}", report.summary());
        for list in [
            &report.flag_mismatch,
            &report.hit_mismatch,
            &report.fraction_mismatch,
            &report.normal_mismatch,
            &report.contents_mismatch,
        ] {
            for line in list.iter().take(5) {
                println!("  {line}");
            }
        }
        assert_eq!(report.failures(), 0, "{}", report.summary());
    }

    #[test]
    fn cuboid_room_matches_the_chain_compiler() {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        let (report, ..) = differential(&brushes, &[CollisionHullBounds::POINT, PLAYER], 3000, 7);
        assert_clean(&report);
    }

    #[test]
    fn brush_grid_matches_the_chain_compiler() {
        let mut brushes = Vec::new();
        for z in 0..9 {
            for x in 0..9 {
                let min = [x * 128, 0, z * 128];
                brushes.push(Brush::cuboid(min, [min[0] + 48, 64, min[2] + 48]));
            }
        }
        let (report, old, new) = differential(&brushes, &[PLAYER], 6000, 11);
        println!(
            "grid clipnodes old {} new {}",
            old.clipnodes.len() / 6,
            new.clipnodes.len() / 6
        );
        assert_clean(&report);
    }

    #[test]
    fn generated_terrain_matches_the_chain_compiler_and_is_shallower() {
        let terrain = crate::terrain::Terrain::generate(
            [8, 8],
            [256, 256],
            [-1024, 0, -1024],
            384,
            23,
            crate::terrain::TerrainShape::Hills,
            0.5,
        )
        .expect("terrain");
        let brushes = terrain.brushes(None).expect("wedges");
        assert_eq!(brushes.len(), 128);
        let (report, old, new) = differential(&brushes, &[PLAYER], 30000, 5);
        assert_clean(&report);
        let before = mean_point_depth(&old, 0, &brushes, 4000, 1).0;
        let after = mean_point_depth(&new, 0, &brushes, 4000, 1).0;
        println!(
            "terrain clipnodes {} -> {}, mean depth {before:.1} -> {after:.1}",
            old.clipnodes.len() / 6,
            new.clipnodes.len() / 6
        );
        assert!(after * 2.0 < before, "BSP must at least halve the depth");
        assert!(
            new.clipnodes.len() <= old.clipnodes.len() * 2,
            "BSP must stay within twice the chain table"
        );
    }

    #[test]
    fn liquids_keep_their_precedence_under_a_solid() {
        use crate::brush::BrushContents;
        let mut water = Brush::cuboid([0, 0, 0], [512, 256, 512]);
        water.contents = BrushContents::Water;
        let mut lava = Brush::cuboid([64, 0, 64], [448, 256, 448]);
        lava.contents = BrushContents::Lava;
        let solid = Brush::cuboid([192, 0, 192], [320, 256, 320]);
        let (report, ..) = differential(
            &[water, lava, solid],
            &[CollisionHullBounds::POINT, PLAYER],
            4000,
            9,
        );
        assert_clean(&report);
    }

    #[test]
    fn an_empty_brush_set_still_has_a_valid_head() {
        let compiled = compile_collision_hulls_with(&[], &[PLAYER], CollisionHullStrategy::HullBsp)
            .expect("cook");
        assert!(compiled.head_nodes[0] >= 0);
        let hull = hull(&compiled, 0);
        assert_eq!(
            hull.point_contents(point([0.0, 0.0, 0.0])),
            Some(psx_bsp::collision::CONTENTS_EMPTY)
        );
    }

    /// Mean clipnode count a point query visits, from a Q12-free f64 walk.
    pub(crate) fn mean_point_depth(
        compiled: &CompiledCollisionHulls,
        hull_index: usize,
        brushes: &[Brush],
        samples: usize,
        seed: u64,
    ) -> (f64, usize) {
        let (min, max, _) = bounds_of(brushes);
        let mut rng = Rng(seed | 1);
        let planes: Vec<(V3, f64)> = compiled
            .planes
            .chunks_exact(14)
            .map(|c| decode(&c.try_into().unwrap()))
            .collect();
        let nodes: Vec<[i16; 3]> = compiled
            .clipnodes
            .chunks_exact(6)
            .map(|c| {
                [
                    i16::from_le_bytes([c[0], c[1]]),
                    i16::from_le_bytes([c[2], c[3]]),
                    i16::from_le_bytes([c[4], c[5]]),
                ]
            })
            .collect();
        let (mut total, mut worst) = (0usize, 0usize);
        for _ in 0..samples {
            let p = [
                rng.range(min[0], max[0]),
                rng.range(min[1], max[1]),
                rng.range(min[2], max[2]),
            ];
            let mut node = compiled.head_nodes[hull_index];
            let mut depth = 0;
            while node >= 0 {
                let [plane, front, back] = nodes[node as usize];
                let (n, d) = planes[plane as usize];
                node = if dot(n, p) - d >= 0.0 { front } else { back };
                depth += 1;
            }
            total += depth;
            worst = worst.max(depth);
        }
        (total as f64 / samples as f64, worst)
    }

    /// Load a project's static brushes and cooked body hull bounds the way
    /// the playtest cook does.
    pub(crate) fn project_inputs(path: &str) -> Option<(Vec<Brush>, [CollisionHullBounds; 3])> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let file = root.join(path);
        if !file.exists() {
            println!("skipping {path}: not present");
            return None;
        }
        let mut project = crate::ProjectDocument::load_from_path(&file).expect("project");
        crate::units::scale_project_to_engine_units(&mut project);
        let hulls = crate::brush_world::collision_hull_bounds(
            crate::brush_world::authored_body_hulls(&project),
        );
        let brushes: Vec<Brush> = project
            .active_scene()
            .brushes
            .iter()
            .filter(|brush| brush.mover.is_none())
            .cloned()
            .collect();
        Some((brushes, hulls))
    }

    fn project_report(path: &str, samples: usize) -> usize {
        let Some((brushes, hulls)) = project_inputs(path) else {
            return 0;
        };
        println!("{path}: {} brushes, hulls {:?}", brushes.len(), &hulls[1..]);
        let started = std::time::Instant::now();
        let (report, old, new) = differential(&brushes, &hulls[1..], samples, 0x5eed);
        println!(
            "{path}: clipnodes old {} new {}, planes old {} new {}, bytes old {} new {} ({:.1}s)",
            old.clipnodes.len() / 6,
            new.clipnodes.len() / 6,
            old.planes.len() / 14,
            new.planes.len() / 14,
            old.clipnodes.len() + old.planes.len(),
            new.clipnodes.len() + new.planes.len(),
            started.elapsed().as_secs_f64()
        );
        for hull_index in 0..2 {
            let a = mean_point_depth(&old, hull_index, &brushes, 20000, 3);
            let b = mean_point_depth(&new, hull_index, &brushes, 20000, 3);
            println!(
                "{path}: hull {} mean/max point depth old {:.1}/{} new {:.1}/{}",
                hull_index + 1,
                a.0,
                a.1,
                b.0,
                b.1
            );
        }
        for hull_index in 0..2 {
            for length in [32.0, 256.0] {
                let a = mean_trace_work(&old, hull_index, &brushes, 5000, length, 5);
                let b = mean_trace_work(&new, hull_index, &brushes, 5000, length, 5);
                println!(
                    "{path}: hull {} len {length}: visits/deferred/max-stack old {:.1}/{:.1}/{} new {:.1}/{:.1}/{}",
                    hull_index + 1, a.0, a.1, a.2, b.0, b.1, b.2
                );
            }
        }
        println!("{path}: {}", report.summary());
        for list in [
            &report.flag_mismatch,
            &report.hit_mismatch,
            &report.fraction_mismatch,
            &report.normal_mismatch,
            &report.contents_mismatch,
        ] {
            for line in list.iter().take(6) {
                println!("  {line}");
            }
        }
        report.failures()
    }

    fn sample_count(default: usize) -> usize {
        std::env::var("HULL_DIFF_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    #[test]
    fn graybox_valley_matches_the_chain_compiler() {
        let failures = project_report(
            "editor/projects/graybox-valley/project.ron",
            sample_count(3000),
        );
        assert_eq!(failures, 0);
    }

    #[test]
    fn graybox_reach_matches_the_chain_compiler() {
        let failures = project_report(
            "editor/projects/graybox-reach/project.ron",
            sample_count(3000),
        );
        assert_eq!(failures, 0);
    }

    #[test]
    #[ignore = "heavy: needs the local graybox-terrain projects"]
    fn terrain_projects_match_the_chain_compiler() {
        let mut failures = 0;
        let only = std::env::var("HULL_ONLY").ok();
        for path in [
            "editor/projects/graybox-terrain/flat-control.ron",
            "editor/projects/graybox-terrain-light/project.ron",
            "editor/projects/graybox-terrain-balanced/project.ron",
            "editor/projects/graybox-terrain/project.ron",
            "editor/projects/cortex-ignition-0.5/project.ron",
        ]
        .into_iter()
        .filter(|path| only.as_deref().is_none_or(|only| path.contains(only)))
        {
            failures += project_report(path, sample_count(20000));
        }
        assert_eq!(failures, 0);
    }

    /// Nodes visited and far pieces deferred by an exact front-to-back walk
    /// of a segment (the work `trace_segment` does), averaged over random
    /// segments of the given length in world units. Reports (visits, deferred,
    /// max deferred stack).
    pub(crate) fn mean_trace_work(
        compiled: &CompiledCollisionHulls,
        hull_index: usize,
        brushes: &[Brush],
        samples: usize,
        length: f64,
        seed: u64,
    ) -> (f64, f64, usize) {
        let (min, max, _) = bounds_of(brushes);
        let mut rng = Rng(seed | 1);
        let planes: Vec<(V3, f64)> = compiled
            .planes
            .chunks_exact(14)
            .map(|c| decode(&c.try_into().unwrap()))
            .collect();
        let nodes: Vec<[i16; 3]> = compiled
            .clipnodes
            .chunks_exact(6)
            .map(|c| {
                [
                    i16::from_le_bytes([c[0], c[1]]),
                    i16::from_le_bytes([c[2], c[3]]),
                    i16::from_le_bytes([c[4], c[5]]),
                ]
            })
            .collect();
        let (mut visits, mut deferred, mut worst_stack) = (0usize, 0usize, 0usize);
        for _ in 0..samples {
            let a = [
                rng.range(min[0], max[0]),
                rng.range(min[1], max[1]),
                rng.range(min[2], max[2]),
            ];
            let mut dir = [
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
            ];
            let norm = dot(dir, dir).sqrt().max(1e-9);
            dir = dir.map(|v| v / norm * length);
            let b = [a[0] + dir[0], a[1] + dir[1], a[2] + dir[2]];
            // Pieces: (node, t0, t1). Depth-first with an explicit stack.
            let mut stack = vec![(compiled.head_nodes[hull_index], 0.0f64, 1.0f64)];
            while let Some((mut node, mut t0, mut t1)) = stack.pop() {
                while node >= 0 {
                    visits += 1;
                    let [plane, front, back] = nodes[node as usize];
                    let (n, d) = planes[plane as usize];
                    let da = dot(n, a) - d;
                    let db = dot(n, b) - d;
                    let at = |t: f64| da + (db - da) * t;
                    let (s0, s1) = (at(t0), at(t1));
                    if s0 >= 0.0 && s1 >= 0.0 {
                        node = front;
                    } else if s0 < 0.0 && s1 < 0.0 {
                        node = back;
                    } else {
                        let tc = da / (da - db);
                        let (near, far) = if s0 >= 0.0 {
                            (front, back)
                        } else {
                            (back, front)
                        };
                        deferred += 1;
                        stack.push((far, tc, t1));
                        worst_stack = worst_stack.max(stack.len());
                        node = near;
                        t1 = tc;
                    }
                    let _ = &mut t0;
                }
            }
        }
        (
            visits as f64 / samples as f64,
            deferred as f64 / samples as f64,
            worst_stack,
        )
    }
}
