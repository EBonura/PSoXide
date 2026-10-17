//! `psx_engine::CollisionTraceProvider` over the grid world.

use super::body::{Query, Raw, NO_Y};
use super::scan::nc;
use super::world::{GridCollision, COORD_LIMIT};
use psx_engine::{
    CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, CollisionTraceShape, RoomPoint,
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

impl<'a> GridCollision<'a> {
    /// Expand a raw result into the engine's trace.
    #[inline(never)]
    fn finish(&self, q: &Query, raw: &Raw, output: &mut CollisionTrace) {
        output.all_solid = raw.all_solid;
        output.start_solid = raw.start_solid;
        output.fraction_q12 = raw.fraction;
        output.end = RoomPoint::from_array(if raw.end_y != NO_Y {
            [q.a[0], raw.end_y, q.a[2]]
        } else if raw.fraction == 4096 {
            // The whole segment: no interpolation.
            [q.a[0] + q.d[0], q.a[1] + q.d[1], q.a[2] + q.d[2]]
        } else {
            q.at(raw.fraction)
        });
        output.normal_q12 = match raw.normal {
            nc::NONE => [0; 3],
            nc::UP => [0, 4096, 0],
            nc::FLOOR => self.floor_normal(
                usize::from(raw.src.room),
                usize::from(raw.src.cx),
                usize::from(raw.src.cz),
                raw.src.aux != 0,
            ),
            code => axis_normal(code),
        };
        output.plane_distance = 0;
    }
}

/// Q12 vector of a non-floor normal code.
#[inline(never)]
fn axis_normal(code: u8) -> [i16; 3] {
    const D: i16 = 2896;
    match code {
        nc::DOWN => [0, -4096, 0],
        nc::PX => [4096, 0, 0],
        nc::NX => [-4096, 0, 0],
        nc::PZ => [0, 0, 4096],
        nc::NZ => [0, 0, -4096],
        nc::PP => [D, 0, D],
        nc::PN => [D, 0, -D],
        nc::NP => [-D, 0, D],
        nc::NN => [-D, 0, -D],
        _ => [0; 3],
    }
}

impl CollisionTraceProvider for GridTraceProvider<'_, '_> {
    #[inline(never)]
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        let a = query.start.to_array();
        let b = query.end.to_array();
        if a.iter()
            .chain(b.iter())
            .any(|&c| (c.wrapping_add(COORD_LIMIT) as u32) > 2 * COORD_LIMIT as u32)
        {
            return false;
        }
        let world = self.world;
        let (r, h, body) = match query.shape {
            CollisionTraceShape::Point => (0, 0, false),
            CollisionTraceShape::Body { radius, height } => {
                (radius.max(0).min(world.s), height.max(0), true)
            }
        };
        let q = Query {
            a,
            d: [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            r: r as i16,
            h: h.min(30_000) as i16,
            body,
        };
        let mut raw = Raw::new();
        world.trace(&q, &mut raw);
        if raw.contact_hi != 0 {
            world.contact(&q, raw.fraction, i32::from(raw.contact_hi), &mut raw);
        }
        world.finish(&q, &raw, output);
        true
    }
}
