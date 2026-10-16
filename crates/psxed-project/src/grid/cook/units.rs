//! Grid units to runtime units.
//!
//! One function maps every grid coordinate the cook emits, so the vertices
//! the renderer draws, the heights collision stands on and the light bake's
//! points all round identically:
//!
//! ```text
//! rt(v) = round_half_away_from_zero(v * num / (16 * den))
//! ```
//!
//! The scale must make the sector edge a whole number of runtime units
//! (`64 * num` divisible by `den`), so editor sector units are exactly
//! `16 * S` and editor and runtime agree on every sector boundary.

use crate::grid::units::{GridScale, SECTOR_UNITS};

/// Smallest and largest sector edge in runtime units. The upper bound keeps a
/// local offset inside a sector in a `u8`.
pub const SECTOR_RANGE: (u32, u32) = (2, 255);

/// The grid to runtime mapping of one world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeUnits {
    num: i64,
    den: i64,
    sector: u32,
}

impl RuntimeUnits {
    /// The mapping for `scale`, or why the scale cannot be cooked.
    pub fn new(scale: GridScale) -> Result<Self, String> {
        if !scale.is_valid() {
            return Err(format!(
                "scale {}/{} must be positive",
                scale.num, scale.den
            ));
        }
        let (num, den) = (i64::from(scale.num), i64::from(scale.den));
        let sector = i64::from(SECTOR_UNITS) * num;
        if sector % (16 * den) != 0 {
            return Err(format!(
                "scale {}/{} makes a sector {} editor units; it must be a whole multiple of 16 so sectors fall on the runtime lattice (use 5/4, 21/16, 9/16 or 7/8)",
                scale.num,
                scale.den,
                scale.sector_editor_units()
            ));
        }
        let sector = (sector / (16 * den)) as u32;
        if sector < SECTOR_RANGE.0 || sector > SECTOR_RANGE.1 {
            return Err(format!(
                "scale {}/{} gives {sector} runtime units per sector; it must be {} to {}",
                scale.num, scale.den, SECTOR_RANGE.0, SECTOR_RANGE.1
            ));
        }
        Ok(Self { num, den, sector })
    }

    /// Runtime units per sector edge (S).
    pub fn sector(&self) -> u32 {
        self.sector
    }

    /// Grid length or coordinate to runtime units, rounded half away from
    /// zero so the mapping is symmetric around the origin.
    pub fn rt(&self, grid: i32) -> i32 {
        let n = i64::from(grid) * self.num;
        let d = 16 * self.den;
        let half = d / 2;
        let r = if n >= 0 {
            (n + half) / d
        } else {
            -((-n + half) / d)
        };
        r.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_scales_give_whole_sectors() {
        for (num, den, s) in [(5, 4, 80), (21, 16, 84), (9, 16, 36), (7, 8, 56)] {
            let units = RuntimeUnits::new(GridScale { num, den }).unwrap();
            assert_eq!(units.sector(), s, "{num}/{den}");
            // Sector boundaries land exactly on multiples of S.
            for k in -4..=4 {
                assert_eq!(units.rt(k * SECTOR_UNITS), k * s as i32);
            }
        }
    }

    #[test]
    fn off_lattice_and_oversize_scales_are_refused_with_a_reason() {
        let off = RuntimeUnits::new(GridScale { num: 5, den: 3 }).unwrap_err();
        assert!(off.contains("multiple of 16"), "{off}");
        let big = RuntimeUnits::new(GridScale { num: 8, den: 1 }).unwrap_err();
        assert!(big.contains("runtime units per sector"), "{big}");
        assert!(RuntimeUnits::new(GridScale { num: 0, den: 1 }).is_err());
    }

    #[test]
    fn rounding_is_symmetric() {
        let units = RuntimeUnits::new(GridScale { num: 5, den: 4 }).unwrap();
        // 64 grid units are exactly 5 runtime units.
        assert_eq!(units.rt(64), 5);
        assert_eq!(units.rt(-64), -5);
        // 13 grid units are 1.01 runtime units; 6 are 0.47.
        assert_eq!(units.rt(13), 1);
        assert_eq!(units.rt(6), 0);
        assert_eq!(units.rt(-13), -1);
    }
}
