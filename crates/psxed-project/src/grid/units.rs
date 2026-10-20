//! Grid units and the scale that maps them to editor units.

use serde::{Deserialize, Serialize};

/// Grid units in one sector edge.
pub const SECTOR_UNITS: i32 = 1024;
/// Grid units in one height click, the default vertical snap.
pub const CLICK_UNITS: i32 = 256;
/// Grid units in the fine snap (held with Alt in the editor).
pub const FINE_UNITS: i32 = 64;
/// Grid units per texel of the original 32 texels per sector mapping. The
/// generated mesh recipes (arches, columns, rubble) measure their own
/// texture coordinates against it.
pub const UNITS_PER_TEXEL: i32 = SECTOR_UNITS / SectorTexels::LEGACY.get() as i32;

/// Texels the default world-aligned mapping spends on one sector edge, a
/// setting of the world. Floors, ceilings and walls all use it, so adjacent
/// faces tile.
///
/// The grid renderer tiles 64 by 64 textures, so a new world spends 64: one
/// whole texture per sector. Worlds made before the setting existed spent
/// 32 (half a texture per sector), which is what a stored world without the
/// field reads as and keeps, so no existing face changes its texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SectorTexels(u16);

impl SectorTexels {
    /// New worlds: one 64 texel texture per sector.
    pub const DEFAULT: Self = Self(64);
    /// What a world stored without the setting means.
    pub const LEGACY: Self = Self(32);

    /// A density, or `None` for zero (a sector with no texels).
    pub const fn new(texels: u16) -> Option<Self> {
        if texels > 0 {
            Some(Self(texels))
        } else {
            None
        }
    }

    /// The texel count, at least 1 so damaged data cannot collapse the
    /// mapping.
    pub const fn get(self) -> u16 {
        if self.0 == 0 {
            1
        } else {
            self.0
        }
    }

    /// Whether this is the pre-setting density (the document then omits it).
    pub fn is_legacy(&self) -> bool {
        *self == Self::LEGACY
    }

    /// What a stored world without the setting reads as.
    pub const fn legacy() -> Self {
        Self::LEGACY
    }
}

impl Default for SectorTexels {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Rational scale from grid units to editor units: `editor = grid * num / den`.
///
/// New worlds use 5/4, which makes a sector 1280 editor units (80 runtime
/// units) so every snap, 256 click and 64 fine step alike, lands on a whole
/// runtime unit, and the 1024 unit Cortex character stands 0.8 of a sector
/// tall. Earlier worlds used 21/16 (1344 editor units, 84 runtime units per
/// sector, a fractional fine snap); a stored world keeps whatever scale it was
/// saved with. The bonnie importer picks the scale from the level's own
/// player height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridScale {
    /// Numerator.
    pub num: i32,
    /// Denominator, positive.
    pub den: i32,
}

impl GridScale {
    /// Default scale for new worlds.
    pub const DEFAULT: Self = Self { num: 5, den: 4 };

    /// A scale, or `None` unless both terms are positive.
    pub const fn new(num: i32, den: i32) -> Option<Self> {
        if num > 0 && den > 0 {
            Some(Self { num, den })
        } else {
            None
        }
    }

    /// Whether both terms are positive.
    pub const fn is_valid(self) -> bool {
        self.num > 0 && self.den > 0
    }

    /// Grid units to editor units, rounded to nearest with ties away from
    /// zero so the mapping is symmetric around the origin.
    pub fn to_editor(self, grid: i32) -> i32 {
        let num = i64::from(grid) * i64::from(self.num);
        let den = i64::from(self.den.max(1));
        let half = den / 2;
        let rounded = if num >= 0 {
            (num + half) / den
        } else {
            -((-num + half) / den)
        };
        rounded.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }

    /// Editor units to grid units, rounded the same way.
    pub fn to_grid(self, editor: i32) -> i32 {
        let num = i64::from(editor) * i64::from(self.den.max(1));
        let den = i64::from(self.num.max(1));
        let half = den / 2;
        let rounded = if num >= 0 {
            (num + half) / den
        } else {
            -((-num + half) / den)
        };
        rounded.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }

    /// Editor units in one sector edge.
    pub fn sector_editor_units(self) -> i32 {
        self.to_editor(SECTOR_UNITS)
    }
}

impl Default for GridScale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Snap `value` to the nearest multiple of `step` (positive), ties away from
/// zero.
pub fn snap_to(value: i32, step: i32) -> i32 {
    let step = step.max(1);
    let half = step / 2;
    if value >= 0 {
        ((value + half) / step) * step
    } else {
        -(((-value + half) / step) * step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scale_gives_1280_unit_sectors() {
        assert_eq!(GridScale::DEFAULT.sector_editor_units(), 1280);
        assert_eq!(GridScale::DEFAULT.to_editor(CLICK_UNITS), 320);
        assert_eq!(GridScale::DEFAULT.to_editor(FINE_UNITS), 80);
        // Every snap is a whole number of runtime units (editor / 16).
        for step in [CLICK_UNITS, FINE_UNITS] {
            assert_eq!(GridScale::DEFAULT.to_editor(step) % 16, 0);
        }
    }

    #[test]
    fn the_21_16_scale_stays_supported() {
        let scale = GridScale { num: 21, den: 16 };
        assert_eq!(scale.sector_editor_units(), 1344);
        assert_eq!(scale.to_editor(CLICK_UNITS), 336);
        assert_eq!(scale.to_editor(FINE_UNITS), 84);
    }

    #[test]
    fn rounding_is_symmetric_around_zero() {
        let scale = GridScale { num: 3, den: 2 };
        assert_eq!(scale.to_editor(1), 2);
        assert_eq!(scale.to_editor(-1), -2);
        assert_eq!(scale.to_editor(3), 5);
        assert_eq!(scale.to_editor(-3), -5);
    }

    #[test]
    fn grid_editor_round_trip_on_the_sixteen_unit_lattice() {
        for scale in [
            GridScale { num: 9, den: 16 },
            GridScale { num: 21, den: 16 },
            GridScale { num: 7, den: 8 },
        ] {
            for grid in (-4096..=4096).step_by(16) {
                assert_eq!(
                    scale.to_grid(scale.to_editor(grid)),
                    grid,
                    "{scale:?} {grid}"
                );
            }
        }
    }

    #[test]
    fn invalid_scales_are_rejected() {
        assert!(GridScale::new(0, 16).is_none());
        assert!(GridScale::new(21, 0).is_none());
        assert!(GridScale::new(-1, 16).is_none());
        assert!(!GridScale { num: 0, den: 1 }.is_valid());
    }

    #[test]
    fn snap_rounds_half_away_from_zero() {
        assert_eq!(snap_to(127, 256), 0);
        assert_eq!(snap_to(128, 256), 256);
        assert_eq!(snap_to(-128, 256), -256);
        assert_eq!(snap_to(-127, 256), 0);
    }
}
