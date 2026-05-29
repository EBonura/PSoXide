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
