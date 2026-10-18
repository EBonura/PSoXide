//! Rough: render-only vertex jitter and noise tint, the rock look.
//!
//! A straight wall is the one thing the grid cannot avoid, so a room can
//! displace the corners of its faces in plan by a small, deterministic
//! amount. The displacement is a function of the plan position alone (the
//! lattice point, hashed with the room's seed), never of the height, so every
//! face that meets at a lattice point moves with it and the surface stays
//! watertight: a vertical edge shifts rigidly, and a point in the middle of an
//! edge (a band cut, a texture split) takes the linear interpolation of the
//! two ends. Collision is cooked from the document, not from the faces, so it
//! does not move: the visible rock is up to `amp` away from what the body
//! stops against, which is why the amplitudes are capped well under the
//! character's radius (see [`MAX_AMP_RU`]).
//!
//! Which sectors are rough is a level (0 to 3) per sector; the offset at a
//! lattice point uses the highest level among the up to four sectors that
//! touch it. The preview and the cook both build faces through
//! [`super::faces`], so they place every corner identically.

use serde::{Deserialize, Serialize};

use super::room::GridRoom;
use super::units::GridScale;

/// Highest rough level of a sector.
pub const MAX_LEVEL: u8 = 3;

/// The largest amplitude, in runtime units, the validator accepts without a
/// warning: a third of the default character radius (12), under one fine step
/// of the grid. Inward jitter bigger than the clearance between the model and
/// its collision radius clips the character into the rock; outward jitter
/// makes an invisible wall of the same size.
pub const MAX_AMP_RU: i32 = 4;

/// The rough settings of one room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rough {
    /// Seed of the jitter and of the tint noise: a different value makes a
    /// different rock with the same amplitude.
    #[serde(default)]
    pub seed: u16,
    /// Largest plan offset at levels 1, 2 and 3, grid units either way along
    /// each axis. All zero means no jitter.
    #[serde(default)]
    pub amp: [u8; 3],
    /// Brightness noise on the faces of rough sectors, percent either way.
    #[serde(default)]
    pub tint: u8,
}

impl Rough {
    /// Whether every field is at its default, for `skip_serializing_if`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The amplitudes for levels 1, 2 and 3 at `scale`: 1, 2 and 4 runtime
    /// units (the last is [`MAX_AMP_RU`], a third of the character's radius),
    /// in grid units rounded to the nearest.
    pub fn default_amp(scale: GridScale) -> [u8; 3] {
        // One runtime unit is 16 editor units, an editor unit is den/num
        // grid units.
        let ru_gu = |ru: i64| -> u8 {
            let num = i64::from(scale.num.max(1));
            let den = i64::from(scale.den.max(1));
            ((ru * 16 * den + num / 2) / num).clamp(0, 255) as u8
        };
        [ru_gu(1), ru_gu(2), ru_gu(MAX_AMP_RU.into())]
    }

    /// The amplitude of `level` (1 to 3), 0 for no level.
    pub fn level_amp(&self, level: u8) -> i32 {
        match level {
            1..=3 => i32::from(self.amp[usize::from(level) - 1]),
            _ => 0,
        }
    }

    /// Whether the room draws any jitter at all.
    pub fn jitters(&self) -> bool {
        self.amp != [0; 3]
    }
}

fn mix(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

fn hash(seed: u16, a: i32, b: i32, salt: u32) -> u32 {
    let mut h = mix(u32::from(seed) ^ 0x517c_c1b7);
    h = mix(h ^ (a as u32).wrapping_mul(0x9e37_79b1));
    h = mix(h ^ (b as u32).wrapping_mul(0x85eb_ca77));
    mix(h ^ salt.wrapping_mul(0xc2b2_ae3d))
}

/// A signed number in `-amp..=amp` from the hash `h`.
fn spread(h: u32, amp: i32) -> i32 {
    if amp <= 0 {
        0
    } else {
        (h % (2 * amp as u32 + 1)) as i32 - amp
    }
}

/// The jitter of one room, for the face builder.
#[derive(Clone, Copy)]
pub struct Jitter<'a> {
    room: &'a GridRoom,
    rough: Rough,
}

impl<'a> Jitter<'a> {
    /// The jitter of `room`, or `None` when it has none.
    pub fn new(room: &'a GridRoom) -> Option<Self> {
        room.rough.jitters().then_some(Self {
            room,
            rough: room.rough,
        })
    }

    /// The rough level at lattice point `[vx, vz]` (in sectors): the highest
    /// among the sectors that touch it.
    pub fn level_at(&self, vx: i32, vz: i32) -> u8 {
        let mut level = 0;
        for (dx, dz) in [(-1, -1), (0, -1), (-1, 0), (0, 0)] {
            let sector = self
                .room
                .local([vx + dx, vz + dz])
                .and_then(|[x, z]| self.room.sector(x, z));
            level = level.max(sector.map_or(0, |s| s.rough & 3));
        }
        level
    }

    /// The plan offset `[dx, dz]`, grid units, of lattice point `[vx, vz]`.
    pub fn at(&self, vx: i32, vz: i32) -> [i32; 2] {
        let amp = self.rough.level_amp(self.level_at(vx, vz));
        if amp == 0 {
            return [0, 0];
        }
        [
            spread(hash(self.rough.seed, vx, vz, 1), amp),
            spread(hash(self.rough.seed, vx, vz, 2), amp),
        ]
    }

    /// The offset of the point `num / den` of the way from grid point `a` to
    /// grid point `b` along a segment between two lattice points (a wall, a
    /// diagonal): the two ends' offsets interpolated, rounded to the nearest.
    pub fn along(&self, a: [i32; 2], b: [i32; 2], num: i64, den: i64) -> [i32; 2] {
        let lattice = |p: [i32; 2]| {
            [
                p[0].div_euclid(super::SECTOR_UNITS),
                p[1].div_euclid(super::SECTOR_UNITS),
            ]
        };
        let (la, lb) = (lattice(a), lattice(b));
        let (da, db) = (self.at(la[0], la[1]), self.at(lb[0], lb[1]));
        let den = den.max(1);
        let lerp = |x: i32, y: i32| -> i32 {
            let d = i64::from(y) - i64::from(x);
            let n = d * num;
            let r = if n >= 0 {
                (n + den / 2) / den
            } else {
                -((-n + den / 2) / den)
            };
            x + r as i32
        };
        [lerp(da[0], db[0]), lerp(da[1], db[1])]
    }
}

/// The brightness factor, Q8 (256 is unchanged), of the noise tint at a
/// plan position `[x, z]` in grid units: a hash of the 64 unit cell the
/// position falls in, `pct` percent either way.
pub fn tint_q8(seed: u16, pct: u8, x: i32, z: i32) -> i32 {
    let h = hash(seed, x.div_euclid(64), z.div_euclid(64), 3);
    let n = (h % 2001) as i32 - 1000;
    256 + n * i32::from(pct.min(100)) * 256 / 100 / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::GridSurface;

    fn room(level: u8) -> GridRoom {
        let mut room = GridRoom::new_derived("r", 3, 3);
        for z in 0..3 {
            for x in 0..3 {
                let s = room.sector_or_insert(x, z).unwrap();
                s.set_floor(Some(GridSurface::flat(0, None)));
                s.rough = level;
            }
        }
        room.rough = Rough {
            seed: 7,
            amp: [10, 20, 40],
            tint: 0,
        };
        room
    }

    #[test]
    fn a_room_without_amplitude_or_levels_has_no_jitter() {
        let mut r = room(2);
        r.rough.amp = [0; 3];
        assert!(Jitter::new(&r).is_none());
        let r = room(0);
        let j = Jitter::new(&r).unwrap();
        for v in 0..4 {
            assert_eq!(j.at(v, v), [0, 0]);
        }
    }

    #[test]
    fn the_offset_is_a_pure_function_of_the_lattice_point_and_bounded_by_the_level() {
        for level in 1..=3u8 {
            let r = room(level);
            let j = Jitter::new(&r).unwrap();
            let amp = r.rough.level_amp(level);
            for vz in 0..=3 {
                for vx in 0..=3 {
                    let d = j.at(vx, vz);
                    assert_eq!(d, j.at(vx, vz));
                    assert!(d.iter().all(|v| v.abs() <= amp), "{d:?} > {amp}");
                }
            }
        }
        // The offset reaches the cap somewhere: it is not stuck at zero.
        let r = room(3);
        let j = Jitter::new(&r).unwrap();
        let max = (0..=3)
            .flat_map(|z| (0..=3).map(move |x| (x, z)))
            .flat_map(|(x, z)| j.at(x, z))
            .map(i32::abs)
            .max()
            .unwrap();
        assert!(max > 20, "{max}");
    }

    #[test]
    fn a_lattice_point_takes_the_highest_level_around_it() {
        let mut r = room(0);
        r.sector_mut(1, 1).unwrap().rough = 3;
        let j = Jitter::new(&r).unwrap();
        // The four corners of the rough sector use level 3, the far corner
        // of the room none.
        assert_eq!(j.level_at(1, 1), 3);
        assert_eq!(j.level_at(2, 2), 3);
        assert_eq!(j.level_at(0, 0), 0);
        assert_eq!(j.at(0, 0), [0, 0]);
    }

    #[test]
    fn a_point_along_an_edge_interpolates_its_ends_exactly_at_the_ends() {
        let r = room(3);
        let j = Jitter::new(&r).unwrap();
        let (a, b) = ([1024, 1024], [2048, 1024]);
        assert_eq!(j.along(a, b, 0, 1024), j.at(1, 1));
        assert_eq!(j.along(a, b, 1024, 1024), j.at(2, 1));
        let mid = j.along(a, b, 512, 1024);
        let (p, q) = (j.at(1, 1), j.at(2, 1));
        for i in 0..2 {
            let want = (p[i] + q[i]) as f64 / 2.0;
            assert!(
                (f64::from(mid[i]) - want).abs() <= 0.5 + 1e-9,
                "{mid:?} {p:?} {q:?}"
            );
        }
    }

    #[test]
    fn default_amplitudes_are_one_two_and_four_runtime_units() {
        // 5/4: one runtime unit is 16 editor units, 12.8 grid units.
        assert_eq!(
            Rough::default_amp(GridScale { num: 5, den: 4 }),
            [13, 26, 51]
        );
        assert!(Rough::default().is_default());
        let tint = tint_q8(3, 50, 100, 200);
        assert!((128..=384).contains(&tint));
        assert_eq!(tint_q8(3, 0, 100, 200), 256);
    }
}
