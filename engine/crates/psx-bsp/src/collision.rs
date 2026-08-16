//! Caller-owned PXBSP collision-hull traversal.
//!
//! Derived from quake-psx `crates/quake-core` (commit 9e20a1b, same GPL-2
//! authorship). This is PSoXide's canonical allocation-free implementation.
//! PXBSP positions are Y-up Q20.12 and plane normals are Q3.12.

use crate::{ClipNode, Plane, RecordSlice, Vec3I16, Vec3I32};
use psx_engine::div_q12_i32;
use psx_gte::math::Mat3I16;
use psx_math::int32::{mul_q12_i32, mul_q12_i32_wide};

pub const CONTENTS_EMPTY: i16 = -1;
pub const CONTENTS_SOLID: i16 = -2;
pub const CONTENTS_WATER: i16 = -3;
pub const CONTENTS_SLIME: i16 = -4;
pub const CONTENTS_LAVA: i16 = -5;
pub const CONTENTS_SKY: i16 = -6;
pub const Q12_ONE: i32 = 4096;
pub const TRACE_PLANE_EPSILON_Q12: i32 = 128;
pub const TRACE_STACK_CAPACITY: usize = 64;

/// Result of sampling an ordered feet-to-head point sequence through a hull.
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

/// Rigid local-to-world transform shared by brush rendering and collision.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrushTransform {
    /// Q20.12 world position of model-local zero.
    pub origin: Vec3I32,
    /// Q3.12 model-local to world rotation.
    pub rotation: Mat3I16,
}

impl BrushTransform {
    pub const IDENTITY: Self = Self {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        rotation: Mat3I16::IDENTITY,
    };

    pub const fn translated(origin: Vec3I32) -> Self {
        Self {
            origin,
            rotation: Mat3I16::IDENTITY,
        }
    }

    /// Transform one Q20.12 world point into model-local coordinates.
    pub fn point_to_local(self, point: Vec3I32) -> Vec3I32 {
        inverse_rotate_q12(
            self.rotation,
            Vec3I32 {
                x: point.x.saturating_sub(self.origin.x),
                y: point.y.saturating_sub(self.origin.y),
                z: point.z.saturating_sub(self.origin.z),
            },
        )
    }
}

/// One byte-backed flag slot in a caller-owned [`Trace`].
///
/// [`CollisionHull::trace_into`] guarantees that a failed trace leaves every
/// output byte, padding included, exactly as the caller left it. The caller
/// therefore hands this boundary whatever bytes its storage already held, and
/// those bytes are legal by construction. A `bool` field would make any byte
/// other than 0 or 1 undefined behaviour the instant the struct was formed or
/// copied, before a single branch on it ran, and no later read could repair
/// that: the invalid value would already exist.
///
/// Every one of the 256 byte patterns is a valid `TraceFlag`, so the invalid
/// value cannot be created. [`TraceFlag::is_set`] is the only way to ask what a
/// slot means, and it normalizes any non-zero byte to `true` at the point a
/// `bool` is first constructed.
///
/// Equality is byte equality, not meaning equality, because the byte-preserving
/// contract is what callers assert against. The tracer itself only ever writes
/// [`TraceFlag::CLEAR`] or [`TraceFlag::SET`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct TraceFlag(u8);

impl TraceFlag {
    /// The flag is not set. This is also the [`Default`].
    pub const CLEAR: Self = Self(0);
    /// The flag is set, in the canonical byte the tracer writes.
    pub const SET: Self = Self(1);

    /// The flag a Rust `bool` denotes.
    pub const fn new(value: bool) -> Self {
        if value {
            Self::SET
        } else {
            Self::CLEAR
        }
    }

    /// Adopt one arbitrary byte. Every byte is a valid flag.
    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    /// The stored byte, unnormalized. Useful only for byte-level assertions.
    pub const fn byte(self) -> u8 {
        self.0
    }

    /// Normalize the stored byte into a valid `bool`.
    pub const fn is_set(self) -> bool {
        self.0 != 0
    }
}

impl From<bool> for TraceFlag {
    fn from(value: bool) -> Self {
        Self::new(value)
    }
}

impl From<TraceFlag> for bool {
    fn from(flag: TraceFlag) -> Self {
        flag.is_set()
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct Trace {
    pub all_solid: TraceFlag,
    pub start_solid: TraceFlag,
    pub in_open: TraceFlag,
    pub in_water: TraceFlag,
    pub fraction: i32,
    pub end: Vec3I32,
    pub normal: Vec3I16,
    pub plane_distance: i32,
}

#[derive(Copy, Clone)]
struct TraceContinuation {
    far_child: i16,
    plane_index: i16,
    side: u8,
    middle_fraction: i32,
    end_fraction: i32,
    middle: Vec3I32,
    end: Vec3I32,
}

impl TraceContinuation {
    const EMPTY: Self = Self {
        far_child: 0,
        plane_index: 0,
        side: 0,
        middle_fraction: 0,
        end_fraction: 0,
        middle: Vec3I32 { x: 0, y: 0, z: 0 },
        end: Vec3I32 { x: 0, y: 0, z: 0 },
    };
}

/// Caller-owned workspace for one allocation-free BSP hull trace.
///
/// The fixed stack stores at most [`TRACE_STACK_CAPACITY`] pending far-side
/// traversals. A trace that needs one more entry returns `false`; the scratch
/// remains reusable and the caller's output is not modified.
pub struct TraceScratch {
    continuations: [TraceContinuation; TRACE_STACK_CAPACITY],
}

impl TraceScratch {
    pub const fn new() -> Self {
        Self {
            continuations: [TraceContinuation::EMPTY; TRACE_STACK_CAPACITY],
        }
    }
}

impl Default for TraceScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Trace {
    const fn unobstructed(end: Vec3I32) -> Self {
        Self {
            all_solid: TraceFlag::SET,
            start_solid: TraceFlag::CLEAR,
            in_open: TraceFlag::CLEAR,
            in_water: TraceFlag::CLEAR,
            fraction: Q12_ONE,
            end,
            normal: Vec3I16 { x: 0, y: 0, z: 0 },
            plane_distance: 0,
        }
    }
}

#[derive(Copy, Clone)]
enum PlaneRecords<'a> {
    Packed(RecordSlice<'a, Plane>),
    Decoded(&'a [Plane]),
}

impl PlaneRecords<'_> {
    #[inline(always)]
    fn get(self, index: usize) -> Option<Plane> {
        match self {
            Self::Packed(records) => records.get(index),
            Self::Decoded(records) => records.get(index).copied(),
        }
    }
}

#[derive(Copy, Clone)]
enum ClipNodeRecords<'a> {
    Packed(RecordSlice<'a, ClipNode>),
    Decoded(&'a [ClipNode]),
}

impl ClipNodeRecords<'_> {
    #[inline(always)]
    fn len(self) -> usize {
        match self {
            Self::Packed(records) => records.len(),
            Self::Decoded(records) => records.len(),
        }
    }

    #[inline(always)]
    fn get(self, index: usize) -> Option<ClipNode> {
        match self {
            Self::Packed(records) => records.get(index),
            Self::Decoded(records) => records.get(index).copied(),
        }
    }
}

#[derive(Copy, Clone)]
pub struct CollisionHull<'a> {
    planes: PlaneRecords<'a>,
    nodes: ClipNodeRecords<'a>,
    head_node: i16,
}

impl<'a> CollisionHull<'a> {
    pub const fn new(
        planes: RecordSlice<'a, Plane>,
        nodes: RecordSlice<'a, ClipNode>,
        head_node: i16,
    ) -> Self {
        Self {
            planes: PlaneRecords::Packed(planes),
            nodes: ClipNodeRecords::Packed(nodes),
            head_node,
        }
    }

    /// Construct a collision hull over records decoded once by the map owner.
    /// This removes packed-record decoding from every traversal step while
    /// retaining the same validated collision implementation.
    pub const fn new_decoded(planes: &'a [Plane], nodes: &'a [ClipNode], head_node: i16) -> Self {
        Self {
            planes: PlaneRecords::Decoded(planes),
            nodes: ClipNodeRecords::Decoded(nodes),
            head_node,
        }
    }

    pub fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.point_contents_from(self.head_node, &point)
    }

    /// Sample an ordered feet-to-head point sequence and return Quake-style
    /// liquid depth plus the strongest encountered hazard. Unknown contents,
    /// sky, empty, and solid do not count as liquid. Any malformed tree fails
    /// the whole query instead of returning a partial classification.
    pub fn sample_liquid_contents(&self, points: &[Vec3I32]) -> Option<LiquidContentsSample> {
        let mut sample = LiquidContentsSample::default();
        let mut precedence = 0u8;
        for (index, point) in points.iter().copied().enumerate() {
            let contents = self.point_contents(point)?;
            let candidate = liquid_precedence(contents);
            if candidate == 0 {
                break;
            }
            sample.water_level = (index + 1).min(u8::MAX as usize) as u8;
            if candidate > precedence {
                precedence = candidate;
                sample.contents = contents;
            }
        }
        Some(sample)
    }

    /// Trace a Q20.12 point segment through this Y-up PXBSP hull.
    ///
    /// The output position and plane distance are Q20.12. The output normal
    /// is Q3.12. Traversal uses only deterministic `i32` fixed-point math and
    /// a plane epsilon of 128 in Q20.12. `false` reports malformed BSP data or
    /// scratch overflow and leaves `output` byte-for-byte unchanged.
    ///
    /// Byte-for-byte means exactly that, so a caller's flag slot may hold any
    /// of the 256 byte patterns across a failed call. [`TraceFlag`] is what
    /// makes those bytes legal values rather than undefined behaviour; see
    /// there.
    pub fn trace_into(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        let mut trace = Trace::unobstructed(*end);
        let mut continuation_count = 0usize;
        let mut node_index = self.head_node;
        let mut start_fraction: i32 = 0;
        let mut end_fraction: i32 = Q12_ONE;
        let mut segment_start = *start;
        let mut segment_end = *end;

        loop {
            let mut descent_budget = self.nodes.len();
            while node_index >= 0 {
                if descent_budget == 0 {
                    return false;
                }
                descent_budget -= 1;
                let Some(node) = self.nodes.get(node_index as usize) else {
                    return false;
                };
                let Some(plane) = self.planes.get(node.plane as usize) else {
                    return false;
                };
                let start_distance = plane_distance(plane, segment_start);
                let end_distance = plane_distance(plane, segment_end);

                if start_distance >= 0 && end_distance >= 0 {
                    node_index = node.children[0];
                    continue;
                }
                if start_distance < 0 && end_distance < 0 {
                    node_index = node.children[1];
                    continue;
                }

                let numerator = if start_distance < 0 {
                    start_distance.saturating_add(TRACE_PLANE_EPSILON_Q12)
                } else {
                    start_distance.saturating_sub(TRACE_PLANE_EPSILON_Q12)
                };
                let fraction = div_q12_i32(numerator, start_distance.saturating_sub(end_distance))
                    .clamp(0, Q12_ONE);
                let middle_fraction = start_fraction.saturating_add(mul_q12_i32(
                    end_fraction.saturating_sub(start_fraction),
                    fraction,
                ));
                let middle = interpolate(segment_start, segment_end, fraction);
                let side = usize::from(start_distance < 0);
                if continuation_count == TRACE_STACK_CAPACITY {
                    return false;
                }
                scratch.continuations[continuation_count] = TraceContinuation {
                    far_child: node.children[side ^ 1],
                    plane_index: node.plane,
                    side: side as u8,
                    middle_fraction,
                    end_fraction,
                    middle,
                    end: segment_end,
                };
                continuation_count += 1;
                node_index = node.children[side];
                end_fraction = middle_fraction;
                segment_end = middle;
            }

            if node_index != CONTENTS_SOLID {
                trace.all_solid = TraceFlag::CLEAR;
                if node_index == CONTENTS_EMPTY {
                    trace.in_open = TraceFlag::SET;
                } else {
                    trace.in_water = TraceFlag::SET;
                }
            } else {
                trace.start_solid = TraceFlag::SET;
            }

            if continuation_count == 0 {
                *output = trace;
                return true;
            }
            continuation_count -= 1;
            let continuation = scratch.continuations[continuation_count];
            let Some(far_contents) =
                self.point_contents_from(continuation.far_child, &continuation.middle)
            else {
                return false;
            };
            if far_contents != CONTENTS_SOLID {
                node_index = continuation.far_child;
                start_fraction = continuation.middle_fraction;
                end_fraction = continuation.end_fraction;
                segment_start = continuation.middle;
                segment_end = continuation.end;
                continue;
            }
            if trace.all_solid.is_set() {
                *output = trace;
                return true;
            }

            let Some(plane) = self.planes.get(continuation.plane_index as usize) else {
                return false;
            };
            if continuation.side == 0 {
                trace.normal = plane.normal;
                trace.plane_distance = plane.distance;
            } else {
                trace.normal = Vec3I16 {
                    x: plane.normal.x.saturating_neg(),
                    y: plane.normal.y.saturating_neg(),
                    z: plane.normal.z.saturating_neg(),
                };
                trace.plane_distance = plane.distance.saturating_neg();
            }
            trace.fraction = continuation.middle_fraction;
            trace.end = continuation.middle;
            *output = trace;
            return true;
        }
    }

    /// Apply one mover transform while retaining this model-local hull.
    pub const fn transformed(self, transform: BrushTransform) -> TransformedCollisionHull<'a> {
        TransformedCollisionHull {
            local: self,
            transform,
        }
    }

    fn point_contents_from(&self, mut node_index: i16, point: &Vec3I32) -> Option<i16> {
        let mut descent_budget = self.nodes.len();
        while node_index >= 0 {
            if descent_budget == 0 {
                return None;
            }
            descent_budget -= 1;
            let node = self.nodes.get(node_index as usize)?;
            let plane = self.planes.get(node.plane as usize)?;
            node_index = node.children[(plane_distance(plane, *point) < 0) as usize];
        }
        Some(node_index)
    }
}

/// World-space query facade over one model-local clipnode hull.
#[derive(Copy, Clone)]
pub struct TransformedCollisionHull<'a> {
    local: CollisionHull<'a>,
    transform: BrushTransform,
}

impl TransformedCollisionHull<'_> {
    pub fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.local
            .point_contents(self.transform.point_to_local(point))
    }

    /// Trace a world-space segment through this transformed model hull.
    ///
    /// Failure and output-preservation semantics match
    /// [`CollisionHull::trace_into`].
    pub fn trace_into(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        let local_start = self.transform.point_to_local(*start);
        let local_end = self.transform.point_to_local(*end);
        let mut trace = Trace::default();
        if !self
            .local
            .trace_into(&local_start, &local_end, scratch, &mut trace)
        {
            return false;
        }
        trace.end = interpolate(*start, *end, trace.fraction);
        trace.normal = rotate_normal(self.transform.rotation, trace.normal);
        trace.plane_distance = trace
            .plane_distance
            .saturating_add(normal_dot_point(trace.normal, self.transform.origin));
        *output = trace;
        true
    }
}

/// Signed Q20.12 distance of `point` from `plane`.
///
/// Q20.12 points against Q3.12 unit normals keep every product and the
/// three-term sum inside `i32`, so the wide multiply and wrapping adds are
/// exactly the saturating form's result on validated hulls, at a fraction of
/// its instruction count (this runs twice per visited node).
#[inline(always)]
fn plane_distance(plane: Plane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32_wide(point.x, plane.normal.x as i32)
            .wrapping_add(mul_q12_i32_wide(point.y, plane.normal.y as i32))
            .wrapping_add(mul_q12_i32_wide(point.z, plane.normal.z as i32)),
    };
    dot.wrapping_sub(plane.distance)
}

const fn liquid_precedence(contents: i16) -> u8 {
    match contents {
        CONTENTS_LAVA => 3,
        CONTENTS_SLIME => 2,
        CONTENTS_WATER => 1,
        _ => 0,
    }
}

fn interpolate(start: Vec3I32, end: Vec3I32, fraction: i32) -> Vec3I32 {
    // Segment deltas are Q20.12 world spans against a Q0.12 fraction: bounded
    // like the plane math above, so the wide multiply is exact here too.
    let along =
        |from: i32, to: i32| from.wrapping_add(mul_q12_i32_wide(to.wrapping_sub(from), fraction));
    Vec3I32 {
        x: along(start.x, end.x),
        y: along(start.y, end.y),
        z: along(start.z, end.z),
    }
}

fn inverse_rotate_q12(rotation: Mat3I16, vector: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: q12_dot(
            [rotation.m[0][0], rotation.m[1][0], rotation.m[2][0]],
            vector,
        ),
        y: q12_dot(
            [rotation.m[0][1], rotation.m[1][1], rotation.m[2][1]],
            vector,
        ),
        z: q12_dot(
            [rotation.m[0][2], rotation.m[1][2], rotation.m[2][2]],
            vector,
        ),
    }
}

fn rotate_normal(rotation: Mat3I16, normal: Vec3I16) -> Vec3I16 {
    let normal = Vec3I32 {
        x: normal.x as i32,
        y: normal.y as i32,
        z: normal.z as i32,
    };
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    Vec3I16 {
        x: clamp(q12_dot(rotation.m[0], normal)),
        y: clamp(q12_dot(rotation.m[1], normal)),
        z: clamp(q12_dot(rotation.m[2], normal)),
    }
}

fn q12_dot(row: [i16; 3], vector: Vec3I32) -> i32 {
    mul_q12_i32(vector.x, row[0] as i32)
        .saturating_add(mul_q12_i32(vector.y, row[1] as i32))
        .saturating_add(mul_q12_i32(vector.z, row[2] as i32))
}

fn normal_dot_point(normal: Vec3I16, point: Vec3I32) -> i32 {
    mul_q12_i32(point.x, normal.x as i32)
        .saturating_add(mul_q12_i32(point.y, normal.y as i32))
        .saturating_add(mul_q12_i32(point.z, normal.z as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CookedRecord;
    use alloc::vec::Vec;

    fn axial_x_plane() -> [u8; Plane::SIZE] {
        plane(
            Vec3I16 {
                x: Q12_ONE as i16,
                y: 0,
                z: 0,
            },
            0,
            0,
        )
    }

    fn plane(normal: Vec3I16, distance: i32, kind: i32) -> [u8; Plane::SIZE] {
        let mut bytes = [0u8; Plane::SIZE];
        bytes[0..2].copy_from_slice(&normal.x.to_le_bytes());
        bytes[2..4].copy_from_slice(&normal.y.to_le_bytes());
        bytes[4..6].copy_from_slice(&normal.z.to_le_bytes());
        bytes[6..10].copy_from_slice(&distance.to_le_bytes());
        bytes[10..14].copy_from_slice(&kind.to_le_bytes());
        bytes
    }

    fn one_node() -> [u8; 6] {
        node(0, CONTENTS_EMPTY, CONTENTS_SOLID)
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
            RecordSlice::new(planes).unwrap(),
            RecordSlice::new(nodes).unwrap(),
            0,
        )
    }

    fn trace(
        hull: &CollisionHull<'_>,
        start: Vec3I32,
        end: Vec3I32,
        scratch: &mut TraceScratch,
    ) -> Trace {
        let mut output = Trace::default();
        assert!(hull.trace_into(&start, &end, scratch, &mut output));
        output
    }

    /// A trace whose every byte, flag slots and padding included, is the
    /// caller's own arbitrary fill or an arbitrary written value.
    ///
    /// The flag slots deliberately keep `fill`, which is normally neither 0 nor
    /// 1. That is legal precisely because [`TraceFlag`] is byte-backed; with
    /// `bool` slots this helper had to overwrite them with valid booleans, and
    /// so could never test the bytes that actually matter.
    fn sentinel_trace(fill: u8) -> Trace {
        let mut output = core::mem::MaybeUninit::<Trace>::uninit();
        unsafe {
            core::ptr::write_bytes(
                output.as_mut_ptr().cast::<u8>(),
                fill,
                core::mem::size_of::<Trace>(),
            );
            let pointer = output.as_mut_ptr();
            core::ptr::addr_of_mut!((*pointer).fraction).write(0x1122_3344);
            core::ptr::addr_of_mut!((*pointer).end).write(Vec3I32 {
                x: 0x0102_0304,
                y: 0x1112_1314,
                z: 0x2122_2324,
            });
            core::ptr::addr_of_mut!((*pointer).normal).write(Vec3I16 {
                x: 0x3132,
                y: 0x4142,
                z: 0x5152,
            });
            core::ptr::addr_of_mut!((*pointer).plane_distance).write(0x6162_6364);
            output.assume_init()
        }
    }

    fn trace_bytes(trace: &Trace) -> [u8; core::mem::size_of::<Trace>()] {
        let mut bytes = [0u8; core::mem::size_of::<Trace>()];
        unsafe {
            core::ptr::copy_nonoverlapping(
                (trace as *const Trace).cast::<u8>(),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
        }
        bytes
    }

    fn deep_crossing_hull(depth: usize) -> (Vec<u8>, Vec<u8>) {
        let mut planes = Vec::with_capacity(depth * 14);
        let mut nodes = Vec::with_capacity(depth * 6);
        for index in 0..depth {
            planes.extend_from_slice(&plane(
                Vec3I16 {
                    x: Q12_ONE as i16,
                    y: 0,
                    z: 0,
                },
                index as i32 * Q12_ONE,
                0,
            ));
            let front = if index + 1 == depth {
                CONTENTS_EMPTY
            } else {
                (index + 1) as i16
            };
            nodes.extend_from_slice(&node(index as i16, front, CONTENTS_SOLID));
        }
        (planes, nodes)
    }

    #[test]
    fn point_contents_follows_axial_plane_children() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let hull = hull(&planes, &nodes);
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 4096,
                y: 0,
                z: 0
            }),
            Some(CONTENTS_EMPTY)
        );
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: -4096,
                y: 0,
                z: 0
            }),
            Some(CONTENTS_SOLID)
        );
    }

    #[test]
    fn decoded_working_set_matches_packed_collision() {
        let plane_bytes = axial_x_plane();
        let node_bytes = one_node();
        let packed = hull(&plane_bytes, &node_bytes);
        let decoded_planes = [Plane::decode(&plane_bytes)];
        let decoded_nodes = [ClipNode::decode(&node_bytes)];
        let decoded = CollisionHull::new_decoded(&decoded_planes, &decoded_nodes, 0);
        let start = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };

        assert_eq!(packed.point_contents(start), decoded.point_contents(start));
        assert_eq!(
            trace(&packed, start, end, &mut TraceScratch::new()),
            trace(&decoded, start, end, &mut TraceScratch::new())
        );
    }

    #[test]
    fn trace_stops_on_the_near_side_of_a_solid_plane() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let trace = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: 4096,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: -4096,
                y: 0,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert!(!trace.all_solid.is_set());
        assert!(!trace.start_solid.is_set());
        assert_eq!(trace.fraction, 1984);
        assert_eq!(trace.end.x, 128);
        assert_eq!(trace.normal.x, 4096);
    }

    #[test]
    fn transformed_hull_rotates_and_translates_world_queries() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let transform = BrushTransform {
            origin: Vec3I32 {
                x: 10 * Q12_ONE,
                y: 20 * Q12_ONE,
                z: 30 * Q12_ONE,
            },
            rotation: Mat3I16::rotate_z(64),
        };
        let hull = hull(&planes, &nodes).transformed(transform);
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 10 * Q12_ONE,
                y: 21 * Q12_ONE,
                z: 30 * Q12_ONE,
            }),
            Some(CONTENTS_EMPTY)
        );
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 10 * Q12_ONE,
                y: 19 * Q12_ONE,
                z: 30 * Q12_ONE,
            }),
            Some(CONTENTS_SOLID)
        );

        let start = Vec3I32 {
            x: 10 * Q12_ONE,
            y: 21 * Q12_ONE,
            z: 30 * Q12_ONE,
        };
        let end = Vec3I32 {
            x: 10 * Q12_ONE,
            y: 19 * Q12_ONE,
            z: 30 * Q12_ONE,
        };
        let mut trace = Trace::default();
        assert!(hull.trace_into(&start, &end, &mut TraceScratch::new(), &mut trace,));
        assert_eq!(
            trace.normal,
            Vec3I16 {
                x: 0,
                y: 4096,
                z: 0
            }
        );
        assert_eq!(trace.plane_distance, 20 * Q12_ONE);
        assert!((20 * Q12_ONE..=20 * Q12_ONE + TRACE_PLANE_EPSILON_Q12).contains(&trace.end.y));
    }

    #[test]
    fn contents_codes_match_the_pxbsp_contract() {
        assert_eq!(CONTENTS_EMPTY, -1);
        assert_eq!(CONTENTS_SOLID, -2);
        assert_eq!(CONTENTS_WATER, -3);
        assert_eq!(CONTENTS_SLIME, -4);
        assert_eq!(CONTENTS_LAVA, -5);
        assert_eq!(CONTENTS_SKY, -6);
    }

    #[test]
    fn ordered_liquid_samples_report_depth_and_strongest_hazard() {
        let mut planes = Vec::new();
        for distance in [0, -Q12_ONE, -2 * Q12_ONE] {
            planes.extend_from_slice(&plane(
                Vec3I16 {
                    x: Q12_ONE as i16,
                    y: 0,
                    z: 0,
                },
                distance,
                0,
            ));
        }
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&node(0, CONTENTS_EMPTY, 1));
        nodes.extend_from_slice(&node(1, CONTENTS_WATER, 2));
        nodes.extend_from_slice(&node(2, CONTENTS_SLIME, CONTENTS_LAVA));
        let hull = hull(&planes, &nodes);
        let at = |x| Vec3I32 { x, y: 0, z: 0 };

        assert_eq!(
            hull.sample_liquid_contents(&[at(Q12_ONE), at(Q12_ONE), at(Q12_ONE)]),
            Some(LiquidContentsSample::default())
        );
        assert_eq!(
            hull.sample_liquid_contents(&[at(-Q12_ONE / 2), at(Q12_ONE), at(Q12_ONE)]),
            Some(LiquidContentsSample {
                contents: CONTENTS_WATER,
                water_level: 1,
            })
        );
        assert_eq!(
            hull.sample_liquid_contents(&[at(Q12_ONE), at(-Q12_ONE / 2), at(-3 * Q12_ONE)]),
            Some(LiquidContentsSample::default()),
            "Quake water level must begin at the feet"
        );
        assert_eq!(
            hull.sample_liquid_contents(&[
                at(-Q12_ONE / 2),
                at(-3 * Q12_ONE / 2),
                at(-3 * Q12_ONE),
            ]),
            Some(LiquidContentsSample {
                contents: CONTENTS_LAVA,
                water_level: 3,
            })
        );
    }

    #[test]
    fn trace_storage_has_a_fixed_guest_size() {
        assert_eq!(core::mem::size_of::<Trace>(), 32);
        assert_eq!(core::mem::size_of::<TraceScratch>(), 2_560);
    }

    #[test]
    fn unobstructed_trace_has_exact_open_result() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let end = Vec3I32 {
            x: 2 * Q12_ONE,
            y: 3 * Q12_ONE,
            z: 4 * Q12_ONE,
        };
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: Q12_ONE,
                y: 3 * Q12_ONE,
                z: 4 * Q12_ONE,
            },
            end,
            &mut TraceScratch::new(),
        );
        assert_eq!(
            result,
            Trace {
                all_solid: TraceFlag::CLEAR,
                start_solid: TraceFlag::CLEAR,
                in_open: TraceFlag::SET,
                in_water: TraceFlag::CLEAR,
                fraction: Q12_ONE,
                end,
                normal: Vec3I16 { x: 0, y: 0, z: 0 },
                plane_distance: 0,
            }
        );
    }

    #[test]
    fn start_solid_and_all_solid_are_exact() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let end = Vec3I32 {
            x: -2 * Q12_ONE,
            y: Q12_ONE,
            z: 0,
        };
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: -Q12_ONE,
                y: Q12_ONE,
                z: 0,
            },
            end,
            &mut TraceScratch::new(),
        );
        assert_eq!(
            result,
            Trace {
                all_solid: TraceFlag::SET,
                start_solid: TraceFlag::SET,
                in_open: TraceFlag::CLEAR,
                in_water: TraceFlag::CLEAR,
                fraction: Q12_ONE,
                end,
                normal: Vec3I16 { x: 0, y: 0, z: 0 },
                plane_distance: 0,
            }
        );
    }

    #[test]
    fn start_solid_trace_can_exit_into_open_space() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let end = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: -Q12_ONE,
                y: 0,
                z: 0,
            },
            end,
            &mut TraceScratch::new(),
        );
        assert!(result.start_solid.is_set());
        assert!(!result.all_solid.is_set());
        assert!(result.in_open.is_set());
        assert_eq!(result.fraction, Q12_ONE);
        assert_eq!(result.end, end);
    }

    #[test]
    fn non_axial_plane_collision_is_exact() {
        let planes = plane(
            Vec3I16 {
                x: 2896,
                y: 2896,
                z: 0,
            },
            0,
            3,
        );
        let nodes = one_node();
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: Q12_ONE,
                y: Q12_ONE,
                z: 0,
            },
            Vec3I32 {
                x: -Q12_ONE,
                y: -Q12_ONE,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, 2002);
        assert_eq!(result.end, Vec3I32 { x: 92, y: 92, z: 0 });
        assert_eq!(
            result.normal,
            Vec3I16 {
                x: 2896,
                y: 2896,
                z: 0,
            }
        );
        assert_eq!(result.plane_distance, 0);
    }

    #[test]
    fn near_plane_epsilon_clamps_to_the_segment_start() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let start = Vec3I32 { x: 64, y: 0, z: 0 };
        let result = trace(
            &hull(&planes, &nodes),
            start,
            Vec3I32 {
                x: -Q12_ONE,
                y: 0,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, 0);
        assert_eq!(result.end, start);
        assert_eq!(result.normal.x, Q12_ONE as i16);
    }

    #[test]
    fn zero_length_trace_is_deterministic() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let point = Vec3I32 {
            x: Q12_ONE,
            y: -7 * Q12_ONE,
            z: 9 * Q12_ONE,
        };
        let result = trace(
            &hull(&planes, &nodes),
            point,
            point,
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, Q12_ONE);
        assert_eq!(result.end, point);
        assert!(result.in_open.is_set());
        assert!(!result.all_solid.is_set());
    }

    #[test]
    fn on_plane_tie_uses_the_front_child_and_normal() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let start = Vec3I32 { x: 0, y: 0, z: 0 };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let first = trace(&hull(&planes, &nodes), start, end, &mut scratch);
        let second = trace(&hull(&planes, &nodes), start, end, &mut scratch);
        assert_eq!(first, second);
        assert_eq!(first.fraction, 0);
        assert_eq!(first.end, start);
        assert_eq!(first.normal.x, Q12_ONE as i16);
    }

    #[test]
    fn static_y_up_brush_floor_hit_uses_the_public_contract() {
        let planes = plane(
            Vec3I16 {
                x: 0,
                y: Q12_ONE as i16,
                z: 0,
            },
            0,
            1,
        );
        let nodes = one_node();
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: 0,
                y: Q12_ONE,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -Q12_ONE,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.end.y, TRACE_PLANE_EPSILON_Q12);
        assert_eq!(result.normal.y, Q12_ONE as i16);
        assert_eq!(result.plane_distance, 0);
    }

    #[test]
    fn failed_static_and_transformed_traces_preserve_every_output_byte() {
        let planes = axial_x_plane();
        let invalid_nodes = node(7, CONTENTS_EMPTY, CONTENTS_SOLID);
        let invalid_hull = hull(&planes, &invalid_nodes);
        let start = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let mut static_output = sentinel_trace(0xa5);
        let static_before = trace_bytes(&static_output);
        assert!(!invalid_hull.trace_into(&start, &end, &mut scratch, &mut static_output));
        assert_eq!(trace_bytes(&static_output), static_before);

        let transformed = invalid_hull.transformed(BrushTransform::translated(Vec3I32 {
            x: Q12_ONE,
            y: 2 * Q12_ONE,
            z: 3 * Q12_ONE,
        }));
        let mut mover_output = sentinel_trace(0x5a);
        let mover_before = trace_bytes(&mover_output);
        assert!(!transformed.trace_into(&start, &end, &mut scratch, &mut mover_output));
        assert_eq!(trace_bytes(&mover_output), mover_before);

        let cyclic_nodes = node(0, 0, CONTENTS_SOLID);
        let cyclic_hull = hull(&planes, &cyclic_nodes);
        let cyclic_end = Vec3I32 {
            x: 2 * Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut cyclic_output = sentinel_trace(0xc3);
        let cyclic_before = trace_bytes(&cyclic_output);
        assert!(!cyclic_hull.trace_into(&start, &cyclic_end, &mut scratch, &mut cyclic_output,));
        assert_eq!(trace_bytes(&cyclic_output), cyclic_before);
    }

    #[test]
    fn stack_capacity_boundary_succeeds_and_overflow_preserves_output() {
        let (boundary_planes, boundary_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY);
        let start = Vec3I32 {
            x: (TRACE_STACK_CAPACITY as i32 + 1) * Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let boundary = trace(
            &hull(&boundary_planes, &boundary_nodes),
            start,
            end,
            &mut scratch,
        );
        assert_eq!(
            boundary.plane_distance,
            (TRACE_STACK_CAPACITY as i32 - 1) * Q12_ONE
        );
        assert_eq!(boundary.normal.x, Q12_ONE as i16);
        assert!(!boundary.all_solid.is_set());

        let (overflow_planes, overflow_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
        let mut output = sentinel_trace(0x3c);
        let before = trace_bytes(&output);
        assert!(!hull(&overflow_planes, &overflow_nodes).trace_into(
            &start,
            &end,
            &mut scratch,
            &mut output,
        ));
        assert_eq!(trace_bytes(&output), before);
    }

    #[test]
    fn scratch_reuse_after_overflow_has_no_stale_state() {
        let (overflow_planes, overflow_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
        let start = Vec3I32 {
            x: (TRACE_STACK_CAPACITY as i32 + 2) * Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let mut ignored = Trace::default();
        assert!(!hull(&overflow_planes, &overflow_nodes).trace_into(
            &start,
            &end,
            &mut scratch,
            &mut ignored,
        ));

        let planes = axial_x_plane();
        let nodes = one_node();
        let simple_start = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let simple_end = Vec3I32 {
            x: 2 * Q12_ONE,
            y: 0,
            z: 0,
        };
        let reused = trace(
            &hull(&planes, &nodes),
            simple_start,
            simple_end,
            &mut scratch,
        );
        let fresh = trace(
            &hull(&planes, &nodes),
            simple_start,
            simple_end,
            &mut TraceScratch::new(),
        );
        assert_eq!(reused, fresh);
    }

    /// The flag slots are storage for arbitrary bytes, and the boundary that
    /// turns them into meaning normalizes rather than reinterprets.
    ///
    /// Every one of the 256 byte patterns is written into a live flag slot and
    /// read back, which is only a defined program because the slot is byte
    /// backed. The guest once produced 0xe7 in exactly this slot; with a `bool`
    /// there, merely holding that byte was already undefined behaviour and no
    /// later read could undo it.
    #[test]
    fn every_flag_byte_is_a_legal_value_and_normalizes_to_one_bool() {
        for byte in 0..=u8::MAX {
            let flag = TraceFlag::from_byte(byte);
            assert_eq!(flag.byte(), byte, "the slot stores the caller's byte");
            assert_eq!(flag.is_set(), byte != 0, "0x{byte:02x} normalizes wrong");
            // Constructing the `bool` may not smuggle the byte through.
            let boolean: bool = flag.into();
            let observed =
                unsafe { core::ptr::read_volatile(core::ptr::from_ref(&boolean).cast::<u8>()) };
            assert!(observed <= 1, "0x{byte:02x} produced an invalid bool byte");
        }
        assert_eq!(TraceFlag::default(), TraceFlag::CLEAR);
        assert_eq!(TraceFlag::new(true), TraceFlag::SET);
        assert_eq!(TraceFlag::new(false), TraceFlag::CLEAR);
    }

    /// A failed trace preserves poisoned flag bytes exactly, and the caller can
    /// still read them afterwards.
    ///
    /// This is the pairing that used to be impossible to state: the contract
    /// says the bytes survive untouched, and with `bool` slots surviving
    /// untouched meant surviving as an invalid value.
    #[test]
    fn a_failed_trace_preserves_poisoned_flag_bytes_and_they_stay_readable() {
        let (overflow_planes, overflow_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
        let start = Vec3I32 {
            x: (TRACE_STACK_CAPACITY as i32 + 2) * Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut output = Trace {
            all_solid: TraceFlag::from_byte(0xe7),
            start_solid: TraceFlag::from_byte(0x00),
            in_open: TraceFlag::from_byte(0x02),
            in_water: TraceFlag::from_byte(0xff),
            ..Trace::default()
        };
        let before = trace_bytes(&output);
        assert!(!hull(&overflow_planes, &overflow_nodes).trace_into(
            &start,
            &end,
            &mut TraceScratch::new(),
            &mut output,
        ));
        assert_eq!(trace_bytes(&output), before);
        assert_eq!(output.all_solid.byte(), 0xe7);
        assert!(output.all_solid.is_set());
        assert!(!output.start_solid.is_set());
        assert!(output.in_open.is_set());
        assert!(output.in_water.is_set());
    }
}
