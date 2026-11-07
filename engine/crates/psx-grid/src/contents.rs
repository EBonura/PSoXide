//! Liquid contents codes shared by the grid collision provider and the
//! playtest's hazard logic.

/// Open air.
pub const CONTENTS_EMPTY: i16 = -1;
/// Water.
pub const CONTENTS_WATER: i16 = -3;
/// Slime.
pub const CONTENTS_SLIME: i16 = -4;
/// Lava.
pub const CONTENTS_LAVA: i16 = -5;

/// Contents code of a liquid kind (`format::liquid_kind`), or
/// [`CONTENTS_EMPTY`] for any other value.
pub const fn contents_of_liquid_kind(kind: u8) -> i16 {
    match kind {
        crate::liquid_kind::WATER => CONTENTS_WATER,
        crate::liquid_kind::SLIME => CONTENTS_SLIME,
        crate::liquid_kind::LAVA => CONTENTS_LAVA,
        _ => CONTENTS_EMPTY,
    }
}

/// Result of sampling an ordered feet-to-head point sequence.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LiquidContentsSample {
    /// Strongest sampled liquid code: lava, slime, water, then empty.
    pub contents: i16,
    /// One-based index of the highest sample in any liquid, or zero.
    pub water_level: u8,
}

impl Default for LiquidContentsSample {
    fn default() -> Self {
        Self {
            contents: CONTENTS_EMPTY,
            water_level: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_kinds_map_to_distinct_codes_ordered_by_severity() {
        assert_eq!(contents_of_liquid_kind(0), CONTENTS_EMPTY);
        assert!(
            contents_of_liquid_kind(crate::liquid_kind::LAVA)
                < contents_of_liquid_kind(crate::liquid_kind::SLIME)
        );
        assert!(
            contents_of_liquid_kind(crate::liquid_kind::SLIME)
                < contents_of_liquid_kind(crate::liquid_kind::WATER)
        );
        assert!(contents_of_liquid_kind(crate::liquid_kind::WATER) < CONTENTS_EMPTY);
    }
}
