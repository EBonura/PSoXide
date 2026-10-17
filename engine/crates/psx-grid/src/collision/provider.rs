//! `psx_engine::CollisionTraceProvider` over the grid world.

use super::world::{GridCollision, COORD_LIMIT};
use psx_engine::{
    CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, CollisionTraceShape,
};

/// Trace provider over a [`GridCollision`].
///
/// Bodies need `radius <= S / 2`; larger radii are clamped to `S` so a trace
/// stays bounded rather than failing. Queries with a coordinate beyond
/// [`COORD_LIMIT`] return `false` and leave the output untouched.
pub struct GridTraceProvider<'a, 'b> {
    world: &'b GridCollision<'a>,
}

impl<'a, 'b> GridTraceProvider<'a, 'b> {
    pub fn new(world: &'b GridCollision<'a>) -> Self {
        Self { world }
    }
}

impl CollisionTraceProvider for GridTraceProvider<'_, '_> {
    #[inline(never)]
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        let a = query.start.to_array();
        let b = query.end.to_array();
        if a.iter().chain(b.iter()).any(|c| c.abs() > COORD_LIMIT) {
            return false;
        }
        let (radius, height, body) = match query.shape {
            CollisionTraceShape::Point => (0, 0, false),
            CollisionTraceShape::Body { radius, height } => (radius.max(0), height.max(0), true),
        };
        *output = self.world.trace(a, b, radius, height, body);
        true
    }
}
