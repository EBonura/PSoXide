//! Quake BSP collision-hull traversal.
//!
//! Lifted from quake-psx `crates/quake-core` (commit 9e20a1b, same GPL-2
//! authorship) so the guest runtime, the editor cook and quake-psx share
//! one hull tracer (docs/quake-bsp-migration-plan.md, P2).

use crate::{ClipNode, Plane, RecordSlice, Vec3I16, Vec3I32};
use psx_engine::div_q12_i32;
use psx_gte::math::Mat3I16;
use psx_math::int32::mul_q12_i32;

pub const CONTENTS_EMPTY: i16 = -1;
pub const CONTENTS_SOLID: i16 = -2;
pub const CONTENTS_WATER: i16 = -3;
pub const Q12_ONE: i32 = 4096;
const DIST_EPSILON: i32 = 128;

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

    fn point_to_local(self, point: Vec3I32) -> Vec3I32 {
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Trace {
    pub all_solid: bool,
    pub start_solid: bool,
    pub in_open: bool,
    pub in_water: bool,
    pub fraction: i32,
    pub end: Vec3I32,
    pub normal: Vec3I16,
    pub plane_distance: i32,
}

impl Trace {
    const fn unobstructed(end: Vec3I32) -> Self {
        Self {
            all_solid: true,
            start_solid: false,
            in_open: false,
            in_water: false,
            fraction: Q12_ONE,
            end,
            normal: Vec3I16 { x: 0, y: 0, z: 0 },
            plane_distance: 0,
        }
    }
}

#[derive(Copy, Clone)]
pub struct CollisionHull<'a> {
    planes: RecordSlice<'a, Plane>,
    nodes: RecordSlice<'a, ClipNode>,
    head_node: i16,
}

impl<'a> CollisionHull<'a> {
    pub const fn new(
        planes: RecordSlice<'a, Plane>,
        nodes: RecordSlice<'a, ClipNode>,
        head_node: i16,
    ) -> Self {
        Self {
            planes,
            nodes,
            head_node,
        }
    }

    pub fn point_contents(self, point: Vec3I32) -> Option<i16> {
        self.point_contents_from(self.head_node, point)
    }

    pub fn trace(self, start: Vec3I32, end: Vec3I32) -> Option<Trace> {
        let mut trace = Trace::unobstructed(end);
        self.recursive_check(self.head_node, 0, Q12_ONE, start, end, &mut trace)?;
        Some(trace)
    }

    /// Apply one mover transform while retaining this model-local hull.
    pub const fn transformed(self, transform: BrushTransform) -> TransformedCollisionHull<'a> {
        TransformedCollisionHull {
            local: self,
            transform,
        }
    }

    fn point_contents_from(self, mut node_index: i16, point: Vec3I32) -> Option<i16> {
        while node_index >= 0 {
            let node = self.nodes.get(node_index as usize)?;
            let plane = self.planes.get(node.plane as usize)?;
            node_index = node.children[(plane_distance(plane, point) < 0) as usize];
        }
        Some(node_index)
    }

    #[allow(clippy::too_many_arguments)]
    fn recursive_check(
        self,
        mut node_index: i16,
        mut start_fraction: i32,
        end_fraction: i32,
        mut start: Vec3I32,
        end: Vec3I32,
        trace: &mut Trace,
    ) -> Option<bool> {
        while node_index >= 0 {
            let node = self.nodes.get(node_index as usize)?;
            let plane = self.planes.get(node.plane as usize)?;
            let start_distance = plane_distance(plane, start);
            let end_distance = plane_distance(plane, end);

            if start_distance >= 0 && end_distance >= 0 {
                node_index = node.children[0];
                continue;
            }
            if start_distance < 0 && end_distance < 0 {
                node_index = node.children[1];
                continue;
            }

            let numerator = if start_distance < 0 {
                start_distance.saturating_add(DIST_EPSILON)
            } else {
                start_distance.saturating_sub(DIST_EPSILON)
            };
            let fraction = div_q12_i32(numerator, start_distance.saturating_sub(end_distance))
                .clamp(0, Q12_ONE);
            let middle_fraction = start_fraction.saturating_add(mul_q12_i32(
                end_fraction.saturating_sub(start_fraction),
                fraction,
            ));
            let middle = interpolate(start, end, fraction);
            let side = usize::from(start_distance < 0);

            if !self.recursive_check(
                node.children[side],
                start_fraction,
                middle_fraction,
                start,
                middle,
                trace,
            )? {
                return Some(false);
            }

            let far_child = node.children[side ^ 1];
            if self.point_contents_from(far_child, middle)? != CONTENTS_SOLID {
                node_index = far_child;
                start_fraction = middle_fraction;
                start = middle;
                continue;
            }
            if trace.all_solid {
                return Some(false);
            }

            if side == 0 {
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
            trace.fraction = middle_fraction;
            trace.end = middle;
            return Some(false);
        }

        if node_index != CONTENTS_SOLID {
            trace.all_solid = false;
            if node_index == CONTENTS_EMPTY {
                trace.in_open = true;
            } else {
                trace.in_water = true;
            }
        } else {
            trace.start_solid = true;
        }
        Some(true)
    }
}

/// World-space query facade over one model-local clipnode hull.
#[derive(Copy, Clone)]
pub struct TransformedCollisionHull<'a> {
    local: CollisionHull<'a>,
    transform: BrushTransform,
}

impl TransformedCollisionHull<'_> {
    pub fn point_contents(self, point: Vec3I32) -> Option<i16> {
        self.local
            .point_contents(self.transform.point_to_local(point))
    }

    pub fn trace(self, start: Vec3I32, end: Vec3I32) -> Option<Trace> {
        let mut trace = self.local.trace(
            self.transform.point_to_local(start),
            self.transform.point_to_local(end),
        )?;
        trace.end = interpolate(start, end, trace.fraction);
        trace.normal = rotate_normal(self.transform.rotation, trace.normal);
        trace.plane_distance = trace
            .plane_distance
            .saturating_add(normal_dot_point(trace.normal, self.transform.origin));
        Some(trace)
    }
}

fn plane_distance(plane: Plane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32(point.x, plane.normal.x as i32)
            .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
            .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
    };
    dot.saturating_sub(plane.distance)
}

fn interpolate(start: Vec3I32, end: Vec3I32, fraction: i32) -> Vec3I32 {
    Vec3I32 {
        x: start
            .x
            .saturating_add(mul_q12_i32(end.x.saturating_sub(start.x), fraction)),
        y: start
            .y
            .saturating_add(mul_q12_i32(end.y.saturating_sub(start.y), fraction)),
        z: start
            .z
            .saturating_add(mul_q12_i32(end.z.saturating_sub(start.z), fraction)),
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

    fn axial_x_plane() -> [u8; 14] {
        let mut bytes = [0u8; 14];
        bytes[0..2].copy_from_slice(&4096i16.to_le_bytes());
        bytes[10..14].copy_from_slice(&0i32.to_le_bytes());
        bytes
    }

    fn one_node() -> [u8; 6] {
        let mut bytes = [0u8; 6];
        bytes[0..2].copy_from_slice(&0i16.to_le_bytes());
        bytes[2..4].copy_from_slice(&CONTENTS_EMPTY.to_le_bytes());
        bytes[4..6].copy_from_slice(&CONTENTS_SOLID.to_le_bytes());
        bytes
    }

    fn hull<'a>(planes: &'a [u8], nodes: &'a [u8]) -> CollisionHull<'a> {
        CollisionHull::new(
            RecordSlice::new(planes).unwrap(),
            RecordSlice::new(nodes).unwrap(),
            0,
        )
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
    fn trace_stops_on_the_near_side_of_a_solid_plane() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let trace = hull(&planes, &nodes)
            .trace(
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
            )
            .unwrap();
        assert!(!trace.all_solid);
        assert!(!trace.start_solid);
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

        let trace = hull
            .trace(
                Vec3I32 {
                    x: 10 * Q12_ONE,
                    y: 21 * Q12_ONE,
                    z: 30 * Q12_ONE,
                },
                Vec3I32 {
                    x: 10 * Q12_ONE,
                    y: 19 * Q12_ONE,
                    z: 30 * Q12_ONE,
                },
            )
            .expect("transformed trace");
        assert_eq!(
            trace.normal,
            Vec3I16 {
                x: 0,
                y: 4096,
                z: 0
            }
        );
        assert_eq!(trace.plane_distance, 20 * Q12_ONE);
        assert!((20 * Q12_ONE..=20 * Q12_ONE + DIST_EPSILON).contains(&trace.end.y));
    }
}
