//! Pieces of the world runtime shared by every world kind.
//!
//! The camera conventions, constants, visibility bounds and trace counter
//! the grid runtime ([`crate::WorldRuntime`]) and the rest of the playtest
//! share.

use psx_engine::Mat3I16;
use psx_engine::WorldCamera;
use psx_grid::render::Camera;
use psx_gte::math::Vec3I32;

/// Collision body of a Character-less player, runtime units.
pub(super) const WORLD_PLAYER_RADIUS: i32 = 1;
/// Collision body height of a Character-less player, runtime units.
pub(super) const WORLD_PLAYER_HEIGHT: i32 = 4;
/// Walking speed of the Character-less debug body. Character-backed projects
/// keep their authored speed.
pub(super) const WORLD_FALLBACK_PLAYER_SPEED: i32 = 4 << 8;
/// Compact third-person rig for the Character-less debug controller. The
/// ordinary authored camera remains authoritative once a Character is bound.
/// Keep the boom close to the invisible debug body while it turns; this reads
/// as a player-height exploration camera and prevents annex columns from
/// filling the frame while the collision-driven route rounds a doorway.
pub(super) const WORLD_FALLBACK_CAMERA_DISTANCE: i32 = 24;
pub(super) const WORLD_FALLBACK_CAMERA_HEIGHT: i32 = 40;
pub(super) const WORLD_FALLBACK_CAMERA_TARGET_HEIGHT: i32 = 32;
pub(super) const WORLD_FALLBACK_CAMERA_CLEARANCE: i32 = 8;
/// Boom-to-wall margin for the point-traced follow camera.
pub(super) const WORLD_CAMERA_WALL_MARGIN: i32 = 16;
pub(super) const WORLD_FALLBACK_CAMERA_MARGIN: i32 = 4;
pub(super) const WORLD_USE_DISTANCE: i32 = 256;

/// Dynamic world bounds used to activate actors and instances by room.
/// Coordinates use the playtest's integer engine-unit convention.
#[derive(Copy, Clone, Debug, Default, Eq)]
pub(super) struct VisibilityBounds {
    min: [i32; 3],
    max: [i32; 3],
}

// Word by word: the derived `[i32; 3] == [i32; 3]` lowers to a call to
// memcmp on MIPS, and the visibility cache compares bounds for every
// linked actor and instance each frame (about 2.3M instructions over the
// Cortex whole-level route, all inside memcmp).
impl PartialEq for VisibilityBounds {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        (self.min[0] ^ other.min[0])
            | (self.min[1] ^ other.min[1])
            | (self.min[2] ^ other.min[2])
            | (self.max[0] ^ other.max[0])
            | (self.max[1] ^ other.max[1])
            | (self.max[2] ^ other.max[2])
            == 0
    }
}

impl VisibilityBounds {
    pub(super) const fn min(&self) -> [i32; 3] {
        self.min
    }

    pub(super) const fn max(&self) -> [i32; 3] {
        self.max
    }

    pub(super) const EMPTY: Self = Self {
        min: [0; 3],
        max: [0; 3],
    };

    pub(super) fn cylinder(position: [i32; 3], radius: i32, height: i32) -> Self {
        let radius = radius.max(0);
        let height = height.max(1);
        Self {
            min: [
                position[0].saturating_sub(radius),
                position[1],
                position[2].saturating_sub(radius),
            ],
            max: [
                position[0].saturating_add(radius),
                position[1].saturating_add(height),
                position[2].saturating_add(radius),
            ],
        }
    }
}

/// Counts the traces a camera update asks of its provider.
pub(super) struct CountingTraceProvider<'a, P: ?Sized> {
    inner: &'a mut P,
    traces: u32,
}

impl<'a, P: ?Sized> CountingTraceProvider<'a, P> {
    pub(super) fn new(inner: &'a mut P) -> Self {
        Self { inner, traces: 0 }
    }

    pub(super) fn traces(&self) -> u32 {
        self.traces
    }
}

impl<P: psx_engine::CollisionTraceProvider + ?Sized> psx_engine::CollisionTraceProvider
    for CountingTraceProvider<'_, P>
{
    fn trace_into(
        &mut self,
        query: psx_engine::CollisionTraceQuery,
        output: &mut psx_engine::CollisionTrace,
    ) -> bool {
        self.traces += 1;
        self.inner.trace_into(query, output)
    }
}

/// The world pass view rotation from the camera's exact Q12 trig, in the
/// PXBSP convention [`view_camera`] derives its angles in: PXBSP yaw is
/// the orbit yaw plus a quarter turn and PXBSP pitch the negated look
/// pitch, so `sin(yaw + 90) = cos(yaw)`, `cos(yaw + 90) = -sin(yaw)`,
/// `sin(-pitch) = -sin(pitch)`. No table, no angle recovery: the world
/// rotates by exactly the camera the model pass renders with.
pub(super) fn world_view_rotation(camera: WorldCamera) -> Mat3I16 {
    let q12 = |value: i32| value.clamp(-0x1000, 0x1000) as i16;
    psx_grid::render::view_rotation(
        q12(-camera.sin_pitch.raw()),
        q12(camera.cos_pitch.raw()),
        q12(camera.cos_yaw.raw()),
        q12(-camera.sin_yaw.raw()),
    )
}

pub(super) fn view_camera(camera: WorldCamera) -> Camera {
    let orbit_yaw = angle_q12_from_basis(camera.sin_yaw.raw(), camera.cos_yaw.raw());
    // WorldCamera stores the target-to-camera orbit angle with the engine's
    // `x = sin(yaw), z = cos(yaw)` convention; the view direction is the
    // opposite. PXBSP zero yaw looks along +X and its yaw turns the same way
    // as the engine's now that `load_pxbsp_view` is a proper rotation (the
    // old `3072 - yaw` compensated its mirrored remap).
    let yaw = orbit_yaw.wrapping_add(1024) & 0x0fff;
    let look_pitch = signed_quarter_angle_q12(camera.sin_pitch.raw(), camera.cos_pitch.raw());
    let pitch = (-look_pitch) as u16 & 0x0fff;
    Camera {
        origin: Vec3I32 {
            x: camera.position.x.saturating_mul(4096),
            y: camera.position.y.saturating_mul(4096),
            z: camera.position.z.saturating_mul(4096),
        },
        angles: [pitch as i16, yaw as i16, 0],
    }
}

fn angle_q12_from_basis(sin: i32, cos: i32) -> u16 {
    if sin == 0 && cos == 0 {
        return 0;
    }
    let ax = sin.saturating_abs();
    let az = cos.saturating_abs();
    let base = if ax <= az {
        ax.saturating_mul(512) / az.max(1)
    } else {
        1024 - az.saturating_mul(512) / ax.max(1)
    };
    let angle = if cos >= 0 {
        if sin >= 0 {
            base
        } else {
            4096 - base
        }
    } else if sin >= 0 {
        2048 - base
    } else {
        2048 + base
    };
    (angle & 0x0fff) as u16
}

fn signed_quarter_angle_q12(sin: i32, cos: i32) -> i32 {
    let sin_abs = sin.saturating_abs();
    let cos_abs = cos.saturating_abs();
    let angle = if sin_abs <= cos_abs {
        sin_abs.saturating_mul(512) / cos_abs.max(1)
    } else {
        1024 - cos_abs.saturating_mul(512) / sin_abs.max(1)
    };
    if sin < 0 {
        -angle
    } else {
        angle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::WorldProjection;

    use psx_engine::Q12;

    #[test]
    fn engine_view_cardinals_map_to_pxbsp_yaw() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        // Camera at -X (looks +X) -> pxbsp 0; at -Z (looks +Z) -> 3072;
        // at +X (looks -X) -> 2048; at +Z (looks -Z) -> 1024.
        for (sin_yaw, cos_yaw, expected) in [
            (Q12::NEG_ONE, Q12::ZERO, 0),
            (Q12::ZERO, Q12::NEG_ONE, 3072),
            (Q12::ONE, Q12::ZERO, 2048),
            (Q12::ZERO, Q12::ONE, 1024),
        ] {
            let camera = WorldCamera::from_basis(
                projection,
                psx_engine::WorldVertex::ZERO,
                sin_yaw,
                cos_yaw,
                Q12::ZERO,
                Q12::ONE,
            );
            assert_eq!(view_camera(camera).angles[1], expected);
        }
    }

    #[test]
    fn engine_view_pitch_keeps_pxbsp_up_and_down_orientation() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        for (sin_pitch, expected) in [(Q12::NEG_ONE, 1024), (Q12::ONE, 3072)] {
            let camera = WorldCamera::from_basis(
                projection,
                psx_engine::WorldVertex::ZERO,
                Q12::NEG_ONE,
                Q12::ZERO,
                sin_pitch,
                Q12::ZERO,
            );
            assert_eq!(view_camera(camera).angles[0], expected);
        }
    }
}
