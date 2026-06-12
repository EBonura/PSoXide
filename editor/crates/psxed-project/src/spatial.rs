//! Shared editor/runtime spatial conversion helpers.
//!
//! The editor has two room-space targets that are intentionally
//! different:
//!
//! - **Preview world** keeps authored cells at their physical
//!   editor-world coordinates. Room `origin` is part of the result,
//!   so growing a room toward negative X/Z does not visually move
//!   existing geometry, lights, models, or selection bounds.
//! - **Cooked room local** matches the compact `.psxw` layout. Room
//!   geometry is array-rooted at `(0, 0)`, so entity records use the
//!   current array centre and ignore `origin`; `origin` is emitted in
//!   the manifest only as editor metadata.
//!
//! Keeping both conversions named here is deliberate: call sites
//! should choose the space they need rather than re-derive a formula.

use crate::{GridCellBounds, GridDirection, Transform3, WorldGrid};
use psx_engine::Angle;

/// Integer room-space position `[x, y, z]`.
pub type RoomPoint = [i32; 3];

/// Floating room-space position `[x, y, z]`.
pub type RoomPointF = [f32; 3];

/// Origin of a node in editor preview world space.
///
/// This is origin-aware and should be used by editor 3D rendering,
/// picking/bounds, gizmos, and any other authoring-surface feature
/// that must line up with what the user sees.
pub fn node_preview_origin(grid: &WorldGrid, transform: &Transform3) -> RoomPoint {
    let xz = grid.editor_to_room_local([transform.translation[0], transform.translation[2]]);
    [
        xz[0] as i32,
        (transform.translation[1] * grid.sector_size as f32) as i32,
        xz[2] as i32,
    ]
}

/// Origin of a floor-anchored node in editor preview world space.
/// X/Z come from the authored transform; Y is sampled from the
/// floor directly under that X/Z when one exists.
pub fn floor_anchored_node_preview_origin(grid: &WorldGrid, transform: &Transform3) -> RoomPoint {
    let mut origin = node_preview_origin(grid, transform);
    if let Some(floor_y) = grid.floor_height_at_room_local(origin[0], origin[2]) {
        origin[1] = floor_y;
    }
    origin
}

/// Floating-point form of [`node_preview_origin`].
pub fn node_preview_origin_f32(grid: &WorldGrid, transform: &Transform3) -> RoomPointF {
    let xz = grid.editor_to_room_local([transform.translation[0], transform.translation[2]]);
    [
        xz[0],
        transform.translation[1] * grid.sector_size as f32,
        xz[2],
    ]
}

/// Centre of a selectable node bound in editor preview world space.
pub fn node_preview_bounds_center(
    grid: &WorldGrid,
    transform: &Transform3,
    half_extents: [f32; 3],
) -> RoomPointF {
    let origin = node_preview_origin_f32(grid, transform);
    [origin[0], origin[1] + half_extents[1], origin[2]]
}

/// Centre of a selectable floor-anchored node bound in editor
/// preview world space.
pub fn floor_anchored_node_preview_bounds_center(
    grid: &WorldGrid,
    transform: &Transform3,
    half_extents: [f32; 3],
) -> RoomPointF {
    let origin = floor_anchored_node_preview_origin(grid, transform);
    [
        origin[0] as f32,
        origin[1] as f32 + half_extents[1],
        origin[2] as f32,
    ]
}

/// Origin of a node in cooked `.psxw` room-local space.
///
/// This intentionally ignores [`WorldGrid::origin`]. The cooked room
/// geometry is array-rooted, so a node authored at editor `(0, 0)`
/// belongs at the centre of the current array.
pub fn node_cooked_room_local_origin(grid: &WorldGrid, transform: &Transform3) -> RoomPoint {
    let s = grid.sector_size as f32;
    [
        ((transform.translation[0] + grid.width as f32 * 0.5) * s) as i32,
        (transform.translation[1] * s) as i32,
        ((transform.translation[2] + grid.depth as f32 * 0.5) * s) as i32,
    ]
}

/// Geometric centre of a room in editor preview world space.
pub fn room_preview_center(grid: &WorldGrid) -> RoomPoint {
    let center = room_preview_center_f32(grid);
    [center[0] as i32, 0, center[2] as i32]
}

/// Floating-point geometric centre of a room in editor preview world
/// space.
pub fn room_preview_center_f32(grid: &WorldGrid) -> RoomPointF {
    grid.editor_to_room_local([0.0, 0.0])
}

/// Convert an authored light radius, expressed in sectors, to engine
/// world units.
pub fn light_radius_engine_units(grid: &WorldGrid, radius_sectors: f32) -> i32 {
    (radius_sectors * grid.sector_size as f32) as i32
}

/// Convert an authored light radius to the manifest wire format.
pub fn light_radius_record_units(grid: &WorldGrid, radius_sectors: f32) -> u16 {
    (radius_sectors * grid.sector_size as f32).clamp(1.0, u16::MAX as f32) as u16
}

/// Pick the editor cardinal wall edge from a point's offset relative
/// to a cell centre. Editor convention: North = +Z, South = -Z.
pub fn editor_wall_direction_from_offset(dx: f32, dz: f32) -> GridDirection {
    if dz.abs() > dx.abs() {
        if dz >= 0.0 {
            GridDirection::North
        } else {
            GridDirection::South
        }
    } else if dx >= 0.0 {
        GridDirection::East
    } else {
        GridDirection::West
    }
}

/// Inward-facing X/Z normal for an editor cardinal wall edge.
pub const fn editor_wall_inward_normal(direction: GridDirection) -> Option<[i32; 2]> {
    match direction {
        GridDirection::North => Some([0, -1]),
        GridDirection::East => Some([-1, 0]),
        GridDirection::South => Some([0, 1]),
        GridDirection::West => Some([1, 0]),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

/// Build cell bounds from a world-cell coordinate. Used for off-grid
/// paint ghosts before the grid has been grown to contain the cell.
pub const fn cell_bounds_from_world_cell(
    world_cell_x: i32,
    world_cell_z: i32,
    sector_size: i32,
) -> GridCellBounds {
    let x0 = world_cell_x * sector_size;
    let z0 = world_cell_z * sector_size;
    GridCellBounds {
        x0,
        x1: x0 + sector_size,
        z0,
        z1: z0 + sector_size,
    }
}

/// Wall outline corners in editor preview world space, optionally
/// inset by `lift` along the wall's inward normal.
pub fn editor_wall_outline_corners(
    bounds: GridCellBounds,
    direction: GridDirection,
    heights: [i32; 4],
    lift: i32,
) -> Option<[RoomPoint; 4]> {
    let (bl, br) = bounds.wall_endpoints_xz(direction)?;
    let [nx, nz] = editor_wall_inward_normal(direction).unwrap_or([0, 0]);
    Some([
        [bl[0] + lift * nx, heights[0], bl[1] + lift * nz],
        [br[0] + lift * nx, heights[1], br[1] + lift * nz],
        [br[0] + lift * nx, heights[2], br[1] + lift * nz],
        [bl[0] + lift * nx, heights[3], bl[1] + lift * nz],
    ])
}

// --- Q12 fixed-point transforms -------------------------------------------
//
// Authored rotations must transform a vertex identically in the editor
// preview, the playtest cooker, and the runtime. These primitives are the one
// definition of that math; the preview and the cooker both call them rather
// than re-deriving the formula. (Two divergent degrees->Q12 conversions, with
// different integer casts, previously lived in `editor_preview` and
// `playtest` -- the source of the authored-facing drift.)

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

    #[test]
    fn preview_origin_accounts_for_negative_room_origin() {
        let mut grid = WorldGrid::stone_room(4, 7, 1024, None, None);
        grid.origin = [-1, -3];
        let transform = Transform3 {
            translation: [0.5, 0.25, -1.0],
            ..Transform3::default()
        };

        let origin = node_preview_origin(&grid, &transform);
        let expected_xz = grid.editor_to_room_local([0.5, -1.0]);
        assert_eq!(origin, [expected_xz[0] as i32, 256, expected_xz[2] as i32]);
        assert_ne!(
            origin,
            [
                ((transform.translation[0] + grid.width as f32 * 0.5) * 1024.0) as i32,
                256,
                ((transform.translation[2] + grid.depth as f32 * 0.5) * 1024.0) as i32,
            ]
        );
    }

    #[test]
    fn cooked_room_local_origin_is_array_rooted() {
        let mut grid = WorldGrid::stone_room(4, 7, 1024, None, None);
        grid.origin = [-1, -3];
        let transform = Transform3 {
            translation: [0.5, 0.25, -1.0],
            ..Transform3::default()
        };

        assert_eq!(
            node_cooked_room_local_origin(&grid, &transform),
            [2560, 256, 2560]
        );
    }

    #[test]
    fn wall_direction_and_outline_share_editor_convention() {
        assert_eq!(
            editor_wall_direction_from_offset(0.1, 0.9),
            GridDirection::North
        );
        assert_eq!(
            editor_wall_direction_from_offset(0.9, 0.9),
            GridDirection::East
        );

        let bounds = GridCellBounds {
            x0: 0,
            x1: 1024,
            z0: 0,
            z1: 1024,
        };
        assert_eq!(
            editor_wall_outline_corners(bounds, GridDirection::North, [0, 0, 1024, 1024], 4),
            Some([
                [0, 0, 1020],
                [1024, 0, 1020],
                [1024, 1024, 1020],
                [0, 1024, 1020]
            ])
        );
    }
}
