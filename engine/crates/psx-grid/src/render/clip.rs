//! Polygon clipping against one frustum plane.

use psx_engine::{lerp_vertex, AffineVertex};
use psx_math::attributed_clip::{
    clip_to_plane_unchecked, crossing_fraction_q16_i32, AttributedClipPlane, ClipTraversal,
};

use super::frustum::FrustumPlanes;

pub(super) struct ClipPlane<'a>(pub(super) &'a ([i32; 3], i32));

impl AttributedClipPlane<AffineVertex> for ClipPlane<'_> {
    type Distance = i32;

    #[inline(always)]
    fn distance(&self, _: usize, vertex: &AffineVertex) -> Self::Distance {
        FrustumPlanes::distance(self.0, vertex.position)
    }

    #[inline(always)]
    fn inside(&self, distance: Self::Distance) -> bool {
        distance >= 0
    }

    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &AffineVertex,
        first_distance: Self::Distance,
        _: usize,
        second: &AffineVertex,
        second_distance: Self::Distance,
    ) -> AffineVertex {
        // Two faces share an edge but traverse it in opposite directions; a
        // fixed endpoint order gives both the same crossing.
        let (first, first_distance, second, second_distance) = if first.position <= second.position
        {
            (first, first_distance, second, second_distance)
        } else {
            (second, second_distance, first, first_distance)
        };
        let fraction = crossing_fraction_q16_i32(first_distance, second_distance);
        lerp_vertex(first, second, fraction)
    }
}

#[inline(never)]
pub fn clip_polygon_plane(
    plane: &([i32; 3], i32),
    source: &[AffineVertex],
    destination: &mut [AffineVertex],
) -> usize {
    // SAFETY: `clip_polygon` documents room for `count + 5` records in each
    // buffer, and the source here is a convex polygon of at most `count`
    // vertices clipped by one plane whose `inside` answer depends only on the
    // vertex distance, so it gains at most one vertex: `source.len() + 1`
    // slots suffice.
    unsafe {
        clip_to_plane_unchecked(
            source,
            destination,
            &ClipPlane(plane),
            ClipTraversal::PreviousToCurrent,
        )
    }
}
