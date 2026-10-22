//! Affine transforms, curve flattening and small geometry helpers (f64).

/// A point in f64 user or output space.
pub type Pt = (f64, f64);

/// An SVG affine matrix `[a b c d e f]`: `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    /// x scale / rotation term.
    pub a: f64,
    /// y shear term.
    pub b: f64,
    /// x shear term.
    pub c: f64,
    /// y scale / rotation term.
    pub d: f64,
    /// x translation.
    pub e: f64,
    /// y translation.
    pub f: f64,
}

impl Affine {
    /// The identity.
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Translation by `(x, y)`.
    pub fn translate(x: f64, y: f64) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    /// Scale by `(x, y)`.
    pub fn scale(x: f64, y: f64) -> Self {
        Self {
            a: x,
            d: y,
            ..Self::IDENTITY
        }
    }

    /// Rotation by `degrees`, clockwise on screen.
    pub fn rotate(degrees: f64) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }

    /// The matrix that applies `inner` first and then `self`.
    pub fn compose(&self, inner: &Self) -> Self {
        Self {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    /// Transform a point.
    pub fn apply(&self, p: Pt) -> Pt {
        (
            self.a * p.0 + self.c * p.1 + self.e,
            self.b * p.0 + self.d * p.1 + self.f,
        )
    }

    /// The inverse, or `None` for a singular matrix.
    pub fn inverse(&self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Self {
            a,
            b,
            c,
            d,
            e: -(a * self.e + c * self.f),
            f: -(b * self.e + d * self.f),
        })
    }

    /// The largest factor this matrix stretches a length by.
    pub fn max_scale(&self) -> f64 {
        (self.a * self.a + self.b * self.b)
            .max(self.c * self.c + self.d * self.d)
            .sqrt()
    }
}

/// Distance from `p` to the infinite line through `a` and `b` (or to `a` when
/// they coincide).
fn line_distance(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-12 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len
}

/// Append the flattening of a cubic Bezier to `out`, excluding `p0` and
/// including `p3`, with every point of the curve within `tolerance` of the
/// polyline. Subdivides at the midpoint until both control points are flat.
pub fn flatten_cubic(p0: Pt, p1: Pt, p2: Pt, p3: Pt, tolerance: f64, out: &mut Vec<Pt>) {
    flatten_cubic_depth(p0, p1, p2, p3, tolerance, 0, out);
}

fn flatten_cubic_depth(
    p0: Pt,
    p1: Pt,
    p2: Pt,
    p3: Pt,
    tolerance: f64,
    depth: u32,
    out: &mut Vec<Pt>,
) {
    let flat = line_distance(p1, p0, p3).max(line_distance(p2, p0, p3)) <= tolerance;
    if flat || depth >= 18 {
        out.push(p3);
        return;
    }
    let mid = |a: Pt, b: Pt| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
    let (p01, p12, p23) = (mid(p0, p1), mid(p1, p2), mid(p2, p3));
    let (p012, p123) = (mid(p01, p12), mid(p12, p23));
    let p0123 = mid(p012, p123);
    flatten_cubic_depth(p0, p01, p012, p0123, tolerance, depth + 1, out);
    flatten_cubic_depth(p0123, p123, p23, p3, tolerance, depth + 1, out);
}

/// The control-point constant of a quarter-ellipse cubic.
pub const KAPPA: f64 = 0.552_284_749_830_793_4;
