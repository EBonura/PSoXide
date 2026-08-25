//! Camera/GTE setup helpers for the editor 3D preview.

use super::*;

/// Configure the host-side GTE so subsequent `project_vertex` /
/// `project_triangle` calls produce screen-space coords for the
/// requested editor camera.
pub(super) fn setup_gte_for_camera(camera: ViewportCameraState) -> psx_engine::WorldCamera {
    let cos_p = cos_q12_turn(camera.pitch_q12);
    let sin_p = sin_q12_turn(camera.pitch_q12);
    let cos_y = cos_q12_turn(camera.yaw_q12);
    let sin_y = sin_q12_turn(camera.yaw_q12);
    let anchor = camera.anchor_i32();
    let [cam_x, cam_y, cam_z] = camera.position_i32();

    // View rotation: world →camera. Built so that:
    //   row0 = right (= +X in camera space)
    //   row1 = -up   (PSX screen Y points down, so we flip)
    //   row2 = forward (= +Z in camera space; camera looks along view direction)
    // Matches `psx_engine::render3d::camera_gte_view_matrix`.
    let view = preview_view_rotation(camera);

    // Vertex emit will subtract `anchor` from each world coord
    // (see `world_to_view`), so anything inside ±i16 of the
    // camera anchor is safe to GTE-project. Compose the GTE
    // translation around that anchor: view·(anchor - cam_world)
    // = view·(-cam_local) where cam_local lives entirely within
    // a small local range. Orbit anchors on its target; Free anchors
    // on its position, which keeps large authored rooms camera-local.
    // Zoom-out depth guard: projected depth lives in the GTE's 16-bit
    // SZ range, so a large orbit radius saturates it long before the
    // radius clamp and the view distorts instead of receding. A
    // perspective image is invariant under uniformly scaling the scene
    // and the camera offset together, so once the radius leaves the
    // safe band, shift BOTH the anchor-relative vertices
    // (`world_to_view`) and the camera offset down by 2^k. Keeps
    // radius>>k <= 24576; with anchor-relative geometry <= 32767 the
    // worst-case depth stays inside 65535 for any k.
    let mut view_shift = 0u32;
    if camera.mode == psxed_ui::ViewportCameraMode::Orbit {
        while (camera.radius >> view_shift) > 24_576 {
            view_shift += 1;
        }
    }

    let cam_local = [
        (cam_x - anchor[0]) >> view_shift,
        (cam_y - anchor[1]) >> view_shift,
        (cam_z - anchor[2]) >> view_shift,
    ];
    let tr = Vec3I32::new(
        -dot_view_world(view.m[0], cam_local),
        -dot_view_world(view.m[1], cam_local),
        -dot_view_world(view.m[2], cam_local),
    );

    set_view_anchor(anchor, view_shift);
    gte_scene::load_rotation(&view);
    gte_scene::load_translation(tr);
    gte_scene::set_screen_offset(SCREEN_CX << 16, SCREEN_CY << 16);
    gte_scene::set_projection_plane(PROJ_H as u16);

    // Build a `WorldCamera` matching the same basis so the
    // engine model pass composes joint transforms against the
    // same view matrix the editor geometry just loaded.
    psx_engine::WorldCamera::from_basis(
        psx_engine::WorldProjection::new(SCREEN_CX as i16, SCREEN_CY as i16, PROJ_H, 32),
        psx_engine::WorldVertex::new(cam_x, cam_y, cam_z),
        psx_engine::Q12::from_raw(sin_y),
        psx_engine::Q12::from_raw(cos_y),
        psx_engine::Q12::from_raw(sin_p),
        psx_engine::Q12::from_raw(cos_p),
    )
}

/// World-to-view Q12 rotation shared by geometry setup and the exact runtime
/// sky projection kernels.
pub(super) fn preview_view_rotation(camera: ViewportCameraState) -> Mat3I16 {
    let cos_p = cos_q12_turn(camera.pitch_q12);
    let sin_p = sin_q12_turn(camera.pitch_q12);
    let cos_y = cos_q12_turn(camera.yaw_q12);
    let sin_y = sin_q12_turn(camera.yaw_q12);
    Mat3I16 {
        m: [
            [clamp_i16(cos_y), 0, clamp_i16(-sin_y)],
            [
                clamp_i16(-((sin_y * sin_p) >> 12)),
                clamp_i16(-cos_p),
                clamp_i16(-((cos_y * sin_p) >> 12)),
            ],
            [
                clamp_i16(-((sin_y * cos_p) >> 12)),
                clamp_i16(sin_p),
                clamp_i16(-((cos_y * cos_p) >> 12)),
            ],
        ],
    }
}

/// Shared anchor that `world_to_view` subtracts from each vertex
/// before squashing to `i16`, plus the zoom-out shift applied to both
/// the vertices and the camera offset (see `setup_gte_for_camera`).
/// Set per-frame to the camera anchor so the emitted vertices stay
/// anchor-relative -- the GTE absorbs the offset via its translation
/// register. Without this, a single 32-sector room
/// (32 × 1024 = 32 768) sits exactly on the i16 cliff.
static VIEW_ANCHOR: std::sync::Mutex<([i32; 3], u32)> = std::sync::Mutex::new(([0, 0, 0], 0));

pub(super) fn set_view_anchor(anchor: [i32; 3], shift: u32) {
    if let Ok(mut a) = VIEW_ANCHOR.lock() {
        *a = (anchor, shift);
    }
}

pub(super) fn view_anchor() -> ([i32; 3], u32) {
    VIEW_ANCHOR.lock().map(|a| *a).unwrap_or(([0, 0, 0], 0))
}

/// `view_row · world_pos` with the >>12 the GTE does internally for
/// matrix * world products.
pub(super) fn dot_view_world(row: [i16; 3], v: [i32; 3]) -> i32 {
    let a = (row[0] as i32).saturating_mul(v[0]);
    let b = (row[1] as i32).saturating_mul(v[1]);
    let c = (row[2] as i32).saturating_mul(v[2]);
    a.saturating_add(b).saturating_add(c) >> 12
}

pub(super) fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Thin alias for the shared Q12 multiply; the editor preview uses it for
/// camera-basis and gizmo math beyond the rotation helpers.
pub(super) fn mul_q12_i32(value: i32, q12: i32) -> i32 {
    psxed_project::spatial::mul_q12(value, q12)
}

pub(super) fn sin_q12_turn(angle_q12: u16) -> i32 {
    psx_engine::Angle::from_q12(angle_q12).sin().raw()
}

pub(super) fn cos_q12_turn(angle_q12: u16) -> i32 {
    psx_engine::Angle::from_q12(angle_q12).cos().raw()
}

/// Squash a world-space i32 corner into the i16 the GTE V0 register
/// expects. Subtracts the per-frame view anchor (= camera target)
/// first so the emitted coord is anchor-relative. With sector_size
/// 1024, this gives ±32 sectors of headroom from the camera target
/// before clamp truncation kicks in -- comfortably the editor's
/// budget cap.
///
/// Saturates out-of-range authoring coordinates rather than crashing
/// the editor. Runtime room windows should keep PS1-visible geometry
/// local, but the editor can inspect imported TR levels whose full
/// world coordinates are much larger than a single GTE input range.
pub(super) fn world_to_view(world: [i32; 3]) -> Vec3I16 {
    let (a, shift) = view_anchor();
    let lx = (world[0] - a[0]) >> shift;
    let ly = (world[1] - a[1]) >> shift;
    let lz = (world[2] - a[2]) >> shift;
    Vec3I16::new(clamp_i16(lx), clamp_i16(ly), clamp_i16(lz))
}
