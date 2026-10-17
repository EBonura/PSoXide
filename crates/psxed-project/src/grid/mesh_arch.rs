//! The arch generator: a band of masonry over an opening, with piers.
//!
//! Frame: origin on the floor at the centre of the opening, `x` across it,
//! `y` up, `z` the depth of the band, centred. The clear opening is `span`
//! wide between the piers; the springing line (where the curve starts) is
//! `spring` above the base, the crown of the intrados `rise` above that. The
//! band is `thickness` thick measured from the intrados outward, and the piers
//! under it are `thickness` wide.
//!
//! Profiles share one construction: two polylines of `segments` pieces per
//! half, the intrados `P` (the soffit, facing the opening) and the extrados
//! `Q` (the top), paired by index. A half is a quadrant of an ellipse
//! (`round`), an arc below a semicircle (`segmental`), an arc centred on the
//! opposite springing so the two halves meet in a point (`pointed`) or a
//! straight lintel (`flat`).
//!
//! Collision: one slab per extrados piece (thickness = the band's), so the
//! crown segments are surfaces to stand on and the haunches block, plus one
//! box per pier. Socket: a hook grip at the crown of the intrados.

use serde::{Deserialize, Serialize};

use super::mesh::{BuiltMesh, MeshFace, Proxy, Socket};
use super::units::UNITS_PER_TEXEL;

/// The curve of the intrados.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchProfile {
    /// A quadrant of an ellipse on each side: a semicircle when the rise is
    /// half the span.
    #[default]
    Round,
    /// An arc flatter than a semicircle (rise under half the span).
    Segmental,
    /// Two arcs meeting in a point (rise at least half the span).
    Pointed,
    /// A straight lintel (the rise is ignored).
    Flat,
}

impl ArchProfile {
    /// Every profile, in menu order.
    pub const ALL: [Self; 4] = [Self::Round, Self::Segmental, Self::Pointed, Self::Flat];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Round => "round",
            Self::Segmental => "segmental",
            Self::Pointed => "pointed",
            Self::Flat => "flat",
        }
    }

    /// The profile named `name`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.label().eq_ignore_ascii_case(name))
    }
}

/// The recipe of an arch, grid units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchSpec {
    /// Curve.
    #[serde(default)]
    pub profile: ArchProfile,
    /// Clear opening between the piers.
    pub span: i32,
    /// Height of the intrados crown above the springing line.
    pub rise: i32,
    /// Height of the springing line above the base (the piers).
    pub spring: i32,
    /// Thickness of the band and width of the piers.
    pub thickness: i32,
    /// Depth of the band along z.
    pub depth: i32,
    /// Pieces per half of the curve, 1 to 8.
    pub segments: u8,
    /// Whether the crown carries a hook socket, so the player can hang from
    /// it (the cook spawns a hook point per instance).
    #[serde(default = "perch_default")]
    pub perch: bool,
}

fn perch_default() -> bool {
    true
}

/// Material slots of an arch.
pub const SLOT_FASCIA: u8 = 0;
/// The underside of the band.
pub const SLOT_SOFFIT: u8 = 1;
/// The top of the band.
pub const SLOT_EXTRADOS: u8 = 2;
/// Piers.
pub const SLOT_PIER: u8 = 3;

/// Smallest and largest span, thickness, depth, grid units.
pub const SPAN_RANGE: (i32, i32) = (256, 8192);
/// Thickness limits.
pub const THICKNESS_RANGE: (i32, i32) = (64, 2048);
/// Depth limits.
pub const DEPTH_RANGE: (i32, i32) = (64, 4096);
/// Most pieces per half.
pub const MAX_SEGMENTS: u8 = 8;

impl Default for ArchSpec {
    fn default() -> Self {
        Self {
            profile: ArchProfile::Round,
            span: 2048,
            rise: 1024,
            spring: 1536,
            thickness: 256,
            depth: 1024,
            segments: 4,
            perch: true,
        }
    }
}

type P2 = [f64; 2];

/// The two polylines of the band, `(intrados, extrados)`, each from the left
/// springing over the crown to the right springing in the xy plane (grid
/// units, `y` measured from the springing line).
struct Curves {
    p: Vec<P2>,
    q: Vec<P2>,
}

impl ArchSpec {
    /// A spec spanning `span` with the proportions of a good default arch.
    pub fn spanning(profile: ArchProfile, span: i32) -> Self {
        let rise = match profile {
            ArchProfile::Round => span / 2,
            ArchProfile::Segmental => span / 4,
            ArchProfile::Pointed => span * 3 / 4,
            ArchProfile::Flat => 0,
        };
        Self {
            profile,
            span,
            rise,
            thickness: (span / 8).clamp(THICKNESS_RANGE.0, 512),
            spring: 1024,
            ..Self::default()
        }
    }

    /// Check the numbers.
    pub fn check(&self) -> Result<(), String> {
        let range = |what: &str, v: i32, (lo, hi): (i32, i32)| {
            if v < lo || v > hi {
                Err(format!("arch {what} {v} is outside {lo}..={hi} grid units"))
            } else {
                Ok(())
            }
        };
        range("span", self.span, SPAN_RANGE)?;
        range("thickness", self.thickness, THICKNESS_RANGE)?;
        range("depth", self.depth, DEPTH_RANGE)?;
        if self.spring < 0 || self.spring > 8192 {
            return Err(format!("arch spring {} is outside 0..=8192", self.spring));
        }
        if self.segments == 0 || self.segments > MAX_SEGMENTS {
            return Err(format!("arch segments must be 1 to {MAX_SEGMENTS}"));
        }
        let c = self.span / 2;
        match self.profile {
            ArchProfile::Flat => {}
            ArchProfile::Round => {
                if self.rise < 64 || self.rise > 8192 {
                    return Err(format!("arch rise {} is outside 64..=8192", self.rise));
                }
            }
            ArchProfile::Segmental => {
                if self.rise < 64 || self.rise >= c {
                    return Err(format!(
                        "a segmental arch rises less than half its span: rise {} must be 64..{c}",
                        self.rise
                    ));
                }
            }
            ArchProfile::Pointed => {
                if self.rise < c || self.rise > 8192 {
                    return Err(format!(
                        "a pointed arch rises at least half its span: rise {} must be {c}..=8192",
                        self.rise
                    ));
                }
            }
        }
        Ok(())
    }

    /// Height of the intrados crown above the base.
    pub fn crown(&self) -> i32 {
        match self.profile {
            ArchProfile::Flat => self.spring,
            _ => self.spring + self.rise,
        }
    }

    fn curves(&self) -> Curves {
        let c = f64::from(self.span) / 2.0;
        let t = f64::from(self.thickness);
        let n = usize::from(self.segments);
        // Right half from the springing (index 0) to the crown (index n).
        let (p, q): (Vec<P2>, Vec<P2>) = match self.profile {
            ArchProfile::Flat => (0..=n)
                .map(|k| {
                    let x = c * (1.0 - k as f64 / n as f64);
                    ([x, 0.0], [x, t])
                })
                .unzip(),
            ArchProfile::Round => {
                let b = f64::from(self.rise);
                (0..=n)
                    .map(|k| {
                        let a = (k as f64 / n as f64) * core::f64::consts::FRAC_PI_2;
                        // x = c cos a, y = b sin a, from the springing up.
                        let p = [c * a.cos(), b * a.sin()];
                        // Outward normal of the ellipse, (b cos a, c sin a).
                        let nx = b * a.cos();
                        let ny = c * a.sin();
                        let len = nx.hypot(ny).max(1e-9);
                        (p, [p[0] + t * nx / len, p[1] + t * ny / len])
                    })
                    .unzip()
            }
            ArchProfile::Segmental => {
                let r = (c * c + f64::from(self.rise).powi(2)) / (2.0 * f64::from(self.rise));
                let cy = f64::from(self.rise) - r;
                let max = (c / r).asin();
                (0..=n)
                    .map(|k| {
                        // Angle from the vertical: max at the springing, 0 at the crown.
                        let a = max * (1.0 - k as f64 / n as f64);
                        let dir = [a.sin(), a.cos()];
                        (
                            [r * dir[0], cy + r * dir[1]],
                            [(r + t) * dir[0], cy + (r + t) * dir[1]],
                        )
                    })
                    .unzip()
            }
            ArchProfile::Pointed => {
                let rise = f64::from(self.rise);
                let r = (c * c + rise * rise) / (2.0 * c);
                // The right half is an arc about (c - r, 0).
                let cx = c - r;
                let top_in = ((r - c) / r).clamp(-1.0, 1.0).acos();
                let top_ex = ((r - c) / (r + t)).clamp(-1.0, 1.0).acos();
                (0..=n)
                    .map(|k| {
                        let f = k as f64 / n as f64;
                        let (ai, ae) = (top_in * f, top_ex * f);
                        (
                            [cx + r * ai.cos(), r * ai.sin()],
                            [cx + (r + t) * ae.cos(), (r + t) * ae.sin()],
                        )
                    })
                    .unzip()
            }
        };
        // Left to right: the mirrored right half (springing to crown), then
        // the right half back down to the springing, the crown shared.
        let mirror = |half: &Vec<P2>| -> Vec<P2> {
            let mut all: Vec<P2> = half.iter().map(|p| [-p[0], p[1]]).collect();
            all.extend(half.iter().rev().skip(1));
            all
        };
        Curves {
            p: mirror(&p),
            q: mirror(&q),
        }
    }

    /// Build the faces, collision shapes and the crown socket.
    pub fn build(&self) -> Result<BuiltMesh, String> {
        self.check()?;
        let curves = self.curves();
        let spring = self.spring;
        let half_depth = self.depth / 2;
        let (z0, z1) = (-half_depth, half_depth);
        let lift = |p: &P2, z: i32| [p[0].round() as i32, spring + p[1].round() as i32, z];
        let texel = |p: [i32; 3]| -> [i32; 2] { [p[0] / UNITS_PER_TEXEL, -p[1] / UNITS_PER_TEXEL] };
        let mut faces: Vec<MeshFace> = Vec::new();
        let mut path_u = 0.0f64;
        let mut proxies: Vec<Proxy> = Vec::new();
        let count = curves.p.len() - 1;
        for i in 0..count {
            let (pa, pb) = (&curves.p[i], &curves.p[i + 1]);
            let (qa, qb) = (&curves.q[i], &curves.q[i + 1]);
            let along_p = (pb[0] - pa[0]).hypot(pb[1] - pa[1]);
            let along_q = (qb[0] - qa[0]).hypot(qb[1] - qa[1]);
            let u = |len: f64| (len / f64::from(UNITS_PER_TEXEL)).round() as i32;
            let v = |z: i32| (z - z0) / UNITS_PER_TEXEL;
            // Soffit, seen from the opening: ring runs so the right-hand
            // normal points into the opening.
            faces.push(MeshFace {
                pts: [lift(pa, z0), lift(pb, z0), lift(pb, z1), lift(pa, z1)],
                len: 4,
                texels: [
                    [u(path_u), v(z0)],
                    [u(path_u + along_p), v(z0)],
                    [u(path_u + along_p), v(z1)],
                    [u(path_u), v(z1)],
                ],
                slot: SLOT_SOFFIT,
                double_sided: false,
            });
            // Extrados, seen from above.
            faces.push(MeshFace {
                pts: [lift(qa, z0), lift(qa, z1), lift(qb, z1), lift(qb, z0)],
                len: 4,
                texels: [
                    [u(path_u), v(z0)],
                    [u(path_u), v(z1)],
                    [u(path_u + along_q), v(z1)],
                    [u(path_u + along_q), v(z0)],
                ],
                slot: SLOT_EXTRADOS,
                double_sided: false,
            });
            path_u += along_p;
            // Fascias, front (+z) and back (-z).
            let front = [lift(pa, z1), lift(pb, z1), lift(qb, z1), lift(qa, z1)];
            let back = [lift(pb, z0), lift(pa, z0), lift(qa, z0), lift(qb, z0)];
            // The ring order that points +z: p_a, p_b, q_b, q_a runs
            // left-to-right along the inside then back along the outside,
            // counter-clockwise seen from +z only when x increases, which
            // holds because the curve runs left to right.
            faces.push(MeshFace::new(&front, SLOT_FASCIA, texel, false));
            faces.push(MeshFace::new(&back, SLOT_FASCIA, texel, false));
            // Collision: the extrados piece as a slab the band's thickness.
            proxies.push(Proxy::Slab {
                corners: [lift(qa, z0), lift(qa, z1), lift(qb, z1), lift(qb, z0)],
                thickness: self.thickness,
            });
        }
        // Springing ends: with no piers the band's underside at the springing
        // line shows, from the intrados end out to the extrados end, facing
        // down, on both sides.
        if spring == 0 {
            for (end, left) in [(0usize, true), (count, false)] {
                let (p, q) = (&curves.p[end], &curves.q[end]);
                let mut ring = [lift(p, z0), lift(q, z0), lift(q, z1), lift(p, z1)];
                // The ring (p, q, q', p') faces up on the left end and down
                // on the right end.
                if left {
                    ring.reverse();
                }
                faces.push(MeshFace::new(
                    &ring,
                    SLOT_FASCIA,
                    |p| [p[0] / UNITS_PER_TEXEL, p[2] / UNITS_PER_TEXEL],
                    false,
                ));
            }
        }
        // Piers under the springing.
        if spring > 0 {
            let c = self.span / 2;
            let t = self.thickness;
            for sign in [-1i32, 1] {
                let (x_in, x_out) = (sign * c, sign * (c + t));
                let (lo, hi) = (x_in.min(x_out), x_in.max(x_out));
                self.pier(&mut faces, lo, hi, spring, (z0, z1));
                proxies.push(Proxy::Box {
                    min: [lo, 0, z0],
                    max: [hi, spring, z1],
                    top: false,
                });
            }
        }
        let anchor = [0, self.crown(), 0];
        Ok(BuiltMesh {
            faces,
            proxies,
            sockets: if self.perch {
                vec![Socket::Hook { anchor, yaw: 0 }]
            } else {
                Vec::new()
            },
            slots: vec![
                "fascia".to_string(),
                "soffit".to_string(),
                "extrados".to_string(),
                "pier".to_string(),
            ],
        })
    }

    /// The four visible sides of one pier box `[lo, hi] x [0, top] x [z0, z1]`.
    fn pier(&self, faces: &mut Vec<MeshFace>, lo: i32, hi: i32, top: i32, (z0, z1): (i32, i32)) {
        let tx = |p: [i32; 3]| [p[0] / UNITS_PER_TEXEL, -p[1] / UNITS_PER_TEXEL];
        let tz = |p: [i32; 3]| [p[2] / UNITS_PER_TEXEL, -p[1] / UNITS_PER_TEXEL];
        // Counter-clockwise seen from outside each side.
        let sides: [([[i32; 3]; 4], bool); 4] = [
            ([[lo, 0, z1], [hi, 0, z1], [hi, top, z1], [lo, top, z1]], true),
            ([[hi, 0, z0], [lo, 0, z0], [lo, top, z0], [hi, top, z0]], true),
            ([[lo, 0, z0], [lo, 0, z1], [lo, top, z1], [lo, top, z0]], false),
            ([[hi, 0, z1], [hi, 0, z0], [hi, top, z0], [hi, top, z1]], false),
        ];
        for (ring, across_x) in sides {
            if across_x {
                faces.push(MeshFace::new(&ring, SLOT_PIER, tx, false));
            } else {
                faces.push(MeshFace::new(&ring, SLOT_PIER, tz, false));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(face: &MeshFace) -> [f64; 3] {
        let r = face.ring();
        let a = [0, 1, 2].map(|i| f64::from(r[1][i] - r[0][i]));
        let b = [0, 1, 2].map(|i| f64::from(r[2][i] - r[0][i]));
        let n = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-9);
        n.map(|v| v / len)
    }

    fn centre(face: &MeshFace) -> [f64; 3] {
        let r = face.ring();
        [0, 1, 2].map(|i| r.iter().map(|p| f64::from(p[i])).sum::<f64>() / r.len() as f64)
    }

    #[test]
    fn every_profile_builds_and_keeps_the_crown_at_the_rise() {
        for profile in ArchProfile::ALL {
            let mut spec = ArchSpec::spanning(profile, 2048);
            spec.spring = 1024;
            let built = spec.build().unwrap_or_else(|e| panic!("{profile:?}: {e}"));
            assert!(!built.faces.is_empty());
            assert_eq!(built.sockets.len(), 1);
            let Socket::Hook { anchor, .. } = built.sockets[0];
            assert_eq!(anchor, [0, spec.crown(), 0], "{profile:?}");
            // The highest intrados point is the crown.
            let top = built
                .faces
                .iter()
                .filter(|f| f.slot == SLOT_SOFFIT)
                .flat_map(|f| f.ring().iter().map(|p| p[1]))
                .max()
                .unwrap();
            assert_eq!(top, spec.crown(), "{profile:?}");
        }
    }

    #[test]
    fn faces_point_the_way_a_viewer_expects() {
        let spec = ArchSpec {
            profile: ArchProfile::Round,
            ..ArchSpec::default()
        };
        let built = spec.build().unwrap();
        let crown = f64::from(spec.crown());
        for face in &built.faces {
            let n = normal(face);
            let c = centre(face);
            match face.slot {
                SLOT_SOFFIT => {
                    // The soffit faces the inside of the opening: toward the
                    // point under the crown at the springing line.
                    let to = [-c[0], f64::from(spec.spring) - c[1], 0.0];
                    assert!(n[0] * to[0] + n[1] * to[1] > 0.0, "soffit {c:?} {n:?}");
                }
                SLOT_EXTRADOS => {
                    assert!(n[1] >= -1e-6 && c[1] < crown + f64::from(spec.thickness) + 1.0);
                    // Faces away from the centre of the opening.
                    let out = [c[0], c[1] - f64::from(spec.spring), 0.0];
                    assert!(n[0] * out[0] + n[1] * out[1] > 0.0, "extrados {c:?} {n:?}");
                }
                SLOT_PIER => {
                    // Away from the pier's own centre.
                    let sign = c[0].signum();
                    let pier = [
                        sign * (f64::from(spec.span) / 2.0 + f64::from(spec.thickness) / 2.0),
                        f64::from(spec.spring) / 2.0,
                        0.0,
                    ];
                    let out = [c[0] - pier[0], c[1] - pier[1], c[2] - pier[2]];
                    assert!(
                        n[0] * out[0] + n[1] * out[1] + n[2] * out[2] > 0.0,
                        "pier {c:?} {n:?}"
                    );
                }
                _ => {
                    // Fascias look along the depth axis, away from the middle.
                    if n[2].abs() > 0.99 {
                        assert!(n[2] * c[2].signum() > 0.0, "fascia {c:?} {n:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn collision_is_one_slab_per_piece_and_one_box_per_pier() {
        let spec = ArchSpec::default();
        let built = spec.build().unwrap();
        let slabs = built
            .proxies
            .iter()
            .filter(|p| matches!(p, Proxy::Slab { .. }))
            .count();
        let boxes = built
            .proxies
            .iter()
            .filter(|p| matches!(p, Proxy::Box { .. }))
            .count();
        assert_eq!(slabs, 2 * usize::from(spec.segments));
        assert_eq!(boxes, 2);
        let flat = ArchSpec {
            spring: 0,
            ..spec
        };
        assert_eq!(
            flat.build()
                .unwrap()
                .proxies
                .iter()
                .filter(|p| matches!(p, Proxy::Box { .. }))
                .count(),
            0
        );
    }

    #[test]
    fn bad_recipes_say_what_is_wrong() {
        let base = ArchSpec::default();
        let pointed = ArchSpec {
            profile: ArchProfile::Pointed,
            rise: 512,
            ..base
        };
        assert!(pointed.build().unwrap_err().contains("at least half"));
        let segmental = ArchSpec {
            profile: ArchProfile::Segmental,
            rise: 1500,
            ..base
        };
        assert!(segmental.build().unwrap_err().contains("less than half"));
        assert!(ArchSpec { span: 10, ..base }.build().is_err());
        assert!(ArchSpec { segments: 0, ..base }.build().is_err());
    }

    #[test]
    fn the_largest_arch_stays_inside_one_plane_run() {
        let spec = ArchSpec {
            segments: MAX_SEGMENTS,
            ..ArchSpec::default()
        };
        let built = spec.build().unwrap();
        assert!(built.faces.len() <= super::super::mesh::MAX_MESH_FACES);
        // 16 pieces of 4 faces each and 8 pier faces; piers cover the ends.
        assert_eq!(built.faces.len(), 16 * 4 + 8);
    }

    #[test]
    fn a_round_arch_is_a_semicircle_when_the_rise_is_half_the_span() {
        let spec = ArchSpec {
            profile: ArchProfile::Round,
            span: 2048,
            rise: 1024,
            spring: 0,
            thickness: 256,
            depth: 512,
            segments: 8,
            perch: false,
        };
        let curves = spec.curves();
        for p in &curves.p {
            let r = p[0].hypot(p[1]);
            assert!((r - 1024.0).abs() < 1.0, "{p:?} radius {r}");
        }
        for q in &curves.q {
            let r = q[0].hypot(q[1]);
            assert!((r - 1280.0).abs() < 1.0, "{q:?} radius {r}");
        }
    }
}
