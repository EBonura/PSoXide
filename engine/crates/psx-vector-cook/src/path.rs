//! SVG path data: parsing, arc conversion and flattening.

use crate::geom::{flatten_cubic, Pt, KAPPA};

/// One absolute path segment. Quadratics and arcs are converted to cubics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    /// Start a new subpath.
    Move(Pt),
    /// Straight line.
    Line(Pt),
    /// Cubic Bezier: two control points and the end point.
    Cubic(Pt, Pt, Pt),
    /// Close the subpath back to its start.
    Close,
}

/// A flattened subpath.
#[derive(Clone, Debug, PartialEq)]
pub struct Poly {
    /// Vertices; a closed polygon does not repeat its first point.
    pub pts: Vec<Pt>,
    /// Whether the subpath ended with `Z`.
    pub closed: bool,
}

struct Scanner<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    fn skip_sep(&mut self) {
        while self.pos < self.s.len()
            && (self.s[self.pos].is_ascii_whitespace() || self.s[self.pos] == b',')
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_sep();
        self.s.get(self.pos).copied()
    }

    fn number(&mut self) -> Result<f64, String> {
        self.skip_sep();
        let start = self.pos;
        if matches!(self.s.get(self.pos), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut digits = 0;
        while matches!(self.s.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
            digits += 1;
        }
        if self.s.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while matches!(self.s.get(self.pos), Some(b'0'..=b'9')) {
                self.pos += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return Err(format!("expected a number at offset {start}"));
        }
        if matches!(self.s.get(self.pos), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.s.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if matches!(self.s.get(self.pos), Some(b'0'..=b'9')) {
                while matches!(self.s.get(self.pos), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        std::str::from_utf8(&self.s[start..self.pos])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("bad number at offset {start}"))
    }

    /// An arc flag: a single `0` or `1` that may be packed against the next number.
    fn flag(&mut self) -> Result<bool, String> {
        self.skip_sep();
        match self.s.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Ok(true)
            }
            _ => Err(format!("expected an arc flag at offset {}", self.pos)),
        }
    }
}

/// Parse path data `d` into absolute segments.
pub fn parse_path(d: &str) -> Result<Vec<Seg>, String> {
    let mut sc = Scanner {
        s: d.as_bytes(),
        pos: 0,
    };
    let mut segs = Vec::new();
    let (mut cur, mut start) = ((0.0, 0.0), (0.0, 0.0));
    let mut last_cubic_ctrl: Option<Pt> = None;
    let mut last_quad_ctrl: Option<Pt> = None;
    let mut cmd = b' ';
    while let Some(next) = sc.peek() {
        if next.is_ascii_alphabetic() {
            cmd = next;
            sc.pos += 1;
            if cmd == b'Z' || cmd == b'z' {
                segs.push(Seg::Close);
                cur = start;
                last_cubic_ctrl = None;
                last_quad_ctrl = None;
                continue;
            }
        } else if cmd == b' ' || cmd == b'Z' || cmd == b'z' {
            return Err("path data must start with a command".into());
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { (0.0, 0.0) };
        let mut prev_cubic = None;
        let mut prev_quad = None;
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                segs.push(Seg::Move(p));
                cur = p;
                start = p;
                // Further coordinate pairs after M are implicit lineto.
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                segs.push(Seg::Line(p));
                cur = p;
            }
            b'H' => {
                let x = if rel { cur.0 } else { 0.0 } + sc.number()?;
                cur = (x, cur.1);
                segs.push(Seg::Line(cur));
            }
            b'V' => {
                let y = if rel { cur.1 } else { 0.0 } + sc.number()?;
                cur = (cur.0, y);
                segs.push(Seg::Line(cur));
            }
            b'C' => {
                let c1 = (base.0 + sc.number()?, base.1 + sc.number()?);
                let c2 = (base.0 + sc.number()?, base.1 + sc.number()?);
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                segs.push(Seg::Cubic(c1, c2, p));
                prev_cubic = Some(c2);
                cur = p;
            }
            b'S' => {
                let c1 = match last_cubic_ctrl {
                    Some(c) => (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1),
                    None => cur,
                };
                let c2 = (base.0 + sc.number()?, base.1 + sc.number()?);
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                segs.push(Seg::Cubic(c1, c2, p));
                prev_cubic = Some(c2);
                cur = p;
            }
            b'Q' => {
                let q = (base.0 + sc.number()?, base.1 + sc.number()?);
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                segs.push(quad_to_cubic(cur, q, p));
                prev_quad = Some(q);
                cur = p;
            }
            b'T' => {
                let q = match last_quad_ctrl {
                    Some(c) => (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1),
                    None => cur,
                };
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                segs.push(quad_to_cubic(cur, q, p));
                prev_quad = Some(q);
                cur = p;
            }
            b'A' => {
                let (rx, ry, rot) = (sc.number()?, sc.number()?, sc.number()?);
                let (large, sweep) = (sc.flag()?, sc.flag()?);
                let p = (base.0 + sc.number()?, base.1 + sc.number()?);
                arc_to_cubics(cur, rx, ry, rot, large, sweep, p, &mut segs);
                cur = p;
            }
            other => return Err(format!("unsupported path command `{}`", other as char)),
        }
        last_cubic_ctrl = prev_cubic;
        last_quad_ctrl = prev_quad;
    }
    Ok(segs)
}

fn quad_to_cubic(p0: Pt, q: Pt, p: Pt) -> Seg {
    let two_thirds = |a: Pt, b: Pt| (a.0 + 2.0 / 3.0 * (b.0 - a.0), a.1 + 2.0 / 3.0 * (b.1 - a.1));
    Seg::Cubic(two_thirds(p0, q), two_thirds(p, q), p)
}

/// Convert an SVG elliptical arc to cubic segments of at most a quarter turn.
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    from: Pt,
    rx: f64,
    ry: f64,
    rotation_deg: f64,
    large: bool,
    sweep: bool,
    to: Pt,
    out: &mut Vec<Seg>,
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if (from.0 - to.0).abs() < 1e-12 && (from.1 - to.1).abs() < 1e-12 {
        return;
    }
    if rx < 1e-12 || ry < 1e-12 {
        out.push(Seg::Line(to));
        return;
    }
    let (sin_phi, cos_phi) = rotation_deg.to_radians().sin_cos();
    let (dx, dy) = ((from.0 - to.0) / 2.0, (from.1 - to.1) / 2.0);
    let (x1p, y1p) = (cos_phi * dx + sin_phi * dy, -sin_phi * dx + cos_phi * dy);
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let (cxp, cyp) = (coef * rx * y1p / ry, -coef * ry * x1p / rx);
    let cx = cos_phi * cxp - sin_phi * cyp + (from.0 + to.0) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (from.1 + to.1) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| {
        let dot = ux * vx + uy * vy;
        let det = ux * vy - uy * vx;
        det.atan2(dot)
    };
    let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut delta = angle(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && delta > 0.0 {
        delta -= std::f64::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f64::consts::TAU;
    }
    let steps = (delta.abs() / std::f64::consts::FRAC_PI_2 - 1e-9)
        .ceil()
        .max(1.0) as usize;
    let step = delta / steps as f64;
    let t = 4.0 / 3.0 * (step / 4.0).tan();
    let point = |theta: f64| {
        let (s, c) = theta.sin_cos();
        (
            cx + rx * c * cos_phi - ry * s * sin_phi,
            cy + rx * c * sin_phi + ry * s * cos_phi,
        )
    };
    let deriv = |theta: f64| {
        let (s, c) = theta.sin_cos();
        (
            -rx * s * cos_phi - ry * c * sin_phi,
            -rx * s * sin_phi + ry * c * cos_phi,
        )
    };
    for i in 0..steps {
        let (a, b) = (theta1 + step * i as f64, theta1 + step * (i + 1) as f64);
        let (pa, pb) = (point(a), point(b));
        let (da, db) = (deriv(a), deriv(b));
        let end = if i + 1 == steps { to } else { pb };
        out.push(Seg::Cubic(
            (pa.0 + t * da.0, pa.1 + t * da.1),
            (pb.0 - t * db.0, pb.1 - t * db.1),
            end,
        ));
    }
}

/// Flatten `segs` (in user space) to polylines, with every curve within
/// `tolerance` user units.
pub fn flatten(segs: &[Seg], tolerance: f64) -> Vec<Poly> {
    let mut polys: Vec<Poly> = Vec::new();
    let mut cur: Option<Poly> = None;
    let mut last = (0.0, 0.0);
    let mut start = (0.0, 0.0);
    let finish = |poly: Option<Poly>, polys: &mut Vec<Poly>| {
        if let Some(p) = poly {
            if p.pts.len() > 1 {
                polys.push(p);
            }
        }
    };
    for seg in segs {
        match *seg {
            Seg::Move(p) => {
                finish(cur.take(), &mut polys);
                cur = Some(Poly {
                    pts: vec![p],
                    closed: false,
                });
                last = p;
                start = p;
            }
            Seg::Line(p) => {
                cur.get_or_insert_with(|| Poly {
                    pts: vec![last],
                    closed: false,
                })
                .pts
                .push(p);
                last = p;
            }
            Seg::Cubic(c1, c2, p) => {
                let poly = cur.get_or_insert_with(|| Poly {
                    pts: vec![last],
                    closed: false,
                });
                flatten_cubic(last, c1, c2, p, tolerance, &mut poly.pts);
                last = p;
            }
            Seg::Close => {
                if let Some(mut p) = cur.take() {
                    p.closed = true;
                    if p.pts.len() > 1 && p.pts.first() == p.pts.last() {
                        p.pts.pop();
                    }
                    finish(Some(p), &mut polys);
                }
                last = start;
            }
        }
    }
    finish(cur, &mut polys);
    polys
}

/// Segments of a rectangle, with optional rounded corners.
pub fn rect_segs(x: f64, y: f64, w: f64, h: f64, rx: f64, ry: f64) -> Vec<Seg> {
    let (rx, ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
    if rx <= 0.0 || ry <= 0.0 {
        return vec![
            Seg::Move((x, y)),
            Seg::Line((x + w, y)),
            Seg::Line((x + w, y + h)),
            Seg::Line((x, y + h)),
            Seg::Close,
        ];
    }
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    vec![
        Seg::Move((x + rx, y)),
        Seg::Line((x + w - rx, y)),
        Seg::Cubic((x + w - rx + kx, y), (x + w, y + ry - ky), (x + w, y + ry)),
        Seg::Line((x + w, y + h - ry)),
        Seg::Cubic(
            (x + w, y + h - ry + ky),
            (x + w - rx + kx, y + h),
            (x + w - rx, y + h),
        ),
        Seg::Line((x + rx, y + h)),
        Seg::Cubic((x + rx - kx, y + h), (x, y + h - ry + ky), (x, y + h - ry)),
        Seg::Line((x, y + ry)),
        Seg::Cubic((x, y + ry - ky), (x + rx - kx, y), (x + rx, y)),
        Seg::Close,
    ]
}

/// Segments of an ellipse as four cubic quarters.
pub fn ellipse_segs(cx: f64, cy: f64, rx: f64, ry: f64) -> Vec<Seg> {
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    vec![
        Seg::Move((cx + rx, cy)),
        Seg::Cubic((cx + rx, cy + ky), (cx + kx, cy + ry), (cx, cy + ry)),
        Seg::Cubic((cx - kx, cy + ry), (cx - rx, cy + ky), (cx - rx, cy)),
        Seg::Cubic((cx - rx, cy - ky), (cx - kx, cy - ry), (cx, cy - ry)),
        Seg::Cubic((cx + kx, cy - ry), (cx + rx, cy - ky), (cx + rx, cy)),
        Seg::Close,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_implicit_commands() {
        let segs = parse_path("m 10 10 20 0 v 10 h -20 z").unwrap();
        assert_eq!(
            segs,
            vec![
                Seg::Move((10.0, 10.0)),
                Seg::Line((30.0, 10.0)),
                Seg::Line((30.0, 20.0)),
                Seg::Line((10.0, 20.0)),
                Seg::Close
            ]
        );
    }

    #[test]
    fn packed_numbers_and_flags() {
        let segs = parse_path("M0,0a5,5 0 1010,0").unwrap();
        // Arc with flags 1,0 packed against the next number.
        assert!(matches!(segs.last(), Some(Seg::Cubic(..))));
    }

    #[test]
    fn cubic_flattening_stays_within_tolerance() {
        let (p0, p1, p2, p3) = ((0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0));
        let mut pts = vec![p0];
        flatten_cubic(p0, p1, p2, p3, 0.1, &mut pts);
        assert!(pts.len() > 4);
        for i in 0..=200 {
            let t = i as f64 / 200.0;
            let u = 1.0 - t;
            let x = u * u * u * p0.0
                + 3.0 * u * u * t * p1.0
                + 3.0 * u * t * t * p2.0
                + t * t * t * p3.0;
            let y = u * u * u * p0.1
                + 3.0 * u * u * t * p1.1
                + 3.0 * u * t * t * p2.1
                + t * t * t * p3.1;
            let d = pts
                .windows(2)
                .map(|w| {
                    let (a, b) = (w[0], w[1]);
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let l2 = dx * dx + dy * dy;
                    let s = (((x - a.0) * dx + (y - a.1) * dy) / l2).clamp(0.0, 1.0);
                    ((x - a.0 - s * dx).powi(2) + (y - a.1 - s * dy).powi(2)).sqrt()
                })
                .fold(f64::MAX, f64::min);
            assert!(d <= 0.1 + 1e-9, "t={t} distance {d}");
        }
    }
}
