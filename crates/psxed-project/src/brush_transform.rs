//! Exact orthogonal brush transforms with texture lock.
//!
//! Mirror, quarter-turn about the vertical axis, then translate: the moves
//! TrenchBroom offers as flip, rotate 90 and move. Points stay integers
//! because the pivot is an integer and every step is a signed axis swap, so
//! nothing drifts off the grid the way a free rotation does.
//!
//! Texture lock keeps every surface point showing the texel it showed before.
//! The paraxial projection can change axis under a quarter turn (an X-facing
//! wall becomes Z-facing), so the face's rotation, scale signs and offset are
//! re-solved from three points on the face rather than patched.

use crate::brush::{paraxial_uv, Brush, FaceUv, Plane, BRUSH_UV_UNITS_PER_TEXEL};

/// One orthogonal move, applied as mirror, then rotate, then translate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrthoTransform {
    /// Point the mirror and rotation are taken about.
    pub pivot: [i32; 3],
    /// Axis to mirror across the plane through `pivot`: 0 = X, 1 = Y, 2 = Z.
    pub mirror_axis: Option<usize>,
    /// Quarter turns about the vertical (Y) axis through `pivot`. One turn
    /// carries +X onto +Z. Negative turns go the other way.
    pub quarter_turns: i32,
    /// Offset added last.
    pub translate: [i32; 3],
}

impl OrthoTransform {
    /// Where the transform sends a point.
    pub fn apply(&self, point: [i32; 3]) -> [i32; 3] {
        let mut local = [0i64; 3];
        for axis in 0..3 {
            local[axis] = i64::from(point[axis]) - i64::from(self.pivot[axis]);
        }
        if let Some(axis) = self.mirror_axis {
            local[axis] = -local[axis];
        }
        for _ in 0..self.quarter_turns.rem_euclid(4) {
            let (x, z) = (local[0], local[2]);
            local[0] = -z;
            local[2] = x;
        }
        let mut out = [0i32; 3];
        for axis in 0..3 {
            let value = local[axis] + i64::from(self.pivot[axis]) + i64::from(self.translate[axis]);
            out[axis] = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        }
        out
    }

    /// Whether the transform turns the solid inside out (a mirror), which
    /// means face windings have to be reversed to keep normals outward.
    pub const fn reverses_winding(&self) -> bool {
        self.mirror_axis.is_some()
    }
}

/// Texel a face's mapping gives a point on it.
fn texel(plane: &Plane, uv: &FaceUv, point: [i32; 3]) -> [f64; 2] {
    let raw = paraxial_uv(plane, point.map(f64::from));
    uv.apply([
        raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
        raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
    ])
}

impl Brush {
    /// Apply an orthogonal transform, keeping each face's texture pinned to
    /// the surface. Returns how many faces could only be locked
    /// approximately (a sheared mapping the rotation-plus-scale model cannot
    /// hold, which happens only on faces at exactly 45 degrees where the
    /// paraxial projection picks a different axis afterwards).
    pub fn transform_ortho_with_uv_lock(&mut self, transform: OrthoTransform) -> usize {
        let mut approximate = 0;
        for face in &mut self.faces {
            let Some(old_plane) = Plane::from_points(face.points) else {
                face.points = face.points.map(|point| transform.apply(point));
                continue;
            };
            let samples = face.points;
            let targets = samples.map(|point| texel(&old_plane, &face.uv, point));
            let moved = samples.map(|point| transform.apply(point));
            let mut points = moved;
            if transform.reverses_winding() {
                points.swap(1, 2);
            }
            face.points = points;
            let Some(new_plane) = Plane::from_points(points) else {
                continue;
            };
            let raw = moved.map(|point| {
                let projected = paraxial_uv(&new_plane, point.map(f64::from));
                [
                    projected[0] / BRUSH_UV_UNITS_PER_TEXEL,
                    projected[1] / BRUSH_UV_UNITS_PER_TEXEL,
                ]
            });
            match solve_face_uv(raw, targets, &face.uv) {
                Some((uv, exact)) => {
                    face.uv = uv;
                    if !exact {
                        approximate += 1;
                    }
                }
                None => approximate += 1,
            }
        }
        approximate
    }
}

/// The mapping that sends each `raw` sample to its `target` texel, or `None`
/// when the samples are degenerate. The flag says whether it is exact.
fn solve_face_uv(
    raw: [[f64; 2]; 3],
    target: [[f64; 2]; 3],
    previous: &FaceUv,
) -> Option<(FaceUv, bool)> {
    let d = [
        [raw[1][0] - raw[0][0], raw[2][0] - raw[0][0]],
        [raw[1][1] - raw[0][1], raw[2][1] - raw[0][1]],
    ];
    let e = [
        [target[1][0] - target[0][0], target[2][0] - target[0][0]],
        [target[1][1] - target[0][1], target[2][1] - target[0][1]],
    ];
    let det = d[0][0] * d[1][1] - d[0][1] * d[1][0];
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = [
        [d[1][1] / det, -d[0][1] / det],
        [-d[1][0] / det, d[0][0] / det],
    ];
    // L = E * D^-1: the linear part of the new mapping.
    let l = [
        [
            e[0][0] * inv[0][0] + e[0][1] * inv[1][0],
            e[0][0] * inv[0][1] + e[0][1] * inv[1][1],
        ],
        [
            e[1][0] * inv[0][0] + e[1][1] * inv[1][0],
            e[1][0] * inv[0][1] + e[1][1] * inv[1][1],
        ],
    ];
    // L = Rot(theta) * diag(1/sx, 1/sy); column 0 fixes theta with sx > 0.
    let col0 = [l[0][0], l[1][0]];
    let col1 = [l[0][1], l[1][1]];
    let len0 = col0[0].hypot(col0[1]);
    if len0 < 1e-12 {
        return None;
    }
    let theta = col0[1].atan2(col0[0]);
    let (sin, cos) = theta.sin_cos();
    let along = col1[0] * -sin + col1[1] * cos;
    let shear = col1[0] * cos + col1[1] * sin;
    if along.abs() < 1e-12 {
        return None;
    }
    let to_q8 = |scale: f64| {
        (scale * 256.0)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    };
    let mut uv = FaceUv {
        offset_texels: previous.offset_texels,
        rotation_deg: theta.to_degrees().round().rem_euclid(360.0) as i16,
        scale_q8: [to_q8(1.0 / len0), to_q8(1.0 / along)],
    };
    uv.reanchor_to(target[0], raw[0], [0.0, 0.0]);
    let exact = shear.abs() <= 1e-6 * len0.max(along.abs());
    Some((uv, exact))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn textured_cube() -> Brush {
        let mut brush = Brush::cuboid([0, 0, 0], [256, 128, 64]);
        for (index, face) in brush.faces.iter_mut().enumerate() {
            face.uv = FaceUv {
                offset_texels: [3 * index as i16, -5],
                rotation_deg: 0,
                scale_q8: [256 + 64 * index as i16, 256],
            };
        }
        brush
    }

    /// Every face keeps showing the same texel at the same surface point.
    fn assert_uv_locked(before: &Brush, after: &Brush, transform: OrthoTransform) {
        for (old, new) in before.faces.iter().zip(&after.faces) {
            let old_plane = Plane::from_points(old.points).expect("valid");
            let new_plane = Plane::from_points(new.points).expect("valid");
            for point in old.points {
                let was = texel(&old_plane, &old.uv, point);
                let now = texel(&new_plane, &new.uv, transform.apply(point));
                assert!(
                    (was[0] - now[0]).abs() <= 0.51 && (was[1] - now[1]).abs() <= 0.51,
                    "texel moved from {was:?} to {now:?} under {transform:?}"
                );
            }
        }
    }

    fn assert_outward(brush: &Brush) {
        let solved = brush.solve();
        let centre =
            [0, 1, 2].map(|axis| ((solved.min[axis] + solved.max[axis]) / 2.0).round() as i32);
        for face in &brush.faces {
            let plane = Plane::from_points(face.points).expect("valid");
            assert!(plane.side(centre) < 0, "face {face:?} points inward");
        }
    }

    #[test]
    fn four_quarter_turns_are_the_identity() {
        let start = textured_cube();
        let mut brush = start.clone();
        let turn = OrthoTransform {
            pivot: [100, 0, 30],
            quarter_turns: 1,
            ..OrthoTransform::default()
        };
        for _ in 0..4 {
            brush.transform_ortho_with_uv_lock(turn);
        }
        // Geometry, rotation and scale come back exactly. Offsets are i16
        // texels, so a fractional scale can bank up to half a texel of
        // rounding per turn, the same limit translate_with_uv_lock has.
        for (back, original) in brush.faces.iter().zip(&start.faces) {
            assert_eq!(back.points, original.points);
            assert_eq!(back.uv.rotation_deg, original.uv.rotation_deg);
            assert_eq!(back.uv.scale_q8, original.uv.scale_q8);
            for axis in 0..2 {
                let drift = (back.uv.offset_texels[axis] - original.uv.offset_texels[axis]).abs();
                assert!(drift <= 2, "offset drifted {drift} texels: {back:?}");
            }
        }
    }

    #[test]
    fn a_quarter_turn_swaps_the_footprint_and_keeps_textures() {
        let before = textured_cube();
        let mut after = before.clone();
        let turn = OrthoTransform {
            quarter_turns: 1,
            ..OrthoTransform::default()
        };
        assert_eq!(after.transform_ortho_with_uv_lock(turn), 0);
        let solved = after.solve();
        // 256 along X and 64 along Z become 64 along X and 256 along Z.
        assert_eq!(
            (solved.min, solved.max),
            ([-64.0, 0.0, 0.0], [0.0, 128.0, 256.0])
        );
        assert_outward(&after);
        assert_uv_locked(&before, &after, turn);
    }

    #[test]
    fn a_mirror_keeps_normals_outward_and_textures_locked() {
        for axis in 0..3 {
            let before = textured_cube();
            let mut after = before.clone();
            let flip = OrthoTransform {
                pivot: [512, 0, 0],
                mirror_axis: Some(axis),
                quarter_turns: 3,
                translate: [16, 32, -48],
            };
            assert_eq!(after.transform_ortho_with_uv_lock(flip), 0);
            assert_outward(&after);
            assert_uv_locked(&before, &after, flip);
        }
    }

    #[test]
    fn a_plain_move_matches_translate_with_uv_lock() {
        let mut ours = textured_cube();
        let mut theirs = textured_cube();
        let delta = [48, -16, 80];
        ours.transform_ortho_with_uv_lock(OrthoTransform {
            translate: delta,
            ..OrthoTransform::default()
        });
        theirs.translate_with_uv_lock(delta, BRUSH_UV_UNITS_PER_TEXEL);
        assert_eq!(ours, theirs);
    }
}
