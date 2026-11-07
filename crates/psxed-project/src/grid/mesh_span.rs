//! The span generator: a thick deck between two supports.
//!
//! A bridge, a gantry or the lintel of a floating stone arch is a straight
//! prism whose top and underside are parallel and which may climb from one
//! end to the other. It builds six closed faces and one collision slab as
//! thick as the deck.
//!
//! Frame: `x` runs along the span from the start (`-length / 2`) to the end
//! (`+length / 2`), `z` across it (centred) and `y` up. The top surface
//! meets the supports at its two ends: `rise` is how much higher the end is
//! than the start. The origin sits at the height of the LOWER end of the top
//! surface, so a level deck has its top at `y = 0` and its underside at
//! `y = -thickness`.

use serde::{Deserialize, Serialize};

use super::mesh::{BuiltMesh, MeshFace, Proxy};
use super::mesh_column::{outward, texel};

/// Material slot of the top surface.
pub const SLOT_TOP: u8 = 0;
/// The four sides.
pub const SLOT_SIDES: u8 = 1;
/// The underside.
pub const SLOT_UNDERSIDE: u8 = 2;

/// Length limits, grid units.
pub const LENGTH_RANGE: (i32, i32) = (128, 16384);
/// Width limits.
pub const WIDTH_RANGE: (i32, i32) = (64, 8192);
/// Thickness limits.
pub const THICKNESS_RANGE: (i32, i32) = (64, 8192);

/// The recipe of a span, grid units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanSpec {
    /// Between the two supports, along the deck.
    pub length: i32,
    /// Across the deck.
    pub width: i32,
    /// Vertical thickness.
    pub thickness: i32,
    /// Height of the end above the start (negative descends). At most the
    /// length, which is a slope of 45 degrees.
    pub rise: i32,
}

impl Default for SpanSpec {
    fn default() -> Self {
        Self {
            length: 4096,
            width: 1024,
            thickness: 512,
            rise: 0,
        }
    }
}

impl SpanSpec {
    /// A level deck of `length`, with proportions that read as a bridge: a
    /// quarter as wide and an eighth as thick, within the limits.
    pub fn spanning(length: i32) -> Self {
        Self {
            length,
            width: (length / 4).clamp(WIDTH_RANGE.0, WIDTH_RANGE.1),
            thickness: (length / 8).clamp(THICKNESS_RANGE.0, 1024),
            rise: 0,
        }
    }

    /// Check the numbers.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first number outside its range.
    pub fn check(&self) -> Result<(), String> {
        let within = |name: &str, v: i32, (lo, hi): (i32, i32)| {
            if (lo..=hi).contains(&v) {
                Ok(())
            } else {
                Err(format!("span {name} {v} is outside {lo}..={hi}"))
            }
        };
        within("length", self.length, LENGTH_RANGE)?;
        within("width", self.width, WIDTH_RANGE)?;
        within("thickness", self.thickness, THICKNESS_RANGE)?;
        if self.rise.abs() > self.length {
            return Err(format!(
                "span rise {} is steeper than 45 degrees over a length of {}",
                self.rise, self.length
            ));
        }
        Ok(())
    }

    /// Heights of the top surface at the start and at the end.
    pub fn end_tops(&self) -> [i32; 2] {
        [(-self.rise).max(0), self.rise.max(0)]
    }

    /// Build the six faces and the slab.
    ///
    /// # Errors
    ///
    /// Returns the message of [`Self::check`].
    pub fn build(&self) -> Result<BuiltMesh, String> {
        self.check()?;
        let (hx, hz) = (self.length / 2, self.width / 2);
        let [t0, t1] = self.end_tops();
        let (b0, b1) = (t0 - self.thickness, t1 - self.thickness);
        // The eight corners: start/end, left/right (z), top/bottom.
        let corner = |x: i32, y: i32, z: i32| [x, y, z];
        let (s_top_l, s_top_r) = (corner(-hx, t0, -hz), corner(-hx, t0, hz));
        let (e_top_l, e_top_r) = (corner(hx, t1, -hz), corner(hx, t1, hz));
        let (s_bot_l, s_bot_r) = (corner(-hx, b0, -hz), corner(-hx, b0, hz));
        let (e_bot_l, e_bot_r) = (corner(hx, b1, -hz), corner(hx, b1, hz));
        let centre = [0.0, f64::from(t0 + t1 + b0 + b1) / 4.0, 0.0];
        let face = |ring: [[i32; 3]; 4], slot: u8| {
            MeshFace::new(&outward(ring.to_vec(), centre), slot, texel, false)
        };
        let faces = vec![
            face([s_top_l, s_top_r, e_top_r, e_top_l], SLOT_TOP),
            face([s_bot_l, s_bot_r, e_bot_r, e_bot_l], SLOT_UNDERSIDE),
            face([s_top_l, e_top_l, e_bot_l, s_bot_l], SLOT_SIDES),
            face([s_top_r, e_top_r, e_bot_r, s_bot_r], SLOT_SIDES),
            face([s_top_l, s_top_r, s_bot_r, s_bot_l], SLOT_SIDES),
            face([e_top_l, e_top_r, e_bot_r, e_bot_l], SLOT_SIDES),
        ];
        Ok(BuiltMesh {
            faces,
            proxies: vec![Proxy::Slab {
                corners: [s_top_l, s_top_r, e_top_r, e_top_l],
                thickness: self.thickness,
            }],
            sockets: Vec::new(),
            slots: vec![
                "top".to_string(),
                "sides".to_string(),
                "underside".to_string(),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Normal of a face's ring, right-handed, unnormalised.
    fn normal(face: &MeshFace) -> [i64; 3] {
        let p = |i: usize| face.pts[i].map(i64::from);
        let (a, b, c) = (p(0), p(1), p(2));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
        [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ]
    }

    #[test]
    fn a_level_span_is_a_closed_prism_facing_outward() {
        let built = SpanSpec::default().build().unwrap();
        assert_eq!(built.faces.len(), 6);
        assert_eq!(built.proxies.len(), 1);
        // The six normals point along +y, -y, -z, +z, -x, +x in some order:
        // each axis direction exactly once.
        let mut seen = [false; 6];
        for face in &built.faces {
            let n = normal(face);
            let axis = (0..3).find(|&i| n[i] != 0).unwrap();
            assert!((0..3).filter(|&i| n[i] != 0).count() == 1, "{n:?}");
            seen[axis * 2 + usize::from(n[axis] > 0)] = true;
        }
        assert!(seen.iter().all(|&s| s), "{seen:?}");
        // The top of a level span is the origin plane.
        let Proxy::Slab { corners, thickness } = built.proxies[0] else {
            panic!("a slab");
        };
        assert!(corners.iter().all(|c| c[1] == 0));
        assert_eq!(thickness, 512);
    }

    #[test]
    fn a_climbing_span_rises_from_its_start_and_stays_a_prism() {
        let spec = SpanSpec {
            rise: 1024,
            ..SpanSpec::default()
        };
        let built = spec.build().unwrap();
        let Proxy::Slab { corners, .. } = built.proxies[0] else {
            panic!("a slab");
        };
        // Start corners at 0, end corners at the rise.
        assert_eq!(corners.map(|c| c[1]), [0, 0, 1024, 1024]);
        // Descending puts the start high.
        let down = SpanSpec {
            rise: -1024,
            ..SpanSpec::default()
        };
        assert_eq!(down.end_tops(), [1024, 0]);
        // The underside follows the top.
        let lowest = built
            .faces
            .iter()
            .flat_map(|f| f.ring().iter().map(|p| p[1]))
            .min();
        assert_eq!(lowest, Some(-512));
    }

    #[test]
    fn the_numbers_are_checked() {
        assert!(SpanSpec::default().check().is_ok());
        for bad in [
            SpanSpec {
                length: 64,
                ..SpanSpec::default()
            },
            SpanSpec {
                width: 20000,
                ..SpanSpec::default()
            },
            SpanSpec {
                thickness: 0,
                ..SpanSpec::default()
            },
            SpanSpec {
                rise: 5000,
                ..SpanSpec::default()
            },
        ] {
            assert!(bad.build().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_document_form_round_trips() {
        let spec = SpanSpec::spanning(6144);
        let text = ron::to_string(&spec).unwrap();
        assert_eq!(ron::from_str::<SpanSpec>(&text).unwrap(), spec);
    }
}
