//! Shared editor/runtime spatial helpers: fixed-point rotation and Euler
//! conversions used by the cooker and the editor gizmos.

use psx_engine::Angle;

/// Integer room-space position `[x, y, z]`.
pub type RoomPoint = [i32; 3];

/// Floating room-space position `[x, y, z]`.
pub type RoomPointF = [f32; 3];

/// Q12 fixed-point multiply: `(value * q12) >> 12`, saturated to `i32`.
pub fn mul_q12(value: i32, q12: i32) -> i32 {
    (((value as i64) * (q12 as i64)) >> 12).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Rotate `v` about the X axis by a Q12 turn angle (4096 units per turn).
pub fn rotate_x_q12(v: RoomPoint, angle_q12: u16) -> RoomPoint {
    let angle = Angle::from_q12(angle_q12);
    let (s, c) = (angle.sin().raw(), angle.cos().raw());
    [
        v[0],
        mul_q12(v[1], c) - mul_q12(v[2], s),
        mul_q12(v[1], s) + mul_q12(v[2], c),
    ]
}

/// Rotate `v` about the Y axis by a Q12 turn angle.
pub fn rotate_y_q12(v: RoomPoint, angle_q12: u16) -> RoomPoint {
    let angle = Angle::from_q12(angle_q12);
    let (s, c) = (angle.sin().raw(), angle.cos().raw());
    [
        mul_q12(v[0], c) + mul_q12(v[2], s),
        v[1],
        -mul_q12(v[0], s) + mul_q12(v[2], c),
    ]
}

/// Rotate `v` about the Z axis by a Q12 turn angle.
pub fn rotate_z_q12(v: RoomPoint, angle_q12: u16) -> RoomPoint {
    let angle = Angle::from_q12(angle_q12);
    let (s, c) = (angle.sin().raw(), angle.cos().raw());
    [
        mul_q12(v[0], c) - mul_q12(v[1], s),
        mul_q12(v[0], s) + mul_q12(v[1], c),
        v[2],
    ]
}

/// Apply an authored Euler rotation in the editor/runtime order: pitch about
/// X, then yaw about Y, then roll about Z (all Q12 turn angles). The editor
/// card, selection outline, cooked record, and runtime draw path agree
/// because they share this one function.
pub fn rotate_euler_local_q12(v: RoomPoint, pitch: u16, yaw: u16, roll: u16) -> RoomPoint {
    rotate_z_q12(rotate_y_q12(rotate_x_q12(v, pitch), yaw), roll)
}

/// Convert an authored Euler angle in degrees to a PSX Q12 angle unit
/// (`0..4096`). Single source of truth for the editor preview and the
/// playtest cooker, so authored facing can't diverge between what the user
/// sees and what ships.
pub fn euler_degrees_to_q12(degrees: f32) -> u16 {
    let normalised = degrees.rem_euclid(360.0);
    (normalised * (4096.0 / 360.0)) as i32 as u16
}

/// Reference frame for an interactive rotation delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationSpace {
    /// Rotate about the world axes.
    Global,
    /// Rotate about the node's own (already-rotated) axes.
    Local,
}

/// Column-vector rotation matrix for the authored Euler order
/// (`Rz(roll) * Ry(yaw) * Rx(pitch)`), the f32 mirror of
/// [`rotate_euler_local_q12`] / the runtime's `euler_q12_rotation`.
/// `m[row][col]`; world direction of the node's local axis `i` is
/// column `i`.
pub fn euler_degrees_to_matrix(degrees: [f32; 3]) -> [[f32; 3]; 3] {
    let [x, y, z] = degrees.map(f32::to_radians);
    let (sx, cx) = x.sin_cos();
    let (sy, cy) = y.sin_cos();
    let (sz, cz) = z.sin_cos();
    // Rz * Ry * Rx expanded.
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}

/// Extract authored Euler degrees back out of a rotation matrix in the
/// [`euler_degrees_to_matrix`] convention. Angles return normalised to
/// `[0, 360)`. At the `|pitch around Y| = 90 deg` gimbal singularity the
/// X/Z split is ambiguous; X is pinned to zero there.
pub fn matrix_to_euler_degrees(m: &[[f32; 3]; 3]) -> [f32; 3] {
    let sy = -m[2][0];
    let cy = (m[2][1] * m[2][1] + m[2][2] * m[2][2]).sqrt();
    let (x, y, z) = if cy > 1e-5 {
        (m[2][1].atan2(m[2][2]), sy.atan2(cy), m[1][0].atan2(m[0][0]))
    } else {
        // Gimbal lock: only x+z (or x-z) is observable; put it all in z.
        (0.0, sy.atan2(cy), (-m[0][1]).atan2(m[1][1]))
    };
    [x, y, z].map(|r| r.to_degrees().rem_euclid(360.0))
}

fn mat3_mul_f32(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn single_axis_matrix(axis: usize, degrees: f32) -> [[f32; 3]; 3] {
    let mut euler = [0.0f32; 3];
    euler[axis] = degrees;
    euler_degrees_to_matrix(euler)
}

/// Apply an interactive rotation delta about one axis to an authored
/// Euler triple and return the new Euler triple.
///
/// `Global` composes the delta in the world frame (`R_delta * R`),
/// `Local` in the node's own frame (`R * R_delta`). Editing a single
/// Euler component directly (the old gizmo behaviour) is only a true
/// axis rotation while the other two components are zero; this stays
/// correct for any starting orientation.
pub fn rotate_euler_degrees(
    start_degrees: [f32; 3],
    axis: usize,
    delta_degrees: f32,
    space: RotationSpace,
) -> [f32; 3] {
    debug_assert!(axis < 3);
    // Single-axis starts keep exact degree arithmetic (no matrix
    // round-trip noise) when the delta spins the same axis: both
    // spaces agree there.
    let others_zero = (0..3).all(|i| i == axis || start_degrees[i] == 0.0);
    if others_zero {
        let mut out = start_degrees;
        out[axis] = (start_degrees[axis] + delta_degrees).rem_euclid(360.0);
        return out;
    }
    let start = euler_degrees_to_matrix(start_degrees);
    let delta = single_axis_matrix(axis, delta_degrees);
    let combined = match space {
        RotationSpace::Global => mat3_mul_f32(&delta, &start),
        RotationSpace::Local => mat3_mul_f32(&start, &delta),
    };
    matrix_to_euler_degrees(&combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euler_degrees_to_q12_quarters_and_wraps() {
        assert_eq!(euler_degrees_to_q12(0.0), 0);
        assert_eq!(euler_degrees_to_q12(90.0), 1024);
        assert_eq!(euler_degrees_to_q12(180.0), 2048);
        assert_eq!(euler_degrees_to_q12(270.0), 3072);
        assert_eq!(euler_degrees_to_q12(360.0), 0);
        assert_eq!(euler_degrees_to_q12(-90.0), 3072);
    }

    fn assert_euler_approx(actual: [f32; 3], expected: [f32; 3]) {
        for i in 0..3 {
            // Compare on the circle so 359.99 matches 0.0.
            let delta = (actual[i] - expected[i]).rem_euclid(360.0);
            let distance = delta.min(360.0 - delta);
            assert!(
                distance < 0.01,
                "axis {i}: actual {actual:?} expected {expected:?}"
            );
        }
    }

    #[test]
    fn euler_matrix_roundtrips_across_angle_grid() {
        for x in [0.0f32, 30.0, 90.0, 145.0, 210.0, 300.0] {
            for y in [0.0f32, 45.0, 120.0, 270.0] {
                for z in [0.0f32, 60.0, 180.0, 315.0] {
                    let degrees = [x, y, z];
                    let roundtrip = matrix_to_euler_degrees(&euler_degrees_to_matrix(degrees));
                    // Compare matrices, not angles: distinct Euler
                    // triples can encode the same rotation.
                    let a = euler_degrees_to_matrix(degrees);
                    let b = euler_degrees_to_matrix(roundtrip);
                    for i in 0..3 {
                        for j in 0..3 {
                            assert!(
                                (a[i][j] - b[i][j]).abs() < 1e-4,
                                "degrees {degrees:?} roundtrip {roundtrip:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn euler_matrix_matches_q12_vector_path() {
        // The f32 matrix and the Q12 vector helper must agree, or the
        // gizmo would drift from the cooked/runtime orientation.
        let degrees = [30.0f32, 75.0, 285.0];
        let m = euler_degrees_to_matrix(degrees);
        let v = [3000i32, -1500, 700];
        let q12 = rotate_euler_local_q12(
            v,
            euler_degrees_to_q12(degrees[0]),
            euler_degrees_to_q12(degrees[1]),
            euler_degrees_to_q12(degrees[2]),
        );
        for i in 0..3 {
            let f = m[i][0] * v[0] as f32 + m[i][1] * v[1] as f32 + m[i][2] * v[2] as f32;
            assert!(
                (f - q12[i] as f32).abs() < 8.0,
                "axis {i}: f32 {f} q12 {}",
                q12[i]
            );
        }
    }

    #[test]
    fn rotate_euler_single_axis_keeps_exact_degrees() {
        assert_euler_approx(
            rotate_euler_degrees([0.0, 45.0, 0.0], 1, 30.0, RotationSpace::Global),
            [0.0, 75.0, 0.0],
        );
        assert_euler_approx(
            rotate_euler_degrees([0.0, 45.0, 0.0], 1, -90.0, RotationSpace::Local),
            [0.0, 315.0, 0.0],
        );
    }

    #[test]
    fn rotate_euler_global_spins_world_axis() {
        // A prop pitched 90 deg nose-down: rotating +90 about the WORLD
        // Y must keep the nose down and swing it a quarter turn, which
        // in this convention is exactly yaw+90 applied after the pitch.
        let start = [90.0f32, 0.0, 0.0];
        let rotated = rotate_euler_degrees(start, 1, 90.0, RotationSpace::Global);
        let expected = mat3_mul_f32(
            &euler_degrees_to_matrix([0.0, 90.0, 0.0]),
            &euler_degrees_to_matrix(start),
        );
        let actual = euler_degrees_to_matrix(rotated);
        for i in 0..3 {
            for j in 0..3 {
                assert!((actual[i][j] - expected[i][j]).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn rotate_euler_local_spins_node_axis() {
        // Same pitched prop rotating about its LOCAL Y (the axis now
        // pointing along world -Z): result must equal R * Ry(90).
        let start = [90.0f32, 0.0, 0.0];
        let rotated = rotate_euler_degrees(start, 1, 90.0, RotationSpace::Local);
        let expected = mat3_mul_f32(
            &euler_degrees_to_matrix(start),
            &euler_degrees_to_matrix([0.0, 90.0, 0.0]),
        );
        let actual = euler_degrees_to_matrix(rotated);
        for i in 0..3 {
            for j in 0..3 {
                assert!((actual[i][j] - expected[i][j]).abs() < 1e-4);
            }
        }
        // And local differs from global here: the two compositions
        // disagree once the start orientation is non-trivial.
        let global = rotate_euler_degrees(start, 1, 90.0, RotationSpace::Global);
        let global_m = euler_degrees_to_matrix(global);
        let mut differs = false;
        for i in 0..3 {
            for j in 0..3 {
                differs |= (actual[i][j] - global_m[i][j]).abs() > 1e-3;
            }
        }
        assert!(differs);
    }

    #[test]
    fn q12_transforms_scale_and_identity() {
        assert_eq!(mul_q12(4096, 4096), 4096); // 1.0 * 1.0
        assert_eq!(mul_q12(2048, 2048), 1024); // 0.5 * 0.5 = 0.25
        let v = [123, -456, 789];
        // A zero rotation is the identity on every axis.
        assert_eq!(rotate_euler_local_q12(v, 0, 0, 0), v);
        assert_eq!(rotate_x_q12(v, 0), v);
        assert_eq!(rotate_y_q12(v, 0), v);
        assert_eq!(rotate_z_q12(v, 0), v);
    }
}
