//! Slope classes of floors, read the way the character motor reads them.
//!
//! Heights go through the cook's own grid to runtime rounding and the cook's
//! own triangle normal ([`psx_grid::height::triangle_normal_q12`]), then are
//! compared with the shared [`psx_engine::WalkLimits`], so what the editor
//! calls walkable is what the motor will stand on.

use psx_engine::WalkLimits;
use psx_grid::height::triangle_normal_q12;

use super::cook::collision::local_height;
use super::cook::units::RuntimeUnits;
use super::surface::{GridSplit, GridSurface};

/// How a floor slopes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlopeClass {
    /// Level: every corner at one height.
    Flat,
    /// Tilted but steady enough to stand on.
    Slope,
    /// Steeper than the motor lets a body stand.
    Steep,
}

impl SlopeClass {
    /// Index into per-class tables: flat, slope, steep.
    pub const fn index(self) -> usize {
        match self {
            Self::Flat => 0,
            Self::Slope => 1,
            Self::Steep => 2,
        }
    }

    /// Lower-case label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Slope => "slope",
            Self::Steep => "steep",
        }
    }

    /// Every class, in index order.
    pub const ALL: [Self; 3] = [Self::Flat, Self::Slope, Self::Steep];
}

fn rt_corners(units: &RuntimeUnits, y_offset: i32, heights: [i32; 4]) -> [i16; 4] {
    heights.map(|h| {
        local_height(units, y_offset, h).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    })
}

/// Upward normal component, Q12, of triangle `0` or `1` of `surface`, from
/// runtime-rounded heights. 4096 is level.
pub fn triangle_up_q12(
    units: &RuntimeUnits,
    y_offset: i32,
    surface: &GridSurface,
    triangle: usize,
) -> i16 {
    let h = rt_corners(units, y_offset, surface.heights_for(triangle));
    let nesw = matches!(surface.split, GridSplit::NeSw);
    triangle_normal_q12(h, nesw, triangle == 1, units.sector())[1]
}

/// The class of one triangle under `limits`.
pub fn triangle_class(
    units: &RuntimeUnits,
    y_offset: i32,
    surface: &GridSurface,
    triangle: usize,
    limits: &WalkLimits,
) -> SlopeClass {
    let h = rt_corners(units, y_offset, surface.heights_for(triangle));
    if h.iter().all(|v| *v == h[0]) {
        return SlopeClass::Flat;
    }
    if triangle_up_q12(units, y_offset, surface, triangle) >= limits.min_floor_normal_y_q12 {
        SlopeClass::Slope
    } else {
        SlopeClass::Steep
    }
}

/// The class of a whole surface: the worse of its two triangles.
pub fn surface_class(
    units: &RuntimeUnits,
    y_offset: i32,
    surface: &GridSurface,
    limits: &WalkLimits,
) -> SlopeClass {
    let a = triangle_class(units, y_offset, surface, 0, limits);
    let b = triangle_class(units, y_offset, surface, 1, limits);
    a.max(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::units::GridScale;

    fn units() -> RuntimeUnits {
        RuntimeUnits::new(GridScale::DEFAULT).unwrap()
    }

    fn ramp(rise: i32) -> GridSurface {
        let mut s = GridSurface::flat(0, None);
        s.heights = [0, 0, rise, rise];
        s
    }

    #[test]
    fn level_tilted_and_steep_floors_are_told_apart() {
        let limits = WalkLimits::DEFAULT;
        let u = units();
        assert_eq!(
            surface_class(&u, 0, &GridSurface::flat(512, None), &limits),
            SlopeClass::Flat
        );
        // 512 grid units is 40 runtime units over an 80 unit sector: a 0.5 slope.
        assert_eq!(surface_class(&u, 0, &ramp(512), &limits), SlopeClass::Slope);
        // 2048 grid units is 160 runtime units: far too steep.
        assert_eq!(
            surface_class(&u, 0, &ramp(2048), &limits),
            SlopeClass::Steep
        );
    }

    #[test]
    fn the_limit_is_the_motors_normal() {
        let limits = WalkLimits::DEFAULT;
        let u = units();
        // 80 runtime units of rise over an 80 unit sector is 45 degrees, a
        // normal of 0.707 (2896 in Q12), which stands; 85 does not (0.685).
        let rise = |ru: i32| ru * 64 / 5;
        assert_eq!(
            surface_class(&u, 0, &ramp(rise(80)), &limits),
            SlopeClass::Slope
        );
        assert_eq!(
            surface_class(&u, 0, &ramp(rise(85)), &limits),
            SlopeClass::Steep
        );
    }
}
