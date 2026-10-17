//! Decorative generators: a column and a scatter of rubble.
//!
//! Both build in the asset frame (origin on the floor at the centre of the
//! base, `y` up). Shared helper [`outward`] winds a ring so its normal points
//! away from a centre, which is how a closed convex piece is made visible
//! from outside whatever order its corners were generated in.

use core::f64::consts::TAU;

use serde::{Deserialize, Serialize};

use super::mesh::{BuiltMesh, MeshFace, Proxy};
use super::units::UNITS_PER_TEXEL;

/// Material slot of the shaft or the stones.
pub const SLOT_STONE: u8 = 0;
/// Material slot of the plinth and capital.
pub const SLOT_TRIM: u8 = 1;

/// Wind `ring` so its right-hand normal points away from `centre`.
pub(super) fn outward(mut ring: Vec<[i32; 3]>, centre: [f64; 3]) -> Vec<[i32; 3]> {
    let mut n = [0.0f64; 3];
    let len = ring.len();
    for i in 0..len {
        let a = ring[i].map(f64::from);
        let b = ring[(i + 1) % len].map(f64::from);
        n[0] += a[1] * b[2] - a[2] * b[1];
        n[1] += a[2] * b[0] - a[0] * b[2];
        n[2] += a[0] * b[1] - a[1] * b[0];
    }
    let mid: [f64; 3] = core::array::from_fn(|i| ring.iter().map(|p| f64::from(p[i])).sum::<f64>() / len as f64);
    let away = [mid[0] - centre[0], mid[1] - centre[1], mid[2] - centre[2]];
    if n[0] * away[0] + n[1] * away[1] + n[2] * away[2] < 0.0 {
        ring.reverse();
    }
    ring
}

/// Planar texels: `x` and `z` along the face, `-y` down it, 32 to a sector.
fn texel(p: [i32; 3]) -> [i32; 2] {
    [(p[0] + p[2]) / UNITS_PER_TEXEL, -p[1] / UNITS_PER_TEXEL]
}

/// The recipe of a column, grid units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpec {
    /// Circumradius of the shaft.
    pub radius: i32,
    /// Total height.
    pub height: i32,
    /// Sides of the shaft, 3 to 12.
    pub sides: u8,
    /// Height of the square plinth under the shaft.
    pub plinth: i32,
    /// Height of the square capital over it.
    pub capital: i32,
}

impl Default for ColumnSpec {
    fn default() -> Self {
        Self {
            radius: 192,
            height: 2560,
            sides: 8,
            plinth: 256,
            capital: 192,
        }
    }
}

impl ColumnSpec {
    /// Check the numbers.
    pub fn check(&self) -> Result<(), String> {
        if !(64..=1024).contains(&self.radius) {
            return Err(format!("column radius {} is outside 64..=1024", self.radius));
        }
        if !(3..=12).contains(&self.sides) {
            return Err("column sides must be 3 to 12".to_string());
        }
        if self.plinth < 0 || self.capital < 0 {
            return Err("column plinth and capital cannot be negative".to_string());
        }
        if self.height < self.plinth + self.capital + 128 || self.height > 16384 {
            return Err(format!(
                "column height {} must leave at least 128 for the shaft (plinth {} + capital {}) and stay under 16384",
                self.height, self.plinth, self.capital
            ));
        }
        Ok(())
    }

    /// Half width of the plinth and capital.
    pub fn trim_half(&self) -> i32 {
        self.radius * 3 / 2
    }

    /// Build the faces and collision boxes.
    pub fn build(&self) -> Result<BuiltMesh, String> {
        self.check()?;
        let r = f64::from(self.radius);
        let w = self.trim_half();
        let (lo, hi) = (self.plinth, self.height - self.capital);
        let n = usize::from(self.sides);
        let mut faces = Vec::new();
        let ring_pt = |k: usize, y: i32| {
            let a = TAU * (k as f64 + 0.5) / n as f64;
            [(r * a.cos()).round() as i32, y, (r * a.sin()).round() as i32]
        };
        let axis = [0.0, f64::from(self.height) / 2.0, 0.0];
        for k in 0..n {
            let ring = vec![ring_pt(k, lo), ring_pt(k + 1, lo), ring_pt(k + 1, hi), ring_pt(k, hi)];
            let centre = [0.0, f64::from(lo + hi) / 2.0, 0.0];
            faces.push(MeshFace::new(&outward(ring, centre), SLOT_STONE, texel, false));
        }
        let mut block = |y0: i32, y1: i32, centre_y: f64| {
            if y1 <= y0 {
                return;
            }
            let centre = [0.0, centre_y, 0.0];
            let sides = [
                [[-w, y0, w], [w, y0, w], [w, y1, w], [-w, y1, w]],
                [[w, y0, -w], [-w, y0, -w], [-w, y1, -w], [w, y1, -w]],
                [[-w, y0, -w], [-w, y0, w], [-w, y1, w], [-w, y1, -w]],
                [[w, y0, w], [w, y0, -w], [w, y1, -w], [w, y1, w]],
            ];
            for ring in sides {
                faces.push(MeshFace::new(&outward(ring.to_vec(), centre), SLOT_TRIM, texel, false));
            }
        };
        block(0, lo, f64::from(lo) / 2.0);
        block(hi, self.height, f64::from(hi + self.height) / 2.0);
        // Plinth top and capital underside and top.
        let flat = |y: i32, up: bool| {
            let ring = vec![[-w, y, -w], [-w, y, w], [w, y, w], [w, y, -w]];
            let centre = [0.0, if up { f64::from(y) - 1.0 } else { f64::from(y) + 1.0 }, 0.0];
            MeshFace::new(&outward(ring, centre), SLOT_TRIM, texel, false)
        };
        if lo > 0 {
            faces.push(flat(lo, true));
        }
        if self.capital > 0 {
            faces.push(flat(hi, false));
            faces.push(flat(self.height, true));
        }
        let _ = axis;
        let mut proxies = Vec::new();
        if lo > 0 {
            proxies.push(Proxy::Box {
                min: [-w, 0, -w],
                max: [w, lo, w],
                top: false,
            });
        }
        proxies.push(Proxy::Box {
            min: [-self.radius, lo, -self.radius],
            max: [self.radius, hi, self.radius],
            top: self.capital == 0,
        });
        if self.capital > 0 {
            proxies.push(Proxy::Box {
                min: [-w, hi, -w],
                max: [w, self.height, w],
                top: true,
            });
        }
        Ok(BuiltMesh {
            faces,
            proxies,
            sockets: Vec::new(),
            slots: vec!["shaft".to_string(), "trim".to_string()],
        })
    }
}

/// The recipe of a scatter of stones, grid units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RubbleSpec {
    /// Diameter of the scatter.
    pub size: i32,
    /// Stones, 1 to 6.
    pub pieces: u8,
    /// Shape seed: the same seed always builds the same stones.
    pub seed: u16,
}

impl Default for RubbleSpec {
    fn default() -> Self {
        Self {
            size: 768,
            pieces: 4,
            seed: 1,
        }
    }
}

/// A small deterministic generator (xorshift32); no dependency, same on
/// every platform.
struct Rng(u32);

impl Rng {
    fn new(seed: u16) -> Self {
        Self(u32::from(seed).wrapping_mul(2_654_435_761) | 1)
    }

    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// A value in `[lo, hi]`.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u32) as i32
    }
}

impl RubbleSpec {
    /// Check the numbers.
    pub fn check(&self) -> Result<(), String> {
        if !(128..=4096).contains(&self.size) {
            return Err(format!("rubble size {} is outside 128..=4096", self.size));
        }
        if !(1..=6).contains(&self.pieces) {
            return Err("rubble pieces must be 1 to 6".to_string());
        }
        Ok(())
    }

    /// Build the stones (decoration only: no collision).
    pub fn build(&self) -> Result<BuiltMesh, String> {
        self.check()?;
        let mut rng = Rng::new(self.seed);
        let mut faces = Vec::new();
        let spread = self.size / 2;
        let n = usize::from(self.pieces);
        for piece in 0..n {
            // Spread the stones round a ring so they do not pile up.
            let around = TAU * piece as f64 / n as f64 + f64::from(rng.range(-20, 20)) / 100.0;
            let from_centre = if n == 1 { 0 } else { rng.range(spread / 3, spread * 3 / 4) };
            let cx = (f64::from(from_centre) * around.cos()).round() as i32;
            let cz = (f64::from(from_centre) * around.sin()).round() as i32;
            let radius = rng.range(self.size / 8, self.size / 4).max(48);
            let height = rng.range(radius * 4 / 10, radius * 8 / 10).max(32);
            let sides = 5usize;
            let mut base = Vec::new();
            let mut mid = Vec::new();
            for k in 0..sides {
                let a = TAU * k as f64 / sides as f64 + f64::from(rng.range(-25, 25)) / 100.0;
                let rb = f64::from(radius) * f64::from(rng.range(85, 115)) / 100.0;
                let rm = rb * f64::from(rng.range(65, 90)) / 100.0;
                let lift = rng.range(height * 35 / 100, height * 60 / 100);
                base.push([cx + (rb * a.cos()).round() as i32, 0, cz + (rb * a.sin()).round() as i32]);
                mid.push([cx + (rm * a.cos()).round() as i32, lift, cz + (rm * a.sin()).round() as i32]);
            }
            let apex = [cx + rng.range(-radius / 6, radius / 6), height, cz + rng.range(-radius / 6, radius / 6)];
            let centre = [f64::from(cx), f64::from(height) * 0.4, f64::from(cz)];
            for k in 0..sides {
                let j = (k + 1) % sides;
                faces.push(MeshFace::new(
                    &outward(vec![mid[k], mid[j], apex], centre),
                    SLOT_STONE,
                    texel,
                    false,
                ));
                faces.push(MeshFace::new(
                    &outward(vec![base[k], base[j], mid[j], mid[k]], centre),
                    SLOT_STONE,
                    texel,
                    false,
                ));
            }
        }
        Ok(BuiltMesh {
            faces,
            proxies: Vec::new(),
            sockets: Vec::new(),
            slots: vec!["stone".to_string()],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal_of(ring: &[[i32; 3]]) -> [f64; 3] {
        let a = [0, 1, 2].map(|i| f64::from(ring[1][i] - ring[0][i]));
        let b = [0, 1, 2].map(|i| f64::from(ring[2][i] - ring[0][i]));
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    #[test]
    fn a_column_faces_outward_everywhere() {
        let spec = ColumnSpec::default();
        let built = spec.build().unwrap();
        // 8 shaft sides, 4 plinth sides, 4 capital sides, plinth top, capital
        // underside and top.
        assert_eq!(built.faces.len(), 8 + 4 + 4 + 3);
        for face in &built.faces {
            let n = normal_of(face.ring());
            let mid: [f64; 3] = core::array::from_fn(|i| {
                face.ring().iter().map(|p| f64::from(p[i])).sum::<f64>() / face.ring().len() as f64
            });
            // Side faces point away from the axis, the flat ones up or down
            // away from the column's body.
            let away = if n[1].abs() > n[0].abs() + n[2].abs() {
                // Plinth top faces up, capital underside faces down.
                let y = f64::from(spec.height) / 2.0;
                [0.0, mid[1] - y, 0.0]
            } else {
                [mid[0], 0.0, mid[2]]
            };
            assert!(
                n[0] * away[0] + n[1] * away[1] + n[2] * away[2] > 0.0
                    || (mid[1] == f64::from(spec.plinth) && n[1] > 0.0)
                    || (mid[1] == f64::from(spec.height - spec.capital) && n[1] < 0.0),
                "{mid:?} {n:?}"
            );
        }
    }

    #[test]
    fn a_column_collides_as_stacked_boxes_with_a_standable_top() {
        let built = ColumnSpec::default().build().unwrap();
        assert_eq!(built.proxies.len(), 3);
        let tops = built
            .proxies
            .iter()
            .filter(|p| matches!(p, Proxy::Box { top: true, .. }))
            .count();
        assert_eq!(tops, 1);
        let no_capital = ColumnSpec {
            capital: 0,
            ..ColumnSpec::default()
        };
        assert!(no_capital
            .build()
            .unwrap()
            .proxies
            .iter()
            .any(|p| matches!(p, Proxy::Box { top: true, min, .. } if min[1] == 256)));
    }

    #[test]
    fn rubble_is_deterministic_and_seeds_differ() {
        let spec = RubbleSpec::default();
        assert_eq!(spec.build().unwrap(), spec.build().unwrap());
        let other = RubbleSpec { seed: 2, ..spec };
        assert_ne!(spec.build().unwrap().faces, other.build().unwrap().faces);
        assert_eq!(spec.build().unwrap().faces.len(), 4 * 10);
        assert!(spec.build().unwrap().proxies.is_empty());
    }

    #[test]
    fn rubble_sits_on_the_floor_and_inside_its_size() {
        let spec = RubbleSpec {
            size: 1024,
            pieces: 6,
            seed: 9,
        };
        let built = spec.build().unwrap();
        let mut lo = i32::MAX;
        for face in &built.faces {
            for p in face.ring() {
                lo = lo.min(p[1]);
                assert!(p[0].abs() <= 1024 && p[2].abs() <= 1024, "{p:?}");
            }
        }
        assert_eq!(lo, 0);
    }

    #[test]
    fn bad_numbers_are_refused() {
        assert!(ColumnSpec {
            sides: 2,
            ..ColumnSpec::default()
        }
        .build()
        .is_err());
        assert!(ColumnSpec {
            height: 400,
            ..ColumnSpec::default()
        }
        .build()
        .unwrap_err()
        .contains("shaft"));
        assert!(RubbleSpec {
            pieces: 0,
            ..RubbleSpec::default()
        }
        .build()
        .is_err());
    }
}
