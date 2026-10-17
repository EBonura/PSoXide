//! Free planes: flat textured quads placed anywhere in a room.
//!
//! A plane is not bound to the sector lattice. It has a centre, a width and a
//! height, three integer rotation angles, a material with the usual texture
//! placement, a sidedness and a collision class. Walls, floors and ceilings
//! stay on the grid; planes are for everything off it: bridges, slanted
//! walls, ramps, banners, grates, foliage cards.
//!
//! Frame (all integers in the document, `f64` only inside the geometry
//! helpers below):
//!
//! * Position: `center` is `[x, y, z]` in grid units, `x` and `z` in the
//!   world lattice like every sector, `y` relative to the owning room's
//!   `y_offset` like every height.
//! * Axes at zero rotation: `right` is `+X` (the width axis), `up` is `+Y`
//!   (the height axis), the front normal is `+Z`. So an unrotated plane
//!   stands like the north wall of a sector, facing south into the room.
//! * Angles, degrees: `roll` turns the plane about its normal, then `pitch`
//!   tilts the normal up (pitch 90 lays the plane flat, facing up; -90 flat,
//!   facing down), then `yaw` turns the result about the vertical axis.
//! * Texture: in [`PlaneUvMode::Tile`] the default mapping repeats every
//!   sector (32 texels) measured from the plane's top-left corner, so a
//!   banner and the wall beside it share a texel density. In
//!   [`PlaneUvMode::Stretch`] one whole texture covers the plane.

use serde::{Deserialize, Serialize};

use crate::face_uv::FaceUv;
use crate::ResourceId;

use super::units::{FINE_UNITS, SECTOR_UNITS};

/// Smallest width or height of a plane, grid units.
pub const MIN_PLANE_SIZE: i32 = FINE_UNITS;
/// Largest width or height of a plane, grid units: eight sectors.
pub const MAX_PLANE_SIZE: i32 = 8 * SECTOR_UNITS;
/// Size of a new plane, grid units.
pub const DEFAULT_PLANE_SIZE: [i32; 2] = [SECTOR_UNITS, SECTOR_UNITS];
/// Rotation snap of the editor, degrees.
pub const ROTATION_STEP: i32 = 15;

/// How the default texture mapping is laid on a plane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaneUvMode {
    /// Repeat the texture every sector, from the top-left corner.
    #[default]
    Tile,
    /// One whole texture across the plane.
    Stretch,
}

/// What a plane does to bodies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaneCollision {
    /// Drawn only.
    #[default]
    Decor,
    /// A surface to stand on: a bridge, platform or ramp. Planes steeper
    /// than the motor's walk limit cook as solid instead.
    Walkable,
    /// Blocks bodies from every side: a wall or fence.
    Solid,
}

impl PlaneCollision {
    /// Every class, in menu order.
    pub const ALL: [Self; 3] = [Self::Decor, Self::Walkable, Self::Solid];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Decor => "None (decor)",
            Self::Walkable => "Walkable",
            Self::Solid => "Solid",
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// One free plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridPlane {
    /// Centre `[x, y, z]`, grid units (`y` relative to the room's offset).
    pub center: [i32; 3],
    /// Width and height, grid units.
    pub size: [i32; 2],
    /// `[yaw, pitch, roll]`, degrees.
    #[serde(default, skip_serializing_if = "is_default")]
    pub rotation: [i32; 3],
    /// Material resource, `None` for the untextured fallback. Blend mode and
    /// alpha cutout (palette index 0 transparent) come from the material.
    #[serde(default)]
    pub material: Option<ResourceId>,
    /// Texture offset, rotation and scale (a negative scale mirrors).
    #[serde(default, skip_serializing_if = "FaceUv::is_identity")]
    pub uv: FaceUv,
    /// How the default mapping is laid out.
    #[serde(default, skip_serializing_if = "is_default")]
    pub uv_mode: PlaneUvMode,
    /// Whether both sides are drawn.
    #[serde(default, skip_serializing_if = "is_default")]
    pub double_sided: bool,
    /// Collision class.
    #[serde(default, skip_serializing_if = "is_default")]
    pub collision: PlaneCollision,
    /// Drawn at the material tint, with no light baked into it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub fullbright: bool,
    /// Hidden planes stay in the document but draw and collide with nothing.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl GridPlane {
    /// A plane of `size` centred on `center`, facing south, untextured.
    pub fn new(center: [i32; 3], size: [i32; 2]) -> Self {
        Self {
            center,
            size,
            rotation: [0; 3],
            material: None,
            uv: FaceUv::default(),
            uv_mode: PlaneUvMode::Tile,
            double_sided: false,
            collision: PlaneCollision::Decor,
            fullbright: false,
            enabled: true,
        }
    }

    /// Unit axes `(right, up, normal)` after the three rotations.
    pub fn axes(&self) -> ([f64; 3], [f64; 3], [f64; 3]) {
        let [yaw, pitch, roll] = self.rotation.map(|d| f64::from(d).to_radians());
        let (sr, cr) = roll.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let turn = |v: [f64; 3]| {
            // Pitch about X (positive tilts the normal up), then yaw about Y.
            let v = [v[0], v[1] * cp + v[2] * sp, -v[1] * sp + v[2] * cp];
            [v[0] * cy + v[2] * sy, v[1], -v[0] * sy + v[2] * cy]
        };
        (
            turn([cr, sr, 0.0]),
            turn([-sr, cr, 0.0]),
            turn([0.0, 0.0, 1.0]),
        )
    }

    /// Unit front normal.
    pub fn normal(&self) -> [f64; 3] {
        self.axes().2
    }

    /// Position of the point `s` grid units along `right` from the left
    /// edge and `t` down `up` from the top edge, grid units, unrounded.
    pub fn point(&self, s: f64, t: f64) -> [f64; 3] {
        let (right, up, _) = self.axes();
        let (w, h) = (f64::from(self.size[0]), f64::from(self.size[1]));
        let (a, b) = (s - w / 2.0, h / 2.0 - t);
        core::array::from_fn(|i| f64::from(self.center[i]) + right[i] * a + up[i] * b)
    }

    /// [`Self::point`] rounded to grid units.
    pub fn point_grid(&self, s: f64, t: f64) -> [i32; 3] {
        self.point(s, t).map(|v| v.round() as i32)
    }

    /// Corners top-left, top-right, bottom-right, bottom-left, grid units.
    pub fn corners(&self) -> [[i32; 3]; 4] {
        let (w, h) = (f64::from(self.size[0]), f64::from(self.size[1]));
        [
            self.point_grid(0.0, 0.0),
            self.point_grid(w, 0.0),
            self.point_grid(w, h),
            self.point_grid(0.0, h),
        ]
    }

    /// Box around the corners, `(min, max)`, grid units.
    pub fn bounds(&self) -> ([i32; 3], [i32; 3]) {
        let corners = self.corners();
        (
            core::array::from_fn(|i| corners.iter().map(|c| c[i]).min().unwrap_or(0)),
            core::array::from_fn(|i| corners.iter().map(|c| c[i]).max().unwrap_or(0)),
        )
    }

    /// World sector rectangle `(min, max)`, inclusive, the plane's box
    /// overlaps. A box that ends exactly on a sector edge does not reach the
    /// next sector.
    pub fn cell_span(&self) -> ([i32; 2], [i32; 2]) {
        let (lo, hi) = self.bounds();
        let first = |v: i32| v.div_euclid(SECTOR_UNITS);
        let last = |lo: i32, hi: i32| (hi - 1).div_euclid(SECTOR_UNITS).max(first(lo));
        (
            [first(lo[0]), first(lo[2])],
            [last(lo[0], hi[0]), last(lo[2], hi[2])],
        )
    }

    /// World sector containing the plane's centre.
    pub fn center_cell(&self) -> [i32; 2] {
        [
            self.center[0].div_euclid(SECTOR_UNITS),
            self.center[2].div_euclid(SECTOR_UNITS),
        ]
    }

    /// Slope of the front normal from vertical, as the `y` component of the
    /// unit normal. `1.0` faces up, `0.0` stands upright, `-1.0` faces down.
    pub fn normal_y(&self) -> f64 {
        self.normal()[1]
    }

    /// Clamp the size and the angles into the document's ranges and wrap the
    /// angles into `-180..=180`.
    pub fn normalize(&mut self) {
        for size in &mut self.size {
            *size = (*size).clamp(MIN_PLANE_SIZE, MAX_PLANE_SIZE);
        }
        for angle in &mut self.rotation {
            *angle = wrap_degrees(*angle);
        }
    }
}

/// Wrap an angle into `-180..=180`.
pub fn wrap_degrees(angle: i32) -> i32 {
    let wrapped = angle.rem_euclid(360);
    if wrapped > 180 {
        wrapped - 360
    } else {
        wrapped
    }
}

/// Rotation `[yaw, pitch, roll]` that makes a plane's front face `normal`
/// (a unit vector, `y` up) with the plane's height axis as upright as it can
/// be: walls and slanted walls stay level at the bottom, floors and ceilings
/// get the yaw nearest `fallback_yaw` (the viewer's heading).
pub fn rotation_facing(normal: [f64; 3], fallback_yaw: i32) -> [i32; 3] {
    let horizontal = (normal[0] * normal[0] + normal[2] * normal[2]).sqrt();
    let pitch = normal[1].atan2(horizontal).to_degrees().round() as i32;
    let yaw = if horizontal < 0.2 {
        fallback_yaw
    } else {
        normal[0].atan2(normal[2]).to_degrees().round() as i32
    };
    [wrap_degrees(yaw), pitch, 0]
}

/// Snap `value` to the nearest multiple of `step`.
pub fn snap_angle(value: i32, step: i32) -> i32 {
    super::units::snap_to(value, step.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn an_unrotated_plane_stands_like_a_north_wall() {
        let plane = GridPlane::new([512, 512, 0], [1024, 1024]);
        let (right, up, normal) = plane.axes();
        assert!(near(right, [1.0, 0.0, 0.0]));
        assert!(near(up, [0.0, 1.0, 0.0]));
        assert!(near(normal, [0.0, 0.0, 1.0]));
        assert_eq!(
            plane.corners(),
            [[0, 1024, 0], [1024, 1024, 0], [1024, 0, 0], [0, 0, 0]]
        );
    }

    #[test]
    fn pitch_lays_the_plane_flat_and_yaw_turns_it() {
        let mut plane = GridPlane::new([0, 0, 0], [1024, 512]);
        plane.rotation = [0, 90, 0];
        assert!(near(plane.normal(), [0.0, 1.0, 0.0]));
        assert!((plane.normal_y() - 1.0).abs() < 1e-9);
        plane.rotation = [90, 0, 0];
        assert!(near(plane.normal(), [1.0, 0.0, 0.0]));
        plane.rotation = [0, -90, 0];
        assert!(near(plane.normal(), [0.0, -1.0, 0.0]));
        // Roll leaves the normal alone and turns the width axis.
        plane.rotation = [0, 0, 90];
        assert!(near(plane.normal(), [0.0, 0.0, 1.0]));
        assert!(near(plane.axes().0, [0.0, 1.0, 0.0]));
    }

    #[test]
    fn a_flat_plane_is_horizontal_at_its_height() {
        let mut plane = GridPlane::new([2048, 256, 3072], [2048, 512]);
        plane.rotation = [0, 90, 0];
        for corner in plane.corners() {
            assert_eq!(corner[1], 256, "{corner:?}");
        }
        let (lo, hi) = plane.bounds();
        assert_eq!((hi[0] - lo[0], hi[2] - lo[2]), (2048, 512));
    }

    #[test]
    fn the_cell_span_covers_a_yawed_plane() {
        let mut plane = GridPlane::new([1024, 0, 1024], [2048, 256]);
        plane.rotation = [45, 0, 0];
        let (lo, hi) = plane.cell_span();
        assert!(lo[0] <= 0 && hi[0] >= 1 && lo[1] <= 0 && hi[1] >= 1);
    }

    #[test]
    fn rotation_facing_inverts_axes() {
        for rotation in [
            [0, 0, 0],
            [90, 0, 0],
            [-135, 0, 0],
            [30, 90, 0],
            [200, -90, 0],
        ] {
            let mut plane = GridPlane::new([0; 3], [1024; 2]);
            plane.rotation = rotation.map(wrap_degrees);
            let back = rotation_facing(plane.normal(), plane.rotation[0]);
            let mut again = GridPlane::new([0; 3], [1024; 2]);
            again.rotation = back;
            assert!(
                near(again.normal(), plane.normal()),
                "{rotation:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn angles_wrap_and_sizes_clamp() {
        assert_eq!(wrap_degrees(190), -170);
        assert_eq!(wrap_degrees(-190), 170);
        assert_eq!(wrap_degrees(360), 0);
        assert_eq!(wrap_degrees(180), 180);
        let mut plane = GridPlane::new([0; 3], [10, 99_999]);
        plane.rotation = [270, 0, -270];
        plane.normalize();
        assert_eq!(plane.size, [MIN_PLANE_SIZE, MAX_PLANE_SIZE]);
        assert_eq!(plane.rotation, [-90, 0, 90]);
    }

    #[test]
    fn defaults_keep_the_record_short() {
        let text = ron::to_string(&GridPlane::new([1, 2, 3], [64, 64])).unwrap();
        assert!(
            !text.contains("rotation") && !text.contains("collision"),
            "{text}"
        );
        let back: GridPlane = ron::from_str("(center:(1,2,3),size:(64,64))").unwrap();
        assert_eq!(back, GridPlane::new([1, 2, 3], [64, 64]));
    }
}
