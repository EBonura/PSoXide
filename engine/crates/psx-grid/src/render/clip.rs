//! Polygon clipping against one frustum plane.

use psx_engine::attributed_clip::{
    clip_to_plane_unchecked, crossing_fraction_q16_i32, lerp_q16_i32_exact, AttributedClipPlane,
    ClipTraversal,
};
use psx_engine::AffineVertex;

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

fn lerp_vertex(a: &AffineVertex, b: &AffineVertex, fraction_q16: u32) -> AffineVertex {
    let lerp_i = |x: i32, y: i32| -> i32 { lerp_q16_i32_exact(x, y, fraction_q16) };
    let lerp_u8 = |x: u8, y: u8| -> u8 { lerp_i(x as i32, y as i32).clamp(0, 255) as u8 };
    let ca = a.color;
    let cb = b.color;
    let color = (lerp_u8(ca as u8, cb as u8) as u32)
        | ((lerp_u8((ca >> 8) as u8, (cb >> 8) as u8) as u32) << 8)
        | ((lerp_u8((ca >> 16) as u8, (cb >> 16) as u8) as u32) << 16)
        | (ca & 0xff00_0000);
    AffineVertex {
        position: [
            lerp_i(a.position[0] as i32, b.position[0] as i32).clamp(-32768, 32767) as i16,
            lerp_i(a.position[1] as i32, b.position[1] as i32).clamp(-32768, 32767) as i16,
            lerp_i(a.position[2] as i32, b.position[2] as i32).clamp(-32768, 32767) as i16,
        ],
        uv: [lerp_u8(a.uv[0], b.uv[0]), lerp_u8(a.uv[1], b.uv[1])],
        color,
        screen: [0, 0],
        depth: 0,
    }
}
