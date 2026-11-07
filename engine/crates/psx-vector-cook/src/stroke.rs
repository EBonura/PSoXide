//! Stroking: a polyline to triangles in user space.
//!
//! Each segment becomes a quad, each join a wedge (bevel), a mitre quad or a
//! round fan, each end a cap. Pieces overlap on the inner side of a join,
//! which is invisible for opaque strokes; a semi-transparent stroke shows the
//! overlap, so author those as filled outlines.

use crate::geom::Pt;
use crate::paint::{Cap, Join};

/// A user-space triangle.
pub type Tri = [Pt; 3];

fn sub(a: Pt, b: Pt) -> Pt {
    (a.0 - b.0, a.1 - b.1)
}

fn add(a: Pt, b: Pt) -> Pt {
    (a.0 + b.0, a.1 + b.1)
}

fn scale(a: Pt, k: f64) -> Pt {
    (a.0 * k, a.1 * k)
}

fn norm(a: Pt) -> Option<Pt> {
    let len = (a.0 * a.0 + a.1 * a.1).sqrt();
    (len > 1e-12).then(|| (a.0 / len, a.1 / len))
}

fn cross(a: Pt, b: Pt) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn dot(a: Pt, b: Pt) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

/// Arc fan around `center` from direction `from` to `to` (unit vectors) by the
/// shorter way, as triangles.
fn arc_fan(center: Pt, from: Pt, to: Pt, radius: f64, tol: f64, out: &mut Vec<Tri>) {
    let angle = cross(from, to).atan2(dot(from, to));
    let step = if radius > tol {
        2.0 * (1.0 - tol / radius).acos()
    } else {
        std::f64::consts::FRAC_PI_2
    };
    let n = (angle.abs() / step.max(0.05)).ceil().max(1.0) as usize;
    let mut last = add(center, scale(from, radius));
    for i in 1..=n {
        let a = angle * i as f64 / n as f64;
        let (s, c) = a.sin_cos();
        let dir = (from.0 * c - from.1 * s, from.0 * s + from.1 * c);
        let p = add(center, scale(dir, radius));
        out.push([center, last, p]);
        last = p;
    }
}

/// Half-disc fan at `center`: from `normal` through `outward` to `-normal`.
fn cap_fan(center: Pt, normal: Pt, outward: Pt, radius: f64, tol: f64, out: &mut Vec<Tri>) {
    let step = if radius > tol {
        2.0 * (1.0 - tol / radius).acos()
    } else {
        std::f64::consts::FRAC_PI_2
    };
    let n = (std::f64::consts::PI / step.max(0.05)).ceil().max(2.0) as usize;
    let at = |i: usize| {
        let (s, c) = (std::f64::consts::PI * i as f64 / n as f64).sin_cos();
        add(
            center,
            scale(add(scale(normal, c), scale(outward, s)), radius),
        )
    };
    for i in 0..n {
        out.push([center, at(i), at(i + 1)]);
    }
}

/// Stroke parameters.
#[derive(Clone, Copy, Debug)]
pub struct StrokeStyle {
    /// Full width in user units.
    pub width: f64,
    /// Join style.
    pub join: Join,
    /// Cap style.
    pub cap: Cap,
    /// Miter limit.
    pub miter_limit: f64,
    /// Curve tolerance for round joins and caps, in user units.
    pub tolerance: f64,
}

/// Triangles covering the stroke of `pts` (closed when `closed`).
pub fn stroke_polyline(pts: &[Pt], closed: bool, st: &StrokeStyle) -> Vec<Tri> {
    let hw = st.width / 2.0;
    let mut clean: Vec<Pt> = Vec::with_capacity(pts.len());
    for &p in pts {
        if clean.last().is_none_or(|&q| norm(sub(p, q)).is_some()) {
            clean.push(p);
        }
    }
    if closed && clean.len() > 1 && norm(sub(clean[0], clean[clean.len() - 1])).is_none() {
        clean.pop();
    }
    let n = clean.len();
    let mut out = Vec::new();
    if n < 2 || hw <= 0.0 {
        return out;
    }
    let seg_count = if closed { n } else { n - 1 };
    let dir_of = |i: usize| norm(sub(clean[(i + 1) % n], clean[i])).unwrap_or((1.0, 0.0));
    for i in 0..seg_count {
        let (a, b) = (clean[i], clean[(i + 1) % n]);
        let d = dir_of(i);
        let nrm = (-d.1 * hw, d.0 * hw);
        let (mut a, mut b) = (a, b);
        if !closed && st.cap == Cap::Square {
            if i == 0 {
                a = sub(a, scale(d, hw));
            }
            if i == seg_count - 1 {
                b = add(b, scale(d, hw));
            }
        }
        let (a1, a2, b1, b2) = (add(a, nrm), sub(a, nrm), add(b, nrm), sub(b, nrm));
        out.push([a1, a2, b1]);
        out.push([a2, b2, b1]);
    }
    let join_range = if closed { 0..n } else { 1..n - 1 };
    for i in join_range {
        let d0 = dir_of((i + seg_count - 1) % seg_count);
        let d1 = dir_of(i % seg_count);
        let turn = cross(d0, d1);
        if turn.abs() < 1e-9 && dot(d0, d1) > 0.0 {
            continue;
        }
        let p = clean[i];
        // Outer side is opposite the turn.
        let s = if turn > 0.0 { -1.0 } else { 1.0 };
        let u0 = (-d0.1 * s, d0.0 * s);
        let u1 = (-d1.1 * s, d1.0 * s);
        let o0 = add(p, scale(u0, hw));
        let o1 = add(p, scale(u1, hw));
        match st.join {
            Join::Bevel => out.push([p, o0, o1]),
            Join::Round => arc_fan(p, u0, u1, hw, st.tolerance, &mut out),
            Join::Miter => {
                let cos_half_sq = (1.0 + dot(u0, u1)) / 2.0;
                let ratio = if cos_half_sq > 1e-12 {
                    1.0 / cos_half_sq.sqrt()
                } else {
                    f64::MAX
                };
                if ratio <= st.miter_limit {
                    let tip = add(p, scale(add(u0, u1), hw / (1.0 + dot(u0, u1))));
                    out.push([p, o0, tip]);
                    out.push([p, tip, o1]);
                } else {
                    out.push([p, o0, o1]);
                }
            }
        }
    }
    if !closed && st.cap == Cap::Round {
        let side = |d: Pt| (-d.1, d.0);
        let (d_first, d_last) = (dir_of(0), dir_of(seg_count - 1));
        cap_fan(
            clean[0],
            side(d_first),
            scale(d_first, -1.0),
            hw,
            st.tolerance,
            &mut out,
        );
        cap_fan(
            clean[n - 1],
            side(d_last),
            d_last,
            hw,
            st.tolerance,
            &mut out,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(tris: &[Tri]) -> f64 {
        tris.iter()
            .map(|t| cross(sub(t[1], t[0]), sub(t[2], t[0])).abs() / 2.0)
            .sum()
    }

    fn style(join: Join) -> StrokeStyle {
        StrokeStyle {
            width: 2.0,
            join,
            cap: Cap::Butt,
            miter_limit: 4.0,
            tolerance: 0.05,
        }
    }

    #[test]
    fn straight_segment_is_a_rectangle() {
        let t = stroke_polyline(&[(0.0, 0.0), (10.0, 0.0)], false, &style(Join::Miter));
        assert!((area(&t) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn closed_square_with_mitre_joins_has_a_ring_area() {
        let sq = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let t = stroke_polyline(&sq, true, &style(Join::Miter));
        // Union of the ring is 12x12 - 8x8 = 80; the segment quads overlap the
        // join squares so the triangle sum is at least that.
        assert!(area(&t) >= 80.0 - 1e-9);
    }
}
