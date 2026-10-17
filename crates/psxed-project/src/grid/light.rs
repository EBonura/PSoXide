//! Per-corner light bake of grid faces.
//!
//! Ambient plus lambert-weighted linear falloff from point lights, modulated
//! by the material tint, evaluated at each corner of a [`GridFace`]. Shadows
//! come from an occlusion query the caller supplies: the cook passes the
//! runtime collision's own line of sight, so the bake and the walkable world
//! agree about what is solid. Lights and points share one unit (editor units
//! for the editor preview, runtime units for the cook).

use super::faces::{GridFace, GridFaceKind};

/// Offset of a shadow ray's start along the surface normal, so a corner does
/// not shadow itself.
const SHADOW_NUDGE: f64 = 0.25;
/// Below this a distance or a ray length counts as zero.
const EPSILON: f64 = 1.0 / 1024.0;
/// The tint value that leaves a texture unmodified.
const NEUTRAL: f64 = 128.0;

/// A point light the bake evaluates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLight {
    /// Position in the bake's unit.
    pub position: [f64; 3],
    /// Reach in the same unit; no light beyond it.
    pub radius: f64,
    /// Q8.8 intensity, where 256 is one authored unit.
    pub intensity_q8: u16,
    /// Light colour.
    pub color: [u8; 3],
}

impl GridLight {
    /// Whether the light can be evaluated (finite, positive radius).
    pub fn is_valid(&self) -> bool {
        self.position.into_iter().all(f64::is_finite)
            && self.radius.is_finite()
            && self.radius > 0.0
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn modulated(tint: u8, light: f64) -> u8 {
    (f64::from(tint) * light.clamp(0.0, 255.0) / NEUTRAL)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Lit colour of one point on a surface facing `normal` (unit length).
///
/// `occluded(from, to)` says whether the segment between two points is
/// blocked by solid world; pass `&|_, _| false` for the unshadowed look.
pub fn lit_corner(
    point: [f64; 3],
    normal: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
) -> [u8; 3] {
    lit_corner_nudged(point, normal, tint, ambient, lights, occluded, SHADOW_NUDGE)
}

/// [`lit_corner`] with an explicit start offset of the shadow ray along the
/// normal. A free plane needs a larger one than a floor: its slab collision
/// is cooked from rounded corners, so a corner can sit a unit inside it.
pub fn lit_corner_nudged(
    point: [f64; 3],
    normal: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    nudge: f64,
) -> [u8; 3] {
    let mut accumulated = ambient.map(f64::from);
    let start = [
        point[0] + normal[0] * nudge,
        point[1] + normal[1] * nudge,
        point[2] + normal[2] * nudge,
    ];
    for light in lights {
        let to_light = [
            light.position[0] - point[0],
            light.position[1] - point[1],
            light.position[2] - point[2],
        ];
        let distance_squared = dot(to_light, to_light);
        if distance_squared >= light.radius * light.radius {
            continue;
        }
        let distance = distance_squared.sqrt();
        let direction = if distance > EPSILON {
            to_light.map(|v| v / distance)
        } else {
            normal
        };
        let lambert = dot(normal, direction).max(0.0);
        if lambert <= 0.0 || occluded(start, light.position) {
            continue;
        }
        let attenuation = 1.0 - distance / light.radius;
        let weight = attenuation * lambert * f64::from(light.intensity_q8) / 256.0;
        for (channel, color) in accumulated.iter_mut().zip(light.color) {
            *channel += f64::from(color) * weight;
        }
    }
    [
        modulated(tint[0], accumulated[0]),
        modulated(tint[1], accumulated[1]),
        modulated(tint[2], accumulated[2]),
    ]
}

/// Shadow ray start offset of a free plane's corner, in the bake's unit when
/// that is runtime units (the cook). The editor preview casts no shadows and
/// ignores it.
pub const PLANE_SHADOW_NUDGE: f64 = 2.0;

/// Lit colour of one corner of `face`. Planes add two things the sector
/// faces do not have: a plane in `fullbright` shows its tint unlit, and a
/// double-sided plane is lit from whichever side the light is on.
pub fn lit_face_corner(
    face: &GridFace,
    point: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
) -> [u8; 3] {
    let plane = matches!(face.kind, GridFaceKind::Plane { .. });
    if !plane {
        return lit_corner(point, face.normal, tint, ambient, lights, occluded);
    }
    if face.unlit {
        return lit_corner(
            point,
            face.normal,
            tint,
            [NEUTRAL as u8; 3],
            &[],
            &|_, _| false,
        );
    }
    let front = lit_corner_nudged(
        point,
        face.normal,
        tint,
        ambient,
        lights,
        occluded,
        PLANE_SHADOW_NUDGE,
    );
    if !face.double_sided {
        return front;
    }
    let back = lit_corner_nudged(
        point,
        face.normal.map(|v| -v),
        tint,
        ambient,
        lights,
        occluded,
        PLANE_SHADOW_NUDGE,
    );
    core::array::from_fn(|i| front[i].max(back[i]))
}

/// Lit colour of every corner of `face` in editor units, without shadows.
///
/// `tint` is the material's modulation colour (`[128; 3]` leaves the texture
/// as authored) and `ambient` the room's ambient light.
pub fn bake_face_corners(
    face: &GridFace,
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
) -> Vec<[u8; 3]> {
    face.points
        .iter()
        .map(|p| lit_face_corner(face, p.map(f64::from), tint, ambient, lights, &|_, _| false))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::faces::all_faces;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;
    use crate::grid::world::GridWorld;

    fn floor_face() -> GridFace {
        let mut room = GridRoom::new("r", 1, 1);
        room.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
        };
        all_faces(&world).remove(0)
    }

    #[test]
    fn unlit_faces_show_ambient_modulated_by_tint() {
        let face = floor_face();
        let colors = bake_face_corners(&face, [128; 3], [64; 3], &[]);
        assert_eq!(colors, vec![[64; 3]; 4]);
        let tinted = bake_face_corners(&face, [255, 128, 0], [128; 3], &[]);
        assert_eq!(tinted[0], [255, 128, 0]);
    }

    #[test]
    fn a_lamp_above_lights_the_near_corners_more() {
        let face = floor_face();
        let lamp = GridLight {
            position: [0.0, 400.0, 0.0],
            radius: 2000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let colors = bake_face_corners(&face, [128; 3], [32; 3], &[lamp]);
        let by_point = |x: i32, z: i32| {
            let i = face
                .points
                .iter()
                .position(|p| p[0] == x && p[2] == z)
                .unwrap();
            colors[i][0]
        };
        assert!(by_point(0, 0) > by_point(1024, 1024));
        assert!(by_point(1024, 1024) >= 32);
    }

    #[test]
    fn faces_turned_away_from_the_lamp_stay_at_ambient() {
        let face = floor_face();
        let below = GridLight {
            position: [512.0, -400.0, 512.0],
            radius: 4000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let colors = bake_face_corners(&face, [128; 3], [32; 3], &[below]);
        assert!(colors.iter().all(|c| *c == [32; 3]));
    }

    #[test]
    fn a_blocked_shadow_ray_leaves_the_corner_at_ambient() {
        let face = floor_face();
        let lamp = GridLight {
            position: [0.0, 400.0, 0.0],
            radius: 2000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let lit = bake_face_corners(&face, [128; 3], [32; 3], &[lamp]);
        let shadowed: Vec<[u8; 3]> = face
            .points
            .iter()
            .map(|p| {
                lit_corner(
                    p.map(f64::from),
                    face.normal,
                    [128; 3],
                    [32; 3],
                    &[lamp],
                    &|_, _| true,
                )
            })
            .collect();
        assert!(lit.iter().any(|c| c[0] > 32));
        assert!(shadowed.iter().all(|c| *c == [32; 3]));
    }
}
