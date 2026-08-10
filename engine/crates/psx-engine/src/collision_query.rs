//! Allocation-free collision trace contract shared by gameplay systems.
//!
//! The engine owns the coordinate- and failure-semantics contract while world
//! implementations own their traversal scratch. This keeps character and
//! camera code independent of any one world format; in particular,
//! `psx-engine` never needs to depend on `psx-bsp`.

use crate::RoomPoint;

/// Q0.12 fraction representing the complete requested trace segment.
pub const COLLISION_FRACTION_ONE_Q12: i32 = 4096;

/// Collision volume requested by one engine trace.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CollisionTraceShape {
    /// Infinitesimal point trace, used by camera spring arms and probes.
    Point,
    /// Upright body rooted at its bottom centre.
    ///
    /// BSP implementations may conservatively back this with a cooked box
    /// hull whose horizontal half-width is `radius`.
    Body {
        /// Horizontal half-width/radius in engine world units.
        radius: i32,
        /// Height above the bottom-centre origin in engine world units.
        height: i32,
    },
}

/// One trace request in engine world units.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CollisionTraceQuery {
    /// Inclusive segment start.
    pub start: RoomPoint,
    /// Inclusive segment end.
    pub end: RoomPoint,
    /// Collision volume swept along the segment.
    pub shape: CollisionTraceShape,
}

impl CollisionTraceQuery {
    /// Construct a point trace.
    pub const fn point(start: RoomPoint, end: RoomPoint) -> Self {
        Self {
            start,
            end,
            shape: CollisionTraceShape::Point,
        }
    }

    /// Construct an upright body trace.
    pub const fn body(start: RoomPoint, end: RoomPoint, radius: i32, height: i32) -> Self {
        Self {
            start,
            end,
            shape: CollisionTraceShape::Body { radius, height },
        }
    }
}

/// Format-independent result of one collision trace.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CollisionTrace {
    /// The complete segment remained inside solid contents.
    pub all_solid: bool,
    /// The trace began inside solid contents.
    pub start_solid: bool,
    /// Q0.12 distance along the requested segment, in `0..=4096`.
    pub fraction_q12: i32,
    /// End position in engine world units.
    pub end: RoomPoint,
    /// Contact-plane normal in signed Q3.12 units.
    pub normal_q12: [i16; 3],
    /// Contact-plane distance in engine world units.
    pub plane_distance: i32,
}

impl CollisionTrace {
    /// Exact clear result for a requested endpoint.
    pub const fn unobstructed(end: RoomPoint) -> Self {
        Self {
            all_solid: false,
            start_solid: false,
            fraction_q12: COLLISION_FRACTION_ONE_Q12,
            end,
            normal_q12: [0; 3],
            plane_distance: 0,
        }
    }

    /// True when the provider reported contact before the requested endpoint.
    pub const fn hit(self) -> bool {
        self.start_solid || self.fraction_q12 < COLLISION_FRACTION_ONE_Q12
    }
}

/// Provider failure reported to stateful engine callers.
///
/// A failure denotes malformed/unavailable world data, unsupported trace
/// shape, or exhausted caller-owned traversal scratch. It is deliberately not
/// treated as either a hit or a clear path.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CollisionQueryError;

/// Allocation-free segment-trace provider.
///
/// Implementations own or borrow any required fixed scratch. `false` must
/// leave `output` byte-for-byte unchanged so a failed compound query can be
/// retried safely and stateful motor/camera callers can roll back cleanly.
pub trait CollisionTraceProvider {
    /// Trace one request into caller-owned output.
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool;
}

/// Run one provider trace while preserving an explicit failure channel.
pub fn trace_collision<P: CollisionTraceProvider + ?Sized>(
    provider: &mut P,
    query: CollisionTraceQuery,
) -> Result<CollisionTrace, CollisionQueryError> {
    let mut output = CollisionTrace::unobstructed(query.end);
    if provider.trace_into(query, &mut output) {
        Ok(output)
    } else {
        Err(CollisionQueryError)
    }
}
