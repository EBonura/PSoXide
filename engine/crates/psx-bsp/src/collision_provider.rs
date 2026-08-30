//! `psx-engine` collision-provider adapters for resident PXBSP hulls.

use crate::collision::{
    BrushTransform, CollisionHull, Trace, TraceFlag, TraceScratch, TransformedCollisionHull,
    Q12_ONE, TRACE_PLANE_EPSILON_Q12,
};
use crate::pxbsp_resident::PxbspResidentMap;
use crate::Vec3I32;
use psx_engine::{
    CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, CollisionTraceShape, RoomPoint,
};

/// Maximum transformed brush models composed into one production trace.
pub const MAX_COMPOSED_COLLISION_MODELS: usize = 32;

/// Number of upright body hulls carried by one PXBSP brush model.
///
/// `BrushModel::head_nodes` reserves one render head node, one point hull,
/// and exactly these two body hulls. Small and standard authored bodies may
/// therefore share the tighter cooked envelope; an oversized body uses the
/// second envelope.
pub const PXBSP_BODY_HULL_COUNT: usize = 2;

/// One caller-defined cooked hull envelope for an upright body.
///
/// PXBSP stores Quake-style hull head nodes but deliberately does not impose
/// game-specific hull dimensions. The cooker/runtime contract supplies those
/// dimensions through a fixed table and [`select_body_hull`] returns the first
/// envelope that fully contains the authored body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CookedBodyHull {
    /// Collision hull index in the PXBSP brush-model record.
    pub hull_index: usize,
    /// Maximum horizontal half-width supported by this hull.
    pub radius: i32,
    /// Maximum upright height supported by this hull.
    pub height: i32,
}

impl CookedBodyHull {
    /// Describe one fixed cooked body hull.
    pub const fn new(hull_index: usize, radius: i32, height: i32) -> Self {
        Self {
            hull_index,
            radius,
            height,
        }
    }
}

/// Select the tightest cooked hull envelope that fully contains an authored body.
///
/// Manifest order is not trusted. Invalid bodies, malformed tables, and bodies
/// larger than every cooked envelope return `None` rather than silently using
/// an undersized hull.
pub fn select_body_hull(hulls: &[CookedBodyHull], radius: i32, height: i32) -> Option<usize> {
    if radius < 0
        || height <= 0
        || hulls.is_empty()
        || hulls.iter().any(|hull| hull.radius < 0 || hull.height <= 0)
    {
        return None;
    }
    for (index, hull) in hulls.iter().enumerate() {
        if hulls[..index]
            .iter()
            .any(|earlier| earlier.hull_index == hull.hull_index)
        {
            return None;
        }
    }

    // Do not trust manifest order. Pick the tightest containing envelope by
    // a 32-bit footprint proxy, then stable dimensions and hull index. The
    // authored radii/heights are u16-backed, and saturating arithmetic keeps
    // this guest path deterministic at corrupted extremes.
    hulls
        .iter()
        .filter(|hull| radius <= hull.radius && height <= hull.height)
        .min_by_key(|hull| {
            (
                (hull.radius as u32).saturating_mul(hull.height as u32),
                hull.radius,
                hull.height,
                hull.hull_index,
            )
        })
        .map(|hull| hull.hull_index)
}

/// Validate the complete body-hull table embedded beside a PXBSP map.
///
/// The table must describe hulls one and two exactly once. Validation is a
/// fixed two-entry pass and allocates nothing.
pub fn valid_pxbsp_body_hulls(hulls: &[CookedBodyHull]) -> bool {
    hulls.len() == PXBSP_BODY_HULL_COUNT
        && hulls.iter().all(|hull| {
            (1..=PXBSP_BODY_HULL_COUNT).contains(&hull.hull_index)
                && hull.radius >= 0
                && hull.height > 0
        })
        && hulls[0].hull_index != hulls[1].hull_index
}

/// One transformed PXBSP submodel included in a world collision query.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PxbspCollisionModel {
    /// Index into [`PxbspResidentMap::brush_models`].
    pub model_index: u16,
    /// Current model-local to world transform.
    pub transform: BrushTransform,
}

impl PxbspCollisionModel {
    /// Construct one transformed submodel descriptor.
    pub const fn new(model_index: u16, transform: BrushTransform) -> Self {
        Self {
            model_index,
            transform,
        }
    }
}

/// Direct provider over already-resolved world and transformed-model hulls.
///
/// This is useful when the caller owns a fixed array of mover hulls. Most
/// resident-map users should prefer [`PxbspCollisionProvider`].
pub struct CollisionHullTraceProvider<'map, 'models, 'scratch> {
    world: CollisionHull<'map>,
    models: &'models [TransformedCollisionHull<'map>],
    supported_shape: CollisionTraceShape,
    scratch: &'scratch mut TraceScratch,
}

impl<'map, 'models, 'scratch> CollisionHullTraceProvider<'map, 'models, 'scratch> {
    /// Compose a static world hull with caller-owned transformed hulls.
    pub fn new(
        world: CollisionHull<'map>,
        models: &'models [TransformedCollisionHull<'map>],
        supported_shape: CollisionTraceShape,
        scratch: &'scratch mut TraceScratch,
    ) -> Option<Self> {
        if !valid_shape(supported_shape) || models.len() > MAX_COMPOSED_COLLISION_MODELS {
            return None;
        }
        Some(Self {
            world,
            models,
            supported_shape,
            scratch,
        })
    }
}

impl CollisionTraceProvider for CollisionHullTraceProvider<'_, '_, '_> {
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        if query.shape != self.supported_shape {
            return false;
        }
        let start = point_to_q12(query.start);
        let end = point_to_q12(query.end);
        let mut best = Trace::default();
        if !self.world.trace_into(&start, &end, self.scratch, &mut best) {
            return false;
        }
        for model in self.models.iter() {
            let mut candidate = Trace::default();
            if !model.trace_into(&start, &end, self.scratch, &mut candidate) {
                return false;
            }
            merge_trace(&mut best, candidate);
        }
        *output = trace_to_engine(best);
        true
    }
}

/// Resident-map provider composing model zero with transformed brush models.
///
/// The selected hull and supported engine shape are fixed at construction.
/// A mismatched query shape, malformed traversal, or scratch overflow returns
/// `false` without modifying the caller's output.
pub struct PxbspCollisionProvider<'map, 'models, 'scratch> {
    map: &'map PxbspResidentMap,
    world: CollisionHull<'map>,
    hull_index: usize,
    models: &'models [PxbspCollisionModel],
    supported_shape: CollisionTraceShape,
    scratch: &'scratch mut TraceScratch,
}

impl<'map, 'models, 'scratch> PxbspCollisionProvider<'map, 'models, 'scratch> {
    /// Build a bounded provider after validating every selected hull.
    pub fn new(
        map: &'map PxbspResidentMap,
        hull_index: usize,
        models: &'models [PxbspCollisionModel],
        supported_shape: CollisionTraceShape,
        scratch: &'scratch mut TraceScratch,
    ) -> Option<Self> {
        let world = map.model_collision_hull(0, hull_index)?;
        if !valid_shape(supported_shape)
            || models.len() > MAX_COMPOSED_COLLISION_MODELS
            || models.iter().any(|model| {
                map.model_collision_hull(model.model_index as usize, hull_index)
                    .is_none()
            })
        {
            return None;
        }
        Some(Self {
            map,
            world,
            hull_index,
            models,
            supported_shape,
            scratch,
        })
    }
}

impl CollisionTraceProvider for PxbspCollisionProvider<'_, '_, '_> {
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        if query.shape != self.supported_shape {
            return false;
        }
        let start = point_to_q12(query.start);
        let end = point_to_q12(query.end);
        let mut best = Trace::default();
        if !self.world.trace_into(&start, &end, self.scratch, &mut best) {
            return false;
        }
        for model in self.models.iter() {
            let Some(hull) = self
                .map
                .model_collision_hull(model.model_index as usize, self.hull_index)
            else {
                return false;
            };
            let mut candidate = Trace::default();
            if !hull.transformed(model.transform).trace_into(
                &start,
                &end,
                self.scratch,
                &mut candidate,
            ) {
                return false;
            }
            merge_trace(&mut best, candidate);
        }
        *output = trace_to_engine(best);
        true
    }
}

fn valid_shape(shape: CollisionTraceShape) -> bool {
    match shape {
        CollisionTraceShape::Point => true,
        CollisionTraceShape::Body { radius, height } => radius >= 0 && height > 0,
    }
}

fn point_to_q12(point: RoomPoint) -> Vec3I32 {
    Vec3I32 {
        x: point.x.saturating_mul(Q12_ONE),
        y: point.y.saturating_mul(Q12_ONE),
        z: point.z.saturating_mul(Q12_ONE),
    }
}

/// Quantise one Q12 axis to world units without landing inside the surface the
/// sweep stopped against.
///
/// `>> 12` floors and plain nearest-rounding both put the body inside geometry
/// whenever a contact lands on a fractional coordinate. Two real landings from
/// the cortex 0.3 map, body hull radius 12 height 64:
///
///   Q12 200311 = 48.90 -> floor 48 (solid), nearest 49 (free)
///   Q12 283326 = 69.17 -> floor 69 (solid), nearest 69 (solid), 70 is free
///
/// So the endpoint is pushed out along the contact normal instead: an axis the
/// surface faces along rounds away from that surface. The epsilon
/// keeps a clean contact on an integer plane where it is, because the traces
/// already back off a hair from the plane they hit and a bare ceiling would
/// promote an exact hit on x=2 to 3.

fn quantise_out_of_surface(value_q12: i32, normal_q12: i16) -> i32 {
    if normal_q12 > 0 {
        // The surface faces the positive axis, so the body must not end below
        // it: round up, less the epsilon that absorbs the trace back-off.
        let biased = value_q12.saturating_sub(TRACE_PLANE_EPSILON_Q12);
        -((-biased) >> 12)
    } else if normal_q12 < 0 {
        (value_q12.saturating_add(TRACE_PLANE_EPSILON_Q12)) >> 12
    } else {
        // No contact on this axis: nearest keeps free motion unbiased.
        const HALF: i32 = Q12_ONE / 2;
        if value_q12 >= 0 {
            value_q12.saturating_add(HALF) >> 12
        } else {
            -(value_q12.saturating_neg().saturating_add(HALF) >> 12)
        }
    }
}

fn point_from_q12(point: Vec3I32, normal: crate::Vec3I16) -> RoomPoint {
    RoomPoint::new(
        quantise_out_of_surface(point.x, normal.x),
        quantise_out_of_surface(point.y, normal.y),
        quantise_out_of_surface(point.z, normal.z),
    )
}

/// Cross from the byte-backed shared trace into the engine's boolean result.
///
/// This is where the flag bytes become `bool`s, so it is also where they are
/// normalized: [`psx_bsp::collision::TraceFlag::is_set`] maps any non-zero byte
/// to `true` rather than reinterpreting the byte as a `bool`.
fn trace_to_engine(trace: Trace) -> CollisionTrace {
    CollisionTrace {
        all_solid: trace.all_solid.is_set(),
        start_solid: trace.start_solid.is_set(),
        fraction_q12: trace.fraction,
        end: point_from_q12(trace.end, trace.normal),
        normal_q12: [trace.normal.x, trace.normal.y, trace.normal.z],
        plane_distance: trace.plane_distance >> 12,
    }
}

fn merge_trace(best: &mut Trace, candidate: Trace) {
    let start_solid = TraceFlag::new(best.start_solid.is_set() || candidate.start_solid.is_set());
    let all_solid = TraceFlag::new(best.all_solid.is_set() || candidate.all_solid.is_set());
    if candidate.fraction < best.fraction {
        *best = candidate;
    }
    best.start_solid = start_solid;
    best.all_solid = all_solid;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CookedRecord;
    use crate::{ClipNode, Plane, RecordSlice};
    use psx_engine::COLLISION_FRACTION_ONE_Q12;

    #[test]
    fn a_landing_never_quantises_into_the_surface_it_hit() {
        // Both measured on the shipping cortex 0.3 map with the player body
        // hull (index 1, radius 12, height 64), dropping onto a floor whose
        // contact normal points up.
        //
        // The body hull is inside the floor brush at the floored value in both
        // cases, and at the nearest value in the second. Standing there makes
        // every later trace return start_solid, so the motor cannot move the
        // character in any direction.
        const UP: i16 = 4090;

        // x=-65 z=-1030: rest at 48.90. floor 48 solid, 49 free.
        assert_eq!(quantise_out_of_surface(200311, UP), 49);
        assert_eq!(200311 >> 12, 48, "flooring landed inside solid");

        // x=-433 z=-1164: rest at 69.17. floor and nearest both 69 and solid,
        // 70 free. This is why nearest is not enough on its own.
        assert_eq!(quantise_out_of_surface(283326, UP), 70);
        assert_eq!(283326 >> 12, 69, "flooring landed inside solid");
        assert_eq!((283326 + 2048) >> 12, 69, "nearest also landed inside solid");

        // A clean contact on an integer plane stays put. The traces back off a
        // hair from the plane they hit, so a hit on x=2 arrives just past it
        // and a bare ceiling would promote it to 3.
        assert_eq!(
            quantise_out_of_surface(2 * Q12_ONE + TRACE_PLANE_EPSILON_Q12, 4096),
            2
        );
        assert_eq!(quantise_out_of_surface(2 * Q12_ONE, 4096), 2);

        // A surface facing the negative axis pushes the other way.
        assert_eq!(quantise_out_of_surface(-200311, -4096), -49);

        // No contact on an axis leaves free motion unbiased.
        assert_eq!(quantise_out_of_surface(200311, 0), 49);
        assert_eq!(
            quantise_out_of_surface(2 * Q12_ONE + TRACE_PLANE_EPSILON_Q12, 0),
            2
        );
        assert_eq!(quantise_out_of_surface(0, 0), 0);
    }

    #[test]
    fn body_hull_selection_is_ordered_bounded_and_fail_closed() {
        let hulls = [
            CookedBodyHull::new(1, 16, 56),
            CookedBodyHull::new(2, 32, 96),
        ];
        assert_eq!(select_body_hull(&hulls, 0, 1), Some(1));
        assert_eq!(select_body_hull(&hulls, 16, 56), Some(1));
        assert_eq!(select_body_hull(&hulls, 17, 56), Some(2));
        assert_eq!(select_body_hull(&hulls, 16, 57), Some(2));
        assert_eq!(select_body_hull(&hulls, 32, 96), Some(2));
        assert_eq!(select_body_hull(&hulls, -1, 56), None);
        assert_eq!(select_body_hull(&hulls, 16, 0), None);
        assert_eq!(select_body_hull(&hulls, 33, 1), None);
        assert_eq!(select_body_hull(&hulls, 1, 97), None);
    }

    #[test]
    fn body_hull_selection_is_order_independent_and_rejects_malformed_tables() {
        let reversed = [
            CookedBodyHull::new(2, 32, 96),
            CookedBodyHull::new(1, 16, 56),
        ];
        assert_eq!(select_body_hull(&reversed, 8, 32), Some(1));
        assert_eq!(select_body_hull(&reversed, 24, 72), Some(2));

        let hulls = [
            CookedBodyHull::new(7, -1, 56),
            CookedBodyHull::new(8, 16, 0),
            CookedBodyHull::new(9, 16, 56),
        ];
        assert_eq!(select_body_hull(&hulls, 16, 56), None);
        let duplicate = [
            CookedBodyHull::new(1, 16, 56),
            CookedBodyHull::new(1, 32, 96),
        ];
        assert_eq!(select_body_hull(&duplicate, 16, 56), None);
    }

    #[test]
    fn pxbsp_body_hull_table_is_exact_and_fixed_capacity() {
        let valid = [
            CookedBodyHull::new(2, 32, 96),
            CookedBodyHull::new(1, 16, 56),
        ];
        assert!(valid_pxbsp_body_hulls(&valid));
        assert!(!valid_pxbsp_body_hulls(&valid[..1]));
        assert!(!valid_pxbsp_body_hulls(&[
            valid[0],
            valid[1],
            CookedBodyHull::new(3, 64, 128),
        ]));
        assert!(!valid_pxbsp_body_hulls(&[
            CookedBodyHull::new(1, 16, 56),
            CookedBodyHull::new(1, 32, 96),
        ]));
    }

    fn plane_x(distance_units: i32) -> [u8; Plane::SIZE] {
        let mut bytes = [0u8; Plane::SIZE];
        bytes[0..2].copy_from_slice(&(Q12_ONE as i16).to_le_bytes());
        bytes[6..10].copy_from_slice(&distance_units.saturating_mul(Q12_ONE).to_le_bytes());
        bytes
    }

    fn node(plane: i16, front: i16, back: i16) -> [u8; 6] {
        let mut bytes = [0u8; 6];
        bytes[0..2].copy_from_slice(&plane.to_le_bytes());
        bytes[2..4].copy_from_slice(&front.to_le_bytes());
        bytes[4..6].copy_from_slice(&back.to_le_bytes());
        bytes
    }

    fn hull<'a>(planes: &'a [u8], nodes: &'a [u8]) -> CollisionHull<'a> {
        CollisionHull::new(
            RecordSlice::<Plane>::new(planes).expect("plane records"),
            RecordSlice::<ClipNode>::new(nodes).expect("clipnode records"),
            0,
        )
        .expect("aligned hull fixture")
    }

    #[test]
    fn transformed_door_wins_before_static_world_hit() {
        let planes = plane_x(0);
        let nodes = node(
            0,
            crate::collision::CONTENTS_EMPTY,
            crate::collision::CONTENTS_SOLID,
        );
        let world = hull(&planes, &nodes);
        let movers = [world.transformed(BrushTransform::translated(Vec3I32 {
            x: 2 * Q12_ONE,
            y: 0,
            z: 0,
        }))];
        let mut scratch = TraceScratch::new();
        let mut provider = CollisionHullTraceProvider::new(
            world,
            &movers,
            CollisionTraceShape::Point,
            &mut scratch,
        )
        .expect("provider");
        let mut output = CollisionTrace::default();
        assert!(provider.trace_into(
            CollisionTraceQuery::point(RoomPoint::new(4, 0, 0), RoomPoint::new(-4, 0, 0)),
            &mut output,
        ));
        assert!(output.fraction_q12 < COLLISION_FRACTION_ONE_Q12 / 2);
        assert_eq!(output.end.x, 2);
        assert_eq!(output.normal_q12, [Q12_ONE as i16, 0, 0]);
    }

    #[test]
    fn static_world_and_earlier_movers_retain_exact_fraction_ties() {
        let mut world = Trace {
            fraction: 1024,
            normal: crate::Vec3I16 { x: 1, y: 2, z: 3 },
            plane_distance: 11,
            ..Trace::default()
        };
        let tied_mover = Trace {
            fraction: 1024,
            normal: crate::Vec3I16 { x: 4, y: 5, z: 6 },
            plane_distance: 22,
            ..Trace::default()
        };
        merge_trace(&mut world, tied_mover);
        assert_eq!(world.normal, crate::Vec3I16 { x: 1, y: 2, z: 3 });
        assert_eq!(world.plane_distance, 11, "static world retains the tie");

        let earlier = Trace {
            fraction: 768,
            normal: crate::Vec3I16 { x: 7, y: 8, z: 9 },
            plane_distance: 33,
            ..Trace::default()
        };
        merge_trace(&mut world, earlier);
        let later_tie = Trace {
            fraction: 768,
            normal: crate::Vec3I16 {
                x: 10,
                y: 11,
                z: 12,
            },
            plane_distance: 44,
            ..Trace::default()
        };
        merge_trace(&mut world, later_tie);
        assert_eq!(world.normal, crate::Vec3I16 { x: 7, y: 8, z: 9 });
        assert_eq!(world.plane_distance, 33, "earlier mover retains the tie");
    }

    #[test]
    fn compound_failure_preserves_output_and_scratch_reuses_cleanly() {
        let planes = plane_x(0);
        let valid_nodes = node(
            0,
            crate::collision::CONTENTS_EMPTY,
            crate::collision::CONTENTS_SOLID,
        );
        let invalid_nodes = node(
            7,
            crate::collision::CONTENTS_EMPTY,
            crate::collision::CONTENTS_SOLID,
        );
        let world = hull(&planes, &valid_nodes);
        let invalid_movers =
            [
                hull(&planes, &invalid_nodes).transformed(BrushTransform::translated(Vec3I32 {
                    x: Q12_ONE,
                    y: 0,
                    z: 0,
                })),
            ];
        let query = CollisionTraceQuery::point(RoomPoint::new(4, 0, 0), RoomPoint::new(-4, 0, 0));
        let sentinel = CollisionTrace {
            all_solid: true,
            start_solid: true,
            fraction_q12: 123,
            end: RoomPoint::new(7, 8, 9),
            normal_q12: [10, 11, 12],
            plane_distance: 13,
        };
        let mut output = sentinel;
        let mut scratch = TraceScratch::new();
        {
            let mut failing = CollisionHullTraceProvider::new(
                world,
                &invalid_movers,
                CollisionTraceShape::Point,
                &mut scratch,
            )
            .expect("provider");
            assert!(!failing.trace_into(query, &mut output));
        }
        assert_eq!(output, sentinel);

        let valid_movers = [world.transformed(BrushTransform::translated(Vec3I32 {
            x: 2 * Q12_ONE,
            y: 0,
            z: 0,
        }))];
        let mut reused_output = CollisionTrace::default();
        {
            let mut reused = CollisionHullTraceProvider::new(
                world,
                &valid_movers,
                CollisionTraceShape::Point,
                &mut scratch,
            )
            .expect("provider");
            assert!(reused.trace_into(query, &mut reused_output));
        }
        let mut fresh_output = CollisionTrace::default();
        let mut fresh_scratch = TraceScratch::new();
        let mut fresh = CollisionHullTraceProvider::new(
            world,
            &valid_movers,
            CollisionTraceShape::Point,
            &mut fresh_scratch,
        )
        .expect("provider");
        assert!(fresh.trace_into(query, &mut fresh_output));
        assert_eq!(reused_output, fresh_output);
    }

    #[test]
    fn unsupported_shape_fails_without_modifying_output() {
        let planes = plane_x(0);
        let nodes = node(
            0,
            crate::collision::CONTENTS_EMPTY,
            crate::collision::CONTENTS_SOLID,
        );
        let mut scratch = TraceScratch::new();
        let mut provider = CollisionHullTraceProvider::new(
            hull(&planes, &nodes),
            &[],
            CollisionTraceShape::Point,
            &mut scratch,
        )
        .expect("provider");
        let sentinel = CollisionTrace {
            fraction_q12: 17,
            ..CollisionTrace::default()
        };
        let mut output = sentinel;
        assert!(!provider.trace_into(
            CollisionTraceQuery::body(RoomPoint::ZERO, RoomPoint::new(1, 0, 0), 16, 56),
            &mut output,
        ));
        assert_eq!(output, sentinel);
    }
}
