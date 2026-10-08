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
