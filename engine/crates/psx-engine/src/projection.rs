// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared PS1 projection scheduling and screen-space classification.
//!
//! The GTE wrappers are deliberately always-inlined: HL-PSX proved that an
//! ordinary shared call in the quad projection path loses to the R3000A's
//! direct-mapped instruction cache. Keeping the schedule here gives every
//! renderer one source contract without adding a call or dynamic dispatch.

pub use psx_gte::scene::{project_triangle_scheduled, project_vertex_scheduled};

/// Inclusive screen-space clip bounds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScreenClipBounds {
    /// Inclusive left edge.
    pub min_x: i32,
    /// Inclusive right edge.
    pub max_x: i32,
    /// Inclusive top edge.
    pub min_y: i32,
    /// Inclusive bottom edge.
    pub max_y: i32,
}

impl ScreenClipBounds {
    /// Construct inclusive integer screen bounds.
    pub const fn new(min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> Self {
        Self {
            min_x,
            max_x,
            min_y,
            max_y,
        }
    }
}

/// Point lies left of the clip bounds.
pub const OUT_LEFT: u8 = 1 << 0;
/// Point lies right of the clip bounds.
pub const OUT_RIGHT: u8 = 1 << 1;
/// Point lies above the clip bounds.
pub const OUT_TOP: u8 = 1 << 2;
/// Point lies below the clip bounds.
pub const OUT_BOTTOM: u8 = 1 << 3;

/// Four-bit Cohen-Sutherland code for an integer screen position.
#[inline(always)]
pub fn screen_outcode(position: [i32; 2], bounds: ScreenClipBounds) -> u8 {
    let x = position[0];
    let y = position[1];
    (((x < bounds.min_x) as u8) * OUT_LEFT)
        | (((x > bounds.max_x) as u8) * OUT_RIGHT)
        | (((y < bounds.min_y) as u8) * OUT_TOP)
        | (((y > bounds.max_y) as u8) * OUT_BOTTOM)
}

/// Branch-minimal outcode for the common zero-origin viewport.
///
/// This retains the exact arithmetic previously embedded in the classic
/// affine packet writer, including its fast all-inside return.
#[inline(always)]
pub fn zero_origin_screen_outcode(position: [i16; 2], right: i32, bottom: i32) -> u8 {
    let x = position[0] as i32;
    let y = position[1] as i32;
    if (x as u32) <= right as u32 && (y as u32) <= bottom as u32 {
        return 0;
    }
    ((x as u32 >> 31) as u8)
        | ((((right - x) as u32 >> 31) as u8) << 1)
        | (((y as u32 >> 31) as u8) << 2)
        | ((((bottom - y) as u32 >> 31) as u8) << 3)
}

/// True when three points all lie outside at least one common half-space.
#[inline(always)]
pub fn triangle_outside_common_plane(points: [[i32; 2]; 3], bounds: ScreenClipBounds) -> bool {
    let common = screen_outcode(points[0], bounds);
    if common == 0 {
        return false;
    }
    let common = common & screen_outcode(points[1], bounds);
    common != 0 && (common & screen_outcode(points[2], bounds)) != 0
}

/// Classic-affine triangle screen rejection.
///
/// Preserve the established pairwise rule byte-for-byte rather than silently
/// replacing it with a common-bit test; its packet topology is part of the
/// three-game visual-equivalence contract.
#[inline(always)]
pub fn classic_triangle_screen_rejected(points: [[i16; 2]; 3], right: i32, bottom: i32) -> bool {
    let c0 = zero_origin_screen_outcode(points[0], right, bottom);
    if c0 == 0 {
        return false;
    }
    let c1 = zero_origin_screen_outcode(points[1], right, bottom);
    if (c0 & c1) == 0 {
        return false;
    }
    let c2 = zero_origin_screen_outcode(points[2], right, bottom);
    (c1 & c2) != 0 && (c2 & c0) != 0
}

/// Classic-affine quad screen rejection with the established six pair tests.
#[inline(always)]
pub fn classic_quad_screen_rejected(points: [[i16; 2]; 4], right: i32, bottom: i32) -> bool {
    let c0 = zero_origin_screen_outcode(points[0], right, bottom);
    if c0 == 0 {
        return false;
    }
    let c1 = zero_origin_screen_outcode(points[1], right, bottom);
    if (c0 & c1) == 0 {
        return false;
    }
    let c2 = zero_origin_screen_outcode(points[2], right, bottom);
    if (c0 & c2) == 0 || (c1 & c2) == 0 {
        return false;
    }
    let c3 = zero_origin_screen_outcode(points[3], right, bottom);
    (c2 & c3) != 0 && (c3 & c0) != 0 && (c1 & c3) != 0
}

/// A cheap sufficient proof that a set of vertices fits GPU polygon extents.
///
/// The fixed rectangle [-352, 671] x [-136, 375] includes a 320x240 viewport
/// and spans exactly 1023 x 511 pixels. OR the codes of every vertex: zero
/// proves that every triangle formed from them fits those hardware limits.
/// A nonzero code requires the caller's ordinary exact extent test; it never
/// authorizes rejection or a change in tessellation.
#[inline(always)]
pub fn gpu_extent_box_code(screen: [i16; 2]) -> u32 {
    (((screen[0] as i32 + 352) as u32) & !1023) | (((screen[1] as i32 + 136) as u32) & !511)
}

/// Pack five signed half-space distances into an outcode.
///
/// Renderers choose the planes and their order. This helper only standardises
/// the sign-to-bit conversion used before selective clipping.
#[inline(always)]
pub fn half_space_outcode5(distances: [i32; 5]) -> u8 {
    ((distances[0] < 0) as u8)
        | (((distances[1] < 0) as u8) << 1)
        | (((distances[2] < 0) as u8) << 2)
        | (((distances[3] < 0) as u8) << 3)
        | (((distances[4] < 0) as u8) << 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_rejection_matches_all_historical_corner_combinations() {
        // Every side/corner code, inclusive edges, and the inside region.
        let positions = [
            [-1, -1],
            [0, -1],
            [320, -1],
            [-1, 0],
            [0, 0],
            [320, 0],
            [-1, 240],
            [0, 240],
            [320, 240],
            [319, 239],
        ];
        for a in positions {
            for b in positions {
                for c in positions {
                    let codes = [a, b, c].map(|p| zero_origin_screen_outcode(p, 319, 239));
                    let [x, y, z] = codes;
                    assert_eq!(
                        classic_triangle_screen_rejected([a, b, c], 319, 239),
                        (x & y) != 0 && (y & z) != 0 && (z & x) != 0
                    );
                    assert_eq!(
                        triangle_outside_common_plane(
                            [a, b, c].map(|p| [p[0] as i32, p[1] as i32]),
                            ScreenClipBounds::new(0, 319, 0, 239)
                        ),
                        (x & y & z) != 0
                    );
                    for d in positions {
                        let w = zero_origin_screen_outcode(d, 319, 239);
                        assert_eq!(
                            classic_quad_screen_rejected([a, b, c, d], 319, 239),
                            (x & y) != 0
                                && (y & z) != 0
                                && (z & w) != 0
                                && (w & x) != 0
                                && (x & z) != 0
                                && (y & w) != 0
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn extent_box_proof_covers_exactly_the_safe_rectangle() {
        for coordinate in i16::MIN..=i16::MAX {
            assert_eq!(
                gpu_extent_box_code([coordinate, 0]) == 0,
                (-352..=671).contains(&coordinate)
            );
            assert_eq!(
                gpu_extent_box_code([0, coordinate]) == 0,
                (-136..=375).contains(&coordinate)
            );
        }
    }

    #[test]
    fn outcode_uses_inclusive_edges() {
        let bounds = ScreenClipBounds::new(-8, 8, -4, 4);
        assert_eq!(screen_outcode([-8, 4], bounds), 0);
        assert_eq!(screen_outcode([-9, 5], bounds), OUT_LEFT | OUT_BOTTOM);
        assert_eq!(screen_outcode([9, -5], bounds), OUT_RIGHT | OUT_TOP);
    }

    #[test]
    fn common_plane_rejection_keeps_crossing_triangle() {
        let bounds = ScreenClipBounds::new(0, 319, 0, 239);
        assert!(triangle_outside_common_plane(
            [[-3, 20], [-2, 100], [-1, 200]],
            bounds
        ));
        assert!(!triangle_outside_common_plane(
            [[-3, 20], [160, 100], [330, 200]],
            bounds
        ));
    }

    #[test]
    fn half_space_bits_keep_authored_order() {
        assert_eq!(half_space_outcode5([0, -1, 2, -3, -4]), 0b1_1010);
    }
}
