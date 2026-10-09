//! Small geometry kit for the stream partitioner: boxes, convex solids with
//! point and segment tests, a spatial index over them, and exact-enough
//! polygon clipping against an axis-aligned cell.
//!
//! Everything is `f64` host math over engine units. Nothing here reaches the
//! guest, so there is no fixed-point requirement; determinism comes from
//! fixed iteration order and no use of hash iteration anywhere.

use std::collections::HashMap;

use crate::brush::Brush;

pub type V3 = [f64; 3];

/// Tolerance for "lies on the plane" and sliver rejection, engine units.
pub const PLANE_EPS: f64 = 1.0e-6;
/// Pieces smaller than this (units squared) are dropped after clipping.
pub const MIN_PIECE_AREA: f64 = 1.0e-3;

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}

pub fn distance(a: V3, b: V3) -> f64 {
    length(sub(a, b))
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: V3,
    pub max: V3,
}

impl Aabb {
    pub const EMPTY: Self = Self {
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
    };

    pub fn from_points<'a>(points: impl IntoIterator<Item = &'a V3>) -> Self {
        let mut bounds = Self::EMPTY;
        for point in points {
            bounds.grow(*point);
        }
        bounds
    }

    pub fn grow(&mut self, point: V3) {
        for (axis, &value) in point.iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        let mut result = *self;
        result.grow(other.min);
        result.grow(other.max);
        result
    }

    pub fn is_empty(&self) -> bool {
        (0..3).any(|axis| self.min[axis] > self.max[axis])
    }

    pub fn extent(&self, axis: usize) -> f64 {
        self.max[axis] - self.min[axis]
    }

    pub fn center(&self) -> V3 {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    pub fn volume(&self) -> f64 {
        (0..3).map(|axis| self.extent(axis).max(0.0)).product()
    }

    /// Closed containment.
    pub fn contains(&self, point: V3) -> bool {
        (0..3).all(|axis| point[axis] >= self.min[axis] && point[axis] <= self.max[axis])
    }

    /// Half-open containment `[min, max)` used to assign points to cells so a
    /// point on a shared face belongs to exactly one of them.
    pub fn contains_half_open(&self, point: V3) -> bool {
        (0..3).all(|axis| point[axis] >= self.min[axis] && point[axis] < self.max[axis])
    }

    /// Overlap with positive volume on every axis.
    pub fn overlaps_strict(&self, other: &Self) -> bool {
        (0..3).all(|axis| self.min[axis] < other.max[axis] && other.min[axis] < self.max[axis])
    }

    /// Overlap including touching boundaries.
    pub fn overlaps(&self, other: &Self) -> bool {
        (0..3).all(|axis| self.min[axis] <= other.max[axis] && other.min[axis] <= self.max[axis])
    }

    pub fn distance_to_point(&self, point: V3) -> f64 {
        let mut sum = 0.0;
        for (axis, &value) in point.iter().enumerate() {
            let delta = (self.min[axis] - value)
                .max(value - self.max[axis])
                .max(0.0);
            sum += delta * delta;
        }
        sum.sqrt()
    }

    pub fn distance_to_box(&self, other: &Self) -> f64 {
        let mut sum = 0.0;
        for axis in 0..3 {
            let delta = (self.min[axis] - other.max[axis])
                .max(other.min[axis] - self.max[axis])
                .max(0.0);
            sum += delta * delta;
        }
        sum.sqrt()
    }

    /// Split at `position` along `axis` into (low, high).
    pub fn split(&self, axis: usize, position: f64) -> (Self, Self) {
        let mut low = *self;
        let mut high = *self;
        low.max[axis] = position;
        high.min[axis] = position;
        (low, high)
    }

    pub fn expanded(&self, amount: f64) -> Self {
        Self {
            min: self.min.map(|v| v - amount),
            max: self.max.map(|v| v + amount),
        }
    }
}

/// A convex solid as outward unit planes (`dot(n, p) <= d` inside).
#[derive(Clone, Debug)]
pub struct ConvexSolid {
    pub planes: Vec<(V3, f64)>,
    pub bounds: Aabb,
}

impl ConvexSolid {
    pub fn from_brush(brush: &Brush) -> Option<Self> {
        let solved = brush.solve();
        if !solved.is_valid() {
            return None;
        }
        let mut planes = Vec::new();
        for face in &brush.faces {
            let plane = crate::brush::Plane::from_points(face.points)?;
            let normal = plane.normal.map(|v| v as f64);
            let len = length(normal);
            if len == 0.0 {
                return None;
            }
            planes.push((scale(normal, 1.0 / len), plane.dist as f64 / len));
        }
        Some(Self {
            planes,
            bounds: Aabb {
                min: solved.min,
                max: solved.max,
            },
        })
    }

    /// Strict interior test with `margin` units of shrink.
    pub fn contains(&self, point: V3, margin: f64) -> bool {
        self.bounds.contains(point)
            && self
                .planes
                .iter()
                .all(|(normal, d)| dot(*normal, point) - d < -margin)
    }

    /// Whether the open segment `a..b` passes through the solid's interior.
    pub fn blocks_segment(&self, a: V3, b: V3) -> bool {
        let direction = sub(b, a);
        let mut t_enter = 0.0f64;
        let mut t_exit = 1.0f64;
        for (normal, d) in &self.planes {
            let denom = dot(*normal, direction);
            let start = dot(*normal, a) - d;
            if denom.abs() < 1.0e-12 {
                if start >= 0.0 {
                    return false;
                }
                continue;
            }
            let t = -start / denom;
            if denom < 0.0 {
                t_enter = t_enter.max(t);
            } else {
                t_exit = t_exit.min(t);
            }
            if t_enter >= t_exit {
                return false;
            }
        }
        t_exit - t_enter > 1.0e-9
    }
}

/// Uniform grid over solids for point and segment candidates.
pub struct SolidIndex {
    pub solids: Vec<ConvexSolid>,
    cell: f64,
    buckets: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl SolidIndex {
    pub fn new(solids: Vec<ConvexSolid>, cell: f64) -> Self {
        let mut buckets: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        let key = |v: f64| (v / cell).floor() as i32;
        for (index, solid) in solids.iter().enumerate() {
            let (lo, hi) = (solid.bounds.min, solid.bounds.max);
            for x in key(lo[0])..=key(hi[0]) {
                for y in key(lo[1])..=key(hi[1]) {
                    for z in key(lo[2])..=key(hi[2]) {
                        buckets.entry((x, y, z)).or_default().push(index as u32);
                    }
                }
            }
        }
        Self {
            solids,
            cell,
            buckets,
        }
    }

    fn key(&self, point: V3) -> (i32, i32, i32) {
        (
            (point[0] / self.cell).floor() as i32,
            (point[1] / self.cell).floor() as i32,
            (point[2] / self.cell).floor() as i32,
        )
    }

    /// Whether `point` lies strictly inside any solid.
    pub fn point_in_solid(&self, point: V3) -> bool {
        self.buckets.get(&self.key(point)).is_some_and(|list| {
            list.iter()
                .any(|&index| self.solids[index as usize].contains(point, 1.0e-6))
        })
    }

    /// Whether the segment passes through any solid's interior.
    pub fn segment_blocked(&self, a: V3, b: V3) -> bool {
        let delta = sub(b, a);
        let len = length(delta);
        if len < 1.0e-9 {
            return self.point_in_solid(a);
        }
        let steps = ((len / (self.cell * 0.5)).ceil() as usize).max(1);
        let mut seen: Vec<u32> = Vec::new();
        let mut last_key = None;
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            let point = add(a, scale(delta, t));
            let key = self.key(point);
            if last_key == Some(key) {
                continue;
            }
            last_key = Some(key);
            if let Some(list) = self.buckets.get(&key) {
                for &index in list {
                    if seen.contains(&index) {
                        continue;
                    }
                    seen.push(index);
                    if self.solids[index as usize].blocks_segment(a, b) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Polygon area via the vector area.
pub fn polygon_area(vertices: &[V3]) -> f64 {
    if vertices.len() < 3 {
        return 0.0;
    }
    let mut sum = [0.0; 3];
    for index in 1..vertices.len() - 1 {
        let c = cross(
            sub(vertices[index], vertices[0]),
            sub(vertices[index + 1], vertices[0]),
        );
        sum = add(sum, c);
    }
    length(sum) * 0.5
}

pub fn polygon_centroid(vertices: &[V3]) -> V3 {
    let mut sum = [0.0; 3];
    for vertex in vertices {
        sum = add(sum, *vertex);
    }
    scale(sum, 1.0 / vertices.len().max(1) as f64)
}

/// Clip a convex polygon to a closed box. `None` when nothing of positive
/// area survives.
pub fn clip_polygon_to_box(vertices: &[V3], bounds: &Aabb) -> Option<Vec<V3>> {
    let mut polygon = vertices.to_vec();
    for axis in 0..3 {
        polygon = clip_half(&polygon, axis, bounds.min[axis], true)?;
        polygon = clip_half(&polygon, axis, bounds.max[axis], false)?;
    }
    (polygon.len() >= 3 && polygon_area(&polygon) >= MIN_PIECE_AREA).then_some(polygon)
}

/// Keep the part with `v[axis] >= limit` (`keep_high`) or `<= limit`.
fn clip_half(polygon: &[V3], axis: usize, limit: f64, keep_high: bool) -> Option<Vec<V3>> {
    let signed = |v: &V3| {
        if keep_high {
            v[axis] - limit
        } else {
            limit - v[axis]
        }
    };
    let mut out = Vec::with_capacity(polygon.len() + 2);
    for index in 0..polygon.len() {
        let current = polygon[index];
        let next = polygon[(index + 1) % polygon.len()];
        let (a, b) = (signed(&current), signed(&next));
        if a >= -PLANE_EPS {
            out.push(current);
        }
        if (a > PLANE_EPS && b < -PLANE_EPS) || (a < -PLANE_EPS && b > PLANE_EPS) {
            let t = a / (a - b);
            let mut crossing = add(current, scale(sub(next, current), t));
            crossing[axis] = limit;
            out.push(crossing);
        }
    }
    (out.len() >= 3).then_some(out)
}

/// Whether the polygon lies on the axis-aligned plane `axis = value`.
pub fn polygon_on_axis_plane(vertices: &[V3], axis: usize, value: f64) -> bool {
    vertices
        .iter()
        .all(|v| (v[axis] - value).abs() <= PLANE_EPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(y: f64) -> Vec<V3> {
        vec![
            [0.0, y, 0.0],
            [10.0, y, 0.0],
            [10.0, y, 10.0],
            [0.0, y, 10.0],
        ]
    }

    #[test]
    fn clip_keeps_overlap_and_drops_outside() {
        let cell = Aabb {
            min: [5.0, -1.0, 0.0],
            max: [20.0, 1.0, 10.0],
        };
        let clipped = clip_polygon_to_box(&square(0.0), &cell).unwrap();
        assert!((polygon_area(&clipped) - 50.0).abs() < 1.0e-9);
        let away = Aabb {
            min: [20.0, -1.0, 0.0],
            max: [30.0, 1.0, 10.0],
        };
        assert!(clip_polygon_to_box(&square(0.0), &away).is_none());
    }

    #[test]
    fn solid_segment_and_point_tests() {
        let brush = Brush::cuboid([0, 0, 0], [10, 10, 10]);
        let solid = ConvexSolid::from_brush(&brush).unwrap();
        assert!(solid.contains([5.0, 5.0, 5.0], 0.0));
        assert!(!solid.contains([15.0, 5.0, 5.0], 0.0));
        assert!(solid.blocks_segment([-5.0, 5.0, 5.0], [15.0, 5.0, 5.0]));
        assert!(!solid.blocks_segment([-5.0, 15.0, 5.0], [15.0, 15.0, 5.0]));
        // Grazing a face is not blocked.
        assert!(!solid.blocks_segment([-5.0, 10.0, 5.0], [15.0, 10.0, 5.0]));
        let index = SolidIndex::new(vec![solid], 8.0);
        assert!(index.segment_blocked([-5.0, 5.0, 5.0], [15.0, 5.0, 5.0]));
        assert!(!index.segment_blocked([-5.0, 12.0, 5.0], [15.0, 12.0, 5.0]));
        assert!(index.point_in_solid([5.0, 5.0, 5.0]));
    }
}
