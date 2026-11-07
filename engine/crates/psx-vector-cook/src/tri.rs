//! Polygon triangulation on an integer lattice: contour nesting, fill rules,
//! hole bridging and ear clipping.
//!
//! Written from the standard formulation of each step. All predicates use
//! exact `i64` arithmetic on `i32` coordinates, so the same input gives the
//! same triangles on every machine. Contours of one fill must not cross each
//! other or themselves (touching is tolerated); crossings are reported.

use crate::paint::FillRule;

/// A lattice point (Q4 pixels in the cooker).
pub type IPt = (i32, i32);

/// A triangle of lattice points.
pub type ITri = [IPt; 3];

/// Why a fill could not be triangulated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriError {
    /// Two edges cross; split the path so each shape is simple.
    Crossing,
    /// Ear clipping found no ear, which means touching or degenerate contours
    /// it cannot resolve.
    Stuck,
}

fn cross(o: IPt, a: IPt, b: IPt) -> i64 {
    (a.0 as i64 - o.0 as i64) * (b.1 as i64 - o.1 as i64)
        - (a.1 as i64 - o.1 as i64) * (b.0 as i64 - o.0 as i64)
}

/// Twice the signed area (positive when counter-clockwise in a y-up frame).
pub fn area2(poly: &[IPt]) -> i64 {
    let mut sum = 0i64;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        sum += a.0 as i64 * b.1 as i64 - b.0 as i64 * a.1 as i64;
    }
    sum
}

fn on_segment_interior(p: IPt, a: IPt, b: IPt) -> bool {
    cross(a, b, p) == 0
        && p != a
        && p != b
        && p.0 >= a.0.min(b.0)
        && p.0 <= a.0.max(b.0)
        && p.1 >= a.1.min(b.1)
        && p.1 <= a.1.max(b.1)
}

fn sign(v: i64) -> i32 {
    (v > 0) as i32 - (v < 0) as i32
}

/// Segments `ab` and `cd` cross at a point interior to both.
fn proper_cross(a: IPt, b: IPt, c: IPt, d: IPt) -> bool {
    let (d1, d2) = (sign(cross(a, b, c)), sign(cross(a, b, d)));
    let (d3, d4) = (sign(cross(c, d, a)), sign(cross(c, d, b)));
    d1 * d2 < 0 && d3 * d4 < 0
}

/// Where a point lies relative to a polygon.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Where {
    Inside,
    Outside,
    Boundary,
}

fn locate(poly: &[IPt], p: IPt) -> Where {
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if p == a || p == b || on_segment_interior(p, a, b) {
            return Where::Boundary;
        }
        // Crossing-number test against a ray towards +x.
        if (a.1 > p.1) != (b.1 > p.1) {
            let c = cross(a, b, p);
            if (b.1 > a.1) == (c > 0) {
                inside = !inside;
            }
        }
    }
    if inside {
        Where::Inside
    } else {
        Where::Outside
    }
}

/// Remove duplicate and collinear vertices until stable.
fn clean(mut poly: Vec<IPt>) -> Vec<IPt> {
    loop {
        let before = poly.len();
        poly.dedup();
        while poly.len() > 1 && poly.first() == poly.last() {
            poly.pop();
        }
        if poly.len() < 3 {
            return Vec::new();
        }
        let n = poly.len();
        let keep: Vec<bool> = (0..n)
            .map(|i| cross(poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]) != 0)
            .collect();
        let mut i = 0;
        poly.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
        if poly.len() == before {
            return if poly.len() < 3 { Vec::new() } else { poly };
        }
    }
}

fn contours_cross(polys: &[Vec<IPt>]) -> bool {
    for i in 0..polys.len() {
        for j in i..polys.len() {
            let (p, q) = (&polys[i], &polys[j]);
            for a in 0..p.len() {
                let (a0, a1) = (p[a], p[(a + 1) % p.len()]);
                let start = if i == j { a + 2 } else { 0 };
                for b in start..q.len() {
                    if i == j && (b + 1) % q.len() == a {
                        continue;
                    }
                    if proper_cross(a0, a1, q[b], q[(b + 1) % q.len()]) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// One piece of the nesting analysis.
struct Node {
    ring: Vec<IPt>,
    parent: Option<usize>,
    /// Winding (or crossing count) just inside this contour.
    inside: i32,
    /// Winding just outside.
    outside: i32,
}

fn filled(rule: FillRule, w: i32) -> bool {
    match rule {
        FillRule::NonZero => w != 0,
        FillRule::EvenOdd => w & 1 != 0,
    }
}

/// Triangulate the region the contours enclose under `rule`.
///
/// Contours may be listed in any order and orientation. Holes are the
/// contours whose inside is unfilled while their surroundings are filled.
pub fn triangulate(contours: &[Vec<IPt>], rule: FillRule) -> Result<Vec<ITri>, TriError> {
    let rings: Vec<Vec<IPt>> = contours
        .iter()
        .map(|c| clean(c.clone()))
        .filter(|c| !c.is_empty())
        .collect();
    if rings.is_empty() {
        return Ok(Vec::new());
    }
    if contours_cross(&rings) {
        return Err(TriError::Crossing);
    }
    let n = rings.len();
    // Parent: the smallest-area contour that contains this one.
    let mut parents: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let mut best: Option<(usize, i64)> = None;
        for j in 0..n {
            if i == j {
                continue;
            }
            let locs: Vec<Where> = rings[i].iter().map(|&p| locate(&rings[j], p)).collect();
            let contained =
                locs.iter().all(|&w| w != Where::Outside) && locs.contains(&Where::Inside);
            if contained {
                let a = area2(&rings[j]).abs();
                if best.is_none_or(|(_, ba)| a < ba) {
                    best = Some((j, a));
                }
            }
        }
        parents[i] = best.map(|(j, _)| j);
    }
    // Resolve windings parent-first. A contour's parent has strictly larger
    // area, so sorting by area gives a valid order.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(area2(&rings[i]).abs()));
    let mut nodes: Vec<Option<Node>> = (0..n).map(|_| None).collect();
    for &i in &order {
        let outside = parents[i].map_or(0, |p| nodes[p].as_ref().map_or(0, |nd| nd.inside));
        let step = match rule {
            FillRule::NonZero => area2(&rings[i]).signum() as i32,
            FillRule::EvenOdd => 1,
        };
        nodes[i] = Some(Node {
            ring: rings[i].clone(),
            parent: parents[i],
            inside: outside + step,
            outside,
        });
    }
    let nodes: Vec<Node> = nodes.into_iter().flatten().collect();
    let is_outer = |nd: &Node| filled(rule, nd.inside) && !filled(rule, nd.outside);
    let is_hole = |nd: &Node| !filled(rule, nd.inside) && filled(rule, nd.outside);

    let mut tris = Vec::new();
    for (i, nd) in nodes.iter().enumerate() {
        if !is_outer(nd) {
            continue;
        }
        let mut holes: Vec<Vec<IPt>> = Vec::new();
        for h in nodes.iter().filter(|h| is_hole(h)) {
            // Nearest enclosing outer boundary.
            let mut up = h.parent;
            while let Some(p) = up {
                if is_outer(&nodes[p]) {
                    break;
                }
                up = nodes[p].parent;
            }
            if up == Some(i) {
                holes.push(h.ring.clone());
            }
        }
        tris.extend(triangulate_face(&nd.ring, holes)?);
    }
    Ok(tris)
}

/// Orient `ring` counter-clockwise (positive area) when `ccw`, else clockwise.
fn oriented(ring: &[IPt], ccw: bool) -> Vec<IPt> {
    let mut r = ring.to_vec();
    if (area2(&r) > 0) != ccw {
        r.reverse();
    }
    r
}

fn triangulate_face(outer: &[IPt], holes: Vec<Vec<IPt>>) -> Result<Vec<ITri>, TriError> {
    let mut ring = oriented(outer, true);
    let mut pending: Vec<Vec<IPt>> = holes.iter().map(|h| oriented(h, false)).collect();
    // Bridge the hole with the rightmost vertex first so later bridges see
    // the already merged ring.
    pending.sort_by_key(|h| std::cmp::Reverse(h.iter().map(|p| p.0).max().unwrap_or(0)));
    while !pending.is_empty() {
        let hole = pending.remove(0);
        // Hole vertex with the greatest x (ties: lowest y, for determinism).
        let (m_idx, &m) = hole
            .iter()
            .enumerate()
            .max_by_key(|&(_, p)| (p.0, std::cmp::Reverse(p.1)))
            .ok_or(TriError::Stuck)?;
        let mut candidates: Vec<usize> = (0..ring.len()).collect();
        candidates.sort_by_key(|&i| {
            let (dx, dy) = (ring[i].0 as i64 - m.0 as i64, ring[i].1 as i64 - m.1 as i64);
            (dx * dx + dy * dy, i)
        });
        let visible = |p: IPt| {
            let blocked = |poly: &[IPt]| {
                (0..poly.len()).any(|k| {
                    let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                    proper_cross(m, p, a, b) || on_segment_interior(a, m, p)
                })
            };
            !blocked(&ring) && !blocked(&hole) && !pending.iter().any(|h| blocked(h))
        };
        let p_idx = candidates
            .into_iter()
            .find(|&i| ring[i] != m && visible(ring[i]))
            .ok_or(TriError::Stuck)?;
        // ring[..=p] + hole from m around to m + ring[p] + ring[p+1..]
        let mut merged = Vec::with_capacity(ring.len() + hole.len() + 2);
        merged.extend_from_slice(&ring[..=p_idx]);
        for k in 0..=hole.len() {
            merged.push(hole[(m_idx + k) % hole.len()]);
        }
        merged.push(ring[p_idx]);
        merged.extend_from_slice(&ring[p_idx + 1..]);
        ring = merged;
    }
    ear_clip(&ring)
}

fn point_in_tri_inclusive(p: IPt, a: IPt, b: IPt, c: IPt) -> bool {
    cross(a, b, p) >= 0 && cross(b, c, p) >= 0 && cross(c, a, p) >= 0
}

/// Ear clipping of a counter-clockwise, weakly simple ring.
fn ear_clip(ring: &[IPt]) -> Result<Vec<ITri>, TriError> {
    let n = ring.len();
    let mut prev: Vec<usize> = (0..n).map(|i| (i + n - 1) % n).collect();
    let mut next: Vec<usize> = (0..n).map(|i| (i + 1) % n).collect();
    let mut alive = vec![true; n];
    let mut remaining = n;
    let mut tris = Vec::with_capacity(n.saturating_sub(2));
    let convex =
        |i: usize, prev: &[usize], next: &[usize]| cross(ring[prev[i]], ring[i], ring[next[i]]) > 0;
    let mut cur = 0usize;
    while remaining > 3 {
        let mut found = None;
        let mut i = cur;
        for _ in 0..remaining {
            let (a, c) = (prev[i], next[i]);
            let (pa, pb, pc) = (ring[a], ring[i], ring[c]);
            let turn = cross(pa, pb, pc);
            if turn == 0 {
                // Degenerate vertex: drop it without a triangle.
                found = Some((i, false));
                break;
            }
            if turn > 0 {
                let blocked = (0..n).any(|v| {
                    alive[v]
                        && v != a
                        && v != i
                        && v != c
                        && ring[v] != pa
                        && ring[v] != pb
                        && ring[v] != pc
                        && !convex(v, &prev, &next)
                        && point_in_tri_inclusive(ring[v], pa, pb, pc)
                });
                if !blocked {
                    found = Some((i, true));
                    break;
                }
            }
            i = next[i];
        }
        let Some((i, emit)) = found else {
            return Err(TriError::Stuck);
        };
        let (a, c) = (prev[i], next[i]);
        if emit {
            tris.push([ring[a], ring[i], ring[c]]);
        }
        alive[i] = false;
        next[a] = c;
        prev[c] = a;
        remaining -= 1;
        cur = c;
    }
    let start = (0..n).find(|&v| alive[v]).ok_or(TriError::Stuck)?;
    let (a, b, c) = (ring[start], ring[next[start]], ring[next[next[start]]]);
    if cross(a, b, c) != 0 {
        tris.push([a, b, c]);
    }
    Ok(tris)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area_of(tris: &[ITri]) -> i64 {
        tris.iter().map(|t| cross(t[0], t[1], t[2]).abs()).sum()
    }

    fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<IPt> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    #[test]
    fn convex_polygon_fans_to_n_minus_two_triangles() {
        let t = triangulate(&[rect(0, 0, 100, 50)], FillRule::NonZero).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(area_of(&t), 2 * 100 * 50);
    }

    #[test]
    fn concave_l_shape_keeps_its_area() {
        let l = vec![(0, 0), (60, 0), (60, 20), (20, 20), (20, 60), (0, 60)];
        let t = triangulate(std::slice::from_ref(&l), FillRule::NonZero).unwrap();
        assert_eq!(area_of(&t), area2(&l).abs());
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn hole_is_cut_out() {
        let outer = rect(0, 0, 100, 100);
        let hole = rect(30, 30, 70, 70);
        let t = triangulate(&[outer.clone(), hole.clone()], FillRule::EvenOdd).unwrap();
        assert_eq!(area_of(&t), area2(&outer).abs() - area2(&hole).abs());
        // No triangle centre lies inside the hole.
        for tri in &t {
            let cx = (tri[0].0 + tri[1].0 + tri[2].0) as f64 / 3.0;
            let cy = (tri[0].1 + tri[1].1 + tri[2].1) as f64 / 3.0;
            assert!(
                !(cx > 30.0 && cx < 70.0 && cy > 30.0 && cy < 70.0),
                "centre in hole"
            );
        }
    }

    #[test]
    fn nonzero_same_direction_nested_contour_is_not_a_hole() {
        let outer = rect(0, 0, 100, 100);
        let inner = rect(30, 30, 70, 70);
        let t = triangulate(&[outer.clone(), inner], FillRule::NonZero).unwrap();
        assert_eq!(area_of(&t), area2(&outer).abs());
    }

    #[test]
    fn nonzero_opposite_direction_nested_contour_is_a_hole() {
        let outer = rect(0, 0, 100, 100);
        let mut inner = rect(30, 30, 70, 70);
        inner.reverse();
        let t = triangulate(&[outer.clone(), inner.clone()], FillRule::NonZero).unwrap();
        assert_eq!(area_of(&t), area2(&outer).abs() - area2(&inner).abs());
    }

    #[test]
    fn island_inside_a_hole_is_filled_again() {
        let t = triangulate(
            &[
                rect(0, 0, 200, 200),
                rect(20, 20, 180, 180),
                rect(60, 60, 140, 140),
            ],
            FillRule::EvenOdd,
        )
        .unwrap();
        let expect = 2 * (200 * 200 - 160 * 160 + 80 * 80);
        assert_eq!(area_of(&t), expect);
    }

    #[test]
    fn two_holes_in_one_face() {
        let outer = rect(0, 0, 300, 100);
        let (h1, h2) = (rect(20, 20, 80, 80), rect(120, 20, 180, 80));
        let t = triangulate(&[outer.clone(), h1.clone(), h2.clone()], FillRule::EvenOdd).unwrap();
        assert_eq!(
            area_of(&t),
            area2(&outer).abs() - area2(&h1).abs() - area2(&h2).abs()
        );
    }

    #[test]
    fn crossing_contours_are_reported() {
        let bow = vec![(0, 0), (100, 100), (100, 0), (0, 100)];
        assert_eq!(
            triangulate(&[bow], FillRule::NonZero),
            Err(TriError::Crossing)
        );
    }

    #[test]
    fn collinear_and_duplicate_vertices_are_tolerated() {
        let poly = vec![(0, 0), (50, 0), (50, 0), (100, 0), (100, 100), (0, 100)];
        let t = triangulate(&[poly], FillRule::NonZero).unwrap();
        assert_eq!(area_of(&t), 2 * 100 * 100);
    }

    #[test]
    fn many_sided_polygon_with_central_hole() {
        let ngon = |r: f64, n: usize| -> Vec<IPt> {
            (0..n)
                .map(|i| {
                    let a = i as f64 / n as f64 * std::f64::consts::TAU;
                    ((r * a.cos()).round() as i32, (r * a.sin()).round() as i32)
                })
                .collect()
        };
        let (outer, hole) = (ngon(1000.0, 48), ngon(400.0, 24));
        let t = triangulate(&[outer.clone(), hole.clone()], FillRule::EvenOdd).unwrap();
        assert_eq!(area_of(&t), area2(&outer).abs() - area2(&hole).abs());
    }
}
