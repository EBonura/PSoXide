//! Camera, view transform and projection parameters of the grid renderer.
//!
//! The world pass runs the GTE in a Y-up frame that looks along +X at zero
//! yaw, remapped into the GTE's +Z-forward frame and scaled uniformly by
//! [`VIEW_SCALE_Q12`].

use psx_gte::math::{Mat3I16, Vec3I16 as GteVec3I16, Vec3I32 as GteVec3I32};
use psx_gte::scene;

/// Q20.12 world camera and Q0.12 turn angles.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Camera {
    /// Position in Q12 world units.
    pub origin: GteVec3I32,
    /// Pitch, yaw and roll as Q0.12 turn angles.
    pub angles: [i16; 3],
}

/// Camera transform retained for composing model-local transforms.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewTransform {
    /// View rotation including the axis remap and the view scale.
    pub rotation: Mat3I16,
    /// View-space translation of the world origin.
    pub translation: GteVec3I32,
}

/// Uniform scale baked into the view rotation: 3.0 in Q12.
///
/// What it does. Screen position is `H * x / z`, so a uniform factor on the
/// view cancels and every pixel lands where it would at scale 1. What it
/// changes is the integer resolution of the GTE's view-space results: the
/// IR registers and `SZ` hold `S` times the true view coordinate, so depth,
/// the perspective divide and the OT key the classic path stages from it
/// (`SZ / 4` under the `ZSF3 = 0x155`, `ZSF4 = 0x100` average-Z weights) are
/// all resolved in `1/S` world units.
///
/// The bounds on S, for H = 160 and a 2048-slot table:
/// - the rotation entries become `S * 4096` in a signed 16-bit matrix, so
///   `S <= 7`;
/// - the GTE divide saturates once `SZ <= H / 2` (psx-spx, RTPS/RTPT
///   divide overflow), which is a true depth of `80 / S` units: 80 at
///   S = 1, 40 at 2, 26.7 at 3, 20 at 4;
/// - the last table slot is reached at a true depth of `4 * 2048 / S`:
///   8192 at S = 1, 4096 at 2, 2731 at 3, 2048 at 4;
/// - `SZ` and the IR registers stay in range for every depth the table can
///   key, since `S * 2731 <= 4 * 2048` is far under their 16-bit limits.
///
/// So S trades the zone in front of the camera where projection saturates
/// against how far the world can be keyed before the classic path rejects
/// it. Three is the balance the depth bands and the runtime depth ranges
/// were tuned at. Anything else that sorts into the same ordering table must
/// key its depth through the same law; see [`classic_far_depth`].
///
/// The zone is `H / (2 * S)` units deep: 26 at H = 160, 53 at the third-person
/// camera's H = 320. The GTE pulls a vertex that close toward the screen
/// centre, so a wall or floor inside it was drawn warped and left holes at the
/// screen edge. The GTE flags the divide that saturates, so the shared batch
/// writer leaves such a surface to be drawn on its own, with its vertices
/// projected exactly (see `psx_engine::NearView`) and any polygon the GPU
/// would drop clipped to its window instead.
pub const VIEW_SCALE_Q12: i32 = 0x3000;

/// True view depth at which a flat surface reaches OT slot `ot_depth`, the
/// first slot the classic path rejects: `ot_depth * 4 / scale`.
///
/// Mapping `0..=this` linearly onto the world band reproduces the classic
/// triangle key for equal-depth vertices to within a slot, so the runtime
/// uses it as the far end of the depth range every non-world draw maps
/// through. Surfaces beyond it are not drawn by the world renderer at all.
pub const fn classic_far_depth(ot_depth: u16) -> i32 {
    (ot_depth as i32 * 4 * 4096 + VIEW_SCALE_Q12 / 2) / VIEW_SCALE_Q12
}

const VIEW_SCALE: i16 = VIEW_SCALE_Q12 as i16;

/// The axis remap from the world frame (`+X` forward, `+Y` up) into the GTE's
/// `+Z` forward, `-Y` up frame, carrying the view scale.
///
/// The remap is a proper rotation (determinant > 0) so the world keeps the
/// editor's handedness: view right = world +Z, view down = world -Y, view
/// depth = world +X. A `-Z` right axis would be a reflection and draw every
/// world as its mirror image while models, which use the engine camera, were
/// not mirrored.
const COORDINATES: Mat3I16 = Mat3I16 {
    m: [[0, 0, VIEW_SCALE], [0, -VIEW_SCALE, 0], [VIEW_SCALE, 0, 0]],
};

/// The exact view rotation for a camera whose pitch and yaw are known as Q12
/// sine and cosine pairs: `Rx(pitch) * Ry(yaw)`.
///
/// A third-person engine camera carries full-precision trig and the model
/// pass rotates with it. Quantising that camera to a 256-step table for the
/// world pass rotated the world by up to 0.7 degrees against the actors
/// standing in it, which at 250 units of depth is three units of floor:
/// every actor looked a little above or below the ground depending on where
/// the camera sat between two table steps.
pub fn view_rotation(sin_pitch: i16, cos_pitch: i16, sin_yaw: i16, cos_yaw: i16) -> Mat3I16 {
    let pitch = Mat3I16 {
        m: [
            [0x1000, 0, 0],
            [0, cos_pitch, 0i16.wrapping_sub(sin_pitch)],
            [0, sin_pitch, cos_pitch],
        ],
    };
    let yaw = Mat3I16 {
        m: [
            [cos_yaw, 0, sin_yaw],
            [0, 0x1000, 0],
            [0i16.wrapping_sub(sin_yaw), 0, cos_yaw],
        ],
    };
    pitch.mul(&yaw)
}

/// Load the GTE view for a caller-built view rotation such as
/// [`view_rotation`]; `origin` is Q12 world units as in [`Camera`].
pub fn load_view_rotation(origin: GteVec3I32, view: Mat3I16) -> ViewTransform {
    let rotation = scene::compose_rotation_scheduled(&view, &COORDINATES);
    scene::load_rotation(&rotation);
    scene::load_translation(GteVec3I32::ZERO);
    let translation = scene::transform_vertex_scheduled(GteVec3I16::new(
        (origin.x.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (origin.y.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        (origin.z.saturating_neg() >> 12).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
    ));
    scene::load_translation(translation);
    ViewTransform {
        rotation,
        translation,
    }
}

/// Projection parameters the frustum clip is built from: the GTE H register
/// and the screen half-extents the caller projects into, plus a pixel margin
/// kept outside the visible edge and the near distance in world units.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewProjection {
    /// GTE H register (projection plane distance).
    pub focal_length: i32,
    /// Half the screen width, pixels.
    pub half_width: i32,
    /// Half the screen height, pixels.
    pub half_height: i32,
    /// Pixel margin kept outside the visible edge.
    pub edge_margin: i32,
    /// Near plane distance in world units.
    pub near_world: i32,
    /// Far cull distance in world units along the view axis; `i32::MAX`
    /// disables it. Cells wholly beyond it are skipped in the selection;
    /// quads of a straddling cell draw whole, so this is a cull, not a clip.
    pub far_world: i32,
}

impl ViewProjection {
    /// The widest projection any consumer uses (H=160, 90 degree horizontal
    /// FOV): a consumer that never overrides it clips less than it could,
    /// never more.
    pub const DEFAULT: Self = Self {
        focal_length: 160,
        half_width: 160,
        half_height: 120,
        edge_margin: 16,
        near_world: 8,
        far_world: i32::MAX,
    };
}
